//! フォルダ表示時のファイルID解決を非同期で行うワーカー。
//!
//! 一覧のファイルを表示順（=画面上部から）に、stat一致なら何もせず、不一致なら観測（FP算出）→
//! 解決する。移動・リネームされたファイルは、元のパスの評価・しおりを引き継いだ状態でIDが
//! 付き替わるので、UI側は結果を受けて評価キャッシュ等を引き直す。書き込み中のファイルは後回しにして、
//! 数秒おきに数回だけ再試行する。新しいスキャンが始まると世代が進み、古いバッチは次のファイルで打ち切る。
//!
//! DBロックはファイル1件ごとに短く握る（`file_identity::resolve_path` の3段階）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use redb::Database;

use super::NekoviewApp;
use crate::file_identity::{self, Resolution, ResolveOutcome};

/// 結果を返す単位。これだけ溜まるか、バッチが終わると `Refresh` を送る。
const REFRESH_BATCH: usize = 50;
/// 書き込み中で後回しにしたファイルの再試行間隔と回数。
const RETRY_DELAY: Duration = Duration::from_secs(4);
const MAX_RETRY_ROUNDS: usize = 3;

pub(super) enum IdentityEvent {
    /// 移動・統合で評価等が付き替わった可能性のあるパス。UI側で引き直す。
    Refresh { generation: u64, paths: Vec<PathBuf> },
    /// この世代のバッチを処理し終えた（打ち切りを除く）。
    #[allow(dead_code)] // 解決バッチ完了時の再ソート（フェーズ2b）で使う。
    Done { generation: u64 },
}

struct Job {
    generation: u64,
    db: Arc<Mutex<Database>>,
    paths: Vec<PathBuf>,
}

pub(super) struct IdentityWorker {
    job_tx: Option<mpsc::Sender<Job>>,
    pub(super) event_rx: mpsc::Receiver<IdentityEvent>,
    current: Arc<AtomicU64>,
}

impl IdentityWorker {
    pub(super) fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (event_tx, event_rx) = mpsc::channel::<IdentityEvent>();
        let current = Arc::new(AtomicU64::new(0));
        let worker_current = Arc::clone(&current);
        std::thread::Builder::new()
            .name("identity-resolver".into())
            .spawn(move || run_worker(job_rx, event_tx, worker_current, ctx))
            .expect("spawn identity-resolver");
        Self { job_tx: Some(job_tx), event_rx, current }
    }

    /// バッチを投入する。世代を進めるので、処理中の古いバッチは次のファイルで打ち切られる。
    /// 返り値は今回の世代。
    pub(super) fn submit(&mut self, db: Arc<Mutex<Database>>, paths: Vec<PathBuf>) -> u64 {
        let generation = self.current.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job { generation, db, paths });
        }
        generation
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.current.load(Ordering::Acquire) == generation
    }

    /// 終了時に呼ぶ。処理中のバッチは次のファイルで打ち切り、以後の投入は受け付けない。
    pub(super) fn shutdown(&mut self) {
        self.current.fetch_add(1, Ordering::AcqRel);
        self.job_tx = None;
    }
}

fn run_worker(
    job_rx: mpsc::Receiver<Job>,
    event_tx: mpsc::Sender<IdentityEvent>,
    current: Arc<AtomicU64>,
    ctx: egui::Context,
) {
    while let Ok(job) = job_rx.recv() {
        let keep_going = || current.load(Ordering::Acquire) == job.generation;
        let mut pending = job.paths;
        let mut aborted = false;
        for round in 0..=MAX_RETRY_ROUNDS {
            let outcome = process_batch(
                &job.db,
                &pending,
                &keep_going,
                file_identity::now_unix(),
                REFRESH_BATCH,
                &mut |paths| {
                    let _ = event_tx.send(IdentityEvent::Refresh { generation: job.generation, paths });
                    ctx.request_repaint();
                },
            );
            aborted = outcome.aborted;
            pending = outcome.unstable;
            if aborted || pending.is_empty() || round == MAX_RETRY_ROUNDS {
                break;
            }
            if !sleep_while(RETRY_DELAY, &keep_going) {
                aborted = true;
                break;
            }
        }
        if !aborted {
            let _ = event_tx.send(IdentityEvent::Done { generation: job.generation });
            ctx.request_repaint();
        }
    }
}

/// `keep_going` が true の間だけ待つ。待ち切れたら true、打ち切られたら false。
fn sleep_while(total: Duration, keep_going: &dyn Fn() -> bool) -> bool {
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < total {
        if !keep_going() {
            return false;
        }
        std::thread::sleep(step);
        waited += step;
    }
    keep_going()
}

#[derive(Debug, Default)]
pub(super) struct BatchOutcome {
    /// 書き込み中の疑いで後回しにしたパス。
    pub unstable: Vec<PathBuf>,
    pub aborted: bool,
}

/// ディレクトリごとに正規化したキーの接頭辞を使い回す（`make_key` は呼ぶたびに canonicalize する）。
#[derive(Default)]
struct KeyMaker {
    prefixes: HashMap<PathBuf, String>,
}

impl KeyMaker {
    fn key(&mut self, dir: &Path, filename: &str) -> String {
        let prefix = self.prefixes.entry(dir.to_path_buf()).or_insert_with(|| {
            let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            format!("{}\0", canon.to_string_lossy())
        });
        format!("{prefix}{filename}")
    }
}

/// パスを順に解決する。`emit` には、移動・統合で内容が付き替わったパスを `refresh_batch` 件ずつ渡す。
/// `keep_going` が false になったら、次のファイルの前で打ち切る（溜まった分は先に渡す）。
pub(super) fn process_batch(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    keep_going: &dyn Fn() -> bool,
    now: i64,
    refresh_batch: usize,
    emit: &mut dyn FnMut(Vec<PathBuf>),
) -> BatchOutcome {
    let mut keys = KeyMaker::default();
    let mut outcome = BatchOutcome::default();
    let mut refresh: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !keep_going() {
            outcome.aborted = true;
            break;
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            continue;
        };
        let key = keys.key(dir, name);
        match file_identity::resolve_file(db, &key, path, now, false) {
            ResolveOutcome::Resolved(Resolution::Moved { .. } | Resolution::Merged { .. }) => {
                refresh.push(path.clone());
                if refresh.len() >= refresh_batch {
                    emit(std::mem::take(&mut refresh));
                }
            }
            ResolveOutcome::Unstable => outcome.unstable.push(path.clone()),
            // 一覧に出た後で消えた。削除はせず「未確認」にする。
            ResolveOutcome::Gone => {
                file_identity::mark_unconfirmed(db, &key);
            }
            ResolveOutcome::Unchanged | ResolveOutcome::Resolved(_) | ResolveOutcome::Failed(_) => {}
        }
    }
    if !refresh.is_empty() {
        emit(refresh);
    }
    outcome
}

impl NekoviewApp {
    /// 現在のフォルダの一覧（表示順）をワーカーへ投入する。スキャンとソートが済んだ直後に呼ぶ。
    pub(super) fn start_identity_resolution(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let paths: Vec<PathBuf> = self.archives.clone();
        self.identity_worker.submit(db, paths);
    }

    /// ワーカーの結果を受けて、付き替わったパスの評価・設定表示を引き直す。毎フレーム呼ぶ。
    pub(super) fn poll_identity_results(&mut self) {
        let events: Vec<IdentityEvent> = std::iter::from_fn(|| self.identity_worker.event_rx.try_recv().ok()).collect();
        for event in events {
            match event {
                IdentityEvent::Refresh { generation, paths } => {
                    if !self.identity_worker.is_current(generation) {
                        continue;
                    }
                    for p in &paths {
                        // 表示中の一覧に残っているものだけ（スキャンで入れ替わった後は無視する）。
                        if self.archive_rating_cache.contains_key(p) {
                            self.refresh_rating_cache(p);
                            self.refresh_saved_archive_settings(p);
                        }
                    }
                }
                IdentityEvent::Done { .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::spread_state::{open_spread_db, read_archive_rating, write_archive_rating};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_identity_worker_test_{}_{}_{}",
                std::process::id(),
                nonce,
                tag
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let d = self.0.join(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn file(dir: &Path, name: &str, len: usize, age_secs: u64) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(age_secs)).unwrap();
        p
    }

    fn run(db: &Arc<Mutex<Database>>, paths: &[PathBuf]) -> (BatchOutcome, Vec<Vec<PathBuf>>) {
        let mut emitted = Vec::new();
        let outcome = process_batch(
            db,
            paths,
            &|| true,
            file_identity::now_unix(),
            REFRESH_BATCH,
            &mut |p| emitted.push(p),
        );
        (outcome, emitted)
    }

    fn rec_of(db: &Arc<Mutex<Database>>, path: &Path) -> Option<file_identity::FileRecord> {
        file_identity::lookup(db, &crate::spread_state::make_key(path.parent()?, path.file_name()?.to_str()?))
    }

    #[test]
    fn batch_records_every_settled_file_without_refresh() {
        let t = TempRoot::new("record");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let paths = vec![file(&dir, "a.zip", 3000, 1000), file(&dir, "b.zip", 4000, 1000)];
        let (outcome, emitted) = run(&db, &paths);
        assert!(!outcome.aborted && outcome.unstable.is_empty());
        assert!(emitted.is_empty(), "新規作成だけでは引き直し不要");
        assert!(paths.iter().all(|p| rec_of(&db, p).is_some_and(|r| r.fp.is_some())));
        // 2回目はstat一致で何も変わらない（IDも同じ）。
        let ids: Vec<u64> = paths.iter().map(|p| rec_of(&db, p).unwrap().id).collect();
        let (_, emitted) = run(&db, &paths);
        assert!(emitted.is_empty());
        let ids2: Vec<u64> = paths.iter().map(|p| rec_of(&db, p).unwrap().id).collect();
        assert_eq!(ids, ids2);
    }

    #[test]
    fn moved_file_is_resolved_and_reported_for_refresh() {
        let t = TempRoot::new("moved");
        let db = open_spread_db(&t.0).unwrap();
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let p1 = file(&d1, "a.zip", 5000, 1000);
        assert!(write_archive_rating(&db, &d1, "a.zip", 9));

        let p2 = d2.join("renamed.zip");
        std::fs::rename(&p1, &p2).unwrap();
        // 新しい場所は未解決なので、評価はまだ見えない。
        assert_eq!(read_archive_rating(&db, &d2, "renamed.zip"), None);
        let (_, emitted) = run(&db, &[p2.clone()]);
        assert_eq!(emitted, vec![vec![p2]]);
        assert_eq!(read_archive_rating(&db, &d2, "renamed.zip").unwrap().rating_half, 9);
    }

    #[test]
    fn fresh_file_is_deferred_and_recorded_on_retry() {
        let t = TempRoot::new("unstable");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let p = file(&dir, "new.zip", 3000, 0); // mtimeが今＝書き込み中の疑い
        let (outcome, _) = run(&db, &[p.clone()]);
        assert_eq!(outcome.unstable, vec![p.clone()]);
        assert!(rec_of(&db, &p).is_none());
        // 落ち着いた後の再試行で記録される。
        file(&dir, "new.zip", 3000, 1000);
        let (outcome, _) = run(&db, &outcome.unstable);
        assert!(outcome.unstable.is_empty());
        assert!(rec_of(&db, &p).is_some());
    }

    #[test]
    fn batch_stops_when_generation_changes_and_flushes_pending_refresh() {
        let t = TempRoot::new("abort");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let paths: Vec<PathBuf> = (0..5).map(|i| file(&dir, &format!("{i}.zip"), 2000 + i, 1000)).collect();
        let calls = Cell::new(0);
        let keep_going = || {
            calls.set(calls.get() + 1);
            calls.get() <= 2 // 2件目まで処理して打ち切る
        };
        let outcome = process_batch(&db, &paths, &keep_going, file_identity::now_unix(), 50, &mut |_| {});
        assert!(outcome.aborted);
        assert!(rec_of(&db, &paths[0]).is_some() && rec_of(&db, &paths[1]).is_some());
        assert!(rec_of(&db, &paths[2]).is_none());
    }

    #[test]
    fn file_that_disappears_after_listing_becomes_unconfirmed_not_deleted() {
        let t = TempRoot::new("gone");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let p = file(&dir, "a.zip", 3000, 1000);
        run(&db, &[p.clone()]);
        std::fs::remove_file(&p).unwrap();
        let (outcome, _) = run(&db, &[p.clone()]);
        assert!(outcome.unstable.is_empty());
        let rec = rec_of(&db, &p).expect("レコードは消さない");
        assert_eq!(rec.status, file_identity::FileStatus::Unconfirmed);
    }

    #[test]
    fn refresh_is_emitted_in_batches() {
        let t = TempRoot::new("batches");
        let db = open_spread_db(&t.0).unwrap();
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let mut moved = Vec::new();
        for i in 0..5 {
            let p = file(&d1, &format!("{i}.zip"), 2000 + i, 1000);
            run(&db, &[p.clone()]);
            let q = d2.join(format!("m{i}.zip"));
            std::fs::rename(&p, &q).unwrap();
            moved.push(q);
        }
        let mut emitted = Vec::new();
        process_batch(&db, &moved, &|| true, file_identity::now_unix(), 2, &mut |p| emitted.push(p.len()));
        assert_eq!(emitted, vec![2, 2, 1]);
    }

    #[test]
    fn worker_thread_processes_a_job_and_reports_done() {
        let t = TempRoot::new("thread");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let p = file(&dir, "a.zip", 3000, 1000);
        let mut worker = IdentityWorker::spawn(egui::Context::default());
        let generation = worker.submit(db.clone(), vec![p.clone()]);
        assert!(worker.is_current(generation));
        let event = worker.event_rx.recv_timeout(Duration::from_secs(10)).expect("Done が届く");
        assert!(matches!(event, IdentityEvent::Done { generation: g } if g == generation));
        assert!(rec_of(&db, &p).is_some());
        // 新しい投入で世代が進み、古い世代は現行でなくなる。
        let generation2 = worker.submit(db, vec![]);
        assert!(generation2 > generation && !worker.is_current(generation));
        worker.shutdown();
        assert!(!worker.is_current(generation2));
    }
}
