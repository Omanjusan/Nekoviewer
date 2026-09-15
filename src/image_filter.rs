//! 画像処理フィルター（ブルーライトカット／セピア／モノクロ／ガンマ／ブライトネス／シャープネス）の
//! 設定データモデル。演算本体（Phase 1）はここでは扱わず、状態の型・既定値・永続化用の
//! 文字列変換のみを置く。処理順は toolbar.rs の bar_order と同じ「全項目の順列」方式で
//! D&D 並べ替えを永続化する。

/// 排他選択の色系統フィルター。同時に有効化できるのは1つのみ。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorFilterMode {
    /// フィルター無し
    None,
    /// ブルーライトカット（色温度指定）
    BlueLightCut,
    /// セピア
    Sepia,
    /// モノクロ（グレースケール）
    Grayscale,
}

pub fn parse_color_filter_mode(s: &str) -> ColorFilterMode {
    match s.trim() {
        "blue_light_cut" => ColorFilterMode::BlueLightCut,
        "sepia"          => ColorFilterMode::Sepia,
        "grayscale"      => ColorFilterMode::Grayscale,
        _                => ColorFilterMode::None,
    }
}

pub fn color_filter_mode_to_str(m: ColorFilterMode) -> &'static str {
    match m {
        ColorFilterMode::None         => "none",
        ColorFilterMode::BlueLightCut => "blue_light_cut",
        ColorFilterMode::Sepia        => "sepia",
        ColorFilterMode::Grayscale    => "grayscale",
    }
}

/// フィルター処理ステージ。ユーザーが設定画面でカードのD&Dにより処理順を並べ替える単位。
///
/// 項目を追加するときは DEFAULT_FILTER_ORDER・id()・from_id() の3箇所を揃えること
/// （`default_order_is_complete_permutation` テストが検出する）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterStage {
    /// 色系統フィルター（None/ブルーライトカット/セピア/モノクロ、排他択一）
    ColorFilter,
    /// ガンマ補正
    Gamma,
    /// ブライトネス
    Brightness,
    /// シャープネス（アンシャープマスク）
    Sharpness,
}

pub const FILTER_STAGE_COUNT: usize = 4;

/// 既定の処理順。色調整 → トーン調整 → ディテール強調、という一般的な画像編集の慣習に合わせる。
pub const DEFAULT_FILTER_ORDER: [FilterStage; FILTER_STAGE_COUNT] = [
    FilterStage::ColorFilter,
    FilterStage::Gamma,
    FilterStage::Brightness,
    FilterStage::Sharpness,
];

impl FilterStage {
    /// state ファイル永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            FilterStage::ColorFilter => "color_filter",
            FilterStage::Gamma       => "gamma",
            FilterStage::Brightness  => "brightness",
            FilterStage::Sharpness   => "sharpness",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "color_filter" => FilterStage::ColorFilter,
            "gamma"        => FilterStage::Gamma,
            "brightness"   => FilterStage::Brightness,
            "sharpness"    => FilterStage::Sharpness,
            _ => return None,
        })
    }
}

/// state ファイルのカンマ区切りID列から処理順を復元する。
/// 未知IDはこの段階で読み捨てる（新バージョンの state を旧バージョンが読んでも壊れない）。
pub fn parse_filter_order(s: &str) -> [FilterStage; FILTER_STAGE_COUNT] {
    let saved: Vec<FilterStage> = s
        .split(',')
        .filter_map(|t| FilterStage::from_id(t.trim()))
        .collect();
    resolve_filter_order(&saved)
}

/// 保存済み順序（部分列・重複・欠落あり得る）を全ステージの順列へ正規化する。
/// 前方互換規則は toolbar.rs の resolve_bar_order と同一（重複は先勝ち、欠落は
/// 既定順で直前にあるステージの直後へ補完）。
pub fn resolve_filter_order(saved: &[FilterStage]) -> [FilterStage; FILTER_STAGE_COUNT] {
    let mut order: Vec<FilterStage> = Vec::with_capacity(FILTER_STAGE_COUNT);
    for &st in saved {
        if !order.contains(&st) {
            order.push(st);
        }
    }
    for (di, &st) in DEFAULT_FILTER_ORDER.iter().enumerate() {
        if order.contains(&st) {
            continue;
        }
        let pos = DEFAULT_FILTER_ORDER[..di]
            .iter()
            .rev()
            .find_map(|prev| order.iter().position(|&x| x == *prev).map(|p| p + 1))
            .unwrap_or(0);
        order.insert(pos, st);
    }
    order.try_into().unwrap_or(DEFAULT_FILTER_ORDER)
}

/// state ファイル保存用のカンマ区切りID列へ変換する。
pub fn filter_order_to_str(order: &[FilterStage]) -> String {
    order.iter().map(|s| s.id()).collect::<Vec<_>>().join(",")
}

// ── パラメータの範囲・既定値 ─────────────────────────────────────────

/// ブルーライトカットの色温度スライダー下限/上限(K)。
pub const BLC_TEMP_FLOOR_K: u32 = 1000;
pub const BLC_TEMP_CEILING_K: u32 = 10000;
/// 色温度の既定値(K)。一般的な昼光色相当。
pub const BLC_TEMP_DEFAULT_K: u32 = 6500;
/// ワンアクション設定ボタンのプリセット色温度(K)。
pub const BLC_PRESET_TEMPS_K: [u32; 3] = [6500, 6000, 5500];

/// ガンマ補正スライダー下限/上限/既定値。1.0 = 無変化。
pub const GAMMA_FLOOR: f32 = 0.2;
pub const GAMMA_CEILING: f32 = 3.0;
pub const GAMMA_DEFAULT: f32 = 1.0;

/// ブライトネススライダー下限/上限/既定値(%)。0 = 無変化。
pub const BRIGHTNESS_FLOOR: f32 = -100.0;
pub const BRIGHTNESS_CEILING: f32 = 100.0;
pub const BRIGHTNESS_DEFAULT: f32 = 0.0;

/// シャープネス（アンシャープマスク強度）スライダー下限/上限/既定値。0 = 無変化。
pub const SHARPNESS_FLOOR: f32 = 0.0;
pub const SHARPNESS_CEILING: f32 = 100.0;
pub const SHARPNESS_DEFAULT: f32 = 0.0;

/// スライダードラッグ中のプレビュー更新デバウンス時間(ms)。
pub const FILTER_PREVIEW_DEBOUNCE_MS: u64 = 500;

/// 画像処理フィルターの永続設定一式。ViewerConfig に埋め込む。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ImageFilterSettings {
    /// 排他選択の色系統フィルター
    pub color_filter_mode: ColorFilterMode,
    /// ブルーライトカットの色温度(K)。プリセットボタン・スライダーいずれからも
    /// この単一値を書き換える（最後に操作した方が有効値になる）。
    pub blc_color_temperature_k: u32,
    /// ガンマ補正値
    pub gamma: f32,
    /// ガンマステージの有効/無効。false の間は値を保持したまま処理をスキップする
    /// （値を既定値に戻さずに一時的にOFFへ切り替えたいケース向け）。
    pub gamma_enabled: bool,
    /// ブライトネス(%)
    pub brightness: f32,
    /// ブライトネスステージの有効/無効（gamma_enabledと同じ位置づけ）。
    pub brightness_enabled: bool,
    /// シャープネス強度
    pub sharpness: f32,
    /// シャープネスステージの有効/無効（gamma_enabledと同じ位置づけ）。
    pub sharpness_enabled: bool,
    /// 処理順（設定画面のカードD&Dで並べ替え、既定はDEFAULT_FILTER_ORDER）
    pub filter_order: [FilterStage; FILTER_STAGE_COUNT],
}

impl Default for ImageFilterSettings {
    fn default() -> Self {
        Self {
            color_filter_mode: ColorFilterMode::None,
            blc_color_temperature_k: BLC_TEMP_DEFAULT_K,
            gamma: GAMMA_DEFAULT,
            gamma_enabled: true,
            brightness: BRIGHTNESS_DEFAULT,
            brightness_enabled: true,
            sharpness: SHARPNESS_DEFAULT,
            sharpness_enabled: true,
            filter_order: DEFAULT_FILTER_ORDER,
        }
    }
}

// ── フィルター演算コア ──────────────────────────────────────────────
// 確定後（設定変更→再デコード時）に1回だけ画像バッファへ適用する想定。
// no-op値（既定値）のステージは早期リターンで計算そのものをスキップする。

use image::RgbaImage;

/// 設定済みの処理順に従い、各フィルターステージを画像バッファへ順次適用する。
///
/// UI・キャッシュ層に依存しない純粋関数（`RgbaImage`と`ImageFilterSettings`のみを扱う）
/// なので、将来の画像エクスポート機能もそのままこの関数を呼べる設計になっている。
/// 想定パターン: デコード→(必要ならリサイズ)→`apply_image_filters`→エンコード保存、と
/// 現在のビューアー表示パイプライン（cache.rs の spawn_worker 内呼び出し）と同じ箇所に
/// 挟むだけでよい。呼び出し元(cache.rs)は現状「静止画のみ・アニメーション除外」の判断を
/// 呼び出し側で行っているが、この関数自体にその制約はなく、アニメーションの1フレームに
/// 対しても同様に呼び出せる。
pub fn apply_image_filters(img: &mut RgbaImage, settings: &ImageFilterSettings) {
    for stage in settings.filter_order {
        match stage {
            FilterStage::ColorFilter => apply_color_filter(
                img,
                settings.color_filter_mode,
                settings.blc_color_temperature_k,
            ),
            FilterStage::Gamma if settings.gamma_enabled => apply_gamma(img, settings.gamma),
            FilterStage::Brightness if settings.brightness_enabled => apply_brightness(img, settings.brightness),
            FilterStage::Sharpness if settings.sharpness_enabled => apply_sharpness(img, settings.sharpness),
            FilterStage::Gamma | FilterStage::Brightness | FilterStage::Sharpness => {}
        }
    }
}

/// 色系統フィルター（排他択一）を適用する。
fn apply_color_filter(img: &mut RgbaImage, mode: ColorFilterMode, blc_temp_k: u32) {
    match mode {
        ColorFilterMode::None => {}
        ColorFilterMode::BlueLightCut => apply_blue_light_cut(img, blc_temp_k),
        ColorFilterMode::Sepia => apply_sepia(img),
        ColorFilterMode::Grayscale => apply_grayscale(img),
    }
}

/// 色温度(K)からTanner Helland近似式でRGB(0〜255相当)を求める。
/// 1000〜10000Kの実用域で十分な精度の近似式として広く使われているもの。
fn color_temperature_to_rgb(kelvin: u32) -> (f32, f32, f32) {
    let temp = kelvin as f32 / 100.0;

    let red = if temp <= 66.0 {
        255.0
    } else {
        (329.698727446 * (temp - 60.0).powf(-0.1332047592)).clamp(0.0, 255.0)
    };

    let green = if temp <= 66.0 {
        (99.4708025861 * temp.ln() - 161.1195681661).clamp(0.0, 255.0)
    } else {
        (288.1221695283 * (temp - 60.0).powf(-0.0755148492)).clamp(0.0, 255.0)
    };

    let blue = if temp >= 66.0 {
        255.0
    } else if temp <= 19.0 {
        0.0
    } else {
        (138.5177312231 * (temp - 10.0).ln() - 305.0447927307).clamp(0.0, 255.0)
    };

    (red, green, blue)
}

/// 色温度をブルーライトカット用の乗算ゲイン(R,G,B)へ変換する。
/// BLC_TEMP_DEFAULT_K(6500K)を基準（ゲイン1.0＝無変化）とした相対値。
fn blc_gain(kelvin: u32) -> (f32, f32, f32) {
    let (r0, g0, b0) = color_temperature_to_rgb(BLC_TEMP_DEFAULT_K);
    let (r, g, b) = color_temperature_to_rgb(kelvin);
    (r / r0, g / g0, b / b0)
}

fn apply_blue_light_cut(img: &mut RgbaImage, kelvin: u32) {
    if kelvin == BLC_TEMP_DEFAULT_K {
        return;
    }
    let (gr, gg, gb) = blc_gain(kelvin);
    for px in img.pixels_mut() {
        px.0[0] = (px.0[0] as f32 * gr).round().clamp(0.0, 255.0) as u8;
        px.0[1] = (px.0[1] as f32 * gg).round().clamp(0.0, 255.0) as u8;
        px.0[2] = (px.0[2] as f32 * gb).round().clamp(0.0, 255.0) as u8;
    }
}

/// 標準的なセピア変換行列。
fn apply_sepia(img: &mut RgbaImage) {
    for px in img.pixels_mut() {
        let r = px.0[0] as f32;
        let g = px.0[1] as f32;
        let b = px.0[2] as f32;
        px.0[0] = (0.393 * r + 0.769 * g + 0.189 * b).round().clamp(0.0, 255.0) as u8;
        px.0[1] = (0.349 * r + 0.686 * g + 0.168 * b).round().clamp(0.0, 255.0) as u8;
        px.0[2] = (0.272 * r + 0.534 * g + 0.131 * b).round().clamp(0.0, 255.0) as u8;
    }
}

/// ITU-R BT.601輝度係数によるグレースケール変換。
fn apply_grayscale(img: &mut RgbaImage) {
    for px in img.pixels_mut() {
        let r = px.0[0] as f32;
        let g = px.0[1] as f32;
        let b = px.0[2] as f32;
        let gray = (0.299 * r + 0.587 * g + 0.114 * b).round().clamp(0.0, 255.0) as u8;
        px.0[0] = gray;
        px.0[1] = gray;
        px.0[2] = gray;
    }
}

/// ガンマ補正LUTを構築する。出力 = 255*(入力/255)^(1/gamma)。
fn gamma_lut(gamma: f32) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let inv = 1.0 / gamma;
    for (i, v) in lut.iter_mut().enumerate() {
        let normalized = i as f32 / 255.0;
        *v = (normalized.powf(inv) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

fn apply_gamma(img: &mut RgbaImage, gamma: f32) {
    if (gamma - GAMMA_DEFAULT).abs() < f32::EPSILON {
        return;
    }
    let lut = gamma_lut(gamma);
    for px in img.pixels_mut() {
        px.0[0] = lut[px.0[0] as usize];
        px.0[1] = lut[px.0[1] as usize];
        px.0[2] = lut[px.0[2] as usize];
    }
}

/// ブライトネスLUTを構築する。出力 = 入力 + (brightness/100)*255。
fn brightness_lut(brightness: f32) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let offset = (brightness / 100.0) * 255.0;
    for (i, v) in lut.iter_mut().enumerate() {
        *v = (i as f32 + offset).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

fn apply_brightness(img: &mut RgbaImage, brightness: f32) {
    if brightness.abs() < f32::EPSILON {
        return;
    }
    let lut = brightness_lut(brightness);
    for px in img.pixels_mut() {
        px.0[0] = lut[px.0[0] as usize];
        px.0[1] = lut[px.0[1] as usize];
        px.0[2] = lut[px.0[2] as usize];
    }
}

/// アンシャープマスクによるシャープネス強調。3x3ボックスブラーを低域成分とみなし、
/// 原画像との差分（高域成分）を strength 倍して原画像に加算する。
/// 唯一畳み込みを要するステージ（O(9×W×H)）。確定後の1回適用のみに留める前提。
fn apply_sharpness(img: &mut RgbaImage, sharpness: f32) {
    if sharpness <= 0.0 {
        return;
    }
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return;
    }
    let amount = (sharpness / 100.0) * 2.0;
    let src = img.clone();

    let sample = |x: i32, y: i32| -> [f32; 3] {
        let cx = x.clamp(0, w as i32 - 1) as u32;
        let cy = y.clamp(0, h as i32 - 1) as u32;
        let p = src.get_pixel(cx, cy);
        [p.0[0] as f32, p.0[1] as f32, p.0[2] as f32]
    };

    for y in 0..h {
        for x in 0..w {
            let mut blurred = [0f32; 3];
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let s = sample(x as i32 + dx, y as i32 + dy);
                    blurred[0] += s[0];
                    blurred[1] += s[1];
                    blurred[2] += s[2];
                }
            }
            blurred[0] /= 9.0;
            blurred[1] /= 9.0;
            blurred[2] /= 9.0;

            let orig = sample(x as i32, y as i32);
            let px = img.get_pixel_mut(x, y);
            for c in 0..3 {
                let sharpened = orig[c] + amount * (orig[c] - blurred[c]);
                px.0[c] = sharpened.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_order_is_complete_permutation() {
        for (i, &a) in DEFAULT_FILTER_ORDER.iter().enumerate() {
            assert!(
                !DEFAULT_FILTER_ORDER[..i].contains(&a),
                "{:?} が既定順に重複している",
                a
            );
            assert_eq!(FilterStage::from_id(a.id()), Some(a));
        }
    }

    #[test]
    fn roundtrip_default() {
        let s = filter_order_to_str(&DEFAULT_FILTER_ORDER);
        assert_eq!(parse_filter_order(&s), DEFAULT_FILTER_ORDER);
    }

    #[test]
    fn empty_string_falls_back_to_default() {
        assert_eq!(parse_filter_order(""), DEFAULT_FILTER_ORDER);
    }

    #[test]
    fn unknown_ids_are_ignored() {
        let s = format!("future_stage_xyz,{}", filter_order_to_str(&DEFAULT_FILTER_ORDER));
        assert_eq!(parse_filter_order(&s), DEFAULT_FILTER_ORDER);
    }

    #[test]
    fn custom_order_is_preserved() {
        let mut custom = DEFAULT_FILTER_ORDER;
        custom.reverse();
        let s = filter_order_to_str(&custom);
        assert_eq!(parse_filter_order(&s), custom);
    }

    #[test]
    fn missing_id_is_inserted_at_default_position() {
        let saved: Vec<FilterStage> = DEFAULT_FILTER_ORDER
            .iter()
            .copied()
            .filter(|&st| st != FilterStage::Gamma)
            .collect();
        assert_eq!(resolve_filter_order(&saved), DEFAULT_FILTER_ORDER);
    }

    #[test]
    fn color_filter_mode_roundtrip() {
        for m in [
            ColorFilterMode::None,
            ColorFilterMode::BlueLightCut,
            ColorFilterMode::Sepia,
            ColorFilterMode::Grayscale,
        ] {
            assert_eq!(parse_color_filter_mode(color_filter_mode_to_str(m)), m);
        }
    }

    #[test]
    fn default_settings_are_no_op_values() {
        let d = ImageFilterSettings::default();
        assert_eq!(d.color_filter_mode, ColorFilterMode::None);
        assert_eq!(d.gamma, GAMMA_DEFAULT);
        assert_eq!(d.brightness, BRIGHTNESS_DEFAULT);
        assert_eq!(d.sharpness, SHARPNESS_DEFAULT);
    }

    #[test]
    fn default_settings_have_stages_enabled() {
        let d = ImageFilterSettings::default();
        assert!(d.gamma_enabled);
        assert!(d.brightness_enabled);
        assert!(d.sharpness_enabled);
    }

    use image::Rgba;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(rgba))
    }

    #[test]
    fn default_pipeline_is_no_op() {
        let before = solid(4, 4, [120, 130, 140, 255]);
        let mut after = before.clone();
        apply_image_filters(&mut after, &ImageFilterSettings::default());
        assert_eq!(before, after);
    }

    #[test]
    fn grayscale_equalizes_channels() {
        let mut img = solid(2, 2, [200, 50, 10, 255]);
        apply_grayscale(&mut img);
        let p = img.get_pixel(0, 0);
        assert_eq!(p.0[0], p.0[1]);
        assert_eq!(p.0[1], p.0[2]);
        assert_eq!(p.0[3], 255); // アルファは変化しない
    }

    #[test]
    fn sepia_shifts_toward_warm_tone() {
        let mut img = solid(2, 2, [100, 100, 100, 255]);
        apply_sepia(&mut img);
        let p = img.get_pixel(0, 0);
        // セピアはR>G>Bの暖色寄りになる
        assert!(p.0[0] > p.0[1]);
        assert!(p.0[1] > p.0[2]);
    }

    #[test]
    fn gamma_above_one_brightens_midtone() {
        let mut img = solid(2, 2, [128, 128, 128, 255]);
        apply_gamma(&mut img, 2.0);
        assert!(img.get_pixel(0, 0).0[0] > 128);
    }

    #[test]
    fn gamma_default_is_no_op() {
        let before = solid(2, 2, [77, 88, 99, 255]);
        let mut after = before.clone();
        apply_gamma(&mut after, GAMMA_DEFAULT);
        assert_eq!(before, after);
    }

    #[test]
    fn brightness_max_saturates_to_white() {
        let mut img = solid(2, 2, [0, 0, 0, 255]);
        apply_brightness(&mut img, BRIGHTNESS_CEILING);
        assert_eq!(img.get_pixel(0, 0).0[0], 255);
    }

    #[test]
    fn brightness_zero_is_no_op() {
        let before = solid(2, 2, [10, 20, 30, 255]);
        let mut after = before.clone();
        apply_brightness(&mut after, 0.0);
        assert_eq!(before, after);
    }

    #[test]
    fn sharpness_zero_is_no_op() {
        let before = solid(4, 4, [50, 60, 70, 255]);
        let mut after = before.clone();
        apply_sharpness(&mut after, 0.0);
        assert_eq!(before, after);
    }

    #[test]
    fn sharpness_does_not_change_flat_image() {
        // 平坦な画像はアンシャープマスクをかけても変化しない（周辺と同値のため）
        let before = solid(5, 5, [100, 100, 100, 255]);
        let mut after = before.clone();
        apply_sharpness(&mut after, 50.0);
        assert_eq!(before, after);
    }

    #[test]
    fn blue_light_cut_default_temp_is_no_op() {
        let before = solid(2, 2, [200, 150, 100, 255]);
        let mut after = before.clone();
        apply_blue_light_cut(&mut after, BLC_TEMP_DEFAULT_K);
        assert_eq!(before, after);
    }

    #[test]
    fn blue_light_cut_warm_reduces_blue_relative_to_red() {
        let mut img = solid(2, 2, [200, 200, 200, 255]);
        apply_blue_light_cut(&mut img, BLC_TEMP_FLOOR_K);
        let p = img.get_pixel(0, 0);
        // 低色温度(暖色)側では青の減衰が赤より大きくなる
        assert!(p.0[2] < p.0[0]);
    }

    #[test]
    fn pipeline_respects_configured_order() {
        // 色系統フィルター(グレースケール)を最後に回すと、その前段のセピア色付けが
        // 最終的にグレースケールで打ち消される
        let mut settings = ImageFilterSettings {
            color_filter_mode: ColorFilterMode::Grayscale,
            ..ImageFilterSettings::default()
        };
        settings.filter_order = [
            FilterStage::Gamma,
            FilterStage::Brightness,
            FilterStage::Sharpness,
            FilterStage::ColorFilter,
        ];
        let mut img = solid(2, 2, [200, 50, 10, 255]);
        apply_image_filters(&mut img, &settings);
        let p = img.get_pixel(0, 0);
        assert_eq!(p.0[0], p.0[1]);
        assert_eq!(p.0[1], p.0[2]);
    }

    #[test]
    fn disabled_stage_is_skipped_even_with_non_default_value() {
        // 値そのものは非デフォルトのままでも、enabledがfalseなら処理をスキップする
        // （一時的にOFFにして値を保持したまま比較したいケース）。
        let settings = ImageFilterSettings {
            gamma: 2.0,
            gamma_enabled: false,
            brightness: 50.0,
            brightness_enabled: false,
            sharpness: 80.0,
            sharpness_enabled: false,
            ..ImageFilterSettings::default()
        };
        let before = solid(3, 3, [100, 120, 140, 255]);
        let mut after = before.clone();
        apply_image_filters(&mut after, &settings);
        assert_eq!(before, after);
    }

    #[test]
    fn re_enabling_a_stage_restores_its_effect_with_the_kept_value() {
        let mut settings = ImageFilterSettings {
            gamma: 2.0,
            gamma_enabled: false,
            ..ImageFilterSettings::default()
        };
        let mut disabled = solid(2, 2, [128, 128, 128, 255]);
        apply_image_filters(&mut disabled, &settings);

        settings.gamma_enabled = true;
        let mut enabled = solid(2, 2, [128, 128, 128, 255]);
        apply_image_filters(&mut enabled, &settings);

        assert_ne!(disabled, enabled, "再度ONにすれば保持していた値(2.0)がそのまま効くはず");
        assert!(enabled.get_pixel(0, 0).0[0] > disabled.get_pixel(0, 0).0[0]);
    }
}
