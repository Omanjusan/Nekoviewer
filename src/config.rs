use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ── ログ設定グローバル ─────────────────────────────────────────────────────────

pub struct LogConfig {
    pub perf:   bool,
    pub key:    bool,
    pub common: bool,
}

static LOG: OnceLock<LogConfig> = OnceLock::new();

/// どこからでも呼べるログ設定取得。AppConfig::load() より前に呼ぶとデフォルト値を返す。
pub fn log() -> &'static LogConfig {
    LOG.get_or_init(|| LogConfig { perf: false, key: true, common: true })
}

#[macro_export]
macro_rules! log_perf {
    ($($arg:tt)*) => { if $crate::config::log().perf   { eprintln!($($arg)*); } };
}
#[macro_export]
macro_rules! log_key {
    ($($arg:tt)*) => { if $crate::config::log().key    { eprintln!($($arg)*); } };
}
#[macro_export]
macro_rules! log_common {
    ($($arg:tt)*) => { if $crate::config::log().common { eprintln!($($arg)*); } };
}

#[derive(Clone, Copy, PartialEq)]
pub enum CacheStorage {
    /// 実行ファイル配下の cache/ に保存（開発・確認用）
    Local,
    /// ~/.local/share/nekoview/cache/ に保存（本番推奨）
    Xdg,
}

#[derive(Clone, Copy)]
pub enum ResizeFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Lanczos3,
}

impl ResizeFilter {
    pub fn to_image_filter(self) -> image::imageops::FilterType {
        match self {
            Self::Nearest   => image::imageops::FilterType::Nearest,
            Self::Triangle  => image::imageops::FilterType::Triangle,
            Self::CatmullRom => image::imageops::FilterType::CatmullRom,
            Self::Lanczos3  => image::imageops::FilterType::Lanczos3,
        }
    }
}

pub struct StartupConfig {
    /// true = 最後に開いていた場所から起動（アクセス不可時は fixed_dir にフォールバック）
    pub use_last_dir: bool,
    /// 固定起動フォルダ。None または空欄の場合はホームディレクトリ
    pub fixed_dir: Option<std::path::PathBuf>,
}

pub struct AppConfig {
    pub cache_storage: CacheStorage,
    pub thumb_filter: ResizeFilter,
    pub viewer_filter: ResizeFilter,
    /// グリッドのサムネイル長辺サイズ（px）
    pub thumb_size: u32,
    /// ページデコードの並列スレッド数（0 = 自動: 論理コア数/2）
    pub decode_threads: usize,
    pub startup: StartupConfig,
    /// ページキャッシュの最大メモリ上限（MB）。None = システムRAMの25%
    pub cache_max_mb: Option<u64>,
    /// ファイルキャッシュの最大メモリ上限（MB）。None = システムRAMの5%
    pub file_cache_max_mb: Option<u64>,
    /// ビューアー既定スロット index（0..3 = F5〜F8）。None = デフォルト無し（空欄/不正値）
    pub default_slot: Option<usize>,
    /// アニメーションリングバッファの先読み枚数下限（フェーズ4）。空欄/不正値は既定4。
    pub anim_ring_min_frames: usize,
    /// アニメーションリングバッファの先読み枚数上限（フェーズ4）。空欄/不正値は既定32。
    pub anim_ring_max_frames: usize,
    /// アニメーション1フレームあたりの生デコードサイズ上限（MB、フェーズ5）。空欄/不正値は既定100。
    pub anim_frame_hard_limit_mb: usize,
}

impl AppConfig {
    pub fn resolved_decode_threads(&self) -> usize {
        if self.decode_threads == 0 {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2);
            (cores / 2).max(1)
        } else {
            self.decode_threads.max(1)
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));

        let mut parsed = ParsedIni::default();

        if let Some(ref dir) = exe_dir {
            let conf_path = dir.join("nekoviewer.conf");
            if conf_path.exists() {
                parsed = parse_ini(&conf_path);
            } else {
                let _ = std::fs::write(&conf_path, DEFAULT_INI);
            }
        }

        // グローバルに設定（2回目以降の load() 呼び出しは無視される）
        let _ = LOG.set(LogConfig {
            perf:   parsed.log_perf,
            key:    parsed.log_key.0,
            common: parsed.log_common.0,
        });

        AppConfig {
            cache_storage: parsed.storage,
            thumb_filter: parsed.thumb_filter,
            viewer_filter: parsed.viewer_filter,
            thumb_size: parsed.thumb_size.0,
            decode_threads: parsed.decode_threads,
            startup: StartupConfig {
                use_last_dir: parsed.startup_use_last_dir,
                fixed_dir: parsed.startup_fixed_dir,
            },
            cache_max_mb: parsed.cache_max_mb,
            file_cache_max_mb: parsed.file_cache_max_mb,
            default_slot: parsed.default_slot,
            anim_ring_min_frames: parsed.anim_ring_min_frames.0,
            anim_ring_max_frames: parsed.anim_ring_max_frames.0,
            anim_frame_hard_limit_mb: parsed.anim_frame_hard_limit_mb.0,
        }
    }

    /// 起動時の初期フォルダを解決する（CLI引数は呼び出し元で優先済みを想定）
    pub fn startup_dir(&self, state: &AppState) -> PathBuf {
        let fixed = self.startup.fixed_dir.as_deref()
            .filter(|p| !p.as_os_str().is_empty());

        log_common!("[startup] use_last_dir = {}", self.startup.use_last_dir);
        log_common!("[startup] fixed_dir = {:?}", fixed);

        if self.startup.use_last_dir {
            // ① 復帰用データがあるか
            match &state.last_dir {
                None => {
                    log_common!("[startup] last_dir: state file なし or 空 → フォールバックへ");
                }
                Some(last) => {
                    log_common!("[startup] last_dir: state file から読み込み成功 = {:?}", last);
                    // ② 読み込めているか（アクセス可能か）
                    let accessible = last.is_dir();
                    log_common!("[startup] last_dir: アクセス確認 = {}", accessible);
                    // ③ 復帰動作をしているか
                    if accessible {
                        log_common!("[startup] → last_dir に復帰: {:?}", last);
                        return last.clone();
                    } else {
                        log_common!("[startup] last_dir にアクセス不可 → フォールバックへ");
                    }
                }
            }
        }

        let fallback = resolve_fallback_dir(fixed);
        log_common!("[startup] → フォールバック先: {:?}", fallback);
        fallback
    }

    pub fn cache_root(&self) -> Option<PathBuf> {
        match self.cache_storage {
            CacheStorage::Local => std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("cache"))),
            CacheStorage::Xdg => {
                #[cfg(windows)]
                let base = std::env::var("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."));
                #[cfg(not(windows))]
                let base = std::env::var("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| {
                        std::env::var("HOME")
                            .map(|h| PathBuf::from(h).join(".local/share"))
                            .unwrap_or_else(|_| PathBuf::from(".local/share"))
                    });
                Some(base.join("nekoview/cache"))
            }
        }
    }
}

#[derive(Default)]
struct ParsedIni {
    storage: CacheStorage,
    thumb_filter: ResizeFilter,
    viewer_filter: ResizeFilter,
    thumb_size: ThumbSize,
    decode_threads: usize,
    log_perf:   bool,
    log_key:    LogDefault<true>,
    log_common: LogDefault<true>,
    startup_use_last_dir: bool,
    startup_fixed_dir: Option<PathBuf>,
    cache_max_mb: Option<u64>,
    file_cache_max_mb: Option<u64>,
    default_slot: Option<usize>,
    anim_ring_min_frames: UsizeDefault<4>,
    anim_ring_max_frames: UsizeDefault<32>,
    anim_frame_hard_limit_mb: UsizeDefault<100>,
}

/// usize のデフォルト値を const ジェネリクスで指定するラッパー（空欄/不正値は既定にフォールバック）
struct UsizeDefault<const V: usize>(usize);
impl<const V: usize> Default for UsizeDefault<V> {
    fn default() -> Self { Self(V) }
}

/// bool のデフォルト値を const ジェネリクスで指定するラッパー
struct LogDefault<const V: bool>(bool);
impl<const V: bool> Default for LogDefault<V> {
    fn default() -> Self { Self(V) }
}
impl<const V: bool> std::ops::Deref for LogDefault<V> {
    type Target = bool;
    fn deref(&self) -> &bool { &self.0 }
}

struct ThumbSize(u32);
impl Default for ThumbSize {
    fn default() -> Self { Self(256) }
}

impl Default for CacheStorage {
    fn default() -> Self { Self::Local }
}

impl Default for ResizeFilter {
    fn default() -> Self { Self::Triangle }
}

fn parse_ini(path: &std::path::Path) -> ParsedIni {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return ParsedIni::default(),
    };

    let mut result = ParsedIni::default();
    let mut section = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let (k, v) = (key.trim(), val.trim());
            match (section.as_str(), k) {
                ("cache", "storage") => {
                    result.storage = match v {
                        "xdg" => CacheStorage::Xdg,
                        _ => CacheStorage::Local,
                    };
                }
                ("cache", "max_mb") => {
                    if let Ok(n) = v.parse::<u64>() {
                        result.cache_max_mb = Some(n.max(64));
                    }
                }
                ("cache", "file_cache_max_mb") => {
                    if let Ok(n) = v.parse::<u64>() {
                        result.file_cache_max_mb = Some(n.max(16));
                    }
                }
                ("cache", "anim_ring_min_frames") => {
                    if let Ok(n) = v.parse::<usize>() {
                        result.anim_ring_min_frames = UsizeDefault(n.max(1));
                    }
                }
                ("cache", "anim_ring_max_frames") => {
                    if let Ok(n) = v.parse::<usize>() {
                        result.anim_ring_max_frames = UsizeDefault(n.max(1));
                    }
                }
                ("cache", "anim_frame_hard_limit_mb") => {
                    if let Ok(n) = v.parse::<usize>() {
                        result.anim_frame_hard_limit_mb = UsizeDefault(n.max(1));
                    }
                }
                ("thumbnail", "filter") => {
                    result.thumb_filter = parse_filter(v);
                }
                ("viewer", "filter") => {
                    result.viewer_filter = parse_filter(v);
                }
                ("viewer", "default_slot") => {
                    // (a) 5〜8 のみ採用。空欄・範囲外・不正値は None（デフォルト無し）。
                    result.default_slot = match v {
                        "5" => Some(0),
                        "6" => Some(1),
                        "7" => Some(2),
                        "8" => Some(3),
                        _   => None,
                    };
                }
                ("grid", "thumb_size") => {
                    if let Ok(n) = v.parse::<u32>() {
                        result.thumb_size = ThumbSize(n.max(64).min(512));
                    }
                }
                ("worker", "decode_threads") => {
                    if let Ok(n) = v.parse::<usize>() {
                        result.decode_threads = n;
                    }
                }
                ("log", "perf")   => result.log_perf   = parse_bool(v, false),
                ("log", "key")    => result.log_key    = LogDefault(parse_bool(v, true)),
                ("log", "common") => result.log_common = LogDefault(parse_bool(v, true)),
                ("startup", "use_last_dir") => {
                    result.startup_use_last_dir = parse_bool(v, false);
                }
                ("startup", "fixed_dir") => {
                    if !v.is_empty() {
                        result.startup_fixed_dir = Some(PathBuf::from(v));
                    }
                }
                _ => {}
            }
        }
    }
    result
}

// ── State ファイル（動的状態: 最後のディレクトリ・ウィンドウサイズ）────────────

pub struct SortState {
    pub key: String,
    pub ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self { key: "name".to_string(), ascending: true }
    }
}

/// ファイルをまたいで維持するビューア設定（ウィンドウを開き直しても保持）
#[derive(Clone, Copy)]
pub struct ViewerConfig {
    /// true = 1:1等倍表示、false = ウィンドウフィット
    pub zoom_actual: bool,
    /// フルスクリーン状態
    pub fullscreen: bool,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self { zoom_actual: false, fullscreen: false }
    }
}

/// ビューアウィンドウの位置・サイズスロット（論理ピクセル）
#[derive(Clone, Copy)]
pub struct WindowSlot {
    /// outer_rect の左上 x 座標
    pub x: i32,
    /// outer_rect の左上 y 座標
    pub y: i32,
    /// inner_rect の幅（コンテンツ領域）
    pub w: u32,
    /// inner_rect の高さ（コンテンツ領域）
    pub h: u32,
}

pub struct AppState {
    pub last_dir: Option<PathBuf>,
    /// (width, height) in logical pixels
    pub window_size: Option<(u32, u32)>,
    /// ビューアウィンドウの位置・サイズスロット（F5〜F8 対応）
    pub viewer_slots: [Option<WindowSlot>; 4],
    pub sort_state: SortState,
    /// UI言語コード: "ja" / "en" / "cn"
    pub lang: String,
    /// ファイル切替後も維持するビューア設定
    pub viewer_cfg: ViewerConfig,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_dir: None,
            window_size: None,
            viewer_slots: [None; 4],
            sort_state: SortState::default(),
            lang: "ja".to_string(),
            viewer_cfg: ViewerConfig::default(),
        }
    }
}

fn state_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state")))
}

fn state_bak_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state.bak")))
}

fn state_tmp_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state.tmp")))
}

pub fn load_state() -> AppState {
    if let Some(path) = state_path() {
        if let Some(state) = parse_state_file(&path) {
            return state;
        }
    }
    // メインが読めなければ bak を試みる
    if let Some(bak) = state_bak_path() {
        if let Some(state) = parse_state_file(&bak) {
            log_common!("[state] メイン読み込み失敗 → bak から復元");
            return state;
        }
    }
    AppState::default()
}

fn parse_state_file(path: &Path) -> Option<AppState> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut last_dir: Option<PathBuf> = None;
    let mut window_width: Option<u32> = None;
    let mut window_height: Option<u32> = None;
    let mut slot_x: [Option<i32>; 4] = [None; 4];
    let mut slot_y: [Option<i32>; 4] = [None; 4];
    let mut slot_w: [Option<u32>; 4] = [None; 4];
    let mut slot_h: [Option<u32>; 4] = [None; 4];
    let mut sort_key: Option<String> = None;
    let mut sort_ascending: Option<bool> = None;
    let mut lang: Option<String> = None;
    let mut viewer_zoom: Option<bool> = None;
    let mut viewer_fullscreen: Option<bool> = None;
    let mut has_kv = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some((k, v)) = line.split_once('=') {
            has_kv = true;
            match k.trim() {
                "last_dir" => {
                    let v = v.trim();
                    if !v.is_empty() { last_dir = Some(PathBuf::from(v)); }
                }
                "window_width"  => { window_width  = v.trim().parse().ok(); }
                "window_height" => { window_height = v.trim().parse().ok(); }
                "slot1_x" => { slot_x[0] = v.trim().parse().ok(); }
                "slot1_y" => { slot_y[0] = v.trim().parse().ok(); }
                "slot1_w" => { slot_w[0] = v.trim().parse().ok(); }
                "slot1_h" => { slot_h[0] = v.trim().parse().ok(); }
                "slot2_x" => { slot_x[1] = v.trim().parse().ok(); }
                "slot2_y" => { slot_y[1] = v.trim().parse().ok(); }
                "slot2_w" => { slot_w[1] = v.trim().parse().ok(); }
                "slot2_h" => { slot_h[1] = v.trim().parse().ok(); }
                "slot3_x" => { slot_x[2] = v.trim().parse().ok(); }
                "slot3_y" => { slot_y[2] = v.trim().parse().ok(); }
                "slot3_w" => { slot_w[2] = v.trim().parse().ok(); }
                "slot3_h" => { slot_h[2] = v.trim().parse().ok(); }
                "slot4_x" => { slot_x[3] = v.trim().parse().ok(); }
                "slot4_y" => { slot_y[3] = v.trim().parse().ok(); }
                "slot4_w" => { slot_w[3] = v.trim().parse().ok(); }
                "slot4_h" => { slot_h[3] = v.trim().parse().ok(); }
                "sort_key" => {
                    let v = v.trim();
                    if matches!(v, "name" | "date" | "size") {
                        sort_key = Some(v.to_string());
                    }
                }
                "sort_ascending" => { sort_ascending = v.trim().parse().ok(); }
                "lang" => {
                    let v = v.trim();
                    if matches!(v, "ja" | "en" | "cn") {
                        lang = Some(v.to_string());
                    }
                }
                "viewer_zoom"       => { viewer_zoom       = v.trim().parse().ok(); }
                "viewer_fullscreen" => { viewer_fullscreen = v.trim().parse().ok(); }
                _ => {}
            }
        }
    }

    // 旧フォーマット互換: key=value がなければ全体をパスとして扱う
    if !has_kv {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            last_dir = Some(PathBuf::from(trimmed));
        }
    }

    let window_size = match (window_width, window_height) {
        (Some(w), Some(h)) if w >= 200 && h >= 150 => Some((w, h)),
        _ => None,
    };

    let mut viewer_slots: [Option<WindowSlot>; 4] = [None; 4];
    for i in 0..4 {
        if let (Some(x), Some(y), Some(w), Some(h)) = (slot_x[i], slot_y[i], slot_w[i], slot_h[i]) {
            if w >= 100 && h >= 100 {
                viewer_slots[i] = Some(WindowSlot { x, y, w, h });
            }
        }
    }

    let sort_state = SortState {
        key: sort_key.unwrap_or_else(|| "name".to_string()),
        ascending: sort_ascending.unwrap_or(true),
    };

    Some(AppState {
        last_dir,
        window_size,
        viewer_slots,
        sort_state,
        lang: lang.unwrap_or_else(|| "ja".to_string()),
        viewer_cfg: ViewerConfig {
            zoom_actual: viewer_zoom.unwrap_or(false),
            fullscreen: viewer_fullscreen.unwrap_or(false),
        },
    })
}

pub fn save_state(dir: &Path, window_size: (u32, u32), viewer_slots: &[Option<WindowSlot>; 4], sort_state: &SortState, lang: &str, viewer_cfg: &ViewerConfig) {
    let (Some(path), Some(bak), Some(tmp)) =
        (state_path(), state_bak_path(), state_tmp_path())
    else { return; };

    let mut content = format!(
        "last_dir={}\nwindow_width={}\nwindow_height={}\nsort_key={}\nsort_ascending={}\nlang={}\nviewer_zoom={}\nviewer_fullscreen={}\n",
        dir.to_string_lossy(), window_size.0, window_size.1, sort_state.key, sort_state.ascending, lang,
        viewer_cfg.zoom_actual, viewer_cfg.fullscreen,
    );
    for (i, slot) in viewer_slots.iter().enumerate() {
        if let Some(s) = slot {
            content.push_str(&format!(
                "slot{n}_x={x}\nslot{n}_y={y}\nslot{n}_w={w}\nslot{n}_h={h}\n",
                n = i + 1, x = s.x, y = s.y, w = s.w, h = s.h
            ));
        }
    }

    // アトミック書き込み: tmp に書いてから rename
    if std::fs::write(&tmp, &content).is_err() { return; }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // 書き込み成功を確認してから bak に同内容をミラー
    let _ = std::fs::write(&bak, &content);
}

fn resolve_fallback_dir(fixed: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = fixed {
        if p.is_dir() {
            return p.to_path_buf();
        }
    }
    #[cfg(windows)]
    let home_var = "USERPROFILE";
    #[cfg(not(windows))]
    let home_var = "HOME";

    if let Some(home) = std::env::var(home_var).ok().map(PathBuf::from) {
        if home.is_dir() {
            return home;
        }
    }

    #[cfg(windows)]
    return PathBuf::from("C:\\");
    #[cfg(not(windows))]
    return PathBuf::from("/");
}

fn parse_bool(s: &str, default: bool) -> bool {
    match s {
        "true" | "on" | "1"  => true,
        "false" | "off" | "0" => false,
        _ => default,
    }
}

fn parse_filter(s: &str) -> ResizeFilter {
    match s {
        "nearest"   => ResizeFilter::Nearest,
        "catmullrom" => ResizeFilter::CatmullRom,
        "lanczos3"  => ResizeFilter::Lanczos3,
        _           => ResizeFilter::Triangle,
    }
}

const DEFAULT_INI: &str = "\
# ============================================================================
#  Nekoviewer 設定ファイル (nekoviewer.conf)
#
#  ・この実行ファイルと同じフォルダに置かれます。
#  ・ファイルを削除すると、次回起動時にこの既定値で再生成されます。
#  ・'#' または ';' で始まる行はコメントです。'キー = 値' 形式で記述します。
#  ・不明なキーや不正な値は無視され、そのキーの既定値が使われます。
# ============================================================================

# ── 起動 ────────────────────────────────────────────────────────────────────
[startup]
# 起動時に最後に開いていた場所を復元する（true / false）。
# 最後のフォルダにアクセスできない場合（ネットワークドライブ切断など）は
# fixed_dir にフォールバックします。
use_last_dir = false

# 起動時に開く固定フォルダ。
# ・use_last_dir = false のときの起動フォルダ。
# ・use_last_dir = true でフォルダにアクセスできないときのフォールバック先。
# 空欄ならホームディレクトリ、ホームにも移動できなければルート (/) を使います。
fixed_dir =

# ── ビューアー ──────────────────────────────────────────────────────────────
[viewer]
# 表示時の拡大縮小フィルタ：nearest / triangle / catmullrom / lanczos3
# catmullrom 推奨（フルサイズ表示で品質差が目に見えるためバランス重視）。
filter = catmullrom

# ビューアーを開くときの既定の位置・サイズに使うスロット番号（5 / 6 / 7 / 8 / 空欄）。
# F5〜F8 で保存したスロットを既定値として、ビューアーを開くたびに適用します。
# ・空欄、または 5〜8 以外を指定した場合はデフォルト無し（OS既定位置・800x600）。
# ・番号は正しくても該当スロットがまだ未保存の場合はデフォルト無しになります。
# ・適用後でも F5〜F8 を押せば、その回だけ別スロットへ切り替えられます。
default_slot =

# ── サムネイル ──────────────────────────────────────────────────────────────
[thumbnail]
# 縮小時のフィルタ：nearest / triangle / catmullrom / lanczos3
# triangle 推奨（256px 縮小では品質差が小さく速い）。
filter = triangle

# ── グリッド ────────────────────────────────────────────────────────────────
[grid]
# サムネイル長辺サイズ（px）。64〜512 の範囲で指定。幅は 1:√2 で自動計算。
thumb_size = 256

# ── ワーカー ────────────────────────────────────────────────────────────────
[worker]
# ページデコードの並列スレッド数。0 = 自動（論理コア数の半分）。
decode_threads = 0

# ── キャッシュ ──────────────────────────────────────────────────────────────
[cache]
# サムネイルのディスクキャッシュ保存先。
#   local : 実行ファイル配下の cache/ に保存（開発・確認用）
#   xdg   : ~/.local/share/nekoview/cache/ に保存（本番推奨）
storage = local

# ページキャッシュ（デコード済み画像）の最大メモリ上限（MB / 整数）。
# 既定はシステムRAMの25%。最小値は64MB。
# 通常は既定のままで問題ありません。指定する場合は行頭の '#' を外します。
# max_mb = 1024

# ファイルキャッシュ（圧縮ファイルまるごと）の最大メモリ上限（MB / 整数）。
# 既定はシステムRAMの5%。最小値は16MB。
# 通常は既定のままで問題ありません。指定する場合は行頭の '#' を外します。
# file_cache_max_mb = 200

# アニメーション（GIF/APNG/AVIF/WebP）のリングバッファ先読み枚数の下限・上限。
# 解像度に応じてこの範囲内で自動調整されます（大きいほど滑らかだがメモリを使う）。
# 空欄・不正値は既定（下限4 / 上限32）にフォールバックします。
# anim_ring_min_frames = 4
# anim_ring_max_frames = 32

# アニメーション1フレームあたりの生デコードサイズ上限（MB / 整数、リサイズ前のw*h*4基準）。
# 同一アニメ内で解像度が異常に大きいフレームに遭遇した際、そのフレームだけ縮小して再生を継続します。
# 一般的なアニメ解像度（4K級まで）は約34MB程度に収まるため、既定100MBで十分な余裕があります。
# 空欄・不正値は既定（100）にフォールバックします。
# anim_frame_hard_limit_mb = 100

# ── ログ ────────────────────────────────────────────────────────────────────
[log]
# パフォーマンス計測ログ（ページ読み込み時間など）。
perf = false
# キーイベント・スクロールの入力ログ。
key = true
# 起動・初期化など共通ログ。
common = true
";
