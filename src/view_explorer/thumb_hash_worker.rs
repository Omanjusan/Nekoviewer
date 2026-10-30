//! サムネのdHash計算の専用スレッド。フォルダのファイルについて、ハッシュが無い・古いものを計算して共有DBへ保存する。
//!
//! - JPEGの読み出しとデコードがあるので、マーカー確認のスレッド（`marker_probe`）とは分ける
//! - 依頼は「フォルダ1つぶん」。ID解決が済んだ時と、サムネ生成が一段落した時に出す（後から生成された分を拾う）
//! - 世代: フォルダを読み込み直すたびに進める。古い依頼は次のファイルで打ち切る
//! - 保存の中身と無効化の規則は [`crate::thumb_hash_store`]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use redb::Database;

struct Job {
    generation: u64,
    spread_db: Arc<Mutex<Database>>,
    cache_db: Arc<Mutex<Database>>,
    dir: std::path::PathBuf,
    names: Vec<String>,
}

pub(super) struct ThumbHashWorker {
    job_tx: Option<mpsc::Sender<Job>>,
    current: Arc<AtomicU64>,
}

impl ThumbHashWorker {
    pub(super) fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let current = Arc::new(AtomicU64::new(0));
        let worker_current = Arc::clone(&current);
        std::thread::Builder::new()
            .name("thumb-hash".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    // 同じフォルダの依頼が溜まっていたら、最新の1件だけ処理する（名前の一覧は後のものが新しい）。
                    let mut job = job;
                    while let Ok(next) = job_rx.try_recv() {
                        job = next;
                    }
                    let keep_going = || worker_current.load(Ordering::Acquire) == job.generation;
                    crate::thumb_hash_store::hash_files(
                        &job.spread_db,
                        &job.dir,
                        &job.names,
                        &|name| crate::neko_dir::read_thumb_unchecked(&job.cache_db, name),
                        &keep_going,
                    );
                }
            })
            .expect("spawn thumb-hash");
        Self { job_tx: Some(job_tx), current }
    }

    /// 世代を進める（フォルダを読み込み直す時）。処理中の古い依頼は次のファイルで打ち切られる。
    pub(super) fn invalidate(&self) {
        self.current.fetch_add(1, Ordering::AcqRel);
    }

    /// 今の世代で、フォルダ1つぶんの計算を依頼する。
    pub(super) fn submit(
        &self,
        spread_db: Arc<Mutex<Database>>,
        cache_db: Arc<Mutex<Database>>,
        dir: std::path::PathBuf,
        names: Vec<String>,
    ) {
        let generation = self.current.load(Ordering::Acquire);
        if let Some(tx) = &self.job_tx {
            let _ = tx.send(Job { generation, spread_db, cache_db, dir, names });
        }
    }

    /// 終了時に呼ぶ。処理中の依頼は打ち切られ、以後の依頼は受け付けない。
    pub(super) fn shutdown(&mut self) {
        self.invalidate();
        self.job_tx = None;
    }
}

impl super::NekoviewApp {
    /// 表示中のフォルダのサムネについて、ハッシュの計算を依頼する。
    /// 通常のフォルダ表示だけ。サムネ未生成のファイルは飛ばされるので、生成が一段落した時にも呼ぶ。
    pub(super) fn request_thumb_hash(&mut self) {
        if !self.marker_view_active() {
            return;
        }
        let (Some(spread_db), Some(cache_db)) = (self.spread_db.clone(), self.cache_db.clone()) else { return };
        let names: Vec<String> = self
            .archives
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .collect();
        if !names.is_empty() {
            self.thumb_hash.submit(spread_db, cache_db, self.current_dir.clone(), names);
        }
    }
}
