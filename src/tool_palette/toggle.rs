//! ツールパレットのワンアクション型（Toggle）スロット定義。
//! 各Toggleの実行内容はラベル・get/setアクセサとして静的テーブル(TOGGLE_DEFS)に
//! データとして持たせ、実行本体(execute_toggle)は1箇所に集約する。
//! 新規Toggleを追加する場合は ToggleKind に1バリアント、TOGGLE_DEFS に1エントリを足す。

use crate::gui_config::ViewerConfig;
use crate::i18n::Lang;
use crate::image_filter::ColorFilterMode;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToggleKind {
    /// ブルーライトカット（色温度は変更せず、None⇔BlueLightCutを切替）
    BlueLightCut,
    /// ガンマ補正ステージの有効/無効
    GammaEnabled,
    /// ブライトネスステージの有効/無効
    BrightnessEnabled,
    /// シャープネスステージの有効/無効
    SharpnessEnabled,
    /// 虫眼鏡（ホイール拡縮）モードのON/OFF
    Magnifier,
    /// 画像情報（解像度・ページ数）オーバーレイの表示ON/OFF
    ImageInfo,
}

impl ToggleKind {
    /// 永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            ToggleKind::BlueLightCut => "blue_light_cut",
            ToggleKind::GammaEnabled => "gamma_enabled",
            ToggleKind::BrightnessEnabled => "brightness_enabled",
            ToggleKind::SharpnessEnabled => "sharpness_enabled",
            ToggleKind::Magnifier => "magnifier",
            ToggleKind::ImageInfo => "image_info",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "blue_light_cut" => ToggleKind::BlueLightCut,
            "gamma_enabled" => ToggleKind::GammaEnabled,
            "brightness_enabled" => ToggleKind::BrightnessEnabled,
            "sharpness_enabled" => ToggleKind::SharpnessEnabled,
            "magnifier" => ToggleKind::Magnifier,
            "image_info" => ToggleKind::ImageInfo,
            _ => return None,
        })
    }
}

/// 1つのToggleスロットの定義。UI表示に必要な情報と、ViewerConfigへの
/// get/setアクセサを1箇所にまとめる（データ駆動）。
pub struct ToggleDef {
    pub key: ToggleKind,
    pub label: fn(Lang) -> &'static str,
    pub get: fn(&ViewerConfig) -> bool,
    pub set: fn(&mut ViewerConfig, bool),
}

pub const TOGGLE_DEFS: &[ToggleDef] = &[
    ToggleDef {
        key: ToggleKind::BlueLightCut,
        label: Lang::tool_palette_toggle_label_blue_light_cut,
        get: |cfg| cfg.image_filter.color_filter_mode == ColorFilterMode::BlueLightCut,
        set: |cfg, on| {
            cfg.image_filter.color_filter_mode =
                if on { ColorFilterMode::BlueLightCut } else { ColorFilterMode::None };
        },
    },
    ToggleDef {
        key: ToggleKind::GammaEnabled,
        label: Lang::tool_palette_toggle_label_gamma,
        get: |cfg| cfg.image_filter.gamma_enabled,
        set: |cfg, on| cfg.image_filter.gamma_enabled = on,
    },
    ToggleDef {
        key: ToggleKind::BrightnessEnabled,
        label: Lang::tool_palette_toggle_label_brightness,
        get: |cfg| cfg.image_filter.brightness_enabled,
        set: |cfg, on| cfg.image_filter.brightness_enabled = on,
    },
    ToggleDef {
        key: ToggleKind::SharpnessEnabled,
        label: Lang::tool_palette_toggle_label_sharpness,
        get: |cfg| cfg.image_filter.sharpness_enabled,
        set: |cfg, on| cfg.image_filter.sharpness_enabled = on,
    },
    ToggleDef {
        key: ToggleKind::Magnifier,
        label: Lang::tool_palette_toggle_label_magnifier,
        get: |cfg| cfg.magnifier_on,
        // パレットでの明示的な切替は、既定動作での入場ではない扱い（自動退場の対象外）。
        set: |cfg, on| {
            cfg.magnifier_on = on;
            cfg.magnifier_entered_by_default = false;
        },
    },
    ToggleDef {
        key: ToggleKind::ImageInfo,
        label: Lang::tool_palette_toggle_label_image_info,
        get: |cfg| cfg.image_info_visible,
        set: |cfg, on| cfg.image_info_visible = on,
    },
];

/// key に対応する定義を探す。TOGGLE_DEFS は全 ToggleKind を網羅している前提
/// （`toggle_defs_cover_all_kinds` テストで担保）。
pub fn find_toggle_def(key: ToggleKind) -> &'static ToggleDef {
    TOGGLE_DEFS
        .iter()
        .find(|d| d.key == key)
        .expect("TOGGLE_DEFS must cover all ToggleKind variants")
}

/// 現在値を反転してViewerConfigへ書き込む。
pub fn execute_toggle(cfg: &mut ViewerConfig, kind: ToggleKind) {
    let def = find_toggle_def(kind);
    let now = (def.get)(cfg);
    (def.set)(cfg, !now);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [ToggleKind; 6] = [
        ToggleKind::BlueLightCut,
        ToggleKind::GammaEnabled,
        ToggleKind::BrightnessEnabled,
        ToggleKind::SharpnessEnabled,
        ToggleKind::Magnifier,
        ToggleKind::ImageInfo,
    ];

    #[test]
    fn toggle_defs_cover_all_kinds() {
        for k in ALL_KINDS {
            assert!(TOGGLE_DEFS.iter().any(|d| d.key == k), "missing ToggleDef for {k:?}");
        }
    }

    #[test]
    fn id_roundtrip() {
        for k in ALL_KINDS {
            assert_eq!(ToggleKind::from_id(k.id()), Some(k));
        }
    }

    #[test]
    fn execute_toggle_flips_value() {
        let mut cfg = ViewerConfig::default();
        assert!(cfg.image_filter.gamma_enabled);
        execute_toggle(&mut cfg, ToggleKind::GammaEnabled);
        assert!(!cfg.image_filter.gamma_enabled);
        execute_toggle(&mut cfg, ToggleKind::GammaEnabled);
        assert!(cfg.image_filter.gamma_enabled);
    }

    #[test]
    fn execute_toggle_magnifier_clears_the_default_entry_flag() {
        let mut cfg = ViewerConfig::default();
        // 既定動作で入場した状態から、パレットで切り替えると、明示操作になる。
        cfg.magnifier_on = true;
        cfg.magnifier_entered_by_default = true;
        execute_toggle(&mut cfg, ToggleKind::Magnifier);
        assert!(!cfg.magnifier_on);
        assert!(!cfg.magnifier_entered_by_default);
        // 明示ONにした場合も、既定動作での入場ではない。
        execute_toggle(&mut cfg, ToggleKind::Magnifier);
        assert!(cfg.magnifier_on);
        assert!(!cfg.magnifier_entered_by_default);
    }

    #[test]
    fn execute_toggle_magnifier_flips_runtime_flag() {
        let mut cfg = ViewerConfig::default();
        assert!(!cfg.magnifier_on);
        execute_toggle(&mut cfg, ToggleKind::Magnifier);
        assert!(cfg.magnifier_on);
        execute_toggle(&mut cfg, ToggleKind::Magnifier);
        assert!(!cfg.magnifier_on);
    }

    #[test]
    fn execute_toggle_image_info_flips_visibility() {
        let mut cfg = ViewerConfig::default();
        assert!(cfg.image_info_visible, "既定はON");
        execute_toggle(&mut cfg, ToggleKind::ImageInfo);
        assert!(!cfg.image_info_visible);
        execute_toggle(&mut cfg, ToggleKind::ImageInfo);
        assert!(cfg.image_info_visible);
    }

    #[test]
    fn execute_toggle_blue_light_cut_is_exclusive_with_none() {
        let mut cfg = ViewerConfig::default();
        assert_eq!(cfg.image_filter.color_filter_mode, ColorFilterMode::None);
        execute_toggle(&mut cfg, ToggleKind::BlueLightCut);
        assert_eq!(cfg.image_filter.color_filter_mode, ColorFilterMode::BlueLightCut);
        execute_toggle(&mut cfg, ToggleKind::BlueLightCut);
        assert_eq!(cfg.image_filter.color_filter_mode, ColorFilterMode::None);
    }
}
