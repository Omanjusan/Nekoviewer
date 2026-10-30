//! 設定→エクスプローラー「データベース管理」の、バックアップ世代の操作（手動作成・一覧・復元の予約）。
//!
//! 本体は `db_backup.rs`。復元は起動中のDBを差し替えず、予約して次回起動時に適用する。

use super::*;
use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::db_backup::{self, Generation};
use crate::i18n;

/// unix秒をローカル時刻の `YYYY-MM-DD HH:MM:SS` にする（ローカル時差が取れなければUTC）。
fn format_unix(secs: i64) -> String {
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return "-".to_owned();
    };
    let t = time::UtcOffset::current_local_offset().map(|o| utc.to_offset(o)).unwrap_or(utc);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

impl NekoviewApp {
    /// 「今すぐバックアップ」ボタン、復元の予約状況、世代の一覧、復元の確認ダイアログを描く。
    pub(crate) fn draw_db_backup_section(&mut self, ui: &mut egui::Ui) {
        let root = self.config.config_root.clone();
        let t = i18n::t();

        ui.add_space(4.0);
        let mut backup_now = false;
        if ui
            .add_enabled(self.spread_db.is_some(), egui::Button::new(t.settings_backup_now_button()))
            .clicked()
        {
            backup_now = true;
        }
        if backup_now {
            self.create_backup_now(&root);
        }

        // 復元の予約状況。予約中は取り消せる。
        if db_backup::restore_pending(&root) {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, t.settings_backup_restore_pending());
                if ui.button(t.settings_backup_restore_cancel()).clicked() {
                    match db_backup::cancel_restore(&root) {
                        Ok(()) => self.set_toast(t.settings_backup_restore_cancelled_toast()),
                        Err(e) => self.set_toast(t.settings_backup_failed_toast(&e.to_string())),
                    }
                }
            });
        }

        ui.label(t.settings_backup_generations_list_heading());
        let generations = db_backup::list_generations(&root);
        if generations.is_empty() {
            ui.label(t.settings_backup_generations_none());
        }
        let mut chosen: Option<String> = None;
        for g in &generations {
            ui.horizontal(|ui| {
                ui.label(format!("{}  ({})", format_unix(g.modified_unix), format_size(g.size)));
                if ui.button(t.settings_backup_restore_button()).clicked() {
                    chosen = Some(g.name.clone());
                }
            });
        }
        if let Some(name) = chosen {
            self.db_restore_confirm = Some(name);
        }
        ui.label(t.settings_backup_generations_explain());

        self.draw_db_restore_confirm(ui.ctx(), &root, &generations);
    }

    fn create_backup_now(&mut self, root: &std::path::Path) {
        let Some(db) = self.spread_db.clone() else { return };
        let keep = self.config.backup_generations as usize;
        match db_backup::create_generation(&db, root, keep) {
            Ok(_) => self.set_toast(i18n::t().settings_backup_done_toast()),
            Err(e) => self.set_toast(i18n::t().settings_backup_failed_toast(&format!("{e:?}"))),
        }
    }

    fn draw_db_restore_confirm(&mut self, ctx: &egui::Context, root: &std::path::Path, generations: &[Generation]) {
        let Some(name) = self.db_restore_confirm.clone() else { return };
        // 一覧から消えた世代（ローテーション等）の確認は、黙って閉じる。
        let Some(g) = generations.iter().find(|g| g.name == name) else {
            self.db_restore_confirm = None;
            return;
        };
        let t = i18n::t();
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: "db_restore_confirm_window",
                title: t.settings_backup_restore_confirm_title(),
                ok_label: t.settings_backup_restore_ok(),
                cancel_label: t.favorite_dialog_cancel(),
                foreground: true,
            },
            |ui| {
                ui.label(t.settings_backup_restore_confirm_body(&format_unix(g.modified_unix)));
            },
        );
        match outcome {
            ConfirmOutcome::None => {}
            ConfirmOutcome::Cancel => self.db_restore_confirm = None,
            ConfirmOutcome::Ok => {
                self.db_restore_confirm = None;
                match db_backup::request_restore(root, &name) {
                    Ok(()) => self.set_toast(t.settings_backup_restore_requested_toast()),
                    Err(e) => self.set_toast(t.settings_backup_failed_toast(&format!("{e:?}"))),
                }
            }
        }
    }
}
