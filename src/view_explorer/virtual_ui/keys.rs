//! 仮想ツリーのキー操作（フォーカスが VirtualTab の間）。実ツリー（handle_tree_keys）と同じ操作感:
//! 上下=カーソル移動（グリッドは変えない）、右=展開、左=折りたたみ／親へ、Enter=そのノードを開く、
//! 先頭（`/`）でさらに上=タブバーへ戻る。

use std::collections::{HashMap, HashSet};

use crate::keymap::ExplorerAction;

use super::*;

#[derive(Clone, Copy)]
pub(super) enum NavKey {
    Down,
    Up,
    Right,
    Left,
}

/// 1キー分のカーソル操作の結果。適用は呼び出し側が行う。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CursorStep {
    pub cursor: u32,
    pub expand: Option<u32>,
    pub collapse: Option<u32>,
    /// 先頭でさらに上: フォーカスをタブバーへ戻す
    pub to_tab_bar: bool,
}

/// 見えているノードのid（`/` を先頭に、展開中の枝だけを先行順に並べる）。
pub(super) fn visible_nodes(nodes: &[TreeNode], expanded: &HashSet<u32>) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for n in nodes {
        children.entry(n.parent).or_default().push(n.id);
    }
    let mut out = Vec::new();
    let mut stack = vec![ROOT];
    while let Some(id) = stack.pop() {
        out.push(id);
        if expanded.contains(&id) {
            if let Some(kids) = children.get(&id) {
                stack.extend(kids.iter().rev());
            }
        }
    }
    out
}

pub(super) fn step_cursor(
    nodes: &[TreeNode],
    expanded: &HashSet<u32>,
    cursor: u32,
    key: NavKey,
) -> CursorStep {
    let flat = visible_nodes(nodes, expanded);
    let pos = flat.iter().position(|&id| id == cursor).unwrap_or(0);
    let cur = flat[pos];
    let has_children = nodes.iter().any(|n| n.parent == cur);
    let mut step = CursorStep { cursor: cur, expand: None, collapse: None, to_tab_bar: false };
    match key {
        NavKey::Down => {
            if pos + 1 < flat.len() {
                step.cursor = flat[pos + 1];
            }
        }
        NavKey::Up => {
            if pos > 0 {
                step.cursor = flat[pos - 1];
            } else {
                step.to_tab_bar = true;
            }
        }
        NavKey::Right => {
            if has_children && !expanded.contains(&cur) {
                step.expand = Some(cur);
            }
        }
        NavKey::Left => {
            if has_children && expanded.contains(&cur) {
                step.collapse = Some(cur);
            } else if cur != ROOT {
                if let Some(n) = nodes.iter().find(|n| n.id == cur) {
                    step.cursor = n.parent;
                }
            }
        }
    }
    step
}

impl NekoviewApp {
    /// 仮想ツリーのカーソル位置（フォーカス中のリングと Enter の対象）。
    /// 見えていない位置（親が折りたたまれた等）なら、表示中のノード、無ければ `/` に落とす。
    pub(super) fn virtual_cursor(&self) -> u32 {
        let flat = visible_nodes(&self.virtual_state.nodes, &self.virtual_state.expanded);
        self.virtual_state
            .cursor
            .filter(|c| flat.contains(c))
            .or(self.viewing_virtual_node.filter(|c| flat.contains(c)))
            .unwrap_or(ROOT)
    }

    /// フォーカスが入った時点のカーソル位置は、表示中のノード（未表示なら `/`）。
    pub(in crate::view_explorer) fn reset_virtual_cursor(&mut self) {
        self.virtual_state.cursor = Some(self.viewing_virtual_node.unwrap_or(ROOT));
    }

    pub(in crate::view_explorer) fn handle_virtual_keys(&mut self, ctx: &egui::Context) {
        // 登録ピッカー・確認・大量登録・名前変更・削除ダイアログが開いている間は、背後のツリーを操作しない
        let vs = &self.virtual_state;
        if vs.picker.is_some()
            || vs.confirm.is_some()
            || vs.large_import.is_some()
            || vs.rename.is_some()
            || vs.delete.is_some()
        {
            return;
        }
        let km = &self.config.keymap;
        let (down, up, right, left, enter, rename) = ctx.input(|i| {
            (
                km.explorer_binding(ExplorerAction::NavDown).key_pressed(i),
                km.explorer_binding(ExplorerAction::NavUp).key_pressed(i),
                km.explorer_binding(ExplorerAction::NavRight).key_pressed(i),
                km.explorer_binding(ExplorerAction::NavLeft).key_pressed(i),
                km.explorer_binding(ExplorerAction::Confirm).key_pressed(i),
                km.explorer_binding(ExplorerAction::Rename).key_pressed(i),
            )
        });
        for (pressed, key) in [
            (down, NavKey::Down),
            (up, NavKey::Up),
            (right, NavKey::Right),
            (left, NavKey::Left),
        ] {
            if !pressed {
                continue;
            }
            let cur = self.virtual_cursor();
            let step = step_cursor(&self.virtual_state.nodes, &self.virtual_state.expanded, cur, key);
            if step.to_tab_bar {
                self.focused_pane = FocusPane::FolderTabBar;
                return;
            }
            self.virtual_state.cursor = Some(step.cursor);
            self.virtual_state.scroll_to_cursor = true;
            if let Some(id) = step.expand {
                self.virtual_state.expanded.insert(id);
            }
            if let Some(id) = step.collapse {
                self.virtual_state.expanded.remove(&id);
            }
        }
        if enter {
            let cur = self.virtual_cursor();
            self.select_virtual_node(cur);
        }
        // F2: カーソル行の名前変更（右クリックメニューと同じ入口。`/` は開かない）
        if rename {
            let cur = self.virtual_cursor();
            self.open_rename_dialog(cur);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u32, parent: u32) -> TreeNode {
        TreeNode { id, parent, name: format!("n{id}"), real: PathBuf::from(format!("/n{id}")) }
    }

    /// /
    /// ├ 1
    /// │ ├ 2
    /// │ └ 3
    /// └ 4
    fn sample() -> Vec<TreeNode> {
        vec![node(1, ROOT), node(2, 1), node(3, 1), node(4, ROOT)]
    }

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn visible_nodes_follow_expansion() {
        assert_eq!(visible_nodes(&sample(), &set(&[])), vec![ROOT]);
        assert_eq!(visible_nodes(&sample(), &set(&[ROOT])), vec![ROOT, 1, 4]);
        assert_eq!(visible_nodes(&sample(), &set(&[ROOT, 1])), vec![ROOT, 1, 2, 3, 4]);
        // 親が折りたたまれていれば、子が展開状態でも見えない
        assert_eq!(visible_nodes(&sample(), &set(&[ROOT, 2])), vec![ROOT, 1, 4]);
    }

    #[test]
    fn down_and_up_move_through_visible_nodes() {
        let exp = set(&[ROOT, 1]);
        assert_eq!(step_cursor(&sample(), &exp, ROOT, NavKey::Down).cursor, 1);
        assert_eq!(step_cursor(&sample(), &exp, 1, NavKey::Down).cursor, 2);
        assert_eq!(step_cursor(&sample(), &exp, 3, NavKey::Down).cursor, 4);
        assert_eq!(step_cursor(&sample(), &exp, 4, NavKey::Up).cursor, 3);
        // 末尾でさらに下: 動かない
        assert_eq!(step_cursor(&sample(), &exp, 4, NavKey::Down).cursor, 4);
    }

    #[test]
    fn up_at_top_returns_to_tab_bar() {
        let exp = set(&[ROOT]);
        let s = step_cursor(&sample(), &exp, ROOT, NavKey::Up);
        assert!(s.to_tab_bar);
        assert_eq!(s.cursor, ROOT);
        assert!(!step_cursor(&sample(), &exp, 1, NavKey::Up).to_tab_bar);
    }

    #[test]
    fn right_expands_collapsed_node_with_children_only() {
        let exp = set(&[ROOT]);
        assert_eq!(step_cursor(&sample(), &exp, 1, NavKey::Right).expand, Some(1));
        // 子のないノード・展開済みノードでは何も起きない
        assert_eq!(step_cursor(&sample(), &exp, 4, NavKey::Right).expand, None);
        let exp = set(&[ROOT, 1]);
        assert_eq!(step_cursor(&sample(), &exp, 1, NavKey::Right).expand, None);
    }

    #[test]
    fn left_collapses_then_moves_to_parent() {
        let exp = set(&[ROOT, 1]);
        let s = step_cursor(&sample(), &exp, 1, NavKey::Left);
        assert_eq!(s.collapse, Some(1));
        assert_eq!(s.cursor, 1);
        // 折りたたみ済み（または子なし）は親へ移る
        let s = step_cursor(&sample(), &exp, 2, NavKey::Left);
        assert_eq!(s.collapse, None);
        assert_eq!(s.cursor, 1);
        // ルートは展開中なら折りたたみ、折りたたみ済みなら何も起きない
        assert_eq!(step_cursor(&sample(), &exp, ROOT, NavKey::Left).collapse, Some(ROOT));
        let s = step_cursor(&sample(), &set(&[]), ROOT, NavKey::Left);
        assert_eq!((s.cursor, s.collapse), (ROOT, None));
    }

    #[test]
    fn unknown_cursor_falls_back_to_top() {
        let s = step_cursor(&sample(), &set(&[ROOT]), 99, NavKey::Down);
        assert_eq!(s.cursor, 1);
    }
}
