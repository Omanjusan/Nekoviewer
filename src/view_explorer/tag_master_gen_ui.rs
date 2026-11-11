//! タグ管理画面の「世代」窓。タグマスターの履歴（`tag_master_gen.rs`）を一覧から選んで、その状態に戻す。
//!
//! 復帰の手順: ①いまのマスターを世代に残す ②DBの世代を1つ作る（失敗したら何も変えない）③選んだ世代でマスターを置き換える。
//! ファイルに付いたタグ（DB）は変えない。復帰した世代に無い要素のタグは、マスターに無いので使えなくなる（DBには残る）。
//! 試用中は仮マスターを編集中なので戻せない。

use super::*;
use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::i18n;
use crate::tag_master_gen::{self, MasterGen};

/// 世代窓の状態。
pub(super) struct GenWindow {
    gens: Vec<MasterGen>,
    /// 一覧で選んでいる世代のファイル名。
    selected: Option<String>,
    /// 「この世代に戻す」の確認待ちか。
    confirming: bool,
}

impl NekoviewApp {
    /// 世代窓を開く（一覧は開くたびに読み直す）。開いていれば閉じる。
    pub(super) fn toggle_tag_master_gen_window(&mut self) {
        if self.tag_master_gen_ui.is_some() {
            self.tag_master_gen_ui = None;
            return;
        }
        let gens = tag_master_gen::list(&self.config.config_root);
        self.tag_master_gen_ui = Some(GenWindow { gens, selected: None, confirming: false });
    }

    /// 世代窓と、復帰の確認ダイアログを描く。
    pub(super) fn draw_tag_master_gen_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.tag_master_gen_ui.take() else { return };
        let t = i18n::t();
        let mut open = true;
        let mut ask_restore = false;
        egui::Window::new(t.tag_gen_title())
            .id(egui::Id::new("tag_master_gen_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(t.tag_gen_explain());
                ui.add_space(4.0);
                ui.add_enabled(false, egui::Button::selectable(false, t.tag_gen_current()));
                if state.gens.is_empty() {
                    ui.weak(t.tag_gen_empty());
                }
                egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                    for g in &state.gens {
                        let selected = state.selected.as_deref() == Some(g.name.as_str());
                        let label = format!(
                            "{}   {}",
                            super::db_backup_ui::format_unix(g.modified_unix),
                            super::db_backup_ui::format_size(g.size)
                        );
                        if ui.selectable_label(selected, label).clicked() {
                            state.selected = Some(g.name.clone());
                        }
                    }
                });
                ui.add_space(6.0);
                let can_restore = state.selected.is_some() && !self.tag_trial_active;
                let resp = ui.add_enabled(can_restore, egui::Button::new(t.tag_gen_restore()));
                if self.tag_trial_active {
                    resp.on_disabled_hover_text(t.tag_gen_trial_tip());
                } else if resp.clicked() {
                    ask_restore = true;
                }
            });
        if ask_restore {
            state.confirming = true;
        }
        if !open {
            return; // × で閉じた（状態は捨てる）
        }
        // 選んだ世代が一覧から消えていたら（ローテーション等）、確認は黙って閉じる。
        let chosen = state.selected.clone().and_then(|n| state.gens.iter().find(|g| g.name == n).cloned());
        if state.confirming && chosen.is_none() {
            state.confirming = false;
        }
        if state.confirming && let Some(g) = chosen {
            let outcome = draw_confirm_dialog(
                ctx,
                ConfirmDialogSpec {
                    id_salt: "tag_master_gen_confirm_window",
                    title: t.tag_gen_confirm_title(),
                    ok_label: t.tag_gen_restore(),
                    cancel_label: t.favorite_dialog_cancel(),
                    foreground: true,
                },
                |ui| {
                    ui.label(t.tag_gen_confirm_body(&super::db_backup_ui::format_unix(g.modified_unix)));
                },
            );
            match outcome {
                ConfirmOutcome::None => {}
                ConfirmOutcome::Cancel => state.confirming = false,
                ConfirmOutcome::Ok => {
                    match self.restore_tag_master_gen(&g.name) {
                        Ok(()) => {
                            self.set_toast(t.tag_gen_restored_toast());
                            state.gens = tag_master_gen::list(&self.config.config_root);
                        }
                        Err(msg) => self.set_toast(msg),
                    }
                    state.confirming = false;
                    state.selected = None;
                }
            }
        }
        self.tag_master_gen_ui = Some(state);
    }

    /// 世代 `name` のマスターへ戻す。失敗したときは、何も変えずにトースト用の文言を返す。
    fn restore_tag_master_gen(&mut self, name: &str) -> Result<(), String> {
        let t = i18n::t();
        let root = self.config.config_root.clone();
        let keep = self.tag_master_gen_keep();
        // 先に読めるかを確かめる（壊れた世代で、いまの状態を失わないため）。
        let Some((cats, next)) = tag_master_gen::read_for_restore(&root, name, self.tag_manager_next_tier_id) else {
            return Err(t.tag_gen_restore_failed_toast().to_string());
        };
        // DBの世代を1つ作る。作れなければ復帰しない。
        if let Some(db) = self.spread_db.clone() {
            let db_keep = self.config.backup_generations as usize;
            if let Err(e) = crate::db_backup::create_generation(&db, &root, db_keep) {
                return Err(t.settings_backup_failed_toast(&format!("{e:?}")));
            }
        }
        // いまのマスターを世代に残す（復帰を取り消せるように）。
        tag_master_gen::snapshot_before_save(&root, keep, true);
        self.tag_manager_categories = cats;
        self.tag_manager_next_tier_id = next;
        self.tag_manager_selected_category = if self.tag_manager_categories.is_empty() { None } else { Some(0) };
        self.tag_manager_bulk_input.clear();
        self.tag_manager_delete_confirm = None;
        self.ensure_tag_main_nonempty();
        self.save_tag_manager();
        // 選択中ファイルのタグ表示を、新しいマスターで読み直す。
        self.tag_binding_path = None;
        Ok(())
    }
}
