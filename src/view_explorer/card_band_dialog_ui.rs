//! カード帯の設定ダイアログ（**MOCK**: 見た目と操作感の確定用。実カード・state・DBには一切つながない）。
//!
//! 仕様は memory の project_card_band_custom_plan。ここで確定させるのは UI の骨格:
//! - 左右縦割（従来UI｜中央線｜新UI）＋共通のサンプル帯＋キャンセル/確定
//! - 従来UI: 情報・評価の循環ボタンを縦に2個。押すとサンプル帯だけが変わる
//! - 新UI: 要素ボタンを押した順にスタック。各項目に [寄せ][X]。差し込み・並べ替えは無し
//! - ✕なしのモーダル。Esc・枠外クリックはキャンセル扱い
//!
//! データモデル（`Elem` / `Align` / `Item`）と帯のレイアウトは、P1（card_band.rs・純関数）／
//! P2（実カード描画）で正式版に置き換える前提の仮実装。文言も日本語直書き（i18nはP4で）。

use super::{CardInfoMode, CardInfoStyle, CardRatingMode, NekoviewApp};
use egui::{Align2, Color32, Pos2, Rect, Stroke, Vec2};

/// 要素の合計上限と行数上限（確定仕様）。
const MAX_ITEMS: usize = 24;
const MAX_LINES: usize = 6;
/// サンプルカードの大きさ（px）。
const SAMPLE_CARD_W: f32 = 240.0;
const SAMPLE_CARD_H: f32 = 120.0;
/// 文字サイズの算出に使う仮想のカード高（実カードの cell_h 相当。サンプルの見た目の高さとは独立）。
const SAMPLE_CELL_H: f32 = 200.0;
/// 帯の左右余白（実カード描画の PAD と同じ）。
const PAD: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Elem {
    Date,
    Title,
    Size,
    Stars,
    Score,
    Visits,
    Space,
    Newline,
    Divider,
}

impl Elem {
    const ALL: [Elem; 9] = [
        Elem::Date,
        Elem::Title,
        Elem::Size,
        Elem::Stars,
        Elem::Score,
        Elem::Visits,
        Elem::Space,
        Elem::Newline,
        Elem::Divider,
    ];

    fn label(self) -> &'static str {
        match self {
            Elem::Date => "日付",
            Elem::Title => "タイトル",
            Elem::Size => "サイズ",
            Elem::Stars => "評価点★",
            Elem::Score => "評価点数値",
            Elem::Visits => "訪問回数",
            Elem::Space => "空白",
            Elem::Newline => "改行",
            Elem::Divider => "区切りバー",
        }
    }
}

/// 寄せ属性。未設定 → 左 → 中央 → 右 → 未設定 の順にトグルする。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Align {
    Unset,
    Left,
    Center,
    Right,
}

impl Align {
    fn next(self) -> Self {
        match self {
            Align::Unset => Align::Left,
            Align::Left => Align::Center,
            Align::Center => Align::Right,
            Align::Right => Align::Unset,
        }
    }

    /// 未設定のときは何も表示しない（決まっているときだけ方向を出す）。
    fn label(self) -> &'static str {
        match self {
            Align::Unset => "　",
            Align::Left => "左",
            Align::Center => "中央",
            Align::Right => "右",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Item {
    elem: Elem,
    align: Align,
}

/// どちらの導線の内容をサンプル帯に出すか（最後に触った側）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Legacy,
    Custom,
}

/// ダイアログの編集内容（ドラフト／確定済みの両方に使う）。
#[derive(Clone, PartialEq, Debug)]
pub(super) struct BandDraft {
    info: CardInfoMode,
    rating: CardRatingMode,
    items: Vec<Item>,
    side: Side,
}

impl Default for BandDraft {
    fn default() -> Self {
        Self { info: CardInfoMode::Off, rating: CardRatingMode::Off, items: Vec::new(), side: Side::Legacy }
    }
}

impl BandDraft {
    fn line_count(&self) -> usize {
        if self.items.is_empty() {
            0
        } else {
            1 + self.items.iter().filter(|i| i.elem == Elem::Newline).count()
        }
    }

    fn can_add(&self, e: Elem) -> bool {
        self.items.len() < MAX_ITEMS && !(e == Elem::Newline && self.line_count() >= MAX_LINES)
    }

    /// 確定時の正規化: 先頭・末尾の改行を削除し、連続改行を1つにする。
    fn normalized(mut self) -> Self {
        let mut out: Vec<Item> = Vec::with_capacity(self.items.len());
        for it in self.items.drain(..) {
            let prev_nl = out.last().is_none_or(|p| p.elem == Elem::Newline);
            if it.elem == Elem::Newline && prev_nl {
                continue;
            }
            out.push(it);
        }
        while out.last().is_some_and(|p| p.elem == Elem::Newline) {
            out.pop();
        }
        self.items = out;
        self
    }

    /// 従来導線の選択を、サンプル描画用の要素列へ展開する（情報行は左寄せ・評価行は中央寄せ）。
    fn legacy_items(&self) -> Vec<Item> {
        let mut v: Vec<Item> = Vec::new();
        let line = |v: &mut Vec<Item>, e: Elem, a: Align| {
            if !v.is_empty() {
                v.push(Item { elem: Elem::Newline, align: Align::Unset });
            }
            v.push(Item { elem: e, align: a });
        };
        let n = self.info.line_count();
        if n >= 1 { line(&mut v, Elem::Title, Align::Left); }
        if n >= 2 { line(&mut v, Elem::Date, Align::Left); }
        if n >= 3 { line(&mut v, Elem::Size, Align::Left); }
        if self.rating.shows_stars() { line(&mut v, Elem::Stars, Align::Center); }
        if self.rating.shows_visits() { line(&mut v, Elem::Visits, Align::Center); }
        v
    }

    fn sample_items(&self) -> Vec<Item> {
        match self.side {
            Side::Legacy => self.legacy_items(),
            Side::Custom => self.items.clone(),
        }
    }
}

/// アプリが持つMOCK状態。`saved` は確定済み（セッション内のみ）、`draft` はダイアログ表示中の編集内容。
#[derive(Default)]
pub(super) struct CardBandMock {
    saved: BandDraft,
    draft: Option<BandDraft>,
}

impl CardBandMock {
    pub(super) fn open(&mut self) {
        self.draft = Some(self.saved.clone());
    }
}

// ── サンプル帯の描画（仮実装） ───────────────────────────────────────────

const SAMPLE_TITLE: &str = "サンプル_タイトル.zip";
const SAMPLE_DATE: &str = "2026-01-31";
const SAMPLE_SIZE: &str = "12.3 MB";
const SAMPLE_HALF: u8 = 7; // ★3.5
const SAMPLE_VISITS: &str = "訪問回数：12回";

fn text_of(e: Elem) -> Option<&'static str> {
    match e {
        Elem::Date => Some(SAMPLE_DATE),
        Elem::Title => Some(SAMPLE_TITLE),
        Elem::Size => Some(SAMPLE_SIZE),
        Elem::Score => Some("3.5"),
        Elem::Visits => Some(SAMPLE_VISITS),
        _ => None,
    }
}

/// 1要素の幅。
fn item_width(painter: &egui::Painter, font: &egui::FontId, e: Elem, line_px: f32, star_r: f32) -> f32 {
    match e {
        Elem::Stars => {
            let l = crate::rating_overlay::compact_star_layout(SAMPLE_HALF, Pos2::ZERO, star_r);
            match (l.first(), l.last()) {
                (Some(a), Some(b)) => (b.0.x - a.0.x) + star_r * 1.902,
                _ => 0.0,
            }
        }
        Elem::Space => line_px * 0.3,
        Elem::Divider => 7.0,
        Elem::Newline => 0.0,
        _ => painter
            .layout_no_wrap(text_of(e).unwrap_or("").to_string(), font.clone(), Color32::WHITE)
            .size()
            .x,
    }
}

/// ゾーン（左／中央／右）内に要素を左から順に描く。クリップは呼び出し側の painter が担う。
fn paint_run(
    painter: &egui::Painter,
    font: &egui::FontId,
    color: Color32,
    run: &[Elem],
    mut x: f32,
    cy: f32,
    row_h: f32,
    line_px: f32,
    star_r: f32,
) {
    for &e in run {
        let w = item_width(painter, font, e, line_px, star_r);
        match e {
            Elem::Stars => {
                crate::rating_overlay::paint_compact_stars(painter, SAMPLE_HALF, Pos2::new(x + w / 2.0, cy), star_r);
            }
            Elem::Divider => {
                painter.line_segment(
                    [Pos2::new(x + w / 2.0, cy - row_h / 2.0 + 1.0), Pos2::new(x + w / 2.0, cy + row_h / 2.0 - 1.0)],
                    Stroke::new(1.0, color),
                );
            }
            Elem::Space | Elem::Newline => {}
            _ => {
                painter.text(Pos2::new(x, cy), Align2::LEFT_CENTER, text_of(e).unwrap_or(""), font.clone(), color);
            }
        }
        x += w;
    }
}

/// 1行ぶんを3ゾーンで配置する。明示指定を優先し、未設定は左ゾーンの明示左の後ろへ書式順で続ける。
/// 幅が足りないときの優先は 左 → 右 → 中央（溢れた側を切り詰める）。
fn paint_row(painter: &egui::Painter, band: Rect, row: &[Item], cy: f32, row_h: f32, line_px: f32, style: &CardInfoStyle) {
    let font = egui::FontId::proportional(line_px);
    let star_r = row_h * 0.45;
    let pick = |a: Align| -> Vec<Elem> { row.iter().filter(|i| i.align == a).map(|i| i.elem).collect() };
    let mut left = pick(Align::Left);
    left.extend(pick(Align::Unset));
    let center = pick(Align::Center);
    let right = pick(Align::Right);
    let width = |run: &[Elem]| -> f32 { run.iter().map(|&e| item_width(painter, &font, e, line_px, star_r)).sum() };

    let l = band.min.x + PAD;
    let r = band.max.x - PAD;
    let avail = (r - l).max(1.0);
    let (lw, cw, rw) = (width(&left), width(&center), width(&right));

    let lw_shown = lw.min(avail);
    let rw_shown = rw.min(avail - lw_shown);
    let right_start = r - rw_shown;
    let c_zone_l = l + lw_shown;
    let c_zone_r = right_start;
    let zone = |a: f32, b: f32| Rect::from_min_max(Pos2::new(a, band.min.y), Pos2::new(b.max(a), band.max.y));

    let color = style.text_color;
    paint_run(&painter.with_clip_rect(zone(l, l + lw_shown)), &font, color, &left, l, cy, row_h, line_px, star_r);
    paint_run(&painter.with_clip_rect(zone(right_start, r)), &font, color, &right, right_start, cy, row_h, line_px, star_r);
    if cw > 0.0 && c_zone_r > c_zone_l {
        let cx = ((band.center().x) - cw / 2.0).clamp(c_zone_l, (c_zone_r - cw).max(c_zone_l));
        paint_run(&painter.with_clip_rect(zone(c_zone_l, c_zone_r)), &font, color, &center, cx, cy, row_h, line_px, star_r);
    }
}

/// 疑似カード＋帯を描く。
fn paint_sample(ui: &mut egui::Ui, items: &[Item], style: &CardInfoStyle) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(SAMPLE_CARD_W, SAMPLE_CARD_H), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(70));
    painter.text(
        rect.center() - Vec2::new(0.0, 20.0),
        Align2::CENTER_CENTER,
        "サムネイル",
        egui::FontId::proportional(14.0),
        Color32::from_gray(120),
    );
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(110)), egui::StrokeKind::Inside);

    // 行に分割
    let mut rows: Vec<Vec<Item>> = vec![Vec::new()];
    for it in items {
        if it.elem == Elem::Newline {
            rows.push(Vec::new());
        } else if let Some(last) = rows.last_mut() {
            last.push(*it);
        }
    }
    if items.is_empty() {
        return;
    }
    let line_px = (SAMPLE_CELL_H * (style.text_size / 200.0)).clamp(9.0, style.text_size);
    let row_h = line_px * 1.35;
    let band_h = rows.len() as f32 * row_h + 4.0;
    let band = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - band_h), rect.max);
    painter.rect_filled(band, 0.0, style.band_color);
    for (i, row) in rows.iter().enumerate() {
        let cy = band.min.y + 2.0 + i as f32 * row_h + row_h / 2.0;
        paint_row(&painter, band, row, cy, row_h, line_px, style);
    }
}

// ── ダイアログ本体 ───────────────────────────────────────────────────────

impl NekoviewApp {
    /// 毎フレーム、MOCKダイアログが開いていれば描く。実状態には触れない。
    pub(super) fn draw_card_band_mock_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.card_band_mock.draft.take() else { return };
        let t = crate::i18n::t();
        let style = self.card_info_style;
        let mut confirm = false;
        let mut cancel = false;

        let modal = egui::Modal::new(egui::Id::new("card_band_mock_dialog")).show(ctx, |ui| {
            ui.set_width(740.0);
            ui.horizontal(|ui| {
                ui.heading("カード帯の設定");
                ui.weak("MOCK: 実カードには反映されません");
            });
            ui.add_space(2.0);

            ui.horizontal_top(|ui| {
                // ── 従来UI ──
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    ui.strong("従来（ワンタッチ）");
                    ui.add_space(4.0);
                    let info_label = match draft.info {
                        CardInfoMode::Off => t.card_info_off(),
                        CardInfoMode::Name => t.card_info_name(),
                        CardInfoMode::NameDate => t.card_info_name_date(),
                        CardInfoMode::NameDateSize => t.card_info_name_date_size(),
                    };
                    if ui.add_sized([210.0, 26.0], egui::Button::new(info_label)).clicked() {
                        draft.info = draft.info.next();
                        draft.side = Side::Legacy;
                    }
                    let rating_label = match draft.rating {
                        CardRatingMode::Off => t.card_rating_off(),
                        CardRatingMode::Stars => t.card_rating_stars(),
                        CardRatingMode::Visits => t.card_rating_visits(),
                        CardRatingMode::StarsVisits => t.card_rating_stars_visits(),
                    };
                    if ui.add_sized([210.0, 26.0], egui::Button::new(rating_label)).clicked() {
                        draft.rating = draft.rating.next();
                        draft.side = Side::Legacy;
                    }
                });

                ui.separator();

                // ── 新UI ──
                ui.vertical(|ui| {
                    ui.set_width(470.0);
                    ui.strong("カスタム");
                    ui.horizontal_wrapped(|ui| {
                        for e in Elem::ALL {
                            if ui.add_enabled(draft.can_add(e), egui::Button::new(e.label())).clicked() {
                                draft.items.push(Item { elem: e, align: Align::Unset });
                                draft.side = Side::Custom;
                            }
                        }
                    });
                    ui.weak(format!("要素 {}/{}　行 {}/{}", draft.items.len(), MAX_ITEMS, draft.line_count(), MAX_LINES));

                    let mut remove: Option<usize> = None;
                    egui::ScrollArea::vertical().max_height(84.0).auto_shrink([false, true]).show(ui, |ui| {
                        if draft.items.is_empty() {
                            ui.weak("（要素ボタンを押して積む）");
                        }
                        for (i, it) in draft.items.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{:>2}", i + 1)).weak());
                                let name = egui::RichText::new(it.elem.label());
                                ui.label(if it.elem == Elem::Newline { name.weak() } else { name });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").on_hover_text("この項目を削除").clicked() {
                                        remove = Some(i);
                                    }
                                    if ui
                                        .add_sized([40.0, 18.0], egui::Button::new(it.align.label()).small())
                                        .on_hover_text("寄せを切替（左 → 中央 → 右 → 未設定）")
                                        .clicked()
                                    {
                                        it.align = it.align.next();
                                        draft.side = Side::Custom;
                                    }
                                });
                            });
                        }
                    });
                    if let Some(i) = remove {
                        draft.items.remove(i);
                        draft.side = Side::Custom;
                    }
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("サンプル");
                ui.weak(match draft.side {
                    Side::Legacy => "（従来の内容）",
                    Side::Custom => "（カスタムの内容）",
                });
            });
            ui.vertical_centered(|ui| paint_sample(ui, &draft.sample_items(), &style));

            ui.add_space(2.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("確定").clicked() {
                    confirm = true;
                }
                if ui.button("キャンセル").clicked() {
                    cancel = true;
                }
            });
        });

        // Esc・枠外クリックはキャンセル扱い
        if modal.should_close() {
            cancel = true;
        }
        if confirm {
            self.card_band_mock.saved = draft.normalized();
        } else if !cancel {
            self.card_band_mock.draft = Some(draft);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(e: Elem) -> Item {
        Item { elem: e, align: Align::Unset }
    }

    #[test]
    fn align_cycles_through_unset_left_center_right() {
        let mut a = Align::Unset;
        let seq: Vec<Align> = (0..5).map(|_| { a = a.next(); a }).collect();
        assert_eq!(seq, vec![Align::Left, Align::Center, Align::Right, Align::Unset, Align::Left]);
        assert_eq!(Align::Unset.label().trim(), "", "未設定は文字を出さない");
    }

    #[test]
    fn normalize_trims_and_collapses_newlines() {
        let d = BandDraft {
            items: vec![
                it(Elem::Newline), it(Elem::Title), it(Elem::Newline), it(Elem::Newline),
                it(Elem::Date), it(Elem::Newline),
            ],
            ..BandDraft::default()
        }
        .normalized();
        let kinds: Vec<Elem> = d.items.iter().map(|i| i.elem).collect();
        assert_eq!(kinds, vec![Elem::Title, Elem::Newline, Elem::Date]);
    }

    #[test]
    fn limits_block_adding() {
        let mut d = BandDraft::default();
        for _ in 0..MAX_LINES - 1 {
            d.items.push(it(Elem::Title));
            d.items.push(it(Elem::Newline));
        }
        d.items.push(it(Elem::Title));
        assert_eq!(d.line_count(), MAX_LINES);
        assert!(!d.can_add(Elem::Newline), "行上限で改行は追加できない");
        assert!(d.can_add(Elem::Title));
        while d.items.len() < MAX_ITEMS {
            d.items.push(it(Elem::Space));
        }
        assert!(!d.can_add(Elem::Title), "要素上限で何も追加できない");
    }

    #[test]
    fn legacy_expands_to_left_info_and_centered_rating_rows() {
        let d = BandDraft { info: CardInfoMode::NameDate, rating: CardRatingMode::StarsVisits, ..BandDraft::default() };
        let kinds: Vec<(Elem, Align)> = d.legacy_items().iter().map(|i| (i.elem, i.align)).collect();
        assert_eq!(
            kinds,
            vec![
                (Elem::Title, Align::Left), (Elem::Newline, Align::Unset),
                (Elem::Date, Align::Left), (Elem::Newline, Align::Unset),
                (Elem::Stars, Align::Center), (Elem::Newline, Align::Unset),
                (Elem::Visits, Align::Center),
            ]
        );
    }
}
