//! リストの「★/訪問回数の編集」のロック。
//!
//! 普段はロック状態で、行の＋－は押せない。ボタンで解除している間だけ編集できる。
//! 解除は保存しない（起動時は必ずロック）。表示が変わったら自動でロックに戻す:
//! フォルダ移動・タブ切替・お気に入り／検索／仮想フォルダ／類似ファイル一覧への出入り・表示モードの切替。
//! 個々の移動処理に書き足す方式ではなく、毎フレーム「今の表示の同一性」を比べて検出する（書き忘れを構造的に防ぐ）。

use std::path::PathBuf;

use super::view_mode::ViewMode;
use super::{FavoriteSelection, FolderPaneTab, NekoviewApp};

/// 「今どの表示を見ているか」。これが前のフレームと違えば、別の表示へ移ったとみなす。
#[derive(Clone, PartialEq, Debug)]
pub(super) struct ViewKey {
    pub dir: PathBuf,
    pub tab: FolderPaneTab,
    pub favorites: Option<FavoriteSelection>,
    pub virtual_node: Option<u32>,
    pub search: Option<usize>,
    /// 類似ファイル一覧の入口のファイル。
    pub similar_entry: Option<PathBuf>,
    pub mode: ViewMode,
}

#[derive(Default)]
pub(super) struct ListEditLock {
    unlocked: bool,
    seen: Option<ViewKey>,
}

impl ListEditLock {
    pub(super) fn is_unlocked(&self) -> bool {
        self.unlocked
    }

    /// ボタンの押下。ロック ⇄ アンロック。
    pub(super) fn toggle(&mut self) {
        self.unlocked = !self.unlocked;
    }

    /// 毎フレーム、今の表示を渡す。前と違えば自動でロックに戻す。
    /// ロックしなおした（アンロック → ロック）ときだけ true を返す。
    pub(super) fn sync(&mut self, key: ViewKey) -> bool {
        let moved = self.seen.as_ref().is_some_and(|prev| *prev != key);
        self.seen = Some(key);
        if moved && self.unlocked {
            self.unlocked = false;
            return true;
        }
        false
    }

    /// 手動でロックに戻す。アンロック → ロックになったときだけ true。
    pub(super) fn lock(&mut self) -> bool {
        std::mem::replace(&mut self.unlocked, false)
    }
}

impl NekoviewApp {
    /// 今の表示を `ListEditLock` に渡す（前のフレームと違えばロックに戻る）。ツールバーの描画のたびに呼ぶ。
    pub(super) fn sync_list_edit_lock(&mut self) {
        let key = ViewKey {
            dir: self.current_dir.clone(),
            tab: self.folder_pane_tab,
            favorites: self.viewing_favorites,
            virtual_node: self.viewing_virtual_node,
            search: self.viewing_search,
            similar_entry: self.viewing_similar.as_ref().map(|v| v.entry_path.clone()),
            mode: self.view_mode.mode,
        };
        self.list_edit_lock.sync(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(dir: &str) -> ViewKey {
        ViewKey {
            dir: PathBuf::from(dir),
            tab: FolderPaneTab::RealTree,
            favorites: None,
            virtual_node: None,
            search: None,
            similar_entry: None,
            mode: ViewMode::List,
        }
    }

    #[test]
    fn starts_locked() {
        assert!(!ListEditLock::default().is_unlocked());
    }

    #[test]
    fn toggle_flips_between_locked_and_unlocked() {
        let mut l = ListEditLock::default();
        l.toggle();
        assert!(l.is_unlocked());
        l.toggle();
        assert!(!l.is_unlocked());
    }

    #[test]
    fn staying_on_the_same_view_keeps_it_unlocked() {
        let mut l = ListEditLock::default();
        l.sync(key("/a"));
        l.toggle();
        assert!(!l.sync(key("/a")));
        assert!(l.is_unlocked());
    }

    #[test]
    fn moving_to_another_folder_locks_again() {
        let mut l = ListEditLock::default();
        l.sync(key("/a"));
        l.toggle();
        assert!(l.sync(key("/b")), "ロックに戻した合図が返る");
        assert!(!l.is_unlocked());
    }

    #[test]
    fn switching_tab_or_entering_other_views_locks_again() {
        let variants: [fn(&mut ViewKey); 5] = [
            |k| k.tab = FolderPaneTab::Search,
            |k| k.virtual_node = Some(3),
            |k| k.search = Some(0),
            |k| k.similar_entry = Some(PathBuf::from("/a/x.zip")),
            |k| k.mode = ViewMode::Card,
        ];
        for change in variants {
            let mut l = ListEditLock::default();
            l.sync(key("/a"));
            l.toggle();
            let mut next = key("/a");
            change(&mut next);
            assert!(l.sync(next.clone()));
            assert!(!l.is_unlocked(), "{next:?}");
        }
    }

    #[test]
    fn the_first_frame_does_not_count_as_a_move() {
        let mut l = ListEditLock::default();
        l.toggle();
        assert!(!l.sync(key("/a")), "最初のフレームは比べる相手がない");
        assert!(l.is_unlocked());
    }

    #[test]
    fn manual_lock_reports_only_a_real_change() {
        let mut l = ListEditLock::default();
        assert!(!l.lock());
        l.toggle();
        assert!(l.lock());
        assert!(!l.is_unlocked());
    }
}
