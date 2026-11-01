//! 計測フィクスチャーのUI（カード右クリックのメニューと確認ダイアログ）。開発専用・日本語直書き。
//!
//! 設定→デバッグの「サムネ周りのリファクタリング用ログ」がONの間だけ、メニューに出る。
//! クリア対象は表示中フォルダ直下のファイルのID層とサムネ。実行前に spread DB を丸ごと退避し
//! （初回のみ保持、`perf_fixture_db`）、「DB消去前に戻す」は再起動で適用する。
//! 計測の中身は `perf_fixture`。

use super::NekoviewApp;
use crate::perf_fixture::{self, Mode};
use crate::perf_fixture_db as pdb;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialog {
    ConfirmClear(Mode),
    ConfirmRestore,
    RestoreScheduled,
    Message,
}

#[derive(Default)]
pub(super) struct PerfFixtureUi {
    dialog: Option<Dialog>,
    message: Option<String>,
}

impl NekoviewApp {
    /// ファイルカードの右クリックメニューの末尾に足す項目。実フォルダ表示中だけ。
    pub(super) fn draw_perf_fixture_menu(&mut self, ui: &mut egui::Ui) {
        if !perf_fixture::enabled()
            || !self.folder_pane_tab.shows_real_dir()
            || self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
            || self.viewing_similar.is_some()
            || self.viewing_dir.is_none()
        {
            return;
        }
        ui.separator();
        ui.label(egui::RichText::new("計測フィクスチャー（開発用）").weak());
        if ui.button("このフォルダのDBをクリアし計測を開始する（サムネのみ）").clicked() {
            self.perf_fixture_ui.dialog = Some(Dialog::ConfirmClear(Mode::ThumbOnly));
            ui.close();
        }
        if ui.button("このフォルダのDBをクリアし計測を開始する（フルサポート）").clicked() {
            self.perf_fixture_ui.dialog = Some(Dialog::ConfirmClear(Mode::Full));
            ui.close();
        }
        if pdb::restore_pending(&self.config.config_root) {
            ui.colored_label(egui::Color32::YELLOW, "復元予約あり（再起動で適用）");
        }
        let has_backup = pdb::has_backup(&self.config.config_root);
        if ui.add_enabled(has_backup, egui::Button::new("DB消去前に戻す")).clicked() {
            self.perf_fixture_ui.dialog = Some(Dialog::ConfirmRestore);
            ui.close();
        }
    }

    pub(super) fn draw_perf_fixture_dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.perf_fixture_ui.dialog else { return };
        let mut next: Option<Option<Dialog>> = None;
        let mut run_clear: Option<Mode> = None;
        let mut run_restore = false;
        let root = self.config.config_root.clone();

        egui::Modal::new(egui::Id::new("perf_fixture_dialog")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            match dialog {
                Dialog::ConfirmClear(mode) => {
                    ui.heading("DBをクリアして計測");
                    let dir = self.viewing_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
                    ui.label(format!("対象: {dir}\n（直下のファイル {} 件。サブフォルダは対象外）", self.archives.len()));
                    ui.label(
                        "これらのファイルのID・評価・しおり・お気に入り・重複マーカー・dHash・サムネを消します。\n\
                         実行前にDB全体を perf_fixture/before.redb へ退避します（復元するまで初回のみ保持）。\n\
                         「DB消去前に戻す」は再起動で適用され、他のフォルダで計測後に行った変更も退避時点に戻ります。",
                    );
                    if pdb::has_backup(&root) {
                        ui.label("退避は既にあるため、上書きしません。");
                    }
                    ui.horizontal(|ui| {
                        if ui.button("キャンセル").clicked() {
                            next = Some(None);
                        }
                        if ui.button("クリアして計測開始").clicked() {
                            next = Some(None);
                            run_clear = Some(mode);
                        }
                    });
                }
                Dialog::ConfirmRestore => {
                    ui.heading("DB消去前に戻す");
                    ui.label(
                        "最初のクリア前に退避したDBへ戻します。現在のDBは perf_fixture/after-measure.redb へ退避します。\n\
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
                Dialog::RestoreScheduled => {
                    ui.heading("復元を予約しました");
                    ui.label("アプリのウィンドウを閉じて、再起動してください。起動時に復元が適用されます。");
                    if ui.button("閉じる").clicked() {
                        next = Some(None);
                    }
                }
                Dialog::Message => {
                    ui.label(self.perf_fixture_ui.message.as_deref().unwrap_or(""));
                    if ui.button("閉じる").clicked() {
                        next = Some(None);
                    }
                }
            }
        });

        if let Some(n) = next {
            self.perf_fixture_ui.dialog = n;
        }
        if let Some(mode) = run_clear {
            self.run_perf_fixture_clear(mode);
        }
        if run_restore {
            match pdb::request_restore(&root) {
                Ok(()) => {
                    perf_fixture::cancel();
                    self.perf_fixture_ui.dialog = Some(Dialog::RestoreScheduled);
                }
                Err(e) => self.perf_fixture_message(format!("復元の予約に失敗: {e:?}")),
            }
        }
    }

    fn perf_fixture_message(&mut self, text: String) {
        self.perf_fixture_ui.message = Some(text);
        self.perf_fixture_ui.dialog = Some(Dialog::Message);
    }

    /// 退避 → ID層とサムネのクリア → 計測開始 → 再スキャン。
    fn run_perf_fixture_clear(&mut self, mode: Mode) {
        let (Some(db), Some(dir)) = (self.spread_db.clone(), self.viewing_dir.clone()) else {
            self.perf_fixture_message("DBまたは表示中フォルダがありません".to_owned());
            return;
        };
        let names: Vec<String> = self
            .archives
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .collect();
        if names.is_empty() {
            self.perf_fixture_message("直下にファイルがありません".to_owned());
            return;
        }
        let root = self.config.config_root.clone();
        if let Err(e) = pdb::ensure_backup(&db, &root) {
            // 退避できないままクリアしない。
            self.perf_fixture_message(format!("DBの退避に失敗したため中止: {e:?}"));
            return;
        }
        let cleared = pdb::clear_dir_ids(&db, &dir);
        if let Some(cache_db) = &self.cache_db {
            crate::neko_dir::remove_thumbs(cache_db, &names);
        }
        perf_fixture::start(mode, dir.display().to_string(), names.len());
        println!("[perf-fixture] クリア: ID {cleared} 件 / サムネ {} 件", names.len());
        self.start_scan();
    }
}
