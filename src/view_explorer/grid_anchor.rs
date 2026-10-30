//! 中央グリッドの列数が変わった時（タグパネルの開閉・幅ドラッグ・ウィンドウリサイズ）に、
//! 見ていた位置を保つためのスクロール補正。
//!
//! スクロール位置はピクセルで持っているが、列数が変わるとカードの行番号が変わるため、
//! そのままだと選択カードなどがビューポートから外れる。そこで「基準カード」（グリッド全体の
//! 並び `grid_entries()` 上のindex）と、その画面上の高さを覚えておき、列数が変わったフレームで
//! 新しい行位置から逆算してオフセットを決め直す。
//!
//! 基準カードの選び方:
//! - 選択（カーソル）カードが一部でも可視範囲内にあれば、それを同じ画面上の位置に保つ
//! - それ以外（選択なし・範囲外）は、ビューポート上端の行の先頭カードを基準にする
//!
//! どちらも見切れている量(px)ごと復元するため、半端に見切れた状態も保たれる。
//!
//! 基準は「ユーザーのスクロール・選択変更・並びの変化」があった時だけ取り直す。
//! 列数が変わる補正自体では取り直さない（パネルのドラッグ中に列数が行き来しても、
//! 基準がずれ続けないようにするため）。末尾での上限クランプはegui側に任せる。

/// グリッドのセル間隔。`draw_archive_grid` の `GAP` および入力側の `KEY_GAP` と同じ値。
pub(super) const GRID_GAP: f32 = 8.0;

/// オフセットのずれをスクロールとみなす許容誤差（px）。
const OFFSET_EPS: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    /// `grid_entries()` 上の位置
    index: usize,
    /// 基準カードの行の上端が、ビューポート上端から何px下にあるか。負なら上に見切れている
    y: f32,
}

/// 基準の取得と、列数変化時のオフセット補正を担う。
#[derive(Debug, Default)]
pub(super) struct GridScrollAnchor {
    anchor: Option<Anchor>,
    cols: usize,
    /// 基準を取った（または補正を適用した）時点のオフセット。これとずれたらスクロールされたとみなす
    valid_offset: f32,
    total: usize,
    selected: Option<usize>,
    /// 直前の `begin_frame` で補正を適用した
    just_applied: bool,
}

fn row_pitch(cell_h: f32) -> f32 {
    cell_h + GRID_GAP
}

fn make_anchor(
    offset: f32,
    viewport_h: f32,
    cell_h: f32,
    cols: usize,
    total: usize,
    selected: Option<usize>,
) -> Option<Anchor> {
    if total == 0 || cols == 0 {
        return None;
    }
    let pitch = row_pitch(cell_h);
    if let Some(sel) = selected.filter(|&s| s < total) {
        let top = (sel / cols) as f32 * pitch - offset;
        if viewport_h > 0.0 && top + cell_h > 0.0 && top < viewport_h {
            return Some(Anchor { index: sel, y: top });
        }
    }
    let max_row = (total - 1) / cols;
    // ビューポート上端の辺が通っている行。辺が行間の隙間にある時は、次の行を基準にする
    let mut row = (offset / pitch).floor().max(0.0) as usize;
    if offset - row as f32 * pitch > cell_h {
        row += 1;
    }
    let row = row.min(max_row);
    Some(Anchor { index: row * cols, y: row as f32 * pitch - offset })
}

impl GridScrollAnchor {
    /// 描画の頭（列数が確定した直後、`ScrollArea` にオフセットを渡す前）に呼ぶ。
    /// 列数が変わっていて補正できる時だけ、補正後のオフセットを返す。
    pub(super) fn begin_frame(
        &mut self,
        cols: usize,
        offset: f32,
        cell_h: f32,
        total: usize,
    ) -> Option<f32> {
        self.just_applied = false;
        let changed = self.cols != 0 && self.cols != cols;
        self.cols = cols;
        if !changed {
            return None;
        }
        // 外からオフセットが動かされた（キー移動・ディレクトリ切替）／並びが変わった場合、
        // 保持中の基準は古いので使わない
        let anchor = self.anchor?;
        if (offset - self.valid_offset).abs() > OFFSET_EPS || total != self.total || cols == 0 {
            return None;
        }
        let new_row = anchor.index / cols;
        let new_offset = (new_row as f32 * row_pitch(cell_h) - anchor.y).max(0.0);
        self.just_applied = true;
        Some(new_offset)
    }

    /// 描画の末尾（`ScrollArea` のオフセットを読み戻した後）に呼ぶ。
    pub(super) fn end_frame(
        &mut self,
        offset_out: f32,
        viewport_h: f32,
        cell_h: f32,
        cols: usize,
        total: usize,
        selected: Option<usize>,
    ) {
        let applied = std::mem::take(&mut self.just_applied);
        let scrolled = !applied && (offset_out - self.valid_offset).abs() > OFFSET_EPS;
        if self.anchor.is_none() || scrolled || total != self.total || selected != self.selected {
            self.anchor = make_anchor(offset_out, viewport_h, cell_h, cols, total, selected);
        }
        self.valid_offset = offset_out;
        self.total = total;
        self.selected = selected;
        self.cols = cols;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_H: f32 = 192.0;
    const PITCH: f32 = CELL_H + GRID_GAP; // 200
    const VP: f32 = 600.0;

    /// 1フレーム分（begin→ScrollAreaがoffsetをそのまま採用→end）を回す。
    fn frame(
        a: &mut GridScrollAnchor,
        cols: usize,
        offset: &mut f32,
        total: usize,
        selected: Option<usize>,
    ) {
        if let Some(o) = a.begin_frame(cols, *offset, CELL_H, total) {
            *offset = o;
        }
        a.end_frame(*offset, VP, CELL_H, cols, total, selected);
    }

    #[test]
    fn selected_card_keeps_its_screen_height_when_cols_shrink() {
        let mut a = GridScrollAnchor::default();
        let mut off = 400.0; // 2行ぶん下
        // 8列: index 20 は行2、上端は 2*200-400 = 0 に見えている
        frame(&mut a, 8, &mut off, 100, Some(20));
        // 列数 8→6: index 20 は行3。画面上の高さ0を保つので offset = 600
        frame(&mut a, 6, &mut off, 100, Some(20));
        assert_eq!(off, 3.0 * PITCH);
    }

    #[test]
    fn selected_card_is_kept_at_nonzero_screen_height() {
        let mut a = GridScrollAnchor::default();
        let mut off = 300.0;
        // 8列: index 24 は行3、top = 600-300 = 300。top+cell_h=492 <= 600 で完全に可視
        frame(&mut a, 8, &mut off, 100, Some(24));
        // 8→4: index 24 は行6。画面上の高さ300を保つ → 1200 - 300 = 900
        frame(&mut a, 4, &mut off, 100, Some(24));
        assert_eq!(off, 6.0 * PITCH - 300.0);
    }

    #[test]
    fn partly_cut_selection_is_kept_with_its_cut() {
        let mut a = GridScrollAnchor::default();
        let mut off = 300.0;
        // index 8 は行1 (top 200)。offset 300 では top=-100 で上に100px見切れている
        frame(&mut a, 8, &mut off, 100, Some(8));
        // 8→4: index 8 は行2 → 400 + 100 = 500。見切れ100pxのまま
        frame(&mut a, 4, &mut off, 100, Some(8));
        assert_eq!(off, 2.0 * PITCH + 100.0);
    }

    #[test]
    fn selection_fully_outside_falls_back_to_top_row() {
        let mut a = GridScrollAnchor::default();
        let mut off = 300.0;
        // index 40 は行5、top = 1000-300 = 700 > 600 で範囲外 → 上端の辺が通る行1 (index 8)
        frame(&mut a, 8, &mut off, 100, Some(40));
        // 8→4: 行1 の index 8 は行2。見切れ100pxを保つ → 400+100
        frame(&mut a, 4, &mut off, 100, Some(40));
        assert_eq!(off, 2.0 * PITCH + 100.0);
    }

    #[test]
    fn without_selection_the_top_row_is_the_anchor() {
        let mut a = GridScrollAnchor::default();
        let mut off = 400.0;
        frame(&mut a, 8, &mut off, 100, None); // 上端行=行2 → index 16
        frame(&mut a, 4, &mut off, 100, None); // 行4
        assert_eq!(off, 4.0 * PITCH);
        frame(&mut a, 8, &mut off, 100, None); // index 16 は行2 に戻る
        assert_eq!(off, 2.0 * PITCH);
    }

    #[test]
    fn offscreen_selection_falls_back_to_top_row() {
        let mut a = GridScrollAnchor::default();
        let mut off = 1000.0;
        // 選択 index 0 は画面外（行0、top=-1000）
        frame(&mut a, 8, &mut off, 100, Some(0));
        frame(&mut a, 4, &mut off, 100, Some(0));
        // 上端行は行5 (index 40) → 4列では行10
        assert_eq!(off, 10.0 * PITCH);
    }

    #[test]
    fn partial_top_row_keeps_its_cut_amount() {
        let mut a = GridScrollAnchor::default();
        let mut off = 130.0; // 行0を130px見切れ
        frame(&mut a, 8, &mut off, 100, None);
        frame(&mut a, 4, &mut off, 100, None);
        // 基準は行0 (index 0)。4列でも行0のまま、130px見切れを保つ
        assert_eq!(off, 130.0);

        let mut a = GridScrollAnchor::default();
        let mut off = 2.0 * PITCH + 50.0; // 行2を50px見切れ → index 16
        frame(&mut a, 8, &mut off, 100, None);
        frame(&mut a, 4, &mut off, 100, None); // 行4
        assert_eq!(off, 4.0 * PITCH + 50.0);
    }

    #[test]
    fn top_edge_in_the_gap_uses_the_next_row() {
        let mut a = GridScrollAnchor::default();
        let mut off = CELL_H + 4.0; // 行0の下端と行1の上端の隙間
        frame(&mut a, 8, &mut off, 100, None); // 基準は行1 (index 8)、y=+4
        frame(&mut a, 4, &mut off, 100, None); // 行2 → 400 - 4
        assert_eq!(off, 2.0 * PITCH - 4.0);
    }

    #[test]
    fn drag_back_and_forth_does_not_drift() {
        let mut a = GridScrollAnchor::default();
        let mut off = 400.0;
        frame(&mut a, 8, &mut off, 100, None); // index 16
        for cols in [7, 6, 5, 6, 7, 8] {
            frame(&mut a, cols, &mut off, 100, None);
        }
        // 基準index 16 は固定のまま。8列に戻れば元の位置
        assert_eq!(off, 400.0);
    }

    #[test]
    fn user_scroll_retakes_the_anchor() {
        let mut a = GridScrollAnchor::default();
        let mut off = 0.0;
        frame(&mut a, 8, &mut off, 100, None);
        off = 600.0; // ユーザーがスクロール（行3）
        frame(&mut a, 8, &mut off, 100, None);
        frame(&mut a, 4, &mut off, 100, None); // index 24 → 行6
        assert_eq!(off, 6.0 * PITCH);
    }

    #[test]
    fn external_offset_change_skips_the_stale_anchor() {
        let mut a = GridScrollAnchor::default();
        let mut off = 400.0;
        frame(&mut a, 8, &mut off, 100, None);
        // キー移動等で、フレーム間にオフセットが外から書き換えられた状態で列数も変わった
        off = 1000.0;
        let r = a.begin_frame(4, off, CELL_H, 100);
        assert_eq!(r, None);
    }

    #[test]
    fn changed_total_skips_the_stale_anchor() {
        let mut a = GridScrollAnchor::default();
        let mut off = 400.0;
        frame(&mut a, 8, &mut off, 100, None);
        assert_eq!(a.begin_frame(4, off, CELL_H, 50), None);
    }

    #[test]
    fn first_frame_and_unchanged_cols_do_nothing() {
        let mut a = GridScrollAnchor::default();
        assert_eq!(a.begin_frame(8, 300.0, CELL_H, 100), None);
        a.end_frame(300.0, VP, CELL_H, 8, 100, None);
        assert_eq!(a.begin_frame(8, 300.0, CELL_H, 100), None);
    }

    #[test]
    fn empty_grid_has_no_anchor() {
        let mut a = GridScrollAnchor::default();
        let mut off = 0.0;
        frame(&mut a, 8, &mut off, 0, None);
        assert_eq!(a.begin_frame(4, off, CELL_H, 0), None);
    }

    #[test]
    fn negative_result_is_clamped_to_zero() {
        let mut a = GridScrollAnchor::default();
        let mut off = 200.0;
        // 選択 index 8 (行1, top 0)。可視
        frame(&mut a, 8, &mut off, 100, Some(8));
        // 列数が増えて index 8 が行0へ。top=0を保つと offset=0
        frame(&mut a, 16, &mut off, 100, Some(8));
        assert_eq!(off, 0.0);
    }
}
