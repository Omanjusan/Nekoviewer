//! 虫眼鏡（ホイール拡縮）モードの純粋ロジック。状態・変換・クランプ・設定値の検証付きAPI。
//! 倍率は「原寸比」で持つ（1.0 = 原寸デコードしたテクスチャの等倍。GUI設定の「原寸」と同じ意味）。
//! 入力・描画・デコード連動は別フェーズ。ここは egui::Vec2 以外に依存しない。

use egui::Vec2;

/// 原寸（100%）の倍率。
pub const ACTUAL_SCALE: f32 = 1.0;
/// 原寸付近へ吸着させる許容幅。浮動小数の丸め誤差でノッチが空振りするのを防ぐ。
const ACTUAL_SNAP_EPS: f32 = 1e-3;

pub const DEFAULT_MAX_SCALE: f32 = 4.0;
const MAX_SCALE_RANGE: (f32, f32) = (1.0, 32.0);

pub const DEFAULT_AUTOHIDE_SECS: f32 = 2.0;
const AUTOHIDE_SECS_RANGE: (f32, f32) = (0.1, 30.0);
const AUTOHIDE_SECS_STEP: f32 = 0.1;

/// 1ノッチの倍率の既定。候補の切替（×1.1 / 1.25 / 1.5 / 2）はフェーズ5で足す。
pub const DEFAULT_NOTCH_RATIO: f32 = 1.25;

pub const DEFAULT_BAR_WIDTH_PCT: f32 = 20.0;
const BAR_WIDTH_PCT_RANGE: (f32, f32) = (10.0, 60.0);
const BAR_WIDTH_PCT_STEP: f32 = 0.5;
// 以下のサイズ既定は仮置き。フェーズ4の実描画で調整する。
const DEFAULT_BAR_BODY_HEIGHT: f32 = 24.0;
const BAR_BODY_HEIGHT_RANGE: (f32, f32) = (8.0, 64.0);
const DEFAULT_BUTTON_SIZE: Vec2 = Vec2::new(44.0, 24.0);
const BUTTON_SIZE_RANGE: (f32, f32) = (16.0, 96.0);

fn clamp_finite(v: f32, (lo, hi): (f32, f32), current: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { current }
}

fn round_to_step(v: f32, step: f32) -> f32 {
    (v / step).round() * step
}

/// 虫眼鏡バーのレイアウト設定。幅は親（全体）と子（各パーツ）の2階層。
/// 実際の矩形への解決（狭い窓での最小値保護を含む）はフェーズ4で `resolve` として足す。
#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    width_pct: f32,
    body_height: f32,
    step_button: Vec2,
    detail_button: Vec2,
}

impl Default for BarLayout {
    fn default() -> Self {
        Self {
            width_pct: DEFAULT_BAR_WIDTH_PCT,
            body_height: DEFAULT_BAR_BODY_HEIGHT,
            step_button: DEFAULT_BUTTON_SIZE,
            detail_button: DEFAULT_BUTTON_SIZE,
        }
    }
}

impl BarLayout {
    /// 親: バー全体（本体＋2ボタン＋隙間）の幅。ビューアー描画領域の幅に対する割合(%)。
    pub fn width_pct(&self) -> f32 { self.width_pct }
    pub fn body_height(&self) -> f32 { self.body_height }
    pub fn step_button_size(&self) -> Vec2 { self.step_button }
    pub fn detail_button_size(&self) -> Vec2 { self.detail_button }

    /// 範囲・刻み（0.5%）に丸めて適用し、実際に適用した値を返す。非有限値は無視する。
    pub fn set_width_pct(&mut self, pct: f32) -> f32 {
        if pct.is_finite() {
            self.width_pct = round_to_step(pct, BAR_WIDTH_PCT_STEP)
                .clamp(BAR_WIDTH_PCT_RANGE.0, BAR_WIDTH_PCT_RANGE.1);
        }
        self.width_pct
    }

    /// 子: バー本体の高さ(px)。本体の幅は親の幅から導出するので持たない。
    pub fn set_body_height(&mut self, px: f32) -> f32 {
        self.body_height = clamp_finite(px, BAR_BODY_HEIGHT_RANGE, self.body_height);
        self.body_height
    }

    /// 子: 倍率トグルボタンのサイズ(px)。軸ごとに範囲へ丸める。
    pub fn set_step_button_size(&mut self, size: Vec2) -> Vec2 {
        self.step_button = clamp_size(size, self.step_button);
        self.step_button
    }

    /// 子: 詳細／簡易切替ボタンのサイズ(px)。軸ごとに範囲へ丸める。
    pub fn set_detail_button_size(&mut self, size: Vec2) -> Vec2 {
        self.detail_button = clamp_size(size, self.detail_button);
        self.detail_button
    }
}

fn clamp_size(size: Vec2, current: Vec2) -> Vec2 {
    Vec2::new(
        clamp_finite(size.x, BUTTON_SIZE_RANGE, current.x),
        clamp_finite(size.y, BUTTON_SIZE_RANGE, current.y),
    )
}

/// 虫眼鏡モードの設定値。setter は「範囲に丸めて適用し、適用後の値を返す」。
/// GUI設定へ昇格するときはこの setter を呼ぶだけで済む（永続化は現状スコープ外）。
#[derive(Clone, Debug, PartialEq)]
pub struct MagnifierConfig {
    max_scale: f32,
    autohide_secs: f32,
    pub bar: BarLayout,
}

impl Default for MagnifierConfig {
    fn default() -> Self {
        Self {
            max_scale: DEFAULT_MAX_SCALE,
            autohide_secs: DEFAULT_AUTOHIDE_SECS,
            bar: BarLayout::default(),
        }
    }
}

impl MagnifierConfig {
    /// 上限倍率（原寸比）。
    pub fn max_scale(&self) -> f32 { self.max_scale }
    /// スライダーバーの自動ハイドまでの秒数。
    pub fn autohide_secs(&self) -> f32 { self.autohide_secs }

    pub fn set_max_scale(&mut self, v: f32) -> f32 {
        self.max_scale = clamp_finite(v, MAX_SCALE_RANGE, self.max_scale);
        self.max_scale
    }

    /// 0.1秒刻みに丸める。
    pub fn set_autohide_secs(&mut self, secs: f32) -> f32 {
        if secs.is_finite() {
            self.autohide_secs = round_to_step(secs, AUTOHIDE_SECS_STEP)
                .clamp(AUTOHIDE_SECS_RANGE.0, AUTOHIDE_SECS_RANGE.1);
        }
        self.autohide_secs
    }
}

/// 虫眼鏡の表示状態。`offset` は ScrollArea のスクロールオフセットと同じ意味
/// （ビューポート左上のコンテンツ座標）。コンテンツがビューポートより小さい軸は
/// 中央寄せの余白が入るので、その軸の `offset` は常に 0。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MagnifierView {
    pub scale: f32,
    pub offset: Vec2,
}

impl MagnifierView {
    /// フィット表示（モードに入った直後の状態）。
    pub fn fit(viewport: Vec2, img: Vec2) -> Self {
        Self { scale: fit_scale(viewport, img), offset: Vec2::ZERO }
    }
}

/// `img`（原寸テクスチャの寸法。90/270度回転時は外接寸法）を `viewport` にcontain-fitする倍率。
/// 寸法が不正なときは原寸（1.0）を返す。
pub fn fit_scale(viewport: Vec2, img: Vec2) -> f32 {
    if !(img.x > 0.0 && img.y > 0.0 && viewport.x > 0.0 && viewport.y > 0.0) {
        return ACTUAL_SCALE;
    }
    (viewport.x / img.x).min(viewport.y / img.y)
}

/// 倍率の許容範囲 (下限, 上限)。下限はフィット倍率。フィットが上限を超える小さな画像でも
/// フィット自体は表現できるよう、上限は少なくともフィット倍率にする。
pub fn scale_range(fit: f32, max_scale: f32) -> (f32, f32) {
    (fit, max_scale.max(fit))
}

/// 現在倍率から `notches` ノッチ（正=拡大）進めた倍率を返す。1ノッチは `ratio` 倍。
/// 原寸(100%)をまたぐ動きは一度100%ぴったりで止め、その後範囲へ丸める。
pub fn notch_scale(current: f32, notches: f32, ratio: f32, range: (f32, f32)) -> f32 {
    let current = current.clamp(range.0, range.1);
    if !(notches.is_finite() && ratio.is_finite() && ratio > 0.0) {
        return current;
    }
    let mut next = current * ratio.powf(notches);
    let crosses_actual = (current < ACTUAL_SCALE && next > ACTUAL_SCALE)
        || (current > ACTUAL_SCALE && next < ACTUAL_SCALE);
    if crosses_actual || (next - ACTUAL_SCALE).abs() < ACTUAL_SNAP_EPS {
        next = ACTUAL_SCALE;
    }
    next.clamp(range.0, range.1)
}

fn axis_pad(viewport: f32, content: f32) -> f32 {
    ((viewport - content) / 2.0).max(0.0)
}

fn pad_of(viewport: Vec2, content: Vec2) -> Vec2 {
    Vec2::new(axis_pad(viewport.x, content.x), axis_pad(viewport.y, content.y))
}

fn clamp_offset_to_content(offset: Vec2, viewport: Vec2, content: Vec2) -> Vec2 {
    Vec2::new(
        offset.x.clamp(0.0, (content.x - viewport.x).max(0.0)),
        offset.y.clamp(0.0, (content.y - viewport.y).max(0.0)),
    )
}

/// スクロール範囲（0〜コンテンツ−ビューポート）へ `offset` を丸める。
pub fn clamp_offset(view: MagnifierView, viewport: Vec2, img: Vec2) -> MagnifierView {
    MagnifierView {
        scale: view.scale,
        offset: clamp_offset_to_content(view.offset, viewport, img * view.scale),
    }
}

/// `anchor`（ビューポート左上を原点とした画面座標）の下にある、原寸ピクセル座標を返す。
pub fn image_point_at(view: MagnifierView, anchor: Vec2, viewport: Vec2, img: Vec2) -> Vec2 {
    let pad = pad_of(viewport, img * view.scale);
    (anchor + view.offset - pad) / view.scale
}

/// `anchor` の下の画像点を動かさないまま `new_scale` へ拡縮した表示状態を返す
/// （ホイール拡縮のポインタ基準）。スクロール端では、画像がビューポートを覆えなくなる
/// ぶんだけ基準点がずれる（丸めの結果）。
pub fn zoom_about(
    view: MagnifierView,
    anchor: Vec2,
    viewport: Vec2,
    img: Vec2,
    new_scale: f32,
) -> MagnifierView {
    if !(view.scale > 0.0 && new_scale > 0.0 && new_scale.is_finite()) {
        return view;
    }
    let point = image_point_at(view, anchor, viewport, img);
    let content = img * new_scale;
    let offset = point * new_scale + pad_of(viewport, content) - anchor;
    MagnifierView { scale: new_scale, offset: clamp_offset_to_content(offset, viewport, content) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-3;

    fn v(x: f32, y: f32) -> Vec2 { Vec2::new(x, y) }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS
    }

    #[test]
    fn fit_scale_contains_image() {
        let s = fit_scale(v(1920.0, 1000.0), v(1600.0, 2400.0));
        assert!((s - 1000.0 / 2400.0).abs() < 1e-6);
        // 小さい画像はフィットで拡大される。
        let s = fit_scale(v(1920.0, 1000.0), v(400.0, 600.0));
        assert!((s - 1000.0 / 600.0).abs() < 1e-6);
        // 不正な寸法は原寸。
        assert_eq!(fit_scale(v(0.0, 100.0), v(10.0, 10.0)), ACTUAL_SCALE);
        assert_eq!(fit_scale(v(100.0, 100.0), v(0.0, 10.0)), ACTUAL_SCALE);
    }

    #[test]
    fn scale_range_keeps_fit_representable() {
        assert_eq!(scale_range(0.4, 4.0), (0.4, 4.0));
        // フィットが上限を超えても、範囲が反転しない。
        assert_eq!(scale_range(10.0, 4.0), (10.0, 10.0));
    }

    #[test]
    fn notch_steps_multiply_and_clamp() {
        let range = (0.4, 4.0);
        assert!((notch_scale(1.25, 1.0, 1.25, range) - 1.5625).abs() < 1e-5);
        assert!((notch_scale(1.25, -1.0, 1.25, range) - 1.0).abs() < 1e-5);
        assert_eq!(notch_scale(3.5, 1.0, 1.25, range), 4.0);
        assert_eq!(notch_scale(0.45, -1.0, 1.25, range), 0.4);
        // 0ノッチは現在値（範囲内へ丸め）。
        assert_eq!(notch_scale(2.0, 0.0, 1.25, range), 2.0);
        // 不正な入力は現在値のまま。
        assert_eq!(notch_scale(2.0, f32::NAN, 1.25, range), 2.0);
        assert_eq!(notch_scale(2.0, 1.0, 0.0, range), 2.0);
    }

    #[test]
    fn notch_stops_at_actual_size() {
        let range = (0.3, 4.0);
        // 拡大でまたぐ・縮小でまたぐ、どちらも100%で止まる。
        assert_eq!(notch_scale(0.9, 1.0, 1.25, range), ACTUAL_SCALE);
        assert_eq!(notch_scale(1.1, -1.0, 1.25, range), ACTUAL_SCALE);
        // 100%ちょうどからは止まらずに進む。
        assert!((notch_scale(ACTUAL_SCALE, 1.0, 1.25, range) - 1.25).abs() < 1e-5);
        assert!((notch_scale(ACTUAL_SCALE, -1.0, 1.25, range) - 0.8).abs() < 1e-5);
        // 丸め誤差で100%のごく近傍になった場合も100%へ揃う。
        assert_eq!(notch_scale(1.25, -1.0, 1.25, range), ACTUAL_SCALE);
    }

    #[test]
    fn notch_when_actual_is_out_of_range() {
        // 小さい画像: フィットが原寸を超える。100%は範囲外なので下限で止まる。
        let range = (1.67, 4.0);
        assert_eq!(notch_scale(1.7, -2.0, 1.25, range), 1.67);
        assert!((notch_scale(1.67, 1.0, 1.25, range) - 1.67 * 1.25).abs() < 1e-4);
    }

    #[test]
    fn zoom_about_keeps_anchor_point() {
        let viewport = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        let view = MagnifierView { scale: 0.5, offset: v(100.0, 200.0) };
        let anchor = v(300.0, 250.0);
        let before = image_point_at(view, anchor, viewport, img);
        for new_scale in [0.7, 1.0, 1.5, 2.0] {
            let zoomed = zoom_about(view, anchor, viewport, img, new_scale);
            assert_eq!(zoomed.scale, new_scale);
            let after = image_point_at(zoomed, anchor, viewport, img);
            assert!(close(before, after), "scale {new_scale}: {before:?} vs {after:?}");
        }
    }

    #[test]
    fn zoom_about_round_trip_returns_to_same_view() {
        let viewport = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        let view = MagnifierView { scale: 1.0, offset: v(500.0, 900.0) };
        let anchor = v(400.0, 300.0);
        let zoomed = zoom_about(view, anchor, viewport, img, 1.25);
        let back = zoom_about(zoomed, anchor, viewport, img, 1.0);
        assert!(close(view.offset, back.offset), "{:?} vs {:?}", view.offset, back.offset);
    }

    #[test]
    fn zoom_about_clamps_at_content_edges() {
        let viewport = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        // 左上端を見ている状態で、右下端付近を基準に拡大しても範囲外へは出ない。
        let view = MagnifierView { scale: 0.4, offset: Vec2::ZERO };
        let zoomed = zoom_about(view, v(790.0, 590.0), viewport, img, 2.0);
        let content = img * 2.0;
        assert!(zoomed.offset.x >= 0.0 && zoomed.offset.x <= content.x - viewport.x);
        assert!(zoomed.offset.y >= 0.0 && zoomed.offset.y <= content.y - viewport.y);
        // 縮小してビューポートより小さくなった軸は 0（中央寄せ）。
        let small = zoom_about(zoomed, v(400.0, 300.0), viewport, v(400.0, 300.0), 1.0);
        assert_eq!(small.offset, Vec2::ZERO);
    }

    #[test]
    fn zoom_about_with_centered_small_content() {
        // コンテンツがビューポートより小さい間は offset=0 のまま（中央寄せ）。
        let viewport = v(800.0, 600.0);
        let img = v(400.0, 300.0);
        let view = MagnifierView { scale: 1.0, offset: Vec2::ZERO };
        let zoomed = zoom_about(view, v(600.0, 100.0), viewport, img, 1.5);
        assert_eq!(zoomed.offset, Vec2::ZERO);
        // 画像がビューポートを超えた軸だけスクロールが生じる。
        let zoomed = zoom_about(view, v(600.0, 100.0), viewport, img, 3.0);
        assert!(zoomed.offset.x > 0.0);
        assert_eq!(zoomed.offset.y, 0.0);
    }

    #[test]
    fn zoom_about_ignores_invalid_scale() {
        let view = MagnifierView { scale: 1.0, offset: v(10.0, 10.0) };
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(zoom_about(view, v(1.0, 1.0), v(100.0, 100.0), v(500.0, 500.0), bad), view);
        }
    }

    #[test]
    fn fit_view_starts_at_origin() {
        let view = MagnifierView::fit(v(1920.0, 1000.0), v(1600.0, 2400.0));
        assert!((view.scale - 1000.0 / 2400.0).abs() < 1e-6);
        assert_eq!(view.offset, Vec2::ZERO);
    }

    #[test]
    fn config_defaults() {
        let c = MagnifierConfig::default();
        assert_eq!(c.max_scale(), 4.0);
        assert_eq!(c.autohide_secs(), 2.0);
        assert_eq!(c.bar.width_pct(), 20.0);
    }

    #[test]
    fn config_setters_clamp_and_round() {
        let mut c = MagnifierConfig::default();
        assert_eq!(c.set_max_scale(8.0), 8.0);
        assert_eq!(c.set_max_scale(0.2), 1.0);
        assert_eq!(c.set_max_scale(999.0), 32.0);
        // 非有限値は無視して現在値を返す。
        assert_eq!(c.set_max_scale(f32::NAN), 32.0);

        assert_eq!(c.set_autohide_secs(2.26), 2.3);
        assert_eq!(c.set_autohide_secs(0.0), 0.1);
        assert_eq!(c.set_autohide_secs(99.0), 30.0);
        assert_eq!(c.set_autohide_secs(f32::INFINITY), 30.0);
    }

    #[test]
    fn bar_layout_setters_clamp_and_round() {
        let mut b = BarLayout::default();
        assert_eq!(b.set_width_pct(20.3), 20.5);
        assert_eq!(b.set_width_pct(5.0), 10.0);
        assert_eq!(b.set_width_pct(90.0), 60.0);
        assert_eq!(b.set_width_pct(f32::NAN), 60.0);

        assert_eq!(b.set_body_height(2.0), 8.0);
        assert_eq!(b.set_body_height(30.0), 30.0);

        // 軸ごとに丸め、非有限の軸は現在値を保つ。
        let applied = b.set_step_button_size(v(200.0, 4.0));
        assert_eq!(applied, v(96.0, 16.0));
        let applied = b.set_detail_button_size(v(f32::NAN, 30.0));
        assert_eq!(applied, v(DEFAULT_BUTTON_SIZE.x, 30.0));
    }
}
