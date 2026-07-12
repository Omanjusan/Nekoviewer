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
static LOG_KEY:    AtomicBool = AtomicBool::new(true);
static LOG_COMMON: AtomicBool = AtomicBool::new(true);

/// どこからでも呼べるログ設定取得。AppConfig::load() より前に呼ぶとデフォルト値を返す。
pub fn log() -> LogConfig {
    LogConfig {
        perf:   LOG_PERF.load(Ordering::Relaxed),
        key:    LOG_KEY.load(Ordering::Relaxed),
        common: LOG_COMMON.load(Ordering::Relaxed),
    }
}

fn set_log(cfg: LogConfig) {
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

#[derive(Clone, Copy, PartialEq)]
pub enum CacheStorage {
    /// 実行ファイル配下の cache/ に保存（開発・確認用）
    Local,
    /// ~/.local/share/nekoview/cache/ に保存（本番推奨）
    Xdg,
}

#[derive(Clone, Copy, PartialEq)]
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
    /// キーアサイン設定（TODO項目J）。[keymap] セクションから読み込む。
    pub keymap: Keymap,
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

        // グローバルに設定（main() 冒頭の早期ログで確定した値も、ここで確実に上書きする）
        set_log(LogConfig {
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
            cache_total_mb: parsed.cache_total_mb,
            default_slot: parsed.default_slot,
            anim_ring_min_frames: parsed.anim_ring_min_frames.0,
            anim_ring_max_frames: parsed.anim_ring_max_frames.0,
            anim_frame_hard_limit_mb: parsed.anim_frame_hard_limit_mb.0,
            max_decode_edge: 1920,
            keymap: parsed.keymap,
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

    /// 設定ダイアログの[反映]時に呼ぶ。config.ini本体のうち、ダイアログ経由で変更可能だが
    /// これまで永続化されていなかった単純スカラー項目（thumb_size, thumb_filter）を
    /// 行単位で書き換えて保存する。他の項目（cache_total_mb等）は既にstateファイル
    /// (gui_config::save_state)経由で永続化済みのためここでは触らない。
    pub fn save(&self) {
        let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
        let path = dir.join("nekoviewer.conf");
        let content = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_INI.to_string());

        let updates = [
            ("thumbnail", "filter", filter_to_str(self.thumb_filter).to_string()),
            ("grid", "thumb_size", self.thumb_size.to_string()),
        ];
        let new_content = apply_ini_updates(&content, &updates);

        let tmp = dir.join("nekoviewer.conf.tmp");
        let bak = dir.join("nekoviewer.conf.bak");
        if std::fs::write(&tmp, &new_content).is_err() { return; }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = std::fs::write(&bak, &new_content);
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
    cache_total_mb: Option<u64>,
    default_slot: Option<usize>,
    anim_ring_min_frames: UsizeDefault<4>,
    anim_ring_max_frames: UsizeDefault<32>,
    anim_frame_hard_limit_mb: UsizeDefault<100>,
    keymap: Keymap,
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
            if section == "keymap" {
                result.keymap.apply_ini_entry(k, v);
                continue;
            }
            match (section.as_str(), k) {
                ("cache", "storage") => {
                    result.storage = match v {
                        "xdg" => CacheStorage::Xdg,
                        _ => CacheStorage::Local,
                    };
                }
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

/// `[section]\nkey = value` 形式の行を、行単位で書き換える（コメント行・他のキーの行は
/// そのまま保持）。コメントアウトされている対象キー（`# key = ...`）は非コメント化して
/// 値を書き込む。対象キーがそのセクションに存在しない場合は、末尾に新規セクションとして追記する。
fn apply_ini_updates(content: &str, updates: &[(&str, &str, String)]) -> String {
    let mut out = String::new();
    let mut section = String::new();
    let mut applied = vec![false; updates.len()];
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].to_string();
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let body = trimmed.strip_prefix('#').or_else(|| trimmed.strip_prefix(';'))
            .map(|s| s.trim_start()).unwrap_or(trimmed);
        if let Some((k, _)) = body.split_once('=') {
            let k = k.trim();
            if let Some(idx) = updates.iter().position(|(s, key, _)| *s == section && *key == k) {
                out.push_str(&format!("{} = {}\n", k, updates[idx].2));
                applied[idx] = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    for (idx, (sec, key, val)) in updates.iter().enumerate() {
        if !applied[idx] {
            out.push_str(&format!("\n[{sec}]\n{key} = {val}\n"));
        }
    }
    out
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

# ── キーアサイン ────────────────────────────────────────────────────────────
[keymap]
# キー割り当てのカスタマイズ（TODO項目J、設定UIは今後追加予定）。
# 形式: reader.<アクション名>.keyboard / .mouse = 値（未指定のアクションは既定値を使用）。
# キーボード値の例: ArrowUp / shift+ArrowUp / alt+Enter
# マウス値の例: wheel_up / shift_wheel_down / middle_click

# ── ログ ────────────────────────────────────────────────────────────────────
[log]
# パフォーマンス計測ログ（ページ読み込み時間など）。
perf = false
# キーイベント・スクロールの入力ログ。
key = true
# 起動・初期化など共通ログ。
common = true
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_ini_updates_rewrites_existing_key_in_place() {
        let content = "[thumbnail]\n# 縮小時のフィルタ\nfilter = triangle\n\n[grid]\nthumb_size = 256\n";
        let updates = [
            ("thumbnail", "filter", "lanczos3".to_string()),
            ("grid", "thumb_size", "320".to_string()),
        ];
        let out = apply_ini_updates(content, &updates);
        assert!(out.contains("filter = lanczos3"));
        assert!(out.contains("thumb_size = 320"));
        assert!(out.contains("# 縮小時のフィルタ"), "コメント行は保持される");
        assert!(!out.contains("filter = triangle"));
    }

    #[test]
    fn apply_ini_updates_uncomments_commented_key() {
        let content = "[cache]\n# cache_total_mb = 2048\n";
        let updates = [("cache", "cache_total_mb", "3000".to_string())];
        let out = apply_ini_updates(content, &updates);
        assert!(out.contains("cache_total_mb = 3000"));
        assert!(!out.contains("# cache_total_mb"));
    }

    #[test]
    fn apply_ini_updates_appends_missing_key_as_new_section() {
        let content = "[startup]\nuse_last_dir = false\n";
        let updates = [("grid", "thumb_size", "128".to_string())];
        let out = apply_ini_updates(content, &updates);
        assert!(out.contains("use_last_dir = false"), "既存の内容は維持");
        assert!(out.contains("[grid]"));
        assert!(out.contains("thumb_size = 128"));
    }
}
