//! カード帯の設定ダイアログ（**MOCK**: 見た目と操作感の確定用。実カード・state・DBには一切つながない）。
//!
//! 仕様は memory の project_card_band_custom_plan。ここで確定させるのは UI の骨格:
//! - 左右縦割（従来UI｜中央線｜新UI）＋共通のサンプル帯＋キャンセル/確定
//! - 従来UI: 情報・評価の循環ボタンを縦に2個。押すとサンプル帯だけが変わる
//! - 新UI: 要素ボタンを押した順にスタック。各項目に [寄せ][X]。差し込み・並べ替えは無し
//! - ✕なしのモーダル。Esc・枠外クリックはキャンセル扱い
//!
//! データモデルと配置計画は card_band.rs（純関数・P1）を使う。実カード描画への接続はP2、文言のi18nはP4。

use super::{CardInfoMode, CardInfoStyle, CardRatingMode, NekoviewApp};
use crate::card_band::{self, Active, Align, Elem, Item, MAX_ITEMS, MAX_LINES};
use egui::{Align2, Color32, Pos2, Rect, Stroke, Vec2};

/// サンプルカードの大きさ（px）。
const SAMPLE_CARD_W: f32 = 240.0;
const SAMPLE_CARD_H: f32 = 120.0;
/// 文字サイズの算出に使う仮想のカード高（実カードの cell_h 相当。サンプルの見た目の高さとは独立）。
const SAMPLE_CELL_H: f32 = 200.0;
/// 帯の左右余白（実カード描画の PAD と同じ）。
const PAD: f32 = 4.0;

/// 要素ボタン・スタック欄に出す名称（i18nはP4で）。
fn elem_label(e: Elem) -> &'static str {
    match e {
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

/// 寄せ属性の表示。未設定も明示する。
fn align_label(a: Align) -> &'static str {
    match a {
        Align::Unset => "未設定",
        Align::Left => "左",
        Align::Center => "中央",
        Align::Right => "右",
    }
}

/// ダイアログの編集内容（ドラフト／確定済みの両方に使う）。
#[derive(Clone, PartialEq, Debug)]
pub(super) struct BandDraft {
    info: CardInfoMode,
    rating: CardRatingMode,
    items: Vec<Item>,
    side: Active,
}

impl Default for BandDraft {
    fn default() -> Self {
        Self { info: CardInfoMode::Off, rating: CardRatingMode::Off, items: Vec::new(), side: Active::Classic }
    }
}

impl BandDraft {
    fn line_count(&self) -> usize {
        card_band::line_count(&self.items)
    }

    fn can_add(&self, e: Elem) -> bool {
        card_band::can_add(&self.items, e)
    }

    /// 確定時の正規化（カスタム側のみ。従来側の選択はそのまま）。
    fn normalized(mut self) -> Self {
        self.items = card_band::normalize(&self.items);
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
            Active::Classic => self.legacy_items(),
            Active::Custom => self.items.clone(),
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

/// 1行ぶんを配置計画（card_band::plan_row）どおりに描く。
fn paint_row(painter: &egui::Painter, band: Rect, row: &[Item], cy: f32, row_h: f32, line_px: f32, style: &CardInfoStyle) {
    let font = egui::FontId::proportional(line_px);
    let star_r = row_h * 0.45;
    let l = band.min.x + PAD;
    let avail = (band.max.x - PAD - l).max(1.0);
    let aligns: Vec<Align> = row.iter().map(|i| i.align).collect();
    let widths: Vec<f32> = row.iter().map(|i| item_width(painter, &font, i.elem, line_px, star_r)).collect();
    let plan = card_band::plan_row(&aligns, &widths, avail);
    let color = style.text_color;
    for zone in [&plan.left, &plan.center, &plan.right] {
        if zone.clip.1 <= zone.clip.0 || zone.idx.is_empty() {
            continue;
        }
        let clip = Rect::from_min_max(Pos2::new(l + zone.clip.0, band.min.y), Pos2::new(l + zone.clip.1, band.max.y));
        let run: Vec<Elem> = zone.idx.iter().map(|&i| row[i].elem).collect();
        paint_run(&painter.with_clip_rect(clip), &font, color, &run, l + zone.start, cy, row_h, line_px, star_r);
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

    let rows = card_band::split_rows(items);
    if rows.is_empty() {
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

        // 窓が狭いときははみ出さないよう、幅を窓に合わせて縮める。
        // 左右の欄は同じ幅にし、中央線（欄の間の隙間の真ん中）がダイアログの中央に来るようにする。
        let dialog_w = (ctx.content_rect().width() - 60.0).clamp(480.0, 660.0);
        const GAP: f32 = 12.0;

        let modal = egui::Modal::new(egui::Id::new("card_band_mock_dialog")).show(ctx, |ui| {
            ui.set_width(dialog_w);
            // 横並びでは項目の間に item_spacing.x が両側に入るので、その分を欄幅から引いて全体を dialog_w に収める。
            let sp = ui.spacing().item_spacing.x;
            let col_w = (dialog_w - GAP - 2.0 * sp) / 2.0;
            let legacy_w = col_w;
            let custom_w = col_w;
            let legacy_btn_w = (col_w / 2.0).max(110.0);
            ui.horizontal(|ui| {
                ui.heading("カード帯の設定");
                ui.weak("MOCK: 実カードには反映されません");
            });
            ui.add_space(2.0);

            let mut left_rect = Rect::NOTHING;
            let columns = ui.horizontal_top(|ui| {
                // ── 従来UI ──
                left_rect = ui.vertical(|ui| {
                    ui.set_width(legacy_w);
                    // 見出しは切替ボタンを兼ねる（従来／カスタムは排他）。押した側がアクティブになり、
                    // 非アクティブ側の欄はグレーアウトして内容を保持する。確定は両方を保存し、アクティブ側を実採用する。
                    if ui
                        .add_sized([col_w, 28.0], egui::Button::new(egui::RichText::new("従来方式").strong()).selected(draft.side == Active::Classic))
                        .on_hover_text("こちらを採用する（カスタム側の内容は保持されます）")
                        .clicked()
                    {
                        draft.side = Active::Classic;
                    }
                    ui.add_space(4.0);
                    ui.add_enabled_ui(draft.side == Active::Classic, |ui| {
                    let info_label = match draft.info {
                        CardInfoMode::Off => t.card_info_off(),
                        CardInfoMode::Name => t.card_info_name(),
                        CardInfoMode::NameDate => t.card_info_name_date(),
                        CardInfoMode::NameDateSize => t.card_info_name_date_size(),
                    };
                    if ui.add_sized([legacy_btn_w, 26.0], egui::Button::new(info_label)).clicked() {
                        draft.info = draft.info.next();
                    }
                    let rating_label = match draft.rating {
                        CardRatingMode::Off => t.card_rating_off(),
                        CardRatingMode::Stars => t.card_rating_stars(),
                        CardRatingMode::Visits => t.card_rating_visits(),
                        CardRatingMode::StarsVisits => t.card_rating_stars_visits(),
                    };
                    if ui.add_sized([legacy_btn_w, 26.0], egui::Button::new(rating_label)).clicked() {
                        draft.rating = draft.rating.next();
                    }
                    });
                }).response.rect;

                // 中央線は下の columns.response から固定高さで描く（縦のui.separator()は
                // モーダル内で利用可能な高さ全部を取り、ダイアログが縦に巨大化するため使わない）。
                ui.add_space(GAP);

                // ── 新UI ──
                ui.vertical(|ui| {
                    ui.set_width(custom_w);
                    if ui
                        .add_sized([col_w, 28.0], egui::Button::new(egui::RichText::new("カスタム").strong()).selected(draft.side == Active::Custom))
                        .on_hover_text("こちらを採用する（従来側の選択は保持されます）")
                        .clicked()
                    {
                        draft.side = Active::Custom;
                    }
                    ui.add_enabled_ui(draft.side == Active::Custom, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for e in Elem::ALL {
                            if ui.add_enabled(draft.can_add(e), egui::Button::new(elem_label(e))).clicked() {
                                draft.items.push(Item { elem: e, align: Align::Unset });
                            }
                        }
                    });
                    ui.weak(format!("要素制限数 {}/{}　行制限数 {}/{}", draft.items.len(), MAX_ITEMS, draft.line_count(), MAX_LINES));

                    let mut remove: Option<usize> = None;
                    egui::ScrollArea::vertical().max_height(84.0).auto_shrink([false, true]).show(ui, |ui| {
                        if draft.items.is_empty() {
                            ui.weak("（要素ボタンを押して積む）");
                        }
                        for (i, it) in draft.items.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{:>2}", i + 1)).weak());
                                let name = egui::RichText::new(elem_label(it.elem));
                                ui.label(if it.elem == Elem::Newline { name.weak() } else { name });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").on_hover_text("この項目を削除").clicked() {
                                        remove = Some(i);
                                    }
                                    if ui
                                        .add_sized([52.0, 18.0], egui::Button::new(align_label(it.align)).small())
                                        .on_hover_text("寄せを切替（左 → 中央 → 右 → 未設定）")
                                        .clicked()
                                    {
                                        it.align = it.align.next();
                                    }
                                });
                            });
                        }
                    });
                    if let Some(i) = remove {
                        draft.items.remove(i);
                    }
                    });
                });
            });
            let x = left_rect.max.x + sp + GAP / 2.0;
            ui.painter().line_segment(
                [Pos2::new(x, columns.response.rect.min.y), Pos2::new(x, columns.response.rect.max.y)],
                Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );

            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("サンプル");
                ui.weak(match draft.side {
                    Active::Classic => "（従来の内容）",
                    Active::Custom => "（カスタムの内容）",
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
        Item::new(e, Align::Unset)
    }

    #[test]
    fn labels_are_explicit_even_when_unset() {
        assert_eq!(align_label(Align::Unset), "未設定", "未設定は明示する");
        for e in Elem::ALL {
            assert!(!elem_label(e).is_empty());
        }
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

    #[test]
    fn confirm_keeps_both_sides_and_adopts_the_active_one() {
        let d = BandDraft {
            info: CardInfoMode::NameDate,
            rating: CardRatingMode::Stars,
            items: vec![it(Elem::Title), it(Elem::Newline)],
            side: Active::Custom,
        }
        .normalized();
        assert_eq!(d.info, CardInfoMode::NameDate, "従来側の選択は保持される");
        assert_eq!(d.rating, CardRatingMode::Stars);
        assert_eq!(d.items.len(), 1, "カスタム側は正規化される");
        assert_eq!(d.side, Active::Custom, "アクティブな側が実採用される");
        let kinds: Vec<Elem> = d.sample_items().iter().map(|i| i.elem).collect();
        assert_eq!(kinds, vec![Elem::Title], "採用側の内容が出る");
    }
}
