use std::collections::HashSet;
use std::path::PathBuf;

use crate::i18n;
use crate::fs::archive;
use super::*;

impl NekoviewApp {
    /// DBから定義済みお気に入りフォルダ一覧を読み直してキャッシュを更新する。
    pub(super) fn refresh_favorite_folders(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        self.favorite_folders = crate::favorites::list_folders(&db);
    }

    fn enter_favorite_view(&mut self, selection: FavoriteSelection) {
        let Some(db) = self.spread_db.clone() else { return };
        let entries: Vec<(PathBuf, String)> = match selection {
            FavoriteSelection::Unsorted => crate::favorites::list_unsorted_files(&db),
            FavoriteSelection::Folder(id) => crate::favorites::list_files_in_folder(&db, id),
            FavoriteSelection::None => Vec::new(),
        };
        self.favorite_view_markers = entries
            .iter()
            .map(|(dir, name)| {
                let path = dir.join(name);
                let ids = crate::favorites::get_membership(&db, dir, name).unwrap_or_default();
                (path, ids)
            })
            .collect();
        self.archives = entries.into_iter().map(|(dir, name)| dir.join(name)).collect();
        self.raw_image_files = self
            .archives
            .iter()
            .filter(|p| archive::is_supported_image_file(p))
            .cloned()
            .collect();
        // 横断一覧では単一ディレクトリ前提のキャッシュDB/セッション状態は無効化する
        self.cache_db = None;
        self.invalid_archives.clear();
        self.thumb_failed.clear();
        self.viewing_favorites = Some(selection);
        self.sort_archives();
        self.recompute_filter();
        self.selected_archive_index = if self.archives.is_empty() { None } else { Some(0) };
        self.selected_archive_meta = None;
        self.multi_selected.clear();
        self.select_anchor = None;
    }

    /// お気に入り一覧表示を終え、実ディレクトリ（current_dir）表示に戻す。
    pub(super) fn exit_favorite_view(&mut self) {
        if self.viewing_favorites.is_none() {
            return;
        }
        self.viewing_favorites = None;
        self.favorite_selected = FavoriteSelection::None;
        self.start_scan();
    }

    pub(super) fn draw_favorites_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("+").clicked() {
                self.favorite_dialog = Some(FavoriteDialogState {
                    mode: FavoriteDialogMode::Create,
                    name: String::new(),
                    marker: FAVORITE_MARKER_CANDIDATES[0].to_string(),
                    color: default_favorite_color(),
                    error: None,
                });
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("favorites_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                // 特別枠: 未整理のお気に入り。ソート設定に関わらず常に最上位固定。
                let unsorted_selected = self.favorite_selected == FavoriteSelection::Unsorted;
                if ui
                    .selectable_label(unsorted_selected, i18n::t().favorite_unsorted_label())
                    .clicked()
                {
                    self.enter_favorite_view(FavoriteSelection::Unsorted);
                }

                for folder in self.favorite_folders.clone() {
                    let label = format!("{} {}", folder.marker, folder.name);
                    let selected = self.favorite_selected == FavoriteSelection::Folder(folder.id);
                    let resp = ui.selectable_label(selected, label);
                    if resp.clicked() {
                        self.enter_favorite_view(FavoriteSelection::Folder(folder.id));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(i18n::t().favorite_rename_menu()).clicked() {
                            self.favorite_dialog = Some(FavoriteDialogState {
                                mode: FavoriteDialogMode::Rename(folder.id),
                                name: folder.name.clone(),
                                marker: folder.marker.clone(),
                                color: rgba_u32_to_color32(folder.color_rgba),
                                error: None,
                            });
                            ui.close();
                        }
                        if ui.button(i18n::t().favorite_delete_menu()).clicked() {
                            self.favorite_delete_confirm = Some(folder.id);
                            ui.close();
                        }
                    });
                }
            });
    }

    /// お気に入りフォルダの新規作成・リネームダイアログ。
    pub(super) fn draw_favorite_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.favorite_dialog.as_mut() else {
            return;
        };
        let title = match dialog.mode {
            FavoriteDialogMode::Create => i18n::t().favorite_dialog_title_create(),
            FavoriteDialogMode::Rename(_) => i18n::t().favorite_dialog_title_rename(),
        };
        let mut cancel = false;
        let mut commit = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().favorite_dialog_prompt());
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut dialog.name);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(i18n::t().favorite_dialog_marker_label());
                    egui::ComboBox::from_id_salt("favorite_marker_combo")
                        .selected_text(dialog.marker.clone())
                        .show_ui(ui, |ui| {
                            // 候補が多いため縦一列ではなく折り返しグリッドで並べる
                            ui.set_min_width(230.0);
                            ui.horizontal_wrapped(|ui| {
                                for m in FAVORITE_MARKER_CANDIDATES {
                                    ui.selectable_value(&mut dialog.marker, (*m).to_string(), *m);
                                }
                            });
                        });
                    egui::widgets::color_picker::color_edit_button_srgba(
                        ui,
                        &mut dialog.color,
                        egui::widgets::color_picker::Alpha::Opaque,
                    );
                });
                if let Some(err) = dialog.error.clone() {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::RED, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().favorite_dialog_ok()).clicked() {
                        commit = true;
                    }
                });
            });

        if cancel {
            self.favorite_dialog = None;
            return;
        }
        if commit {
            self.commit_favorite_dialog();
        }
    }

    fn commit_favorite_dialog(&mut self) {
        let Some(dialog) = self.favorite_dialog.take() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        let color_rgba = color32_to_rgba_u32(dialog.color);
        let result = match dialog.mode {
            FavoriteDialogMode::Create => {
                crate::favorites::create_folder(&db, &dialog.name, &dialog.marker, color_rgba).map(|_| ())
            }
            FavoriteDialogMode::Rename(id) => crate::favorites::rename_folder(&db, id, &dialog.name)
                .and_then(|()| crate::favorites::set_marker(&db, id, &dialog.marker, color_rgba)),
        };
        match result {
            Ok(()) => {
                self.refresh_favorite_folders();
            }
            Err(err) => {
                let msg = match err {
                    crate::favorites::FavoriteFolderError::NameEmpty => i18n::t().favorite_error_name_empty(),
                    crate::favorites::FavoriteFolderError::NameTooLong => i18n::t().favorite_error_name_too_long(),
                    crate::favorites::FavoriteFolderError::NameConflict => i18n::t().favorite_error_name_conflict(),
                    crate::favorites::FavoriteFolderError::LimitReached => i18n::t().favorite_error_limit_reached(),
                    crate::favorites::FavoriteFolderError::NotFound
                    | crate::favorites::FavoriteFolderError::Db => i18n::t().favorite_error_generic(),
                };
                self.favorite_dialog = Some(FavoriteDialogState {
                    error: Some(msg.to_string()),
                    ..dialog
                });
            }
        }
    }

    /// お気に入りフォルダ削除の確認ダイアログ。
    pub(super) fn draw_favorite_delete_confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.favorite_delete_confirm else {
            return;
        };
        let name = self
            .favorite_folders
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        let mut cancel = false;
        let mut confirm = false;
        egui::Window::new(i18n::t().favorite_delete_confirm_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().favorite_delete_confirm_body(&name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().favorite_delete_confirm_ok()).clicked() {
                        confirm = true;
                    }
                });
            });
        if cancel {
            self.favorite_delete_confirm = None;
        }
        if confirm {
            if let Some(db) = self.spread_db.clone() {
                let _ = crate::favorites::delete_folder(&db, id);
                self.refresh_favorite_folders();
                for ids in self.favorite_states.values_mut() {
                    ids.retain(|&f| f != id);
                }
            }
            if self.favorite_selected == FavoriteSelection::Folder(id) {
                self.favorite_selected = FavoriteSelection::None;
            }
            self.favorite_delete_confirm = None;
        }
    }

    /// ビューアー右クリック「お気に入り詳細設定」ダイアログを、現在表示中ファイルの
    /// 既存お気に入り登録状態を読み込んだ上で開く。
    pub(super) fn open_favorite_detail_dialog(&mut self) {
        let path = {
            let viewer_guard = self.viewer.lock().unwrap();
            let Some(viewer) = viewer_guard.as_ref() else { return };
            viewer.archive_path().clone()
        };
        self.open_favorite_detail_dialog_for_paths(vec![path]);
    }

    /// エクスプローラー部のグリッド右クリックから、単一ファイルまたは複数選択集合を
    /// 対象にお気に入り詳細設定ダイアログを開く。
    pub(super) fn open_favorite_detail_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        let Some(db) = self.spread_db.clone() else { return };
        // 各対象ファイルの実際の所属状態を取得する（未お気に入りは空集合扱い）。
        let memberships: Vec<Option<Vec<u8>>> = targets.iter().map(|path| {
            let dir = path.parent().map(|p| p.to_path_buf());
            let filename = path.file_name().and_then(|n| n.to_str()).map(str::to_string);
            match (dir, filename) {
                (Some(dir), Some(filename)) => crate::favorites::get_membership(&db, &dir, &filename),
                _ => None,
            }
        }).collect();
        // チェックボックス初期値: 1件でも既にお気に入り登録済みならON
        // （単一選択時はそのファイル自身の状態そのもの）
        let favorite_enabled = memberships.iter().any(|m| m.is_some());
        // 積集合（共通部分）を計算する。個々のファイルで食い違う所属をダイアログ上に
        // 混在表示すると誤操作を招くため、共通部分だけを表示・編集対象にする
        // （単一選択時は積集合=そのファイル自身の所属と一致するため、従来通りの動作になる）。
        let mut common: Option<HashSet<u8>> = None;
        for m in &memberships {
            let set: HashSet<u8> = m.clone().unwrap_or_default().into_iter().collect();
            common = Some(match common {
                None => set,
                Some(prev) => prev.intersection(&set).copied().collect(),
            });
        }
        let mut common: Vec<u8> = common.unwrap_or_default().into_iter().collect();
        common.sort_unstable();
        self.favorite_detail_dialog = Some(FavoriteDetailDialogState {
            targets,
            favorite_enabled,
            common: common.clone(),
            assigned: common,
            left_selected: HashSet::new(),
            right_selected: HashSet::new(),
            pending_overwrite_confirm: false,
        });
    }

    /// お気に入り詳細設定ダイアログを描画する（デュアルリストボックス方式）。
    /// 複数選択時は決定ボタン押下後に上書き確認モーダルを1段挟む。
    pub(super) fn draw_favorite_detail_dialog(&mut self, ctx: &egui::Context) {
        if self.favorite_detail_dialog.is_none() {
            return;
        }
        if self.favorite_detail_dialog.as_ref().is_some_and(|d| d.pending_overwrite_confirm) {
            self.draw_favorite_overwrite_confirm(ctx);
            return;
        }
        let folders = self.favorite_folders.clone();
        let Some(dialog) = self.favorite_detail_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut commit = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().favorite_detail_dialog_title())
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().favorite_detail_menu_bulk(dialog.targets.len())
                } else {
                    dialog.targets.first()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string()
                };
                ui.label(label);
                ui.add_space(4.0);
                ui.checkbox(&mut dialog.favorite_enabled, i18n::t().favorite_detail_enable_checkbox());
                ui.add_space(8.0);

                ui.add_enabled_ui(dialog.favorite_enabled, |ui| {
                    ui.horizontal(|ui| {
                        // 左: 未登録の定義済みフォルダ一覧
                        ui.vertical(|ui| {
                            ui.label(i18n::t().favorite_detail_available_label());
                            egui::ScrollArea::vertical()
                                .id_salt("favorite_detail_left")
                                .min_scrolled_height(160.0)
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    for folder in folders.iter().filter(|f| !dialog.assigned.contains(&f.id)) {
                                        let selected = dialog.left_selected.contains(&folder.id);
                                        let label = format!("{} {}", folder.marker, folder.name);
                                        if ui.selectable_label(selected, label).clicked() {
                                            if selected {
                                                dialog.left_selected.remove(&folder.id);
                                            } else {
                                                dialog.left_selected.insert(folder.id);
                                            }
                                        }
                                    }
                                });
                        });

                        ui.vertical(|ui| {
                            ui.add_space(24.0);
                            if ui.button(">").clicked() {
                                for id in dialog.left_selected.drain() {
                                    if !dialog.assigned.contains(&id) {
                                        dialog.assigned.push(id);
                                    }
                                }
                            }
                            if ui.button("<").clicked() {
                                let removed = dialog.right_selected.clone();
                                dialog.assigned.retain(|id| !removed.contains(id));
                                dialog.right_selected.clear();
                            }
                        });

                        // 右: このファイルの登録先
                        ui.vertical(|ui| {
                            ui.label(i18n::t().favorite_detail_assigned_label());
                            egui::ScrollArea::vertical()
                                .id_salt("favorite_detail_right")
                                .min_scrolled_height(160.0)
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    for &id in &dialog.assigned {
                                        let Some(folder) = folders.iter().find(|f| f.id == id) else { continue };
                                        let selected = dialog.right_selected.contains(&id);
                                        let label = format!("{} {}", folder.marker, folder.name);
                                        if ui.selectable_label(selected, label).clicked() {
                                            if selected {
                                                dialog.right_selected.remove(&id);
                                            } else {
                                                dialog.right_selected.insert(id);
                                            }
                                        }
                                    }
                                });
                        });
                    });
                });
                if is_bulk {
                    ui.add_space(4.0);
                    ui.label(i18n::t().favorite_detail_common_only_note());
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().favorite_dialog_ok()).clicked() {
                        commit = true;
                    }
                });
            });

        if cancel {
            self.favorite_detail_dialog = None;
            return;
        }
        if commit {
            if is_bulk && !dialog.favorite_enabled {
                // 複数選択かつチェックボックスOFF（全ファイルをお気に入りから丸ごと削除する
                // 破壊的操作）の時のみ、実行前にもう一段確認を挟む。
                // チェックボックスON時の加減算編集は、ダイアログ上で明示的に選んだ操作の
                // 反映であり隠れた破壊が起きないため確認不要。
                if let Some(d) = self.favorite_detail_dialog.as_mut() {
                    d.pending_overwrite_confirm = true;
                }
            } else {
                self.commit_favorite_detail_dialog();
            }
        }
    }

    /// 上書き確認モーダル（複数選択時のみ）。確定でDB反映、キャンセルで詳細設定に戻る。
    fn draw_favorite_overwrite_confirm(&mut self, ctx: &egui::Context) {
        let Some(count) = self.favorite_detail_dialog.as_ref().map(|d| d.targets.len()) else { return };
        let mut confirm = false;
        let mut back = false;
        egui::Window::new(i18n::t().favorite_overwrite_confirm_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().favorite_overwrite_confirm_message(count));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        back = true;
                    }
                    if ui.button(i18n::t().favorite_overwrite_confirm_ok()).clicked() {
                        confirm = true;
                    }
                });
            });
        if back && let Some(d) = self.favorite_detail_dialog.as_mut() {
            d.pending_overwrite_confirm = false;
        }
        if confirm {
            self.commit_favorite_detail_dialog();
        }
    }

    /// お気に入り詳細設定ダイアログの内容を、対象の全ファイルへ同一状態で書き込む。
    fn commit_favorite_detail_dialog(&mut self) {
        let Some(dialog) = self.favorite_detail_dialog.take() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        // common（ダイアログを開いた時点の共通所属）と assigned（ユーザー操作後の右リスト）
        // の差分だけを加減算適用する。単一選択時は common == そのファイルの実際の所属と
        // 一致するため、この差分計算は従来通りの「right リストをそのまま反映」と同じ結果になる。
        let removed: HashSet<u8> = dialog.common.iter().filter(|id| !dialog.assigned.contains(id)).copied().collect();
        let added: HashSet<u8> = dialog.assigned.iter().filter(|id| !dialog.common.contains(id)).copied().collect();
        for path in &dialog.targets {
            let Some(dir) = path.parent().map(|p| p.to_path_buf()) else { continue };
            let Some(filename) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if !dialog.favorite_enabled {
                // チェックボックスOFF: 表示されていない個別所属も含めて丸ごと削除する核オプション
                crate::favorites::remove_favorite(&db, &dir, filename);
                if dir == self.current_dir {
                    self.favorite_states.remove(filename);
                }
                continue;
            }
            let existing = crate::favorites::get_membership(&db, &dir, filename).unwrap_or_default();
            let mut new_ids: Vec<u8> = existing.into_iter().filter(|id| !removed.contains(id)).collect();
            for id in &added {
                if !new_ids.contains(id) {
                    new_ids.push(*id);
                }
            }
            crate::favorites::set_membership(&db, &dir, filename, &new_ids);
            if dir == self.current_dir {
                self.favorite_states.insert(filename.to_string(), new_ids);
            }
        }
        // お気に入り一覧表示中は所属変更で対象ファイルが表示リストから
        // 外れうるため、同じ選択条件で一覧を再構築して追従させる
        if let Some(selection) = self.viewing_favorites {
            self.enter_favorite_view(selection);
        } else {
            // 通常のディレクトリ表示中はお気に入りスティッキーソートの
            // グループが変わりうるため並び替えを反映する
            self.sort_archives();
        }
    }
}
