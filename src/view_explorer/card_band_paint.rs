//! カスタム書式の帯（card_band.rs）を描く。実カード（`paint_custom_card_band`）と、
//! 設定ダイアログのサンプル帯（`paint_band`）が同じ描画を共有する。
//!
//! 従来方式（`CardInfoMode`/`CardRatingMode`）の帯の描画コードは panels.rs にあり、ここは触らない。
//! 配置計画・データ不在時の表示は card_band.rs（純関数）が決め、ここは座標を受けて描くだけ。

use super::{CardInfoStyle, NekoviewApp};
use crate::card_band::{self, BandData, Content, Item};
use egui::{Align2, Color32, Pos2, Rect, Stroke};

/// 帯の左右余白（従来方式の描画と同じ）。
const PAD: f32 = 4.0;
/// 区切りバーの占める幅（線＋左右の余白）。
const BAR_W: f32 = 7.0;
/// ホバーで溢れ行をスクロールするときの再描画間隔（ms。panels.rs の MARQUEE_FRAME_MS と同じ約30fps）。
const MARQUEE_FRAME_MS: u64 = 33;

/// 文字サイズ・行の高さ・帯の高さ（従来方式と同じ算出）。
pub(super) fn band_metrics(cell_h: f32, style: &CardInfoStyle, n_rows: usize) -> (f32, f32, f32) {
    let line_px = (cell_h * (style.text_size / 200.0)).clamp(9.0, style.text_size);
    let row_h = line_px * 1.35;
    (line_px, row_h, n_rows as f32 * row_h + 4.0)
}

/// 溢れ行の往復スクロール量（両端に0.6秒静止し、その間を一定速で往復。従来方式と同じ）。
/// 戻り値は 0 以下のオフセット（左へ動く量）。
fn marquee_offset(over: f32, t: f32) -> f32 {
    let speed = 42.0_f32; // px/sec
    let travel = (over / speed).max(0.05);
    let hold = 0.6_f32;
    let period = (travel + hold) * 2.0;
    let ph = t % period;
    let d = if ph < hold {
        0.0
    } else if ph < hold + travel {
        (ph - hold) / travel
    } else if ph < hold + travel + hold {
        1.0
    } else {
        1.0 - (ph - hold - travel - hold) / travel
    };
    -over * d
}

fn content_width(painter: &egui::Painter, font: &egui::FontId, c: &Content, line_px: f32, star_r: f32) -> f32 {
    match c {
        Content::Stars(h) => crate::rating_overlay::compact_stars_width(*h, star_r),
        Content::Space => line_px * 0.3,
        Content::Bar => BAR_W,
        Content::Text(t) if t.is_empty() => 0.0,
        Content::Text(t) => painter.layout_no_wrap(t.clone(), font.clone(), Color32::WHITE).size().x,
    }
}

/// ゾーン内の内容を `x` から左へ順に描く（クリップは呼び出し側の painter が担う）。
fn paint_run(
    painter: &egui::Painter,
    font: &egui::FontId,
    color: Color32,
    run: &[(Content, f32)],
    mut x: f32,
    cy: f32,
    row_h: f32,
    star_r: f32,
) {
    for (c, w) in run {
        match c {
            Content::Stars(h) => {
                crate::rating_overlay::paint_compact_stars(painter, *h, Pos2::new(x + w / 2.0, cy), star_r);
            }
            Content::Bar => {
                let bx = x + w / 2.0;
                painter.line_segment(
                    [Pos2::new(bx, cy - row_h / 2.0 + 1.0), Pos2::new(bx, cy + row_h / 2.0 - 1.0)],
                    Stroke::new(1.0, color),
                );
            }
            Content::Space => {}
            Content::Text(t) => {
                if !t.is_empty() {
                    painter.text(Pos2::new(x, cy), Align2::LEFT_CENTER, t, font.clone(), color);
                }
            }
        }
        x += w;
    }
}

/// 帯（背景＋全行）を `band` に描く。`scroll_t` は、ホバー中のカードで溢れたゾーンを
/// スクロールさせる経過秒数（None ならスクロールしない）。戻り値: 溢れたゾーンがあったか。
pub(super) fn paint_band(
    painter: &egui::Painter,
    band: Rect,
    rows: &[Vec<Item>],
    data: &BandData,
    style: &CardInfoStyle,
    line_px: f32,
    row_h: f32,
    scroll_t: Option<f32>,
) -> bool {
    let painter = painter.with_clip_rect(band);
    painter.rect_filled(band, 0.0, style.band_color);
    let font = egui::FontId::proportional(line_px);
    let star_r = row_h * 0.45;
    let l = band.min.x + PAD;
    let avail = (band.max.x - PAD - l).max(1.0);
    let mut any_overflow = false;

    for (ri, row) in rows.iter().enumerate() {
        let cy = band.min.y + 2.0 + ri as f32 * row_h + row_h / 2.0;
        let contents: Vec<Content> = row.iter().map(|i| card_band::content_of(i.elem, data)).collect();
        let widths: Vec<f32> = contents.iter().map(|c| content_width(&painter, &font, c, line_px, star_r)).collect();
        let aligns: Vec<card_band::Align> = row.iter().map(|i| i.align).collect();
        let plan = card_band::plan_row(&aligns, &widths, avail);

        for zone in [&plan.left, &plan.center, &plan.right] {
            if zone.idx.is_empty() || zone.clip.1 <= zone.clip.0 {
                continue;
            }
            let clip = Rect::from_min_max(Pos2::new(l + zone.clip.0, band.min.y), Pos2::new(l + zone.clip.1, band.max.y));
            let run: Vec<(Content, f32)> = zone.idx.iter().map(|&i| (contents[i].clone(), widths[i])).collect();
            let over = card_band::zone_overflow(run.iter().map(|(_, w)| *w).sum(), zone.clip);
            let off = if over > 0.0 {
                any_overflow = true;
                scroll_t.map_or(0.0, |t| marquee_offset(over, t))
            } else {
                0.0
            };
            paint_run(&painter.with_clip_rect(clip), &font, style.text_color, &run, l + zone.start + off, cy, row_h, star_r);
        }
    }
    any_overflow
}

impl NekoviewApp {
    /// ホバー中のカードの経過秒数を返す（ホバー開始でリセット）。ホバーしていなければ None。
    fn card_hover_scroll_t(&mut self, path: &std::path::PathBuf, hovered: bool) -> Option<f32> {
        if hovered {
            let fresh = !matches!(&self.card_info_hover, Some((p, _)) if p == path);
            if fresh {
                self.card_info_hover = Some((path.clone(), std::time::Instant::now()));
            }
            self.card_info_hover.as_ref().filter(|(p, _)| p == path).map(|(_, t)| t.elapsed().as_secs_f32())
        } else {
            if matches!(&self.card_info_hover, Some((p, _)) if p == path) {
                self.card_info_hover = None;
            }
            None
        }
    }

    /// カスタム書式の帯を実カードの下部に重ねて描く。
    /// 日付・サイズ・評価は、書式がそれを使うときだけ取得する（可視カードぶんの遅延取得）。
    pub(super) fn paint_custom_card_band(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        path: &std::path::PathBuf,
        cell_h: f32,
        items: &[Item],
    ) {
        let rows = card_band::split_rows(items);
        if rows.is_empty() {
            return;
        }
        let t = crate::i18n::t();

        if card_band::needs_meta(items)
            && !self.archive_meta_cache.contains_key(path)
            && let Ok(md) = std::fs::metadata(path)
        {
            let mt = md.modified().unwrap_or(std::time::UNIX_EPOCH);
            self.archive_meta_cache.insert(path.clone(), (mt, md.len()));
        }
        let meta = self.archive_meta_cache.get(path).copied();
        let is_raw = self.raw_image_files.contains(path);
        let rating = if card_band::needs_rating(items) { self.archive_rating_of(path) } else { None };

        let data = BandData {
            title: path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            date: meta
                .map(|(mt, _)| super::panels::format_mtime(mt, &self.card_date_format, t))
                .unwrap_or_default(),
            size: meta.map(|(_, sz)| super::panels::humanize_size(sz)).unwrap_or_default(),
            rating_half: if is_raw { None } else { Some(rating.map_or(0, |r| r.rating_half)) },
            unrated: t.rating_unrated().to_string(),
            visits_text: t.visit_count_line(rating.map_or(0, |r| r.visit_count)),
        };

        let (line_px, row_h, band_h) = band_metrics(cell_h, &self.card_info_style, rows.len());
        let band = Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - band_h), rect.max);
        let hovered = response.hovered();
        let scroll_t = self.card_hover_scroll_t(path, hovered);
        let style = self.card_info_style;
        let overflow = paint_band(&ui.painter_at(rect), band, &rows, &data, &style, line_px, row_h, scroll_t);
        if hovered && overflow {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(MARQUEE_FRAME_MS));
        }
    }
}
