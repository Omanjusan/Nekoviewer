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
    /// ダイアログ本体の描画。viewer_cfgへの反映は実装側で行う
    /// （Phase4で既存 view_gui_config.rs のスライダー描画コードとの共通化を検討する）。
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

    /// 画像フィルタのライブ設定ダイアログ。中身の実装はPhase4で行う
    /// （既存 view_gui_config.rs のスライダー描画コードとの共通化を調査したうえで着手）。
    #[derive(Default)]
    pub struct ImageFilterDialog;

    impl PaletteDialog for ImageFilterDialog {
        fn title(&self) -> &'static str {
            "画像フィルタ"
        }

        fn render(&mut self, ui: &mut egui::Ui, _viewer_cfg: &mut ViewerConfig) {
            ui.label("(Phase4で実装)");
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
