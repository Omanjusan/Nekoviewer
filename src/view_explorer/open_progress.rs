//! エクスプローラーからのアーカイブオープンを非同期化するための状態管理。
//! `archive::list_images_with_progress`をワーカースレッドで実行し、
//! 進捗（件数 or 不確定）をポーリング可能な形で保持する。キャンセルにも対応する。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::fs::archive::{self, ArchiveOpenProgress, ImageEntry};

/// ポーリング結果。オープン完了までは`Pending`。
pub enum OpenPollResult {
    /// まだ読み込み中。
    Pending,
    /// 読み込み完了。有効な画像アーカイブだった。
    Ready(Vec<ImageEntry>),
    /// 読み込み完了したが画像が1件も無かった（無効アーカイブ扱い）。
    Empty,
    /// ユーザーがキャンセルした。
    Cancelled,
}

/// エクスプローラーからダブルクリックされたアーカイブの非同期オープン処理。
pub struct PendingOpen {
    pub path: PathBuf,
    progress: Arc<Mutex<ArchiveOpenProgress>>,
    cancel: Arc<AtomicBool>,
    result_rx: mpsc::Receiver<Option<Vec<ImageEntry>>>,
    cancelled_by_user: bool,
}

impl PendingOpen {
    /// ワーカースレッドを起動し、非同期でアーカイブの一覧取得を開始する。
    pub fn spawn(path: PathBuf) -> Self {
        let progress = Arc::new(Mutex::new(ArchiveOpenProgress::Indeterminate));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let worker_path = path.clone();
        let worker_progress = Arc::clone(&progress);
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let mut on_progress = |p: ArchiveOpenProgress| -> bool {
                *worker_progress.lock().unwrap() = p;
                !worker_cancel.load(Ordering::Relaxed)
            };
            let result = archive::list_images_with_progress(&worker_path, &mut on_progress);
            // 受信側が既に破棄されていても（キャンセル後の取りこぼし）エラーは無視する。
            let _ = tx.send(result);
        });

        Self { path, progress, cancel, result_rx: rx, cancelled_by_user: false }
    }

    /// 現在の進捗を返す。
    pub fn progress(&self) -> ArchiveOpenProgress {
        *self.progress.lock().unwrap()
    }

    /// ユーザーによるキャンセルを要求する。ワーカーは次の進捗チェック地点で打ち切る。
    pub fn cancel(&mut self) {
        self.cancelled_by_user = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 完了しているかを確認する。まだなら`Pending`を返す（毎フレーム呼んでよい）。
    pub fn poll(&mut self) -> OpenPollResult {
        match self.result_rx.try_recv() {
            Ok(Some(entries)) if entries.is_empty() => OpenPollResult::Empty,
            Ok(Some(entries)) => OpenPollResult::Ready(entries),
            Ok(None) => {
                if self.cancelled_by_user {
                    OpenPollResult::Cancelled
                } else {
                    // コールバックがfalseを返す経路は現状キャンセルのみだが、
                    // 将来の拡張に備えて非キャンセル終了もCancelled扱いにせず無効アーカイブに倒す。
                    OpenPollResult::Empty
                }
            }
            Err(mpsc::TryRecvError::Empty) => OpenPollResult::Pending,
            Err(mpsc::TryRecvError::Disconnected) => OpenPollResult::Empty,
        }
    }
}
