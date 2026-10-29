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

/// 書類アイコン（P2案：縦長の白い紙・折れ角を塗る・横線3本で文字を表す）。
/// 紙は背景に依らず白で固定し、輪郭と横線の色だけ `hovered` で NAV_ICON_COLOR に切り替える。
/// 24x24の設計座標を rect へ均一スケールで写像する。形はすべて凸（紙の五角形・折れ角の三角形）。
pub fn draw_document_icon(painter: &Painter, rect: Rect, hovered: bool) {
    let scale = rect.width().min(rect.height()) / 24.0;
    let center = rect.center();
    let tf = |x: f32, y: f32| center + (pos2(x, y) - pos2(12.0, 12.0)) * scale;
    let (outline, text_line) = if hovered {
        (NAV_ICON_COLOR, NAV_ICON_COLOR)
    } else {
        (Color32::from_rgb(86, 96, 112), Color32::from_rgb(125, 134, 150))
    };
    let stroke_w = (1.5 * scale).max(1.0);
    let outline_stroke = Stroke::new(stroke_w, outline);

    // 紙：左上から時計回り。右上の角を折れ角で落とした五角形。
    let paper = vec![tf(5.0, 2.0), tf(14.0, 2.0), tf(19.0, 7.0), tf(19.0, 22.0), tf(5.0, 22.0)];
    painter.add(Shape::convex_polygon(paper, Color32::WHITE, outline_stroke));
    // 折れ角：薄いグレーの三角形
    let fold = vec![tf(14.0, 2.0), tf(19.0, 7.0), tf(14.0, 7.0)];
    painter.add(Shape::convex_polygon(fold, Color32::from_rgb(213, 217, 224), outline_stroke));
    // 文字を表す横線
    let line_stroke = Stroke::new(stroke_w, text_line);
    painter.line_segment([tf(8.0, 11.0), tf(16.0, 11.0)], line_stroke);
    painter.line_segment([tf(8.0, 14.0), tf(16.0, 14.0)], line_stroke);
    painter.line_segment([tf(8.0, 17.0), tf(13.0, 17.0)], line_stroke);
}

/// text を font_id で計測し、max_width に収まらなければ末尾を "…" 付きでカットする。
/// egui の実測フォントメトリクスを使うため、フォント・文字種によらず正確に収まる。
pub fn truncate_to_width(ui: &egui::Ui, text: &str, font_id: egui::FontId, max_width: f32) -> String {
    let measure = |s: &str| -> f32 {
        ui.painter().layout_no_wrap(s.to_string(), font_id.clone(), Color32::WHITE).size().x
    };
    if text.is_empty() || measure(text) <= max_width {
        return text.to_string();
    }
    const ELLIPSIS: &str = "…";
    let chars: Vec<char> = text.chars().collect();
    for end in (0..chars.len()).rev() {
        let candidate: String = chars[..end].iter().collect::<String>() + ELLIPSIS;
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }
    ELLIPSIS.to_string()
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
