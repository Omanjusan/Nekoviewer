use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::fs::dir;
use crate::view_reader::ViewerState;
use crate::keymap::ExplorerAction;
use super::*;

impl NekoviewApp {
    /// フォーカス巡回。検索フォームは全項目を回り、その後に履歴へ進む。
    /// 履歴が空ならその位置は飛ばす。着地したペインのカーソルも復元する。
    /// TreeTab/Drivesは「今folder_pane_tabが検索タブかどうか」で意味が変わる二重の顔を持つ：
    /// 実ツリータブ選択中は左ペインの実ツリー/ドライブ、検索タブ選択中はアイテムペイン内の
    /// ツリー/ドライブ（検索基点選択ツール、実ナビゲーションはしない）を指す。
    ///
    /// キー判定はキーアサイン設定(TODO項目J)経由。ActionBinding::pressedは修飾キー完全一致で
    /// 判定するため、旧来の consume_key(matches_logically) が抱えていた
    /// 「Shift+TabをTabとして誤検出する」問題は起きず、消費順序に気を配る必要もない。
    pub(super) fn handle_focus_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (tab, shift_tab) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::FocusNext).key_pressed(i),
            km.explorer_binding(ExplorerAction::FocusPrev).key_pressed(i),
        ));
        if !tab && !shift_tab {
            return;
        }
        if self.focused_pane == FocusPane::SearchForm {
            let next = if shift_tab { self.search_form_focus.prev() } else { self.search_form_focus.next() };
            if let Some(next) = next {
                self.search_form_focus = next;
                self.search_form_focus_request = true;
                return;
            }
            self.focused_pane = if shift_tab { FocusPane::FolderTabBar } else if self.search_history.is_empty() { FocusPane::TreeTab } else { FocusPane::SearchHistory };
            self.surrender_native_focus(ctx);
            self.on_focus_pane_changed();
            return;
        }
        self.focused_pane = if shift_tab {
            self.focused_pane.prev(self.folder_pane_tab)
        } else {
            self.focused_pane.next(self.folder_pane_tab)
        };
        if self.focused_pane == FocusPane::SearchHistory && self.search_history.is_empty() {
            self.focused_pane = if shift_tab { FocusPane::SearchForm } else { FocusPane::TreeTab };
        }
        if self.focused_pane == FocusPane::SearchForm {
            self.search_form_focus = if shift_tab { SearchFormFocus::Clear } else { SearchFormFocus::NamePattern };
            self.search_form_focus_request = true;
        }
        // 検索条件フォームのテキスト欄は lock_focus(true) にしているため、Tabで
        // focused_pane を進めても egui ネイティブのフォーカスはテキスト欄に残り続ける。
        // 放置すると次フレームの has_focus/gained_focus 判定と自前状態が食い違って
        // 行き来してしまうため、ここで egui 側のフォーカスを明示的に外す
        // （Filter欄のように自前状態→egui の一方向で揃える）。
        if self.focused_pane != FocusPane::SearchForm && self.focused_pane != FocusPane::Filter {
            self.surrender_native_focus(ctx);
        }
        self.on_focus_pane_changed();
    }

    fn surrender_native_focus(&self, ctx: &egui::Context) {
        ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id); });
    }

    fn on_focus_pane_changed(&mut self) {
        match self.focused_pane {
            FocusPane::FolderTabBar => {
                // タブバー自体。folder_pane_tabは既に確定済みなのでリセットは不要
                // （左右キーでの切替はhandle_folder_tab_bar_keysが担う）。
            }
            FocusPane::TreeTab => {
                let flat = self.flatten_visible_tree();
                let valid = self.tree_cursor.as_ref().is_some_and(|p| flat.contains(p));
                if !valid {
                    self.tree_cursor = self.viewing_dir.clone()
                        .filter(|p| flat.contains(p))
                        .or_else(|| flat.first().cloned());
                }
            }
            FocusPane::FavoriteTab => {
                let items: Vec<FavoriteSelection> = std::iter::once(FavoriteSelection::Unsorted)
                    .chain(self.favorite_folders.iter().map(|f| FavoriteSelection::Folder(f.id)))
                    .collect();
                let valid = self.favorite_cursor.is_some_and(|c| items.contains(&c));
                if !valid {
                    self.favorite_cursor = Some(FavoriteSelection::Unsorted);
                }
            }
            FocusPane::SearchForm => {}
            FocusPane::SearchHistory => {
                let valid = self.search_selected.is_some_and(|i| i < self.search_history.len());
                if !valid {
                    self.search_selected = if self.search_history.is_empty() { None } else { Some(0) };
                }
            }
            FocusPane::Drives => {
                let valid = self.drive_cursor.as_ref()
                    .is_some_and(|p| self.drives.iter().any(|d| &d.path == p));
                if !valid {
                    self.drive_cursor = self.drives.first().map(|d| d.path.clone());
                }
            }
            FocusPane::Grid => {
                // Gridへ着地した時は常にグリッド先頭（↑があればそれ、無ければ一番左上）へ
                // カーソルを戻す。以前どこにいたかは覚えない。
                let entries = self.grid_entries();
                if let Some(first) = entries.first() {
                    self.set_grid_cursor(first.clone());
                }
            }
            FocusPane::Filter | FocusPane::MenuBar => {}
        }
    }

    fn handle_search_history_keys(&mut self, ctx: &egui::Context) {
        if self.search_history.is_empty() { return; }
        let km = &self.config.keymap;
        let (up, down, enter) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
        ));
        let mut idx = self.search_selected.unwrap_or(0).min(self.search_history.len() - 1);
        if up && idx > 0 { idx -= 1; }
        if down && idx + 1 < self.search_history.len() { idx += 1; }
        self.search_selected = Some(idx);
        if enter { self.select_search_history(idx); }
    }

    /// 実ツリーの現在展開状態における「見えているノード」を上から順に平坦化したもの。
    /// ツリーカーソルの上下移動対象になる。
    pub(super) fn flatten_visible_tree(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        fn walk(
            path: &PathBuf,
            tree_expanded: &HashSet<PathBuf>,
            tree_children: &HashMap<PathBuf, Vec<PathBuf>>,
            show_hidden: bool,
            out: &mut Vec<PathBuf>,
        ) {
            out.push(path.clone());
            if tree_expanded.contains(path) {
                if let Some(children) = tree_children.get(path) {
                    for child in children {
                        if !show_hidden {
                            let hidden = child.file_name()
                                .and_then(|n| n.to_str())
                                .map_or(false, |n| n.starts_with('.'));
                            if hidden {
                                continue;
                            }
                        }
                        walk(child, tree_expanded, tree_children, show_hidden, out);
                    }
                }
            }
        }
        walk(&self.tree_root, &self.tree_expanded, &self.tree_children, self.show_hidden, &mut out);
        out
    }

    /// ツリータブにフォーカスがある間のプレターゲティングカーソル操作。
    /// 上下=移動、右=展開（未取得なら取得も要求）、左=折り畳み/親へ、Enter=確定navigate。
    fn handle_tree_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (key_down, key_up, key_right, key_left, key_enter) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavRight).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavLeft).key_pressed(i),
            km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
        ));
        if !(key_down || key_up || key_right || key_left || key_enter) {
            return;
        }

        let flat = self.flatten_visible_tree();
        if flat.is_empty() {
            return;
        }

        let cur = self.tree_cursor.clone()
            .filter(|p| flat.contains(p))
            .unwrap_or_else(|| self.viewing_dir.clone().filter(|p| flat.contains(p)).unwrap_or_else(|| flat[0].clone()));
        let pos = flat.iter().position(|p| *p == cur).unwrap_or(0);

        if key_down && pos + 1 < flat.len() {
            self.tree_cursor = Some(flat[pos + 1].clone());
        }
        if key_up {
            if pos > 0 {
                self.tree_cursor = Some(flat[pos - 1].clone());
            } else {
                // 先頭ノードでさらにUp: タブバーへ戻る
                self.focused_pane = FocusPane::FolderTabBar;
                return;
            }
        }
        if key_right {
            if !self.tree_expanded.contains(&cur) {
                self.tree_expanded.insert(cur.clone());
                if !self.tree_children.contains_key(&cur) {
                    self.tree_scan_pending = Some(TreeScanPending {
                        path: cur.clone(),
                        rx: dir::spawn_scan_subdirs(cur.clone(), {
                            let c = self.egui_ctx.clone();
                            move || c.request_repaint()
                        }),
                    });
                }
            }
            self.tree_cursor = Some(cur.clone());
        }
        if key_left {
            if self.tree_expanded.contains(&cur) {
                self.tree_expanded.remove(&cur);
                self.tree_cursor = Some(cur.clone());
            } else if let Some(parent) = cur.parent().map(|p| p.to_path_buf()) {
                if flat.contains(&parent) {
                    self.tree_cursor = Some(parent);
                }
            }
        }
        if key_enter {
            if self.folder_pane_tab == FolderPaneTab::Search {
                self.search_form.base_dir = Some(cur);
            } else {
                self.navigate_to(cur, DirectoryNavigationSource::Tree);
            }
        }
    }

    /// お気に入りタブにフォーカスがある間のプレターゲティングカーソル操作。
    /// 並びは [未整理, 定義済みフォルダ...]。上下=移動、Enter=確定enter_favorite_view。
    fn handle_favorites_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        // F2: カーソル位置のフォルダをリネーム（未整理枠はリネーム対象外）。
        // ガード（favorite_dialogが開いていない事）はアクション化せず既存のif文のまま維持する。
        if self.favorite_dialog.is_none()
            && ctx.input(|i| km.explorer_binding(ExplorerAction::Rename).key_pressed(i))
        {
            if let Some(FavoriteSelection::Folder(id)) = self.favorite_cursor {
                if let Some(folder) = self.favorite_folders.iter().find(|f| f.id == id) {
                    self.favorite_dialog = Some(FavoriteDialogState {
                        mode: FavoriteDialogMode::Rename(folder.id),
                        name: folder.name.clone(),
                        marker: folder.marker.clone(),
                        color: rgba_u32_to_color32(folder.color_rgba),
                        error: None,
                    });
                }
            }
        }

        let km = &self.config.keymap;
        let (key_down, key_up, key_enter) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
        ));
        if !(key_down || key_up || key_enter) {
            return;
        }

        let items: Vec<FavoriteSelection> = std::iter::once(FavoriteSelection::Unsorted)
            .chain(self.favorite_folders.iter().map(|f| FavoriteSelection::Folder(f.id)))
            .collect();
        if items.is_empty() {
            return;
        }

        let cur = self.favorite_cursor
            .filter(|c| items.contains(c))
            .unwrap_or(items[0]);
        let pos = items.iter().position(|c| *c == cur).unwrap_or(0);
        let mut new_pos = pos;
        if key_down && pos + 1 < items.len() {
            new_pos = pos + 1;
        }
        if key_up {
            if pos > 0 {
                new_pos = pos - 1;
            } else {
                // 先頭項目でさらにUp: タブバーへ戻る
                self.focused_pane = FocusPane::FolderTabBar;
                return;
            }
        }
        if new_pos != pos {
            self.favorite_cursor = Some(items[new_pos]);
        }
        if key_enter {
            self.enter_favorite_view(items[new_pos]);
        }
    }

    /// Drivesにフォーカスがある間のプレターゲティングカーソル操作。
    /// 上下=移動、Enter=確定navigate_to_drive。
    fn handle_drives_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (key_down, key_up, key_enter) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
        ));
        if !(key_down || key_up || key_enter) {
            return;
        }
        if self.drives.is_empty() {
            return;
        }
        let cur = self.drive_cursor.clone().unwrap_or_else(|| self.drives[0].path.clone());
        let pos = self.drives.iter().position(|d| d.path == cur).unwrap_or(0);
        let mut new_pos = pos;
        if key_down && pos + 1 < self.drives.len() {
            new_pos = pos + 1;
        }
        if key_up && pos > 0 {
            new_pos = pos - 1;
        }
        if new_pos != pos {
            self.drive_cursor = Some(self.drives[new_pos].path.clone());
        }
        if key_enter {
            let path = self.drives[new_pos].path.clone();
            if self.folder_pane_tab == FolderPaneTab::Search {
                self.set_search_base_drive(path);
            } else {
                self.navigate_to_drive(path);
            }
        }
    }

    /// MenuBarにフォーカスがある間の操作。左右=次の有効ボタンへ移動（無効ボタンは飛ばす）、
    /// Enter=クリック相当を発火。
    fn handle_menu_bar_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (key_left, key_right, key_enter) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavLeft).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavRight).key_pressed(i),
            km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
        ));
        if !(key_left || key_right || key_enter) {
            return;
        }
        let items = self.menu_bar_items();
        if key_left {
            if let Some(new_pos) = (0..self.menu_cursor).rev().find(|&i| items[i].1) {
                self.menu_cursor = new_pos;
            }
        }
        if key_right {
            if let Some(new_pos) = (self.menu_cursor + 1..items.len()).find(|&i| items[i].1) {
                self.menu_cursor = new_pos;
            }
        }
        if key_enter {
            if let Some(&(button, enabled)) = items.get(self.menu_cursor) {
                if enabled {
                    self.activate_menu_button(button);
                }
            }
        }
    }

    /// FolderTabBar（タブ切替バー自体）にフォーカスがある間の操作。
    /// 左右キーでタブ（実ツリー/お気に入り/検索）を切り替える。表示順（お気に入り→
    /// フォルダ→検索）に合わせた並びで左右移動する。
    fn handle_folder_tab_bar_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (key_left, key_right) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavLeft).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavRight).key_pressed(i),
        ));
        if !(key_left || key_right) {
            return;
        }
        const ORDER: [FolderPaneTab; 3] = [FolderPaneTab::Favorites, FolderPaneTab::RealTree, FolderPaneTab::Search];
        let pos = ORDER.iter().position(|&t| t == self.folder_pane_tab).unwrap_or(1);
        let mut new_pos = pos;
        if key_left && pos > 0 {
            new_pos = pos - 1;
        }
        if key_right && pos + 1 < ORDER.len() {
            new_pos = pos + 1;
        }
        if new_pos != pos {
            self.switch_folder_tab(ORDER[new_pos]);
        }
    }

    pub(super) fn handle_explorer_keys(&mut self, ctx: &egui::Context) {
        self.handle_focus_keys(ctx);
        if self.focused_pane == FocusPane::FolderTabBar {
            self.handle_folder_tab_bar_keys(ctx);
            return;
        }
        if self.focused_pane == FocusPane::TreeTab {
            self.handle_tree_keys(ctx);
            return;
        }
        if self.focused_pane == FocusPane::FavoriteTab {
            self.handle_favorites_keys(ctx);
            return;
        }
        if self.focused_pane == FocusPane::SearchHistory {
            self.handle_search_history_keys(ctx);
            return;
        }
        if self.focused_pane == FocusPane::Drives {
            self.handle_drives_keys(ctx);
            return;
        }
        if self.focused_pane == FocusPane::MenuBar {
            self.handle_menu_bar_keys(ctx);
            return;
        }
        if self.focused_pane != FocusPane::Grid {
            return;
        }
        self.handle_grid_keys(ctx);
    }

    /// Grid（↑・サブフォルダ・アーカイブファイルを連続した1本の列として扱う）の
    /// キーボードカーソル操作。フォルダ系エントリはEnterで即navigate（複数選択は破棄）。
    /// アーカイブエントリはEnterでカーソル位置の1件のみを開く（複数選択は維持したまま）。
    /// Escで複数選択解除。
    fn handle_grid_keys(&mut self, ctx: &egui::Context) {
        // Escは常時（矢印等が押されてなくても）反応させる
        if ctx.input(|i| self.config.keymap.explorer_binding(ExplorerAction::ClearSelection).key_pressed(i)) {
            self.multi_selected.clear();
            self.select_anchor = None;
        }

        let entries = self.grid_entries();
        if entries.is_empty() {
            return;
        }
        let total = entries.len();
        let cols = self.explorer_cols.max(1);
        let cell_h = self.config.thumb_size as f32;
        const KEY_GAP: f32 = 8.0;

        let cur_entry = self.grid_cursor.clone()
            .filter(|e| entries.contains(e))
            .or_else(|| self.selected_archive_index
                .map(GridEntry::Archive)
                .filter(|e| entries.contains(e)))
            .unwrap_or_else(|| entries[0].clone());
        let pos = entries.iter().position(|e| *e == cur_entry).unwrap_or(0);

        // キー判定はキーアサイン設定(TODO項目J)経由。ActionBinding::key_pressedは修飾キー
        // 完全一致で判定するため、旧来の consume_key(matches_logically) が抱えていた
        // 「Shift+矢印を無修飾矢印として誤検出する」問題は起きず、消費順序を気にする必要もない。
        // Shift+矢印/Home/End(Extend系)は現時点でキーアサインUIからは変更不可の固定仕様
        // （ExplorerAction::is_editable() == false）。
        let km = &self.config.keymap;
        let (skey_left, skey_right, skey_down, skey_up, skey_home, skey_end) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::ExtendLeft).key_pressed(i),
            km.explorer_binding(ExplorerAction::ExtendRight).key_pressed(i),
            km.explorer_binding(ExplorerAction::ExtendDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::ExtendUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::ExtendHome).key_pressed(i),
            km.explorer_binding(ExplorerAction::ExtendEnd).key_pressed(i),
        ));
        let (key_left, key_right, key_down, key_up, key_home, key_end) = ctx.input(|i| (
            km.explorer_binding(ExplorerAction::NavLeft).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavRight).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavHome).key_pressed(i),
            km.explorer_binding(ExplorerAction::NavEnd).key_pressed(i),
        ));
        let key_left = key_left || skey_left;
        let key_right = key_right || skey_right;
        let key_down = key_down || skey_down;
        let key_up = key_up || skey_up;
        let key_home = key_home || skey_home;
        let key_end = key_end || skey_end;
        let extend = skey_left || skey_right || skey_down || skey_up || skey_home || skey_end;
        let key_enter = ctx.input(|i| km.explorer_binding(ExplorerAction::Confirm).key_pressed(i));
        let select_all = ctx.input(|i| km.explorer_binding(ExplorerAction::SelectAll).key_pressed(i));

        if select_all {
            // Ctrl+A: 複数選択の対象はアーカイブファイルのみ
            self.multi_selected = self.filtered_indices.iter().copied().collect();
            if self.select_anchor.is_none() {
                self.select_anchor = self.filtered_indices.first().copied();
            }
            if let Some(&last) = self.filtered_indices.last() {
                self.selected_archive_index = Some(last);
                self.grid_cursor = Some(GridEntry::Archive(last));
            }
        }

        // 段階4（窓ごとキー配送）: ビューアーは独立した OS 窓になり、自分の左右キーで
        // ファイル間ナビゲーションを処理する（view_reader::process_navigation →
        // ViewerOutput.nav → render_viewer）。よって 86eca4b の「ビューア起動中は
        // エクスプローラー窓の左右キーで viewer nav を肩代わりする」回避策は撤去する。
        // エクスプローラー窓の左右キーは常にグリッド選択移動とする。
        let mut move_to: Option<usize> = None;
        if key_right && pos + 1 < total {
            move_to = Some(pos + 1);
        }
        if key_left && pos > 0 {
            move_to = Some(pos - 1);
        }
        if key_down {
            let current_row = pos / cols;
            let last_row = (total - 1) / cols;
            if current_row < last_row {
                move_to = Some((pos + cols).min(total - 1));
            }
        }
        if key_up && pos >= cols {
            move_to = Some(pos - cols);
        }
        if key_home {
            move_to = Some(0);
        }
        if key_end {
            move_to = Some(total - 1);
        }

        let prev_selected = self.selected_archive_index;
        let effective_entry = if let Some(target_pos) = move_to {
            let target = entries[target_pos].clone();
            self.grid_cursor = Some(target.clone());
            match &target {
                GridEntry::Archive(idx) => {
                    if extend {
                        self.extend_selection_to(*idx, &self.filtered_indices.clone());
                    } else {
                        self.select_single(*idx);
                    }
                }
                GridEntry::Up(_) | GridEntry::Subdir(_) => {
                    // フォルダ系エントリに乗った間はアーカイブ選択枠・ファイル情報を消す
                    // （複数選択そのものは維持し、Enterで実際に移動した時だけ破棄する）
                    self.selected_archive_index = None;
                    self.selected_archive_meta = None;
                }
            }
            target
        } else {
            cur_entry
        };

        if key_enter {
            match &effective_entry {
                GridEntry::Up(path) | GridEntry::Subdir(path) => {
                    self.multi_selected.clear();
                    self.select_anchor = None;
                    self.navigate_to(path.clone(), DirectoryNavigationSource::ItemPane);
                }
                GridEntry::Archive(idx) => {
                    // 複数選択中でもEnter時点のカーソル位置1件のみを開く（複数選択は維持）
                    if let Some(path) = self.archives.get(*idx).cloned() {
                        if self.network_gate(&path) {
                            let is_raw = self.raw_image_files.contains(&path);
                            if is_raw {
                                self.open_viewer(ViewerState::new_raw(path, self.viewer_slots, self.config.default_slot));
                            } else {
                                self.start_archive_open(path);
                            }
                        }
                    }
                }
            }
        }

        // 選択が変わったらコンテンツ空間で最小スクロールを計算（アニメーションなし）
        if self.selected_archive_index != prev_selected {
            self.selected_archive_meta = self.selected_archive_index
                .and_then(|idx| self.archives.get(idx))
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
        }
        if let Some(target_pos) = move_to {
            let row = target_pos / cols;
            let item_top = row as f32 * (cell_h + KEY_GAP);
            let item_bottom = item_top + cell_h;
            let vp = self.explorer_viewport_h;
            if item_top < self.explorer_scroll_offset {
                self.explorer_scroll_offset = item_top;
            } else if vp > 0.0 && item_bottom > self.explorer_scroll_offset + vp {
                self.explorer_scroll_offset = item_bottom - vp;
            }
        }
    }

    /// 中央グリッドの表示対象を、選択されたお気に入り（フォルダ横断）一覧に切り替える。
    /// 単一選択に置き換える（複数選択を解除し、指定インデックスを起点にする）。
    fn select_single(&mut self, idx: usize) {
        self.multi_selected.clear();
        self.select_anchor = Some(idx);
        self.selected_archive_index = Some(idx);
    }

    /// 現在の選択起点(select_anchor)から idx までの範囲を multi_selected に反映する
    /// （Shift+矢印キーによる範囲拡張用。filtered は表示順のインデックス列）。
    fn extend_selection_to(&mut self, idx: usize, filtered: &[usize]) {
        let anchor = self.select_anchor.unwrap_or(idx);
        let anchor_pos = filtered.iter().position(|&i| i == anchor).unwrap_or(0);
        let target_pos = filtered.iter().position(|&i| i == idx).unwrap_or(anchor_pos);
        let (from, to) = if anchor_pos <= target_pos { (anchor_pos, target_pos) } else { (target_pos, anchor_pos) };
        self.multi_selected = filtered[from..=to].iter().copied().collect();
        self.selected_archive_index = Some(idx);
    }
}
