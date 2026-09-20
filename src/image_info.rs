//! 画像情報オーバーレイ（解像度・倍率・ページ数）の表示文字列と、実表示寸法の純粋関数。
//! 描画・状態には触らない（読み取り専用）。接続は view_reader.rs の右下オーバーレイ側で行う。

use egui::Vec2;

/// 解像度の表示モード。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InfoMode {
    /// ウィンドウ追従（フィット表示）。フィット後に実際に描かれる画像の寸法を出す。
    Fit,
    /// 原寸表示。画像のもつオリジナルピクセル寸法を出す。
    Actual,
    /// 虫眼鏡（拡縮）中。オリジナルピクセル寸法に倍率を添える。
    Magnifier,
}

/// 「1920×1080」。
pub fn format_resolution(w: u32, h: u32) -> String {
    format!("{w}×{h}")
}

/// 「(x1.10)」。
pub fn format_scale(scale: f32) -> String {
    format!("(x{scale:.2})")
}

/// 「12/240」。アニメ情報をもつページは「12A/240」。`page` は1始まり。
pub fn format_page_counter(page: usize, total: usize, animated: bool) -> String {
    format!("{page}{}/{total}", if animated { "A" } else { "" })
}

/// 寸法が正の有限値か。
fn valid(v: Vec2) -> bool {
    v.x > 0.0 && v.y > 0.0 && v.x.is_finite() && v.y.is_finite()
}

/// 表示モードの判定。原寸表示は回転(0度以外)に未対応で、回転中はフィット表示へ
/// フォールバックする（view_reader の render_spread と同じ規則）。虫眼鏡が最優先。
pub fn info_mode(magnifier: bool, zoom_actual: bool, angle_deg: i32) -> InfoMode {
    if magnifier {
        InfoMode::Magnifier
    } else if zoom_actual && angle_deg == 0 {
        InfoMode::Actual
    } else {
        InfoMode::Fit
    }
}

/// 描画矩形の寸法（論理px）を物理pxへ換算する。寸法が不正なら None。
/// 見開きは実際の配置矩形（spread_rects の結果）をそのまま渡して使う。
pub fn rect_size_px(size: Vec2, ppp: f32) -> Option<(u32, u32)> {
    if !valid(size) || !(ppp.is_finite() && ppp > 0.0) {
        return None;
    }
    let px = size * ppp;
    Some((px.x.round() as u32, px.y.round() as u32))
}

/// フィット表示で実際に描かれる画像の寸法（物理px）。レターボックスの余白は含まない。
/// `viewport` は画像領域(論理px)、`tex` は表示テクスチャの寸法（回転前）、`ppp` は
/// pixels_per_point。90/270度回転は縦横を入れ替えて収める。入力が不正なら None。
/// 縮小のみでなく拡大も含めて contain-fit する（描画側の paint_single_at と同じ）。
pub fn fitted_display_px(viewport: Vec2, tex: Vec2, angle_deg: i32, ppp: f32) -> Option<(u32, u32)> {
    let ext = crate::magnifier::rotated_extent(tex, angle_deg);
    if !valid(viewport) || !valid(ext) || !(ppp.is_finite() && ppp > 0.0) {
        return None;
    }
    let scale = (viewport.x / ext.x).min(viewport.y / ext.y);
    rect_size_px(ext * scale, ppp)
}

/// 虫眼鏡の倍率（テクスチャ基準）を、オリジナル寸法基準へ換算する。
/// `tex_len` / `orig_len` は同じ軸（高さなど）の長さ。オリジナルが不明（0以下）なら換算しない。
pub fn orig_relative_scale(scale: f32, tex_len: f32, orig_len: f32) -> f32 {
    if tex_len > 0.0 && orig_len > 0.0 && tex_len.is_finite() && orig_len.is_finite() {
        scale * tex_len / orig_len
    } else {
        scale
    }
}

/// 1ページぶんの解像度の文字列。出せる情報が無ければ空文字。
/// - `fitted`: フィット後の実表示寸法（`InfoMode::Fit` で使う）
/// - `orig`: オリジナルピクセル寸法。不明ならテクスチャ寸法 `tex` で代用する
///   （`InfoMode::Actual` / `InfoMode::Magnifier`）
pub fn compose_resolution_text(
    mode: InfoMode,
    fitted: Option<(u32, u32)>,
    orig: Option<(u32, u32)>,
    tex: Option<(u32, u32)>,
) -> String {
    let dims = match mode {
        InfoMode::Fit => fitted,
        InfoMode::Actual | InfoMode::Magnifier => orig.or(tex),
    };
    dims.map(|(w, h)| format_resolution(w, h)).unwrap_or_default()
}

/// ページ毎の解像度文字列（画面の左→右の順）を空白で連結し、末尾に倍率を1つだけ添える。
/// 空のページは省略する。全ページが空なら倍率も出さず空文字。
pub fn join_page_texts(parts: &[String], scale: Option<f32>) -> String {
    let mut text = parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    if let Some(s) = scale.filter(|_| !text.is_empty()) {
        text.push(' ');
        text.push_str(&format_scale(s));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_and_scale_formats() {
        assert_eq!(format_resolution(1920, 1080), "1920×1080");
        assert_eq!(format_scale(1.1), "(x1.10)");
        assert_eq!(format_scale(0.5), "(x0.50)");
        assert_eq!(format_scale(1.0), "(x1.00)");
        assert_eq!(format_scale(2.345), "(x2.35)");
    }

    #[test]
    fn page_counter_adds_a_only_for_animated() {
        assert_eq!(format_page_counter(1, 30, false), "1/30");
        assert_eq!(format_page_counter(1, 30, true), "1A/30");
        assert_eq!(format_page_counter(120, 240, true), "120A/240");
    }

    #[test]
    fn fitted_size_excludes_letterbox() {
        // 横長画像を 1000×800 に収める → 横一杯、縦は余白（レターボックス）を除いた寸法。
        assert_eq!(fitted_display_px(Vec2::new(1000.0, 800.0), Vec2::new(2000.0, 1000.0), 0, 1.0), Some((1000, 500)));
        // 縦長画像 → 縦一杯。
        assert_eq!(fitted_display_px(Vec2::new(1000.0, 800.0), Vec2::new(1000.0, 2000.0), 0, 1.0), Some((400, 800)));
    }

    #[test]
    fn fitted_size_scales_to_physical_px() {
        assert_eq!(fitted_display_px(Vec2::new(1000.0, 800.0), Vec2::new(2000.0, 1000.0), 0, 2.0), Some((2000, 1000)));
    }

    #[test]
    fn fitted_size_upscales_small_images_like_the_painter() {
        assert_eq!(fitted_display_px(Vec2::new(1000.0, 800.0), Vec2::new(100.0, 50.0), 0, 1.0), Some((1000, 500)));
    }

    #[test]
    fn fitted_size_swaps_axes_for_quarter_turns() {
        // 2000×1000 を90度回すと 1000×2000 として 1000×800 に収まる → 400×800。
        let vp = Vec2::new(1000.0, 800.0);
        let tex = Vec2::new(2000.0, 1000.0);
        assert_eq!(fitted_display_px(vp, tex, 90, 1.0), Some((400, 800)));
        assert_eq!(fitted_display_px(vp, tex, 270, 1.0), Some((400, 800)));
        assert_eq!(fitted_display_px(vp, tex, 180, 1.0), Some((1000, 500)));
    }

    #[test]
    fn info_mode_priority() {
        assert_eq!(info_mode(true, true, 0), InfoMode::Magnifier);
        assert_eq!(info_mode(true, false, 90), InfoMode::Magnifier);
        assert_eq!(info_mode(false, true, 0), InfoMode::Actual);
        assert_eq!(info_mode(false, false, 0), InfoMode::Fit);
        // 原寸は回転に未対応。回転中はフィットへフォールバックする。
        assert_eq!(info_mode(false, true, 90), InfoMode::Fit);
        assert_eq!(info_mode(false, true, 180), InfoMode::Fit);
    }

    #[test]
    fn rect_size_converts_and_rejects() {
        assert_eq!(rect_size_px(Vec2::new(500.4, 300.6), 2.0), Some((1001, 601)));
        assert_eq!(rect_size_px(Vec2::new(0.0, 300.0), 1.0), None);
        assert_eq!(rect_size_px(Vec2::new(10.0, 10.0), f32::NAN), None);
    }

    #[test]
    fn fitted_size_rejects_invalid_input() {
        let ok = Vec2::new(100.0, 100.0);
        assert_eq!(fitted_display_px(Vec2::ZERO, ok, 0, 1.0), None);
        assert_eq!(fitted_display_px(ok, Vec2::new(0.0, 10.0), 0, 1.0), None);
        assert_eq!(fitted_display_px(ok, ok, 0, 0.0), None);
        assert_eq!(fitted_display_px(ok, Vec2::new(f32::NAN, 10.0), 0, 1.0), None);
    }

    #[test]
    fn scale_converts_to_original_basis() {
        // テクスチャが元の半分（4000→2000）に縮小されている。テクスチャ等倍(1.0)は元寸法比 0.5。
        assert!((orig_relative_scale(1.0, 2000.0, 4000.0) - 0.5).abs() < 1e-6);
        // 縮小なし（テクスチャ = 元寸法）なら換算しない。
        assert!((orig_relative_scale(1.25, 3000.0, 3000.0) - 1.25).abs() < 1e-6);
        // 元寸法が不明ならそのまま。
        assert_eq!(orig_relative_scale(1.25, 3000.0, 0.0), 1.25);
    }

    #[test]
    fn compose_text_per_mode() {
        let fitted = Some((1000, 500));
        let orig = Some((4000, 2000));
        let tex = Some((2000, 1000));
        assert_eq!(compose_resolution_text(InfoMode::Fit, fitted, orig, tex), "1000×500");
        assert_eq!(compose_resolution_text(InfoMode::Actual, fitted, orig, tex), "4000×2000");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, fitted, orig, tex), "4000×2000");
    }

    #[test]
    fn compose_text_falls_back_to_texture_size_without_original() {
        assert_eq!(compose_resolution_text(InfoMode::Actual, None, None, Some((2000, 1000))), "2000×1000");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, None, None, Some((2000, 1000))), "2000×1000");
    }

    #[test]
    fn compose_text_is_empty_without_data() {
        assert_eq!(compose_resolution_text(InfoMode::Fit, None, None, None), "");
        assert_eq!(compose_resolution_text(InfoMode::Actual, None, None, None), "");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, None, None, None), "");
        // フィットでは、元寸法があっても実表示寸法が無ければ出さない。
        assert_eq!(compose_resolution_text(InfoMode::Fit, None, Some((10, 10)), Some((10, 10))), "");
    }

    #[test]
    fn join_puts_pages_in_order_with_one_trailing_scale() {
        let parts = vec!["800×1200".to_string(), "700×1100".to_string()];
        assert_eq!(join_page_texts(&parts, None), "800×1200 700×1100");
        assert_eq!(join_page_texts(&parts, Some(1.1)), "800×1200 700×1100 (x1.10)");
        assert_eq!(join_page_texts(&parts[..1], Some(0.5)), "800×1200 (x0.50)");
    }

    #[test]
    fn join_skips_empty_pages_and_hides_scale_without_any() {
        let parts = vec![String::new(), "700×1100".to_string()];
        assert_eq!(join_page_texts(&parts, Some(2.0)), "700×1100 (x2.00)");
        assert_eq!(join_page_texts(&[String::new(), String::new()], Some(2.0)), "");
        assert_eq!(join_page_texts(&[], Some(2.0)), "");
    }
}
