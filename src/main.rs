#![cfg_attr(windows, windows_subsystem = "windows")]
mod anim;
mod cache;
mod card_date_format;
mod config;
mod controller;
mod decode_jobs;
mod favorites;
mod fs;
mod gui_config;
mod i18n;
mod types;
mod keymap;
mod model_innerlog;
mod neko_dir;
mod rotation;
mod single_instance;
mod spread_offset;
mod spread_state;
mod toolbar;
mod translate;
mod view_explorer;
mod view_gui_config;
mod view_innerlog;
mod view_reader;
mod view_status;
#[cfg(windows)]
mod win_registry;

mod winit_app;

use std::path::PathBuf;

fn main() {
    // config読み込み前はログ設定のデフォルト（common=false）が使われるため、この行自体は
    // 実際には出力されない。設定確定後の "[startup] config loaded" 以降が実質の起点。
    log_common!("[startup] main() start");

    let instance_guard = match single_instance::acquire() {
        single_instance::AcquireResult::Acquired(guard) => guard,
        single_instance::AcquireResult::AlreadyRunning => {
            log_common!("[startup] already running -> send ping");
            if single_instance::send_ping() {
                std::process::exit(0);
            } else {
                // 先発プロセスがロックを握ったままpingに応答しない
                // （フリーズ等）。救済せずエラー終了する。
                log_common!("[startup] ping failed (existing process unresponsive) -> exit(1)");
                std::process::exit(1);
            }
        }
    };

    // Windows は windows_subsystem="windows" によりコンソールを持たないため、
    // 初期化中に panic が起きても標準エラーが誰にも見えず、プロセスが無言で
    // 消えるだけになる。init_result で起動シーケンスを catch_unwind して
    // 拾い、Windows ではダイアログで知らせてから終了する（Linuxは従来通り
    // 標準エラーへの panic メッセージで足りるため、そちらに任せる）。
    let init_result = std::panic::catch_unwind(|| {
        let mut cfg = config::AppConfig::load();
        log_common!("[startup] config loaded");

        let state = gui_config::load_state(&cfg.config_root);
        log_common!("[startup] state loaded (window_size = {:?})", state.window_size);
        i18n::set_from_code(&state.lang);

        // 設定ダイアログ（共通/アニメタブ）で編集された値は state 側が config.ini より優先される。
        if let Some(v) = state.app_cache_total_mb { cfg.cache_total_mb = Some(v); }
        if let Some(v) = state.app_anim_ring_min_frames { cfg.anim_ring_min_frames = v; }
        if let Some(v) = state.app_anim_ring_max_frames { cfg.anim_ring_max_frames = v; }
        if let Some(v) = state.app_anim_frame_hard_limit_mb { cfg.anim_frame_hard_limit_mb = v; }
        if let Some(v) = state.app_viewer_filter { cfg.viewer_filter = v; }
        if let Some(v) = state.app_max_decode_edge { cfg.max_decode_edge = v; }

        // デバッグタブで編集されたログ設定も同様に state 側を優先する。
        let mut log_cfg = config::log();
        if let Some(v) = state.app_log_perf { log_cfg.perf = v; }
        if let Some(v) = state.app_log_key { log_cfg.key = v; }
        if let Some(v) = state.app_log_common { log_cfg.common = v; }
        config::set_log(log_cfg);

        // その他タブで編集された起動時フォルダ設定も同様に state 側を優先する。
        if let Some(v) = state.app_startup_use_last_dir { cfg.startup.use_last_dir = v; }
        if let Some(v) = state.app_startup_fixed_dir.clone() { cfg.startup.fixed_dir = Some(v); }

        // フェーズ4a: thumb_size/thumb_filter も config.ini直接保存を廃止しstate側優先へ。
        if let Some(v) = state.app_thumb_filter { cfg.thumb_filter = v; }
        if let Some(v) = state.app_thumb_size { cfg.thumb_size = v; }

        fs::mount::log_gvfs_status();
        log_common!("[startup] gvfs check done");

        let args = CliArgs::parse();
        if let Some(v) = args.cache_max_mb { cfg.cache_total_mb = Some(v.max(64)); }

        // 「賢く開く」：ファイル指定なら親DIR起動＋起動後の自動オープン対象を、DIR指定なら
        // そのDIRをそれぞれ導出する。いずれも成立しなければ従来通りの起動フォルダ解決へ委ねる。
        let (cli_dir, open_target) = match args.start_path {
            Some(p) => config::AppConfig::resolve_cli_open_target(p),
            None => (None, None),
        };
        let start_dir = cfg.resolve_start_dir(cli_dir, &state);
        log_common!("[startup] start_dir = {:?}, open_target = {:?}", start_dir, open_target);

        (cfg, state, start_dir, open_target)
    });

    let (cfg, state, start_dir, open_target) = match init_result {
        Ok(v) => v,
        Err(_) => {
            show_init_failure_dialog();
            std::process::exit(1);
        }
    };

    log_common!("[startup] starting winit event loop ...");
    winit_app::run(start_dir, cfg, state, open_target);
    drop(instance_guard);
}

/// 初期化失敗時のブロッキングダイアログ（Windowsのみ）。Linuxはコンソール起動が
/// 前提のため、panic 時の標準エラー出力（既定の panic hook）で足りるとして何もしない。
#[cfg(windows)]
fn show_init_failure_dialog() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK, MB_ICONERROR};

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let text = to_wide(
        "初期化に失敗しました。\n\
         nekoviewer.state, nekoviewer.conf に汚染の疑いがあるので、\n\
         バックアップをとってから削除して再起動することをおすすめします。",
    );
    let caption = to_wide("Nekoviewer");

    unsafe {
        MessageBoxW(0, text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn show_init_failure_dialog() {}

/// 窓ごとの egui::Context を生成した直後に、日本語フォントとスタイルを適用する。
/// （旧 eframe では cc.egui_ctx に対し 1 回だけ行っていたが、winit では窓ごとに Context を持つ）
fn setup_egui_context(ctx: &egui::Context) {
    setup_japanese_font(ctx);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.scroll.bar_outer_margin = 0.0;
    });
    ctx.style_mut_of(egui::Theme::Light, |s| {
        s.spacing.scroll.bar_outer_margin = 0.0;
    });
}

fn setup_japanese_font(ctx: &egui::Context) {
    let Some(font_data) = japanese_font_data() else { return };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "PrimaryCJK".to_owned(),
        egui::FontData::from_owned(font_data).into(),
    );
    // ASCII はデフォルト優先、日本語はこちらにフォールバック
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("PrimaryCJK".to_owned());

    // Windows の日本語フォントは簡体字グリフを持たないため、
    // 簡体字中国語フォントをさらに後段の fallback として追加する。
    // Linux は NotoSansCJK が全 CJK をカバーするので不要。
    if let Some(cn_data) = simplified_chinese_font_data() {
        fonts.font_data.insert(
            "SimpChinese".to_owned(),
            egui::FontData::from_owned(cn_data).into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("SimpChinese".to_owned());
    }

    ctx.set_fonts(fonts);
}

#[cfg(target_os = "windows")]
fn japanese_font_data() -> Option<Vec<u8>> {
    let candidates = [
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
        r"C:\Windows\Fonts\YuGothM.ttc",
    ];
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

#[cfg(target_os = "windows")]
fn simplified_chinese_font_data() -> Option<Vec<u8>> {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",    // Microsoft YaHei（Win Vista 以降に同梱）
        r"C:\Windows\Fonts\simsun.ttc",  // SimSun
        r"C:\Windows\Fonts\simhei.ttf",  // SimHei
    ];
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

#[cfg(target_os = "linux")]
fn japanese_font_data() -> Option<Vec<u8>> {
    let candidates = [
        // Flatpak exposes the host fonts below /run/host/fonts.  Reading the
        // font bytes directly is necessary because egui does not use
        // fontconfig by itself.
        "/run/host/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/run/host/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/run/host/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ];
    candidates.iter().find_map(|p| std::fs::read(p).ok()).or_else(|| {
        // Font locations differ between distributions and host setups.
        // Flatpak makes the host fontconfig database available, so ask it for
        // a Japanese-capable sans-serif font when no known path matched.
        let output = std::process::Command::new("fc-match")
            .args(["-f", "%{file}\n", "sans-serif:lang=ja"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?;
        path.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .and_then(|path| std::fs::read(path).ok())
    })
}

#[cfg(target_os = "macos")]
fn japanese_font_data() -> Option<Vec<u8>> {
    std::fs::read("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc").ok()
}

#[cfg(not(target_os = "windows"))]
fn simplified_chinese_font_data() -> Option<Vec<u8>> {
    None // NotoSansCJK が全 CJK をカバーするため追加フォント不要
}

// ── コマンドライン引数 ────────────────────────────────────────────────────────

struct CliArgs {
    start_path:   Option<PathBuf>,
    /// キャッシュ合計（ページ+ファイル）の上限MB。
    cache_max_mb: Option<u64>,
}

impl CliArgs {
    /// `--cache-max-mb=N` / `--cache-max-mb N` 形式を手動パース。
    /// 不明なオプションは無視、位置引数は start_path として扱う。
    fn parse() -> Self {
        let mut start_path   = None::<PathBuf>;
        let mut cache_max_mb = None::<u64>;

        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            if let Some(rest) = arg.strip_prefix("--cache-max-mb") {
                cache_max_mb = Self::take_value(rest, &mut it);
            } else if !arg.starts_with('-') {
                // 複数の位置引数（複数ファイル/複数DIR）が来ても先頭のみを対象とし、以降は無視する。
                if start_path.is_none() {
                    start_path = Some(PathBuf::from(&arg));
                }
            }
            // 未知の --xxx オプションは無視
        }

        Self { start_path, cache_max_mb }
    }

    fn take_value(rest: &str, it: &mut impl Iterator<Item = String>) -> Option<u64> {
        let s = if let Some(s) = rest.strip_prefix('=') {
            s.to_owned()
        } else {
            it.next().unwrap_or_default()
        };
        s.parse::<u64>().ok()
    }
}
