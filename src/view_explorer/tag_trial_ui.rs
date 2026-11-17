//! 一括タグ付けの適用後の「試用中」の操作（確定・破棄）。タグパネルの上部に帯を出し、確認ダイアログを挟んで実行する。
//!
//! - 確定: 仮マスター（`nekoviewer_tags.trial.json`）を本物へ置き換え、試用の記録を消す。ファイルのタグは適用時に
//!   DBへ書いてあるので、それ以上の作業は無い
//! - 破棄: 記録のタグID（適用で足した分）だけを各ファイルから外し（別スレッド・進捗オーバーレイ）、仮マスターを捨てて
//!   本物を読み直す。`next_tier_id` は巻き戻さない（`tag_manager::discard_trial`）。適用のあとに手で付けた・外したタグには触らない
//! - 破棄には DB が要る。DB を開けていないときは破棄できない（確定はできる）

use std::sync::mpsc;

use super::*;
use crate::confirm_dialog::{draw_confirm_dialog_at, ConfirmDialogSpec, ConfirmOutcome};
use crate::i18n;
use crate::tag_trial::{remove_added_tags, DiscardReport, TrialRecord};

/// 開いている確認ダイアログ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrialDialog {
    Confirm,
    /// 破棄（外すタグがあるファイルの数・タグIDの総数。開くときに記録から数えておく）。
    Discard { files: usize, ids: usize },
}

/// 破棄の実行中のジョブ（各ファイルからタグを外している間）。
pub(super) struct TagDiscardJob {
    rx: mpsc::Receiver<DiscardReport>,
}

impl NekoviewApp {
    /// タグパネルの上部の帯。試用中だけ出る。
    pub(super) fn draw_tag_trial_banner(&mut self, ui: &mut egui::Ui) {
        if !self.tag_trial_active {
            return;
        }
        let t = i18n::t();
        let amber = egui::Color32::from_rgb(230, 160, 40);
        egui::Frame::group(ui.style()).stroke(egui::Stroke::new(1.0, amber)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(t.tag_trial_banner()).strong().color(amber)).on_hover_text(t.tag_trial_banner_hover());
                ui.horizontal(|ui| {
                    if ui.button(t.tag_trial_confirm_button()).on_hover_text(t.tag_trial_confirm_hover()).clicked() {
                        self.tag_trial_dialog = Some(TrialDialog::Confirm);
                    }
                    let can_discard = self.spread_db.is_some() && self.tag_discard_job.is_none();
                    let resp = ui.add_enabled(can_discard, egui::Button::new(t.tag_trial_discard_button()));
                    let resp = if can_discard { resp.on_hover_text(t.tag_trial_discard_hover()) } else { resp.on_disabled_hover_text(t.tag_trial_discard_no_db()) };
                    if resp.clicked() {
                        let record = TrialRecord::load(&self.config.config_root).unwrap_or_default();
                        self.tag_trial_dialog = Some(TrialDialog::Discard { files: record.files.len(), ids: record.added_total() });
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    /// 確定・破棄の確認ダイアログ。
    pub(super) fn draw_tag_trial_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.tag_trial_dialog else { return };
        if !self.tag_trial_active {
            self.tag_trial_dialog = None;
            return;
        }
        let t = i18n::t();
        let (title, ok_label) = match dialog {
            TrialDialog::Confirm => (t.tag_trial_confirm_title(), t.tag_trial_confirm_button()),
            TrialDialog::Discard { .. } => (t.tag_trial_discard_title(), t.tag_trial_discard_button()),
        };
        let outcome = draw_confirm_dialog_at(
            ctx,
            ConfirmDialogSpec { id_salt: "tag_trial_dialog", title, ok_label, cancel_label: t.favorite_dialog_cancel(), foreground: true },
            Some(self.explorer_center(ctx)),
            |ui| {
                ui.set_max_width(420.0);
                match dialog {
                    TrialDialog::Confirm => ui.label(t.tag_trial_confirm_body()),
                    TrialDialog::Discard { files, ids } => ui.label(t.tag_trial_discard_body(files, ids)),
                };
            },
        );
        match outcome {
            ConfirmOutcome::None => {}
            ConfirmOutcome::Cancel => self.tag_trial_dialog = None,
            ConfirmOutcome::Ok => {
                self.tag_trial_dialog = None;
                match dialog {
                    TrialDialog::Confirm => self.confirm_tag_trial(),
                    TrialDialog::Discard { .. } => self.start_tag_discard(),
                }
            }
        }
    }

    /// 確定: 仮マスターを本物にする。
    fn confirm_tag_trial(&mut self) {
        let root = self.config.config_root.clone();
        // 確定は本物を置き換えるので、置き換え前の本物を世代に残す。
        crate::tag_master_gen::snapshot_before_save(&root, self.tag_master_gen_keep(), true);
        if crate::tag_manager::promote_trial(&root) {
            // 自動付与で足したタグIDを、ファイルごとの記録（DB）へ移す。再抽出で「前回分を外して付け直す」ための元になる。
            let report = self
                .spread_db
                .as_ref()
                .map(|db| crate::tag_auto_record::record_from_trial(db, &TrialRecord::load(&root).unwrap_or_default()))
                .unwrap_or_default();
            TrialRecord::remove(&root);
            self.tag_trial_active = false;
            if report.failed > 0 {
                self.set_toast(i18n::t().tag_trial_confirmed_record_failed(report.failed));
            } else {
                self.set_toast(i18n::t().tag_trial_confirmed());
            }
        } else {
            self.set_toast(i18n::t().tag_trial_confirm_failed());
        }
    }

    /// 破棄を始める。別スレッドで、適用で足したタグを各ファイルから外す。
    fn start_tag_discard(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        if self.tag_discard_job.is_some() {
            return;
        }
        let record = TrialRecord::load(&self.config.config_root).unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(remove_added_tags(&db, &record));
            ctx.request_repaint();
        });
        self.tag_discard_job = Some(TagDiscardJob { rx });
    }

    /// 破棄の完了を取り込む。起動時に試用中だったときの案内も、ここで1回だけ出す（毎フレーム呼んでよい）。
    pub(super) fn poll_tag_trial(&mut self) {
        if self.tag_trial_active && !self.tag_trial_notified && self.tag_apply_job.is_none() {
            self.tag_trial_notified = true;
            self.set_toast(i18n::t().tag_trial_resumed());
        }
        let Some(job) = self.tag_discard_job.as_ref() else { return };
        let report = match job.rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.tag_discard_job = None;
                return;
            }
        };
        self.tag_discard_job = None;
        let root = self.config.config_root.clone();
        // 仮マスターを捨てて本物に戻す（番号は巻き戻さない）。
        let (cats, next) = crate::tag_manager::discard_trial(&root);
        TrialRecord::remove(&root);
        self.tag_trial_active = false;
        self.tag_manager_categories = cats;
        self.tag_manager_next_tier_id = next;
        self.save_tag_manager();
        // 試用中だけにあったタグを指す検索条件と、選択中のファイルのタグ表示を直す。
        let valid: std::collections::HashSet<u64> =
            self.tag_manager_categories.iter().flat_map(|c| c.tiers.iter().filter(|t| t.element.is_some()).map(|t| t.id)).collect();
        self.tag_search_enabled.retain(|id| *id == crate::tag_manager::TAG_MAIN_UNSET_ID || valid.contains(id));
        self.tag_binding_path = None;
        self.set_toast(i18n::t().tag_trial_discarded(&report));
    }

    /// 破棄の実行中の進捗オーバーレイ（背後のマウス入力を遮断する）。
    pub(super) fn draw_tag_discard_overlay(&mut self, ctx: &egui::Context) {
        if self.tag_discard_job.is_none() {
            return;
        }
        self.explorer_modal(ctx, "tag_discard_progress").show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(24.0));
                ui.add_space(6.0);
                ui.label(i18n::t().tag_trial_discard_progress());
            });
        });
        ctx.request_repaint();
    }
}
