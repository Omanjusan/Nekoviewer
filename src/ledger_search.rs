//! 検索タブ「新」モードの走査エンジン。spread DB のID台帳を全件走査し、検索文字列とサイズ条件に
//! 合うファイルをバックグラウンドで拾って、一定件数ごとのチャンクで送る。
//!
//! - 検索文字列はスペース（半角・全角）区切りの語の並び。`"..."` で囲むと、中のスペースを含む1語になる。
//!   語は **どれか1つに当たればヒット（OR）**。当たる先は、ファイル名か、ファイルに付いているタグ名。
//!   - ファイル名: `fs::dir::name_matches`（小文字の部分一致。`*?[` を含む語は glob）。
//!   - タグ名: 全カテゴリの全要素名。別名（`A|B|C`）のどれかに、語が小文字で含まれれば当たる。
//!   - タグ名との照合は走査の前に済ませておく（語ごとに「当たるtier_idの集合」を作る）ので、
//!     走査中はファイルのタグ（tier_id）が集合に入っているかを見るだけ。
//! - 文字列が空なら文字列の条件は無し（全ファイルが通る）。サイズ条件とは AND。
//!   条件がすべて空なら全件がヒットする（UI側で防ぐかどうかは呼び出し側が決める）。
//! - ファイルの実在確認（stat）はここではしない。パスは台帳の現在のパスキーのまま返す
//!   （実在・オフラインの判定と表示用の解決は、呼び出し側が表示領域の分だけ行う＝LAZY）。
//!   前回の確認でファイルが見つからなかったレコードは `confirmed: false` で返す。
//! - 総数（台帳の件数）は走査の前に取る。進捗は「調べた件数 / 総数」。
//! - 打ち切りは `LedgerScan` を捨てる（Drop）か `cancel` で行う。
//! - 日付条件は今は無い。`LedgerQuery::matches` に足せる形にしてある。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{FileRecord, FileStatus};

/// 1チャンクに詰める最大件数。
pub const CHUNK_SIZE: usize = 200;
/// ヒットが無くても、これだけ調べるごとに進捗を送る。
pub const PROGRESS_STEP: usize = 2000;

/// 検索文字列を語に分ける。半角・全角スペースで区切り、`"`（と全角の `“` `”`）で囲んだ中は
/// 区切らずに1語にする。閉じていない引用符は、末尾までを1語にする。空の語と重複は捨てる（順序は入力順）。
pub fn parse_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let push = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.trim().to_string();
        cur.clear();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    };
    for ch in text.chars() {
        match ch {
            '"' | '\u{201C}' | '\u{201D}' => {
                // 開く側も閉じる側も、ここまでの語を確定させる（引用符は語の区切りにもなる）。
                push(&mut cur, &mut out);
                quoted = !quoted;
            }
            ' ' | '\u{3000}' if !quoted => push(&mut cur, &mut out),
            _ => cur.push(ch),
        }
    }
    push(&mut cur, &mut out);
    out
}

/// 1つの語。ファイル名との照合用の文字列と、当たるタグ（tier_id）の集合。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    text: String,
    tag_ids: HashSet<u64>,
}

/// 検索条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerQuery {
    tokens: Vec<Token>,
    size_min: Option<u64>,
    size_max: Option<u64>,
    /// いずれかの語に当たるタグがある（無ければ走査中にタグを読まなくて済む）。
    any_tag_ids: bool,
}

impl LedgerQuery {
    /// `text`: 検索文字列（未パース）。`tier_names`: 全カテゴリの全要素（tier_id, 要素名）。
    /// 要素名は別名つき（`A|B|C`）のままでよい。
    pub fn new(text: &str, tier_names: &[(u64, String)], size_min: Option<u64>, size_max: Option<u64>) -> Self {
        let tokens: Vec<Token> = parse_tokens(text)
            .into_iter()
            .map(|text| {
                let needle = text.to_lowercase();
                let tag_ids = tier_names
                    .iter()
                    .filter(|(_, name)| {
                        crate::tag_master::split_aliases(name)
                            .iter()
                            .any(|alias| alias.to_lowercase().contains(&needle))
                    })
                    .map(|(id, _)| *id)
                    .collect();
                Token { text, tag_ids }
            })
            .collect();
        let any_tag_ids = tokens.iter().any(|t| !t.tag_ids.is_empty());
        Self { tokens, size_min, size_max, any_tag_ids }
    }

    /// 文字列もサイズも空（全件が通る）か。
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.size_min.is_none() && self.size_max.is_none()
    }

    /// 走査中にファイルのタグを読む必要があるか。
    fn needs_tags(&self) -> bool {
        self.any_tag_ids
    }

    fn size_ok(&self, size: u64) -> bool {
        self.size_min.is_none_or(|min| size >= min) && self.size_max.is_none_or(|max| size <= max)
    }

    /// ファイル名とサイズで決まる部分（タグを読む前に判定できる）。`Some(true)` ならヒット確定、
    /// `Some(false)` なら不一致確定、`None` なら、タグを見ないと決まらない。
    fn pre_match(&self, file_name: &str, size: u64) -> Option<bool> {
        if !self.size_ok(size) {
            return Some(false);
        }
        if self.tokens.is_empty() {
            return Some(true);
        }
        if self.tokens.iter().any(|t| crate::fs::dir::name_matches(&t.text, file_name)) {
            return Some(true);
        }
        if self.any_tag_ids {
            None
        } else {
            Some(false)
        }
    }

    /// タグ（tier_id）のどれかが、いずれかの語に当たるか。
    fn tags_match(&self, tags: &[u64]) -> bool {
        self.tokens.iter().any(|t| tags.iter().any(|id| t.tag_ids.contains(id)))
    }

    /// 1ファイルが条件を満たすか（単体テスト・呼び出し側の確認用。走査は `pre_match` と `tags_match` を使う）。
    pub fn matches(&self, file_name: &str, size: u64, tags: &[u64]) -> bool {
        match self.pre_match(file_name, size) {
            Some(r) => r,
            None => self.tags_match(tags),
        }
    }
}

/// 走査で見つかった1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerHit {
    pub id: u64,
    /// 台帳の現在のパス（実在は未確認）。
    pub path: PathBuf,
    /// 前回の確認でファイルが見つからなかったレコードなら false。
    pub confirmed: bool,
}

#[derive(Debug)]
pub enum LedgerScanMsg {
    Hits(Vec<LedgerHit>),
    /// 進捗（調べた件数 / 総数）。
    Progress { scanned: usize, total: usize },
    /// 走査完了（打ち切りでは送られない）。
    Done { scanned: usize, total: usize },
}

/// 実行中の走査。捨てると（Drop）走査スレッドは次のレコードで打ち切る。
pub struct LedgerScan {
    pub rx: mpsc::Receiver<LedgerScanMsg>,
    cancel: Arc<AtomicBool>,
}

impl LedgerScan {
    /// 打ち切る（Drop と同じ。受け取り側を残したまま止めたいとき）。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for LedgerScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// バックグラウンドで走査を始める。メッセージを送るたびに `wake` を呼ぶ（再描画要求用）。
pub fn spawn(db: Arc<Mutex<Database>>, query: LedgerQuery, wake: impl Fn() + Send + 'static) -> LedgerScan {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    std::thread::spawn(move || {
        let (scanned, total) = run_scan(&db, &query, CHUNK_SIZE, PROGRESS_STEP, &flag, |msg| {
            if tx.send(msg).is_ok() {
                wake();
            }
        });
        if !flag.load(Ordering::Relaxed) {
            let _ = tx.send(LedgerScanMsg::Done { scanned, total });
            wake();
        }
    });
    LedgerScan { rx, cancel }
}

/// 走査本体。`emit` に、ヒット（`chunk` 件ごと、と最後の端数）と進捗（`progress_step` 件ごと）を渡し、
/// （調べた件数, 総数）を返す。`cancel` が立ったら、その時点までの端数は送らずに打ち切る。
fn run_scan(
    db: &Arc<Mutex<Database>>,
    query: &LedgerQuery,
    chunk: usize,
    progress_step: usize,
    cancel: &AtomicBool,
    mut emit: impl FnMut(LedgerScanMsg),
) -> (usize, usize) {
    // 読み取りトランザクションを取ったらロックは放す（MVCCなので書き込みを止めない）。
    let tx = {
        let Ok(guard) = db.lock() else { return (0, 0) };
        let Ok(tx) = guard.begin_read() else { return (0, 0) };
        tx
    };
    let total = crate::file_identity::record_count_tx(&tx) as usize;
    let needs_tags = query.needs_tags();
    let mut scanned = 0usize;
    let mut buf: Vec<LedgerHit> = Vec::new();
    crate::file_identity::for_each_record_tx(&tx, |rec: FileRecord| {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        scanned += 1;
        let name = crate::file_identity::file_name_of(&rec.path_key);
        let hit = match query.pre_match(name, rec.size) {
            Some(r) => r,
            None => {
                debug_assert!(needs_tags);
                let tags = crate::file_settings::effective_tags_tx(&tx, &rec).unwrap_or_default();
                query.tags_match(&tags)
            }
        };
        if hit {
            buf.push(LedgerHit {
                id: rec.id,
                path: crate::file_identity::path_of_key(&rec.path_key),
                confirmed: rec.status == FileStatus::Confirmed,
            });
            if buf.len() >= chunk {
                emit(LedgerScanMsg::Hits(std::mem::take(&mut buf)));
            }
        }
        if progress_step > 0 && scanned % progress_step == 0 {
            emit(LedgerScanMsg::Progress { scanned, total });
        }
        true
    });
    if !buf.is_empty() && !cancel.load(Ordering::Relaxed) {
        emit(LedgerScanMsg::Hits(buf));
    }
    (scanned, total)
}

/// サイズ欄（MB。小数可）をバイトにする。空・数値でない・負・非有限は「制限なし」（None）。
pub fn parse_size_mb(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).map(|mb| (mb * 1024.0 * 1024.0) as u64)
}

/// タグマネージャーの全カテゴリ（メイン・ネガティブ tier を含む）から、全要素の（tier_id, 要素名）を集める。
/// 要素名は別名つき（`A|B|C`）のまま返す。要素が空の tier は飛ばす。
pub fn tier_names_of(categories: &[crate::view_explorer::TagManagerCategoryUi]) -> Vec<(u64, String)> {
    categories
        .iter()
        .flat_map(|c| c.tiers.iter())
        .filter_map(|t| t.element.as_ref().filter(|e| !e.trim().is_empty()).map(|e| (t.id, e.clone())))
        .collect()
}

impl LedgerQuery {
    /// 画面の入力（未パースの文字列）から条件を作る。サイズは MB の文字列。
    pub fn from_inputs(
        text: &str,
        size_min_mb: &str,
        size_max_mb: &str,
        tier_names: &[(u64, String)],
    ) -> Self {
        Self::new(text, tier_names, parse_size_mb(size_min_mb), parse_size_mb(size_max_mb))
    }
}

/// ジョブの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    /// 最後まで走査した。
    Done,
    /// 途中で打ち切った（それまでのヒットは残る）。
    Cancelled,
}

/// `LedgerSearchJob::poll` の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPoll {
    /// この呼び出しで増えたヒットの範囲（`hits()` の添字）。表示中の一覧へ追記する分。
    pub new_hits: std::ops::Range<usize>,
    /// この呼び出しで完了へ移った（1回だけ true になる）。
    pub finished: bool,
}

/// 1回の検索の実行を持つ。画面（egui）にも履歴にも依存しない状態機械で、呼び出し側が毎フレーム
/// `poll` するだけで、ヒット・進捗・未確認件数・完了が集まる。どのタブを表示していても、ジョブ自身は
/// 走査スレッドの結果を受け取り続ける（表示への反映は呼び出し側が `hits()` から行う）。
/// 捨てる（Drop）と走査は打ち切られる。
pub struct LedgerSearchJob {
    scan: Option<LedgerScan>,
    hits: Vec<PathBuf>,
    unconfirmed: usize,
    progress: Option<(usize, usize)>,
    state: JobState,
}

impl LedgerSearchJob {
    pub fn start(db: Arc<Mutex<Database>>, query: LedgerQuery, wake: impl Fn() + Send + 'static) -> Self {
        Self::from_scan(spawn(db, query, wake))
    }

    fn from_scan(scan: LedgerScan) -> Self {
        Self { scan: Some(scan), hits: Vec::new(), unconfirmed: 0, progress: None, state: JobState::Running }
    }

    /// 走査スレッドからのメッセージを全部受け取って取り込む。
    pub fn poll(&mut self) -> JobPoll {
        let before = self.hits.len();
        let mut finished = false;
        if let Some(scan) = &self.scan {
            loop {
                match scan.rx.try_recv() {
                    Ok(LedgerScanMsg::Hits(hits)) => {
                        for h in hits {
                            if h.confirmed {
                                self.hits.push(h.path);
                            } else {
                                self.unconfirmed += 1;
                            }
                        }
                    }
                    Ok(LedgerScanMsg::Progress { scanned, total }) => self.progress = Some((scanned, total)),
                    Ok(LedgerScanMsg::Done { scanned, total }) => {
                        self.progress = Some((scanned, total));
                        finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    // 走査スレッドが完了を送らずに消えた（異常終了）。永遠に実行中にならないよう完了扱いにする。
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.scan = None;
            self.state = JobState::Done;
        }
        JobPoll { new_hits: before..self.hits.len(), finished }
    }

    /// 打ち切る。実行中でなければ何もしない。それまでのヒットは残る。
    pub fn cancel(&mut self) {
        if self.state != JobState::Running {
            return;
        }
        self.scan = None;
        self.state = JobState::Cancelled;
    }

    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn is_running(&self) -> bool {
        self.state == JobState::Running
    }

    /// 確認済みのヒット（到着順）。前回確認で見つからなかったものは含まない。
    pub fn hits(&self) -> &[PathBuf] {
        &self.hits
    }

    /// 前回の確認で見つからなかったため、一覧から外した件数。
    pub fn unconfirmed(&self) -> usize {
        self.unconfirmed
    }

    /// 進捗（調べた件数, 総数）。まだ最初の進捗が届いていなければ None。完了後は最終値。
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spread_state::{open_spread_db, write_archive_tags};
    use std::path::Path;

    fn toks(s: &str) -> Vec<String> {
        parse_tokens(s)
    }

    fn names_table() -> Vec<(u64, String)> {
        vec![
            (1, "サークルA|Circle A".to_string()),
            (2, "サークルAB".to_string()),
            (3, "Kouji".to_string()),
            (4, "漫画".to_string()),
        ]
    }

    fn q(text: &str) -> LedgerQuery {
        LedgerQuery::new(text, &names_table(), None, None)
    }

    #[test]
    fn tokens_split_on_half_and_full_width_spaces() {
        assert_eq!(toks("a b　c"), vec!["a", "b", "c"]);
        assert_eq!(toks("  a   b  "), vec!["a", "b"]);
        assert!(toks("").is_empty());
        assert!(toks("   　 ").is_empty());
    }

    #[test]
    fn quotes_keep_spaces_inside_one_token() {
        assert_eq!(toks("サークル名１ サークル名２ \"サー　クル名３\" ファイル名候補１"),
            vec!["サークル名１", "サークル名２", "サー　クル名３", "ファイル名候補１"]);
        assert_eq!(toks("\"a b\" c"), vec!["a b", "c"]);
        // 全角の引用符も使える
        assert_eq!(toks("\u{201C}a b\u{201D} c"), vec!["a b", "c"]);
        // 閉じていない引用符は末尾までを1語に
        assert_eq!(toks("x \"a b"), vec!["x", "a b"]);
        // 空の引用符と重複は捨てる
        assert_eq!(toks("\"\" a a"), vec!["a"]);
        // 引用符は語の途中でも区切りになる
        assert_eq!(toks("ab\"c d\"e"), vec!["ab", "c d", "e"]);
    }

    #[test]
    fn token_matches_the_file_name_case_insensitively_by_substring() {
        let query = q("circle");
        assert!(query.matches("My CIRCLE book.zip", 10, &[]));
        assert!(!query.matches("other.zip", 10, &[]));
        // glob
        assert!(q("a*.zip").matches("abc.zip", 1, &[]));
        assert!(!q("a*.zip").matches("xabc.zip", 1, &[]));
    }

    #[test]
    fn token_matches_tag_names_including_aliases_by_substring() {
        // 別名 "Circle A" のどれかに含まれる語は、tier 1 に当たる。
        let query = q("\"circle a\"");
        assert!(query.matches("x.zip", 1, &[1]));
        assert!(!query.matches("x.zip", 1, &[2]), "「サークルAB」は別名 Circle A を持たない");
        // 部分一致なので「サークルA」は 1 と 2 の両方に当たる
        let query = q("サークルa");
        assert!(query.matches("x.zip", 1, &[1]));
        assert!(query.matches("x.zip", 1, &[2]));
        assert!(!query.matches("x.zip", 1, &[3, 4]));
        // 大文字小文字は同一視
        assert!(q("KOUJI").matches("x.zip", 1, &[3]));
    }

    #[test]
    fn any_token_is_enough() {
        let query = q("kouji 漫画 nothing");
        assert!(query.matches("x.zip", 1, &[3]));
        assert!(query.matches("x.zip", 1, &[4]));
        assert!(query.matches("nothing here.zip", 1, &[]));
        assert!(!query.matches("x.zip", 1, &[1, 2]));
    }

    #[test]
    fn empty_text_passes_everything_and_size_narrows() {
        let all = q("");
        assert!(all.is_empty());
        assert!(all.matches("x.zip", 0, &[]));
        let sized = LedgerQuery::new("", &names_table(), Some(100), Some(200));
        assert!(!sized.is_empty());
        assert!(!sized.matches("x.zip", 99, &[]));
        assert!(sized.matches("x.zip", 100, &[]));
        assert!(sized.matches("x.zip", 200, &[]));
        assert!(!sized.matches("x.zip", 201, &[]));
    }

    #[test]
    fn size_is_anded_with_the_text() {
        let query = LedgerQuery::new("kouji", &names_table(), Some(100), None);
        assert!(query.matches("x.zip", 100, &[3]));
        assert!(!query.matches("x.zip", 99, &[3]), "文字列に当たってもサイズが足りない");
        assert!(!query.matches("x.zip", 1000, &[4]), "サイズは良いが文字列に当たらない");
    }

    #[test]
    fn tags_are_not_needed_when_no_token_matches_any_tag_name() {
        assert!(!q("zzz").needs_tags());
        assert!(q("kouji").needs_tags());
        assert!(!q("").needs_tags());
    }

    // ---- 走査（実DB） ----

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_ledger_search_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_file(dir: &Path, name: &str, len: usize) {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| ((i + name.len()) % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
    }

    /// (名前, サイズ, タグ) の台帳を作る（タグ空のファイルもIDだけ作る）。
    fn seed(tag: &str, files: &[(&str, usize, &[u64])]) -> (Arc<Mutex<Database>>, PathBuf) {
        let root = temp_dir(tag);
        let db = open_spread_db(&root).unwrap();
        for (name, len, tags) in files {
            make_file(&root, name, *len);
            crate::file_identity::ensure_record(&db, &root, name).unwrap();
            if !tags.is_empty() {
                assert!(write_archive_tags(&db, &root, name, tags));
            }
        }
        (db, root)
    }

    struct Run {
        hits: Vec<Vec<LedgerHit>>,
        progress: Vec<(usize, usize)>,
        scanned: usize,
        total: usize,
    }

    fn run(db: &Arc<Mutex<Database>>, query: &LedgerQuery, chunk: usize, step: usize, cancel: &AtomicBool) -> Run {
        let mut hits = Vec::new();
        let mut progress = Vec::new();
        let (scanned, total) = run_scan(db, query, chunk, step, cancel, |m| match m {
            LedgerScanMsg::Hits(h) => hits.push(h),
            LedgerScanMsg::Progress { scanned, total } => progress.push((scanned, total)),
            LedgerScanMsg::Done { .. } => unreachable!("run_scan は Done を送らない"),
        });
        Run { hits, progress, scanned, total }
    }

    fn names(r: &Run) -> Vec<String> {
        let mut v: Vec<String> =
            r.hits.iter().flatten().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn scan_finds_by_file_name_tag_name_and_size_across_the_ledger() {
        let (db, _root) = seed(
            "find",
            &[
                ("circle-a.zip", 3000, &[]),
                ("b.zip", 3100, &[3]),      // タグ Kouji
                ("c.zip", 3200, &[4]),      // タグ 漫画
                ("d.zip", 3300, &[1, 3]),   // サークルA|Circle A と Kouji
                ("e.zip", 3400, &[]),
            ],
        );
        let no_cancel = AtomicBool::new(false);
        // ファイル名だけ
        let r = run(&db, &q("circle-a"), 100, 0, &no_cancel);
        assert_eq!((r.scanned, r.total), (5, 5));
        assert_eq!(names(&r), vec!["circle-a.zip"]);
        // タグ名（OR）とファイル名の混在: kouji の b,d / 漫画 の c / ファイル名 e
        let r = run(&db, &q("kouji 漫画 e.zip"), 100, 0, &no_cancel);
        assert_eq!(names(&r), vec!["b.zip", "c.zip", "d.zip", "e.zip"]);
        // 別名のどれかで
        let r = run(&db, &q("\"circle a\""), 100, 0, &no_cancel);
        assert_eq!(names(&r), vec!["d.zip"]);
        // サイズだけ（文字列は空）
        let sized = LedgerQuery::new("", &names_table(), Some(3200), Some(3300));
        let r = run(&db, &sized, 100, 0, &no_cancel);
        assert_eq!(names(&r), vec!["c.zip", "d.zip"]);
        // 文字列とサイズのAND
        let both = LedgerQuery::new("kouji", &names_table(), Some(3200), None);
        let r = run(&db, &both, 100, 0, &no_cancel);
        assert_eq!(names(&r), vec!["d.zip"]);
        assert!(r.hits.iter().flatten().all(|h| h.confirmed));
    }

    #[test]
    fn scan_matches_nothing_for_an_unknown_word_and_everything_for_an_empty_query() {
        let (db, _root) = seed("empty", &[("a.zip", 3000, &[]), ("b.zip", 3100, &[3])]);
        let no_cancel = AtomicBool::new(false);
        assert!(names(&run(&db, &q("zzzzz"), 100, 0, &no_cancel)).is_empty());
        let all = run(&db, &q(""), 100, 0, &no_cancel);
        assert_eq!(names(&all), vec!["a.zip", "b.zip"]);
    }

    #[test]
    fn hits_come_in_chunks_with_a_final_remainder() {
        let files: Vec<(String, usize, &[u64])> = (0..5).map(|i| (format!("k{i}.zip"), 3000 + i * 7, &[][..])).collect();
        let refs: Vec<(&str, usize, &[u64])> = files.iter().map(|(n, l, t)| (n.as_str(), *l, *t)).collect();
        let (db, _root) = seed("chunk", &refs);
        let r = run(&db, &q("k"), 2, 0, &AtomicBool::new(false));
        assert_eq!(r.hits.iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 1]);
    }

    #[test]
    fn progress_is_reported_every_step_with_the_total() {
        let files: Vec<(String, usize, &[u64])> = (0..5).map(|i| (format!("p{i}.zip"), 3000 + i * 7, &[][..])).collect();
        let refs: Vec<(&str, usize, &[u64])> = files.iter().map(|(n, l, t)| (n.as_str(), *l, *t)).collect();
        let (db, _root) = seed("progress", &refs);
        let r = run(&db, &q("zzz"), 100, 2, &AtomicBool::new(false));
        assert_eq!(r.progress, vec![(2, 5), (4, 5)]);
        assert_eq!((r.scanned, r.total), (5, 5));
    }

    #[test]
    fn cancel_stops_the_scan_and_drops_the_remainder() {
        let (db, _root) = seed("cancel", &[("a.zip", 3000, &[]), ("b.zip", 3100, &[])]);
        let cancel = AtomicBool::new(true);
        let r = run(&db, &q("zip"), 100, 0, &cancel);
        assert_eq!(r.scanned, 0);
        assert!(r.hits.is_empty());
    }

    #[test]
    fn unconfirmed_records_are_flagged() {
        let (db, root) = seed("unconfirmed", &[("gone.zip", 3000, &[]), ("here.zip", 3100, &[])]);
        let key = crate::spread_state::make_key(&root, "gone.zip");
        assert!(crate::file_identity::mark_unconfirmed(&db, &key, crate::file_identity::now_unix()));
        let r = run(&db, &q("zip"), 100, 0, &AtomicBool::new(false));
        let mut flags: Vec<(String, bool)> = r
            .hits
            .iter()
            .flatten()
            .map(|h| (h.path.file_name().unwrap().to_string_lossy().into_owned(), h.confirmed))
            .collect();
        flags.sort();
        assert_eq!(flags, vec![("gone.zip".to_string(), false), ("here.zip".to_string(), true)]);
    }

    // ---- 条件の部品 ----

    #[test]
    fn size_inputs_are_parsed_in_megabytes_and_garbage_means_no_limit() {
        assert_eq!(parse_size_mb("10"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size_mb(" 0.5 "), Some(512 * 1024));
        assert_eq!(parse_size_mb(""), None);
        assert_eq!(parse_size_mb("abc"), None);
        assert_eq!(parse_size_mb("-1"), None);
        assert_eq!(parse_size_mb("NaN"), None);
        let q = LedgerQuery::from_inputs("", "1", "", &names_table());
        assert!(!q.is_empty());
        assert!(!q.matches("x.zip", 1024 * 1024 - 1, &[]));
        assert!(q.matches("x.zip", 1024 * 1024, &[]));
    }

    #[test]
    fn tier_names_come_from_every_category_including_main_and_skip_empty_tiers() {
        use crate::view_explorer::{TagManagerCategoryUi, TagManagerTierUi};
        let tier = |id: u64, element: Option<&str>, negative: bool| TagManagerTierUi {
            id,
            tier_no: 0,
            negative,
            element: element.map(String::from),
        };
        let cat = |name: &str, is_main: bool, tiers: Vec<TagManagerTierUi>| TagManagerCategoryUi {
            name: name.to_string(),
            tiers,
            color: egui::Color32::WHITE,
            is_main,
            single_select: is_main,
            system_kind: None,
        };
        let cats = vec![
            cat("main", true, vec![tier(1, Some("Doujinshi"), false), tier(2, None, false)]),
            cat("作者", false, vec![tier(10, Some("Kouji|コウジ"), false), tier(11, Some("  "), false), tier(12, Some("NG"), true)]),
        ];
        assert_eq!(
            tier_names_of(&cats),
            vec![(1, "Doujinshi".to_string()), (10, "Kouji|コウジ".to_string()), (12, "NG".to_string())]
        );
    }

    // ---- 検索ジョブ（状態機械） ----

    /// 完了まで poll を回す（タイムアウトで失敗）。途中の poll 結果も返す。
    fn drive(job: &mut LedgerSearchJob) -> Vec<JobPoll> {
        let start = std::time::Instant::now();
        let mut polls = Vec::new();
        while job.is_running() {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "検索ジョブが終わらない");
            polls.push(job.poll());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        polls
    }

    fn job_names(job: &LedgerSearchJob) -> Vec<String> {
        let mut v: Vec<String> =
            job.hits().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn job_collects_hits_progress_and_finishes_exactly_once() {
        let (db, _root) = seed("job_basic", &[("a.zip", 3000, &[]), ("b.zip", 3100, &[3]), ("c.txt", 3200, &[])]);
        let mut job = LedgerSearchJob::start(db, q("zip kouji"), || {});
        assert!(job.is_running());
        assert_eq!(job.state(), JobState::Running);
        let polls = drive(&mut job);
        assert_eq!(job.state(), JobState::Done);
        assert_eq!(job_names(&job), vec!["a.zip", "b.zip"]);
        assert_eq!(job.progress(), Some((3, 3)));
        assert_eq!(job.unconfirmed(), 0);
        assert_eq!(polls.iter().filter(|p| p.finished).count(), 1, "完了は1回だけ通知される");
        // 追記範囲の合計 = ヒット全体（取りこぼしも重複も無い）
        let total: usize = polls.iter().map(|p| p.new_hits.len()).sum();
        assert_eq!(total, job.hits().len());
        // 完了後の poll は何も起こさない
        let after = job.poll();
        assert!(!after.finished);
        assert!(after.new_hits.is_empty());
    }

    #[test]
    fn job_keeps_unconfirmed_files_out_of_the_hits_but_counts_them() {
        let (db, root) = seed("job_unconfirmed", &[("gone.zip", 3000, &[]), ("here.zip", 3100, &[])]);
        let key = crate::spread_state::make_key(&root, "gone.zip");
        assert!(crate::file_identity::mark_unconfirmed(&db, &key, crate::file_identity::now_unix()));
        let mut job = LedgerSearchJob::start(db, q("zip"), || {});
        drive(&mut job);
        assert_eq!(job_names(&job), vec!["here.zip"]);
        assert_eq!(job.unconfirmed(), 1);
    }

    #[test]
    fn job_new_hits_ranges_let_the_caller_append_to_a_shown_list() {
        let files: Vec<(String, usize, &[u64])> = (0..450).map(|i| (format!("n{i:03}.zip"), 3000 + i, &[][..])).collect();
        let refs: Vec<(&str, usize, &[u64])> = files.iter().map(|(n, l, t)| (n.as_str(), *l, *t)).collect();
        let (db, _root) = seed("job_ranges", &refs);
        let mut job = LedgerSearchJob::start(db, q("n"), || {});
        // 表示中の一覧を模す: 範囲ごとに追記していくと、最後にヒット全体と一致する
        let mut shown: Vec<PathBuf> = Vec::new();
        let start = std::time::Instant::now();
        while job.is_running() {
            assert!(start.elapsed() < std::time::Duration::from_secs(20));
            let p = job.poll();
            shown.extend_from_slice(&job.hits()[p.new_hits.clone()]);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(shown, job.hits());
        assert_eq!(shown.len(), 450);
    }

    #[test]
    fn job_cancel_stops_it_and_keeps_what_arrived() {
        let files: Vec<(String, usize, &[u64])> = (0..30).map(|i| (format!("c{i:02}.zip"), 3000 + i, &[][..])).collect();
        let refs: Vec<(&str, usize, &[u64])> = files.iter().map(|(n, l, t)| (n.as_str(), *l, *t)).collect();
        let (db, _root) = seed("job_cancel", &refs);
        let mut job = LedgerSearchJob::start(db, q("c"), || {});
        job.cancel();
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(!job.is_running());
        let before = job.hits().len();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let p = job.poll();
        assert!(!p.finished, "打ち切ったジョブは完了を通知しない");
        assert_eq!(job.hits().len(), before);
        // 完了後・打ち切り後の cancel は状態を変えない
        job.cancel();
        assert_eq!(job.state(), JobState::Cancelled);
    }

    #[test]
    fn cancel_after_done_does_not_turn_it_into_cancelled() {
        let (db, _root) = seed("job_done_cancel", &[("a.zip", 3000, &[])]);
        let mut job = LedgerSearchJob::start(db, q("a"), || {});
        drive(&mut job);
        job.cancel();
        assert_eq!(job.state(), JobState::Done);
    }

    #[test]
    fn dropping_the_job_cancels_the_scan_thread() {
        let (db, _root) = seed("job_drop", &[("a.zip", 3000, &[])]);
        let job = LedgerSearchJob::start(db, q("a"), || {});
        let flag = Arc::clone(&job.scan.as_ref().unwrap().cancel);
        drop(job);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn a_dead_scan_thread_is_treated_as_finished() {
        let (tx, rx) = mpsc::channel();
        drop(tx);
        let scan = LedgerScan { rx, cancel: Arc::new(AtomicBool::new(false)) };
        let mut job = LedgerSearchJob::from_scan(scan);
        let p = job.poll();
        assert!(p.finished);
        assert_eq!(job.state(), JobState::Done);
    }

    #[test]
    fn a_job_keeps_collecting_while_nobody_looks_at_its_hits() {
        // 「他のタブにいても継続する」の本体: 呼び出し側が表示に使わなくても、poll だけで集まり続ける。
        let (db, _root) = seed("job_background", &[("x1.zip", 3000, &[]), ("x2.zip", 3100, &[])]);
        let mut job = LedgerSearchJob::start(db, q("x"), || {});
        drive(&mut job);
        assert_eq!(job.hits().len(), 2);
    }

    // ---- 実フィクスチャ（test/tag_fixtures）の結合テスト ----

    /// `test/tag_fixtures` のファイルを一時フォルダへ写し、台帳に登録する。
    /// 名前に作者の表記ゆれ（Kouji/Koji/Kohji/コウジ…）を含むファイルへ作者タグ(10)、
    /// グループ名（Hawk Bit/hawk-bit/ホークビット…）を含むファイルへグループタグ(20)を付ける。
    fn seed_fixtures(tag: &str) -> (Arc<Mutex<Database>>, PathBuf, Vec<String>) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/tag_fixtures");
        let root = temp_dir(tag);
        let db = open_spread_db(&root).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&src)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in &names {
            std::fs::copy(src.join(name), root.join(name)).unwrap();
            crate::file_identity::ensure_record(&db, &root, name).unwrap();
            let lower = name.to_lowercase();
            let mut tags = Vec::new();
            if ["kouji", "koji", "kohji", "ｺｳｼﾞ", "こうじ"].iter().any(|k| lower.contains(k)) {
                tags.push(10);
            }
            if ["hawk bit", "hawk-bit", "ホークビット", "ﾎｰｸﾋﾞｯﾄ"].iter().any(|k| lower.contains(k)) {
                tags.push(20);
            }
            if !tags.is_empty() {
                assert!(write_archive_tags(&db, &root, name, &tags));
            }
        }
        (db, root, names)
    }

    fn fixture_tier_names() -> Vec<(u64, String)> {
        vec![
            (10, "Kouji|Kohji|Koji|コウジ|ｺｳｼﾞ|こうじ".to_string()),
            (20, "Hawk Bit|hawk-bit|ホークビット".to_string()),
        ]
    }

    fn fixture_search(db: &Arc<Mutex<Database>>, text: &str) -> Vec<String> {
        let query = LedgerQuery::from_inputs(text, "", "", &fixture_tier_names());
        let mut job = LedgerSearchJob::start(Arc::clone(db), query, || {});
        drive(&mut job);
        job_names(&job)
    }

    fn expected(names: &[String], pred: impl Fn(&str) -> bool) -> Vec<String> {
        let mut v: Vec<String> = names.iter().filter(|n| pred(n)).cloned().collect();
        v.sort();
        v
    }

    #[test]
    fn fixtures_are_found_by_author_alias_even_when_the_file_name_has_another_spelling() {
        let (db, _root, names) = seed_fixtures("fx_author");
        // 「コウジ」は別名。ファイル名が Kouji/Koji/Kohji でも、作者タグ経由で拾える。
        let got = fixture_search(&db, "コウジ");
        let want = expected(&names, |n| {
            let l = n.to_lowercase();
            ["kouji", "koji", "kohji", "ｺｳｼﾞ", "こうじ"].iter().any(|k| l.contains(k))
        });
        assert!(want.len() >= 6, "フィクスチャに作者ゆれの付いたファイルがある前提: {want:?}");
        assert_eq!(got, want);
        // ファイル名だけでは「コウジ」に当たるのは ホークビット(こうじ) 等に限られる = タグ経由で増えている
        assert!(got.len() > expected(&names, |n| n.contains("コウジ")).len());
    }

    #[test]
    fn fixtures_are_found_by_group_alias_and_quoted_phrases() {
        let (db, _root, names) = seed_fixtures("fx_group");
        let got = fixture_search(&db, "\"hawk bit\"");
        let want = expected(&names, |n| {
            let l = n.to_lowercase();
            ["hawk bit", "hawk-bit", "ホークビット", "ﾎｰｸﾋﾞｯﾄ"].iter().any(|k| l.contains(k))
        });
        assert!(!want.is_empty());
        assert_eq!(got, want);
    }

    #[test]
    fn fixtures_or_across_tag_names_and_file_names_with_full_width_separators() {
        let (db, _root, names) = seed_fixtures("fx_or");
        // 全角スペース区切り・引用符つきの句・ファイル名の語を混ぜる（どれかに当たればヒット）。
        let got = fixture_search(&db, "ホークビット\u{3000}\"Circle D\"\u{3000}Plain");
        let want = expected(&names, |n| {
            let l = n.to_lowercase();
            ["hawk bit", "hawk-bit", "ホークビット", "ﾎｰｸﾋﾞｯﾄ"].iter().any(|k| l.contains(k))
                || l.contains("circle d")
                || l.contains("plain")
        });
        assert!(want.iter().any(|n| n.contains("Circle D")) && want.iter().any(|n| n.contains("Plain")));
        assert_eq!(got, want);
    }

    #[test]
    fn fixtures_file_name_only_search_needs_no_tags_and_unknown_words_find_nothing() {
        let (db, _root, names) = seed_fixtures("fx_name");
        let got = fixture_search(&db, "Circle");
        assert_eq!(got, expected(&names, |n| n.contains("Circle")));
        assert_eq!(got.len(), 4);
        assert!(fixture_search(&db, "zzzz-no-such-word").is_empty());
        // glob（従来のファイル名検索と同じ規則）
        let got = fixture_search(&db, "*.7z");
        assert_eq!(got, expected(&names, |n| n.ends_with(".7z")));
    }

    #[test]
    fn fixtures_size_narrows_the_text_result() {
        let (db, root, names) = seed_fixtures("fx_size");
        let sizes: Vec<(String, u64)> =
            names.iter().map(|n| (n.clone(), std::fs::metadata(root.join(n)).unwrap().len())).collect();
        let max = sizes.iter().map(|(_, s)| *s).max().unwrap();
        let min_mb = format!("{}", (max as f64) / (1024.0 * 1024.0));
        // 最大サイズ以上だけ = 最大サイズのファイルだけ（丸めの誤差を避けるため、結果が1件以上で最大に一致するものだけを確認）
        let query = LedgerQuery::from_inputs("", &min_mb, "", &fixture_tier_names());
        let mut job = LedgerSearchJob::start(Arc::clone(&db), query, || {});
        drive(&mut job);
        for p in job.hits() {
            assert!(std::fs::metadata(p).unwrap().len() as f64 >= max as f64 - 1.0);
        }
        assert!(!job.hits().is_empty());
        // 上限を 0 にすると、サイズ0のファイルだけ（フィクスチャに無ければ0件）
        let none = LedgerQuery::from_inputs("", "", "0", &fixture_tier_names());
        let mut job = LedgerSearchJob::start(db, none, || {});
        drive(&mut job);
        assert_eq!(job.hits().len(), sizes.iter().filter(|(_, s)| *s == 0).count());
    }
}
