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

// ── 設定ファイル本体の置き場所（conf/keymap/state/spread.redb）───────────────
// フェーズ2: 従来 [cache] storage は cache_root() のみに影響していたが、conf本体・
// keymap.ini・nekoviewer.state・nekoviewer_spread.redb 全部の置き場所も同じ設定で
// 統一する（AppImage対応: バイナリ横は読み取り専用squashfsマウントで機能しないため）。

/// AppImage実行中かどうか（AppImage実行時に環境変数 APPIMAGE が自動で立つ）。
/// バイナリ横（current_exe()の親）は起動毎に変わる一時マウントパスになるため、
/// この場合は storage 設定に関わらず常に XDG を使う。
pub fn is_appimage() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}

/// Flatpak sandbox 内で実行中かどうか。
/// Flatpak は `/app` を読み取り専用で提供するため、実行ファイル横への保存は許可しない。
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

pub fn is_read_only_package() -> bool {
    is_appimage() || is_flatpak()
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

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// unix epoch秒を "YYYY-MM-DD HH:MM:SS UTC" に変換する（依存クレード無しの自前実装、
/// Howard Hinnant の civil_from_days アルゴリズムに基づく）。両方confが見つかった際の
/// ダイアログ表示にのみ使う簡易フォーマットで、タイムゾーン変換等は行わない。
pub fn format_epoch(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };

    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// バイナリ横・XDG両方に有効な conf が見つかった際の情報（起動時ダイアログ用）。
#[derive(Clone)]
pub struct ConfigConflict {
    pub exe_root: PathBuf,
    pub exe_updated_at: Option<u64>,
    pub xdg_root: PathBuf,
    pub xdg_updated_at: Option<u64>,
}

/// conf の置き場所を解決する。
/// 優先順位: ①読み取り専用パッケージ内なら常にXDG ②バイナリ横に既存confがあればそれ（後方互換）
/// ③XDGに既存confがあればそれ ④どちらにも無ければ新規はXDG（新規インストールの既定値）
/// ⑤両方に既存confがあれば暫定でXDGを採用しつつ conflict を返す（起動後にダイアログで解消）
fn resolve_config_root() -> (PathBuf, Option<ConfigConflict>) {
    let xdg_root = xdg_config_root();

    if is_read_only_package() {
        return (xdg_root, None);
    }

    let exe_root = exe_dir();
    let exe_conf = exe_root.as_ref().map(|d| d.join("nekoviewer.conf"));
    let xdg_conf = xdg_root.join("nekoviewer.conf");

    let exe_exists = exe_conf.as_ref().is_some_and(|p| p.exists());
    let xdg_exists = xdg_conf.exists();

    match (exe_exists, xdg_exists) {
        (true, true) => {
            let exe_root = exe_root.unwrap();
            let exe_dt = parse_ini(exe_conf.as_ref().unwrap()).updated_at;
            let xdg_dt = parse_ini(&xdg_conf).updated_at;
            let conflict = ConfigConflict {
                exe_root: exe_root.clone(),
                exe_updated_at: exe_dt,
                xdg_root: xdg_root.clone(),
                xdg_updated_at: xdg_dt,
            };
            // このセッションの暫定選択は新しい方（同着・欠損はXDG優先）。
            let root = if exe_dt.unwrap_or(0) > xdg_dt.unwrap_or(0) { exe_root } else { xdg_root };
            (root, Some(conflict))
        }
        (true, false) => (exe_root.unwrap(), None),
        (false, _) => (xdg_root, None),
    }
}

/// 設定ファイル一式（conf/keymap.ini/nekoviewer.state/nekoviewer_spread.redb）を
/// from から to へコピーし、成功した分は from 側を削除する（対称性を残さない）。
/// サムネイルキャッシュ本体（cache.redb）は対象外（再生成させる。容量・時間のコストを避ける）。
/// 戻り値: 削除に失敗したファイルパスの一覧（空なら完全成功）。
pub fn migrate_storage_files(from: &std::path::Path, to: &std::path::Path) -> Vec<PathBuf> {
    let names = ["nekoviewer.conf", "keymap.ini", "nekoviewer.state", "nekoviewer_spread.redb"];
    let mut delete_failed = Vec::new();

    let _ = std::fs::create_dir_all(to);
    for name in names {
        let src = from.join(name);
        if !src.exists() { continue; }
        let dst = to.join(name);
        if std::fs::copy(&src, &dst).is_err() { continue; }
        if std::fs::remove_file(&src).is_err() {
            delete_failed.push(src);
        }
    }
    delete_failed
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
    /// キーアサイン設定（TODO項目J）。config.iniとは別のkeymap.iniから読み込む
    /// （Keymap::load/save参照）。行数が可変長で他のスカラー設定と性質が異なるため分離した。
    pub keymap: Keymap,
    /// conf/keymap.ini/state/spread.redb の置き場所（resolve_config_root() で解決済み）。
    pub config_root: PathBuf,
    /// バイナリ横・XDG両方に有効なconfが見つかった場合の情報。Some の間はUI側で
    /// 選択ダイアログを出す（起動直後の稀な安全網パス）。
    pub conflict: Option<ConfigConflict>,
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
        let (root, conflict) = resolve_config_root();
        let conf_path = root.join("nekoviewer.conf");

        let parsed = if conf_path.exists() {
            parse_ini(&conf_path)
        } else {
            let _ = std::fs::create_dir_all(&root);
            let _ = std::fs::write(&conf_path, default_ini_with_timestamp());
            ParsedIni::default()
        };

        // グローバルに設定（main() 冒頭の早期ログで確定した値も、ここで確実に上書きする）
        set_log(LogConfig {
            perf:   parsed.log_perf,
            key:    parsed.log_key.0,
            common: parsed.log_common.0,
        });

        AppConfig {
            cache_storage: effective_cache_storage(parsed.storage, is_read_only_package()),
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
            conflict,
        }
    }

    /// 起動時ダイアログでユーザーが conflict のどちらかを選んだ後に呼ぶ。
    /// 選んだ側の updated_at を「今」に更新して書き戻し、次回起動時に同じ質問が
    /// 繰り返し出るのを防ぐ。選ばなかった側は削除せず残す（ユーザーが手動で消す前提）。
    pub fn resolve_conflict(&mut self, chosen_root: PathBuf) {
        self.config_root = chosen_root.clone();
        let conf_path = chosen_root.join("nekoviewer.conf");
        let parsed = parse_ini(&conf_path);
        self.cache_storage = parsed.storage;
        self.save();
        self.conflict = None;
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

    /// 設定ダイアログの[反映]時に呼ぶ。config.ini本体のうち、ダイアログ経由で変更可能だが
    /// これまで永続化されていなかった単純スカラー項目（thumb_size, thumb_filter）を
    /// 行単位で書き換えて保存する。他の項目（cache_total_mb等）は既にstateファイル
    /// (gui_config::save_state)経由で永続化済みのためここでは触らない。
    pub fn save(&self) {
        let dir = &self.config_root;
        let path = dir.join("nekoviewer.conf");
        let content = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_INI.to_string());

        let updates = [
            ("thumbnail", "filter", filter_to_str(self.thumb_filter).to_string()),
            ("grid", "thumb_size", self.thumb_size.to_string()),
            ("cache", "storage", storage_to_str(self.cache_storage).to_string()),
            ("meta", "updated_at", now_epoch().to_string()),
        ];
        let new_content = apply_ini_updates(&content, &updates);

        let _ = std::fs::create_dir_all(dir);
        let tmp = dir.join("nekoviewer.conf.tmp");
        let bak = dir.join("nekoviewer.conf.bak");
        if std::fs::write(&tmp, &new_content).is_err() { return; }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let _ = std::fs::write(&bak, &new_content);
    }

    /// 設定画面で storage(local/xdg) を切り替えたときに呼ぶ。
    /// 新しい置き場所へ conf/keymap.ini/state/spread.redb をコピーし、成功分は旧側を削除する。
    /// 戻り値: 削除に失敗したファイルパス一覧（空なら完全成功、UI側は手動削除を案内する）。
    pub fn migrate_storage(&mut self, to: CacheStorage) -> Vec<PathBuf> {
        let from_root = self.config_root.clone();
        let to_root = match to {
            CacheStorage::Local if is_read_only_package() => xdg_config_root(),
            CacheStorage::Local => exe_dir().unwrap_or_else(|| from_root.clone()),
            CacheStorage::Xdg => xdg_config_root(),
        };
        if to_root == from_root {
            return Vec::new();
        }

        let delete_failed = migrate_storage_files(&from_root, &to_root);

        self.config_root = to_root;
        self.cache_storage = to;
        self.keymap = Keymap::load(&self.config_root);
        self.save();

        delete_failed
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

fn effective_cache_storage(configured: CacheStorage, is_read_only_package: bool) -> CacheStorage {
    if is_read_only_package {
        CacheStorage::Xdg
    } else {
        configured
    }
}

struct ParsedIni {
    storage: CacheStorage,
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
    /// [meta] updated_at（unix epoch秒）。バイナリ横・XDG両方にconfが見つかった際に
    /// どちらが新しいか比較するために使う。無ければ None（未対応の旧フォーマット扱い）。
    updated_at: Option<u64>,
}

impl Default for ParsedIni {
    fn default() -> Self {
        Self {
            storage: CacheStorage::default(),
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
            updated_at: None,
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
                ("meta", "updated_at") => {
                    result.updated_at = v.parse::<u64>().ok();
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

pub fn storage_to_str(s: CacheStorage) -> &'static str {
    match s {
        CacheStorage::Local => "local",
        CacheStorage::Xdg   => "xdg",
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
#  ・[cache] storage 設定に従い、実行ファイルと同じフォルダ、または
#    ~/.config/nekoview/ に置かれます（AppImage/Flatpakでは常に後者）。
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
# サムネイルのディスクキャッシュ保存先。conf/keymap.ini/state/spread.redbの置き場所も
# これに従います（AppImage/Flatpak実行時は常にxdg扱いになります）。
#   local : 実行ファイル配下に保存（開発・確認用。AppImage/Flatpakでは機能しません）
#   xdg   : ~/.config/nekoview/（設定）・~/.local/share/nekoview/cache/（キャッシュ）に保存（推奨）
storage = xdg

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

# ── メタ情報（手動編集不要）─────────────────────────────────────────────────
[meta]
# このconfが最後に保存された日時（unix epoch秒）。バイナリ横・XDG両方にconfが
# 見つかった場合にどちらが新しいか判定するために使います。
updated_at = 0
";

/// 新規作成時、DEFAULT_INI の updated_at プレースホルダを現在時刻で埋めて返す。
fn default_ini_with_timestamp() -> String {
    apply_ini_updates(DEFAULT_INI, &[("meta", "updated_at", now_epoch().to_string())])
}

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

    #[test]
    fn read_only_package_reports_xdg_as_effective_storage() {
        assert!(matches!(
            effective_cache_storage(CacheStorage::Local, true),
            CacheStorage::Xdg
        ));
        assert!(matches!(
            effective_cache_storage(CacheStorage::Local, false),
            CacheStorage::Local
        ));
    }

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

    #[test]
    fn format_epoch_known_value() {
        // 2024-01-01 00:00:00 UTC
        assert_eq!(format_epoch(1704067200), "2024-01-01 00:00:00 UTC");
    }

    #[test]
    fn format_epoch_epoch_zero() {
        assert_eq!(format_epoch(0), "1970-01-01 00:00:00 UTC");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nekoviewer_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migrate_storage_files_copies_and_deletes_source() {
        let from = temp_dir("migrate_from_a");
        let to = temp_dir("migrate_to_a");
        std::fs::write(from.join("nekoviewer.conf"), "dummy conf").unwrap();
        std::fs::write(from.join("keymap.ini"), "dummy keymap").unwrap();
        // state/spread.redb は存在しないケース(未使用ユーザー)も許容する

        let failed = migrate_storage_files(&from, &to);

        assert!(failed.is_empty(), "削除失敗が無いこと: {failed:?}");
        assert!(to.join("nekoviewer.conf").exists(), "コピー先にconfが存在する");
        assert!(to.join("keymap.ini").exists(), "コピー先にkeymapが存在する");
        assert!(!from.join("nekoviewer.conf").exists(), "コピー元のconfは削除される");
        assert!(!from.join("keymap.ini").exists(), "コピー元のkeymapは削除される");
        assert_eq!(std::fs::read_to_string(to.join("nekoviewer.conf")).unwrap(), "dummy conf");

        let _ = std::fs::remove_dir_all(&from);
        let _ = std::fs::remove_dir_all(&to);
    }

    #[test]
    fn migrate_storage_files_skips_absent_files_without_error() {
        let from = temp_dir("migrate_from_b");
        let to = temp_dir("migrate_to_b");
        // from は空（何もコピーされないはず）

        let failed = migrate_storage_files(&from, &to);

        assert!(failed.is_empty());
        assert!(!to.join("nekoviewer.conf").exists());

        let _ = std::fs::remove_dir_all(&from);
        let _ = std::fs::remove_dir_all(&to);
    }

    #[test]
    fn is_appimage_reflects_env_var() {
        // 他テストと並列実行されるため env::set_var の副作用リスクはあるが、
        // このテストは自分で set→unset まで完結させるので実害は無い。
        unsafe { std::env::set_var("APPIMAGE", "/tmp/dummy.AppImage"); }
        assert!(is_appimage());
        unsafe { std::env::remove_var("APPIMAGE"); }
        assert!(!is_appimage());
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
