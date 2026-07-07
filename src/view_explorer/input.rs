
use crate::view_reader::ViewerState;
use super::*;

impl NekoviewApp {
    pub(super) fn handle_explorer_keys(&mut self, ctx: &egui::Context) {
        // ── お気に入りペイン: F2でリネームダイアログを開く ──────────────────
        if self.folder_pane_tab == FolderPaneTab::Favorites
            && self.favorite_dialog.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2))
        {
            if let FavoriteSelection::Folder(id) = self.favorite_selected {
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

        // ── エクスプローラー キーナビゲーション ─────────────────────────────
        // フィルタ適用中は見えている項目（filtered_indices）だけを移動対象にする。
        let filtered = self.filtered_indices.clone();
        let total = filtered.len();
        let cols = self.explorer_cols.max(1);
        let cell_h = self.config.thumb_size as f32;
        const KEY_GAP: f32 = 8.0;
        if total > 0 {
            let prev = self.selected_archive_index;
            let cur_pos = self.selected_archive_index
                .and_then(|idx| filtered.iter().position(|&i| i == idx));

            // キー入力を一括消費してからクロージャ外で処理する（borrow 競合回避）
            let (key_left, key_right, key_down, key_up, key_home, key_end) = ctx.input_mut(|i| (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Home),
                i.consume_key(egui::Modifiers::NONE, egui::Key::End),
            ));
            // Shift併用版（範囲選択の拡張用）
            let (skey_left, skey_right, skey_down, skey_up, skey_home, skey_end) = ctx.input_mut(|i| (
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::Home),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::End),
            ));
            let key_left = key_left || skey_left;
            let key_right = key_right || skey_right;
            let key_down = key_down || skey_down;
            let key_up = key_up || skey_up;
            let key_home = key_home || skey_home;
            let key_end = key_end || skey_end;
            let extend = skey_left || skey_right || skey_down || skey_up || skey_home || skey_end;
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
                self.multi_selected = filtered.iter().copied().collect();
                if self.select_anchor.is_none() {
                    self.select_anchor = filtered.first().copied();
                }
                if let Some(&last) = filtered.last() {
                    self.selected_archive_index = Some(last);
                }
            }

            // 段階4（窓ごとキー配送）: ビューアーは独立した OS 窓になり、自分の左右キーで
            // ファイル間ナビゲーションを処理する（view_reader::process_navigation →
            // ViewerOutput.nav → render_viewer）。よって 86eca4b の「ビューア起動中は
            // エクスプローラー窓の左右キーで viewer nav を肩代わりする」回避策は撤去する。
            // エクスプローラー窓の左右キーは常にグリッド選択移動とする。
            let mut move_to: Option<usize> = None;
            if key_right {
                if let Some(pos) = cur_pos {
                    if pos + 1 < total {
                        move_to = Some(filtered[pos + 1]);
                    }
                }
            }
            if key_left {
                if let Some(pos) = cur_pos {
                    if pos > 0 {
                        move_to = Some(filtered[pos - 1]);
                    }
                }
            }

            // 上下キーは常にグリッド選択移動
            if key_down {
                if let Some(pos) = cur_pos {
                    let current_row = pos / cols;
                    let last_row = (total - 1) / cols;
                    if current_row < last_row {
                        move_to = Some(filtered[(pos + cols).min(total - 1)]);
                    }
                }
            }
            if key_up {
                if let Some(pos) = cur_pos {
                    if pos >= cols {
                        move_to = Some(filtered[pos - cols]);
                    }
                }
            }

            // Home/End: ファイラー先頭（左上）/末尾（右下）へ絶対ジャンプ
            if key_home {
                move_to = Some(filtered[0]);
            }
            if key_end {
                move_to = Some(filtered[total - 1]);
            }
            if let Some(target) = move_to {
                if extend {
                    self.extend_selection_to(target, &filtered);
                } else {
                    self.select_single(target);
                }
            }

            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(idx) = self.selected_archive_index {
                    if let Some(path) = self.archives.get(idx).cloned() {
                        if self.network_gate(&path) {
                            let is_raw = self.raw_image_files.contains(&path);
                            let state = if is_raw {
                                Some(ViewerState::new_raw(path.clone(), self.viewer_slots, self.config.default_slot))
                            } else if self.check_memory_budget(&path) {
                                ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot)
                            } else {
                                None
                            };
                            if let Some(state) = state {
                                self.open_viewer(state);
                            }
                        }
                    }
                }
            }
            // 選択が変わったらコンテンツ空間で最小スクロールを計算（アニメーションなし）
            if self.selected_archive_index != prev {
                self.selected_archive_meta = self.selected_archive_index
                    .and_then(|idx| self.archives.get(idx))
                    .and_then(|path| std::fs::metadata(path).ok())
                    .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
                if let Some(idx) = self.selected_archive_index {
                    if let Some(pos) = filtered.iter().position(|&i| i == idx) {
                        let row = pos / cols;
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
