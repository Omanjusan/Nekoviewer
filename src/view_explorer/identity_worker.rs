//! フォルダ表示時のファイルID解決を非同期で行うワーカー。
//!
//! 一覧のファイルを表示順（=画面上部から）に、stat一致なら何もせず、不一致なら観測（FP算出）→
//! 解決する。移動・リネームされたファイルは、元のパスの評価・しおりを引き継いだ状態でIDが
//! 付き替わるので、UI側は結果を受けて評価キャッシュ等を引き直す。書き込み中のファイルは後回しにして、
//! 数秒おきに数回だけ再試行する。新しいスキャンが始まると世代が進み、古いバッチは次のファイルで打ち切る。
//!
//! DBロックはファイル1件ごとに短く握る（`file_identity::resolve_path` の3段階）。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use redb::Database;

use super::NekoviewApp;
use crate::file_identity::{self, Resolution, ResolveOutcome};
use crate::identity_pending::PendingKind;

/// 結果を返す単位。これだけ溜まるか、バッチが終わると `Chunk` を送る。
/// 「検証中」のサムネ生成はここで解放されるので、小さめにして待ち時間を短くする。
const CHUNK_SIZE: usize = 10;
/// 進捗（処理済み件数）を、これだけ進むごとに通知する（検証済みが無くても送る）。
const PROGRESS_STEP: usize = 100;
/// 処理中トーストを出し始めるまでの待ち。これより早く終われば、トーストは出さない。
const TOAST_DELAY: Duration = Duration::from_secs(2);
/// 書き込み中で後回しにしたファイルの再試行間隔と回数。
const RETRY_DELAY: Duration = Duration::from_secs(4);
const MAX_RETRY_ROUNDS: usize = 3;
/// 起動後、バックフィルを始めるまでの待ち（初回スキャンと競合させない）。
const BACKFILL_DELAY: Duration = Duration::from_secs(5);
/// バックフィルは、ジョブが無い間だけ、この間隔で1件ずつ進める。
const BACKFILL_STEP: Duration = Duration::from_millis(5);

pub(super) enum IdentityEvent {
    /// 処理した一部の結果。`settled` は検証が済んだパス（「検証中」表示を外す）、`refresh` は
    /// そのうち移動・統合で評価等が付き替わった可能性のあるパス（UI側で引き直す）。
    Chunk { generation: u64, refresh: Vec<PathBuf>, settled: Vec<PathBuf>, processed: usize },
    /// この世代のバッチを処理し終えた（打ち切りを除く）。`pending_created` は今回新しく記録した未解決の件数
    /// （通知トーストの対象）、`pending_count` は掃除後の未解決の総数。
    Done { generation: u64, pending_created: usize, pending_count: usize },
}

enum Job {
    Resolve {
        generation: u64,
        db: Arc<Mutex<Database>>,
        paths: Vec<PathBuf>,
        /// サムネキャッシュの置き場所。移動したファイルのサムネを新しい名前へ引っ越すのに使う。
        cache_root: Option<PathBuf>,
    },
    /// ユーザーデータを持つ旧レコードを、アイドル時にFP化する。
    Backfill { db: Arc<Mutex<Database>> },
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
    pub(super) fn submit(
        &mut self,
        db: Arc<Mutex<Database>>,
        paths: Vec<PathBuf>,
        cache_root: Option<PathBuf>,
    ) -> u64 {
        let generation = self.current.fetch_add(1, Ordering::AcqRel) + 1;
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job::Resolve { generation, db, paths, cache_root });
        }
        generation
    }

    /// 旧レコードのバックフィルを依頼する（起動後に1回）。フォルダ表示のバッチが来れば、そちらが先。
    pub(super) fn submit_backfill(&mut self, db: Arc<Mutex<Database>>) {
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job::Backfill { db });
        }
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.current.load(Ordering::Acquire) == generation
    }

    /// 終了時に呼ぶ。処理中のバッチは次のファイルで打ち切り、以後の投入は受け付けない
    /// （バックフィルはジョブ送信側が閉じたことで止まる）。
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
    let mut backfill: Option<Backfill> = None;
    loop {
        // バックフィル中は、短い待ちでジョブを確認しつつ1件ずつ進める（ジョブが来たら必ずそちらが先）。
        let job = if backfill.is_some() {
            match job_rx.recv_timeout(BACKFILL_STEP) {
                Ok(job) => Some(job),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match job_rx.recv() {
                Ok(job) => Some(job),
                Err(_) => break,
            }
        };
        match job {
            Some(Job::Resolve { generation, db, paths, cache_root }) => {
                run_resolve_job(generation, &db, paths, cache_root.as_deref(), &current, &event_tx, &ctx);
            }
            Some(Job::Backfill { db }) => backfill = Some(Backfill::new(db, BACKFILL_DELAY)),
            None => {
                if backfill.as_mut().is_some_and(|b| !b.step()) {
                    backfill = None;
                }
            }
        }
    }
}

fn run_resolve_job(
    generation: u64,
    db: &Arc<Mutex<Database>>,
    paths: Vec<PathBuf>,
    cache_root: Option<&Path>,
    current: &AtomicU64,
    event_tx: &mpsc::Sender<IdentityEvent>,
    ctx: &egui::Context,
) {
    let keep_going = || current.load(Ordering::Acquire) == generation;
    let mut pending = paths;
    let mut aborted = false;
    let mut pending_created = 0;
    for round in 0..=MAX_RETRY_ROUNDS {
        let outcome = process_batch_with(
            db,
            &pending,
            &keep_going,
            file_identity::now_unix(),
            CHUNK_SIZE,
            &|new_path, from_key| {
                if let Some(root) = cache_root {
                    transplant_thumbnail_for_move(root, new_path, from_key);
                }
            },
            &mut |chunk| {
                let _ = event_tx.send(IdentityEvent::Chunk {
                    generation,
                    refresh: chunk.refresh,
                    settled: chunk.settled,
                    processed: chunk.processed,
                });
                ctx.request_repaint();
            },
        );
        aborted = outcome.aborted;
        pending_created += outcome.pending_created;
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
        // 状況が変わって意味を失った未解決（対象に記録が付いた、候補が消えた等）を除いてから、件数を知らせる。
        crate::identity_pending::prune_real(db);
        let pending_count = crate::identity_pending::count(db);
        let _ = event_tx.send(IdentityEvent::Done { generation, pending_created, pending_count });
        ctx.request_repaint();
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

/// ユーザーデータ（評価・有効なしおり）を持つのにID記録が無い旧レコードを、1件ずつFP化する。
/// 未訪問のフォルダのファイルも、アップグレード後すぐ移動追従の対象にするため。
/// 存在しないパス（移動・削除済み）は救えないので何もしない。
struct Backfill {
    db: Arc<Mutex<Database>>,
    not_before: Instant,
    keys: Option<VecDeque<String>>,
}

impl Backfill {
    fn new(db: Arc<Mutex<Database>>, delay: Duration) -> Self {
        Self { db, not_before: Instant::now() + delay, keys: None }
    }

    /// 1件進める。まだ残りがあれば true。
    fn step(&mut self) -> bool {
        if Instant::now() < self.not_before {
            return true;
        }
        let keys = self
            .keys
            .get_or_insert_with(|| crate::spread_state::legacy_data_keys_without_id(&self.db).into());
        let Some(key) = keys.pop_front() else { return false };
        let path = file_identity::path_of_key(&key);
        // 書き込み中・読めないものは今回は見送る（次回起動時にまた対象になる）。
        file_identity::resolve_file(&self.db, &key, &path, file_identity::now_unix(), false);
        !keys.is_empty()
    }
}

#[derive(Debug, Default)]
pub(super) struct BatchOutcome {
    /// 書き込み中の疑いで後回しにしたパス。
    pub unstable: Vec<PathBuf>,
    pub aborted: bool,
    /// この処理で新しく記録した未解決の件数（同じ (種別, 対象) の更新・変更なしは数えない）。
    pub pending_created: usize,
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

/// `emit` に渡す、処理済みの一部。
#[derive(Debug, Default)]
pub(super) struct BatchChunk {
    /// 検証が済んだパス（移動・統合を含む）。
    pub settled: Vec<PathBuf>,
    /// settled のうち、移動・統合で評価等が付き替わった可能性のあるパス。
    pub refresh: Vec<PathBuf>,
    /// このバッチ（再試行の周）で、ここまでに処理したパス数（進捗表示用）。
    pub processed: usize,
}

/// パスを順に解決する。`emit` には、処理した結果を `chunk_size` 件ずつ渡す。
/// `keep_going` が false になったら、次のファイルの前で打ち切る（溜まった分は先に渡す）。
/// stat一致（`Unchanged`）と後回し（`Unstable`）は結果に含めない。
///
/// `on_relocated(新しいパス, 元のパスキー)` は、移動・リネーム、または複製（コピー）と判明したファイルごとに、
/// 検証済みとして報告する前に呼ぶ（サムネの引っ越し・複製等。完了前にサムネ生成が始まらないようにするため）。
///
/// 複製（`Copied`）と決まった新ファイルへは、ここで元のデータ（お気に入りを除く）を複製する。
/// 引き継ぎ元・移動元が決められない場合（`AmbiguousCopy`・`AmbiguousMove`）は、新ファイルを空のまま、
/// 未解決として記録する（解決はユーザーが行う）。
pub(super) fn process_batch_with(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    keep_going: &dyn Fn() -> bool,
    now: i64,
    chunk_size: usize,
    on_relocated: &dyn Fn(&Path, &str),
    emit: &mut dyn FnMut(BatchChunk),
) -> BatchOutcome {
    let mut keys = KeyMaker::default();
    let mut outcome = BatchOutcome::default();
    let mut chunk = BatchChunk::default();
    let mut last_flushed = 0;
    for (index, path) in paths.iter().enumerate() {
        if !keep_going() {
            outcome.aborted = true;
            break;
        }
        chunk.processed = index + 1;
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            continue;
        };
        let key = keys.key(dir, name);
        match file_identity::resolve_file(db, &key, path, now, false) {
            ResolveOutcome::Resolved(Resolution::Moved { from, .. }) => {
                on_relocated(path, &from);
                chunk.refresh.push(path.clone());
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Resolved(Resolution::Copied { id, source }) => {
                // 対象に既にデータが付いた等で複製できなくても、新ファイルは空のまま使える（無視してよい）。
                let _ = crate::spread_state::clone_inheritable_data(db, source, id);
                if let Some(src) = record_by_id(db, source) {
                    on_relocated(path, &src.path_key);
                }
                chunk.refresh.push(path.clone());
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Resolved(Resolution::AmbiguousCopy { id, candidates }) => {
                note_pending(db, PendingKind::CopySource, id, &candidates, now, &mut outcome);
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Resolved(Resolution::AmbiguousMove { id, candidates }) => {
                note_pending(db, PendingKind::MoveTarget, id, &candidates, now, &mut outcome);
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Resolved(Resolution::Merged { .. }) => {
                chunk.refresh.push(path.clone());
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Resolved(_) | ResolveOutcome::Failed(_) => chunk.settled.push(path.clone()),
            // 一覧に出た後で消えた。削除はせず「未確認」にする。
            ResolveOutcome::Gone => {
                file_identity::mark_unconfirmed(db, &key);
                chunk.settled.push(path.clone());
            }
            ResolveOutcome::Unstable => outcome.unstable.push(path.clone()),
            ResolveOutcome::Unchanged => {}
        }
        if chunk.settled.len() >= chunk_size || chunk.processed - last_flushed >= PROGRESS_STEP {
            last_flushed = chunk.processed;
            let processed = chunk.processed;
            emit(std::mem::take(&mut chunk));
            chunk.processed = processed;
        }
    }
    if !chunk.settled.is_empty() {
        emit(chunk);
    }
    outcome
}

fn record_by_id(db: &Arc<Mutex<Database>>, id: u64) -> Option<file_identity::FileRecord> {
    use redb::ReadableDatabase;
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    file_identity::record_by_id_tx(&tx, id)
}

/// 未解決を記録する。新しく作ったときだけ、通知の対象として数える。
fn note_pending(
    db: &Arc<Mutex<Database>>,
    kind: PendingKind,
    subject: u64,
    candidates: &[u64],
    now: i64,
    outcome: &mut BatchOutcome,
) {
    if let Some(crate::identity_pending::AddOutcome::Created(_)) =
        crate::identity_pending::add(db, kind, subject, candidates, now)
    {
        outcome.pending_created += 1;
    }
}

/// フォルダ表示時のID解決の進捗。処理が `TOAST_DELAY` を過ぎても終わらない時だけ、処理中トーストを出す。
pub(super) struct IdentityProgress {
    total: usize,
    processed: usize,
    started: Instant,
}

impl IdentityProgress {
    fn new(total: usize) -> Self {
        Self { total, processed: 0, started: Instant::now() }
    }

    /// 開始から待ち時間が過ぎたか（過ぎていれば、まだ終わっていない処理のトーストを出す）。
    fn toast_due(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= TOAST_DELAY
    }

    fn message(&self) -> String {
        crate::i18n::t().identity_toast(self.processed.min(self.total), self.total)
    }
}

/// 移動・リネームされたファイルの、サムネキャッシュの行を新しい名前（新しいフォルダ）へ引っ越す。
/// 元のDB（旧フォルダ）に生成済みのサムネがある場合だけ行い、元の行は消さない。
/// 引っ越し先のDBは、サムネが実際にある場合にだけ開く（空のDBを作らない）。
pub(super) fn transplant_thumbnail_for_move(cache_root: &Path, new_path: &Path, from_key: &str) {
    let Some((old_dir, old_name)) = from_key.split_once('\0') else { return };
    let (Some(new_dir), Some(new_name)) = (new_path.parent(), new_path.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let old_dir = Path::new(old_dir);
    let Some(src) = crate::neko_dir::open_cache_db_if_exists(
        &crate::neko_dir::neko_dir_for_root(old_dir, cache_root),
        old_dir,
    ) else {
        return;
    };
    let Some(rows) = crate::neko_dir::take_thumbnail_rows(&src, old_name) else { return };
    let Some(dst) = crate::neko_dir::open_cache_db(
        &crate::neko_dir::neko_dir_for_root(new_dir, cache_root),
        new_dir,
    ) else {
        return;
    };
    if crate::neko_dir::put_thumbnail_rows(&dst, new_name, &rows) {
        // 検索用のファイル索引も、移動先のファイルとして登録する（サムネ生成の完了時と同じ）。
        crate::neko_dir::write_file_record(
            &dst,
            new_name,
            crate::neko_dir::file_mtime(new_path),
            crate::neko_dir::file_size(new_path),
        );
    }
}

/// 引っ越し処理なしで解決する（テスト用）。
#[cfg(test)]
pub(super) fn process_batch(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    keep_going: &dyn Fn() -> bool,
    now: i64,
    chunk_size: usize,
    emit: &mut dyn FnMut(BatchChunk),
) -> BatchOutcome {
    process_batch_with(db, paths, keep_going, now, chunk_size, &|_, _| {}, emit)
}

impl NekoviewApp {
    /// 現在のフォルダの一覧（表示順）をワーカーへ投入する。スキャンとソートが済んだ直後に呼ぶ。
    /// ID記録も旧データも無いファイル（新規・移動直後）には、解決が済むまで「検証中」を出す。
    pub(super) fn start_identity_resolution(&mut self) {
        let Some(db) = self.spread_db.clone() else { return };
        let paths: Vec<PathBuf> = self.archives.clone();
        self.identity_verifying = crate::spread_state::paths_without_identity_or_legacy(&db, &paths);
        self.identity_dirty = false;
        self.identity_progress = (!paths.is_empty()).then(|| IdentityProgress::new(paths.len()));
        self.identity_worker.submit(db, paths, self.config.cache_root());
    }

    /// 処理中トーストを、進捗に合わせて出し入れする。待ち時間内、または完了後は出さない。
    fn update_identity_toast(&mut self) {
        let desired = match &self.identity_progress {
            Some(p) if p.toast_due(Instant::now()) => Some(p.message()),
            Some(p) => {
                // 待ち時間が明けるフレームを確実に作る（入力が無くても再描画する）。
                let remaining = TOAST_DELAY.saturating_sub(Instant::now().duration_since(p.started));
                self.egui_ctx.request_repaint_after(remaining);
                None
            }
            None => None,
        };
        if desired != self.identity_toast_msg {
            self.set_sticky_toast(desired.clone());
            self.identity_toast_msg = desired;
        }
    }

    /// ワーカーの結果を受けて、付き替わったパスの評価・設定表示を引き直し、検証中表示を外す。
    /// 解決が全て終わった時、付き替わりがあれば一度だけ並び・フィルタを作り直す。毎フレーム呼ぶ。
    pub(super) fn poll_identity_results(&mut self) {
        // 起動後の最初のフレームで、旧レコードのバックフィルを依頼する（ワーカー側でさらに数秒待つ）。
        if !self.identity_backfill_started {
            if let Some(db) = self.spread_db.clone() {
                self.identity_pending_count = crate::identity_pending::count(&db);
                self.identity_worker.submit_backfill(db);
                self.identity_backfill_started = true;
            }
        }
        let events: Vec<IdentityEvent> = std::iter::from_fn(|| self.identity_worker.event_rx.try_recv().ok()).collect();
        for event in events {
            match event {
                IdentityEvent::Chunk { generation, refresh, settled, processed } => {
                    if !self.identity_worker.is_current(generation) {
                        continue;
                    }
                    if let Some(p) = &mut self.identity_progress {
                        p.processed = p.processed.max(processed);
                    }
                    for p in &settled {
                        self.identity_verifying.remove(p);
                    }
                    let mut reload_maps = false;
                    for p in &refresh {
                        // 表示中の一覧に残っているものだけ（スキャンで入れ替わった後は無視する）。
                        if self.archive_rating_cache.contains_key(p) {
                            self.refresh_rating_cache(p);
                            self.refresh_saved_archive_settings(p);
                            self.identity_dirty = true;
                            reload_maps |= p.parent().is_some_and(|d| d == self.current_dir);
                        }
                    }
                    // 見開き・ソート・お気に入りの一覧（★の表示、ビューアーを開く時の復元に使う）も
                    // 付き替わるので、現在のフォルダぶんを読み直す。
                    if reload_maps && self.viewing_favorites.is_none() {
                        self.reload_dir_state_maps();
                    }
                    self.egui_ctx.request_repaint();
                }
                IdentityEvent::Done { generation, pending_created, pending_count } => {
                    if !self.identity_worker.is_current(generation) {
                        continue;
                    }
                    // 未解決の総数（掃除後）を反映し、新しく増えた時だけ通知する。
                    self.identity_pending_count = pending_count;
                    if pending_created > 0 {
                        self.set_toast(crate::i18n::t().identity_pending_toast(pending_created));
                    }
                    // 後回しのまま残った・失敗したものも含め、検証中表示と処理中トーストを残さない。
                    self.identity_verifying.clear();
                    self.identity_progress = None;
                    if std::mem::take(&mut self.identity_dirty) {
                        // 評価以外（お気に入りの先頭固定など）の保存状態も付き替わるので、並び順の軸に
                        // 関わらず、並びとフィルタを一度だけ作り直す。
                        self.resort_keeping_selection_always();
                    }
                    self.egui_ctx.request_repaint();
                }
            }
        }
        self.update_identity_toast();
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

    fn run(db: &Arc<Mutex<Database>>, paths: &[PathBuf]) -> (BatchOutcome, Vec<BatchChunk>) {
        let mut emitted = Vec::new();
        let outcome = process_batch(
            db,
            paths,
            &|| true,
            file_identity::now_unix(),
            CHUNK_SIZE,
            &mut |c| emitted.push(c),
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
        assert!(emitted.iter().all(|c| c.refresh.is_empty()), "新規作成だけでは引き直し不要");
        let settled: Vec<PathBuf> = emitted.iter().flat_map(|c| c.settled.clone()).collect();
        assert_eq!(settled, paths, "検証が済んだパスとして報告される（検証中表示を外す）");
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
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].refresh, vec![p2.clone()]);
        assert_eq!(emitted[0].settled, vec![p2]);
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
        process_batch(&db, &moved, &|| true, file_identity::now_unix(), 2, &mut |c| emitted.push(c.refresh.len()));
        assert_eq!(emitted, vec![2, 2, 1]);
    }

    #[test]
    fn worker_thread_processes_a_job_and_reports_done() {
        let t = TempRoot::new("thread");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let p = file(&dir, "a.zip", 3000, 1000);
        let mut worker = IdentityWorker::spawn(egui::Context::default());
        let generation = worker.submit(db.clone(), vec![p.clone()], None);
        assert!(worker.is_current(generation));
        // 検証済みの Chunk の後に Done が届く。
        let mut settled = Vec::new();
        loop {
            match worker.event_rx.recv_timeout(Duration::from_secs(10)).expect("Done が届く") {
                IdentityEvent::Chunk { settled: s, .. } => settled.extend(s),
                IdentityEvent::Done { generation: g, .. } => {
                    assert_eq!(g, generation);
                    break;
                }
            }
        }
        assert_eq!(settled, vec![p.clone()]);
        assert!(rec_of(&db, &p).is_some());
        // 新しい投入で世代が進み、古い世代は現行でなくなる。
        let generation2 = worker.submit(db, vec![], None);
        assert!(generation2 > generation && !worker.is_current(generation));
        worker.shutdown();
        assert!(!worker.is_current(generation2));
    }

    #[test]
    fn settled_chunks_are_cut_by_chunk_size_and_exclude_unchanged() {
        let t = TempRoot::new("settled");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let paths: Vec<PathBuf> = (0..5).map(|i| file(&dir, &format!("{i}.zip"), 2000 + i, 1000)).collect();
        let mut sizes = Vec::new();
        process_batch(&db, &paths, &|| true, file_identity::now_unix(), 2, &mut |c| sizes.push(c.settled.len()));
        assert_eq!(sizes, vec![2, 2, 1]);
        // 2回目はstat一致なので、何も報告しない。
        let mut again = 0;
        process_batch(&db, &paths, &|| true, file_identity::now_unix(), 2, &mut |_| again += 1);
        assert_eq!(again, 0);
    }

    #[test]
    fn backfill_records_legacy_data_files_that_exist_and_skips_missing_ones() {
        let t = TempRoot::new("backfill");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let exists = file(&dir, "has_data.zip", 3000, 1000);
        let _no_data = file(&dir, "plain.zip", 3000, 1000);
        // 旧v1の評価（実在するファイルと、既に移動・削除済みのファイル）。
        for name in ["has_data.zip", "ghost.zip"] {
            let key = crate::spread_state::make_key(&dir, name);
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            {
                let mut tb = tx.open_table(crate::spread_state::ARCHIVE_RATING_TABLE_V1).unwrap();
                tb.insert(key.as_str(), (6u8, 1u32, 5i64)).unwrap();
            }
            tx.commit().unwrap();
        }
        let mut bf = Backfill::new(db.clone(), Duration::ZERO);
        while bf.step() {}
        let rec = rec_of(&db, &exists).expect("データを持つ旧レコードはID記録される");
        assert!(rec.fp.is_some());
        assert!(rec_of(&db, &dir.join("plain.zip")).is_none(), "データ無しは対象外（フォルダ表示時に処理）");
        assert!(rec_of(&db, &dir.join("ghost.zip")).is_none(), "実体が無いものは何もしない");
        // 評価は旧v1のまま読める（IDが付いても値は変わらない）。
        assert_eq!(read_archive_rating(&db, &dir, "has_data.zip").unwrap().rating_half, 6);
        // 2回目の列挙では対象が残らない。
        assert!(crate::spread_state::legacy_data_keys_without_id(&db)
            .iter()
            .all(|k| k.ends_with("ghost.zip")));
    }

    #[test]
    fn backfill_also_covers_files_that_only_have_spread_sort_or_thumbnail_settings() {
        let t = TempRoot::new("backfill_settings");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let spread_only = file(&dir, "spread.zip", 3000, 1000);
        let sort_only = file(&dir, "sort.zip", 3100, 1000);
        let plain = file(&dir, "plain.zip", 3200, 1000);
        {
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            {
                let mut a = tx.open_table(crate::spread_state::SPREAD_TABLE).unwrap();
                let k = crate::spread_state::make_key(&dir, "spread.zip");
                a.insert(k.as_str(), (1u8, 0i32)).unwrap();
                let mut b = tx.open_table(crate::spread_state::ARCHIVE_SORT_TABLE_V1).unwrap();
                let k = crate::spread_state::make_key(&dir, "sort.zip");
                b.insert(k.as_str(), (1u8, true)).unwrap();
            }
            tx.commit().unwrap();
        }
        let mut bf = Backfill::new(db.clone(), Duration::ZERO);
        while bf.step() {}
        assert!(rec_of(&db, &spread_only).is_some());
        assert!(rec_of(&db, &sort_only).is_some());
        assert!(rec_of(&db, &plain).is_none());
        // 設定は旧v1のまま読める。
        assert!(crate::spread_state::read_spread(&db, &dir, "spread.zip").is_some());
        // 「検証中」の対象判定でも、旧データありとして除外される。
        let set = crate::spread_state::paths_without_identity_or_legacy(&db, &[plain.clone()]);
        assert_eq!(set, std::iter::once(plain).collect());
    }

    #[test]
    fn backfill_waits_for_its_start_delay() {
        let t = TempRoot::new("backfill_delay");
        let db = open_spread_db(&t.0).unwrap();
        let mut bf = Backfill::new(db, Duration::from_secs(3600));
        assert!(bf.step(), "待機中は何もせず、まだ続く");
        assert!(bf.keys.is_none(), "開始前は列挙もしない");
    }

    #[test]
    fn verifying_set_excludes_files_with_identity_or_legacy_data() {
        let t = TempRoot::new("verifying");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let with_id = file(&dir, "id.zip", 3000, 1000);
        let with_legacy = file(&dir, "legacy.zip", 3000, 1000);
        let brand_new = file(&dir, "new.zip", 3000, 1000);
        run(&db, &[with_id.clone()]);
        {
            let key = crate::spread_state::make_key(&dir, "legacy.zip");
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            {
                let mut tb = tx.open_table(crate::spread_state::ARCHIVE_RATING_TABLE_V1).unwrap();
                tb.insert(key.as_str(), (3u8, 0u32, 0i64)).unwrap();
            }
            tx.commit().unwrap();
        }
        let set = crate::spread_state::paths_without_identity_or_legacy(
            &db,
            &[with_id, with_legacy, brand_new.clone()],
        );
        assert_eq!(set, std::iter::once(brand_new).collect());
    }

    #[test]
    fn progress_is_reported_every_step_even_when_nothing_is_settled() {
        let t = TempRoot::new("progress");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        // 全てstat一致になるよう、先に記録しておく。
        let paths: Vec<PathBuf> = (0..250).map(|i| file(&dir, &format!("{i}.zip"), 1000 + i, 1000)).collect();
        run(&db, &paths);
        let mut processed = Vec::new();
        let mut settled = 0;
        process_batch(&db, &paths, &|| true, file_identity::now_unix(), CHUNK_SIZE, &mut |c| {
            processed.push(c.processed);
            settled += c.settled.len();
        });
        // 検証済みは0件でも、100件ごとに処理済み件数が届く（最後の端数は届かなくてよい＝Doneで消える）。
        assert_eq!(processed, vec![100, 200]);
        assert_eq!(settled, 0);
    }

    #[test]
    fn processed_count_accompanies_settled_chunks() {
        let t = TempRoot::new("processed");
        let db = open_spread_db(&t.0).unwrap();
        let dir = t.dir("d");
        let paths: Vec<PathBuf> = (0..5).map(|i| file(&dir, &format!("{i}.zip"), 2000 + i, 1000)).collect();
        let mut seen = Vec::new();
        process_batch(&db, &paths, &|| true, file_identity::now_unix(), 2, &mut |c| {
            seen.push((c.settled.len(), c.processed));
        });
        assert_eq!(seen, vec![(2, 2), (2, 4), (1, 5)]);
    }

    #[test]
    fn toast_is_due_only_after_the_initial_wait() {
        let p = IdentityProgress::new(10);
        assert!(!p.toast_due(Instant::now()), "待ち時間内は出さない（速く終わる処理では出ない）");
        let later = p.started + TOAST_DELAY;
        assert!(p.toast_due(later), "待ち時間を過ぎても終わらなければ出す");
        assert!(!p.toast_due(p.started + TOAST_DELAY - Duration::from_millis(1)));
    }

    #[test]
    fn toast_message_never_exceeds_total() {
        let mut p = IdentityProgress::new(5);
        p.processed = 9;
        assert!(p.message().contains("5 / 5"), "{}", p.message());
        p.processed = 2;
        assert!(p.message().contains("2 / 5"));
    }

    #[test]
    fn dir_state_lists_include_a_moved_file_after_resolution() {
        let t = TempRoot::new("dir_lists");
        let db = open_spread_db(&t.0).unwrap();
        crate::favorites::init_favorite_tables(&db).unwrap();
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let p1 = file(&d1, "a.zip", 5000, 1000);
        crate::favorites::set_membership(&db, &d1, "a.zip", &[3]);
        assert!(crate::spread_state::write_spread(&db, &d1, "a.zip", crate::types::PageMode::SpreadLeft, 0));
        assert!(crate::spread_state::write_archive_sort(&db, &d1, "a.zip", crate::types::ReaderSortKey::Date, true));
        let p2 = d2.join("renamed.zip");
        std::fs::rename(&p1, &p2).unwrap();
        // 解決前は、移動先の一覧に出ない（読み直しても空）。
        assert!(crate::favorites::list_dir_favorites(&db, &d2).is_empty());
        run(&db, &[p2]);
        // 解決後に読み直した一覧（UI側の reload_dir_state_maps と同じ読み出し）に載る。
        assert_eq!(crate::favorites::list_dir_favorites(&db, &d2), vec![("renamed.zip".to_owned(), vec![3])]);
        assert_eq!(crate::spread_state::list_dir_entries(&db, &d2).len(), 1);
        assert_eq!(crate::spread_state::list_dir_archive_sorts(&db, &d2).len(), 1);
    }

    // ---- サムネの引っ越し（Phase 4）----

    fn cache_db_for(root: &Path, dir: &Path) -> Arc<Mutex<Database>> {
        crate::neko_dir::open_cache_db(&crate::neko_dir::neko_dir_for_root(dir, root), dir).unwrap()
    }

    #[test]
    fn thumbnail_follows_a_rename_within_the_same_folder() {
        let t = TempRoot::new("thumb_same");
        let cache_root = t.0.join("cache");
        let dir = t.dir("d");
        let renamed = file(&dir, "b.zip", 4000, 1000);
        let db = cache_db_for(&cache_root, &dir);
        crate::neko_dir::test_seed_current(&db, "a.zip", 100, b"jpeg", "full");
        let from_key = crate::spread_state::make_key(&dir, "a.zip");

        transplant_thumbnail_for_move(&cache_root, &renamed, &from_key);

        assert_eq!(crate::neko_dir::read_thumb_unchecked(&db, "b.zip"), Some((100, b"jpeg".to_vec())));
        assert_eq!(
            crate::neko_dir::read_thumbnail_state(&db, "b.zip").unwrap().status,
            crate::neko_dir::ThumbnailStatus::Current
        );
        // 検索用のファイル索引も、移動先のファイルとして載る。
        assert!(crate::neko_dir::search_files(&db, |n| n == "b.zip", None, None, None, None).contains(&"b.zip".to_owned()));
        // 元の行は残る（非破壊）。
        assert!(crate::neko_dir::read_thumb_unchecked(&db, "a.zip").is_some());
    }

    #[test]
    fn thumbnail_follows_a_move_to_another_folder() {
        let t = TempRoot::new("thumb_cross");
        let cache_root = t.0.join("cache");
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let moved = file(&d2, "b.zip", 4000, 1000);
        let src = cache_db_for(&cache_root, &d1);
        crate::neko_dir::test_seed_current(&src, "a.zip", 77, b"blob", "full");
        let from_key = crate::spread_state::make_key(&d1, "a.zip");

        transplant_thumbnail_for_move(&cache_root, &moved, &from_key);

        let dst = cache_db_for(&cache_root, &d2);
        assert_eq!(crate::neko_dir::read_thumb_unchecked(&dst, "b.zip"), Some((77, b"blob".to_vec())));
    }

    #[test]
    fn no_cache_database_is_created_when_there_is_nothing_to_transplant() {
        let t = TempRoot::new("thumb_none");
        let cache_root = t.0.join("cache");
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let moved = file(&d2, "b.zip", 4000, 1000);
        let from_key = crate::spread_state::make_key(&d1, "a.zip");
        // 元のDBが無い／サムネが無い場合は、移動先のDB（ディレクトリ）を作らない。
        transplant_thumbnail_for_move(&cache_root, &moved, &from_key);
        assert!(!crate::neko_dir::neko_dir_for_root(&d2, &cache_root).exists());
        let src = cache_db_for(&cache_root, &d1);
        assert!(crate::neko_dir::read_thumbnail_state(&src, "a.zip").is_none());
        transplant_thumbnail_for_move(&cache_root, &moved, &from_key);
        assert!(!crate::neko_dir::neko_dir_for_root(&d2, &cache_root).exists());
    }

    #[test]
    fn moved_files_are_transplanted_before_they_are_reported_as_settled() {
        let t = TempRoot::new("thumb_order");
        let db = open_spread_db(&t.0).unwrap();
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let p1 = file(&d1, "a.zip", 5000, 1000);
        run(&db, &[p1.clone()]);
        let p2 = d2.join("b.zip");
        std::fs::rename(&p1, &p2).unwrap();

        let log = std::cell::RefCell::new(Vec::<String>::new());
        process_batch_with(
            &db,
            &[p2.clone()],
            &|| true,
            file_identity::now_unix(),
            CHUNK_SIZE,
            &|path, from| log.borrow_mut().push(format!("moved {} from {}", path.display(), from.contains("a.zip"))),
            &mut |c| log.borrow_mut().push(format!("settled {}", c.settled.len())),
        );
        let log = log.into_inner();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[0].starts_with("moved") && log[0].ends_with("true"), "{log:?}");
        assert_eq!(log[1], "settled 1", "引っ越しが済んでから検証済みとして報告する");
    }

    #[test]
    fn worker_transplants_thumbnails_end_to_end() {
        let t = TempRoot::new("thumb_e2e");
        let db = open_spread_db(&t.0).unwrap();
        let cache_root = t.0.join("cache");
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let p1 = file(&d1, "a.zip", 6000, 1000);
        let mut worker = IdentityWorker::spawn(egui::Context::default());
        // 1回目: 元の場所を記録（IDが付く）。
        let g1 = worker.submit(db.clone(), vec![p1.clone()], Some(cache_root.clone()));
        wait_done(&worker, g1);
        let src = cache_db_for(&cache_root, &d1);
        crate::neko_dir::test_seed_current(&src, "a.zip", 123, b"thumb", "full");
        // 移動して、移動先を解決させる。
        let p2 = d2.join("moved.zip");
        std::fs::rename(&p1, &p2).unwrap();
        let g2 = worker.submit(db, vec![p2], Some(cache_root.clone()));
        wait_done(&worker, g2);
        let dst = cache_db_for(&cache_root, &d2);
        assert_eq!(crate::neko_dir::read_thumb_unchecked(&dst, "moved.zip"), Some((123, b"thumb".to_vec())));
        worker.shutdown();
    }

    fn wait_done(worker: &IdentityWorker, generation: u64) {
        loop {
            match worker.event_rx.recv_timeout(Duration::from_secs(10)).expect("Done が届く") {
                IdentityEvent::Done { generation: g, .. } if g == generation => return,
                _ => {}
            }
        }
    }

    // ---- 複製・曖昧の結線（R4）----

    /// 同じ内容（同じ長さ）の古いファイル。`file()` は長さで内容が決まる。
    fn twin(dir: &Path, name: &str) -> PathBuf {
        file(dir, name, 4000, 1000)
    }

    fn new_env(t: &TempRoot) -> Arc<Mutex<Database>> {
        let db = open_spread_db(&t.0).unwrap();
        crate::favorites::init_favorite_tables(&db).unwrap();
        db
    }

    #[test]
    fn a_copy_of_a_file_with_data_inherits_it_except_favorite_and_visits() {
        let t = TempRoot::new("r4_copy");
        let db = new_env(&t);
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let src = twin(&d1, "a.zip");
        run(&db, &[src.clone()]);
        assert!(write_archive_rating(&db, &d1, "a.zip", 8));
        assert!(crate::spread_state::record_archive_visit(&db, &d1, "a.zip"));
        assert!(crate::spread_state::write_archive_tags(&db, &d1, "a.zip", &[4, 2]));
        crate::favorites::set_membership(&db, &d1, "a.zip", &[3]);

        let copy = twin(&d2, "a_copy.zip");
        let (outcome, chunks) = run(&db, &[copy.clone()]);

        assert_eq!(outcome.pending_created, 0);
        assert!(chunks.iter().any(|c| c.refresh.contains(&copy)), "データが付いたので引き直し対象");
        let r = read_archive_rating(&db, &d2, "a_copy.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count), (8, 0));
        assert_eq!(crate::spread_state::read_archive_tags(&db, &d2, "a_copy.zip"), vec![4, 2]);
        assert_eq!(crate::favorites::get_membership(&db, &d2, "a_copy.zip"), None, "お気に入りは複製しない");
        assert_eq!(crate::favorites::get_membership(&db, &d1, "a.zip"), Some(vec![3]));
        assert_eq!(crate::identity_pending::count(&db), 0);
    }

    #[test]
    fn ambiguous_copy_sources_are_recorded_as_pending_and_leave_the_copy_blank() {
        let t = TempRoot::new("r4_ambiguous_copy");
        let db = new_env(&t);
        let (d1, d2, d3) = (t.dir("d1"), t.dir("d2"), t.dir("d3"));
        let a = twin(&d1, "a.zip");
        run(&db, &[a.clone()]);
        assert!(write_archive_rating(&db, &d1, "a.zip", 8));
        let b = twin(&d2, "b.zip");
        run(&db, &[b.clone()]); // aのコピーとして複製される
        assert!(write_archive_rating(&db, &d2, "b.zip", 2)); // 後から別の評価にする → データが食い違う

        let c = twin(&d3, "c.zip");
        let (outcome, _) = run(&db, &[c.clone()]);
        assert_eq!(outcome.pending_created, 1);
        assert_eq!(read_archive_rating(&db, &d3, "c.zip"), None, "空のまま");
        let list = crate::identity_pending::list(&db);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, PendingKind::CopySource);
        assert_eq!(list[0].candidates.len(), 2);
        // 同じファイルを再び処理しても、新しい未解決は増えない（stat一致で何もしない）。
        let (again, _) = run(&db, &[c]);
        assert_eq!(again.pending_created, 0);
        assert_eq!(crate::identity_pending::count(&db), 1);
    }

    #[test]
    fn ambiguous_move_sources_are_recorded_as_pending_without_assigning() {
        let t = TempRoot::new("r4_ambiguous_move");
        let db = new_env(&t);
        let (d1, d2, d3) = (t.dir("d1"), t.dir("d2"), t.dir("d3"));
        let a = twin(&d1, "a.zip");
        run(&db, &[a.clone()]);
        assert!(write_archive_rating(&db, &d1, "a.zip", 8));
        let b = twin(&d2, "b.zip");
        run(&db, &[b.clone()]);
        assert!(write_archive_rating(&db, &d2, "b.zip", 2));
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();

        let c = twin(&d3, "c.zip");
        let (outcome, _) = run(&db, &[c.clone()]);
        assert_eq!(outcome.pending_created, 1);
        let list = crate::identity_pending::list(&db);
        assert_eq!(list[0].kind, PendingKind::MoveTarget);
        assert_eq!(read_archive_rating(&db, &d3, "c.zip"), None, "自動割当はしない");
        assert_eq!(read_archive_rating(&db, &d1, "a.zip").unwrap().rating_half, 8, "元の記録は無傷");
    }

    #[test]
    fn copy_hook_is_called_with_the_source_key_before_the_copy_is_settled() {
        let t = TempRoot::new("r4_hook");
        let db = new_env(&t);
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let a = twin(&d1, "a.zip");
        run(&db, &[a.clone()]);
        assert!(write_archive_rating(&db, &d1, "a.zip", 6));
        let copy = twin(&d2, "copy.zip");
        let log = std::cell::RefCell::new(Vec::<String>::new());
        process_batch_with(
            &db,
            &[copy.clone()],
            &|| true,
            file_identity::now_unix(),
            CHUNK_SIZE,
            &|p, from| log.borrow_mut().push(format!("hook {} {}", p.file_name().unwrap().to_string_lossy(), from.ends_with("a.zip"))),
            &mut |c| log.borrow_mut().push(format!("settled {}", c.settled.len())),
        );
        assert_eq!(log.into_inner(), vec!["hook copy.zip true".to_owned(), "settled 1".to_owned()]);
    }

    #[test]
    fn thumbnail_is_copied_to_a_copy_and_the_original_keeps_its_own() {
        let t = TempRoot::new("r4_thumb_copy");
        let db = new_env(&t);
        let cache_root = t.0.join("cache");
        let (d1, d2) = (t.dir("d1"), t.dir("d2"));
        let a = twin(&d1, "a.zip");
        run(&db, &[a.clone()]);
        assert!(write_archive_rating(&db, &d1, "a.zip", 6));
        let src_cache = cache_db_for(&cache_root, &d1);
        crate::neko_dir::test_seed_current(&src_cache, "a.zip", 321, b"thumb", "full");
        let copy = twin(&d2, "copy.zip");
        process_batch_with(
            &db,
            &[copy.clone()],
            &|| true,
            file_identity::now_unix(),
            CHUNK_SIZE,
            &|p, from| transplant_thumbnail_for_move(&cache_root, p, from),
            &mut |_| {},
        );
        let dst_cache = cache_db_for(&cache_root, &d2);
        assert_eq!(crate::neko_dir::read_thumb_unchecked(&dst_cache, "copy.zip"), Some((321, b"thumb".to_vec())));
        assert!(crate::neko_dir::read_thumb_unchecked(&src_cache, "a.zip").is_some());
    }

    #[test]
    fn worker_reports_pending_counts_and_prunes_stale_records_on_done() {
        let t = TempRoot::new("r4_worker");
        let db = new_env(&t);
        let (d1, d2, d3) = (t.dir("d1"), t.dir("d2"), t.dir("d3"));
        let a = twin(&d1, "a.zip");
        let b = twin(&d2, "b.zip");
        let mut worker = IdentityWorker::spawn(egui::Context::default());
        let g = worker.submit(db.clone(), vec![a, b], None);
        wait_done(&worker, g);
        assert!(write_archive_rating(&db, &d1, "a.zip", 8));
        assert!(write_archive_rating(&db, &d2, "b.zip", 2));
        // 意味を失った未解決（対象のIDが無い）を、先に1件入れておく。
        crate::identity_pending::add(&db, PendingKind::CopySource, 987654, &[1], 1).unwrap();
        let c = twin(&d3, "c.zip");
        let g2 = worker.submit(db.clone(), vec![c], None);
        let (created, count) = loop {
            match worker.event_rx.recv_timeout(Duration::from_secs(10)).expect("Done が届く") {
                IdentityEvent::Done { generation, pending_created, pending_count } if generation == g2 => {
                    break (pending_created, pending_count)
                }
                _ => {}
            }
        };
        assert_eq!(created, 1);
        assert_eq!(count, 1, "意味を失った未解決は掃除され、実際の1件だけが残る");
        worker.shutdown();
    }
}
