use std::path::PathBuf;

use crate::i18n;
use crate::types::{PageMode, ReaderSortKey};
use super::*;

/// エクスプローラー部の右クリックから開く「ソート条件」「しおり保存」「見開き設定」
/// 一括変更ダイアログ群。
///
/// 【モック段階（フェーズ0〜3）】DB読み書き・対象外フィルタリング・アーカイブ正当性判定は
/// 未実装。反映ボタンは押せるがダイアログを閉じるのみで実際の設定変更は行わない。
/// レイアウト確定後に別フェーズでAPI紐付けを行う。
impl NekoviewApp {
    fn dialog_target_label(targets: &[PathBuf]) -> String {
        targets
            .first()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }

    // ── ソート条件 ──────────────────────────────────────────────────────────

    pub(super) fn open_sort_condition_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.sort_condition_dialog = Some(SortConditionDialogState {
            targets,
            sort_key: ReaderSortKey::Name,
            ascending: true,
        });
    }

    pub(super) fn draw_sort_condition_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.sort_condition_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().sort_condition_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().sort_condition_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Name, i18n::t().sort_name().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Natural, i18n::t().sort_natural().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.sort_key, ReaderSortKey::Date, i18n::t().sort_date().trim_matches(['[', ']']));
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut dialog.ascending, true, i18n::t().sort_asc().trim_matches(['[', ']']));
                    ui.radio_value(&mut dialog.ascending, false, i18n::t().sort_desc().trim_matches(['[', ']']));
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel || apply {
            // 【モック段階】applyでもDB書き込みは行わず閉じるのみ。
            self.sort_condition_dialog = None;
        }
    }

    // ── しおり保存 ──────────────────────────────────────────────────────────

    pub(super) fn open_bookmark_setting_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.bookmark_setting_dialog = Some(BookmarkSettingDialogState {
            targets,
            enabled: false,
        });
    }

    pub(super) fn draw_bookmark_setting_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.bookmark_setting_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().bookmark_setting_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().bookmark_setting_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.checkbox(&mut dialog.enabled, i18n::t().bookmark_save_toggle_label());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel || apply {
            self.bookmark_setting_dialog = None;
        }
    }

    // ── 見開き設定 ──────────────────────────────────────────────────────────

    pub(super) fn open_spread_setting_dialog_for_paths(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        self.spread_setting_dialog = Some(SpreadSettingDialogState {
            targets,
            page_mode: PageMode::Single,
            offset: 0,
        });
    }

    pub(super) fn draw_spread_setting_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.spread_setting_dialog.as_mut() else {
            return;
        };
        let mut cancel = false;
        let mut apply = false;
        let is_bulk = dialog.targets.len() > 1;
        egui::Window::new(i18n::t().spread_setting_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let label = if is_bulk {
                    i18n::t().spread_setting_menu_bulk(dialog.targets.len())
                } else {
                    Self::dialog_target_label(&dialog.targets)
                };
                ui.label(label);
                ui.add_space(8.0);
                ui.radio_value(&mut dialog.page_mode, PageMode::Single, i18n::t().spread_mode_single_label());
                ui.radio_value(&mut dialog.page_mode, PageMode::SpreadRight, i18n::t().spread_mode_right_label());
                ui.radio_value(&mut dialog.page_mode, PageMode::SpreadLeft, i18n::t().spread_mode_left_label());
                ui.add_space(8.0);
                ui.add_enabled_ui(dialog.page_mode != PageMode::Single, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(i18n::t().spread_offset_dialog_label());
                        ui.add(egui::Slider::new(&mut dialog.offset, -1..=1));
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(i18n::t().favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.button(i18n::t().bulk_setting_apply_button()).clicked() {
                        apply = true;
                    }
                });
            });

        if cancel || apply {
            self.spread_setting_dialog = None;
        }
    }
}
