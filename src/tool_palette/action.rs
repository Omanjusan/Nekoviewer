//! ツールパレットのワンアクション型（Action）スロット定義。
//! Toggle/DialogとはことなりViewerConfigだけでは完結しない（ページ位置はViewerState側の
//! 状態のため）。ここではラベル・IDの定義のみを持ち、実行本体は view_reader.rs の
//! draw_tool_palette（ViewerStateのメソッド）側で直接 advance_page/retreat_page を呼ぶ。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionKind {
    /// 次の見開き/ページへ進む
    NextPage,
    /// 前の見開き/ページへ戻る
    PrevPage,
    /// 現ファイルが配置されているフォルダをOSのファイラーで開く
    OpenFolder,
    /// ビューアーウィンドウのフルスクリーン⇔ウィンドウモード切替
    ToggleFullscreen,
    /// スライドショーのON/OFF切替（右クリックメニュー等、他導線からの起動/停止状態も表示する）
    SlideshowToggle,
}

/// 全ActionKind。登録メニュー（マス右クリック）はこれを走査して選択肢を出す。
pub const ALL_ACTION_KINDS: [ActionKind; 5] = [
    ActionKind::NextPage,
    ActionKind::PrevPage,
    ActionKind::OpenFolder,
    ActionKind::ToggleFullscreen,
    ActionKind::SlideshowToggle,
];

impl ActionKind {
    /// 永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            ActionKind::NextPage => "next_page",
            ActionKind::PrevPage => "prev_page",
            ActionKind::OpenFolder => "open_folder",
            ActionKind::ToggleFullscreen => "toggle_fullscreen",
            ActionKind::SlideshowToggle => "slideshow_toggle",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "next_page" => ActionKind::NextPage,
            "prev_page" => ActionKind::PrevPage,
            "open_folder" => ActionKind::OpenFolder,
            "toggle_fullscreen" => ActionKind::ToggleFullscreen,
            "slideshow_toggle" => ActionKind::SlideshowToggle,
            _ => return None,
        })
    }

    /// マス上のデフォルト表示名。
    pub fn label(self, lang: crate::i18n::Lang) -> &'static str {
        match self {
            ActionKind::NextPage => lang.tool_palette_action_label_next_page(),
            ActionKind::PrevPage => lang.tool_palette_action_label_prev_page(),
            ActionKind::OpenFolder => lang.tool_palette_action_label_open_folder(),
            ActionKind::ToggleFullscreen => lang.tool_palette_action_label_toggle_fullscreen(),
            ActionKind::SlideshowToggle => lang.tool_palette_action_label_slideshow_toggle(),
        }
    }

    /// マス上に描くグリフ。
    pub fn glyph(self) -> &'static str {
        match self {
            ActionKind::NextPage => "▶",
            ActionKind::PrevPage => "◀",
            ActionKind::OpenFolder => "📁",
            ActionKind::ToggleFullscreen => "⛶",
            ActionKind::SlideshowToggle => "⏯",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [ActionKind; 5] = [
        ActionKind::NextPage,
        ActionKind::PrevPage,
        ActionKind::OpenFolder,
        ActionKind::ToggleFullscreen,
        ActionKind::SlideshowToggle,
    ];

    #[test]
    fn id_roundtrip() {
        for k in ALL_KINDS {
            assert_eq!(ActionKind::from_id(k.id()), Some(k));
        }
    }

    #[test]
    fn from_unknown_id_is_none() {
        assert_eq!(ActionKind::from_id("nonexistent"), None);
    }
}
