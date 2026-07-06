use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

/// お気に入りフォルダ数の上限。
pub const MAX_FOLDERS: usize = 200;
/// お気に入りフォルダ名の文字数上限。
pub const MAX_NAME_CHARS: usize = 200;

/// キー = folder_id（u8、0-199）
/// 値 = (name, marker, color_rgba, order)
pub const FAVORITE_FOLDERS_TABLE: TableDefinition<u8, (&str, &str, u32, u8)> =
    TableDefinition::new("favorite_folders");

/// キー = "{正規化済みディレクトリ}\0{ファイル名}"
/// 値 = 所属folder_idのバイト列。空スライス = どのフォルダにも属さない
///      （未整理お気に入り＝先頭スティッキー表示のテンポラリお気に入り）
pub const FAVORITE_MEMBERSHIP_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("favorite_membership");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteFolder {
    pub id: u8,
    pub name: String,
    pub marker: String,
    pub color_rgba: u32,
    pub order: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FavoriteFolderError {
    NameEmpty,
    NameTooLong,
    NameConflict,
    LimitReached,
    NotFound,
    Db,
}

/// 既存の spread_state 用 DB（nekoviewer_spread.redb）にお気に入り用テーブルを
/// 追加する。テーブルが無ければ自動作成される（redb の性質上マイグレーション不要）。
pub fn init_favorite_tables(db: &Arc<Mutex<Database>>) -> Option<()> {
    let db = db.lock().ok()?;
    let tx = db.begin_write().ok()?;
    tx.open_table(FAVORITE_FOLDERS_TABLE).ok()?;
    tx.open_table(FAVORITE_MEMBERSHIP_TABLE).ok()?;
    tx.commit().ok()?;
    Some(())
}

fn make_key(dir: &Path, filename: &str) -> String {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("{}\0{}", key.to_string_lossy(), filename)
}

fn validate_name(name: &str) -> Result<(), FavoriteFolderError> {
    if name.is_empty() {
        return Err(FavoriteFolderError::NameEmpty);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(FavoriteFolderError::NameTooLong);
    }
    Ok(())
}

/// 定義済みお気に入りフォルダ一覧を order 昇順（同order時はid昇順）で返す。
pub fn list_folders(db: &Arc<Mutex<Database>>) -> Vec<FavoriteFolder> {
    let mut out = Vec::new();
    let Ok(db) = db.lock() else { return out };
    let Ok(tx) = db.begin_read() else { return out };
    let Ok(table) = tx.open_table(FAVORITE_FOLDERS_TABLE) else {
        return out;
    };
    let Ok(iter) = table.iter() else { return out };
    for entry in iter {
        let Ok((k, v)) = entry else { continue };
        let (name, marker, color_rgba, order) = v.value();
        out.push(FavoriteFolder {
            id: k.value(),
            name: name.to_string(),
            marker: marker.to_string(),
            color_rgba,
            order,
        });
    }
    out.sort_by_key(|f| (f.order, f.id));
    out
}

/// お気に入りフォルダを新規作成する。id・order は自動採番。
pub fn create_folder(
    db: &Arc<Mutex<Database>>,
    name: &str,
    marker: &str,
    color_rgba: u32,
) -> Result<FavoriteFolder, FavoriteFolderError> {
    validate_name(name)?;
    let Ok(db) = db.lock() else {
        return Err(FavoriteFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| FavoriteFolderError::Db)?;
    let new_id;
    let new_order;
    {
        let mut table = tx
            .open_table(FAVORITE_FOLDERS_TABLE)
            .map_err(|_| FavoriteFolderError::Db)?;
        let mut count = 0usize;
        let mut max_id: Option<u8> = None;
        let mut max_order: Option<u8> = None;
        {
            let iter = table.iter().map_err(|_| FavoriteFolderError::Db)?;
            for entry in iter {
                let Ok((k, v)) = entry else { continue };
                count += 1;
                let id = k.value();
                max_id = Some(max_id.map_or(id, |m| m.max(id)));
                let (existing_name, _, _, order) = v.value();
                if existing_name == name {
                    return Err(FavoriteFolderError::NameConflict);
                }
                max_order = Some(max_order.map_or(order, |m| m.max(order)));
            }
        }
        if count >= MAX_FOLDERS {
            return Err(FavoriteFolderError::LimitReached);
        }
        new_id = match max_id {
            Some(m) => m.checked_add(1).ok_or(FavoriteFolderError::LimitReached)?,
            None => 0,
        };
        new_order = match max_order {
            Some(m) => m.saturating_add(1),
            None => 0,
        };
        table
            .insert(new_id, (name, marker, color_rgba, new_order))
            .map_err(|_| FavoriteFolderError::Db)?;
    }
    tx.commit().map_err(|_| FavoriteFolderError::Db)?;
    Ok(FavoriteFolder {
        id: new_id,
        name: name.to_string(),
        marker: marker.to_string(),
        color_rgba,
        order: new_order,
    })
}

/// お気に入りフォルダ名をリネームする（衝突チェックあり、自分自身は除外）。
pub fn rename_folder(
    db: &Arc<Mutex<Database>>,
    id: u8,
    new_name: &str,
) -> Result<(), FavoriteFolderError> {
    validate_name(new_name)?;
    let Ok(db) = db.lock() else {
        return Err(FavoriteFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| FavoriteFolderError::Db)?;
    {
        let mut table = tx
            .open_table(FAVORITE_FOLDERS_TABLE)
            .map_err(|_| FavoriteFolderError::Db)?;
        let current = {
            let iter = table.iter().map_err(|_| FavoriteFolderError::Db)?;
            let mut found = None;
            for entry in iter {
                let Ok((k, v)) = entry else { continue };
                let (existing_name, marker, color_rgba, order) = v.value();
                if k.value() == id {
                    found = Some((marker.to_string(), color_rgba, order));
                } else if existing_name == new_name {
                    return Err(FavoriteFolderError::NameConflict);
                }
            }
            found
        };
        let Some((marker, color_rgba, order)) = current else {
            return Err(FavoriteFolderError::NotFound);
        };
        table
            .insert(id, (new_name, marker.as_str(), color_rgba, order))
            .map_err(|_| FavoriteFolderError::Db)?;
    }
    tx.commit().map_err(|_| FavoriteFolderError::Db)?;
    Ok(())
}

/// お気に入りフォルダのマーカー記号・色を設定する。
pub fn set_marker(
    db: &Arc<Mutex<Database>>,
    id: u8,
    marker: &str,
    color_rgba: u32,
) -> Result<(), FavoriteFolderError> {
    let Ok(db) = db.lock() else {
        return Err(FavoriteFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| FavoriteFolderError::Db)?;
    {
        let mut table = tx
            .open_table(FAVORITE_FOLDERS_TABLE)
            .map_err(|_| FavoriteFolderError::Db)?;
        let current_name = {
            let iter = table.iter().map_err(|_| FavoriteFolderError::Db)?;
            let mut found = None;
            for entry in iter {
                let Ok((k, v)) = entry else { continue };
                if k.value() == id {
                    let (name, _, _, order) = v.value();
                    found = Some((name.to_string(), order));
                    break;
                }
            }
            found
        };
        let Some((name, order)) = current_name else {
            return Err(FavoriteFolderError::NotFound);
        };
        table
            .insert(id, (name.as_str(), marker, color_rgba, order))
            .map_err(|_| FavoriteFolderError::Db)?;
    }
    tx.commit().map_err(|_| FavoriteFolderError::Db)?;
    Ok(())
}

/// お気に入りフォルダを削除する。全ファイルの所属情報からも当該IDを取り除く。
pub fn delete_folder(db: &Arc<Mutex<Database>>, id: u8) -> Result<(), FavoriteFolderError> {
    let Ok(db) = db.lock() else {
        return Err(FavoriteFolderError::Db);
    };
    let tx = db.begin_write().map_err(|_| FavoriteFolderError::Db)?;
    {
        let mut table = tx
            .open_table(FAVORITE_FOLDERS_TABLE)
            .map_err(|_| FavoriteFolderError::Db)?;
        if table.remove(id).map_err(|_| FavoriteFolderError::Db)?.is_none() {
            return Err(FavoriteFolderError::NotFound);
        }
    }
    {
        let mut table = tx
            .open_table(FAVORITE_MEMBERSHIP_TABLE)
            .map_err(|_| FavoriteFolderError::Db)?;
        let stale: Vec<(String, Vec<u8>)> = {
            let iter = table.iter().map_err(|_| FavoriteFolderError::Db)?;
            let mut out = Vec::new();
            for entry in iter {
                let Ok((k, v)) = entry else { continue };
                if v.value().contains(&id) {
                    let remaining: Vec<u8> =
                        v.value().iter().copied().filter(|&f| f != id).collect();
                    out.push((k.value().to_string(), remaining));
                }
            }
            out
        };
        for (key, remaining) in stale {
            table
                .insert(key.as_str(), remaining.as_slice())
                .map_err(|_| FavoriteFolderError::Db)?;
        }
    }
    tx.commit().map_err(|_| FavoriteFolderError::Db)?;
    Ok(())
}

/// ファイルの所属お気に入りフォルダID一覧を返す。None = お気に入りではない。
/// Some(空Vec) = 未整理のお気に入り（テンポラリ・スティッキー表示）。
pub fn get_membership(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) -> Option<Vec<u8>> {
    let key = make_key(dir, filename);
    let db = db.lock().ok()?;
    let tx = db.begin_read().ok()?;
    let table = tx.open_table(FAVORITE_MEMBERSHIP_TABLE).ok()?;
    let value = table.get(key.as_str()).ok()??;
    Some(value.value().to_vec())
}

/// ファイルの所属お気に入りフォルダIDを設定する（全置換）。空Vecなら未整理として登録。
pub fn set_membership(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str, folder_ids: &[u8]) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) {
        let _ = table.insert(key.as_str(), folder_ids);
    }
    let _ = tx.commit();
}

/// お気に入り登録を完全に解除する（未整理状態も含めて削除）。
pub fn remove_favorite(db: &Arc<Mutex<Database>>, dir: &Path, filename: &str) {
    let key = make_key(dir, filename);
    let Ok(db) = db.lock() else { return };
    let Ok(tx) = db.begin_write() else { return };
    if let Ok(mut table) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) {
        let _ = table.remove(key.as_str());
    }
    let _ = tx.commit();
}

/// dir 配下で登録済みのお気に入りファイル一覧を返す（GC・サムネ表示用）。
/// 戻り値: (filename, 所属folder_id一覧)
pub fn list_dir_favorites(db: &Arc<Mutex<Database>>, dir: &Path) -> Vec<(String, Vec<u8>)> {
    let prefix = {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        format!("{}\0", key.to_string_lossy())
    };
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) else {
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
        let filename = &full_key[prefix.len()..];
        out.push((filename.to_string(), v.value().to_vec()));
    }
    out
}

/// 全ディレクトリ横断でのお気に入りエントリ一覧（フルテーブルスキャン）。
/// お気に入り一覧表示（エクスプローラー部でフォルダ/未整理を選んだ時）専用。
fn list_all_favorites(db: &Arc<Mutex<Database>>) -> Vec<(PathBuf, String, Vec<u8>)> {
    let Ok(db) = db.lock() else { return Vec::new() };
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(FAVORITE_MEMBERSHIP_TABLE) else {
        return Vec::new();
    };
    let Ok(iter) = table.iter() else { return Vec::new() };
    let mut out = Vec::new();
    for entry in iter {
        let Ok((k, v)) = entry else { continue };
        let key = k.value();
        let Some(pos) = key.find('\0') else { continue };
        out.push((
            PathBuf::from(&key[..pos]),
            key[pos + 1..].to_string(),
            v.value().to_vec(),
        ));
    }
    out
}

/// 指定お気に入りフォルダに所属するファイルを全ディレクトリ横断で列挙する。
pub fn list_files_in_folder(db: &Arc<Mutex<Database>>, folder_id: u8) -> Vec<(PathBuf, String)> {
    list_all_favorites(db)
        .into_iter()
        .filter(|(_, _, ids)| ids.contains(&folder_id))
        .map(|(dir, name, _)| (dir, name))
        .collect()
}

/// 未整理のお気に入り（どのフォルダにも属さない）ファイルを全ディレクトリ横断で列挙する。
pub fn list_unsorted_files(db: &Arc<Mutex<Database>>) -> Vec<(PathBuf, String)> {
    list_all_favorites(db)
        .into_iter()
        .filter(|(_, _, ids)| ids.is_empty())
        .map(|(dir, name, _)| (dir, name))
        .collect()
}

/// dir 配下で existing_filenames に存在しないエントリを削除する（GC）。削除件数を返す。
pub fn gc_dir(db: &Arc<Mutex<Database>>, dir: &Path, existing_filenames: &[String]) -> usize {
    let stale: Vec<String> = list_dir_favorites(db, dir)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !existing_filenames.contains(name))
        .collect();
    for name in &stale {
        remove_favorite(db, dir, name);
    }
    stale.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> Arc<Mutex<Database>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nekoviewer_favorites_test_{}_{}.redb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = Database::create(&path).unwrap();
        let db = Arc::new(Mutex::new(db));
        init_favorite_tables(&db).unwrap();
        db
    }

    fn dummy_dir() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn create_and_list_folder() {
        let db = temp_db();
        let f = create_folder(&db, "本棚", "★", 0xFFCC00FF).unwrap();
        assert_eq!(f.id, 0);
        assert_eq!(f.order, 0);
        let folders = list_folders(&db);
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "本棚");
    }

    #[test]
    fn name_conflict_on_create() {
        let db = temp_db();
        create_folder(&db, "本棚", "★", 0).unwrap();
        let err = create_folder(&db, "本棚", "☆", 0).unwrap_err();
        assert_eq!(err, FavoriteFolderError::NameConflict);
    }

    #[test]
    fn name_too_long_rejected() {
        let db = temp_db();
        let long_name: String = "あ".repeat(MAX_NAME_CHARS + 1);
        let err = create_folder(&db, &long_name, "★", 0).unwrap_err();
        assert_eq!(err, FavoriteFolderError::NameTooLong);
    }

    #[test]
    fn folder_limit_enforced() {
        let db = temp_db();
        for i in 0..MAX_FOLDERS {
            create_folder(&db, &format!("f{i}"), "★", 0).unwrap();
        }
        let err = create_folder(&db, "overflow", "★", 0).unwrap_err();
        assert_eq!(err, FavoriteFolderError::LimitReached);
    }

    #[test]
    fn rename_folder_conflict_and_success() {
        let db = temp_db();
        let a = create_folder(&db, "A", "★", 0).unwrap();
        create_folder(&db, "B", "★", 0).unwrap();
        assert_eq!(
            rename_folder(&db, a.id, "B").unwrap_err(),
            FavoriteFolderError::NameConflict
        );
        rename_folder(&db, a.id, "A2").unwrap();
        let folders = list_folders(&db);
        assert!(folders.iter().any(|f| f.name == "A2"));
    }

    #[test]
    fn set_marker_updates_symbol_and_color() {
        let db = temp_db();
        let a = create_folder(&db, "A", "★", 0).unwrap();
        set_marker(&db, a.id, "♪", 0x00FF00FF).unwrap();
        let folders = list_folders(&db);
        assert_eq!(folders[0].marker, "♪");
        assert_eq!(folders[0].color_rgba, 0x00FF00FF);
    }

    #[test]
    fn membership_roundtrip_and_unsorted() {
        let db = temp_db();
        let dir = dummy_dir();
        assert_eq!(get_membership(&db, &dir, "a.zip"), None);

        set_membership(&db, &dir, "a.zip", &[]);
        assert_eq!(get_membership(&db, &dir, "a.zip"), Some(vec![]));

        let f1 = create_folder(&db, "F1", "★", 0).unwrap();
        set_membership(&db, &dir, "a.zip", &[f1.id]);
        assert_eq!(get_membership(&db, &dir, "a.zip"), Some(vec![f1.id]));

        remove_favorite(&db, &dir, "a.zip");
        assert_eq!(get_membership(&db, &dir, "a.zip"), None);
    }

    #[test]
    fn delete_folder_strips_membership() {
        let db = temp_db();
        let dir = dummy_dir();
        let f1 = create_folder(&db, "F1", "★", 0).unwrap();
        let f2 = create_folder(&db, "F2", "★", 0).unwrap();
        set_membership(&db, &dir, "a.zip", &[f1.id, f2.id]);
        delete_folder(&db, f1.id).unwrap();
        assert_eq!(get_membership(&db, &dir, "a.zip"), Some(vec![f2.id]));
        assert!(list_folders(&db).iter().all(|f| f.id != f1.id));
    }

    #[test]
    fn list_files_in_folder_spans_directories() {
        let db = temp_db();
        let dir_a = PathBuf::from("/tmp/nekoviewer_fav_test_a");
        let dir_b = PathBuf::from("/tmp/nekoviewer_fav_test_b");
        let f1 = create_folder(&db, "F1", "★", 0).unwrap();
        set_membership(&db, &dir_a, "a.zip", &[f1.id]);
        set_membership(&db, &dir_b, "b.zip", &[f1.id]);
        set_membership(&db, &dir_a, "unsorted.zip", &[]);

        let mut in_folder = list_files_in_folder(&db, f1.id);
        in_folder.sort();
        assert_eq!(
            in_folder,
            vec![
                (dir_a.canonicalize().unwrap_or(dir_a.clone()), "a.zip".to_string()),
                (dir_b.canonicalize().unwrap_or(dir_b.clone()), "b.zip".to_string()),
            ]
        );

        let unsorted = list_unsorted_files(&db);
        assert_eq!(
            unsorted,
            vec![(dir_a.canonicalize().unwrap_or(dir_a), "unsorted.zip".to_string())]
        );
    }

    #[test]
    fn gc_dir_removes_stale_entries() {
        let db = temp_db();
        let dir = dummy_dir();
        set_membership(&db, &dir, "a.zip", &[]);
        set_membership(&db, &dir, "b.zip", &[]);
        let removed = gc_dir(&db, &dir, &["a.zip".to_string()]);
        assert_eq!(removed, 1);
        assert_eq!(get_membership(&db, &dir, "b.zip"), None);
        assert_eq!(get_membership(&db, &dir, "a.zip"), Some(vec![]));
    }
}
