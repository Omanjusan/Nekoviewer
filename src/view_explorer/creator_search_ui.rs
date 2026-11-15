//! 右クリック「この作者／グループのファイルを検索」の入口と、複数候補のときの選択ダイアログ。
//!
//! 名前の解決は `creator_names`（DBのタグ優先、無ければ学習データでのファイル名抽出）。
//! 解決は「右クリックした瞬間」に1回だけ行ってキャッシュし、メニューを開いている間は読み直さない
//! （メニューは毎フレーム描き直されるため、DB読み取りを避ける）。
//! 項目は既定でグレー。単一ファイルで名前が取れたときだけ有効。複数選択中は無効。

use std::path::{Path, PathBuf};

use crate::creator_names::{self, CreatorName, CreatorSource};
use crate::i18n;
use crate::tag_candidates::Role;

use super::*;

/// 右クリックしたファイルについて解決した、作者・グループ名のキャッシュ。
pub(super) struct CreatorMenuCache {
    path: PathBuf,
    names: Vec<CreatorName>,
}

/// 複数候補のときの選択ダイアログ。`checked` は `names` と同じ並び。
pub(super) struct CreatorPickDialog {
    names: Vec<CreatorName>,
    checked: Vec<bool>,
}

fn role_label(role: Role) -> &'static str {
    let t = i18n::t();
    if role == Role::Author { t.creator_role_author() } else { t.creator_role_group() }
}

impl NekoviewApp {
    /// 右クリックの瞬間に呼ぶ。そのファイルの作者・グループ名を解決して持つ。
    pub(super) fn cache_creator_menu(&mut self, path: &Path) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            self.creator_menu = None;
            return;
        };
        let tag_ids = match self.spread_db.as_ref() {
            Some(db) => crate::spread_state::read_archive_tags(db, dir, name),
            None => Vec::new(),
        };
        let lang = i18n::t();
        let names = creator_names::resolve_creators(
            &tag_ids,
            name,
            &self.tag_manager_categories,
            &self.config.tag_match,
            lang,
            self.config.tag_register_name.name_lang(lang),
            self.active_tag_model().as_deref(),
            &self.tag_rules.rules,
        );
        self.creator_menu = Some(CreatorMenuCache { path: path.to_path_buf(), names });
    }

    /// メニュー項目。名前が取れていて、単一ファイルを右クリックしたときだけ有効。
    pub(super) fn draw_creator_search_menu_item(&mut self, ui: &mut egui::Ui, path: &Path) {
        let t = i18n::t();
        let multi = !self.multi_selected.is_empty();
        let names: Vec<CreatorName> = match &self.creator_menu {
            Some(c) if !multi && c.path == path => c.names.clone(),
            _ => Vec::new(),
        };
        let enabled = !names.is_empty();
        let mut button = egui::Button::new(t.creator_search_menu());
        if let [only] = names.as_slice() {
            button = button.right_text(egui::RichText::new(&only.name).weak());
        } else if names.len() > 1 {
            button = button.right_text(egui::RichText::new("…").weak());
        }
        let resp = ui.add_enabled(enabled, button);
        let resp = if enabled {
            resp.on_hover_text(creator_hover(&names))
        } else {
            resp.on_disabled_hover_text(if multi { t.creator_search_disabled_multi() } else { t.creator_search_disabled_none() })
        };
        if resp.clicked() {
            if let [only] = names.as_slice() {
                self.request_creator_search(creator_names::search_text(&[only]));
            } else {
                let checked = vec![false; names.len()];
                self.creator_pick = Some(CreatorPickDialog { names, checked });
            }
            ui.close();
        }
    }

    /// 選んだ名前で検索を始める。検索タブへ移って即実行する本体は次のフェーズ。いまは検索語をトーストで知らせるだけ。
    fn request_creator_search(&mut self, text: String) {
        self.set_toast(i18n::t().creator_search_pending_toast(&text));
    }

    /// 選択ダイアログ。1つもチェックが無いうちは「検索」がグレー。
    pub(super) fn draw_creator_pick_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.creator_pick.take() else { return };
        let t = i18n::t();
        let mut open = true;
        let (mut cancel, mut ok) = (false, false);
        egui::Window::new(t.creator_pick_title())
            .id(egui::Id::new("creator_pick_window"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(self.explorer_center(ctx))
            .order(egui::Order::Foreground)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(t.creator_pick_body());
                ui.add_space(6.0);
                for (n, checked) in dlg.names.iter().zip(dlg.checked.iter_mut()) {
                    let origin = if n.source == CreatorSource::Db { t.creator_source_db() } else { t.creator_source_file_name() };
                    let resp = ui.checkbox(checked, format!("[{}] {}", role_label(n.role), n.name));
                    resp.on_hover_text(format!("{}（{}）", n.full, origin));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t.favorite_dialog_cancel()).clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(dlg.checked.iter().any(|c| *c), egui::Button::new(t.creator_pick_ok())).clicked() {
                        ok = true;
                    }
                });
            });
        if cancel || !open {
            return;
        }
        if ok {
            let chosen: Vec<&CreatorName> = dlg.names.iter().zip(&dlg.checked).filter(|(_, c)| **c).map(|(n, _)| n).collect();
            let text = creator_names::search_text(&chosen);
            self.request_creator_search(text);
            return;
        }
        self.creator_pick = Some(dlg);
    }
}

/// 有効なときのホバー: 各名前（別名つきは全体）と、判定の出どころ。
fn creator_hover(names: &[CreatorName]) -> String {
    let t = i18n::t();
    let mut lines: Vec<String> = names.iter().map(|n| format!("{}: {}", role_label(n.role), n.full)).collect();
    let from_file = names.iter().any(|n| n.source == CreatorSource::FileName);
    lines.push(if from_file { t.creator_source_file_name() } else { t.creator_source_db() }.to_string());
    lines.join("\n")
}
