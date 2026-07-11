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

use crate::config::AppConfig;
use crate::gui_config::AppState;
use crate::view_explorer::NekoviewApp;

/// ビューアー窓に割り当てる ViewportId（ROOT=エクスプローラーと区別する）。
fn viewer_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("viewer_window")
}

/// ステータス窓（debug ビルドのみ）に割り当てる ViewportId。
fn status_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("status_window")
}

/// OCR/翻訳子ウィンドウに割り当てる ViewportId。
fn translate_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("translate_window")
}

/// 自前ループへ送る独自イベント。今は再描画要求のみ。
#[derive(Debug)]
enum UserEvent {
    /// 指定窓を `when` 時刻までに再描画してほしい（egui の repaint コールバック由来）。
    Repaint { viewport_id: ViewportId, when: Instant },
    /// 多重起動された後発プロセスから ping を受けた。エクスプローラー窓へフォーカスを試みる
    /// （Waylandではcompositorのフォーカス盗み防止により効かないことがある。ベストエフォート）。
    FocusRequested,
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
/// さらに **窓ごとに独立した `Painter`（= 独立した wgpu Device/Renderer）** を持つ。
/// egui_wgpu の `Renderer` はテクスチャを `TextureId` キーの単一マップで保持するが、
/// `TextureId::Managed` の採番は `Context` ごとに 0 から独立する。よって複数 Context で
/// 1 個の Renderer を共有するとフォントアトラス（Managed(0)）等が窓間で衝突し描画が壊れる。
/// Painter を窓ごとに分けることで TextureId 名前空間を窓ごとに隔離する。
struct EguiWindow {
    window: Arc<Window>,
    /// この窓専用の描画器（自前 wgpu Device/Queue/Renderer + サーフェス1枚を内包）。
    painter: Painter,
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
    /// フレームキャップの下限時刻（前回 render 開始＋最小フレーム間隔）。
    /// どの経路の再描画要求もこれより前には描かない。Wayland では vsync を外す
    /// （occluded サーフェスへの frame callback 停止で acquire が無期限ブロックするため）ので、
    /// vsync に代わるフレームペーサーとして全プラットフォームで一律に効かせる。
    not_before: Instant,
}

/// モニタ Hz が取得できないときの上限フォールバック。
const FALLBACK_FRAME_CAP_MILLIHERTZ: u32 = 120_000;

impl EguiWindow {
    /// `next_repaint` を `t` と比較してより早い方に更新する（フレームキャップ下限つき）。
    fn bump(&mut self, t: Instant) {
        let t = t.max(self.not_before);
        self.next_repaint = Some(match self.next_repaint {
            Some(cur) => cur.min(t),
            None => t,
        });
    }

    /// 今すぐ再描画を要求する（入力イベント・リサイズ・OS expose 用）。
    fn bump_now(&mut self) {
        self.bump(Instant::now());
    }

    fn due(&self, now: Instant) -> bool {
        matches!(self.next_repaint, Some(t) if now >= t)
    }

    /// この窓の最小フレーム間隔。窓が今いるモニタのリフレッシュレートを上限とし、
    /// 取得できなければ 120Hz 相当にフォールバックする。
    fn frame_cap_interval(&self) -> Duration {
        let mhz = self
            .window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .filter(|&mhz| mhz > 0)
            .unwrap_or(FALLBACK_FRAME_CAP_MILLIHERTZ);
        Duration::from_nanos(1_000_000_000_000 / mhz as u64)
    }
}

/// 1 枚の窓の egui プラミング（Context/State/repaint コールバック）を組み立てて返す。
/// **この窓専用の `Painter`（独立 wgpu Device/Renderer）を生成**し、サーフェスを登録する。
fn make_egui_window(
    window: Arc<Window>,
    viewport_id: ViewportId,
    proxy: &EventLoopProxy<UserEvent>,
) -> EguiWindow {
    let egui_ctx = egui::Context::default();
    crate::setup_egui_context(&egui_ctx);

    // 窓ごとに独立した Painter を作る。Painter::new で wgpu Instance を用意し、
    // set_window で初回サーフェス登録時に専用の Device/Queue/Renderer を生成する。
    // 内部の error-repaint 用 Context にはこの窓自身の egui_ctx を渡す。
    //
    // present mode: 既定は vsync（AutoVsync）だが、Wayland セッションでは外す。
    // Wayland コンポジタは完全に隠れた（occluded）サーフェスに frame callback を送らず、
    // vsync 付き acquire（get_current_texture）が無期限ブロックする。単一スレッドで全窓を
    // render する本ループでは、擬似フルスクのビューアーに覆われたエクスプローラー窓の
    // render がループごと止め、ビューアーの画面更新が停止していた（フォーカス切替で復帰）。
    // vsync を外すぶんのフレームペーシングは EguiWindow::frame_cap_interval が担う。
    // NEKO_NO_VSYNC=1 は動作検証用の強制スイッチ。
    let no_vsync =
        crate::fs::dir::is_wayland_session() || std::env::var_os("NEKO_NO_VSYNC").is_some();
    let wgpu_config = if no_vsync {
        crate::log_common!("[render] present_mode = AutoNoVsync (Wayland or NEKO_NO_VSYNC)");
        WgpuConfiguration {
            surface: egui_wgpu::SurfaceConfig {
                present_mode: egui_wgpu::wgpu::PresentMode::AutoNoVsync,
                desired_maximum_frame_latency: None,
            },
            ..Default::default()
        }
    } else {
        WgpuConfiguration::default()
    };
    let mut painter = pollster::block_on(Painter::new(
        egui_ctx.clone(),
        wgpu_config,
        false,
        RendererOptions::default(),
    ));
    pollster::block_on(painter.set_window(viewport_id, Some(window.clone()))).expect("set_window");

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
        painter,
        egui_ctx,
        egui_state,
        viewport_id,
        info: egui::ViewportInfo::default(),
        first_frame: true,
        next_repaint: Some(Instant::now()),
        not_before: Instant::now(),
    }
}

/// 1 つの窓を 1 フレーム描画し、egui が望む次回再描画までの猶予を返す。
/// `build` は egui パス内で UI を構築するクロージャ。
/// 描画後、egui が出した `ViewportCommand` 群を `process_viewport_commands` で winit Window へ適用する。
fn render_window(win: &mut EguiWindow, build: impl FnMut(&mut egui::Ui)) -> Duration {
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
    win.painter.paint_and_update_textures(
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
    explorer: Option<EguiWindow>,
    viewer: Option<EguiWindow>,
    /// ステータス窓（debug ビルドでのみ生成される。release では常に `None`）。
    status: Option<EguiWindow>,
    /// OCR/翻訳子ウィンドウ（1P担当、独立OS窓）。
    translate: Option<EguiWindow>,
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
            explorer: None,
            viewer: None,
            status: None,
            translate: None,
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

        let win = make_egui_window(window, ViewportId::ROOT, &self.proxy);

        // ワーカー起床・テクスチャ登録に使う ctx はエクスプローラー窓の Context を渡す。
        let app = NekoviewApp::new(
            start_dir,
            cfg,
            state.viewer_slots,
            state.sort_state,
            state.viewer_cfg,
            state.show_hidden,
            state.translate_cfg,
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
            // conf 既定スロットが解決できれば、その位置・サイズで生成して初回フラッシュを避ける。
            // 画面外補正は ViewerState 初回フレームの apply_default_slot が担う。
            let mut attrs = Window::default_attributes().with_title("Nekoview");
            if let Some(slot) = app.resolved_default_viewer_slot() {
                attrs = attrs
                    .with_position(winit::dpi::LogicalPosition::new(slot.x as f64, slot.y as f64))
                    .with_inner_size(winit::dpi::LogicalSize::new(slot.w as f64, slot.h as f64));
            } else {
                attrs = attrs.with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
            }
            let window = Arc::new(event_loop.create_window(attrs).expect("create viewer window"));
            let win = make_egui_window(window.clone(), viewer_viewport_id(), &self.proxy);
            if app.take_viewer_focus_request() {
                window.focus_window();
            }
            self.viewer = Some(win);
            crate::log_common!("[viewer] window created");
        } else if !want && have {
            // 窓を破棄。EguiWindow を drop すると専用 Painter（サーフェス・Device・Renderer）も
            // 一緒に解放される（共有 Painter 時代の gc_viewports は不要）。
            self.viewer = None;
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
                let win = make_egui_window(window, status_viewport_id(), &self.proxy);
                self.status = Some(win);
                crate::log_common!("[status] window created");
            } else if !want && have {
                // 窓を破棄。EguiWindow の drop で専用 Painter ごと解放される。
                self.status = None;
                crate::log_common!("[status] window destroyed");
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = event_loop;
    }

    /// `NekoviewApp::translate_window_is_open()` に合わせて OCR/翻訳子ウィンドウを
    /// 生成/破棄する（status_window と同じ枠組み。ただし debug 限定ではない）。
    fn sync_translate_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(app) = self.app.as_mut() else { return };
        let want = app.translate_window_is_open();
        let have = self.translate.is_some();

        if want && !have {
            let attrs = Window::default_attributes()
                .with_title("Nekoview OCR/Translate")
                .with_inner_size(winit::dpi::LogicalSize::new(480.0, 640.0));
            let window = Arc::new(event_loop.create_window(attrs).expect("create translate window"));
            let win = make_egui_window(window, translate_viewport_id(), &self.proxy);
            self.translate = Some(win);
            crate::log_common!("[translate] window created");
        } else if !want && have {
            self.translate = None;
            crate::log_common!("[translate] window destroyed");
        }
    }

    /// 期限が来た窓をループ本体から直接 render する。
    /// render 後は「render 開始＋最小フレーム間隔」を `not_before` に記録し、
    /// 次回予定をそれ以降にクランプする（vsync 非依存のフレームキャップ）。
    fn render_due_windows(&mut self) {
        let now = Instant::now();

        fn finish_frame(win: &mut EguiWindow, started: Instant, delay: Duration) {
            win.not_before = started + win.frame_cap_interval();
            win.next_repaint = schedule_after(delay).map(|t| t.max(win.not_before));
        }

        if self.explorer.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(win), Some(app)) = (self.explorer.as_mut(), self.app.as_mut()) {
                let started = Instant::now();
                let delay = render_window(win, |ui| {
                    // 常時走る処理（旧 eframe::App::logic）。egui パス内で UI より前に呼ぶ。
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            app.ui(ui);
                        });
                });
                finish_frame(win, started, delay);
            }
        }

        if self.viewer.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(win), Some(app)) = (self.viewer.as_mut(), self.app.as_mut()) {
                let started = Instant::now();
                let delay = render_window(win, |ui| {
                    app.render_viewer(ui);
                });
                finish_frame(win, started, delay);
            }
        }

        if self.status.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(win), Some(app)) = (self.status.as_mut(), self.app.as_mut()) {
                let started = Instant::now();
                let delay = render_window(win, |ui| {
                    app.render_status(ui);
                });
                finish_frame(win, started, delay);
            }
        }

        if self.translate.as_ref().map_or(false, |w| w.due(now)) {
            if let (Some(win), Some(app)) = (self.translate.as_mut(), self.app.as_mut()) {
                let started = Instant::now();
                let delay = render_window(win, |ui| {
                    app.render_translate_window(ui);
                });
                finish_frame(win, started, delay);
            }
        }
    }

    /// 全窓の `next_repaint` のうち最短を返す。
    fn earliest_repaint(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        for w in [self.explorer.as_ref(), self.viewer.as_ref(), self.status.as_ref(), self.translate.as_ref()] {
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
        } else if self.translate.as_ref().map_or(false, |w| w.window.id() == window_id) {
            self.translate.as_mut()
        } else {
            None
        }
    }
}

impl ApplicationHandler<UserEvent> for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // 各窓は自前の Painter を持つ（make_egui_window 内で生成）。ここでは
        // エクスプローラー窓をまだ作っていなければ作る。
        if self.explorer.is_none() {
            self.create_explorer_window(event_loop);
            crate::log_common!("[startup] explorer window created");
        }
    }

    /// イベントループが終了する直前（プラットフォームの接続がまだ生きている状態）に呼ばれる。
    /// ここで全窓（Arc<Window> + wgpu Painter/Surface）を明示的に drop しないと、
    /// `run_app` から戻った後（＝プラットフォーム接続が既に破棄された後）に `main` 側の
    /// `WinitApp` が drop される際に窓を破棄することになり、Wayland 環境で
    /// セグメンテーションフォルトを起こす（エクスプローラー窓を閉じてアプリを終了した
    /// ときにのみ再現していた原因）。ビューアー/ステータス窓の個別クローズはイベントループが
    /// 生きている間に drop されるため影響を受けない。
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.viewer = None;
        self.status = None;
        self.translate = None;
        self.explorer = None;
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
                } else if self.translate.as_ref().map_or(false, |w| w.viewport_id == viewport_id) {
                    if let Some(w) = self.translate.as_mut() {
                        w.bump(when);
                    }
                }
            }
            UserEvent::FocusRequested => {
                if let Some(w) = self.explorer.as_ref() {
                    crate::log_common!("[single_instance] focus requested -> focusing explorer window");
                    w.window.focus_window();
                    w.window.request_user_attention(Some(winit::window::UserAttentionType::Critical));
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
        let is_translate = self.translate.as_ref().map_or(false, |w| w.window.id() == window_id);
        if !is_explorer && !is_viewer && !is_status && !is_translate {
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
                } else if is_status {
                    // ステータス窓の OS クローズ。トグルを下ろし、窓を破棄する。
                    if let Some(app) = self.app.as_mut() {
                        app.close_status();
                    }
                    self.sync_status_window(event_loop);
                    return;
                } else {
                    // OCR/翻訳子ウィンドウの OS クローズ。トグルを下ろし、窓を破棄する。
                    if let Some(app) = self.app.as_mut() {
                        app.close_translate_window();
                    }
                    self.sync_translate_window(event_loop);
                    return;
                }
            }
            WindowEvent::Resized(size) => {
                // 各窓は自前 Painter を持つので、その窓の Painter を窓自身の viewport_id で
                // リサイズする。
                if let (Some(nw), Some(nh), Some(win)) = (
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                    self.window_mut(window_id),
                ) {
                    win.painter.on_window_resized(win.viewport_id, nw, nh);
                }
                if let Some(w) = self.window_mut(window_id) {
                    w.bump_now();
                }
                // フェーズ6: ビューアー窓のリサイズのみ再デコードのデバウンス対象にする
                // （エクスプローラー窓のリサイズは表示画像と無関係）。
                if is_viewer {
                    if let Some(app) = self.app.as_mut() {
                        app.notify_viewer_resized();
                    }
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
        self.sync_translate_window(event_loop);
        // 期限の来た窓を描画する。
        self.render_due_windows();
        // 描画中（ビューアーの ESC/X、ステータスの [?] トグル等）に窓が閉じられた可能性に追従する。
        self.sync_viewer_window(event_loop);
        self.sync_status_window(event_loop);
        self.sync_translate_window(event_loop);

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

    let ping_proxy = proxy.clone();
    crate::single_instance::start_ping_listener(move || {
        let _ = ping_proxy.send_event(UserEvent::FocusRequested);
    });

    let mut app = WinitApp::new(start_dir, cfg, state, proxy);
    event_loop.run_app(&mut app).expect("run_app");
}
