//! ファイルカード右クリック「選択した項目を維持したままファイラーで開く」。
//! アプリの選択状態をOS標準のファイラーで再現する（実処理は [`crate::fs::reveal`]）。
//!
//! - 単品選択時: ダイアログなしで、右クリックしたカードのファイルを開く。
//! - 複数選択中: 「複数選択した項目」か「ホバーした単体のファイル」かをラジオで選ぶダイアログを出す。
//! - ネットワークパスはリストから除いて実行する（黄字で予告）。除いた結果が空ならグレーアウト。
//! - 親フォルダが `MAX_REVEAL_FOLDERS` を超える選択はグレーアウト（赤字で理由を出す）。
//! - ファイラー呼び出しは外部コマンド/COMの完了待ちがあるため別スレッドで行い、結果はトーストで返す。

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::fs::mount::network_mount_root;
use crate::fs::reveal::{exceeds_folder_limit, reveal_in_file_manager, RevealReport, MAX_REVEAL_FOLDERS};
use crate::i18n;
use super::*;
use super::help::help_tip_auto;

/// 黄字（除外の予告）。仮想フォルダ系ダイアログの警告色に合わせる。
const WARN_YELLOW: egui::Color32 = egui::Color32::from_rgb(230, 160, 40);

#[derive(Clone, Copy, PartialEq, Eq)]
enum RevealChoice {
    Selection,
    Hover,
}

/// 複数選択中に開く「どちらを開くか」ダイアログの状態。
pub(crate) struct RevealDialogState {
    /// 複数選択した項目（ネットワークパス除外済み）
    selection: Vec<PathBuf>,
    /// 選択集合にネットワークパスが含まれていた（除外の予告を出す）
    selection_had_network: bool,
    /// 右クリックしたカードのファイル
    hover: PathBuf,
    choice: RevealChoice,
}

fn is_network(path: &Path) -> bool {
    network_mount_root(path).is_some()
}

impl NekoviewApp {
    /// 右クリックメニューの項目（と、その下に出す赤字/黄字のラベル）。
    /// `hover` は右クリックしたカードのファイル。複数選択中は選択集合が対象で、ホバー位置は無視する
    /// （ダイアログで「ホバーした単体」を選べる）。
    pub(super) fn draw_reveal_menu_item(&mut self, ui: &mut egui::Ui, hover: &Path) {
        let multi = !self.multi_selected.is_empty();
        let raw: Vec<PathBuf> = if multi {
            self.multi_selected.iter().filter_map(|&idx| self.archives.get(idx).cloned()).collect()
        } else {
            vec![hover.to_path_buf()]
        };
        let had_network = raw.iter().any(|p| is_network(p));
        let targets: Vec<PathBuf> = raw.into_iter().filter(|p| !is_network(p)).collect();
        let over_limit = exceeds_folder_limit(&targets);

        // 複数選択中は、選択集合が使えなくてもホバー単体を開ける（ダイアログ側で選ぶ）ので、
        // 項目のグレーアウトは「開ける対象が何も無い」場合だけにするのが筋だが、
        // 10フォルダ超は仕様として項目ごとグレーアウトする。
        let enabled = !targets.is_empty() && !over_limit;
        let r_item = ui.add_enabled(enabled, egui::Button::new(i18n::t().reveal_menu()));
        help_tip_auto(&r_item, &i18n::t().help_card_reveal());
        if over_limit {
            ui.colored_label(ui.visuals().error_fg_color, i18n::t().reveal_over_limit(MAX_REVEAL_FOLDERS));
        }
        if had_network {
            ui.colored_label(WARN_YELLOW, i18n::t().reveal_network_excluded());
        }
        if r_item.clicked() {
            if multi {
                self.reveal_dialog = Some(RevealDialogState {
                    selection: targets,
                    selection_had_network: had_network,
                    hover: hover.to_path_buf(),
                    choice: RevealChoice::Selection,
                });
            } else {
                self.start_reveal(ui.ctx(), targets);
            }
            ui.close();
        }
    }

    /// 「どちらを開くか」ダイアログ。OKで別スレッドのファイラー呼び出しを始める。
    pub(super) fn draw_reveal_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = self.reveal_dialog.as_mut() else { return };
        let hover_is_network = is_network(&state.hover);
        let mut choice = state.choice;
        let selection_len = state.selection.len();
        let had_network = state.selection_had_network;
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: "reveal_dialog_window",
                title: i18n::t().reveal_dialog_title(),
                ok_label: i18n::t().reveal_dialog_ok(),
                cancel_label: i18n::t().favorite_dialog_cancel(),
                foreground: false,
            },
            |ui| {
                ui.radio_value(&mut choice, RevealChoice::Selection, i18n::t().reveal_choice_selection(selection_len));
                if had_network {
                    ui.colored_label(WARN_YELLOW, i18n::t().reveal_network_excluded());
                }
                ui.add_enabled_ui(!hover_is_network, |ui| {
                    ui.radio_value(&mut choice, RevealChoice::Hover, i18n::t().reveal_choice_hover());
                });
                if hover_is_network {
                    ui.colored_label(WARN_YELLOW, i18n::t().reveal_network_excluded());
                }
            },
        );
        state.choice = choice;
        match outcome {
            ConfirmOutcome::Cancel => self.reveal_dialog = None,
            ConfirmOutcome::Ok => {
                if let Some(state) = self.reveal_dialog.take() {
                    let paths = match state.choice {
                        RevealChoice::Selection => state.selection,
                        RevealChoice::Hover if !is_network(&state.hover) => vec![state.hover],
                        RevealChoice::Hover => Vec::new(),
                    };
                    self.start_reveal(ctx, paths);
                }
            }
            ConfirmOutcome::None => {}
        }
    }

    fn start_reveal(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(reveal_in_file_manager(&paths));
            ctx.request_repaint();
        });
        self.reveal_pending.push(rx);
    }

    /// ファイラー呼び出しの結果を回収してトーストに出す。成功時は黙る（選択の反映は検知できないため）。
    pub(super) fn poll_reveal(&mut self) {
        let mut reports: Vec<RevealReport> = Vec::new();
        self.reveal_pending.retain(|rx| match rx.try_recv() {
            Ok(report) => {
                reports.push(report);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for r in reports {
            if r.too_many {
                self.set_toast(i18n::t().reveal_over_limit(MAX_REVEAL_FOLDERS));
            } else if r.failed > 0 {
                self.set_toast(i18n::t().reveal_toast_failed(r.failed));
            } else if r.folder_only > 0 {
                self.set_toast(i18n::t().reveal_toast_folder_only(r.folder_only));
            }
        }
    }
}
