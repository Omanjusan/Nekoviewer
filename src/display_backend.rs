//! 起動時の表示バックエンド（Wayland ネイティブ / XWayland）の管理。
//!
//! Wayland ネイティブでは winit が窓の座標を取得・指定できず、「ウィンドウ位置」メニューの記録・展開・
//! 復帰が効かない。XWayland（X11 バックエンド）なら動くので、**次回起動時に XWayland を使う**かどうかを
//! 永続設定にし、起動時に `with_x11()` で強制する（`WINIT_UNIX_BACKEND` は winit 0.30 に無い）。
//! 保存するのは「次回の起動モード」だけ。メニューのチェックは状態ではなく、
//! 「次回に今のモードを変えたい」という意思として表示する（[`switch_pending`]）。
//!
//! 描画の present mode（[`crate::fs::dir::is_wayland_session`]）は、実バックエンドではなく
//! 「Wayland セッションか」で決め続ける。XWayland でも Wayland コンポジタが隠れた窓の frame を
//! 返さない問題の影響を受けうるため。

use std::sync::atomic::{AtomicBool, Ordering};

/// いま動いているバックエンド。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Wayland セッションではない（X11 セッション・Windows）。メニューには何も出さない。
    NotWayland,
    /// Wayland ネイティブ。窓位置は扱えない。
    Wayland,
    /// Wayland セッション上で X11 バックエンドを強制している（XWayland）。
    XWayland,
}

static FORCED_X11: AtomicBool = AtomicBool::new(false);
static X11_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
static NEXT_LAUNCH_X11: AtomicBool = AtomicBool::new(false);
static WAYLAND_SESSION: AtomicBool = AtomicBool::new(false);

/// 起動時の判断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub force_x11: bool,
    /// X11 を望まれているのに DISPLAY が無く、Wayland のまま起動する。
    pub unavailable: bool,
}

/// 純粋な判断。X11 を強制するのは、Wayland セッションで、望まれていて、DISPLAY があるときだけ。
pub fn decide(wayland_session: bool, display_set: bool, want_x11: bool) -> Decision {
    let wants = wayland_session && want_x11;
    Decision { force_x11: wants && display_set, unavailable: wants && !display_set }
}

/// 起動時に一度だけ呼ぶ。保存済みの「次回 XWayland」と環境から、今回のバックエンドを決める。
/// 戻り値が true なら、イベントループを `with_x11()` で作ること。
pub fn init(want_x11: bool) -> bool {
    let wayland = crate::fs::dir::is_wayland_session();
    let display_set = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    let d = decide(wayland, display_set, want_x11);
    WAYLAND_SESSION.store(wayland, Ordering::Relaxed);
    FORCED_X11.store(d.force_x11, Ordering::Relaxed);
    X11_UNAVAILABLE.store(d.unavailable, Ordering::Relaxed);
    NEXT_LAUNCH_X11.store(want_x11, Ordering::Relaxed);
    if d.unavailable {
        crate::log_common!("[backend] XWayland was requested but DISPLAY is not set -> staying on Wayland");
    } else if d.force_x11 {
        crate::log_common!("[backend] forcing X11 backend (XWayland)");
    }
    #[cfg(feature = "test-control")]
    apply_fake_backend();
    d.force_x11
}

/// 自動テスト用: 実際のバックエンドは変えず、メニューの表示だけを指定の状態にする。
/// `NEKO_FAKE_BACKEND` = `wayland` / `xwayland` / `unavailable`（Wayland のまま、XWayland 不可のエラー付き）。
#[cfg(feature = "test-control")]
fn apply_fake_backend() {
    let Ok(v) = std::env::var("NEKO_FAKE_BACKEND") else { return };
    let (wayland, forced, unavailable) = match v.as_str() {
        "wayland" => (true, false, false),
        "xwayland" => (true, true, false),
        "unavailable" => (true, false, true),
        _ => return,
    };
    WAYLAND_SESSION.store(wayland, Ordering::Relaxed);
    FORCED_X11.store(forced, Ordering::Relaxed);
    X11_UNAVAILABLE.store(unavailable, Ordering::Relaxed);
    // 実際の XWayland 起動は「次回 XWayland」が保存されているときだけ起こるので、それに合わせる
    if forced {
        NEXT_LAUNCH_X11.store(true, Ordering::Relaxed);
    }
}

pub fn mode() -> Mode {
    if !WAYLAND_SESSION.load(Ordering::Relaxed) {
        Mode::NotWayland
    } else if FORCED_X11.load(Ordering::Relaxed) {
        Mode::XWayland
    } else {
        Mode::Wayland
    }
}

/// XWayland を望まれたのに DISPLAY が無く、Wayland のまま起動したか。
pub fn x11_unavailable() -> bool {
    X11_UNAVAILABLE.load(Ordering::Relaxed)
}

pub fn next_launch_x11() -> bool {
    NEXT_LAUNCH_X11.load(Ordering::Relaxed)
}

pub fn set_next_launch_x11(v: bool) {
    NEXT_LAUNCH_X11.store(v, Ordering::Relaxed);
}

/// メニューのチェックの表示。「次回に今のモードを変える」意思があるか。
/// Wayland 起動中は「次回 XWayland」が保存されていれば、XWayland 起動中は保存されていなければ true。
pub fn switch_pending_for(mode: Mode, next_launch_x11: bool) -> bool {
    match mode {
        Mode::Wayland => next_launch_x11,
        Mode::XWayland => !next_launch_x11,
        Mode::NotWayland => false,
    }
}

/// チェックを `pending` にしたときに保存すべき「次回 XWayland」の値。
pub fn next_launch_for_pending(mode: Mode, pending: bool) -> bool {
    match mode {
        Mode::XWayland => !pending,
        Mode::Wayland | Mode::NotWayland => pending,
    }
}

pub fn switch_pending() -> bool {
    switch_pending_for(mode(), next_launch_x11())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_x11_only_in_a_wayland_session_that_wants_it_and_has_display() {
        assert_eq!(decide(true, true, true), Decision { force_x11: true, unavailable: false });
        assert_eq!(decide(true, false, true), Decision { force_x11: false, unavailable: true });
        assert_eq!(decide(true, true, false), Decision { force_x11: false, unavailable: false });
        // X11 セッション・Windows では、保存値が true でも何もしない
        assert_eq!(decide(false, true, true), Decision { force_x11: false, unavailable: false });
    }

    #[test]
    fn the_checkbox_means_a_wish_to_change_the_current_mode() {
        // Wayland 起動中: 保存が false(=Wayland のまま)なら未チェック、true ならチェック済み
        assert!(!switch_pending_for(Mode::Wayland, false));
        assert!(switch_pending_for(Mode::Wayland, true));
        // XWayland 起動中: 保存が true(=XWayland のまま)なら未チェック、false ならチェック済み
        assert!(!switch_pending_for(Mode::XWayland, true));
        assert!(switch_pending_for(Mode::XWayland, false));
        assert!(!switch_pending_for(Mode::NotWayland, true));
    }

    #[test]
    fn toggling_the_checkbox_round_trips_through_the_saved_value() {
        for mode in [Mode::Wayland, Mode::XWayland] {
            for pending in [false, true] {
                assert_eq!(switch_pending_for(mode, next_launch_for_pending(mode, pending)), pending);
            }
        }
        // 起動直後（保存値=今のモードのまま）は未チェック
        assert_eq!(next_launch_for_pending(Mode::Wayland, false), false);
        assert_eq!(next_launch_for_pending(Mode::XWayland, false), true);
    }
}
