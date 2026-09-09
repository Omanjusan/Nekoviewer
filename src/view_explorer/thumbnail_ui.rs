use super::*;

impl NekoviewApp {
    pub(super) fn thumbnail_menu_available(&self) -> bool {
        self.folder_pane_tab == FolderPaneTab::RealTree
            && self.viewing_favorites.is_none()
            && self.viewing_search.is_none()
    }

    pub(super) fn open_thumbnail_dialog(&mut self) {
        if !self.thumbnail_menu_available() {
            return;
        }
        self.thumbnail_dialog_open = true;
        self.thumbnail_delete_confirm_all = false;
        self.thumbnail_delete_finished = None;
    }

    fn start_thumbnail_delete(&mut self, mode: ThumbnailDeleteMode) {
        let Some(db) = self.cache_db.clone() else {
            self.thumbnail_delete_finished = Some(ThumbnailDeleteFinished {
                success: true,
                deleted: 0,
            });
            return;
        };
        let path = self.current_dir.clone();
        let requested_edge = self.config.thumb_size;
        let ctx = self.egui_ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.thumbnail_delete_rx = Some(rx);
        self.thumbnail_delete_finished = None;
        self.thumbnail_delete_confirm_all = false;
        // 次フレーム以降の新規生成を、削除トランザクション完了まで止める。
        self.thumb_generation_state.allowed = false;
        std::thread::spawn(move || {
            let result = match mode {
                ThumbnailDeleteMode::Mismatched =>
                    crate::neko_dir::delete_mismatched_thumbnails(&db, requested_edge),
                ThumbnailDeleteMode::All =>
                    crate::neko_dir::delete_all_thumbnails(&db, requested_edge),
            };
            let _ = tx.send((path, result));
            ctx.request_repaint();
        });
    }

    fn poll_thumbnail_delete(&mut self) {
        let result = self.thumbnail_delete_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        let Some((path, result)) = result else { return };
        self.thumbnail_delete_rx = None;
        self.thumbnail_delete_finished = Some(ThumbnailDeleteFinished {
            success: result.success,
            deleted: result.deleted,
        });
        if path == self.current_dir {
            self.refresh_thumbnail_generation_state();
            self.thumbnails.clear();
            self.thumb_pending.clear();
            self.thumb_generation_blocked.clear();
            self.thumb_failed.clear();
            // PWD横の「保存数/総数」は消さず、削除前の値を表示したままRDBを再集計する。
            // 結果は既存のpoll_workers経路で現在PWDとの一致を確認して差し替えられる。
            if result.success {
                self.cd_summary_rx = Some(super::scan::spawn_summary_worker(
                    path,
                    self.archive_filenames(),
                    self.cache_db.clone(),
                    self.egui_ctx.clone(),
                ));
            }
        }
    }

    pub(super) fn draw_thumbnail_dialog(&mut self, ctx: &egui::Context) {
        self.poll_thumbnail_delete();
        if !self.thumbnail_dialog_open {
            return;
        }

        let stats = self.cache_db.as_ref().map_or(
            crate::neko_dir::ThumbnailCacheStats::default(),
            |db| crate::neko_dir::thumbnail_cache_stats(db, self.config.thumb_size),
        );
        let busy = self.thumbnail_delete_rx.is_some();
        let mut close = false;
        let mut delete_mismatched = false;
        let mut request_delete_all = false;
        let mut confirm_delete_all = false;
        let mut cancel_delete_all = false;

        egui::Modal::new(egui::Id::new("thumbnail_cache_dialog")).show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.heading(i18n::t().thumbnail_dialog_title());
            ui.separator();
            ui.label(i18n::t().thumbnail_dialog_target());
            ui.monospace(self.current_dir.display().to_string());
            ui.add_space(8.0);
            ui.label(format!(
                "{}: {} px",
                i18n::t().thumbnail_dialog_requested_size(),
                self.config.thumb_size,
            ));
            if let (Some(min), Some(max)) = (stats.min_edge, stats.max_edge) {
                let saved = if min == max {
                    format!("{min} px")
                } else {
                    format!("{min}–{max} px")
                };
                ui.label(format!("{}: {saved}", i18n::t().thumbnail_dialog_saved_sizes()));
            }
            ui.label(format!("{}: {}", i18n::t().thumbnail_dialog_matching(), stats.matching));
            ui.label(format!("{}: {}", i18n::t().thumbnail_dialog_mismatched(), stats.mismatched));
            ui.add_space(8.0);
            ui.label(i18n::t().thumbnail_dialog_preserve_note());
            ui.separator();

            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(i18n::t().thumbnail_dialog_deleting());
                });
            } else if self.thumbnail_delete_confirm_all {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 120, 40),
                    i18n::t().thumbnail_dialog_all_warning(),
                );
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().thumbnail_dialog_back()).clicked() {
                        cancel_delete_all = true;
                    }
                    if ui.button(i18n::t().thumbnail_dialog_delete_all_confirm()).clicked() {
                        confirm_delete_all = true;
                    }
                });
            } else if let Some(finished) = &self.thumbnail_delete_finished {
                if finished.success {
                    ui.label(format!(
                        "{}: {}",
                        i18n::t().thumbnail_dialog_deleted_count(),
                        finished.deleted,
                    ));
                } else {
                    ui.colored_label(egui::Color32::RED, i18n::t().thumbnail_dialog_delete_failed());
                }
                if ui.button(i18n::t().thumbnail_dialog_close()).clicked() {
                    close = true;
                }
            } else {
                if stats.mismatched == 0 {
                    ui.label(i18n::t().thumbnail_dialog_no_mismatch());
                }
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().thumbnail_dialog_cancel()).clicked() {
                        close = true;
                    }
                    if ui.add_enabled(
                        stats.mismatched > 0,
                        egui::Button::new(i18n::t().thumbnail_dialog_delete_mismatched()),
                    ).clicked() {
                        delete_mismatched = true;
                    }
                    if ui.button(i18n::t().thumbnail_dialog_delete_all()).clicked() {
                        request_delete_all = true;
                    }
                });
            }
        });

        if close {
            self.thumbnail_dialog_open = false;
            self.thumbnail_delete_finished = None;
        } else if cancel_delete_all {
            self.thumbnail_delete_confirm_all = false;
        } else if delete_mismatched {
            self.start_thumbnail_delete(ThumbnailDeleteMode::Mismatched);
        } else if request_delete_all {
            self.thumbnail_delete_confirm_all = true;
        } else if confirm_delete_all {
            self.start_thumbnail_delete(ThumbnailDeleteMode::All);
        }
    }
}
