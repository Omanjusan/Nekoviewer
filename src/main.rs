mod anim;
mod cache;
mod config;
mod controller;
mod fs;
mod i18n;
mod model;
mod neko_dir;
mod spread_offset;
mod view_explorer;
mod view_reader;
mod view_status;

mod winit_app;

use std::path::PathBuf;

fn main() {
    // config 読み込み前なのでデフォルト値（common=true）でログ出力
    log_common!("[startup] main() start");

    fs::mount::log_gvfs_status();
    log_common!("[startup] gvfs check done");

    let mut cfg = config::AppConfig::load();
    log_common!("[startup] config loaded");

    let state = config::load_state();
    log_common!("[startup] state loaded (window_size = {:?})", state.window_size);
    i18n::set_from_code(&state.lang);

    // 設定ダイアログ（共通/アニメタブ）で編集された値は state 側が config.ini より優先される。
    if let Some(v) = state.app_cache_max_mb { cfg.cache_max_mb = Some(v); }
    if let Some(v) = state.app_file_cache_max_mb { cfg.file_cache_max_mb = Some(v); }
    if let Some(v) = state.app_anim_ring_min_frames { cfg.anim_ring_min_frames = v; }
    if let Some(v) = state.app_anim_ring_max_frames { cfg.anim_ring_max_frames = v; }
    if let Some(v) = state.app_anim_frame_hard_limit_mb { cfg.anim_frame_hard_limit_mb = v; }
    if let Some(v) = state.app_viewer_filter { cfg.viewer_filter = v; }
    if let Some(v) = state.app_max_decode_edge { cfg.max_decode_edge = v; }

    let args = CliArgs::parse();
    if let Some(v) = args.cache_max_mb    { cfg.cache_max_mb      = Some(v.max(64)); }
    if let Some(v) = args.file_cache_max_mb { cfg.file_cache_max_mb = Some(v.max(16)); }

    let start_dir = args.start_path
        .unwrap_or_else(|| cfg.startup_dir(&state));
    log_common!("[startup] start_dir = {:?}", start_dir);

    log_common!("[startup] starting winit event loop ...");
    winit_app::run(start_dir, cfg, state);
}

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

#[cfg(not(target_os = "windows"))]
fn japanese_font_data() -> Option<Vec<u8>> {
    let candidates = [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    ];
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

#[cfg(not(target_os = "windows"))]
fn simplified_chinese_font_data() -> Option<Vec<u8>> {
    None // NotoSansCJK が全 CJK をカバーするため追加フォント不要
}

// ── コマンドライン引数 ────────────────────────────────────────────────────────

struct CliArgs {
    start_path:        Option<PathBuf>,
    cache_max_mb:      Option<u64>,
    file_cache_max_mb: Option<u64>,
}

impl CliArgs {
    /// `--cache-max-mb=N` / `--cache-max-mb N` 形式を手動パース。
    /// 不明なオプションは無視、位置引数は start_path として扱う。
    fn parse() -> Self {
        let mut start_path        = None::<PathBuf>;
        let mut cache_max_mb      = None::<u64>;
        let mut file_cache_max_mb = None::<u64>;

        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            if let Some(rest) = arg.strip_prefix("--cache-max-mb") {
                cache_max_mb = Self::take_value(rest, &mut it);
            } else if let Some(rest) = arg.strip_prefix("--file-cache-max-mb") {
                file_cache_max_mb = Self::take_value(rest, &mut it);
            } else if !arg.starts_with('-') {
                start_path = Some(PathBuf::from(&arg));
            }
            // 未知の --xxx オプションは無視
        }

        Self { start_path, cache_max_mb, file_cache_max_mb }
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
