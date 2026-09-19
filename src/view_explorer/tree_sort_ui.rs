//! ツリーの「ソート条件設定」ダイアログ。右クリックメニューから開き、開くときは現在の設定値で復元する。
//! 「適用」で保存して並べ直し、「キャンセル」で何も変えない。対象は開いたツリー1つだけ。

use crate::i18n;
use crate::tree_sort::{TreeSort, TreeSortKey};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TreeSortTarget {
    Virtual,
    Real,
}

pub(super) struct TreeSortDialog {
    target: TreeSortTarget,
    key: TreeSortKey,
    ascending: bool,
}

impl NekoviewApp {
    pub(super) fn open_tree_sort_dialog(&mut self, target: TreeSortTarget) {
        let current = match target {
            TreeSortTarget::Virtual => self.tree_sorts.virtual_tree_or_default(),
            TreeSortTarget::Real => self.tree_sorts.real_tree_or_default(),
        };
        self.tree_sort_dialog = Some(TreeSortDialog { target, key: current.key, ascending: current.ascending });
    }

    pub(super) fn draw_tree_sort_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.tree_sort_dialog.as_mut() else { return };
        let t = i18n::t();
        let (mut cancel, mut apply) = (false, false);
        egui::Window::new(t.tree_sort_title())
            .id(egui::Id::new("tree_sort_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(match d.target {
                    TreeSortTarget::Virtual => t.tree_sort_target_virtual(),
                    TreeSortTarget::Real => t.tree_sort_target_real(),
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let keys: &[(TreeSortKey, &str)] = match d.target {
                        TreeSortTarget::Virtual => &[
                            (TreeSortKey::Registration, t.tree_sort_registration()),
                            (TreeSortKey::Name, t.sort_name().trim_matches(['[', ']'])),
                            (TreeSortKey::Date, t.sort_date().trim_matches(['[', ']'])),
                        ],
                        // 実ツリーに「登録順」は無い
                        TreeSortTarget::Real => &[
                            (TreeSortKey::Name, t.sort_name().trim_matches(['[', ']'])),
                            (TreeSortKey::Date, t.sort_date().trim_matches(['[', ']'])),
                        ],
                    };
                    for (key, label) in keys {
                        ui.radio_value(&mut d.key, *key, *label);
                    }
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.ascending, true, t.sort_asc().trim_matches(['[', ']']));
                    ui.radio_value(&mut d.ascending, false, t.sort_desc().trim_matches(['[', ']']));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    cancel = ui.button(t.favorite_dialog_cancel()).clicked();
                    apply = ui.button(t.bulk_setting_apply_button()).clicked();
                });
            });
        if cancel {
            self.tree_sort_dialog = None;
        } else if apply {
            if let Some(d) = self.tree_sort_dialog.take() {
                self.apply_tree_sort(d.target, TreeSort { key: d.key, ascending: d.ascending });
            }
        }
    }

    /// 並び条件を保存して、対象のツリーを並べ直す。変わらなければ何もしない。
    fn apply_tree_sort(&mut self, target: TreeSortTarget, sort: TreeSort) {
        match target {
            TreeSortTarget::Virtual => {
                // 既定と同じ値は「未設定」として保存しない（stateを増やさない）
                let stored = (sort != TreeSort::VIRTUAL_DEFAULT).then_some(sort);
                if self.tree_sorts.virtual_tree == stored {
                    return;
                }
                self.tree_sorts.virtual_tree = stored;
                self.persist_state();
                self.resort_virtual_nodes();
            }
            TreeSortTarget::Real => {
                let stored = (sort != TreeSort::REAL_DEFAULT).then_some(sort);
                if self.tree_sorts.real_tree == stored {
                    return;
                }
                self.tree_sorts.real_tree = stored;
                self.persist_state();
                self.resort_real_tree();
            }
        }
    }
}
