//! デバッグタブの「開発用DBツール」（FP管理移行の繰り返しテスト用）。
//! 旧パス仕様DBの基準バックアップ/リストアを行う。ロジックは `crate::dev_db_backup`。
//!
//! 実リリース時はこのファイルごと削除する（`mod dev_db_tools;`・`NekoviewApp::dev_db_ui`・
//! `view_gui_config.rs` の呼び出し2箇所・起動フックも一緒に）。それまでは、リリースビルドでも
//! 動作確認できるよう、チェックボックスで表示を切り替える（デバッグビルドは常時表示）。
//! 文言は開発専用のため i18n へは入れず日本語直書きにしている。

use std::path::Path;

use super::NekoviewApp;
use crate::dev_db_backup::{self as backup, DevBackupError};
use crate::spread_state;

/// リリースビルドでもツールを表示するかを覚えておくマーカーファイル（設定フォルダ直下）。
const VISIBLE_MARKER_FILE: &str = "dev_db_tools.visible";

#[derive(Clone, Copy, PartialEq, Eq)]
enum DevDbDialog {
    /// FP仕様のDBはバックアップ対象外。
    IdentityGuard,
    /// 既存の基準バックアップを上書きしてよいか。
    ConfirmOverwrite,
    /// リストアを予約してよいか。
    ConfirmRestore,
    /// 予約済み。再起動で適用される。
    RestoreScheduled,
}

pub(crate) struct DevDbUiState {
    /// リリースビルドでも表示するか（デバッグビルドでは無視して常時表示）。
    show_in_release: bool,
    dialog: Option<DevDbDialog>,
    /// 直近の操作結果。
    message: Option<String>,
}

impl DevDbUiState {
    pub(crate) fn load(config_root: &Path) -> Self {
        Self {
            show_in_release: config_root.join(VISIBLE_MARKER_FILE).is_file(),
            dialog: None,
            message: None,
        }
    }

    fn visible(&self) -> bool {
        cfg!(debug_assertions) || self.show_in_release
    }
}

fn error_text(e: &DevBackupError) -> String {
    match e {
        DevBackupError::IdentitySpec => "FP仕様のDBのためバックアップ対象外".to_owned(),
        DevBackupError::NoBaseline => "基準バックアップがありません".to_owned(),
        DevBackupError::DbBusy => "DBがロック中です".to_owned(),
        DevBackupError::Io(m) => format!("I/Oエラー: {m}"),
    }
}

/// unix秒をローカル時刻の `YYYY-MM-DD HH:MM:SS` にする（ローカル時差が取れなければUTC）。
fn format_unix(secs: i64) -> String {
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return "-".to_owned();
    };
    let t = time::UtcOffset::current_local_offset()
        .map(|o| utc.to_offset(o))
        .unwrap_or(utc);
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

impl NekoviewApp {
    /// デバッグタブの末尾に「開発用DBツール」の節を描く。
    pub(crate) fn draw_dev_db_tools(&mut self, ui: &mut egui::Ui) {
        let root = self.config.config_root.clone();
        ui.separator();
        ui.label(egui::RichText::new("開発用DBツール（FP管理移行テスト用）").strong());

        // リリースビルドでは、表示切替のチェックボックスだけは常に見せる。
        if !cfg!(debug_assertions) {
            let mut on = self.dev_db_ui.show_in_release;
            if ui
                .checkbox(&mut on, "リリースビルドでもDBツールを表示（実リリース時に削除）")
                .changed()
            {
                self.dev_db_ui.show_in_release = on;
                let marker = root.join(VISIBLE_MARKER_FILE);
                let _ = if on { std::fs::write(&marker, b"") } else { std::fs::remove_file(&marker) };
            }
        }
        if !self.dev_db_ui.visible() {
            return;
        }

        let baseline = backup::baseline_info(&root);
        match baseline {
            Some(b) => ui.label(format!(
                "基準バックアップ: {}（{} KB）",
                format_unix(b.modified_unix),
                b.size / 1024
            )),
            None => ui.label("基準バックアップ: なし"),
        };
        if backup::pending_exists(&root) {
            ui.colored_label(egui::Color32::YELLOW, "復元予約あり（アプリを閉じて再起動すると適用）");
        }

        let identity = self.spread_db.as_ref().map(spread_state::is_identity_spec);
        ui.horizontal(|ui| {
            if ui.button("現DBをバックアップ").clicked() {
                self.dev_db_ui.message = None;
                match identity {
                    None => self.dev_db_ui.message = Some("DB未接続".to_owned()),
                    Some(true) => self.dev_db_ui.dialog = Some(DevDbDialog::IdentityGuard),
                    Some(false) if baseline.is_some() => {
                        self.dev_db_ui.dialog = Some(DevDbDialog::ConfirmOverwrite)
                    }
                    Some(false) => self.run_dev_db_backup(),
                }
            }
            if ui
                .add_enabled(baseline.is_some(), egui::Button::new("バックアップからリストア"))
                .clicked()
            {
                self.dev_db_ui.message = None;
                self.dev_db_ui.dialog = Some(DevDbDialog::ConfirmRestore);
            }
        });

        // Phase 1 でID層が自動で立てるマーカーの代わりに、ガードの動作確認用に手で切り替える。
        if let Some(on) = identity {
            if ui
                .button(format!(
                    "テスト用: FP仕様マーカー切替（現在: {}）",
                    if on { "ON" } else { "OFF" }
                ))
                .clicked()
            {
                if let Some(db) = &self.spread_db {
                    let ok = spread_state::set_identity_spec(db, !on);
                    self.dev_db_ui.message = Some(if ok {
                        format!("FP仕様マーカーを{}にしました", if on { "OFF" } else { "ON" })
                    } else {
                        "FP仕様マーカーの切替に失敗".to_owned()
                    });
                }
            }
        }

        if let Some(m) = &self.dev_db_ui.message {
            ui.label(m);
        }
    }

    fn run_dev_db_backup(&mut self) {
        let Some(db) = self.spread_db.clone() else {
            self.dev_db_ui.message = Some("DB未接続".to_owned());
            return;
        };
        let root = self.config.config_root.clone();
        self.dev_db_ui.message = Some(match backup::backup(&db, &root) {
            Ok(info) => format!("バックアップしました（{} KB）", info.size / 1024),
            Err(DevBackupError::IdentitySpec) => {
                // ダイアログ表示後にFP仕様へ変わった場合の保険。
                self.dev_db_ui.dialog = Some(DevDbDialog::IdentityGuard);
                return;
            }
            Err(e) => format!("バックアップ失敗: {}", error_text(&e)),
        });
    }

    fn run_dev_db_restore_request(&mut self) {
        let root = self.config.config_root.clone();
        match backup::request_restore(&root) {
            Ok(()) => self.dev_db_ui.dialog = Some(DevDbDialog::RestoreScheduled),
            Err(e) => self.dev_db_ui.message = Some(format!("リストア予約失敗: {}", error_text(&e))),
        }
    }

    /// 確認・ガードのダイアログ。設定ダイアログ（`egui::Modal`）の上に重ねるため、設定側の
    /// Modal を描いた後に呼ぶ。
    pub(crate) fn draw_dev_db_dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dev_db_ui.dialog else { return };
        let root = self.config.config_root.clone();
        let mut next: Option<Option<DevDbDialog>> = None; // Some(x) = dialog を x にする
        let mut run_backup = false;
        let mut run_restore = false;

        egui::Modal::new(egui::Id::new("dev_db_tools_dialog")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            match dialog {
                DevDbDialog::IdentityGuard => {
                    ui.heading("バックアップ対象外");
                    ui.label(
                        "このDBはFP仕様（ID層を使用済み）のため、バックアップできません。\n\
                         旧パス仕様DBへ戻すための基準にならないためです。",
                    );
                    if ui.button("閉じる").clicked() {
                        next = Some(None);
                    }
                }
                DevDbDialog::ConfirmOverwrite => {
                    ui.heading("基準バックアップを上書き");
                    ui.label("既存の基準バックアップを、現在のDBの内容で上書きします。");
                    ui.horizontal(|ui| {
                        if ui.button("キャンセル").clicked() {
                            next = Some(None);
                        }
                        if ui.button("上書きしてバックアップ").clicked() {
                            next = Some(None);
                            run_backup = true;
                        }
                    });
                }
                DevDbDialog::ConfirmRestore => {
                    ui.heading("バックアップからリストア");
                    if let Some(b) = backup::baseline_info(&root) {
                        ui.label(format!("基準バックアップ（{}）へ戻します。", format_unix(b.modified_unix)));
                    }
                    ui.label(
                        "現在のDBは dev_backup/failed/ へ退避します。\n\
                         起動中のDBは差し替えず、アプリを閉じて再起動したときに適用されます。",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("キャンセル").clicked() {
                            next = Some(None);
                        }
                        if ui.button("復元を予約").clicked() {
                            next = Some(None);
                            run_restore = true;
                        }
                    });
                }
                DevDbDialog::RestoreScheduled => {
                    ui.heading("復元を予約しました");
                    ui.label("アプリのウィンドウを閉じて、再起動してください。起動時に復元が適用されます。");
                    if ui.button("閉じる").clicked() {
                        next = Some(None);
                    }
                }
            }
        });

        if let Some(n) = next {
            self.dev_db_ui.dialog = n;
        }
        if run_backup {
            self.run_dev_db_backup();
        }
        if run_restore {
            self.run_dev_db_restore_request();
        }
    }
}
