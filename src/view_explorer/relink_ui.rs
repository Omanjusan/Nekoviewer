//! お気に入りタブの右クリック「移動先を指定して元ファイルを検索」「お気に入り設定から外す」。
//!
//! 再紐付けの本体は `identity_relink::relink`（FP照合は別スレッド）。ここはゴースト判定・フォルダ選択
//! （アプリ内ツリーピッカー）・確認・結果の通知を受け持つ。「外す」は `favorites::remove_favorite`。

use std::path::PathBuf;
use std::sync::mpsc;

use super::virtual_ui::RealTreeState;
use super::*;
use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::file_identity::{lookup, probe_record, Presence};
use crate::i18n;
use crate::spread_state::make_key;

pub(super) enum RelinkDialog {
    /// 移動先フォルダの選択。ダブルクリックで確認へ進む。
    Pick { ghosts: Vec<u64>, tree: RealTreeState },
    /// サブフォルダを含めるかの確認。OKで検索を始める。
    Confirm { ghosts: Vec<u64>, folder: PathBuf, recursive: bool },
}

impl NekoviewApp {
    /// 対象パスのうち、ID記録があり、パスが開けない（消えている・ボリュームがオフライン）ものの ID。
    /// IDの無い旧v1だけの行はFPが無く照合できないので含めない。
    pub(super) fn relink_ghost_ids(&self, targets: &[PathBuf]) -> Vec<u64> {
        let Some(db) = self.spread_db.as_ref() else { return Vec::new() };
        targets
            .iter()
            .filter_map(|path| {
                let (dir, name) = (path.parent()?, path.file_name()?.to_str()?);
                let rec = lookup(db, &make_key(dir, name))?;
                (probe_record(&rec) != Presence::Present).then_some(rec.id)
            })
            .collect()
    }

    pub(super) fn open_relink_picker(&mut self, ghosts: Vec<u64>) {
        if ghosts.is_empty() {
            return;
        }
        let tree = RealTreeState::new(&self.drives, &self.tree_root, &self.egui_ctx);
        self.relink_dialog = Some(RelinkDialog::Pick { ghosts, tree });
    }

    /// 「お気に入り設定から外す」。1件は即実行、2件以上は確認ダイアログを挟む。
    pub(super) fn request_unset_favorites(&mut self, targets: Vec<PathBuf>) {
        match targets.len() {
            0 => {}
            1 => self.unset_favorites(&targets),
            _ => self.favorite_unset_confirm = Some(targets),
        }
    }

    fn unset_favorites(&mut self, targets: &[PathBuf]) {
        let Some(db) = self.spread_db.clone() else { return };
        for path in targets {
            if let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) {
                crate::favorites::remove_favorite(&db, dir, name);
            }
        }
        self.set_toast(i18n::t().favorite_unset_done_toast(targets.len()));
        self.refresh_favorites_after_change();
    }

    /// お気に入りの一覧と未解決の件数を引き直す（付け替え・解除のあと）。
    pub(super) fn refresh_favorites_after_change(&mut self) {
        if let Some(db) = self.spread_db.clone() {
            self.identity_pending_count = crate::identity_pending::count(&db);
        }
        if let Some(selection) = self.viewing_favorites {
            self.enter_favorite_view(selection);
        }
    }

    fn start_relink(&mut self, ghosts: Vec<u64>, folder: PathBuf, recursive: bool) {
        let Some(db) = self.spread_db.clone() else { return };
        if self.relink_job.is_some() {
            self.set_toast(i18n::t().relink_busy_toast());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let report = crate::identity_relink::relink(&db, &ghosts, &folder, recursive);
            let _ = tx.send(report);
            ctx.request_repaint();
        });
        self.relink_job = Some(rx);
        self.set_sticky_toast(Some(i18n::t().relink_running_toast().to_owned()));
    }

    /// 実行中のジョブの完了を受け取る。
    fn poll_relink_job(&mut self) {
        let Some(rx) = self.relink_job.as_ref() else { return };
        match rx.try_recv() {
            Ok(report) => {
                self.relink_job = None;
                self.set_sticky_toast(None);
                self.set_toast(i18n::t().relink_result(&report));
                self.refresh_favorites_after_change();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.relink_job = None;
                self.set_sticky_toast(None);
            }
        }
    }

    pub(super) fn draw_relink_dialogs(&mut self, ctx: &egui::Context) {
        self.poll_relink_job();
        self.draw_relink_dialog(ctx);
        self.draw_favorite_unset_confirm(ctx);
    }

    fn draw_relink_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.relink_dialog.take() else { return };
        match &mut dialog {
            RelinkDialog::Pick { ghosts, tree } => {
                tree.poll();
                let mut open = true;
                let mut action = TreeAction::None;
                let mut picked_drive = None;
                egui::Window::new(i18n::t().relink_picker_title())
                    .id(egui::Id::new("relink_picker_window"))
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([360.0, 420.0])
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .show(ctx, |ui| {
                        ui.label(i18n::t().relink_picker_hint());
                        let current = self.drives.get(tree.drive).map_or(String::new(), |d| d.label.clone());
                        egui::ComboBox::from_id_salt("relink_picker_drive")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (i, d) in self.drives.iter().enumerate() {
                                    if ui.selectable_label(tree.drive == i, &d.label).clicked() {
                                        picked_drive = Some(i);
                                    }
                                }
                            });
                        ui.separator();
                        egui::ScrollArea::both()
                            .id_salt("relink_picker_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut scroll_pending = false;
                                super::panels::show_tree_node(
                                    ui,
                                    &tree.root,
                                    0,
                                    &tree.selected,
                                    &None,
                                    false,
                                    &tree.expanded,
                                    &tree.children,
                                    false,
                                    super::panels::TreeMenu::NONE,
                                    &mut action,
                                    &mut scroll_pending,
                                );
                            });
                    });
                if !open {
                    return;
                }
                if let Some(i) = picked_drive {
                    tree.set_drive(&self.drives, i, ctx);
                }
                match action {
                    TreeAction::ToggleExpand(path) => tree.toggle_expand(path, ctx),
                    TreeAction::Navigate(path) => tree.selected = Some(path),
                    TreeAction::DoubleClick(folder) => {
                        let ghosts = std::mem::take(ghosts);
                        self.relink_dialog = Some(RelinkDialog::Confirm { ghosts, folder, recursive: false });
                        return;
                    }
                    TreeAction::AddToVirtual(_)
                    | TreeAction::OpenInSearch(_)
                    | TreeAction::SortSetting
                    | TreeAction::None => {}
                }
            }
            RelinkDialog::Confirm { ghosts, folder, recursive } => {
                let outcome = draw_confirm_dialog(
                    ctx,
                    ConfirmDialogSpec {
                        id_salt: "relink_confirm_window",
                        title: i18n::t().relink_confirm_title(),
                        ok_label: i18n::t().relink_ok(),
                        cancel_label: i18n::t().favorite_dialog_cancel(),
                        foreground: true,
                    },
                    |ui| {
                        ui.label(i18n::t().relink_confirm_body(ghosts.len(), &folder.to_string_lossy()));
                        ui.add_space(6.0);
                        ui.checkbox(recursive, i18n::t().relink_recursive_label());
                    },
                );
                match outcome {
                    ConfirmOutcome::None => {}
                    ConfirmOutcome::Cancel => return,
                    ConfirmOutcome::Ok => {
                        let (ghosts, folder, recursive) = (std::mem::take(ghosts), folder.clone(), *recursive);
                        self.start_relink(ghosts, folder, recursive);
                        return;
                    }
                }
            }
        }
        self.relink_dialog = Some(dialog);
    }

    fn draw_favorite_unset_confirm(&mut self, ctx: &egui::Context) {
        let Some(targets) = self.favorite_unset_confirm.as_ref() else { return };
        let count = targets.len();
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: "favorite_unset_confirm_window",
                title: i18n::t().favorite_unset_confirm_title(),
                ok_label: i18n::t().favorite_unset_ok(),
                cancel_label: i18n::t().favorite_dialog_cancel(),
                foreground: true,
            },
            |ui| {
                ui.label(i18n::t().favorite_unset_confirm_body(count));
            },
        );
        match outcome {
            ConfirmOutcome::None => {}
            ConfirmOutcome::Cancel => self.favorite_unset_confirm = None,
            ConfirmOutcome::Ok => {
                if let Some(targets) = self.favorite_unset_confirm.take() {
                    self.unset_favorites(&targets);
                }
            }
        }
    }
}
