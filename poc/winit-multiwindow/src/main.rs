//! eframe を捨て、自前 winit ループで egui マルチウィンドウを実現できるかの PoC。
//!
//! 確認したい失敗シナリオ（eframe で 3 日詰まった点）:
//!   - 窓Bにフォーカスが移ると ROOT が背面アイドル化し、モデル更新もイベント処理も止まる
//!
//! この PoC で証明したいこと:
//!   (1) どの窓にフォーカスがあっても、ループ本体の `model.counter += 1` が回り続け、
//!       両方の窓の表示が 1 フレームごとに更新される（フォーカス非依存のモデル更新）。
//!   (2) Viewer 窓にフォーカス中に Left/Right を押すと、その入力が確実に `model` に届く
//!       （winit は WindowEvent に window_id が付くので「どの窓のキーか」が最初から判る）。

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// アプリ全体で 1 つだけ持つ共有モデル（フレームワーク非依存。本番では model.rs 相当）。
#[derive(Default)]
struct Model {
    counter: u64,
    viewer_page: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Explorer,
    Viewer,
}

/// 1 ウィンドウぶんの描画コンテキスト。**窓ごとに独立した egui::Context** を持つ点が肝。
/// （eframe は全 viewport で単一 Context を共有し ROOT に結合させるが、ここでは分離する）
struct WinCtx {
    window: Arc<Window>,
    egui_ctx: egui::Context,
    state: egui_winit::State,
    viewport_id: ViewportId,
    role: Role,
    /// この窓が実際に描画した回数（about_to_wait の集計ログで使う）
    paint_count: u64,
    focused: bool,
}

struct App {
    painter: Option<Painter>,
    windows: HashMap<WindowId, WinCtx>,
    model: Model,
    last_log: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            painter: None,
            windows: HashMap::new(),
            model: Model::default(),
            last_log: Instant::now(),
        }
    }

    fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        title: &str,
        role: Role,
        vp_key: &str,
        pos: (i32, i32),
    ) {
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(420.0, 300.0))
            .with_position(winit::dpi::LogicalPosition::new(pos.0, pos.1));
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));

        let viewport_id = ViewportId::from_hash_of(vp_key);
        let painter = self.painter.as_mut().expect("painter must exist");
        pollster::block_on(painter.set_window(viewport_id, Some(window.clone())))
            .expect("set_window");

        let egui_ctx = egui::Context::default();
        let state = egui_winit::State::new(
            egui_ctx.clone(),
            viewport_id,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );

        self.windows.insert(
            window.id(),
            WinCtx {
                window,
                egui_ctx,
                state,
                viewport_id,
                role,
                paint_count: 0,
                focused: false,
            },
        );
    }
}

/// 1 つの窓を描画する。`self` を分割借用して painter / win / model を同時に触れるよう、
/// 呼び出し側で各フィールドを切り出してから渡す（borrow 競合回避）。
fn render(painter: &mut Painter, win: &mut WinCtx, model: &Model) {
    win.paint_count += 1;
    let raw_input = win.state.take_egui_input(&win.window);
    let title = match win.role {
        Role::Explorer => "EXPLORER 窓",
        Role::Viewer => "VIEWER 窓",
    };
    let role = win.role;
    let counter = model.counter;
    let page = model.viewer_page;

    let full_output = win.egui_ctx.run_ui(raw_input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(title);
            ui.separator();
            ui.label(format!("共有 model.counter = {counter}"));
            ui.label(format!("viewer_page = {page}"));
            ui.add_space(8.0);
            match role {
                Role::Explorer => {
                    ui.label("この窓は何も操作しなくても、");
                    ui.label("counter が回り続ければ「フォーカス非依存更新」成功。");
                }
                Role::Viewer => {
                    ui.label("この窓にフォーカスして Left / Right を押すと");
                    ui.label("viewer_page が ±1 されれば「窓ごとのキー配送」成功。");
                }
            }
        });
    });

    win.state
        .handle_platform_output(&win.window, full_output.platform_output);
    let clipped = win
        .egui_ctx
        .tessellate(full_output.shapes, full_output.pixels_per_point);
    painter.paint_and_update_textures(
        win.viewport_id,
        full_output.pixels_per_point,
        [0.08, 0.08, 0.10, 1.0],
        &clipped,
        &full_output.textures_delta,
        Vec::new(),
        &win.window,
    );
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.painter.is_none() {
            let painter = pollster::block_on(Painter::new(
                egui::Context::default(),
                WgpuConfiguration::default(),
                false,
                RendererOptions::default(),
            ));
            self.painter = Some(painter);

            self.create_window(event_loop, "EXPLORER", Role::Explorer, "win_explorer", (60, 80));
            self.create_window(event_loop, "VIEWER", Role::Viewer, "win_viewer", (520, 80));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // self を分割借用（painter / windows / model はすべて別フィールド）
        let Self {
            painter,
            windows,
            model,
            ..
        } = self;

        let Some(win) = windows.get_mut(&window_id) else {
            return;
        };

        let _ = win.state.on_window_event(&win.window, &event);

        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Focused(f) => {
                win.focused = *f;
                eprintln!("[focus] {:?} focused={}", win.role, f);
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
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state == ElementState::Pressed && win.role == Role::Viewer {
                    match &key.logical_key {
                        Key::Named(NamedKey::ArrowLeft) => {
                            model.viewer_page -= 1;
                            eprintln!(
                                "[viewer key] Left -> viewer_page = {}",
                                model.viewer_page
                            );
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            model.viewer_page += 1;
                            eprintln!(
                                "[viewer key] Right -> viewer_page = {}",
                                model.viewer_page
                            );
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // 描画は about_to_wait（ループ本体）で全窓まとめて行うため、ここでは何もしない。
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // ループ本体。フォーカスに関係なく毎周回す（= eframe の ROOT 結合が無い）。
        self.model.counter += 1;

        // 非フォーカス窓は RedrawRequested が OS に間引かれるので、request_redraw 頼みを
        // やめ、ループ本体から全窓を直接描画する（フォーカス非依存の描画駆動）。
        let Self {
            painter,
            windows,
            model,
            ..
        } = self;
        if let Some(p) = painter.as_mut() {
            for win in windows.values_mut() {
                render(p, win, model);
            }
        }

        // 1 秒ごとに集計ログ。
        //  - loop_rate     : このループ本体が 1 秒に何周したか（フォーカス非依存で回るべき）
        //  - EXPLORER/VIEWER の painted : 各窓がこの 1 秒で実際に描画した回数
        // ループは回っているのに EXPLORER の painted が 0 なら「ループ非依存だが
        // 背面窓の RedrawRequested が OS に間引かれている」= winit/OS 層の描画問題。
        // ループ自体が止まるなら別問題。
        if self.last_log.elapsed().as_secs_f64() >= 1.0 {
            let mut parts = vec![format!("counter={}", self.model.counter)];
            for win in self.windows.values_mut() {
                parts.push(format!(
                    "{:?}{{painted/s={}, focused={}}}",
                    win.role, win.paint_count, win.focused
                ));
                win.paint_count = 0;
            }
            eprintln!("[1s] {}", parts.join("  "));
            self.last_log = Instant::now();
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("run_app");
}
