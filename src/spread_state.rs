use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, TableDefinition};

use crate::types::{PageMode, ReaderSortKey};

/// キー = "{正規化済みディレクトリ}\0{ファイル名}"
/// 値 = (page_mode: u8, spread_offset: i32)
/// 復帰は常にファイル先頭固定。spread_offset は「先頭から見開きを組んだときの
/// ズレ状態」(-1/0/+1) で、絶対ページ位置は保存しない。
pub const SPREAD_TABLE: TableDefinition<&str, (u8, i32)> = TableDefinition::new("spread_state");

/// アーカイブ単位のソート条件保存テーブル（第1世代）。
///
/// キーは SPREAD_TABLE と同じ「正規化済みディレクトリ\0ファイル名」。
/// 値は (sort_key: u8, ascending: bool)。レコード不在が保存OFFを表す。
/// 値形式を将来変更する場合はこの定義を変更せず、`archive_sort_state_v2` のような
/// 新しいテーブルを追加して移行すること。
pub const ARCHIVE_SORT_TABLE_V1: TableDefinition<&str, (u8, bool)> =
    TableDefinition::new("archive_sort_state_v1");

/// アーカイブ単位の登録サムネイルページ。値は表示順に依存しないentry_name。
pub const THUMBNAIL_SELECTION_TABLE_V1: TableDefinition<&str, &str> =
    TableDefinition::new("thumbnail_selection_v1");

/// サムネイル上の保存設定表示に必要な、アーカイブ単位の状態。
#[derive(Clone, Copy, PartialEq, Default)]
pub struct SavedArchiveSettings {
    pub spread_mode: Option<PageMode>,
    pub has_saved_sort: bool,
    pub has_custom_thumbnail: bool,
}

/// root（config.rsが解決したconf置き場所）の nekoviewer_spread.redb を開く。
/// 失敗時は None（保存機能自体を無効化）。
pub fn open_spread_db(root: &Path) -> Option<Arc<Mutex<Database>>> {
    let _ = std::fs::create_dir_all(root);
    let db_path = root.join("nekoviewer_spread.redb");
    let db = Database::create(&db_path).ok()?;
    {
        let tx = db.begin_write().ok()?;
        tx.open_table(SPREAD_TABLE).ok()?;
        tx.open_table(ARCHIVE_SORT_TABLE_V1).ok()?;
        tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok()?;
        tx.commit().ok()?;
    }
    Some(Arc::new(Mutex::new(db)))
}

pub fn write_thumbnail_selection(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    entry_name: &str,
) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        let _ = table.insert(key.as_str(), entry_name);
    }
    let _ = tx.commit();
}

pub fn read_thumbnail_selection(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<String> {
    let key = make_key(dir, filename);
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok()?;
    Some(table.get(key.as_str()).ok()??.value().to_string())
}

pub fn remove_thumbnail_selection(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}

fn make_key(dir: &Path, filename: &str) -> String {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("{}\0{}", key.to_string_lossy(), filename)
}

/// 複数ディレクトリを横断する一覧向けに、3種類の保存設定を一括取得する。
/// いずれの設定もないパスは戻り値へ含めない。
pub fn saved_settings_for_paths(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
) -> HashMap<PathBuf, SavedArchiveSettings> {
    let Ok(db) = db.lock() else {
        return HashMap::new();
    };
    let Ok(tx) = db.begin_read() else {
        return HashMap::new();
    };
    let spread_table = tx.open_table(SPREAD_TABLE).ok();
    let sort_table = tx.open_table(ARCHIVE_SORT_TABLE_V1).ok();
    let thumbnail_table = tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).ok();
    let mut out = HashMap::new();

    for path in paths {
        let Some(dir) = path.parent() else { continue };
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let key = make_key(dir, filename);
        let spread_mode = spread_table.as_ref().and_then(|table| {
            let value = table.get(key.as_str()).ok()??;
            page_mode_from_u8(value.value().0)
        });
        let has_saved_sort = sort_table.as_ref().is_some_and(|table| {
            table.get(key.as_str()).ok().flatten()
                .and_then(|value| reader_sort_key_from_u8(value.value().0))
                .is_some()
        });
        let has_custom_thumbnail = thumbnail_table.as_ref().is_some_and(|table| {
            table.get(key.as_str()).ok().flatten().is_some()
        });
        let settings = SavedArchiveSettings {
            spread_mode,
            has_saved_sort,
            has_custom_thumbnail,
        };
        if settings != SavedArchiveSettings::default() {
            out.insert(path.clone(), settings);
        }
    }
    out
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

/// archive_sort_state_v1 に保存する値。割り当てはリリース後に変更しないこと。
pub fn reader_sort_key_to_u8(key: ReaderSortKey) -> u8 {
    match key {
        ReaderSortKey::Name => 0,
        ReaderSortKey::Natural => 1,
        ReaderSortKey::Date => 2,
    }
}

/// 未知の値は、将来世代や破損データを既定値と誤認しないよう未保存扱いにする。
pub fn reader_sort_key_from_u8(v: u8) -> Option<ReaderSortKey> {
    match v {
        0 => Some(ReaderSortKey::Name),
        1 => Some(ReaderSortKey::Natural),
        2 => Some(ReaderSortKey::Date),
        _ => None,
    }
}

/// アーカイブのソート条件を保存する（上書き）。
pub fn write_archive_sort(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
    key: ReaderSortKey,
    ascending: bool,
) {
    let db_key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        let _ = table.insert(db_key.as_str(), (reader_sort_key_to_u8(key), ascending));
    }
    let _ = tx.commit();
}

/// アーカイブの保存済みソート条件を返す。レコード不在・未知値は None。
pub fn read_archive_sort(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<(ReaderSortKey, bool)> {
    let db_key = make_key(dir, filename);
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(ARCHIVE_SORT_TABLE_V1).ok()?;
    let value = table.get(db_key.as_str()).ok()??;
    let (key_raw, ascending) = value.value();
    Some((reader_sort_key_from_u8(key_raw)?, ascending))
}

/// アーカイブのソート条件保存を解除する。
pub fn remove_archive_sort(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let db_key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(ARCHIVE_SORT_TABLE_V1) {
        let _ = table.remove(db_key.as_str());
    }
    let _ = tx.commit();
}

/// dir 配下の有効なソート保存値を列挙する。
pub fn list_dir_archive_sorts(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
) -> Vec<(String, ReaderSortKey, bool)> {
    let prefix = {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        format!("{}\0", key.to_string_lossy())
    };
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else {
        return Vec::new();
    };
    let Ok(table) = tx.open_table(ARCHIVE_SORT_TABLE_V1) else {
        return Vec::new();
    };
    let Ok(range) = table.range(prefix.as_str()..) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in range {
        let Ok((k, v)) = entry else { continue };
        let full_key = k.value();
        if !full_key.starts_with(&prefix) {
            break;
        }
        let (key_raw, ascending) = v.value();
        let Some(sort_key) = reader_sort_key_from_u8(key_raw) else {
            continue;
        };
        out.push((full_key[prefix.len()..].to_string(), sort_key, ascending));
    }
    out
}

/// dir 配下で存在しなくなったアーカイブのソート保存値を削除する。
pub fn gc_archive_sorts(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    existing_filenames: &[String],
) -> usize {
    let stale: Vec<String> = list_dir_archive_sorts(db, dir)
        .into_iter()
        .map(|(name, _, _)| name)
        .filter(|name| !existing_filenames.contains(name))
        .collect();
    for name in &stale {
        remove_archive_sort(db, dir, name);
    }
    stale.len()
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

/// 保存済みの見開き状態を返す。レコード不在・未知値は None。
pub fn read_spread(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    filename: &str,
) -> Option<(PageMode, i32)> {
    let key = make_key(dir, filename);
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(SPREAD_TABLE).ok()?;
    let value = table.get(key.as_str()).ok()??;
    let (mode_raw, offset) = value.value();
    Some((page_mode_from_u8(mode_raw)?, offset))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_temp_path(suffix: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "nekoviewer_sort_state_test_{}_{}_{}",
            std::process::id(),
            nonce,
            suffix
        ))
    }

    fn temp_db() -> Arc<Mutex<Database>> {
        let path = unique_temp_path("db.redb");
        let db = Database::create(path).unwrap();
        {
            let tx = db.begin_write().unwrap();
            tx.open_table(SPREAD_TABLE).unwrap();
            tx.open_table(ARCHIVE_SORT_TABLE_V1).unwrap();
            tx.open_table(THUMBNAIL_SELECTION_TABLE_V1).unwrap();
            tx.commit().unwrap();
        }
        Arc::new(Mutex::new(db))
    }

    fn dummy_dir() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn archive_sort_key_encoding_is_stable() {
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Name), 0);
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Natural), 1);
        assert_eq!(reader_sort_key_to_u8(ReaderSortKey::Date), 2);
        assert!(matches!(
            reader_sort_key_from_u8(0),
            Some(ReaderSortKey::Name)
        ));
        assert!(matches!(
            reader_sort_key_from_u8(1),
            Some(ReaderSortKey::Natural)
        ));
        assert!(matches!(
            reader_sort_key_from_u8(2),
            Some(ReaderSortKey::Date)
        ));
        assert!(reader_sort_key_from_u8(3).is_none());
        assert!(reader_sort_key_from_u8(u8::MAX).is_none());
    }

    #[test]
    fn spread_records_are_read_by_actual_parent_directory() {
        let db = temp_db();
        let dir = unique_temp_path("spread_dir");
        let other_dir = unique_temp_path("other_spread_dir");

        write_spread(&db, &dir, "book.zip", PageMode::SpreadLeft, 1);
        write_spread(&db, &other_dir, "book.zip", PageMode::SpreadRight, -1);

        let first = read_spread(&db, &dir, "book.zip").unwrap();
        assert!(matches!(first.0, PageMode::SpreadLeft));
        assert_eq!(first.1, 1);

        let other = read_spread(&db, &other_dir, "book.zip").unwrap();
        assert!(matches!(other.0, PageMode::SpreadRight));
        assert_eq!(other.1, -1);

        remove_spread(&db, &dir, "book.zip");
        assert!(read_spread(&db, &dir, "book.zip").is_none());
        assert!(read_spread(&db, &other_dir, "book.zip").is_some());
    }

    #[test]
    fn saved_settings_for_paths_combines_flags_and_keeps_full_paths() {
        let db = temp_db();
        let dir_a = unique_temp_path("settings_dir_a");
        let dir_b = unique_temp_path("settings_dir_b");
        let path_a = dir_a.join("same.zip");
        let path_b = dir_b.join("same.zip");
        let plain = dir_a.join("plain.zip");

        write_spread(&db, &dir_a, "same.zip", PageMode::SpreadLeft, 1);
        write_archive_sort(&db, &dir_a, "same.zip", ReaderSortKey::Natural, false);
        write_thumbnail_selection(&db, &dir_a, "same.zip", "003.jpg");
        write_spread(&db, &dir_b, "same.zip", PageMode::SpreadRight, -1);

        let settings =
            saved_settings_for_paths(&db, &[path_a.clone(), path_b.clone(), plain.clone()]);
        let settings_a = settings.get(&path_a).unwrap();
        assert!(matches!(settings_a.spread_mode, Some(PageMode::SpreadLeft)));
        assert!(settings_a.has_saved_sort);
        assert!(settings_a.has_custom_thumbnail);

        let settings_b = settings.get(&path_b).unwrap();
        assert!(matches!(settings_b.spread_mode, Some(PageMode::SpreadRight)));
        assert!(!settings_b.has_saved_sort);
        assert!(!settings_b.has_custom_thumbnail);
        assert!(!settings.contains_key(&plain));
    }

    #[test]
    fn archive_sort_roundtrip_overwrite_and_remove() {
        let db = temp_db();
        let dir = dummy_dir();
        assert!(read_archive_sort(&db, &dir, "a.zip").is_none());

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Natural, false);
        let value = read_archive_sort(&db, &dir, "a.zip").unwrap();
        assert!(matches!(value.0, ReaderSortKey::Natural));
        assert!(!value.1);

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Date, true);
        let value = read_archive_sort(&db, &dir, "a.zip").unwrap();
        assert!(matches!(value.0, ReaderSortKey::Date));
        assert!(value.1);

        remove_archive_sort(&db, &dir, "a.zip");
        assert!(read_archive_sort(&db, &dir, "a.zip").is_none());
    }

    #[test]
    fn thumbnail_selection_roundtrip_replace_and_remove() {
        let db = temp_db();
        let dir = dummy_dir();
        assert!(read_thumbnail_selection(&db, &dir, "book.zip").is_none());

        write_thumbnail_selection(&db, &dir, "book.zip", "pages/001.jpg");
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "book.zip").as_deref(),
            Some("pages/001.jpg")
        );

        write_thumbnail_selection(&db, &dir, "book.zip", "pages/cover.png");
        assert_eq!(
            read_thumbnail_selection(&db, &dir, "book.zip").as_deref(),
            Some("pages/cover.png")
        );

        remove_thumbnail_selection(&db, &dir, "book.zip");
        assert!(read_thumbnail_selection(&db, &dir, "book.zip").is_none());
    }

    #[test]
    fn archive_sort_records_are_independent_and_listed_by_directory() {
        let db = temp_db();
        let dir = dummy_dir();
        let other_dir = unique_temp_path("other_dir");

        write_archive_sort(&db, &dir, "a.zip", ReaderSortKey::Name, true);
        write_archive_sort(&db, &dir, "b.zip", ReaderSortKey::Date, false);
        write_archive_sort(&db, &other_dir, "a.zip", ReaderSortKey::Natural, false);

        let mut values = list_dir_archive_sorts(&db, &dir);
        values.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].0, "a.zip");
        assert!(matches!(values[0].1, ReaderSortKey::Name));
        assert!(values[0].2);
        assert_eq!(values[1].0, "b.zip");
        assert!(matches!(values[1].1, ReaderSortKey::Date));
        assert!(!values[1].2);

        let other = read_archive_sort(&db, &other_dir, "a.zip").unwrap();
        assert!(matches!(other.0, ReaderSortKey::Natural));
        assert!(!other.1);
    }

    #[test]
    fn unknown_archive_sort_key_is_treated_as_unsaved() {
        let db = temp_db();
        let dir = dummy_dir();
        let db_key = make_key(&dir, "unknown.zip");
        {
            let db_guard = db.lock().unwrap();
            let tx = db_guard.begin_write().unwrap();
            {
                let mut table = tx.open_table(ARCHIVE_SORT_TABLE_V1).unwrap();
                table.insert(db_key.as_str(), (99, false)).unwrap();
            }
            tx.commit().unwrap();
        }

        assert!(read_archive_sort(&db, &dir, "unknown.zip").is_none());
        assert!(list_dir_archive_sorts(&db, &dir).is_empty());
    }

    #[test]
    fn archive_sort_gc_only_removes_missing_files() {
        let db = temp_db();
        let dir = dummy_dir();
        write_archive_sort(&db, &dir, "keep.zip", ReaderSortKey::Name, false);
        write_archive_sort(&db, &dir, "stale.zip", ReaderSortKey::Date, true);

        assert_eq!(gc_archive_sorts(&db, &dir, &["keep.zip".to_string()]), 1);
        assert!(read_archive_sort(&db, &dir, "keep.zip").is_some());
        assert!(read_archive_sort(&db, &dir, "stale.zip").is_none());
    }

    #[test]
    fn opening_legacy_spread_only_db_adds_v1_sort_table_without_data_loss() {
        let root = unique_temp_path("legacy_root");
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("nekoviewer_spread.redb");
        {
            let db = Database::create(&db_path).unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut table = tx.open_table(SPREAD_TABLE).unwrap();
                let key = make_key(&root, "legacy.zip");
                table
                    .insert(key.as_str(), (page_mode_to_u8(PageMode::SpreadLeft), 1))
                    .unwrap();
            }
            tx.commit().unwrap();
        }

        let db = open_spread_db(&root).unwrap();
        let spreads = list_dir_entries(&db, &root);
        assert_eq!(spreads.len(), 1);
        assert_eq!(spreads[0].0, "legacy.zip");
        assert!(matches!(spreads[0].1, PageMode::SpreadLeft));
        assert_eq!(spreads[0].2, 1);
        assert!(list_dir_archive_sorts(&db, &root).is_empty());
    }
}
