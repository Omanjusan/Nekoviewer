//! 1.11 の起動時の注意ダイアログ（ファイルID管理への移行の案内）。判定は [release_notice.rs](crate::release_notice)。
//!
//! 旧版からの更新時に1回だけ出す。自動バックアップは DB を開く前に済んでおり（`backup_failure_ui`）、
//! 失敗した場合はその確認ダイアログが先に出る。OKを押すと前回起動バージョンを今のものへ更新する
//! （state への保存は終了時）。

use super::NekoviewApp;
use crate::i18n;

impl NekoviewApp {
    /// 毎フレーム、表示待ちがあればダイアログを出す（`egui::Modal` で背後の操作を遮る）。
    pub(super) fn draw_release_notice_dialog(&mut self, ctx: &egui::Context) {
        // バックアップ失敗の確認が済むまでは出さない。
        if !self.release_notice_pending || self.backup_failure.is_some() {
            return;
        }
        let t = i18n::t();
        let mut ok = false;
        egui::Modal::new(egui::Id::new("release_notice_dialog")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(t.relnotice_title());
            ui.add_space(8.0);
            ui.add(egui::Label::new(t.relnotice_body()).wrap());
            ui.add_space(8.0);
            ui.add(egui::Label::new(egui::RichText::new(t.relnotice_backup_hint()).weak()).wrap());
            ui.add_space(10.0);
            if ui.button(t.relnotice_ok()).clicked() {
                ok = true;
            }
        });
        if ok {
            self.release_notice_pending = false;
            self.config.last_run_version = env!("CARGO_PKG_VERSION").to_string();
        }
    }
}
