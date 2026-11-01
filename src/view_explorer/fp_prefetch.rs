//! ID解決のFP読み（`file_identity::observe`）の先読み。複数スレッドでファイルを先に観測しておき、
//! 解決（DBへの書き込みと判定）は呼び出し側が従来どおり一覧の順に1件ずつ行う。
//!
//! - 目的: 初回登録でFPの窓読み（ファイル1つにつき5か所のシーク、コールドで50〜70ms）が直列に並ぶのを、
//!   I/Oの待ちを重ねて短くする。解決の順序と判定は変えない
//! - 先読みはstat一致（変更なし）のファイルを読まない（再訪のたびに全件のFPを読み直さないため）
//! - 先読みの先頭は、消費位置から `ahead` 件までに制限する（中断時の無駄読みと、結果の滞留を抑える）
//! - スレッド数は環境変数 `NEKO_FP_THREADS`（既定1＝先読みなし＝従来の動作）。試作中は実験用に残す

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use redb::Database;

use crate::file_identity::{self, Observed};

/// 環境変数で上書きできるスレッド数の上限。
const MAX_THREADS: usize = 16;
/// 消費位置からどこまで先読みするか（スレッド1本あたり）。
const AHEAD_PER_THREAD: usize = 8;
/// 待ちの間に中断（`keep_going`）を確認する間隔。
const POLL: Duration = Duration::from_millis(50);

/// 環境変数 `NEKO_FP_THREADS` から先読みのスレッド数を読む。未設定・不正は1。
pub(super) fn threads_from_env() -> usize {
    std::env::var("NEKO_FP_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, MAX_THREADS)
}

/// 先読みの結果。
pub(super) enum Fetched {
    /// 読む必要が無い（stat一致、またはファイルが取れない）。呼び出し側が従来どおり処理する。
    Skip,
    Observed(Observed),
}

pub(super) struct Prefetcher {
    slots: Vec<Mutex<Option<Fetched>>>,
    ready: (Mutex<()>, Condvar),
    /// 消費済みの件数（これ + ahead までを先読みしてよい）。
    consumed: AtomicUsize,
    next: AtomicUsize,
    active: AtomicUsize,
    stop: AtomicBool,
    ahead: usize,
}

impl Prefetcher {
    /// 先読みを待って受け取る。中断された・先読みが尽きた場合は `None`（呼び出し側が自分で観測する）。
    pub(super) fn take(&self, index: usize, keep_going: &dyn Fn() -> bool) -> Option<Fetched> {
        let started = std::time::Instant::now();
        let slot = self.slots.get(index)?;
        let result = loop {
            if let Some(f) = slot.lock().ok()?.take() {
                break Some(f);
            }
            // 全ワーカーが終わっているのに未取得なら、待っても来ない。
            if self.active.load(Ordering::Acquire) == 0 || !keep_going() {
                break None;
            }
            let guard = self.ready.0.lock().ok()?;
            let _ = self.ready.1.wait_timeout(guard, POLL);
        };
        crate::perf_fixture::detail(crate::perf_fixture::Stage::IdResolve, "先読み待ち(解決スレッド)", started.elapsed());
        self.consumed.store(index + 1, Ordering::Release);
        self.ready.1.notify_all();
        result
    }
}

fn fetch(db: &Arc<Mutex<Database>>, key: &str, path: &std::path::Path, now: i64) -> Fetched {
    let Ok(meta) = std::fs::metadata(path) else { return Fetched::Skip };
    if !meta.is_file() {
        return Fetched::Skip;
    }
    let (size, mtime) = (meta.len(), file_identity::mtime_of(&meta));
    if file_identity::lookup(db, key).is_some_and(|r| file_identity::quick_hit(&r, size, mtime)) {
        return Fetched::Skip;
    }
    Fetched::Observed(file_identity::observe(path, now))
}

/// `threads` 本のスレッドで `paths` の観測を先読みしながら `f` を実行する。`f` が返ったら先読みを止めて待ち合わせる。
/// `keys[i]` は `paths[i]` のパスキー（作れないものは `None`＝先読みしない）。
pub(super) fn run<R>(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    keys: &[Option<String>],
    threads: usize,
    now: i64,
    f: impl FnOnce(&Prefetcher) -> R,
) -> R {
    let pf = Prefetcher {
        slots: (0..paths.len()).map(|_| Mutex::new(None)).collect(),
        ready: (Mutex::new(()), Condvar::new()),
        consumed: AtomicUsize::new(0),
        next: AtomicUsize::new(0),
        active: AtomicUsize::new(threads),
        stop: AtomicBool::new(false),
        ahead: threads * AHEAD_PER_THREAD,
    };
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let i = pf.next.fetch_add(1, Ordering::AcqRel);
                    if i >= paths.len() {
                        break;
                    }
                    // 先読みの範囲を超えないよう、消費が進むまで待つ。
                    while !pf.stop.load(Ordering::Acquire) && i >= pf.consumed.load(Ordering::Acquire) + pf.ahead {
                        if let Ok(guard) = pf.ready.0.lock() {
                            let _ = pf.ready.1.wait_timeout(guard, POLL);
                        }
                    }
                    if pf.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let fetched = match &keys[i] {
                        Some(key) => fetch(db, key, &paths[i], now),
                        None => Fetched::Skip,
                    };
                    if let Ok(mut slot) = pf.slots[i].lock() {
                        *slot = Some(fetched);
                    }
                    pf.ready.1.notify_all();
                }
                pf.active.fetch_sub(1, Ordering::AcqRel);
                pf.ready.1.notify_all();
            });
        }
        let result = f(&pf);
        pf.stop.store(true, Ordering::Release);
        pf.ready.1.notify_all();
        result
    })
}
