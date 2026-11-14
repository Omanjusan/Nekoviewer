//! 検索タブ「新」モードの実行配線。検索ジョブ（`ledger_search::LedgerSearchJob`）はアプリ本体が持ち、
//! 毎フレームの `poll_search` から受け取る。表示しているタブには依存しないので、他のタブにいても
//! 走査と収集は続く。
//!
//! - 結果は履歴の「実行中エントリ」（`SearchResultEntry::running`）の `hits` へ溜める。中央の一覧
//!   （`archives`）へは、検索タブでその行を表示している間だけ追記する（LAZY: サムネは表示中のセルだけが要求する）。
//!   別のタブにいる間は `archives` に一切触らない（そのタブの表示を壊さない）。
//! - 実行中は同時に1つだけ。開始ボタンは「中断」に変わる。
//! - 完了・中断の通知は、履歴行のラベル（進捗 n/N・中断・確認不可の件数）で行う。

use std::path::PathBuf;

use crate::i18n;
use crate::ledger_search::{tier_names_of, LedgerQuery, LedgerSearchJob};

use super::{push_search_result, FolderPaneTab, NekoviewApp, SearchFormState, SearchMode, SearchResultEntry};

/// 新モードの履歴行ラベル。検索文字列（空でサイズだけの指定ならサイズ範囲）と件数に、
/// 実行中は進捗「n/N」、中断、確認できずに外した件数を添える。
pub(super) fn new_search_label(
    form: &SearchFormState,
    count: usize,
    running: bool,
    progress: Option<(usize, usize)>,
    cancelled: bool,
    unconfirmed: usize,
) -> String {
    let text = form.new_text.trim();
    let (min, max) = (form.new_size_min_mb.trim(), form.new_size_max_mb.trim());
    let name = if text.is_empty() && !(min.is_empty() && max.is_empty()) {
        format!("[{min}–{max} MB]")
    } else {
        text.to_string()
    };
    let mut s = i18n::t().search_result_label(&name, count);
    if running {
        match progress {
            Some((scanned, total)) => s.push_str(&format!(" {scanned}/{total}")),
            None => s.push_str(" …"),
        }
    }
    if cancelled {
        s.push_str(i18n::t().search_cancelled_suffix());
    }
    if unconfirmed > 0 {
        s.push_str(&i18n::t().search_unconfirmed_suffix(unconfirmed));
    }
    s
}

impl SearchResultEntry {
    fn refresh_new_label(&mut self) {
        if self.form.mode == SearchMode::New {
            self.label = new_search_label(&self.form, self.hits.len(), self.running, self.progress, self.cancelled, self.unconfirmed);
        }
    }

    /// 実行中エントリへ、ジョブの途中経過を取り込む（ヒットの追記・進捗・外した件数）。
    pub(super) fn absorb_progress(&mut self, new_hits: &[PathBuf], progress: Option<(usize, usize)>, unconfirmed: usize) {
        self.hits.extend_from_slice(new_hits);
        self.progress = progress;
        self.unconfirmed = unconfirmed;
        self.refresh_new_label();
    }

    /// 実行を終える（完了なら `cancelled=false`、中断なら true）。
    pub(super) fn finish_run(&mut self, cancelled: bool) {
        self.running = false;
        self.cancelled = cancelled;
        self.refresh_new_label();
    }
}

impl NekoviewApp {
    /// 旧・新どちらかの検索が実行中か（同時に走らせるのは1つだけ）。
    pub(super) fn any_search_running(&self) -> bool {
        self.search_running || self.search_job.is_some()
    }

    /// 新モードの検索を始める。履歴の先頭に実行中エントリを置いて選択し、空の一覧から表示を始める
    /// （ヒットは `poll_new_search` が逐次追記する）。実行中・台帳なしなら何もしない。
    pub(super) fn start_new_search(&mut self) {
        if self.any_search_running() {
            return;
        }
        let Some(db) = self.spread_db.clone() else { return };
        let form = self.search_form.clone();
        let tier_names = tier_names_of(&self.tag_manager_categories);
        let query = LedgerQuery::from_inputs(&form.new_text, &form.new_size_min_mb, &form.new_size_max_mb, &tier_names);
        let ctx = self.egui_ctx.clone();
        self.search_job = Some(LedgerSearchJob::start(db, query, move || ctx.request_repaint()));
        let mut entry = SearchResultEntry { form, running: true, ..Default::default() };
        entry.refresh_new_label();
        push_search_result(&mut self.search_history, entry);
        self.enter_search_view(0);
    }

    /// 実行中の新モード検索を中断する。それまでに集まった結果は履歴に残る。
    pub(super) fn cancel_new_search(&mut self) {
        // 中断の直前までに届いている分を取り込んでから止める。
        self.poll_new_search();
        let Some(mut job) = self.search_job.take() else { return };
        job.cancel();
        self.finish_new_search_entry(true);
    }

    /// 毎フレーム: ジョブの結果を履歴の実行中エントリへ取り込む。表示中なら一覧にも追記する。
    pub(super) fn poll_new_search(&mut self) {
        let Some(job) = self.search_job.as_mut() else { return };
        let polled = job.poll();
        let new_paths: Vec<PathBuf> = job.hits()[polled.new_hits.clone()].to_vec();
        let (progress, unconfirmed) = (job.progress(), job.unconfirmed());
        let Some(idx) = self.search_history.iter().position(|e| e.running) else {
            // 受け皿のエントリが無い（想定外）。ジョブだけ残らないよう捨てる。
            self.search_job = None;
            return;
        };
        self.search_history[idx].absorb_progress(&new_paths, progress, unconfirmed);
        if self.viewing_new_search(idx) && !new_paths.is_empty() {
            self.append_search_view_hits(new_paths);
        }
        if polled.finished {
            self.search_job = None;
            self.finish_new_search_entry(false);
        }
    }

    /// 実行中エントリを完了／中断にする。表示中なら、一覧を並べ直す（途中は到着順）。
    fn finish_new_search_entry(&mut self, cancelled: bool) {
        let Some(idx) = self.search_history.iter().position(|e| e.running) else { return };
        self.search_history[idx].finish_run(cancelled);
        if self.viewing_new_search(idx) {
            self.sort_archives();
            self.recompute_filter();
        }
    }

    /// 検索タブで、履歴の idx 番目を表示しているか（ここが true の間だけ、一覧へ追記する）。
    fn viewing_new_search(&self, idx: usize) -> bool {
        self.folder_pane_tab == FolderPaneTab::Search && self.viewing_search == Some(idx)
    }

    /// 表示中の検索結果の一覧へヒットを追記する（タグ検索の追記と同じ扱い）。
    fn append_search_view_hits(&mut self, paths: Vec<PathBuf>) {
        if let Some(db) = self.spread_db.clone() {
            self.cross_view_favorite_markers.extend(crate::favorites::memberships_for_paths(&db, &paths));
            self.saved_archive_settings.extend(crate::spread_state::saved_settings_for_paths(&db, &paths));
        }
        self.raw_image_files
            .extend(paths.iter().filter(|p| crate::fs::archive::is_supported_image_file(p)).cloned());
        self.archives.extend(paths);
        self.recompute_filter();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(text: &str, min: &str, max: &str) -> SearchFormState {
        SearchFormState {
            mode: SearchMode::New,
            new_text: text.to_string(),
            new_size_min_mb: min.to_string(),
            new_size_max_mb: max.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn running_label_shows_count_and_progress() {
        let l = new_search_label(&form("kouji", "", ""), 12, true, Some((345, 1000)), false, 0);
        assert!(l.contains("kouji") && l.contains("12") && l.contains("345/1000"), "{l}");
        // まだ最初の進捗が届いていない
        let l = new_search_label(&form("kouji", "", ""), 0, true, None, false, 0);
        assert!(l.ends_with(" …"), "{l}");
    }

    #[test]
    fn finished_label_has_no_progress_and_notes_cancel_and_unverified() {
        let done = new_search_label(&form("a", "", ""), 3, false, Some((10, 10)), false, 0);
        assert!(!done.contains("10/10"), "完了後は進捗を出さない: {done}");
        let stopped = new_search_label(&form("a", "", ""), 3, false, Some((4, 10)), true, 0);
        assert!(stopped.contains(i18n::t().search_cancelled_suffix().trim()), "{stopped}");
        let unverified = new_search_label(&form("a", "", ""), 3, false, None, false, 2);
        assert!(unverified.contains(i18n::t().search_unconfirmed_suffix(2).trim()), "{unverified}");
    }

    #[test]
    fn size_only_search_is_named_by_its_range() {
        let l = new_search_label(&form("", "1", "5"), 2, false, None, false, 0);
        assert!(l.contains("[1–5 MB]"), "{l}");
        let l = new_search_label(&form("", "1", ""), 2, false, None, false, 0);
        assert!(l.contains("[1– MB]"), "{l}");
        // 文字列があればそちらを名前にする
        let l = new_search_label(&form("abc", "1", "5"), 2, false, None, false, 0);
        assert!(l.contains("abc") && !l.contains("MB"), "{l}");
    }

    #[test]
    fn entry_collects_hits_and_finishes_with_a_refreshed_label() {
        let mut e = SearchResultEntry { form: form("x", "", ""), running: true, ..Default::default() };
        e.absorb_progress(&[PathBuf::from("/a/1.zip"), PathBuf::from("/a/2.zip")], Some((50, 100)), 0);
        e.absorb_progress(&[PathBuf::from("/a/3.zip")], Some((90, 100)), 1);
        assert_eq!(e.hits.len(), 3);
        assert_eq!((e.progress, e.unconfirmed), (Some((90, 100)), 1));
        assert!(e.running && e.label.contains("90/100") && e.label.contains('3'));
        e.finish_run(false);
        assert!(!e.running && !e.cancelled);
        assert!(!e.label.contains("90/100"));
        assert_eq!(e.hits.len(), 3, "完了後もヒットは残る");

        let mut c = SearchResultEntry { form: form("x", "", ""), running: true, ..Default::default() };
        c.absorb_progress(&[PathBuf::from("/a/1.zip")], Some((5, 100)), 0);
        c.finish_run(true);
        assert!(!c.running && c.cancelled);
        assert_eq!(c.hits.len(), 1, "中断してもそれまでのヒットは残る");
    }

    #[test]
    fn legacy_entries_keep_their_stored_label() {
        let mut e = SearchResultEntry {
            label: "旧の行".to_string(),
            form: SearchFormState { mode: SearchMode::Legacy, ..Default::default() },
            ..Default::default()
        };
        e.finish_run(false);
        assert_eq!(e.label, "旧の行");
    }
}
