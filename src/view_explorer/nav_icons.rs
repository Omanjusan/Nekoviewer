//! サムネグリッドの「↑（親フォルダへ）」「フォルダ」アイコンをベクター描画する。
//! 画像リソースは使わず、100x100の設計座標系をアイコン用矩形へ均一スケールして描く。

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2};

/// 両アイコン共通のアクセント色（ライト/ダーク双方で視認性を確認済み）。
pub const NAV_ICON_COLOR: Color32 = Color32::from_rgb(70, 140, 235);

const DESIGN_CENTER: Pos2 = pos2(50.0, 50.0);

/// 設計座標(0..100)を rect 内へ均一スケールで写像する変換。
/// rect.center() が設計座標の中心(50,50)に一致するため、アイコン中心と
/// 矩形中心は常に一致する。
fn transform(rect: Rect, scale: f32) -> impl Fn(f32, f32) -> Pos2 {
    let center = rect.center();
    move |x: f32, y: f32| center + (pos2(x, y) - DESIGN_CENTER) * scale
}

/// rect 幅の 78% をアイコン幅として使う均一スケール係数。
fn icon_scale(rect: Rect) -> f32 {
    rect.width() * 0.78 / 100.0
}

/// 「↑（親フォルダへ）」アイコン。ENTERキーのシジル(⏎)を右へ90°回転させた
/// 形状＝L字の横棒・縦棒＋先端の上向き矢印。ラベルは描画しない（矢印のみ）。
pub fn draw_up_icon(painter: &Painter, rect: Rect, color: Color32) {
    let scale = icon_scale(rect);
    let tf = transform(rect, scale);
    let stroke_w = 9.0 * scale;
    let stroke = Stroke::new(stroke_w, color);

    let corner = tf(40.0, 74.0);
    painter.line_segment([tf(78.0, 74.0), corner], stroke);
    painter.line_segment([corner, tf(40.0, 34.0)], stroke);
    // stroke-linejoin:round 相当。折れ角を丸めるための小円。
    painter.circle_filled(corner, stroke_w / 2.0, color);

    let apex = tf(40.0, 14.0);
    let left = tf(22.0, 38.0);
    let right = tf(58.0, 38.0);
    painter.add(Shape::convex_polygon(vec![apex, left, right], color, Stroke::NONE));
}

/// フォルダアイコン（F案：前後2枚重ねで厚みを表現）。矩形2枚の組み合わせのみで
/// 構成し、凹多角形の塗り潰しに頼らない（egui の凸多角形塗りでは意図しない
/// アーティファクトが出うるため）。
pub fn draw_folder_icon(painter: &Painter, rect: Rect, color: Color32) {
    let scale = icon_scale(rect);
    let tf = transform(rect, scale);

    // 背面シート：右上にわずかにずらした薄い塗り＋輪郭線で「奥のシート」を表現
    let back_body = Rect::from_two_pos(tf(20.0, 40.0), tf(92.0, 76.0));
    let back_tab = Rect::from_two_pos(tf(20.0, 30.0), tf(54.0, 40.0));
    let back_fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60);
    painter.rect_filled(back_body, 2.0 * scale, back_fill);
    painter.rect_filled(back_tab, 1.0 * scale, back_fill);
    let back_stroke = Stroke::new(1.5 * scale, color);
    painter.rect_stroke(back_body, 2.0 * scale, back_stroke, egui::StrokeKind::Outside);

    // 前面シート：本体＋タブの2矩形、共に不透明で塗り潰し（凸形状のみ使用）
    let front_body = Rect::from_two_pos(tf(8.0, 36.0), tf(84.0, 72.0));
    let front_tab = Rect::from_two_pos(tf(8.0, 26.0), tf(38.0, 36.0));
    painter.rect_filled(front_body, 2.0 * scale, color);
    painter.rect_filled(front_tab, 1.0 * scale, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_maps_center_to_rect_center() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(74.0, 104.6));
        let tf = transform(rect, icon_scale(rect));
        let mapped = tf(50.0, 50.0);
        assert!((mapped - rect.center()).length() < 0.001);
    }
}
