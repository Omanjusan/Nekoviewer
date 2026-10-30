//! 「選択状態を維持したままファイラーで開く」。アプリはRO動作のため、ファイル操作（削除・整理など）は
//! OS標準のファイラーに任せる。その入口として、アプリ側で選択中のファイルを、ファイラー側でも選択した状態で
//! 親フォルダを開く。削除専用ではなく「アプリの選択状態をファイラーで再現する」汎用機能（重複チェックなどの土台）。
//!
//! - Windows: `SHOpenFolderAndSelectItems`（親フォルダごとに1回）
//! - Linux: `org.freedesktop.FileManager1.ShowItems`（`gdbus`、無ければ `dbus-send`）
//! - 選択を依頼できなかった親フォルダは、`open_in_file_manager` で親フォルダだけ開く（フォールバック）
//!
//! Linuxの Nautilus は `ShowItems` / `--select` のどちらでも「1ファイルにつき1ウィンドウ」を開き、同一フォルダの
//! 複数選択を1ウィンドウで再現できない（実測: Nautilus 46.4）。ウィンドウの乱立を避けるため、Nautilus が
//! `FileManager1` の所有者のときは、同一フォルダに2ファイル以上ある場合に親フォルダだけ開く（フォールバック）。
//! Nautilus 以外のファイラーは検証しておらず、全ファイルをそのまま渡す（成り行き任せ）。
//!
//! 選択が実際に反映されたかは検知できない（「依頼できたか」までしか分からない）。
//! 外部コマンド/COMの完了を待つため、呼び出し側は必ずバックグラウンドスレッドで呼ぶこと。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 一度に開く親フォルダ数の上限。超える選択は呼び出し側で項目をグレーアウトする（`exceeds_folder_limit`）。
pub const MAX_REVEAL_FOLDERS: usize = 10;

/// 親フォルダ単位の結果の集計。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RevealReport {
    /// 選択付きの表示を依頼できた親フォルダ数
    pub selected: usize,
    /// 選択を依頼できず、親フォルダだけ開いた数（フォールバック）
    pub folder_only: usize,
    /// 親フォルダが存在しないなどで何も開けなかった数
    pub failed: usize,
    /// 親フォルダ数が上限を超えていたため何もしなかった
    pub too_many: bool,
}

/// 選択を親フォルダごとにまとめる（親フォルダ順、フォルダ内は重複排除のうえパス順）。親を持たないパスは除く。
pub fn group_by_parent(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut map: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for p in paths {
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            map.entry(parent.to_path_buf()).or_default().push(p.clone());
        }
    }
    map.into_iter()
        .map(|(dir, mut files)| {
            files.sort();
            files.dedup();
            (dir, files)
        })
        .collect()
}

/// 選択が `MAX_REVEAL_FOLDERS` を超える数の親フォルダにまたがるか。
pub fn exceeds_folder_limit(paths: &[PathBuf]) -> bool {
    group_by_parent(paths).len() > MAX_REVEAL_FOLDERS
}

/// `paths` を、親フォルダごとにファイラーで選択状態にして開く。ブロッキング。
pub fn reveal_in_file_manager(paths: &[PathBuf]) -> RevealReport {
    let groups = group_by_parent(paths);
    let mut report = RevealReport::default();
    if groups.len() > MAX_REVEAL_FOLDERS {
        report.too_many = true;
        return report;
    }
    // ファイラーの判定は外部コマンドを伴うため、複数ファイルのフォルダがあるときだけ、1回だけ行う
    let mut no_multi_select: Option<bool> = None;
    for (dir, files) in groups {
        let unsupported = files.len() >= 2 && *no_multi_select.get_or_insert_with(file_manager_lacks_multi_select);
        if !dir.is_dir() {
            report.failed += 1;
        } else if !unsupported && reveal_group(&dir, &files) {
            report.selected += 1;
        } else {
            crate::translate::open_in_file_manager(&dir);
            report.folder_only += 1;
        }
    }
    report
}

/// 同一フォルダの複数選択を1ウィンドウで再現できないファイラーか。現状は Nautilus のみ判定する。
#[cfg(unix)]
fn file_manager_lacks_multi_select() -> bool {
    file_manager_process_name().is_some_and(|name| name == "nautilus")
}

#[cfg(not(unix))]
fn file_manager_lacks_multi_select() -> bool {
    false
}

/// D-Bus 名 `org.freedesktop.FileManager1` の所有プロセス名（`/proc/<pid>/comm`）。
/// 未起動なら `StartServiceByName` で起動してから引く（どのみちこのあと ShowItems で起動するため）。
#[cfg(unix)]
fn file_manager_process_name() -> Option<String> {
    const NAME: &str = "org.freedesktop.FileManager1";
    let bus = |method: &str, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("gdbus")
            .args([
                "call", "--session", "--timeout", "10", "--dest", "org.freedesktop.DBus",
                "--object-path", "/org/freedesktop/DBus", "--method",
                &format!("org.freedesktop.DBus.{method}"),
            ])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let owner = bus("GetNameOwner", &[NAME]).or_else(|| {
        bus("StartServiceByName", &[NAME, "0"]).and_then(|_| bus("GetNameOwner", &[NAME]))
    })?;
    // 出力は "(':1.271',)"
    let owner = owner.split('\'').nth(1)?.to_string();
    // 出力は "(uint32 55721,)"
    let pid_out = bus("GetConnectionUnixProcessID", &[&owner])?;
    let pid: u32 = pid_out.split_once("uint32 ")?.1.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

#[cfg(unix)]
fn reveal_group(_dir: &Path, files: &[PathBuf]) -> bool {
    let uris: Vec<String> = files.iter().map(|f| file_uri(f)).collect();
    // gdbus: 配列は GVariant テキスト形式。URI は file_uri で ' や , を %エンコード済みなので素通しでよい
    let array = format!("[{}]", uris.iter().map(|u| format!("'{u}'")).collect::<Vec<_>>().join(","));
    if run_quiet(
        "gdbus",
        &[
            "call",
            "--session",
            "--timeout",
            "10",
            "--dest",
            "org.freedesktop.FileManager1",
            "--object-path",
            "/org/freedesktop/FileManager1",
            "--method",
            "org.freedesktop.FileManager1.ShowItems",
            &array,
            "",
        ],
    ) {
        return true;
    }
    run_quiet(
        "dbus-send",
        &[
            "--session",
            "--print-reply",
            "--reply-timeout=10000",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{}", uris.join(",")),
            "string:",
        ],
    )
}

/// 外部コマンドを実行して終了を待ち、正常終了なら true。コマンドが無い/起動失敗/非ゼロ終了は false。
#[cfg(unix)]
fn run_quiet(program: &str, args: &[&str]) -> bool {
    use std::process::{Command, Stdio};
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `file://` URI。英数字と `- . _ ~ /` 以外は%エンコードする（`,` `'` 空白などを含む）。
#[cfg(unix)]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut uri = String::from("file://");
    for &b in abs.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(windows)]
fn reveal_group(dir: &Path, files: &[PathBuf]) -> bool {
    win::select_items(dir, files)
}

#[cfg(not(any(unix, windows)))]
fn reveal_group(_dir: &Path, _files: &[PathBuf]) -> bool {
    false
}

#[cfg(windows)]
mod win {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::null;

    use windows_sys::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    // windows-sys 0.52 には ITEMIDLIST 系の feature が無いため、shell32 の必要な4関数だけ自前で宣言する。
    // ITEMIDLIST は中身を触らない不透明ポインタ（*mut c_void）として扱う。
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ILCreateFromPathW(path: *const u16) -> *mut c_void;
        fn ILFindLastID(pidl: *const c_void) -> *mut c_void;
        fn ILFree(pidl: *const c_void);
        fn SHOpenFolderAndSelectItems(
            folder: *const c_void,
            count: u32,
            items: *const *const c_void,
            flags: u32,
        ) -> i32;
    }

    fn to_wide(path: &Path) -> Vec<u16> {
        // `\\?\` 接頭辞は ILCreateFromPathW が受け付けないことがあるため外す（`\\?\UNC\` はそのまま）
        let s = path.as_os_str().to_string_lossy();
        let trimmed: PathBuf = match s.strip_prefix(r"\\?\") {
            Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
            _ => path.to_path_buf(),
        };
        OsStr::new(&trimmed).encode_wide().chain(std::iter::once(0)).collect()
    }

    /// `dir` を開き、`files`（すべて `dir` 直下）を選択状態にする。依頼できたら true。
    pub fn select_items(dir: &Path, files: &[PathBuf]) -> bool {
        unsafe {
            // 呼び出しスレッドが既に別モードでCOM初期化済み（RPC_E_CHANGED_MODE）でもそのまま使える。
            // その場合は自分で初期化していないので CoUninitialize も呼ばない。
            let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
            let owns_com = hr >= 0;
            if !owns_com && hr != RPC_E_CHANGED_MODE {
                return false;
            }

            let folder = ILCreateFromPathW(to_wide(dir).as_ptr());
            // 子の相対PIDLは、フルPIDLの最後の要素を指すポインタ（フルPIDLが生きている間だけ有効）
            let fulls: Vec<*mut c_void> = files.iter().map(|f| ILCreateFromPathW(to_wide(f).as_ptr())).collect();
            let children: Vec<*const c_void> =
                fulls.iter().filter(|p| !p.is_null()).map(|&p| ILFindLastID(p) as *const c_void).collect();

            let ok = !folder.is_null()
                && !children.is_empty()
                && SHOpenFolderAndSelectItems(folder, children.len() as u32, children.as_ptr(), 0) >= 0;

            for p in fulls.into_iter().filter(|p| !p.is_null()) {
                ILFree(p);
            }
            if !folder.is_null() {
                ILFree(folder);
            }
            if owns_com {
                CoUninitialize();
            }
            ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn group_by_parent_groups_sorts_and_dedups() {
        let paths = vec![p("/b/2.zip"), p("/a/y.zip"), p("/b/1.zip"), p("/a/y.zip")];
        let groups = group_by_parent(&paths);
        assert_eq!(
            groups,
            vec![(p("/a"), vec![p("/a/y.zip")]), (p("/b"), vec![p("/b/1.zip"), p("/b/2.zip")])]
        );
    }

    #[test]
    fn group_by_parent_skips_parentless() {
        assert!(group_by_parent(&[p("/"), p("name_only")]).is_empty());
    }

    #[test]
    fn folder_limit_boundary() {
        let ten: Vec<PathBuf> = (0..10).map(|i| p(&format!("/d{i}/f.zip"))).collect();
        assert!(!exceeds_folder_limit(&ten));
        let mut eleven = ten.clone();
        eleven.push(p("/d10/f.zip"));
        assert!(exceeds_folder_limit(&eleven));
        // 同一フォルダ内の件数は上限に影響しない
        let many_files: Vec<PathBuf> = (0..50).map(|i| p(&format!("/d/f{i}.zip"))).collect();
        assert!(!exceeds_folder_limit(&many_files));
    }

    /// 実セッションのD-Bus/ファイラーに依存するため通常は走らせない（`cargo test -- --ignored`）。
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn detects_file_manager_process_name() {
        let name = file_manager_process_name();
        eprintln!("FileManager1 owner comm = {name:?}");
        assert!(name.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn file_uri_encodes_reserved_chars() {
        assert_eq!(file_uri(Path::new("/a b/c,d'e.zip")), "file:///a%20b/c%2Cd%27e.zip");
        // 日本語はUTF-8バイト列で%エンコード
        assert_eq!(file_uri(Path::new("/あ")), "file:///%E3%81%82");
    }
}
