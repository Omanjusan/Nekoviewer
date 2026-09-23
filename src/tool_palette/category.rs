//! ツールパレット登録メニュー（マス右クリック）のカテゴリ分け。
//! Toggle/Dialog/Action は実装上の区分でユーザーには意味がないため、メニューは
//! 目的別カテゴリ→項目の1段サブメニューで見せる。カテゴリ内で3種は混在してよい。
//! 新規スロット種を追加したら、いずれか1カテゴリの items に必ず入れる
//! （`every_slot_kind_belongs_to_exactly_one_category` テストで担保）。

use super::{ActionKind, DialogKind, PaletteSlotContent, ToggleKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteCategory {
    /// ページ移動
    Navigate,
    /// 読書補助（閲覧）: コマ送り・スライドショー・虫眼鏡
    ReadingView,
    /// 画質（画像フィルタ系）
    ImageQuality,
    /// 表示・ウィンドウ
    Display,
}

/// メニューに並べる順。
pub const ALL_CATEGORIES: [PaletteCategory; 4] = [
    PaletteCategory::Navigate,
    PaletteCategory::ReadingView,
    PaletteCategory::ImageQuality,
    PaletteCategory::Display,
];

impl PaletteCategory {
    pub fn label(self, lang: crate::i18n::Lang) -> &'static str {
        match self {
            PaletteCategory::Navigate => lang.tool_palette_category_navigate(),
            PaletteCategory::ReadingView => lang.tool_palette_category_reading_view(),
            PaletteCategory::ImageQuality => lang.tool_palette_category_image_quality(),
            PaletteCategory::Display => lang.tool_palette_category_display(),
        }
    }

    /// カテゴリに属する項目（サブメニュー内の表示順）。
    pub fn items(self) -> &'static [PaletteSlotContent] {
        use PaletteSlotContent::{Action, Dialog, Toggle};
        match self {
            PaletteCategory::Navigate => &[
                Action(ActionKind::NextPage),
                Action(ActionKind::PrevPage),
            ],
            PaletteCategory::ReadingView => &[
                Toggle(ToggleKind::KomaMode),
                Action(ActionKind::KomaNext),
                Action(ActionKind::KomaPrev),
                Action(ActionKind::SlideshowToggle),
                Toggle(ToggleKind::Magnifier),
            ],
            PaletteCategory::ImageQuality => &[
                Toggle(ToggleKind::BlueLightCut),
                Toggle(ToggleKind::GammaEnabled),
                Toggle(ToggleKind::BrightnessEnabled),
                Toggle(ToggleKind::SharpnessEnabled),
                Dialog(DialogKind::ImageFilter),
            ],
            PaletteCategory::Display => &[
                Action(ActionKind::ToggleFullscreen),
                Toggle(ToggleKind::ImageInfo),
                Action(ActionKind::OpenFolder),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_palette::action::ALL_ACTION_KINDS;
    use crate::tool_palette::dialog::ALL_DIALOG_KINDS;
    use crate::tool_palette::toggle::TOGGLE_DEFS;

    #[test]
    fn every_slot_kind_belongs_to_exactly_one_category() {
        let all: Vec<PaletteSlotContent> = TOGGLE_DEFS
            .iter()
            .map(|d| PaletteSlotContent::Toggle(d.key))
            .chain(ALL_DIALOG_KINDS.iter().map(|&k| PaletteSlotContent::Dialog(k)))
            .chain(ALL_ACTION_KINDS.iter().map(|&k| PaletteSlotContent::Action(k)))
            .collect();
        for content in &all {
            let n = ALL_CATEGORIES
                .iter()
                .flat_map(|c| c.items())
                .filter(|c| *c == content)
                .count();
            assert_eq!(n, 1, "{content:?} は {n} カテゴリに属している（1であるべき）");
        }
        // 逆方向: カテゴリ側に未定義種や Empty が紛れ込んでいないこと
        let total: usize = ALL_CATEGORIES.iter().map(|c| c.items().len()).sum();
        assert_eq!(total, all.len());
    }
}
