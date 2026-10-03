//! 起動時の自動バックアップ（`dev_db_backup::ensure_pre_migration_backup`）の失敗を知らせ、続行するかを選ばせる。
//!
//! 失敗時は、DBを開いてもID層を使い始めず、`migrate_markers`（DBの中身を書き換える起動時の移行）も
//! 保留したまま起動する。ダイアログで選ぶまで、ユーザーのデータは移行されない。
//! - 旧パス仕様のDB: 続行（バックアップ無しで移行）／ID層なしで続行（今回の起動のみ）／終了
//! - FP仕様のDB: 続行／終了。ID層は既に使われているため、OFFにすると書いたデータが見えなくなる

use std::path::Path;
use std::sync::{Arc, Mutex};

use redb::Database;

use super::{NekoviewApp, FAVORITE_MARKER_MIGRATION};
use crate::dev_db_backup::{self, DevBackupError, PreMigrationBackup};
use crate::i18n;

/// 確認待ちのバックアップ失敗。
pub(super) struct BackupFailure {
    /// 失敗の理由（OSのエラー文字列など）。
    reason: String,
    /// DBが既にFP仕様か。true なら「ID層なしで続行」を出さない。
    identity_spec: bool,
}

fn failure_reason(e: &DevBackupError) -> String {
    match e {
        DevBackupError::Io(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// 起動時のDBの準備。保留したバックアップ失敗があれば一緒に返す。
/// 復元予約の適用 → 自動バックアップ → DBを開く → テーブル作成 → （失敗が無ければ）マーカー移行、の順。
pub(super) fn open_spread_db_with_backup(config_root: &Path) -> (Option<Arc<Mutex<Database>>>, Option<BackupFailure>) {
    // 開発用DBツールの復元予約は、DBを開く前に適用する（実リリース時に削除）。
    match dev_db_backup::apply_pending_restore(config_root) {
        Ok(true) => crate::log_common!("[dev_db] 復元予約を適用した"),
        Ok(false) => {}
        Err(e) => crate::log_common!("[dev_db] 復元予約の適用に失敗（現DBのまま起動）: {:?}", e),
    }
    // FP仕様への移行前の自動バックアップ。DBを開くとロックされ、開いた直後から書き換わるため、
    // 開く前にファイルをコピーする（1回きり）。
    let backup = dev_db_backup::ensure_pre_migration_backup(config_root);
    match &backup {
        Ok(PreMigrationBackup::Created(p)) => crate::log_common!("[backup] 移行前の自動バックアップを作成: {}", p.display()),
        Ok(_) => {}
        Err(e) => crate::log_common!("[backup] 移行前の自動バックアップに失敗: {:?}", e),
    }
    let failed_reason = backup.err().map(|e| failure_reason(&e));
    if failed_reason.is_some() {
        // ダイアログで選ぶまで、ID層は使い始めない。
        crate::file_identity::set_enabled(false);
    }
    let db = crate::spread_state::open_spread_db(config_root);
    if let Some(db) = &db {
        crate::favorites::init_favorite_tables(db);
        crate::virtual_folders::init_virtual_folder_tables(db);
        if failed_reason.is_none() {
            // 候補刷新で廃止した空洞・豆腐マーカーを塗り版へ一括移行
            crate::favorites::migrate_markers(db, FAVORITE_MARKER_MIGRATION);
        }
    }
    let failure = failed_reason.map(|reason| {
        let identity_spec = db.as_ref().is_some_and(crate::spread_state::is_identity_spec);
        if identity_spec {
            // 既にID層を使っているDBは、保留中も止めない（止めると v2 のデータが見えなくなる）。
            crate::file_identity::set_enabled(true);
        }
        BackupFailure { reason, identity_spec }
    });
    (db, failure)
}

enum Choice {
    /// バックアップ無しでID層を有効にして続行する。
    Continue,
    /// ID層なしで続行する（今回の起動のみ）。
    ContinueWithoutIdentity,
    Quit,
}

impl NekoviewApp {
    /// 毎フレーム、確認待ちがあればダイアログを出す（`egui::Modal` で背後の操作を遮る）。
    pub(super) fn draw_backup_failure_dialog(&mut self, ctx: &egui::Context) {
        let Some(failure) = &self.backup_failure else { return };
        let t = i18n::t();
        let mut choice = None;
        egui::Modal::new(egui::Id::new("backup_failure_dialog")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(t.bkfail_title());
            ui.add_space(6.0);
            ui.add(egui::Label::new(t.bkfail_body(failure.identity_spec)).wrap());
            ui.add_space(6.0);
            ui.add(egui::Label::new(format!("{}{}", t.bkfail_reason_label(), failure.reason)).wrap());
            ui.add_space(10.0);
            if ui.button(t.bkfail_continue(failure.identity_spec)).clicked() {
                choice = Some(Choice::Continue);
            }
            if !failure.identity_spec && ui.button(t.bkfail_continue_without_identity()).clicked() {
                choice = Some(Choice::ContinueWithoutIdentity);
            }
            if ui.button(t.bkfail_quit()).clicked() {
                choice = Some(Choice::Quit);
            }
        });
        let Some(choice) = choice else { return };
        self.backup_failure = None;
        match choice {
            Choice::Continue => {
                crate::file_identity::set_enabled(true);
                self.finish_deferred_startup();
                // 保留中に見送った、表示中フォルダの解決を始める。
                self.start_identity_resolution();
            }
            Choice::ContinueWithoutIdentity => {
                // ID層はOFFのまま。旧パス仕様のまま動かす。
                self.finish_deferred_startup();
            }
            Choice::Quit => self.exit_requested = true,
        }
    }

    /// 保留していた起動時の移行（マーカー移行）を実行する。
    fn finish_deferred_startup(&mut self) {
        if let Some(db) = &self.spread_db {
            crate::favorites::migrate_markers(db, FAVORITE_MARKER_MIGRATION);
        }
    }

    /// アプリ側から終了を要求されたか（`winit_app` が毎フレーム後に確認する）。
    pub fn take_exit_request(&mut self) -> bool {
        std::mem::take(&mut self.exit_requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spread_state::{is_identity_spec, write_archive_rating};

    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_bkfail_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn startup_without_failure_leaves_layer_enabled() {
        let t = TempRoot::new("ok");
        let (db, failure) = open_spread_db_with_backup(&t.0);
        assert!(db.is_some() && failure.is_none());
        assert!(crate::file_identity::is_enabled());
    }

    #[test]
    fn backup_failure_on_legacy_db_disables_layer_until_user_chooses() {
        let t = TempRoot::new("legacy_fail");
        let db = crate::spread_state::open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, std::path::Path::new("/x"), "a.zip", 3));
        drop(db);
        // 保存先をファイルにして、バックアップを失敗させる。
        std::fs::write(dev_db_backup::auto_backup_dir(&t.0), b"not a dir").unwrap();

        let (db, failure) = open_spread_db_with_backup(&t.0);
        let failure = failure.expect("failure is reported");
        assert!(!failure.identity_spec && !failure.reason.is_empty());
        assert!(!crate::file_identity::is_enabled());
        assert!(!is_identity_spec(&db.unwrap()));
    }

    #[test]
    fn backup_failure_on_identity_spec_db_keeps_layer_enabled() {
        let t = TempRoot::new("fp_fail");
        let db = crate::spread_state::open_spread_db(&t.0).unwrap();
        assert!(crate::spread_state::set_identity_spec(&db, true));
        drop(db);
        std::fs::write(dev_db_backup::auto_backup_dir(&t.0), b"not a dir").unwrap();

        let (_db, failure) = open_spread_db_with_backup(&t.0);
        assert!(failure.expect("failure is reported").identity_spec);
        assert!(crate::file_identity::is_enabled());
    }

    #[test]
    fn dialog_texts_exist_in_all_languages() {
        for lang in [i18n::Lang::Japanese, i18n::Lang::English, i18n::Lang::Chinese] {
            for spec in [false, true] {
                assert!(!lang.bkfail_body(spec).is_empty() && !lang.bkfail_continue(spec).is_empty());
            }
            assert!(!lang.bkfail_title().is_empty() && !lang.bkfail_reason_label().is_empty());
            assert!(!lang.bkfail_continue_without_identity().is_empty() && !lang.bkfail_quit().is_empty());
        }
    }
}
