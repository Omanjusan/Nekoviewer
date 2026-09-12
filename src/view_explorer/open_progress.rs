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
    /// `None`はワーカーがまだ最初の進捗コールバックを送っていない状態
    /// （フォーマット未確定＝zip/7zなのかtarなのかもまだ分からない）を表す。
    /// これを`ArchiveOpenProgress::Indeterminate`で代用すると、起動直後の
    /// 数フレームやすぐ完了する小さいzip/7zでも「tar読み込み中」表示になってしまうため分離する。
    progress: Arc<Mutex<Option<ArchiveOpenProgress>>>,
    cancel: Arc<AtomicBool>,
    result_rx: mpsc::Receiver<Option<Vec<ImageEntry>>>,
    cancelled_by_user: bool,
}

impl PendingOpen {
    /// ワーカースレッドを起動し、非同期でアーカイブの一覧取得を開始する。
    pub fn spawn(path: PathBuf) -> Self {
        let progress = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let worker_path = path.clone();
        let worker_progress = Arc::clone(&progress);
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let mut on_progress = |p: ArchiveOpenProgress| -> bool {
                *worker_progress.lock().unwrap() = Some(p);
                !worker_cancel.load(Ordering::Relaxed)
            };
            let result = archive::list_images_with_progress(&worker_path, &mut on_progress);
            // 受信側が既に破棄されていても（キャンセル後の取りこぼし）エラーは無視する。
            let _ = tx.send(result);
        });

        Self { path, progress, cancel, result_rx: rx, cancelled_by_user: false }
    }

    /// 現在の進捗を返す。まだ最初のコールバックが来ていなければ`None`
    /// （フォーマット未確定の起動直後）。
    pub fn progress(&self) -> Option<ArchiveOpenProgress> {
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

impl super::NekoviewApp {
    /// エクスプローラーからのダブルクリック等でアーカイブオープンを非同期開始する。
    /// 既に処理中（オーバーレイ表示中）なら何もしない。ダブルクリック等の入口側は
    /// `pending_open.is_some()` の間ガードされる想定だが、防御的に二重起動を防ぐ。
    pub(super) fn start_archive_open(&mut self, path: PathBuf) {
        if self.pending_open.is_some() {
            return;
        }
        self.pending_open = Some(PendingOpen::spawn(path));
    }

    /// 非同期オープンの完了を毎フレーム確認する。完了していれば
    /// メモリ見積もりゲート→ViewerState構築→open_viewer、または
    /// 無効アーカイブ/キャンセルの後始末を行う。
    pub(super) fn poll_pending_open(&mut self) {
        let result = match self.pending_open.as_mut() {
            Some(pending) => pending.poll(),
            None => return,
        };
        match result {
            OpenPollResult::Pending => {}
            OpenPollResult::Cancelled => {
                self.pending_open = None;
            }
            OpenPollResult::Empty => {
                let path = self.pending_open.take().expect("pending_open just polled").path;
                self.mark_archive_invalid(&path);
                let name = super::panels::truncate_filename(&path);
                self.app_toast = Some((crate::i18n::t().invalid_zip(&name), std::time::Instant::now()));
            }
            OpenPollResult::Ready(entries) => {
                let path = self.pending_open.take().expect("pending_open just polled").path;
                if self.check_memory_budget_for_entries(&path, &entries) {
                    let state = crate::view_reader::ViewerState::from_image_entries(
                        path,
                        entries,
                        self.viewer_slots,
                        self.config.default_slot,
                    );
                    self.open_viewer(state);
                }
                // OverBudgetの場合はcheck_memory_budget_for_entries内でmemory_warning_openが立つ。
            }
        }
    }

    /// アーカイブオープン中央オーバーレイを描画する。
    /// `egui::Modal`は背後のウィジェットへのマウス入力を自動的に遮断する
    /// （キーボードは遮断しないため、呼び出し元で別途`handle_explorer_keys`をガードする）。
    /// `egui::Modal`はデフォルトで画面中央に表示されるため、位置指定は不要。
    pub(super) fn render_pending_open_overlay(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_open.as_ref() else { return };
        let name = super::panels::truncate_filename(&pending.path);
        let bar = match pending.progress() {
            None => {
                // ワーカーがまだ最初のコールバックを送っていない（フォーマット未確定）。
                // zip/7zかtarかもまだ分からないため、tar専用文言は出さず中立な文言にする。
                egui::ProgressBar::new(0.0)
                    .desired_width(280.0)
                    .animate(true)
                    .text(crate::i18n::t().archive_open_progress_starting())
            }
            Some(crate::fs::archive::ArchiveOpenProgress::Determinate { current, total }) => {
                let fraction = if total == 0 { 0.0 } else { current as f32 / total as f32 };
                egui::ProgressBar::new(fraction)
                    .desired_width(280.0)
                    .text(crate::i18n::t().archive_open_progress(current, total))
            }
            Some(crate::fs::archive::ArchiveOpenProgress::Indeterminate) => {
                // 全件数が事前にわからない(tar)ため、%表示はせず不確定アニメーションのみ示す。
                egui::ProgressBar::new(0.0)
                    .desired_width(280.0)
                    .animate(true)
                    .text(crate::i18n::t().archive_open_progress_indeterminate())
            }
        };
        let mut cancel_clicked = false;
        egui::Modal::new(egui::Id::new("archive_open_progress")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(name).strong());
                ui.add_space(10.0);
                ui.add(bar);
                ui.add_space(12.0);
                if ui.button(crate::i18n::t().archive_open_cancel()).clicked() {
                    cancel_clicked = true;
                }
            });
        });
        if cancel_clicked {
            if let Some(pending) = self.pending_open.as_mut() {
                pending.cancel();
            }
        }
        ctx.request_repaint();
    }
}
