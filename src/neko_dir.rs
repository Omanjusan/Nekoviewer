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
