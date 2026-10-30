//! リストの「エラー」列（最左）と「属性」列（最右）のセル描画。
//!
//! どちらも固定スロット方式: 該当しないスロットは詰めずに空けて、行どうしを縦に見比べられるようにする。
//! エラー列は [赤X][⚠]（無効ZIP・サムネ失敗 / ネットワーク切れ）の2スロット。
//! 属性列は [L/R][S][T][B]（保存設定）＋ お気に入り3スロットの計7スロット。
//! お気に入りは先頭から順に3つまで並べ、4つ目以降はホバーの一覧で見る。

use std::path::Path;

use crate::i18n;

use super::*;

/// エラー列の1スロットの辺（px）。アイコンはこの正方形の中に描く。
const SLOT_SIZE: f32 = 16.0;
const RED_X_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 50, 50);
const WARN_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 160, 40);

/// エラー列の2スロットの矩形（左から 赤X・⚠）。セルの中央に、等間隔で並べる。
pub(super) fn error_slot_rects(cell: egui::Rect) -> [egui::Rect; 2] {
    let slot_w = cell.width() / 2.0;
    let at = |i: usize| {
        let center = egui::pos2(cell.min.x + slot_w * (i as f32 + 0.5), cell.center().y);
        egui::Rect::from_center_size(center, egui::vec2(SLOT_SIZE, SLOT_SIZE))
    };
    [at(0), at(1)]
}

/// 属性列の保存設定スロットの数（L/R・S・T・B）。
pub(super) const SAVED_SLOTS: usize = 4;
/// 属性列のお気に入りスロットの数。
pub(super) const FAVORITE_SLOTS: usize = 3;
/// 属性列の全スロット数。
pub(super) const ATTR_SLOTS: usize = SAVED_SLOTS + FAVORITE_SLOTS;

/// 属性列の `slot` 番目（0始まり。保存設定 → お気に入りの順）の矩形。セルを全スロットで等分する。
pub(super) fn attr_slot_rect(cell: egui::Rect, slot: usize) -> egui::Rect {
    let slot_w = cell.width() / ATTR_SLOTS as f32;
    let center = egui::pos2(cell.min.x + slot_w * (slot as f32 + 0.5), cell.center().y);
    egui::Rect::from_center_size(center, egui::vec2(SLOT_SIZE, SLOT_SIZE))
}

/// 赤X（無効ZIP・サムネ失敗）。カードと同じ見た目。
fn paint_red_x(painter: &egui::Painter, rect: egui::Rect) {
    let r = rect.shrink(1.0);
    let stroke = egui::Stroke::new(2.5, RED_X_COLOR);
    painter.line_segment([r.left_top(), r.right_bottom()], stroke);
    painter.line_segment([r.right_top(), r.left_bottom()], stroke);
}

/// ⚠（ネットワーク切れ）。カードと同じ見た目。
fn paint_warn(painter: &egui::Painter, rect: egui::Rect) {
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, "⚠", egui::FontId::proportional(14.0), WARN_COLOR);
}

impl NekoviewApp {
    /// 「エラー」列のセル。赤X = 無効ZIP・サムネのデコード失敗、⚠ = 大元マウントに到達できない。
    /// 両方あてはまる行は2つとも出す。ホバーで理由を出す。
    pub(super) fn paint_error_cell(&self, ui: &egui::Ui, response: &egui::Response, cell: egui::Rect, path: &Path) {
        if cell.width() <= 0.0 {
            return;
        }
        let t = i18n::t();
        let [x_slot, warn_slot] = error_slot_rects(cell);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hovered = |r: egui::Rect| pointer.is_some_and(|p| r.expand(2.0).contains(p));

        let thumb_failed = self.thumb_failed.contains(path);
        if self.invalid_archives.contains(path) || thumb_failed {
            paint_red_x(ui.painter(), x_slot);
            if hovered(x_slot) {
                let tip = if thumb_failed {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
                    t.list_error_thumb_tip(ext)
                } else {
                    t.list_error_invalid_tip().to_string()
                };
                response.clone().on_hover_text(tip);
            }
        }
        if let Some(root) = self.network_mount_root_cached(path)
            && self.network_unreachable_mounts.contains(&root)
        {
            paint_warn(ui.painter(), warn_slot);
            if hovered(warn_slot) {
                response.clone().on_hover_text(t.list_error_network_tip());
            }
        }
    }
}

/// お気に入りマーカー1件の表示内容。
#[derive(Debug, PartialEq)]
struct FavMark {
    marker: String,
    color: egui::Color32,
    /// フォルダ名。フォルダ指定なしの登録（既定の★）は None。
    name: Option<String>,
}

/// ファイルに付いたお気に入りの ID 列から、表示するマーカーを順に作る。
/// ID 列が空 = フォルダ指定なしで登録しただけ（既定の★）。一覧に無い ID は飛ばす（カードと同じ）。
fn favorite_marks(ids: &[u8], folders: &[crate::favorites::FavoriteFolder]) -> Vec<FavMark> {
    if ids.is_empty() {
        return vec![FavMark { marker: "★".to_string(), color: default_favorite_color(), name: None }];
    }
    ids.iter()
        .filter_map(|id| folders.iter().find(|f| f.id == *id))
        .map(|f| FavMark { marker: f.marker.clone(), color: rgba_u32_to_color32(f.color_rgba), name: Some(f.name.clone()) })
        .collect()
}

/// お気に入りの一覧（ホバーで出す。スロットに入らない分も含めて全件）。
fn favorites_tip(marks: &[FavMark]) -> String {
    marks
        .iter()
        .map(|m| match &m.name {
            Some(name) => format!("{} {}", m.marker, name),
            None => m.marker.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 保存設定マーカー（L/R・S・T・B）。カードと同じ濃い灰色の角丸に白文字。
fn paint_saved_label(painter: &egui::Painter, rect: egui::Rect, label: &str) {
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgba_unmultiplied(64, 64, 64, 191));
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::monospace(12.0), egui::Color32::WHITE);
}

/// 保存設定マーカー `slot`（0=L/R, 1=S, 2=T, 3=B）のホバー説明。
fn saved_slot_tip(slot: usize, label: &str) -> String {
    let t = i18n::t();
    match slot {
        0 => if label == "L" { t.page_spread_left() } else { t.page_spread_right() }.to_string(),
        1 => t.list_attr_sort_tip().to_string(),
        2 => t.list_attr_thumb_tip().to_string(),
        _ => t.list_attr_bookmark_tip().to_string(),
    }
}

impl NekoviewApp {
    /// ファイルに付いたお気に入りフォルダの ID 列。横断一覧（お気に入り・検索・類似・タグ検索）の表示中は
    /// フルパスキー、通常のディレクトリ表示中はファイル名キーで引く。カードとリストで共通。
    pub(super) fn favorite_marker_ids(&self, path: &Path) -> Option<&Vec<u8>> {
        if self.viewing_favorites.is_some()
            || self.viewing_search.is_some()
            || self.viewing_similar.is_some()
            || self.folder_pane_tab == FolderPaneTab::TagSearch
        {
            self.cross_view_favorite_markers.get(path)
        } else {
            self.favorite_states.get(path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
        }
    }

    /// 「属性」列のセル。保存設定の4スロット → お気に入りの3スロット。該当しないスロットは空けたまま（詰めない）。
    pub(super) fn paint_attr_cell(&self, ui: &egui::Ui, response: &egui::Response, cell: egui::Rect, path: &Path) {
        if cell.width() <= 0.0 {
            return;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hovered = |r: egui::Rect| pointer.is_some_and(|p| r.expand(1.0).contains(p));
        if let Some(settings) = self.saved_archive_settings.get(path) {
            for (slot, label) in super::panels::saved_setting_marker_labels(*settings).into_iter().enumerate() {
                let Some(label) = label else { continue };
                let rect = attr_slot_rect(cell, slot);
                paint_saved_label(ui.painter(), rect, label);
                if hovered(rect) {
                    response.clone().on_hover_text(saved_slot_tip(slot, label));
                }
            }
        }
        if let Some(ids) = self.favorite_marker_ids(path) {
            let marks = favorite_marks(ids, &self.favorite_folders);
            let font = egui::FontId::proportional(14.0);
            let mut any_hovered = false;
            for (i, mark) in marks.iter().take(FAVORITE_SLOTS).enumerate() {
                let rect = attr_slot_rect(cell, SAVED_SLOTS + i);
                ui.painter().with_clip_rect(rect.expand(1.0)).text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &mark.marker,
                    font.clone(),
                    mark.color,
                );
                any_hovered |= hovered(rect);
            }
            if any_hovered {
                response.clone().on_hover_text(favorites_tip(&marks));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_slots_stay_inside_the_cell_without_overlap() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 4.0), egui::vec2(44.0, 24.0));
        let [a, b] = error_slot_rects(cell);
        assert!(cell.contains_rect(a) && cell.contains_rect(b));
        assert!(a.max.x <= b.min.x, "赤Xと⚠は重ならない");
        assert!(a.center().x < b.center().x, "左が赤X、右が⚠");
        assert_eq!(a.center().y, cell.center().y);
    }

    #[test]
    fn attr_slots_tile_the_cell_in_order_without_overlap() {
        let cell = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(126.0, 24.0));
        let mut prev_right = cell.min.x;
        for slot in 0..ATTR_SLOTS {
            let r = attr_slot_rect(cell, slot);
            assert!(cell.contains_rect(r), "スロット{slot}がセルからはみ出している");
            assert!(r.min.x >= prev_right, "スロット{slot}が前のスロットと重なっている");
            prev_right = r.max.x;
        }
        assert_eq!(ATTR_SLOTS, 7);
    }

    fn folder(id: u8, name: &str, marker: &str) -> crate::favorites::FavoriteFolder {
        crate::favorites::FavoriteFolder { id, name: name.to_string(), marker: marker.to_string(), color_rgba: 0xff0000ff, order: id }
    }

    #[test]
    fn a_favorite_without_folders_is_the_default_star() {
        let marks = favorite_marks(&[], &[folder(1, "a", "●")]);
        assert_eq!(marks.len(), 1);
        assert_eq!((marks[0].marker.as_str(), marks[0].name.as_deref()), ("★", None));
    }

    #[test]
    fn favorite_marks_follow_the_id_order_and_skip_unknown_ids() {
        let folders = [folder(1, "one", "A"), folder(2, "two", "B")];
        let marks = favorite_marks(&[2, 9, 1], &folders);
        let got: Vec<_> = marks.iter().map(|m| m.marker.as_str()).collect();
        assert_eq!(got, ["B", "A"], "未知のIDは飛ばし、IDの並びのまま");
    }

    #[test]
    fn the_tip_lists_every_favorite_even_beyond_the_slots() {
        let folders = [folder(1, "one", "A"), folder(2, "two", "B"), folder(3, "three", "C"), folder(4, "four", "D")];
        let marks = favorite_marks(&[1, 2, 3, 4], &folders);
        assert!(marks.len() > FAVORITE_SLOTS);
        assert_eq!(favorites_tip(&marks), "A one\nB two\nC three\nD four");
        assert_eq!(favorites_tip(&favorite_marks(&[], &[])), "★");
    }

    #[test]
    fn saved_slot_tips_distinguish_left_and_right_spread() {
        assert_ne!(saved_slot_tip(0, "L"), saved_slot_tip(0, "R"));
    }
}
