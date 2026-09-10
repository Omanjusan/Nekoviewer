use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// サムネキャッシュテーブル: キー=ファイル名, バリュー=(source_mtime_secs: i64, jpeg_blob: Vec<u8>)
pub const THUMBS_TABLE: TableDefinition<&str, (i64, &[u8])> = TableDefinition::new("thumbs");
/// サムネイルJPEGの生成元entry_name。空文字は従来のデフォルト（先頭画像）。
pub const THUMB_SOURCES_TABLE: TableDefinition<&str, &str> = TableDefinition::new("thumb_sources_v1");
/// 実行中の旧ワーカーが設定変更後のJPEGを上書きしないための期待生成元。
pub const THUMB_DESIRED_SOURCES_TABLE: TableDefinition<&str, &str> =
    TableDefinition::new("thumb_desired_sources_v1");
/// サムネイルJPEGを生成した時の長辺サイズ。未登録の既存レコードは旧仕様の256pxとみなす。
pub const THUMB_EDGES_TABLE: TableDefinition<&str, u32> = TableDefinition::new("thumb_edges_v1");
/// サムネイルJPEG生成時のリサイズフィルタ安定ID。未登録は旧形式のため不明とみなす。
pub const THUMB_FILTERS_TABLE: TableDefinition<&str, u32> = TableDefinition::new("thumb_filters_v1");
/// ファイル単位の生成状態。
/// value=(status, token, generated_at, updated_at, target_edge, target_filter, target_mtime)
pub const THUMB_STATES_TABLE: TableDefinition<&str, (u8, u64, i64, i64, u32, u32, i64)> =
    TableDefinition::new("thumb_states_v2");
/// 旧削除UIとの移行期間だけ読み書きする互換テーブル。生成可否には使用しない。
const THUMB_GENERATION_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("thumb_generation_v1");
const THUMB_GENERATION_EDGE_KEY: &str = "edge";
const THUMB_GENERATION_FILTER_KEY: &str = "filter";
const THUMB_GENERATION_EPOCH_KEY: &str = "epoch";
const LEGACY_THUMB_EDGE: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThumbnailStatus {
    Missing = 1,
    Processing = 2,
    Current = 3,
    Stale = 4,
}

impl ThumbnailStatus {
    fn from_id(id: u8) -> Self {
        match id {
            2 => Self::Processing,
            3 => Self::Current,
            4 => Self::Stale,
            _ => Self::Missing,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailRecordState {
    pub status: ThumbnailStatus,
    pub token: u64,
    pub generated_at: i64,
    pub updated_at: i64,
    pub target_edge: u32,
    pub target_filter: u32,
    pub target_mtime: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbnailGenerationState {
    pub requested_edge: u32,
    pub requested_filter: u32,
    pub epoch: u64,
    pub allowed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailDeleteResult {
    pub success: bool,
    pub deleted: usize,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailCacheStats {
    pub total: usize,
    pub matching: usize,
    pub mismatched: usize,
    pub min_edge: Option<u32>,
    pub max_edge: Option<u32>,
    pub filter_mask: u8,
    pub unknown_filter: usize,
}

/// 非画像ZIPマーカーテーブル: キー=ファイル名, バリュー=source_mtime_secs: i64
pub const INVALID_TABLE: TableDefinition<&str, i64> = TableDefinition::new("invalid");

/// ファイル索引テーブル（検索機能用）: キー=ファイル名, バリュー=(mtime_secs: i64, size_bytes: u64)
/// サムネ生成が完了したファイルのみ記録される（サムネ未取得ファイルは検索対象外）。
pub const FILES_TABLE: TableDefinition<&str, (i64, u64)> = TableDefinition::new("files");

/// 逆引き用テーブル: キー="source_dir"固定, バリュー=このDBに対応する実ディレクトリの絶対パス文字列。
/// neko_dir_for() が一方向ハッシュのため、DBファイル単体からPWDを算出するにはこれが要る。
const SOURCE_DIR_TABLE: TableDefinition<&str, &str> = TableDefinition::new("source_dir");
const SOURCE_DIR_KEY: &str = "source_dir";

/// メタ情報テーブル: キー="schema_version"等の固定文字列, バリュー=u32
const META_TABLE: TableDefinition<&str, u32> = TableDefinition::new("meta");
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// サムネ生成ロジック（Exif Orientation対応等）を変えてキャッシュ済みJPEG blobの
/// 中身が古い前提と食い違うようになった時にインクリメントする。アプリのバージョン
/// (Cargo.toml)とは無関係の、DBスキーマ専用の値。
const SCHEMA_VERSION: u32 = 2;

/// dir に対応するキャッシュディレクトリのパスを返す（まだ作成しない）。
pub fn neko_dir_for(dir: &Path, config: &AppConfig) -> Option<PathBuf> {
    Some(neko_dir_for_root(dir, &config.cache_root()?))
}

/// dir に対応するキャッシュディレクトリのパスを、cache_root を直接指定して返す。
/// バックグラウンドスレッドなど AppConfig（非Send/Clone）を持ち込めない文脈から使う
/// （検索ワーカー等）。cache_root は呼び出し側が事前に config.cache_root() で取得しておく。
pub fn neko_dir_for_root(dir: &Path, cache_root: &Path) -> PathBuf {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = sha256_hex(key.to_string_lossy().as_bytes());
    cache_root.join(hash)
}

/// プロセス内で開いた cache.redb のレジストリ（メモリ上のみ。ディスクには何も作らない）。
/// redb は同一ファイルの多重オープンを排他ロックで拒否するため、ワーカーのキューに
/// 旧 Arc が残っている間に同じフォルダへ戻ると再オープンが失敗して cache_db=None になる。
/// 一度開いたDBはセッション中ここに保持して使い回し、再オープン自体を発生させない。
static OPEN_DBS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Database>>>>> = OnceLock::new();

/// cache.redb が既に存在する場合のみ開いて返す。無ければ None（作成しない）。
/// 対象ファイルの無いフォルダに空DBを量産しないための入口。
pub fn open_cache_db_if_exists(neko_dir: &Path, source_dir: &Path) -> Option<Arc<Mutex<Database>>> {
    if !neko_dir.join("cache.redb").exists() {
        return None;
    }
    open_cache_db(neko_dir, source_dir)
}

/// キャッシュディレクトリ以下の cache.redb を開いて返す。
/// ディレクトリが存在しなければ作成する。失敗時は None。
/// 同じDBを既に開いている場合はレジストリの既存ハンドルを返す。
/// source_dir は検索機能の逆引き（DB→PWD）用に SOURCE_DIR_TABLE へ記録する。
pub fn open_cache_db(neko_dir: &Path, source_dir: &Path) -> Option<Arc<Mutex<Database>>> {
    let db_path = neko_dir.join("cache.redb");
    let registry = OPEN_DBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock().ok()?;
    if let Some(db) = registry.get(&db_path) {
        return Some(Arc::clone(db));
    }
    std::fs::create_dir_all(neko_dir).ok()?;
    let db = Database::create(&db_path).ok()?;
    // テーブルを初期化（存在しなければ作成）
    {
        let tx = db.begin_write().ok()?;
        tx.open_table(INVALID_TABLE).ok()?;
        tx.open_table(THUMBS_TABLE).ok()?;
        tx.open_table(THUMB_SOURCES_TABLE).ok()?;
        tx.open_table(THUMB_DESIRED_SOURCES_TABLE).ok()?;
        tx.open_table(THUMB_EDGES_TABLE).ok()?;
        tx.open_table(THUMB_FILTERS_TABLE).ok()?;
        tx.open_table(THUMB_STATES_TABLE).ok()?;
        tx.open_table(THUMB_GENERATION_TABLE).ok()?;
        tx.open_table(FILES_TABLE).ok()?;
        {
            let mut source_dir_table = tx.open_table(SOURCE_DIR_TABLE).ok()?;
            let abs = source_dir.to_string_lossy();
            let _ = source_dir_table.insert(SOURCE_DIR_KEY, abs.as_ref());
        }
        tx.commit().ok()?;
    }
    enforce_schema_version(&db);
    let db = Arc::new(Mutex::new(db));
    registry.insert(db_path, Arc::clone(&db));
    Some(db)
}

/// このDBファイルが対応する実ディレクトリの絶対パスを返す（PWDの逆引き）。
/// 検索機能で「cache.redb一覧→対応PWD」を辿るために使う。
pub fn dir_for_db(db: &Arc<Mutex<Database>>) -> Option<PathBuf> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(SOURCE_DIR_TABLE).ok()?;
    let guard = table.get(SOURCE_DIR_KEY).ok()??;
    Some(PathBuf::from(guard.value()))
}

/// 旧形式のJPEGを保持したままv2状態へ移行する。
/// 中断されたprocessingもここで回収し、次回訪問時に再生成できる状態へ戻す。
fn enforce_schema_version(db: &Database) {
    let Ok(tx) = db.begin_write() else { return };
    let now = unix_timestamp_secs();
    let thumb_keys: Vec<String> = {
        let Ok(thumbs) = tx.open_table(THUMBS_TABLE) else { return };
        thumbs.iter().ok().into_iter().flatten().flatten()
            .map(|item| item.0.value().to_owned())
            .collect()
    };
    let existing_keys: std::collections::HashSet<&str> =
        thumb_keys.iter().map(String::as_str).collect();
    {
        let Ok(mut states) = tx.open_table(THUMB_STATES_TABLE) else { return };
        for key in &thumb_keys {
            if states.get(key.as_str()).ok().flatten().is_none() {
                let _ = states.insert(
                    key.as_str(),
                    (ThumbnailStatus::Stale as u8, 0, 0, now, 0, 0, 0),
                );
            }
        }
        let processing: Vec<(String, ThumbnailRecordState)> = states.iter().ok()
            .into_iter().flatten().flatten()
            .filter_map(|item| {
                let state = decode_thumbnail_state(item.1.value());
                (state.status == ThumbnailStatus::Processing)
                    .then(|| (item.0.value().to_owned(), state))
            })
            .collect();
        for (key, state) in processing {
            let recovered = if existing_keys.contains(key.as_str()) {
                ThumbnailStatus::Stale
            } else {
                ThumbnailStatus::Missing
            };
            let _ = states.insert(
                key.as_str(),
                encode_thumbnail_state(ThumbnailRecordState {
                    status: recovered,
                    updated_at: now,
                    ..state
                }),
            );
        }
    }
    if let Ok(mut meta) = tx.open_table(META_TABLE) {
        let _ = meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION);
    }
    let _ = tx.commit();
}

fn unix_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn decode_thumbnail_state(value: (u8, u64, i64, i64, u32, u32, i64)) -> ThumbnailRecordState {
    ThumbnailRecordState {
        status: ThumbnailStatus::from_id(value.0),
        token: value.1,
        generated_at: value.2,
        updated_at: value.3,
        target_edge: value.4,
        target_filter: value.5,
        target_mtime: value.6,
    }
}

fn encode_thumbnail_state(state: ThumbnailRecordState) -> (u8, u64, i64, i64, u32, u32, i64) {
    (
        state.status as u8,
        state.token,
        state.generated_at,
        state.updated_at,
        state.target_edge,
        state.target_filter,
        state.target_mtime,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRIANGLE: u32 = 2;
    const LANCZOS3: u32 = 4;

    fn unique_test_db_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nekoviewer_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("cache.redb")
    }

    /// open_cache_db に渡す neko_dir（キャッシュ先ディレクトリ）用のユニークな未作成パスを返す。
    /// ディレクトリ自体は open_cache_db 側が作成する。
    fn unique_test_neko_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nekoviewer_test_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ))
    }

    #[test]
    fn enforce_schema_version_preserves_legacy_thumb_as_stale() {
        let db_path = unique_test_db_path("schema_version");
        let db = Database::create(&db_path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            tx.open_table(THUMBS_TABLE).unwrap();
            tx.open_table(META_TABLE).unwrap();
            tx.commit().unwrap();
        }

        {
            let tx = db.begin_write().unwrap();
            let mut thumbs = tx.open_table(THUMBS_TABLE).unwrap();
            thumbs.insert("a.zip", (100i64, b"jpeg-bytes".as_slice())).unwrap();
            drop(thumbs);
            tx.commit().unwrap();
        }
        enforce_schema_version(&db);
        {
            let tx = db.begin_read().unwrap();
            let thumbs = tx.open_table(THUMBS_TABLE).unwrap();
            assert!(thumbs.get("a.zip").unwrap().is_some(), "旧JPEGを保持する");
            let states = tx.open_table(THUMB_STATES_TABLE).unwrap();
            let state = decode_thumbnail_state(states.get("a.zip").unwrap().unwrap().value());
            assert_eq!(state.status, ThumbnailStatus::Stale);
            let meta = tx.open_table(META_TABLE).unwrap();
            assert_eq!(meta.get(SCHEMA_VERSION_KEY).unwrap().unwrap().value(), SCHEMA_VERSION);
        }

        drop(db);
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
    }

    #[test]
    fn dir_for_db_round_trips_source_dir() {
        let neko_dir = unique_test_neko_dir("source_dir_roundtrip");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_test");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");

        let recovered = dir_for_db(&db).expect("source dir should be recorded");
        assert_eq!(recovered, source_dir);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn thumb_source_roundtrips_default_and_registered_entry() {
        let neko_dir = unique_test_neko_dir("thumb_source_roundtrip");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_source");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");

        assert!(read_thumb_source(&db, "book.zip").is_none());
        write_thumb_source(&db, "book.zip", Some("pages/cover.jpg"));
        assert_eq!(read_thumb_source(&db, "book.zip").as_deref(), Some("pages/cover.jpg"));
        write_thumb_source(&db, "book.zip", None);
        assert_eq!(read_thumb_source(&db, "book.zip").as_deref(), Some(""));

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn remove_thumb_deletes_jpeg_and_source_only() {
        let neko_dir = unique_test_neko_dir("remove_thumb");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_remove_thumb");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        write_thumb(&db, "book.zip", 100, b"jpeg-bytes");
        write_thumb_source(&db, "book.zip", Some("left\0pages/cover.jpg"));
        write_file_record(&db, "book.zip", 100, 1234);

        remove_thumb(&db, "book.zip");

        assert!(read_thumb_unchecked(&db, "book.zip").is_none());
        assert!(read_thumb_source(&db, "book.zip").is_none());
        {
            let db = db.lock().unwrap();
            let tx = db.begin_read().unwrap();
            let table = tx.open_table(FILES_TABLE).unwrap();
            assert_eq!(table.get("book.zip").unwrap().unwrap().value(), (100, 1234));
        }
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn source_change_keeps_old_blob_and_rejects_old_token() {
        let neko_dir = unique_test_neko_dir("thumb_source_race");
        let db = open_cache_db(&neko_dir, Path::new("/tmp/fake_thumb_source_race")).unwrap();
        write_thumb(&db, "book.zip", 100, b"old-jpeg");
        write_thumb_source(&db, "book.zip", Some("left\0pages/cover.jpg"));
        enforce_schema_version(&db.lock().unwrap());

        let old_token = claim_thumbnail_generation(
            &db, "book.zip", 100, 256, TRIANGLE, "left\0pages/cover.jpg",
        ).unwrap();
        reset_thumb_for_source(&db, "book.zip", "right\0pages/cover.jpg");
        assert_eq!(read_thumb_unchecked(&db, "book.zip").unwrap().1, b"old-jpeg");
        assert!(!finish_thumbnail_generation(
            &db, "book.zip", 100, b"late-left", "left\0pages/cover.jpg",
            256, TRIANGLE, old_token,
        ));

        let new_token = claim_thumbnail_generation(
            &db, "book.zip", 100, 256, TRIANGLE, "right\0pages/cover.jpg",
        ).unwrap();
        assert!(finish_thumbnail_generation(
            &db, "book.zip", 100, b"right-jpeg", "right-v2\0pages/cover.jpg",
            256, TRIANGLE, new_token,
        ));
        assert_eq!(read_thumb_unchecked(&db, "book.zip").unwrap().1, b"right-jpeg");
        assert_eq!(read_thumbnail_state(&db, "book.zip").unwrap().status, ThumbnailStatus::Current);
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn file_level_cas_does_not_block_other_profiles() {
        let neko_dir = unique_test_neko_dir("thumb_file_cas");
        let db = open_cache_db(&neko_dir, Path::new("/tmp/fake_thumb_file_cas")).unwrap();
        let a = claim_thumbnail_generation(&db, "a.zip", 100, 256, TRIANGLE, "").unwrap();
        assert!(claim_thumbnail_generation(&db, "a.zip", 100, 256, TRIANGLE, "").is_none());
        let b = claim_thumbnail_generation(&db, "b.zip", 200, 384, LANCZOS3, "").unwrap();
        assert!(finish_thumbnail_generation(&db, "a.zip", 100, b"a", "", 256, TRIANGLE, a));
        assert!(finish_thumbnail_generation(&db, "b.zip", 200, b"b", "", 384, LANCZOS3, b));
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn failed_generation_restores_stale_without_deleting_blob() {
        let neko_dir = unique_test_neko_dir("thumb_failure_restore");
        let db = open_cache_db(&neko_dir, Path::new("/tmp/fake_thumb_failure_restore")).unwrap();
        write_thumb(&db, "book.zip", 100, b"old-jpeg");
        enforce_schema_version(&db.lock().unwrap());
        let token = claim_thumbnail_generation(&db, "book.zip", 100, 384, TRIANGLE, "").unwrap();
        fail_thumbnail_generation(&db, "book.zip", token);
        assert_eq!(read_thumb_unchecked(&db, "book.zip").unwrap().1, b"old-jpeg");
        assert_eq!(read_thumbnail_state(&db, "book.zip").unwrap().status, ThumbnailStatus::Stale);
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn interrupted_processing_is_recovered_without_deleting_blob() {
        let db_path = unique_test_db_path("processing_recovery");
        let db = Database::create(&db_path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            tx.open_table(THUMBS_TABLE).unwrap()
                .insert("with-thumb.zip", (100, b"jpeg".as_slice())).unwrap();
            let mut states = tx.open_table(THUMB_STATES_TABLE).unwrap();
            let processing = |token| encode_thumbnail_state(ThumbnailRecordState {
                status: ThumbnailStatus::Processing,
                token,
                generated_at: 0,
                updated_at: 1,
                target_edge: 384,
                target_filter: TRIANGLE,
                target_mtime: 100,
            });
            states.insert("with-thumb.zip", processing(7)).unwrap();
            states.insert("missing.zip", processing(8)).unwrap();
            drop(states);
            tx.commit().unwrap();
        }

        enforce_schema_version(&db);
        let tx = db.begin_read().unwrap();
        let states = tx.open_table(THUMB_STATES_TABLE).unwrap();
        assert_eq!(
            decode_thumbnail_state(states.get("with-thumb.zip").unwrap().unwrap().value()).status,
            ThumbnailStatus::Stale,
        );
        assert_eq!(
            decode_thumbnail_state(states.get("missing.zip").unwrap().unwrap().value()).status,
            ThumbnailStatus::Missing,
        );
        drop(states);
        drop(tx);
        drop(db);
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap());
    }

    #[test]
    fn successful_pwd_sync_adds_missing_and_removes_deleted_records() {
        let neko_dir = unique_test_neko_dir("thumb_pwd_sync");
        let db = open_cache_db(&neko_dir, Path::new("/tmp/fake_thumb_pwd_sync")).unwrap();
        write_thumb(&db, "deleted.zip", 100, b"old");
        write_file_record(&db, "deleted.zip", 100, 10);
        enforce_schema_version(&db.lock().unwrap());

        assert!(sync_thumbnail_records(&db, &["present.zip".to_string()]));
        assert_eq!(
            read_thumbnail_state(&db, "present.zip").unwrap().status,
            ThumbnailStatus::Missing,
        );
        assert!(read_thumbnail_state(&db, "deleted.zip").is_none());
        assert!(read_thumb_unchecked(&db, "deleted.zip").is_none());
        let db_guard = db.lock().unwrap();
        let tx = db_guard.begin_read().unwrap();
        assert!(tx.open_table(FILES_TABLE).unwrap().get("deleted.zip").unwrap().is_none());
        drop(tx);
        drop(db_guard);
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn search_files_applies_and_conditions() {
        let neko_dir = unique_test_neko_dir("search_and");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_search_test");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");

        // a.zip: 10MB, 2024-01-01 / b.zip: 20MB, 2024-06-01 / c.zip: 5MB, 2024-06-01
        const JAN1_2024: i64 = 1_704_067_200;
        const JUN1_2024: i64 = 1_717_200_000;
        const MB: u64 = 1024 * 1024;
        write_file_record(&db, "a.zip", JAN1_2024, 10 * MB);
        write_file_record(&db, "b.zip", JUN1_2024, 20 * MB);
        write_file_record(&db, "c.zip", JUN1_2024, 5 * MB);

        // ファイル名条件のみ
        let by_name = search_files(&db, |n| n.starts_with('a'), None, None, None, None);
        assert_eq!(by_name, vec!["a.zip".to_string()]);

        // サイズ10MB以上
        let mut by_size_min = search_files(&db, |_| true, Some(10 * MB), None, None, None);
        by_size_min.sort();
        assert_eq!(by_size_min, vec!["a.zip".to_string(), "b.zip".to_string()]);

        // サイズ10MB以下
        let mut by_size_max = search_files(&db, |_| true, None, Some(10 * MB), None, None);
        by_size_max.sort();
        assert_eq!(by_size_max, vec!["a.zip".to_string(), "c.zip".to_string()]);

        // 2024-06-01以降
        let mut by_mtime_min = search_files(&db, |_| true, None, None, Some(JUN1_2024), None);
        by_mtime_min.sort();
        assert_eq!(by_mtime_min, vec!["b.zip".to_string(), "c.zip".to_string()]);

        // 2024-06-01より前
        let by_mtime_max = search_files(&db, |_| true, None, None, None, Some(JAN1_2024));
        assert_eq!(by_mtime_max, vec!["a.zip".to_string()]);

        // AND条件: サイズ10MB以上 かつ 2024-06-01以降 -> b.zipのみ
        let and_result = search_files(&db, |_| true, Some(10 * MB), None, Some(JUN1_2024), None);
        assert_eq!(and_result, vec!["b.zip".to_string()]);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn search_files_excludes_thumbnail_not_generated_files() {
        let neko_dir = unique_test_neko_dir("search_excludes_no_thumb");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_exclude_test");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");

        // サムネ取得済み(FILES_TABLE書き込み済み)のファイルのみ検索対象になる。
        // "not_indexed.zip" はあえて write_file_record を呼ばず、サムネ未取得状態を模す。
        write_file_record(&db, "indexed.zip", 0, 100);

        let results = search_files(&db, |_| true, None, None, None, None);
        assert_eq!(results, vec!["indexed.zip".to_string()]);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }
}

/// サムネをmtime検証なしでDBから読み込む。戻り値は (保存時のsource_mtime, jpeg)。
/// mtime検証は呼び出し側が表示後に後追いで行う（stale-while-revalidate）。
/// ネットワークパスではstatがDB読みより桁違いに遅い・失敗しうるため、
/// 検証をこの関数に含めない。
pub fn read_thumb_unchecked(db: &Arc<Mutex<Database>>, filename: &str) -> Option<(i64, Vec<u8>)> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMBS_TABLE).ok()?;
    let guard = table.get(filename).ok()??;
    let (stored_mtime, jpeg) = guard.value();
    Some((stored_mtime, jpeg.to_vec()))
}

/// サムネをDBに書き込む。source_mtime==0（stat失敗）のエントリは保存しない。
/// 0を保存するとネットワーク回復後に実mtimeと不一致になり、恒久的に再生成が走る。
#[cfg(test)]
pub fn write_thumb(db: &Arc<Mutex<Database>>, filename: &str, source_mtime: i64, jpeg: &[u8]) {
    if source_mtime == 0 {
        return;
    }
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBS_TABLE) {
        let _ = table.insert(filename, (source_mtime, jpeg));
    }
    let _ = tx.commit();
}

pub fn read_thumb_source(db: &Arc<Mutex<Database>>, filename: &str) -> Option<String> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMB_SOURCES_TABLE).ok()?;
    Some(table.get(filename).ok()??.value().to_string())
}

pub fn read_thumbnail_state(
    db: &Arc<Mutex<Database>>,
    filename: &str,
) -> Option<ThumbnailRecordState> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMB_STATES_TABLE).ok()?;
    Some(decode_thumbnail_state(table.get(filename).ok()??.value()))
}

/// 正常完了したPWD走査結果とRDBのファイル単位状態を同期する。
/// 未登録ファイルにはmissingを作り、実体が消えたキーだけを関連テーブルから除去する。
pub fn sync_thumbnail_records(db: &Arc<Mutex<Database>>, filenames: &[String]) -> bool {
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    let wanted: std::collections::HashSet<&str> = filenames.iter().map(String::as_str).collect();
    let now = unix_timestamp_secs();
    let thumb_keys: std::collections::HashSet<String> = {
        let Ok(thumbs) = tx.open_table(THUMBS_TABLE) else { return false };
        thumbs.iter().ok().into_iter().flatten().flatten()
            .map(|item| item.0.value().to_owned())
            .collect()
    };
    let stale_keys: Vec<String> = {
        let Ok(states) = tx.open_table(THUMB_STATES_TABLE) else { return false };
        states.iter().ok().into_iter().flatten().flatten()
            .filter_map(|item| (!wanted.contains(item.0.value())).then(|| item.0.value().to_owned()))
            .collect()
    };
    {
        let Ok(mut states) = tx.open_table(THUMB_STATES_TABLE) else { return false };
        for filename in filenames {
            if states.get(filename.as_str()).ok().flatten().is_none() {
                let status = if thumb_keys.contains(filename) {
                    ThumbnailStatus::Stale
                } else {
                    ThumbnailStatus::Missing
                };
                let _ = states.insert(filename.as_str(), encode_thumbnail_state(ThumbnailRecordState {
                    status,
                    token: 0,
                    generated_at: 0,
                    updated_at: now,
                    target_edge: 0,
                    target_filter: 0,
                    target_mtime: 0,
                }));
            }
        }
    }
    for key in &stale_keys {
        if let Ok(mut table) = tx.open_table(THUMBS_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_SOURCES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_EDGES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_FILTERS_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_STATES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(FILES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(INVALID_TABLE) { let _ = table.remove(key.as_str()); }
    }
    tx.commit().is_ok()
}

#[cfg(test)]
pub fn write_thumb_source(db: &Arc<Mutex<Database>>, filename: &str, entry_name: Option<&str>) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMB_SOURCES_TABLE) {
        let _ = table.insert(filename, entry_name.unwrap_or(""));
    }
    let _ = tx.commit();
}

/// 対象アーカイブのJPEGと生成元情報だけを同一トランザクションで破棄する。
#[cfg(test)]
pub fn remove_thumb(db: &Arc<Mutex<Database>>, filename: &str) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBS_TABLE) {
        let _ = table.remove(filename);
    }
    if let Ok(mut table) = tx.open_table(THUMB_SOURCES_TABLE) {
        let _ = table.remove(filename);
    }
    if let Ok(mut table) = tx.open_table(THUMB_EDGES_TABLE) {
        let _ = table.remove(filename);
    }
    if let Ok(mut table) = tx.open_table(THUMB_FILTERS_TABLE) {
        let _ = table.remove(filename);
    }
    if let Ok(mut table) = tx.open_table(THUMB_STATES_TABLE) {
        let _ = table.remove(filename);
    }
    let _ = tx.commit();
}

/// 旧JPEGを保持したまま、登録ページ変更をファイル単位でstale化する。
pub fn reset_thumb_for_source(db: &Arc<Mutex<Database>>, filename: &str, source: &str) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    let has_thumb = tx.open_table(THUMBS_TABLE).ok()
        .is_some_and(|table| table.get(filename).ok().flatten().is_some());
    let old_state = tx.open_table(THUMB_STATES_TABLE).ok()
        .and_then(|table| table.get(filename).ok().flatten().map(|v| decode_thumbnail_state(v.value())));
    let token = old_state.map_or(1, |state| state.token.wrapping_add(1));
    let generated_at = old_state.map_or(0, |state| state.generated_at);
    if let Ok(mut table) = tx.open_table(THUMB_STATES_TABLE) {
        let _ = table.insert(filename, encode_thumbnail_state(ThumbnailRecordState {
            status: if has_thumb { ThumbnailStatus::Stale } else { ThumbnailStatus::Missing },
            token,
            generated_at,
            updated_at: unix_timestamp_secs(),
            target_edge: 0,
            target_filter: 0,
            target_mtime: 0,
        }));
    }
    if let Ok(mut table) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) {
        let _ = table.insert(filename, source);
    }
    let _ = tx.commit();
}

/// missing/staleからprocessingへのファイル単位CASを行う。
/// 既に同じ条件でprocessing中、またはcurrentなら取得しない。
pub fn claim_thumbnail_generation(
    db: &Arc<Mutex<Database>>,
    filename: &str,
    source_mtime: i64,
    requested_edge: u32,
    requested_filter: u32,
    desired_source: &str,
) -> Option<u64> {
    if source_mtime == 0 {
        return None;
    }
    let db = db.lock().ok()?;
    let tx = db.begin_write().ok()?;
    let old_state = tx.open_table(THUMB_STATES_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| decode_thumbnail_state(v.value()));
    let stored_desired = tx.open_table(THUMB_DESIRED_SOURCES_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| v.value().to_owned());
    if old_state.is_some_and(|state| {
        state.status == ThumbnailStatus::Processing
            && state.target_edge == requested_edge
            && state.target_filter == requested_filter
            && state.target_mtime == source_mtime
            && stored_desired.as_deref().is_some_and(|stored| {
                thumbnail_desired_source_matches(stored, desired_source)
            })
    }) {
        return None;
    }

    let stored_mtime = tx.open_table(THUMBS_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| v.value().0);
    let stored_edge = tx.open_table(THUMB_EDGES_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| v.value());
    let stored_filter = tx.open_table(THUMB_FILTERS_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| v.value());
    let stored_source = tx.open_table(THUMB_SOURCES_TABLE).ok()?
        .get(filename).ok().flatten().map(|v| v.value().to_owned());
    let profile_matches = stored_mtime == Some(source_mtime)
        && stored_edge == Some(requested_edge)
        && stored_filter == Some(requested_filter)
        && match stored_source.as_deref() {
            Some(actual) => thumbnail_desired_source_matches(desired_source, actual),
            None => desired_source.is_empty(),
        };
    if old_state.is_some_and(|state| state.status == ThumbnailStatus::Current)
        && profile_matches
    {
        return None;
    }

    let token = old_state.map_or(1, |state| state.token.wrapping_add(1));
    let generated_at = old_state.map_or(0, |state| state.generated_at);
    {
        let mut states = tx.open_table(THUMB_STATES_TABLE).ok()?;
        states.insert(filename, encode_thumbnail_state(ThumbnailRecordState {
            status: ThumbnailStatus::Processing,
            token,
            generated_at,
            updated_at: unix_timestamp_secs(),
            target_edge: requested_edge,
            target_filter: requested_filter,
            target_mtime: source_mtime,
        })).ok()?;
    }
    {
        let mut desired = tx.open_table(THUMB_DESIRED_SOURCES_TABLE).ok()?;
        desired.insert(filename, desired_source).ok()?;
    }
    tx.commit().ok()?;
    Some(token)
}

/// CAS取得時のtokenと生成条件が現在も一致する場合だけ、旧Blobを原子的に置き換える。
pub fn finish_thumbnail_generation(
    db: &Arc<Mutex<Database>>,
    filename: &str,
    source_mtime: i64,
    jpeg: &[u8],
    source: &str,
    requested_edge: u32,
    requested_filter: u32,
    generation_token: u64,
) -> bool {
    if source_mtime == 0 {
        return false;
    }
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    let state = tx.open_table(THUMB_STATES_TABLE).ok()
        .and_then(|table| table.get(filename).ok().flatten().map(|v| decode_thumbnail_state(v.value())));
    if !state.is_some_and(|state| {
        state.status == ThumbnailStatus::Processing
            && state.token == generation_token
            && state.target_edge == requested_edge
            && state.target_filter == requested_filter
            && state.target_mtime == source_mtime
    }) {
        return false;
    }
    if let Ok(table) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) {
        if table.get(filename).ok().flatten().is_some_and(|desired| {
            !thumbnail_desired_source_matches(desired.value(), source)
        }) {
            return false;
        }
    }
    {
        let Ok(mut thumbs) = tx.open_table(THUMBS_TABLE) else { return false };
        let _ = thumbs.insert(filename, (source_mtime, jpeg));
    }
    {
        let Ok(mut sources) = tx.open_table(THUMB_SOURCES_TABLE) else { return false };
        let _ = sources.insert(filename, source);
    }
    {
        let Ok(mut edges) = tx.open_table(THUMB_EDGES_TABLE) else { return false };
        let _ = edges.insert(filename, requested_edge);
    }
    {
        let Ok(mut filters) = tx.open_table(THUMB_FILTERS_TABLE) else { return false };
        let _ = filters.insert(filename, requested_filter);
    }
    {
        let Ok(mut desired) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) else { return false };
        let _ = desired.insert(filename, source);
    }
    {
        let Ok(mut states) = tx.open_table(THUMB_STATES_TABLE) else { return false };
        let now = unix_timestamp_secs();
        let _ = states.insert(filename, encode_thumbnail_state(ThumbnailRecordState {
            status: ThumbnailStatus::Current,
            token: generation_token,
            generated_at: now,
            updated_at: now,
            target_edge: requested_edge,
            target_filter: requested_filter,
            target_mtime: source_mtime,
        }));
    }
    tx.commit().is_ok()
}

/// 生成失敗時、同じtokenがprocessing中なら旧Blobの有無に応じて再試行可能状態へ戻す。
pub fn fail_thumbnail_generation(
    db: &Arc<Mutex<Database>>,
    filename: &str,
    generation_token: u64,
) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    let state = tx.open_table(THUMB_STATES_TABLE).ok()
        .and_then(|table| table.get(filename).ok().flatten().map(|v| decode_thumbnail_state(v.value())));
    let Some(state) = state.filter(|state| {
        state.status == ThumbnailStatus::Processing && state.token == generation_token
    }) else { return };
    let has_thumb = tx.open_table(THUMBS_TABLE).ok()
        .is_some_and(|table| table.get(filename).ok().flatten().is_some());
    if let Ok(mut states) = tx.open_table(THUMB_STATES_TABLE) {
        let _ = states.insert(filename, encode_thumbnail_state(ThumbnailRecordState {
            status: if has_thumb { ThumbnailStatus::Stale } else { ThumbnailStatus::Missing },
            updated_at: unix_timestamp_secs(),
            ..state
        }));
    }
    let _ = tx.commit();
}

/// 互換UI向けのPWD状態。生成可否はファイル単位CASへ移行したため常に許可する。
pub fn thumbnail_generation_state(
    db: &Arc<Mutex<Database>>,
    requested_edge: u32,
    requested_filter: u32,
) -> ThumbnailGenerationState {
    let Ok(db) = db.lock() else {
        return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: true };
    };
    let Ok(tx) = db.begin_write() else {
        return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: true };
    };
    let epoch = {
        let Ok(mut generation) = tx.open_table(THUMB_GENERATION_TABLE) else {
            return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: true };
        };
        let epoch = generation.get(THUMB_GENERATION_EPOCH_KEY).ok().flatten()
            .map(|v| v.value()).unwrap_or(0);
        let _ = generation.insert(THUMB_GENERATION_EDGE_KEY, requested_edge as u64);
        let _ = generation.insert(THUMB_GENERATION_FILTER_KEY, requested_filter as u64);
        epoch
    };
    let _ = tx.commit();
    ThumbnailGenerationState { requested_edge, requested_filter, epoch, allowed: true }
}

pub fn thumbnail_cache_stats(
    db: &Arc<Mutex<Database>>,
    requested_edge: u32,
    requested_filter: u32,
) -> ThumbnailCacheStats {
    let Ok(db) = db.lock() else { return ThumbnailCacheStats::default() };
    let Ok(tx) = db.begin_read() else { return ThumbnailCacheStats::default() };
    let Ok(thumbs) = tx.open_table(THUMBS_TABLE) else { return ThumbnailCacheStats::default() };
    let Ok(edges) = tx.open_table(THUMB_EDGES_TABLE) else { return ThumbnailCacheStats::default() };
    let Ok(filters) = tx.open_table(THUMB_FILTERS_TABLE) else { return ThumbnailCacheStats::default() };
    let mut stats = ThumbnailCacheStats::default();
    if let Ok(iter) = thumbs.iter() {
        for item in iter.flatten() {
            let filename = item.0.value();
            let edge = edges.get(filename).ok().flatten()
                .map(|v| v.value()).unwrap_or(LEGACY_THUMB_EDGE);
            let filter = filters.get(filename).ok().flatten().map(|v| v.value()).unwrap_or(0);
            stats.total += 1;
            if edge == requested_edge && filter == requested_filter { stats.matching += 1; } else { stats.mismatched += 1; }
            if filter == 0 {
                stats.unknown_filter += 1;
            } else if filter <= 7 {
                stats.filter_mask |= 1 << filter;
            }
            stats.min_edge = Some(stats.min_edge.map_or(edge, |v| v.min(edge)));
            stats.max_edge = Some(stats.max_edge.map_or(edge, |v| v.max(edge)));
        }
    }
    stats
}

fn delete_thumbnails(db: &Arc<Mutex<Database>>, requested_edge: u32, requested_filter: u32, all: bool) -> ThumbnailDeleteResult {
    let Ok(db) = db.lock() else { return ThumbnailDeleteResult::default() };
    let Ok(tx) = db.begin_write() else { return ThumbnailDeleteResult::default() };
    let keys: Vec<String> = {
        let Ok(thumbs) = tx.open_table(THUMBS_TABLE) else { return ThumbnailDeleteResult::default() };
        let Ok(edges) = tx.open_table(THUMB_EDGES_TABLE) else { return ThumbnailDeleteResult::default() };
        let Ok(filters) = tx.open_table(THUMB_FILTERS_TABLE) else { return ThumbnailDeleteResult::default() };
        thumbs.iter().ok().into_iter().flatten().flatten().filter_map(|item| {
            let filename = item.0.value();
            let edge = edges.get(filename).ok().flatten()
                .map(|v| v.value()).unwrap_or(LEGACY_THUMB_EDGE);
            let filter = filters.get(filename).ok().flatten().map(|v| v.value()).unwrap_or(0);
            (all || edge != requested_edge || filter != requested_filter).then(|| filename.to_owned())
        }).collect()
    };
    for key in &keys {
        if let Ok(mut table) = tx.open_table(THUMBS_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_SOURCES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_EDGES_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_FILTERS_TABLE) { let _ = table.remove(key.as_str()); }
        if let Ok(mut table) = tx.open_table(THUMB_STATES_TABLE) { let _ = table.remove(key.as_str()); }
    }
    let epoch = {
        let Ok(mut generation) = tx.open_table(THUMB_GENERATION_TABLE) else { return ThumbnailDeleteResult::default() };
        let next = generation.get(THUMB_GENERATION_EPOCH_KEY).ok().flatten()
            .map(|v| v.value()).unwrap_or(0).wrapping_add(1);
        let _ = generation.insert(THUMB_GENERATION_EDGE_KEY, requested_edge as u64);
        let _ = generation.insert(THUMB_GENERATION_FILTER_KEY, requested_filter as u64);
        let _ = generation.insert(THUMB_GENERATION_EPOCH_KEY, next);
        next
    };
    if tx.commit().is_ok() {
        ThumbnailDeleteResult { success: true, deleted: keys.len(), epoch }
    } else {
        ThumbnailDeleteResult::default()
    }
}

pub fn delete_mismatched_thumbnails(db: &Arc<Mutex<Database>>, requested_edge: u32, requested_filter: u32) -> ThumbnailDeleteResult {
    delete_thumbnails(db, requested_edge, requested_filter, false)
}

pub fn delete_all_thumbnails(db: &Arc<Mutex<Database>>, requested_edge: u32, requested_filter: u32) -> ThumbnailDeleteResult {
    delete_thumbnails(db, requested_edge, requested_filter, true)
}

fn thumbnail_desired_source_matches(desired: &str, actual: &str) -> bool {
    if desired == actual {
        return true;
    }
    // 画質修正前の左右マーカーは同じ選択内容なので、新JPEGへの一度だけの更新を許可する。
    actual.strip_prefix("left-v2\0").is_some_and(|entry| desired == format!("left\0{entry}"))
        || actual.strip_prefix("right-v2\0").is_some_and(|entry| desired == format!("right\0{entry}"))
}

/// ファイル索引（検索用）をDBに書き込む。write_thumb と対で呼ぶ想定
/// （サムネ生成が完了したファイルのみ検索対象になる）。
pub fn write_file_record(db: &Arc<Mutex<Database>>, filename: &str, mtime: i64, size: u64) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(FILES_TABLE) {
        let _ = table.insert(filename, (mtime, size));
    }
    let _ = tx.commit();
}

/// 非画像ZIPマーカーを書き込む。
pub fn mark_invalid(db: &Arc<Mutex<Database>>, filename: &str, source_mtime: i64) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(INVALID_TABLE) {
        let _ = table.insert(filename, source_mtime);
    }
    let _ = tx.commit();
}

/// 非画像ZIPマーカーが存在し、かつZIPが差し替えられていない場合 true。
/// 先にローカルDBを引き、マーク済みの場合のみstatする。
/// マーク無しが大多数のため、ネットワークパスへの全件statを避けられる。
pub fn is_invalid_and_current(db: &Arc<Mutex<Database>>, filename: &str, archive_path: &Path) -> bool {
    let stored_mtime = {
        let Ok(db) = db.lock() else { return false };
        let Ok(tx) = db.begin_read() else { return false };
        let Ok(table) = tx.open_table(INVALID_TABLE) else { return false };
        match table.get(filename) {
            Ok(Some(guard)) => guard.value(),
            _ => return false,
        }
    };
    stored_mtime == file_mtime(archive_path)
}

/// キャッシュ済みサムネ件数をカウントする（ツリービュー表示用）。
pub fn count_cached_thumbs(db: &Arc<Mutex<Database>>, filenames: &[String]) -> usize {
    let Ok(db) = db.lock() else { return 0 };
    let Ok(tx) = db.begin_read() else { return 0 };
    let Ok(table) = tx.open_table(THUMBS_TABLE) else { return 0 };
    filenames.iter().filter(|name| {
        matches!(table.get(name.as_str()), Ok(Some(_)))
    }).count()
}

/// ファイルのmtimeをi64（Unix秒）で返す。取得失敗時は0。
pub fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64)
        .unwrap_or(0)
}

/// ファイルサイズをバイト単位で返す。取得失敗時は0。
pub fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// FILES_TABLE をAND条件で絞り込み、マッチしたファイル名一覧を返す
/// （検索機能用。ファイル名判定は呼び出し側のクロージャに委ねる＝既存filterのglob/部分一致
/// ロジックをそのまま渡せる）。size/mtime の各範囲は None で無条件（下限のみ・上限のみも可）。
pub fn search_files(
    db: &Arc<Mutex<Database>>,
    name_matches: impl Fn(&str) -> bool,
    size_min: Option<u64>,
    size_max: Option<u64>,
    mtime_min: Option<i64>,
    mtime_max: Option<i64>,
) -> Vec<String> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(FILES_TABLE) else { return Vec::new() };
    let Ok(iter) = table.iter() else { return Vec::new() };
    iter.filter_map(|entry| entry.ok())
        .filter_map(|(k, v)| {
            let name = k.value().to_string();
            let (mtime, size) = v.value();
            if !name_matches(&name) {
                return None;
            }
            if size_min.is_some_and(|min| size < min) {
                return None;
            }
            if size_max.is_some_and(|max| size > max) {
                return None;
            }
            if mtime_min.is_some_and(|min| mtime < min) {
                return None;
            }
            if mtime_max.is_some_and(|max| mtime > max) {
                return None;
            }
            Some(name)
        })
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
