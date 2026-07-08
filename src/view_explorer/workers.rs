use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::cache::{FileCache, FileCacheEntry, LoadRequest, LoadResult, ThumbResult, EntryThumbRequest};
use crate::fs::archive;
use crate::view_reader::ViewerState;
use super::*;
use super::scan::spawn_summary_worker;

impl NekoviewApp {
    /// 毎フレーム、egui パス内で UI 描画より前に呼ぶ「常時走る処理」。
    /// （旧 eframe::App::logic 相当。winit ループ本体から各フレーム呼ぶ）
    pub fn logic(&mut self, ctx: &egui::Context) {
        // 旧 eframe::App::logic 相当の「常時走る処理」フック。winit ループ本体から
        // 各フレーム呼ばれる。現状は常時処理なし:
        // ・ビューア破棄は window_event の CloseRequested / ESC で直接行う（旧 viewer_closing
        //   フラグ経由の deferred callback 回避策は winit 化で不要になり撤去）。
        // ・ステータス窓（debug）は winit_app が独立 OS 窓として直接 render_status を駆動する。
        self.poll_resize_redecode(ctx);
    }

    /// フェーズ6: リサイズ/zoom_actual切替のデバウンス判定。
    /// viewer_cfg.redecode_trigger_seq の変化を検知してデバウンス期限を(再)セットし、
    /// 期限が過ぎたら発火する。
    /// フェーズ6-E: 待機中は `ctx.request_repaint_after()` で明示的に未来のフレームを
    /// 予約する。これが無いと、リサイズ後にアニメ等の継続的な再描画要因が無い窓では
    /// 「デッドラインは設定されたが、それを再評価するフレームが二度と来ない」ため
    /// デバウンスが体感上まったく発火しないバグがあった（実機確認で発覚）。
    /// また呼び出し元はエクスプローラー窓のlogic()だけでなくビューアー窓のrender_viewer()
    /// からも呼ぶ必要がある（ビューアー窓だけを操作している間はエクスプローラー窓が
    /// 再描画されないため）。
    pub(super) fn poll_resize_redecode(&mut self, ctx: &egui::Context) {
        let (redecode_on, debounce_ms, seq) = {
            let cfg = self.viewer_cfg.lock().unwrap();
            (cfg.redecode_on_resize, cfg.resize_debounce_ms, cfg.redecode_trigger_seq)
        };
        if !redecode_on {
            self.resize_redecode_last_seq = seq;
            self.resize_redecode_deadline = None;
            // 「原寸」選択中は常にガードレール値（長辺 max_decode_edge）を使う。
            // fire_resize_redecode() 経由で decode_target が None（無制限）になったまま
            // 放置されると、一度でも「ウィンドウ追従」+ビューアー等倍ズームを使った後は
            // 「原寸」に戻してもガードレールが永続的に外れたままになるバグがあったため、
            // ここで毎フレーム復元する（実際に変化した時だけ再デコードを発火）。
            let guardrail = Some((self.config.max_decode_edge, self.config.max_decode_edge));
            if self.decode_target != guardrail {
                self.decode_target = guardrail;
                self.redecode_visible_pages(guardrail);
                crate::log_common!("[resize-redecode] restored guardrail target={:?}", guardrail);
            }
            return;
        }
        if seq != self.resize_redecode_last_seq {
            self.resize_redecode_last_seq = seq;
            self.resize_redecode_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(debounce_ms));
        }
        if let Some(deadline) = self.resize_redecode_deadline {
            let now = std::time::Instant::now();
            if now >= deadline {
                self.resize_redecode_deadline = None;
                self.fire_resize_redecode(seq);
            } else {
                ctx.request_repaint_after(deadline - now);
            }
        }
    }

    /// フェーズ6-C/6-D: デバウンス発火時に、表示中ページ(見開き時は2枚)を新しいターゲットサイズで
    /// 再デコードさせる。PageCacheから既存エントリを破棄し、新規LoadRequestを送るだけで、
    /// 静止画・アニメーション(RingAnimation)とも decode_ring_anim/resize_for_display 側の
    /// target_size配線に乗って統一的に再デコードされる。アニメは新規RingAnimationとして
    /// 作られるため自然に再生位置が先頭へ戻る（フェーズ6-A決定事項どおり）。
    fn fire_resize_redecode(&mut self, seq: u64) {
        let zoom_actual = self.viewer_cfg.lock().unwrap().zoom_actual;
        let target = {
            let viewer = self.viewer.lock().unwrap();
            match viewer.as_ref() {
                Some(v) => v.current_decode_target(zoom_actual),
                None => return,
            }
        };
        self.decode_target = target;
        let pages = self.redecode_visible_pages(target);

        crate::log_common!(
            "[resize-redecode] fired (generation={}, target={:?}, pages={})",
            seq, target, pages,
        );
    }

    /// LoadRequestを送出する。7zがFileCacheへの展開待ちの間は、要求を保留キューへ積んで
    /// 展開完了後にまとめて送る（デコードワーカー側でのスレッドごとの重複展開を避けるため）。
    fn dispatch_load_request(&mut self, mut req: LoadRequest) {
        let entry = self.file_cache.get(&req.archive_path);
        if entry.is_none()
            && !req.is_raw_file
            && archive::is_7z_path(&req.archive_path)
            && self.file_cache_pending.contains(&req.archive_path)
        {
            self.deferred_archive_requests
                .entry(req.archive_path.clone())
                .or_default()
                .push(DeferredArchiveRequest::Page(req));
            return;
        }
        req.file_cache_entry = entry;
        let _ = self.req_tx.send(req);
    }

    /// 現在ビューアーに表示中のページ(見開き時は2枚)を、指定ターゲットサイズで再デコードさせる。
    /// PageCacheから既存エントリを破棄し、新規LoadRequestを送るだけで、静止画・アニメーション
    /// (RingAnimation)とも decode_ring_anim/resize_for_display 側の target_size配線に乗って
    /// 統一的に再デコードされる（アニメは新規RingAnimationとして作られるため自然に再生位置が
    /// 先頭へ戻る）。戻り値は再デコード対象にしたページ数（ログ用）。
    fn redecode_visible_pages(&mut self, target: Option<(u32, u32)>) -> usize {
        let (path, is_raw_file, pages) = {
            let viewer = self.viewer.lock().unwrap();
            let Some(v) = viewer.as_ref() else { return 0 };
            let path = v.archive_path().clone();
            let is_raw_file = v.is_raw_file();
            let entries = v.entries();
            let pages: Vec<(usize, String)> = v
                .visible_original_indices()
                .into_iter()
                .filter_map(|orig_i| {
                    entries.iter()
                        .find(|e| e.original_index == orig_i)
                        .map(|e| (orig_i, e.entry_name.clone()))
                })
                .collect();
            (path, is_raw_file, pages)
        };

        for (orig_i, entry_name) in &pages {
            self.page_cache.lock().unwrap().remove(&path, *orig_i);
            let key = (path.clone(), *orig_i);
            self.pending_loads.lock().unwrap().insert(key);
            self.dispatch_load_request(LoadRequest {
                archive_path: path.clone(),
                index: *orig_i,
                entry_name: entry_name.clone(),
                is_raw_file,
                file_cache_entry: None,
                target_size: target,
            });
        }

        if let Some(v) = self.viewer.lock().unwrap().as_mut() {
            let orig_indices: Vec<usize> = pages.iter().map(|(i, _)| *i).collect();
            v.invalidate_pages(&orig_indices);
        }

        pages.len()
    }

    /// フェーズ6: ビューアー窓のリサイズを通知する（winit_app.rs の WindowEvent::Resized から呼ぶ）。
    /// viewer_cfg.redecode_trigger_seq を進め、poll_resize_redecode() 側の変化検知に拾わせる。
    pub fn notify_viewer_resized(&mut self) {
        self.viewer_cfg.lock().unwrap().redecode_trigger_seq += 1;
    }

    /// 終了時に状態を永続化する（旧 eframe::App::on_exit 相当）。
    pub fn on_exit(&mut self) {
        self.persist_state();
    }

    pub(super) fn poll_workers(&mut self, ctx: &egui::Context) {
        self.poll_mount_checks();

        // バックグラウンドスキャン結果をポーリング
        self.poll_scan();
        self.poll_tree_scan();

        // サムネイルワーカーからの結果を受信してGPUテクスチャへアップロード
        let was_pending = !self.thumb_pending.is_empty();
        let thumb_results: Vec<ThumbResult> =
            std::iter::from_fn(|| self.thumb_res_rx.try_recv().ok()).collect();
        for result in thumb_results {
            self.thumb_pending.remove(&result.path);
            match result.rgba {
                Some(rgba) => {
                    if self.archives.contains(&result.path) {
                        let name = result.path.display().to_string();
                        let tex = upload_texture(ctx, &name, &rgba);
                        self.thumbnails.insert(result.path, tex);
                    }
                }
                None => {
                    self.maybe_check_mount_after_failure(&result.path);
                    self.thumb_failed.insert(result.path);
                }
            }
        }
        // pending が空になった瞬間に最終カウントを更新する
        let just_finished = was_pending && self.thumb_pending.is_empty();

        // cd_summary バックグラウンド計算の結果をポーリング
        if let Some(ref rx) = self.cd_summary_rx {
            if let Ok((path, saved, total)) = rx.try_recv() {
                // 現在の CD/LS ディレクトリに対応する結果のみ反映（古い結果を捨てる）
                let is_current = self.viewing_dir.as_ref() == Some(&path);
                if is_current {
                    self.cd_summary = Some((path, saved, total));
                }
                self.cd_summary_rx = None;
                self.cd_summary_updated_at = Some(std::time::Instant::now());
            }
        }

        // サムネ処理中は2秒ごと、完了直後は即座にバックグラウンド再計算をスケジュール
        if self.cd_summary.is_some() && self.cd_summary_rx.is_none() && (just_finished || !self.thumb_pending.is_empty()) {
            let elapsed = self
                .cd_summary_updated_at
                .map_or(f32::MAX, |t| t.elapsed().as_secs_f32());
            if just_finished || elapsed >= 2.0 {
                // archives は current_dir のものなので、サマリー対象が一致する場合のみ再計算する
                let refresh_target = match self.cd_summary {
                    Some((ref cd_path, _, _)) if *cd_path == self.current_dir => Some(cd_path.clone()),
                    _ => None,
                };
                if let Some(path) = refresh_target {
                    self.cd_summary_rx = Some(spawn_summary_worker(
                        path,
                        self.archive_filenames(),
                        self.cache_db.clone(),
                        self.egui_ctx.clone(),
                    ));
                }
            }
        }

        // FileCache ワーカーからの結果を受信して横キャッシュへ投入。
        // None は「予算超過スキップ or 読み込み失敗」でキャッシュには入れないが、
        // pending解放と保留リクエストのフラッシュ（ディスク直読みフォールバック）は行う。
        let file_results: Vec<(PathBuf, Option<FileCacheEntry>)> =
            std::iter::from_fn(|| self.file_cache_res_rx.try_recv().ok()).collect();
        let cur_viewer_path = self.viewer.lock().unwrap().as_ref().map(|v| v.archive_path().clone());
        for (path, entry) in file_results {
            self.file_cache_pending.remove(&path);
            if let Some(entry) = entry {
                let current = cur_viewer_path.clone().unwrap_or_else(|| path.clone());
                self.file_cache.insert(path.clone(), entry, &current, &self.archives);
            }
            // 7zの展開待ちで保留していたページ/サムネ要求をまとめてフラッシュする。
            if let Some(deferred) = self.deferred_archive_requests.remove(&path) {
                let file_cache_entry = self.file_cache.get(&path);
                for d in deferred {
                    match d {
                        DeferredArchiveRequest::Page(mut req) => {
                            req.file_cache_entry = file_cache_entry.clone();
                            let _ = self.req_tx.send(req);
                        }
                        DeferredArchiveRequest::Thumb(mut req) => {
                            req.file_cache_entry = file_cache_entry.clone();
                            let original_index = req.original_index;
                            let archive_path = req.archive_path.clone();
                            if self.entry_thumb_req_tx.send(req).is_ok() {
                                if let Some(v) = self.viewer.lock().unwrap().as_mut() {
                                    if *v.archive_path() == archive_path {
                                        v.mark_thumb_pending(original_index);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ワーカーからの結果を PageCache へ投入
        let results: Vec<LoadResult> =
            std::iter::from_fn(|| self.res_rx.lock().unwrap().try_recv().ok()).collect();
        let (cur_path, cur_idx) = self
            .viewer
            .lock().unwrap()
            .as_ref()
            .map(|v| {
                let sorted_lo = v.spread_lo().max(0) as usize;
                let orig = if sorted_lo < v.entries().len() {
                    v.entries()[sorted_lo].original_index
                } else {
                    0
                };
                (v.archive_path().clone(), orig)
            })
            .unwrap_or_default();
        for result in results {
            self.pending_loads.lock().unwrap()
                .remove(&(result.archive_path.clone(), result.index));
            self.page_cache.lock().unwrap().insert(
                result.archive_path,
                result.index,
                result.content,
                &cur_path,
                cur_idx,
            );
        }
    }

    pub(super) fn prefetch_pages(&mut self) {
        // スライディングウィンドウ: ビューア表示中に前後ページを先読み
        let viewer_prefetch = self.viewer.lock().unwrap().as_ref().map(|viewer| {
            let cur = viewer.spread_lo().max(0) as usize;
            (cur, viewer.archive_path().clone(), viewer.entries().to_vec(), viewer.is_raw_file())
        });
        if let Some((cur, path, entries, is_raw_file)) = viewer_prefetch {
            let total = entries.len();
            let cur_orig_i = entries.get(cur).map(|e| e.original_index);
            let start = cur.saturating_sub(crate::cache::PREFETCH_BEHIND);
            let end = (cur + crate::cache::PREFETCH_AHEAD + 1).min(total);
            for i in start..end {
                let orig_i = entries[i].original_index;
                // 予算超過(bypass)と判明済みのページは、現在表示中でない限り先読み対象から外す。
                // bypass はキャッシュに残らないため、先読みし続けると無限に再デコードされてしまう。
                if Some(orig_i) != cur_orig_i && self.page_cache.lock().unwrap().is_known_bypass(&path, orig_i) {
                    continue;
                }
                let key = (path.clone(), orig_i);
                if !self.page_cache.lock().unwrap().contains(&path, orig_i) && !self.pending_loads.lock().unwrap().contains(&key) {
                    self.pending_loads.lock().unwrap().insert(key);
                    self.dispatch_load_request(LoadRequest {
                        archive_path: path.clone(),
                        index: orig_i,
                        entry_name: entries[i].entry_name.clone(),
                        is_raw_file,
                        file_cache_entry: None,
                        target_size: self.decode_target,
                    });
                }
            }
        }
    }

    /// ファイルが FileCache 未登録かつ未リクエストの場合にバックグラウンド読み込みを起動する。
    pub(super) fn ensure_file_cached(&mut self, path: PathBuf) {
        if !self.file_cache.contains(&path) && !self.file_cache_pending.contains(&path) {
            let _ = self.file_cache_req_tx.send(path.clone());
            self.file_cache_pending.insert(path);
        }
    }
}

/// RgbaImage を egui のテクスチャとして登録する
fn upload_texture(ctx: &egui::Context, name: &str, rgba: &image::RgbaImage) -> egui::TextureHandle {
    let (w, h) = rgba.dimensions();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    );
    ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR)
}

/// EntryThumbRequestを送出する。7zがFileCacheへの展開待ちの間は保留キューへ積んで、
/// 展開完了後にまとめて送る（デコードワーカー側でのスレッドごとの重複展開を避けるため）。
/// フリー関数にしているのは、呼び出し元で`viewer: &mut ViewerState`（`self.viewer`の
/// MutexGuard経由）を同時に借用する必要があり、`&mut self`メソッドにすると
/// 呼び出し元で二重借用エラーになるため（個別フィールド借用なら共存できる）。
pub(super) fn dispatch_thumb_request(
    file_cache: &FileCache,
    file_cache_pending: &HashSet<PathBuf>,
    deferred_archive_requests: &mut HashMap<PathBuf, Vec<DeferredArchiveRequest>>,
    entry_thumb_req_tx: &mpsc::Sender<EntryThumbRequest>,
    mut req: EntryThumbRequest,
    viewer: &mut ViewerState,
) {
    let entry = file_cache.get(&req.archive_path);
    if entry.is_none()
        && !req.is_raw_file
        && archive::is_7z_path(&req.archive_path)
        && file_cache_pending.contains(&req.archive_path)
    {
        deferred_archive_requests
            .entry(req.archive_path.clone())
            .or_default()
            .push(DeferredArchiveRequest::Thumb(req));
        return;
    }
    req.file_cache_entry = entry;
    let original_index = req.original_index;
    if entry_thumb_req_tx.send(req).is_ok() {
        viewer.mark_thumb_pending(original_index);
    }
}
