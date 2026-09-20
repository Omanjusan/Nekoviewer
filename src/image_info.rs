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
    let size = ext * scale * ppp;
    Some((size.x.round() as u32, size.y.round() as u32))
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

/// 解像度部分の文字列を組み立てる。出せる情報が無ければ空文字。
/// - `fitted`: フィット後の実表示寸法（`InfoMode::Fit` で使う）
/// - `orig`: オリジナルピクセル寸法。不明ならテクスチャ寸法 `tex` で代用する
/// - `rel_scale`: オリジナル基準の倍率（`InfoMode::Magnifier` で使う）
pub fn compose_resolution_text(
    mode: InfoMode,
    fitted: Option<(u32, u32)>,
    orig: Option<(u32, u32)>,
    tex: Option<(u32, u32)>,
    rel_scale: Option<f32>,
) -> String {
    let actual = orig.or(tex);
    match mode {
        InfoMode::Fit => fitted.map(|(w, h)| format_resolution(w, h)).unwrap_or_default(),
        InfoMode::Actual => actual.map(|(w, h)| format_resolution(w, h)).unwrap_or_default(),
        InfoMode::Magnifier => match (actual, rel_scale) {
            (Some((w, h)), Some(s)) => format!("{} {}", format_resolution(w, h), format_scale(s)),
            (Some((w, h)), None) => format_resolution(w, h),
            _ => String::new(),
        },
    }
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
        assert_eq!(compose_resolution_text(InfoMode::Fit, fitted, orig, tex, None), "1000×500");
        assert_eq!(compose_resolution_text(InfoMode::Actual, fitted, orig, tex, None), "4000×2000");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, fitted, orig, tex, Some(1.1)), "4000×2000 (x1.10)");
    }

    #[test]
    fn compose_text_falls_back_to_texture_size_without_original() {
        assert_eq!(compose_resolution_text(InfoMode::Actual, None, None, Some((2000, 1000)), None), "2000×1000");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, None, None, Some((2000, 1000)), Some(0.5)), "2000×1000 (x0.50)");
    }

    #[test]
    fn compose_text_is_empty_without_data() {
        assert_eq!(compose_resolution_text(InfoMode::Fit, None, None, None, None), "");
        assert_eq!(compose_resolution_text(InfoMode::Actual, None, None, None, None), "");
        assert_eq!(compose_resolution_text(InfoMode::Magnifier, None, None, None, Some(1.0)), "");
    }
}
