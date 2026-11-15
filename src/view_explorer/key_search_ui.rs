//! 右クリック「タグ検索」（すだれメニュー）と、1つの種類に名前が複数あるときの選択ダイアログ。
//!
//! 「タグ検索 ▶ グループ／作者／演者／キャラクター／パロディ元／イベント／言語／年月」。種類ごとに、名前が1つなら即検索、
//! 複数なら選択ダイアログ（チェックした名前のOR）。名前の解決は `search_keys`（種類ごとに、DBのタグ優先、無ければファイル名抽出）。
//! 解決は「右クリックした瞬間」に1回だけ行ってキャッシュし、メニューを開いている間は読み直さない
//! （メニューは毎フレーム描き直されるため、DB読み取りを避ける）。
//! 項目は既定でグレー。単一ファイルで名前が取れた種類だけ有効。複数選択中は全体がグレー。

use std::path::{Path, PathBuf};

use crate::i18n;
use crate::search_keys::{self, KeyName, KeySource, SEARCH_ROLES};
use crate::tag_candidates::Role;

use super::*;

/// 右クリックしたファイルについて解決した、検索の切り口になる名前のキャッシュ。
pub(super) struct KeyMenuCache {
    path: PathBuf,
    names: Vec<KeyName>,
}

/// 1つの種類に名前が複数あるときの選択ダイアログ。`checked` は `names` と同じ並び。
pub(super) struct KeyPickDialog {
    role: Role,
    names: Vec<KeyName>,
    checked: Vec<bool>,
}

fn role_label(role: Role) -> &'static str {
    role.new_category_name(i18n::t())
}

impl NekoviewApp {
    /// 右クリックの瞬間に呼ぶ。そのファイルの、検索の切り口になる名前を解決して持つ。
    pub(super) fn cache_key_menu(&mut self, path: &Path) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            self.key_menu = None;
            return;
        };
        let tag_ids = match self.spread_db.as_ref() {
            Some(db) => crate::spread_state::read_archive_tags(db, dir, name),
            None => Vec::new(),
        };
        let lang = i18n::t();
        let names = search_keys::resolve_keys(
            &tag_ids,
            name,
            &self.tag_manager_categories,
            &self.config.tag_match,
            lang,
            self.config.tag_register_name.name_lang(lang),
            self.active_tag_model().as_deref(),
            &self.tag_rules.rules,
        );
        self.key_menu = Some(KeyMenuCache { path: path.to_path_buf(), names });
    }

    /// 「タグ検索 ▶」。名前が1つも取れていない、または複数選択中は、全体がグレー。
    pub(super) fn draw_key_search_menu(&mut self, ui: &mut egui::Ui, path: &Path) {
        let t = i18n::t();
        let multi = !self.multi_selected.is_empty();
        let names: Vec<KeyName> = match &self.key_menu {
            Some(c) if !multi && c.path == path => c.names.clone(),
            _ => Vec::new(),
        };
        let enabled = !names.is_empty();
        let mut picked: Option<Role> = None;
        let top = ui.add_enabled_ui(enabled, |ui| {
            egui::containers::menu::SubMenuButton::new(t.key_search_menu()).ui(ui, |ui| {
                ui.set_min_width(240.0);
                for role in SEARCH_ROLES {
                    let of_role = search_keys::of_role(&names, role);
                    let label = role_label(role);
                    let mut button = egui::Button::new(label);
                    if let [only] = of_role.as_slice() {
                        button = button.right_text(egui::RichText::new(&only.name).weak());
                    } else if of_role.len() > 1 {
                        button = button.right_text(egui::RichText::new("…").weak());
                    }
                    let resp = ui.add_enabled(!of_role.is_empty(), button);
                    let resp = if of_role.is_empty() {
                        resp.on_disabled_hover_text(t.key_search_role_none(label))
                    } else {
                        resp.on_hover_text(key_hover(&of_role))
                    };
                    if resp.clicked() {
                        picked = Some(role);
                        ui.close();
                    }
                }
            })
        });
        if !enabled {
            top.inner.0.on_disabled_hover_text(if multi { t.key_search_disabled_multi() } else { t.key_search_disabled_none() });
        }
        if let Some(role) = picked {
            let of_role = search_keys::of_role(&names, role);
            if let [only] = of_role.as_slice() {
                self.request_key_search(search_keys::search_text(&[only]));
            } else if !of_role.is_empty() {
                let checked = vec![false; of_role.len()];
                self.key_pick = Some(KeyPickDialog { role, names: of_role, checked });
            }
        }
    }

    /// 選んだ名前で検索を始める。検索タブへ移って即実行する本体は次のフェーズ。いまは検索語をトーストで知らせるだけ。
    fn request_key_search(&mut self, text: String) {
        self.set_toast(i18n::t().key_search_pending_toast(&text));
    }

    /// 選択ダイアログ。1つもチェックが無いうちは「検索」がグレー。
    pub(super) fn draw_key_pick_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.key_pick.take() else { return };
        let t = i18n::t();
        let mut open = true;
        let (mut cancel, mut ok) = (false, false);
        egui::Window::new(t.key_pick_title(role_label(dlg.role)))
            .id(egui::Id::new("key_pick_window"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(self.explorer_center(ctx))
            .order(egui::Order::Foreground)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(t.key_pick_body());
                ui.add_space(6.0);
                for (n, checked) in dlg.names.iter().zip(dlg.checked.iter_mut()) {
                    let origin = if n.source == KeySource::Db { t.key_source_db() } else { t.key_source_file_name() };
                    ui.checkbox(checked, &n.name).on_hover_text(format!("{}（{}）", n.full, origin));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t.favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(dlg.checked.iter().any(|c| *c), egui::Button::new(t.key_pick_ok())).clicked() {
                        ok = true;
                    }
                });
            });
        if cancel || !open {
            return;
        }
        if ok {
            let chosen: Vec<&KeyName> = dlg.names.iter().zip(&dlg.checked).filter(|(_, c)| **c).map(|(n, _)| n).collect();
            let text = search_keys::search_text(&chosen);
            self.request_key_search(text);
            return;
        }
        self.key_pick = Some(dlg);
    }
}

/// 有効な項目のホバー: 各名前（別名つきは全体）と、判定の出どころ。
fn key_hover(names: &[KeyName]) -> String {
    let t = i18n::t();
    let mut lines: Vec<String> = names.iter().map(|n| n.full.clone()).collect();
    let from_file = names.iter().any(|n| n.source == KeySource::FileName);
    lines.push(if from_file { t.key_source_file_name() } else { t.key_source_db() }.to_string());
    lines.join("\n")
}
