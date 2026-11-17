//! タグ検索タブ（モック）。左ペインにタグ一覧（カテゴリ見出し＋チップ）を並べ、
//! 中央は仮表示のみ。検索・DB読み取り・永続化はまだ接続していない。
//!
//! 左ペイン（タグ一覧）と中央の境目は、右タグパネルと同じ中央グリップのD&Dで幅を変えられる。
//! 幅は `tab_positions.tag_search_left_width` に保存し、ウィンドウが狭くても左を優先して
//! 保存幅どおりに再現する（右タグパネルや中央が先に削られる）。
//!
//! タブに入ると中央の `archives` を結果で差し替え（お気に入り・検索の横断一覧と同じ方式）、
//! タブを出ると走査を打ち切って実フォルダの表示へ戻す。チップを切り替えるたびに古い走査を
//! 捨てて最初からやり直す。ヒットは到着順に追記し、完了時に1回だけソートする。
//! 前回の確認で見つからなかったファイルは一覧から外し、数だけ見出しに出す。
//!
//! 有効タグは `tag_search_enabled`（メイン未設定の疑似タグは `TAG_MAIN_UNSET_ID`）に
//! セッション中だけ保持する。

use std::collections::HashSet;
use std::path::PathBuf;

use crate::fs::archive;

use crate::i18n;
use crate::tag_manager::TAG_MAIN_UNSET_ID;
use crate::tag_search::{TagHit, TagMatchMode, TagQuery, TagScanMsg};

use super::panels::{tag_chip, tag_chip_with_x};
use super::*;

/// 左ペイン幅の既定値。他のタブの固定幅と同じ。
const FOLDER_PANEL_DEFAULT_WIDTH: f32 = 200.0;
const TAG_SEARCH_LEFT_MIN_WIDTH: f32 = 180.0;
const TAG_SEARCH_LEFT_MAX_WIDTH: f32 = 600.0;
const TAG_SEARCH_HANDLE_WIDTH: f32 = 6.0;
const TAG_SEARCH_HANDLE_HEIGHT: f32 = 56.0;
/// 「有効なタグ」一覧の最大高さ。超えたら一覧の中でスクロールする。
const TAG_SEARCH_ACTIVE_MAX_HEIGHT: f32 = 96.0;

/// 保存値（未保存なら既定）を、許容範囲へ収めた左ペイン幅にする。
pub(super) fn resolve_tag_search_left_width(saved: Option<u32>) -> f32 {
    saved
        .map_or(FOLDER_PANEL_DEFAULT_WIDTH, |w| w as f32)
        .clamp(TAG_SEARCH_LEFT_MIN_WIDTH, TAG_SEARCH_LEFT_MAX_WIDTH)
}

/// チップのON/OFFを切り替える。ONのチップを押したらOFF。OFFのチップを押したらON。
/// 単一選択カテゴリ（`single_group` にそのカテゴリの全チップID）では、同じカテゴリで
/// すでにONのチップを先に外す（ラジオ式の排他選択）。
fn toggle_tag_search_chip(enabled: &mut HashSet<u64>, id: u64, single_group: Option<&[u64]>) {
    if enabled.remove(&id) {
        return;
    }
    if let Some(group) = single_group {
        for other in group {
            enabled.remove(other);
        }
    }
    enabled.insert(id);
}

/// 一覧に出す1カテゴリ: (見出し名, 色, 単一選択か, 排他の対象になる全チップのid, 表示するチップ)。
type TagSearchCategory = (String, egui::Color32, bool, Vec<u64>, Vec<(u64, String)>);

/// 要素名が絞り込み文字列に当たるか。大文字小文字を区別しない部分一致で、別名つき（`A|B|C`）は全別名が対象。
/// 空（空白のみ含む）なら全部に当たる。
fn tag_name_matches_filter(name: &str, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    crate::tag_master::split_aliases(name).iter().any(|a| a.to_lowercase().contains(&f)) || name.to_lowercase().contains(&f)
}

/// 「この要素をタグ検索」の有効タグ集合（1要素のみ）。
fn single_tag_enabled(id: u64) -> HashSet<u64> {
    HashSet::from([id])
}

impl NekoviewApp {
    /// 左ペイン（folder_panel）の幅。タグ検索タブだけ可変で、他のタブは固定。
    pub(super) fn folder_panel_width(&self) -> f32 {
        if self.folder_pane_tab == FolderPaneTab::TagSearch {
            resolve_tag_search_left_width(self.tab_positions.tag_search_left_width)
        } else {
            FOLDER_PANEL_DEFAULT_WIDTH
        }
    }

    /// 左ペイン右端の中央グリップ。ドラッグ中は幅をメモリ上で更新し、離したときに保存する。
    fn draw_tag_search_resize_handle(&mut self, ui: &mut egui::Ui, strip: egui::Rect) {
        let handle_height = TAG_SEARCH_HANDLE_HEIGHT.min(strip.height());
        let handle_rect = egui::Rect::from_center_size(strip.center(), egui::vec2(strip.width(), handle_height));
        let response = ui.interact(handle_rect, ui.id().with("tag_search_resize_handle"), egui::Sense::drag());
        if response.dragged() {
            let w = self.folder_panel_width() + response.drag_delta().x;
            let w = w.clamp(TAG_SEARCH_LEFT_MIN_WIDTH, TAG_SEARCH_LEFT_MAX_WIDTH);
            self.tab_positions.tag_search_left_width = Some(w.round() as u32);
        }
        if response.drag_stopped() {
            self.persist_state();
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let visuals = ui.style().interact(&response);
        ui.painter().rect_filled(handle_rect, 3.0, visuals.bg_fill);
        let center = handle_rect.center();
        for dy in [-6.0, 0.0, 6.0] {
            ui.painter().circle_filled(center + egui::vec2(0.0, dy), 1.5, visuals.fg_stroke.color);
        }
    }

    pub(super) fn draw_tag_search_left_pane(&mut self, ui: &mut egui::Ui) {
        // 右端をグリップ用に空け、チップ等はその内側に描く。
        let full = ui.available_rect_before_wrap();
        let strip = egui::Rect::from_min_max(egui::pos2(full.right() - TAG_SEARCH_HANDLE_WIDTH, full.top()), full.right_bottom());
        let content = egui::Rect::from_min_max(full.min, egui::pos2(strip.left(), full.bottom()));
        self.draw_tag_search_resize_handle(ui, strip);
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| self.draw_tag_search_list(ui));
    }

    /// 左ペイン本体。上から「OR/AND・クリア」「要素フィルタ」「有効なタグ一覧」を固定で置き、
    /// その下に全カテゴリのチップ一覧（スクロール）を並べる。長い一覧の外に有効タグが隠れないようにするため。
    fn draw_tag_search_list(&mut self, ui: &mut egui::Ui) {
        ui.weak(i18n::t().tag_search_left_hint());
        ui.separator();
        let mut changed = false;
        changed |= self.draw_tag_search_mode_row(ui);
        self.draw_tag_search_filter_row(ui);
        changed |= self.draw_tag_search_active_list(ui);
        ui.separator();
        let mode = self.tag_match_mode();
        let filter = self.tag_search_filter.clone();
        egui::ScrollArea::vertical()
            .id_salt("tag_search_left_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let full_w = ui.available_width();
                // マスタ定義の借用とチップのトグルが衝突しないよう、先に描画対象を集める。
                // メインカテゴリは先頭固定で、その先頭に「未設定」の疑似チップを置く。
                let cats = self.tag_search_categories(&filter);
                if cats.is_empty() && !filter.trim().is_empty() {
                    ui.weak(i18n::t().tag_search_no_match());
                }
                for (name, color, single_select, group, chips) in cats {
                    // 単一選択の排他はANDのときだけ。ORでは同じカテゴリの複数選択が自然な使い方になる。
                    let single = single_select && mode == TagMatchMode::And;
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(name).strong().color(color));
                        if single {
                            // 単一選択のカテゴリの目印。中のチップは同時に1つしか有効にできない。
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(" S ")
                                        .small()
                                        .strong()
                                        .color(contrasting_text_color(color))
                                        .background_color(color),
                                )
                                .sense(egui::Sense::hover()),
                            )
                            .on_hover_text(i18n::t().tag_search_single_hint());
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for (id, text) in chips {
                            let on = self.tag_search_enabled.contains(&id);
                            let (fill, text_color) = if on {
                                (color, contrasting_text_color(color))
                            } else {
                                (ui.visuals().widgets.inactive.weak_bg_fill, ui.visuals().text_color())
                            };
                            let resp = tag_chip(ui, &text, full_w, fill, text_color, egui::Sense::click(), !on);
                            if !on {
                                // 無効でもカテゴリを見分けられるよう、枠だけカテゴリ色にする。
                                ui.painter().rect_stroke(
                                    resp.rect,
                                    3.0,
                                    egui::Stroke::new(1.0, color),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if resp.clicked() {
                                toggle_tag_search_chip(&mut self.tag_search_enabled, id, single.then_some(group.as_slice()));
                                changed = true;
                            }
                        }
                    });
                }
            });
        if changed {
            self.restart_tag_search();
        }
    }

    /// 現在の組み合わせ方（未保存=OR）。
    pub(super) fn tag_match_mode(&self) -> TagMatchMode {
        self.tab_positions.tag_search_mode.unwrap_or_default()
    }

    /// OR/ANDのラジオと「クリア」。変更があれば true（呼び出し側が再走査する）。
    fn draw_tag_search_mode_row(&mut self, ui: &mut egui::Ui) -> bool {
        let t = i18n::t();
        let mut changed = false;
        let mode = self.tag_match_mode();
        let mut new_mode = mode;
        ui.horizontal(|ui| {
            if ui.radio(mode == TagMatchMode::Or, t.tag_search_mode_or()).clicked() {
                new_mode = TagMatchMode::Or;
            }
            if ui.radio(mode == TagMatchMode::And, t.tag_search_mode_and()).clicked() {
                new_mode = TagMatchMode::And;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has_any = !self.tag_search_enabled.is_empty();
                if ui.add_enabled(has_any, egui::Button::new(t.tag_search_clear())).clicked() {
                    self.tag_search_enabled.clear();
                    changed = true;
                }
            });
        });
        if new_mode != mode {
            // ANDへ戻したとき、すでに複数有効な単一選択カテゴリはその場では直さない（次にそのカテゴリを押したとき排他になる）。
            self.tab_positions.tag_search_mode = Some(new_mode);
            self.persist_state();
            changed = true;
        }
        changed
    }

    /// 要素フィルタ欄（下のチップ一覧だけを絞る。保存しない）。
    fn draw_tag_search_filter_row(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.tag_search_filter)
                .hint_text(i18n::t().tag_search_filter_hint())
                .desired_width(f32::INFINITY),
        );
    }

    /// 「有効なタグ」一覧。×で個別に解除する。0件なら「未選択」。解除があれば true。
    fn draw_tag_search_active_list(&mut self, ui: &mut egui::Ui) -> bool {
        let t = i18n::t();
        let items = self.tag_search_active_items();
        let mut removed = None;
        ui.add_space(2.0);
        ui.label(egui::RichText::new(t.tag_search_active_heading()).small().strong());
        if items.is_empty() {
            ui.weak(t.tag_search_active_none());
        } else {
            let full_w = ui.available_width();
            egui::ScrollArea::vertical()
                .id_salt("tag_search_active_scroll")
                .max_height(TAG_SEARCH_ACTIVE_MAX_HEIGHT)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (id, text, color) in &items {
                            if tag_chip_with_x(ui, text, full_w, *color, contrasting_text_color(*color), *id) {
                                removed = Some(*id);
                            }
                        }
                    });
                });
        }
        if let Some(id) = removed {
            self.tag_search_enabled.remove(&id);
            return true;
        }
        false
    }

    /// 有効タグの表示用一覧 `(id, 表示名, 色)`。カテゴリ登録順→要素順で、メインカテゴリの「未設定」を先頭に置く。
    /// マスタに無いid（削除済みtier）は出さない。
    fn tag_search_active_items(&self) -> Vec<(u64, String, egui::Color32)> {
        let mut out = Vec::new();
        for c in &self.tag_manager_categories {
            if c.is_main && self.tag_search_enabled.contains(&TAG_MAIN_UNSET_ID) {
                out.push((TAG_MAIN_UNSET_ID, i18n::t().tag_main_unset_label().to_string(), c.color));
            }
            for tier in &c.tiers {
                if let Some(name) = tier.element.as_ref().filter(|_| self.tag_search_enabled.contains(&tier.id)) {
                    out.push((tier.id, name.clone(), c.color));
                }
            }
        }
        out
    }

    /// 一覧に出すカテゴリ `TagSearchCategory`。`filter` に当たるチップだけを残し、
    /// 1つも残らないカテゴリは除く。メインカテゴリの先頭には「未設定」の疑似チップを置く。
    fn tag_search_categories(&self, filter: &str) -> Vec<TagSearchCategory> {
        self.tag_manager_categories
            .iter()
            .filter_map(|c| {
                let mut chips: Vec<(u64, String)> = Vec::new();
                if c.is_main {
                    chips.push((TAG_MAIN_UNSET_ID, i18n::t().tag_main_unset_label().to_string()));
                }
                chips.extend(c.tiers.iter().filter_map(|t| t.element.as_ref().map(|name| (t.id, name.clone()))));
                // 排他は絞り込み前の全チップが対象（見えていないチップが有効なままにならないように）
                let group: Vec<u64> = chips.iter().map(|(id, _)| *id).collect();
                chips.retain(|(_, name)| tag_name_matches_filter(name, filter));
                // メインカテゴリは常に単一選択。
                (!chips.is_empty()).then(|| (c.display_name(), c.color, c.is_main || c.single_select, group, chips))
            })
            .collect()
    }

    /// タグチップの左クリックポップアップ「この要素名（X）をタグ検索」（確認メニュー）。
    /// 開いている要素は `open_id` に1つだけ持つ。
    /// - 要素を左クリック: 閉じていれば開く。開いていれば閉じる。
    /// - 他の要素を左クリック: 先のメニューを閉じて、その要素のメニューを開く。
    /// - エクスプローラー上の他の場所を左クリック: ポップアップの外クリックとして閉じる。
    /// - 項目を押す: `search_req` に予約して閉じる（実行は描画後、`run_pending_tag_search`）。
    /// `searchable` が false（メインの「未設定」）のときは項目がグレーで押せない。
    pub(super) fn tag_chip_search_popup(
        resp: &egui::Response,
        open_id: &mut Option<u64>,
        search_req: &mut Option<u64>,
        id: u64,
        name: &str,
        searchable: bool,
    ) {
        let was_open = *open_id == Some(id);
        let mut open = was_open;
        let mut picked = false;
        egui::Popup::menu(resp)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let t = i18n::t();
                let label = t.tag_chip_search_menu(crate::tag_master::display_name(name));
                let item = ui.add_enabled(searchable, egui::Button::new(label));
                let item = if searchable { item } else { item.on_disabled_hover_text(t.tag_chip_search_unset_hint()) };
                picked = item.clicked();
            });
        if picked {
            *search_req = Some(id);
            *open_id = None;
        } else if resp.clicked() {
            *open_id = if was_open { None } else { Some(id) };
            resp.ctx.request_repaint();
        } else if was_open && !open {
            *open_id = None;
        }
    }

    pub(super) fn request_tag_search(&mut self, id: u64) {
        self.tag_search_pending = Some(id);
        self.egui_ctx.request_repaint();
    }

    /// 予約された「この要素をタグ検索」を実行する。描画が終わった後に毎フレーム呼ぶ
    /// （チップの描画中に一覧を差し替えると添字が範囲外になるため）。
    /// 有効タグはその1要素だけに置き換え、タグ検索タブへ移って検索する（すでにタブ内なら再走査のみ）。
    pub(super) fn run_pending_tag_search(&mut self) {
        let Some(id) = self.tag_search_pending.take() else { return };
        if self.spread_db.is_none() {
            self.set_toast(i18n::t().key_search_no_ledger());
            return;
        }
        let already_in_tab = self.folder_pane_tab == FolderPaneTab::TagSearch;
        self.tag_search_enabled = single_tag_enabled(id);
        // 入るときは enter_tag_search_view が再走査する。すでにタブ内なら自前でやり直す。
        self.switch_folder_tab(FolderPaneTab::TagSearch);
        if already_in_tab {
            self.restart_tag_search();
        }
    }

    /// 中央の一覧を空にする（タブに入るとき・条件を変えるとき）。
    fn clear_tag_search_listing(&mut self) {
        self.archives.clear();
        self.cross_view_favorite_markers.clear();
        self.saved_archive_settings.clear();
        self.raw_image_files.clear();
        self.subdirs.clear();
        self.subdir_mtimes.clear();
        // 複数ディレクトリ横断のため単一ディレクトリ前提のキャッシュDB/セッション状態は無効化する
        self.cache_db = None;
        self.invalid_archives.clear();
        self.thumb_failed.clear();
        // 別タブ終了時に開始された実ディレクトリのスキャンが Loading のまま残ると、
        // 結果よりローディング表示が優先され続ける。
        self.scan_state = ScanState::Done;
        self.tag_search_unconfirmed = 0;
        self.selected_archive_index = None;
        self.selected_archive_meta = None;
        self.multi_selected.clear();
        self.select_anchor = None;
        self.recompute_filter();
    }

    /// タグ検索タブに入ったとき: 一覧を結果表示に切り替え、有効タグがあれば走査を始める。
    pub(super) fn enter_tag_search_view(&mut self) {
        self.tag_search_view_open = true;
        self.close_similar_view_state();
        self.restart_tag_search();
    }

    /// タグ検索タブを出るとき: 走査を打ち切って実フォルダの表示へ戻す。開いていなければ何もしない。
    pub(super) fn exit_tag_search_view(&mut self) {
        if !self.tag_search_view_open {
            return;
        }
        self.tag_search_view_open = false;
        self.tag_search_scan = None;
        self.tag_search_running = false;
        self.start_scan();
    }

    /// 古い走査を捨てて一覧を空にし、有効タグに合わせて走査をやり直す（有効タグが無ければ空のまま）。
    fn restart_tag_search(&mut self) {
        self.tag_search_scan = None;
        self.tag_search_running = false;
        self.clear_tag_search_listing();
        let Some(db) = self.spread_db.clone() else { return };
        let main_ids = self
            .tag_manager_categories
            .iter()
            .filter(|c| c.is_main)
            .flat_map(|c| c.tiers.iter().map(|t| t.id))
            .collect();
        let query = TagQuery::new(&self.tag_search_enabled, main_ids, self.tag_match_mode());
        if query.is_empty() {
            return;
        }
        let ctx = self.egui_ctx.clone();
        self.tag_search_scan = Some(crate::tag_search::spawn(db, query, move || ctx.request_repaint()));
        self.tag_search_running = true;
    }

    /// 走査スレッドの結果を受け取る（毎フレーム）。ヒットは追記し、完了で1回だけソートする。
    pub(super) fn poll_tag_search(&mut self) {
        let Some(scan) = &self.tag_search_scan else { return };
        let mut chunks: Vec<Vec<TagHit>> = Vec::new();
        let mut done = false;
        while let Ok(msg) = scan.rx.try_recv() {
            match msg {
                TagScanMsg::Hits(hits) => chunks.push(hits),
                TagScanMsg::Done { .. } => done = true,
            }
        }
        for hits in chunks {
            self.append_tag_search_hits(hits);
        }
        if done {
            self.tag_search_scan = None;
            self.tag_search_running = false;
            self.sort_archives();
            self.recompute_filter();
        }
    }

    fn append_tag_search_hits(&mut self, hits: Vec<TagHit>) {
        let (confirmed, missing): (Vec<TagHit>, Vec<TagHit>) = hits.into_iter().partition(|h| h.confirmed);
        self.tag_search_unconfirmed += missing.len();
        let paths: Vec<PathBuf> = confirmed.into_iter().map(|h| h.path).collect();
        if paths.is_empty() {
            return;
        }
        if let Some(db) = self.spread_db.clone() {
            self.cross_view_favorite_markers.extend(crate::favorites::memberships_for_paths(&db, &paths));
            self.saved_archive_settings.extend(crate::spread_state::saved_settings_for_paths(&db, &paths));
        }
        self.raw_image_files.extend(paths.iter().filter(|p| archive::is_supported_image_file(p)).cloned());
        self.archives.extend(paths);
        self.recompute_filter();
    }

    /// 中央上部の見出し: ヒット件数（走査中はその旨）と、一覧から外した確認できない件数。
    pub(super) fn tag_search_header(&self) -> String {
        i18n::t().tag_search_header(self.archives.len(), self.tag_search_running, self.tag_search_unconfirmed)
    }

    pub(super) fn draw_tag_search_central(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.weak(i18n::t().tag_search_center_guide());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_is_case_insensitive_substring_over_all_aliases() {
        assert!(tag_name_matches_filter("Sousou no Frieren", ""));
        assert!(tag_name_matches_filter("Sousou no Frieren", "  "));
        assert!(tag_name_matches_filter("Sousou no Frieren", "frieren"));
        assert!(!tag_name_matches_filter("Sousou no Frieren", "xyz"));
        assert!(tag_name_matches_filter("葬送のフリーレン|Frieren", "frie"));
        assert!(tag_name_matches_filter("葬送のフリーレン|Frieren", "葬送"));
        assert!(!tag_name_matches_filter("葬送のフリーレン|Frieren", "ABC"));
    }

    #[test]
    fn single_tag_enabled_holds_only_that_element() {
        let set = single_tag_enabled(7);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&7));
    }

    #[test]
    fn left_width_defaults_and_clamps_to_range() {
        assert_eq!(resolve_tag_search_left_width(None), 200.0);
        assert_eq!(resolve_tag_search_left_width(Some(320)), 320.0);
        assert_eq!(resolve_tag_search_left_width(Some(10)), TAG_SEARCH_LEFT_MIN_WIDTH);
        assert_eq!(resolve_tag_search_left_width(Some(5000)), TAG_SEARCH_LEFT_MAX_WIDTH);
    }

    #[test]
    fn single_group_allows_only_one_enabled_chip() {
        let mut on: HashSet<u64> = HashSet::new();
        let group = [0u64, 1, 2];
        toggle_tag_search_chip(&mut on, 1, Some(&group));
        toggle_tag_search_chip(&mut on, 2, Some(&group));
        assert_eq!(on, HashSet::from([2]), "別のチップを押すと入れ替わる");
        toggle_tag_search_chip(&mut on, 2, Some(&group));
        assert!(on.is_empty(), "押し直しでOFF");
        // 他カテゴリのチップは巻き込まない
        toggle_tag_search_chip(&mut on, 10, None);
        toggle_tag_search_chip(&mut on, 1, Some(&group));
        toggle_tag_search_chip(&mut on, 0, Some(&group));
        assert_eq!(on, HashSet::from([10, 0]));
    }

    #[test]
    fn multi_group_allows_several_enabled_chips() {
        let mut on: HashSet<u64> = HashSet::new();
        toggle_tag_search_chip(&mut on, 10, None);
        toggle_tag_search_chip(&mut on, 11, None);
        assert_eq!(on, HashSet::from([10, 11]));
    }

    #[test]
    fn header_shows_count_running_and_unconfirmed() {
        let t = crate::i18n::t();
        let plain = t.tag_search_header(12, false, 0);
        let running = t.tag_search_header(12, true, 0);
        let missing = t.tag_search_header(12, false, 3);
        assert!(plain.contains("12"));
        assert!(running.len() > plain.len(), "走査中の注記が付く");
        assert!(missing.len() > plain.len() && missing.contains('3'), "確認できない件数が付く");
    }
}
