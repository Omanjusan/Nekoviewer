//! フォルダ（仮想アーカイブ）バックエンド。アーカイブファイルを含まないフォルダ直下の
//! 生画像群を、1つの複数ページアーカイブのように扱う（「フォルダ本アクセス」機能）。
//! zip等と異なりエントリはファイルシステム上に直接存在するファイルであり、
//! `entry_name` はそのファイルの絶対パス文字列を持つ。

use std::path::Path;

use super::{ArchiveMemoryEstimate, ArchiveOpenProgress, EntryEstimate, ImageEntry, ProgressCallback};

/// フォルダ直下の生画像を列挙する。進捗通知版。
pub(crate) fn list_images_folder_with_progress(path: &Path, on_progress: &mut ProgressCallback) -> Option<Vec<ImageEntry>> {
    let files = crate::fs::dir::list_raw_images(path);
    let total = files.len();
    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for (i, file_path) in files.into_iter().enumerate() {
        if !on_progress(ArchiveOpenProgress::Determinate { current: i, total }) {
            return None;
        }
        let display_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let entry_name = file_path.to_string_lossy().to_string();
        let date_key = std::fs::metadata(&file_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| super::unix_secs_to_date_key(d.as_secs()))
            .unwrap_or(0);
        pairs.push((display_name, entry_name, date_key));
    }
    Some(super::finalize_entries(pairs))
}

/// フォルダ版のメモリ見積もり。zip版(`estimate_archive_memory_zip`)と同じサンプリング方式だが、
/// エントリの生バイトは ZipArchive 経由ではなくファイルシステムから直接読む
/// （`entry_name` がそのままファイルパスであるため）。
pub(crate) fn estimate_archive_memory_folder(
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
    max_decode_edge: u32,
) -> ArchiveMemoryEstimate {
    let mut sample_bytes: Vec<usize> = Vec::new();
    for idx in super::select_sample_indices(entries.len()) {
        let Ok(buf) = std::fs::read(&entries[idx].entry_name) else { continue };
        match super::estimate_bytes_for_entry(&buf, &entries[idx].display_name, budget_bytes, ring_bounds, max_decode_edge) {
            Some(EntryEstimate::OverBudget) => return ArchiveMemoryEstimate::OverBudget,
            Some(EntryEstimate::Bytes(n)) => sample_bytes.push(n),
            None => {} // 読み込み/デコード失敗エントリはサンプルから除外
        }
    }

    let Some(avg) = super::average(&sample_bytes) else { return ArchiveMemoryEstimate::Ok };
    let window = crate::cache::PREFETCH_WINDOW.min(entries.len());
    let resident_estimate = avg.saturating_mul(window);
    if resident_estimate > budget_bytes {
        ArchiveMemoryEstimate::OverBudget
    } else {
        ArchiveMemoryEstimate::Ok
    }
}

/// フォルダ内の先頭画像1枚をデコードして返す（サムネイル用）。
pub(crate) fn load_first_image_folder(path: &Path) -> Option<image::DynamicImage> {
    let first = crate::fs::dir::list_raw_images(path).into_iter().next()?;
    let buf = std::fs::read(&first).ok()?;
    super::decode_image_bytes(&buf, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_file(dir: &Path, name: &str, content: &[u8]) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(content).unwrap();
    }

    fn temp_dir(suffix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nekoviewer_folder_test_{}_{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_folder_images_sorted_by_name_and_skips_non_images() {
        let dir = temp_dir("list");
        write_temp_file(&dir, "b.jpg", b"dummy");
        write_temp_file(&dir, "a.png", b"dummy");
        write_temp_file(&dir, "readme.txt", b"not an image");

        let entries = list_images_folder_with_progress(&dir, &mut |_| true).unwrap();
        assert_eq!(entries.len(), 2, "非画像ファイルは対象外");
        assert_eq!(entries[0].display_name, "a.png");
        assert_eq!(entries[1].display_name, "b.jpg");
        // entry_name はファイルの絶対パス文字列で、そのまま読み込みに使える
        assert!(std::fs::read(&entries[0].entry_name).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn on_progress_returning_false_cancels_the_listing() {
        let dir = temp_dir("cancel");
        write_temp_file(&dir, "a.png", b"dummy");
        write_temp_file(&dir, "b.png", b"dummy");

        let result = list_images_folder_with_progress(&dir, &mut |_| false);
        assert!(result.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
