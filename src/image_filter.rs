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
    /// ブライトネス(%)
    pub brightness: f32,
    /// シャープネス強度
    pub sharpness: f32,
    /// 処理順（設定画面のカードD&Dで並べ替え、既定はDEFAULT_FILTER_ORDER）
    pub filter_order: [FilterStage; FILTER_STAGE_COUNT],
}

impl Default for ImageFilterSettings {
    fn default() -> Self {
        Self {
            color_filter_mode: ColorFilterMode::None,
            blc_color_temperature_k: BLC_TEMP_DEFAULT_K,
            gamma: GAMMA_DEFAULT,
            brightness: BRIGHTNESS_DEFAULT,
            sharpness: SHARPNESS_DEFAULT,
            filter_order: DEFAULT_FILTER_ORDER,
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
}
