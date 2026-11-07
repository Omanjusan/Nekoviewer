//! ZIP/CBZ バックエンド。エントリ列挙・展開・先頭画像の順読み最適化を担う。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decode::decode_image;
use super::detect::is_image_entry_raw;
use super::{ArchiveMemoryEstimate, ArchiveOpenProgress, EntryEstimate, ImageEntry, ProgressCallback};

/// ZIPエントリのファイル名をUTF-8文字列にデコードする。
/// UTF-8として無効な場合はShift-JISとして試みる（Windowsで作成された日本語ZIPに対応）。
pub(super) fn decode_zip_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
    decoded.into_owned()
}

/// アーカイブ(ZIP)内の全画像をフラット化して返す。進捗通知版。
/// エントリ数は`archive.len()`で開始前に確定するため`ArchiveOpenProgress::Determinate`で件数ベースの進捗を通知できる。
pub(crate) fn list_images_zip_with_progress(path: &Path, on_progress: &mut ProgressCallback) -> Option<Vec<ImageEntry>> {
    let Ok(file) = std::fs::File::open(path) else {
        return Some(Vec::new());
    };
    let Ok(mut archive) = ::zip::ZipArchive::new(file) else {
        return Some(Vec::new());
    };

    let total = archive.len();
    let text_opts = super::text_page::options();
    // (display_name, entry_name, date_key)
    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for i in 0..total {
        if !on_progress(ArchiveOpenProgress::Determinate { current: i, total }) {
            return None;
        }
        let Ok(entry) = archive.by_index(i) else { continue };
        if entry.is_dir() {
            continue;
        }
        let raw = entry.name_raw().to_vec();
        let is_text = text_opts.enabled && super::text_page::is_text_entry_raw(&raw);
        if !is_image_entry_raw(&raw) && !is_text {
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
        if is_text {
            // TXTは画像ページ。分割モードのときだけ本文を読んでページ数を決める（切り詰めなら1ページで読まない）。
            let pages = if text_opts.split {
                let declared_larger = entry.size() > text_opts.read_limit as u64;
                match read_limited_entry(entry, text_opts.read_limit) {
                    Some(body) => {
                        let truncated = declared_larger || body.len() >= text_opts.read_limit;
                        super::text_page::count_pages(&body, truncated, &text_opts)
                    }
                    None => 1,
                }
            } else {
                1
            };
            for page in 0..pages {
                pairs.push((
                    super::text_page::page_display_name(&display_name, page),
                    super::text_page::page_entry_name(&entry_name, page),
                    date_key,
                ));
            }
        } else {
            pairs.push((display_name, entry_name, date_key));
        }
    }

    Some(super::finalize_entries(pairs))
}

/// 開済みの ZipArchive からエントリの生バイトと表示名を返す（デコードしない）
pub fn load_bytes_from_archive<R: Read + Seek>(
    archive: &mut ::zip::ZipArchive<R>,
    entry_name: &str,
) -> Option<(Vec<u8>, String)> {
    // TXTページ（分割モードの仮想名を含む）は、元のエントリを上限まで読み、目印つきのペイロードにして返す。
    let (real_name, page) = super::text_page::split_entry_name(entry_name);
    if super::text_page::is_text_entry_raw(real_name.as_bytes()) {
        let limit = super::text_page::options().read_limit;
        let entry = archive.by_name(real_name).ok()?;
        let display_name = decode_zip_name(entry.name_raw());
        let declared_larger = entry.size() > limit as u64;
        let body = read_limited_entry(entry, limit)?;
        return Some((super::text_page::wrap_entry(body, page, declared_larger), display_name));
    }
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    let display_name = decode_zip_name(entry.name_raw());
    Some((buf, display_name))
}

/// エントリを `limit` バイトまで読む。宣言サイズは信用せず、展開後のバイト数で打ち切る。
fn read_limited_entry<R: Read>(entry: R, limit: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    entry.take(limit as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
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
    // 画像が1枚も無いときは、TXTページの先頭ページをサムネにする（TXTだけのアーカイブ用）。
    if super::text_page::options().enabled {
        for i in 0..archive.len() {
            let Ok(entry) = archive.by_index(i) else { continue };
            if entry.is_dir() || !super::text_page::is_text_entry_raw(entry.name_raw()) {
                continue;
            }
            let limit = super::text_page::options().read_limit;
            let declared_larger = entry.size() > limit as u64;
            let Some(body) = read_limited_entry(entry, limit) else { continue };
            return decode_image(&super::text_page::wrap_entry(body, 0, declared_larger));
        }
    }
    None
}

#[cfg(test)]
mod text_page_tests {
    use super::*;
    use crate::fs::archive::text_page::{self, TextPageOptions};
    use std::io::Write;

    fn jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    fn make_zip(tag: &str, files: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("nekoviewer_txtpage_{}_{tag}.zip", std::process::id()));
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (n, d) in files {
            w.start_file(*n, ::zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
        path
    }

    /// 設定は全体で共有なので、設定を変えるテストは1つにまとめて直列に走らせ、最後に既定へ戻す。
    #[test]
    fn txt_entries_become_pages_in_listing_loading_and_thumbnail() {
        let long_text = (0..400).map(|i| format!("行{i} のテキストです")).collect::<Vec<_>>().join("\n");
        let path = make_zip("main", &[
            ("0001.jpg", jpeg()),
            ("note.TXT", long_text.clone().into_bytes()),
            ("zzz.bin", b"binary".to_vec()),
        ]);

        // 既定（切り詰め・有効）: txt は1エントリ。
        text_page::set_options(TextPageOptions::default());
        let entries = list_images_zip_with_progress(&path, &mut |_| true).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, ["0001.jpg", "note.TXT"]);
        let txt = entries.iter().find(|e| text_page::is_text_entry_name(&e.entry_name)).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = ::zip::ZipArchive::new(file).unwrap();
        let (bytes, display) = load_bytes_from_archive(&mut archive, &txt.entry_name).unwrap();
        assert_eq!(display, "note.TXT");
        assert!(text_page::is_payload(&bytes));
        let img = crate::fs::archive::decode_image_bytes(&bytes, true).unwrap();
        assert_eq!(img.width(), text_page::PAGE_W);
        assert!(img.height() <= 4000);
        // ふつうの画像はこれまでどおり。
        let jpg = entries.iter().find(|e| e.display_name == "0001.jpg").unwrap();
        let (jb, _) = load_bytes_from_archive(&mut archive, &jpg.entry_name).unwrap();
        assert!(!text_page::is_payload(&jb));

        // 分割: 同じTXTが複数ページになり、各ページが読める。
        text_page::set_options(TextPageOptions { split: true, height_limit: 1000, ..Default::default() });
        let entries = list_images_zip_with_progress(&path, &mut |_| true).unwrap();
        let txt_pages: Vec<_> = entries.iter().filter(|e| text_page::is_text_entry_name(&e.entry_name)).collect();
        assert!(txt_pages.len() > 3, "400行は1000pxの高さでは複数ページ: {}", txt_pages.len());
        assert_eq!(txt_pages[0].display_name, "note.TXT");
        assert_eq!(txt_pages[1].display_name, "note_p02.TXT");
        for e in &txt_pages {
            let (b, _) = load_bytes_from_archive(&mut archive, &e.entry_name).unwrap();
            assert!(crate::fs::archive::decode_image_bytes(&b, true).is_some(), "{}", e.display_name);
        }

        // 無効化: txt は一覧に出ない。
        text_page::set_options(TextPageOptions { enabled: false, ..Default::default() });
        let entries = list_images_zip_with_progress(&path, &mut |_| true).unwrap();
        assert_eq!(entries.len(), 1);

        // 読み込み上限: 上限を超える本文は途中までしか読まない。
        let huge = vec![b'a'; 3 * 1024 * 1024];
        let big = make_zip("huge", &[("big.txt", huge)]);
        text_page::set_options(TextPageOptions { read_limit: 100 * 1024, ..Default::default() });
        let mut archive2 = ::zip::ZipArchive::new(std::fs::File::open(&big).unwrap()).unwrap();
        let (b, _) = load_bytes_from_archive(&mut archive2, "big.txt").unwrap();
        assert_eq!(b.len(), 17 + 100 * 1024, "本文は上限まで（ヘッダー17バイト＋100KB）");
        assert_eq!(b[16] & 2, 2, "上限で切れたフラグ");

        // TXTだけのアーカイブは、サムネ用の先頭画像としてTXTの1ページ目を返す。
        text_page::set_options(TextPageOptions::default());
        let only = make_zip("only", &[("a.txt", b"hello".to_vec())]);
        assert!(load_first_image_via_archive(&only).is_some());

        text_page::set_options(TextPageOptions::default());
        for p in [path, big, only] {
            let _ = std::fs::remove_file(p);
        }
    }
}
