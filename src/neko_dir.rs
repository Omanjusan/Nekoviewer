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
/// PWD単位の生成許可サイズと世代。削除前のワーカーによる遅延書き戻しを拒否する。
const THUMB_GENERATION_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("thumb_generation_v1");
const THUMB_GENERATION_EDGE_KEY: &str = "edge";
const THUMB_GENERATION_FILTER_KEY: &str = "filter";
const THUMB_GENERATION_EPOCH_KEY: &str = "epoch";
const LEGACY_THUMB_EDGE: u32 = 256;

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
const SCHEMA_VERSION: u32 = 1;

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

/// スキーマバージョン不一致（未対応の生成ロジックで焼かれた古いサムネが混在しうる）
/// ならサムネだけ丸ごと破棄して全再生成させる。失敗時は何もしない（次回オープン時に再試行される）。
fn enforce_schema_version(db: &Database) {
    let Ok(tx) = db.begin_write() else { return };
    {
        let Ok(mut thumbs) = tx.open_table(THUMBS_TABLE) else { return };
        let Ok(mut meta) = tx.open_table(META_TABLE) else { return };
        let stored_version = meta.get(SCHEMA_VERSION_KEY).ok().flatten().map(|g| g.value());
        if stored_version != Some(SCHEMA_VERSION) {
            let _ = thumbs.retain(|_, _| false);
            let _ = meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION);
        }
    }
    let _ = tx.commit();
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
    fn enforce_schema_version_clears_thumbs_on_version_mismatch() {
        let db_path = unique_test_db_path("schema_version");
        let db = Database::create(&db_path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            tx.open_table(THUMBS_TABLE).unwrap();
            tx.open_table(META_TABLE).unwrap();
            tx.commit().unwrap();
        }

        // 初回: バージョン未記録 -> サムネ書き込み後もこの時点では影響なし
        enforce_schema_version(&db);
        {
            let tx = db.begin_write().unwrap();
            let mut thumbs = tx.open_table(THUMBS_TABLE).unwrap();
            thumbs.insert("a.zip", (100i64, b"jpeg-bytes".as_slice())).unwrap();
            drop(thumbs);
            tx.commit().unwrap();
        }
        assert!({
            let tx = db.begin_read().unwrap();
            let thumbs = tx.open_table(THUMBS_TABLE).unwrap();
            thumbs.get("a.zip").unwrap().is_some()
        });

        // バージョンを意図的に古い値へ書き換えて再度enforceすると、サムネが一掃される
        {
            let tx = db.begin_write().unwrap();
            {
                let mut meta = tx.open_table(META_TABLE).unwrap();
                meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.wrapping_sub(1)).unwrap();
            }
            tx.commit().unwrap();
        }
        enforce_schema_version(&db);
        {
            let tx = db.begin_read().unwrap();
            let thumbs = tx.open_table(THUMBS_TABLE).unwrap();
            assert!(thumbs.get("a.zip").unwrap().is_none(), "バージョン不一致でサムネが破棄されるはず");
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
    fn reset_thumb_rejects_late_result_from_old_source() {
        let neko_dir = unique_test_neko_dir("thumb_source_race");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_source_race");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        write_thumb(&db, "book.zip", 100, b"old-jpeg");
        write_thumb_source(&db, "book.zip", Some("left\0pages/cover.jpg"));

        reset_thumb_for_source(&db, "book.zip", "right\0pages/cover.jpg");
        let generation = thumbnail_generation_state(&db, 256, TRIANGLE);

        assert!(read_thumb_unchecked(&db, "book.zip").is_none());
        assert_eq!(
            read_thumb_desired_source(&db, "book.zip").as_deref(),
            Some("right\0pages/cover.jpg")
        );
        assert!(!write_generated_thumb_if_current(
            &db,
            "book.zip",
            100,
            b"late-left-jpeg",
            "left\0pages/cover.jpg",
            256,
            TRIANGLE,
            generation.epoch,
        ));
        assert!(read_thumb_unchecked(&db, "book.zip").is_none());
        assert!(write_generated_thumb_if_current(
            &db,
            "book.zip",
            100,
            b"right-jpeg",
            "right-v2\0pages/cover.jpg",
            256,
            TRIANGLE,
            generation.epoch,
        ));
        assert_eq!(
            read_thumb_source(&db, "book.zip").as_deref(),
            Some("right-v2\0pages/cover.jpg")
        );
        assert_eq!(
            read_thumb_desired_source(&db, "book.zip").as_deref(),
            Some("right-v2\0pages/cover.jpg")
        );
        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn mismatched_delete_keeps_matching_thumbs_and_advances_epoch() {
        let neko_dir = unique_test_neko_dir("thumb_size_mismatch");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_size_mismatch");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        {
            let db_guard = db.lock().unwrap();
            let tx = db_guard.begin_write().unwrap();
            {
                let mut thumbs = tx.open_table(THUMBS_TABLE).unwrap();
                thumbs.insert("old.zip", (100, b"old".as_slice())).unwrap();
                thumbs.insert("current.zip", (100, b"current".as_slice())).unwrap();
            }
            {
                let mut edges = tx.open_table(THUMB_EDGES_TABLE).unwrap();
                edges.insert("old.zip", 128).unwrap();
                edges.insert("current.zip", 384).unwrap();
            }
            {
                let mut filters = tx.open_table(THUMB_FILTERS_TABLE).unwrap();
                filters.insert("old.zip", TRIANGLE).unwrap();
                filters.insert("current.zip", TRIANGLE).unwrap();
            }
            tx.commit().unwrap();
        }

        let before = thumbnail_generation_state(&db, 384, TRIANGLE);
        assert!(!before.allowed);
        let stats = thumbnail_cache_stats(&db, 384, TRIANGLE);
        assert_eq!((stats.total, stats.matching, stats.mismatched), (2, 1, 1));
        assert_eq!((stats.min_edge, stats.max_edge), (Some(128), Some(384)));
        let deleted = delete_mismatched_thumbnails(&db, 384, TRIANGLE);
        assert!(deleted.success);
        assert_eq!(deleted.deleted, 1);
        assert!(read_thumb_unchecked(&db, "old.zip").is_none());
        assert!(read_thumb_unchecked(&db, "current.zip").is_some());
        let after = thumbnail_generation_state(&db, 384, TRIANGLE);
        assert!(after.allowed);
        assert_eq!(after.epoch, deleted.epoch);
        assert_ne!(after.epoch, before.epoch);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn full_delete_rejects_result_from_previous_epoch() {
        let neko_dir = unique_test_neko_dir("thumb_delete_epoch");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_delete_epoch");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        let old = thumbnail_generation_state(&db, 256, TRIANGLE);
        assert!(old.allowed);

        let deleted = delete_all_thumbnails(&db, 384, LANCZOS3);
        assert!(deleted.success);
        assert!(!write_generated_thumb_if_current(
            &db, "late.zip", 100, b"late", "", 256, TRIANGLE, old.epoch,
        ));
        assert!(write_generated_thumb_if_current(
            &db, "new.zip", 100, b"new", "", 384, LANCZOS3, deleted.epoch,
        ));
        assert_eq!(read_thumb_unchecked(&db, "new.zip").unwrap().1, b"new");

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn changing_size_blocks_generation_and_changing_back_restores_it() {
        let neko_dir = unique_test_neko_dir("thumb_size_roundtrip");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_size_roundtrip");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        write_thumb(&db, "legacy.zip", 100, b"legacy-256");
        {
            let db_guard = db.lock().unwrap();
            let tx = db_guard.begin_write().unwrap();
            tx.open_table(THUMB_FILTERS_TABLE).unwrap().insert("legacy.zip", TRIANGLE).unwrap();
            tx.commit().unwrap();
        }

        let original = thumbnail_generation_state(&db, 256, TRIANGLE);
        assert!(original.allowed, "既存レコードはレガシー256pxとして利用できる");
        let changed = thumbnail_generation_state(&db, 384, TRIANGLE);
        assert!(!changed.allowed, "既存256pxを残したまま384px生成を開始しない");
        assert_ne!(changed.epoch, original.epoch, "設定変更で旧ワーカーを失効させる");
        assert!(!write_generated_thumb_if_current(
            &db, "late.zip", 100, b"late", "", 256, TRIANGLE, original.epoch,
        ));

        let restored = thumbnail_generation_state(&db, 256, TRIANGLE);
        assert!(restored.allowed, "元の設定へ戻せば削除せず再利用できる");
        assert_ne!(restored.epoch, changed.epoch);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn same_size_with_different_filter_blocks_until_mismatch_delete() {
        let neko_dir = unique_test_neko_dir("thumb_filter_mismatch");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_filter_mismatch");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        let triangle = thumbnail_generation_state(&db, 256, TRIANGLE);
        assert!(write_generated_thumb_if_current(
            &db, "book.zip", 100, b"triangle", "", 256, TRIANGLE, triangle.epoch,
        ));

        let lanczos = thumbnail_generation_state(&db, 256, LANCZOS3);
        assert!(!lanczos.allowed, "サイズが同じでもフィルタ違いなら生成を止める");
        assert_ne!(lanczos.epoch, triangle.epoch);
        let stats = thumbnail_cache_stats(&db, 256, LANCZOS3);
        assert_eq!((stats.matching, stats.mismatched), (0, 1));
        assert_eq!(stats.filter_mask, 1 << TRIANGLE);

        let deleted = delete_mismatched_thumbnails(&db, 256, LANCZOS3);
        assert!(deleted.success);
        assert_eq!(deleted.deleted, 1);
        assert!(thumbnail_generation_state(&db, 256, LANCZOS3).allowed);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn legacy_record_with_unknown_filter_is_mismatched() {
        let neko_dir = unique_test_neko_dir("thumb_legacy_filter_unknown");
        let source_dir = PathBuf::from("/tmp/fake_source_dir_for_thumb_legacy_filter_unknown");
        let db = open_cache_db(&neko_dir, &source_dir).expect("db should open");
        write_thumb(&db, "legacy.zip", 100, b"legacy");

        let state = thumbnail_generation_state(&db, 256, TRIANGLE);
        assert!(!state.allowed);
        let stats = thumbnail_cache_stats(&db, 256, TRIANGLE);
        assert_eq!(stats.unknown_filter, 1);
        assert_eq!(stats.mismatched, 1);

        let _ = std::fs::remove_dir_all(&neko_dir);
    }

    #[test]
    fn generation_state_is_independent_for_each_pwd_database() {
        let neko_dir_a = unique_test_neko_dir("thumb_pwd_a");
        let neko_dir_b = unique_test_neko_dir("thumb_pwd_b");
        let db_a = open_cache_db(&neko_dir_a, Path::new("/tmp/fake_thumb_pwd_a")).unwrap();
        let db_b = open_cache_db(&neko_dir_b, Path::new("/tmp/fake_thumb_pwd_b")).unwrap();
        write_thumb(&db_a, "legacy.zip", 100, b"legacy-256");
        {
            let db_guard = db_a.lock().unwrap();
            let tx = db_guard.begin_write().unwrap();
            tx.open_table(THUMB_FILTERS_TABLE).unwrap().insert("legacy.zip", TRIANGLE).unwrap();
            tx.commit().unwrap();
        }

        let state_a = thumbnail_generation_state(&db_a, 384, TRIANGLE);
        let state_b = thumbnail_generation_state(&db_b, 384, TRIANGLE);
        assert!(!state_a.allowed, "既存256pxを持つPWDは不一致");
        assert!(state_b.allowed, "空の別PWDは384px生成を直ちに許可");

        let deleted = delete_mismatched_thumbnails(&db_a, 384, TRIANGLE);
        assert!(deleted.success);
        assert!(thumbnail_generation_state(&db_a, 384, TRIANGLE).allowed);
        assert_eq!(thumbnail_cache_stats(&db_b, 384, TRIANGLE).total, 0, "別PWDは変更しない");

        let _ = std::fs::remove_dir_all(&neko_dir_a);
        let _ = std::fs::remove_dir_all(&neko_dir_b);
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
    let _ = tx.commit();
}

/// 旧JPEGを破棄し、次に許可する生成元を同一トランザクションで切り替える。
pub fn reset_thumb_for_source(db: &Arc<Mutex<Database>>, filename: &str, source: &str) {
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
    if let Ok(mut table) = tx.open_table(THUMB_DESIRED_SOURCES_TABLE) {
        let _ = table.insert(filename, source);
    }
    let _ = tx.commit();
}

#[cfg(test)]
fn read_thumb_desired_source(db: &Arc<Mutex<Database>>, filename: &str) -> Option<String> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMB_DESIRED_SOURCES_TABLE).ok()?;
    Some(table.get(filename).ok()??.value().to_string())
}

/// 現在期待されている生成元と一致する場合だけJPEGと生成元を一括保存する。
pub fn write_generated_thumb_if_current(
    db: &Arc<Mutex<Database>>,
    filename: &str,
    source_mtime: i64,
    jpeg: &[u8],
    source: &str,
    requested_edge: u32,
    requested_filter: u32,
    generation_epoch: u64,
) -> bool {
    if source_mtime == 0 {
        return false;
    }
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_write() else { return false };
    if let Ok(generation) = tx.open_table(THUMB_GENERATION_TABLE) {
        let current_edge = generation
            .get(THUMB_GENERATION_EDGE_KEY).ok().flatten().map(|v| v.value() as u32);
        let current_epoch = generation
            .get(THUMB_GENERATION_EPOCH_KEY).ok().flatten().map(|v| v.value()).unwrap_or(0);
        let current_filter = generation
            .get(THUMB_GENERATION_FILTER_KEY).ok().flatten().map(|v| v.value() as u32);
        if current_edge != Some(requested_edge)
            || current_filter != Some(requested_filter)
            || current_epoch != generation_epoch
        {
            return false;
        }
    } else {
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
    tx.commit().is_ok()
}

/// 現在のRDBが requested_edge で生成可能かを判定する。
/// JPEGが無い、または全JPEGが要求サイズと一致する場合だけ生成を許可する。
pub fn thumbnail_generation_state(
    db: &Arc<Mutex<Database>>,
    requested_edge: u32,
    requested_filter: u32,
) -> ThumbnailGenerationState {
    let Ok(db) = db.lock() else {
        return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
    };
    let Ok(tx) = db.begin_write() else {
        return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
    };
    let mut allowed = true;
    {
        let Ok(thumbs) = tx.open_table(THUMBS_TABLE) else {
            return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
        };
        let Ok(edges) = tx.open_table(THUMB_EDGES_TABLE) else {
            return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
        };
        let Ok(filters) = tx.open_table(THUMB_FILTERS_TABLE) else {
            return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
        };
        if let Ok(iter) = thumbs.iter() {
            for item in iter.flatten() {
                let filename = item.0.value();
                let edge = edges.get(filename).ok().flatten()
                    .map(|v| v.value()).unwrap_or(LEGACY_THUMB_EDGE);
                let filter = filters.get(filename).ok().flatten().map(|v| v.value()).unwrap_or(0);
                if edge != requested_edge || filter != requested_filter {
                    allowed = false;
                    break;
                }
            }
        }
    }
    let epoch = {
        let Ok(mut generation) = tx.open_table(THUMB_GENERATION_TABLE) else {
            return ThumbnailGenerationState { requested_edge, requested_filter, epoch: 0, allowed: false };
        };
        let mut epoch = generation.get(THUMB_GENERATION_EPOCH_KEY).ok().flatten()
            .map(|v| v.value()).unwrap_or(0);
        let stored_edge = generation.get(THUMB_GENERATION_EDGE_KEY).ok().flatten()
            .map(|v| v.value() as u32);
        let stored_filter = generation.get(THUMB_GENERATION_FILTER_KEY).ok().flatten()
            .map(|v| v.value() as u32);
        if stored_edge != Some(requested_edge) || stored_filter != Some(requested_filter) {
            // 不一致で生成停止中でも世代を進め、旧サイズの処理結果を即座に無効化する。
            epoch = epoch.wrapping_add(1);
            let _ = generation.insert(THUMB_GENERATION_EDGE_KEY, requested_edge as u64);
            let _ = generation.insert(THUMB_GENERATION_FILTER_KEY, requested_filter as u64);
            let _ = generation.insert(THUMB_GENERATION_EPOCH_KEY, epoch);
        }
        epoch
    };
    if tx.commit().is_err() {
        allowed = false;
    }
    ThumbnailGenerationState { requested_edge, requested_filter, epoch, allowed }
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
