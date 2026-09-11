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

/// ツリーの一括リロード用。複数フォルダの子ディレクトリ一覧をスレッド1本の中で
/// 順番に取得し、まとめて1回で返す（フォルダごとにスレッドを立てない）。
/// 到達不能なパス（GVFS切断等）は list_subdirs が空Vecを返すだけで panic しない。
pub fn spawn_scan_subdirs_many(
    dirs: Vec<PathBuf>,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<Vec<(PathBuf, Vec<PathBuf>)>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let results = dirs
            .into_iter()
            .map(|d| {
                let children = list_subdirs(&d);
                (d, children)
            })
            .collect();
        let _ = tx.send(results);
        wake();
    });
    rx
}

/// ファイル名がフィルタ/検索条件にマッチするか判定する（フィルタ機能・検索機能で共有）。
/// glob特殊文字（*, ?, [）を含む場合はglobパターンとして、それ以外は小文字部分一致として扱う
/// （globとして不正な場合も部分一致にフォールバックする）。pattern_text が空（trim後）なら
/// 常にマッチする。
pub fn name_matches(pattern_text: &str, filename: &str) -> bool {
    let text = pattern_text.trim();
    if text.is_empty() {
        return true;
    }
    if text.contains(['*', '?', '[']) {
        if let Ok(pat) = glob::Pattern::new(text) {
            let match_opts = glob::MatchOptions {
                case_sensitive: false,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            };
            return pat.matches_with(filename, match_opts);
        }
    }
    filename.to_lowercase().contains(&text.to_lowercase())
}

/// パスが対応アーカイブのファイル名サフィックスを持つか。
/// `.tar.gz` のような二重拡張子を正しく扱うため `extension()` ではなくファイル名末尾で判定する。
/// 7z/tar は対応 feature が有効なときのみ列挙対象に含める。
pub(crate) fn is_archive_path(p: &Path) -> bool {
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
    #[cfg(feature = "tar-zstd")]
    if ends(".tar.zst") || ends(".tzst") {
        return true;
    }
    false
}

/// 右クリックメニュー登録など「拡張子そのもの」を列挙したい場面向けの単純な
/// （複合でない）対応アーカイブ拡張子一覧。`.tar.gz`/`.tar.zst` はWindowsが
/// 最後のドット以降のみを拡張子とみなすため対象外（is_archive_pathの複合判定とは別枠）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn simple_archive_extensions() -> Vec<&'static str> {
    let mut exts = vec!["zip", "cbz"];
    #[cfg(feature = "fmt-7z")]
    exts.extend(["7z", "cb7"]);
    #[cfg(feature = "fmt-tar")]
    exts.extend(["tar", "cbt", "tgz"]);
    #[cfg(feature = "tar-zstd")]
    exts.push("tzst");
    exts
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
