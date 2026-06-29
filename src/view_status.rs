use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// ── リングバッファログ ────────────────────────────────────────────────────────

pub struct StatusLog {
    entries: VecDeque<String>,
    capacity: usize,
}

impl StatusLog {
    pub fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::with_capacity(capacity), capacity }
    }

    pub fn push(&mut self, msg: String) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(msg);
    }

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

fn draw_content(ui: &mut egui::Ui, data: &StatusData) {
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

/// debug ビルド: 完全分離型の OS ウィンドウ（deferred viewport）
/// release ビルド: メインウィンドウ内フローティング egui::Window
pub fn show(
    ctx: &egui::Context,
    open: &mut bool,
    data: &Arc<Mutex<StatusData>>,
    closing: &Arc<Mutex<bool>>,
) {
    #[cfg(debug_assertions)]
    {
        if std::mem::take(&mut *closing.lock().unwrap()) {
            *open = false;
        }
        if !*open {
            return;
        }
        let data_arc    = Arc::clone(data);
        let closing_arc = Arc::clone(closing);
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("status_window"),
            egui::ViewportBuilder::default()
                .with_title("Nekoview Status")
                .with_inner_size([300.0, 280.0])
                .with_resizable(true),
            move |vp_ctx, _class| {
                eprintln!("[probe3] status viewport closure PAINTED");
                egui::CentralPanel::default().show(vp_ctx, |ui| {
                    let d = data_arc.lock().unwrap();
                    draw_content(ui, &d);
                });
                // root がスロットルされていても1秒ごとに自己 repaint しつつ
                // root も叩き起こしてデータ更新を促す
                vp_ctx.request_repaint_after(std::time::Duration::from_secs(1));
                vp_ctx.request_repaint_of(egui::ViewportId::ROOT);
                if vp_ctx.input(|i| i.viewport().close_requested()) {
                    *closing_arc.lock().unwrap() = true;
                    vp_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    vp_ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }

    #[cfg(not(debug_assertions))]
    {
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
        let _ = closing; // 未使用警告抑制
    }
}
