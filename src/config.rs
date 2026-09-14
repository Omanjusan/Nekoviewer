use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::gui_config::AppState;
use crate::keymap::Keymap;

// ── ログ設定グローバル ─────────────────────────────────────────────────────────
// AtomicBool を使うのは、AppConfig::load() より前（main() 冒頭）で一度 log() が
// 呼ばれてしまってもデフォルト値で確定させず、設定ダイアログ（デバッグタブ）で
// 変更された state 側の値を後から上書きできるようにするため（以前は
// OnceLock<LogConfig> で、一度確定すると二度と変更できないバグがあった）。

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
    /// 既定値はここに直書き（stateファイル経由の上書きのみ）。
    pub max_decode_edge: u32,
    /// キーアサイン設定（TODO項目J）。他のスカラー設定とは別のkeymap.iniから読み込む
    /// （Keymap::load/save参照）。行数が可変長で他のスカラー設定と性質が異なるため分離した。
    pub keymap: Keymap,
    /// keymap.ini/state/spread.redb の置き場所（resolve_config_root() で解決済み）。
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
    /// nekoviewer.conf は廃止済み。すべての既定値はここに直書きし、設定ダイアログで
    /// 変更した値は gui_config::AppState 経由で state ファイルへ永続化される
    /// （main() 起動シーケンスで state 側の値をこの既定値に上書き適用する）。
    pub fn load() -> Self {
        let root = resolve_config_root();

        AppConfig {
            thumb_filter: ResizeFilter::Triangle,
            viewer_filter: ResizeFilter::Lanczos3,
            thumb_size: 256,
            decode_threads: 0,
            startup: StartupConfig {
                use_last_dir: false,
                fixed_dir: None,
            },
            cache_total_mb: None,
            default_slot: None,
            anim_ring_min_frames: 4,
            anim_ring_max_frames: 32,
            anim_frame_hard_limit_mb: 100,
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_defaults_to_all_quiet_until_something_calls_set_log() {
        // AppConfig::load() はもうログ設定に触れない（nekoviewer.conf廃止）。
        // main()がstate側の値をset_log()で明示的に適用するまで、既定は全false。
        let cfg = log();
        assert!(!cfg.perf);
        assert!(!cfg.key);
        assert!(!cfg.common);
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
