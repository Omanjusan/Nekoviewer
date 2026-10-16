//! 設定→エクスプローラー「データベース管理」の「追えない記録の掃除」。
//!
//! ファイルが見つからないIDの記録を、保持日数を待たずに手動で削除する。確認ダイアログで内訳（お気に入り・
//! 評価タグ等付き・空）を見せ、オフラインのドライブはドライブ単位で明示的に含めたときだけ対象にする。
//! 本体は `identity_sweep.rs`（候補の検索 `scan_unreachable`、削除 `clean_unreachable_with`）。

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc;

use super::*;
use crate::confirm_dialog::{draw_confirm_dialog, ConfirmDialogSpec, ConfirmOutcome};
use crate::i18n;
use crate::identity_sweep::{AutoSweepOutcome, Unreachable};

pub(super) enum UnreachableClean {
    /// 候補を調べている（バックグラウンド）。
    Scanning(mpsc::Receiver<Vec<Unreachable>>),
    /// 確認待ち。`offline_chosen` は、対象に含めると選んだオフラインのドライブ。
    Confirm { items: Vec<Unreachable>, offline_chosen: HashSet<String> },
    /// 削除している（バックグラウンド）。
    Cleaning(mpsc::Receiver<AutoSweepOutcome>),
}

/// 選ばれている候補（オンラインのもの＋選んだドライブのオフラインのもの）の内訳。
#[derive(Debug, Default, PartialEq, Eq)]
struct Breakdown {
    total: usize,
    favorite: usize,
    with_data: usize,
    blank: usize,
}

fn selected<'a>(items: &'a [Unreachable], chosen: &'a HashSet<String>) -> impl Iterator<Item = &'a Unreachable> {
    items.iter().filter(|u| !u.offline || chosen.contains(&u.volume))
}

fn breakdown(items: &[Unreachable], chosen: &HashSet<String>) -> Breakdown {
    let mut b = Breakdown::default();
    for u in selected(items, chosen) {
        b.total += 1;
        if u.favorite {
            b.favorite += 1;
        } else if u.has_user_data {
            b.with_data += 1;
        } else {
            b.blank += 1;
        }
    }
    b
}

/// オフラインのドライブ別の件数（表示順を安定させるため BTreeMap）。
fn offline_groups(items: &[Unreachable]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for u in items.iter().filter(|u| u.offline) {
        *m.entry(u.volume.clone()).or_insert(0) += 1;
    }
    m
}

impl NekoviewApp {
    /// 設定画面の「データベース管理」に出す、説明とボタン。
    pub(crate) fn draw_unreachable_clean_section(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        ui.separator();
        ui.label(t.settings_unreachable_label());
        let enabled =
            self.unreachable_clean.is_none() && self.spread_db.is_some() && crate::file_identity::is_enabled();
        if ui.add_enabled(enabled, egui::Button::new(t.settings_unreachable_button())).clicked() {
            self.start_unreachable_scan();
        }
        ui.label(t.settings_unreachable_explain());
    }

    fn start_unreachable_scan(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::identity_sweep::scan_unreachable(&db));
            ctx.request_repaint();
        });
        self.unreachable_clean = Some(UnreachableClean::Scanning(rx));
        self.set_sticky_toast(Some(i18n::t().unreachable_scanning_toast().to_owned()));
    }

    fn start_unreachable_clean(&mut self, ids: Vec<u64>, offline_chosen: HashSet<String>) {
        let Some(db) = self.spread_db.clone() else { return };
        let root = self.config.config_root.clone();
        let keep = self.config.backup_generations as usize;
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let backup = || match crate::db_backup::create_generation(&db, &root, keep) {
                Ok(g) => {
                    crate::log_common!("[sweep] 手動掃除前のバックアップを作成: {}", g.name);
                    true
                }
                Err(e) => {
                    crate::log_common!("[sweep] 手動掃除前のバックアップに失敗（掃除を中止）: {:?}", e);
                    false
                }
            };
            let out = crate::identity_sweep::clean_unreachable_with(
                &db,
                &ids,
                &offline_chosen,
                &crate::file_identity::probe_record,
                &backup,
            );
            crate::log_common!("[sweep] 手動掃除: {:?} backup_failed={}", out.report, out.backup_failed);
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.unreachable_clean = Some(UnreachableClean::Cleaning(rx));
        self.set_sticky_toast(Some(i18n::t().unreachable_cleaning_toast().to_owned()));
    }

    /// 毎フレーム呼ぶ。バックグラウンドの結果を受け取り、確認ダイアログを描く。
    pub(super) fn draw_unreachable_clean_dialogs(&mut self, ctx: &egui::Context) {
        let Some(state) = self.unreachable_clean.take() else { return };
        let t = i18n::t();
        match state {
            UnreachableClean::Scanning(rx) => match rx.try_recv() {
                Ok(items) => {
                    self.set_sticky_toast(None);
                    if items.is_empty() {
                        self.set_toast(t.unreachable_none_toast());
                    } else {
                        self.unreachable_clean =
                            Some(UnreachableClean::Confirm { items, offline_chosen: HashSet::new() });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => self.unreachable_clean = Some(UnreachableClean::Scanning(rx)),
                Err(mpsc::TryRecvError::Disconnected) => self.set_sticky_toast(None),
            },
            UnreachableClean::Cleaning(rx) => match rx.try_recv() {
                Ok(out) => {
                    self.set_sticky_toast(None);
                    if out.backup_failed {
                        self.set_toast(t.identity_sweep_backup_failed_toast());
                    } else {
                        self.set_toast(t.unreachable_done_toast(out.report.deleted, out.report.deleted_with_data));
                    }
                    if let Some(db) = self.spread_db.as_ref() {
                        self.identity_pending_count = crate::identity_pending::count(db);
                    }
                    self.refresh_favorites_after_change();
                }
                Err(mpsc::TryRecvError::Empty) => self.unreachable_clean = Some(UnreachableClean::Cleaning(rx)),
                Err(mpsc::TryRecvError::Disconnected) => self.set_sticky_toast(None),
            },
            UnreachableClean::Confirm { items, mut offline_chosen } => {
                let groups = offline_groups(&items);
                let b = breakdown(&items, &offline_chosen);
                let outcome = draw_confirm_dialog(
                    ctx,
                    ConfirmDialogSpec {
                        id_salt: "unreachable_clean_window",
                        title: t.unreachable_confirm_title(),
                        ok_label: t.unreachable_ok(),
                        cancel_label: t.favorite_dialog_cancel(),
                        foreground: true,
                    },
                    |ui| {
                        ui.label(t.unreachable_confirm_body(b.total, b.favorite, b.with_data, b.blank));
                        ui.add_space(6.0);
                        ui.colored_label(ui.visuals().warn_fg_color, t.unreachable_confirm_warning());
                        if !groups.is_empty() {
                            ui.add_space(6.0);
                            ui.label(t.unreachable_offline_heading());
                            for (volume, count) in &groups {
                                let mut on = offline_chosen.contains(volume);
                                if ui.checkbox(&mut on, t.unreachable_offline_check(volume, *count)).changed() {
                                    if on {
                                        offline_chosen.insert(volume.clone());
                                    } else {
                                        offline_chosen.remove(volume);
                                    }
                                }
                            }
                        }
                    },
                );
                match outcome {
                    ConfirmOutcome::None => {
                        self.unreachable_clean = Some(UnreachableClean::Confirm { items, offline_chosen });
                    }
                    ConfirmOutcome::Cancel => {}
                    ConfirmOutcome::Ok => {
                        let ids: Vec<u64> = selected(&items, &offline_chosen).map(|u| u.id).collect();
                        if ids.is_empty() {
                            self.set_toast(t.unreachable_none_toast());
                        } else {
                            self.start_unreachable_clean(ids, offline_chosen);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(id: u64, volume: &str, offline: bool, favorite: bool, has_user_data: bool) -> Unreachable {
        Unreachable { id, volume: volume.to_owned(), offline, favorite, has_user_data }
    }

    #[test]
    fn breakdown_counts_each_item_once_with_favorites_first() {
        let items = vec![
            u(1, "/", false, true, true),   // お気に入り（データも付くが、お気に入りに数える）
            u(2, "/", false, false, true),  // 評価・タグ等
            u(3, "/", false, false, false), // 空
            u(4, "/mnt/usb", true, true, true),
            u(5, "/mnt/usb", true, false, false),
        ];
        assert_eq!(breakdown(&items, &HashSet::new()), Breakdown { total: 3, favorite: 1, with_data: 1, blank: 1 });
        let chosen: HashSet<String> = ["/mnt/usb".to_owned()].into_iter().collect();
        assert_eq!(breakdown(&items, &chosen), Breakdown { total: 5, favorite: 2, with_data: 1, blank: 2 });
    }

    #[test]
    fn offline_groups_count_per_volume_and_ignore_online_items() {
        let items = vec![
            u(1, "/", false, false, false),
            u(2, "/mnt/b", true, false, false),
            u(3, "/mnt/a", true, false, false),
            u(4, "/mnt/a", true, true, true),
        ];
        let g: Vec<(String, usize)> = offline_groups(&items).into_iter().collect();
        assert_eq!(g, vec![("/mnt/a".to_owned(), 2), ("/mnt/b".to_owned(), 1)]);
    }
}
