//! ツールパレットのダイアログ型（Dialog）スロット定義。
//! DialogKind ごとに専用サブモジュール・専用構造体を用意し、PaletteDialog trait を
//! 実装する。見た目・項目構成はダイアログ自身が宣言的に描く（データ駆動）。
//! DialogKindから実装への紐付けは create_dialog() のみで行う（ハードコード、動的検索はしない）。

use crate::gui_config::ViewerConfig;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogKind {
    /// 画像フィルタ設定（ガンマ・ブライトネス・シャープネスのライブ調整）
    ImageFilter,
}

/// 全DialogKind。登録メニュー（マス右クリック）はこれを走査して選択肢を出す
/// （新規Dialog追加時にメニュー側の変更が要らないようにするため）。
pub const ALL_DIALOG_KINDS: [DialogKind; 1] = [DialogKind::ImageFilter];

impl DialogKind {
    /// 永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            DialogKind::ImageFilter => "image_filter",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "image_filter" => DialogKind::ImageFilter,
            _ => return None,
        })
    }
}

/// パレットのダイアログ型マスが描く中身。実装はDialogKindごとに専用サブモジュールへ置く。
pub trait PaletteDialog {
    /// マス上・ダイアログタイトルに使う表示名
    fn title(&self) -> &'static str;
    /// 展開時の希望サイズ(幅, 高さ)。マス/パレット本体の幅には引っ張られず、
    /// ダイアログの中身（項目数・スライダー幅）に応じて実装側が自己申告する。
    fn preferred_size(&self) -> egui::Vec2;
    /// ダイアログ本体の描画。viewer_cfgへの反映は実装側で行う。
    fn render(&mut self, ui: &mut egui::Ui, viewer_cfg: &mut ViewerConfig);
}

/// DialogKind → 実装インスタンスの紐付け（ハードコード）。
pub fn create_dialog(kind: DialogKind) -> Box<dyn PaletteDialog> {
    match kind {
        DialogKind::ImageFilter => Box::new(image_filter_dialog::ImageFilterDialog::default()),
    }
}

mod image_filter_dialog {
    use super::*;

    /// 画像フィルタのライブ設定ダイアログ。中身は設定画面（画像フィルタータブ）と共通の
    /// draw_image_filter_tone_sliders を呼ぶだけ（ガンマ・ブライトネス・シャープネス）。
    /// 色系統フィルターと処理順D&Dは設定画面側のみに残す（マス1個の限られた面積に収めるため）。
    #[derive(Default)]
    pub struct ImageFilterDialog;

    impl PaletteDialog for ImageFilterDialog {
        fn title(&self) -> &'static str {
            "画像フィルタ"
        }

        fn preferred_size(&self) -> egui::Vec2 {
            egui::vec2(220.0, 190.0)
        }

        fn render(&mut self, ui: &mut egui::Ui, viewer_cfg: &mut ViewerConfig) {
            crate::view_gui_config::draw_image_filter_tone_sliders(ui, &mut viewer_cfg.image_filter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_roundtrip() {
        let all = [DialogKind::ImageFilter];
        for k in all {
            assert_eq!(DialogKind::from_id(k.id()), Some(k));
        }
    }

    #[test]
    fn create_dialog_matches_title() {
        let d = create_dialog(DialogKind::ImageFilter);
        assert_eq!(d.title(), "画像フィルタ");
    }
}
