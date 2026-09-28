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
    /// 疑似コマ送り: 次のコマへ進む（最終コマなら次のページ）
    KomaNext,
    /// 疑似コマ送り: 前のコマへ戻る（先頭コマなら前ページの最終コマ）
    KomaPrev,
    /// 前のファイルへ移動
    FileNavPrev,
    /// 次のファイルへ移動
    FileNavNext,
    /// アーカイブ内先頭ページへジャンプ
    JumpFirstPage,
    /// アーカイブ内末尾ページへジャンプ
    JumpLastPage,
    /// 等倍/fit表示切替
    ToggleZoomActual,
    /// ページ表示モードを循環（単ページ→見開き左→見開き右→単ページ…）
    CyclePageMode,
    /// 見開きオフセットを往復循環（-1→0→+1→0→-1…、境界では往復方向を反転）
    CycleSpreadOffset,
}

/// 全ActionKind。category.rsの網羅テストがこれを基準に「全種がどこかのカテゴリに属する」を検査する
/// （登録メニューはcategory.rsのカテゴリ表を走査する）。
#[cfg(test)]
pub const ALL_ACTION_KINDS: [ActionKind; 14] = [
    ActionKind::NextPage,
    ActionKind::PrevPage,
    ActionKind::OpenFolder,
    ActionKind::ToggleFullscreen,
    ActionKind::SlideshowToggle,
    ActionKind::KomaNext,
    ActionKind::KomaPrev,
    ActionKind::FileNavPrev,
    ActionKind::FileNavNext,
    ActionKind::JumpFirstPage,
    ActionKind::JumpLastPage,
    ActionKind::ToggleZoomActual,
    ActionKind::CyclePageMode,
    ActionKind::CycleSpreadOffset,
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
            ActionKind::KomaNext => "koma_next",
            ActionKind::KomaPrev => "koma_prev",
            ActionKind::FileNavPrev => "file_nav_prev",
            ActionKind::FileNavNext => "file_nav_next",
            ActionKind::JumpFirstPage => "jump_first_page",
            ActionKind::JumpLastPage => "jump_last_page",
            ActionKind::ToggleZoomActual => "toggle_zoom_actual",
            ActionKind::CyclePageMode => "cycle_page_mode",
            ActionKind::CycleSpreadOffset => "cycle_spread_offset",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "next_page" => ActionKind::NextPage,
            "prev_page" => ActionKind::PrevPage,
            "open_folder" => ActionKind::OpenFolder,
            "toggle_fullscreen" => ActionKind::ToggleFullscreen,
            "slideshow_toggle" => ActionKind::SlideshowToggle,
            "koma_next" => ActionKind::KomaNext,
            "koma_prev" => ActionKind::KomaPrev,
            "file_nav_prev" => ActionKind::FileNavPrev,
            "file_nav_next" => ActionKind::FileNavNext,
            "jump_first_page" => ActionKind::JumpFirstPage,
            "jump_last_page" => ActionKind::JumpLastPage,
            "toggle_zoom_actual" => ActionKind::ToggleZoomActual,
            "cycle_page_mode" => ActionKind::CyclePageMode,
            "cycle_spread_offset" => ActionKind::CycleSpreadOffset,
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
            ActionKind::KomaNext => lang.tool_palette_action_label_koma_next(),
            ActionKind::KomaPrev => lang.tool_palette_action_label_koma_prev(),
            ActionKind::FileNavPrev => lang.tool_palette_action_label_file_nav_prev(),
            ActionKind::FileNavNext => lang.tool_palette_action_label_file_nav_next(),
            ActionKind::JumpFirstPage => lang.tool_palette_action_label_jump_first_page(),
            ActionKind::JumpLastPage => lang.tool_palette_action_label_jump_last_page(),
            ActionKind::ToggleZoomActual => lang.tool_palette_action_label_toggle_zoom_actual(),
            ActionKind::CyclePageMode => lang.tool_palette_action_label_cycle_page_mode(),
            ActionKind::CycleSpreadOffset => lang.tool_palette_action_label_cycle_spread_offset(),
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
            ActionKind::KomaNext => "▶▶",
            ActionKind::KomaPrev => "◀◀",
            ActionKind::FileNavPrev => "⏮",
            ActionKind::FileNavNext => "⏭",
            ActionKind::JumpFirstPage => "⇤",
            ActionKind::JumpLastPage => "⇥",
            ActionKind::ToggleZoomActual => "🔍",
            ActionKind::CyclePageMode => "⇄",
            ActionKind::CycleSpreadOffset => "↔",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [ActionKind; 14] = [
        ActionKind::NextPage,
        ActionKind::PrevPage,
        ActionKind::OpenFolder,
        ActionKind::ToggleFullscreen,
        ActionKind::SlideshowToggle,
        ActionKind::KomaNext,
        ActionKind::KomaPrev,
        ActionKind::FileNavPrev,
        ActionKind::FileNavNext,
        ActionKind::JumpFirstPage,
        ActionKind::JumpLastPage,
        ActionKind::ToggleZoomActual,
        ActionKind::CyclePageMode,
        ActionKind::CycleSpreadOffset,
    ];

    #[test]
    fn id_roundtrip() {
        for k in ALL_KINDS {
            assert_eq!(ActionKind::from_id(k.id()), Some(k));
        }
    }

    #[test]
    fn all_action_kinds_lists_every_kind() {
        for k in ALL_KINDS {
            assert!(ALL_ACTION_KINDS.contains(&k), "missing in ALL_ACTION_KINDS: {k:?}");
        }
    }

    #[test]
    fn from_unknown_id_is_none() {
        assert_eq!(ActionKind::from_id("nonexistent"), None);
    }
}
