//! エクスプローラー中央部の表示モード（カード／リスト／一覧）。
//!
//! - カード: 従来のサムネグリッド
//! - リスト: 1行 = サムネ＋各情報。行の高さはサムネ設定に対する S / M / L、または行は文字のみでサムネを
//!   ホバーのポップアップに出す「ホバー」
//! - 一覧: リストからサムネを除いた文字だけの行
//!
//! 全タブ共通で1つ。リストのサイズはカード／一覧にいる間も覚えておき、リストへ戻ったときに復元する。
//! 保存は `nekoviewer.state` の `explorer_view_mode`（`"<mode>:<size>"`、例 `list:m`）。

/// 一覧モードの有効/封印。一覧の仕様が未確定のため封印中（ボタンは残して無反応）。
/// 仕様が固まったら true にして復活させる。描画の実装（`list_view.rs`）は残してある。
pub(crate) const TEXT_MODE_ENABLED: bool = false;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ViewMode {
    #[default]
    Card,
    List,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum ListSize {
    #[default]
    S,
    M,
    L,
    /// 行は文字のみ。サムネはホバーのポップアップに出す。
    Hover,
}

impl ListSize {
    /// 押下ごとの循環順: S → M → L → Hover → S
    pub(crate) fn next(self) -> Self {
        match self {
            Self::S => Self::M,
            Self::M => Self::L,
            Self::L => Self::Hover,
            Self::Hover => Self::S,
        }
    }

    fn as_state_str(self) -> &'static str {
        match self {
            Self::S => "s",
            Self::M => "m",
            Self::L => "l",
            Self::Hover => "hover",
        }
    }

    fn from_state_str(s: &str) -> Self {
        match s {
            "m" => Self::M,
            "l" => Self::L,
            "hover" => Self::Hover,
            _ => Self::S,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ExplorerViewMode {
    pub mode: ViewMode,
    /// 最後に使ったリストのサイズ（リスト以外のモードにいる間も保持する）
    pub list_size: ListSize,
}

impl ExplorerViewMode {
    pub(crate) fn select_card(&mut self) {
        self.mode = ViewMode::Card;
    }

    /// 一覧ボタンの押下。封印中（`TEXT_MODE_ENABLED` が false）は何もしない。
    pub(crate) fn select_text(&mut self) {
        if TEXT_MODE_ENABLED {
            self.mode = ViewMode::Text;
        }
    }

    /// リストボタンの押下。別のモードからはリストへ入り（サイズは前回のまま）、
    /// 既にリストなら S → M → L → Hover と循環する。
    pub(crate) fn press_list(&mut self) {
        if self.mode == ViewMode::List {
            self.list_size = self.list_size.next();
        } else {
            self.mode = ViewMode::List;
        }
    }

    pub(crate) fn as_state_str(self) -> String {
        let mode = match self.mode {
            ViewMode::Card => "card",
            ViewMode::List => "list",
            ViewMode::Text => "text",
        };
        format!("{mode}:{}", self.list_size.as_state_str())
    }

    /// 未知・欠落は既定（カード・S）。一覧は封印中はカードに戻す（以前の保存値で封印中の画面に入らないように）。
    pub(crate) fn from_state_str(s: &str) -> Self {
        let (mode, size) = s.split_once(':').unwrap_or((s, ""));
        Self {
            mode: match mode {
                "list" => ViewMode::List,
                "text" if TEXT_MODE_ENABLED => ViewMode::Text,
                _ => ViewMode::Card,
            },
            list_size: ListSize::from_state_str(size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_size_cycles_s_m_l_hover_and_returns_to_s() {
        let mut s = ListSize::S;
        let mut seen = Vec::new();
        for _ in 0..4 {
            s = s.next();
            seen.push(s);
        }
        assert_eq!(seen, vec![ListSize::M, ListSize::L, ListSize::Hover, ListSize::S]);
    }

    #[test]
    fn pressing_list_from_another_mode_enters_with_the_remembered_size() {
        let mut m = ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::L };
        m.press_list();
        assert_eq!(m, ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::L });
        m.press_list();
        assert_eq!(m.list_size, ListSize::Hover, "リストにいる間の押下はサイズを循環する");
        m.select_card();
        assert_eq!(m.list_size, ListSize::Hover, "カードにいる間もサイズを覚える");
        m.press_list();
        assert_eq!(m, ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::Hover });
        m.select_card();
        assert_eq!(m.mode, ViewMode::Card);
    }

    #[test]
    fn state_string_round_trips_for_every_combination() {
        for mode in [ViewMode::Card, ViewMode::List] {
            for list_size in [ListSize::S, ListSize::M, ListSize::L, ListSize::Hover] {
                let v = ExplorerViewMode { mode, list_size };
                assert_eq!(ExplorerViewMode::from_state_str(&v.as_state_str()), v);
            }
        }
    }

    #[test]
    fn unknown_or_missing_state_falls_back_to_card_s() {
        let default = ExplorerViewMode::default();
        assert_eq!(default, ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::S });
        assert_eq!(ExplorerViewMode::from_state_str(""), default);
        assert_eq!(ExplorerViewMode::from_state_str("bogus"), default);
        assert_eq!(ExplorerViewMode::from_state_str("bogus:zzz"), default);
        // サイズだけ壊れていてもモードは活かす
        assert_eq!(
            ExplorerViewMode::from_state_str("list:zzz"),
            ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::S }
        );
    }

    #[test]
    fn text_mode_is_sealed_while_its_spec_is_undecided() {
        assert!(!TEXT_MODE_ENABLED, "封印を解くときはこのテストと一緒に仕様を確認する");
        // 一覧ボタンは無反応
        let mut m = ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::M };
        m.select_text();
        assert_eq!(m.mode, ViewMode::Card);
        let mut m = ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::L };
        m.select_text();
        assert_eq!(m.mode, ViewMode::List, "リストにいても切り替わらない");
        // 以前に保存された一覧はカードへ戻す（リストのサイズは残す）
        assert_eq!(
            ExplorerViewMode::from_state_str("text:m"),
            ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::M }
        );
    }
}
