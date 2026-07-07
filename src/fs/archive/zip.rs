//! ZIP/CBZ バックエンド。エントリ列挙・展開・先頭画像の順読み最適化を担う。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decode::decode_image;
use super::detect::is_image_entry_raw;
use super::{ArchiveMemoryEstimate, EntryEstimate, ImageEntry};

/// ZIPエントリのファイル名をUTF-8文字列にデコードする。
/// UTF-8として無効な場合はShift-JISとして試みる（Windowsで作成された日本語ZIPに対応）。
fn decode_zip_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
    decoded.into_owned()
}

pub(crate) fn list_images_zip(path: &Path) -> Vec<ImageEntry> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut archive) = ::zip::ZipArchive::new(file) else {
        return Vec::new();
    };

    // (display_name, entry_name, date_key)
    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else { continue };
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name_raw().to_vec();
        if !is_image_entry_raw(&raw) {
            continue;
        }
        let display_name = decode_zip_name(&raw);
        let entry_name = entry.name().to_string();
        let date_key = entry.last_modified().map_or(0u64, |dt| {
            (dt.year() as u64) * 10_000_000_000
                + (dt.month() as u64) * 100_000_000
                + (dt.day() as u64) * 1_000_000
                + (dt.hour() as u64) * 10_000
                + (dt.minute() as u64) * 100
                + (dt.second() as u64)
        });
        pairs.push((display_name, entry_name, date_key));
    }

    super::finalize_entries(pairs)
}

/// 開済みの ZipArchive からエントリの生バイトと表示名を返す（デコードしない）
pub fn load_bytes_from_archive<R: Read + Seek>(
    archive: &mut ::zip::ZipArchive<R>,
    entry_name: &str,
) -> Option<(Vec<u8>, String)> {
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    let display_name = decode_zip_name(entry.name_raw());
    Some((buf, display_name))
}

/// フェーズ2: 開済みZIPアーカイブから1エントリを展開し、サイズを見積もる。
/// エントリの読み込み自体に失敗した場合は None（このサンプルは無視する）。
pub(crate) fn estimate_entry_bytes<R: Read + Seek>(
    archive: &mut ::zip::ZipArchive<R>,
    entry_name: &str,
    budget_bytes: usize,
    ring_bounds: (usize, usize),
    max_decode_edge: u32,
) -> Option<EntryEstimate> {
    let (buf, display_name) = load_bytes_from_archive(archive, entry_name)?;
    super::estimate_bytes_for_entry(&buf, &display_name, budget_bytes, ring_bounds, max_decode_edge)
}

/// ZIP版のメモリ見積もり。サンプリングした平均 × 先読みウィンドウ幅で
/// 「閲覧中に同時常駐しうる量」を推定する（全ページ同時常駐は仮定しない）。
pub(crate) fn estimate_archive_memory_zip(
    path: &Path,
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
    max_decode_edge: u32,
) -> ArchiveMemoryEstimate {
    let Ok(file) = std::fs::File::open(path) else { return ArchiveMemoryEstimate::Ok };
    let Ok(mut archive) = ::zip::ZipArchive::new(file) else { return ArchiveMemoryEstimate::Ok };

    let mut sample_bytes: Vec<usize> = Vec::new();
    for idx in super::select_sample_indices(entries.len()) {
        match estimate_entry_bytes(&mut archive, &entries[idx].entry_name, budget_bytes, ring_bounds, max_decode_edge) {
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

/// Local File Header を先頭から順読みする（セントラルディレクトリ・末尾シーク不要）。
/// Data Descriptor フラグ付き DEFLATE エントリはストリーム展開で処理する。
pub(crate) fn load_first_image_sequential(path: &Path) -> Option<image::DynamicImage> {
    const LFH_SIG: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
    const DATA_DESCRIPTOR_BIT: u16 = 1 << 3;
    const METHOD_DEFLATE: u16 = 8;

    let mut f = std::fs::File::open(path).ok()?;
    loop {
        let mut sig = [0u8; 4];
        f.read_exact(&mut sig).ok()?;
        if sig != LFH_SIG {
            return None;
        }

        let _version  = read_u16(&mut f)?;
        let flags     = read_u16(&mut f)?;
        let method    = read_u16(&mut f)?;
        let _mod_time = read_u16(&mut f)?;
        let _mod_date = read_u16(&mut f)?;
        let _crc32    = read_u32(&mut f)?;
        let comp_size = read_u32(&mut f)?;
        let _uncomp   = read_u32(&mut f)?;
        let fname_len = read_u16(&mut f)? as usize;
        let extra_len = read_u16(&mut f)? as usize;

        let mut fname = vec![0u8; fname_len];
        f.read_exact(&mut fname).ok()?;
        f.seek(SeekFrom::Current(extra_len as i64)).ok()?;

        let has_dd = flags & DATA_DESCRIPTOR_BIT != 0;

        if has_dd {
            // DEFLATE + 画像エントリなら末尾シークなしでストリーム展開して返す。
            // DeflateDecoder は DEFLATE 終端ビットで自動停止するため comp_size 不要。
            if method == METHOD_DEFLATE && is_image_entry_raw(&fname) {
                let mut raw = Vec::new();
                let mut dec = flate2::read::DeflateDecoder::new(&mut f);
                if dec.read_to_end(&mut raw).is_ok() {
                    if let Some(img) = decode_image(&raw) {
                        return Some(img);
                    }
                }
            }
            // 非 DEFLATE・非画像・デコード失敗はフォールバックへ
            return None;
        }

        if is_image_entry_raw(&fname) && comp_size > 0 {
            let mut comp = vec![0u8; comp_size as usize];
            f.read_exact(&mut comp).ok()?;
            let raw = decompress_entry(method, &comp)?;
            if let Some(img) = decode_image(&raw) {
                return Some(img);
            }
            // デコード失敗 → 次のエントリへ（位置は comp_size 分進んでいる）
        } else {
            // 画像でない or 空エントリ: comp_size 分シークしてスキップ
            f.seek(SeekFrom::Current(comp_size as i64)).ok()?;
        }
    }
}

fn decompress_entry(method: u16, data: &[u8]) -> Option<Vec<u8>> {
    match method {
        0 => Some(data.to_vec()),
        8 => {
            use flate2::read::DeflateDecoder;
            let mut dec = DeflateDecoder::new(data);
            let mut out = Vec::new();
            dec.read_to_end(&mut out).ok()?;
            Some(out)
        }
        _ => None,
    }
}

fn read_u16<R: Read>(r: &mut R) -> Option<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).ok()?;
    Some(u16::from_le_bytes(buf))
}

fn read_u32<R: Read>(r: &mut R) -> Option<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).ok()?;
    Some(u32::from_le_bytes(buf))
}

/// フォールバック: ZipArchive 経由（末尾シークあり）
pub(crate) fn load_first_image_via_archive(path: &Path) -> Option<image::DynamicImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = ::zip::ZipArchive::new(file).ok()?;
    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name_raw().to_vec();
        if !is_image_entry_raw(&raw) {
            continue;
        }
        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        if let Some(img) = decode_image(&buf) {
            return Some(img);
        }
    }
    None
}
