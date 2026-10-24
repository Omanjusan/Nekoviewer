//! 重複マーカーのアイコン描画と当たり判定。カード（上辺中央）とリスト（重複列）で共通に使う。
//!
//! 見た目は図形で描く（フォントの字形に依存しない）。種別は色と形で区別する:
//! 移動 = 青い丸に右向きの矢印、再取得 = 橙の丸に重なった二重四角、
//! 旧ファイル側（他に重複あり）= 再取得と同じ形の半透明。

use crate::duplicate_marker::{DirMarker, MarkerKind};
use crate::i18n;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MarkerIcon {
    Moved,
    Reacquired,
    Elsewhere,
}

impl MarkerIcon {
    /// ホバー時に出す説明。
    pub(super) fn tooltip(self) -> &'static str {
        let t = i18n::t();
        match self {
            Self::Moved => t.dup_marker_moved(),
            Self::Reacquired => t.dup_marker_reacquired(),
            Self::Elsewhere => t.dup_marker_elsewhere(),
        }
    }
}

/// 出すアイコン。自身のマーカーが（消去済みでなければ）優先、無ければ被参照の薄いアイコン。
pub(super) fn icon_for(m: &DirMarker) -> Option<MarkerIcon> {
    match &m.own {
        Some(own) if !own.dismissed => Some(match own.kind {
            MarkerKind::Moved => MarkerIcon::Moved,
            MarkerKind::Reacquired => MarkerIcon::Reacquired,
        }),
        _ if !m.referenced_by.is_empty() => Some(MarkerIcon::Elsewhere),
        _ => None,
    }
}

const ICON_SIZE: f32 = 16.0;
const CARD_TOP_GAP: f32 = 4.0;

/// カード上辺の左右中央に置くアイコンの矩形。
pub(super) fn card_icon_rect(card: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(card.center().x, card.min.y + CARD_TOP_GAP + ICON_SIZE / 2.0),
        egui::vec2(ICON_SIZE, ICON_SIZE),
    )
}

/// リストの重複列（`cell` の中央）に置くアイコンの矩形。
pub(super) fn list_icon_rect(cell: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(cell.center(), egui::vec2(ICON_SIZE, ICON_SIZE))
}

pub(super) fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: MarkerIcon) {
    let (base, alpha) = match icon {
        MarkerIcon::Moved => (egui::Color32::from_rgb(60, 130, 220), 255),
        MarkerIcon::Reacquired => (egui::Color32::from_rgb(235, 140, 35), 255),
        MarkerIcon::Elsewhere => (egui::Color32::from_rgb(235, 140, 35), 120),
    };
    let with_alpha = |c: egui::Color32, a: u8| egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
    let c = rect.center();
    painter.circle_filled(c, ICON_SIZE / 2.0, with_alpha(base, alpha));
    let ink = egui::Stroke::new(1.5, egui::Color32::from_white_alpha(if alpha == 255 { 255 } else { 210 }));
    match icon {
        MarkerIcon::Moved => {
            let (l, r) = (c + egui::vec2(-4.0, 0.0), c + egui::vec2(4.0, 0.0));
            painter.line_segment([l, r], ink);
            painter.line_segment([r, r + egui::vec2(-3.0, -3.0)], ink);
            painter.line_segment([r, r + egui::vec2(-3.0, 3.0)], ink);
        }
        MarkerIcon::Reacquired | MarkerIcon::Elsewhere => {
            let side = egui::vec2(6.0, 6.0);
            let back = egui::Rect::from_min_size(c + egui::vec2(-4.5, -4.5), side);
            let front = egui::Rect::from_min_size(c + egui::vec2(-1.5, -1.5), side);
            painter.rect_stroke(back, 1.0, ink, egui::StrokeKind::Inside);
            painter.rect_filled(front, 1.0, with_alpha(base, alpha));
            painter.rect_stroke(front, 1.0, ink, egui::StrokeKind::Inside);
        }
    }
}

impl super::NekoviewApp {
    /// 重複マーカーを出す表示か。通常のフォルダ表示だけ（お気に入り・検索の横断一覧は対象外）。
    pub(super) fn marker_view_active(&self) -> bool {
        self.folder_pane_tab.shows_real_dir() && self.viewing_favorites.is_none() && self.viewing_search.is_none()
    }

    /// そのファイル名のアイコン。マーカーを出さない表示や、マーカーの無いファイルは None。
    pub(super) fn marker_icon(&self, filename: &str) -> Option<MarkerIcon> {
        if !self.marker_view_active() {
            return None;
        }
        self.dir_markers.get(filename).and_then(icon_for)
    }

    /// 表示領域に入ったファイルの移動マーカーを控える（書くのはフォルダ離脱・終了時）。
    pub(super) fn note_marker_seen(&mut self, filename: &str) {
        if !self.marker_view_active() {
            return;
        }
        let Some(own) = self.dir_markers.get(filename).and_then(|m| m.own.as_ref()) else { return };
        if own.kind == MarkerKind::Moved && !own.dismissed && !own.in_viewport {
            self.marker_seen.insert(own.id);
        }
    }

    /// 控えた「表示領域に入った移動マーカー」の印をDBへ書く。
    pub(super) fn flush_seen_markers(&mut self) {
        if self.marker_seen.is_empty() {
            return;
        }
        let ids: Vec<u64> = self.marker_seen.drain().collect();
        if let Some(db) = &self.spread_db {
            crate::duplicate_marker::mark_in_viewport(db, &ids);
        }
    }

    /// フォルダのスキャン開始時。別のフォルダへ移ったなら、前のフォルダで控えた印を書く
    /// （同じフォルダの再スキャンは訪問が続いているので書かない）。
    pub(super) fn on_marker_dir_scan(&mut self) {
        if self.marker_seen_dir.as_deref() != Some(self.current_dir.as_path()) {
            self.flush_seen_markers();
            self.marker_seen_dir = Some(self.current_dir.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duplicate_marker::MarkerRecord;

    fn own(kind: MarkerKind, dismissed: bool) -> MarkerRecord {
        MarkerRecord {
            id: 1,
            kind,
            targets: vec![],
            in_viewport: false,
            dismissed,
            mtime_match: false,
            detected_at: 0,
        }
    }

    #[test]
    fn own_marker_decides_the_icon_by_kind() {
        let moved = DirMarker { own: Some(own(MarkerKind::Moved, false)), referenced_by: vec![] };
        let re = DirMarker { own: Some(own(MarkerKind::Reacquired, false)), referenced_by: vec![] };
        assert_eq!(icon_for(&moved), Some(MarkerIcon::Moved));
        assert_eq!(icon_for(&re), Some(MarkerIcon::Reacquired));
    }

    #[test]
    fn own_marker_wins_over_being_referenced() {
        let both = DirMarker { own: Some(own(MarkerKind::Reacquired, false)), referenced_by: vec![9] };
        assert_eq!(icon_for(&both), Some(MarkerIcon::Reacquired));
    }

    #[test]
    fn referenced_only_is_the_faint_icon() {
        let old = DirMarker { own: None, referenced_by: vec![9] };
        assert_eq!(icon_for(&old), Some(MarkerIcon::Elsewhere));
    }

    #[test]
    fn dismissed_own_marker_shows_nothing_unless_referenced() {
        let gone = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![] };
        assert_eq!(icon_for(&gone), None);
        let gone_but_old = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![9] };
        assert_eq!(icon_for(&gone_but_old), Some(MarkerIcon::Elsewhere));
    }

    #[test]
    fn card_icon_sits_at_top_center() {
        let card = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 80.0));
        let r = card_icon_rect(card);
        assert_eq!(r.center().x, card.center().x);
        assert!(r.min.y > card.min.y && r.max.y < card.center().y);
    }
}
