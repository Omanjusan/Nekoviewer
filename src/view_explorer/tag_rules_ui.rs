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

/// 一覧の操作（描画中は状態を変えられないので、あとでまとめて行う）。
enum RuleAction {
    /// 有効/無効を切り替える。
    Toggle(u64),
    /// 削除の確認を出す／やめる。
    AskDelete(Option<u64>),
    /// 削除する。
    Delete(u64),
    /// コピー元のルールをコピーして、新しいルールを足す。
    Copy,
    /// 無い初期ルールを、無効で足す。
    RestoreInitial,
    /// 設定フォルダのルールファイルを読み直す。
    Reload,
}

/// コピーの名前: 「元の名前 のコピー」。同じ名前があれば「 のコピー 2」「 のコピー 3」…にする。
fn unique_copy_name(set: &crate::tag_rules::RuleSet, base: &str, suffix: &str) -> String {
    let first = format!("{base}{suffix}");
    if !set.rules.iter().any(|r| r.name == first) {
        return first;
    }
    (2..).map(|n| format!("{first} {n}")).find(|name| !set.rules.iter().any(|r| &r.name == name)).unwrap_or(first)
}

impl NekoviewApp {
    /// ルール一式を差し替えて保存する。保存できなかったら、いまのルールのまま残して、トーストで知らせる。成否を返す。
    fn commit_tag_rules(&mut self, new: crate::tag_rules::RuleSet) -> bool {
        if new.save(&self.config.config_root) {
            self.tag_rules = new;
            true
        } else {
            self.set_toast(i18n::t().tag_rules_save_failed());
            false
        }
    }

    /// 「タグ抽出ルール定義」窓（一覧）。カードで有効/無効・削除、既存ルールのコピー、初期ルールの足し直し、再読込。
    /// 有効/無効・削除・コピーは、押したときに保存する。条件・結果の編集は準備中。
    pub(super) fn draw_tag_rules_window(&mut self, ctx: &egui::Context) {
        if !self.tag_rules_open {
            return;
        }
        let t = i18n::t();
        let mut open = true;
        let mut action: Option<RuleAction> = None;
        let center = self.explorer_center(ctx);
        let warn = egui::Color32::from_rgb(230, 90, 80);
        // コピー元は、選んでいなければ先頭。消えていたら先頭に戻す。
        let copy_src = self.tag_rules_copy_src.filter(|id| self.tag_rules.rules.iter().any(|r| r.id == *id)).or_else(|| self.tag_rules.rules.first().map(|r| r.id));
        let mut copy_pick = copy_src;
        egui::Window::new(t.tag_rules_window_title())
            .id(egui::Id::new("tag_rules_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0, 460.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .show(ctx, |ui| {
                ui.weak(t.tag_rules_list_hint());
                ui.add_space(4.0);
                if self.tag_rules.rules.is_empty() {
                    ui.label(t.tag_rules_empty());
                }
                egui::ScrollArea::vertical().max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
                    for rule in &self.tag_rules.rules {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let name = egui::RichText::new(&rule.name);
                                ui.label(if rule.enabled { name.strong() } else { name.weak() });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if self.tag_rules_del_ask == Some(rule.id) {
                                        if ui.button(t.tag_rules_delete_cancel()).clicked() {
                                            action = Some(RuleAction::AskDelete(None));
                                        }
                                        if ui.button(egui::RichText::new(t.tag_rules_delete_do()).color(warn)).clicked() {
                                            action = Some(RuleAction::Delete(rule.id));
                                        }
                                        ui.colored_label(warn, t.tag_rules_delete_ask());
                                    } else {
                                        if ui.button(egui::RichText::new(t.tag_rules_delete_button()).color(warn)).clicked() {
                                            action = Some(RuleAction::AskDelete(Some(rule.id)));
                                        }
                                        ui.add_enabled(false, egui::Button::new(t.tag_rules_edit_button())).on_disabled_hover_text(t.tag_rules_edit_pending_hover());
                                        let on = rule.enabled;
                                        if ui.add(egui::Button::selectable(on, t.tag_rules_toggle(on))).clicked() {
                                            action = Some(RuleAction::Toggle(rule.id));
                                        }
                                    }
                                });
                            });
                            ui.weak(summary(rule));
                        });
                    }
                });
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong(t.tag_rules_copy_heading());
                    ui.horizontal(|ui| {
                        let selected = self.tag_rules.rules.iter().find(|r| Some(r.id) == copy_pick).map(|r| r.name.clone()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("tag_rules_copy_src").selected_text(selected).width(280.0).show_ui(ui, |ui| {
                            for r in &self.tag_rules.rules {
                                ui.selectable_value(&mut copy_pick, Some(r.id), &r.name);
                            }
                        });
                        let can_copy = copy_pick.is_some();
                        if ui.add_enabled(can_copy, egui::Button::new(t.tag_rules_copy_button())).clicked() {
                            action = Some(RuleAction::Copy);
                        }
                    });
                    ui.weak(t.tag_rules_copy_hint());
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(t.tag_rules_restore_button()).on_hover_text(t.tag_rules_restore_hover()).clicked() {
                        action = Some(RuleAction::RestoreInitial);
                    }
                    if ui.button(t.tag_rules_reload_button()).on_hover_text(t.tag_rules_reload_hover()).clicked() {
                        action = Some(RuleAction::Reload);
                    }
                });
            });
        self.tag_rules_copy_src = copy_pick;
        if let Some(a) = action {
            self.apply_rule_action(a);
        }
        if !open {
            self.tag_rules_open = false;
            self.tag_rules_del_ask = None;
        }
    }

    fn apply_rule_action(&mut self, action: RuleAction) {
        let t = i18n::t();
        match action {
            RuleAction::AskDelete(id) => self.tag_rules_del_ask = id,
            RuleAction::Toggle(id) => {
                let mut new = self.tag_rules.clone();
                let Some(rule) = new.rules.iter().find(|r| r.id == id).cloned() else { return };
                let mut rule = rule;
                rule.enabled = !rule.enabled;
                // 有効にするには効果が要る（効果が空のひな形は、無効のまま）。
                if new.update(rule).is_err() {
                    self.set_toast(t.tag_rules_enable_needs_effect());
                    return;
                }
                self.commit_tag_rules(new);
            }
            RuleAction::Delete(id) => {
                let mut new = self.tag_rules.clone();
                new.remove(id);
                if self.commit_tag_rules(new) {
                    self.tag_rules_del_ask = None;
                }
            }
            RuleAction::Copy => {
                let Some(src) = self.tag_rules_copy_src.and_then(|id| self.tag_rules.rules.iter().find(|r| r.id == id)).cloned() else { return };
                let mut new = self.tag_rules.clone();
                let mut copy = src.clone();
                copy.name = unique_copy_name(&new, &src.name, t.tag_rules_copy_suffix());
                if new.add(copy).is_ok() && self.commit_tag_rules(new) {
                    self.set_toast(t.tag_rules_copied(&src.name));
                }
            }
            RuleAction::RestoreInitial => {
                let mut new = self.tag_rules.clone();
                let n = new.add_missing_initial();
                if n == 0 {
                    self.set_toast(t.tag_rules_restore_none());
                } else if self.commit_tag_rules(new) {
                    self.set_toast(t.tag_rules_restored(n));
                }
            }
            RuleAction::Reload => match crate::tag_rules::RuleSet::load(&self.config.config_root) {
                Some(set) => {
                    let n = set.rules.len();
                    self.tag_rules = set;
                    self.tag_rules_del_ask = None;
                    self.set_toast(t.tag_rules_reloaded(n));
                }
                None if crate::tag_rules::RuleSet::file_exists(&self.config.config_root) => self.set_toast(t.tag_rules_reload_failed()),
                None => {
                    self.tag_rules = crate::tag_rules::RuleSet::default();
                    self.set_toast(t.tag_rules_reloaded(0));
                }
            },
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
    fn copy_names_are_numbered_when_taken() {
        let mut set = crate::tag_rules::RuleSet::default();
        set.add(Preset::Cosplay.to_rule(0)).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー");
        let mut c1 = Preset::Cosplay.to_rule(0);
        c1.name = "cosplay のコピー".into();
        set.add(c1).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー 2");
        let mut c2 = Preset::Cosplay.to_rule(0);
        c2.name = "cosplay のコピー 2".into();
        set.add(c2).unwrap();
        assert_eq!(unique_copy_name(&set, "cosplay", " のコピー"), "cosplay のコピー 3");
    }

    #[test]
    fn every_role_has_a_label() {
        for r in RuleRole::ALL {
            assert!(!role_label(r).is_empty(), "{r:?}");
        }
    }
}
