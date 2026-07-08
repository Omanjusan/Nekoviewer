use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

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
        tx.open_table(THUMBS_TABLE).ok()?;
        tx.open_table(INVALID_TABLE).ok()?;
        tx.commit().ok()?;
    }
    let db = Arc::new(Mutex::new(db));
    registry.insert(db_path, Arc::clone(&db));
    Some(db)
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
