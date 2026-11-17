//! 「便利機能」展開メニュー（フォルダ本アクセス・スコアリング・ツールボックスのON/OFF）の文言。
//!
//! - 日本語・English・中文の3言語。固定の文言は `i18n::tr` を通す（言語トグルの「日中（テスト）」で併記される）
//! - 説明文は定数（[ja, en, zh]）にしてある。ヘルプ（`i18n::help_view_toggles`）も同じ定数を使い、文面を一本化する
//! - 呼び出し側は view_explorer/convenience_menu.rs

use crate::i18n;

/// メニューバーのボタン。ON/OFFの数は出さず、機能の数を固定で出す。
pub(crate) fn button_label() -> &'static str {
    i18n::tr("便利機能　全3種", "Extras: 3 features", "便利功能　共3项")
}

/// 展開メニューのタイトル。
pub(crate) fn menu_title() -> &'static str {
    i18n::tr("便利機能ON/OFF設定", "Convenience features ON/OFF", "便利功能 开/关 设置")
}

pub(crate) fn name_folder_book() -> &'static str {
    i18n::tr("フォルダ本アクセス", "Folder-as-Book", "文件夹当书")
}
pub(crate) fn name_scoring() -> &'static str {
    i18n::tr("スコアリング", "Scoring", "评分")
}
pub(crate) fn name_toolbox() -> &'static str {
    i18n::tr("ツールボックス", "Toolbox", "工具箱")
}

pub(crate) fn on_label() -> &'static str {
    i18n::tr("ON", "ON", "开")
}
pub(crate) fn off_label() -> &'static str {
    i18n::tr("OFF", "OFF", "关")
}

/// 閉じるボタンのホバー。日中（テスト）でも日本語。
pub(crate) fn close_hover() -> &'static str {
    i18n::as_ja(|| i18n::tr("閉じる", "Close", "关闭"))
}

/// 説明文（[ja, en, zh]）。
pub(crate) const DESC_FOLDER_BOOK: [&str; 3] = [
    "単体画像とサブフォルダだけのフォルダへ移動すると、そのままビューアーが開き、1冊のアーカイブとして閲覧できる。画像アーカイブファイルが1つでも混在しているフォルダでは機能しない。また、しおりやソートなど、実アーカイブファイルへの保存機能は使えない。",
    "Moving into a folder that contains only standalone images and subfolders opens the viewer right away, so you can read it as a single archive. It does not work in a folder that also contains even one image archive file. Saving features such as bookmarks and sort order, which write to a real archive file, are not available.",
    "进入仅包含独立图片和子文件夹的文件夹时，会直接打开查看器，作为一本压缩包阅读。只要文件夹中混有哪怕一个图片压缩包文件，此功能就不会生效。另外，书签、排序等需要写入真实压缩包文件的保存功能无法使用。",
];
pub(crate) const DESC_SCORING: [&str; 3] = [
    "アーカイブ末尾ページの評価オーバーレイと、ビューアーメニュー上の採点UIを出す。OFFにしてもエクスプローラー上の採点済み星マークまでは消さない。",
    "Shows the rating overlay on the last page of an archive and the scoring controls on the viewer menu. Turning it OFF does not remove the star marks of already-scored files in the explorer.",
    "显示压缩包末页的评分浮层，以及查看器菜单上的评分控件。关闭后，资源管理器中已评分文件的星标不会消失。",
];
pub(crate) const DESC_TOOLBOX: [&str; 3] = [
    "ビューアー内のツールパレット（マス配置のツールボックス）を出す。登録ボタンを設定した上でさらにキーアサイン設定も可能。",
    "Shows the tool palette (the grid-layout toolbox) in the viewer. After you register buttons, you can also assign keyboard keys to them.",
    "显示查看器内的工具面板（格子布局的工具箱）。设置好登记的按钮后，还可以进一步设置键位分配。",
];

pub(crate) fn desc_folder_book() -> &'static str {
    i18n::tr(DESC_FOLDER_BOOK[0], DESC_FOLDER_BOOK[1], DESC_FOLDER_BOOK[2])
}
pub(crate) fn desc_scoring() -> &'static str {
    i18n::tr(DESC_SCORING[0], DESC_SCORING[1], DESC_SCORING[2])
}
pub(crate) fn desc_toolbox() -> &'static str {
    i18n::tr(DESC_TOOLBOX[0], DESC_TOOLBOX[1], DESC_TOOLBOX[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_display, Display};

    fn all(d: Display) -> Vec<&'static str> {
        with_display(d, || {
            vec![
                button_label(), menu_title(), name_folder_book(), name_scoring(), name_toolbox(),
                on_label(), off_label(), desc_folder_book(), desc_scoring(), desc_toolbox(),
            ]
        })
    }

    #[test]
    fn every_text_is_filled_in_all_three_languages() {
        for d in [Display::Ja, Display::En, Display::Zh] {
            for s in all(d) {
                assert!(!s.trim().is_empty());
            }
        }
        // 3言語で、同じ文面のままの文言（訳し忘れ）が無いこと。ON/OFF の2語は、日英で同じでよい
        let (ja, en, zh) = (all(Display::Ja), all(Display::En), all(Display::Zh));
        for i in 0..ja.len() {
            if i == 5 || i == 6 { continue; }
            assert_ne!(ja[i], en[i], "日英が同じ: {}", ja[i]);
            assert_ne!(ja[i], zh[i], "日中が同じ: {}", ja[i]);
        }
    }

    #[test]
    fn chinese_text_has_no_kana() {
        let has_kana = |s: &str| s.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c));
        for s in all(Display::Zh) {
            assert!(!has_kana(s), "中文に仮名が混ざっている: {s}");
        }
    }

    #[test]
    fn close_hover_is_japanese_in_the_test_display() {
        assert_eq!(with_display(Display::JaZh, close_hover), "閉じる");
        assert_eq!(with_display(Display::En, close_hover), "Close");
        assert_eq!(with_display(Display::Zh, close_hover), "关闭");
    }
}
