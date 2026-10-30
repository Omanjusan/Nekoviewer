//! 重複マーカーのアイコン描画と当たり判定。カード（上辺中央）とリスト（重複列）で共通に使う。
//!
//! 見た目は図形で描く（フォントの字形に依存しない）。種別は色と形で区別する:
//! 移動 = 青い丸に右向きの矢印、再取得 = 橙の丸に重なった二重四角、
//! 旧ファイル側（他に重複あり）= 再取得と同じ形の半透明。

use std::collections::HashMap;

use crate::duplicate_marker::{DirMarker, MarkerKind};
use crate::file_identity::Presence;
use crate::i18n;


/// 確認済みの重複先ID -> 存在確認の結果。
pub(super) type PresenceMap = HashMap<u64, Presence>;

/// `ids` が空でなく、全てが「消失」と確認できている。未確認・オフライン・現存が1つでもあれば false
/// （消えたと断定できるのは、全員が確かに消えているときだけ）。
pub(super) fn all_gone(ids: &[u64], presence: &PresenceMap) -> bool {
    !ids.is_empty() && ids.iter().all(|id| presence.get(id) == Some(&Presence::Missing))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MarkerIcon {
    Moved,
    Reacquired,
    Elsewhere,
    /// 「正常として消した」再取得（「消去済みも」を入れた時だけ出す。グレー）。
    Dismissed,
}

impl MarkerIcon {
    /// ホバー時に出す説明。
    pub(super) fn tooltip(self) -> &'static str {
        let t = i18n::t();
        match self {
            Self::Moved => t.dup_marker_moved(),
            Self::Reacquired => t.dup_marker_reacquired(),
            Self::Elsewhere => t.dup_marker_elsewhere(),
            Self::Dismissed => t.dup_marker_dismissed(),
        }
    }
}

/// 絞り込み・並びの対象になる自身のマーカーの検出日時。マーカー無し、旧ファイル側だけ（被参照）、
/// 消去済み（`show_dismissed` でない時）は None。
/// 再取得で、重複先が全て消失と確認できているものも対象外。
pub(super) fn active_detected_at(m: &DirMarker, show_dismissed: bool, presence: &PresenceMap) -> Option<i64> {
    let own = m.own.as_ref()?;
    if own.kind == MarkerKind::Reacquired && all_gone(&own.targets, presence) {
        return None;
    }
    (!own.dismissed || show_dismissed).then_some(own.detected_at)
}

/// 出すアイコン。自身のマーカーが（消去済みでなければ）優先、無ければ被参照の薄いアイコン。
/// 消去済みは、`show_dismissed` のときだけグレーで出す。
/// 再取得の重複先が全て消失、旧ファイル側の参照元が全て消失と確認できたものは出さない。
pub(super) fn icon_for(m: &DirMarker, show_dismissed: bool, presence: &PresenceMap) -> Option<MarkerIcon> {
    let own_icon = m.own.as_ref().and_then(|own| {
        if own.kind == MarkerKind::Reacquired && all_gone(&own.targets, presence) {
            return None;
        }
        if !own.dismissed {
            Some(match own.kind {
                MarkerKind::Moved => MarkerIcon::Moved,
                MarkerKind::Reacquired => MarkerIcon::Reacquired,
            })
        } else if show_dismissed {
            Some(MarkerIcon::Dismissed)
        } else {
            None
        }
    });
    own_icon.or_else(|| {
        (!m.referenced_by.is_empty() && !all_gone(&m.referenced_by, presence)).then_some(MarkerIcon::Elsewhere)
    })
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

/// 重複列の左スロット（重複マーカー）の矩形。右スロットは「？」（似ている画像）が使う。
pub(super) fn list_marker_rect(cell: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(cell.min.x + SLOT_PAD + ICON_SIZE / 2.0, cell.center().y),
        egui::vec2(ICON_SIZE, ICON_SIZE),
    )
}

/// リストの重複列の右スロット（「？」）の矩形。
pub(super) fn list_similar_rect(cell: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(cell.max.x - SLOT_PAD - ICON_SIZE / 2.0, cell.center().y),
        egui::vec2(ICON_SIZE, ICON_SIZE),
    )
}

/// カードの「？」の矩形。重複マーカーのすぐ右に並べる。
pub(super) fn card_similar_rect(card: egui::Rect) -> egui::Rect {
    card_icon_rect(card).translate(egui::vec2(ICON_SIZE + SLOT_PAD, 0.0))
}

/// 重複列の左右スロットの外側の余白と、カードでのアイコン間の隙間。
const SLOT_PAD: f32 = 4.0;

/// しきい値の既定値(%)と、コンボボックスの選択肢（5%刻み）。
pub(super) const SIM_DEFAULT_PCT: u8 = 90;
pub(super) const SIM_PCT_CHOICES: [u8; 7] = [70, 75, 80, 85, 90, 95, 100];

/// 「？」（似ている画像）のアイコン。紫の丸に白い「?」。
pub(super) fn paint_similar_icon(painter: &egui::Painter, rect: egui::Rect) {
    painter.circle_filled(rect.center(), ICON_SIZE / 2.0, egui::Color32::from_rgb(140, 95, 215));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "?",
        egui::FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
}

pub(super) fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: MarkerIcon) {
    let (base, alpha) = match icon {
        MarkerIcon::Moved => (egui::Color32::from_rgb(60, 130, 220), 255),
        MarkerIcon::Reacquired => (egui::Color32::from_rgb(235, 140, 35), 255),
        MarkerIcon::Elsewhere => (egui::Color32::from_rgb(235, 140, 35), 120),
        MarkerIcon::Dismissed => (egui::Color32::from_rgb(140, 140, 140), 255),
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
        MarkerIcon::Reacquired | MarkerIcon::Elsewhere | MarkerIcon::Dismissed => {
            let side = egui::vec2(6.0, 6.0);
            let back = egui::Rect::from_min_size(c + egui::vec2(-4.5, -4.5), side);
            let front = egui::Rect::from_min_size(c + egui::vec2(-1.5, -1.5), side);
            painter.rect_stroke(back, 1.0, ink, egui::StrokeKind::Inside);
            painter.rect_filled(front, 1.0, with_alpha(base, alpha));
            painter.rect_stroke(front, 1.0, ink, egui::StrokeKind::Inside);
        }
    }
}

/// 開いているポップアップ。移動マーカーの説明だけを出す（ほかの種類のアイコンは、押すと類似ファイル一覧を直接開く）。
pub(super) struct MarkerPopup {
    icon: MarkerIcon,
    anchor: egui::Pos2,
}

impl super::NekoviewApp {
    /// 重複マーカーを出す表示か。通常のフォルダ表示だけ（お気に入り・検索の横断一覧は対象外）。
    pub(super) fn marker_view_active(&self) -> bool {
        self.folder_pane_tab.shows_real_dir() && self.viewing_favorites.is_none()
            && self.viewing_search.is_none()
            && self.viewing_similar.is_none()
    }

    /// 絞り込み・並びの対象になる自身のマーカーの検出日時（unix秒）。マーカー無し、消去済み
    /// （「消去済みも」を入れていない時）、マーカーを出さない表示は None。
    pub(super) fn active_marker_detected_at(&self, filename: &str) -> Option<i64> {
        if !self.marker_view_active() {
            return None;
        }
        active_detected_at(self.dir_markers.get(filename)?, self.dup_show_dismissed, &self.marker_presence)
    }

    /// そのファイル名のアイコン。マーカーを出さない表示や、マーカーの無いファイルは None。
    pub(super) fn marker_icon(&self, filename: &str) -> Option<MarkerIcon> {
        if !self.marker_view_active() {
            return None;
        }
        self.dir_markers.get(filename).and_then(|m| icon_for(m, self.dup_show_dismissed, &self.marker_presence))
    }

    /// アイコンを描き、ホバーで説明を出す。見えているファイルの移動マーカーは「見た」印を控え、
    /// クリックの振り分けのため、描いた矩形を覚える。
    pub(super) fn paint_marker_icon(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        icon_rect: egui::Rect,
        real_idx: usize,
        name: &str,
    ) {
        self.note_marker_seen(name);
        let Some(icon) = self.marker_icon(name) else {
            self.marker_icon_hits.remove(&real_idx);
            return;
        };
        paint_icon(&ui.painter().with_clip_rect(ui.clip_rect()), icon_rect, icon);
        self.marker_icon_hits.insert(real_idx, icon_rect);
        let on_icon = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| icon_rect.contains(p));
        if on_icon && self.marker_popup.is_none() {
            let tip = if icon == MarkerIcon::Moved {
                icon.tooltip().to_owned()
            } else {
                format!("{}\n{}", icon.tooltip(), i18n::t().dup_marker_click_hint())
            };
            response.clone().on_hover_text(tip);
        }
    }

    /// そのファイル名に「？」を出すなら、いちばん近い相手との類似度(%)。
    /// マーカーを出さない表示や、しきい値に届かないものは None。照合結果は距離順なので先頭が最も近い。
    pub(super) fn sim_pct(&self, filename: &str) -> Option<u8> {
        if !self.marker_view_active() {
            return None;
        }
        let pct = self.sim_matches.get(filename)?.first()?.pct();
        (pct >= self.sim_threshold_pct).then_some(pct)
    }

    /// 「？」を描き、ホバーで説明を出す。クリックの振り分けのため、描いた矩形を覚える。
    pub(super) fn paint_similar_mock(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        icon_rect: egui::Rect,
        real_idx: usize,
        name: &str,
    ) {
        let Some(pct) = self.sim_pct(name) else {
            self.sim_icon_hits.remove(&real_idx);
            return;
        };
        paint_similar_icon(&ui.painter().with_clip_rect(ui.clip_rect()), icon_rect);
        self.sim_icon_hits.insert(real_idx, (icon_rect, pct));
        let on_icon = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| icon_rect.contains(p));
        if on_icon && self.sim_popup.is_none() && self.marker_popup.is_none() {
            response.clone().on_hover_text(i18n::t().sim_marker_tooltip(pct));
        }
    }

    /// 【モック】「？」の説明ポップアップ。外側クリックかEscで閉じる。
    pub(super) fn draw_sim_popup(&mut self, ctx: &egui::Context) {
        let Some((anchor, pct)) = self.sim_popup else { return };
        let t = i18n::t();
        let area = egui::Area::new(egui::Id::new("sim_mock_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(380.0);
                    ui.label(egui::RichText::new(t.sim_marker_tooltip(pct)).strong());
                    ui.label(t.sim_mock_popup_note());
                });
            });
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p));
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if pressed_outside || escape {
            self.sim_popup = None;
        }
    }

    /// アイコンのクリックなら、ポップアップを開いて true（選択や起動はしない）。
    /// ダブルクリックの2回目も、起動させないよう握りつぶす。
    pub(super) fn try_open_marker_popup(&mut self, ui: &egui::Ui, response: &egui::Response, real_idx: usize) -> bool {
        // 類似ファイル一覧では、この矩形は基準の目印（マーカーのアイコンは出さない）。
        if self.viewing_similar.is_some() {
            return self.try_click_similar_base(response, real_idx);
        }
        if !(response.clicked() || response.double_clicked()) {
            return false;
        }
        // 【モック】「？」のクリック。
        if let Some((rect, pct)) = self.sim_icon_hits.get(&real_idx).copied()
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p))
        {
            if response.clicked() {
                self.sim_popup = Some((egui::pos2(rect.min.x, rect.max.y + 4.0), pct));
            }
            return true;
        }
        let Some(rect) = self.marker_icon_hits.get(&real_idx).copied() else { return false };
        let on_icon = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p));
        if !on_icon {
            return false;
        }
        if response.double_clicked() {
            return true;
        }
        let Some(path) = self.archives.get(real_idx).cloned() else { return false };
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
        let Some(icon) = self.marker_icon(name) else { return false };
        // 移動マーカーは旧ファイルが無く、一覧にならない。説明のポップアップだけを出す。
        if icon == MarkerIcon::Moved {
            self.marker_popup = Some(MarkerPopup { icon, anchor: egui::pos2(rect.min.x, rect.max.y + 4.0) });
            return true;
        }
        // それ以外は、同じ内容のファイルだけを並べた類似ファイル一覧を直接開く。
        let entry_id = self
            .spread_db
            .as_ref()
            .zip(path.parent())
            .and_then(|(db, dir)| crate::file_identity::lookup(db, &crate::spread_state::make_key(dir, name)))
            .map(|rec| rec.id);
        if let Some(id) = entry_id {
            self.enter_similar_view(path, id);
        }
        true
    }

    /// 開いているポップアップを描く。外側の押下かEscで閉じる。
    pub(super) fn draw_marker_popup(&mut self, ctx: &egui::Context) {
        let Some(popup) = &self.marker_popup else { return };
        let t = i18n::t();
        let area = egui::Area::new(egui::Id::new("dup_marker_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(popup.anchor)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(380.0);
                    ui.label(egui::RichText::new(popup.icon.tooltip()).strong());
                    ui.label(t.dup_popup_quick_match());
                });
            });
        let pressed_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p));
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if pressed_outside || escape {
            self.marker_popup = None;
        }
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
        self.marker_popup = None;
        self.marker_icon_hits.clear();
        self.sim_popup = None;
        self.sim_icon_hits.clear();
        self.sim_matches.clear();
        // 重複先の存在確認は、フォルダを読み込むたびにやり直す（確認が済むまでは、いつもどおり表示する）。
        self.marker_probe.invalidate();
        self.thumb_hash.invalidate();
        self.marker_presence.clear();
        self.marker_probe_requested.clear();
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
        assert_eq!(icon_for(&moved, false, &PresenceMap::new()), Some(MarkerIcon::Moved));
        assert_eq!(icon_for(&re, false, &PresenceMap::new()), Some(MarkerIcon::Reacquired));
    }

    #[test]
    fn own_marker_wins_over_being_referenced() {
        let both = DirMarker { own: Some(own(MarkerKind::Reacquired, false)), referenced_by: vec![9] };
        assert_eq!(icon_for(&both, false, &PresenceMap::new()), Some(MarkerIcon::Reacquired));
    }

    #[test]
    fn referenced_only_is_the_faint_icon() {
        let old = DirMarker { own: None, referenced_by: vec![9] };
        assert_eq!(icon_for(&old, false, &PresenceMap::new()), Some(MarkerIcon::Elsewhere));
    }

    #[test]
    fn dismissed_own_marker_shows_nothing_unless_referenced() {
        let gone = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![] };
        assert_eq!(icon_for(&gone, false, &PresenceMap::new()), None);
        let gone_but_old = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![9] };
        assert_eq!(icon_for(&gone_but_old, false, &PresenceMap::new()), Some(MarkerIcon::Elsewhere));
    }

    #[test]
    fn dismissed_own_marker_shows_grey_only_when_asked() {
        let gone = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![] };
        assert_eq!(icon_for(&gone, true, &PresenceMap::new()), Some(MarkerIcon::Dismissed));
        // 消去済みでも旧ファイル側の薄いアイコンは別物（自身の消去済み表示を優先する）。
        let gone_but_old = DirMarker { own: Some(own(MarkerKind::Reacquired, true)), referenced_by: vec![9] };
        assert_eq!(icon_for(&gone_but_old, true, &PresenceMap::new()), Some(MarkerIcon::Dismissed));
    }

    #[test]
    fn active_detected_at_follows_own_marker_and_dismissal() {
        let mut live = own(MarkerKind::Moved, false);
        live.detected_at = 42;
        let m = DirMarker { own: Some(live), referenced_by: vec![] };
        assert_eq!(active_detected_at(&m, false, &PresenceMap::new()), Some(42));
        let mut gone = own(MarkerKind::Reacquired, true);
        gone.detected_at = 7;
        let d = DirMarker { own: Some(gone), referenced_by: vec![] };
        assert_eq!(active_detected_at(&d, false, &PresenceMap::new()), None, "消去済みは対象外");
        assert_eq!(active_detected_at(&d, true, &PresenceMap::new()), Some(7), "「消去済みも」を入れると対象");
        let old = DirMarker { own: None, referenced_by: vec![9] };
        assert_eq!(active_detected_at(&old, true, &PresenceMap::new()), None, "旧ファイル側は絞り込み・並びの対象外");
    }

    fn presence(pairs: &[(u64, Presence)]) -> PresenceMap {
        pairs.iter().copied().collect()
    }

    #[test]
    fn all_gone_needs_every_target_confirmed_missing() {
        assert!(!all_gone(&[], &presence(&[])), "空は消えたと言えない");
        assert!(!all_gone(&[1], &presence(&[])), "未確認は消えたと断定しない");
        assert!(!all_gone(&[1], &presence(&[(1, Presence::Offline)])), "オフラインは断定しない");
        assert!(!all_gone(&[1, 2], &presence(&[(1, Presence::Missing), (2, Presence::Present)])));
        assert!(!all_gone(&[1, 2], &presence(&[(1, Presence::Missing)])), "一部が未確認");
        assert!(all_gone(&[1, 2], &presence(&[(1, Presence::Missing), (2, Presence::Missing)])));
    }

    #[test]
    fn reacquired_icon_hides_only_when_all_targets_are_gone() {
        let mut rec = own(MarkerKind::Reacquired, false);
        rec.targets = vec![1, 2];
        let m = DirMarker { own: Some(rec), referenced_by: vec![] };
        let part = presence(&[(1, Presence::Missing), (2, Presence::Present)]);
        assert_eq!(icon_for(&m, false, &part), Some(MarkerIcon::Reacquired));
        let all = presence(&[(1, Presence::Missing), (2, Presence::Missing)]);
        assert_eq!(icon_for(&m, false, &all), None);
        assert_eq!(icon_for(&m, true, &all), None, "消去済みも表示でも、重複先が無いなら出さない");
        assert_eq!(active_detected_at(&m, false, &all), None, "絞り込み・並びからも外れる");
        assert_eq!(active_detected_at(&m, false, &part), Some(0));
    }

    #[test]
    fn moved_icon_ignores_presence() {
        let m = DirMarker { own: Some(own(MarkerKind::Moved, false)), referenced_by: vec![] };
        let gone = presence(&[(1, Presence::Missing)]);
        assert_eq!(icon_for(&m, false, &gone), Some(MarkerIcon::Moved));
        assert_eq!(active_detected_at(&m, false, &gone), Some(0));
    }

    #[test]
    fn elsewhere_icon_hides_when_every_referrer_is_gone() {
        let m = DirMarker { own: None, referenced_by: vec![8, 9] };
        let one = presence(&[(8, Presence::Missing), (9, Presence::Offline)]);
        assert_eq!(icon_for(&m, false, &one), Some(MarkerIcon::Elsewhere));
        let all = presence(&[(8, Presence::Missing), (9, Presence::Missing)]);
        assert_eq!(icon_for(&m, false, &all), None);
    }

    #[test]
    fn hidden_own_marker_falls_back_to_the_referenced_icon() {
        let mut rec = own(MarkerKind::Reacquired, false);
        rec.targets = vec![1];
        let m = DirMarker { own: Some(rec), referenced_by: vec![9] };
        let gone = presence(&[(1, Presence::Missing)]);
        assert_eq!(icon_for(&m, false, &gone), Some(MarkerIcon::Elsewhere));
    }

    #[test]
    fn card_icon_sits_at_top_center() {
        let card = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 80.0));
        let r = card_icon_rect(card);
        assert_eq!(r.center().x, card.center().x);
        assert!(r.min.y > card.min.y && r.max.y < card.center().y);
    }
}
