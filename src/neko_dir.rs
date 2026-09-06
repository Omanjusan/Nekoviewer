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

pub fn write_thumb_source(db: &Arc<Mutex<Database>>, filename: &str, entry_name: Option<&str>) {
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMB_SOURCES_TABLE) {
        let _ = table.insert(filename, entry_name.unwrap_or(""));
    }
    let _ = tx.commit();
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
