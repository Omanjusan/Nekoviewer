mod anim;
mod app;
mod cache;
mod config;
mod fs;
mod i18n;
mod neko_dir;
mod spread_offset;
mod viewer;

use app::NekoviewApp;
use std::path::PathBuf;

fn main() -> eframe::Result {
    // config 読み込み前なのでデフォルト値（common=true）でログ出力
    log_common!("[startup] main() start");

    fs::mount::log_gvfs_status();
    log_common!("[startup] gvfs check done");

    let cfg = config::AppConfig::load();
    log_common!("[startup] config loaded");

    let state = config::load_state();
    log_common!("[startup] state loaded (window_size = {:?})", state.window_size);
    i18n::set_from_code(&state.lang);

    let start_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.startup_dir(&state));
    log_common!("[startup] start_dir = {:?}", start_dir);

    let mut options = eframe::NativeOptions::default();
    if let Some((w, h)) = state.window_size {
        options.viewport = options.viewport.with_inner_size([w as f32, h as f32]);
    }
    log_common!("[startup] calling eframe::run_native ...");

    eframe::run_native(
        "Nekoview",
        options,
        Box::new(|cc| {
            log_common!("[startup] eframe context ready, setting up font ...");
            setup_japanese_font(&cc.egui_ctx);
            cc.egui_ctx.style_mut(|s| {
                s.spacing.scroll.bar_outer_margin = 0.0;
            });
            log_common!("[startup] font done, creating app ...");
            Ok(Box::new(NekoviewApp::new(start_dir, cfg, state.viewer_slots, state.sort_state)))
        }),
    )
}

fn setup_japanese_font(ctx: &egui::Context) {
    let font_data = japanese_font_data();
    let Some(font_data) = font_data else { return };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "NotoSansCJK".to_owned(),
        egui::FontData::from_owned(font_data).into(),
    );
    // デフォルトフォントの後ろに追加（ASCII はデフォルト優先、日本語はこちらにフォールバック）
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("NotoSansCJK".to_owned());

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
