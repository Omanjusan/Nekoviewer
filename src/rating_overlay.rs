//! アーカイブ評価（半星刻み・★0.5〜★5）のUI部品。
//!
//! 値は「半星単位の整数」（0 = 未評価 / 1..=10 = ★0.5〜★5.0）で扱う。
//! - `paint_star`: 1個の星（空 / 半分 / 全部）の描画。サムネ帯でも共用する
//! - `half_from_x`: 星の並びの上のクリックX座標 → 半星値（純粋関数）
//! - `overlay_visible` / `band_rect`: ビューア最終ページの評価オーバーレイの表示条件と配置
//! - `show`: オーバーレイの描画とクリック検出。DB等には一切触れず、イベントを返すだけ

use egui::{Color32, Pos2, Rect, Stroke};

/// 半星単位の最大値（★5.0）
pub const MAX_HALF: u8 = 10;
/// 星の並びの個数
const STAR_COUNT: usize = 5;

/// 塗りつぶし色（黄）
const STAR_FILL: Color32 = Color32::from_rgb(255, 208, 0);
/// 星の輪郭（白）
const STAR_OUTLINE: Color32 = Color32::WHITE;
/// 輪郭線の太さ
const STAR_OUTLINE_WIDTH: f32 = 1.5;
/// 星の内側頂点の半径比（外側頂点に対する比。五芒星の正則比）
const STAR_INNER_RATIO: f32 = 0.382;
/// 星の幅（外接）/ 外半径。2 * cos(18°)
const STAR_WIDTH_PER_RADIUS: f32 = 1.902;
/// 星の間隔 / 外半径
const STAR_GAP_PER_RADIUS: f32 = 0.5;

/// オーバーレイ帯の背景（薄いグレー。画像が透けて見える半透明）
// premultiplied 指定（const のため）。非乗算に直すと グレー125・アルファ114（約45%）
const BAND_BG: Color32 = Color32::from_rgba_premultiplied(56, 56, 56, 114);
/// オーバーレイ帯の最大サイズ（px）
const BAND_MAX_W: f32 = 420.0;
const BAND_MAX_H: f32 = 140.0;
/// 帯のビューア寸法に対する上限比
const BAND_VIEWPORT_RATIO: f32 = 0.9;
/// 星の外半径の上限（px）
const STAR_MAX_RADIUS: f32 = 22.0;
/// 帯の左右の余白（星の並びを縮める際に確保する）
const BAND_SIDE_PAD: f32 = 16.0;
/// Xボタンの一辺と帯の隅からの余白
const CLOSE_SIZE: f32 = 22.0;
const CLOSE_MARGIN: f32 = 8.0;
/// 「未評価にする」ボタンのサイズと帯下端からの余白
const UNSET_W: f32 = 150.0;
const UNSET_H: f32 = 26.0;
const UNSET_BOTTOM_MARGIN: f32 = 12.0;

/// オーバーレイが返す操作
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingEvent {
    None,
    /// ★をクリックした（半星値 1..=10）
    Set(u8),
    /// 「未評価にする」を押した
    Unset,
    /// Xで閉じた
    Close,
}

/// 評価オーバーレイを出すか。生画像ファイルは対象外、Xで閉じた後は出さない。
pub fn overlay_visible(is_raw_file: bool, dismissed: bool, at_last_page: bool, total: usize) -> bool {
    !is_raw_file && !dismissed && at_last_page && total > 0
}

/// 帯の矩形（ビューア中央）。ビューアが小さいときは寸法を縮める。
pub fn band_rect(viewport: Rect) -> Rect {
    let w = BAND_MAX_W.min(viewport.width() * BAND_VIEWPORT_RATIO);
    let h = BAND_MAX_H.min(viewport.height() * BAND_VIEWPORT_RATIO);
    Rect::from_center_size(viewport.center(), egui::vec2(w.max(0.0), h.max(0.0)))
}

/// 星の並びの配置
#[derive(Clone, Copy, Debug, PartialEq)]
struct StarRow {
    /// 並び全体の中心
    center: Pos2,
    /// 星の外半径
    radius: f32,
}

impl StarRow {
    fn star_w(&self) -> f32 {
        self.radius * STAR_WIDTH_PER_RADIUS
    }

    /// 隣り合う星の中心間距離
    fn pitch(&self) -> f32 {
        self.radius * (STAR_WIDTH_PER_RADIUS + STAR_GAP_PER_RADIUS)
    }

    fn width(&self) -> f32 {
        self.pitch() * (STAR_COUNT as f32 - 1.0) + self.star_w()
    }

    fn left(&self) -> f32 {
        self.center.x - self.width() / 2.0
    }

    /// クリック判定の矩形
    fn rect(&self) -> Rect {
        Rect::from_center_size(self.center, egui::vec2(self.width(), self.radius * 2.0))
    }

    /// i番目（0始まり）の星の中心
    fn star_center(&self, i: usize) -> Pos2 {
        Pos2::new(self.left() + self.star_w() / 2.0 + self.pitch() * i as f32, self.center.y)
    }
}

/// 帯に収まる星の並び。星の外半径は上限を持ち、帯が狭ければ縮める。
fn star_row_in(band: Rect) -> StarRow {
    let row_w_per_radius =
        (STAR_WIDTH_PER_RADIUS + STAR_GAP_PER_RADIUS) * (STAR_COUNT as f32 - 1.0) + STAR_WIDTH_PER_RADIUS;
    let radius = STAR_MAX_RADIUS.min(((band.width() - BAND_SIDE_PAD * 2.0) / row_w_per_radius).max(1.0));
    // 帯の上寄り（下に「未評価にする」ボタンの分を空ける）
    let y = band.min.y + band.height() * 0.4;
    StarRow { center: Pos2::new(band.center().x, y), radius }
}

/// 星の並び上のクリックX座標 → 半星値（1..=10）。
/// 星の左半分 = ★x.5、右半分 = ★x.0。星と星の間・並びの外は近い側の星に丸める。
pub fn half_from_x(x: f32, row_left: f32, pitch: f32, star_w: f32) -> u8 {
    if pitch <= 0.0 || star_w <= 0.0 {
        return 1;
    }
    let rel = x - row_left;
    let idx = (rel / pitch).floor().clamp(0.0, (STAR_COUNT - 1) as f32);
    let local = (rel - idx * pitch).clamp(0.0, star_w);
    let half_in_star = if local < star_w / 2.0 { 1 } else { 2 };
    (idx as u8 * 2 + half_in_star).clamp(1, MAX_HALF)
}

/// 半星値と星番号（0始まり）から、その星の塗り量（0=空 / 1=半分 / 2=全部）
pub fn fill_of_star(value_half: u8, star_index: usize) -> u8 {
    (value_half as i32 - star_index as i32 * 2).clamp(0, 2) as u8
}

/// 星の頂点（外・内を交互に10点。真上から時計回り）
fn star_points(center: Pos2, radius: f32) -> [Pos2; 10] {
    let mut pts = [center; 10];
    for (i, p) in pts.iter_mut().enumerate() {
        let r = if i % 2 == 0 { radius } else { radius * STAR_INNER_RATIO };
        let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        *p = Pos2::new(center.x + r * ang.cos(), center.y + r * ang.sin());
    }
    pts
}

/// 星1個を描く。`fill`: 0=空（白枠のみ）/ 1=左半分だけ黄 / 2=全部黄。
/// 凹図形なので、塗りは中心からの三角形ファンで作る。
pub fn paint_star(painter: &egui::Painter, center: Pos2, radius: f32, fill: u8) {
    let pts = star_points(center, radius);
    if fill > 0 {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(center, STAR_FILL);
        for p in pts {
            mesh.colored_vertex(p, STAR_FILL);
        }
        for i in 0..10u32 {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
        }
        if fill == 1 {
            // 左半分だけ見せる
            let left_half = Rect::from_min_max(
                Pos2::new(center.x - radius - 1.0, center.y - radius - 1.0),
                Pos2::new(center.x, center.y + radius + 1.0),
            );
            painter
                .with_clip_rect(painter.clip_rect().intersect(left_half))
                .add(egui::Shape::mesh(mesh));
        } else {
            painter.add(egui::Shape::mesh(mesh));
        }
    }
    painter.add(egui::Shape::closed_line(
        pts.to_vec(),
        Stroke::new(STAR_OUTLINE_WIDTH, STAR_OUTLINE),
    ));
}

/// 評価オーバーレイの描画とクリック検出（見た目確認用MOCK。値の保存は呼び出し側）。
/// 優先順位は X > 未評価ボタン > ★。
pub fn show(ui: &mut egui::Ui, band: Rect, value_half: u8, unset_label: &str) -> RatingEvent {
    let painter = ui.painter().clone();
    let base_id = ui.id().with("rating_overlay");

    painter.rect_filled(band, 8.0, BAND_BG);
    // 帯の上のクリックを背面へ通さない（他の要素より先に登録し、下敷きにする）
    ui.interact(band, base_id.with("bg"), egui::Sense::click());

    let mut event = RatingEvent::None;

    // ── ★の並び ──────────────────────────────────────────
    let row = star_row_in(band);
    for i in 0..STAR_COUNT {
        paint_star(&painter, row.star_center(i), row.radius, fill_of_star(value_half, i));
    }
    let row_resp = ui
        .interact(row.rect(), base_id.with("stars"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if row_resp.clicked()
        && let Some(pos) = row_resp.interact_pointer_pos()
    {
        event = RatingEvent::Set(half_from_x(pos.x, row.left(), row.pitch(), row.star_w()));
    }

    // ── 未評価にするボタン（★の下側中央）────────────────────
    let unset_rect = Rect::from_center_size(
        Pos2::new(band.center().x, band.max.y - UNSET_BOTTOM_MARGIN - UNSET_H / 2.0),
        egui::vec2(UNSET_W.min(band.width() - BAND_SIDE_PAD * 2.0).max(1.0), UNSET_H),
    );
    let unset_resp = ui
        .interact(unset_rect, base_id.with("unset"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let unset_fill = if unset_resp.hovered() {
        Color32::from_rgb(92, 92, 92)
    } else {
        Color32::from_rgb(76, 76, 76)
    };
    painter.rect_filled(unset_rect, 4.0, unset_fill);
    painter.text(
        unset_rect.center(),
        egui::Align2::CENTER_CENTER,
        unset_label,
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
    if unset_resp.clicked() {
        event = RatingEvent::Unset;
    }

    // ── 右上のXボタン（グリフに頼らず線で描く）─────────────────
    let close_rect = Rect::from_min_size(
        Pos2::new(band.max.x - CLOSE_MARGIN - CLOSE_SIZE, band.min.y + CLOSE_MARGIN),
        egui::vec2(CLOSE_SIZE, CLOSE_SIZE),
    );
    let close_resp = ui
        .interact(close_rect, base_id.with("close"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if close_resp.hovered() {
        painter.rect_filled(close_rect, 4.0, Color32::from_rgb(92, 92, 92));
    }
    let c = close_rect.center();
    let d = CLOSE_SIZE * 0.27;
    let stroke = Stroke::new(2.0, Color32::WHITE);
    painter.line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
    painter.line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
    if close_resp.clicked() {
        event = RatingEvent::Close;
    }

    event
}

#[cfg(test)]
mod tests {
    use super::*;

    // 星幅40・間隔10（pitch50）・並び左端100 の並びを想定
    const LEFT: f32 = 100.0;
    const PITCH: f32 = 50.0;
    const W: f32 = 40.0;

    fn h(x: f32) -> u8 {
        half_from_x(x, LEFT, PITCH, W)
    }

    #[test]
    fn click_on_left_half_gives_half_star_and_right_half_gives_whole() {
        assert_eq!(h(LEFT + 5.0), 1); // 1番星の左半分 = ★0.5
        assert_eq!(h(LEFT + 30.0), 2); // 1番星の右半分 = ★1.0
        assert_eq!(h(LEFT + PITCH + 5.0), 3); // 2番星の左半分 = ★1.5
        assert_eq!(h(LEFT + PITCH * 2.0 + 30.0), 6); // 3番星の右半分 = ★3.0
        assert_eq!(h(LEFT + PITCH * 4.0 + 30.0), 10); // 5番星の右半分 = ★5.0
    }

    #[test]
    fn half_boundary_belongs_to_the_right_half() {
        assert_eq!(h(LEFT + W / 2.0 - 0.01), 1);
        assert_eq!(h(LEFT + W / 2.0), 2);
    }

    #[test]
    fn gap_between_stars_rounds_to_the_left_star_whole() {
        // 1番星と2番星の間（星幅40〜pitch50）は1番星の右端扱い
        assert_eq!(h(LEFT + 45.0), 2);
    }

    #[test]
    fn out_of_row_clicks_clamp_to_the_ends() {
        assert_eq!(h(LEFT - 100.0), 1);
        assert_eq!(h(LEFT + PITCH * 10.0), 10);
    }

    #[test]
    fn degenerate_geometry_falls_back_to_the_minimum() {
        assert_eq!(half_from_x(0.0, 0.0, 0.0, 10.0), 1);
        assert_eq!(half_from_x(0.0, 0.0, 10.0, 0.0), 1);
    }

    #[test]
    fn fill_of_star_fills_up_to_the_value_and_leaves_the_rest_empty() {
        let fills: Vec<u8> = (0..5).map(|i| fill_of_star(7, i)).collect(); // ★3.5
        assert_eq!(fills, vec![2, 2, 2, 1, 0]);
        let none: Vec<u8> = (0..5).map(|i| fill_of_star(0, i)).collect();
        assert_eq!(none, vec![0, 0, 0, 0, 0]);
        let full: Vec<u8> = (0..5).map(|i| fill_of_star(10, i)).collect();
        assert_eq!(full, vec![2, 2, 2, 2, 2]);
    }

    #[test]
    fn overlay_shows_only_on_the_last_page_of_a_non_raw_undismissed_archive() {
        assert!(overlay_visible(false, false, true, 10));
        assert!(!overlay_visible(true, false, true, 10)); // 生画像は対象外
        assert!(!overlay_visible(false, true, true, 10)); // Xで閉じた後
        assert!(!overlay_visible(false, false, false, 10)); // 途中ページ
        assert!(!overlay_visible(false, false, true, 0)); // 空
    }

    #[test]
    fn star_row_hit_test_is_consistent_with_its_own_geometry() {
        let band = band_rect(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 800.0)));
        let row = star_row_in(band);
        // 1番星の中心の少し左 = ★0.5、5番星の中心の少し右 = ★5.0
        let first = row.star_center(0);
        let last = row.star_center(4);
        assert_eq!(half_from_x(first.x - 1.0, row.left(), row.pitch(), row.star_w()), 1);
        assert_eq!(half_from_x(last.x + 1.0, row.left(), row.pitch(), row.star_w()), 10);
        // 並びは帯の内側に収まる
        assert!(row.rect().min.x >= band.min.x && row.rect().max.x <= band.max.x);
    }

    #[test]
    fn band_shrinks_with_small_viewports() {
        let small = band_rect(Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0)));
        assert!(small.width() <= 200.0 * BAND_VIEWPORT_RATIO + 0.01);
        assert!(small.height() <= 100.0 * BAND_VIEWPORT_RATIO + 0.01);
        let row = star_row_in(small);
        assert!(row.rect().min.x >= small.min.x && row.rect().max.x <= small.max.x);
    }
}
