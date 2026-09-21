//! 虫眼鏡モードのマウスカーソル画像（実行時に生成するRGBA）。
//! OS標準のズームカーソルは、Windows では winit が矢印へ落とし、Linux でもテーマ依存になる。
//! 画像ファイルを持たず、生成した画像を egui の `cursor_image`（winit の CustomCursor）で使う。
//! ホットスポットはレンズの中心。

/// 基準サイズ(px)。HiDPI では `cursor_size_for_scale` で拡大する。
const BASE_SIZE: f32 = 32.0;
const MIN_SIZE: u32 = 32;
const MAX_SIZE: u32 = 64;

/// レンズ中心・半径・リングの太さ・柄の太さ（いずれも一辺に対する比率）。
const LENS_CENTER: f32 = 0.40;
const LENS_RADIUS: f32 = 0.27;
const RING_THICKNESS: f32 = 0.085;
const HANDLE_END: f32 = 0.92;
const HANDLE_THICKNESS: f32 = 0.11;
/// 縁取り（黒）の太さ。基準サイズで約1px。
const OUTLINE: f32 = 1.0 / BASE_SIZE;
/// レンズ内側の薄い白（透けるガラス風。明るい背景でもレンズ領域が分かる）。
const LENS_TINT_ALPHA: f32 = 0.10;

pub struct CursorBitmap {
    /// 非乗算済みRGBA（行優先）。
    pub rgba: Vec<u8>,
    pub size: u32,
    pub hotspot: (u32, u32),
}

/// 画面の拡大率（pixels_per_point）に応じたカーソルの一辺(px)。
pub fn cursor_size_for_scale(pixels_per_point: f32) -> u32 {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return MIN_SIZE;
    }
    ((BASE_SIZE * pixels_per_point).round() as u32).clamp(MIN_SIZE, MAX_SIZE)
}

/// 点 `p` から線分 `a-b` までの距離。
fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + abx * t, a.1 + aby * t);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// 一辺 `size` px の虫眼鏡カーソルを生成する。白い本体に黒い縁取り。
pub fn magnifier_cursor(size: u32) -> CursorBitmap {
    let n = size.max(1) as f32;
    let center = (LENS_CENTER * n, LENS_CENTER * n);
    let radius = LENS_RADIUS * n;
    let ring_half = RING_THICKNESS * n / 2.0;
    let handle_half = HANDLE_THICKNESS * n / 2.0;
    let outline = OUTLINE * n;
    // 柄はリング外縁の右下45度から、右下の端まで。
    let dir = std::f32::consts::FRAC_1_SQRT_2;
    let handle_start = (center.0 + (radius + ring_half) * dir, center.1 + (radius + ring_half) * dir);
    let handle_end = (HANDLE_END * n, HANDLE_END * n);

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let dist_center = ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt();
            // 図形（リング＋柄）への符号付き距離。負なら内側。
            let d_ring = (dist_center - radius).abs() - ring_half;
            let d_handle = segment_distance(p, handle_start, handle_end) - handle_half;
            let d = d_ring.min(d_handle);

            let white = (0.5 - d).clamp(0.0, 1.0);
            let with_outline = (0.5 - (d - outline)).clamp(0.0, 1.0);
            let mut alpha = with_outline.max(white);
            let mut value = if alpha > 0.0 { white / alpha } else { 0.0 };
            if dist_center < radius - ring_half && alpha < LENS_TINT_ALPHA {
                alpha = LENS_TINT_ALPHA;
                value = 1.0;
            }
            let i = ((y * size + x) * 4) as usize;
            let v = (value * 255.0).round() as u8;
            rgba[i] = v;
            rgba[i + 1] = v;
            rgba[i + 2] = v;
            rgba[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    CursorBitmap {
        rgba,
        size,
        hotspot: ((center.0.round() as u32).min(size - 1), (center.1.round() as u32).min(size - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(bmp: &CursorBitmap, x: u32, y: u32) -> u8 {
        bmp.rgba[((y * bmp.size + x) * 4 + 3) as usize]
    }

    #[test]
    fn size_scales_with_dpi_and_is_clamped() {
        assert_eq!(cursor_size_for_scale(1.0), 32);
        assert_eq!(cursor_size_for_scale(1.5), 48);
        assert_eq!(cursor_size_for_scale(2.0), 64);
        assert_eq!(cursor_size_for_scale(4.0), 64);
        assert_eq!(cursor_size_for_scale(0.5), 32);
        assert_eq!(cursor_size_for_scale(f32::NAN), 32);
        assert_eq!(cursor_size_for_scale(-1.0), 32);
    }

    #[test]
    fn bitmap_has_expected_layout() {
        for size in [32, 48, 64] {
            let bmp = magnifier_cursor(size);
            assert_eq!(bmp.size, size);
            assert_eq!(bmp.rgba.len(), (size * size * 4) as usize);
            assert!(bmp.hotspot.0 < size && bmp.hotspot.1 < size);
        }
    }

    #[test]
    fn hotspot_is_lens_center_and_lens_interior_is_nearly_transparent() {
        let bmp = magnifier_cursor(32);
        // レンズ中心付近: 縁取りより内側で、薄い白（ほぼ透明）だけ。
        let a = alpha_at(&bmp, bmp.hotspot.0, bmp.hotspot.1);
        assert!(a > 0 && a < 80, "lens interior alpha = {a}");
    }

    #[test]
    fn ring_and_handle_are_opaque_and_far_corner_is_transparent() {
        let n = 32u32;
        let bmp = magnifier_cursor(n);
        // リングの真上（中心から半径ぶん上）。
        let ring_y = (LENS_CENTER * n as f32 - LENS_RADIUS * n as f32).round() as u32;
        assert_eq!(alpha_at(&bmp, bmp.hotspot.0, ring_y), 255);
        // 柄の途中（右下45度方向）。
        let hx = (LENS_CENTER * n as f32 + 0.30 * n as f32).round() as u32;
        assert_eq!(alpha_at(&bmp, hx, hx), 255);
        // 右上・左下の隅は透明。
        assert_eq!(alpha_at(&bmp, n - 1, 0), 0);
        assert_eq!(alpha_at(&bmp, 0, n - 1), 0);
    }

    #[test]
    fn outline_is_dark_and_body_is_white() {
        let n = 32u32;
        let bmp = magnifier_cursor(n);
        let mut has_white = false;
        let mut has_dark_opaque = false;
        for px in bmp.rgba.chunks_exact(4) {
            if px[3] == 255 && px[0] == 255 { has_white = true; }
            if px[3] > 200 && px[0] < 40 { has_dark_opaque = true; }
        }
        assert!(has_white, "no white body pixels");
        assert!(has_dark_opaque, "no dark outline pixels");
    }
}
