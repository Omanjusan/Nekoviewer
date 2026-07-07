use std::path::Path;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, TableDefinition};

use crate::types::PageMode;

/// キー = "{正規化済みディレクトリ}\0{ファイル名}"
/// 値 = (page_mode: u8, spread_offset: i32)
/// 復帰は常にファイル先頭固定。spread_offset は「先頭から見開きを組んだときの
/// ズレ状態」(-1/0/+1) で、絶対ページ位置は保存しない。
pub const SPREAD_TABLE: TableDefinition<&str, (u8, i32)> = TableDefinition::new("spread_state");

/// exe横の spread_state.redb を開く。失敗時は None（保存機能自体を無効化）。
pub fn open_spread_db() -> Option<Arc<Mutex<Database>>> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let db_path = exe_dir.join("nekoviewer_spread.redb");
    let db = Database::create(&db_path).ok()?;
    {
        let tx = db.begin_write().ok()?;
        tx.open_table(SPREAD_TABLE).ok()?;
        tx.commit().ok()?;
    }
    Some(Arc::new(Mutex::new(db)))
}

fn make_key(dir: &Path, filename: &str) -> String {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("{}\0{}", key.to_string_lossy(), filename)
}

pub fn page_mode_to_u8(mode: PageMode) -> u8 {
    match mode {
        PageMode::Single => 0,
        PageMode::SpreadLeft => 1,
        PageMode::SpreadRight => 2,
    }
}

pub fn page_mode_from_u8(v: u8) -> Option<PageMode> {
    match v {
        1 => Some(PageMode::SpreadLeft),
        2 => Some(PageMode::SpreadRight),
        _ => None,
    }
}

/// 見開き状態を保存する（上書き）。
pub fn write_spread(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, mode: PageMode, offset: i32) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(SPREAD_TABLE) {
        let _ = table.insert(key.as_str(), (page_mode_to_u8(mode), offset));
    }
    let _ = tx.commit();
}

/// 見開き状態を削除する（保存解除）。
pub fn remove_spread(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(SPREAD_TABLE) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}

/// dir 配下で保存済みのファイル名一覧を返す（GC・入場時ロード用）。
/// 戻り値: (filename, page_mode, spread_offset)
pub fn list_dir_entries(db: &Arc<Mutex<Database>>, dir: &Path) -> Vec<(String, PageMode, i32)> {
    let prefix = {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        format!("{}\0", key.to_string_lossy())
    };
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(SPREAD_TABLE) else { return Vec::new() };
    let Ok(range) = table.range(prefix.as_str()..) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in range {
        let Ok((k, v)) = entry else { continue };
        let full_key = k.value();
        if !full_key.starts_with(&prefix) {
            break;
        }
        let filename = &full_key[prefix.len()..];
        let (mode_raw, offset) = v.value();
        if let Some(mode) = page_mode_from_u8(mode_raw) {
            out.push((filename.to_string(), mode, offset));
        }
    }
    out
}

/// dir 配下で existing_filenames に存在しないエントリを削除する（GC）。削除件数を返す。
pub fn gc_dir(db: &Arc<Mutex<Database>>, dir: &Path, existing_filenames: &[String]) -> usize {
    let stale: Vec<String> = list_dir_entries(db, dir)
        .into_iter()
        .map(|(name, _, _)| name)
        .filter(|name| !existing_filenames.contains(name))
        .collect();
    for name in &stale {
        remove_spread(db, dir, name);
    }
    stale.len()
}
