use std::collections::VecDeque;
#[cfg(not(debug_assertions))]
use std::sync::{Arc, Mutex};

// ── リングバッファログ ────────────────────────────────────────────────────────

pub struct StatusLog {
    entries: VecDeque<String>,
    // push が未配線のため capacity は現状参照されない（ログパネルは表示のみ）。
    // 将来ログ投入を配線したら push 経由で参照される。
    #[allow(dead_code)]
    capacity: usize,
}

impl StatusLog {
    pub fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::with_capacity(capacity), capacity }
    }

    /// ログ投入 API。現状は呼び出し元未配線（debug ステータス窓のログ機能は未完成）。
    #[allow(dead_code)]
    pub fn push(&mut self, msg: String) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(msg);
    }

    // 参照元は draw_log（debug 専用）のみ。release ではログパネルを出さないため不要。
    #[cfg(debug_assertions)]
    pub fn entries(&self) -> &VecDeque<String> {
        &self.entries
    }
}

impl Default for StatusLog {
    fn default() -> Self {
        Self::new(64)
    }
}

// ── ステータスデータ ──────────────────────────────────────────────────────────

// debug グリッド（frame_dt_ms / scan_state / thumb_pending / pending_loads /
// thumbnails_loaded / log）は draw_content の #[cfg(debug_assertions)] ブロックでのみ
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
    pub log: StatusLog,
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
            log: StatusLog::default(),
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

        ui.separator();
        draw_log(ui, &data.log);
    }
}

#[cfg(debug_assertions)]
fn draw_log(ui: &mut egui::Ui, log: &StatusLog) {
    ui.label("Log:");
    let text_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let available_h = (ui.available_height()).max(60.0).min(120.0);
    egui::ScrollArea::vertical()
        .id_salt("status_log_scroll")
        .max_height(available_h)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if log.entries().is_empty() {
                ui.label(
                    egui::RichText::new("(no entries)")
                        .monospace()
                        .color(egui::Color32::DARK_GRAY),
                );
            } else {
                for entry in log.entries() {
                    ui.label(egui::RichText::new(entry).monospace().size(text_height));
                }
            }
        });
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
