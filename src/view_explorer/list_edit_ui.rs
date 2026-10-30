//! リストの評価・訪問回数セルの「＋－」ボタン（編集のロックを解除している間だけ出す）。
//!
//! セルは2段になる: 上段に今の値、下段に [－][＋]。ボタンの矩形は描画のたびに `list_edit_hits` へ登録し、
//! 押下は `try_click_edit` が選択や起動より先に拾う（「ファイラー」ボタンと同じ方式）。
//! 値を端まで動かしたボタンは灰色にして押せなくする。書き込みは `spread_state` の加減算（読み書き一体）。

use crate::i18n;
use crate::spread_state::{ArchiveRating, RATING_HALF_MAX, VISIT_COUNT_MAX};

use super::view_mode::ViewMode;
use super::*;

const BTN_W: f32 = 22.0;
const BTN_H: f32 = 18.0;
const BTN_GAP: f32 = 6.0;
const CELL_PAD: f32 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EditBtn {
    RatingDown,
    RatingUp,
    VisitDown,
    VisitUp,
}

impl EditBtn {
    fn is_up(self) -> bool {
        matches!(self, Self::RatingUp | Self::VisitUp)
    }

    fn is_rating(self) -> bool {
        matches!(self, Self::RatingDown | Self::RatingUp)
    }

    /// 押せる状態か。端（未評価・★5.0・訪問0・訪問上限）まで動かした側は押せない。
    pub(super) fn enabled(self, rating: Option<ArchiveRating>) -> bool {
        let r = rating.unwrap_or_default();
        match self {
            Self::RatingDown => r.rating_half > 0,
            Self::RatingUp => r.rating_half < RATING_HALF_MAX,
            Self::VisitDown => r.visit_count > 0,
            Self::VisitUp => r.visit_count < VISIT_COUNT_MAX,
        }
    }
}

/// 編集中のセルの内訳。
pub(super) struct EditCellRects {
    /// 上段（今の値を描く）
    pub value: egui::Rect,
    pub down: egui::Rect,
    pub up: egui::Rect,
}

/// セルを上下に2分し、下段の左に [－][＋] を並べる。
pub(super) fn edit_cell_rects(cell: egui::Rect) -> EditCellRects {
    let half = cell.height() / 2.0;
    let value = egui::Rect::from_min_size(cell.min, egui::vec2(cell.width(), half));
    let y = cell.min.y + half * 1.5;
    let down = egui::Rect::from_center_size(egui::pos2(cell.min.x + CELL_PAD + BTN_W / 2.0, y), egui::vec2(BTN_W, BTN_H));
    let up = down.translate(egui::vec2(BTN_W + BTN_GAP, 0.0));
    EditCellRects { value, down, up }
}

impl NekoviewApp {
    /// いま、行に＋－を出す状態か（リスト表示で、編集のロックを解除している）。
    pub(super) fn list_editing(&self) -> bool {
        self.list_edit_lock.is_unlocked() && self.view_mode.mode == ViewMode::List
    }

    /// 編集中セルの下段に [－][＋] を描き、当たり判定を登録する。
    pub(super) fn paint_edit_buttons(
        &mut self,
        ui: &egui::Ui,
        cell: egui::Rect,
        real_idx: usize,
        (down, up): (EditBtn, EditBtn),
        rating: Option<ArchiveRating>,
    ) {
        let rects = edit_cell_rects(cell);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let painter = ui.painter().with_clip_rect(cell.intersect(ui.clip_rect()));
        let visuals = ui.visuals();
        for (btn, rect, symbol) in [(down, rects.down, "－"), (up, rects.up, "＋")] {
            let enabled = btn.enabled(rating);
            let hovered = enabled && pointer.is_some_and(|p| rect.contains(p));
            let fill = if hovered { visuals.widgets.hovered.bg_fill } else { visuals.widgets.inactive.bg_fill };
            let text_color = if enabled { visuals.strong_text_color() } else { visuals.weak_text_color().gamma_multiply(0.5) };
            painter.rect_filled(rect, 3.0, if enabled { fill } else { fill.gamma_multiply(0.5) });
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, symbol, egui::FontId::proportional(13.0), text_color);
            self.list_edit_hits.push((real_idx, btn, rect, enabled));
        }
    }

    /// ＋－を押したときの処理。押したら true（選択や起動へ回さない）。押せない端のボタンも握りつぶす。
    pub(super) fn try_click_edit(&mut self, ui: &egui::Ui, response: &egui::Response, real_idx: usize) -> bool {
        // 当たり判定は描画のたびに作り直す。カードに切り替えた直後の古い矩形は拾わない。
        if !self.list_editing() || !(response.clicked() || response.double_clicked()) {
            return false;
        }
        let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else { return false };
        let Some(&(_, btn, _, enabled)) =
            self.list_edit_hits.iter().find(|(i, _, rect, _)| *i == real_idx && rect.contains(pos))
        else {
            return false;
        };
        if response.clicked() && enabled {
            self.apply_list_edit(real_idx, btn);
            ui.ctx().request_repaint();
        }
        true
    }

    /// 評価・訪問回数を1段階動かして保存し、リスト用のキャッシュを最新にする。
    /// 並びは動かさない（ロックし直した時点で反映する）。
    fn apply_list_edit(&mut self, real_idx: usize, btn: EditBtn) {
        let Some(path) = self.archives.get(real_idx).cloned() else { return };
        let Some(db) = self.spread_db.clone() else { return };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return };
        let ok = if btn.is_rating() {
            crate::spread_state::adjust_archive_rating(&db, dir, name, btn.is_up())
        } else {
            crate::spread_state::adjust_archive_visit(&db, dir, name, btn.is_up())
        };
        if ok {
            self.list_edit_dirty = true;
            self.refresh_rating_cache(&path);
        } else {
            self.set_toast(i18n::t().list_edit_failed());
        }
    }

    /// 編集をロックし直したとき（Phase 9）。編集で動かした値を、並び・絞り込みに反映する。
    pub(super) fn finish_list_edit(&mut self) {
        if !std::mem::take(&mut self.list_edit_dirty) {
            return;
        }
        if self.explorer_sort().needs_rating() || self.rating_filter.enabled {
            self.resort_keeping_selection_always();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating(half: u8, visits: u32) -> Option<ArchiveRating> {
        Some(ArchiveRating { rating_half: half, visit_count: visits, last_visit_at: 0 })
    }

    #[test]
    fn buttons_are_disabled_only_at_the_ends() {
        assert!(!EditBtn::RatingDown.enabled(None), "未評価で－は押せない");
        assert!(EditBtn::RatingUp.enabled(None));
        assert!(EditBtn::RatingDown.enabled(rating(1, 0)), "0.5から－で未評価へ");
        assert!(!EditBtn::RatingUp.enabled(rating(RATING_HALF_MAX, 0)), "★5.0で＋は押せない");
        assert!(!EditBtn::VisitDown.enabled(None));
        assert!(EditBtn::VisitUp.enabled(None));
        assert!(!EditBtn::VisitDown.enabled(rating(3, 0)));
        assert!(!EditBtn::VisitUp.enabled(rating(0, VISIT_COUNT_MAX)));
        assert!(!EditBtn::VisitUp.enabled(rating(0, VISIT_COUNT_MAX + 5)), "上限超えの実績も＋は押せない");
        assert!(EditBtn::VisitDown.enabled(rating(0, VISIT_COUNT_MAX + 5)), "－は効く");
    }

    #[test]
    fn edit_cell_has_the_value_above_and_the_buttons_below_inside_the_cell() {
        let cell = egui::Rect::from_min_size(egui::pos2(100.0, 10.0), egui::vec2(80.0, 46.0));
        let r = edit_cell_rects(cell);
        assert!(r.value.max.y <= r.down.min.y && r.value.max.y <= r.up.min.y, "ボタンは値の下");
        assert!(cell.contains_rect(r.down) && cell.contains_rect(r.up), "既定幅(80px)の列に収まる");
        assert!(r.down.max.x <= r.up.min.x, "－は＋の左で、重ならない");
        assert_eq!(r.value.width(), cell.width());
    }
}
