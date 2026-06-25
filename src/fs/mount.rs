use std::path::PathBuf;

pub struct MountEntry {
    pub label: String,
    pub path: PathBuf,
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
