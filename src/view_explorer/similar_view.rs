//! 類似ファイル一覧: 重複マーカーのポップアップから開く、同じ内容のファイルだけを並べる一時ビュー。
//!
//! - 現在のタブの中央グリッドを差し替える（お気に入り・検索の横断一覧と同じ「archives を差し替える」方式）。
//!   タブも検索履歴も触らない。閉じると、入口にしたフォルダの表示に戻る
//! - 並べるのは、入口のファイルと同じFPの現存ファイル全て（件数の上限なし）。消失・オフラインは数だけ数える
//! - 収集は専用スレッド（marker-probe）で行い、届くまでは空の一覧に「集めています」を出す
//! - 実フォルダのスキャンや状態の再読み込みが一覧を上書きしないよう、`start_scan` は一覧を閉じる扱い、
//!   `reload_dir_state_maps` は何もしない

use std::path::PathBuf;

use crate::file_identity::Presence;

use super::marker_compare::{CompareOutcome, CompareState, PairKey};
use super::marker_probe::{SimilarFile, SimilarGroup};
use super::*;

/// 開いている類似ファイル一覧。
pub(super) struct SimilarView {
    /// 入口にしたファイル。
    pub entry_path: PathBuf,
    /// 入口にしたファイルのID（一覧の作り直しで、同じ組を引き直すのに使う）。
    pub entry_id: u64,
    /// 並べているファイル（ID昇順）。
    pub files: Vec<SimilarFile>,
    /// 完全比較の基準のファイルID。ユーザーが付け替えるまでは最古のID（`files` の先頭）。
    /// 一覧に無いIDのときは、最古のIDとして扱う。
    pub base_id: Option<u64>,
    /// 収集の結果待ち。
    pub loading: bool,
    /// 一覧に出せなかったファイルの数（消失／オフライン）。
    pub unlisted: Vec<(PathBuf, Presence)>,
    /// 収集依頼の番号（`NekoviewApp::similar_token`）。
    token: u64,
    /// 似ている画像ビュー（サムネの類似）か。false は同じ内容（FP一致）の一覧。
    /// 似ている画像ビューには基準も完全比較も無く、「完全比較」列は入口との類似度になる。
    pub look_alike: bool,
    /// 似ている画像ビューの、ファイルID → 入口との類似度(%)。入口は100。
    pub pcts: std::collections::HashMap<u64, u8>,
}

/// 告知バーの高さ（px）と背景色（青と水色の中間）。
const BANNER_H: f32 = 22.0;
const BANNER_COLOR: egui::Color32 = egui::Color32::from_rgb(40, 150, 215);
/// 似ている画像ビューの告知バーの色（「？」アイコンと同じ紫）。
const LOOK_ALIKE_BANNER_COLOR: egui::Color32 = egui::Color32::from_rgb(140, 95, 215);

/// 基準のファイル。既定（`base_id` が無い・一覧に無い）は最古のID。比べる相手が無い（2件未満）ときは None。
fn base_of(files: &[SimilarFile], base_id: Option<u64>) -> Option<&SimilarFile> {
    if files.len() < 2 {
        return None;
    }
    base_id.and_then(|id| files.iter().find(|f| f.id == id)).or_else(|| files.first())
}

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
            files: Vec::new(),
            base_id: None,
            loading: true,
            unlisted: Vec::new(),
            token,
            look_alike: false,
            pcts: std::collections::HashMap::new(),
        });
        self.show_similar_paths(Vec::new());
        self.marker_probe.submit_group(db, entry_id, token);
    }

    /// 似ている画像ビューか。
    pub(super) fn is_look_alike_view(&self) -> bool {
        self.viewing_similar.as_ref().is_some_and(|v| v.look_alike)
    }

    /// 似ている画像ビューに並べるファイル（入口を含む）と、入口との類似度。
    /// 照合結果（`sim_matches`）のうち、いまのしきい値を満たす相手。相手のパスはIDのレコードから引く。
    fn look_alike_files(
        &self,
        entry_path: &std::path::Path,
        entry_id: u64,
    ) -> Option<(Vec<SimilarFile>, std::collections::HashMap<u64, u8>)> {
        use redb::ReadableDatabase;
        let db = self.spread_db.as_ref()?;
        let name = entry_path.file_name()?.to_str()?;
        let mut files = vec![SimilarFile { id: entry_id, path: entry_path.to_path_buf(), stat: None }];
        let mut pcts = std::collections::HashMap::from([(entry_id, 100_u8)]);
        let Some(matches) = self.sim_matches.get(name) else { return Some((files, pcts)) };
        let guard = db.lock().ok()?;
        let tx = guard.begin_read().ok()?;
        for m in matches.iter().take_while(|m| m.pct() >= self.sim_threshold_pct) {
            let Some(rec) = crate::file_identity::record_by_id_tx(&tx, m.other) else { continue };
            files.push(SimilarFile { id: m.other, path: crate::file_identity::path_of_key(&rec.path_key), stat: None });
            pcts.insert(m.other, m.pct());
        }
        Some((files, pcts))
    }

    /// 似ている画像ビューを開く。`entry_path` は入口のファイル（「？」を押したファイル）、`entry_id` はそのファイルID。
    /// 照合は済んでいるので、収集の待ちは無い。
    pub(super) fn enter_look_alike_view(&mut self, entry_path: PathBuf, entry_id: u64) {
        let Some((files, pcts)) = self.look_alike_files(&entry_path, entry_id) else { return };
        self.marker_popup = None;
        self.similar_token += 1;
        let token = self.similar_token;
        if self.similar_saved_mode.is_none() {
            self.similar_saved_mode = Some(self.view_mode.mode);
        }
        self.view_mode.mode = view_mode::ViewMode::List;
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        self.viewing_similar = Some(SimilarView {
            entry_path: entry_path.clone(),
            entry_id,
            files,
            base_id: None,
            loading: false,
            unlisted: Vec::new(),
            token,
            look_alike: true,
            pcts,
        });
        self.show_similar_paths(paths);
        if let Some(pos) = self.archives.iter().position(|p| *p == entry_path) {
            self.selected_archive_index = Some(pos);
        }
    }

    /// 似ている画像ビューの並びを、いまの照合結果としきい値で作り直す（「更新」とコンボボックスの変更）。
    /// 似ている画像ビューでなければ何もしない。
    pub(super) fn rebuild_look_alike(&mut self) {
        let Some(view) = self.viewing_similar.as_ref().filter(|v| v.look_alike) else { return };
        let (entry_path, entry_id) = (view.entry_path.clone(), view.entry_id);
        let Some((files, pcts)) = self.look_alike_files(&entry_path, entry_id) else { return };
        let previous = self.selected_archive_index.and_then(|i| self.archives.get(i)).cloned();
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        if let Some(view) = self.viewing_similar.as_mut() {
            view.files = files;
            view.pcts = pcts;
        }
        self.show_similar_paths(paths);
        let keep = previous.filter(|p| self.archives.contains(p)).unwrap_or(entry_path);
        if let Some(pos) = self.archives.iter().position(|p| *p == keep) {
            self.selected_archive_index = Some(pos);
        }
    }

    /// 入口と、一覧に並んでいる全ての相手を「別物」として消し（ペアで記録）、元のフォルダへ戻る。
    pub(super) fn dismiss_look_alike_pairs(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let Some(view) = self.viewing_similar.as_ref().filter(|v| v.look_alike) else { return };
        let entry_id = view.entry_id;
        let dismissed = view
            .files
            .iter()
            .filter(|f| f.id != entry_id && crate::thumb_hash_store::dismiss_pair(&db, entry_id, f.id, crate::file_identity::now_unix()))
            .count();
        self.set_toast(i18n::t().sim_dismissed_toast(dismissed));
        self.exit_similar_view();
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
        view.unlisted = group.unlisted;
        let entry = view.entry_path.clone();
        let previous = self.selected_archive_index.and_then(|i| self.archives.get(i)).cloned();
        let Some(view) = self.viewing_similar.as_mut() else { return };
        view.files = group.files.clone();
        // 付け替えた基準が一覧から消えていたら、最古のIDに戻す。
        if view.base_id.is_some_and(|id| !view.files.iter().any(|f| f.id == id)) {
            view.base_id = None;
        }
        let paths: Vec<PathBuf> = group.files.into_iter().map(|f| f.path).collect();
        self.prune_stale_compare_results();
        self.show_similar_paths(paths);
        // 更新のときは直前に選んでいたファイル、初回は入口のファイルを選んでおく（無ければ先頭）。
        let keep = previous.filter(|p| self.archives.contains(p)).unwrap_or(entry);
        if let Some(pos) = self.archives.iter().position(|p| *p == keep) {
            self.selected_archive_index = Some(pos);
        }
    }

    /// 類似ファイル一覧であることを示す告知バー（中央ペインの最上部。カードとリストで共通）。
    pub(super) fn draw_similar_banner(&self, ui: &mut egui::Ui) {
        if self.viewing_similar.is_none() {
            return;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BANNER_H), egui::Sense::hover());
        let look_alike = self.is_look_alike_view();
        ui.painter().rect_filled(rect, 0.0, if look_alike { LOOK_ALIKE_BANNER_COLOR } else { BANNER_COLOR });
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if look_alike { i18n::t().sim_banner() } else { i18n::t().similar_banner() },
            egui::FontId::proportional(14.0),
            egui::Color32::WHITE,
        );
    }

    /// 一覧を作り直す（外部でファイルを消した・書き換えた後）。消えたファイルは外れ、結果の古い組は捨てられる。
    pub(super) fn refresh_similar_view(&mut self) {
        if self.is_look_alike_view() {
            self.rebuild_look_alike();
            return;
        }
        let Some(db) = self.spread_db.clone() else { return };
        let Some(view) = self.viewing_similar.as_mut() else { return };
        self.similar_token += 1;
        view.token = self.similar_token;
        let (entry_id, token) = (view.entry_id, view.token);
        self.marker_probe.submit_group(db, entry_id, token);
    }

    /// この組の再取得マーカー（一覧のファイルが持つもの）を全て正常として消し、一覧を閉じて元のフォルダへ戻る。
    pub(super) fn dismiss_similar_markers(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let Some(view) = &self.viewing_similar else { return };
        let dismissed = view.files.iter().filter(|f| crate::duplicate_marker::dismiss(&db, f.id)).count();
        let t = i18n::t();
        if dismissed == 0 {
            self.set_toast(t.similar_nothing_to_dismiss_toast());
            return;
        }
        self.set_toast(t.similar_dismissed_toast(dismissed));
        self.exit_similar_view();
    }

    /// ウィンドウにフォーカスが戻ったとき、一覧のファイルがまだ在るかを確認に出す（結果は `marker_presence`。
    /// 消えていた行は薄く表示する。一覧からの除外は「更新」）。
    pub(super) fn poll_similar_focus(&mut self) {
        let focused = self.egui_ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let regained = focused && !self.similar_was_focused;
        self.similar_was_focused = focused;
        if !regained {
            return;
        }
        let (Some(db), Some(view)) = (self.spread_db.clone(), &self.viewing_similar) else { return };
        let ids: Vec<u64> = view.files.iter().map(|f| f.id).collect();
        self.marker_probe.submit(db, ids);
    }

    /// 確認の結果、消えていると分かったファイルか（行・カードを薄く表示する）。
    pub(super) fn is_similar_gone(&self, path: &std::path::Path) -> bool {
        let Some(view) = &self.viewing_similar else { return false };
        view.files
            .iter()
            .find(|f| f.path == path)
            .is_some_and(|f| self.marker_presence.get(&f.id) == Some(&crate::file_identity::Presence::Missing))
    }

    /// 完全比較の基準のファイル（既定は最古のID）。比べる相手が無ければ None。
    fn compare_base(&self) -> Option<&SimilarFile> {
        let view = self.viewing_similar.as_ref().filter(|v| !v.look_alike)?;
        base_of(&view.files, view.base_id)
    }

    /// そのファイルが基準か。
    pub(super) fn is_similar_base(&self, path: &std::path::Path) -> bool {
        self.compare_base().is_some_and(|b| b.path == path)
    }

    /// 基準を付け替える。実行中・順番待ちの比較は全て取り消す（結果は「その基準との組」として残る）。
    pub(super) fn set_similar_base(&mut self, id: u64) {
        let Some(view) = &self.viewing_similar else { return };
        let current = base_of(&view.files, view.base_id).map(|f| f.id);
        if current == Some(id) || !view.files.iter().any(|f| f.id == id) {
            return;
        }
        self.cancel_all_marker_compares();
        if let Some(view) = self.viewing_similar.as_mut() {
            view.base_id = Some(id);
        }
    }

    /// 基準の目印（ラジオ）を描き、押されたら基準を付け替える。ホバーで説明を出す。
    /// 矩形は `marker_icon_hits` に登録する（類似ビューではマーカーのアイコンを出さないので、同じ仕組みで押下を振り分ける）。
    pub(super) fn paint_similar_base_mark(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: egui::Rect,
        real_idx: usize,
        path: &std::path::Path,
    ) {
        if self.compare_base().is_none() {
            self.marker_icon_hits.remove(&real_idx);
            return;
        }
        let is_base = self.is_similar_base(path);
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        let ring = egui::Stroke::new(1.5, if is_base { BANNER_COLOR } else { ui.visuals().weak_text_color() });
        painter.circle_stroke(rect.center(), 6.5, ring);
        if is_base {
            painter.circle_filled(rect.center(), 3.5, BANNER_COLOR);
        }
        self.marker_icon_hits.insert(real_idx, rect);
        let on_mark = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p));
        if on_mark {
            let t = i18n::t();
            response.clone().on_hover_text(if is_base { t.similar_base_tip_current() } else { t.similar_base_tip_set() });
        }
    }

    /// 基準の目印の押下なら、基準を付け替えて true（選択や起動はしない。ダブルクリックの2回目も握りつぶす）。
    pub(super) fn try_click_similar_base(&mut self, response: &egui::Response, real_idx: usize) -> bool {
        if !(response.clicked() || response.double_clicked()) {
            return false;
        }
        let Some(rect) = self.marker_icon_hits.get(&real_idx).copied() else { return false };
        let on_mark = response.ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p));
        if !on_mark {
            return false;
        }
        if response.clicked()
            && let Some(path) = self.archives.get(real_idx)
            && let Some(id) = self.viewing_similar.as_ref().and_then(|v| v.files.iter().find(|f| f.path == *path)).map(|f| f.id)
        {
            self.set_similar_base(id);
        }
        true
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
        let others: Vec<SimilarFile> = view.files.iter().filter(|f| f.id != base.id).cloned().collect();
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
        view.files.iter().filter(|f| f.id != base.id).all(|f| {
            matches!(
                self.marker_compare_states.get(&(base.id, f.id)),
                Some(CompareState::Done(CompareOutcome::Identical | CompareOutcome::Different))
            )
        })
    }

    /// そのファイルの完全比較の結果バッジ（文言と色）。基準の行は、比較の前から「基準」。
    pub(super) fn compare_badge(&self, path: &std::path::Path) -> Option<(String, egui::Color32)> {
        if self.is_look_alike_view() {
            // 似ている画像ビュー: 入口との類似度。入口の行は「基準」。
            let view = self.viewing_similar.as_ref()?;
            let id = view.files.iter().find(|f| f.path == path)?.id;
            let color = egui::Color32::from_rgb(140, 95, 215);
            // 入口の行は、計算の前から「基準」と出す（他の行の類似度の比べ先を示す）。
            if id == view.entry_id {
                return Some((i18n::t().similar_badge_base().to_owned(), color));
            }
            let pct = *view.pcts.get(&id)?;
            return Some((format!("{pct}%"), color));
        }
        let base = self.compare_base()?;
        let view = self.viewing_similar.as_ref()?;
        let id = view.files.iter().find(|f| f.path == path)?.id;
        let t = i18n::t();
        if id == base.id {
            // 基準の行は、比較を始める前から「基準」と出す。基準を付け替えると、この文字も追従する。
            return Some((t.similar_badge_base().to_owned(), egui::Color32::from_rgb(120, 130, 150)));
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
        if view.look_alike {
            let others = self.archives.len().saturating_sub(1);
            ui.label(if others == 0 { t.sim_view_empty().to_owned() } else { t.sim_view_header(others) });
            ui.separator();
            if ui.button(t.similar_refresh()).clicked() {
                self.rebuild_look_alike();
            }
            if ui.add_enabled(others > 0, egui::Button::new(t.similar_dismiss_all())).clicked() {
                self.dismiss_look_alike_pairs();
            }
            return;
        }
        if view.loading {
            ui.label(t.similar_loading());
            return;
        }
        let unlisted = view.unlisted.len();
        let header = if self.archives.len() < 2 {
            t.similar_empty().to_owned()
        } else {
            t.similar_header(self.archives.len())
        };
        ui.label(header);
        if unlisted > 0 {
            let tip: String = view
                .unlisted
                .iter()
                .map(|(path, presence)| {
                    let state = match presence {
                        Presence::Offline => t.similar_unlisted_offline(),
                        _ => t.similar_unlisted_missing(),
                    };
                    format!("{}  {state}", path.display())
                })
                .collect::<Vec<_>>()
                .join("\n");
            ui.label(egui::RichText::new(t.similar_unlisted(unlisted)).weak()).on_hover_text(tip);
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
        ui.separator();
        if ui.button(t.similar_refresh()).clicked() {
            self.refresh_similar_view();
        }
        let can_dismiss = self.compare_base().is_some();
        if ui.add_enabled(can_dismiss, egui::Button::new(t.similar_dismiss_all())).clicked() {
            self.dismiss_similar_markers();
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

    fn file(id: u64) -> SimilarFile {
        SimilarFile { id, path: PathBuf::from(format!("/x/{id}")), stat: None }
    }

    #[test]
    fn base_defaults_to_the_oldest_id() {
        let files = [file(3), file(5), file(9)];
        assert_eq!(base_of(&files, None).map(|f| f.id), Some(3));
    }

    #[test]
    fn chosen_base_wins_and_a_missing_one_falls_back() {
        let files = [file(3), file(5), file(9)];
        assert_eq!(base_of(&files, Some(9)).map(|f| f.id), Some(9));
        assert_eq!(base_of(&files, Some(77)).map(|f| f.id), Some(3), "一覧に無い基準は最古のIDに戻る");
    }

    #[test]
    fn no_base_without_a_partner_to_compare() {
        assert!(base_of(&[file(3)], None).is_none());
        assert!(base_of(&[], Some(3)).is_none());
    }
}
