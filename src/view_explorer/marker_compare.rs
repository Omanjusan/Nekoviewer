//! 重複マーカーの完全比較。ユーザーが「おかしい」と思ったときだけ、2つのファイルを先頭から直接バイト比較する。
//!
//! - 自動では動かさない。ハッシュはDBに保存しない（結果の保存は「完全一致を確認済み」の印だけ）
//! - 不一致が見つかった時点で打ち切る。一致なら最後まで読む
//! - 開始時と終了時に、両方のサイズ・更新日時を確認する。変わっていたら結果を信用せず「変化」で終える
//! - キャンセルは、ジョブごとのフラグで次の塊の前に止まる
//! - 専用スレッドで1件ずつ処理する（大きなファイルを比べている間に、存在確認のスレッドを止めない）

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

/// 一度に読む量（ファイルごと）。
const BUF_SIZE: usize = 1 << 20;
/// 進捗を送る最短の間隔。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 比べる2つのファイル。`owner` は再取得マーカーを持つ側、`target` はその重複先。
pub(super) type PairKey = (u64, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompareOutcome {
    /// 最後まで全く同じ。
    Identical,
    /// どこかが違う（サイズ違いを含む）。
    Different,
    /// 比較の前後でサイズか更新日時が変わった（書き込み中など）。結果は信用しない。
    Changed,
    /// 利用者がキャンセルした。
    Cancelled,
    /// 読めなかった（無い・権限・読み取りエラー）。
    Failed,
}

pub(super) enum CompareEvent {
    Progress { key: PairKey, done: u64, total: u64 },
    Finished { key: PairKey, outcome: CompareOutcome },
}

struct Job {
    key: PairKey,
    a: PathBuf,
    b: PathBuf,
    cancel: Arc<AtomicBool>,
}

pub(super) struct MarkerCompareWorker {
    job_tx: Option<mpsc::Sender<Job>>,
    pub(super) event_rx: mpsc::Receiver<CompareEvent>,
}

impl MarkerCompareWorker {
    pub(super) fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (event_tx, event_rx) = mpsc::channel::<CompareEvent>();
        std::thread::Builder::new()
            .name("marker-compare".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let key = job.key;
                    let mut last_sent = Instant::now() - PROGRESS_INTERVAL;
                    let outcome = compare_paths(&job.a, &job.b, &|| !job.cancel.load(Ordering::Acquire), &mut |done, total| {
                        if last_sent.elapsed() >= PROGRESS_INTERVAL {
                            last_sent = Instant::now();
                            let _ = event_tx.send(CompareEvent::Progress { key, done, total });
                            ctx.request_repaint();
                        }
                    });
                    let _ = event_tx.send(CompareEvent::Finished { key, outcome });
                    ctx.request_repaint();
                }
            })
            .expect("spawn marker-compare");
        Self { job_tx: Some(job_tx), event_rx }
    }

    /// 比較を依頼する。依頼順に1件ずつ処理する。
    pub(super) fn submit(&self, key: PairKey, a: PathBuf, b: PathBuf, cancel: Arc<AtomicBool>) {
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job { key, a, b, cancel });
        }
    }

    /// 終了時に呼ぶ。以後の依頼は受け付けない（処理中のものは、呼び出し側が各フラグで止める）。
    pub(super) fn shutdown(&mut self) {
        self.job_tx = None;
    }
}

/// 比較の状態（UI側が持つ）。
#[derive(Clone)]
pub(super) enum CompareState {
    /// 依頼済み・実行中。`total` が 0 の間は、まだ大きさが分からない。
    Running { done: u64, total: u64, cancel: Arc<AtomicBool> },
    Done(CompareOutcome),
}

/// 重複先の確認を省くかの判定に使う、2つのファイルの「いまの姿」。
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Stat {
    size: u64,
    mtime: Option<SystemTime>,
}

pub(super) fn stat(path: &Path) -> Option<Stat> {
    let md = std::fs::metadata(path).ok()?;
    md.is_file().then(|| Stat { size: md.len(), mtime: md.modified().ok() })
}

/// 2つのファイルを先頭から比べる。`keep_going` が false になったら Cancelled。
/// `progress(読んだ量, 全体)` を塊ごとに呼ぶ。
pub(super) fn compare_paths(
    a: &Path,
    b: &Path,
    keep_going: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> CompareOutcome {
    let (Some(a_before), Some(b_before)) = (stat(a), stat(b)) else { return CompareOutcome::Failed };
    if a_before.size != b_before.size {
        return CompareOutcome::Different;
    }
    let total = a_before.size;
    let (Ok(mut fa), Ok(mut fb)) = (File::open(a), File::open(b)) else { return CompareOutcome::Failed };
    let (mut buf_a, mut buf_b) = (vec![0u8; BUF_SIZE], vec![0u8; BUF_SIZE]);
    let mut done = 0u64;
    progress(0, total);
    loop {
        if !keep_going() {
            return CompareOutcome::Cancelled;
        }
        let (Ok(na), Ok(nb)) = (read_full(&mut fa, &mut buf_a), read_full(&mut fb, &mut buf_b)) else {
            return CompareOutcome::Failed;
        };
        if na != nb || buf_a[..na] != buf_b[..nb] {
            // 比べている最中に書き換えられて食い違った可能性があるので、姿が変わっていないときだけ不一致とする。
            return if unchanged(a, &a_before, b, &b_before) { CompareOutcome::Different } else { CompareOutcome::Changed };
        }
        if na == 0 {
            break;
        }
        done += na as u64;
        progress(done, total);
    }
    // 読み終えた後に、サイズ・更新日時が変わっていないことを確かめる（比較中に書き換えられていないか）。
    if !unchanged(a, &a_before, b, &b_before) || done != total {
        return CompareOutcome::Changed;
    }
    CompareOutcome::Identical
}

/// 2つのファイルが、比較の開始時と同じサイズ・更新日時のままか。
fn unchanged(a: &Path, a_before: &Stat, b: &Path, b_before: &Stat) -> bool {
    stat(a).as_ref() == Some(a_before) && stat(b).as_ref() == Some(b_before)
}

/// `buf` が埋まるか、終端に着くまで読む。読めた量を返す。
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// 比較結果が今も有効か（比べた時のサイズ・更新日時から変わっていないか）。
pub(super) fn stamps_valid(stamped: &(Stat, Stat), a_now: Option<&Stat>, b_now: Option<&Stat>) -> bool {
    a_now == Some(&stamped.0) && b_now == Some(&stamped.1)
}

/// 結果を保持する値か（一致・相違だけ。変化・読取不可は、もう一度比べたいので保持しない扱い）。
fn is_settled(outcome: CompareOutcome) -> bool {
    matches!(outcome, CompareOutcome::Identical | CompareOutcome::Different)
}

impl super::NekoviewApp {
    /// 組 `key` の完全比較を依頼する（順番待ちを含む。実行は専用スレッドで1組ずつ）。
    pub(super) fn start_marker_compare(&mut self, key: PairKey, a: PathBuf, b: PathBuf) {
        if matches!(self.marker_compare_states.get(&key), Some(CompareState::Running { .. })) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.marker_compare_states.insert(key, CompareState::Running { done: 0, total: 0, cancel: Arc::clone(&cancel) });
        self.marker_compare_stamps.remove(&key);
        self.marker_compare_jobs.insert(key, (a.clone(), b.clone()));
        self.marker_compare.submit(key, a, b, cancel);
    }

    /// 実行中・順番待ちの比較を全て止める（順番待ちのものは、始まる前に取り消される）。
    pub(super) fn cancel_all_marker_compares(&mut self) {
        for state in self.marker_compare_states.values() {
            if let CompareState::Running { cancel, .. } = state {
                cancel.store(true, Ordering::Release);
            }
        }
    }

    /// 実行中・順番待ちの比較の数。
    pub(super) fn running_marker_compares(&self) -> usize {
        self.marker_compare_states.values().filter(|s| matches!(s, CompareState::Running { .. })).count()
    }

    pub(super) fn shutdown_marker_compare(&mut self) {
        self.cancel_all_marker_compares();
        self.marker_compare.shutdown();
    }

    pub(super) fn poll_marker_compare(&mut self) {
        let mut touched = false;
        while let Ok(event) = self.marker_compare.event_rx.try_recv() {
            touched = true;
            match event {
                CompareEvent::Progress { key, done, total } => {
                    if let Some(CompareState::Running { done: d, total: t, .. }) = self.marker_compare_states.get_mut(&key) {
                        (*d, *t) = (done, total);
                    }
                }
                CompareEvent::Finished { key, outcome } => self.finish_marker_compare(key, outcome),
            }
        }
        if touched {
            self.egui_ctx.request_repaint();
        }
    }

    fn finish_marker_compare(&mut self, key: PairKey, outcome: CompareOutcome) {
        let paths = self.marker_compare_jobs.remove(&key);
        if outcome == CompareOutcome::Cancelled {
            self.marker_compare_states.remove(&key);
            return;
        }
        self.marker_compare_states.insert(key, CompareState::Done(outcome));
        if is_settled(outcome)
            && let Some((a, b)) = paths
            && let (Some(sa), Some(sb)) = (stat(&a), stat(&b))
        {
            self.marker_compare_stamps.insert(key, (sa, sb));
        }
        // 全ての比較が終わったら、1回だけ知らせる（一覧を閉じていても結果が分かるように）。
        if self.running_marker_compares() == 0 {
            self.set_toast(crate::i18n::t().similar_compare_done_toast());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::file_identity::Presence;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("nekoviewer_marker_compare_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, data: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, data).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(a: &Path, b: &Path) -> CompareOutcome {
        compare_paths(a, b, &|| true, &mut |_, _| {})
    }

    /// BUF_SIZE をまたぐ、中身が位置で決まるデータ。
    fn big(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn identical_files_including_across_buffer_boundaries() {
        let t = TempDir::new("same");
        for len in [0, 1, BUF_SIZE - 1, BUF_SIZE, BUF_SIZE + 1, BUF_SIZE * 2 + 17] {
            let (a, b) = (t.file("a", &big(len)), t.file("b", &big(len)));
            assert_eq!(run(&a, &b), CompareOutcome::Identical, "len={len}");
        }
    }

    #[test]
    fn a_single_byte_difference_anywhere_is_found() {
        let t = TempDir::new("diff");
        let len = BUF_SIZE * 2 + 100;
        for pos in [0, BUF_SIZE - 1, BUF_SIZE, len - 1] {
            let mut other = big(len);
            other[pos] ^= 0xFF;
            let (a, b) = (t.file("a", &big(len)), t.file("b", &other));
            assert_eq!(run(&a, &b), CompareOutcome::Different, "pos={pos}");
        }
    }

    #[test]
    fn different_sizes_are_different_without_reading() {
        let t = TempDir::new("size");
        let (a, b) = (t.file("a", &big(10)), t.file("b", &big(11)));
        let reads = Cell::new(0);
        let out = compare_paths(&a, &b, &|| true, &mut |_, _| reads.set(reads.get() + 1));
        assert_eq!((out, reads.get()), (CompareOutcome::Different, 0));
    }

    #[test]
    fn missing_file_fails() {
        let t = TempDir::new("missing");
        let a = t.file("a", &big(10));
        assert_eq!(run(&a, &t.0.join("nope")), CompareOutcome::Failed);
        assert_eq!(run(&t.0.join("nope"), &a), CompareOutcome::Failed);
        assert_eq!(run(&a, &t.0), CompareOutcome::Failed, "ディレクトリは比べない");
    }

    #[test]
    fn cancel_stops_before_the_next_chunk() {
        let t = TempDir::new("cancel");
        let len = BUF_SIZE * 3;
        let (a, b) = (t.file("a", &big(len)), t.file("b", &big(len)));
        let chunks = Cell::new(0);
        let keep_going = || chunks.get() < 1;
        let out = compare_paths(&a, &b, &keep_going, &mut |done, _| {
            if done > 0 {
                chunks.set(chunks.get() + 1);
            }
        });
        assert_eq!(out, CompareOutcome::Cancelled);
        assert_eq!(chunks.get(), 1, "1塊読んだ後に止まる");
    }

    #[test]
    fn reports_progress_up_to_the_total() {
        let t = TempDir::new("progress");
        let len = BUF_SIZE + 5;
        let (a, b) = (t.file("a", &big(len)), t.file("b", &big(len)));
        let seen = RefCell::new(Vec::new());
        assert_eq!(
            compare_paths(&a, &b, &|| true, &mut |d, tot| seen.borrow_mut().push((d, tot))),
            CompareOutcome::Identical
        );
        let seen = seen.into_inner();
        assert_eq!(seen.first(), Some(&(0, len as u64)));
        assert_eq!(seen.last(), Some(&(len as u64, len as u64)));
    }

    #[test]
    fn a_file_changed_during_the_compare_is_reported_as_changed() {
        let t = TempDir::new("changed");
        let len = BUF_SIZE * 2;
        let (a, b) = (t.file("a", &big(len)), t.file("b", &big(len)));
        let appended = Cell::new(false);
        // 1塊読んだ後で、読み終えた領域の外（末尾）に追記する。読み取り範囲は変わらず一致に見えるが、
        // 終了時のサイズが開始時と違うので「変化」にする。
        let out = compare_paths(&a, &b, &|| true, &mut |done, _| {
            if done > 0 && !appended.get() {
                appended.set(true);
                let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
                std::io::Write::write_all(&mut f, b"x").unwrap();
            }
        });
        assert_eq!(out, CompareOutcome::Changed);
    }

    #[test]
    fn stamps_are_valid_only_while_both_files_are_unchanged() {
        let st = |size: u64| Stat { size, mtime: None };
        let stamped = (st(10), st(20));
        assert!(stamps_valid(&stamped, Some(&st(10)), Some(&st(20))));
        assert!(!stamps_valid(&stamped, Some(&st(11)), Some(&st(20))), "片方のサイズが変わった");
        assert!(!stamps_valid(&stamped, Some(&st(10)), None), "消えた");
    }

    #[test]
    fn only_identical_and_different_are_kept() {
        assert!(is_settled(CompareOutcome::Identical));
        assert!(is_settled(CompareOutcome::Different));
        assert!(!is_settled(CompareOutcome::Changed));
        assert!(!is_settled(CompareOutcome::Failed));
    }
}
