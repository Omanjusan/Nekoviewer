use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, TableDefinition};
use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// サムネキャッシュテーブル: キー=ファイル名, バリュー=(source_mtime_secs: i64, jpeg_blob: Vec<u8>)
pub const THUMBS_TABLE: TableDefinition<&str, (i64, &[u8])> = TableDefinition::new("thumbs");

/// 非画像ZIPマーカーテーブル: キー=ファイル名, バリュー=source_mtime_secs: i64
pub const INVALID_TABLE: TableDefinition<&str, i64> = TableDefinition::new("invalid");

/// dir に対応するキャッシュディレクトリのパスを返す（まだ作成しない）。
pub fn neko_dir_for(dir: &Path, config: &AppConfig) -> Option<PathBuf> {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = sha256_hex(key.to_string_lossy().as_bytes());
    Some(config.cache_root()?.join(hash))
}

/// キャッシュディレクトリ以下の cache.redb を開いて返す。
/// ディレクトリが存在しなければ作成する。失敗時は None。
pub fn open_cache_db(neko_dir: &Path) -> Option<Arc<Mutex<Database>>> {
    std::fs::create_dir_all(neko_dir).ok()?;
    let db_path = neko_dir.join("cache.redb");
    let db = Database::create(&db_path).ok()?;
    // テーブルを初期化（存在しなければ作成）
    {
        let tx = db.begin_write().ok()?;
        tx.open_table(THUMBS_TABLE).ok()?;
        tx.open_table(INVALID_TABLE).ok()?;
        tx.commit().ok()?;
    }
    Some(Arc::new(Mutex::new(db)))
}

/// サムネをDBから読み込む。source_mtime が一致しない場合は None（再生成が必要）。
pub fn read_thumb(db: &Arc<Mutex<Database>>, filename: &str, source_mtime: i64) -> Option<Vec<u8>> {
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMBS_TABLE).ok()?;
    let guard = table.get(filename).ok()??;
    let (stored_mtime, jpeg) = guard.value();
    if stored_mtime != source_mtime {
        return None;
    }
    Some(jpeg.to_vec())
}

/// サムネをDBに書き込む。
pub fn write_thumb(db: &Arc<Mutex<Database>>, filename: &str, source_mtime: i64, jpeg: &[u8]) {
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
pub fn is_invalid_and_current(db: &Arc<Mutex<Database>>, filename: &str, archive_path: &Path) -> bool {
    let current_mtime = file_mtime(archive_path);
    let Ok(db) = db.lock() else { return false };
    let Ok(tx) = db.begin_read() else { return false };
    let Ok(table) = tx.open_table(INVALID_TABLE) else { return false };
    match table.get(filename) {
        Ok(Some(guard)) => guard.value() == current_mtime,
        _ => false,
    }
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
