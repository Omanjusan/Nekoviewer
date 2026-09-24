use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct MountEntry {
    pub label: String,
    pub path: PathBuf,
}

/// path が既知のネットワークマウント（SMB等）配下にあれば、そのマウント大元の
/// ルートパスを返す。ファイル単位ではなく大元単位で判定するための入口。
///
/// Unix はパス構造（/run/user/{uid}/gvfs/smb-share:… 直下）だけで判定し、I/O は行わない。
/// gvfs のトップレベルを readdir すると FUSE 経由で gvfsd の応答待ちになり、接続先が
/// 落ちていると呼び出しスレッド（UI含む）が長時間止まるため。
#[cfg(unix)]
pub fn network_mount_root(path: &Path) -> Option<PathBuf> {
    let gvfs_dir = gvfs_dir();
    let name = path.strip_prefix(&gvfs_dir).ok()?.components().next()?.as_os_str().to_str()?;
    if !name.starts_with("smb-share:") {
        return None;
    }
    Some(gvfs_dir.join(name))
}

#[cfg(windows)]
pub fn network_mount_root(path: &Path) -> Option<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOTE: u32 = 4;

    let root = path.ancestors().last()?.to_path_buf();
    let root_str = root.to_string_lossy();
    let wide: Vec<u16> = root_str.encode_utf16().chain(std::iter::once(0)).collect();
    if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_REMOTE {
        Some(root)
    } else {
        None
    }
}

#[cfg(not(any(unix, windows)))]
pub fn network_mount_root(_path: &Path) -> Option<PathBuf> {
    None
}

/// サムネグリッドの「↑」用: path の一つ上の階層を返す。
/// ローカルのファイルシステムルート・ドライブ文字ルートは `Path::parent()` が
/// 自然に `None` を返すため素通りでよい。ネットワークマウント（gvfs の SMB や
/// Windows のリモートドライブ）は大元（`network_mount_root`）に到達した時点で
/// `None` を返し、その上位（gvfs のマウント列挙ディレクトリ等）へは進ませない。
/// Windows のネットワークドライブはドライブ文字ルートで `parent()` が止まるため
/// ローカルドライブと同じ経路に合流し、特別な分岐は不要。
pub fn up_target(path: &Path) -> Option<PathBuf> {
    if let Some(mount_root) = network_mount_root(path)
        && path == mount_root
    {
        return None;
    }
    path.parent().map(|p| p.to_path_buf())
}

/// マウント大元への到達可否を1アクションで判定する。
/// ネットワークI/Oでブロックしうるため、呼び出し側は必ずバックグラウンドスレッドで呼ぶこと。
///
/// Unix の gvfs SMB マウントは、マウント名の server へ TCP 接続できるかで判定する。
/// read_dir は FUSE 経由のため、接続先が落ちていると gvfsd の応答待ちで止まり、
/// その間はプロセス終了も待たされる（FUSE の要求は kill しても gvfsd の応答まで終わらない）。
/// ソケットの待ちならプロセス終了を止めない。ホストが生きていれば「到達可」とみなす割り切りで、
/// 共有だけ外れているケースは見逃すが、その場合は gvfs 側がすぐエラーを返すため固まらない。
pub fn check_mount_reachable(root: &Path) -> bool {
    #[cfg(unix)]
    if let Some((host, ports)) = root
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(parse_smb_endpoint)
    {
        return tcp_reachable(&host, &ports, SMB_CONNECT_TIMEOUT);
    }
    std::fs::read_dir(root).is_ok()
}

/// SMB 到達判定の接続タイムアウト（アドレス1件あたり）。
#[cfg(unix)]
const SMB_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// "smb-share:server=mynas,share=media[,port=N]" → (host, 試すポート列)。
/// port 指定があればそれのみ、なければ libsmbclient と同じく 445 → 139 の順で試す。
/// server= を含まない名前は None（呼び出し側は read_dir 判定へフォールバックする）。
#[cfg(unix)]
fn parse_smb_endpoint(name: &str) -> Option<(String, Vec<u16>)> {
    let params = name.strip_prefix("smb-share:")?;
    let mut server = None;
    let mut port = None;
    for part in params.split(',') {
        if let Some(v) = part.strip_prefix("server=") {
            server = Some(v);
        } else if let Some(v) = part.strip_prefix("port=") {
            port = v.parse::<u16>().ok();
        }
    }
    let host = server?.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    let ports = match port {
        Some(p) => vec![p],
        None => vec![445, 139],
    };
    Some((host.to_string(), ports))
}

/// host の各ポート・各アドレスへ順に TCP 接続を試み、1件でも繋がれば true。
#[cfg(unix)]
fn tcp_reachable(host: &str, ports: &[u16], timeout: std::time::Duration) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    ports.iter().any(|&port| {
        let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
        addrs.into_iter().any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
    })
}

/// マウント大元の到達可否をバックグラウンドスレッドで確認する。
/// 定期ポーリングはしない前提のため、呼び出し側が明示的なタイミング
/// （オープン失敗の検知・リンク切れ表示中ファイルの再オープン試行）でのみ呼ぶこと。
pub fn spawn_mount_reachability_check(
    root: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> std::sync::mpsc::Receiver<(PathBuf, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reachable = check_mount_reachable(&root);
        let _ = tx.send((root, reachable));
        wake();
    });
    rx
}

/// SMB マウント大元のパスから、ドライブ一覧用のエントリを I/O なしで組み立てる。
/// gvfs の一覧取得（非同期）を待たずに、起動フォルダが SMB 配下のときのツリールートを決めるのに使う。
#[cfg(unix)]
pub fn smb_mount_entry(root: &Path) -> Option<MountEntry> {
    let name = root.file_name()?.to_str()?;
    if !name.starts_with("smb-share:") {
        return None;
    }
    Some(MountEntry { label: parse_smb_label(name), path: root.to_path_buf() })
}

#[cfg(not(unix))]
pub fn smb_mount_entry(_root: &Path) -> Option<MountEntry> {
    None
}

/// list_gvfs_smb_mounts をバックグラウンドスレッドで実行する。
/// トップレベルの readdir も FUSE 経由で止まりうるため、UI スレッドからは必ずこちらを使う。
pub fn spawn_list_gvfs_smb_mounts(
    wake: impl Fn() + Send + 'static,
) -> std::sync::mpsc::Receiver<Vec<MountEntry>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(list_gvfs_smb_mounts());
        wake();
    });
    rx
}

#[cfg(unix)]
fn gvfs_dir() -> PathBuf {
    PathBuf::from(format!("/run/user/{}/gvfs", current_uid()))
}

/// /run/user/{uid}/gvfs/ 以下の SMB マウントを列挙する（Unix のみ）。
/// FUSE 経由でブロックしうるため、呼び出しは spawn_list_gvfs_smb_mounts 経由で行う。
#[cfg(unix)]
fn list_gvfs_smb_mounts() -> Vec<MountEntry> {
    let gvfs_dir = gvfs_dir();

    let read_dir = match std::fs::read_dir(&gvfs_dir) {
        Ok(r) => r,
        Err(e) => {
            crate::log_common!("[gvfs] read_dir({}) failed: {e}", gvfs_dir.display());
            return Vec::new();
        }
    };

    let mut mounts = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("smb-share:") {
            continue;
        }
        // "smb-share:server=mynas,share=media" → label "mynas/media"
        let label = parse_smb_label(&name);
        mounts.push(MountEntry {
            label,
            path: entry.path(),
        });
    }
    mounts.sort_by(|a, b| a.label.cmp(&b.label));
    mounts
}

#[cfg(not(unix))]
fn list_gvfs_smb_mounts() -> Vec<MountEntry> {
    Vec::new()
}

/// 固定ドライブ（ホームとルート）を返す
pub fn list_local_drives() -> Vec<MountEntry> {
    let mut drives = Vec::new();

    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

        if let Ok(home) = std::env::var("USERPROFILE") {
            drives.push(MountEntry {
                label: "ホーム".to_string(),
                path: PathBuf::from(home),
            });
        }

        let bitmask = unsafe { GetLogicalDrives() };
        for bit in 0u32..26 {
            if bitmask & (1 << bit) == 0 {
                continue;
            }
            let letter = (b'A' + bit as u8) as char;
            let path_str = format!("{}:\\", letter);
            let path_wide: Vec<u16> =
                path_str.encode_utf16().chain(std::iter::once(0)).collect();
            // DRIVE_UNKNOWN=0, DRIVE_NO_ROOT_DIR=1 は除外
            if unsafe { GetDriveTypeW(path_wide.as_ptr()) } <= 1 {
                continue;
            }
            drives.push(MountEntry {
                label: path_str.clone(),
                path: PathBuf::from(path_str),
            });
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            drives.push(MountEntry {
                label: "ホーム".to_string(),
                path: PathBuf::from(home),
            });
        }
        drives.push(MountEntry {
            label: "/".to_string(),
            path: PathBuf::from("/"),
        });
    }

    drives
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid は常に成功し、副作用もない。
    unsafe { libc::getuid() }
}

/// "smb-share:server=mynas,share=media" → "mynas/media"
#[cfg(unix)]
fn parse_smb_label(name: &str) -> String {
    let params = name.trim_start_matches("smb-share:");
    let mut server = "";
    let mut share = "";
    for part in params.split(',') {
        if let Some(v) = part.strip_prefix("server=") {
            server = v;
        } else if let Some(v) = part.strip_prefix("share=") {
            share = v;
        }
    }
    if server.is_empty() {
        name.to_string()
    } else {
        format!("{server}/{share}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn network_mount_root_from_path_structure_only() {
        let root = gvfs_dir().join("smb-share:server=nas,share=media");
        assert_eq!(network_mount_root(&root), Some(root.clone()));
        assert_eq!(network_mount_root(&root.join("comics/a.zip")), Some(root.clone()));
        // gvfs 直下でも SMB 以外、gvfs ディレクトリ自体、gvfs 外は対象外。
        assert_eq!(network_mount_root(&gvfs_dir().join("sftp:host=example/x")), None);
        assert_eq!(network_mount_root(&gvfs_dir()), None);
        assert_eq!(network_mount_root(Path::new("/home/neko/smb-share:server=nas,share=media")), None);
        // マウント大元では「↑」を出さない。
        assert_eq!(up_target(&root), None);
        assert_eq!(up_target(&root.join("comics")), Some(root));
    }

    #[cfg(unix)]
    #[test]
    fn smb_mount_entry_builds_label_without_io() {
        let root = gvfs_dir().join("smb-share:server=nas,share=media");
        let entry = smb_mount_entry(&root).unwrap();
        assert_eq!(entry.label, "nas/media");
        assert_eq!(entry.path, root);
        assert!(smb_mount_entry(&gvfs_dir().join("sftp:host=example")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn parse_smb_endpoint_defaults_to_445_then_139() {
        assert_eq!(
            parse_smb_endpoint("smb-share:server=mynas.local,share=media"),
            Some(("mynas.local".to_string(), vec![445, 139])),
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_smb_endpoint_uses_explicit_port_and_ignores_user() {
        assert_eq!(
            parse_smb_endpoint("smb-share:port=1445,server=10.0.0.5,share=media,user=neko"),
            Some(("10.0.0.5".to_string(), vec![1445])),
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_smb_endpoint_strips_ipv6_brackets() {
        assert_eq!(
            parse_smb_endpoint("smb-share:server=[fe80::1],share=media"),
            Some(("fe80::1".to_string(), vec![445, 139])),
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_smb_endpoint_none_without_server() {
        assert_eq!(parse_smb_endpoint("smb-share:share=media"), None);
        assert_eq!(parse_smb_endpoint("smb-share:server=,share=media"), None);
        assert_eq!(parse_smb_endpoint("sftp:host=example"), None);
    }

    #[cfg(unix)]
    #[test]
    fn tcp_reachable_detects_listening_and_closed_ports() {
        let timeout = std::time::Duration::from_secs(1);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open_port = listener.local_addr().unwrap().port();
        assert!(tcp_reachable("127.0.0.1", &[open_port], timeout));

        // バインドして即閉じたポートは待ち受けがなく、接続は拒否される。
        let closed_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(!tcp_reachable("127.0.0.1", &[closed_port], timeout));
        // 先頭ポートが不通でも後続ポートで繋がれば到達可。
        assert!(tcp_reachable("127.0.0.1", &[closed_port, open_port], timeout));
    }

    #[test]
    fn up_target_ascends_local_parent() {
        let dir = std::env::temp_dir().join("nekoviewer_up_target_test").join("child");
        let expected = dir.parent().map(|p| p.to_path_buf());
        assert_eq!(up_target(&dir), expected);
    }

    #[test]
    fn up_target_none_at_filesystem_root() {
        // network_mount_root がこの環境で誤検出しない前提のローカルルート判定。
        let roots = path_roots();
        for root in roots {
            if network_mount_root(&root).is_none() {
                assert_eq!(up_target(&root), None);
            }
        }
    }

    fn path_roots() -> Vec<PathBuf> {
        #[cfg(windows)]
        {
            vec![PathBuf::from("C:\\")]
        }
        #[cfg(not(windows))]
        {
            vec![PathBuf::from("/")]
        }
    }
}
