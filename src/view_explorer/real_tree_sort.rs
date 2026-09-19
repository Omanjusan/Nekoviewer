//! 実ツリーの並び条件（ソート条件設定）の適用。実ツリータブ・仮想タブ内の実ツリーペイン・検索タブ内のツリーが
//! 共有する `tree_children` の各階層を、名前または更新日付で並べる（登録ピッカーの実ツリーは対象外）。
//!
//! 更新日付は、子フォルダの更新日時を別スレッドで調べてキャッシュ（`tree_mtimes`）する。UIスレッドでは
//! I/Oしない。ネットワークマウント配下は調べず、日付順では末尾になる。日時が届いたら全階層を並べ直す。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::SystemTime;

use crate::tree_sort::{TreeSort, TreeSortKey};
use crate::types::ExplorerSortKey;

use super::*;

/// 更新日時のキャッシュ。`None` は「調べたが不明（取得できない・ネットワーク配下）」。
pub(super) type TreeMtimes = HashMap<PathBuf, Option<SystemTime>>;

/// 1階層ぶんの子を並べる。日付の不明は昇降どちらでも末尾（名前の昇順）。
pub(super) fn sort_children(children: Vec<PathBuf>, sort: TreeSort, mtimes: &TreeMtimes) -> Vec<PathBuf> {
    let key = match sort.key {
        TreeSortKey::Date => ExplorerSortKey::Date,
        // 実ツリーに「登録順」は無い（名前として扱う）
        TreeSortKey::Name | TreeSortKey::Registration => ExplorerSortKey::Name,
    };
    super::folder_sort::sort_folders(children, key, sort.ascending, |p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        (name, mtimes.get(p).copied().flatten())
    })
}

impl NekoviewApp {
    /// 子の一覧を、現在の並び条件で並べて `tree_children` に入れる（実ツリーの子の入れ口はここに集約）。
    pub(super) fn insert_tree_children(&mut self, path: PathBuf, children: Vec<PathBuf>) {
        let sort = self.tree_sorts.real_tree_or_default();
        self.tree_children.insert(path, sort_children(children, sort, &self.tree_mtimes));
        self.request_missing_tree_mtimes();
    }

    /// 全階層を、現在の並び条件で並べ直す（設定変更時・更新日時が届いたとき）。
    pub(super) fn resort_real_tree(&mut self) {
        let sort = self.tree_sorts.real_tree_or_default();
        let all = std::mem::take(&mut self.tree_children);
        self.tree_children = all
            .into_iter()
            .map(|(path, children)| (path, sort_children(children, sort, &self.tree_mtimes)))
            .collect();
        self.request_missing_tree_mtimes();
    }

    /// 日付順のとき、更新日時が未確認の子フォルダを別スレッドで調べる（1本ずつ。届いたらまた足りなければ続ける）。
    fn request_missing_tree_mtimes(&mut self) {
        if self.tree_sorts.real_tree_or_default().key != TreeSortKey::Date || self.tree_mtimes_rx.is_some() {
            return;
        }
        let mut missing: Vec<PathBuf> = Vec::new();
        for child in self.tree_children.values().flatten() {
            if self.tree_mtimes.contains_key(child) {
                continue;
            }
            if self.network_mount_root_cached(child).is_some() {
                // ネットワーク配下は調べない（末尾）
                self.tree_mtimes.insert(child.clone(), None);
            } else {
                missing.push(child.clone());
            }
        }
        if missing.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let found: Vec<(PathBuf, Option<SystemTime>)> = missing
                .into_iter()
                .map(|p| {
                    let m = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                    (p, m)
                })
                .collect();
            let _ = tx.send(found);
            ctx.request_repaint();
        });
        self.tree_mtimes_rx = Some(rx);
    }

    /// 毎フレーム呼ぶ。更新日時が届いていれば取り込んで並べ直す。
    pub(super) fn poll_tree_mtimes(&mut self) {
        let Some(rx) = &self.tree_mtimes_rx else { return };
        match rx.try_recv() {
            Ok(found) => {
                self.tree_mtimes_rx = None;
                self.tree_mtimes.extend(found);
                self.resort_real_tree();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.tree_mtimes_rx = None,
        }
    }

    /// ツリーを作り直すとき（ルート変更・リロード）に、古い更新日時を捨てる。
    pub(super) fn clear_tree_mtimes(&mut self) {
        self.tree_mtimes.clear();
        self.tree_mtimes_rx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from(format!("/r/{n}"))).collect()
    }

    fn names(v: &[PathBuf]) -> Vec<String> {
        v.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    fn sort(key: TreeSortKey, ascending: bool) -> TreeSort {
        TreeSort { key, ascending }
    }

    #[test]
    fn name_sort_is_the_default_path_order_and_reverses() {
        let asc = sort_children(paths(&["b", "c", "a"]), TreeSort::REAL_DEFAULT, &TreeMtimes::new());
        assert_eq!(names(&asc), ["a", "b", "c"]);
        let desc = sort_children(paths(&["b", "c", "a"]), sort(TreeSortKey::Name, false), &TreeMtimes::new());
        assert_eq!(names(&desc), ["c", "b", "a"]);
    }

    #[test]
    fn date_sort_uses_cached_mtimes_with_unknown_last() {
        let at = |s: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        let mut m = TreeMtimes::new();
        m.insert(PathBuf::from("/r/a"), at(30));
        m.insert(PathBuf::from("/r/b"), at(10));
        m.insert(PathBuf::from("/r/net"), None);
        // /r/new は未確認（キャッシュに無い）＝不明扱い
        let items = paths(&["a", "net", "b", "new"]);
        assert_eq!(names(&sort_children(items.clone(), sort(TreeSortKey::Date, true), &m)), ["b", "a", "net", "new"]);
        assert_eq!(names(&sort_children(items, sort(TreeSortKey::Date, false), &m)), ["a", "b", "net", "new"]);
    }

    #[test]
    fn registration_key_falls_back_to_name() {
        let v = sort_children(paths(&["b", "a"]), sort(TreeSortKey::Registration, true), &TreeMtimes::new());
        assert_eq!(names(&v), ["a", "b"]);
    }
}
