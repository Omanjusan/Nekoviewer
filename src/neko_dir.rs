use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// dir に対応するキャッシュディレクトリのパスを返す（まだ作成しない）。
/// パスの正規化に失敗した場合（存在しないパス等）は None を返す。
pub fn neko_dir_for(dir: &Path, config: &AppConfig) -> Option<PathBuf> {
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = sha256_hex(key.to_string_lossy().as_bytes());
    Some(config.cache_root()?.join(hash))
}

/// キャッシュディレクトリ以下の invalid/ に置くマーカーファイルのパスを返す。
pub fn invalid_marker_path(neko_dir: &Path, archive_path: &Path) -> PathBuf {
    let stem = archive_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    let ext = archive_path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    neko_dir.join("invalid").join(format!("{stem}.{ext}.invalid"))
}

/// 無効マーカーを作成する（invalid/ ディレクトリがなければ作る）。
pub fn mark_invalid(neko_dir: &Path, archive_path: &Path) -> bool {
    let dir = neko_dir.join("invalid");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    std::fs::File::create(invalid_marker_path(neko_dir, archive_path)).is_ok()
}

/// マーカーが存在し、かつZIPがマーカーより古い（差し替えられていない）場合 true。
/// ZIPが差し替えられていた場合（mtime比較）はマーカーを削除して false を返す。
pub fn is_invalid_and_current(neko_dir: &Path, archive_path: &Path) -> bool {
    let marker = invalid_marker_path(neko_dir, archive_path);
    let Ok(marker_meta) = std::fs::metadata(&marker) else { return false };
    let Ok(archive_meta) = std::fs::metadata(archive_path) else {
        let _ = std::fs::remove_file(&marker);
        return false;
    };
    if let (Ok(m), Ok(a)) = (marker_meta.modified(), archive_meta.modified()) {
        if a > m {
            let _ = std::fs::remove_file(&marker);
            return false;
        }
    }
    true
}

/// キャッシュディレクトリ以下の thumbs/ パスを返し、存在しなければ作成する。
pub fn ensure_thumbs_dir(neko_dir: &Path) -> Option<PathBuf> {
    let thumbs = neko_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs).ok()?;
    Some(thumbs)
}

/// アーカイブパスからサムネイルキャッシュファイル名（拡張子 .jpg）を生成する。
/// 元ファイルの拡張子を含めることで同名・異種ファイル間の衝突を防ぐ。
pub fn thumb_filename(archive_path: &Path) -> String {
    let stem = archive_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let ext = archive_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");
    format!("{stem}.{ext}.jpg")
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
