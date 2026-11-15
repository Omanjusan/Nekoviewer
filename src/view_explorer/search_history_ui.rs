//! 検索条件の履歴（保存・復元・上限）の配線。保存の形式と読み書きは [search_history_store.rs](crate::search_history_store)。
//!
//! - 検索を始めた時点で、条件だけを保存する（別スレッド。結果は保存しない）。
//! - 起動時に、保存された条件を「結果を持たない行」（`restored`）として履歴に並べる。
//!   その行を選ぶと、条件をフォームへ復元して再走査する。再走査した行は、元の行を置き換えて先頭へ移る。
//! - セッション内の行は結果を保持して、選ぶとすぐ表示する。保持が多すぎるときだけ、古い行から結果を手放して
//!   「結果を持たない行」に戻す（選ぶと再走査）。
//! - 履歴は、新モード・旧モードそれぞれ50件まで（ファイルの上限と同じ）。古い行から消える。
//! - 条件が空の検索は、新旧とも開始できない（`search_condition_is_valid`）。

use std::path::{Path, PathBuf};

use crate::search_history_store::{self as store, Condition, Entry, Kind};

use super::{NekoviewApp, SearchFormState, SearchMode, SearchResultEntry};

/// セッション内で結果を保持するヒット件数の合計の上限。超えたら、古い行から結果を手放す。
pub(super) const SESSION_HIT_BUDGET: usize = 500_000;

/// 検索を始められる条件が入っているか。文字列・サイズ・日付のどれか1つは、有効な値で入っている必要がある
/// （空や、数値・日付として読めない入力だけでは「無条件」と同じなので、全件が当たってしまう）。
pub(super) fn search_condition_is_valid(form: &SearchFormState) -> bool {
    let size_ok = |s: &str| crate::ledger_search::parse_size_mb(s).is_some();
    match form.mode {
        SearchMode::New => {
            !crate::ledger_search::parse_tokens(&form.new_text).is_empty()
                || size_ok(&form.new_size_min_mb)
                || size_ok(&form.new_size_max_mb)
        }
        SearchMode::Legacy => {
            !form.name_pattern.trim().is_empty()
                || size_ok(&form.size_min_mb)
                || size_ok(&form.size_max_mb)
                || super::search::parse_date(&form.date_after).is_some()
                || super::search::parse_date(&form.date_before).is_some()
        }
    }
}

/// フォームの入力を、保存する条件にする（モードに合う欄だけ）。
pub(super) fn to_condition(form: &SearchFormState) -> Condition {
    match form.mode {
        SearchMode::New => Condition::New {
            text: form.new_text.clone(),
            size_min: form.new_size_min_mb.clone(),
            size_max: form.new_size_max_mb.clone(),
        },
        SearchMode::Legacy => Condition::Legacy {
            base_dir: form.base_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
            name: form.name_pattern.clone(),
            subdirs: form.include_subdirs,
            size_min: form.size_min_mb.clone(),
            size_max: form.size_max_mb.clone(),
            date_after: form.date_after.clone(),
            date_before: form.date_before.clone(),
        },
    }
}

/// 保存された条件から、フォームの入力を作る（もう一方のモードの欄は空）。
pub(super) fn from_condition(cond: &Condition) -> SearchFormState {
    match cond {
        Condition::New { text, size_min, size_max } => SearchFormState {
            mode: SearchMode::New,
            new_text: text.clone(),
            new_size_min_mb: size_min.clone(),
            new_size_max_mb: size_max.clone(),
            ..Default::default()
        },
        Condition::Legacy { base_dir, name, subdirs, size_min, size_max, date_after, date_before } => SearchFormState {
            mode: SearchMode::Legacy,
            base_dir: base_dir.as_ref().map(PathBuf::from),
            name_pattern: name.clone(),
            include_subdirs: *subdirs,
            size_min_mb: size_min.clone(),
            size_max_mb: size_max.clone(),
            date_after: date_after.clone(),
            date_before: date_before.clone(),
            ..Default::default()
        },
    }
}

/// 結果を持たない行のラベル（条件の名前に、再走査の印「↻」を添える）。
pub(super) fn restored_label(form: &SearchFormState) -> String {
    let range = |a: &str, b: &str, unit: &str| format!("{}–{}{}", a.trim(), b.trim(), unit);
    let name = match form.mode {
        SearchMode::New => {
            if form.new_text.trim().is_empty() {
                format!("[{}]", range(&form.new_size_min_mb, &form.new_size_max_mb, " MB"))
            } else {
                form.new_text.trim().to_string()
            }
        }
        SearchMode::Legacy => {
            if !form.name_pattern.trim().is_empty() {
                form.name_pattern.trim().to_string()
            } else {
                let mut parts = Vec::new();
                if !(form.size_min_mb.trim().is_empty() && form.size_max_mb.trim().is_empty()) {
                    parts.push(range(&form.size_min_mb, &form.size_max_mb, " MB"));
                }
                if !(form.date_after.trim().is_empty() && form.date_before.trim().is_empty()) {
                    parts.push(range(&form.date_after, &form.date_before, ""));
                }
                format!("[{}]", parts.join(" "))
            }
        }
    };
    format!("{name} ↻")
}

/// 保存された1行から、結果を持たない履歴の行を作る。
fn restored_entry(entry: &Entry) -> SearchResultEntry {
    let form = from_condition(&entry.cond);
    SearchResultEntry { label: restored_label(&form), form, restored: true, saved_at: Some(entry.saved_at), ..Default::default() }
}

/// 起動時: 保存された条件（新・旧）を、新しい順に並べた履歴の行にする。
pub(super) fn load_restored_history(dir: &Path) -> Vec<SearchResultEntry> {
    let mut all: Vec<(i64, u8, SearchResultEntry)> = Vec::new();
    for (rank, kind) in [(0u8, Kind::New), (1u8, Kind::Legacy)] {
        for e in store::load(dir, kind) {
            all.push((e.saved_at, rank, restored_entry(&e)));
        }
    }
    // 新しい順。同じ秒なら新モードを上に（並びを一定にするだけで、意味は無い）。ファイル内の並びは保つ（安定ソート）。
    all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    all.into_iter().map(|(_, _, e)| e).collect()
}

/// 履歴を、モードごとに `store::MAX_ENTRIES` 件までにする（先頭が新しい。古い行から消す）。実行中の行は消さない。
pub(super) fn trim_history_per_mode(history: &mut Vec<SearchResultEntry>) {
    let (mut new_n, mut legacy_n) = (0usize, 0usize);
    history.retain(|e| {
        if e.running {
            return true;
        }
        let n = match e.form.mode {
            SearchMode::New => &mut new_n,
            SearchMode::Legacy => &mut legacy_n,
        };
        *n += 1;
        *n <= store::MAX_ENTRIES
    });
}

/// 保持しているヒットの合計が `budget` を超えたら、古い行から結果を手放して「結果を持たない行」に戻す。
/// 実行中の行と、いま表示している行（`keep`）は手放さない。
pub(super) fn evict_old_hits(history: &mut [SearchResultEntry], budget: usize, keep: Option<usize>) {
    let mut total: usize = history.iter().filter(|e| !e.running).map(|e| e.hits.len()).sum();
    for idx in (0..history.len()).rev() {
        if total <= budget {
            break;
        }
        let e = &mut history[idx];
        if e.running || Some(idx) == keep || e.hits.is_empty() {
            continue;
        }
        total -= e.hits.len();
        e.hits = Vec::new();
        e.restored = true;
        e.progress = None;
        e.cancelled = false;
        e.unconfirmed = 0;
        e.label = restored_label(&e.form);
    }
}

impl NekoviewApp {
    /// 検索を始めた時点の条件を保存する（別スレッド）。`replace` は、保存済みの行を選んで再実行したときの
    /// 元の行の時刻（同じ行を重複させず、先頭へ移す）。積んだ行の時刻を返す。
    pub(super) fn save_search_condition(&self, form: &SearchFormState, replace: Option<i64>) -> i64 {
        let saved_at = crate::file_identity::now_unix();
        let entry = Entry { saved_at, cond: to_condition(form) };
        let dir = self.config.config_root.clone();
        std::thread::spawn(move || {
            let _ = store::push(&dir, entry, replace);
        });
        saved_at
    }

    /// 履歴の idx 番目の行を取り除く。表示中・選択中の位置は、ずれた分を直す。
    pub(super) fn remove_history_row(&mut self, idx: usize) {
        if idx >= self.search_history.len() {
            return;
        }
        self.search_history.remove(idx);
        let shift = |p: Option<usize>| match p {
            Some(v) if v == idx => None,
            Some(v) if v > idx => Some(v - 1),
            other => other,
        };
        self.viewing_search = shift(self.viewing_search);
        self.search_selected = shift(self.search_selected);
    }

    /// 保存済みの行（`saved_at` が一致する行）を取り除く（再実行で、同じ行を置き換えるため）。
    pub(super) fn remove_stored_row(&mut self, saved_at: i64) {
        if let Some(idx) = self.search_history.iter().position(|e| e.saved_at == Some(saved_at) && !e.running) {
            self.remove_history_row(idx);
        }
    }

    /// 履歴を上限内に保ち、保持するヒットが多すぎれば古い行から手放す（履歴を積んだあとに呼ぶ）。
    pub(super) fn enforce_search_history_limits(&mut self) {
        trim_history_per_mode(&mut self.search_history);
        evict_old_hits(&mut self.search_history, SESSION_HIT_BUDGET, self.viewing_search);
    }

    /// 結果を持たない行を選んだとき: 条件をフォームへ復元して、再走査する（元の行は、先頭の新しい行に置き換わる）。
    /// 実行中は何もしない。条件が空（古い保存など）なら、復元だけして走査はしない。
    pub(super) fn rerun_stored_search(&mut self, idx: usize) {
        let Some(entry) = self.search_history.get(idx) else { return };
        let (form, saved_at) = (entry.form.clone(), entry.saved_at);
        if self.any_search_running() {
            return;
        }
        self.search_form = form.clone();
        self.search_form_focus = form.mode.first_field();
        if !search_condition_is_valid(&form) {
            return;
        }
        match form.mode {
            SearchMode::New => self.start_new_search(saved_at),
            SearchMode::Legacy => {
                let ctx = self.egui_ctx.clone();
                self.start_search(&ctx, saved_at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_form(text: &str, min: &str, max: &str) -> SearchFormState {
        SearchFormState {
            mode: SearchMode::New,
            new_text: text.to_string(),
            new_size_min_mb: min.to_string(),
            new_size_max_mb: max.to_string(),
            ..Default::default()
        }
    }

    fn legacy_form() -> SearchFormState {
        SearchFormState {
            mode: SearchMode::Legacy,
            base_dir: Some(PathBuf::from("/data/x")),
            include_subdirs: true,
            ..Default::default()
        }
    }

    #[test]
    fn new_mode_needs_a_word_or_a_valid_size() {
        assert!(!search_condition_is_valid(&new_form("", "", "")), "空は禁止");
        assert!(!search_condition_is_valid(&new_form("   \u{3000} ", "", "")), "空白だけも空");
        assert!(!search_condition_is_valid(&new_form("\"\"", "", "")), "空の引用符も空");
        assert!(!search_condition_is_valid(&new_form("", "abc", "-1")), "数値として読めないサイズは無条件と同じ");
        assert!(search_condition_is_valid(&new_form("alpha", "", "")));
        assert!(search_condition_is_valid(&new_form("", "1", "")));
        assert!(search_condition_is_valid(&new_form("", "", "0.5")));
    }

    #[test]
    fn legacy_mode_needs_a_name_size_or_date_and_a_base_dir_alone_is_not_enough() {
        let mut f = legacy_form();
        assert!(!search_condition_is_valid(&f), "基点フォルダ（とサブディレクトリ）だけでは、全件の列挙になる");
        f.name_pattern = "  ".to_string();
        assert!(!search_condition_is_valid(&f));
        f.name_pattern = "a*.zip".to_string();
        assert!(search_condition_is_valid(&f));
        let mut f = legacy_form();
        f.size_max_mb = "5".to_string();
        assert!(search_condition_is_valid(&f));
        let mut f = legacy_form();
        f.date_after = "2025-01-02".to_string();
        assert!(search_condition_is_valid(&f));
        let mut f = legacy_form();
        f.date_before = "not-a-date".to_string();
        assert!(!search_condition_is_valid(&f), "日付として読めない入力だけでは無条件と同じ");
    }

    #[test]
    fn condition_round_trips_through_the_form_for_both_modes() {
        let f = new_form("a \"b c\"", "1", "9");
        assert_eq!(from_condition(&to_condition(&f)), f);
        let mut g = legacy_form();
        g.name_pattern = "x*".to_string();
        g.size_min_mb = "2".to_string();
        g.date_after = "2025-01-02".to_string();
        assert_eq!(from_condition(&to_condition(&g)), g);
        // 入力されていないモードの欄は、保存にも復元にも出てこない
        let mut mixed = new_form("a", "", "");
        mixed.name_pattern = "legacy-only".to_string();
        assert_eq!(from_condition(&to_condition(&mixed)).name_pattern, "");
    }

    #[test]
    fn restored_labels_name_the_condition_and_mark_it_for_rescan() {
        assert_eq!(restored_label(&new_form("alpha", "", "")), "alpha ↻");
        assert_eq!(restored_label(&new_form("", "1", "5")), "[1–5 MB] ↻");
        let mut g = legacy_form();
        g.name_pattern = "x*".to_string();
        assert_eq!(restored_label(&g), "x* ↻");
        g.name_pattern.clear();
        g.date_after = "2025-01-02".to_string();
        assert_eq!(restored_label(&g), "[2025-01-02–] ↻");
    }

    fn entry_at(mode: SearchMode, name: &str, hits: usize) -> SearchResultEntry {
        let form = if mode == SearchMode::New { new_form(name, "", "") } else { SearchFormState { mode, name_pattern: name.to_string(), ..Default::default() } };
        SearchResultEntry { label: name.to_string(), hits: (0..hits).map(|i| PathBuf::from(format!("/f/{name}{i}"))).collect(), form, ..Default::default() }
    }

    #[test]
    fn history_is_limited_per_mode_dropping_the_oldest_first() {
        let mut history: Vec<SearchResultEntry> = Vec::new();
        // 先頭が新しい。新モード60件・旧モード3件を混ぜる
        for i in 0..60 {
            history.push(entry_at(SearchMode::New, &format!("n{i:02}"), 0));
            if i % 20 == 0 {
                history.push(entry_at(SearchMode::Legacy, &format!("l{i}"), 0));
            }
        }
        trim_history_per_mode(&mut history);
        let news: Vec<&str> = history.iter().filter(|e| e.form.mode == SearchMode::New).map(|e| e.label.as_str()).collect();
        assert_eq!(news.len(), store::MAX_ENTRIES);
        assert_eq!(news[0], "n00", "先頭（新しい側）は残る");
        assert_eq!(*news.last().unwrap(), "n49", "後ろ（古い側）の10件が消える");
        assert_eq!(history.iter().filter(|e| e.form.mode == SearchMode::Legacy).count(), 3, "旧モードは新モードの上限に巻き込まれない");
    }

    #[test]
    fn a_running_row_is_never_trimmed() {
        let mut history: Vec<SearchResultEntry> = (0..55).map(|i| entry_at(SearchMode::New, &format!("n{i}"), 0)).collect();
        history[54].running = true;
        trim_history_per_mode(&mut history);
        assert!(history.last().unwrap().running);
    }

    #[test]
    fn old_hits_are_released_first_and_the_row_returns_to_a_rescan_row() {
        let mut history = vec![
            entry_at(SearchMode::New, "newest", 400),
            entry_at(SearchMode::New, "middle", 300),
            entry_at(SearchMode::New, "oldest", 200),
        ];
        evict_old_hits(&mut history, 700, None);
        assert_eq!(history[0].hits.len(), 400);
        assert_eq!(history[1].hits.len(), 300);
        assert!(history[2].hits.is_empty() && history[2].restored, "古い行から手放す");
        assert_eq!(history[2].label, "oldest ↻");
        // まだ超えていれば、次に古い行も手放す
        evict_old_hits(&mut history, 400, None);
        assert!(history[1].restored && !history[0].restored);
    }

    #[test]
    fn the_viewed_row_and_the_running_row_keep_their_hits() {
        let mut history = vec![entry_at(SearchMode::New, "a", 300), entry_at(SearchMode::New, "b", 300), entry_at(SearchMode::New, "c", 300)];
        history[0].running = true;
        evict_old_hits(&mut history, 100, Some(2));
        assert_eq!(history[2].hits.len(), 300, "表示中の行は手放さない");
        assert!(history[1].restored, "それ以外の古い行は手放す");
        assert_eq!(history[0].hits.len(), 300, "実行中の行は対象外（件数の合計にも入れない）");
    }

    #[test]
    fn restoring_orders_both_modes_newest_first() {
        let dir = std::env::temp_dir().join(format!("nekoviewer_search_history_ui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let n = |t: i64, text: &str| Entry { saved_at: t, cond: Condition::New { text: text.into(), size_min: String::new(), size_max: String::new() } };
        let l = |t: i64, name: &str| Entry {
            saved_at: t,
            cond: Condition::Legacy { base_dir: None, name: name.into(), subdirs: false, size_min: String::new(), size_max: String::new(), date_after: String::new(), date_before: String::new() },
        };
        store::push(&dir, n(10, "n10"), None).unwrap();
        store::push(&dir, l(20, "l20"), None).unwrap();
        store::push(&dir, n(30, "n30"), None).unwrap();
        let rows = load_restored_history(&dir);
        assert_eq!(rows.iter().map(|r| r.saved_at.unwrap()).collect::<Vec<_>>(), vec![30, 20, 10]);
        assert!(rows.iter().all(|r| r.restored && r.hits.is_empty()));
        assert_eq!(rows[1].form.mode, SearchMode::Legacy);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
