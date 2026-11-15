//! 右クリック「主要タグ検索」（すだれメニュー）と、1つの種類に名前が複数あるときの選択ダイアログ。
//!
//! 「タグ検索 ▶ グループ／作者／演者／キャラクター／パロディ元／イベント／言語／年月」。種類ごとに、名前が1つなら即検索、
//! 複数なら選択ダイアログ（チェックした名前のOR）。名前の解決は `search_keys`（種類ごとに、DBのタグ優先、無ければファイル名抽出）。
//! 解決は「右クリックした瞬間」に1回だけ行ってキャッシュし、メニューを開いている間は読み直さない
//! （メニューは毎フレーム描き直されるため、DB読み取りを避ける）。
//! 項目は既定でグレー。単一ファイルで名前が取れた種類だけ有効。複数選択中は全体がグレー。

use std::path::{Path, PathBuf};

use crate::i18n;
use crate::search_keys::{self, KeyName, KeySource, KeyStatus, SEARCH_ROLES};
use crate::tag_candidates::Role;

use super::*;

/// 右クリックしたファイルについて解決した、検索の切り口になる名前のキャッシュ。
pub(super) struct KeyMenuCache {
    path: PathBuf,
    names: Vec<KeyName>,
    /// 主要タグの取得状況（サブメニュー末尾のステータスボタンの表示）。
    status: KeyStatus,
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
        // 高精度の根拠（ComicInfo.xml・meta.json の有無）は、キャッシュの詳細フラグから読む。現物と一致する記録だけを使い、
        // 記録が無い・古い（未確認）ときは根拠なし。
        let meta = std::fs::metadata(path).ok();
        let precise_meta = meta
            .and_then(|m| self.fresh_archive_info(path, m.modified().ok(), m.len()))
            .is_some_and(|r| search_keys::has_precise_meta(r.detail));
        let status = search_keys::key_status(&names, precise_meta);
        self.key_menu = Some(KeyMenuCache { path: path.to_path_buf(), names, status });
    }

    /// 「主要タグ検索 ▶」。名前が1つも取れていない、または複数選択中は、全体がグレー。
    pub(super) fn draw_key_search_menu(&mut self, ui: &mut egui::Ui, path: &Path) {
        let t = i18n::t();
        let multi = !self.multi_selected.is_empty();
        let (names, status): (Vec<KeyName>, KeyStatus) = match &self.key_menu {
            Some(c) if !multi && c.path == path => (c.names.clone(), c.status),
            _ => (Vec::new(), KeyStatus::Unavailable),
        };
        // 名前が1つも取れなくても、取得状況の案内（ステータスボタン）を開けるよう、サブメニュー自体は開ける。複数選択中だけグレー。
        let enabled = !multi;
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
                ui.separator();
                if self.draw_key_status_button(ui, status) {
                    ui.close();
                }
            })
        });
        if enabled {
            top.inner.0.on_hover_text(t.key_search_menu_hover());
        } else {
            top.inner.0.on_disabled_hover_text(t.key_search_disabled_multi());
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

    /// 取得状況のステータスボタン。高精度はグレーの表示のみ。簡易取得・取得不可は押すと案内を開く。
    /// 押されたら true（メニューを閉じる）。
    fn draw_key_status_button(&mut self, ui: &mut egui::Ui, status: KeyStatus) -> bool {
        let t = i18n::t();
        match status {
            KeyStatus::Precise => {
                ui.add_enabled(false, egui::Button::new(t.key_status_precise()))
                    .on_disabled_hover_text(t.key_status_precise_hover());
                false
            }
            KeyStatus::Simple | KeyStatus::Unavailable => {
                let label = if status == KeyStatus::Simple { t.key_status_simple() } else { t.key_status_unavailable() };
                let resp = ui.add(egui::Button::new(label).right_text(egui::RichText::new("?").weak()));
                let clicked = resp.on_hover_text(t.key_status_click_hover()).clicked();
                if clicked {
                    self.key_notice = Some(status);
                }
                clicked
            }
        }
    }

    /// 選んだ名前での検索を予約する。メニューのクリックはカードの描画ループの途中で呼ばれ、その中で検索タブへ移って
    /// 一覧（`archives`）を差し替えると、描画中の添字が範囲外になる。実行は、描画が終わってから（`run_pending_key_search`）。
    fn request_key_search(&mut self, text: String) {
        self.key_search_pending = Some(text);
        self.egui_ctx.request_repaint();
    }

    /// 予約された検索を実行する。画面の描画が終わった後（ダイアログ描画と同じ位置）で毎フレーム呼ぶ。
    /// 検索タブ（新モード）へ移ってすぐ検索を始める（ワンアクション）。
    /// 検索文字列はこの名前だけにし、サイズ条件は前回の入力が結果を黙って絞らないよう空にする。
    /// 台帳（DB）が無ければ移らずに知らせる。別の検索が実行中なら、条件だけ入れて移り、開始はせずに知らせる
    /// （検索は同時に1つだけ）。
    pub(super) fn run_pending_key_search(&mut self) {
        let Some(text) = self.key_search_pending.take() else { return };
        let t = i18n::t();
        if self.spread_db.is_none() {
            self.set_toast(t.key_search_no_ledger());
            return;
        }
        self.switch_folder_tab(FolderPaneTab::Search);
        self.set_search_mode(SearchMode::New);
        self.search_form.new_text = text;
        self.search_form.new_size_min_mb.clear();
        self.search_form.new_size_max_mb.clear();
        if self.any_search_running() {
            self.set_toast(t.key_search_busy());
            return;
        }
        self.start_new_search(None);
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
