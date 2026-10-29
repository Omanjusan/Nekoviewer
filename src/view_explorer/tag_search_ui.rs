//! タグ検索タブ（モック）。左ペインにタグ一覧（カテゴリ見出し＋チップ）を並べ、
//! 中央は仮表示のみ。検索・DB読み取り・永続化はまだ接続していない。
//!
//! 左ペイン（タグ一覧）と中央の境目は、右タグパネルと同じ中央グリップのD&Dで幅を変えられる。
//! 幅は `tab_positions.tag_search_left_width` に保存し、ウィンドウが狭くても左を優先して
//! 保存幅どおりに再現する（右タグパネルや中央が先に削られる）。
//!
//! 有効タグは `tag_search_enabled`（メイン未設定の疑似タグは `TAG_MAIN_UNSET_ID`）に
//! セッション中だけ保持する。

use crate::i18n;
use crate::tag_manager::TAG_MAIN_UNSET_ID;

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
                        (c.display_name(), c.color, chips)
                    })
                    .collect();
                for (name, color, chips) in cats {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(name).strong().color(color));
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
                            if resp.clicked() && !self.tag_search_enabled.remove(&id) {
                                self.tag_search_enabled.insert(id);
                            }
                        }
                    });
                }
            });
    }

    pub(super) fn draw_tag_search_central(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            if self.tag_search_enabled.is_empty() {
                ui.weak(i18n::t().tag_search_center_guide());
            } else {
                ui.label(i18n::t().tag_search_center_mock(self.tag_search_enabled.len()));
            }
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
}
