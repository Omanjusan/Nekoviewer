use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::cache::ThumbRequest;
use crate::i18n;
use crate::types::ExplorerSortKey;
use crate::fs::dir;
use crate::view_reader::{PageMode, ViewerState};
use super::*;
use super::scan::spawn_summary_worker;

impl NekoviewApp {
    /// エクスプローラー窓の中身を描画する（旧 eframe::App::ui 相当）。
    /// 呼び出し元が CentralPanel の Ui を渡す。
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // ウィンドウサイズを毎フレーム記録
        let rect = ctx.input(|i| i.viewport_rect());
        self.window_size = (rect.width() as u32, rect.height() as u32);

        self.poll_workers(&ctx);
        self.prefetch_pages();

        egui::Panel::top("menu_bar").show(ui, |ui| {
            self.draw_menu_bar(ui);
        });

        {
            let style_clone = ui.style().clone();
            egui::Panel::left("folder_panel")
                .exact_size(200.0)
                .frame({
                    let mut f = egui::Frame::side_top_panel(&style_clone);
                    f.inner_margin.right = 0;
                    f
                })
                .show(ui, |ui| {
                    self.draw_folder_panel(ui);
                });
        }

        {
            let style_clone = ui.style().clone();
            egui::CentralPanel::default()
                .frame({
                    let mut f = egui::Frame::central_panel(&style_clone);
                    f.inner_margin.right = 0;
                    f
                })
                .show(ui, |ui| {
                    self.draw_central_panel(ui);
                });
        }

        self.handle_explorer_keys(&ctx);
        // release ビルドは ROOT 内フローティングウィンドウのため ui() で描画する。
        // debug ビルドの独立 deferred viewport は logic() 側で駆動する（上記参照）。
        #[cfg(not(debug_assertions))]
        self.draw_status_window(&ctx);
        self.draw_toast(&ctx);
        self.draw_memory_warning_dialog(&ctx);
        self.draw_favorite_dialog(&ctx);
        self.draw_favorite_delete_confirm_dialog(&ctx);
        self.draw_favorite_detail_dialog(&ctx);
        self.draw_settings_dialog(&ctx);
        // 旧来の無条件 ctx.request_repaint() は撤去（イベント駆動化）。
        // ROOT は入力イベント・各ワーカーの起床通知・ステータス窓の1Hzハートビートで再描画される。
    }

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // 隠しファイル表示トグルは設定ダイアログの「共通」タブへ移設した。

            // ── ページ表示モード ──────────────────────────────────────────
            let (viewer_open, is_raw_viewer, cur_mode, is_spread, can_back, can_fwd, is_offset) = {
                let guard = self.viewer.lock().unwrap();
                let viewer_open = guard.is_some();
                let is_raw_viewer = guard.as_ref().map_or(false, |v| v.is_raw_file());
                let cur_mode = guard.as_ref().map(|v| v.page_mode());
                let is_spread = cur_mode.map_or(false, |m| m != PageMode::Single);
                let can_back = guard.as_ref().map_or(false, |v| v.can_shift_backward());
                let can_fwd  = guard.as_ref().map_or(false, |v| v.can_shift_forward());
                let is_offset = guard.as_ref().map_or(false, |v| v.is_spread_offset());
                (viewer_open, is_raw_viewer, cur_mode, is_spread, can_back, can_fwd, is_offset)
            };

            ui.add_enabled_ui(viewer_open, |ui| {
                if ui.selectable_label(cur_mode == Some(PageMode::Single), i18n::t().page_single()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::Single, &mut *cfg_guard); }
                }
            });
            ui.add_enabled_ui(viewer_open && !is_raw_viewer, |ui| {
                if ui.selectable_label(cur_mode == Some(PageMode::SpreadLeft), i18n::t().page_spread_left()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::SpreadLeft, &mut *cfg_guard); }
                }
                if ui.selectable_label(cur_mode == Some(PageMode::SpreadRight), i18n::t().page_spread_right()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::SpreadRight, &mut *cfg_guard); }
                }
            });

            ui.add_enabled_ui(viewer_open && is_spread && !is_raw_viewer, |ui| {
                if ui.add_enabled(can_back, egui::Button::new(i18n::t().spread_back())).clicked() {
                    if let Some(v) = self.viewer.lock().unwrap().as_mut() { v.shift_offset_backward(); }
                }
                if ui.add_enabled(can_fwd, egui::Button::new(i18n::t().spread_fwd())).clicked() {
                    if let Some(v) = self.viewer.lock().unwrap().as_mut() { v.shift_offset_forward(); }
                }
                ui.label(if is_offset { i18n::t().spread_offset_on() } else { i18n::t().spread_aligned() });
            });

            ui.separator();

            // ── エクスプローラーソート ────────────────────────────────────
            let mut sort_changed = false;
            for key in [ExplorerSortKey::Name, ExplorerSortKey::Date, ExplorerSortKey::Size] {
                let active = self.sort_key == key;
                let clicked = ui.scope(|ui| {
                    if active {
                        ui.visuals_mut().selection.bg_fill =
                            egui::Color32::from_rgb(30, 100, 200);
                        ui.visuals_mut().selection.stroke.color = egui::Color32::WHITE;
                    }
                    ui.selectable_label(active, key.label()).clicked()
                }).inner;
                if clicked {
                    self.sort_key = key;
                    sort_changed = true;
                }
            }

            ui.label(":");

            let order_label = if self.sort_ascending { i18n::t().sort_asc() } else { i18n::t().sort_desc() };
            if ui.button(order_label).clicked() {
                self.sort_ascending = !self.sort_ascending;
                sort_changed = true;
            }

            if sort_changed {
                self.sort_archives();
                // ソート変更で archives の並びが変わり、インデックスベースの複数選択が
                // 無関係な項目を指す可能性があるため安全側に倒して解除する
                self.multi_selected.clear();
                self.select_anchor = None;
            }

            // ── ステータスウィンドウボタン（右端） ────────────────────────
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = ui.button("[?]");
                if btn.clicked() {
                    self.show_status_window = !self.show_status_window;
                }

                ui.separator();

                // 設定ダイアログを開く。旧・再デコードトグル/デバウンスサイクル/言語切替
                // ボタン列はダイアログの「共通」タブに統合した。
                if ui.button(i18n::t().settings_button()).clicked() {
                    self.open_settings();
                }
            });
        });
    }

    fn draw_folder_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.folder_pane_tab == FolderPaneTab::RealTree, i18n::t().folder_tab_real())
                .clicked()
            {
                self.folder_pane_tab = FolderPaneTab::RealTree;
                self.exit_favorite_view();
            }
            if ui
                .selectable_label(self.folder_pane_tab == FolderPaneTab::Favorites, i18n::t().folder_tab_favorites())
                .clicked()
            {
                self.folder_pane_tab = FolderPaneTab::Favorites;
            }
        });
        ui.separator();

        match self.folder_pane_tab {
            FolderPaneTab::RealTree => self.draw_real_tree_panel(ui),
            FolderPaneTab::Favorites => self.draw_favorites_pane(ui),
        }
    }

    fn draw_real_tree_panel(&mut self, ui: &mut egui::Ui) {
        // 下部に確保する高さ（ドライブ数に応じて可変）
        let drive_rows = self.drives.len() as f32;
        let bottom_h = (drive_rows * 24.0 + 44.0).min(200.0); // heading+sep+rows
        let top_h = (ui.available_height() - bottom_h - 8.0).max(40.0);

        // ── 上部: ディレクトリツリー ──
        let mut tree_action = TreeAction::None;
        egui::ScrollArea::both()
            .id_salt("folder_scroll")
            .max_height(top_h)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                show_tree_node(
                    ui,
                    &self.tree_root.clone(),
                    0,
                    &self.viewing_dir,
                    &self.tree_expanded,
                    &self.tree_children,
                    self.show_hidden,
                    &mut tree_action,
                );
            });

        match tree_action {
            TreeAction::None => {}
            TreeAction::ToggleExpand(path) => {
                if self.tree_expanded.contains(&path) {
                    self.tree_expanded.remove(&path);
                } else {
                    self.tree_expanded.insert(path.clone());
                    if !self.tree_children.contains_key(&path) {
                        // バックグラウンドでサブディレクトリを取得
                        self.tree_scan_pending = Some(TreeScanPending {
                            path: path.clone(),
                            rx: dir::spawn_scan_subdirs(path, {
                                let c = self.egui_ctx.clone();
                                move || c.request_repaint()
                            }),
                        });
                    }
                }
            }
            TreeAction::Navigate(path) => {
                self.viewing_favorites = None;
                self.current_dir = path.clone();
                self.viewing_dir = Some(path.clone());
                self.start_scan(); // cache_db をここで確定させてから clone して渡す
                self.cd_summary_rx = Some(spawn_summary_worker(path.clone(), self.cache_db.clone(), self.egui_ctx.clone()));
                self.persist_state();
            }
        }

        ui.separator();

        // ── 下部: ドライブ選択 ──
        ui.small(i18n::t().drives());
        egui::ScrollArea::vertical()
            .id_salt("drive_scroll")
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                let drives: Vec<_> = self.drives
                    .iter()
                    .map(|d| (d.label.clone(), d.path.clone()))
                    .collect();
                for (label, path) in drives {
                    let selected = self.tree_root == path;
                    if ui.selectable_label(selected, &label).clicked() {
                        self.current_dir = path.clone();
                        self.start_scan();
                        // ツリーのルートをドライブに切り替え
                        self.tree_root = path.clone();
                        self.tree_expanded.clear();
                        self.tree_children.clear();
                        self.viewing_dir = None;
                        self.cd_summary = None;
                        self.cd_summary_rx = None;
                        // ドライブルートのサブディレクトリをバックグラウンドで取得
                        self.tree_scan_pending = Some(TreeScanPending {
                            path: path.clone(),
                            rx: dir::spawn_scan_subdirs(path, {
                                let c = self.egui_ctx.clone();
                                move || c.request_repaint()
                            }),
                        });
                        self.persist_state();
                    }
                }
            });
    }

    fn draw_central_panel(&mut self, ui: &mut egui::Ui) {
        // お気に入りタブ中は実ディレクトリ由来の表示（パス・サマリー）を出さない。
        // cd_summary はワーカーが非同期で書き込むため、状態クリアではなく描画側でゲートする。
        let in_favorites_ui = self.folder_pane_tab == FolderPaneTab::Favorites
            || self.viewing_favorites.is_some();

        match self.viewing_favorites {
            Some(FavoriteSelection::Unsorted) => {
                ui.label(i18n::t().favorite_view_header_unsorted());
            }
            Some(FavoriteSelection::Folder(id)) => {
                let name = self
                    .favorite_folders
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                ui.label(i18n::t().favorite_view_header_folder(&name));
            }
            _ if in_favorites_ui => {
                ui.label("");
            }
            _ => {
                ui.label(self.current_dir.display().to_string());
            }
        }

        // CD/LS状態: ディレクトリのサマリーを表示
        if let Some((cd_path, saved, total)) = &self.cd_summary
            && !in_favorites_ui
        {
            let dir_name = cd_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("▶ {dir_name}"))
                        .color(ui.visuals().selection.bg_fill),
                );
                ui.label(
                    egui::RichText::new(i18n::t().thumb_saved(*saved, *total))
                        .color(egui::Color32::GRAY),
                );
            });
        }

        if let Some((mtime, size_bytes)) = &self.selected_archive_meta {
            let filename = self.selected_archive_index
                .and_then(|idx| self.archives.get(idx))
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            ui.separator();
            let mb = *size_bytes as f64 / (1024.0 * 1024.0);
            let date_str = format_mtime(*mtime);
            ui.label(i18n::t().file_info(&date_str, mb, filename));
        }

        ui.separator();

        {
            // ローディング中は 0.5秒経過後にスピナーを表示（短いアクセスのチラツキ防止）
            let is_loading = matches!(&self.scan_state, ScanState::Loading { started_at, .. }
                if started_at.elapsed().as_secs_f32() > 0.5);

            const FILTER_BAR_H: f32 = 28.0;
            let content_h = (ui.available_height() - FILTER_BAR_H).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), content_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    if is_loading {
                        ui.centered_and_justified(|ui| {
                            ui.label(i18n::t().loading());
                        });
                    } else {
                        self.draw_archive_grid(ui);
                    }
                },
            );
            self.draw_filter_bar(ui);
        }
    }

    /// サムネグリッド最下部の検索フィルタ行（ラベル＋チェックボックス＋テキスト入力）
    fn draw_filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(i18n::t().explorer_filter_label());
            let mut changed = ui.checkbox(&mut self.filter_enabled, "").changed();
            let resp = ui.add_enabled(
                self.filter_enabled,
                egui::TextEdit::singleline(&mut self.filter_text)
                    .hint_text(i18n::t().explorer_filter_hint())
                    .desired_width(ui.available_width()),
            );
            if resp.changed() {
                changed = true;
            }
            if changed {
                self.recompute_filter();
            }
        });
    }

    fn draw_archive_grid(&mut self, ui: &mut egui::Ui) {
        let cell_h = self.config.thumb_size as f32;
        let cell_w = (cell_h / std::f32::consts::SQRT_2).round();
        const GAP: f32 = 8.0;
        let avail_w = ui.available_width();
        let full_cols = ((avail_w + GAP) / (cell_w + GAP)).floor() as usize;
        let used_w = full_cols as f32 * (cell_w + GAP) - GAP;
        let cols = if avail_w - used_w >= cell_w / 2.0 { full_cols + 1 } else { full_cols }.max(1);
        self.explorer_cols = cols;

        let output = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .vertical_scroll_offset(self.explorer_scroll_offset)
                .show(ui, |ui| {
            egui::Grid::new("archive_grid")
                .num_columns(cols)
                .spacing([GAP, GAP])
                .show(ui, |ui| {
                    // フィルタ適用中は filtered_indices（archives へのインデックス）のみ描画対象にする
                    let visible: Vec<(usize, PathBuf)> = self.filtered_indices.iter()
                        .map(|&idx| (idx, self.archives[idx].clone()))
                        .collect();
                    for (i, (real_idx, path)) in visible.iter().enumerate() {
                        let real_idx = *real_idx;
                        let is_selected = self.selected_archive_index == Some(real_idx)
                            || self.multi_selected.contains(&real_idx);
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(cell_w, cell_h),
                            egui::Sense::click(),
                        );

                        if ui.is_rect_visible(rect) {
                            if let Some(tex) = self.thumbnails.get(path) {
                                ui.painter().image(
                                    tex.id(),
                                    rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(
                                    rect,
                                    4.0,
                                    egui::Color32::from_gray(60),
                                );
                                if !self.thumb_pending.contains(path) && !self.thumb_failed.contains(path) {
                                    if self.thumb_req_tx.try_send(ThumbRequest {
                                        archive_path: path.clone(),
                                        db: self.cache_db.clone(),
                                        is_raw_file: self.raw_image_files.contains(path),
                                    }).is_ok() {
                                        self.thumb_pending.insert(path.clone());
                                    }
                                }
                            }

                            // 無効ZIP・サムネデコード失敗は左上に赤Xを描画
                            let thumb_failed = self.thumb_failed.contains(path);
                            if self.invalid_archives.contains(path) || thumb_failed {
                                let x_size = 16.0;
                                let origin = rect.min + egui::vec2(4.0, 4.0);
                                let end = origin + egui::vec2(x_size, x_size);
                                let stroke = egui::Stroke::new(2.5, egui::Color32::from_rgb(220, 50, 50));
                                ui.painter().line_segment([origin, end], stroke);
                                ui.painter().line_segment(
                                    [egui::pos2(end.x, origin.y), egui::pos2(origin.x, end.y)],
                                    stroke,
                                );
                            }
                            if thumb_failed {
                                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
                                response.clone().on_hover_text(format!("{ext}デコード失敗"));
                            }

                            // お気に入りマーカー: 左上から左下に列挙（表示できる分だけ）
                            // お気に入り一覧表示中はフルパスキー、通常のディレクトリ表示中はファイル名キーで引く。
                            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            let marker_ids = if self.viewing_favorites.is_some() {
                                self.favorite_view_markers.get(path)
                            } else {
                                self.favorite_states.get(filename)
                            };
                            if let Some(folder_ids) = marker_ids {
                                const MARKER_LINE_H: f32 = 16.0;
                                let marker_font = egui::FontId::proportional(14.0);
                                if folder_ids.is_empty() {
                                    ui.painter().text(
                                        rect.min + egui::vec2(4.0, 4.0),
                                        egui::Align2::LEFT_TOP,
                                        "★",
                                        marker_font,
                                        default_favorite_color(),
                                    );
                                } else {
                                    let max_lines = ((cell_h - 8.0) / MARKER_LINE_H).floor().max(1.0) as usize;
                                    for (i, id) in folder_ids.iter().take(max_lines).enumerate() {
                                        let Some(folder) = self.favorite_folders.iter().find(|f| f.id == *id) else {
                                            continue;
                                        };
                                        ui.painter().text(
                                            rect.min + egui::vec2(4.0, 4.0 + i as f32 * MARKER_LINE_H),
                                            egui::Align2::LEFT_TOP,
                                            &folder.marker,
                                            marker_font.clone(),
                                            rgba_u32_to_color32(folder.color_rgba),
                                        );
                                    }
                                }
                            }

                            // ネットワークリンク切れマーカー: 右上（大元マウント単位で判定済みのもののみ）
                            if let Some(root) = crate::fs::mount::network_mount_root(path)
                                && self.network_unreachable_mounts.contains(&root)
                            {
                                let mark_size = 16.0;
                                let origin = egui::pos2(rect.max.x - mark_size - 4.0, rect.min.y + 4.0);
                                ui.painter().text(
                                    origin,
                                    egui::Align2::LEFT_TOP,
                                    "⚠",
                                    egui::FontId::proportional(14.0),
                                    egui::Color32::from_rgb(220, 160, 40),
                                );
                            }

                            // 選択中アイテムを枠で囲む（生ファイルは赤、ZIPは青）
                            if is_selected {
                                let is_raw = self.raw_image_files.contains(path);
                                let stroke_color = if is_raw {
                                    egui::Color32::from_rgb(220, 60, 60)
                                } else {
                                    egui::Color32::from_rgb(50, 120, 230)
                                };
                                ui.painter().rect_stroke(
                                    rect,
                                    0.0,
                                    egui::Stroke::new(2.0, stroke_color),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }

                        let is_raw = self.raw_image_files.contains(path);
                        if response.clicked() {
                            let modifiers = ui.input(|i| i.modifiers);
                            if modifiers.command {
                                // Ctrl(Cmd)+クリック: トグル追加/除外
                                if self.multi_selected.is_empty()
                                    && let Some(prev) = self.selected_archive_index
                                {
                                    self.multi_selected.insert(prev);
                                }
                                if !self.multi_selected.insert(real_idx) {
                                    self.multi_selected.remove(&real_idx);
                                }
                                self.select_anchor = Some(real_idx);
                                self.selected_archive_index = Some(real_idx);
                                self.selected_archive_meta = None;
                                if self.multi_selected.len() == 1 {
                                    self.multi_selected.clear();
                                }
                            } else if modifiers.shift {
                                // Shift+クリック: 起点からの範囲選択
                                let anchor = self.select_anchor.unwrap_or(real_idx);
                                let anchor_pos = visible.iter().position(|(idx, _)| *idx == anchor).unwrap_or(i);
                                let (from, to) = if anchor_pos <= i { (anchor_pos, i) } else { (i, anchor_pos) };
                                self.multi_selected = visible[from..=to].iter().map(|(idx, _)| *idx).collect();
                                self.selected_archive_index = Some(real_idx);
                                self.selected_archive_meta = None;
                            } else if is_raw && self.selected_archive_index == Some(real_idx) && self.multi_selected.is_empty() {
                                // 生ファイル: 選択済み状態のシングルクリックで開く
                                if self.network_gate(path) {
                                    self.open_viewer(ViewerState::new_raw(path.clone(), self.viewer_slots, self.config.default_slot));
                                }
                            } else {
                                self.multi_selected.clear();
                                self.select_anchor = Some(real_idx);
                                self.selected_archive_index = Some(real_idx);
                                self.selected_archive_meta = std::fs::metadata(path)
                                    .ok()
                                    .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
                            }
                        }
                        if response.double_clicked() && !is_raw {
                            if self.invalid_archives.contains(path) {
                                let name = truncate_filename(path);
                                self.app_toast = Some((
                                    i18n::t().invalid_zip(&name),
                                    std::time::Instant::now(),
                                ));
                            } else if !self.network_gate(path) {
                                // トースト表示・再チェックは network_gate 内で処理済み。
                            } else if !self.check_memory_budget(path) {
                                // ダイアログ表示フラグは check_memory_budget 内で立つ。オープンは中止する。
                            } else {
                                match ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot) {
                                    Some(state) => {
                                        self.open_viewer(state);
                                    }
                                    None => {
                                        let p = path.clone();
                                        self.mark_archive_invalid(&p);
                                        let name = truncate_filename(path);
                                        self.app_toast = Some((
                                            i18n::t().invalid_zip(&name),
                                            std::time::Instant::now(),
                                        ));
                                    }
                                }
                            }
                        }

                        // 右クリックメニュー: 複数選択中はホバー位置を無視し選択集合全体を対象にする。
                        // 未選択（単一）時はこのセルのファイル1件のみを対象にする。
                        response.context_menu(|ui| {
                            if !self.multi_selected.is_empty() {
                                let count = self.multi_selected.len();
                                if ui.button(i18n::t().favorite_detail_menu_bulk(count)).clicked() {
                                    let targets: Vec<PathBuf> = self.multi_selected.iter()
                                        .filter_map(|&idx| self.archives.get(idx).cloned())
                                        .collect();
                                    self.open_favorite_detail_dialog_for_paths(targets);
                                    ui.close();
                                }
                            } else if ui.button(i18n::t().favorite_detail_menu()).clicked() {
                                self.open_favorite_detail_dialog_for_paths(vec![path.clone()]);
                                ui.close();
                            }
                        });

                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                    if !visible.is_empty() && visible.len() % cols != 0 {
                        ui.end_row();
                    }
                });
            // グリッド下の余白（サムネの無い領域）への右クリック: メニューは出すが非活性にする
            let bg_size = egui::vec2(ui.available_width(), ui.available_height().max(40.0));
            let (_, bg_response) = ui.allocate_exact_size(bg_size, egui::Sense::click());
            bg_response.context_menu(|ui| {
                ui.add_enabled(false, egui::Button::new(i18n::t().favorite_detail_menu()));
            });
        });
        // ユーザーの手動スクロールを読み戻してストアを更新
        self.explorer_scroll_offset = output.state.offset.y;
        self.explorer_viewport_h = output.inner_rect.height();
    }
}

fn show_tree_node(
    ui: &mut egui::Ui,
    path: &PathBuf,
    depth: usize,
    viewing_dir: &Option<PathBuf>,
    tree_expanded: &HashSet<PathBuf>,
    tree_children: &HashMap<PathBuf, Vec<PathBuf>>,
    show_hidden: bool,
    action: &mut TreeAction,
) {
    if !matches!(action, TreeAction::None) {
        return;
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string());

    let is_expanded = tree_expanded.contains(path);
    let is_current = viewing_dir.as_ref() == Some(path);

    let show_arrow = is_expanded
        || match tree_children.get(path) {
            None => true,
            Some(ch) => !ch.is_empty(),
        };

    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 12.0);

        if show_arrow {
            let arrow = if is_expanded { "▼" } else { "▶" };
            let r = ui.add(egui::Label::new(arrow).sense(egui::Sense::click()));
            if r.clicked() {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        } else {
            ui.add_space(12.0);
        }

        ui.add_space(4.0);

        let r = ui.scope(|ui| {
            if is_current {
                ui.visuals_mut().selection.bg_fill =
                    egui::Color32::from_rgb(160, 50, 50);
                ui.visuals_mut().selection.stroke.color = egui::Color32::WHITE;
            }
            ui.selectable_label(is_current, &name)
        }).inner;
        if r.clicked() && matches!(*action, TreeAction::None) {
            *action = TreeAction::Navigate(path.clone());
        }
    });

    if is_expanded {
        if let Some(children) = tree_children.get(path) {
            let children = children.clone();
            for child in &children {
                if !show_hidden {
                    let hidden = child
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map_or(false, |n| n.starts_with('.'));
                    if hidden {
                        continue;
                    }
                }
                show_tree_node(
                    ui,
                    child,
                    depth + 1,
                    viewing_dir,
                    tree_expanded,
                    tree_children,
                    show_hidden,
                    action,
                );
            }
        }
    }
}

fn truncate_filename(path: &std::path::Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    const MAX: usize = 24;
    if name.chars().count() <= MAX {
        name.to_string()
    } else {
        let s: String = name.chars().take(MAX - 3).collect();
        format!("{s}...")
    }
}

fn format_mtime(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (secs / 86400) as i64 + 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}/{:02}/{:02}", y, m, d)
}
