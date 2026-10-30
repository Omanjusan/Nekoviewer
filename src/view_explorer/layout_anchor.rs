//! 表示のレイアウトが変わったとき（カード↔リスト、リストのサイズ、サムネ設定）に、見ていた位置を保つための
//! スクロール補正。純粋ロジック。
//!
//! 列数だけが変わる補正は `grid_anchor.rs`（カード内）が担う。こちらは行の高さ・並べ方そのものが変わる場合で、
//! 毎フレーム末尾に「そのときのレイアウトと見え方」を覚えておき、次のフレームでレイアウトが変わっていたら、
//! 覚えていた基準の項目が同じ画面上の高さに来るように新しいオフセットを決める。
//!
//! 基準の選び方（`grid_anchor.rs` と同じ）:
//! - 選択（カーソル）項目が一部でも見えていれば、それを同じ高さに保つ
//! - それ以外は、ビューポート上端の行の先頭の項目を基準にする

/// レイアウトの形。`pitch` は行の送り幅（項目の高さ＋隙間）、`item_h` は項目の高さ。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geometry {
    pub cols: usize,
    pub item_h: f32,
    pub pitch: f32,
    /// リスト・一覧か（カードと行の送り幅が偶然同じでも、別のレイアウトとして扱うため）
    pub is_list: bool,
}

impl Geometry {
    fn item_top(&self, index: usize) -> f32 {
        (index / self.cols.max(1)) as f32 * self.pitch
    }
}

/// 基準の項目と、その行の上端がビューポート上端から何px下にあるか（負なら上に見切れている）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Anchor {
    pub index: usize,
    pub y: f32,
}

/// 直前のフレーム末尾の状態。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Snapshot {
    pub geometry: Geometry,
    pub offset: f32,
    pub viewport_h: f32,
    pub total: usize,
    /// カーソル（選択）項目の、並び上の位置
    pub selected: Option<usize>,
}

/// 基準を選ぶ。項目が無ければ None。
pub(super) fn pick_anchor(s: &Snapshot) -> Option<Anchor> {
    let g = &s.geometry;
    if s.total == 0 || g.cols == 0 || g.pitch <= 0.0 {
        return None;
    }
    if let Some(sel) = s.selected.filter(|&i| i < s.total) {
        let top = g.item_top(sel) - s.offset;
        if s.viewport_h > 0.0 && top + g.item_h > 0.0 && top < s.viewport_h {
            return Some(Anchor { index: sel, y: top });
        }
    }
    let max_row = (s.total - 1) / g.cols;
    // ビューポート上端の辺が通っている行。辺が行間の隙間にある時は、次の行を基準にする
    let mut row = (s.offset / g.pitch).floor().max(0.0) as usize;
    if s.offset - row as f32 * g.pitch > g.item_h {
        row += 1;
    }
    let row = row.min(max_row);
    Some(Anchor { index: row * g.cols, y: row as f32 * g.pitch - s.offset })
}

/// 新しいレイアウトで、基準の項目が同じ高さに来るオフセット。
pub(super) fn offset_for(new: &Geometry, anchor: Anchor) -> f32 {
    (new.item_top(anchor.index) - anchor.y).max(0.0)
}

/// 前のレイアウトから変わった分に補正が要るか。列数だけの変化（カード内）は `grid_anchor.rs` が担当なので除く。
pub(super) fn needs_compensation(prev: &Geometry, now: &Geometry) -> bool {
    prev.is_list != now.is_list || (prev.pitch - now.pitch).abs() > 0.01
}

/// レイアウトが変わっていれば、補正後のオフセットを返す。
/// 並びの件数が変わった（ディレクトリ切替・フィルタ）ときは、基準の項目が別物なので補正しない。
pub(super) fn compensated_offset(prev: &Snapshot, now: &Geometry, total: usize) -> Option<f32> {
    if !needs_compensation(&prev.geometry, now) || prev.total != total {
        return None;
    }
    pick_anchor(prev).map(|a| offset_for(now, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(cols: usize) -> Geometry {
        Geometry { cols, item_h: 192.0, pitch: 200.0, is_list: false }
    }

    fn list() -> Geometry {
        Geometry { cols: 1, item_h: 48.0, pitch: 49.0, is_list: true }
    }

    fn snap(g: Geometry, offset: f32, selected: Option<usize>) -> Snapshot {
        Snapshot { geometry: g, offset, viewport_h: 600.0, total: 100, selected }
    }

    #[test]
    fn visible_selection_keeps_its_height_when_switching_card_to_list() {
        // カード4列。選択は 14番（行3, 上端 600）。オフセット 400 → 画面上 y=200
        let s = snap(card(4), 400.0, Some(14));
        let a = pick_anchor(&s).unwrap();
        assert_eq!(a, Anchor { index: 14, y: 200.0 });
        // リストでは 14番は行14（上端 14*49=686）。y=200 に来るオフセット = 486
        assert_eq!(offset_for(&list(), a), 486.0);
    }

    #[test]
    fn without_a_visible_selection_the_top_row_is_the_anchor() {
        // 選択なし、オフセット 400（カード行2の先頭が画面上 y=0）→ 先頭項目は 8番
        let a = pick_anchor(&snap(card(4), 400.0, None)).unwrap();
        assert_eq!(a, Anchor { index: 8, y: 0.0 });
        assert_eq!(offset_for(&list(), a), 8.0 * 49.0);
    }

    #[test]
    fn an_offscreen_selection_falls_back_to_the_top_row() {
        let a = pick_anchor(&snap(card(4), 400.0, Some(90))).unwrap();
        assert_eq!(a.index, 8);
    }

    #[test]
    fn a_partly_cut_off_row_keeps_the_cut_amount() {
        // オフセット 450 → 行2 の上端は -50（上に50px見切れ）
        let a = pick_anchor(&snap(card(4), 450.0, None)).unwrap();
        assert_eq!(a, Anchor { index: 8, y: -50.0 });
        // 見切れ量ごと復元する
        assert_eq!(offset_for(&list(), a), 8.0 * 49.0 + 50.0);
    }

    #[test]
    fn list_to_card_maps_the_row_back_to_the_card_grid() {
        // リストの 21行目が画面上端。カード4列では行5 → 上端 1000
        let a = pick_anchor(&snap(list(), 21.0 * 49.0, None)).unwrap();
        assert_eq!(a.index, 21);
        assert_eq!(offset_for(&card(4), a), 5.0 * 200.0);
    }

    #[test]
    fn offset_never_goes_negative() {
        let a = Anchor { index: 0, y: 300.0 };
        assert_eq!(offset_for(&list(), a), 0.0);
    }

    #[test]
    fn a_gap_between_rows_picks_the_next_row() {
        // オフセット 195（行0の下端192を過ぎて隙間にある）→ 次の行（行1 = 4番）
        let a = pick_anchor(&snap(card(4), 195.0, None)).unwrap();
        assert_eq!(a.index, 4);
    }

    #[test]
    fn compensation_is_needed_for_mode_and_size_changes_only() {
        assert!(needs_compensation(&card(4), &list()));
        assert!(needs_compensation(&list(), &card(4)));
        // サムネ設定の変更（行の送り幅が変わる）
        assert!(needs_compensation(&card(4), &Geometry { pitch: 300.0, item_h: 292.0, ..card(4) }));
        // リストのサイズ変更
        assert!(needs_compensation(&list(), &Geometry { pitch: 73.0, item_h: 72.0, ..list() }));
        // 列数だけの変化は grid_anchor.rs の担当
        assert!(!needs_compensation(&card(4), &card(5)));
        assert!(!needs_compensation(&list(), &list()));
    }

    #[test]
    fn no_compensation_when_the_item_count_changed() {
        let s = snap(card(4), 400.0, Some(14));
        assert_eq!(compensated_offset(&s, &list(), 100), Some(486.0));
        assert_eq!(compensated_offset(&s, &list(), 40), None, "別のフォルダ・フィルタ変更では基準が無効");
    }

    #[test]
    fn empty_list_has_no_anchor() {
        let s = Snapshot { total: 0, ..snap(card(4), 0.0, None) };
        assert_eq!(pick_anchor(&s), None);
    }

    /// 編集の解除（リストの全行が高くなる）で、見ていた行がずれない。
    fn list_of(row_h: f32) -> Geometry {
        Geometry { cols: 1, item_h: row_h, pitch: row_h + super::super::list_view::ROW_GAP, is_list: true }
    }

    #[test]
    fn unlocking_edit_keeps_the_top_row_and_a_visible_selection_in_place() {
        use super::super::list_view::edit_row_height;
        let locked = list_of(24.0);
        let unlocked = list_of(edit_row_height(24.0, true));
        assert!(needs_compensation(&locked, &unlocked), "行の高さが変わるので補正が要る");

        // 選択なし: 先頭に見えている行（40行目、見切れ 7px）が、解除後も同じ見切れ量で先頭に来る
        let top = snap(locked, 40.0 * locked.pitch + 7.0, None);
        let off = compensated_offset(&top, &unlocked, 100).unwrap();
        assert_eq!(off, 40.0 * unlocked.pitch + 7.0);

        // 選択あり(45行目): 画面上の高さが解除前後で同じ
        let sel = snap(locked, 40.0 * locked.pitch, Some(45));
        let before = 45.0 * locked.pitch - sel.offset;
        let off = compensated_offset(&sel, &unlocked, 100).unwrap();
        assert_eq!(45.0 * unlocked.pitch - off, before);

        // ロックし直すと元に戻る
        let back = compensated_offset(&snap(unlocked, off, Some(45)), &locked, 100).unwrap();
        assert!((back - sel.offset).abs() < 0.001);
    }
}
