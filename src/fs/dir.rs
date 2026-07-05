use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// gvfs 経由の SMB パスかどうかを判定する（Unix のみ有効）
#[cfg(unix)]
pub fn is_gvfs_path(path: &Path) -> bool {
    path.to_string_lossy().contains("/gvfs/")
}

#[cfg(not(unix))]
pub fn is_gvfs_path(_path: &Path) -> bool {
    false
}

/// 実行時にWaylandセッションかどうかを判定する（`WAYLAND_DISPLAY`環境変数の有無による簡易判定）。
/// Windowsでは常にfalseを返す。プロセス中に変化しない前提で一度だけ評価する。
pub fn is_wayland_session() -> bool {
    #[cfg(windows)]
    { false }
    #[cfg(not(windows))]
    {
        static WAYLAND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *WAYLAND.get_or_init(|| std::env::var_os("WAYLAND_DISPLAY").is_some())
    }
}

/// サブディレクトリとアーカイブのフルスキャンをバックグラウンドで起動する。
/// タイムアウトなし: 処理が完了するまで待つ（UIはブロックしない）。
/// ユーザーが別ディレクトリに移動した時点で結果を破棄することでキャンセルに相当する。
/// 戻り値: (サブディレクトリ, ZIPアーカイブ, 生画像ファイル)
/// `wake` は結果送信後に1回呼ばれる。呼び出し側で UI（ROOT）を起こすために使う。
/// fs/ 層を egui 非依存に保つため、egui::Context ではなくコールバックを受け取る。
pub fn spawn_scan(
    dir: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send((list_subdirs(&dir), list_archives(&dir), list_raw_images(&dir)));
        wake();
    });
    rx
}

/// ツリー展開用（サブディレクトリのみ）をバックグラウンドで起動する。
/// `wake` は結果送信後に1回呼ばれる（UI を起こすため）。
pub fn spawn_scan_subdirs(
    dir: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(list_subdirs(&dir));
        wake();
    });
    rx
}

/// パスが対応アーカイブのファイル名サフィックスを持つか。
/// `.tar.gz` のような二重拡張子を正しく扱うため `extension()` ではなくファイル名末尾で判定する。
/// 7z/tar は対応 feature が有効なときのみ列挙対象に含める。
fn is_archive_path(p: &Path) -> bool {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let ends = |s: &str| name.ends_with(s);

    if ends(".zip") || ends(".cbz") {
        return true;
    }
    #[cfg(feature = "fmt-7z")]
    if ends(".7z") || ends(".cb7") {
        return true;
    }
    #[cfg(feature = "fmt-tar")]
    if ends(".tar") || ends(".cbt") || ends(".tar.gz") || ends(".tgz") {
        return true;
    }
    false
}

/// ディレクトリ直下の ZIP/CBZ/7z/CB7/TAR/CBT ファイルを列挙する
pub fn list_archives(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut result: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            if !ft.is_file() {
                return None;
            }
            let p = e.path();
            is_archive_path(&p).then_some(p)
        })
        .collect();
    result.sort();
    result
}

/// ディレクトリ直下のビューア対応生画像ファイルを列挙する
pub fn list_raw_images(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut result: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            if !ft.is_file() {
                return None;
            }
            let p = e.path();
            crate::fs::archive::is_supported_image_file(&p).then_some(p)
        })
        .collect();
    result.sort();
    result
}

/// ディレクトリ直下のサブディレクトリを列挙する（1階層のみ）
pub fn list_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut result: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            ft.is_dir().then(|| e.path())
        })
        .collect();
    result.sort();
    result
}
