//! 疑似コマ送りの純粋ロジック。ユーザーが決めた倍率のフレーム（=ビューポート）で、ページ全体を
//! 読み順に塗りつぶすコマ列を作り、現在位置から次/前のコマを選ぶ。
//! コマの論理的な解析はせず、格子だけで決める。座標は magnifier の `MagnifierView::offset`
//! （ビューポート左上のコンテンツ座標・画面px）と同じ。egui::Vec2 以外に依存しない。
//!
//! - 各軸とも、コンテンツがフレームを超える分だけフレームを並べ、最後の1コマは端へクランプする
//!   （前のコマと重なる）。フレームより小さい軸は1コマ（既存の中央寄せで表示される）。
//! - 並びはZ走査。行内は綴じ方向（右綴じ=右→左）、行末で次行の先頭側へ戻る（蛇行しない）。
//! - 最終コマでの「次」・先頭コマでの「前」は `KomaMove::PageBoundary` を返し、
//!   ページ送りは呼び出し側に任せる。

use egui::Vec2;

/// 軸の超過がこれ以下なら「フレームに収まる」とみなす（画面px。目に見えない誤差を無視する）。
const FIT_EPS_PX: f32 = 0.5;
/// フレーム数を数えるときの浮動小数の丸め誤差の逃がし（超過がちょうどフレームの整数倍でも余計な1コマを作らない）。
const COUNT_EPS: f32 = 1e-4;
/// これ未満のフレームは不正とみなして1コマに縮退する（コマ数の暴発防止）。
const MIN_FRAME_PX: f32 = 1.0;
/// 1軸のコマ数の上限。実用の範囲（最大拡大×大判ページ）を十分超える値で、超えたら1コマに縮退する。
const MAX_STOPS_PER_AXIS: f32 = 10_000.0;

/// 行内の読み順（横方向）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadDir {
    /// 左→右（左綴じ・単ページ）。
    LeftToRight,
    /// 右→左（右綴じ）。
    RightToLeft,
}

/// コマ送りの結果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KomaMove {
    /// 同じページ内のこのオフセットへ移る。
    To(Vec2),
    /// ページの端を越える（次ページの先頭コマ／前ページの最終コマへ）。
    PageBoundary,
}

/// 1軸ぶんのコマ位置（スクロールオフセット）の列。昇順。
/// `content` は画面px換算のコンテンツ長、`viewport` はフレーム長。
fn axis_stops(content: f32, viewport: f32) -> Vec<f32> {
    let overflow = content - viewport;
    if !(viewport >= MIN_FRAME_PX && overflow.is_finite() && overflow > FIT_EPS_PX) {
        return vec![0.0];
    }
    // 先頭を除いたコマ数。最後の1コマだけ端へクランプする。
    let extra = (overflow / viewport - COUNT_EPS).ceil().max(1.0);
    if extra > MAX_STOPS_PER_AXIS {
        return vec![0.0];
    }
    let extra = extra as usize;
    (0..=extra).map(|i| if i == extra { overflow } else { i as f32 * viewport }).collect()
}

/// ページ全体を塗りつぶすコマの格子。
#[derive(Clone, Debug, PartialEq)]
pub struct KomaGrid {
    xs: Vec<f32>,
    ys: Vec<f32>,
    dir: ReadDir,
}

impl KomaGrid {
    /// `content` は現在倍率でのコンテンツ寸法（原寸px × 倍率。回転時は外接寸法）、
    /// `viewport` はフレーム（表示領域）の寸法。
    pub fn new(content: Vec2, viewport: Vec2, dir: ReadDir) -> Self {
        Self { xs: axis_stops(content.x, viewport.x), ys: axis_stops(content.y, viewport.y), dir }
    }

    /// コマ数。常に1以上。
    pub fn len(&self) -> usize {
        self.xs.len() * self.ys.len()
    }

    /// 読み順 `index`（0始まり）のコマのオフセット。範囲外は端へ丸める。
    pub fn position(&self, index: usize) -> Vec2 {
        let index = index.min(self.len() - 1);
        let cols = self.xs.len();
        let (row, order) = (index / cols, index % cols);
        let col = match self.dir {
            ReadDir::LeftToRight => order,
            ReadDir::RightToLeft => cols - 1 - order,
        };
        Vec2::new(self.xs[col], self.ys[row])
    }

    /// 先頭コマ（読み始め）。
    pub fn first(&self) -> Vec2 {
        self.position(0)
    }

    /// 最終コマ（読み終わり）。
    pub fn last(&self) -> Vec2 {
        self.position(self.len() - 1)
    }

    /// `offset` に最も近いコマの読み順。ドラッグなどで格子から外れた位置からも、この上で前後へ進む。
    pub fn nearest_index(&self, offset: Vec2) -> usize {
        let row = nearest(&self.ys, offset.y);
        let col = nearest(&self.xs, offset.x);
        let order = match self.dir {
            ReadDir::LeftToRight => col,
            ReadDir::RightToLeft => self.xs.len() - 1 - col,
        };
        row * self.xs.len() + order
    }

    /// 現在位置から1コマ進む。最終コマ（に最も近い位置）からはページ境界。
    pub fn next(&self, from: Vec2) -> KomaMove {
        let i = self.nearest_index(from);
        if i + 1 >= self.len() { KomaMove::PageBoundary } else { KomaMove::To(self.position(i + 1)) }
    }

    /// 現在位置から1コマ戻る。先頭コマ（に最も近い位置）からはページ境界。
    pub fn prev(&self, from: Vec2) -> KomaMove {
        match self.nearest_index(from) {
            0 => KomaMove::PageBoundary,
            i => KomaMove::To(self.position(i - 1)),
        }
    }
}

/// 昇順の `stops` のうち `v` に最も近いものの添字。同距離なら手前（小さい側）。
fn nearest(stops: &[f32], v: f32) -> usize {
    let mut best = 0;
    for (i, s) in stops.iter().enumerate() {
        if (s - v).abs() < (stops[best] - v).abs() {
            best = i;
        }
    }
    best
}

/// コマ移動のアニメーション時間（秒）。移動距離に関係なく一定。
pub const TWEEN_SECS: f64 = 0.18;
/// アニメーション終盤の減速が占める割合。残りは加速。
const TWEEN_DECEL_RATIO: f32 = 0.25;

/// コマ移動の進行度。経過割合 `t`（0..=1）に対し、二次の加速のあと終盤だけ線形に減速して、
/// 0から1へ進む。加速と減速の境目で速度は連続し、始点・終点とも速度0になる。
/// 有限でない `t` は完了（1.0）扱い。
pub fn tween_progress(t: f32) -> f32 {
    if !t.is_finite() {
        return 1.0;
    }
    let t = t.clamp(0.0, 1.0);
    let accel = 1.0 - TWEEN_DECEL_RATIO;
    if t <= accel {
        t * t / accel
    } else {
        let u = t - accel;
        accel + 2.0 * (u - u * u / (2.0 * TWEEN_DECEL_RATIO))
    }
}

/// `from` から `to` へ、進行度 `progress`（0..=1）で補間したオフセット。
pub fn lerp_offset(from: Vec2, to: Vec2, progress: f32) -> Vec2 {
    from + (to - from) * progress
}

/// 倍率を「高さフィット相対」（1.0 = ページの高さがちょうど窓の高さに収まる倍率）へ換算する。
/// 見開きのようにフィットが幅で決まるページでも、ページ高さが同じなら同じ値になるので、
/// ページのアスペクト比が変わっても、フレームがページ高さに占める割合を保てる。
/// `page_h` は回転後の外接高さ（原寸px）。寸法が不正なときは 1.0。
pub fn height_rel_from_scale(scale: f32, page_h: f32, viewport_h: f32) -> f32 {
    if !(scale > 0.0 && scale.is_finite() && page_h > 0.0 && viewport_h > 0.0) {
        return 1.0;
    }
    scale * page_h / viewport_h
}

/// `height_rel_from_scale` の逆変換。`rel` が不正、または寸法が不正なときは原寸（1.0）。
/// 範囲（`scale_range`）への丸めは呼び出し側で行う。
pub fn scale_from_height_rel(rel: f32, page_h: f32, viewport_h: f32) -> f32 {
    if !(rel > 0.0 && rel.is_finite() && page_h > 0.0 && viewport_h > 0.0) {
        return 1.0;
    }
    rel * viewport_h / page_h
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-3;

    fn v(x: f32, y: f32) -> Vec2 { Vec2::new(x, y) }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS
    }

    fn assert_stops(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len(), "{actual:?} != {expected:?}");
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < EPS, "{actual:?} != {expected:?}");
        }
    }

    /// 次を最後まで辿った位置列（先頭含む）と、末尾で返った結果。
    fn walk_next(grid: &KomaGrid) -> (Vec<Vec2>, KomaMove) {
        let mut at = grid.first();
        let mut path = vec![at];
        loop {
            match grid.next(at) {
                KomaMove::To(p) => {
                    path.push(p);
                    at = p;
                }
                end => return (path, end),
            }
        }
    }

    #[test]
    fn axis_fitting_in_frame_is_single_stop() {
        assert_stops(&axis_stops(800.0, 1000.0), &[0.0]);
        assert_stops(&axis_stops(1000.0, 1000.0), &[0.0]);
        // 目に見えない超過（0.5px以下）は無視する。
        assert_stops(&axis_stops(1000.4, 1000.0), &[0.0]);
    }

    #[test]
    fn axis_partial_last_frame_is_clamped_to_edge() {
        // A'・A''・A'''（最後はフレームの半分）。3コマ＝2回のコマ送り。最後は端へクランプ。
        assert_stops(&axis_stops(2500.0, 1000.0), &[0.0, 1000.0, 1500.0]);
        // 超過が1フレーム未満なら2コマ。
        assert_stops(&axis_stops(1300.0, 1000.0), &[0.0, 300.0]);
    }

    #[test]
    fn axis_exact_multiple_has_no_extra_frame() {
        assert_stops(&axis_stops(2000.0, 1000.0), &[0.0, 1000.0]);
        assert_stops(&axis_stops(3000.0, 1000.0), &[0.0, 1000.0, 2000.0]);
        // 浮動小数の誤差でも余計な1コマを作らない。
        assert_stops(&axis_stops(3000.0 + 1e-3, 1000.0), &[0.0, 1000.0, 2000.0]);
    }

    #[test]
    fn axis_invalid_input_degrades_to_single_stop() {
        assert_stops(&axis_stops(5000.0, 0.0), &[0.0]);
        assert_stops(&axis_stops(5000.0, 0.5), &[0.0]);
        assert_stops(&axis_stops(5000.0, f32::NAN), &[0.0]);
        assert_stops(&axis_stops(f32::NAN, 1000.0), &[0.0]);
        assert_stops(&axis_stops(f32::INFINITY, 1000.0), &[0.0]);
        assert_stops(&axis_stops(f32::MAX, 1000.0), &[0.0]);
    }

    #[test]
    fn vertical_only_page_walks_top_to_bottom_then_hits_boundary() {
        // ページは縦に3分割（3個目は半分）。コンテンツの幅はフレームに収まる。
        let grid = KomaGrid::new(v(900.0, 2500.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert_eq!(grid.len(), 3);
        let (path, end) = walk_next(&grid);
        assert_eq!(path.len(), 3);
        assert!(close(path[0], v(0.0, 0.0)));
        assert!(close(path[1], v(0.0, 1000.0)));
        assert!(close(path[2], v(0.0, 1500.0)));
        assert_eq!(end, KomaMove::PageBoundary);
    }

    #[test]
    fn wide_page_left_to_right_is_z_scan() {
        // 横2×縦3。行ごとに左→右、行末で次行の左端へ戻る。
        let grid = KomaGrid::new(v(2000.0, 2500.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert_eq!(grid.len(), 6);
        let (path, end) = walk_next(&grid);
        let expected = [
            v(0.0, 0.0), v(1000.0, 0.0),
            v(0.0, 1000.0), v(1000.0, 1000.0),
            v(0.0, 1500.0), v(1000.0, 1500.0),
        ];
        assert_eq!(path.len(), expected.len());
        for (p, e) in path.iter().zip(expected) {
            assert!(close(*p, e), "{p:?} != {e:?}");
        }
        assert_eq!(end, KomaMove::PageBoundary);
    }

    #[test]
    fn wide_page_right_to_left_starts_at_right_and_returns_to_right_edge() {
        let grid = KomaGrid::new(v(2000.0, 2000.0), v(1000.0, 1000.0), ReadDir::RightToLeft);
        let (path, end) = walk_next(&grid);
        let expected = [v(1000.0, 0.0), v(0.0, 0.0), v(1000.0, 1000.0), v(0.0, 1000.0)];
        assert_eq!(path.len(), expected.len());
        for (p, e) in path.iter().zip(expected) {
            assert!(close(*p, e), "{p:?} != {e:?}");
        }
        assert_eq!(end, KomaMove::PageBoundary);
        // 読み終わりは左下（右綴じ）／右下（左綴じ）。
        assert!(close(grid.last(), v(0.0, 1000.0)));
        let ltr = KomaGrid::new(v(2000.0, 2000.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert!(close(ltr.last(), v(1000.0, 1000.0)));
        assert!(close(ltr.first(), v(0.0, 0.0)));
    }

    #[test]
    fn prev_walks_back_and_hits_boundary_at_first() {
        let grid = KomaGrid::new(v(2000.0, 2500.0), v(1000.0, 1000.0), ReadDir::RightToLeft);
        let mut at = grid.last();
        let mut count = 1;
        while let KomaMove::To(p) = grid.prev(at) {
            at = p;
            count += 1;
        }
        assert_eq!(count, grid.len());
        assert!(close(at, grid.first()));
        assert_eq!(grid.prev(grid.first()), KomaMove::PageBoundary);
    }

    #[test]
    fn next_then_prev_returns_to_the_same_frame() {
        let grid = KomaGrid::new(v(2000.0, 2500.0), v(1000.0, 1000.0), ReadDir::RightToLeft);
        for i in 0..grid.len() - 1 {
            let here = grid.position(i);
            let KomaMove::To(n) = grid.next(here) else { panic!("frame {i} has a next") };
            assert!(close(n, grid.position(i + 1)));
            let KomaMove::To(back) = grid.prev(n) else { panic!("frame {} has a prev", i + 1) };
            assert!(close(back, here));
        }
    }

    #[test]
    fn page_fitting_in_frame_is_one_frame_and_always_page_boundary() {
        // フレーム以下（フィット表示など）はコマ送り＝ページ送りと同じ。
        let grid = KomaGrid::new(v(800.0, 1000.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid.next(v(0.0, 0.0)), KomaMove::PageBoundary);
        assert_eq!(grid.prev(v(0.0, 0.0)), KomaMove::PageBoundary);
        assert!(close(grid.first(), grid.last()));
    }

    #[test]
    fn off_grid_position_snaps_to_nearest_frame_before_moving() {
        // 縦: 0, 1000, 1500。ドラッグで y=1200（1000に最も近い）にいるとき、次は1500、前は0。
        let grid = KomaGrid::new(v(900.0, 2500.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert_eq!(grid.next(v(0.0, 1200.0)), KomaMove::To(v(0.0, 1500.0)));
        assert_eq!(grid.prev(v(0.0, 1200.0)), KomaMove::To(v(0.0, 0.0)));
        // 最終コマに近ければ、そこから「次」はページ境界。
        assert_eq!(grid.next(v(0.0, 1450.0)), KomaMove::PageBoundary);
        // 先頭コマに近ければ、そこから「前」はページ境界。
        assert_eq!(grid.prev(v(0.0, 100.0)), KomaMove::PageBoundary);
    }

    #[test]
    fn off_grid_horizontal_respects_reading_direction() {
        // 横は 0, 1000。右綴じでは右(1000)が先頭。x=900（1000に近い）＝先頭の次は 0 側。
        let rtl = KomaGrid::new(v(2000.0, 900.0), v(1000.0, 1000.0), ReadDir::RightToLeft);
        assert_eq!(rtl.nearest_index(v(900.0, 0.0)), 0);
        assert_eq!(rtl.next(v(900.0, 0.0)), KomaMove::To(v(0.0, 0.0)));
        let ltr = KomaGrid::new(v(2000.0, 900.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert_eq!(ltr.nearest_index(v(900.0, 0.0)), 1);
        assert_eq!(ltr.next(v(900.0, 0.0)), KomaMove::PageBoundary);
    }

    #[test]
    fn position_index_out_of_range_clamps_to_last() {
        let grid = KomaGrid::new(v(2000.0, 2000.0), v(1000.0, 1000.0), ReadDir::LeftToRight);
        assert!(close(grid.position(999), grid.last()));
    }

    #[test]
    fn frames_cover_the_whole_content() {
        // どの倍率・寸法でも、コマ位置の集合はコンテンツを隙間なく覆い、範囲を外れない。
        for &(cw, ch) in &[(2000.0f32, 2500.0f32), (1234.5, 4321.0), (999.0, 3001.0), (5000.0, 1000.0)] {
            let vp = v(1000.0, 900.0);
            let grid = KomaGrid::new(v(cw, ch), vp, ReadDir::LeftToRight);
            let (xs, ys) = (&grid.xs, &grid.ys);
            for (stops, len, view) in [(xs, cw, vp.x), (ys, ch, vp.y)] {
                assert_eq!(stops[0], 0.0);
                if len - view > FIT_EPS_PX {
                    assert!((stops.last().unwrap() + view - len).abs() < EPS, "last frame must end at the edge");
                }
                for w in stops.windows(2) {
                    assert!(w[1] - w[0] <= view + EPS, "gap between frames: {stops:?}");
                    assert!(w[1] > w[0]);
                }
            }
        }
    }

    #[test]
    fn tween_progress_starts_slow_and_ends_at_the_target() {
        assert_eq!(tween_progress(0.0), 0.0);
        assert!((tween_progress(1.0) - 1.0).abs() < 1e-6);
        // 初速は遅い（線形より進んでいない）。
        assert!(tween_progress(0.2) < 0.2 * 0.5, "{}", tween_progress(0.2));
        // 範囲外・不正値は端へ丸める。
        assert_eq!(tween_progress(-1.0), 0.0);
        assert_eq!(tween_progress(2.0), 1.0);
        assert_eq!(tween_progress(f32::NAN), 1.0);
        assert_eq!(tween_progress(f32::INFINITY), 1.0);
    }

    #[test]
    fn tween_progress_is_monotonic_and_speed_is_continuous() {
        let n = 1000;
        let mut prev = tween_progress(0.0);
        let mut speeds = Vec::new();
        for i in 1..=n {
            let p = tween_progress(i as f32 / n as f32);
            assert!(p >= prev, "戻っている: i={i}");
            speeds.push((p - prev) * n as f32);
            prev = p;
        }
        // 速度は加速区間で増え、減速区間で減る。最高速は加速の終わり（75%）付近。
        let peak = speeds.iter().cloned().enumerate().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap();
        assert!((peak.0 as f32 / n as f32 - 0.75).abs() < 0.01, "peak at {}", peak.0);
        assert!(speeds.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01), "速度が飛んでいる");
        // 終点の速度はほぼ0（ピタッと止まる）、始点もほぼ0。
        assert!(*speeds.last().unwrap() < 0.01);
        assert!(speeds[0] < 0.01);
    }

    #[test]
    fn lerp_offset_interpolates_both_axes() {
        let mid = lerp_offset(v(0.0, 100.0), v(200.0, 0.0), 0.5);
        assert!(close(mid, v(100.0, 50.0)));
        assert!(close(lerp_offset(v(1.0, 2.0), v(3.0, 4.0), 0.0), v(1.0, 2.0)));
        assert!(close(lerp_offset(v(1.0, 2.0), v(3.0, 4.0), 1.0), v(3.0, 4.0)));
    }

    #[test]
    fn height_rel_round_trips() {
        let rel = height_rel_from_scale(0.5, 2000.0, 1000.0);
        assert!((rel - 1.0).abs() < 1e-6);
        assert!((scale_from_height_rel(rel, 2000.0, 1000.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn height_rel_keeps_frame_across_pages_by_height() {
        // 解像度が2倍のページ（同アスペクト比）でも、画面上の大きさが同じになる倍率へ換算される。
        let rel = height_rel_from_scale(0.8, 1500.0, 1000.0);
        let s = scale_from_height_rel(rel, 3000.0, 1000.0);
        assert!((s - 0.4).abs() < 1e-6);
        // 高さが同じで幅だけ2倍のページ（見開き）では、倍率がそのまま維持される。
        let s = scale_from_height_rel(rel, 1500.0, 1000.0);
        assert!((s - 0.8).abs() < 1e-6);
    }

    #[test]
    fn height_rel_is_independent_of_window_width_and_follows_window_height() {
        // 窓の高さが変わっても、ページ高さに占めるフレームの割合は保たれる。
        let rel = height_rel_from_scale(0.5, 2000.0, 1000.0);
        assert!((scale_from_height_rel(rel, 2000.0, 500.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn height_rel_invalid_inputs_fall_back() {
        assert_eq!(height_rel_from_scale(0.0, 100.0, 100.0), 1.0);
        assert_eq!(height_rel_from_scale(f32::NAN, 100.0, 100.0), 1.0);
        assert_eq!(height_rel_from_scale(1.0, 0.0, 100.0), 1.0);
        assert_eq!(height_rel_from_scale(1.0, 100.0, 0.0), 1.0);
        assert_eq!(scale_from_height_rel(0.0, 100.0, 100.0), 1.0);
        assert_eq!(scale_from_height_rel(f32::INFINITY, 100.0, 100.0), 1.0);
        assert_eq!(scale_from_height_rel(1.0, 0.0, 100.0), 1.0);
    }
}
