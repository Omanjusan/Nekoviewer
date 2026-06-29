//! eframe を捨てた後の自前 winit イベントループ（段階3: ビューアー窓の独立 OS 窓化）。
//!
//! 段階2まではエクスプローラー窓 1 枚を render-on-demand で駆動していた。
//! 段階3では **ビューアーを 2 枚目の本物の OS 窓**として追加する:
//!
//! - 窓は `EguiWindow`（独立した `egui::Context` + `egui_winit::State` + `ViewportInfo`）として
//!   一般化し、エクスプローラー(ROOT)とビューアー(`viewer_window`)の 2 枚を管理する。
//! - ビューアー窓は `NekoviewApp::viewer_is_open()` の状態に応じて動的に生成/破棄する
//!   （`about_to_wait` で `sync_viewer_window`）。
//! - **ViewportCommand の橋渡し**: `view_reader.rs` は従来どおり `ctx.send_viewport_cmd(..)`
//!   （Title/Fullscreen/Maximized/Decorations/Position/Size/Minimized 等）を出す。これを
//!   各窓の render 後に `egui_winit::process_viewport_commands` で対象 winit Window へ適用する
//!   （eframe が内部でやっていた橋渡しと同一）。よって view_reader 側は無改変で動く。
//! - **描画はループ本体（`about_to_wait`）から直接行う**。PoC の学び（非フォーカス窓の
//!   `request_redraw()`→`RedrawRequested` は Windows が間引く）に従い、`request_redraw` 駆動を
//!   やめてループ本体で必要な窓を直接 render する。再描画予定は窓ごとに `next_repaint` で保持。
//! - 再描画要求（egui の repaint コールバック / ワーカー起床）は `EventLoopProxy::send_event`
//!   でループを叩き起こし、`UserEvent::Repaint{viewport_id}` で対象窓の `next_repaint` を更新する。
//!
//! 段階5では **ステータス窓（debug ビルドのみ）を 3 枚目の独立 OS 窓**にする。ビューアー窓と
//! 同じ枠組み（`EguiWindow` + `sync_status_window` で動的生成/破棄、render-on-demand）で扱い、
//! `NekoviewApp::status_is_open()` の状態に追従する。データは `render_status` 内で 1Hz throttle
//! して更新し、`request_repaint_after(1s)` で自分自身を 1Hz で起こし続ける（旧 1Hz ティッカー
//! スレッド＋ROOT 外部ウェイクは不要になり撤去）。release ビルドでは従来どおり ROOT 内の
//! フローティング `egui::Window`（`ui()` 内 `draw_status_window`）のまま。

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

/// ビューアー窓に割り当てる ViewportId（ROOT=エクスプローラーと区別する）。
fn viewer_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("viewer_window")
}

/// ステータス窓（debug ビルドのみ）に割り当てる ViewportId。
fn status_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("status_window")
}

/// 自前ループへ送る独自イベント。今は再描画要求のみ。
#[derive(Debug)]
enum UserEvent {
    /// 指定窓を `when` 時刻までに再描画してほしい（egui の repaint コールバック由来）。
    Repaint { viewport_id: ViewportId, when: Instant },
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

/// 1 枚の窓ぶんの描画コンテキスト。
/// **窓ごとに独立した `egui::Context`** を持つ（ROOT 結合を作らないため）。
struct EguiWindow {
    window: Arc<Window>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    viewport_id: ViewportId,
    /// egui へ渡す窓情報（outer/inner rect・モニタサイズ・最大化等）。
    /// render 前に `update_viewport_info` で更新し、`process_viewport_commands` の出力も受ける。
    info: egui::ViewportInfo,
    /// 初回フレーム（update_viewport_info の is_init 用）。
    first_frame: bool,
    /// 次に再描画すべき最短時刻。`None` = 予定なし（純粋にイベント待ち）。
    next_repaint: Option<Instant>,
}

impl EguiWindow {
    /// `next_repaint` を `t` と比較してより早い方に更新する。
    fn bump(&mut self, t: Instant) {
        self.next_repaint = Some(match self.next_repaint {
            Some(cur) => cur.min(t),
            None => t,
        });
    }

    /// 今すぐ再描画を要求する（入力イベント・リサイズ・OS expose 用）。
    fn bump_now(&mut self) {
        self.next_repaint = Some(Instant::now());
    }

    fn due(&self, now: Instant) -> bool {
        matches!(self.next_repaint, Some(t) if now >= t)
    }
}

/// 1 枚の窓の egui プラミング（Context/State/repaint コールバック）を組み立てて返す。
/// サーフェスも `painter` に登録する。
fn make_egui_window(
    window: Arc<Window>,
    viewport_id: ViewportId,
    painter: &mut Painter,
    proxy: &EventLoopProxy<UserEvent>,
) -> EguiWindow {
    pollster::block_on(painter.set_window(viewport_id, Some(window.clone()))).expect("set_window");

    let egui_ctx = egui::Context::default();
    crate::setup_egui_context(&egui_ctx);

    // 再描画要求 → ループ起床の橋渡し。ワーカースレッドからの request_repaint もここを通る。
    // EventLoopProxy<UserEvent> は Send なので Mutex で Sync 化してコールバック境界
    // （Fn + Send + Sync）を満たす（eframe と同じ手法）。
    // どの窓宛かはこの窓に割り当てた viewport_id をクロージャに焼き込んで区別する
    // （各 Context は内部的に ROOT viewport を使うため info.viewport_id では区別できない）。
    let proxy = Mutex::new(proxy.clone());
    egui_ctx.set_request_repaint_callback(move |info| {
        let when = Instant::now() + info.delay;
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(UserEvent::Repaint { viewport_id, when });
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

    EguiWindow {
        window,
        egui_ctx,
        egui_state,
        viewport_id,
        info: egui::ViewportInfo::default(),
        first_frame: true,
        next_repaint: Some(Instant::now()),
    }
}

/// 1 つの窓を 1 フレーム描画し、egui が望む次回再描画までの猶予を返す。
/// `build` は egui パス内で UI を構築するクロージャ。
/// 描画後、egui が出した `ViewportCommand` 群を `process_viewport_commands` で winit Window へ適用する。
fn render_window(
    painter: &mut Painter,
    win: &mut EguiWindow,
    build: impl FnMut(&mut egui::Ui),
) -> Duration {
    let ctx = win.egui_ctx.clone();

    // outer/inner rect・モニタサイズ等を最新化し、raw_input に載せる（スロット保存等が参照）。
    egui_winit::update_viewport_info(&mut win.info, &ctx, &win.window, win.first_frame);
    win.first_frame = false;

    let mut raw_input = win.egui_state.take_egui_input(&win.window);
    raw_input.viewports.insert(win.viewport_id, win.info.clone());

    let full_output = ctx.run_ui(raw_input, build);

    win.egui_state
        .handle_platform_output(&win.window, full_output.platform_output);
    let clipped = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
    painter.paint_and_update_textures(
        win.viewport_id,
        full_output.pixels_per_point,
        [0.0, 0.0, 0.0, 1.0],
        &clipped,
        &full_output.textures_delta,
        Vec::new(),
        &win.window,
    );

    // egui が出した窓制御コマンド（Title/Fullscreen/Decorations/Position/Size/Minimized 等）を
    // 対象 winit Window へ適用する。これが eframe の肩代わりだった部分。
    if let Some(vp_out) = full_output.viewport_output.get(&win.viewport_id) {
        let mut actions = Vec::new();
        egui_winit::process_viewport_commands(
            &ctx,
            &mut win.info,
            vp_out.commands.clone(),
            &win.window,
            &mut actions,
        );
    }
    // 今回ぶんの info.events（Close 等）は窓制御では使わないので破棄する
    // （ビューアーの閉じ要求は ViewerOutput と OS の CloseRequested で別途処理する）。
    win.info.events.clear();

    full_output
        .viewport_output
        .get(&win.viewport_id)
        .map(|v| v.repaint_delay)
        .unwrap_or(Duration::MAX)
}

struct WinitApp {
    /// resumed まで初期化を遅延させるための起動データ。
    init: Option<(PathBuf, AppConfig, AppState)>,
    /// 再描画要求でループを起床させるためのプロキシ（各窓のコールバックへ clone して渡す）。
    proxy: EventLoopProxy<UserEvent>,
    painter: Option<Painter>,
    explorer: Option<EguiWindow>,
    viewer: Option<EguiWindow>,
    /// ステータス窓（debug ビルドでのみ生成される。release では常に `None`）。
    status: Option<EguiWindow>,
    app: Option<NekoviewApp>,
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
            viewer: None,
            status: None,
            app: None,
        }
    }

    /// 現在存在する全窓の ViewportId 集合。サーフェス回収（`gc_viewports`）で
    /// 「残す窓」を指定するのに使う（破棄対象は呼び出し前に `None` 済みであること）。
    fn active_viewport_set(&self) -> egui::ViewportIdSet {
        let mut set = egui::ViewportIdSet::default();
        set.insert(ViewportId::ROOT);
        if self.viewer.is_some() {
            set.insert(viewer_viewport_id());
        }
        if self.status.is_some() {
            set.insert(status_viewport_id());
        }
        set
    }

    fn create_explorer_window(&mut self, event_loop: &ActiveEventLoop) {
        let (start_dir, cfg, state) = self.init.take().expect("init data");

        let mut attrs = Window::default_attributes().with_title("Nekoview");
        if let Some((w, h)) = state.window_size {
            attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(w as f64, h as f64));
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));

        let painter = self.painter.as_mut().expect("painter must exist");
        let win = make_egui_window(window, ViewportId::ROOT, painter, &self.proxy);

        // ワーカー起床・テクスチャ登録に使う ctx はエクスプローラー窓の Context を渡す。
        let app = NekoviewApp::new(
            start_dir,
            cfg,
            state.viewer_slots,
            state.sort_state,
            state.viewer_cfg,
            win.egui_ctx.clone(),
        );

        self.explorer = Some(win);
        self.app = Some(app);
    }

    /// `NekoviewApp::viewer_is_open()` の状態に合わせてビューアー窓を生成/破棄する。
    fn sync_viewer_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(app) = self.app.as_mut() else { return };
        let want = app.viewer_is_open();
        let have = self.viewer.is_some();

        if want && !have {
            let attrs = Window::default_attributes()
                .with_title("Nekoview")
                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
            let window = Arc::new(event_loop.create_window(attrs).expect("create viewer window"));
            let painter = self.painter.as_mut().expect("painter must exist");
            let win = make_egui_window(window.clone(), viewer_viewport_id(), painter, &self.proxy);
            if app.take_viewer_focus_request() {
                window.focus_window();
            }
            self.viewer = Some(win);
            crate::log_common!("[viewer] window created");
        } else if !want && have {
            // 窓を破棄。Painter のサーフェスは gc_viewports で当該 viewport だけ除去する
            // （set_window(_, None) は全サーフェスを消すので使わない）。残す窓は
            // active_viewport_set（破棄後の現存窓）で指定する。
            self.viewer = None;
            let active = self.active_viewport_set();
            if let Some(p) = self.painter.as_mut() {
                p.gc_viewports(&active);
            }
            crate::log_common!("[viewer] window destroyed");
        } else if want && have {
            // 既存窓のままファイル切替したとき等のフォーカス前面化要求を処理。
            if app.take_viewer_focus_request() {
                if let Some(v) = self.viewer.as_ref() {
                    v.window.focus_window();
                }
            }
        }
    }

    /// `NekoviewApp::status_is_open()`（debug ビルドの [?] トグル）に合わせて
    /// ステータス窓を生成/破棄する。release ビルドでは何もしない（ROOT 内フローティング窓のまま）。
    fn sync_status_window(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(debug_assertions)]
        {
            let Some(app) = self.app.as_mut() else { return };
            let want = app.status_is_open();
            let have = self.status.is_some();

            if want && !have {
                let attrs = Window::default_attributes()
                    .with_title("Nekoview Status")
                    .with_inner_size(winit::dpi::LogicalSize::new(300.0, 280.0));
                let window = Arc::new(event_loop.create_window(attrs).expect("create status window"));
                let painter = self.painter.as_mut().expect("painter must exist");
                let win = make_egui_window(window, status_viewport_id(), painter, &self.proxy);
                self.status = Some(win);
                crate::log_common!("[status] window created");
            } else if !want && have {
                // 窓を破棄。残す窓は active_viewport_set（破棄後の現存窓）で指定する。
                self.status = None;
                let active = self.active_viewport_set();
                if let Some(p) = self.painter.as_mut() {
                    p.gc_viewports(&active);
                }
                crate::log_common!("[status] window destroyed");
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = event_loop;
    }

    /// 期限が来た窓をループ本体から直接 render する。
    fn render_due_windows(&mut self) {
        let now = Instant::now();

        if self.explorer.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(p), Some(win), Some(app)) =
                (self.painter.as_mut(), self.explorer.as_mut(), self.app.as_mut())
            {
                let delay = render_window(p, win, |ui| {
                    // 常時走る処理（旧 eframe::App::logic）。egui パス内で UI より前に呼ぶ。
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            app.ui(ui);
                        });
                });
                win.next_repaint = schedule_after(delay);
            }
        }

        if self.viewer.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(p), Some(win), Some(app)) =
                (self.painter.as_mut(), self.viewer.as_mut(), self.app.as_mut())
            {
                let delay = render_window(p, win, |ui| {
                    app.render_viewer(ui);
                });
                win.next_repaint = schedule_after(delay);
            }
        }

        if self.status.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(p), Some(win), Some(app)) =
                (self.painter.as_mut(), self.status.as_mut(), self.app.as_mut())
            {
                let delay = render_window(p, win, |ui| {
                    app.render_status(ui);
                });
                win.next_repaint = schedule_after(delay);
            }
        }
    }

    /// 全窓の `next_repaint` のうち最短を返す。
    fn earliest_repaint(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        for w in [self.explorer.as_ref(), self.viewer.as_ref(), self.status.as_ref()] {
            if let Some(t) = w.and_then(|w| w.next_repaint) {
                earliest = Some(earliest.map_or(t, |e| e.min(t)));
            }
        }
        earliest
    }

    /// window_id から対象窓への可変参照を引く。
    fn window_mut(&mut self, window_id: WindowId) -> Option<&mut EguiWindow> {
        if self.explorer.as_ref().map_or(false, |w| w.window.id() == window_id) {
            self.explorer.as_mut()
        } else if self.viewer.as_ref().map_or(false, |w| w.window.id() == window_id) {
            self.viewer.as_mut()
        } else if self.status.as_ref().map_or(false, |w| w.window.id() == window_id) {
            self.status.as_mut()
        } else {
            None
        }
    }
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
        // ワーカーや egui からの再描画要求。対象窓の next_repaint を最短に更新するだけで、
        // 実際の描画と ControlFlow 設定は about_to_wait が行う。
        match event {
            UserEvent::Repaint { viewport_id, when } => {
                if self.explorer.as_ref().map_or(false, |w| w.viewport_id == viewport_id) {
                    if let Some(w) = self.explorer.as_mut() {
                        w.bump(when);
                    }
                } else if self.viewer.as_ref().map_or(false, |w| w.viewport_id == viewport_id) {
                    if let Some(w) = self.viewer.as_mut() {
                        w.bump(when);
                    }
                } else if self.status.as_ref().map_or(false, |w| w.viewport_id == viewport_id) {
                    if let Some(w) = self.status.as_mut() {
                        w.bump(when);
                    }
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let is_explorer = self.explorer.as_ref().map_or(false, |w| w.window.id() == window_id);
        let is_viewer = self.viewer.as_ref().map_or(false, |w| w.window.id() == window_id);
        let is_status = self.status.as_ref().map_or(false, |w| w.window.id() == window_id);
        if !is_explorer && !is_viewer && !is_status {
            return;
        }

        // RedrawRequested は OS/winit からの「今描け」指示であり egui への入力ではない。
        // egui-winit に渡すと repaint:true が返り request_redraw と自己ループになるため分離し、
        // 対象窓を「今すぐ描画予定」にしてループ本体（about_to_wait）に描かせる。
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Some(w) = self.window_mut(window_id) {
                w.bump_now();
            }
            return;
        }

        // それ以外（入力・リサイズ・クローズ等）は egui-winit に配送する。
        let response = {
            let w = self.window_mut(window_id).expect("window checked above");
            w.egui_state.on_window_event(&w.window, &event)
        };

        match event {
            WindowEvent::CloseRequested => {
                if is_explorer {
                    // エクスプローラー窓を閉じる＝アプリ終了。
                    if let Some(app) = self.app.as_mut() {
                        app.on_exit();
                    }
                    event_loop.exit();
                    return;
                } else if is_viewer {
                    // ビューアー窓の OS クローズ。ViewerState を破棄し、窓も即破棄する。
                    if let Some(app) = self.app.as_mut() {
                        app.close_viewer();
                    }
                    self.sync_viewer_window(event_loop);
                    return;
                } else {
                    // ステータス窓の OS クローズ。トグルを下ろし、窓を破棄する。
                    if let Some(app) = self.app.as_mut() {
                        app.close_status();
                    }
                    self.sync_status_window(event_loop);
                    return;
                }
            }
            WindowEvent::Resized(size) => {
                let vp = if is_explorer {
                    ViewportId::ROOT
                } else if is_viewer {
                    viewer_viewport_id()
                } else {
                    status_viewport_id()
                };
                if let (Some(w), Some(h), Some(p)) = (
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                    self.painter.as_mut(),
                ) {
                    p.on_window_resized(vp, w, h);
                }
                if let Some(w) = self.window_mut(window_id) {
                    w.bump_now();
                }
            }
            _ => {}
        }

        // 入力で egui が再描画を要したら、その窓を即時描画予定にする（操作レスポンス確保）。
        if response.repaint {
            if let Some(w) = self.window_mut(window_id) {
                w.bump_now();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // エクスプローラーの操作でビューアー/ステータス窓が開いた/閉じた可能性に追従する。
        self.sync_viewer_window(event_loop);
        self.sync_status_window(event_loop);
        // 期限の来た窓を描画する。
        self.render_due_windows();
        // 描画中（ビューアーの ESC/X、ステータスの [?] トグル等）に窓が閉じられた可能性に追従する。
        self.sync_viewer_window(event_loop);
        self.sync_status_window(event_loop);

        // 全窓の最短再描画予定で ControlFlow を決める。
        match self.earliest_repaint() {
            Some(t) if Instant::now() >= t => {
                // まだ期限済みの窓がある（アニメ等で連続描画中）→ すぐ次周回す。
                event_loop.set_control_flow(ControlFlow::Poll);
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
    // 既定は Wait（render-on-demand）。再描画予定は about_to_wait で WaitUntil/Poll に切替。
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = WinitApp::new(start_dir, cfg, state, proxy);
    event_loop.run_app(&mut app).expect("run_app");
}
