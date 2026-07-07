use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::controller::{self};
use crate::i18n;
use crate::neko_dir;
use crate::fs::archive;
use super::*;

impl NekoviewApp {
    /// ステータスデータを 1Hz throttle で更新する（force 要求があれば即時）。
    /// debug/release 共通。debug 用の追加メトリクスは `frame_dt_ms` を使う。
    fn refresh_status_data(&mut self, frame_dt_ms: f32) {
        let force = self.status_update_requested.swap(false, std::sync::atomic::Ordering::Relaxed);
        let elapsed = self.last_status_update.elapsed();
        if !(force || elapsed >= std::time::Duration::from_secs(1)) {
            return;
        }
        self.last_status_update = std::time::Instant::now();
        let mut data = self.status_window_data.lock().unwrap();
        let page_cache = self.page_cache.lock().unwrap();
        controller::update_status_data(
            &mut data,
            page_cache.total_bytes(),
            page_cache.max_bytes(),
            self.file_cache.total_bytes(),
            self.file_cache.max_bytes(),
        );

        #[cfg(debug_assertions)]
        controller::update_status_data_debug(
            &mut data,
            frame_dt_ms,
            self.thumb_pending.len(),
            self.pending_loads.lock().unwrap().len(),
            self.thumbnails.len(),
            match &self.scan_state {
                ScanState::Idle      => "idle",
                ScanState::Loading { .. } => "loading",
                ScanState::Done      => "done",
            },
        );
        #[cfg(not(debug_assertions))]
        let _ = frame_dt_ms;
    }

    /// ステータス窓（debug 独立 OS 窓 / release ROOT 内フローティング窓）の表示状態。
    /// winit_app が debug ビルドでこの状態に合わせて独立窓を生成/破棄する
    /// （release では sync_status_window が no-op のため未使用）。
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn status_is_open(&self) -> bool {
        self.show_status_window
    }

    /// ステータス窓を閉じる（OS のクローズボタンから winit_app が呼ぶ / debug）。
    pub fn close_status(&mut self) {
        self.show_status_window = false;
    }

    /// debug ビルドのステータス独立窓の 1 フレーム描画。winit_app が status 窓の
    /// egui パスから呼ぶ。データを 1Hz throttle で更新して描画し、
    /// `request_repaint_after(1s)` で自分自身を 1Hz で起こし続ける。
    pub fn render_status(&mut self, ui: &mut egui::Ui) {
        let dt_ms = ui.ctx().input(|i| i.stable_dt) * 1000.0;
        self.refresh_status_data(dt_ms);
        {
            let data = self.status_window_data.lock().unwrap();
            crate::view_status::draw_content(ui, &data);
        }
        // 次の 1Hz tick へ向けて自分自身の再描画を予約する。
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// release ビルド: ROOT 内フローティング `egui::Window` としてステータスを描画する。
    /// debug ビルドでは独立 OS 窓（render_status）が担うため使わない。
    #[cfg(not(debug_assertions))]
    pub(super) fn draw_status_window(&mut self, ctx: &egui::Context) {
        if self.show_status_window {
            self.refresh_status_data(ctx.input(|i| i.stable_dt) * 1000.0);
        }
        crate::view_status::show(
            ctx,
            &mut self.show_status_window,
            &self.status_window_data,
        );
    }

    pub(super) fn draw_toast(&mut self, ctx: &egui::Context) {
        // アプリレベルトースト（3秒で自動消去）
        if let Some((ref msg, since)) = self.app_toast.clone() {
            if since.elapsed().as_secs_f32() < 3.0 {
                egui::Area::new(egui::Id::new("app_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -30.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(egui::Color32::from_rgba_premultiplied(30, 30, 30, 230))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(msg)
                                        .color(egui::Color32::WHITE)
                                        .size(13.0),
                                );
                            });
                    });
                ctx.request_repaint();
            } else {
                self.app_toast = None;
            }
        }
    }

    /// フェーズ2: アーカイブを開く前にメモリ見積もりゲートを通す。
    /// 予算超過と判定した場合は確認ダイアログの表示フラグを立てて false を返す
    /// （呼び出し側はオープンを中止する。invalid_archives への永続マークは行わない。
    /// 一時的な予算状況が変わりうるため、次回オープン時に再度見積もりし直す）。
    /// 7z/tarで判定のために一括展開したデータはそのままFileCacheへ投入し、
    /// オープン後の再展開（二重展開）を回避する。
    pub(super) fn check_memory_budget(&mut self, path: &std::path::Path) -> bool {
        let entries = archive::list_images(path);
        if entries.is_empty() {
            return true; // 空/無効アーカイブの判定は既存の invalid_archives 処理に任せる
        }
        let check = archive::estimate_archive_memory(
            path,
            &entries,
            self.cache_budget_bytes,
            self.anim_ring_bounds,
            self.config.max_decode_edge,
            self.file_cache.max_bytes(),
        );
        match check.estimate {
            archive::ArchiveMemoryEstimate::Ok => {
                if let Some(prepared) = check.prepared {
                    let current = self
                        .viewer
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|v| v.archive_path().clone())
                        .unwrap_or_else(|| path.to_path_buf());
                    self.file_cache.insert(path.to_path_buf(), prepared, &current, &self.archives);
                }
                true
            }
            archive::ArchiveMemoryEstimate::OverBudget => {
                self.memory_warning_open = true;
                false
            }
        }
    }

    /// フェーズ2: メモリ見積もり超過の確認ダイアログを描画する（OKボタンのみ）
    pub(super) fn draw_memory_warning_dialog(&mut self, ctx: &egui::Context) {
        if !self.memory_warning_open {
            return;
        }
        let mut open = true;
        egui::Window::new(i18n::t().memory_warning_title())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().memory_warning_body());
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if ui.button(i18n::t().memory_warning_ok()).clicked() {
                        self.memory_warning_open = false;
                    }
                });
            });
        if !open {
            self.memory_warning_open = false;
        }
    }

    /// ZIPを無効確定してDBにマーカーを書き込む
    pub(super) fn mark_archive_invalid(&mut self, path: &PathBuf) {
        self.invalid_archives.insert(path.clone());
        if let Some(ref db) = self.cache_db {
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let mtime = neko_dir::file_mtime(path);
            neko_dir::mark_invalid(db, filename, mtime);
        }
        self.maybe_check_mount_after_failure(path);
    }

    /// バックグラウンドで進行中のマウント到達可否チェックの結果を回収する。
    pub(super) fn poll_mount_checks(&mut self) {
        let mut still_pending = Vec::new();
        for (root, rx) in self.mount_check_pending.drain(..) {
            match rx.try_recv() {
                Ok((root, reachable)) => {
                    if reachable {
                        self.network_unreachable_mounts.remove(&root);
                    } else {
                        self.network_unreachable_mounts.insert(root);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => still_pending.push((root, rx)),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        self.mount_check_pending = still_pending;
    }

    /// path が既知のネットワークマウント配下にあり、まだチェック中でなければ
    /// 到達可否のバックグラウンド確認を1件発火する（定期ポーリングはしない）。
    fn spawn_mount_check_if_needed(&mut self, root: PathBuf) {
        if self.mount_check_pending.iter().any(|(r, _)| *r == root) {
            return;
        }
        let ctx = self.egui_ctx.clone();
        let rx = crate::fs::mount::spawn_mount_reachability_check(root.clone(), move || ctx.request_repaint());
        self.mount_check_pending.push((root, rx));
    }

    /// サムネ失敗・無効ZIP確定などの開封失敗を検知した際、それがネットワーク
    /// マウント配下のファイルであれば大元の到達可否を確認する（1アクション判定）。
    pub(super) fn maybe_check_mount_after_failure(&mut self, path: &Path) {
        if let Some(root) = crate::fs::mount::network_mount_root(path)
            && !self.network_unreachable_mounts.contains(&root)
        {
            self.spawn_mount_check_if_needed(root);
        }
    }

    /// ファイルを開く直前のゲート。ネットワークマウント配下でなければ常に true。
    /// すでにリンク切れ表示中のマウントに対する明示的なオープン試行の場合は、
    /// ここで再チェックを発火しつつ今回のオープンは保留する（true を返さない）。
    pub(super) fn network_gate(&mut self, path: &Path) -> bool {
        let Some(root) = crate::fs::mount::network_mount_root(path) else {
            return true;
        };
        if self.network_unreachable_mounts.contains(&root) {
            self.spawn_mount_check_if_needed(root);
            self.app_toast = Some((
                i18n::t().network_checking_toast().to_string(),
                std::time::Instant::now(),
            ));
            false
        } else {
            true
        }
    }
}
