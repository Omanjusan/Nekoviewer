//! アーカイブ一覧の選択状態の規則（クリック選択・Ctrl トグル）。描画から独立した純粋ロジック。
//!
//! 状態は2つ: 単一選択 `selected`（カーソル兼用）と複数選択 `multi`。
//! 複数選択が空でないときは `multi` が選択の全て、空のときは `selected` が唯一の選択を表す。
//! `multi` の要素が1つだけの状態は持たない（1つになったら `selected` へ畳む）。

use std::collections::HashSet;

/// `idx` のカードが選択中として描かれるか（青枠を付けるか）。
/// 複数選択中は `selected` を見ない。Ctrl で外した直後の項目が `selected` に残っていても枠を付けないため。
pub(super) fn is_selected(multi: &HashSet<usize>, selected: Option<usize>, idx: usize) -> bool {
    if multi.is_empty() {
        selected == Some(idx)
    } else {
        multi.contains(&idx)
    }
}

/// Ctrl(Cmd)+クリック: `clicked` を選択に加える／選択から外す。
/// 単一選択のみの状態から始めたときは、その項目も複数選択へ取り込んでからトグルする。
pub(super) fn toggle(multi: &mut HashSet<usize>, selected: &mut Option<usize>, clicked: usize) {
    if multi.is_empty()
        && let Some(prev) = *selected
    {
        multi.insert(prev);
    }
    let added = multi.insert(clicked);
    if !added {
        multi.remove(&clicked);
    }
    match multi.len() {
        0 => *selected = None,
        1 => {
            *selected = multi.iter().next().copied();
            multi.clear();
        }
        // 外した項目を `selected`（キーボード操作の起点）に残さず、残った項目の1つへ移す。
        _ => *selected = if added { Some(clicked) } else { multi.iter().max().copied() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[usize]) -> HashSet<usize> {
        items.iter().copied().collect()
    }

    #[test]
    fn single_selection_is_framed_only_when_no_multi_selection() {
        assert!(is_selected(&set(&[]), Some(3), 3));
        assert!(!is_selected(&set(&[]), Some(3), 4));
        assert!(!is_selected(&set(&[]), None, 3));
    }

    #[test]
    fn multi_selection_ignores_the_single_selection_marker() {
        // Ctrl で外した項目 3 が selected に残っていても枠は付かない
        assert!(!is_selected(&set(&[1, 2]), Some(3), 3));
        assert!(is_selected(&set(&[1, 2]), Some(3), 1));
    }

    #[test]
    fn ctrl_click_on_nothing_selects_one() {
        let (mut multi, mut sel) = (set(&[]), None);
        toggle(&mut multi, &mut sel, 5);
        assert_eq!((multi, sel), (set(&[]), Some(5)));
    }

    #[test]
    fn ctrl_click_on_another_item_makes_a_multi_selection() {
        let (mut multi, mut sel) = (set(&[]), Some(2));
        toggle(&mut multi, &mut sel, 5);
        assert_eq!(multi, set(&[2, 5]));
        assert_eq!(sel, Some(5));
        assert!(is_selected(&multi, sel, 2) && is_selected(&multi, sel, 5));
    }

    #[test]
    fn ctrl_click_on_the_only_selected_item_clears_the_selection() {
        let (mut multi, mut sel) = (set(&[]), Some(2));
        toggle(&mut multi, &mut sel, 2);
        assert_eq!((multi, sel), (set(&[]), None));
    }

    #[test]
    fn ctrl_click_removing_down_to_one_keeps_the_remaining_item_framed() {
        let (mut multi, mut sel) = (set(&[2, 5]), Some(5));
        toggle(&mut multi, &mut sel, 5);
        assert_eq!((multi.clone(), sel), (set(&[]), Some(2)));
        assert!(is_selected(&multi, sel, 2));
        assert!(!is_selected(&multi, sel, 5), "外した項目に枠が残らない");
    }

    #[test]
    fn ctrl_click_removing_from_three_leaves_two_and_no_frame_on_the_removed() {
        let (mut multi, mut sel) = (set(&[1, 2, 3]), Some(3));
        toggle(&mut multi, &mut sel, 3);
        assert_eq!(multi, set(&[1, 2]));
        assert!(!is_selected(&multi, sel, 3));
        assert!(sel.is_some_and(|s| multi.contains(&s)), "selected は残った項目のどれか");
    }
}
