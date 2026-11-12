//! タグ自動取得の大ルール（`tag_rules`）の入口。
//!
//! 仮の画面: タグパネルの「大ルール…」ボタンと、いま読み込んでいるルールの一覧（読み取り専用）だけ。
//! 穴埋め式の編集UI（R4）の配置は後で決める。それまでは、設定フォルダの `nekoviewer_tag_rules.json` を
//! 直接置き、この窓の「再読込」で反映する。

use crate::i18n;
use crate::tag_rules::{RuleRole, TagRule};

use super::NekoviewApp;

/// ルール一覧に出す、種類の表示名。
fn role_label(role: RuleRole) -> &'static str {
    let t = i18n::t();
    match role {
        RuleRole::Format => t.tag_confirm_role_format(),
        RuleRole::Main => t.tag_confirm_role_main(),
        RuleRole::Language => t.tag_confirm_role_language(),
        RuleRole::Event => t.tag_confirm_role_event(),
        RuleRole::Author => t.tag_confirm_role_author(),
        RuleRole::Group => t.tag_confirm_role_group(),
        RuleRole::Performer => t.tag_confirm_role_performer(),
        RuleRole::Character => t.tag_confirm_role_character(),
        RuleRole::Parody => t.tag_confirm_role_parody(),
        RuleRole::Attribute => t.tag_confirm_role_attribute(),
        RuleRole::YearMonth => t.tag_confirm_role_year_month(),
    }
}

/// ルール1件の概要（「条件 N件（いずれか） / 無効: … / 有効: …」）。
fn summary(rule: &TagRule) -> String {
    let join = |set: &std::collections::BTreeSet<RuleRole>| set.iter().map(|r| role_label(*r)).collect::<Vec<_>>().join("・");
    i18n::t().tag_rules_summary(rule.conditions.len(), rule.match_all, &join(&rule.effect.disable), &join(&rule.effect.enable))
}

impl NekoviewApp {
    /// タグパネルの1行: 「大ルール…」ボタンとルールの件数。
    pub(super) fn draw_tag_rules_entry(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        ui.horizontal(|ui| {
            if ui.small_button(t.tag_rules_open_button()).on_hover_text(t.tag_rules_open_hover()).clicked() {
                self.tag_rules_open = !self.tag_rules_open;
            }
            ui.weak(t.tag_rules_count(self.tag_rules.rules.len()));
        });
    }

    /// 大ルールの一覧窓（読み取り専用。編集UIは準備中）。
    pub(super) fn draw_tag_rules_window(&mut self, ctx: &egui::Context) {
        if !self.tag_rules_open {
            return;
        }
        let t = i18n::t();
        let mut open = true;
        let mut reload = false;
        egui::Window::new(t.tag_rules_window_title())
            .id(egui::Id::new("tag_rules_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 360.0])
            .show(ctx, |ui| {
                ui.colored_label(egui::Color32::from_rgb(230, 160, 40), t.tag_rules_provisional_notice());
                ui.separator();
                if self.tag_rules.rules.is_empty() {
                    ui.label(t.tag_rules_empty());
                }
                egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                    for rule in &self.tag_rules.rules {
                        ui.horizontal(|ui| {
                            let name = egui::RichText::new(&rule.name);
                            ui.label(if rule.enabled { name.strong() } else { name.weak() });
                            if !rule.enabled {
                                ui.weak(t.tag_rules_disabled());
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.weak(summary(rule));
                        });
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    reload = ui.button(t.tag_rules_reload_button()).on_hover_text(t.tag_rules_reload_hover()).clicked();
                });
            });
        if reload {
            let loaded = crate::tag_rules::RuleSet::load(&self.config.config_root);
            match loaded {
                Some(set) => {
                    let n = set.rules.len();
                    self.tag_rules = set;
                    self.set_toast(t.tag_rules_reloaded(n));
                }
                None if self.config.config_root.join("nekoviewer_tag_rules.json").exists() => self.set_toast(t.tag_rules_reload_failed()),
                None => {
                    self.tag_rules = crate::tag_rules::RuleSet::default();
                    self.set_toast(t.tag_rules_reloaded(0));
                }
            }
        }
        if !open {
            self.tag_rules_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_rules::Preset;

    #[test]
    fn summary_lists_conditions_and_effects() {
        let s = summary(&Preset::Cosplay.to_rule(1));
        assert!(s.contains("無効") && s.contains("有効"), "{s}");
        assert!(s.contains("作者") && s.contains("演者"), "{s}");
    }

    #[test]
    fn every_role_has_a_label() {
        for r in RuleRole::ALL {
            assert!(!role_label(r).is_empty(), "{r:?}");
        }
    }
}
