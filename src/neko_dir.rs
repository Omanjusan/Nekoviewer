use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// サムネキャッシュテーブル: キー=ファイル名, バリュー=(source_mtime_secs: i64, jpeg_blob: Vec<u8>)
pub const THUMBS_TABLE: TableDefinition<&str, (i64, &[u8])> = TableDefinition::new("thumbs");

/// 非画像ZIPマーカーテーブル: キー=ファイル名, バリュー=source_mtime_secs: i64
pub const INVALID_TABLE: TableDefinition<&str, i64> = TableDefinition::new("invalid");

/// メタ情報テーブル: キー="schema_version"等の固定文字列, バリュー=u32
const META_TABLE: TableDefinition<&str, u32> = TableDefinition::new("meta");
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// サムネ生成ロジック（Exif Orientation対応等）を変えてキャッシュ済みJPEG blobの
/// 中身が古い前提と食い違うようになった時にインクリメントする。アプリのバージョン
/// (Cargo.toml)とは無関係の、DBスキーマ専用の値。
const SCHEMA_VERSION: u32 = 1;

/// dir に対応するキャッシュディレクトリのパスを返す（まだ作成しない）。
pub fn neko_dir_for(dir: &Path, config: &AppConfig) -> Option<PathBuf> {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = sha256_hex(key.to_string_lossy().as_bytes());
    Some(config.cache_root()?.join(hash))
}

/// プロセス内で開いた cache.redb のレジストリ（メモリ上のみ。ディスクには何も作らない）。
/// redb は同一ファイルの多重オープンを排他ロックで拒否するため、ワーカーのキューに
/// 旧 Arc が残っている間に同じフォルダへ戻ると再オープンが失敗して cache_db=None になる。
/// 一度開いたDBはセッション中ここに保持して使い回し、再オープン自体を発生させない。
static OPEN_DBS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Database>>>>> = OnceLock::new();

/// cache.redb が既に存在する場合のみ開いて返す。無ければ None（作成しない）。
/// 対象ファイルの無いフォルダに空DBを量産しないための入口。
pub fn open_cache_db_if_exists(neko_dir: &Path) -> Option<Arc<Mutex<Database>>> {
    if !neko_dir.join("cache.redb").exists() {
        return None;
    }
    open_cache_db(neko_dir)
}

/// キャッシュディレクトリ以下の cache.redb を開いて返す。
/// ディレクトリが存在しなければ作成する。失敗時は None。
/// 同じDBを既に開いている場合はレジストリの既存ハンドルを返す。
pub fn open_cache_db(neko_dir: &Path) -> Option<Arc<Mutex<Database>>> {
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
        tx.commit().ok()?;
    }
    enforce_schema_version(&db);
    let db = Arc::new(Mutex::new(db));
    registry.insert(db_path, Arc::clone(&db));
    Some(db)
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

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
