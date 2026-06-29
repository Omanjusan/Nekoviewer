//! eframe を捨てた後の自前 winit イベントループ（段階1: 土台）。
//!
//! この段階ではエクスプローラー窓 **1 枚だけ** を winit + egui-winit + egui-wgpu で
//! 立ち上げ、既存 `NekoviewApp::ui()` の中身をそのまま描画する。
//! 描画駆動は PoC と同じく毎フレーム（`ControlFlow::Poll`）。
//! render-on-demand 化（`EventLoopProxy` 起床・`ControlFlow::Wait`）は段階2で行う。
//!
//! ビューアー窓（段階3）・ステータス窓（段階5）はまだ生成しない。
//! `view_reader.rs` / `view_status.rs` が使う `ViewportCommand` / `show_viewport_deferred`
//! は egui ネイティブ API なので、eframe を外してもコンパイルは通る（実窓が出ないだけ）。

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::config::{AppConfig, AppState};
use crate::view_explorer::NekoviewApp;

/// エクスプローラー窓 1 枚ぶんの描画コンテキスト。
/// **窓ごとに独立した `egui::Context`** を持つ（ROOT 結合を作らないため）。
struct ExplorerWindow {
    window: Arc<Window>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    viewport_id: ViewportId,
}

struct WinitApp {
    /// resumed まで初期化を遅延させるための起動データ。
    init: Option<(PathBuf, AppConfig, AppState)>,
    painter: Option<Painter>,
    explorer: Option<ExplorerWindow>,
    app: Option<NekoviewApp>,
}

impl WinitApp {
    fn new(start_dir: PathBuf, cfg: AppConfig, state: AppState) -> Self {
        Self {
            init: Some((start_dir, cfg, state)),
            painter: None,
            explorer: None,
            app: None,
        }
    }

    fn create_explorer_window(&mut self, event_loop: &ActiveEventLoop) {
        let (start_dir, cfg, state) = self.init.take().expect("init data");

        let mut attrs = Window::default_attributes().with_title("Nekoview");
        if let Some((w, h)) = state.window_size {
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(w as f64, h as f64));
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));

        let viewport_id = ViewportId::ROOT;
        let painter = self.painter.as_mut().expect("painter must exist");
        pollster::block_on(painter.set_window(viewport_id, Some(window.clone())))
            .expect("set_window");

        let egui_ctx = egui::Context::default();
        crate::setup_egui_context(&egui_ctx);
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );

        // ワーカー起床・テクスチャ登録に使う ctx はエクスプローラー窓の Context を渡す。
        let app = NekoviewApp::new(
            start_dir,
            cfg,
            state.viewer_slots,
            state.sort_state,
            state.viewer_cfg,
            egui_ctx.clone(),
        );

        self.explorer = Some(ExplorerWindow {
            window,
            egui_ctx,
            egui_state,
            viewport_id,
        });
        self.app = Some(app);
    }
}

/// エクスプローラー窓を 1 フレーム描画する。
fn render(painter: &mut Painter, win: &mut ExplorerWindow, app: &mut NekoviewApp) {
    let raw_input = win.egui_state.take_egui_input(&win.window);

    let full_output = win.egui_ctx.run_ui(raw_input, |ctx| {
        // 常時走る処理（旧 eframe::App::logic）。egui パス内で UI より前に呼ぶ。
        app.logic(ctx);
        // 既存 ui() は内部で top/left/central パネルを構築するため、
        // eframe 同様に CentralPanel の Ui を渡す。
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                app.ui(ui);
            });
    });

    win.egui_state
        .handle_platform_output(&win.window, full_output.platform_output);
    let clipped = win
        .egui_ctx
        .tessellate(full_output.shapes, full_output.pixels_per_point);
    painter.paint_and_update_textures(
        win.viewport_id,
        full_output.pixels_per_point,
        [0.0, 0.0, 0.0, 1.0],
        &clipped,
        &full_output.textures_delta,
        Vec::new(),
        &win.window,
    );
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.painter.is_none() {
            let painter = pollster::block_on(Painter::new(
                egui::Context::default(),
                WgpuConfiguration::default(),
                false,
                RendererOptions::default(),
            ));
            self.painter = Some(painter);
            self.create_explorer_window(event_loop);
            crate::log_common!("[startup] explorer window created");
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Self {
            painter,
            explorer,
            app,
            ..
        } = self;
        let (Some(win), Some(app)) = (explorer.as_mut(), app.as_mut()) else {
            return;
        };
        if win.window.id() != window_id {
            return;
        }

        let _ = win.egui_state.on_window_event(&win.window, &event);

        match &event {
            WindowEvent::CloseRequested => {
                app.on_exit();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h), Some(p)) = (
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                    painter.as_mut(),
                ) {
                    p.on_window_resized(win.viewport_id, w, h);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(p) = painter.as_mut() {
                    render(p, win, app);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 段階1は PoC 同様に毎周描画する（render-on-demand 化は段階2）。
        let Self {
            painter,
            explorer,
            app,
            ..
        } = self;
        if let (Some(p), Some(win), Some(app)) =
            (painter.as_mut(), explorer.as_mut(), app.as_mut())
        {
            render(p, win, app);
        }
    }
}

/// winit イベントループを起動する（戻ってきたら終了）。
pub fn run(start_dir: PathBuf, cfg: AppConfig, state: AppState) {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = WinitApp::new(start_dir, cfg, state);
    event_loop.run_app(&mut app).expect("run_app");
}
