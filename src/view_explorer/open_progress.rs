//! エクスプローラーからのアーカイブオープンを非同期化するための状態管理。
//! ワーカースレッドで「一覧取得(list_images_with_progress)」と「メモリ見積もり
//! (estimate_archive_memory。7z/tarは実質全画像展開、zipもサンプル画像デコードを伴う
//! ため軽くない)」を両方まとめて実行し、どちらの区間も進捗をポーリング可能な形で
//! 保持する。以前は見積もりだけメインスレッドで同期実行しており、そこが無表示の
//! まま固まって見える抜け穴になっていた。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::fs::archive::{self, ArchiveMemoryCheck, ArchiveOpenProgress, ImageEntry};
use crate::neko_dir::{DetailFlag, SourceRating};

/// 非同期オープン中の局面。
#[derive(Clone, Copy)]
pub enum ArchiveOpenPhase {
    /// アーカイブ内画像の一覧取得中。
    Listing(ArchiveOpenProgress),
    /// 一覧取得後のメモリ見積もり（サンプル画像デコード）中。
    /// サンプル数が少なく件数ベースの%表示に意味が無いため状態のみ。
    Estimating,
}

/// ポーリング結果。オープン完了までは`Pending`。
pub enum OpenPollResult {
    /// まだ読み込み中。
    Pending,
    /// 読み込み・見積もり完了。有効な画像アーカイブだった。
    Ready { entries: Vec<ImageEntry>, check: ArchiveMemoryCheck, detail: DetailFlag, rating: SourceRating },
    /// 読み込み完了したが画像が1件も無かった（無効アーカイブ扱い）。
    Empty,
    /// ユーザーがキャンセルした。
    Cancelled,
}

/// メモリ見積もりに必要な設定値のスナップショット。ワーカースレッドへ`Copy`で渡す
/// ため、`&self`（`NekoviewApp`）を直接キャプチャせずに済ませる。
#[derive(Clone, Copy)]
pub struct MemoryBudgetParams {
    pub cache_budget_bytes: usize,
    pub anim_ring_bounds: (usize, usize),
    pub max_decode_edge: u32,
    pub file_budget_bytes: usize,
}

/// 計測ログ（`[perf/open]`）用の短いファイル名。
pub(super) fn trace_name(path: &std::path::Path) -> String {
    crate::config::short_log_name(path.file_name().and_then(|n| n.to_str()).unwrap_or("?"))
}

/// 「ダブルクリック→ビューアーに1枚目が出るまで」の計測用の時刻メモ（速度調査用。挙動には影響しない）。
/// `poll_workers` が1枚目のページ到着・FileCache到着を拾ってログに出す。
pub(super) struct OpenTrace {
    pub path: PathBuf,
    pub t_click: std::time::Instant,
    /// `open_viewer` を呼び終えた時刻。まだなら None。
    pub t_viewer: Option<std::time::Instant>,
    /// ページ到着ログを出した回数（最初の数件だけ出す）。
    pub pages_logged: usize,
}

enum WorkerOutcome {
    Cancelled,
    Empty,
    Ready { entries: Vec<ImageEntry>, check: ArchiveMemoryCheck, detail: DetailFlag, rating: SourceRating },
}

/// エクスプローラーからダブルクリックされたアーカイブの非同期オープン処理。
pub struct PendingOpen {
    pub path: PathBuf,
    /// `None`はワーカーがまだ最初の進捗コールバックを送っていない状態
    /// （フォーマット未確定＝zip/7zなのかtarなのかもまだ分からない）を表す。
    /// これを`ArchiveOpenProgress::Indeterminate`で代用すると、起動直後の
    /// 数フレームやすぐ完了する小さいzip/7zでも「tar読み込み中」表示になってしまうため分離する。
    progress: Arc<Mutex<Option<ArchiveOpenPhase>>>,
    cancel: Arc<AtomicBool>,
    result_rx: mpsc::Receiver<WorkerOutcome>,
    cancelled_by_user: bool,
}

impl PendingOpen {
    /// ワーカースレッドを起動し、非同期で一覧取得＋メモリ見積もりを開始する。
    pub fn spawn(path: PathBuf, budget: MemoryBudgetParams) -> Self {
        let progress = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        let worker_path = path.clone();
        let worker_progress = Arc::clone(&progress);
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            // 開く処理の間は、FileCache の丸読みや手すきサムネ生成にディスクを譲らせる。
            let _fg = crate::cache::ForegroundIoGuard::begin();
            // サムネバー（前に開いた本のものを含む）の丸読みが、新しい読みを始めずに譲るようにする。
            let _page_io = crate::cache::PageIoGuard::begin();
            let mut on_progress = |p: ArchiveOpenProgress| -> bool {
                *worker_progress.lock().unwrap() = Some(ArchiveOpenPhase::Listing(p));
                !worker_cancel.load(Ordering::Relaxed)
            };
            let t_list = std::time::Instant::now();
            let listed = archive::list_images_with_progress(&worker_path, &mut on_progress);
            let list_ms = t_list.elapsed().as_secs_f64() * 1000.0;
            let outcome = match listed {
                None => WorkerOutcome::Cancelled,
                Some(entries) if entries.is_empty() => WorkerOutcome::Empty,
                Some(_entries) if worker_cancel.load(Ordering::Relaxed) => WorkerOutcome::Cancelled,
                Some(entries) => {
                    *worker_progress.lock().unwrap() = Some(ArchiveOpenPhase::Estimating);
                    let t_est = std::time::Instant::now();
                    let check = archive::estimate_archive_memory(
                        &worker_path,
                        &entries,
                        budget.cache_budget_bytes,
                        budget.anim_ring_bounds,
                        budget.max_decode_edge,
                        budget.file_budget_bytes,
                    );
                    let est_ms = t_est.elapsed().as_secs_f64() * 1000.0;
                    // 詳細テキストの有無もここで確認する（zip/cbzのみ。他形式は未確認のまま）。
                    let t_detail = std::time::Instant::now();
                    let read = archive::meta::read_detail(&worker_path);
                    let detail = DetailFlag::from_read(read.as_ref(), archive::meta::detail_supported(&worker_path));
                    let rating = SourceRating::from_read(read.as_ref());
                    crate::log_perf!(
                        "[perf/open] ワーカー {} entries={} 一覧={:.1}ms 見積もり={:.1}ms 詳細={:.1}ms 合計={:.1}ms",
                        trace_name(&worker_path),
                        entries.len(),
                        list_ms,
                        est_ms,
                        t_detail.elapsed().as_secs_f64() * 1000.0,
                        t_list.elapsed().as_secs_f64() * 1000.0,
                    );
                    WorkerOutcome::Ready { entries, check, detail, rating }
                }
            };
            // 受信側が既に破棄されていても（キャンセル後の取りこぼし）エラーは無視する。
            let _ = tx.send(outcome);
        });

        Self { path, progress, cancel, result_rx: rx, cancelled_by_user: false }
    }

    /// 現在の進捗を返す。まだ最初のコールバックが来ていなければ`None`
    /// （フォーマット未確定の起動直後）。
    pub fn progress(&self) -> Option<ArchiveOpenPhase> {
        *self.progress.lock().unwrap()
    }

    /// ユーザーによるキャンセルを要求する。ワーカーは次の進捗チェック地点で打ち切る。
    /// 見積もりフェーズ（サンプル画像デコード）自体は現状打ち切れず、次のチェック地点
    /// （一覧取得中のエントリ境界、または見積もり完了後）まで待つ。
    pub fn cancel(&mut self) {
        self.cancelled_by_user = true;
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 完了しているかを確認する。まだなら`Pending`を返す（毎フレーム呼んでよい）。
    pub fn poll(&mut self) -> OpenPollResult {
        match self.result_rx.try_recv() {
            Ok(WorkerOutcome::Empty) => OpenPollResult::Empty,
            Ok(WorkerOutcome::Ready { entries, check, detail, rating }) => OpenPollResult::Ready { entries, check, detail, rating },
            Ok(WorkerOutcome::Cancelled) => {
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
        let budget = MemoryBudgetParams {
            cache_budget_bytes: self.cache_budget_bytes,
            anim_ring_bounds: self.anim_ring_bounds,
            max_decode_edge: self.config.max_decode_edge,
            file_budget_bytes: self.file_cache.max_bytes(),
        };
        self.open_trace = Some(OpenTrace { path: path.clone(), t_click: std::time::Instant::now(), t_viewer: None, pages_logged: 0 });
        self.pending_open = Some(PendingOpen::spawn(path, budget));
    }

    /// 非同期オープンの完了を毎フレーム確認する。完了していれば
    /// メモリ見積もり結果の反映→ViewerState構築→open_viewer、または
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
                self.set_toast(crate::i18n::t().invalid_zip(&name));
            }
            OpenPollResult::Ready { entries, check, detail, rating } => {
                let path = self.pending_open.take().expect("pending_open just polled").path;
                let t_ready = std::time::Instant::now();
                if let Some(tr) = self.open_trace.as_ref().filter(|t| t.path == path) {
                    crate::log_perf!(
                        "[perf/open] {} クリック→ワーカー結果受信={:.1}ms",
                        trace_name(&path),
                        tr.t_click.elapsed().as_secs_f64() * 1000.0,
                    );
                }
                // TXTページは、設定でページ数に含める場合だけ数える（既定は含めない）。
                let count_text = archive::text_page::options().count_in_pages;
                let pages = entries
                    .iter()
                    .filter(|e| count_text || !archive::text_page::is_text_entry_name(&e.entry_name))
                    .count();
                let t_meta = std::time::Instant::now();
                self.record_archive_meta(&path, pages, detail, rating);
                crate::log_perf!("[perf/open] {} DB記録(メイン)={:.1}ms", trace_name(&path), t_meta.elapsed().as_secs_f64() * 1000.0);
                if self.apply_memory_check(&path, check) {
                    let is_virtual_book = path.is_dir();
                    let state = crate::view_reader::ViewerState::from_image_entries(
                        path,
                        entries,
                        self.viewer_slots,
                        self.config.default_slot,
                        is_virtual_book,
                    );
                    let t_open = std::time::Instant::now();
                    let name = trace_name(state.archive_path());
                    self.open_viewer(state);
                    crate::log_perf!(
                        "[perf/open] {} open_viewer(メイン)={:.1}ms 受信からここまで={:.1}ms",
                        name,
                        t_open.elapsed().as_secs_f64() * 1000.0,
                        t_ready.elapsed().as_secs_f64() * 1000.0,
                    );
                    if let Some(tr) = self.open_trace.as_mut() {
                        tr.t_viewer = Some(std::time::Instant::now());
                    }
                }
                // OverBudgetの場合はapply_memory_check内でmemory_warning_openが立つ。
            }
        }
    }

    /// ビューアーを開いた時点のページ数・詳細有無を、そのファイルのフォルダの cache.redb に記録する。
    /// 仮想フォルダ・お気に入り・検索などから開いた場合も、ファイル自身のフォルダへ書く。
    /// 書けなくても（DBが開けない等）オープンは止めない。
    fn record_archive_meta(&mut self, path: &std::path::Path, page_count: usize, detail: DetailFlag, rating: SourceRating) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return };
        let db = if dir == self.current_dir {
            self.cache_db.clone()
        } else {
            crate::neko_dir::neko_dir_for(dir, &self.config)
                .and_then(|nd| crate::neko_dir::open_cache_db(&nd, dir))
        };
        let Some(db) = db else { return };
        let mtime = crate::neko_dir::file_mtime(path);
        let size = crate::neko_dir::file_size(path);
        crate::neko_dir::write_archive_meta_full(&db, name, mtime, size, page_count, detail, rating);
        // 次の描画でそのフォルダの記録を読み直す（リストの列にすぐ反映される）。
        self.archive_info.invalidate_dir(dir);
    }

    /// アーカイブオープン中央オーバーレイを描画する。
    /// `egui::Modal`は背後のウィジェットへのマウス入力を自動的に遮断する
    /// （キーボードは遮断しないため、呼び出し元で別途`handle_explorer_keys`をガードする）。
    /// `egui::Modal`はデフォルトで画面中央に表示されるため、位置指定は不要。
    pub(super) fn render_pending_open_overlay(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_open.as_ref() else { return };
        let name = super::panels::truncate_filename(&pending.path);
        // `ProgressBar::animate(true)`のシマー演出は体感でほぼ気づけないほど弱いため、
        // 件数が確定していない局面（フォーマット未確定/tar/見積もり中）は
        // 誰の目にも「動いている」とわかる`egui::Spinner`で示す。件数が確定している
        // 局面（zip/7zの一覧取得中）だけ実%の`ProgressBar`を使う。
        enum Visual {
            Bar(egui::ProgressBar),
            Spinner(&'static str),
        }
        let visual = match pending.progress() {
            None => Visual::Spinner(crate::i18n::t().archive_open_progress_starting()),
            Some(ArchiveOpenPhase::Listing(ArchiveOpenProgress::Determinate { current, total })) => {
                let fraction = if total == 0 { 0.0 } else { current as f32 / total as f32 };
                Visual::Bar(
                    egui::ProgressBar::new(fraction)
                        .desired_width(280.0)
                        .text(crate::i18n::t().archive_open_progress(current, total)),
                )
            }
            Some(ArchiveOpenPhase::Listing(ArchiveOpenProgress::Indeterminate)) => {
                // 全件数が事前にわからない(tar)ため、%表示はせずスピナーで示す。
                Visual::Spinner(crate::i18n::t().archive_open_progress_indeterminate())
            }
            Some(ArchiveOpenPhase::Estimating) => {
                // サンプル画像デコードによる見積もり中。サンプル数が少なく%表示に意味が
                // 無いためスピナーで示す（元の「無表示のまま固まる」問題の本体だった区間）。
                Visual::Spinner(crate::i18n::t().archive_open_estimating())
            }
        };
        let mut cancel_clicked = false;
        egui::Modal::new(egui::Id::new("archive_open_progress")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(name).strong());
                ui.add_space(10.0);
                match visual {
                    Visual::Bar(bar) => {
                        ui.add(bar);
                    }
                    Visual::Spinner(text) => {
                        ui.add(egui::Spinner::new().size(24.0));
                        ui.add_space(6.0);
                        ui.label(text);
                    }
                }
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
