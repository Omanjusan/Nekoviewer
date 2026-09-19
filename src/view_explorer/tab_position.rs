//! お気に入り・検索・仮想フォルダの各タブの位置の保存と復元（stateファイルの `tab_*` キー）。
//!
//! 保存: ユーザーが位置を選ぶたびに `tab_positions` を更新して `persist_state` する（値が変わったときだけ）。
//! 復元: タブに入るたびに保存位置を開く。保存が無い・検証で外れた場合は、各タブの既定の位置にする
//! （お気に入り=未整理、検索=実ツリータブの現在地、仮想=`/`）。実ツリータブは従来どおり `last_dir`。
//! 「前回フォルダに復帰」設定がオフなら、起動時に保存位置を読み捨てる（`NekoviewApp::new`）。
//!
//! 最後に選んだタブ自体（`tab_active`）も同じ仕組みで保存し、起動時にそのタブを開く。
//! CLI引数で起動したときは実ツリータブ固定（`main.rs` が保存値を実ツリーに上書きする）。

use std::path::{Path, PathBuf};

use crate::gui_config::{FavoritePosition, SavedTab};

use super::*;

/// 保存されたお気に入りの選択を、実在するフォルダのときだけ採用する（それ以外は未整理）。
pub(super) fn resolve_favorites(
    saved: Option<FavoritePosition>,
    folder_exists: impl Fn(u8) -> bool,
) -> FavoriteSelection {
    match saved {
        Some(FavoritePosition::Folder(id)) if folder_exists(id) => FavoriteSelection::Folder(id),
        _ => FavoriteSelection::Unsorted,
    }
}

/// 保存された検索対象フォルダを、到達できるときだけ採用する（それ以外はPWD）。
pub(super) fn resolve_search_dir(
    saved: Option<&Path>,
    reachable: impl Fn(&Path) -> bool,
    pwd: &Path,
) -> PathBuf {
    match saved {
        Some(p) if reachable(p) => p.to_path_buf(),
        _ => pwd.to_path_buf(),
    }
}

/// 起動時に復元するタブのフォーカス位置。タブ列のクリックと同じ位置にするが、
/// 検索は入力欄が最初からキー入力を奪わないようタブ列に置く。
pub(super) fn startup_focus(tab: FolderPaneTab) -> FocusPane {
    match tab {
        FolderPaneTab::RealTree => FocusPane::TreeTab,
        FolderPaneTab::Favorites => FocusPane::FavoriteTab,
        FolderPaneTab::Search | FolderPaneTab::VirtualFolders => FocusPane::FolderTabBar,
    }
}

impl From<FolderPaneTab> for SavedTab {
    fn from(tab: FolderPaneTab) -> Self {
        match tab {
            FolderPaneTab::RealTree => Self::RealTree,
            FolderPaneTab::Favorites => Self::Favorites,
            FolderPaneTab::Search => Self::Search,
            FolderPaneTab::VirtualFolders => Self::VirtualFolders,
        }
    }
}

impl From<SavedTab> for FolderPaneTab {
    fn from(tab: SavedTab) -> Self {
        match tab {
            SavedTab::RealTree => Self::RealTree,
            SavedTab::Favorites => Self::Favorites,
            SavedTab::Search => Self::Search,
            SavedTab::VirtualFolders => Self::VirtualFolders,
        }
    }
}

impl NekoviewApp {
    /// 選んだタブを保存する（値が変わったときだけ書く）。
    pub(super) fn remember_active_tab(&mut self, tab: FolderPaneTab) {
        let saved = SavedTab::from(tab);
        if self.tab_positions.active != Some(saved) {
            self.tab_positions.active = Some(saved);
            self.persist_state();
        }
    }

    /// 起動時: 最後に選んでいたタブ（未保存なら実ツリー）を開く。実ツリーは初期状態のままなので何もしない。
    pub(super) fn restore_active_tab(&mut self) {
        let tab = self.tab_positions.active.map_or(FolderPaneTab::RealTree, FolderPaneTab::from);
        if tab == FolderPaneTab::RealTree {
            return;
        }
        self.switch_folder_tab(tab);
        self.focused_pane = startup_focus(tab);
    }

    /// お気に入りタブに入ったとき: 保存された選択（無い・消えていれば未整理）を開く。
    pub(super) fn restore_favorites_position(&mut self) {
        self.refresh_favorite_folders();
        let sel = resolve_favorites(self.tab_positions.favorites, |id| {
            self.favorite_folders.iter().any(|f| f.id == id)
        });
        self.favorite_cursor = Some(sel);
        self.enter_favorite_view(sel);
    }

    /// お気に入りの選択を保存する（値が変わったときだけ書く）。
    pub(super) fn remember_favorites_position(&mut self, selection: FavoriteSelection) {
        let pos = match selection {
            FavoriteSelection::Unsorted => FavoritePosition::Unsorted,
            FavoriteSelection::Folder(id) => FavoritePosition::Folder(id),
            FavoriteSelection::None => return,
        };
        if self.tab_positions.favorites != Some(pos) {
            self.tab_positions.favorites = Some(pos);
            self.persist_state();
        }
    }

    /// 検索タブへの初回入場時の検索対象フォルダ: 保存済み（到達できれば）、無ければ実ツリータブの現在地。
    /// 既定値は保存しない（ユーザーが選んだときだけ保存する）。
    pub(super) fn default_search_dir(&self) -> PathBuf {
        resolve_search_dir(
            self.tab_positions.search_dir.as_deref(),
            |p| self.path_reachable(p),
            self.real_tab_dir(),
        )
    }

    /// ユーザーが検索対象フォルダを選んだ（ツリー・ドライブ・Enter）: 設定して保存する。
    pub(super) fn set_search_base_dir(&mut self, path: PathBuf) {
        self.search_form.base_dir = Some(path.clone());
        if self.tab_positions.search_dir.as_ref() != Some(&path) {
            self.tab_positions.search_dir = Some(path);
            self.persist_state();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_restore_existing_folder_else_unsorted() {
        let exists = |id: u8| id == 3;
        assert_eq!(
            resolve_favorites(Some(FavoritePosition::Folder(3)), exists),
            FavoriteSelection::Folder(3)
        );
        // 消えたフォルダ・未整理・未保存は未整理
        assert_eq!(
            resolve_favorites(Some(FavoritePosition::Folder(9)), exists),
            FavoriteSelection::Unsorted
        );
        assert_eq!(
            resolve_favorites(Some(FavoritePosition::Unsorted), exists),
            FavoriteSelection::Unsorted
        );
        assert_eq!(resolve_favorites(None, exists), FavoriteSelection::Unsorted);
    }

    #[test]
    fn search_dir_prefers_reachable_saved_else_pwd() {
        let pwd = Path::new("/pwd");
        let reachable = |p: &Path| p == Path::new("/saved");
        assert_eq!(resolve_search_dir(Some(Path::new("/saved")), reachable, pwd), PathBuf::from("/saved"));
        // 到達できない・未保存はPWD
        assert_eq!(resolve_search_dir(Some(Path::new("/gone")), reachable, pwd), PathBuf::from("/pwd"));
        assert_eq!(resolve_search_dir(None, reachable, pwd), PathBuf::from("/pwd"));
    }

    #[test]
    fn saved_tab_conversion_is_lossless() {
        for t in [FolderPaneTab::RealTree, FolderPaneTab::Favorites, FolderPaneTab::Search, FolderPaneTab::VirtualFolders] {
            assert_eq!(FolderPaneTab::from(SavedTab::from(t)), t);
        }
    }

    #[test]
    fn startup_focus_avoids_text_input_for_search() {
        assert_eq!(startup_focus(FolderPaneTab::RealTree), FocusPane::TreeTab);
        assert_eq!(startup_focus(FolderPaneTab::Favorites), FocusPane::FavoriteTab);
        assert_eq!(startup_focus(FolderPaneTab::Search), FocusPane::FolderTabBar);
        assert_eq!(startup_focus(FolderPaneTab::VirtualFolders), FocusPane::FolderTabBar);
    }
}
