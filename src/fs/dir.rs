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

/// ディレクトリ直下の ZIP/CBZ ファイルを列挙する
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
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("zip") | Some("cbz") | Some("ZIP") | Some("CBZ")
                    | Some("7z") | Some("cb7") | Some("7Z") | Some("CB7")
            )
            .then_some(p)
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
