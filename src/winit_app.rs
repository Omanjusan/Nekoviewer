//! eframe を捨てた後の自前 winit イベントループ（段階2: ワーカー起床の付け替え）。
//!
//! 段階1ではエクスプローラー窓 1 枚を `ControlFlow::Poll` で毎周描画していた。
//! 段階2では **render-on-demand** へ移行する:
//!
//! - egui の `Context::set_request_repaint_callback` で再描画要求を捕まえ、
//!   `EventLoopProxy::send_event` でループを叩き起こす（ワーカースレッドからの
//!   `ctx.request_repaint()` もこの経路でループに届く＝eframe 内部と同じ橋渡し）。
//!   これにより `cache.rs` などに散在する `request_repaint` 呼び出しは無改変で機能する。
//! - `ControlFlow` は基本 `Wait`、再描画予定があれば `WaitUntil(最短時刻)`。
//!   毎周描画をやめるのでアイドル時 CPU が下がる。
//! - 再描画予定 `next_repaint` は「egui が返す `repaint_delay`（アニメ等）」と
//!   「コールバック経由の起床（ワーカー結果）」の両方を最短で集約する。
//!
//! ビューアー窓（段階3）・ステータス窓（段階5）はまだ生成しない。
//! `view_reader.rs` / `view_status.rs` が使う `ViewportCommand` / `show_viewport_deferred`
//! は egui ネイティブ API なので、eframe を外してもコンパイルは通る（実窓が出ないだけ）。

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::ViewportId;
use egui_wgpu::winit::Painter;
use egui_wgpu::{RendererOptions, WgpuConfiguration};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::config::{AppConfig, AppState};
use crate::view_explorer::NekoviewApp;

/// 自前ループへ送る独自イベント。今は再描画要求のみ。
#[derive(Debug)]
enum UserEvent {
    /// 指定窓を `when` 時刻までに再描画してほしい（egui の repaint コールバック由来）。
    Repaint {
        #[allow(dead_code)] // 段階3以降、窓ごとに分岐するまでは未使用
        viewport_id: ViewportId,
        when: Instant,
    },
}

/// `repaint_delay`（egui が返す「次に描くまでの猶予」）を絶対時刻へ変換する。
/// `Duration::MAX`（≒「予定なし、イベント待ち」）や非現実的に長い遅延は `None`。
fn schedule_after(delay: Duration) -> Option<Instant> {
    // 1 日以上先は「予定なし」と同等に扱い、Instant 加算のオーバーフローも避ける。
    if delay >= Duration::from_secs(86_400) {
        None
    } else {
        Instant::now().checked_add(delay)
    }
}

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
    /// 再描画要求でループを起床させるためのプロキシ（コールバックへ clone して渡す）。
    proxy: EventLoopProxy<UserEvent>,
    painter: Option<Painter>,
    explorer: Option<ExplorerWindow>,
    app: Option<NekoviewApp>,
    /// 次に再描画すべき最短時刻。`None` = 予定なし（純粋にイベント待ち）。
    next_repaint: Option<Instant>,
}

impl WinitApp {
    fn new(
        start_dir: PathBuf,
        cfg: AppConfig,
        state: AppState,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            init: Some((start_dir, cfg, state)),
            proxy,
            painter: None,
            explorer: None,
            app: None,
            next_repaint: None,
        }
    }

    /// `next_repaint` を `t` と比較してより早い方に更新する。
    fn bump_repaint(&mut self, t: Instant) {
        self.next_repaint = Some(match self.next_repaint {
            Some(cur) => cur.min(t),
            None => t,
        });
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

        // 再描画要求 → ループ起床の橋渡し。ワーカースレッドからの request_repaint も
        // ここを通る。EventLoopProxy<UserEvent> は Send なので Mutex で Sync 化して
        // コールバック境界（Fn + Send + Sync）を満たす（eframe と同じ手法）。
        let proxy = Mutex::new(self.proxy.clone());
        egui_ctx.set_request_repaint_callback(move |info| {
            let when = Instant::now() + info.delay;
            if let Ok(proxy) = proxy.lock() {
                let _ = proxy.send_event(UserEvent::Repaint {
                    viewport_id: info.viewport_id,
                    when,
                });
            }
        });

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
        // 初回描画を要求。
        self.bump_repaint(Instant::now());
    }
}

/// エクスプローラー窓を 1 フレーム描画し、egui が望む次回再描画までの猶予を返す。
fn render(painter: &mut Painter, win: &mut ExplorerWindow, app: &mut NekoviewApp) -> Duration {
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

    // egui がこのパスで算出した「次に描くまでの猶予」。アニメ等の自走再描画はここに乗る。
    full_output
        .viewport_output
        .get(&win.viewport_id)
        .map(|v| v.repaint_delay)
        .unwrap_or(Duration::MAX)
}

impl ApplicationHandler<UserEvent> for WinitApp {
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        // ワーカーや egui からの再描画要求。最短時刻に集約するだけで、
        // 実際の ControlFlow 設定は about_to_wait が行う。
        match event {
            UserEvent::Repaint { when, .. } => self.bump_repaint(when),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match self.explorer.as_ref() {
            Some(win) if win.window.id() == window_id => {}
            _ => return,
        }

        // RedrawRequested は「今描け」という OS/winit からの指示であり egui への入力ではない。
        // これを egui-winit の on_window_event に渡すと repaint:true が返り、その後の
        // request_redraw と相互に呼び合って vsync 全力描画の自己ループに陥る。
        // よってここで分離し、描画して次回予定は egui の repaint_delay からのみ決める。
        if matches!(event, WindowEvent::RedrawRequested) {
            let repaint_delay = if let (Some(p), Some(win), Some(app)) =
                (self.painter.as_mut(), self.explorer.as_mut(), self.app.as_mut())
            {
                render(p, win, app)
            } else {
                Duration::MAX
            };
            // 描画したのでこのフレームの予定は消費済み。egui の次回猶予で上書きする。
            self.next_repaint = schedule_after(repaint_delay);
            return;
        }

        // それ以外（入力・リサイズ・クローズ等）は egui-winit に配送する。
        let response = {
            let win = self.explorer.as_mut().expect("explorer checked above");
            win.egui_state.on_window_event(&win.window, &event)
        };

        match event {
            WindowEvent::CloseRequested => {
                if let Some(app) = self.app.as_mut() {
                    app.on_exit();
                }
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                let vp = self.explorer.as_ref().map(|w| w.viewport_id);
                if let (Some(w), Some(h), Some(p), Some(vp)) = (
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                    self.painter.as_mut(),
                    vp,
                ) {
                    p.on_window_resized(vp, w, h);
                }
            }
            _ => {}
        }

        // 入力で egui が再描画を要したら即時に要求する（操作レスポンス確保）。
        if response.repaint {
            self.bump_repaint(Instant::now());
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.next_repaint {
            Some(t) if Instant::now() >= t => {
                // 期限到来：再描画を要求し、待機へ戻す。
                // next_repaint は直後の RedrawRequested 内で egui の猶予から再設定される。
                if let Some(win) = self.explorer.as_ref() {
                    win.window.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// winit イベントループを起動する（戻ってきたら終了）。
pub fn run(start_dir: PathBuf, cfg: AppConfig, state: AppState) {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("event loop");
    // 既定は Wait（render-on-demand）。再描画予定は about_to_wait で WaitUntil に切替。
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = WinitApp::new(start_dir, cfg, state, proxy);
    event_loop.run_app(&mut app).expect("run_app");
}
