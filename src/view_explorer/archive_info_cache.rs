//! リストの「ページ数」「詳細」列が引く、アーカイブ情報（ページ数・詳細有無。`neko_dir::ARCHIVE_META_TABLE`）のメモリキャッシュ。
//!
//! フォルダ単位で cache.redb の該当表を丸ごと読み、以降はメモリから引く。
//! お気に入り・検索・仮想フォルダなど複数フォルダのファイルが並ぶ表示でも、フォルダごとに1回だけ読む。
//! ビューアーを開いて記録が増えたら、そのフォルダの分を捨てて次の描画で読み直す。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::neko_dir::{self, ArchiveMetaRow};

use super::NekoviewApp;

/// フォルダ → （ファイル名 → 記録）。DBが無いフォルダは空の表を持つ（毎フレーム開き直さないため）。
#[derive(Default)]
pub(super) struct ArchiveInfoCache {
    dirs: RefCell<HashMap<PathBuf, HashMap<String, ArchiveMetaRow>>>,
}

impl ArchiveInfoCache {
    pub(super) fn invalidate_dir(&self, dir: &Path) {
        self.dirs.borrow_mut().remove(dir);
    }

    pub(super) fn clear(&self) {
        self.dirs.borrow_mut().clear();
    }

    /// `load` はキャッシュに無いフォルダの表を読む処理（DBが無ければ空を返す）。
    fn get_or_load(
        &self,
        dir: &Path,
        name: &str,
        load: impl FnOnce() -> HashMap<String, ArchiveMetaRow>,
    ) -> Option<ArchiveMetaRow> {
        if let Some(table) = self.dirs.borrow().get(dir) {
            return table.get(name).copied();
        }
        let table = load();
        let row = table.get(name).copied();
        self.dirs.borrow_mut().insert(dir.to_path_buf(), table);
        row
    }
}

impl NekoviewApp {
    /// ファイルの記録（ページ数・詳細有無）。記録が無ければ None。
    /// 現物と一致するか（`is_current`）は呼び出し側が、手持ちの mtime・size で確認する。
    pub(super) fn archive_info_of(&self, path: &Path) -> Option<ArchiveMetaRow> {
        let dir = path.parent()?;
        let name = path.file_name()?.to_str()?;
        self.archive_info.get_or_load(dir, name, || {
            let db = if dir == self.current_dir {
                self.cache_db.clone()
            } else {
                neko_dir::neko_dir_for(dir, &self.config)
                    .and_then(|nd| neko_dir::open_cache_db_if_exists(&nd, dir))
            };
            db.map(|db| neko_dir::read_all_archive_meta(&db)).unwrap_or_default()
        })
    }
}

impl NekoviewApp {
    /// 記録が現物と一致する（mtime・sizeが同じ）ときだけ返す。古い・無いときは None。
    pub(super) fn fresh_archive_info(
        &self,
        path: &Path,
        mtime: Option<std::time::SystemTime>,
        size: u64,
    ) -> Option<ArchiveMetaRow> {
        let secs = mtime?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        self.archive_info_of(path).filter(|r| r.is_current(secs, size))
    }

    /// ページ数（未取得・記録が古い場合は 0）。ソートはこれを使い、未取得を 0 として扱う。
    pub(super) fn fresh_page_count(&self, path: &Path, mtime: Option<std::time::SystemTime>, size: u64) -> u32 {
        self.fresh_archive_info(path, mtime, size).map_or(0, |r| r.page_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neko_dir::DetailFlag;

    fn row(pages: u32) -> ArchiveMetaRow {
        ArchiveMetaRow { mtime: 1, size: 2, page_count: pages, detail: DetailFlag::None }
    }

    #[test]
    fn loads_each_dir_once_and_serves_from_memory() {
        let cache = ArchiveInfoCache::default();
        let mut loads = 0;
        let dir = Path::new("/d");
        let mut table = HashMap::new();
        table.insert("a.zip".to_string(), row(10));
        let first = cache.get_or_load(dir, "a.zip", || {
            loads += 1;
            table.clone()
        });
        assert_eq!(first.map(|r| r.page_count), Some(10));
        let second = cache.get_or_load(dir, "missing.zip", || {
            loads += 1;
            HashMap::new()
        });
        assert!(second.is_none());
        assert_eq!(loads, 1, "同じフォルダは読み直さない");
    }

    #[test]
    fn invalidate_forces_reload_and_empty_dir_is_remembered() {
        let cache = ArchiveInfoCache::default();
        let dir = Path::new("/d");
        let mut loads = 0;
        assert!(cache.get_or_load(dir, "a.zip", || { loads += 1; HashMap::new() }).is_none());
        assert!(cache.get_or_load(dir, "a.zip", || { loads += 1; HashMap::new() }).is_none());
        assert_eq!(loads, 1, "DBが無いフォルダも空として覚える");
        cache.invalidate_dir(dir);
        let mut t = HashMap::new();
        t.insert("a.zip".to_string(), row(3));
        assert_eq!(cache.get_or_load(dir, "a.zip", || t).map(|r| r.page_count), Some(3));
    }
}
