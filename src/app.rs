use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::cache::{FileCache, LoadRequest, LoadResult, PageCache, ThumbRequest, ThumbResult, spawn_worker, spawn_thumb_worker, spawn_file_cache_worker};
use crate::config::{AppConfig, SortState, WindowSlot};
use crate::neko_dir;
use crate::fs::{dir, mount::{list_gvfs_smb_mounts, list_local_drives, MountEntry}};
use crate::viewer::{PageMode, ViewerNav, ViewerState};

#[derive(Debug, Clone, Copy, PartialEq)]
enum SortKey {
    Name,
    Date,
    Size,
}

impl SortKey {
    fn label(self) -> &'static str {
        match self {
            SortKey::Name => "[名前]",
            SortKey::Date => "[日付]",
            SortKey::Size => "[サイズ]",
        }
    }

    fn as_state_key(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Date => "date",
            SortKey::Size => "size",
        }
    }

    fn from_state_key(s: &str) -> Self {
        match s {
            "date" => SortKey::Date,
            "size" => SortKey::Size,
            _ => SortKey::Name,
        }
    }
}

enum TreeAction {
    None,
    ToggleExpand(PathBuf),
    Navigate(PathBuf),
}

fn show_tree_node(
    ui: &mut egui::Ui,
    path: &PathBuf,
    depth: usize,
    viewing_dir: &Option<PathBuf>,
    tree_expanded: &HashSet<PathBuf>,
    tree_children: &HashMap<PathBuf, Vec<PathBuf>>,
    show_hidden: bool,
    action: &mut TreeAction,
) {
    if !matches!(action, TreeAction::None) {
        return;
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string());

    let is_expanded = tree_expanded.contains(path);
    let is_current = viewing_dir.as_ref() == Some(path);

    let show_arrow = is_expanded
        || match tree_children.get(path) {
            None => true,
            Some(ch) => !ch.is_empty(),
        };

    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 12.0);

        if show_arrow {
            let arrow = if is_expanded { "▼" } else { "▶" };
            let r = ui.add(egui::Label::new(arrow).sense(egui::Sense::click()));
            if r.clicked() {
                *action = TreeAction::ToggleExpand(path.clone());
            }
        } else {
            ui.add_space(12.0);
        }

        ui.add_space(4.0);

        let r = ui.scope(|ui| {
            if is_current {
                ui.visuals_mut().selection.bg_fill =
                    egui::Color32::from_rgb(160, 50, 50);
                ui.visuals_mut().selection.stroke.color = egui::Color32::WHITE;
            }
            ui.selectable_label(is_current, &name)
        }).inner;
        if r.clicked() && matches!(*action, TreeAction::None) {
            *action = TreeAction::Navigate(path.clone());
        }
    });

    if is_expanded {
        if let Some(children) = tree_children.get(path) {
            let children = children.clone();
            for child in &children {
                if !show_hidden {
                    let hidden = child
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map_or(false, |n| n.starts_with('.'));
                    if hidden {
                        continue;
                    }
                }
                show_tree_node(
                    ui,
                    child,
                    depth + 1,
                    viewing_dir,
                    tree_expanded,
                    tree_children,
                    show_hidden,
                    action,
                );
            }
        }
    }
}

/// エクスプローラーのディレクトリスキャン状態
enum ScanState {
    /// アイドル（未スキャン）
    Idle,
    /// バックグラウンドでスキャン中
    Loading {
        dir: PathBuf,
        rx: mpsc::Receiver<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)>,
        started_at: std::time::Instant,
    },
    /// スキャン完了
    Done,
}

/// ツリー展開のバックグラウンドスキャン状態
struct TreeScanPending {
    path: PathBuf,
    rx: mpsc::Receiver<Vec<PathBuf>>,
}

pub struct NekoviewApp {
    config: AppConfig,
    current_dir: PathBuf,
    subdirs: Vec<PathBuf>,
    archives: Vec<PathBuf>,
    tree_root: PathBuf,
    tree_expanded: HashSet<PathBuf>,
    tree_children: HashMap<PathBuf, Vec<PathBuf>>,
    viewing_dir: Option<PathBuf>,
    /// CD/LSディレクトリのサマリーキャッシュ (path, saved_thumbs, total_archives)
    cd_summary: Option<(PathBuf, usize, usize)>,
    /// バックグラウンドで計算中のサマリー結果受信チャンネル
    cd_summary_rx: Option<mpsc::Receiver<(PathBuf, usize, usize)>>,
    cd_summary_updated_at: Option<std::time::Instant>,
    /// 現在ディレクトリの redb キャッシュDB（キャッシュ無効なら None）
    cache_db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
    thumbnails: HashMap<PathBuf, egui::TextureHandle>,
    thumb_req_tx: mpsc::SyncSender<ThumbRequest>,
    thumb_res_rx: mpsc::Receiver<ThumbResult>,
    thumb_pending: HashSet<PathBuf>,
    viewer: Option<ViewerState>,
    drives: Vec<MountEntry>,
    page_cache: PageCache,
    file_cache: FileCache,
    file_cache_req_tx: mpsc::Sender<std::path::PathBuf>,
    file_cache_res_rx: mpsc::Receiver<(std::path::PathBuf, std::sync::Arc<[u8]>)>,
    file_cache_pending: HashSet<PathBuf>,
    req_tx: mpsc::Sender<LoadRequest>,
    res_rx: mpsc::Receiver<LoadResult>,
    pending_loads: HashSet<(PathBuf, usize)>,
    scan_state: ScanState,
    tree_scan_pending: Option<TreeScanPending>,
    /// フレームごとに更新されるウィンドウサイズ（論理ピクセル）
    window_size: (u32, u32),
    /// ビューアがフルスクリーンモードでメインウィンドウを使っている間 true
    viewer_fullscreen_active: bool,
    /// ビューアウィンドウの位置・サイズスロット（viewer と共有して永続化）
    viewer_slots: [Option<WindowSlot>; 4],
    /// archives のうち生画像ファイルのセット（赤枠表示・シングルクリック開封用）
    raw_image_files: std::collections::HashSet<PathBuf>,
    /// 無効確定済みZIP（画像エントリなし）のセット（現ディレクトリセッション中に保持）
    invalid_archives: std::collections::HashSet<PathBuf>,
    /// アプリレベルのトーストメッセージ（3秒で自動消去）
    app_toast: Option<(String, std::time::Instant)>,
    show_hidden: bool,
    sort_key: SortKey,
    sort_ascending: bool,
    selected_archive_index: Option<usize>,
    selected_archive_meta: Option<(std::time::SystemTime, u64)>,
    explorer_cols: usize,
    explorer_scroll_offset: f32,
    explorer_viewport_h: f32,
}

impl NekoviewApp {
    pub fn new(start_dir: PathBuf, config: AppConfig, viewer_slots: [Option<WindowSlot>; 4], sort_state: SortState) -> Self {
        let (req_tx, res_rx) = spawn_worker(config.viewer_filter.to_image_filter(), config.resolved_decode_threads());
        let (thumb_req_tx, thumb_res_rx) = spawn_thumb_worker(config.thumb_filter.to_image_filter(), config.resolved_decode_threads());
        let (file_cache_req_tx, file_cache_res_rx) = spawn_file_cache_worker();
        let (cache_max, cache_min, file_cache_max) = crate::cache::resolve_cache_budgets(config.cache_max_mb, config.file_cache_max_mb);
        let mut drives = list_local_drives();
        drives.extend(list_gvfs_smb_mounts());

        // start_dir を含むドライブのパスをツリーのルートにする
        let tree_root = drives
            .iter()
            .filter(|d| start_dir.starts_with(&d.path))
            .max_by_key(|d| d.path.components().count())
            .map(|d| d.path.clone())
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/"))
            });

        // ツリールートのサブディレクトリをバックグラウンドで取得
        let tree_scan_pending = Some(TreeScanPending {
            path: tree_root.clone(),
            rx: dir::spawn_scan_subdirs(tree_root.clone()),
        });

        let mut app = Self {
            config,
            current_dir: start_dir,
            subdirs: Vec::new(),
            archives: Vec::new(),
            tree_root,
            tree_expanded: HashSet::new(),
            tree_children: HashMap::new(),
            viewing_dir: None,
            cd_summary: None,
            cd_summary_rx: None,
            cd_summary_updated_at: None,
            cache_db: None,
            thumbnails: HashMap::new(),
            thumb_req_tx,
            thumb_res_rx,
            thumb_pending: HashSet::new(),
            viewer: None,
            drives,
            page_cache: PageCache::new(cache_max, cache_min),
            file_cache: FileCache::new(file_cache_max),
            file_cache_req_tx,
            file_cache_res_rx,
            file_cache_pending: HashSet::new(),
            req_tx,
            res_rx,
            pending_loads: HashSet::new(),
            scan_state: ScanState::Idle,
            tree_scan_pending,
            window_size: (1024, 768),
            viewer_fullscreen_active: false,
            viewer_slots,
            raw_image_files: std::collections::HashSet::new(),
            invalid_archives: std::collections::HashSet::new(),
            app_toast: None,
            show_hidden: false,
            sort_key: SortKey::from_state_key(&sort_state.key),
            sort_ascending: sort_state.ascending,
            selected_archive_index: None,
            selected_archive_meta: None,
            explorer_cols: 1,
            explorer_scroll_offset: 0.0,
            explorer_viewport_h: 0.0,
        };
        app.start_scan();
        app
    }

    /// バックグラウンドスキャンを起動する（UIをブロックしない）
    fn start_scan(&mut self) {
        let rx = dir::spawn_scan(self.current_dir.clone());
        self.scan_state = ScanState::Loading {
            dir: self.current_dir.clone(),
            rx,
            started_at: std::time::Instant::now(),
        };
        self.subdirs.clear();
        self.archives.clear();
        self.raw_image_files.clear();
        self.invalid_archives.clear();
        self.cache_db = neko_dir::neko_dir_for(&self.current_dir, &self.config)
            .and_then(|nd| neko_dir::open_cache_db(&nd));
        self.thumbnails.clear();
        self.thumb_pending.clear();
        self.pending_loads.clear();
        self.selected_archive_index = None;
        self.explorer_scroll_offset = 0.0;
    }

    /// フレームごとにスキャン結果をポーリングして反映する
    fn poll_scan(&mut self) {
        let result = match self.scan_state {
            ScanState::Loading { ref dir, ref rx, .. } => {
                // 移動先が変わっていたら古い結果を捨てる
                if *dir != self.current_dir {
                    self.scan_state = ScanState::Idle;
                    return;
                }
                rx.try_recv().ok()
            }
            _ => return,
        };

        if let Some((subdirs, archives, raw_images)) = result {
            self.subdirs = subdirs;
            self.archives = archives.into_iter()
                .filter(|p| {
                    let filename = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    self.cache_db.as_ref()
                        .map_or(true, |db| !neko_dir::is_invalid_and_current(db, filename, p))
                })
                .collect();
            for img in raw_images {
                self.raw_image_files.insert(img.clone());
                self.archives.push(img);
            }
            self.scan_state = ScanState::Done;
            self.sort_archives();
            self.selected_archive_index = if self.archives.is_empty() { None } else { Some(0) };
        }
    }

    /// フレームごとにツリー展開スキャン結果をポーリングして反映する
    fn poll_tree_scan(&mut self) {
        let result = if let Some(ref pending) = self.tree_scan_pending {
            pending.rx.try_recv().ok().map(|subdirs| (pending.path.clone(), subdirs))
        } else {
            return;
        };

        if let Some((path, subdirs)) = result {
            self.tree_children.insert(path.clone(), subdirs);
            // ルートの場合は展開済みにする
            if path == self.tree_root {
                self.tree_expanded.insert(path);
            }
            self.tree_scan_pending = None;
        }
    }

    fn sort_archives(&mut self) {
        let ascending = self.sort_ascending;
        match self.sort_key {
            SortKey::Name => {
                self.archives.sort_by(|a, b| {
                    let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let cmp = na.cmp(nb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            SortKey::Date => {
                self.archives.sort_by(|a, b| {
                    let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
                    let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
                    let cmp = ta.cmp(&tb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            SortKey::Size => {
                self.archives.sort_by(|a, b| {
                    let sa = std::fs::metadata(a).map(|m| m.len()).unwrap_or(0);
                    let sb = std::fs::metadata(b).map(|m| m.len()).unwrap_or(0);
                    let cmp = sa.cmp(&sb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
        }
    }
}

/// cd_summary の計算をバックグラウンドスレッドで行い、受信チャンネルを返す。
fn spawn_summary_worker(
    path: PathBuf,
    db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
) -> mpsc::Receiver<(PathBuf, usize, usize)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let archives = dir::list_archives(&path);
        let raw_images = dir::list_raw_images(&path);
        let total = archives.len() + raw_images.len();
        let saved = db.map(|db| {
            let filenames: Vec<String> = archives.iter().chain(raw_images.iter())
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_owned()))
                .collect();
            neko_dir::count_cached_thumbs(&db, &filenames)
        }).unwrap_or(0);
        let _ = tx.send((path, saved, total));
    });
    rx
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

impl eframe::App for NekoviewApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::config::save_state(&self.current_dir, self.window_size, &self.viewer_slots, &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending });
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ウィンドウサイズを毎フレーム記録
        let rect = ctx.screen_rect();
        self.window_size = (rect.width() as u32, rect.height() as u32);

        // バックグラウンドスキャン結果をポーリング
        self.poll_scan();
        self.poll_tree_scan();

        // サムネイルワーカーからの結果を受信してGPUテクスチャへアップロード
        let was_pending = !self.thumb_pending.is_empty();
        let thumb_results: Vec<ThumbResult> =
            std::iter::from_fn(|| self.thumb_res_rx.try_recv().ok()).collect();
        for result in thumb_results {
            self.thumb_pending.remove(&result.path);
            if let Some(rgba) = result.rgba {
                if self.archives.contains(&result.path) {
                    let name = result.path.display().to_string();
                    let tex = upload_texture(ctx, &name, &rgba);
                    self.thumbnails.insert(result.path, tex);
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
                if let Some((ref cd_path, _, _)) = self.cd_summary {
                    let path = cd_path.clone();
                    self.cd_summary_rx = Some(spawn_summary_worker(path, self.cache_db.clone()));
                }
            }
        }

        // FileCache ワーカーからの結果を受信して横キャッシュへ投入
        let file_results: Vec<(PathBuf, std::sync::Arc<[u8]>)> =
            std::iter::from_fn(|| self.file_cache_res_rx.try_recv().ok()).collect();
        let cur_viewer_path = self.viewer.as_ref().map(|v| v.archive_path.clone());
        for (path, bytes) in file_results {
            self.file_cache_pending.remove(&path);
            let current = cur_viewer_path.clone().unwrap_or_else(|| path.clone());
            self.file_cache.insert(path, bytes, &current, &self.archives);
        }

        // ワーカーからの結果を PageCache へ投入
        let results: Vec<LoadResult> =
            std::iter::from_fn(|| self.res_rx.try_recv().ok()).collect();
        let (cur_path, cur_idx) = self
            .viewer
            .as_ref()
            .map(|v| {
                let sorted_lo = v.spread_lo().max(0) as usize;
                let orig = if sorted_lo < v.entries.len() {
                    v.entries[sorted_lo].original_index
                } else {
                    0
                };
                (v.archive_path.clone(), orig)
            })
            .unwrap_or_default();
        for result in results {
            self.pending_loads
                .remove(&(result.archive_path.clone(), result.index));
            self.page_cache.insert(
                result.archive_path,
                result.index,
                result.content,
                &cur_path,
                cur_idx,
            );
        }

        // スライディングウィンドウ: ビューア表示中に前後ページを先読み
        if let Some(viewer) = &self.viewer {
            let cur = viewer.spread_lo().max(0) as usize;
            let path = viewer.archive_path.clone();
            let entries = viewer.entries.clone();
            let total = entries.len();
            let start = cur.saturating_sub(5);
            let end = (cur + 10 + 1).min(total);
            for i in start..end {
                let orig_i = entries[i].original_index;
                let key = (path.clone(), orig_i);
                if !self.page_cache.contains(&path, orig_i) && !self.pending_loads.contains(&key) {
                    let file_bytes = self.file_cache.get(&path);
                    let _ = self.req_tx.send(LoadRequest {
                        archive_path: path.clone(),
                        index: orig_i,
                        entry_name: entries[i].entry_name.clone(),
                        is_raw_file: viewer.is_raw_file,
                        file_bytes,
                    });
                    self.pending_loads.insert(key);
                }
            }
        }

        // ── フルスクリーンビューア: メインウィンドウで直接描画 ─────────────────
        // show_viewport_immediate はセカンダリウィンドウが親を覆うと update() が
        // 止まる問題があるため、fullscreen 中はメインウィンドウ (ROOT viewport) で
        // ビューアを描画して独立したイベントループを維持する。
        let viewer_wants_fullscreen = self.viewer.as_ref().map_or(false, |v| v.fullscreen);

        // セカンダリウィンドウを常に維持し、フルスクリーン中は with_visible(false) で
        // 不可視にする。create/destroy サイクルが Win32 の マウスイベント配信を
        // 壊すのを防ぐため。不可視ウィンドウはフォーカスを奪わない。
        {
            let visible = !viewer_wants_fullscreen;
            let page_cache = &self.page_cache;
            let mut viewer_should_close = false;
            let mut viewport_nav = ViewerNav::None;
            if let Some(viewer) = &mut self.viewer {
                let vp_builder = {
                    let b = egui::ViewportBuilder::default().with_visible(visible);
                    if viewer.first_frame && visible { b.with_inner_size([800.0, 600.0]) } else { b }
                };
                if visible { viewer.first_frame = false; }
                ctx.show_viewport_immediate(
                    egui::ViewportId::from_hash_of("viewer_window"),
                    vp_builder,
                    |vp_ctx, _class| {
                        if visible {
                            viewport_nav = viewer.show(vp_ctx, page_cache);
                        } else {
                            egui::CentralPanel::default().show(vp_ctx, |_| {});
                        }
                    },
                );
                if visible && viewer.save_requested {
                    viewer.save_requested = false;
                    self.viewer_slots = viewer.slots;
                    crate::config::save_state(&self.current_dir, self.window_size, &self.viewer_slots, &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending });
                }
                if visible { viewer_should_close = !viewer.open; }
            }
            if viewer_should_close {
                self.viewer = None;
            }
            self.handle_viewer_nav(viewport_nav);
        }
        if viewer_wants_fullscreen {
            if !self.viewer_fullscreen_active {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                self.viewer_fullscreen_active = true;
            }
            let page_cache = &self.page_cache;
            let (viewer_closed, exited_fullscreen, fs_nav) = if let Some(viewer) = &mut self.viewer {
                let nav = viewer.show(ctx, page_cache);
                if viewer.save_requested {
                    viewer.save_requested = false;
                    self.viewer_slots = viewer.slots;
                    crate::config::save_state(&self.current_dir, self.window_size, &self.viewer_slots, &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending });
                }
                (!viewer.open, !viewer.fullscreen, nav)
            } else {
                (true, true, ViewerNav::None)
            };
            if viewer_closed || exited_fullscreen {
                self.viewer_fullscreen_active = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("Nekoview".to_owned()));
            }
            if viewer_closed {
                self.viewer = None;
            }
            self.handle_viewer_nav(fs_nav);
            ctx.request_repaint();
            return;
        }
        // フルスクリーンからの復帰フレームでメインウィンドウを元に戻す
        if self.viewer_fullscreen_active {
            self.viewer_fullscreen_active = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Title("Nekoview".to_owned()));
        }

        // ── エクスプローラー キーナビゲーション ─────────────────────────────
        {
            let total = self.archives.len();
            let cols = self.explorer_cols.max(1);
            let cell_h = self.config.thumb_size as f32;
            const KEY_GAP: f32 = 8.0;
            if total > 0 {
                let prev = self.selected_archive_index;
                ctx.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                        if let Some(idx) = self.selected_archive_index {
                            if idx + 1 < total {
                                self.selected_archive_index = Some(idx + 1);
                            }
                        }
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                        if let Some(idx) = self.selected_archive_index {
                            if idx > 0 {
                                self.selected_archive_index = Some(idx - 1);
                            }
                        }
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        if let Some(idx) = self.selected_archive_index {
                            let current_row = idx / cols;
                            let last_row = (total - 1) / cols;
                            if current_row < last_row {
                                self.selected_archive_index = Some((idx + cols).min(total - 1));
                            }
                        }
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        if let Some(idx) = self.selected_archive_index {
                            if idx >= cols {
                                self.selected_archive_index = Some(idx - cols);
                            }
                        }
                    }
                });
                if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Some(idx) = self.selected_archive_index {
                        if let Some(path) = self.archives.get(idx).cloned() {
                            self.pending_loads.clear();
                            self.viewer = if self.raw_image_files.contains(&path) {
                                Some(ViewerState::new_raw(path.clone(), self.viewer_slots))
                            } else {
                                ViewerState::new(path.clone(), self.viewer_slots)
                            };
                            self.ensure_file_cached(path);
                        }
                    }
                }
                // 選択が変わったらコンテンツ空間で最小スクロールを計算（アニメーションなし）
                if self.selected_archive_index != prev {
                    self.selected_archive_meta = self.selected_archive_index
                        .and_then(|idx| self.archives.get(idx))
                        .and_then(|path| std::fs::metadata(path).ok())
                        .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
                    if let Some(idx) = self.selected_archive_index {
                        let row = idx / cols;
                        let item_top = row as f32 * (cell_h + KEY_GAP);
                        let item_bottom = item_top + cell_h;
                        let vp = self.explorer_viewport_h;
                        if item_top < self.explorer_scroll_offset {
                            self.explorer_scroll_offset = item_top;
                        } else if vp > 0.0 && item_bottom > self.explorer_scroll_offset + vp {
                            self.explorer_scroll_offset = item_bottom - vp;
                        }
                    }
                }
            }
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let hidden_label = if self.show_hidden { "[隠 ON]" } else { "[隠OFF]" };
                if ui.selectable_label(self.show_hidden, hidden_label).clicked() {
                    self.show_hidden = !self.show_hidden;
                }

                ui.separator();

                // ── ページ表示モード ──────────────────────────────────────────
                let viewer_open = self.viewer.is_some();
                let is_raw_viewer = self.viewer.as_ref().map_or(false, |v| v.is_raw_file);
                let cur_mode = self.viewer.as_ref().map(|v| v.page_mode);
                let is_spread = cur_mode.map_or(false, |m| m != PageMode::Single);

                ui.add_enabled_ui(viewer_open, |ui| {
                    if ui.selectable_label(cur_mode == Some(PageMode::Single), "[単ページ]").clicked() {
                        if let Some(v) = &mut self.viewer { v.set_page_mode(PageMode::Single); }
                    }
                });
                ui.add_enabled_ui(viewer_open && !is_raw_viewer, |ui| {
                    if ui.selectable_label(cur_mode == Some(PageMode::SpreadLeft), "[見開き左]").clicked() {
                        if let Some(v) = &mut self.viewer { v.set_page_mode(PageMode::SpreadLeft); }
                    }
                    if ui.selectable_label(cur_mode == Some(PageMode::SpreadRight), "[見開き右]").clicked() {
                        if let Some(v) = &mut self.viewer { v.set_page_mode(PageMode::SpreadRight); }
                    }
                });

                ui.add_enabled_ui(viewer_open && is_spread && !is_raw_viewer, |ui| {
                    let can_back = self.viewer.as_ref().map_or(false, |v| v.can_shift_backward());
                    let can_fwd  = self.viewer.as_ref().map_or(false, |v| v.can_shift_forward());

                    if ui.add_enabled(can_back, egui::Button::new("[1P戻す]")).clicked() {
                        if let Some(v) = &mut self.viewer { v.shift_offset_backward(); }
                    }
                    if ui.add_enabled(can_fwd, egui::Button::new("[1P進む]")).clicked() {
                        if let Some(v) = &mut self.viewer { v.shift_offset_forward(); }
                    }
                    let is_offset = self.viewer.as_ref().map_or(false, |v| v.is_spread_offset());
                    ui.label(if is_offset { "+1Pずれ中" } else { "整列中" });
                });

                ui.separator();

                // ── エクスプローラーソート ────────────────────────────────────
                let mut sort_changed = false;
                for key in [SortKey::Name, SortKey::Date, SortKey::Size] {
                    let active = self.sort_key == key;
                    let clicked = ui.scope(|ui| {
                        if active {
                            ui.visuals_mut().selection.bg_fill =
                                egui::Color32::from_rgb(30, 100, 200);
                            ui.visuals_mut().selection.stroke.color = egui::Color32::WHITE;
                        }
                        ui.selectable_label(active, key.label()).clicked()
                    }).inner;
                    if clicked {
                        self.sort_key = key;
                        sort_changed = true;
                    }
                }

                ui.label(":");

                let order_label = if self.sort_ascending { "[昇順]" } else { "[降順]" };
                if ui.button(order_label).clicked() {
                    self.sort_ascending = !self.sort_ascending;
                    sort_changed = true;
                }

                if sort_changed {
                    self.sort_archives();
                }

                // ── メモリ情報ボタン（右端） ──────────────────────────────────
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = ui.button("[?]");
                    if btn.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(egui::Id::new("cache_info_popup")));
                    }
                    egui::popup_above_or_below_widget(
                        ui,
                        egui::Id::new("cache_info_popup"),
                        &btn,
                        egui::AboveOrBelow::Below,
                        egui::PopupCloseBehavior::CloseOnClickOutside,
                        |ui| {
                            ui.set_min_width(200.0);
                            let used = self.page_cache.total_bytes();
                            let max  = self.page_cache.max_bytes();
                            let used_mb = used / (1024 * 1024);
                            let max_mb  = max  / (1024 * 1024);
                            ui.label(format!("キャッシュ使用量: {} MB / {} MB", used_mb, max_mb));
                        },
                    );
                });
            });
        });

        egui::SidePanel::left("folder_panel")
            .min_width(200.0)
            .max_width(200.0)
            .frame({
                let mut f = egui::Frame::side_top_panel(ctx.style().as_ref());
                f.inner_margin.right = 0.0;
                f
            })
            .show(ctx, |ui| {
                // 下部に確保する高さ（ドライブ数に応じて可変）
                let drive_rows = self.drives.len() as f32;
                let bottom_h = (drive_rows * 24.0 + 44.0).min(200.0); // heading+sep+rows
                let top_h = (ui.available_height() - bottom_h - 8.0).max(40.0);

                // ── 上部: ディレクトリツリー ──
                let mut tree_action = TreeAction::None;
                egui::ScrollArea::both()
                    .id_salt("folder_scroll")
                    .max_height(top_h)
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        show_tree_node(
                            ui,
                            &self.tree_root.clone(),
                            0,
                            &self.viewing_dir,
                            &self.tree_expanded,
                            &self.tree_children,
                            self.show_hidden,
                            &mut tree_action,
                        );
                    });

                match tree_action {
                    TreeAction::None => {}
                    TreeAction::ToggleExpand(path) => {
                        if self.tree_expanded.contains(&path) {
                            self.tree_expanded.remove(&path);
                        } else {
                            self.tree_expanded.insert(path.clone());
                            if !self.tree_children.contains_key(&path) {
                                // バックグラウンドでサブディレクトリを取得
                                self.tree_scan_pending = Some(TreeScanPending {
                                    path: path.clone(),
                                    rx: dir::spawn_scan_subdirs(path),
                                });
                            }
                        }
                    }
                    TreeAction::Navigate(path) => {
                        self.current_dir = path.clone();
                        self.viewing_dir = Some(path.clone());
                        self.start_scan(); // cache_db をここで確定させてから clone して渡す
                        self.cd_summary_rx = Some(spawn_summary_worker(path.clone(), self.cache_db.clone()));
                        crate::config::save_state(&self.current_dir, self.window_size, &self.viewer_slots, &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending });
                    }
                }

                ui.separator();

                // ── 下部: ドライブ選択 ──
                ui.small("ドライブ");
                egui::ScrollArea::vertical()
                    .id_salt("drive_scroll")
                    .auto_shrink([false, true])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        let drives: Vec<_> = self.drives
                            .iter()
                            .map(|d| (d.label.clone(), d.path.clone()))
                            .collect();
                        for (label, path) in drives {
                            let selected = self.tree_root == path;
                            if ui.selectable_label(selected, &label).clicked() {
                                self.current_dir = path.clone();
                                self.start_scan();
                                // ツリーのルートをドライブに切り替え
                                self.tree_root = path.clone();
                                self.tree_expanded.clear();
                                self.tree_children.clear();
                                self.viewing_dir = None;
                                self.cd_summary = None;
                                self.cd_summary_rx = None;
                                // ドライブルートのサブディレクトリをバックグラウンドで取得
                                self.tree_scan_pending = Some(TreeScanPending {
                                    path: path.clone(),
                                    rx: dir::spawn_scan_subdirs(path),
                                });
                                crate::config::save_state(&self.current_dir, self.window_size, &self.viewer_slots, &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending });
                            }
                        }
                    });
            });

        egui::CentralPanel::default()
            .frame({
                let mut f = egui::Frame::central_panel(ctx.style().as_ref());
                f.inner_margin.right = 0.0;
                f
            })
            .show(ctx, |ui| {
            ui.label(self.current_dir.display().to_string());

            // CD/LS状態: ディレクトリのサマリーを表示
            if let Some((cd_path, saved, total)) = &self.cd_summary {
                let dir_name = cd_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?");
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("▶ {dir_name}"))
                            .color(ui.visuals().selection.bg_fill),
                    );
                    ui.label(
                        egui::RichText::new(format!("サムネ保存: {saved} / {total}"))
                            .color(egui::Color32::GRAY),
                    );
                });
            }

            if let Some((mtime, size_bytes)) = &self.selected_archive_meta {
                let filename = self.selected_archive_index
                    .and_then(|idx| self.archives.get(idx))
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                ui.separator();
                let mb = *size_bytes as f64 / (1024.0 * 1024.0);
                let date_str = format_mtime(*mtime);
                ui.label(format!("更新日時:{date_str}   ファイルサイズ：{:.1}MB   {filename}", mb));
            }

            ui.separator();

            {
                // ローディング中は 0.5秒経過後にスピナーを表示（短いアクセスのチラツキ防止）
                let is_loading = matches!(&self.scan_state, ScanState::Loading { started_at, .. }
                    if started_at.elapsed().as_secs_f32() > 0.5);

                if is_loading {
                    ui.centered_and_justified(|ui| {
                        ui.label("読み込み中...");
                    });
                } else {
                    // ── LS状態: サムネグリッド ──
                    let cell_h = self.config.thumb_size as f32;
                    let cell_w = (cell_h / std::f32::consts::SQRT_2).round();
                    const GAP: f32 = 8.0;
                    let avail_w = ui.available_width();
                    let full_cols = ((avail_w + GAP) / (cell_w + GAP)).floor() as usize;
                    let used_w = full_cols as f32 * (cell_w + GAP) - GAP;
                    let cols = if avail_w - used_w >= cell_w / 2.0 { full_cols + 1 } else { full_cols }.max(1);
                    self.explorer_cols = cols;

                    let output = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                            .vertical_scroll_offset(self.explorer_scroll_offset)
                            .show(ui, |ui| {
                        egui::Grid::new("archive_grid")
                            .num_columns(cols)
                            .spacing([GAP, GAP])
                            .show(ui, |ui| {
                                let archives = self.archives.clone();
                                for (i, path) in archives.iter().enumerate() {
                                    let is_selected = self.selected_archive_index == Some(i);
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(cell_w, cell_h),
                                        egui::Sense::click(),
                                    );

                                    if ui.is_rect_visible(rect) {
                                        if let Some(tex) = self.thumbnails.get(path) {
                                            ui.painter().image(
                                                tex.id(),
                                                rect,
                                                egui::Rect::from_min_max(
                                                    egui::pos2(0.0, 0.0),
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                egui::Color32::WHITE,
                                            );
                                        } else {
                                            ui.painter().rect_filled(
                                                rect,
                                                4.0,
                                                egui::Color32::from_gray(60),
                                            );
                                            if !self.thumb_pending.contains(path) {
                                                if self.thumb_req_tx.try_send(ThumbRequest {
                                                    archive_path: path.clone(),
                                                    db: self.cache_db.clone(),
                                                    is_raw_file: self.raw_image_files.contains(path),
                                                }).is_ok() {
                                                    self.thumb_pending.insert(path.clone());
                                                }
                                            }
                                        }

                                        // 無効ZIPは左上に赤Xを描画
                                        if self.invalid_archives.contains(path) {
                                            let x_size = 16.0;
                                            let origin = rect.min + egui::vec2(4.0, 4.0);
                                            let end = origin + egui::vec2(x_size, x_size);
                                            let stroke = egui::Stroke::new(2.5, egui::Color32::from_rgb(220, 50, 50));
                                            ui.painter().line_segment([origin, end], stroke);
                                            ui.painter().line_segment(
                                                [egui::pos2(end.x, origin.y), egui::pos2(origin.x, end.y)],
                                                stroke,
                                            );
                                        }

                                        // 選択中アイテムを枠で囲む（生ファイルは赤、ZIPは青）
                                        if is_selected {
                                            let is_raw = self.raw_image_files.contains(path);
                                            let stroke_color = if is_raw {
                                                egui::Color32::from_rgb(220, 60, 60)
                                            } else {
                                                egui::Color32::from_rgb(50, 120, 230)
                                            };
                                            ui.painter().rect_stroke(
                                                rect,
                                                0.0,
                                                egui::Stroke::new(2.0, stroke_color),
                                            );
                                        }
                                    }

                                    let is_raw = self.raw_image_files.contains(path);
                                    if response.clicked() {
                                        if is_raw && self.selected_archive_index == Some(i) {
                                            // 生ファイル: 選択済み状態のシングルクリックで開く
                                            self.pending_loads.clear();
                                            self.viewer = Some(ViewerState::new_raw(path.clone(), self.viewer_slots));
                                            self.ensure_file_cached(path.clone());
                                        } else {
                                            self.selected_archive_index = Some(i);
                                            self.selected_archive_meta = std::fs::metadata(path)
                                                .ok()
                                                .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
                                        }
                                    }
                                    if response.double_clicked() && !is_raw {
                                        if self.invalid_archives.contains(path) {
                                            let name = truncate_filename(path);
                                            self.app_toast = Some((
                                                format!("「{name}」は画像が含まれない無効なZIPです。表示できません"),
                                                std::time::Instant::now(),
                                            ));
                                        } else {
                                            self.pending_loads.clear();
                                            match ViewerState::new(path.clone(), self.viewer_slots) {
                                                Some(state) => {
                                                    self.viewer = Some(state);
                                                    self.ensure_file_cached(path.clone());
                                                }
                                                None => {
                                                    let p = path.clone();
                                                    self.mark_archive_invalid(&p);
                                                    let name = truncate_filename(path);
                                                    self.app_toast = Some((
                                                        format!("「{name}」は画像が含まれない無効なZIPです。表示できません"),
                                                        std::time::Instant::now(),
                                                    ));
                                                }
                                            }
                                        }
                                    }

                                    if (i + 1) % cols == 0 {
                                        ui.end_row();
                                    }
                                }
                                if !archives.is_empty() && archives.len() % cols != 0 {
                                    ui.end_row();
                                }
                            });
                    });
                    // ユーザーの手動スクロールを読み戻してストアを更新
                    self.explorer_scroll_offset = output.state.offset.y;
                    self.explorer_viewport_h = output.inner_rect.height();
                }
            }
        });

        // アプリレベルトースト（3秒で自動消去）
        if let Some((ref msg, since)) = self.app_toast.clone() {
            if since.elapsed().as_secs_f32() < 3.0 {
                egui::Area::new(egui::Id::new("app_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -30.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(egui::Color32::from_rgba_premultiplied(30, 30, 30, 230))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(msg)
                                        .color(egui::Color32::WHITE)
                                        .size(13.0),
                                );
                            });
                    });
                ctx.request_repaint();
            } else {
                self.app_toast = None;
            }
        }

        // 未完了の処理がある間はワーカー結果を取りこぼさないよう次フレームを要求する
        // ビューアが開いている間も続ける（フルスクリーン時の入力取りこぼし防止）
        if !self.thumb_pending.is_empty() || !self.pending_loads.is_empty()
            || self.viewer.is_some()
            || matches!(self.scan_state, ScanState::Loading { .. })
            || self.tree_scan_pending.is_some()
        {
            ctx.request_repaint();
        }
    }
}

impl NekoviewApp {
    /// ZIPを無効確定してDBにマーカーを書き込む
    fn mark_archive_invalid(&mut self, path: &PathBuf) {
        self.invalid_archives.insert(path.clone());
        if let Some(ref db) = self.cache_db {
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let mtime = neko_dir::file_mtime(path);
            neko_dir::mark_invalid(db, filename, mtime);
        }
    }

    /// direction(+1/-1) 方向に from_idx から次の有効ファイルを探す。
    /// キャッシュ済み無効ZIPはスキップ、未判定は ViewerState::new() で確認して無効なら登録しスキップ。
    fn find_next_valid(&mut self, from_idx: usize, direction: i32) -> Option<(usize, ViewerState)> {
        let total = self.archives.len() as i32;
        let mut idx = from_idx as i32 + direction;
        loop {
            if idx < 0 || idx >= total {
                return None;
            }
            let path = self.archives[idx as usize].clone();
            if self.raw_image_files.contains(&path) {
                return Some((idx as usize, ViewerState::new_raw(path, self.viewer_slots)));
            }
            if self.invalid_archives.contains(&path) {
                idx += direction;
                continue;
            }
            match ViewerState::new(path.clone(), self.viewer_slots) {
                Some(state) => return Some((idx as usize, state)),
                None => {
                    self.mark_archive_invalid(&path);
                    idx += direction;
                }
            }
        }
    }

    fn handle_viewer_nav(&mut self, nav: ViewerNav) {
        match nav {
            ViewerNav::None => {}
            ViewerNav::PrevFile => {
                if let Some(from) = self.selected_archive_index {
                    if let Some((idx, state)) = self.find_next_valid(from, -1) {
                        let path = state.archive_path.clone();
                        self.selected_archive_index = Some(idx);
                        self.pending_loads.clear();
                        self.viewer = Some(state);
                        self.ensure_file_cached(path);
                    } else if let Some(v) = &mut self.viewer {
                        v.set_toast("これ以上開けるファイルは前方に存在しません".to_string());
                    }
                }
            }
            ViewerNav::NextFile => {
                if let Some(from) = self.selected_archive_index {
                    if let Some((idx, state)) = self.find_next_valid(from, 1) {
                        let path = state.archive_path.clone();
                        self.selected_archive_index = Some(idx);
                        self.pending_loads.clear();
                        self.viewer = Some(state);
                        self.ensure_file_cached(path);
                    } else if let Some(v) = &mut self.viewer {
                        v.set_toast("これ以上開けるファイルは後方に存在しません".to_string());
                    }
                }
            }
        }
    }

    /// ファイルが FileCache 未登録かつ未リクエストの場合にバックグラウンド読み込みを起動する。
    fn ensure_file_cached(&mut self, path: PathBuf) {
        if !self.file_cache.contains(&path) && !self.file_cache_pending.contains(&path) {
            let _ = self.file_cache_req_tx.send(path.clone());
            self.file_cache_pending.insert(path);
        }
    }
}

fn truncate_filename(path: &std::path::Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    const MAX: usize = 24;
    if name.chars().count() <= MAX {
        name.to_string()
    } else {
        let s: String = name.chars().take(MAX - 3).collect();
        format!("{s}...")
    }
}

fn format_mtime(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (secs / 86400) as i64 + 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}/{:02}/{:02}", y, m, d)
}
