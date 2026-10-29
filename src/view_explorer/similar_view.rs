//! 類似ファイル一覧: 重複マーカーのポップアップから開く、同じ内容のファイルだけを並べる一時ビュー。
//!
//! - 現在のタブの中央グリッドを差し替える（お気に入り・検索の横断一覧と同じ「archives を差し替える」方式）。
//!   タブも検索履歴も触らない。閉じると、入口にしたフォルダの表示に戻る
//! - 並べるのは、入口のファイルと同じFPの現存ファイル全て（件数の上限なし）。消失・オフラインは数だけ数える
//! - 収集は専用スレッド（marker-probe）で行い、届くまでは空の一覧に「集めています」を出す
//! - 実フォルダのスキャンや状態の再読み込みが一覧を上書きしないよう、`start_scan` は一覧を閉じる扱い、
//!   `reload_dir_state_maps` は何もしない

use std::path::PathBuf;

use super::marker_compare::{CompareOutcome, CompareState, PairKey};
use super::marker_probe::{SimilarFile, SimilarGroup};
use super::*;

/// 開いている類似ファイル一覧。
pub(super) struct SimilarView {
    /// 入口にしたファイル。
    pub entry_path: PathBuf,
    /// 並べているファイル（ID昇順。先頭が最古 = 完全比較の基準）。
    pub files: Vec<SimilarFile>,
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
            files: Vec::new(),
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
        view.files = group.files.clone();
        let paths: Vec<PathBuf> = group.files.into_iter().map(|f| f.path).collect();
        self.prune_stale_compare_results();
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

    /// 完全比較の基準（最古のID）のファイル。比べる相手が無ければ None。
    fn compare_base(&self) -> Option<&SimilarFile> {
        let view = self.viewing_similar.as_ref()?;
        (view.files.len() >= 2).then(|| &view.files[0])
    }

    /// 一覧のファイルのサイズ・更新日時が、結果を得た時から変わった組の結果を捨てる。
    fn prune_stale_compare_results(&mut self) {
        let Some(view) = &self.viewing_similar else { return };
        let stat_of = |id: u64| view.files.iter().find(|f| f.id == id).map(|f| f.stat.as_ref());
        let stale: Vec<PairKey> = self
            .marker_compare_stamps
            .iter()
            .filter(|(key, stamped)| match (stat_of(key.0), stat_of(key.1)) {
                (Some(a), Some(b)) => !super::marker_compare::stamps_valid(stamped, a, b),
                // 一覧に居ない組は、この一覧とは関係ない（そのまま残す）。
                _ => false,
            })
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.marker_compare_stamps.remove(&key);
            self.marker_compare_states.remove(&key);
        }
    }

    /// 基準と他の全てのファイルを、1組ずつ順に完全比較する。結果のある組（一致・相違）は飛ばす。
    pub(super) fn start_similar_compare_all(&mut self) {
        let Some(view) = &self.viewing_similar else { return };
        let Some(base) = self.compare_base().cloned() else { return };
        let others: Vec<SimilarFile> = view.files[1..].to_vec();
        for other in others {
            let key = (base.id, other.id);
            if matches!(
                self.marker_compare_states.get(&key),
                Some(CompareState::Running { .. } | CompareState::Done(CompareOutcome::Identical | CompareOutcome::Different))
            ) {
                continue;
            }
            self.start_marker_compare(key, base.path.clone(), other.path);
        }
    }

    /// 全ての組に結果があるか（一括比較を押す意味が無いか）。
    fn all_pairs_settled(&self) -> bool {
        let Some(base) = self.compare_base() else { return true };
        let Some(view) = &self.viewing_similar else { return true };
        view.files[1..].iter().all(|f| {
            matches!(
                self.marker_compare_states.get(&(base.id, f.id)),
                Some(CompareState::Done(CompareOutcome::Identical | CompareOutcome::Different))
            )
        })
    }

    /// そのファイルの完全比較の結果バッジ（文言と色）。基準の行は、比較が始まっていれば「基準」。
    pub(super) fn compare_badge(&self, path: &std::path::Path) -> Option<(String, egui::Color32)> {
        let base = self.compare_base()?;
        let view = self.viewing_similar.as_ref()?;
        let id = view.files.iter().find(|f| f.path == path)?.id;
        let t = i18n::t();
        if id == base.id {
            let started = view.files[1..].iter().any(|f| self.marker_compare_states.contains_key(&(base.id, f.id)));
            return started.then(|| (t.similar_badge_base().to_owned(), egui::Color32::from_rgb(120, 130, 150)));
        }
        Some(match self.marker_compare_states.get(&(base.id, id))? {
            CompareState::Running { done, total, .. } => {
                let pct = if *total == 0 { 0 } else { done * 100 / total };
                (t.similar_badge_running(pct), egui::Color32::from_rgb(60, 130, 220))
            }
            CompareState::Done(CompareOutcome::Identical) => {
                (t.similar_badge_identical().to_owned(), egui::Color32::from_rgb(40, 160, 90))
            }
            CompareState::Done(CompareOutcome::Different) => {
                (t.similar_badge_different().to_owned(), egui::Color32::from_rgb(215, 70, 60))
            }
            CompareState::Done(CompareOutcome::Changed) => {
                (t.similar_badge_changed().to_owned(), egui::Color32::from_rgb(210, 150, 30))
            }
            CompareState::Done(CompareOutcome::Failed | CompareOutcome::Cancelled) => {
                (t.similar_badge_failed().to_owned(), egui::Color32::from_rgb(130, 130, 130))
            }
        })
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
        ui.separator();
        let running = self.running_marker_compares();
        if running > 0 {
            ui.label(t.similar_compare_running(running));
            if ui.button(t.similar_compare_cancel_all()).clicked() {
                self.cancel_all_marker_compares();
            }
        } else {
            let enabled = self.compare_base().is_some() && !self.all_pairs_settled();
            if ui.add_enabled(enabled, egui::Button::new(t.similar_compare_all())).clicked() {
                self.start_similar_compare_all();
            }
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
