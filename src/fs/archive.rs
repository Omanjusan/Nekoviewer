use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// ZIPエントリのファイル名をUTF-8文字列にデコードする。
/// UTF-8として無効な場合はShift-JISとして試みる（Windowsで作成された日本語ZIPに対応）。
fn decode_zip_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
    decoded.into_owned()
}

/// 生バイト列で拡張子を確認する（Shift-JIS等でもASCII拡張子は正しく判定できる）
fn is_image_entry_raw(raw: &[u8]) -> bool {
    let lower: Vec<u8> = raw.iter().map(|b| b.to_ascii_lowercase()).collect();
    IMAGE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(format!(".{ext}").as_bytes()))
}

fn decode_image(buf: &[u8], entry_name: &str) -> Option<image::DynamicImage> {
    decode_image_bytes(buf, entry_name)
}

/// バイト列から静止画をデコードする（外部から呼び出し可能）
pub fn decode_image_bytes(buf: &[u8], entry_name: &str) -> Option<image::DynamicImage> {
    let lower = entry_name.to_lowercase();
    if lower.ends_with(".webp") {
        webp::Decoder::new(buf).decode().map(|w| w.to_image())
    } else if lower.ends_with(".avif") {
        decode_avif(buf)
    } else {
        image::load_from_memory(buf).ok()
    }
}

fn decode_avif(buf: &[u8]) -> Option<image::DynamicImage> {
    eprintln!("[avif] buf.len()={}, is_avif={}", buf.len(), libavif::is_avif(buf));
    let rgb = match libavif::decode_rgb(buf) {
        Ok(r) => r,
        Err(e) => {
            // libavif 1.0.4 error codes: 9=BMFF_PARSE_FAILED, 15=NO_CODEC_AVAILABLE
            eprintln!("[avif] decode_rgb error: {:?}", e);
            return None;
        }
    };
    let w = rgb.width();
    let h = rgb.height();
    let pixels = rgb.as_slice().to_vec();
    match image::RgbaImage::from_raw(w, h, pixels) {
        Some(img) => Some(image::DynamicImage::ImageRgba8(img)),
        None => {
            eprintln!("[avif] RgbaImage::from_raw returned None");
            None
        }
    }
}

/// 開済みの ZipArchive からエントリの生バイトと表示名を返す（デコードしない）
pub fn load_bytes_from_archive(
    archive: &mut zip::ZipArchive<std::fs::File>,
    entry_name: &str,
) -> Option<(Vec<u8>, String)> {
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    let display_name = decode_zip_name(entry.name_raw());
    Some((buf, display_name))
}

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "avif"];

/// ディレクトリ直下の生画像ファイルかどうかを拡張子で判定する
pub fn is_supported_image_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    IMAGE_EXTENSIONS
        .iter()
        .any(|e| Some(e.to_string()) == ext)
}

/// ZIP/CBZ 内の画像エントリ
pub struct ImageEntry {
    /// ソート・表示用のファイル名（衝突時は "stem_01.ext" 形式）
    pub display_name: String,
    /// ZIP 内でのフルパス（読み込みに使用）
    pub entry_name: String,
    /// 日付ソートキー（年月日時分秒を1桁ずつパックした u64）
    pub date_key: u64,
}

/// ZIP/CBZ 内の全画像をフラット化して返す。
/// ディレクトリ構造を無視し、ファイル名の衝突は "stem_01.ext" 形式で回避する。
pub fn list_images(path: &Path) -> Vec<ImageEntry> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
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

    // ファイル名優先、同名はentry_nameで安定ソート
    pairs.sort_by(|(da, ea, _), (db, eb, _)| {
        basename(da).cmp(basename(db)).then(ea.cmp(eb))
    });

    // 衝突検出: 同じファイル名が2回目以降なら "stem_01.ext" 形式を付与
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut entries: Vec<ImageEntry> = pairs
        .into_iter()
        .map(|(display_name, entry_name, date_key)| {
            let base = basename(&display_name).to_string();
            let count = seen.entry(base.clone()).or_insert(0);
            let final_display = if *count == 0 {
                base.clone()
            } else {
                collision_name(&base, *count)
            };
            *count += 1;
            ImageEntry { display_name: final_display, entry_name, date_key }
        })
        .collect();

    entries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    entries
}

/// ZIP/CBZ から指定エントリの画像をデコードして返す
pub fn load_image(path: &Path, entry_name: &str) -> Option<image::DynamicImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    load_image_from_archive(&mut archive, entry_name)
}

/// 開済みの ZipArchive から指定エントリの画像をデコードして返す（キープオープン用）
pub fn load_image_from_archive(
    archive: &mut zip::ZipArchive<std::fs::File>,
    entry_name: &str,
) -> Option<image::DynamicImage> {
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut buf = Vec::new();
    let t0 = std::time::Instant::now();
    entry.read_to_end(&mut buf).ok()?;
    let t_read = t0.elapsed();
    let t1 = std::time::Instant::now();
    let display_name = decode_zip_name(entry.name_raw());
    let img = decode_image(&buf, &display_name)?;
    let t_decode = t1.elapsed();
    eprintln!(
        "[perf/load] zip_read={:.1}ms decode={:.1}ms  entry={}",
        t_read.as_secs_f64() * 1000.0,
        t_decode.as_secs_f64() * 1000.0,
        entry_name,
    );
    Some(img)
}

/// ZIP/CBZ の先頭画像1枚をデコードして返す（サムネイル用）。
/// まず Local File Header を先頭から順読みして試みる（ネットワーク帯域節約）。
/// Data Descriptor フラグ等で順読み不可の場合は ZipArchive 経由にフォールバックする。
pub fn load_first_image(path: &Path) -> Option<image::DynamicImage> {
    load_first_image_sequential(path).or_else(|| load_first_image_via_archive(path))
}

/// Local File Header を先頭から順読みする（セントラルディレクトリ・末尾シーク不要）。
/// Data Descriptor フラグが立っているエントリに到達したら None を返す。
fn load_first_image_sequential(path: &Path) -> Option<image::DynamicImage> {
    const LFH_SIG: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
    const DATA_DESCRIPTOR_BIT: u16 = 1 << 3;

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

        // Data Descriptor があるとサイズが LFH に入っていないためスキップ不可
        if has_dd {
            return None;
        }

        if is_image_entry_raw(&fname) && comp_size > 0 {
            let mut comp = vec![0u8; comp_size as usize];
            f.read_exact(&mut comp).ok()?;
            let raw = decompress_entry(method, &comp)?;
            let name = decode_zip_name(&fname);
            if let Some(img) = decode_image(&raw, &name) {
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
fn load_first_image_via_archive(path: &Path) -> Option<image::DynamicImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
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
        let display_name = decode_zip_name(&raw);
        let mut buf = Vec::new();
        let t0 = std::time::Instant::now();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        let t_read = t0.elapsed();
        let t1 = std::time::Instant::now();
        if let Some(img) = decode_image(&buf, &display_name) {
            let t_decode = t1.elapsed();
            eprintln!(
                "[perf/load] zip_read={:.1}ms decode={:.1}ms  entry={}",
                t_read.as_secs_f64() * 1000.0,
                t_decode.as_secs_f64() * 1000.0,
                &display_name,
            );
            return Some(img);
        }
    }
    None
}

fn basename(entry_name: &str) -> &str {
    entry_name
        .rfind('/')
        .map(|i| &entry_name[i + 1..])
        .unwrap_or(entry_name)
}

/// "stem_01.ext" 形式の衝突回避名を生成する
fn collision_name(base: &str, count: usize) -> String {
    if let Some(dot) = base.rfind('.') {
        let (stem, ext) = base.split_at(dot);
        format!("{stem}_{count:02}{ext}")
    } else {
        format!("{base}_{count:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_zip() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/testarchive.zip")
    }

    #[test]
    fn test_list_images_count() {
        let entries = list_images(&test_zip());
        assert!(!entries.is_empty(), "画像エントリが0件");
        let has_thumbs_db = entries.iter().any(|e| e.display_name == "Thumbs.db");
        assert!(!has_thumbs_db, "Thumbs.db が除外されていない");
    }

    #[test]
    fn test_list_images_sorted() {
        let entries = list_images(&test_zip());
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "display_name がソートされていない");
    }

    #[test]
    fn test_load_first_image_returns_some() {
        let img = load_first_image(&test_zip());
        assert!(img.is_some(), "先頭画像の読み込みに失敗");
    }

    #[test]
    fn test_load_image_by_entry_name() {
        let entries = list_images(&test_zip());
        let first = &entries[0];
        let img = load_image(&test_zip(), &first.entry_name);
        assert!(img.is_some(), "entry_name で画像を読み込めない");
    }
}
