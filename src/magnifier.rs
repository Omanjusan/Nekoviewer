//! 虫眼鏡（ホイール拡縮）モードの純粋ロジック。状態・変換・クランプ・設定値の検証付きAPI。
//! 倍率は「原寸比」で持つ（1.0 = 原寸デコードしたテクスチャの等倍。GUI設定の「原寸」と同じ意味）。
//! 入力・描画・デコード連動は別フェーズ。ここは egui::Vec2 以外に依存しない。

use egui::{Pos2, Rect, Vec2};

/// 原寸（100%）の倍率。
pub const ACTUAL_SCALE: f32 = 1.0;
/// 原寸付近へ吸着させる許容幅。浮動小数の丸め誤差でノッチが空振りするのを防ぐ。
const ACTUAL_SNAP_EPS: f32 = 1e-3;

pub const DEFAULT_MAX_SCALE: f32 = 4.0;
const MAX_SCALE_RANGE: (f32, f32) = (1.0, 32.0);

/// 縮小側の下限。フィット倍率に対する比（0.25 = フィット表示の25%まで縮められる）。
pub const DEFAULT_MIN_SHRINK_RATIO: f32 = 0.25;
const MIN_SHRINK_RATIO_RANGE: (f32, f32) = (0.05, 1.0);
/// コマ送りの基準倍率（高さフィット相対）の許容範囲。
const KOMA_HEIGHT_REL_RANGE: (f32, f32) = (0.05, 64.0);
/// コマ送り: 超過分の自動縮小のしきい値（窓の寸法に対する超過の％）の範囲・刻み・既定値。
pub const KOMA_SHRINK_PCT_FLOOR: u32 = 2;
pub const KOMA_SHRINK_PCT_CEILING: u32 = 30;
pub const KOMA_SHRINK_PCT_STEP: u32 = 2;
pub const DEFAULT_KOMA_SHRINK_PCT: u32 = 10;
/// フィット倍率ちょうどかの判定幅。窓サイズ変更への追従（フィット表示のまま）に使う。
pub const FIT_EPS: f32 = 1e-4;

pub const DEFAULT_AUTOHIDE_SECS: f32 = 2.0;
const AUTOHIDE_SECS_RANGE: (f32, f32) = (0.1, 30.0);
const AUTOHIDE_SECS_STEP: f32 = 0.1;

/// 1ノッチの倍率の候補。バー横のボタンで順に巡回する。既定は ×1.25。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotchStep {
    X1_1,
    #[default]
    X1_25,
    X1_5,
    X2,
}

impl NotchStep {
    pub fn ratio(self) -> f32 {
        match self {
            Self::X1_1 => 1.1,
            Self::X1_25 => 1.25,
            Self::X1_5 => 1.5,
            Self::X2 => 2.0,
        }
    }

    /// 次の候補（×2 の次は ×1.1 へ戻る）。
    pub fn next(self) -> Self {
        match self {
            Self::X1_1 => Self::X1_25,
            Self::X1_25 => Self::X1_5,
            Self::X1_5 => Self::X2,
            Self::X2 => Self::X1_1,
        }
    }

    /// ボタンの表示ラベル。
    pub fn label(self) -> &'static str {
        match self {
            Self::X1_1 => "×1.1",
            Self::X1_25 => "×1.25",
            Self::X1_5 => "×1.5",
            Self::X2 => "×2",
        }
    }

    /// stateファイルの値。一度リリースした値は変えない。
    pub fn to_state_str(self) -> &'static str {
        match self {
            Self::X1_1 => "1.1",
            Self::X1_25 => "1.25",
            Self::X1_5 => "1.5",
            Self::X2 => "2",
        }
    }

    pub fn from_state_str(s: &str) -> Option<Self> {
        match s.trim() {
            "1.1" => Some(Self::X1_1),
            "1.25" => Some(Self::X1_25),
            "1.5" => Some(Self::X1_5),
            "2" => Some(Self::X2),
            _ => None,
        }
    }
}

pub const DEFAULT_BAR_WIDTH_PCT: f32 = 20.0;
const BAR_WIDTH_PCT_RANGE: (f32, f32) = (10.0, 60.0);
const BAR_WIDTH_PCT_STEP: f32 = 0.5;
// 以下のサイズ既定は仮置き。フェーズ4の実描画で調整する。
const DEFAULT_BAR_BODY_HEIGHT: f32 = 36.0;
const BAR_BODY_HEIGHT_RANGE: (f32, f32) = (24.0, 96.0);
/// バー全体の内側余白・パーツ間の隙間(px)。
pub const BAR_PAD: f32 = 6.0;
/// ビューポート下端からバー下端までの余白(px)。
pub const BAR_BOTTOM_MARGIN: f32 = 24.0;
/// バー本体の最小幅(px)。これを割るほど狭い窓では、割合よりも最小幅を優先する。
const BAR_BODY_MIN_WIDTH: f32 = 80.0;
/// ホバーで再表示・イベント吸収の判定に使う、バー矩形の外側への拡張(px)。
pub const BAR_HOVER_SLOP: f32 = 8.0;
/// スライダーの100%吸着幅(px)。ドラッグで原寸ぴったりに合わせやすくする。
pub const BAR_ACTUAL_MAGNET_PX: f32 = 6.0;
/// 自動ハイドで薄くなるまでのフェード時間(秒)。
pub const BAR_FADE_SECS: f32 = 0.25;
/// バー本体の上段（目盛りの数値・現在値ラベル）の高さ(px)。トラックはその下。
pub const BAR_LABEL_H: f32 = 14.0;
/// トラック両端の余白(px)。つまみが端で切れないようにする。
const BAR_TRACK_INSET: f32 = 8.0;
const DEFAULT_BUTTON_SIZE: Vec2 = Vec2::new(44.0, 24.0);
/// モード終了ボタンは文字が長い（「モード終了」など）ので、少し広い。
const DEFAULT_EXIT_BUTTON_SIZE: Vec2 = Vec2::new(64.0, 24.0);
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
    exit_button: Vec2,
}

impl Default for BarLayout {
    fn default() -> Self {
        Self {
            width_pct: DEFAULT_BAR_WIDTH_PCT,
            body_height: DEFAULT_BAR_BODY_HEIGHT,
            step_button: DEFAULT_BUTTON_SIZE,
            detail_button: DEFAULT_BUTTON_SIZE,
            exit_button: DEFAULT_EXIT_BUTTON_SIZE,
        }
    }
}

impl BarLayout {
    /// 親: バー全体（本体＋2ボタン＋隙間）の幅。ビューアー描画領域の幅に対する割合(%)。
    pub fn width_pct(&self) -> f32 { self.width_pct }
    pub fn body_height(&self) -> f32 { self.body_height }
    pub fn step_button_size(&self) -> Vec2 { self.step_button }
    pub fn detail_button_size(&self) -> Vec2 { self.detail_button }
    pub fn exit_button_size(&self) -> Vec2 { self.exit_button }

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

    /// 子: モード終了ボタンのサイズ(px)。軸ごとに範囲へ丸める。
    pub fn set_exit_button_size(&mut self, size: Vec2) -> Vec2 {
        self.exit_button = clamp_size(size, self.exit_button);
        self.exit_button
    }

    /// ビューポートの下端中央に置くバーの各矩形を解決する。全体幅は親の割合を基準にするが、
    /// パーツが収まる最小幅は割らない（ただしビューポート幅は超えない）。
    /// `show_buttons` が false の間はボタンの領域を確保せず、全幅を本体に使う。
    pub fn resolve(&self, viewport: Rect, show_buttons: bool) -> BarRects {
        // 左から順に並べるボタン（ノッチ倍率・詳細簡易・モード終了）。
        let mut sizes = Vec::new();
        if show_buttons {
            sizes.push(self.step_button);
            sizes.push(self.detail_button);
            sizes.push(self.exit_button);
        }
        // 各ボタンの右に隙間が1つずつ（最後のボタンと本体の間を含む）。
        let buttons_w: f32 = sizes.iter().map(|s| s.x + BAR_PAD).sum();
        let min_total = BAR_PAD * 2.0 + BAR_BODY_MIN_WIDTH + buttons_w;
        let wanted = viewport.width() * self.width_pct / 100.0;
        let total_w = wanted.max(min_total).min(viewport.width().max(0.0));
        let buttons_h = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
        let total_h = BAR_PAD * 2.0 + self.body_height.max(buttons_h);

        let total = Rect::from_min_size(
            Pos2::new(
                viewport.center().x - total_w / 2.0,
                viewport.bottom() - BAR_BOTTOM_MARGIN - total_h,
            ),
            Vec2::new(total_w, total_h),
        );
        let inner = total.shrink(BAR_PAD);
        let centered = |x: f32, size: Vec2| {
            Rect::from_min_size(Pos2::new(x, inner.center().y - size.y / 2.0), size)
        };
        let mut x = inner.left();
        let mut button_rects = sizes.iter().map(|&size| {
            let rect = centered(x, size);
            x += size.x + BAR_PAD;
            rect
        });
        let step_button = button_rects.next();
        let detail_button = button_rects.next();
        let exit_button = button_rects.next();
        let body_left = x;
        let body = Rect::from_min_max(
            Pos2::new(body_left, inner.center().y - self.body_height / 2.0),
            Pos2::new(inner.right(), inner.center().y + self.body_height / 2.0),
        );
        BarRects { total, body, step_button, detail_button, exit_button }
    }
}

/// バー本体の矩形から、つまみが動くトラックの矩形を求める（上段のラベル行を除いた下段）。
pub fn track_rect(body: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(body.left() + BAR_TRACK_INSET, body.top() + BAR_LABEL_H),
        Pos2::new(body.right() - BAR_TRACK_INSET, body.bottom()),
    )
}

/// `BarLayout::resolve` の結果。ボタンは領域を確保していない間 None。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarRects {
    pub total: Rect,
    pub body: Rect,
    pub step_button: Option<Rect>,
    pub detail_button: Option<Rect>,
    pub exit_button: Option<Rect>,
}

fn clamp_size(size: Vec2, current: Vec2) -> Vec2 {
    Vec2::new(
        clamp_finite(size.x, BUTTON_SIZE_RANGE, current.x),
        clamp_finite(size.y, BUTTON_SIZE_RANGE, current.y),
    )
}

/// 虫眼鏡モードの設定値。setter は「範囲に丸めて適用し、適用後の値を返す」。
/// GUI設定へ昇格するときはこの setter を呼ぶだけで済む。
/// 永続化するのは `notch_step`・`detail_ticks`・`koma_height_rel`・コマ送りの自動縮小設定だけ（他は現状スコープ外）。
#[derive(Clone, Debug, PartialEq)]
pub struct MagnifierConfig {
    max_scale: f32,
    min_shrink_ratio: f32,
    autohide_secs: f32,
    notch_step: NotchStep,
    detail_ticks: bool,
    /// コマ送りの基準倍率（高さフィット相対。`koma::height_rel_from_scale`）。未設定なら None。
    koma_height_rel: Option<f32>,
    /// コマ送り: 軸ごとの超過分の自動縮小の有効フラグと、しきい値（窓の幅/高さに対する超過の％。2%刻み）。
    koma_shrink_x: bool,
    koma_shrink_y: bool,
    koma_shrink_x_pct: u32,
    koma_shrink_y_pct: u32,
    /// 自動縮小の確認ダイアログを表示しない（解除用のフラグ）。
    koma_shrink_hide_ask: bool,
    pub bar: BarLayout,
}

/// しきい値(％)を刻みへ丸め（四捨五入）、範囲へ収める。
fn round_shrink_pct(v: u32) -> u32 {
    let stepped = (v.saturating_add(KOMA_SHRINK_PCT_STEP / 2) / KOMA_SHRINK_PCT_STEP) * KOMA_SHRINK_PCT_STEP;
    stepped.clamp(KOMA_SHRINK_PCT_FLOOR, KOMA_SHRINK_PCT_CEILING)
}

impl Default for MagnifierConfig {
    fn default() -> Self {
        Self {
            max_scale: DEFAULT_MAX_SCALE,
            min_shrink_ratio: DEFAULT_MIN_SHRINK_RATIO,
            autohide_secs: DEFAULT_AUTOHIDE_SECS,
            notch_step: NotchStep::default(),
            detail_ticks: false,
            koma_height_rel: None,
            koma_shrink_x: false,
            koma_shrink_y: false,
            koma_shrink_x_pct: DEFAULT_KOMA_SHRINK_PCT,
            koma_shrink_y_pct: DEFAULT_KOMA_SHRINK_PCT,
            koma_shrink_hide_ask: false,
            bar: BarLayout::default(),
        }
    }
}

impl MagnifierConfig {
    /// 上限倍率（原寸比）。
    pub fn max_scale(&self) -> f32 { self.max_scale }
    /// 縮小側の下限（フィット倍率に対する比）。
    pub fn min_shrink_ratio(&self) -> f32 { self.min_shrink_ratio }
    /// フィット倍率 `fit` に対する倍率の許容範囲。
    pub fn scale_range(&self, fit: f32) -> (f32, f32) {
        scale_range(fit, self.min_shrink_ratio, self.max_scale)
    }
    /// スライダーバーの自動ハイドまでの秒数。
    pub fn autohide_secs(&self) -> f32 { self.autohide_secs }

    /// 1ノッチの倍率の候補。
    pub fn notch_step(&self) -> NotchStep { self.notch_step }
    /// 1ノッチの倍率。
    pub fn notch_ratio(&self) -> f32 { self.notch_step.ratio() }
    /// true = 目盛りを詳細（毎ノッチ）で表示、false = 簡易（原寸基準の×2ごと）。
    pub fn detail_ticks(&self) -> bool { self.detail_ticks }

    pub fn set_notch_step(&mut self, step: NotchStep) { self.notch_step = step; }

    /// 次の候補へ進め、適用後の値を返す。
    pub fn cycle_notch_step(&mut self) -> NotchStep {
        self.notch_step = self.notch_step.next();
        self.notch_step
    }

    pub fn set_detail_ticks(&mut self, detail: bool) { self.detail_ticks = detail; }

    /// 詳細／簡易を切り替え、適用後の値を返す。
    pub fn toggle_detail_ticks(&mut self) -> bool {
        self.detail_ticks = !self.detail_ticks;
        self.detail_ticks
    }

    /// コマ送りの基準倍率（高さフィット相対）。未設定なら None。
    pub fn koma_height_rel(&self) -> Option<f32> { self.koma_height_rel }

    /// 範囲に丸めて設定する。有限でない値は無視して現在値を返す。
    pub fn set_koma_height_rel(&mut self, v: f32) -> Option<f32> {
        if v.is_finite() {
            self.koma_height_rel = Some(v.clamp(KOMA_HEIGHT_REL_RANGE.0, KOMA_HEIGHT_REL_RANGE.1));
        }
        self.koma_height_rel
    }

    /// 横（X）の超過分を自動縮小するか。
    pub fn koma_shrink_x(&self) -> bool { self.koma_shrink_x }
    /// 縦（Y）の超過分を自動縮小するか。
    pub fn koma_shrink_y(&self) -> bool { self.koma_shrink_y }
    /// Xのしきい値（窓の幅に対する超過の％）。
    pub fn koma_shrink_x_pct(&self) -> u32 { self.koma_shrink_x_pct }
    /// Yのしきい値（窓の高さに対する超過の％）。
    pub fn koma_shrink_y_pct(&self) -> u32 { self.koma_shrink_y_pct }
    /// 自動縮小の確認ダイアログを表示しないか。
    pub fn koma_shrink_hide_ask(&self) -> bool { self.koma_shrink_hide_ask }

    pub fn set_koma_shrink_x(&mut self, on: bool) { self.koma_shrink_x = on; }
    pub fn set_koma_shrink_y(&mut self, on: bool) { self.koma_shrink_y = on; }
    pub fn set_koma_shrink_hide_ask(&mut self, hide: bool) { self.koma_shrink_hide_ask = hide; }

    /// 刻み（2%）へ丸め、2〜30%へ収めて設定し、適用後の値を返す。
    pub fn set_koma_shrink_x_pct(&mut self, pct: u32) -> u32 {
        self.koma_shrink_x_pct = round_shrink_pct(pct);
        self.koma_shrink_x_pct
    }

    /// 刻み（2%）へ丸め、2〜30%へ収めて設定し、適用後の値を返す。
    pub fn set_koma_shrink_y_pct(&mut self, pct: u32) -> u32 {
        self.koma_shrink_y_pct = round_shrink_pct(pct);
        self.koma_shrink_y_pct
    }

    pub fn set_min_shrink_ratio(&mut self, v: f32) -> f32 {
        self.min_shrink_ratio = clamp_finite(v, MIN_SHRINK_RATIO_RANGE, self.min_shrink_ratio);
        self.min_shrink_ratio
    }

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

/// 表示対象が変わったとみなす識別子。変わったらフィット表示から作り直す。
/// `mode` は呼び出し側が決める表示形式（0=単ページ）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MagnifierKey {
    pub page: i32,
    pub mode: u8,
    pub angle: i32,
}

impl MagnifierKey {
    /// 同じ表示形式・回転のまま、ページだけが変わったか（ページ送り。倍率を引き継ぐ対象）。
    pub fn is_page_turn_from(self, prev: MagnifierKey) -> bool {
        self.mode == prev.mode && self.angle == prev.angle && self.page != prev.page
    }
}

/// 虫眼鏡の表示対象。倍率（原寸比）の基準となる外接サイズと、識別子を持つ。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MagnifierTarget {
    /// 倍率1.0のときの、回転後の外接サイズ(px)。
    pub img: Vec2,
    /// テクスチャ差し替え（解像度変更）時に画面上の大きさを保つ換算の基準長さ
    /// （単ページ=テクスチャ高さ）。
    pub ref_len: f32,
    pub key: MagnifierKey,
}

/// `angle_deg` 度回転した矩形の外接サイズ。90/270度は縦横が入れ替わる。
pub fn rotated_extent(size: Vec2, angle_deg: i32) -> Vec2 {
    if angle_deg.rem_euclid(180) == 90 {
        Vec2::new(size.y, size.x)
    } else {
        size
    }
}

/// 単ページの表示対象。`tex_size` は回転前のテクスチャ寸法。
pub fn single_target(tex_size: Vec2, angle_deg: i32, page: i32) -> MagnifierTarget {
    MagnifierTarget {
        img: rotated_extent(tex_size, angle_deg),
        ref_len: tex_size.y,
        key: MagnifierKey { page, mode: 0, angle: angle_deg },
    }
}

/// 実ページが無い側（仮想ページ・未取得）のプレースホルダの縦横比。通常の見開き表示と同じ 1:√2。
const PLACEHOLDER_ASPECT: f32 = 1.0 / std::f32::consts::SQRT_2;

/// 見開き2ページの配置（高さ正規化）。通常のフィット表示と同じ見た目で、2ページを
/// 同じ高さに揃えて横に並べる。座標は基準ページのテクスチャpxで、左上が原点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpreadLayout {
    /// 基準ページの高さ(px)。実ページのうち高いほうのテクスチャ高さ。倍率1.0（原寸）はこの
    /// ページの等倍で、低解像側はこの高さへ拡大して並べる。
    pub ref_len: f32,
    pub left: Rect,
    pub right: Rect,
    /// 2ページ全体の外接サイズ。
    pub extent: Vec2,
}

/// 見開きの配置を求める。`left`/`right` はテクスチャ寸法（None=仮想ページ・未取得で、
/// プレースホルダ幅を確保する）。両方 None なら None。
pub fn spread_layout(left: Option<Vec2>, right: Option<Vec2>) -> Option<SpreadLayout> {
    let valid = |s: &Vec2| s.x > 0.0 && s.y > 0.0 && s.x.is_finite() && s.y.is_finite();
    let (left, right) = (left.filter(valid), right.filter(valid));
    let ref_len = match (left, right) {
        (None, None) => return None,
        (Some(l), None) => l.y,
        (None, Some(r)) => r.y,
        (Some(l), Some(r)) => l.y.max(r.y),
    };
    let width = |s: Option<Vec2>| s.map_or(PLACEHOLDER_ASPECT, |s| s.x / s.y) * ref_len;
    let (w_l, w_r) = (width(left), width(right));
    Some(SpreadLayout {
        ref_len,
        left: Rect::from_min_size(Pos2::ZERO, Vec2::new(w_l, ref_len)),
        right: Rect::from_min_size(Pos2::new(w_l, 0.0), Vec2::new(w_r, ref_len)),
        extent: Vec2::new(w_l + w_r, ref_len),
    })
}

/// 見開きの1ページの置き場所。`half` は回転前のページ半サイズ(画面px)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePlacement {
    pub center: Pos2,
    pub half: Vec2,
}

/// 見開きを1つの剛体として `angle_deg` 度回転したときの、左右ページの中心（画面座標）と半サイズ。
/// `bbox_center` は回転後の外接矩形の中心。`scale` は原寸比（基準ページ1pxあたりの画面px）。
/// 回転は画面座標（y下向き）で時計回り。
pub fn spread_placements(
    layout: &SpreadLayout,
    scale: f32,
    angle_deg: i32,
    bbox_center: Pos2,
) -> [PagePlacement; 2] {
    let rot = egui::emath::Rot2::from_angle((angle_deg as f32).to_radians());
    let footprint_center = layout.extent / 2.0;
    [layout.left, layout.right].map(|rect| PagePlacement {
        center: bbox_center + rot * ((rect.center().to_vec2() - footprint_center) * scale),
        half: rect.size() * scale / 2.0,
    })
}

/// 見開きの表示対象。`mode` は呼び出し側が決める見開きの向き（単ページと区別する値）。
pub fn spread_target(
    left: Option<Vec2>,
    right: Option<Vec2>,
    angle_deg: i32,
    page: i32,
    mode: u8,
) -> Option<MagnifierTarget> {
    let layout = spread_layout(left, right)?;
    Some(MagnifierTarget {
        img: rotated_extent(layout.extent, angle_deg),
        ref_len: layout.ref_len,
        key: MagnifierKey { page, mode, angle: angle_deg },
    })
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

/// 倍率の許容範囲 (下限, 上限)。下限は `フィット倍率 × min_ratio`（縮小側の余地）。
/// フィットが上限を超える小さな画像でも、フィット自体は表現できるよう、上限は少なくとも
/// フィット倍率にする。
pub fn scale_range(fit: f32, min_ratio: f32, max_scale: f32) -> (f32, f32) {
    (fit * min_ratio.clamp(MIN_SHRINK_RATIO_RANGE.0, MIN_SHRINK_RATIO_RANGE.1), max_scale.max(fit))
}

/// 拡縮の停止点（原寸とフィット）。ノッチもスライダーも、ここで一度ぴったり止まる。
pub fn snap_stops(fit: f32) -> [f32; 2] {
    [ACTUAL_SCALE, fit]
}

/// 現在倍率から `notches` ノッチ（正=拡大）進めた倍率を返す。1ノッチは `ratio` 倍。
/// `stops`（原寸・フィットなど）をまたぐ動きは、最初に出会う停止点ぴったりで止め、その後範囲へ丸める。
/// 停止点ちょうどにいるときは、そこから離れる動きを止めない。
pub fn notch_scale(current: f32, notches: f32, ratio: f32, range: (f32, f32), stops: &[f32]) -> f32 {
    let current = current.clamp(range.0, range.1);
    if !(notches.is_finite() && ratio.is_finite() && ratio > 0.0) {
        return current;
    }
    let next = current * ratio.powf(notches);
    let hit = stops
        .iter()
        .copied()
        .filter(|s| s.is_finite() && (current - s).abs() >= ACTUAL_SNAP_EPS)
        .filter(|&s| (current < s && next > s) || (current > s && next < s) || (next - s).abs() < ACTUAL_SNAP_EPS)
        .min_by(|a, b| (a - current).abs().total_cmp(&(b - current).abs()));
    hit.unwrap_or(next).clamp(range.0, range.1)
}

/// テクスチャが別解像度に差し替わったとき、画面上の大きさを保つ倍率を返す
/// （倍率はテクスチャのピクセル基準なので、幅の比で換算する）。寸法が不正なら据え置き。
pub fn rescale_for_new_texture(scale: f32, old_width: f32, new_width: f32) -> f32 {
    if old_width > 0.0 && new_width > 0.0 && old_width.is_finite() && new_width.is_finite() {
        scale * old_width / new_width
    } else {
        scale
    }
}

/// 倍率 → スライダー上の位置 t（0..=1）。範囲内で対数的に割り当てる。範囲が潰れているときは 0。
pub fn scale_to_t(scale: f32, range: (f32, f32)) -> f32 {
    let (lo, hi) = range;
    if !(lo > 0.0 && hi > lo && scale > 0.0) {
        return 0.0;
    }
    ((scale / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
}

/// スライダー上の位置 t（0..=1）→ 倍率。`scale_to_t` の逆変換。
pub fn t_to_scale(t: f32, range: (f32, f32)) -> f32 {
    let (lo, hi) = range;
    if !(lo > 0.0 && hi > lo) {
        return lo;
    }
    lo * (hi / lo).powf(t.clamp(0.0, 1.0))
}

/// トラック上の x 座標（`track` = 左端・右端）から倍率を求める。範囲内の停止点（原寸・フィット）の
/// 位置から `magnet_px` 以内なら、その停止点ぴったりへ吸着する（複数なら近い方）。
pub fn slider_scale_at(x: f32, track: (f32, f32), range: (f32, f32), magnet_px: f32, stops: &[f32]) -> f32 {
    let width = track.1 - track.0;
    if !(width > 0.0) {
        return range.0;
    }
    let snapped = stops
        .iter()
        .copied()
        .filter(|s| range.0 < *s && *s < range.1)
        .map(|s| (s, (x - (track.0 + scale_to_t(s, range) * width)).abs()))
        .filter(|(_, dist)| *dist <= magnet_px)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s);
    snapped.unwrap_or_else(|| t_to_scale((x - track.0) / width, range))
}

/// 目盛りの倍率（昇順）。簡易は原寸を基準に ×2 ごと、詳細は原寸を基準に 1ノッチ(`ratio`)ごと。
/// 範囲の両端ちょうどの目盛りも含める。
pub fn tick_scales(range: (f32, f32), detail: bool, ratio: f32) -> Vec<f32> {
    let (lo, hi) = range;
    let base = if detail { ratio } else { 2.0 };
    if !(lo > 0.0 && hi > lo && base.is_finite() && base > 1.0) {
        return Vec::new();
    }
    let eps = 1e-4;
    let k_min = (lo.ln() / base.ln()).floor() as i32;
    let k_max = (hi.ln() / base.ln()).ceil() as i32;
    (k_min..=k_max)
        .map(|k| base.powi(k))
        .filter(|v| *v >= lo * (1.0 - eps) && *v <= hi * (1.0 + eps))
        .collect()
}

/// 「125%」のような原寸比の表示文字列。
pub fn format_percent(scale: f32) -> String {
    format!("{}%", (scale * 100.0).round() as i32)
}

/// バーの不透明度（0..=1）。ホバー中・操作直後は1、`autohide_secs` を過ぎたら
/// `BAR_FADE_SECS` かけて0へ落とす。
pub fn bar_alpha(idle_secs: f32, autohide_secs: f32, hovered: bool) -> f32 {
    if hovered || idle_secs <= autohide_secs {
        return 1.0;
    }
    (1.0 - (idle_secs - autohide_secs) / BAR_FADE_SECS).clamp(0.0, 1.0)
}

/// ページ送りで倍率を引き継いだ、新ページの表示状態。倍率はフィット相対（`rel` × 新ページの
/// フィット倍率）を範囲内へ丸めたもの。解像度やページの大きさが違っても、同じ拡大感になる。
/// 開始位置は上端揃いで、横は `start_at_right` なら右端、そうでなければ左端（進行方向と逆の端）。
pub fn carried_view(
    rel: f32,
    fit: f32,
    range: (f32, f32),
    viewport: Vec2,
    img: Vec2,
    start_at_right: bool,
) -> MagnifierView {
    let scale = if rel.is_finite() && rel > 0.0 {
        (rel * fit).clamp(range.0, range.1)
    } else {
        fit.clamp(range.0, range.1)
    };
    let content = img * scale;
    let max_x = (content.x - viewport.x).max(0.0);
    let x = if start_at_right { max_x } else { 0.0 };
    MagnifierView { scale, offset: Vec2::new(x, 0.0) }
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
    fn koma_shrink_defaults_are_off_with_ten_percent() {
        let m = MagnifierConfig::default();
        assert!(!m.koma_shrink_x() && !m.koma_shrink_y() && !m.koma_shrink_hide_ask());
        assert_eq!((m.koma_shrink_x_pct(), m.koma_shrink_y_pct()), (10, 10));
    }

    #[test]
    fn koma_shrink_pct_rounds_to_the_step_and_clamps() {
        let mut m = MagnifierConfig::default();
        for (input, expected) in [(0, 2), (1, 2), (2, 2), (3, 4), (5, 6), (10, 10), (11, 12), (29, 30), (30, 30), (31, 30), (u32::MAX, 30)] {
            assert_eq!(m.set_koma_shrink_x_pct(input), expected, "x: {input}");
            assert_eq!(m.koma_shrink_x_pct(), expected);
            assert_eq!(m.set_koma_shrink_y_pct(input), expected, "y: {input}");
        }
        // 刻みに乗った全値が、そのまま保たれる。
        for pct in (KOMA_SHRINK_PCT_FLOOR..=KOMA_SHRINK_PCT_CEILING).step_by(KOMA_SHRINK_PCT_STEP as usize) {
            assert_eq!(m.set_koma_shrink_x_pct(pct), pct);
        }
    }

    #[test]
    fn koma_shrink_flags_are_independent_per_axis() {
        let mut m = MagnifierConfig::default();
        m.set_koma_shrink_x(true);
        assert!(m.koma_shrink_x() && !m.koma_shrink_y());
        m.set_koma_shrink_y(true);
        m.set_koma_shrink_x(false);
        assert!(!m.koma_shrink_x() && m.koma_shrink_y());
        m.set_koma_shrink_hide_ask(true);
        assert!(m.koma_shrink_hide_ask());
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
        // 下限はフィットの min_ratio 倍、上限は max_scale。
        assert_eq!(scale_range(0.4, 0.25, 4.0), (0.1, 4.0));
        // 比が1ならフィットが下限（縮小なし）。
        assert_eq!(scale_range(0.4, 1.0, 4.0), (0.4, 4.0));
        // フィットが上限を超えても、範囲が反転しない（フィット自体は範囲内）。
        let (lo, hi) = scale_range(10.0, 0.25, 4.0);
        assert_eq!((lo, hi), (2.5, 10.0));
        // 不正な比は許容範囲へ丸める。
        assert_eq!(scale_range(1.0, 0.0, 4.0).0, MIN_SHRINK_RATIO_RANGE.0);
        assert_eq!(scale_range(1.0, 9.0, 4.0).0, 1.0);
    }

    #[test]
    fn config_shrink_ratio_defaults_to_a_quarter_and_clamps() {
        let mut c = MagnifierConfig::default();
        assert_eq!(c.min_shrink_ratio(), 0.25);
        assert_eq!(c.scale_range(0.4), (0.1, 4.0));
        assert_eq!(c.set_min_shrink_ratio(0.5), 0.5);
        assert_eq!(c.set_min_shrink_ratio(0.0), 0.05);
        assert_eq!(c.set_min_shrink_ratio(3.0), 1.0);
        assert_eq!(c.set_min_shrink_ratio(f32::NAN), 1.0);
    }

    #[test]
    fn notch_steps_multiply_and_clamp() {
        let range = (0.4, 4.0);
        assert!((notch_scale(1.25, 1.0, 1.25, range, &[ACTUAL_SCALE]) - 1.5625).abs() < 1e-5);
        assert!((notch_scale(1.25, -1.0, 1.25, range, &[ACTUAL_SCALE]) - 1.0).abs() < 1e-5);
        assert_eq!(notch_scale(3.5, 1.0, 1.25, range, &[ACTUAL_SCALE]), 4.0);
        assert_eq!(notch_scale(0.45, -1.0, 1.25, range, &[ACTUAL_SCALE]), 0.4);
        // 0ノッチは現在値（範囲内へ丸め）。
        assert_eq!(notch_scale(2.0, 0.0, 1.25, range, &[ACTUAL_SCALE]), 2.0);
        // 不正な入力は現在値のまま。
        assert_eq!(notch_scale(2.0, f32::NAN, 1.25, range, &[ACTUAL_SCALE]), 2.0);
        assert_eq!(notch_scale(2.0, 1.0, 0.0, range, &[ACTUAL_SCALE]), 2.0);
    }

    #[test]
    fn notch_stops_at_actual_size() {
        let range = (0.3, 4.0);
        // 拡大でまたぐ・縮小でまたぐ、どちらも100%で止まる。
        assert_eq!(notch_scale(0.9, 1.0, 1.25, range, &[ACTUAL_SCALE]), ACTUAL_SCALE);
        assert_eq!(notch_scale(1.1, -1.0, 1.25, range, &[ACTUAL_SCALE]), ACTUAL_SCALE);
        // 100%ちょうどからは止まらずに進む。
        assert!((notch_scale(ACTUAL_SCALE, 1.0, 1.25, range, &[ACTUAL_SCALE]) - 1.25).abs() < 1e-5);
        assert!((notch_scale(ACTUAL_SCALE, -1.0, 1.25, range, &[ACTUAL_SCALE]) - 0.8).abs() < 1e-5);
        // 丸め誤差で100%のごく近傍になった場合も100%へ揃う。
        assert_eq!(notch_scale(1.25, -1.0, 1.25, range, &[ACTUAL_SCALE]), ACTUAL_SCALE);
    }

    #[test]
    fn notch_stops_at_fit_going_down_and_up_then_continues_below() {
        // フィット0.4、縮小の余地あり（下限0.1）。原寸(1.0)とフィットの2停止点。
        let fit = 0.4;
        let range = (0.1, 4.0);
        let stops = [ACTUAL_SCALE, fit];
        // フィットのすぐ上から縮小 → フィットぴったりで止まる。
        assert_eq!(notch_scale(0.45, -1.0, 1.25, range, &stops), fit);
        // フィットのすぐ下から拡大 → フィットぴったりで止まる。
        assert_eq!(notch_scale(0.35, 1.0, 1.25, range, &stops), fit);
        // フィットちょうどからは止まらずに縮小へ進む・拡大へ進む。
        assert!((notch_scale(fit, -1.0, 1.25, range, &stops) - fit * 0.8).abs() < 1e-5);
        assert!((notch_scale(fit, 1.0, 1.25, range, &stops) - fit * 1.25).abs() < 1e-5);
        // 下限で止まる。
        assert_eq!(notch_scale(0.11, -1.0, 1.25, range, &stops), 0.1);
        // 拡大側の原寸停止も従来どおり。
        assert_eq!(notch_scale(0.9, 1.0, 1.25, range, &stops), ACTUAL_SCALE);
        // 2つの停止点をまたぐ大きな動きは、最初に出会う方（近い方）で止まる。
        assert_eq!(notch_scale(0.3, 10.0, 1.25, range, &stops), fit);
        assert_eq!(notch_scale(2.0, -10.0, 1.25, range, &stops), ACTUAL_SCALE);
    }

    #[test]
    fn notch_when_actual_is_out_of_range() {
        // 小さい画像: フィットが原寸を超える。100%は範囲外なので下限で止まる。
        let range = (1.67, 4.0);
        assert_eq!(notch_scale(1.7, -2.0, 1.25, range, &[ACTUAL_SCALE]), 1.67);
        assert!((notch_scale(1.67, 1.0, 1.25, range, &[ACTUAL_SCALE]) - 1.67 * 1.25).abs() < 1e-4);
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
    fn rescale_keeps_on_screen_size() {
        // 幅1200pxのテクスチャを 0.8倍で表示中 → 幅2800pxへ差し替え。表示幅は 960px のまま。
        let scale = rescale_for_new_texture(0.8, 1200.0, 2800.0);
        assert!((2800.0 * scale - 960.0).abs() < 1e-3);
        // 不正な寸法は据え置き。
        assert_eq!(rescale_for_new_texture(0.8, 0.0, 2800.0), 0.8);
        assert_eq!(rescale_for_new_texture(0.8, 1200.0, f32::NAN), 0.8);
    }

    #[test]
    fn slider_position_is_logarithmic_and_invertible() {
        let range = (0.25, 4.0);
        assert_eq!(scale_to_t(0.25, range), 0.0);
        assert_eq!(scale_to_t(4.0, range), 1.0);
        // 100% は対数の中点（0.25〜4.0 の幾何平均）。
        assert!((scale_to_t(1.0, range) - 0.5).abs() < 1e-6);
        for scale in [0.3, 0.8, 1.0, 2.5, 3.9] {
            let back = t_to_scale(scale_to_t(scale, range), range);
            assert!((back - scale).abs() < 1e-4, "{scale} -> {back}");
        }
        // 範囲外・不正な範囲。
        assert_eq!(scale_to_t(10.0, range), 1.0);
        assert_eq!(scale_to_t(1.0, (2.0, 2.0)), 0.0);
        assert_eq!(t_to_scale(0.5, (2.0, 2.0)), 2.0);
    }

    #[test]
    fn slider_snaps_to_actual_size_near_its_mark() {
        let range = (0.25, 4.0);
        let track = (100.0, 500.0); // 100% は中点 x=300
        assert_eq!(slider_scale_at(303.0, track, range, 6.0, &[ACTUAL_SCALE]), ACTUAL_SCALE);
        assert_eq!(slider_scale_at(295.0, track, range, 6.0, &[ACTUAL_SCALE]), ACTUAL_SCALE);
        assert_ne!(slider_scale_at(320.0, track, range, 6.0, &[ACTUAL_SCALE]), ACTUAL_SCALE);
        // 端は範囲端、トラック外は丸め。
        assert!((slider_scale_at(100.0, track, range, 6.0, &[ACTUAL_SCALE]) - 0.25).abs() < 1e-5);
        assert!((slider_scale_at(900.0, track, range, 6.0, &[ACTUAL_SCALE]) - 4.0).abs() < 1e-4);
        // 100%が範囲外（小さい画像）なら吸着しない。
        let small = (1.67, 4.0);
        assert!((slider_scale_at(100.0, track, small, 6.0, &[ACTUAL_SCALE]) - 1.67).abs() < 1e-4);
    }

    #[test]
    fn slider_snaps_to_fit_as_well_as_actual() {
        let fit = 0.5;
        let range = (0.125, 4.0);
        let track = (100.0, 500.0);
        let stops = snap_stops(fit);
        let fit_x = 100.0 + scale_to_t(fit, range) * 400.0;
        assert_eq!(slider_scale_at(fit_x + 4.0, track, range, 6.0, &stops), fit);
        assert_eq!(slider_scale_at(fit_x - 4.0, track, range, 6.0, &stops), fit);
        assert_ne!(slider_scale_at(fit_x + 20.0, track, range, 6.0, &stops), fit);
        // 原寸の吸着も残る。
        let actual_x = 100.0 + scale_to_t(ACTUAL_SCALE, range) * 400.0;
        assert_eq!(slider_scale_at(actual_x + 3.0, track, range, 6.0, &stops), ACTUAL_SCALE);
        // 縮小側（フィットより左）へも動かせる。
        assert!(slider_scale_at(fit_x - 40.0, track, range, 6.0, &stops) < fit);
        assert!((slider_scale_at(100.0, track, range, 6.0, &stops) - 0.125).abs() < 1e-5);
    }

    #[test]
    fn simple_ticks_are_powers_of_two_around_actual_size() {
        assert_eq!(tick_scales((0.4, 4.0), false, 1.25), vec![0.5, 1.0, 2.0, 4.0]);
        assert_eq!(tick_scales((1.67, 4.0), false, 1.25), vec![2.0, 4.0]);
        assert!(tick_scales((2.0, 2.0), false, 1.25).is_empty());
    }

    #[test]
    fn detail_ticks_follow_notch_ratio() {
        let ticks = tick_scales((0.4, 4.0), true, 1.25);
        assert!(ticks.contains(&1.0));
        assert!(ticks.windows(2).all(|w| w[0] < w[1]));
        assert!((ticks[ticks.len() - 1] / ticks[ticks.len() - 2] - 1.25).abs() < 1e-4);
        assert!(ticks.iter().all(|t| *t >= 0.4 * 0.999 && *t <= 4.0 * 1.001));
        // 不正な比率は目盛りなし。
        assert!(tick_scales((0.4, 4.0), true, 1.0).is_empty());
    }

    #[test]
    fn percent_text_rounds_to_integer() {
        assert_eq!(format_percent(1.0), "100%");
        assert_eq!(format_percent(1.256), "126%");
        assert_eq!(format_percent(0.4166), "42%");
    }

    #[test]
    fn bar_alpha_fades_after_idle_time() {
        assert_eq!(bar_alpha(0.0, 2.0, false), 1.0);
        assert_eq!(bar_alpha(2.0, 2.0, false), 1.0);
        let mid = bar_alpha(2.0 + BAR_FADE_SECS / 2.0, 2.0, false);
        assert!((mid - 0.5).abs() < 1e-4);
        assert_eq!(bar_alpha(2.0 + BAR_FADE_SECS + 1.0, 2.0, false), 0.0);
        // ホバー中は消えない。
        assert_eq!(bar_alpha(99.0, 2.0, true), 1.0);
    }

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(1920.0, 1000.0))
    }

    #[test]
    fn bar_is_centered_at_bottom_with_default_width() {
        let r = BarLayout::default().resolve(viewport(), false);
        assert!((r.total.width() - 1920.0 * 0.2).abs() < 1e-3);
        assert!((r.total.center().x - viewport().center().x).abs() < 1e-3);
        assert!((viewport().bottom() - BAR_BOTTOM_MARGIN - r.total.bottom()).abs() < 1e-3);
        // ボタン領域を確保しない間は、本体が全幅（内側余白を除く）。
        assert!((r.body.width() - (r.total.width() - BAR_PAD * 2.0)).abs() < 1e-3);
        assert!(r.step_button.is_none() && r.detail_button.is_none());
    }

    #[test]
    fn bar_width_follows_parent_percent() {
        let mut layout = BarLayout::default();
        layout.set_width_pct(40.0);
        let r = layout.resolve(viewport(), false);
        assert!((r.total.width() - 1920.0 * 0.4).abs() < 1e-3);
    }

    #[test]
    fn buttons_sit_left_of_body_inside_total() {
        let r = BarLayout::default().resolve(viewport(), true);
        let (step, detail) = (r.step_button.unwrap(), r.detail_button.unwrap());
        for rect in [step, detail, r.body] {
            assert!(r.total.contains_rect(rect), "{rect:?} not in {:?}", r.total);
        }
        assert!(step.right() <= detail.left());
        assert!(detail.right() <= r.body.left());
        assert!((step.center().y - r.body.center().y).abs() < 1e-3);
        assert!(r.body.width() > 0.0);
    }

    #[test]
    fn track_sits_below_label_row_inside_body() {
        let r = BarLayout::default().resolve(viewport(), false);
        let track = track_rect(r.body);
        assert!(r.body.contains_rect(track));
        assert!((track.top() - r.body.top() - BAR_LABEL_H).abs() < 1e-3);
        assert!(track.width() > 0.0 && track.height() > 0.0);
    }

    #[test]
    fn exit_button_is_added_after_the_other_buttons() {
        let layout = BarLayout::default();
        let with = layout.resolve(viewport(), true);
        let (step, detail, exit) = (with.step_button.unwrap(), with.detail_button.unwrap(), with.exit_button.unwrap());
        for rect in [step, detail, exit, with.body] {
            assert!(with.total.contains_rect(rect), "{rect:?} not in {:?}", with.total);
        }
        assert!(detail.right() <= exit.left());
        assert!(exit.right() <= with.body.left());
        assert!((exit.center().y - with.body.center().y).abs() < 1e-3);
        // ボタンを出さない間は、終了ボタンも出ず、全幅が本体になる。
        let without = layout.resolve(viewport(), false);
        assert!(without.exit_button.is_none());
        // 全体幅は割合のまま、ボタンが増えたぶんだけ本体が狭くなる。
        assert!((with.total.width() - without.total.width()).abs() < 1e-3);
        assert!(with.body.width() < without.body.width());
    }

    #[test]
    fn narrow_viewport_keeps_parts_usable_without_overflowing() {
        // 割合だけだと本体が潰れる幅: 最小幅を優先する。
        let narrow = Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 400.0));
        let r = BarLayout::default().resolve(narrow, true);
        assert!(r.body.width() >= BAR_BODY_MIN_WIDTH - 1e-3);
        assert!(r.total.width() <= narrow.width() + 1e-3);
        // ビューポート自体が最小幅より狭い場合は、ビューポート幅までに収める。
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(120.0, 400.0));
        let r = BarLayout::default().resolve(tiny, true);
        assert!(r.total.width() <= tiny.width() + 1e-3);
    }

    #[test]
    fn rotated_extent_swaps_only_for_quarter_turns() {
        let size = v(400.0, 600.0);
        assert_eq!(rotated_extent(size, 0), size);
        assert_eq!(rotated_extent(size, 180), size);
        assert_eq!(rotated_extent(size, 90), v(600.0, 400.0));
        assert_eq!(rotated_extent(size, 270), v(600.0, 400.0));
        assert_eq!(rotated_extent(size, -90), v(600.0, 400.0));
        assert_eq!(rotated_extent(size, 450), v(600.0, 400.0));
    }

    #[test]
    fn single_target_uses_rotated_extent_and_unrotated_height() {
        let t = single_target(v(400.0, 600.0), 90, 7);
        assert_eq!(t.img, v(600.0, 400.0));
        assert_eq!(t.ref_len, 600.0);
        assert_eq!(t.key, MagnifierKey { page: 7, mode: 0, angle: 90 });
        // 回転・ページが変われば識別子が変わる（フィットから作り直す合図）。
        assert_ne!(t.key, single_target(v(400.0, 600.0), 0, 7).key);
        assert_ne!(t.key, single_target(v(400.0, 600.0), 90, 8).key);
        // 同じ対象なら解像度が変わっても識別子は同じ（見た目維持の換算に回る）。
        assert_eq!(t.key, single_target(v(800.0, 1200.0), 90, 7).key);
    }

    #[test]
    fn spread_layout_places_equal_pages_side_by_side() {
        let l = spread_layout(Some(v(1000.0, 1500.0)), Some(v(1000.0, 1500.0))).unwrap();
        assert_eq!(l.ref_len, 1500.0);
        assert_eq!(l.left.min, Pos2::ZERO);
        assert_eq!(l.left.size(), v(1000.0, 1500.0));
        assert_eq!(l.right.min, Pos2::new(1000.0, 0.0));
        assert_eq!(l.extent, v(2000.0, 1500.0));
    }

    #[test]
    fn spread_layout_normalizes_heights_to_the_taller_page() {
        // 右ページは低解像（高さ1000）。基準は高いほう（1500）で、右は幅750へ拡大して並べる。
        let l = spread_layout(Some(v(1000.0, 1500.0)), Some(v(500.0, 1000.0))).unwrap();
        assert_eq!(l.ref_len, 1500.0);
        assert_eq!(l.left.width(), 1000.0);
        assert_eq!(l.right.width(), 750.0);
        assert_eq!(l.right.height(), 1500.0);
        assert_eq!(l.right.min.x, l.left.max.x);
        assert_eq!(l.extent, v(1750.0, 1500.0));
    }

    #[test]
    fn spread_layout_uses_placeholder_for_missing_page() {
        let l = spread_layout(None, Some(v(1000.0, 1500.0))).unwrap();
        assert_eq!(l.ref_len, 1500.0);
        assert!((l.left.width() - 1500.0 / std::f32::consts::SQRT_2).abs() < 1e-3);
        assert_eq!(l.right.width(), 1000.0);
        // 無効な寸法もプレースホルダ扱い。
        let l2 = spread_layout(Some(v(0.0, 100.0)), Some(v(1000.0, 1500.0))).unwrap();
        assert_eq!(l2.left, l.left);
        // 両方無ければ対象なし。
        assert!(spread_layout(None, None).is_none());
    }

    #[test]
    fn spread_target_uses_layout_extent_and_key() {
        let t = spread_target(Some(v(1000.0, 1500.0)), Some(v(1000.0, 1500.0)), 0, 4, 1).unwrap();
        assert_eq!(t.img, v(2000.0, 1500.0));
        assert_eq!(t.ref_len, 1500.0);
        assert_eq!(t.key, MagnifierKey { page: 4, mode: 1, angle: 0 });
        // 向き（mode）が違えば識別子が違う。
        let t2 = spread_target(Some(v(1000.0, 1500.0)), Some(v(1000.0, 1500.0)), 0, 4, 2).unwrap();
        assert_ne!(t.key, t2.key);
        // 片方だけ解像度が上がっても基準長さだけが変わり、識別子は同じ（見た目維持の換算に回る）。
        let t3 = spread_target(Some(v(2000.0, 3000.0)), Some(v(1000.0, 1500.0)), 0, 4, 1).unwrap();
        assert_eq!(t.key, t3.key);
        assert_eq!(t3.ref_len, 3000.0);
        assert!(spread_target(None, None, 0, 0, 1).is_none());
    }

    #[test]
    fn spread_placements_without_rotation_match_the_layout() {
        let layout = spread_layout(Some(v(1000.0, 1500.0)), Some(v(1000.0, 1500.0))).unwrap();
        let [l, r] = spread_placements(&layout, 0.5, 0, Pos2::new(400.0, 300.0));
        // 左ページ中心は外接中心から x=-500px（基準px）ぶん左、0.5倍で -250。
        assert!(close(l.center.to_vec2(), v(150.0, 300.0)));
        assert!(close(r.center.to_vec2(), v(650.0, 300.0)));
        assert_eq!(l.half, v(250.0, 375.0));
    }

    #[test]
    fn spread_placements_rotate_as_one_rigid_body() {
        let layout = spread_layout(Some(v(1000.0, 1500.0)), Some(v(1000.0, 1500.0))).unwrap();
        let c = Pos2::new(400.0, 300.0);
        // 時計回り90度: 左ページは上、右ページは下へ回る。
        let [l, r] = spread_placements(&layout, 1.0, 90, c);
        assert!(close(l.center.to_vec2(), v(400.0, -200.0)), "{:?}", l.center);
        assert!(close(r.center.to_vec2(), v(400.0, 800.0)), "{:?}", r.center);
        // 180度: 左右が入れ替わる。
        let [l, r] = spread_placements(&layout, 1.0, 180, c);
        assert!(close(l.center.to_vec2(), v(900.0, 300.0)));
        assert!(close(r.center.to_vec2(), v(-100.0, 300.0)));
        // 270度: 左ページは下、右ページは上。
        let [l, r] = spread_placements(&layout, 1.0, 270, c);
        assert!(close(l.center.to_vec2(), v(400.0, 800.0)));
        assert!(close(r.center.to_vec2(), v(400.0, -200.0)));
        // 半サイズは回転前のまま。
        assert_eq!(l.half, v(500.0, 750.0));
        // 2ページの中心間距離は回転しても変わらない。
        assert!(((l.center - r.center).length() - 1000.0).abs() < 1e-2);
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
    fn notch_step_cycles_through_all_and_wraps() {
        let mut step = NotchStep::default();
        assert_eq!(step, NotchStep::X1_25);
        let mut seen = Vec::new();
        for _ in 0..4 {
            step = step.next();
            seen.push(step.ratio());
        }
        assert_eq!(seen, vec![1.5, 2.0, 1.1, 1.25]);
    }

    #[test]
    fn notch_step_state_strings_roundtrip_and_reject_unknown() {
        for step in [NotchStep::X1_1, NotchStep::X1_25, NotchStep::X1_5, NotchStep::X2] {
            assert_eq!(NotchStep::from_state_str(step.to_state_str()), Some(step));
        }
        assert_eq!(NotchStep::from_state_str(" 1.5 "), Some(NotchStep::X1_5));
        assert_eq!(NotchStep::from_state_str("3"), None);
        assert_eq!(NotchStep::from_state_str(""), None);
    }

    #[test]
    fn page_turn_is_a_page_change_with_same_layout_and_angle() {
        let base = MagnifierKey { page: 3, mode: 1, angle: 90 };
        assert!(MagnifierKey { page: 5, ..base }.is_page_turn_from(base));
        assert!(!MagnifierKey { page: 3, ..base }.is_page_turn_from(base));
        // 表示形式・回転が変わったらページ送りではない（フィットから作り直す）。
        assert!(!MagnifierKey { page: 5, mode: 2, angle: 90 }.is_page_turn_from(base));
        assert!(!MagnifierKey { page: 5, mode: 1, angle: 0 }.is_page_turn_from(base));
    }

    #[test]
    fn carried_view_keeps_fit_relative_zoom() {
        let vp = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        let fit = fit_scale(vp, img); // 0.2
        let range = scale_range(fit, 1.0, 4.0);
        let view = carried_view(2.0, fit, range, vp, img, false);
        assert!((view.scale - 2.0 * fit).abs() < 1e-6);
        // 別ページ（フィット倍率が違う）でも、同じ相対倍率になる。
        let img2 = v(1000.0, 1500.0);
        let fit2 = fit_scale(vp, img2); // 0.4
        let view2 = carried_view(2.0, fit2, scale_range(fit2, 1.0, 4.0), vp, img2, false);
        assert!((view2.scale / fit2 - 2.0).abs() < 1e-5);
    }

    #[test]
    fn carried_view_clamps_scale_and_handles_bad_input() {
        let vp = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        let fit = fit_scale(vp, img);
        let range = scale_range(fit, 1.0, 1.0);
        assert_eq!(carried_view(100.0, fit, range, vp, img, false).scale, 1.0);
        assert_eq!(carried_view(0.5, fit, range, vp, img, false).scale, fit);
        assert_eq!(carried_view(f32::NAN, fit, range, vp, img, false).scale, fit);
        assert_eq!(carried_view(-1.0, fit, range, vp, img, false).scale, fit);
    }

    #[test]
    fn carried_view_starts_top_aligned_on_the_side_opposite_to_travel() {
        let vp = v(800.0, 600.0);
        let img = v(2000.0, 3000.0);
        let fit = fit_scale(vp, img);
        let range = scale_range(fit, 1.0, 4.0);
        // 進行方向が右（新ページが右から入る）→ 左端から。
        let forward = carried_view(4.0, fit, range, vp, img, false);
        assert_eq!(forward.offset, Vec2::ZERO);
        // 逆方向 → 右端から（縦は常に上端）。
        let backward = carried_view(4.0, fit, range, vp, img, true);
        let content_w = img.x * backward.scale;
        assert!((backward.offset.x - (content_w - vp.x)).abs() < 1e-3);
        assert_eq!(backward.offset.y, 0.0);
        // コンテンツがビューポートに収まる軸では、右端指定でもオフセットは 0。
        let fit_view = carried_view(1.0, fit, range, vp, img, true);
        assert_eq!(fit_view.offset, Vec2::ZERO);
    }

    #[test]
    fn config_step_and_detail_toggles() {
        let mut c = MagnifierConfig::default();
        assert_eq!(c.notch_ratio(), 1.25);
        assert!(!c.detail_ticks());
        assert_eq!(c.cycle_notch_step(), NotchStep::X1_5);
        assert_eq!(c.notch_ratio(), 1.5);
        assert!(c.toggle_detail_ticks());
        assert!(!c.toggle_detail_ticks());
        c.set_notch_step(NotchStep::X1_1);
        assert_eq!(c.notch_step(), NotchStep::X1_1);
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

        assert_eq!(b.set_body_height(2.0), 24.0);
        assert_eq!(b.set_body_height(30.0), 30.0);

        // 軸ごとに丸め、非有限の軸は現在値を保つ。
        let applied = b.set_step_button_size(v(200.0, 4.0));
        assert_eq!(applied, v(96.0, 16.0));
        let applied = b.set_detail_button_size(v(f32::NAN, 30.0));
        assert_eq!(applied, v(DEFAULT_BUTTON_SIZE.x, 30.0));
        let applied = b.set_exit_button_size(v(500.0, 2.0));
        assert_eq!(applied, v(96.0, 16.0));
    }
}
