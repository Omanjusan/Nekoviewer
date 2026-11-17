//! タグ管理画面の「危険な設定」窓。取り返しのつきにくい操作（タグのクリア・リカバリポイントの設定）をまとめる。
//!
//! - ボタンはタイトルのロールバックボタンの右隣。窓の外をクリックしたとき・もう一度ボタンを押したときに閉じる
//! - 試用中（一括適用の仮マスターを編集中）は押せない
//! - タグのクリア（D2）: 2段階の確認のあと、DB世代とマスター世代を自動で取り、マスターを初期状態へ戻し、全ファイルの
//!   タグ付けを消す。評価・お気に入り・学習データは消さない。`next_tier_id` は巻き戻さない（孤立IDの再利用を防ぐため）
//! - リカバリポイント（D3）: マスターとDB全体を固定枠1つに保存する（`tag_recovery.rs`）。上書きと削除は確認つき。
//!   復元はロールバック窓の一覧の最下段から行う（次回起動時に、マスターとDBをまとめて戻す）

use super::*;
use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::i18n;

impl NekoviewApp {
    /// 窓を開く。開いていれば閉じる。
    pub(super) fn toggle_tag_danger_window(&mut self) {
        self.tag_danger_open = !self.tag_danger_open;
    }

    /// 「危険な設定」窓を描く。窓の外を押したら閉じる（ボタン自体の押下はボタン側で切り替える）。
    pub(super) fn draw_tag_danger_window(&mut self, ctx: &egui::Context) {
        if !self.tag_danger_open {
            return;
        }
        let t = i18n::t();
        let mut open = true;
        let shown = egui::Window::new(egui::RichText::new(t.tag_danger_title()).color(egui::Color32::from_rgb(230, 90, 80)))
            .id(egui::Id::new("tag_danger_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                // タグのクリア（D2で実装）。
                ui.label(egui::RichText::new(t.tag_danger_clear_heading()).strong());
                ui.label(t.tag_danger_clear_explain());
                let can_clear = self.spread_db.is_some() && self.tag_apply_job.is_none();
                let clear = ui.add_enabled(can_clear, egui::Button::new(t.tag_danger_clear_button()));
                if !can_clear {
                    clear.on_disabled_hover_text(t.tag_danger_clear_unavailable());
                } else if clear.clicked() {
                    self.tag_danger_clear_stage = 1;
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                // リカバリポイント（D3で実装）。
                ui.label(egui::RichText::new(t.tag_danger_recovery_heading()).strong());
                ui.label(t.tag_danger_recovery_explain());
                let root = self.config.config_root.clone();
                let info = crate::tag_recovery::info(&root);
                match &info {
                    Some(i) => ui.label(t.tag_danger_recovery_status(
                        &super::db_backup_ui::format_unix(i.modified_unix),
                        &super::db_backup_ui::format_size(i.db_size),
                        &super::db_backup_ui::format_size(i.master_size),
                    )),
                    None => ui.weak(t.tag_danger_recovery_none()),
                };
                let can_set = self.spread_db.is_some() && !self.tag_trial_active && self.tag_apply_job.is_none();
                ui.horizontal(|ui| {
                    let set = ui.add_enabled(can_set, egui::Button::new(t.tag_danger_recovery_set_button()));
                    if !can_set {
                        set.on_disabled_hover_text(t.tag_danger_clear_unavailable());
                    } else if set.clicked() {
                        if info.is_some() {
                            self.tag_danger_recovery_ask = 1;
                        } else {
                            self.run_recovery_set();
                        }
                    }
                    if ui.add_enabled(info.is_some(), egui::Button::new(t.tag_danger_recovery_delete_button())).clicked() {
                        self.tag_danger_recovery_ask = 2;
                    }
                });
            });
        if !open {
            self.tag_danger_open = false;
            self.tag_danger_clear_stage = 0;
            self.tag_danger_recovery_ask = 0;
            return;
        }
        // 確認ダイアログの表示中は、窓の外クリックで閉じない（ダイアログの中を押しても閉じてしまうため）。
        if self.tag_danger_clear_stage != 0 {
            self.draw_tag_clear_confirm(ctx);
            return;
        }
        if self.tag_danger_recovery_ask != 0 {
            self.draw_tag_recovery_confirm(ctx);
            return;
        }
        if ctx.input(|i| i.pointer.primary_pressed()) {
            let win_rect = shown.map(|r| r.response.rect);
            let pos = ctx.input(|i| i.pointer.interact_pos());
            let inside = pos.is_some_and(|p| win_rect.is_some_and(|r| r.contains(p)) || self.tag_danger_btn_rect.contains(p));
            if !inside {
                self.tag_danger_open = false;
            }
        }
    }

    /// リカバリポイントの上書き・削除の確認ダイアログ。
    fn draw_tag_recovery_confirm(&mut self, ctx: &egui::Context) {
        let t = i18n::t();
        let setting = self.tag_danger_recovery_ask == 1;
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: "tag_recovery_confirm",
                title: if setting { t.tag_danger_recovery_overwrite_title() } else { t.tag_danger_recovery_delete_title() },
                ok_label: if setting { t.tag_danger_recovery_overwrite_ok() } else { t.tag_danger_recovery_delete_ok() },
                cancel_label: t.favorite_dialog_cancel(),
                foreground: true,
            },
            |ui| {
                ui.set_max_width(440.0);
                ui.label(if setting { t.tag_danger_recovery_overwrite_body() } else { t.tag_danger_recovery_delete_body() });
            },
        );
        match outcome {
            ConfirmOutcome::None => {}
            ConfirmOutcome::Cancel => self.tag_danger_recovery_ask = 0,
            ConfirmOutcome::Ok => {
                self.tag_danger_recovery_ask = 0;
                if setting {
                    self.run_recovery_set();
                } else {
                    match crate::tag_recovery::delete(&self.config.config_root) {
                        Ok(()) => self.set_toast(t.tag_danger_recovery_deleted_toast()),
                        Err(e) => self.set_toast(t.settings_backup_failed_toast(&e.to_string())),
                    }
                }
            }
        }
    }

    /// いまのマスターとDBを、リカバリポイントとして保存する（既にあれば置き換える）。
    fn run_recovery_set(&mut self) {
        let t = i18n::t();
        let Some(db) = self.spread_db.clone() else { return };
        // マスターが未保存（初回など）でもコピーできるよう、先にファイルへ保存しておく。
        self.save_tag_master_file();
        match crate::tag_recovery::set(&db, &self.config.config_root) {
            Ok(_) => self.set_toast(t.tag_danger_recovery_set_toast()),
            Err(e) => self.set_toast(t.settings_backup_failed_toast(&format!("{e:?}"))),
        }
    }

    /// タグのクリアの確認ダイアログ（1段目: 内容、2段目: 最終確認）。2段目のOKで実行する。
    fn draw_tag_clear_confirm(&mut self, ctx: &egui::Context) {
        let t = i18n::t();
        let (title, body, ok) = if self.tag_danger_clear_stage == 1 {
            (t.tag_danger_clear_confirm1_title(), t.tag_danger_clear_confirm1_body(), t.tag_danger_clear_next())
        } else {
            (t.tag_danger_clear_confirm2_title(), t.tag_danger_clear_confirm2_body(), t.tag_danger_clear_run())
        };
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: if self.tag_danger_clear_stage == 1 { "tag_clear_confirm_1" } else { "tag_clear_confirm_2" },
                title,
                ok_label: ok,
                cancel_label: t.favorite_dialog_cancel(),
                foreground: true,
            },
            |ui| {
                ui.set_max_width(460.0);
                ui.label(body);
            },
        );
        match outcome {
            ConfirmOutcome::None => {}
            ConfirmOutcome::Cancel => self.tag_danger_clear_stage = 0,
            ConfirmOutcome::Ok if self.tag_danger_clear_stage == 1 => self.tag_danger_clear_stage = 2,
            ConfirmOutcome::Ok => {
                self.tag_danger_clear_stage = 0;
                match self.run_tag_clear() {
                    Ok(records) => {
                        self.tag_danger_open = false;
                        self.set_toast(t.tag_danger_cleared_toast(records));
                    }
                    Err(msg) => self.set_toast(msg),
                }
            }
        }
    }

    /// タグをクリアする。手順: ①DB世代を作る（失敗したら何も変えない）②マスター世代を残す③全ファイルのタグ付けを消す
    /// （失敗したら何も変えない）④マスターを初期状態へ戻す。成功したら、タグが入っていた記録の数を返す。
    fn run_tag_clear(&mut self) -> Result<usize, String> {
        let t = i18n::t();
        let Some(db) = self.spread_db.clone() else { return Err(t.tag_danger_clear_unavailable().to_string()) };
        if self.tag_trial_active || self.tag_apply_job.is_some() {
            return Err(t.tag_danger_clear_unavailable().to_string());
        }
        let root = self.config.config_root.clone();
        if let Err(e) = crate::db_backup::create_generation(&db, &root, self.config.backup_generations as usize) {
            return Err(t.settings_backup_failed_toast(&format!("{e:?}")));
        }
        crate::tag_master_gen::snapshot_before_save(&root, self.tag_master_gen_keep(), true);
        let Some(cleared) = crate::file_settings::clear_all_tags(&db) else {
            return Err(t.tag_danger_clear_failed_toast().to_string());
        };

        // マスターを初期状態へ。要素の番号は巻き戻さず、初期要素にも新しい番号を振る（古いIDの再利用を避ける）。
        let (mut cats, _) = crate::tag_manager::default_state();
        let mut next = self.tag_manager_next_tier_id.max(1);
        for tier in cats.iter_mut().flat_map(|c| c.tiers.iter_mut()) {
            tier.id = next;
            next += 1;
        }
        crate::tag_master::TagMaster::new(&mut cats, &mut next).ensure_system_categories();
        self.tag_manager_categories = cats;
        self.tag_manager_next_tier_id = next;
        self.tag_manager_selected_category = Some(0);
        self.tag_manager_bulk_input.clear();
        self.tag_manager_delete_confirm = None;
        self.tag_master_gen_ui = None;
        self.tag_attr_selected.clear();
        self.tag_attr_dormant.clear();
        self.tag_search_enabled.clear();
        self.save_tag_manager();
        // 選択中のファイルのタグ表示を読み直す。
        self.tag_binding_path = None;
        Ok(cleared.records + cleared.legacy_rows)
    }
}
