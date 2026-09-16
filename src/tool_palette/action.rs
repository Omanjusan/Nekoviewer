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
}

/// 全ActionKind。登録メニュー（マス右クリック）はこれを走査して選択肢を出す。
pub const ALL_ACTION_KINDS: [ActionKind; 3] = [ActionKind::NextPage, ActionKind::PrevPage, ActionKind::OpenFolder];

impl ActionKind {
    /// 永続化用ID。一度リリースしたIDは変更しない（前方互換の要）。
    pub fn id(self) -> &'static str {
        match self {
            ActionKind::NextPage => "next_page",
            ActionKind::PrevPage => "prev_page",
            ActionKind::OpenFolder => "open_folder",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "next_page" => ActionKind::NextPage,
            "prev_page" => ActionKind::PrevPage,
            "open_folder" => ActionKind::OpenFolder,
            _ => return None,
        })
    }

    /// マス上のデフォルト表示名。
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::NextPage => "次のページ",
            ActionKind::PrevPage => "前のページ",
            ActionKind::OpenFolder => "フォルダを開く",
        }
    }

    /// マス上に描くグリフ。
    pub fn glyph(self) -> &'static str {
        match self {
            ActionKind::NextPage => "▶",
            ActionKind::PrevPage => "◀",
            ActionKind::OpenFolder => "📁",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [ActionKind; 3] = [ActionKind::NextPage, ActionKind::PrevPage, ActionKind::OpenFolder];

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
