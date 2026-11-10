//! カード帯の設定ダイアログ（メニューバーの[カード帯設定]ボタンから開く）。
//!
//! - 左右縦割（従来方式｜中央線｜カスタム）＋共通のサンプル帯＋キャンセル/確定。見出しは排他の切替ボタン
//! - 従来方式: 情報・評価の循環ボタン。カスタム: 要素ボタンを押した順にスタック（寄せトグル・X削除）
//! - サンプル帯の右上に、帯の背景の透過度ボタン（10%刻み・0〜100%、従来方式・カスタム共通）
//! - ✕なしのモーダル。Esc・枠外クリックはキャンセル扱い。確定で両方の内容を保存し、有効な導線を実採用する
//!
//! データモデルと配置計画は card_band.rs（純関数）、描画は card_band_paint.rs（実カードと共有）。

use super::{CardInfoMode, CardInfoStyle, CardRatingMode, NekoviewApp};
use super::card_band_paint::{band_metrics, paint_band};
use crate::card_band::{self, Active, Align, BandData, Elem, Item, MAX_ITEMS, MAX_LINES};
use egui::{Align2, Color32, Pos2, Rect, Stroke, Vec2};

/// サンプルカードの大きさ（px）。
const SAMPLE_CARD_W: f32 = 240.0;
const SAMPLE_CARD_H: f32 = 120.0;
/// 文字サイズの算出に使う仮想のカード高（実カードの cell_h 相当。サンプルの見た目の高さとは独立）。
const SAMPLE_CELL_H: f32 = 200.0;

/// ダイアログの編集内容（開いている間のドラフト）。
#[derive(Clone, PartialEq, Debug)]
pub(super) struct BandDraft {
    info: CardInfoMode,
    rating: CardRatingMode,
    items: Vec<Item>,
    side: Active,
    /// 帯の背景の透過度（%、10刻み）
    transparency_pct: u8,
}

impl Default for BandDraft {
    fn default() -> Self {
        Self {
            info: CardInfoMode::Off,
            rating: CardRatingMode::Off,
            items: Vec::new(),
            side: Active::Classic,
            transparency_pct: card_band::DEFAULT_TRANSPARENCY_PCT,
        }
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

/// ダイアログの状態。`draft` が Some の間だけ開いている（確定・キャンセルで None に戻る）。
#[derive(Default)]
pub(super) struct CardBandDialog {
    draft: Option<BandDraft>,
}

// ── サンプル帯の描画（実カードと同じ描画 card_band_paint::paint_band を使う） ──

fn sample_data(t: crate::i18n::Lang) -> BandData {
    BandData {
        title: t.band_sample_title().to_string(),
        date: "2026-01-31".to_string(),
        size: "12.3 MB".to_string(),
        rating_half: Some(7), // ★3.5
        unrated: t.rating_unrated().to_string(),
        visits_text: t.visit_count_line(12),
    }
}

/// 疑似カード＋帯を描く。
fn paint_sample(ui: &mut egui::Ui, items: &[Item], style: &CardInfoStyle) {
    let t = crate::i18n::t();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(SAMPLE_CARD_W, SAMPLE_CARD_H), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(70));
    painter.text(
        rect.center() - Vec2::new(0.0, 20.0),
        Align2::CENTER_CENTER,
        t.band_sample_thumb(),
        egui::FontId::proportional(14.0),
        Color32::from_gray(120),
    );
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(110)), egui::StrokeKind::Inside);

    let rows = card_band::split_rows(items);
    if rows.is_empty() {
        return;
    }
    let (line_px, row_h, band_h) = band_metrics(SAMPLE_CELL_H, style, rows.len());
    let band = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - band_h), rect.max);
    paint_band(&painter, band, &rows, &sample_data(t), style, line_px, row_h, None);
}

// ── ダイアログ本体 ───────────────────────────────────────────────────────

impl NekoviewApp {
    /// ダイアログを開く。ドラフトは現在の実状態（従来方式の選択・カスタム書式・有効な導線・透過度）から始める。
    pub(super) fn open_card_band_dialog(&mut self) {
        self.card_band_dialog.draft = Some(BandDraft {
            info: self.card_info_mode,
            rating: self.card_rating_mode,
            items: self.card_band.items.clone(),
            side: self.card_band.active,
            transparency_pct: self.card_band.transparency_pct,
        });
    }

    /// 確定: 従来方式の選択・カスタム書式・有効な導線・透過度の全部を実状態へ反映して保存する。
    fn apply_card_band_draft(&mut self, draft: BandDraft) {
        let draft = draft.normalized();
        self.card_info_mode = draft.info;
        self.card_rating_mode = draft.rating;
        self.card_band = card_band::BandSettings {
            active: draft.side,
            items: draft.items,
            transparency_pct: draft.transparency_pct,
        };
        self.card_info_style.band_color = Color32::from_black_alpha(card_band::band_alpha(draft.transparency_pct));
        self.persist_state();
    }

    /// 毎フレーム、ダイアログが開いていれば描く。確定されたときだけ実状態へ反映する。
    pub(super) fn draw_card_band_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.card_band_dialog.draft.take() else { return };
        let t = crate::i18n::t();
        let base_style = self.card_info_style;
        let mut confirm = false;
        let mut cancel = false;

        // 窓が狭いときははみ出さないよう、幅を窓に合わせて縮める。
        // 左右の欄は同じ幅にし、中央線（欄の間の隙間の真ん中）がダイアログの中央に来るようにする。
        let dialog_w = (ctx.content_rect().width() - 60.0).clamp(480.0, 660.0);
        const GAP: f32 = 12.0;

        let modal = egui::Modal::new(egui::Id::new("card_band_dialog")).show(ctx, |ui| {
            ui.set_width(dialog_w);
            // 横並びでは項目の間に item_spacing.x が両側に入るので、その分を欄幅から引いて全体を dialog_w に収める。
            let sp = ui.spacing().item_spacing.x;
            let col_w = (dialog_w - GAP - 2.0 * sp) / 2.0;
            let legacy_w = col_w;
            let custom_w = col_w;
            let legacy_btn_w = (col_w / 2.0).max(110.0);
            ui.heading(t.band_dialog_title());
            ui.add_space(2.0);

            let mut left_rect = Rect::NOTHING;
            let columns = ui.horizontal_top(|ui| {
                // ── 従来UI ──
                left_rect = ui.vertical(|ui| {
                    ui.set_width(legacy_w);
                    // 見出しは切替ボタンを兼ねる（従来／カスタムは排他）。押した側がアクティブになり、
                    // 非アクティブ側の欄はグレーアウトして内容を保持する。確定は両方を保存し、アクティブ側を実採用する。
                    if ui
                        .add_sized([col_w, 28.0], egui::Button::new(egui::RichText::new(t.band_classic_heading()).strong()).selected(draft.side == Active::Classic))
                        .on_hover_text(t.band_classic_tip())
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
                        .add_sized([col_w, 28.0], egui::Button::new(egui::RichText::new(t.band_custom_heading()).strong()).selected(draft.side == Active::Custom))
                        .on_hover_text(t.band_custom_tip())
                        .clicked()
                    {
                        draft.side = Active::Custom;
                    }
                    ui.add_enabled_ui(draft.side == Active::Custom, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for e in Elem::ALL {
                            if ui.add_enabled(draft.can_add(e), egui::Button::new(t.band_elem(e))).clicked() {
                                draft.items.push(Item { elem: e, align: Align::Unset });
                            }
                        }
                    });
                    ui.weak(t.band_limits(draft.items.len(), MAX_ITEMS, draft.line_count(), MAX_LINES));

                    let mut remove: Option<usize> = None;
                    egui::ScrollArea::vertical().max_height(84.0).auto_shrink([false, true]).show(ui, |ui| {
                        if draft.items.is_empty() {
                            ui.weak(t.band_stack_empty());
                        }
                        for (i, it) in draft.items.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(format!("{:>2}", i + 1)).weak());
                                let name = egui::RichText::new(t.band_elem(it.elem));
                                ui.label(if it.elem == Elem::Newline { name.weak() } else { name });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("X").on_hover_text(t.band_remove_tip()).clicked() {
                                        remove = Some(i);
                                    }
                                    if ui
                                        .add_sized([52.0, 18.0], egui::Button::new(t.band_align(it.align)).small())
                                        .on_hover_text(t.band_align_tip())
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
                ui.strong(t.band_sample_heading());
                ui.weak(match draft.side {
                    Active::Classic => t.band_sample_of_classic(),
                    Active::Custom => t.band_sample_of_custom(),
                });
                // サンプル表示エリアの右上: 帯の背景の透過度（押すたびに10%ずつ。従来方式・カスタム共通）
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(t.band_transparency_button(draft.transparency_pct))
                        .on_hover_text(t.band_transparency_tip())
                        .clicked()
                    {
                        draft.transparency_pct = card_band::next_transparency(draft.transparency_pct);
                    }
                });
            });
            // サンプル帯はドラフトの透過度で描く（確定するまで実カードには反映しない）
            let mut sample_style = base_style;
            sample_style.band_color = Color32::from_black_alpha(card_band::band_alpha(draft.transparency_pct));
            ui.vertical_centered(|ui| paint_sample(ui, &draft.sample_items(), &sample_style));

            ui.add_space(2.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t.band_confirm()).clicked() {
                    confirm = true;
                }
                if ui.button(t.band_cancel()).clicked() {
                    cancel = true;
                }
            });
        });

        // Esc・枠外クリックはキャンセル扱い
        if modal.should_close() {
            cancel = true;
        }
        if confirm {
            self.apply_card_band_draft(draft);
        } else if !cancel {
            self.card_band_dialog.draft = Some(draft);
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
    fn labels_exist_in_every_language_and_unset_is_explicit() {
        use crate::i18n::Lang;
        for lang in [Lang::Japanese, Lang::English, Lang::Chinese] {
            for e in Elem::ALL {
                assert!(!lang.band_elem(e).is_empty());
            }
            for a in [Align::Unset, Align::Left, Align::Center, Align::Right] {
                assert!(!lang.band_align(a).trim().is_empty(), "寄せは未設定も文字で明示する");
            }
        }
        assert_eq!(Lang::Japanese.band_align(Align::Unset), "未設定");
        assert_eq!(Lang::English.band_classic_heading(), "Classic mode");
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
            transparency_pct: 40,
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
