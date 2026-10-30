//! エクスプローラー中央部の表示モード（カード／リスト／一覧）。
//!
//! - カード: 従来のサムネグリッド
//! - リスト: 1行 = サムネ＋各情報。行の高さはサムネ設定に対する S / M / L、またはサムネ列なしの文字のみの行
//!   （サムネ無し）。ホバーでサムネをポップアップに出すかは、サイズとは別のチェックボックス
//! - 一覧: リストからサムネを除いた文字だけの行
//!
//! 全タブ共通で1つ。リストのサイズはカード／一覧にいる間も覚えておき、リストへ戻ったときに復元する。
//! 保存は `nekoviewer.state` の `explorer_view_mode`（`"<mode>:<size>:<ホバー>"`、例 `list:m:0`）。
//! 旧形式 `"<mode>:<size>"` も読む。旧 `hover` サイズは「サムネ無し＋ホバーON」に読み替える（見た目を保つ）。

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
    /// サムネ列なし。行は文字のみ。
    NoThumb,
}

impl ListSize {
    /// 押下ごとの循環順: S → M → L → サムネ無し → S
    pub(crate) fn next(self) -> Self {
        match self {
            Self::S => Self::M,
            Self::M => Self::L,
            Self::L => Self::NoThumb,
            Self::NoThumb => Self::S,
        }
    }

    fn as_state_str(self) -> &'static str {
        match self {
            Self::S => "s",
            Self::M => "m",
            Self::L => "l",
            Self::NoThumb => "none",
        }
    }

    /// 旧形式の `hover` は呼び出し側（`ExplorerViewMode::from_state_str`）で先に読み替える。
    fn from_state_str(s: &str) -> Self {
        match s {
            "m" => Self::M,
            "l" => Self::L,
            "none" => Self::NoThumb,
            _ => Self::S,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ExplorerViewMode {
    pub mode: ViewMode,
    /// 最後に使ったリストのサイズ（リスト以外のモードにいる間も保持する）
    pub list_size: ListSize,
    /// 行にホバーしたときにサムネをポップアップに出すか（サイズとは独立）
    pub hover_popup: bool,
}

impl ExplorerViewMode {
    pub(crate) fn select_card(&mut self) {
        self.mode = ViewMode::Card;
    }

    /// 一覧ボタンの押下。封印中（`TEXT_MODE_ENABLED` が false）は何もしない。
    #[allow(dead_code)] // 一覧ボタンはUIから外した。封印中のコードと一緒に残す
    pub(crate) fn select_text(&mut self) {
        if TEXT_MODE_ENABLED {
            self.mode = ViewMode::Text;
        }
    }

    /// リストボタンの押下。別のモードからはリストへ入り（サイズは前回のまま）、
    /// 既にリストなら S → M → L → サムネ無し と循環する。
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
        format!("{mode}:{}:{}", self.list_size.as_state_str(), u8::from(self.hover_popup))
    }

    /// 未知・欠落は既定（カード・S・ホバーOFF）。一覧は封印中はカードに戻す（以前の保存値で封印中の画面に入らないように）。
    pub(crate) fn from_state_str(s: &str) -> Self {
        let mut parts = s.split(':');
        let mode = parts.next().unwrap_or("");
        let size = parts.next().unwrap_or("");
        let popup = parts.next().unwrap_or("");
        // 旧形式の hover は「サムネ無し＋ホバーON」に読み替える
        let (list_size, hover_popup) = if size == "hover" {
            (ListSize::NoThumb, true)
        } else {
            (ListSize::from_state_str(size), popup == "1")
        };
        Self {
            mode: match mode {
                "list" => ViewMode::List,
                "text" if TEXT_MODE_ENABLED => ViewMode::Text,
                _ => ViewMode::Card,
            },
            list_size,
            hover_popup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_size_cycles_s_m_l_nothumb_and_returns_to_s() {
        let mut s = ListSize::S;
        let mut seen = Vec::new();
        for _ in 0..4 {
            s = s.next();
            seen.push(s);
        }
        assert_eq!(seen, vec![ListSize::M, ListSize::L, ListSize::NoThumb, ListSize::S]);
    }

    #[test]
    fn pressing_list_from_another_mode_enters_with_the_remembered_size() {
        let mut m = ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::L, hover_popup: false };
        m.press_list();
        assert_eq!(m, ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::L, hover_popup: false });
        m.press_list();
        assert_eq!(m.list_size, ListSize::NoThumb, "リストにいる間の押下はサイズを循環する");
        m.select_card();
        assert_eq!(m.list_size, ListSize::NoThumb, "カードにいる間もサイズを覚える");
        m.press_list();
        assert_eq!(m, ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::NoThumb, hover_popup: false });
        m.select_card();
        assert_eq!(m.mode, ViewMode::Card);
    }

    #[test]
    fn state_string_round_trips_for_every_combination() {
        for mode in [ViewMode::Card, ViewMode::List] {
            for list_size in [ListSize::S, ListSize::M, ListSize::L, ListSize::NoThumb] {
                for hover_popup in [false, true] {
                    let v = ExplorerViewMode { mode, list_size, hover_popup };
                    assert_eq!(ExplorerViewMode::from_state_str(&v.as_state_str()), v);
                }
            }
        }
    }

    #[test]
    fn legacy_two_part_state_is_read_with_popup_off_and_legacy_hover_becomes_nothumb_with_popup_on() {
        assert_eq!(
            ExplorerViewMode::from_state_str("list:m"),
            ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::M, hover_popup: false }
        );
        assert_eq!(
            ExplorerViewMode::from_state_str("list:hover"),
            ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::NoThumb, hover_popup: true }
        );
        assert_eq!(ExplorerViewMode::from_state_str("list:l:1").hover_popup, true);
        assert_eq!(ExplorerViewMode::from_state_str("list:l:x").hover_popup, false);
    }

    #[test]
    fn unknown_or_missing_state_falls_back_to_card_s() {
        let default = ExplorerViewMode::default();
        assert_eq!(default, ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::S, hover_popup: false });
        assert_eq!(ExplorerViewMode::from_state_str(""), default);
        assert_eq!(ExplorerViewMode::from_state_str("bogus"), default);
        assert_eq!(ExplorerViewMode::from_state_str("bogus:zzz"), default);
        // サイズだけ壊れていてもモードは活かす
        assert_eq!(
            ExplorerViewMode::from_state_str("list:zzz"),
            ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::S, hover_popup: false }
        );
    }

    #[test]
    fn text_mode_is_sealed_while_its_spec_is_undecided() {
        assert!(!TEXT_MODE_ENABLED, "封印を解くときはこのテストと一緒に仕様を確認する");
        // 一覧ボタンは無反応
        let mut m = ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::M, hover_popup: false };
        m.select_text();
        assert_eq!(m.mode, ViewMode::Card);
        let mut m = ExplorerViewMode { mode: ViewMode::List, list_size: ListSize::L, hover_popup: false };
        m.select_text();
        assert_eq!(m.mode, ViewMode::List, "リストにいても切り替わらない");
        // 以前に保存された一覧はカードへ戻す（リストのサイズは残す）
        assert_eq!(
            ExplorerViewMode::from_state_str("text:m"),
            ExplorerViewMode { mode: ViewMode::Card, list_size: ListSize::M, hover_popup: false }
        );
    }
}
