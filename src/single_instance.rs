//! 多重起動禁止のための単一インスタンス検知。
//!
//! Windows は名前付きMutex、Unix系はロックファイルへの flock(LOCK_EX|LOCK_NB) で判定する。
//! どちらもOSがプロセス終了時に自動解放するため、異常終了時の残骸（stale lock）を
//! 手動で掃除する処理は不要。
//!
//! `NEKOVIEWER_ALLOW_MULTI=1` が設定されていれば検知自体をスキップし、常に
//! `Acquired` を返す（開発時に複数プロセスを並行起動したい場合の逃げ道）。

/// ロックを握り続けるためのガード。`main()` の生存期間いっぱい変数に束縛しておくこと。
/// Drop すると（＝プロセス終了時）OSがロックを解放する。
pub struct InstanceGuard(#[allow(dead_code)] Inner);

enum Inner {
    Bypassed,
    #[cfg(windows)]
    Mutex(windows_sys::Win32::Foundation::HANDLE),
    #[cfg(unix)]
    LockFile(std::fs::File),
}

pub enum AcquireResult {
    /// 自分が先発（唯一のインスタンス）。ガードを保持し続ける。
    Acquired(InstanceGuard),
    /// 既に他プロセスが起動済み。
    AlreadyRunning,
}

pub fn acquire() -> AcquireResult {
    if std::env::var("NEKOVIEWER_ALLOW_MULTI").ok().as_deref() == Some("1") {
        return AcquireResult::Acquired(InstanceGuard(Inner::Bypassed));
    }

    #[cfg(windows)]
    {
        acquire_windows()
    }
    #[cfg(unix)]
    {
        acquire_unix()
    }
    #[cfg(not(any(windows, unix)))]
    {
        // 未対応OS: 検知せず常に先発扱い。
        AcquireResult::Acquired(InstanceGuard(Inner::Bypassed))
    }
}

#[cfg(windows)]
fn acquire_windows() -> AcquireResult {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // "Global\\" プレフィックスでセッションを跨いで単一性を保証する。
    let name = to_wide(r"Global\Nekoviewer_SingleInstance_Lock");

    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        // Mutex作成自体に失敗。判定不能なので先発扱いで続行する。
        return AcquireResult::Acquired(InstanceGuard(Inner::Bypassed));
    }

    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe { CloseHandle(handle) };
        AcquireResult::AlreadyRunning
    } else {
        AcquireResult::Acquired(InstanceGuard(Inner::Mutex(handle)))
    }
}

#[cfg(windows)]
impl Drop for Inner {
    fn drop(&mut self) {
        if let Inner::Mutex(handle) = self {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(*handle) };
        }
    }
}

#[cfg(unix)]
fn acquire_unix() -> AcquireResult {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let path = lock_path();
    let file = match OpenOptions::new().create(true).write(true).open(&path) {
        Ok(f) => f,
        Err(_) => {
            // ロックファイルすら開けない環境。判定不能なので先発扱いで続行する。
            return AcquireResult::Acquired(InstanceGuard(Inner::Bypassed));
        }
    };

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        AcquireResult::Acquired(InstanceGuard(Inner::LockFile(file)))
    } else {
        AcquireResult::AlreadyRunning
    }
}

#[cfg(unix)]
fn lock_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(dir).join("nekoviewer.lock")
}

// ── IPC: 後発プロセス→先発プロセスへの ping ─────────────────────────────────
//
// ping の中身に意味はない（起動パスは無視する仕様のため）。「誰かがもう一度
// 起動しようとした」という事実だけを先発プロセスへ伝える。

/// 先発プロセス側。バックグラウンドスレッドで接続を待ち受け、pingを受けるたびに
/// `on_ping` を呼ぶ。呼び出し元は必要に応じて winit の `EventLoopProxy` 等へ
/// ブリッジすること。
pub fn start_ping_listener(on_ping: impl Fn() + Send + 'static) {
    #[cfg(unix)]
    {
        start_ping_listener_unix(on_ping);
    }
    #[cfg(windows)]
    {
        start_ping_listener_windows(on_ping);
    }
}

/// 後発プロセス側。先発プロセスに繋がれば ping を送って `true` を返す。
/// 繋がらなければ（先発が実は死んでいた等）`false`。
pub fn send_ping() -> bool {
    #[cfg(unix)]
    {
        send_ping_unix()
    }
    #[cfg(windows)]
    {
        send_ping_windows()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(unix)]
fn ipc_socket_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(dir).join("nekoviewer.sock")
}

#[cfg(unix)]
fn start_ping_listener_unix(on_ping: impl Fn() + Send + 'static) {
    use std::os::unix::net::UnixListener;

    let path = ipc_socket_path();
    // 起動時点で flock は既に取得済み（＝自分が唯一の先発）なので、残置ソケット
    // ファイルは前回の異常終了の残骸と断定してよく、安全に消せる。
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            crate::log_common!("[single_instance] failed to bind ipc socket: {e}");
            return;
        }
    };

    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if conn.is_ok() {
                on_ping();
            }
        }
    });
}

#[cfg(unix)]
fn send_ping_unix() -> bool {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    match UnixStream::connect(ipc_socket_path()) {
        Ok(mut s) => s.write_all(b"ping").is_ok(),
        Err(_) => false,
    }
}

#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\Nekoviewer_SingleInstance_Ping";

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn start_ping_listener_windows(on_ping: impl Fn() + Send + 'static) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, FILE_FLAG_FIRST_PIPE_INSTANCE};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
        PIPE_UNLIMITED_INSTANCES,
    };

    std::thread::spawn(move || {
        let name = to_wide(PIPE_NAME);
        loop {
            let handle = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    64,
                    0,
                    std::ptr::null(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                crate::log_common!("[single_instance] CreateNamedPipeW failed");
                return;
            }

            let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
            if connected != 0 {
                on_ping();
            }
            unsafe {
                DisconnectNamedPipe(handle);
                CloseHandle(handle);
            }
        }
    });
    // 未使用警告避け（FILE_FLAG_FIRST_PIPE_INSTANCEは複数インスタンス許容のため使わない）
    let _ = FILE_FLAG_FIRST_PIPE_INSTANCE;
}

#[cfg(windows)]
fn send_ping_windows() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let name = to_wide(PIPE_NAME);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return false;
    }

    let payload = b"ping";
    let mut written: u32 = 0;
    let ok = unsafe {
        WriteFile(
            handle,
            payload.as_ptr(),
            payload.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    ok != 0
}
