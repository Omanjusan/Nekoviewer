//! TAR/CBT バックエンド。raw tar と gzip 圧縮(tar.gz/tgz)に対応する。
//! tar は中央ディレクトリを持たない逐次ストリームでランダムアクセスできないため、
//! 7z と同じく「開いた時点で全画像を一括展開してマップに保持する」ソリッド扱いとする。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decode::decode_image;
use super::detect::is_image_entry_raw;
use super::{ArchiveMemoryEstimate, EntryEstimate, ImageEntry};

/// tar ファイルを開き、gzip 圧縮なら透過的に解凍したリーダを返す。
/// 先頭2バイトの gzip マジックで判定し、raw tar はそのまま返す。
///
/// 将来の追加圧縮（default-off feature の frame）:
///   - `tar-zstd`: 先頭 `28 B5 2F FD` を見て `zstd::Decoder` で包む
///   - `tar-xz`  : 先頭 `FD 37 7A 58 5A 00` を見て liblzma デコーダで包む
/// いずれも C 依存を引き込むため、対応 dep とデコード経路は feature 有効時のみ追加する。
fn open_reader(path: &Path) -> Option<Box<dyn Read>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    if n == 2 && magic == [0x1F, 0x8B] {
        Some(Box::new(flate2::read::GzDecoder::new(file)))
    } else {
        Some(Box::new(file))
    }
}

/// tar のヘッダを順に読み、画像エントリを一覧化する。
/// tar は中央ディレクトリを持たないため全体を走査する（gzip の場合は解凍を伴う）。
pub(crate) fn list_images_tar(path: &Path) -> Vec<ImageEntry> {
    let Some(reader) = open_reader(path) else {
        return Vec::new();
    };
    let mut archive = tar::Archive::new(reader);
    let Ok(entries) = archive.entries() else {
        return Vec::new();
    };

    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let name_bytes = entry.path_bytes().into_owned();
        if !is_image_entry_raw(&name_bytes) {
            continue;
        }
        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        let date_key = entry.header().mtime().map_or(0, super::unix_secs_to_date_key);
        pairs.push((name.clone(), name, date_key));
    }

    super::finalize_entries(pairs)
}

/// tar(raw/gzip)を開き、画像エントリを一括展開して `entry_name -> 生バイト列` の対応表を返す。
/// 以降のページ送りはこの表を引くだけで済み、再展開は発生しない（7z と同じ方式）。
pub fn extract_all_images_tar<R: Read>(source: R) -> HashMap<String, Vec<u8>> {
    let mut archive = tar::Archive::new(source);
    let mut out = HashMap::new();
    let Ok(entries) = archive.entries() else {
        return out;
    };
    for entry in entries {
        let Ok(mut entry) = entry else { continue };
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let name_bytes = entry.path_bytes().into_owned();
        if !is_image_entry_raw(&name_bytes) {
            continue;
        }
        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        let mut buf = Vec::with_capacity(entry.header().size().unwrap_or(0) as usize);
        if entry.read_to_end(&mut buf).is_ok() {
            out.insert(name, buf);
        }
    }
    out
}

/// ディスク上の tar ファイル(raw/gzip)を開いて `extract_all_images_tar` を実行する
pub fn extract_all_images_tar_path(path: &Path) -> HashMap<String, Vec<u8>> {
    let Some(reader) = open_reader(path) else {
        return HashMap::new();
    };
    extract_all_images_tar(reader)
}

/// tar の先頭画像1枚をデコードして返す（サムネイル用）。
/// 逐次ストリームを走査し、最初にデコード成功した画像を採用する。
pub(crate) fn load_first_image_tar(path: &Path) -> Option<image::DynamicImage> {
    let reader = open_reader(path)?;
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().ok()?;
    for entry in entries {
        let Ok(mut entry) = entry else { continue };
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let name_bytes = entry.path_bytes().into_owned();
        if !is_image_entry_raw(&name_bytes) {
            continue;
        }
        let mut buf = Vec::with_capacity(entry.header().size().unwrap_or(0) as usize);
        if entry.read_to_end(&mut buf).is_ok() {
            if let Some(img) = decode_image(&buf) {
                return Some(img);
            }
        }
    }
    None
}

/// tar版のメモリ見積もり。ソリッド扱いのため7z同様、一括展開結果を使って全件厳密に計算する。
pub(crate) fn estimate_archive_memory_tar(
    path: &Path,
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
) -> ArchiveMemoryEstimate {
    let map = extract_all_images_tar_path(path);
    if map.is_empty() {
        return ArchiveMemoryEstimate::Ok;
    }

    let mut total: usize = 0;
    for entry in entries {
        let Some(buf) = map.get(&entry.entry_name) else { continue };
        match super::estimate_bytes_for_entry(buf, &entry.display_name, budget_bytes, ring_bounds) {
            Some(EntryEstimate::OverBudget) => return ArchiveMemoryEstimate::OverBudget,
            Some(EntryEstimate::Bytes(n)) => {
                total = total.saturating_add(n);
                if total > budget_bytes {
                    return ArchiveMemoryEstimate::OverBudget;
                }
            }
            None => {} // 読み込み/デコード失敗エントリは合計から除外
        }
    }
    ArchiveMemoryEstimate::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MB: usize = 1024 * 1024;
    const TEST_RING_BOUNDS: (usize, usize) = (4, 32);

    fn encode_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::new(w, h);
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// 指定寸法のPNGを格納した tar を組み立てる（gzip=true なら tar.gz）。
    fn build_tar(dims: &[(u32, u32)], gzip: bool) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let ext = if gzip { "tar.gz" } else { "tar" };
        let path = std::env::temp_dir()
            .join(format!("nekoviewer_test_tar_{}_{n}.{ext}", std::process::id()));

        let file = std::fs::File::create(&path).unwrap();
        let write_entries = |w: &mut dyn Write| {
            let mut builder = tar::Builder::new(w);
            for (i, (width, height)) in dims.iter().enumerate() {
                let data = encode_png(*width, *height);
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("page{:03}.png", i + 1), data.as_slice())
                    .unwrap();
            }
            builder.finish().unwrap();
        };
        if gzip {
            let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            write_entries(&mut enc);
            enc.finish().unwrap();
        } else {
            let mut file = file;
            write_entries(&mut file);
        }
        path
    }

    #[test]
    fn tar_raw_list_and_extract() {
        let path = build_tar(&[(10, 10), (12, 12), (8, 8)], false);
        assert_eq!(super::super::detect_format(&path), super::super::ArchiveFormat::Tar);

        let entries = list_images_tar(&path);
        assert_eq!(entries.len(), 3, "tarの画像エントリ数が想定と異なる");
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "display_name がソートされていない");

        let map = extract_all_images_tar_path(&path);
        assert_eq!(map.len(), 3, "一括展開後のエントリ数が一覧と一致しない");
        for e in &entries {
            let buf = map.get(&e.entry_name).expect("展開済みマップにエントリが無い");
            assert!(decode_image(buf).is_some(), "{} のデコードに失敗", e.display_name);
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tar_gzip_list_extract_and_first_image() {
        let path = build_tar(&[(16, 16), (10, 10)], true);
        assert_eq!(super::super::detect_format(&path), super::super::ArchiveFormat::Tar);

        let entries = list_images_tar(&path);
        assert_eq!(entries.len(), 2, "tar.gzの画像エントリ数が想定と異なる");

        let map = extract_all_images_tar_path(&path);
        assert_eq!(map.len(), 2);

        assert!(load_first_image_tar(&path).is_some(), "tar.gzの先頭画像の読み込みに失敗");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tar_estimate_within_and_over_budget() {
        // 10x10 RGBA = 400byte/枚 × 3 = 1200byte。10MB予算なら収まる。
        let path = build_tar(&[(10, 10), (10, 10), (10, 10)], false);
        let entries = list_images_tar(&path);
        assert_eq!(
            estimate_archive_memory_tar(&path, &entries, 10 * MB, TEST_RING_BOUNDS),
            ArchiveMemoryEstimate::Ok
        );
        // 1枚(400byte)未満の予算 → 単体超過で OverBudget
        assert_eq!(
            estimate_archive_memory_tar(&path, &entries, 100, TEST_RING_BOUNDS),
            ArchiveMemoryEstimate::OverBudget
        );
        std::fs::remove_file(&path).ok();
    }
}
