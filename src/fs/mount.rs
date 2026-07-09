use std::path::{Path, PathBuf};

pub struct MountEntry {
    pub label: String,
    pub path: PathBuf,
}

/// path が既知のネットワークマウント（SMB等）配下にあれば、そのマウント大元の
/// ルートパスを返す。ファイル単位ではなく大元単位で判定するための入口。
#[cfg(unix)]
pub fn network_mount_root(path: &Path) -> Option<PathBuf> {
    let uid = current_uid();
    let gvfs_dir = PathBuf::from(format!("/run/user/{uid}/gvfs"));
    let entries = std::fs::read_dir(&gvfs_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("smb-share:") {
            continue;
        }
        let root = entry.path();
        if path.starts_with(&root) {
            return Some(root);
        }
    }
    None
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

/// マウント大元への到達可否を1アクション（read_dir）で判定する。
/// ネットワークI/Oでブロックしうるため、呼び出し側は必ずバックグラウンドスレッドで呼ぶこと。
pub fn check_mount_reachable(root: &Path) -> bool {
    std::fs::read_dir(root).is_ok()
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

/// /run/user/{uid}/gvfs/ 以下の SMB マウントを列挙する（Unix のみ）
#[cfg(unix)]
pub fn list_gvfs_smb_mounts() -> Vec<MountEntry> {
    let uid = current_uid();
    let gvfs_dir = PathBuf::from(format!("/run/user/{uid}/gvfs"));

    let read_dir = match std::fs::read_dir(&gvfs_dir) {
        Ok(r) => r,
        Err(e) => {
            println!("[gvfs] read_dir({}) failed: {e}", gvfs_dir.display());
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
pub fn list_gvfs_smb_mounts() -> Vec<MountEntry> {
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

/// 起動時に gvfs の状態をターミナルへ出力する（Unix のみ）
#[cfg(unix)]
pub fn log_gvfs_status() {
    let uid = current_uid();
    let gvfs_dir = PathBuf::from(format!("/run/user/{uid}/gvfs"));
    println!("[gvfs] checking: {}", gvfs_dir.display());

    match std::fs::read_dir(&gvfs_dir) {
        Err(e) => println!("[gvfs] not accessible: {e}"),
        Ok(entries) => {
            let names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            if names.is_empty() {
                println!("[gvfs] directory exists but no mounts found");
            } else {
                println!("[gvfs] found {} entries:", names.len());
                for name in &names {
                    let tag = if name.starts_with("smb-share:") { "SMB" } else { "   " };
                    println!("[gvfs]   [{tag}] {name}");
                }
            }
        }
    }
}

#[cfg(not(unix))]
pub fn log_gvfs_status() {}

#[cfg(unix)]
fn current_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1000)
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
