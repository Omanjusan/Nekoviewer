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
use crate::tag_search::{TagHit, TagQuery, TagScanMsg};

use super::panels::tag_chip;
use super::*;

/// 左ペイン幅の既定値。他のタブの固定幅と同じ。
const FOLDER_PANEL_DEFAULT_WIDTH: f32 = 200.0;
const TAG_SEARCH_LEFT_MIN_WIDTH: f32 = 180.0;
const TAG_SEARCH_LEFT_MAX_WIDTH: f32 = 600.0;
const TAG_SEARCH_HANDLE_WIDTH: f32 = 6.0;
const TAG_SEARCH_HANDLE_HEIGHT: f32 = 56.0;

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

    fn draw_tag_search_list(&mut self, ui: &mut egui::Ui) {
        ui.weak(i18n::t().tag_search_left_hint());
        ui.separator();
        let mut changed = false;
        egui::ScrollArea::vertical()
            .id_salt("tag_search_left_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let full_w = ui.available_width();
                // マスタ定義の借用とチップのトグルが衝突しないよう、先に描画対象を集める。
                // メインカテゴリは先頭固定で、その先頭に「未設定」の疑似チップを置く。
                let cats: Vec<_> = self
                    .tag_manager_categories
                    .iter()
                    .map(|c| {
                        let mut chips: Vec<(u64, String)> = Vec::new();
                        if c.is_main {
                            chips.push((TAG_MAIN_UNSET_ID, i18n::t().tag_main_unset_label().to_string()));
                        }
                        chips.extend(
                            c.tiers
                                .iter()
                                .filter_map(|t| t.element.as_ref().map(|name| (t.id, name.clone()))),
                        );
                        // メインカテゴリは常に単一選択。
                        (c.display_name(), c.color, c.is_main || c.single_select, chips)
                    })
                    .collect();
                for (name, color, single, chips) in cats {
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
                    let group: Vec<u64> = chips.iter().map(|(id, _)| *id).collect();
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
        let query = TagQuery::new(&self.tag_search_enabled, main_ids);
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
