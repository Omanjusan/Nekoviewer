#[cfg(not(debug_assertions))]
use std::sync::{Arc, Mutex};

// ── ステータスデータ ──────────────────────────────────────────────────────────

// debug グリッド（frame_dt_ms / scan_state / thumb_pending / pending_loads /
// thumbnails_loaded）は draw_content の #[cfg(debug_assertions)] ブロックでのみ
// 読まれる。release では書き込むだけで参照しないため、release ビルドに限り未読警告を抑制する。
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub struct StatusData {
    pub page_cache_used_bytes: usize,
    pub page_cache_max_bytes: usize,
    pub file_cache_used_bytes: usize,
    pub file_cache_max_bytes: usize,
    pub thumb_pending: usize,
    pub pending_loads: usize,
    pub thumbnails_loaded: usize,
    pub frame_dt_ms: f32,
    pub scan_state: &'static str,
}

impl Default for StatusData {
    fn default() -> Self {
        Self {
            page_cache_used_bytes: 0,
            page_cache_max_bytes: 0,
            file_cache_used_bytes: 0,
            file_cache_max_bytes: 0,
            thumb_pending: 0,
            pending_loads: 0,
            thumbnails_loaded: 0,
            frame_dt_ms: 0.0,
            scan_state: "idle",
        }
    }
}

// ── 描画 ─────────────────────────────────────────────────────────────────────

pub(crate) fn draw_content(ui: &mut egui::Ui, data: &StatusData) {
    egui::Grid::new("status_grid")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            let used_mb = data.page_cache_used_bytes / (1024 * 1024);
            let max_mb  = data.page_cache_max_bytes  / (1024 * 1024);
            ui.label("Page cache:");
            ui.label(format!("{} / {} MB", used_mb, max_mb));
            ui.end_row();

            let file_mb     = data.file_cache_used_bytes / (1024 * 1024);
            let file_max_mb = data.file_cache_max_bytes  / (1024 * 1024);
            ui.label("File cache:");
            ui.label(format!("{} / {} MB", file_mb, file_max_mb));
            ui.end_row();
        });

    #[cfg(debug_assertions)]
    {
        ui.separator();
        egui::Grid::new("status_debug_grid")
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                ui.label("Frame:");
                ui.label(format!("{:.1} ms", data.frame_dt_ms));
                ui.end_row();

                ui.label("Scan:");
                ui.label(data.scan_state);
                ui.end_row();

                ui.label("Thumb pending:");
                ui.label(data.thumb_pending.to_string());
                ui.end_row();

                ui.label("Load pending:");
                ui.label(data.pending_loads.to_string());
                ui.end_row();

                ui.label("Thumbnails:");
                ui.label(data.thumbnails_loaded.to_string());
                ui.end_row();
            });
    }

    // ログは debug/release 問わず表示する（Windowsでコンソール無し起動時の
    // 唯一のログ確認手段になるため）。
    ui.separator();
    crate::view_innerlog::draw(ui);
}

// ── ウィンドウ表示 ─────────────────────────────────────────────────────────────

/// release ビルド: メインウィンドウ内フローティング `egui::Window` としてステータスを表示する。
///
/// debug ビルドではステータスは独立 OS 窓（段階5）になり、`NekoviewApp::render_status` が
/// `draw_content` を直接描画するため、この関数は使わない。
#[cfg(not(debug_assertions))]
pub fn show(ctx: &egui::Context, open: &mut bool, data: &Arc<Mutex<StatusData>>) {
    if !*open {
        return;
    }
    let data_guard = data.lock().unwrap();
    egui::Window::new("Status")
        .open(open)
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            draw_content(ui, &data_guard);
        });
}
