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

/// Shift+クリック: 起点から `clicked_pos`（`visible` 上の位置）までの範囲。
/// 起点が無い／表示中に無い場合は、クリックした項目だけが起点になる。
pub(super) fn shift_range<T>(visible: &[(usize, T)], anchor: Option<usize>, clicked_pos: usize) -> HashSet<usize> {
    let anchor_pos = anchor
        .and_then(|a| visible.iter().position(|(idx, _)| *idx == a))
        .unwrap_or(clicked_pos);
    let (from, to) = if anchor_pos <= clicked_pos { (anchor_pos, clicked_pos) } else { (clicked_pos, anchor_pos) };
    visible[from..=to].iter().map(|(idx, _)| *idx).collect()
}

/// ファイル行を Shift+クリックするときの、範囲選択の起点。
/// カーソル（選択マーカー）がフォルダ（「↑」・サブフォルダ）にあるときは、フォルダが始点で、フォルダは
/// 複数選択の対象外なので最も近い有効な行（先頭のファイル）に置き換える。それ以外は既存の起点。
pub(super) fn shift_start(anchor: Option<usize>, cursor_on_folder: bool, first_visible: Option<usize>) -> Option<usize> {
    if cursor_on_folder { first_visible } else { anchor }
}

/// 範囲選択の起点のうち、いま実際に選択されている行のものだけを有効とみなす。
/// 画面上に選択が無い（フォルダのクリックで選択が消えた等）のに起点だけが古く残っていると、
/// ユーザーには見えない起点から範囲が引かれてしまうため。
pub(super) fn live_anchor(anchor: Option<usize>, multi: &HashSet<usize>, selected: Option<usize>) -> Option<usize> {
    anchor.filter(|a| selected == Some(*a) || multi.contains(a))
}

/// フォルダ（「↑」・サブフォルダ）を Shift+クリックしたときの範囲選択。
/// フォルダは複数選択の対象外で、必ずファイルより前に並ぶ。そこで「有効な行より手前をクリックしたら、
/// 最も近い有効な行（先頭のファイル）をクリックしたものとして扱う」。
/// `anchor` は `live_anchor` を通した有効な起点（無ければ None）。返すのは（新しい複数選択, 新しい起点）。
/// 起点が無い／表示中に無いときは先頭のファイルが起点になる（以後のShift+クリックがそこからの範囲になる）。
/// 表示中のファイルが無ければ None（何もしない）。
pub(super) fn shift_click_folder(filtered: &[usize], anchor: Option<usize>) -> Option<(HashSet<usize>, usize)> {
    let first = *filtered.first()?;
    let anchor = anchor.filter(|a| filtered.contains(a)).unwrap_or(first);
    let anchor_pos = filtered.iter().position(|&i| i == anchor)?;
    Some((filtered[..=anchor_pos].iter().copied().collect(), anchor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[usize]) -> HashSet<usize> {
        items.iter().copied().collect()
    }

    fn rows(n: usize) -> Vec<(usize, ())> {
        (0..n).map(|i| (i, ())).collect()
    }

    #[test]
    fn shift_range_spans_anchor_to_clicked_in_either_direction() {
        assert_eq!(shift_range(&rows(6), Some(1), 4), set(&[1, 2, 3, 4]));
        assert_eq!(shift_range(&rows(6), Some(4), 1), set(&[1, 2, 3, 4]));
    }

    #[test]
    fn shift_range_without_a_usable_anchor_selects_just_the_clicked_item() {
        assert_eq!(shift_range(&rows(6), None, 3), set(&[3]));
        assert_eq!(shift_range(&rows(6), Some(99), 3), set(&[3]), "表示中に無い起点は無視");
    }

    #[test]
    fn shift_range_uses_positions_in_the_filtered_view_not_raw_indices() {
        // フィルタで 10, 20, 30, 40 だけが見えている
        let visible: Vec<(usize, ())> = [10, 20, 30, 40].iter().map(|&i| (i, ())).collect();
        // 40 は表示上の4番目（位置3）。起点 20 は生のインデックスで渡し、位置に変換して使う。
        assert_eq!(shift_range(&visible, Some(20), 3), set(&[20, 30, 40]));
    }

    #[test]
    fn shift_clicking_a_folder_clamps_to_the_nearest_file_row() {
        // 5番目（インデックス4）を選択済みで、その上に↑がある。↑を Shift+クリック → 先頭のファイルが終端になり
        // 先頭〜5番目が範囲選択される（↑自体は終端にならない）。起点は変わらない。
        let filtered = [0, 1, 2, 3, 4, 5];
        let (multi, anchor) = shift_click_folder(&filtered, Some(4)).unwrap();
        assert_eq!(multi, set(&[0, 1, 2, 3, 4]));
        assert_eq!(anchor, 4);
    }

    #[test]
    fn shift_clicking_a_folder_without_an_anchor_starts_at_the_first_file() {
        // ↑ を Shift+クリック → 4番を Shift+クリック で 1〜4番になる。
        let filtered = [0, 1, 2, 3, 4];
        let (multi, anchor) = shift_click_folder(&filtered, None).unwrap();
        assert_eq!((multi, anchor), (set(&[0]), 0));
        let visible = rows(5);
        assert_eq!(shift_range(&visible, Some(anchor), 3), set(&[0, 1, 2, 3]));
    }

    #[test]
    fn shift_click_on_a_folder_ignores_an_anchor_that_is_not_visible() {
        let (multi, anchor) = shift_click_folder(&[10, 20, 30], Some(99)).unwrap();
        assert_eq!((multi, anchor), (set(&[10]), 10));
    }

    #[test]
    fn shift_click_on_a_folder_uses_filtered_positions() {
        // フィルタで 10, 20, 30, 40 だけが見えていて、起点が 30
        let (multi, anchor) = shift_click_folder(&[10, 20, 30, 40], Some(30)).unwrap();
        assert_eq!(multi, set(&[10, 20, 30]));
        assert_eq!(anchor, 30);
    }

    #[test]
    fn shift_click_on_a_folder_does_nothing_when_there_are_no_files() {
        assert_eq!(shift_click_folder(&[], Some(1)), None);
        assert_eq!(shift_click_folder(&[], None), None);
    }

    #[test]
    fn shift_click_on_a_file_with_the_cursor_on_a_folder_starts_from_the_first_file() {
        // ↑（カーソル）の下の4番を Shift+クリック。古い起点（3番=インデックス2）があっても先頭から。
        assert_eq!(shift_start(Some(2), true, Some(0)), Some(0));
        assert_eq!(shift_start(None, true, Some(0)), Some(0));
        let start = shift_start(Some(2), true, Some(0));
        assert_eq!(shift_range(&rows(5), start, 3), set(&[0, 1, 2, 3]));
    }

    #[test]
    fn shift_start_keeps_the_anchor_when_the_cursor_is_on_a_file() {
        assert_eq!(shift_start(Some(2), false, Some(0)), Some(2));
        assert_eq!(shift_start(None, false, Some(0)), None);
    }

    #[test]
    fn shift_start_is_none_when_there_are_no_visible_files() {
        assert_eq!(shift_start(Some(2), true, None), None);
    }

    #[test]
    fn live_anchor_requires_the_anchor_row_to_be_selected() {
        assert_eq!(live_anchor(Some(3), &set(&[]), Some(3)), Some(3), "単一選択の行");
        assert_eq!(live_anchor(Some(3), &set(&[1, 3]), None), Some(3), "複数選択の中の行");
        assert_eq!(live_anchor(Some(3), &set(&[]), None), None, "選択が消えたのに残った古い起点");
        assert_eq!(live_anchor(Some(3), &set(&[1, 2]), Some(2)), None, "選択されていない行");
        assert_eq!(live_anchor(None, &set(&[1]), Some(1)), None);
    }

    #[test]
    fn stale_anchor_after_clicking_a_folder_does_not_hijack_the_range() {
        // 3番をクリック → ↑を通常クリック（選択が消える）→ ↑を Shift+クリック → 4番を Shift+クリック。
        // 古い起点（3番）を使うと 3〜4番だけになるが、有効な起点だけを使えば 1〜4番になる。
        let filtered = [0, 1, 2, 3, 4];
        let stale = Some(2);
        let live = live_anchor(stale, &set(&[]), None);
        assert_eq!(live, None);
        let (multi, anchor) = shift_click_folder(&filtered, live).unwrap();
        assert_eq!((multi, anchor), (set(&[0]), 0));
        assert_eq!(shift_range(&rows(5), Some(anchor), 3), set(&[0, 1, 2, 3]));
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
