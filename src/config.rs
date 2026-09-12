use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::gui_config::AppState;
use crate::keymap::Keymap;

// ── ログ設定グローバル ─────────────────────────────────────────────────────────
// AtomicBool を使うのは、AppConfig::load() より前（main() 冒頭）で一度 log() が
// 呼ばれてしまってもデフォルト値で確定させず、config.ini 読み込み後に上書きできる
// ようにするため（以前は OnceLock<LogConfig> で、一度確定すると二度と変更できず
// [log] key=false 等が反映されないバグがあった）。

pub struct LogConfig {
    pub perf:   bool,
    pub key:    bool,
    pub common: bool,
}

static LOG_PERF:   AtomicBool = AtomicBool::new(false);
static LOG_KEY:    AtomicBool = AtomicBool::new(false);
static LOG_COMMON: AtomicBool = AtomicBool::new(false);

/// どこからでも呼べるログ設定取得。AppConfig::load() より前に呼ぶとデフォルト値を返す。
pub fn log() -> LogConfig {
    LogConfig {
        perf:   LOG_PERF.load(Ordering::Relaxed),
        key:    LOG_KEY.load(Ordering::Relaxed),
        common: LOG_COMMON.load(Ordering::Relaxed),
    }
}

pub fn set_log(cfg: LogConfig) {
    LOG_PERF.store(cfg.perf, Ordering::Relaxed);
    LOG_KEY.store(cfg.key, Ordering::Relaxed);
    LOG_COMMON.store(cfg.common, Ordering::Relaxed);
}

// perf ログは高頻度パス（デコードループ等）から呼ばれるため、リングバッファ(Mutex)への
// 書き込みコストを避けてターミナル(eprintln!)出力のみに留める。
#[macro_export]
macro_rules! log_perf {
    ($($arg:tt)*) => { if $crate::config::log().perf   { eprintln!($($arg)*); } };
}
#[macro_export]
macro_rules! log_key {
    ($($arg:tt)*) => {
        if $crate::config::log().key {
            let msg = format!($($arg)*);
            eprintln!("{msg}");
            $crate::model_innerlog::push(&msg);
        }
    };
}
#[macro_export]
macro_rules! log_common {
    ($($arg:tt)*) => {
        if $crate::config::log().common {
            let msg = format!($($arg)*);
            eprintln!("{msg}");
            $crate::model_innerlog::push(&msg);
        }
    };
}

// ── 設定ファイル本体の置き場所（conf/keymap/state/spread.redb）───────────────
// 設定・キャッシュ・state・spread.redb は常に XDG（Windowsは%APPDATA%/%LOCALAPPDATA%）
// 固定。以前はバイナリ横保存(Local)も選べたが、AppImage/Flatpakで機能しない・
// 書き込み権限が環境依存という問題があったため廃止した。

/// Flatpak sandbox 内で実行中かどうか。
/// Flatpak は XDG_CACHE_HOME の扱いが通常と異なる（cache_root()参照）ため判定に使う。
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// 設定ファイル本体（conf/keymap/state/spread.redb）用の XDG ルート。
/// キャッシュ本体（cache_root()）は XDG_DATA_HOME を使うが、こちらは設定ファイルらしく
/// XDG_CONFIG_HOME を使う（Windowsは %APPDATA%）。
fn xdg_config_root() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    #[cfg(not(windows))]
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|_| PathBuf::from(".config"))
        });
    base.join("nekoview")
}

/// 設定フォルダ（XDG）を解決する。書き込み権限が無い場合はここで続行できないため
/// エラーメッセージを出して終了する（ユーザー環境の権限設定ミスを起動直後に検知させる）。
fn resolve_config_root() -> PathBuf {
    let root = xdg_config_root();
    if let Err(e) = ensure_writable_dir(&root) {
        eprintln!("[fatal] 設定フォルダに書き込めません: {} ({e})", root.display());
        std::process::exit(1);
    }
    root
}

/// dir を作成し、実際に書き込み可能か probe ファイルの作成・削除で確認する。
fn ensure_writable_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".write_test");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Lanczos3,
}

impl ResizeFilter {
    /// cache.redbへ保存する安定ID。enumの宣言順には依存させない。
    pub const fn thumbnail_cache_id(self) -> u32 {
        match self {
            Self::Nearest => 1,
            Self::Triangle => 2,
            Self::CatmullRom => 3,
            Self::Lanczos3 => 4,
        }
    }

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
    pub thumb_filter: ResizeFilter,
    pub viewer_filter: ResizeFilter,
    /// グリッドのサムネイル長辺サイズ（px）
    pub thumb_size: u32,
    /// ページデコードの並列スレッド数（0 = 自動: 論理コア数/2）
    pub decode_threads: usize,
    pub startup: StartupConfig,
    /// このアプリが使ってよいキャッシュ合計の上限（MB、ページ+ファイル）。None = システムRAMの30%。
    /// ページ/ファイルへの内訳は cache::resolve_cache_budgets の固定比率で分配する。
    pub cache_total_mb: Option<u64>,
    /// ビューアー既定スロット index（0..3 = F5〜F8）。None = デフォルト無し（空欄/不正値）
    pub default_slot: Option<usize>,
    /// アニメーションリングバッファの先読み枚数下限（フェーズ4）。空欄/不正値は既定4。
    pub anim_ring_min_frames: usize,
    /// アニメーションリングバッファの先読み枚数上限（フェーズ4）。空欄/不正値は既定32。
    pub anim_ring_max_frames: usize,
    /// アニメーション1フレームあたりの生デコードサイズ上限（MB、フェーズ5）。空欄/不正値は既定100。
    pub anim_frame_hard_limit_mb: usize,
    /// 表示デコードの取り扱い上限（長辺px）。短辺は縦横比を保って自動的に収まる。
    /// config.ini には持たず、既定値はここに直書き（stateファイル経由の上書きのみ）。
    pub max_decode_edge: u32,
    /// キーアサイン設定（TODO項目J）。config.iniとは別のkeymap.iniから読み込む
    /// （Keymap::load/save参照）。行数が可変長で他のスカラー設定と性質が異なるため分離した。
    pub keymap: Keymap,
    /// conf/keymap.ini/state/spread.redb の置き場所（resolve_config_root() で解決済み）。
    pub config_root: PathBuf,
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
        let root = resolve_config_root();
        let conf_path = root.join("nekoviewer.conf");

        let parsed = if conf_path.exists() {
            parse_ini(&conf_path)
        } else {
            let _ = std::fs::create_dir_all(&root);
            let _ = std::fs::write(&conf_path, DEFAULT_INI);
            ParsedIni::default()
        };

        // グローバルに設定（main() 冒頭の早期ログで確定した値も、ここで確実に上書きする）
        set_log(LogConfig {
            perf:   parsed.log_perf,
            key:    parsed.log_key.0,
            common: parsed.log_common.0,
        });

        AppConfig {
            thumb_filter: parsed.thumb_filter,
            viewer_filter: parsed.viewer_filter,
            thumb_size: parsed.thumb_size.0,
            decode_threads: parsed.decode_threads,
            startup: StartupConfig {
                use_last_dir: parsed.startup_use_last_dir,
                fixed_dir: parsed.startup_fixed_dir,
            },
            cache_total_mb: parsed.cache_total_mb,
            default_slot: parsed.default_slot,
            anim_ring_min_frames: parsed.anim_ring_min_frames.0,
            anim_ring_max_frames: parsed.anim_ring_max_frames.0,
            anim_frame_hard_limit_mb: parsed.anim_frame_hard_limit_mb.0,
            max_decode_edge: 1920,
            keymap: Keymap::load(&root),
            config_root: root,
        }
    }

    /// CLI引数のパスを解釈し、(起動時に開くディレクトリ, 起動後に自動で開くファイル) を返す。
    /// - DIR指定 → (そのDIR, None)
    /// - ファイル指定 → 親DIRにアクセス可能なら (親DIR, 対応拡張子ならそのファイル、非対応ならNone)
    /// - 上記のいずれも成立しない（存在しない・親DIRにもアクセス不可）→ (None, None)
    ///   （呼び出し元の resolve_start_dir が前回フォルダ→固定フォルダ→HOME のフォールバックへ委ねる）
    pub fn resolve_cli_open_target(path: PathBuf) -> (Option<PathBuf>, Option<PathBuf>) {
        if path.is_dir() {
            return (Some(path), None);
        }
        if path.is_file() {
            let openable = crate::fs::archive::detect::is_supported_image_file(&path)
                || crate::fs::dir::is_archive_path(&path);
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            if parent.is_dir() {
                return (Some(parent), if openable { Some(path) } else { None });
            }
        }
        (None, None)
    }

    /// 起動フォルダを最終決定する。CLI引数 > 前回フォルダ > フォールバック の優先順で
    /// 候補を選び、その候補が隠しディレクトリ経路（`.`始まりの構成要素を含む）でありながら
    /// 隠しフォルダ表示がオフのときは、由来（CLI引数・前回フォルダ・固定初期フォルダ）を
    /// 問わず候補を捨て、フォールバック機構（fixed_dir → HOME → ルート）へ委ねる。
    /// フォールバック先すら隠し経路なら fixed_dir も無視して HOME→ルートまで下がる。
    pub fn resolve_start_dir(&self, cli_path: Option<PathBuf>, state: &AppState) -> PathBuf {
        let candidate = match cli_path {
            Some(p) => p,
            None => self.startup_dir(state),
        };
        let fixed = self.startup.fixed_dir.as_deref()
            .filter(|p| !p.as_os_str().is_empty());
        guard_hidden_start_dir(candidate, state.show_hidden, fixed)
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
        if is_flatpak() {
            let base = std::env::var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    std::env::var("HOME")
                        .map(|h| PathBuf::from(h).join(".cache"))
                        .unwrap_or_else(|_| PathBuf::from(".cache"))
                });
            return Some(base.join("nekoview"));
        }

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

struct ParsedIni {
    thumb_filter: ResizeFilter,
    viewer_filter: ResizeFilter,
    thumb_size: ThumbSize,
    decode_threads: usize,
    log_perf:   bool,
    log_key:    LogDefault<false>,
    log_common: LogDefault<false>,
    startup_use_last_dir: bool,
    startup_fixed_dir: Option<PathBuf>,
    cache_total_mb: Option<u64>,
    default_slot: Option<usize>,
    anim_ring_min_frames: UsizeDefault<4>,
    anim_ring_max_frames: UsizeDefault<32>,
    anim_frame_hard_limit_mb: UsizeDefault<100>,
}

impl Default for ParsedIni {
    fn default() -> Self {
        Self {
            thumb_filter: ResizeFilter::Triangle,
            viewer_filter: ResizeFilter::Lanczos3,
            thumb_size: ThumbSize::default(),
            decode_threads: 0,
            log_perf: false,
            log_key: LogDefault(false),
            log_common: LogDefault(false),
            startup_use_last_dir: false,
            startup_fixed_dir: None,
            cache_total_mb: None,
            default_slot: None,
            anim_ring_min_frames: UsizeDefault::default(),
            anim_ring_max_frames: UsizeDefault::default(),
            anim_frame_hard_limit_mb: UsizeDefault::default(),
        }
    }
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
                ("cache", "cache_total_mb") => {
                    if let Ok(n) = v.parse::<u64>() {
                        result.cache_total_mb = Some(n.max(64));
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
                ("log", "key")    => result.log_key    = LogDefault(parse_bool(v, false)),
                ("log", "common") => result.log_common = LogDefault(parse_bool(v, false)),
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


/// 起動候補フォルダに隠し経路ガードを適用する。候補が隠しディレクトリ経路でありながら
/// 隠しフォルダ表示がオフのときは、候補を捨ててフォールバック機構（fixed → HOME → ルート）へ。
/// フォールバック先すら隠し経路なら fixed も無視して HOME→ルートまで下がる。
fn guard_hidden_start_dir(
    candidate: PathBuf,
    show_hidden: bool,
    fixed: Option<&std::path::Path>,
) -> PathBuf {
    if show_hidden || !path_has_hidden_component(&candidate) {
        return candidate;
    }
    log_common!(
        "[startup] 起動候補が隠し経路 かつ show_hidden=off → フォールバックへ: {:?}",
        candidate
    );
    let fb = resolve_fallback_dir(fixed);
    if path_has_hidden_component(&fb) {
        log_common!("[startup] フォールバック先も隠し経路 → fixed_dir を無視して HOME/ルートへ");
        resolve_fallback_dir(None)
    } else {
        fb
    }
}

/// パスの構成要素に隠しディレクトリ（`.` 始まりの通常セグメント）が含まれるか。
/// ルート（`/`）・カレント（`.`）・親（`..`）・Windows のドライブプレフィックスは対象外。
fn path_has_hidden_component(p: &std::path::Path) -> bool {
    use std::path::Component;
    p.components().any(|c| match c {
        Component::Normal(s) => s.to_str().map_or(false, |s| s.starts_with('.')),
        _ => false,
    })
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

pub(crate) fn parse_filter(s: &str) -> ResizeFilter {
    match s {
        "nearest"   => ResizeFilter::Nearest,
        "catmullrom" => ResizeFilter::CatmullRom,
        "lanczos3"  => ResizeFilter::Lanczos3,
        _           => ResizeFilter::Triangle,
    }
}

pub fn filter_to_str(f: ResizeFilter) -> &'static str {
    match f {
        ResizeFilter::Nearest    => "nearest",
        ResizeFilter::Triangle   => "triangle",
        ResizeFilter::CatmullRom => "catmullrom",
        ResizeFilter::Lanczos3   => "lanczos3",
    }
}

const DEFAULT_INI: &str = "\
# ============================================================================
#  Nekoviewer 設定ファイル (nekoviewer.conf)
#
#  ・常に ~/.config/nekoview/ （Windowsは%APPDATA%\\nekoview）に置かれます。
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
# lanczos3 推奨（ビューアー表示の画質を優先）。
filter = lanczos3

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
# キャッシュ合計（ページキャッシュ+ファイルキャッシュ）の最大メモリ上限（MB / 整数）。
# 内訳はページ70% : ファイル30%に自動分配されます。既定はシステムRAMの30%。最小値は64MB。
# 通常は既定のままで問題ありません。指定する場合は行頭の '#' を外します。
# cache_total_mb = 2048

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
key = false
# 起動・初期化など共通ログ。
common = false
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_config_defaults_to_quiet_logs_and_lanczos_viewer_filter() {
        let parsed = ParsedIni::default();
        assert!(!parsed.log_perf);
        assert!(!parsed.log_key.0);
        assert!(!parsed.log_common.0);
        assert!(matches!(parsed.thumb_filter, ResizeFilter::Triangle));
        assert!(matches!(parsed.viewer_filter, ResizeFilter::Lanczos3));

        assert!(DEFAULT_INI.contains("[viewer]\n"));
        assert!(DEFAULT_INI.contains("filter = lanczos3"));
        assert!(DEFAULT_INI.contains("perf = false"));
        assert!(DEFAULT_INI.contains("key = false"));
        assert!(DEFAULT_INI.contains("common = false"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nekoviewer_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ensure_writable_dir_creates_and_accepts_writable_dir() {
        let dir = temp_dir("writable_ok");
        assert!(ensure_writable_dir(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn ensure_writable_dir_fails_on_read_only_parent() {
        use std::os::unix::fs::PermissionsExt;
        let parent = temp_dir("writable_ng_parent");
        let dir = parent.join("child");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(ensure_writable_dir(&dir).is_err());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn is_flatpak_reflects_env_var() {
        unsafe { std::env::set_var("FLATPAK_ID", "io.github.Omanjusan.Nekoviewer"); }
        assert!(is_flatpak());
        unsafe { std::env::remove_var("FLATPAK_ID"); }
        assert!(!is_flatpak());
    }

    #[test]
    fn path_has_hidden_component_detects_dot_segments() {
        use std::path::Path;
        assert!(path_has_hidden_component(Path::new("/home/user/.config/app")));
        assert!(path_has_hidden_component(Path::new(".cache/thumbs")));
        assert!(path_has_hidden_component(Path::new("/srv/.snapshots")));
        assert!(!path_has_hidden_component(Path::new("/home/user/Pictures")));
        assert!(!path_has_hidden_component(Path::new("/")));
        assert!(!path_has_hidden_component(Path::new("../sibling")));
    }

    #[test]
    fn guard_hidden_start_dir_passes_through_when_show_hidden_on() {
        let hidden = PathBuf::from("/home/user/.config/app");
        assert_eq!(
            guard_hidden_start_dir(hidden.clone(), true, None),
            hidden,
            "show_hidden=on なら隠し経路でもそのまま復帰する"
        );
    }

    #[test]
    fn guard_hidden_start_dir_passes_through_for_visible_path() {
        let visible = PathBuf::from("/home/user/Pictures");
        assert_eq!(
            guard_hidden_start_dir(visible.clone(), false, None),
            visible,
            "隠し経路でなければ show_hidden の値に関わらずそのまま"
        );
    }

    #[test]
    fn guard_hidden_start_dir_falls_back_to_fixed_when_hidden_and_off() {
        let fixed = temp_dir("guard_fixed_visible");
        let got = guard_hidden_start_dir(
            PathBuf::from("/home/user/.local/share/x"),
            false,
            Some(fixed.as_path()),
        );
        assert_eq!(got, fixed, "隠し経路 × show_hidden=off → fixed_dir へフォールバック");
        let _ = std::fs::remove_dir_all(&fixed);
    }

    #[test]
    fn guard_hidden_start_dir_ignores_hidden_fixed_and_drops_further() {
        // 候補も fixed も隠し経路（fixed は実在しない）。最終結果には
        // 「隠しの候補」も「隠しの fixed」も出てこず、HOME/ルートまで下がる。
        // ※ HOME が隠し経路を含まない一般的な環境を前提にした判定。
        let hidden_candidate = PathBuf::from("/data/.snapshots/latest");
        let hidden_fixed = PathBuf::from("/nonexistent/.fixed");
        let got = guard_hidden_start_dir(hidden_candidate.clone(), false, Some(hidden_fixed.as_path()));
        assert_ne!(got, hidden_candidate);
        assert_ne!(got, hidden_fixed);
        assert!(!path_has_hidden_component(&got), "最終結果は隠し経路でない");
    }
}
