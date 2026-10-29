//! リスト表示（カードの代わりに1行1項目で並べる）。
//!
//! 列: 重複（枠のみ。中身は重複機能で埋める）／サムネ／名前／サイズ／更新日時／評価／訪問回数。
//! 行の高さはサムネ設定に対する S=25% / M=40% / L=60%。「ホバー」は行を文字のみにして、
//! サムネは行にホバーしたときのポップアップに出す。列の幅は固定で、収まらないときは横スクロールする。
//! フォルダ（「↑」・サブフォルダ・仮想フォルダ）も同じ列の行として並べる。
//!
//! 一覧（文字のみ）は、リストのホバー形式からポップアップを除いたもの（サムネ列なし・最小の行高）。
//!
//! 選択・開く・右クリックは、カードと同じ共通関数（`handle_archive_interaction` ほか）を使う。

use std::path::PathBuf;

use crate::i18n;

use super::panels::{draw_cursor_ring, format_mtime, humanize_size};
use super::list_columns::ListColumns;
use super::view_mode::{ListSize, ViewMode};
use super::*;

const HEADER_H: f32 = 22.0;
/// 行どうしの縦の隙間（px）。スクロール位置の計算（行ピッチ）と共有する。
pub(super) const ROW_GAP: f32 = 1.0;
/// 見出しの列境界の、つかめる幅（px。境界線を中心に左右へ）。
const RESIZE_GRAB_W: f32 = 8.0;
/// 行の高さの下限。ホバー形式（文字のみ）の行の高さでもある。
const MIN_ROW_H: f32 = 24.0;
/// ホバー形式でフォルダのアイコンだけを置く列の幅。
const HOVER_ICON_COL_W: f32 = 24.0;
const CELL_PAD: f32 = 6.0;
/// ホバーのポップアップをポインタからずらす量。
const POPUP_OFFSET: f32 = 18.0;

/// 表示モードから、行の形式（実効のサイズ）とホバーのポップアップの有無を決める。
/// 一覧は常に文字のみの行（= ホバー形式の見た目）で、ポップアップは出さない。
fn layout_for(mode: ViewMode, list_size: ListSize) -> (ListSize, bool) {
    match mode {
        ViewMode::Text => (ListSize::Hover, false),
        _ => (list_size, list_size == ListSize::Hover),
    }
}

/// リスト行の高さ。`thumb_size` はサムネ設定（px）。
pub(super) fn row_height(size: ListSize, thumb_size: f32) -> f32 {
    let ratio = match size {
        ListSize::S => 0.25,
        ListSize::M => 0.40,
        ListSize::L => 0.60,
        ListSize::Hover => return MIN_ROW_H,
    };
    (thumb_size * ratio).max(MIN_ROW_H)
}

/// サムネ列の幅。ホバー形式はフォルダアイコン分だけ。
fn thumb_col_width(size: ListSize, row_h: f32) -> f32 {
    match size {
        ListSize::Hover => HOVER_ICON_COL_W,
        _ => (row_h / std::f32::consts::SQRT_2).round() + CELL_PAD,
    }
}

/// 列の幅（重複・サムネ・名前・サイズ・更新日時・評価・訪問回数）。サムネ以外はユーザー設定。
/// `place_w`・`compare_w` は類似ファイル一覧のときだけ出す「場所」「完全比較」列の幅（出さないときは 0）。
fn column_widths(thumb_w: f32, cols: &ListColumns, place_w: f32, compare_w: f32) -> [f32; 9] {
    [
        cols.width(0),
        thumb_w,
        cols.width(1),
        place_w,
        compare_w,
        cols.width(2),
        cols.width(3),
        cols.width(4),
        cols.width(5),
    ]
}

/// 類似ファイル一覧の「完全比較」列の幅（px。固定）。
const COMPARE_COL_W: f32 = 120.0;

/// 類似ファイル一覧の「場所」列の幅（px。固定）。
const PLACE_COL_W: f32 = 380.0;

#[derive(Clone, Copy)]
enum Col {
    Dup = 0,
    Thumb = 1,
    Name = 2,
    /// 類似ファイル一覧のときだけ幅を持つ（それ以外は幅 0）。
    Place = 3,
    /// 類似ファイル一覧のときだけ幅を持つ（完全比較の結果バッジ）。
    Compare = 4,
    Size = 5,
    Mtime = 6,
    Rating = 7,
    Visits = 8,
}

impl Col {
    /// 見出しのクリックでソートできる列（重複・サムネは対象外）。
    fn sort_column(self) -> Option<crate::explorer_sort::HeaderColumn> {
        use crate::explorer_sort::HeaderColumn;
        match self {
            Col::Name => Some(HeaderColumn::Name),
            Col::Size => Some(HeaderColumn::Size),
            Col::Mtime => Some(HeaderColumn::Date),
            Col::Rating => Some(HeaderColumn::Score),
            Col::Visits => Some(HeaderColumn::Visits),
            Col::Dup | Col::Thumb | Col::Place | Col::Compare => None,
        }
    }

    /// 幅を変えられる列の `ListColumns` 上の番号。サムネ列は行の高さで決まるので変えられない。
    fn resize_index(self) -> Option<usize> {
        match self {
            Col::Thumb | Col::Place | Col::Compare => None,
            Col::Dup => Some(0),
            Col::Name => Some(1),
            Col::Size => Some(2),
            Col::Mtime => Some(3),
            Col::Rating => Some(4),
            Col::Visits => Some(5),
        }
    }
}

/// 行の矩形から、指定した列の矩形を切り出す。
fn col_rect(row: egui::Rect, widths: &[f32; 9], col: Col) -> egui::Rect {
    let idx = col as usize;
    let x: f32 = widths[..idx].iter().sum();
    egui::Rect::from_min_size(egui::pos2(row.min.x + x, row.min.y), egui::vec2(widths[idx], row.height()))
}

/// 類似ファイル一覧で、入口のファイルの行・カードに敷く色。
pub(super) fn similar_entry_color() -> egui::Color32 {
    egui::Color32::from_rgb(235, 170, 40)
}

/// 幅 `max_w` に収まるよう、先頭を「…」で省く（パスは末尾側に区別の手がかりがあるため）。
fn elide_head(painter: &egui::Painter, text: &str, font: &egui::FontId, max_w: f32) -> String {
    let width = |t: &str| painter.layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE).size().x;
    if max_w <= 0.0 || width(text) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    // 先頭から `cut` 文字を省いた「…＋残り」が収まる最小の cut を二分探索する。
    let (mut lo, mut hi) = (1usize, chars.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let candidate: String = std::iter::once('…').chain(chars[mid..].iter().copied()).collect();
        if width(&candidate) <= max_w {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    std::iter::once('…').chain(chars[lo.min(chars.len())..].iter().copied()).collect()
}

/// 列の矩形内へ左寄せで文字を描く（はみ出しは列で切る）。
fn cell_text(painter: &egui::Painter, rect: egui::Rect, text: &str, font: &egui::FontId, color: egui::Color32) {
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    clipped.text(
        egui::pos2(rect.min.x + CELL_PAD, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        font.clone(),
        color,
    );
}

/// 行 `i` の当たり判定。行ごとの子UIの下にIDを置く。
/// 仮想スクロールでは、スクロール量が行ピッチの整数倍のとき、前のフレームで別の行（別のID）が
/// 占めていた矩形に今の行が来る。egui のデバッグビルドは、同じ親の下でこれが起きると
/// 「矩形のIDが変わった」と赤い枠を出すので、行ごとに親を分けて警告の対象から外す（release では出ない警告）。
fn row_response(ui: &mut egui::Ui, rect: egui::Rect, i: usize) -> egui::Response {
    ui.push_id(("list_row", i), |ui| ui.interact(rect, ui.id(), egui::Sense::click())).inner
}

/// 仮想スクロール: 表示領域に掛かる行の範囲（上下に1行ずつ余裕を持たせる）。
/// `top` は全行の先頭から表示領域の上端までの距離、`height` は表示領域の高さ、`pitch` は行の高さ＋隙間。
pub(super) fn visible_rows(top: f32, height: f32, pitch: f32, total: usize) -> std::ops::Range<usize> {
    if total == 0 || height <= 0.0 || pitch <= 0.0 {
        return 0..0;
    }
    let first = ((top / pitch).floor().max(0.0) as usize).saturating_sub(1);
    let last = (((top + height) / pitch).ceil().max(0.0) as usize + 1).min(total);
    first.min(total)..last
}

/// 縦スクロールのオフセットを、`index` 番目の行（0始まり）が画面内に収まる最小の量だけ動かす。
/// `viewport_h` は行を並べる領域の高さ（固定の見出しは含まない）。
pub(super) fn scroll_to_row(offset: f32, viewport_h: f32, index: usize, row_h: f32) -> f32 {
    let top = index as f32 * (row_h + ROW_GAP);
    let bottom = top + row_h;
    if top < offset {
        top
    } else if viewport_h > 0.0 && bottom > offset + viewport_h {
        bottom - viewport_h
    } else {
        offset
    }
}

/// 列どうしの区切り線の色。
fn grid_line_color(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().weak_text_color().gamma_multiply(0.35)
}

/// 行の高さと列の幅（描画と、キーボードのスクロール計算で共有）。
pub(super) struct ListMetrics {
    pub row_h: f32,
    widths: [f32; 9],
    total_w: f32,
}

impl NekoviewApp {
    /// 現在のモード・サイズ・サムネ設定・列幅から決まる、行の高さと列の幅。
    pub(super) fn list_metrics(&self) -> ListMetrics {
        let (size, _) = layout_for(self.view_mode.mode, self.view_mode.list_size);
        let row_h = row_height(size, self.config.thumb_size as f32);
        let place_w = if self.viewing_similar.is_some() { PLACE_COL_W } else { 0.0 };
        let compare_w = if self.viewing_similar.is_some() { COMPARE_COL_W } else { 0.0 };
        let widths = column_widths(thumb_col_width(size, row_h), &self.list_cols, place_w, compare_w);
        ListMetrics { row_h, widths, total_w: widths.iter().sum() }
    }

    /// リスト表示の本体。フォルダの行 → ファイルの行の順に並べる。
    /// フォルダのダブルクリックで移動先があれば返す（開く処理は呼び出し側）。
    /// `view_rect` は表示領域（行の背景を領域いっぱいに広げ、縦の区切り線をこの範囲に収めるため）。
    /// 列見出しは固定表示のため、ここでは描かない（`draw_list_header`）。
    pub(super) fn draw_archive_list(
        &mut self,
        ui: &mut egui::Ui,
        folder_entries: &[GridEntry],
        view_rect: egui::Rect,
    ) -> Option<GridNav> {
        let (size, hover_popup) = layout_for(self.view_mode.mode, self.view_mode.list_size);
        let ListMetrics { row_h, widths, total_w } = self.list_metrics();
        let row_w = total_w.max(view_rect.width());
        let origin_x = ui.cursor().min.x;
        let font = egui::FontId::proportional((row_h * 0.5).clamp(11.0, 15.0));
        let grid_focused = self.focused_pane == FocusPane::Grid;
        // サムネ列を出さない形式（ホバー・一覧）。ポップアップを出すのはリストのホバーだけ。
        let text_only = size == ListSize::Hover;
        let mut nav: Option<GridNav> = None;
        ui.spacing_mut().item_spacing.y = ROW_GAP;

        // 仮想スクロール: 全行ぶんの高さを一度だけ確保し、見えている行（＋前後1行）だけを作る。
        // 行ごとの当たり判定・パスの複製・描画は見えている行に限るので、件数が多くても重くならない。
        let n_folders = folder_entries.len();
        let n_files = self.filtered_indices.len();
        let total_rows = n_folders + n_files;
        let pitch = row_h + ROW_GAP;
        let total_h = (total_rows as f32 * pitch - ROW_GAP).max(0.0);
        let (content, _) = ui.allocate_exact_size(egui::vec2(row_w, total_h), egui::Sense::hover());
        let view_clip = ui.clip_rect().intersect(view_rect);
        let rows = visible_rows(view_clip.min.y - content.min.y, view_clip.height(), pitch, total_rows);
        let mut visible_order_range: Option<(usize, usize)> = None;
        for i in rows {
            let rect = egui::Rect::from_min_size(
                egui::pos2(content.min.x, content.min.y + i as f32 * pitch),
                egui::vec2(row_w, row_h),
            );
            let response = row_response(ui, rect, i);
            if i < n_folders {
                let entry = &folder_entries[i];
                if ui.is_rect_visible(rect) && response.hovered() {
                    ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                }
                match entry {
                    GridEntry::Up(parent) => {
                        self.paint_folder_row(ui, rect, &widths, &font, "..", None, false);
                        if self.finish_real_folder_cell(ui, rect, &response, entry.clone(), parent, grid_focused) {
                            nav = Some(GridNav::Real(parent.clone()));
                        }
                    }
                    GridEntry::Subdir(dir) => {
                        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                        let mtime = self.subdir_mtimes.get(dir).copied();
                        self.paint_folder_row(ui, rect, &widths, &font, &name, mtime, false);
                        if self.finish_real_folder_cell(ui, rect, &response, entry.clone(), dir, grid_focused) {
                            nav = Some(GridNav::Real(dir.clone()));
                        }
                    }
                    GridEntry::VirtualUp(target) => {
                        self.paint_folder_row(ui, rect, &widths, &font, "..", None, false);
                        let folder_target = self.node_folder_target(*target);
                        if self.finish_virtual_card(ui, rect, &response, entry.clone(), folder_target, grid_focused) {
                            nav = Some(GridNav::Virtual(*target));
                        }
                    }
                    GridEntry::VirtualSubdir(id) => {
                        if let Some((name, broken, mtime)) = self.virtual_row_info(*id) {
                            self.paint_folder_row(ui, rect, &widths, &font, &name, mtime, broken);
                        }
                        let folder_target = self.node_folder_target(*id);
                        if self.finish_virtual_card(ui, rect, &response, entry.clone(), folder_target, grid_focused) {
                            nav = Some(GridNav::Virtual(*id));
                        }
                    }
                    GridEntry::Archive(_) => {}
                }
            } else {
                // フィルタ適用中は filtered_indices のみ描画対象（カードと同じ）
                let pos = i - n_folders;
                let real_idx = self.filtered_indices[pos];
                let path = self.archives[real_idx].clone();
                if ui.is_rect_visible(rect) {
                    if !text_only {
                        visible_order_range = Some(match visible_order_range {
                            Some((lo, hi)) => (lo.min(pos), hi.max(pos)),
                            None => (pos, pos),
                        });
                    }
                    self.paint_archive_row(ui, rect, &response, &widths, &font, real_idx, &path, text_only);
                }
                self.handle_archive_interaction(ui, &response, real_idx, pos);
                if hover_popup && response.hovered() {
                    self.show_list_hover_thumbnail(ui, &path);
                }
            }
        }
        if let Some((lo, hi)) = visible_order_range {
            self.update_thumbnail_lookahead(lo, hi);
        }

        // 列どうしの縦の区切り線を、表示領域の全高に引く（横線は引かない）。行の選択枠やホバーの上に重ねる。
        let clip = ui.clip_rect().intersect(view_rect);
        let painter = ui.painter().with_clip_rect(clip);
        let stroke = egui::Stroke::new(1.0, grid_line_color(ui));
        let mut x = origin_x;
        for w in widths {
            x += w;
            painter.vline(x, clip.y_range(), stroke);
        }
        nav
    }

    /// 固定表示の列見出し（本体の縦スクロールの外に置く）。横スクロールは直前のフレームの量だけずらして合わせる。
    /// 列の境界は、つかんで幅を変えられる（ダブルクリックで既定幅）。境界にホバーしたときだけカーソルと線が変わる。
    pub(super) fn draw_list_header(&mut self, ui: &mut egui::Ui, view_w: f32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(view_w, HEADER_H), egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let origin_x = rect.min.x - self.list_scroll_x;
        let header_row = |total_w: f32| {
            egui::Rect::from_min_size(egui::pos2(origin_x, rect.min.y), egui::vec2(total_w, HEADER_H))
        };

        // 見出しのクリックによるソート。列境界のつかみ（下）より先に登録して、境界側を手前（優先）にする。
        let m = self.list_metrics();
        let row = header_row(m.total_w);
        let mut sort_click: Option<crate::explorer_sort::HeaderColumn> = None;
        let mut hovered_col: Option<Col> = None;
        // 重複列の見出しは、他の列と別軸のソート（マーカー付きを検出の新しい順で先頭へ）。
        let mut dup_click = false;
        let dup_cell = col_rect(row, &m.widths, Col::Dup).intersect(rect);
        if dup_cell.width() > 0.0 {
            let resp = ui.interact(dup_cell, egui::Id::new(("list_col_sort", Col::Dup as usize)), egui::Sense::click());
            if resp.hovered() {
                hovered_col = Some(Col::Dup);
            }
            dup_click = resp.clicked();
        }
        for col in [Col::Name, Col::Size, Col::Mtime, Col::Rating, Col::Visits] {
            let Some(sort_col) = col.sort_column() else { continue };
            let cell = col_rect(row, &m.widths, col).intersect(rect);
            if cell.width() <= 0.0 {
                continue;
            }
            let resp = ui.interact(cell, egui::Id::new(("list_col_sort", col as usize)), egui::Sense::click());
            if resp.hovered() {
                hovered_col = Some(col);
            }
            if resp.clicked() {
                sort_click = Some(sort_col);
            }
        }

        // 境界の操作を先に処理して、この後の描画（見出しと本体の行）に今フレームの幅を反映する。
        let mut changed = false;
        let mut active_idx: Option<usize> = None;
        for col in [Col::Dup, Col::Name, Col::Place, Col::Compare, Col::Size, Col::Mtime, Col::Rating, Col::Visits] {
            let Some(idx) = col.resize_index() else { continue };
            let x = col_rect(row, &m.widths, col).max.x;
            let grab = egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(RESIZE_GRAB_W, HEADER_H))
                .intersect(rect);
            if grab.width() <= 0.0 {
                continue;
            }
            let resp = ui.interact(grab, egui::Id::new(("list_col_resize", idx)), egui::Sense::click_and_drag());
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                active_idx = Some(idx);
            }
            if resp.dragged() {
                self.list_cols.set(idx, self.list_cols.width(idx) + resp.drag_delta().x);
            }
            if resp.double_clicked() {
                self.list_cols.reset(idx);
                changed = true;
            }
            if resp.drag_stopped() {
                changed = true;
            }
        }
        if changed {
            self.persist_state();
        }
        if let Some(col) = sort_click {
            let before = crate::explorer_sort::SortSettings {
                key: self.sort_key,
                ascending: self.sort_ascending,
                rating: self.rating_sort,
            };
            let after = crate::explorer_sort::header_click(before, col);
            self.sort_key = after.key;
            self.sort_ascending = after.ascending;
            self.rating_sort = after.rating;
            self.finish_sort_change();
            self.persist_state();
        }

        if dup_click && self.viewing_similar.is_none() {
            self.dup_sort = !self.dup_sort;
            self.finish_sort_change();
        }

        let m = self.list_metrics();
        let row = header_row(m.total_w);
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
        if let Some(col) = hovered_col.filter(|_| active_idx.is_none()) {
            painter.rect_filled(col_rect(row, &m.widths, col), 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }
        let t = i18n::t();
        let font = egui::FontId::proportional(12.0);
        let settings = crate::explorer_sort::SortSettings {
            key: self.sort_key,
            ascending: self.sort_ascending,
            rating: self.rating_sort,
        };
        for (col, label) in [
            (Col::Dup, if self.viewing_similar.is_some() { t.list_col_base() } else { t.list_col_dup() }),
            (Col::Name, t.list_col_name()),
            (Col::Place, t.list_col_place()),
            (Col::Compare, t.list_col_compare()),
            (Col::Size, t.list_col_size()),
            (Col::Mtime, t.list_col_mtime()),
            (Col::Rating, t.list_col_rating()),
            (Col::Visits, t.list_col_visits()),
        ] {
            // 並べている列は、昇順 ▲ / 降順 ▼ を付けて強調する
            let arrow = if matches!(col, Col::Dup) {
                (self.dup_sort && self.viewing_similar.is_none()).then_some(false)
            } else {
                col.sort_column().and_then(|c| crate::explorer_sort::header_arrow(&settings, c))
            };
            let (text, color) = match arrow {
                Some(true) => (format!("{label} ▲"), ui.visuals().strong_text_color()),
                Some(false) => (format!("{label} ▼"), ui.visuals().strong_text_color()),
                None => (label.to_string(), ui.visuals().weak_text_color()),
            };
            cell_text(&painter, col_rect(row, &m.widths, col), &text, &font, color);
        }
        // 境界線。つかめる状態の境界（ホバー・ドラッグ中）だけ太く強調する（リサイズのマーカー）。
        let line = grid_line_color(ui);
        let marker = ui.visuals().strong_text_color();
        for col in [Col::Dup, Col::Name, Col::Place, Col::Compare, Col::Size, Col::Mtime, Col::Rating, Col::Visits] {
            let x = col_rect(row, &m.widths, col).max.x;
            let active = col.resize_index().is_some() && col.resize_index() == active_idx;
            let stroke = if active { egui::Stroke::new(2.0, marker) } else { egui::Stroke::new(1.0, line) };
            painter.vline(x, rect.y_range(), stroke);
        }
        // 見出しの下端
        painter.hline(rect.x_range(), rect.max.y, egui::Stroke::new(1.0, line));
    }

    /// フォルダの行の中身（アイコン・名前・更新日時。リンク切れは名前の前に ⚠）。
    #[allow(clippy::too_many_arguments)]
    fn paint_folder_row(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        widths: &[f32; 9],
        font: &egui::FontId,
        name: &str,
        mtime: Option<std::time::SystemTime>,
        broken: bool,
    ) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let color = ui.visuals().text_color();
        let icon_col = col_rect(rect, widths, Col::Thumb);
        let side = (icon_col.height() - 6.0).min(icon_col.width() - 4.0).max(8.0);
        let icon_rect = egui::Rect::from_center_size(icon_col.center(), egui::vec2(side, side));
        nav_icons::draw_folder_icon(painter, icon_rect, nav_icons::NAV_ICON_COLOR);
        let label = if broken { format!("⚠ {name}") } else { name.to_string() };
        cell_text(painter, col_rect(rect, widths, Col::Name), &label, font, color);
        if let Some(t) = mtime {
            let text = format_mtime(t, &self.card_date_format, i18n::t());
            cell_text(painter, col_rect(rect, widths, Col::Mtime), &text, font, color);
        }
    }

    /// ファイルの行の中身（サムネ・名前・サイズ・更新日時・評価・訪問回数と、選択枠・カーソル）。
    #[allow(clippy::too_many_arguments)]
    fn paint_archive_row(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
        widths: &[f32; 9],
        font: &egui::FontId,
        real_idx: usize,
        path: &PathBuf,
        text_only: bool,
    ) {
        let is_selected = super::selection::is_selected(&self.multi_selected, self.selected_archive_index, real_idx);
        let is_raw = self.raw_image_files.contains(path);
        let text_color = ui.visuals().text_color();

        if self.is_similar_entry(path) {
            ui.painter().rect_filled(rect, 0.0, similar_entry_color().gamma_multiply(0.28));
        }
        let gone = self.is_similar_gone(path);
        if is_selected {
            let base = if is_raw { egui::Color32::from_rgb(220, 60, 60) } else { egui::Color32::from_rgb(50, 120, 230) };
            ui.painter().rect_filled(rect, 0.0, base.gamma_multiply(0.22));
            ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.5, base), egui::StrokeKind::Inside);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
        }

        if !text_only {
            let thumb_col = col_rect(rect, widths, Col::Thumb);
            let thumb_rect = thumb_col.shrink2(egui::vec2(CELL_PAD / 2.0, 2.0));
            self.paint_archive_thumbnail(ui, thumb_rect, path);
            if self.identity_verifying.contains(path) {
                ui.painter().rect_filled(thumb_rect, 4.0, egui::Color32::from_black_alpha(110));
            }
        }

        // 可視行ぶんだけメタデータを遅延取得（失敗時は次フレーム再試行。カードと同じ）
        if !self.archive_meta_cache.contains_key(path)
            && let Ok(md) = std::fs::metadata(path)
        {
            let mt = md.modified().unwrap_or(std::time::UNIX_EPOCH);
            self.archive_meta_cache.insert(path.clone(), (mt, md.len()));
        }
        let meta = self.archive_meta_cache.get(path).copied();

        let painter = ui.painter();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        cell_text(painter, col_rect(rect, widths, Col::Name), name, font, text_color);
        let place_cell = col_rect(rect, widths, Col::Place);
        if place_cell.width() > 0.0 {
            let full = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let shown = elide_head(ui.painter(), &full, font, place_cell.width() - CELL_PAD * 2.0);
            cell_text(ui.painter(), place_cell, &shown, font, ui.visuals().weak_text_color());
            let on_place = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| place_cell.contains(p));
            if on_place {
                response.clone().on_hover_text(&full);
            }
        }
        let compare_cell = col_rect(rect, widths, Col::Compare);
        if compare_cell.width() > 0.0
            && let Some((text, color)) = self.compare_badge(path)
        {
            cell_text(ui.painter(), compare_cell, &text, font, color);
        }
        let dup_cell = col_rect(rect, widths, Col::Dup);
        if self.viewing_similar.is_some() {
            self.paint_similar_base_mark(ui, response, super::dup_marker_ui::list_icon_rect(dup_cell), real_idx, path);
        } else {
            self.paint_marker_icon(ui, response, super::dup_marker_ui::list_icon_rect(dup_cell), real_idx, name);
        }
        let painter = ui.painter();
        if let Some((mt, sz)) = meta {
            cell_text(painter, col_rect(rect, widths, Col::Size), &humanize_size(sz), font, text_color);
            let when = format_mtime(mt, &self.card_date_format, i18n::t());
            cell_text(painter, col_rect(rect, widths, Col::Mtime), &when, font, text_color);
        }

        // 評価（未評価 or ★）と訪問回数。生画像は評価の対象外（カードと同じく「-」）。
        let rating = self.archive_rating_of(path);
        let rating_col = col_rect(rect, widths, Col::Rating);
        let painter = ui.painter();
        let half = rating.map_or(0, |r| r.rating_half);
        if is_raw {
            cell_text(painter, rating_col, "-", font, text_color);
        } else if half == 0 {
            cell_text(painter, rating_col, i18n::t().rating_unrated(), font, ui.visuals().weak_text_color());
        } else {
            let radius = crate::rating_overlay::fit_compact_radius(
                (rect.height() * 0.3).clamp(5.0, 9.0),
                rating_col.width() - CELL_PAD * 2.0,
            );
            let center = egui::pos2(rating_col.min.x + CELL_PAD + radius * 5.5, rating_col.center().y);
            crate::rating_overlay::paint_compact_stars(painter, half, center, radius);
        }
        let visits = rating.map_or(0, |r| r.visit_count);
        cell_text(painter, col_rect(rect, widths, Col::Visits), &visits.to_string(), font, text_color);

        if gone {
            // 確認の結果、もう無いファイル。薄くして、更新で一覧から外れることを待つ。
            ui.painter().rect_filled(rect, 0.0, ui.visuals().panel_fill.gamma_multiply(0.6));
        }

        if self.focused_pane == FocusPane::Grid && self.grid_cursor.as_ref() == Some(&GridEntry::Archive(real_idx)) {
            draw_cursor_ring(ui, rect);
        }
    }

    /// ホバー形式: ポインタの横にサムネのポップアップを出す（遅延なし）。画面端では反対側へ寄せる。
    fn show_list_hover_thumbnail(&mut self, ui: &egui::Ui, path: &PathBuf) {
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else { return };
        let cell_h = self.config.thumb_size as f32;
        let cell_w = (cell_h / std::f32::consts::SQRT_2).round();
        // Frame の内側余白ぶんを見込んだ全体サイズ
        let margin = ui.spacing().window_margin;
        let total = egui::vec2(
            cell_w + f32::from(margin.left) + f32::from(margin.right),
            cell_h + f32::from(margin.top) + f32::from(margin.bottom),
        );
        let screen = ui.ctx().content_rect();
        let mut pos = pointer + egui::vec2(POPUP_OFFSET, POPUP_OFFSET);
        if pos.x + total.x > screen.max.x {
            pos.x = (pointer.x - POPUP_OFFSET - total.x).max(screen.min.x);
        }
        if pos.y + total.y > screen.max.y {
            pos.y = (screen.max.y - total.y).max(screen.min.y);
        }
        let ctx = ui.ctx().clone();
        egui::Area::new(egui::Id::new("list_hover_thumbnail"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos)
            .interactable(false)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(cell_w, cell_h), egui::Sense::hover());
                    self.paint_archive_thumbnail(ui, rect, path);
                });
            });
        // サムネの生成が済むまで描き直して、できた時点で出す
        if !self.thumbnails.contains_key(path) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mode_is_a_text_only_row_without_the_hover_popup() {
        // 一覧: リストのサイズ設定に依らず文字のみ・ポップアップなし
        for size in [ListSize::S, ListSize::M, ListSize::L, ListSize::Hover] {
            assert_eq!(layout_for(ViewMode::Text, size), (ListSize::Hover, false));
        }
    }

    #[test]
    fn list_mode_shows_the_popup_only_in_hover_size() {
        assert_eq!(layout_for(ViewMode::List, ListSize::M), (ListSize::M, false));
        assert_eq!(layout_for(ViewMode::List, ListSize::Hover), (ListSize::Hover, true));
    }

    #[test]
    fn row_height_follows_the_thumbnail_setting_by_size() {
        // f32 の比率なので厳密一致ではなく許容誤差で比べる
        for (size, want) in [(ListSize::S, 50.0), (ListSize::M, 80.0), (ListSize::L, 120.0)] {
            assert!((row_height(size, 200.0) - want).abs() < 0.01, "{size:?}");
        }
    }

    #[test]
    fn visible_rows_covers_the_viewport_with_one_row_of_margin() {
        // 行ピッチ10、上端20・高さ50 → 行2〜6が掛かり（2..7）、前後1行の余裕で 1..8
        assert_eq!(visible_rows(20.0, 50.0, 10.0, 100), 1..8);
    }

    #[test]
    fn visible_rows_at_the_top_starts_from_zero() {
        assert_eq!(visible_rows(0.0, 50.0, 10.0, 100), 0..6);
    }

    #[test]
    fn visible_rows_never_exceeds_the_total() {
        assert_eq!(visible_rows(950.0, 500.0, 10.0, 100), 94..100);
        assert_eq!(visible_rows(0.0, 500.0, 10.0, 3), 0..3);
    }

    #[test]
    fn visible_rows_is_empty_without_rows_or_viewport() {
        assert!(visible_rows(0.0, 100.0, 10.0, 0).is_empty());
        assert!(visible_rows(0.0, 0.0, 10.0, 50).is_empty());
    }

    #[test]
    fn visible_rows_handles_a_viewport_above_the_content() {
        // 領域の上端が内容の上端より上（負）でも先頭から
        assert_eq!(visible_rows(-30.0, 100.0, 10.0, 100), 0..8);
    }

    #[test]
    fn scroll_to_row_scrolls_up_to_a_row_above_the_viewport() {
        let pitch = 50.0 + ROW_GAP;
        // 10行目（インデックス9）の上端が見えるところまで戻る
        assert_eq!(scroll_to_row(1000.0, 300.0, 9, 50.0), 9.0 * pitch);
    }

    #[test]
    fn scroll_to_row_scrolls_down_to_a_row_below_the_viewport() {
        let pitch = 50.0 + ROW_GAP;
        // 縦300pxの窓の下に隠れた行は、下端が窓の下端に合うまで進む
        let got = scroll_to_row(0.0, 300.0, 20, 50.0);
        assert_eq!(got, 20.0 * pitch + 50.0 - 300.0);
    }

    #[test]
    fn scroll_to_row_leaves_a_visible_row_alone() {
        assert_eq!(scroll_to_row(100.0, 300.0, 4, 50.0), 100.0);
        assert_eq!(scroll_to_row(0.0, 300.0, 0, 50.0), 0.0);
    }

    #[test]
    fn scroll_to_row_does_not_move_when_the_viewport_is_unknown() {
        // 初回フレームなど、領域の高さが未確定（0）の間は下方向へ動かさない
        assert_eq!(scroll_to_row(0.0, 0.0, 20, 50.0), 0.0);
    }

    #[test]
    fn only_name_size_mtime_rating_and_visits_are_sortable() {
        use crate::explorer_sort::HeaderColumn as H;
        assert_eq!(Col::Dup.sort_column(), None);
        assert_eq!(Col::Thumb.sort_column(), None);
        assert_eq!(Col::Place.sort_column(), None);
        assert_eq!(Col::Name.sort_column(), Some(H::Name));
        assert_eq!(Col::Size.sort_column(), Some(H::Size));
        assert_eq!(Col::Mtime.sort_column(), Some(H::Date), "更新日時は日付ソート");
        assert_eq!(Col::Rating.sort_column(), Some(H::Score));
        assert_eq!(Col::Visits.sort_column(), Some(H::Visits));
    }

    #[test]
    fn only_the_thumbnail_column_cannot_be_resized() {
        assert_eq!(Col::Thumb.resize_index(), None);
        assert_eq!(Col::Place.resize_index(), None, "場所列の幅は固定");
        let idx: Vec<_> = [Col::Dup, Col::Name, Col::Size, Col::Mtime, Col::Rating, Col::Visits]
            .into_iter()
            .map(|c| c.resize_index())
            .collect();
        assert_eq!(idx, (0..6).map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn row_height_never_goes_below_the_minimum() {
        assert_eq!(row_height(ListSize::S, 40.0), MIN_ROW_H);
        assert_eq!(row_height(ListSize::Hover, 400.0), MIN_ROW_H, "ホバー形式はサムネ設定に依らず文字のみの高さ");
    }

    #[test]
    fn hover_mode_has_only_an_icon_column_for_thumbnails() {
        assert_eq!(thumb_col_width(ListSize::Hover, 24.0), HOVER_ICON_COL_W);
        assert!(thumb_col_width(ListSize::L, 120.0) > thumb_col_width(ListSize::S, 50.0));
    }

    #[test]
    fn place_column_has_no_width_outside_the_similar_view() {
        let widths = column_widths(40.0, &ListColumns::default(), 0.0, 0.0);
        assert_eq!((widths[Col::Place as usize], widths[Col::Compare as usize]), (0.0, 0.0));
        let with = column_widths(40.0, &ListColumns::default(), PLACE_COL_W, COMPARE_COL_W);
        assert_eq!(with.iter().sum::<f32>() - widths.iter().sum::<f32>(), PLACE_COL_W + COMPARE_COL_W);
    }

    #[test]
    fn columns_tile_the_row_without_gaps_or_overlap() {
        let widths = column_widths(40.0, &ListColumns::default(), PLACE_COL_W, COMPARE_COL_W);
        let row = egui::Rect::from_min_size(egui::pos2(10.0, 5.0), egui::vec2(widths.iter().sum(), 30.0));
        let cols = [Col::Dup, Col::Thumb, Col::Name, Col::Place, Col::Compare, Col::Size, Col::Mtime, Col::Rating, Col::Visits];
        let mut x = row.min.x;
        for c in cols {
            let r = col_rect(row, &widths, c);
            assert_eq!(r.min.x, x);
            assert_eq!((r.min.y, r.height()), (row.min.y, row.height()));
            x = r.max.x;
        }
        assert_eq!(x, row.max.x);
    }
}

#[cfg(test)]
mod egui_debug_warning_tests {
    use super::*;

    /// egui のデバッグ警告（赤い矩形）が、高速スクロールで出ないこと。
    /// 警告は `error_fg_color` の枠として描かれる。リストの本体と同じ構造（仮想スクロール＋行の当たり判定）を
    /// ホバーのポップアップ込みで、行ピッチの整数倍になる量を含めて動かす。
    #[test]
    fn fast_scrolling_does_not_trigger_the_rect_id_warning() {
        let ctx = egui::Context::default();
        let err = ctx.global_style().visuals.error_fg_color;
        let (total_rows, row_h) = (5000usize, 24.0f32);
        let pitch = row_h + ROW_GAP;
        let mut red_rects = 0;
        for frame in 0..120 {
            let mut input = egui::RawInput::default();
            input.screen_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)));
            input.events.push(egui::Event::PointerMoved(egui::pos2(100.0, 200.0)));
            // 7フレームごとに 50px（=行ピッチ25の2倍）。それ以外は半端な量
            let dy = if frame % 7 == 0 { 50.0 } else { 37.0 + (frame % 7) as f32 * 91.0 };
            input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -dy),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            let out = ctx.run_ui(input, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = ROW_GAP;
                    let (content, _) =
                        ui.allocate_exact_size(egui::vec2(700.0, total_rows as f32 * pitch), egui::Sense::hover());
                    let clip = ui.clip_rect();
                    for i in visible_rows(clip.min.y - content.min.y, clip.height(), pitch, total_rows) {
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(content.min.x, content.min.y + i as f32 * pitch),
                            egui::vec2(700.0, row_h),
                        );
                        let _ = row_response(ui, rect, i);
                    }
                });
            });
            for clipped in &out.shapes {
                count_red_rects(&clipped.shape, err, &mut red_rects);
            }
        }
        assert_eq!(red_rects, 0, "高速スクロールでeguiのIDの警告（赤い矩形）が出ている");
    }

    fn count_red_rects(shape: &egui::epaint::Shape, err: egui::Color32, n: &mut usize) {
        match shape {
            egui::epaint::Shape::Rect(r) if r.stroke.color == err => *n += 1,
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| count_red_rects(s, err, n)),
            _ => {}
        }
    }
}
