use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use crate::cache::{FileCache, LoadRequest, LoadResult, PageCache, ThumbRequest, ThumbResult, spawn_worker, spawn_thumb_worker, spawn_file_cache_worker};
use crate::config::{AppConfig, ResizeFilter, SortState, ViewerConfig, WindowSlot, filter_to_str};
use crate::controller::{self, ViewerNav};
use crate::i18n;
use crate::model::ExplorerSortKey;
use crate::neko_dir;
use crate::fs::{archive, dir, mount::{list_gvfs_smb_mounts, list_local_drives, MountEntry}};
use crate::view_reader::{PageMode, ViewerState};

impl ExplorerSortKey {
    fn label(self) -> &'static str {
        let t = i18n::t();
        match self {
            Self::Name => t.sort_name(),
            Self::Date => t.sort_date(),
            Self::Size => t.sort_size(),
        }
    }

    fn as_state_key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Date => "date",
            Self::Size => "size",
        }
    }

    fn from_state_key(s: &str) -> Self {
        match s {
            "date" => Self::Date,
            "size" => Self::Size,
            _ => Self::Name,
        }
    }
}

enum TreeAction {
    None,
    ToggleExpand(PathBuf),
    Navigate(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Common,
    Anim,
    Static,
    Other,
}

/// 設定ダイアログの編集用下書き。[反映]を押すまでは AppConfig/ViewerConfig 本体には
/// 一切書き戻さない（自由にタイプ・切り替えさせるための一時バッファ）。
struct SettingsDraft {
    redecode_on_resize: bool,
    debounce_ms: u64,
    cache_max_mb_text: String,
    file_cache_max_mb_text: String,
    max_decode_edge_text: String,
    viewer_filter: ResizeFilter,
    thumb_filter: ResizeFilter,
    lang: i18n::Lang,
    ring_min_text: String,
    ring_max_text: String,
}

impl SettingsDraft {
    fn from_current(config: &AppConfig, viewer_cfg: &ViewerConfig) -> Self {
        Self {
            redecode_on_resize: viewer_cfg.redecode_on_resize,
            debounce_ms: viewer_cfg.resize_debounce_ms,
            cache_max_mb_text: config.cache_max_mb.map(|v| v.to_string()).unwrap_or_default(),
            file_cache_max_mb_text: config.file_cache_max_mb.map(|v| v.to_string()).unwrap_or_default(),
            max_decode_edge_text: config.max_decode_edge.to_string(),
            viewer_filter: config.viewer_filter,
            thumb_filter: config.thumb_filter,
            lang: i18n::t(),
            ring_min_text: config.anim_ring_min_frames.to_string(),
            ring_max_text: config.anim_ring_max_frames.to_string(),
        }
    }

    /// [反映]クリック時に実際の設定へ書き戻す。数値が空欄/不正な場合は元の値を維持する。
    fn apply_to(&self, config: &mut AppConfig, viewer_cfg: &mut ViewerConfig) {
        viewer_cfg.redecode_on_resize = self.redecode_on_resize;
        viewer_cfg.resize_debounce_ms = self.debounce_ms;
        config.cache_max_mb = parse_optional_u64(&self.cache_max_mb_text);
        config.file_cache_max_mb = parse_optional_u64(&self.file_cache_max_mb_text);
        config.max_decode_edge = parse_clamped(&self.max_decode_edge_text, config.max_decode_edge, 256, 16384);
        config.viewer_filter = self.viewer_filter;
        config.thumb_filter = self.thumb_filter;
        i18n::set(self.lang);

        let mut min = parse_clamped(&self.ring_min_text, config.anim_ring_min_frames, 1, 256);
        let mut max = parse_clamped(&self.ring_max_text, config.anim_ring_max_frames, 1, 256);
        if min > max {
            std::mem::swap(&mut min, &mut max);
        }
        config.anim_ring_min_frames = min;
        config.anim_ring_max_frames = max;
    }
}

fn parse_optional_u64(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() { None } else { t.parse().ok() }
}

/// 空欄/不正値は `fallback` を維持しつつ、[min, max] にクランプする。
fn parse_clamped<T: std::str::FromStr + Ord + Copy>(s: &str, fallback: T, min: T, max: T) -> T {
    s.trim().parse::<T>().ok().map(|v| v.clamp(min, max)).unwrap_or(fallback)
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
    viewer: Arc<Mutex<Option<ViewerState>>>,
    /// ファイル切替後も維持するビューア設定（zoom・fullscreen 等）
    viewer_cfg: Arc<Mutex<ViewerConfig>>,
    drives: Vec<MountEntry>,
    page_cache: Arc<Mutex<PageCache>>,
    file_cache: FileCache,
    file_cache_req_tx: mpsc::Sender<std::path::PathBuf>,
    file_cache_res_rx: mpsc::Receiver<(std::path::PathBuf, std::sync::Arc<[u8]>)>,
    file_cache_pending: HashSet<PathBuf>,
    req_tx: mpsc::Sender<LoadRequest>,
    res_rx: Arc<Mutex<mpsc::Receiver<LoadResult>>>,
    pending_loads: Arc<Mutex<HashSet<(PathBuf, usize)>>>,
    scan_state: ScanState,
    tree_scan_pending: Option<TreeScanPending>,
    /// フレームごとに更新されるウィンドウサイズ（論理ピクセル）
    window_size: (u32, u32),
    /// ビューアウィンドウの位置・サイズスロット（viewer と共有して永続化）
    viewer_slots: [Option<WindowSlot>; 4],
    /// archives のうち生画像ファイルのセット（赤枠表示・シングルクリック開封用）
    raw_image_files: std::collections::HashSet<PathBuf>,
    /// 無効確定済みZIP（画像エントリなし）のセット（現ディレクトリセッション中に保持）
    invalid_archives: std::collections::HashSet<PathBuf>,
    /// アプリレベルのトーストメッセージ（3秒で自動消去）
    app_toast: Option<(String, std::time::Instant)>,
    /// フェーズ2: ページキャッシュ予算（見積もりゲートの閾値。resolve_cache_budgetsのpage_max）
    cache_budget_bytes: usize,
    /// フェーズ4: アニメリングバッファ先読み枚数の(下限, 上限)。見積もりゲートも同じ値を使う。
    anim_ring_bounds: (usize, usize),
    /// フェーズ2: メモリ見積もり超過を知らせる確認ダイアログの表示状態
    memory_warning_open: bool,
    /// 設定ダイアログの表示状態・選択中タブ・編集用下書き
    settings_open: bool,
    settings_tab: SettingsTab,
    settings_draft: SettingsDraft,
    /// ビューアウィンドウをフォーカス前面に出すフラグ
    viewer_focus_requested: bool,
    show_hidden: bool,
    sort_key: ExplorerSortKey,
    sort_ascending: bool,
    selected_archive_index: Option<usize>,
    selected_archive_meta: Option<(std::time::SystemTime, u64)>,
    explorer_cols: usize,
    explorer_scroll_offset: f32,
    explorer_viewport_h: f32,
    /// ステータスウィンドウ表示フラグ（[?] ボタンでトグル）
    show_status_window: bool,
    status_window_data: Arc<Mutex<crate::view_status::StatusData>>,
    /// ステータスデータを最後に更新した時刻（1秒間隔制御用）
    last_status_update: std::time::Instant,
    /// 各 View から controller 経由でセットされる即時更新要求フラグ
    status_update_requested: Arc<std::sync::atomic::AtomicBool>,
    /// バックグラウンドワーカーから ROOT を起こす（イベント駆動再描画）ために保持する ctx
    egui_ctx: egui::Context,
    /// フェーズ6: viewer_cfg.redecode_trigger_seq のうち処理済みの値（変化検知用）
    resize_redecode_last_seq: u64,
    /// フェーズ6: デバウンス期限（この時刻を過ぎたら再デコード発火）。None = 待ち無し
    resize_redecode_deadline: Option<std::time::Instant>,
    /// フェーズ6: 直近の再デコードで決まった、以降のデコード要求(先読み含む)に使うターゲットサイズ。
    /// None = 無制限(原寸、zoom_actual時)。起動直後の既定値は従来の固定上限と同じ。
    decode_target: Option<(u32, u32)>,
}

impl NekoviewApp {
    pub fn new(start_dir: PathBuf, config: AppConfig, viewer_slots: [Option<WindowSlot>; 4], sort_state: SortState, viewer_cfg: ViewerConfig, ctx: egui::Context) -> Self {
        let (cache_max, cache_min, file_cache_max) = crate::cache::resolve_cache_budgets(config.cache_max_mb, config.file_cache_max_mb);
        let ring_bounds = (config.anim_ring_min_frames, config.anim_ring_max_frames);
        let frame_hard_limit_bytes = config.anim_frame_hard_limit_mb * 1024 * 1024;
        // 長辺px上限のみ指定し、正方形の箱として resize_for_display に渡す。
        // fit-within(縦横比維持)なので短辺は箱の中に自動的に収まる。
        let max_decode_target = (config.max_decode_edge, config.max_decode_edge);
        let settings_draft = SettingsDraft::from_current(&config, &viewer_cfg);
        let (req_tx, res_rx) = spawn_worker(config.viewer_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone(), cache_max, ring_bounds, frame_hard_limit_bytes);
        let (thumb_req_tx, thumb_res_rx) = spawn_thumb_worker(config.thumb_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone());
        let (file_cache_req_tx, file_cache_res_rx) = spawn_file_cache_worker(ctx.clone());
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
            rx: dir::spawn_scan_subdirs(tree_root.clone(), {
                let c = ctx.clone();
                move || c.request_repaint()
            }),
        });

        // 段階5: 旧 1Hz ティッカースレッド＋ROOT 外部ウェイクは撤去。debug のステータス窓は
        // 独立 OS 窓になり、render_status 内の request_repaint_after(1s) で自分自身を 1Hz で
        // 起こし続ける（winit ループがその予定で WaitUntil する）。

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
            viewer: Arc::new(Mutex::new(None)),
            viewer_cfg: Arc::new(Mutex::new(viewer_cfg)),
            drives,
            page_cache: Arc::new(Mutex::new(PageCache::new(cache_max, cache_min))),
            file_cache: FileCache::new(file_cache_max),
            file_cache_req_tx,
            file_cache_res_rx,
            file_cache_pending: HashSet::new(),
            req_tx,
            res_rx: Arc::new(Mutex::new(res_rx)),
            pending_loads: Arc::new(Mutex::new(HashSet::new())),
            scan_state: ScanState::Idle,
            tree_scan_pending,
            window_size: (1024, 768),
            viewer_slots,
            raw_image_files: std::collections::HashSet::new(),
            invalid_archives: std::collections::HashSet::new(),
            app_toast: None,
            cache_budget_bytes: cache_max,
            anim_ring_bounds: ring_bounds,
            memory_warning_open: false,
            settings_open: false,
            settings_tab: SettingsTab::Common,
            settings_draft,
            viewer_focus_requested: false,
            show_hidden: false,
            sort_key: ExplorerSortKey::from_state_key(&sort_state.key),
            sort_ascending: sort_state.ascending,
            selected_archive_index: None,
            selected_archive_meta: None,
            explorer_cols: 1,
            explorer_scroll_offset: 0.0,
            explorer_viewport_h: 0.0,
            show_status_window: false,
            status_window_data: Arc::new(Mutex::new(crate::view_status::StatusData::default())),
            last_status_update: std::time::Instant::now(),
            status_update_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            egui_ctx: ctx,
            resize_redecode_last_seq: viewer_cfg.redecode_trigger_seq,
            resize_redecode_deadline: None,
            decode_target: Some(max_decode_target),
        };
        app.start_scan();
        app
    }

    /// バックグラウンドスキャンを起動する（UIをブロックしない）
    fn start_scan(&mut self) {
        let rx = dir::spawn_scan(self.current_dir.clone(), {
            let c = self.egui_ctx.clone();
            move || c.request_repaint()
        });
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
        self.pending_loads.lock().unwrap().clear();
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

    /// カレントディレクトリ・ウィンドウ状態・ソート順・言語・ビューア設定・設定ダイアログで
    /// 編集されうる AppConfig 値をまとめて state ファイルへ書き戻す。
    fn persist_state(&self) {
        crate::config::save_state(
            &self.current_dir, self.window_size, &self.viewer_slots,
            &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending },
            i18n::lang_code(),
            &*self.viewer_cfg.lock().unwrap(),
            &self.config,
        );
    }

    fn sort_archives(&mut self) {
        let ascending = self.sort_ascending;
        match self.sort_key {
            ExplorerSortKey::Name => {
                self.archives.sort_by(|a, b| {
                    let na = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let nb = b.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let cmp = na.cmp(nb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            ExplorerSortKey::Date => {
                self.archives.sort_by(|a, b| {
                    let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
                    let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
                    let cmp = ta.cmp(&tb);
                    if ascending { cmp } else { cmp.reverse() }
                });
            }
            ExplorerSortKey::Size => {
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
    ctx: egui::Context,
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
        // ROOT を起こして poll_workers に結果を回収させる
        ctx.request_repaint();
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

/// 数値テキスト入力1行分（空欄可・単位ラベル付き）。ユーザーが直接タイプできる、
/// DragValue のような「ドラッグしないと分からない」操作を避けるための素朴な TextEdit。
fn draw_text_field(ui: &mut egui::Ui, label: &str, text: &mut String, unit: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(text).desired_width(70.0).hint_text(hint));
        if !unit.is_empty() {
            ui.label(unit);
        }
    });
}

/// リサイズフィルタ選択コンボボックス（共通タブで静止画・アニメ共通の1個を編集する）。
fn draw_resize_filter_combo(ui: &mut egui::Ui, id: &str, value: &mut ResizeFilter) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(filter_to_str(*value))
        .show_ui(ui, |ui| {
            for f in [ResizeFilter::Nearest, ResizeFilter::Triangle, ResizeFilter::CatmullRom, ResizeFilter::Lanczos3] {
                ui.selectable_value(value, f, filter_to_str(f));
            }
        });
}

fn draw_settings_tab_common(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    ui.label(i18n::t().settings_base_resolution_label());
    ui.horizontal(|ui| {
        ui.radio_value(&mut draft.redecode_on_resize, false, i18n::t().settings_base_resolution_actual());
        ui.radio_value(&mut draft.redecode_on_resize, true, i18n::t().settings_base_resolution_follow_window());
    });
    ui.label(i18n::t().settings_base_resolution_explain());

    draw_text_field(ui, i18n::t().settings_max_decode_label(), &mut draft.max_decode_edge_text, "px", "");
    ui.label(i18n::t().settings_max_decode_explain());

    ui.separator();

    ui.label(i18n::t().settings_debounce_label());
    if ui.button(i18n::t().redecode_debounce_label(draft.debounce_ms)).clicked() {
        draft.debounce_ms = crate::config::next_debounce_ms(draft.debounce_ms);
    }
    ui.label(i18n::t().settings_debounce_explain());
    ui.separator();

    ui.label(i18n::t().settings_cache_size_label());
    let auto_hint = i18n::t().settings_cache_size_auto();
    draw_text_field(ui, i18n::t().settings_cache_size_page(), &mut draft.cache_max_mb_text, "MB", auto_hint);
    draw_text_field(ui, i18n::t().settings_cache_size_file(), &mut draft.file_cache_max_mb_text, "MB", auto_hint);
    ui.separator();


    ui.label(i18n::t().settings_resize_filter_viewer_label());
    draw_resize_filter_combo(ui, "common_viewer_filter", &mut draft.viewer_filter);
    ui.add_space(4.0);
    ui.label(i18n::t().settings_resize_filter_thumb_label());
    draw_resize_filter_combo(ui, "common_thumb_filter", &mut draft.thumb_filter);
    ui.separator();

    ui.label(i18n::t().settings_lang_label());
    egui::ComboBox::from_id_salt("settings_lang")
        .selected_text(draft.lang.native_name())
        .show_ui(ui, |ui| {
            for lang in [i18n::Lang::Japanese, i18n::Lang::English, i18n::Lang::Chinese] {
                ui.selectable_value(&mut draft.lang, lang, lang.native_name());
            }
        });
}

fn draw_settings_tab_anim(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    ui.label(i18n::t().settings_ring_bounds_label());
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut draft.ring_min_text).desired_width(50.0));
        ui.label("-");
        ui.add(egui::TextEdit::singleline(&mut draft.ring_max_text).desired_width(50.0));
    });
    ui.label(i18n::t().settings_ring_bounds_explain());
}

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
    fn poll_resize_redecode(&mut self, ctx: &egui::Context) {
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
            let file_bytes = self.file_cache.get(&path);
            let _ = self.req_tx.send(LoadRequest {
                archive_path: path.clone(),
                index: *orig_i,
                entry_name: entry_name.clone(),
                is_raw_file,
                file_bytes,
                target_size: target,
            });
            self.pending_loads.lock().unwrap().insert(key);
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

    /// エクスプローラー窓の中身を描画する（旧 eframe::App::ui 相当）。
    /// 呼び出し元が CentralPanel の Ui を渡す。
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // ウィンドウサイズを毎フレーム記録
        let rect = ctx.input(|i| i.viewport_rect());
        self.window_size = (rect.width() as u32, rect.height() as u32);

        self.poll_workers(&ctx);
        self.prefetch_pages();

        egui::Panel::top("menu_bar").show(ui, |ui| {
            self.draw_menu_bar(ui);
        });

        {
            let style_clone = ui.style().clone();
            egui::Panel::left("folder_panel")
                .exact_size(200.0)
                .frame({
                    let mut f = egui::Frame::side_top_panel(&style_clone);
                    f.inner_margin.right = 0;
                    f
                })
                .show(ui, |ui| {
                    self.draw_folder_panel(ui);
                });
        }

        {
            let style_clone = ui.style().clone();
            egui::CentralPanel::default()
                .frame({
                    let mut f = egui::Frame::central_panel(&style_clone);
                    f.inner_margin.right = 0;
                    f
                })
                .show(ui, |ui| {
                    self.draw_central_panel(ui);
                });
        }

        self.handle_explorer_keys(&ctx);
        // release ビルドは ROOT 内フローティングウィンドウのため ui() で描画する。
        // debug ビルドの独立 deferred viewport は logic() 側で駆動する（上記参照）。
        #[cfg(not(debug_assertions))]
        self.draw_status_window(&ctx);
        self.draw_toast(&ctx);
        self.draw_memory_warning_dialog(&ctx);
        self.draw_settings_dialog(&ctx);
        // 旧来の無条件 ctx.request_repaint() は撤去（イベント駆動化）。
        // ROOT は入力イベント・各ワーカーの起床通知・ステータス窓の1Hzハートビートで再描画される。
    }
}

impl NekoviewApp {
    fn poll_workers(&mut self, ctx: &egui::Context) {
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
                    self.cd_summary_rx = Some(spawn_summary_worker(path, self.cache_db.clone(), self.egui_ctx.clone()));
                }
            }
        }

        // FileCache ワーカーからの結果を受信して横キャッシュへ投入
        let file_results: Vec<(PathBuf, std::sync::Arc<[u8]>)> =
            std::iter::from_fn(|| self.file_cache_res_rx.try_recv().ok()).collect();
        let cur_viewer_path = self.viewer.lock().unwrap().as_ref().map(|v| v.archive_path().clone());
        for (path, bytes) in file_results {
            self.file_cache_pending.remove(&path);
            let current = cur_viewer_path.clone().unwrap_or_else(|| path.clone());
            self.file_cache.insert(path, bytes, &current, &self.archives);
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

    fn prefetch_pages(&self) {
        // スライディングウィンドウ: ビューア表示中に前後ページを先読み
        let viewer_prefetch = self.viewer.lock().unwrap().as_ref().map(|viewer| {
            let cur = viewer.spread_lo().max(0) as usize;
            (cur, viewer.archive_path().clone(), viewer.entries().to_vec(), viewer.is_raw_file())
        });
        if let Some((cur, path, entries, is_raw_file)) = viewer_prefetch {
            let total = entries.len();
            let cur_orig_i = entries.get(cur).map(|e| e.original_index);
            let start = cur.saturating_sub(5);
            let end = (cur + 10 + 1).min(total);
            for i in start..end {
                let orig_i = entries[i].original_index;
                // 予算超過(bypass)と判明済みのページは、現在表示中でない限り先読み対象から外す。
                // bypass はキャッシュに残らないため、先読みし続けると無限に再デコードされてしまう。
                if Some(orig_i) != cur_orig_i && self.page_cache.lock().unwrap().is_known_bypass(&path, orig_i) {
                    continue;
                }
                let key = (path.clone(), orig_i);
                if !self.page_cache.lock().unwrap().contains(&path, orig_i) && !self.pending_loads.lock().unwrap().contains(&key) {
                    let file_bytes = self.file_cache.get(&path);
                    let _ = self.req_tx.send(LoadRequest {
                        archive_path: path.clone(),
                        index: orig_i,
                        entry_name: entries[i].entry_name.clone(),
                        is_raw_file,
                        file_bytes,
                        target_size: self.decode_target,
                    });
                    self.pending_loads.lock().unwrap().insert(key);
                }
            }
        }
    }

    /// ビューアー窓が開いているか（winit_app が窓の生成/破棄判定に使う）。
    pub fn viewer_is_open(&self) -> bool {
        self.viewer.lock().unwrap().is_some()
    }

    /// ビューアー窓を生成する winit 側が、初回フラッシュを避けるために参照する
    /// 解決済み既定スロット（conf default_slot × 現在の viewer_slots）。
    /// None のとき winit は従来の OS既定位置・800x600 で生成する。
    pub fn resolved_default_viewer_slot(&self) -> Option<WindowSlot> {
        crate::controller::resolve_default_slot(self.config.default_slot, &self.viewer_slots)
    }

    /// ビューアー窓のフォーカス前面化要求を取り出す（取り出したら false に戻す）。
    pub fn take_viewer_focus_request(&mut self) -> bool {
        let f = self.viewer_focus_requested;
        self.viewer_focus_requested = false;
        f
    }

    /// ビューアーを閉じる（OS のクローズボタン等から winit_app が呼ぶ）。
    pub fn close_viewer(&mut self) {
        *self.viewer.lock().unwrap() = None;
    }

    /// ビューアー独立窓の 1 フレーム描画。winit_app がビューアー窓の egui パスから呼ぶ。
    /// 旧 `draw_viewer_viewport` の deferred callback 相当（ページ供給 → show → nav/close 処理）。
    pub fn render_viewer(&mut self, ui: &mut egui::Ui) {
        // フェーズ6-E: poll_resize_redecode()はエクスプローラー窓のlogic()からしか
        // 呼ばれていなかったため、ビューアー窓だけを操作している間はエクスプローラー窓が
        // 再描画されずデバウンスが発火しないバグがあった。ビューアー窓自身の毎フレームでも
        // 呼ぶことで、エクスプローラー窓の再描画タイミングに依存しないようにする。
        self.poll_resize_redecode(ui.ctx());

        // 設定ダイアログ表示中は、エクスプローラー窓と合わせてビューアー窓側の操作も
        // ブロックする。独立 OS 窓（別 egui::Context）なので winit_app 側の入力横取りは
        // 使わない。egui::Modal はレイヤー順で「後に出した方が最前面」になるため、通常の
        // viewer.show() より後に出しても既に処理済みのウィジェットの入力は防げない
        // （同一フレーム内で先に走った側が先に入力を消費してしまう）。そのため
        // viewer.show() 自体を呼ばず、Modal だけを描いて操作を完全に止める。
        if self.settings_is_open() {
            egui::Modal::new(egui::Id::new("viewer_settings_blocked")).show(ui.ctx(), |ui| {
                ui.label(i18n::t().settings_viewer_blocked());
            });
            return;
        }

        // エクスプローラー窓が起きていなくてもページ送りが進むよう、
        // ここでワーカー結果回収（res_rx drain）と先読みを回す。
        self.pump_viewer_pages();

        let output = {
            let mut viewer_guard = self.viewer.lock().unwrap();
            let page_cache_guard = self.page_cache.lock().unwrap();
            let mut cfg_guard = self.viewer_cfg.lock().unwrap();
            match viewer_guard.as_mut() {
                Some(viewer) => viewer.show(ui, &*page_cache_guard, &mut *cfg_guard),
                None => return,
            }
        };

        if let Some(slots) = output.save_slots {
            self.viewer_slots = slots;
            self.persist_state();
        }

        let had_nav = output.nav != ViewerNav::None;
        if output.close_requested {
            *self.viewer.lock().unwrap() = None;
            controller::request_status_update(&self.status_update_requested);
            self.egui_ctx.request_repaint();
        } else if had_nav {
            self.handle_viewer_nav(output.nav);
            controller::request_status_update(&self.status_update_requested);
            self.egui_ctx.request_repaint();
        }
    }

    /// ビューアー独立窓パスからのページワーカー結果回収（res_rx drain）と先読み。
    /// エクスプローラー窓の poll_workers / prefetch_pages が起きていない間でもページを進める。
    fn pump_viewer_pages(&mut self) {
        let results: Vec<LoadResult> =
            std::iter::from_fn(|| self.res_rx.lock().unwrap().try_recv().ok()).collect();
        if !results.is_empty() {
            let (cur_path, cur_idx) = self.viewer.lock().unwrap().as_ref()
                .map(|v| {
                    let lo = v.spread_lo().max(0) as usize;
                    let orig = v.entries().get(lo).map(|e| e.original_index).unwrap_or(0);
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
        self.prefetch_pages();
    }

    fn handle_explorer_keys(&mut self, ctx: &egui::Context) {
        // ── エクスプローラー キーナビゲーション ─────────────────────────────
        let total = self.archives.len();
        let cols = self.explorer_cols.max(1);
        let cell_h = self.config.thumb_size as f32;
        const KEY_GAP: f32 = 8.0;
        if total > 0 {
            let prev = self.selected_archive_index;

            // キー入力を一括消費してからクロージャ外で処理する（borrow 競合回避）
            let (key_left, key_right, key_down, key_up) = ctx.input_mut(|i| (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            ));

            // 段階4（窓ごとキー配送）: ビューアーは独立した OS 窓になり、自分の左右キーで
            // ファイル間ナビゲーションを処理する（view_reader::process_navigation →
            // ViewerOutput.nav → render_viewer）。よって 86eca4b の「ビューア起動中は
            // エクスプローラー窓の左右キーで viewer nav を肩代わりする」回避策は撤去する。
            // エクスプローラー窓の左右キーは常にグリッド選択移動とする。
            if key_right {
                if let Some(idx) = self.selected_archive_index {
                    if idx + 1 < total {
                        self.selected_archive_index = Some(idx + 1);
                    }
                }
            }
            if key_left {
                if let Some(idx) = self.selected_archive_index {
                    if idx > 0 {
                        self.selected_archive_index = Some(idx - 1);
                    }
                }
            }

            // 上下キーは常にグリッド選択移動
            if key_down {
                if let Some(idx) = self.selected_archive_index {
                    let current_row = idx / cols;
                    let last_row = (total - 1) / cols;
                    if current_row < last_row {
                        self.selected_archive_index = Some((idx + cols).min(total - 1));
                    }
                }
            }
            if key_up {
                if let Some(idx) = self.selected_archive_index {
                    if idx >= cols {
                        self.selected_archive_index = Some(idx - cols);
                    }
                }
            }

            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(idx) = self.selected_archive_index {
                    if let Some(path) = self.archives.get(idx).cloned() {
                        let is_raw = self.raw_image_files.contains(&path);
                        let state = if is_raw {
                            Some(ViewerState::new_raw(path.clone(), self.viewer_slots, self.config.default_slot))
                        } else if self.check_memory_budget(&path) {
                            ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot)
                        } else {
                            None
                        };
                        if let Some(state) = state {
                            self.open_viewer(state);
                        }
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

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let hidden_label = if self.show_hidden { i18n::t().hidden_on() } else { i18n::t().hidden_off() };
            if ui.selectable_label(self.show_hidden, hidden_label).clicked() {
                self.show_hidden = !self.show_hidden;
            }

            ui.separator();

            // ── ページ表示モード ──────────────────────────────────────────
            let (viewer_open, is_raw_viewer, cur_mode, is_spread, can_back, can_fwd, is_offset) = {
                let guard = self.viewer.lock().unwrap();
                let viewer_open = guard.is_some();
                let is_raw_viewer = guard.as_ref().map_or(false, |v| v.is_raw_file());
                let cur_mode = guard.as_ref().map(|v| v.page_mode());
                let is_spread = cur_mode.map_or(false, |m| m != PageMode::Single);
                let can_back = guard.as_ref().map_or(false, |v| v.can_shift_backward());
                let can_fwd  = guard.as_ref().map_or(false, |v| v.can_shift_forward());
                let is_offset = guard.as_ref().map_or(false, |v| v.is_spread_offset());
                (viewer_open, is_raw_viewer, cur_mode, is_spread, can_back, can_fwd, is_offset)
            };

            ui.add_enabled_ui(viewer_open, |ui| {
                if ui.selectable_label(cur_mode == Some(PageMode::Single), i18n::t().page_single()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::Single, &mut *cfg_guard); }
                }
            });
            ui.add_enabled_ui(viewer_open && !is_raw_viewer, |ui| {
                if ui.selectable_label(cur_mode == Some(PageMode::SpreadLeft), i18n::t().page_spread_left()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::SpreadLeft, &mut *cfg_guard); }
                }
                if ui.selectable_label(cur_mode == Some(PageMode::SpreadRight), i18n::t().page_spread_right()).clicked() {
                    let mut v_guard = self.viewer.lock().unwrap();
                    let mut cfg_guard = self.viewer_cfg.lock().unwrap();
                    if let Some(v) = v_guard.as_mut() { v.set_page_mode(PageMode::SpreadRight, &mut *cfg_guard); }
                }
            });

            ui.add_enabled_ui(viewer_open && is_spread && !is_raw_viewer, |ui| {
                if ui.add_enabled(can_back, egui::Button::new(i18n::t().spread_back())).clicked() {
                    if let Some(v) = self.viewer.lock().unwrap().as_mut() { v.shift_offset_backward(); }
                }
                if ui.add_enabled(can_fwd, egui::Button::new(i18n::t().spread_fwd())).clicked() {
                    if let Some(v) = self.viewer.lock().unwrap().as_mut() { v.shift_offset_forward(); }
                }
                ui.label(if is_offset { i18n::t().spread_offset_on() } else { i18n::t().spread_aligned() });
            });

            ui.separator();

            // ── エクスプローラーソート ────────────────────────────────────
            let mut sort_changed = false;
            for key in [ExplorerSortKey::Name, ExplorerSortKey::Date, ExplorerSortKey::Size] {
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

            let order_label = if self.sort_ascending { i18n::t().sort_asc() } else { i18n::t().sort_desc() };
            if ui.button(order_label).clicked() {
                self.sort_ascending = !self.sort_ascending;
                sort_changed = true;
            }

            if sort_changed {
                self.sort_archives();
            }

            // ── ステータスウィンドウボタン（右端） ────────────────────────
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = ui.button("[?]");
                if btn.clicked() {
                    self.show_status_window = !self.show_status_window;
                }

                ui.separator();

                // 設定ダイアログを開く。旧・再デコードトグル/デバウンスサイクル/言語切替
                // ボタン列はダイアログの「共通」タブに統合した。
                if ui.button(i18n::t().settings_button()).clicked() {
                    self.open_settings();
                }
            });
        });
    }

    fn draw_folder_panel(&mut self, ui: &mut egui::Ui) {
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
                            rx: dir::spawn_scan_subdirs(path, {
                                let c = self.egui_ctx.clone();
                                move || c.request_repaint()
                            }),
                        });
                    }
                }
            }
            TreeAction::Navigate(path) => {
                self.current_dir = path.clone();
                self.viewing_dir = Some(path.clone());
                self.start_scan(); // cache_db をここで確定させてから clone して渡す
                self.cd_summary_rx = Some(spawn_summary_worker(path.clone(), self.cache_db.clone(), self.egui_ctx.clone()));
                self.persist_state();
            }
        }

        ui.separator();

        // ── 下部: ドライブ選択 ──
        ui.small(i18n::t().drives());
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
                            rx: dir::spawn_scan_subdirs(path, {
                                let c = self.egui_ctx.clone();
                                move || c.request_repaint()
                            }),
                        });
                        self.persist_state();
                    }
                }
            });
    }

    fn draw_central_panel(&mut self, ui: &mut egui::Ui) {
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
                    egui::RichText::new(i18n::t().thumb_saved(*saved, *total))
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
            ui.label(i18n::t().file_info(&date_str, mb, filename));
        }

        ui.separator();

        {
            // ローディング中は 0.5秒経過後にスピナーを表示（短いアクセスのチラツキ防止）
            let is_loading = matches!(&self.scan_state, ScanState::Loading { started_at, .. }
                if started_at.elapsed().as_secs_f32() > 0.5);

            if is_loading {
                ui.centered_and_justified(|ui| {
                    ui.label(i18n::t().loading());
                });
            } else {
                self.draw_archive_grid(ui);
            }
        }
    }

    fn draw_archive_grid(&mut self, ui: &mut egui::Ui) {
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
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }

                        let is_raw = self.raw_image_files.contains(path);
                        if response.clicked() {
                            if is_raw && self.selected_archive_index == Some(i) {
                                // 生ファイル: 選択済み状態のシングルクリックで開く
                                self.open_viewer(ViewerState::new_raw(path.clone(), self.viewer_slots, self.config.default_slot));
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
                                    i18n::t().invalid_zip(&name),
                                    std::time::Instant::now(),
                                ));
                            } else if !self.check_memory_budget(path) {
                                // ダイアログ表示フラグは check_memory_budget 内で立つ。オープンは中止する。
                            } else {
                                match ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot) {
                                    Some(state) => {
                                        self.open_viewer(state);
                                    }
                                    None => {
                                        let p = path.clone();
                                        self.mark_archive_invalid(&p);
                                        let name = truncate_filename(path);
                                        self.app_toast = Some((
                                            i18n::t().invalid_zip(&name),
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

    /// ステータスデータを 1Hz throttle で更新する（force 要求があれば即時）。
    /// debug/release 共通。debug 用の追加メトリクスは `frame_dt_ms` を使う。
    fn refresh_status_data(&mut self, frame_dt_ms: f32) {
        let force = self.status_update_requested.swap(false, std::sync::atomic::Ordering::Relaxed);
        let elapsed = self.last_status_update.elapsed();
        if !(force || elapsed >= std::time::Duration::from_secs(1)) {
            return;
        }
        self.last_status_update = std::time::Instant::now();
        let mut data = self.status_window_data.lock().unwrap();
        let page_cache = self.page_cache.lock().unwrap();
        controller::update_status_data(
            &mut data,
            page_cache.total_bytes(),
            page_cache.max_bytes(),
            self.file_cache.total_bytes(),
            self.file_cache.max_bytes(),
        );

        #[cfg(debug_assertions)]
        controller::update_status_data_debug(
            &mut data,
            frame_dt_ms,
            self.thumb_pending.len(),
            self.pending_loads.lock().unwrap().len(),
            self.thumbnails.len(),
            match &self.scan_state {
                ScanState::Idle      => "idle",
                ScanState::Loading { .. } => "loading",
                ScanState::Done      => "done",
            },
        );
        #[cfg(not(debug_assertions))]
        let _ = frame_dt_ms;
    }

    /// ステータス窓（debug 独立 OS 窓 / release ROOT 内フローティング窓）の表示状態。
    /// winit_app が debug ビルドでこの状態に合わせて独立窓を生成/破棄する
    /// （release では sync_status_window が no-op のため未使用）。
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn status_is_open(&self) -> bool {
        self.show_status_window
    }

    /// ステータス窓を閉じる（OS のクローズボタンから winit_app が呼ぶ / debug）。
    pub fn close_status(&mut self) {
        self.show_status_window = false;
    }

    /// debug ビルドのステータス独立窓の 1 フレーム描画。winit_app が status 窓の
    /// egui パスから呼ぶ。データを 1Hz throttle で更新して描画し、
    /// `request_repaint_after(1s)` で自分自身を 1Hz で起こし続ける。
    pub fn render_status(&mut self, ui: &mut egui::Ui) {
        let dt_ms = ui.ctx().input(|i| i.stable_dt) * 1000.0;
        self.refresh_status_data(dt_ms);
        {
            let data = self.status_window_data.lock().unwrap();
            crate::view_status::draw_content(ui, &data);
        }
        // 次の 1Hz tick へ向けて自分自身の再描画を予約する。
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// release ビルド: ROOT 内フローティング `egui::Window` としてステータスを描画する。
    /// debug ビルドでは独立 OS 窓（render_status）が担うため使わない。
    #[cfg(not(debug_assertions))]
    fn draw_status_window(&mut self, ctx: &egui::Context) {
        if self.show_status_window {
            self.refresh_status_data(ctx.input(|i| i.stable_dt) * 1000.0);
        }
        crate::view_status::show(
            ctx,
            &mut self.show_status_window,
            &self.status_window_data,
        );
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
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
    }

    /// フェーズ2: アーカイブを開く前にメモリ見積もりゲートを通す。
    /// 予算超過と判定した場合は確認ダイアログの表示フラグを立てて false を返す
    /// （呼び出し側はオープンを中止する。invalid_archives への永続マークは行わない。
    /// 一時的な予算状況が変わりうるため、次回オープン時に再度見積もりし直す）。
    fn check_memory_budget(&mut self, path: &std::path::Path) -> bool {
        let entries = archive::list_images(path);
        if entries.is_empty() {
            return true; // 空/無効アーカイブの判定は既存の invalid_archives 処理に任せる
        }
        match archive::estimate_archive_memory(path, &entries, self.cache_budget_bytes, self.anim_ring_bounds) {
            archive::ArchiveMemoryEstimate::Ok => true,
            archive::ArchiveMemoryEstimate::OverBudget => {
                self.memory_warning_open = true;
                false
            }
        }
    }

    /// フェーズ2: メモリ見積もり超過の確認ダイアログを描画する（OKボタンのみ）
    /// 設定ダイアログが開いているか（ビューアー窓側の操作ブロック判定にも使う）。
    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    /// 設定ダイアログを開く。編集用の下書き(draft)を現在値から作り直す
    /// （[反映]を押すまで実際の設定には反映されない）。
    pub fn open_settings(&mut self) {
        self.settings_draft = SettingsDraft::from_current(&self.config, &self.viewer_cfg.lock().unwrap());
        self.settings_open = true;
    }

    /// 設定ダイアログ本体。`egui::Modal` はこの `ctx`（エクスプローラー窓）内の入力を
    /// 自動的にブロックする。ビューアー窓側は別 Context のため、`render_viewer` 側で
    /// 同様の Modal を出して操作を止める（`settings_is_open()` 参照）。
    /// 各タブの入力は draft（`self.settings_draft`）に対して自由に編集させ、
    /// タブ共通の[反映]/[閉じる]ボタンでのみ実際の設定へ書き戻す。
    fn draw_settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut close = false;
        let mut apply = false;
        egui::Modal::new(egui::Id::new("settings_dialog")).show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.heading(i18n::t().settings_title());
            ui.separator();

            ui.horizontal(|ui| {
                for (tab, label) in [
                    (SettingsTab::Common, i18n::t().settings_tab_common()),
                    (SettingsTab::Anim, i18n::t().settings_tab_anim()),
                    (SettingsTab::Static, i18n::t().settings_tab_static()),
                    (SettingsTab::Other, i18n::t().settings_tab_other()),
                ] {
                    ui.selectable_value(&mut self.settings_tab, tab, label);
                }
            });
            ui.separator();

            match self.settings_tab {
                SettingsTab::Common => draw_settings_tab_common(ui, &mut self.settings_draft),
                SettingsTab::Anim => draw_settings_tab_anim(ui, &mut self.settings_draft),
                SettingsTab::Static => self.draw_settings_tab_static(ui),
                SettingsTab::Other => self.draw_settings_tab_other(ui),
            }

            ui.separator();
            ui.label(i18n::t().settings_legend());
            ui.separator();
            // GTK/GNOME 慣習（キャンセル系=左、既定/主アクション=右）に合わせて
            // [閉じる]を左、主アクションの[反映]を右に置く。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(i18n::t().settings_apply()).clicked() {
                    apply = true;
                }
                if ui.button(i18n::t().settings_close()).clicked() {
                    close = true;
                }
            });
        });
        if apply {
            self.settings_draft.apply_to(&mut self.config, &mut self.viewer_cfg.lock().unwrap());
            self.persist_state();
            close = true;
        }
        if close {
            self.settings_open = false;
        }
    }

    fn draw_settings_tab_static(&mut self, ui: &mut egui::Ui) {
        ui.label(i18n::t().settings_static_placeholder());
    }

    fn draw_settings_tab_other(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(i18n::t().settings_version_label());
            ui.label(env!("CARGO_PKG_VERSION"));
        });
    }

    fn draw_memory_warning_dialog(&mut self, ctx: &egui::Context) {
        if !self.memory_warning_open {
            return;
        }
        let mut open = true;
        egui::Window::new(i18n::t().memory_warning_title())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(i18n::t().memory_warning_body());
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if ui.button(i18n::t().memory_warning_ok()).clicked() {
                        self.memory_warning_open = false;
                    }
                });
            });
        if !open {
            self.memory_warning_open = false;
        }
    }

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
        let mut skip_idx = from_idx;
        loop {
            match controller::find_next_file(
                &self.archives,
                &self.raw_image_files,
                &self.invalid_archives,
                skip_idx,
                direction,
            ) {
                None => return None,
                Some((idx, path, true)) => {
                    return Some((idx, ViewerState::new_raw(path, self.viewer_slots, self.config.default_slot)));
                }
                Some((idx, path, false)) => {
                    if !self.check_memory_budget(&path) {
                        // ダイアログ表示フラグは check_memory_budget 内で立つ。
                        // invalid_archives には永続マークせず、現在のファイルに留まる。
                        return None;
                    }
                    match ViewerState::new(path.clone(), self.viewer_slots, self.config.default_slot) {
                        Some(state) => return Some((idx, state)),
                        None => {
                            self.mark_archive_invalid(&path);
                            skip_idx = idx;
                        }
                    }
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
                        self.selected_archive_index = Some(idx);
                        self.open_viewer(state);
                    } else if !self.memory_warning_open {
                        // メモリ見積もり超過が原因の場合は memory_warning ダイアログの方を
                        // 表示するため、「これ以上開けない」トーストは出さない。
                        if let Some(v) = self.viewer.lock().unwrap().as_mut() {
                            v.set_toast(i18n::t().toast_no_prev().to_string());
                        }
                    }
                }
            }
            ViewerNav::NextFile => {
                if let Some(from) = self.selected_archive_index {
                    if let Some((idx, state)) = self.find_next_valid(from, 1) {
                        self.selected_archive_index = Some(idx);
                        self.open_viewer(state);
                    } else if !self.memory_warning_open {
                        if let Some(v) = self.viewer.lock().unwrap().as_mut() {
                            v.set_toast(i18n::t().toast_no_next().to_string());
                        }
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

    /// ビューアを開く（ページキャッシュクリア・ファイルキャッシュ投入・フォーカス要求を一括処理）
    fn open_viewer(&mut self, state: ViewerState) {
        let path = state.archive_path().clone();
        self.pending_loads.lock().unwrap().clear();
        *self.viewer.lock().unwrap() = Some(state);
        self.ensure_file_cached(path);
        self.viewer_focus_requested = true;
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
