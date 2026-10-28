//! 重複マーカーの完全比較。ユーザーが「おかしい」と思ったときだけ、2つのファイルを先頭から直接バイト比較する。
//!
//! - 自動では動かさない。ハッシュはDBに保存しない（結果の保存は「完全一致を確認済み」の印だけ）
//! - 不一致が見つかった時点で打ち切る。一致なら最後まで読む
//! - 開始時と終了時に、両方のサイズ・更新日時を確認する。変わっていたら結果を信用せず「変化」で終える
//! - キャンセルは、ジョブごとのフラグで次の塊の前に止まる
//! - 専用スレッドで1件ずつ処理する（大きなファイルを比べている間に、存在確認のスレッドを止めない）

use std::collections::HashMap;
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
#[derive(PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime: Option<SystemTime>,
}

fn stat(path: &Path) -> Option<Stat> {
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

impl super::NekoviewApp {
    /// 完全比較を始める。同じ組が実行中なら何もしない。`label` は終了時のトーストに出す名前。
    pub(super) fn start_marker_compare(&mut self, key: PairKey, a: PathBuf, b: PathBuf, label: String) {
        if matches!(self.marker_compare_states.get(&key), Some(CompareState::Running { .. })) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.marker_compare_states.insert(key, CompareState::Running { done: 0, total: 0, cancel: Arc::clone(&cancel) });
        self.marker_compare_labels.insert(key, label);
        self.marker_compare.submit(key, a, b, cancel);
    }

    /// 実行中（または順番待ち）の比較を止める。止まったら状態ごと消えて、またボタンから始められる。
    pub(super) fn cancel_marker_compare(&mut self, key: PairKey) {
        if let Some(CompareState::Running { cancel, .. }) = self.marker_compare_states.get(&key) {
            cancel.store(true, Ordering::Release);
        }
    }

    /// 終了時。実行中の比較を全て止める。
    pub(super) fn shutdown_marker_compare(&mut self) {
        for state in self.marker_compare_states.values() {
            if let CompareState::Running { cancel, .. } = state {
                cancel.store(true, Ordering::Release);
            }
        }
        self.marker_compare.shutdown();
    }

    /// 比較の進捗・結果を受け取る。完全一致で、全ての重複先と一致が揃ったら台帳に「確認済み」を残す。
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
        let label = self.marker_compare_labels.remove(&key).unwrap_or_default();
        if outcome == CompareOutcome::Cancelled {
            self.marker_compare_states.remove(&key);
            return;
        }
        self.marker_compare_states.insert(key, CompareState::Done(outcome));
        let t = crate::i18n::t();
        let msg = match outcome {
            CompareOutcome::Identical => t.dup_compare_toast_identical(&label),
            CompareOutcome::Different => t.dup_compare_toast_different(&label),
            CompareOutcome::Changed => t.dup_compare_toast_changed(&label),
            CompareOutcome::Failed | CompareOutcome::Cancelled => t.dup_compare_toast_failed(&label),
        };
        self.set_toast(msg);
        if outcome == CompareOutcome::Identical {
            self.mark_verified_if_complete(key.0);
        }
    }

    /// 持ち主のマーカーの全ての重複先と完全一致が揃っていれば、「確認済み」を台帳に残して表示を読み直す。
    fn mark_verified_if_complete(&mut self, owner: u64) {
        let Some(db) = self.spread_db.clone() else { return };
        let Some(rec) = crate::duplicate_marker::get(&db, owner) else { return };
        if all_targets_identical(owner, &rec.targets, &self.marker_presence, &self.marker_compare_states)
            && crate::duplicate_marker::set_verified(&db, owner)
        {
            self.reload_dir_state_maps();
        }
    }
}

/// 「全ての重複先と一致を確認できたか」。消えたと確認できた重複先は比べようがないので数えない。
/// 比べる対象が1つも残らない、未確認のオフライン・未比較が1つでもあれば false。
pub(super) fn all_targets_identical(
    owner: u64,
    targets: &[u64],
    presence: &super::dup_marker_ui::PresenceMap,
    states: &HashMap<PairKey, CompareState>,
) -> bool {
    let required: Vec<u64> = targets
        .iter()
        .copied()
        .filter(|t| presence.get(t) != Some(&crate::file_identity::Presence::Missing))
        .collect();
    !required.is_empty()
        && required.iter().all(|t| matches!(states.get(&(owner, *t)), Some(CompareState::Done(CompareOutcome::Identical))))
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
    fn all_targets_identical_needs_every_live_target_done() {
        let ok = |t: u64| ((9, t), CompareState::Done(CompareOutcome::Identical));
        let presence = |pairs: &[(u64, Presence)]| pairs.iter().copied().collect();
        let states: HashMap<_, _> = [ok(1), ok(2)].into_iter().collect();
        assert!(all_targets_identical(9, &[1, 2], &presence(&[]), &states));
        assert!(!all_targets_identical(9, &[1, 2, 3], &presence(&[]), &states), "未比較の重複先がある");
        assert!(all_targets_identical(9, &[1, 2, 3], &presence(&[(3, Presence::Missing)]), &states), "消失は数えない");
        assert!(!all_targets_identical(9, &[1, 2, 3], &presence(&[(3, Presence::Offline)]), &states), "オフラインは数える");
        assert!(!all_targets_identical(9, &[3], &presence(&[(3, Presence::Missing)]), &states), "比べる対象が無い");
        let mut diff = states.clone();
        diff.insert((9, 2), CompareState::Done(CompareOutcome::Different));
        assert!(!all_targets_identical(9, &[1, 2], &presence(&[]), &diff));
        let mut running = states;
        running.insert((9, 2), CompareState::Running { done: 0, total: 0, cancel: Arc::new(AtomicBool::new(false)) });
        assert!(!all_targets_identical(9, &[1, 2], &presence(&[]), &running));
    }
}
