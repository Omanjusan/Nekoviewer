//! 類似ファイル一覧: 重複マーカーのポップアップから開く、同じ内容のファイルだけを並べる一時ビュー。
//!
//! - 現在のタブの中央グリッドを差し替える（お気に入り・検索の横断一覧と同じ「archives を差し替える」方式）。
//!   タブも検索履歴も触らない。閉じると、入口にしたフォルダの表示に戻る
//! - 並べるのは、入口のファイルと同じFPの現存ファイル全て（件数の上限なし）。消失・オフラインは数だけ数える
//! - 収集は専用スレッド（marker-probe）で行い、届くまでは空の一覧に「集めています」を出す
//! - 実フォルダのスキャンや状態の再読み込みが一覧を上書きしないよう、`start_scan` は一覧を閉じる扱い、
//!   `reload_dir_state_maps` は何もしない

use std::path::PathBuf;

use super::marker_probe::SimilarGroup;
use super::*;

/// 開いている類似ファイル一覧。
pub(super) struct SimilarView {
    /// 入口にしたファイル。
    pub entry_path: PathBuf,
    pub entry_id: u64,
    /// 収集の結果待ち。
    pub loading: bool,
    /// 一覧に出せなかったファイルの数（消失／オフライン）。
    pub missing: usize,
    pub offline: usize,
    /// 収集依頼の番号（`NekoviewApp::similar_token`）。
    token: u64,
}

/// 告知バーの高さ（px）と背景色（青と水色の中間）。
const BANNER_H: f32 = 22.0;
const BANNER_COLOR: egui::Color32 = egui::Color32::from_rgb(40, 150, 215);

/// カードの行どうしの隙間（キー移動のスクロール計算と同じ値）。
const CARD_ROW_GAP: f32 = 8.0;

/// `index` 番目の項目（高さ `item_h`、ピッチ `pitch`）が表示領域の中央に来るスクロール量。
/// 表示領域の高さが分からない（0以下）ときは、項目を上端に合わせる。
fn centered_offset(index: usize, pitch: f32, item_h: f32, viewport_h: f32) -> f32 {
    let top = index as f32 * pitch;
    if viewport_h <= 0.0 {
        return top;
    }
    (top - (viewport_h - item_h) / 2.0).max(0.0)
}

impl NekoviewApp {
    /// 類似ファイル一覧を開く。`entry_path` は入口のファイル、`entry_id` はそのファイルID。
    pub(super) fn enter_similar_view(&mut self, entry_path: PathBuf, entry_id: u64) {
        let Some(db) = self.spread_db.clone() else { return };
        self.marker_popup = None;
        self.similar_token += 1;
        let token = self.similar_token;
        // 一覧中はリスト表示（サムネS/M/Lと「場所」列で見比べる）。閉じるときに元のモードへ戻す。
        // すでに一覧を開いているときの開き直しでは、最初に保存したモードを残す。
        if self.similar_saved_mode.is_none() {
            self.similar_saved_mode = Some(self.view_mode.mode);
        }
        self.view_mode.mode = view_mode::ViewMode::List;
        self.viewing_similar = Some(SimilarView {
            entry_path,
            entry_id,
            loading: true,
            missing: 0,
            offline: 0,
            token,
        });
        self.show_similar_paths(Vec::new());
        self.marker_probe.submit_group(db, entry_id, token);
    }

    /// 類似ファイル一覧を閉じ、入口にしたフォルダの表示に戻す。開いていなければ何もしない。
    pub(super) fn exit_similar_view(&mut self) {
        let Some(view) = self.close_similar_view_state() else { return };
        self.similar_return = Some(view.entry_path);
        self.start_scan();
    }

    /// 一覧の状態を畳み、表示モードを開く前に戻す（実スキャンの開始は呼び出し側）。
    /// 一覧を抜ける全ての経路（スキャン開始・お気に入り・検索・仮想ノード）がここを通る。
    pub(super) fn close_similar_view_state(&mut self) -> Option<SimilarView> {
        let view = self.viewing_similar.take();
        if let Some(mode) = self.similar_saved_mode.take() {
            self.view_mode.mode = mode;
        }
        view
    }

    /// 入口にしたファイルか（行・カードの背景色に使う）。
    pub(super) fn is_similar_entry(&self, path: &std::path::Path) -> bool {
        self.viewing_similar.as_ref().is_some_and(|v| v.entry_path == path)
    }

    /// 一覧から戻った後の再スキャンが済んだとき、入口にしていたファイルを選び直して、その位置までスクロールする。
    /// 削除済み・別のフォルダへ移った後などで見つからなければ何もしない。
    pub(super) fn restore_similar_return(&mut self) {
        let Some(path) = self.similar_return.take() else { return };
        let Some(idx) = self.archives.iter().position(|p| *p == path) else { return };
        let entries = self.grid_entries();
        let Some(pos) = entries.iter().position(|e| *e == GridEntry::Archive(idx)) else { return };
        self.set_grid_cursor(GridEntry::Archive(idx));
        self.select_anchor = Some(idx);
        let vp = self.explorer_viewport_h;
        self.explorer_scroll_offset = if self.view_mode.mode == view_mode::ViewMode::Card {
            let cell_h = self.config.thumb_size as f32;
            let row = pos / self.explorer_cols.max(1);
            centered_offset(row, cell_h + CARD_ROW_GAP, cell_h, vp)
        } else {
            let row_h = self.list_metrics().row_h;
            centered_offset(pos, row_h + list_view::ROW_GAP, row_h, vp)
        };
    }

    /// 収集結果を受け取る。古い一覧（閉じた後・開き直した後）の結果は捨てる。
    pub(super) fn apply_similar_group(&mut self, token: u64, group: SimilarGroup) {
        let Some(view) = self.viewing_similar.as_mut().filter(|v| v.token == token) else { return };
        view.loading = false;
        view.missing = group.missing;
        view.offline = group.offline;
        let entry = view.entry_path.clone();
        let paths: Vec<PathBuf> = group.files.into_iter().map(|f| f.path).collect();
        self.show_similar_paths(paths);
        // 入口のファイルを選んでおく（無ければ先頭）。
        if let Some(pos) = self.archives.iter().position(|p| *p == entry) {
            self.selected_archive_index = Some(pos);
        }
    }

    /// 類似ファイル一覧であることを示す告知バー（中央ペインの最上部。カードとリストで共通）。
    pub(super) fn draw_similar_banner(&self, ui: &mut egui::Ui) {
        if self.viewing_similar.is_none() {
            return;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BANNER_H), egui::Sense::hover());
        ui.painter().rect_filled(rect, 0.0, BANNER_COLOR);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            i18n::t().similar_banner(),
            egui::FontId::proportional(14.0),
            egui::Color32::WHITE,
        );
    }

    /// パス行に出す、戻るボタンと見出し。
    pub(super) fn draw_similar_header(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        if ui.button(t.similar_back()).clicked() {
            self.exit_similar_view();
            return;
        }
        let Some(view) = &self.viewing_similar else { return };
        if view.loading {
            ui.label(t.similar_loading());
            return;
        }
        let unlisted = view.missing + view.offline;
        let header = if self.archives.len() < 2 {
            t.similar_empty().to_owned()
        } else {
            t.similar_header(self.archives.len())
        };
        ui.label(header);
        if unlisted > 0 {
            ui.label(egui::RichText::new(t.similar_unlisted(unlisted)).weak());
        }
    }

    /// 中央グリッドを `paths` のフラットな一覧にする（`enter_search_view` と同じ要領）。
    fn show_similar_paths(&mut self, paths: Vec<PathBuf>) {
        self.archives = paths;
        self.cross_view_favorite_markers = self
            .spread_db
            .as_ref()
            .map(|db| crate::favorites::memberships_for_paths(db, &self.archives))
            .unwrap_or_default();
        self.saved_archive_settings = self
            .spread_db
            .as_ref()
            .map(|db| crate::spread_state::saved_settings_for_paths(db, &self.archives))
            .unwrap_or_default();
        self.raw_image_files.clear();
        // 階層概念を持ち込まない平坦一覧という契約のため、サブフォルダ一覧も空にする。
        self.subdirs.clear();
        self.subdir_mtimes.clear();
        // 複数ディレクトリ横断のため、単一ディレクトリ前提のキャッシュDB/セッション状態は無効化する。
        self.cache_db = None;
        self.invalid_archives.clear();
        self.thumb_failed.clear();
        self.marker_icon_hits.clear();
        // 実ディレクトリのスキャンが Loading のまま残ると、構築済みの一覧よりローディング表示が優先され続ける。
        self.scan_state = ScanState::Done;
        self.sort_archives();
        self.recompute_filter();
        self.selected_archive_index = if self.archives.is_empty() { None } else { Some(0) };
        self.selected_archive_meta = None;
        self.multi_selected.clear();
        self.select_anchor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_offset_puts_the_item_in_the_middle() {
        // 項目0〜: ピッチ50・高さ40・表示領域300。20番目の上端は1000 → 中央に来る量は 1000 - (300-40)/2。
        assert_eq!(centered_offset(20, 50.0, 40.0, 300.0), 870.0);
    }

    #[test]
    fn centered_offset_never_scrolls_above_the_top() {
        assert_eq!(centered_offset(0, 50.0, 40.0, 300.0), 0.0);
        assert_eq!(centered_offset(1, 50.0, 40.0, 300.0), 0.0);
    }

    #[test]
    fn unknown_viewport_aligns_the_item_to_the_top() {
        assert_eq!(centered_offset(4, 50.0, 40.0, 0.0), 200.0);
    }
}
