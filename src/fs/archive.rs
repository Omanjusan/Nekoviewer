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
        // 静止画デコードを先に試みる
        if let Some(img) = webp::Decoder::new(buf).decode().map(|w| w.to_image()) {
            return Some(img);
        }
        // アニメーション WebP: 最初のフレームを静止画として返す
        if let Ok(anim) = webp::AnimDecoder::new(buf).decode() {
            if let Some(frame) = anim.into_iter().next() {
                let (w, h) = (frame.width(), frame.height());
                let raw = frame.get_image();
                let rgba = match frame.get_layout() {
                    webp::PixelLayout::Rgba => image::RgbaImage::from_raw(w, h, raw.to_vec()),
                    webp::PixelLayout::Rgb => {
                        let data: Vec<u8> = raw
                            .chunks_exact(3)
                            .flat_map(|p| [p[0], p[1], p[2], 255u8])
                            .collect();
                        image::RgbaImage::from_raw(w, h, data)
                    }
                };
                return rgba.map(image::DynamicImage::ImageRgba8);
            }
        }
        None
    } else if lower.ends_with(".avif") {
        decode_avif(buf)
    } else {
        image::load_from_memory(buf).ok()
    }
}

fn decode_avif(buf: &[u8]) -> Option<image::DynamicImage> {
    let rgb = match libavif::decode_rgb(buf) {
        Ok(r) => r,
        Err(_) => return None,
    };
    let w = rgb.width();
    let h = rgb.height();
    let pixels = rgb.as_slice().to_vec();
    image::RgbaImage::from_raw(w, h, pixels).map(image::DynamicImage::ImageRgba8)
}

/// フェーズ2: 静止画1エントリのデコード後サイズ(RGBA, byte)をヘッダ情報のみから推定する。
/// ピクセルデータは一切デコードしないため、寸法を偽装したデコンプレッションボム的な
/// エントリが来ても本体デコードは発生しない。ヘッダ解析に失敗した場合は None を返し、
/// 呼び出し側でフルデコードへのフォールバックを判断する。
pub fn estimate_static_decoded_bytes(buf: &[u8]) -> Option<usize> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(buf))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    Some((w as usize) * (h as usize) * 4)
}

/// フェーズ2: アニメーションエントリ1件のサンプル見積もり結果。
pub enum AnimSampleEstimate {
    /// 拡張子非対応、または構造的に非アニメーション（単一フレーム含む）と判定された。
    /// 呼び出し側は `estimate_static_decoded_bytes` にフォールバックする。
    NotAnimated,
    /// フェーズ1.5のインクリメンタルガードが `budget_bytes` 超過を検出した。
    OverBudget,
    /// アニメーションとして `budget_bytes` 以内でデコードできた実サイズ。
    Bytes(usize),
}

/// フェーズ2: アニメーション拡張子のエントリを、フェーズ1.5のガード付きデコード経路
/// (`AnimatedImage::from_*`) で実際にデコードしてサイズを見積もる。
/// `budget_bytes` を hard_limit / cache_budget の両方に使うことで、単体で予算超過する
/// サンプルはガードの時点で `None` として弾かれ、`OverBudget` に変換される。
///
/// 「非アニメーション」と「予算超過」がどちらも `AnimatedImage::from_*` の `None` に
/// 集約されるため、事前に構造的な非アニメーション判定（PNGのacTLチャンク有無、
/// WebPの静止画デコード可否）を行い、`NotAnimated` を先に弾いてから呼び出す。
pub fn estimate_anim_sample_bytes(buf: &[u8], ext: &str, budget_bytes: usize, ring_bounds: (usize, usize)) -> AnimSampleEstimate {
    use crate::anim::{AnimFormat, SequentialAnimDecoder, resolve_ring_capacity};
    use crate::cache::ANIM_RING_BUDGET_PCT;

    /// フェーズ3/3.5でアニメは全フレーム常駐ではなくリングバッファで保持されるため、
    /// 見積もりも「実際に常駐しうる最大バイト数」＝リング容量分だけをデコードして求める
    /// （`cache.rs::RingAnimation::from_source`と同じ判定基準・同じ容量算出式を使う。
    /// 実質1フレームならNotAnimated、1フレーム目の時点でbudget_bytesを超えるなら即OverBudget）。
    fn ring_bounded_estimate(format: AnimFormat, buf: &[u8], budget_bytes: usize, ring_bounds: (usize, usize)) -> AnimSampleEstimate {
        let Some(mut decoder) = SequentialAnimDecoder::new(format, std::sync::Arc::from(buf)) else {
            return AnimSampleEstimate::NotAnimated;
        };
        let Some(frame0) = decoder.next_frame() else {
            return AnimSampleEstimate::NotAnimated;
        };
        let frame_bytes = |img: &image::RgbaImage| (img.width() as usize) * (img.height() as usize) * 4;
        let frame0_bytes = frame_bytes(&frame0.image);
        if frame0_bytes > budget_bytes {
            return AnimSampleEstimate::OverBudget;
        }

        let Some(frame1) = decoder.next_frame() else {
            // 実質1フレームしかない = 静止画相当（decode_ring_anim の SingleFrame 判定と同じ）
            return AnimSampleEstimate::NotAnimated;
        };
        let mut total = frame0_bytes + frame_bytes(&frame1.image);

        let ring_budget_bytes = budget_bytes * ANIM_RING_BUDGET_PCT / 100;
        let (min_frames, max_frames) = ring_bounds;
        let capacity = resolve_ring_capacity(frame0_bytes, ring_budget_bytes, min_frames, max_frames);
        for _ in 2..capacity {
            match decoder.next_frame() {
                Some(f) => total += frame_bytes(&f.image),
                None => break, // アニメ自体がリング容量より短い
            }
        }
        AnimSampleEstimate::Bytes(total)
    }

    match ext {
        "gif" => ring_bounded_estimate(AnimFormat::Gif, buf, budget_bytes, ring_bounds),
        "webp" => {
            // 静止画WebPはAnimDecoderがデコード失敗またはhas_animation()==falseを返すことが
            // あり、budget_bytes超過とは無関係にNoneになりうる。先に静止画デコードを試して
            // 弾くことで、typicalな静止画WebPページの誤検知(OverBudget誤判定)を避ける。
            if webp::Decoder::new(buf).decode().is_some() {
                return AnimSampleEstimate::NotAnimated;
            }
            ring_bounded_estimate(AnimFormat::Webp, buf, budget_bytes, ring_bounds)
        }
        "png" => {
            let is_apng = image::codecs::png::PngDecoder::new(std::io::Cursor::new(buf))
                .ok()
                .and_then(|d| d.is_apng().ok())
                .unwrap_or(false);
            if !is_apng {
                return AnimSampleEstimate::NotAnimated;
            }
            ring_bounded_estimate(AnimFormat::Apng, buf, budget_bytes, ring_bounds)
        }
        "avif" => ring_bounded_estimate(AnimFormat::Avif, buf, budget_bytes, ring_bounds),
        _ => AnimSampleEstimate::NotAnimated,
    }
}

/// フェーズ2: 総ページ数に応じてサンプリング対象のインデックスを決める。
/// 3枚以下→先頭1枚、4〜10枚→先頭・末尾2枚、11枚以上→先頭・中間・末尾3枚。
fn select_sample_indices(total_pages: usize) -> Vec<usize> {
    match total_pages {
        0 => Vec::new(),
        1..=3 => vec![0],
        4..=10 => vec![0, total_pages - 1],
        _ => vec![0, total_pages / 2, total_pages - 1],
    }
}

/// フェーズ2: サンプル1エントリの見積もり結果。
enum EntryEstimate {
    /// budget_bytes 以内でのデコード後（推定）サイズ。
    Bytes(usize),
    /// このエントリ単体で budget_bytes を超過することが確定した。
    OverBudget,
}

/// フェーズ2: 開済みアーカイブから1エントリを展開し、サイズを見積もる。
/// アニメーション拡張子ならまずフェーズ1.5ガード付きデコードを試し、非アニメーションと
/// 判定された場合は静止画のヘッダ読みにフォールバックする。ヘッダ解析にも失敗した場合の
/// 最終手段としてフルデコードして実サイズを使う（通常のページ読み込みと同じコスト）。
/// エントリの読み込み自体に失敗した場合は None（このサンプルは無視する）。
fn estimate_entry_bytes<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
    budget_bytes: usize,
    ring_bounds: (usize, usize),
) -> Option<EntryEstimate> {
    let (buf, display_name) = load_bytes_from_archive(archive, entry_name)?;
    let ext = display_name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();

    if matches!(ext.as_str(), "gif" | "webp" | "png" | "avif") {
        match estimate_anim_sample_bytes(&buf, &ext, budget_bytes, ring_bounds) {
            AnimSampleEstimate::Bytes(n) => return Some(EntryEstimate::Bytes(n)),
            AnimSampleEstimate::OverBudget => return Some(EntryEstimate::OverBudget),
            AnimSampleEstimate::NotAnimated => {} // 静止画推定へフォールバック
        }
    }

    if let Some(n) = estimate_static_decoded_bytes(&buf) {
        return Some(to_entry_estimate(n, budget_bytes));
    }

    // ヘッダ解析失敗時の最終手段: フルデコードして実サイズを使う。
    let img = decode_image(&buf, &display_name)?;
    let n = (img.width() as usize) * (img.height() as usize) * 4;
    Some(to_entry_estimate(n, budget_bytes))
}

fn to_entry_estimate(bytes: usize, budget_bytes: usize) -> EntryEstimate {
    if bytes > budget_bytes {
        EntryEstimate::OverBudget
    } else {
        EntryEstimate::Bytes(bytes)
    }
}

/// フェーズ2: アーカイブ全体の推定結果。
#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveMemoryEstimate {
    /// サンプル平均 × 総ページ数が budget_bytes 以内。
    Ok,
    /// budget_bytes を超過（サンプル単体超過、または合計見積もり超過のいずれか）。
    OverBudget,
}

/// フェーズ2: `list_images` の結果に対してサンプリングを行い、アーカイブ全体を
/// デコードした場合の推定合計サイズが `budget_bytes` に収まるかを判定する。
/// サンプル1枚でも単体で budget_bytes を超えた時点で残りのサンプリングを打ち切り、
/// 即座に `OverBudget` と判定する（サンプル単体チェック + 合計見積もりチェックの二重構成）。
/// 全サンプルの読み込みに失敗した場合（判定不能）は `Ok` を返し、通常のオープンを妨げない。
pub fn estimate_archive_memory(
    path: &Path,
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
) -> ArchiveMemoryEstimate {
    if entries.is_empty() {
        return ArchiveMemoryEstimate::Ok;
    }
    let Ok(file) = std::fs::File::open(path) else { return ArchiveMemoryEstimate::Ok };
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return ArchiveMemoryEstimate::Ok };

    let mut sample_bytes: Vec<usize> = Vec::new();
    for idx in select_sample_indices(entries.len()) {
        match estimate_entry_bytes(&mut archive, &entries[idx].entry_name, budget_bytes, ring_bounds) {
            Some(EntryEstimate::OverBudget) => return ArchiveMemoryEstimate::OverBudget,
            Some(EntryEstimate::Bytes(n)) => sample_bytes.push(n),
            None => {} // 読み込み/デコード失敗エントリはサンプルから除外
        }
    }

    let Some(avg) = average(&sample_bytes) else { return ArchiveMemoryEstimate::Ok };
    let total_estimate = avg.saturating_mul(entries.len());
    if total_estimate > budget_bytes {
        ArchiveMemoryEstimate::OverBudget
    } else {
        ArchiveMemoryEstimate::Ok
    }
}

fn average(values: &[usize]) -> Option<usize> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<usize>() / values.len())
}

/// 開済みの ZipArchive からエントリの生バイトと表示名を返す（デコードしない）
pub fn load_bytes_from_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
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

/// 拡張子で 7z/CB7 かどうかを判定する
pub fn is_7z_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("7z") || e.eq_ignore_ascii_case("cb7"))
        .unwrap_or(false)
}

/// (display_name, entry_name, date_key) のペア列から、ソート・衝突回避済みの
/// `ImageEntry` 列を組み立てる（ZIP/7z共通処理）。
fn finalize_entries(mut pairs: Vec<(String, String, u64)>) -> Vec<ImageEntry> {
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

/// アーカイブ(ZIP/CBZ/7z/CB7)内の全画像をフラット化して返す。
/// ディレクトリ構造を無視し、ファイル名の衝突は "stem_01.ext" 形式で回避する。
pub fn list_images(path: &Path) -> Vec<ImageEntry> {
    if is_7z_path(path) {
        list_images_7z(path)
    } else {
        list_images_zip(path)
    }
}

fn list_images_zip(path: &Path) -> Vec<ImageEntry> {
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

    finalize_entries(pairs)
}

/// 7z のヘッダ(ファイル一覧・メタデータ)のみを読み、画像エントリを一覧化する。
/// Phase1時点ではデータストリームの展開は行わない(展開はPhase2で対応)。
fn list_images_7z(path: &Path) -> Vec<ImageEntry> {
    let Ok(archive) = sevenz_rust2::Archive::open(path) else {
        return Vec::new();
    };

    let mut pairs: Vec<(String, String, u64)> = Vec::new();
    for entry in &archive.files {
        if entry.is_directory || !entry.has_stream {
            continue;
        }
        if !is_image_entry_raw(entry.name.as_bytes()) {
            continue;
        }
        let date_key = if entry.has_last_modified_date {
            nt_time_to_date_key(entry.last_modified_date)
        } else {
            0
        };
        pairs.push((entry.name.clone(), entry.name.clone(), date_key));
    }

    finalize_entries(pairs)
}

/// 7zの`NtTime`(Windows FILETIME)をZIP版と同じ形式(年月日時分秒を1桁ずつパックしたu64)に変換する。
/// 変換不能(エポック未満・オーバーフロー等)なら0を返す。
fn nt_time_to_date_key(t: sevenz_rust2::NtTime) -> u64 {
    let st: std::time::SystemTime = t.into();
    let Ok(dur) = st.duration_since(std::time::SystemTime::UNIX_EPOCH) else {
        return 0;
    };
    let secs = dur.as_secs();
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Howard Hinnant の civil_from_days アルゴリズム(days-since-epoch -> 暦日)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { yoe as i64 + era * 400 + 1 } else { yoe as i64 + era * 400 };

    (year as u64) * 10_000_000_000
        + month * 100_000_000
        + day * 1_000_000
        + hour * 10_000
        + minute * 100
        + second
}

/// 7z(ソリッド圧縮)は1エントリだけの取り出しがブロック先頭からの再展開になり非効率なため、
/// 開いた時点で画像エントリを一括デコードし `entry_name -> 生バイト列` の対応表にまとめて返す。
/// 以降のページ送りはこの表を引くだけで済み、再展開は発生しない。
pub fn extract_all_images_7z<R: Read + Seek>(source: R) -> HashMap<String, Vec<u8>> {
    let Ok(mut reader) = sevenz_rust2::ArchiveReader::new(source, sevenz_rust2::Password::empty()) else {
        return HashMap::new();
    };

    let mut out = HashMap::new();
    let _ = reader.for_each_entries(&mut |entry: &sevenz_rust2::ArchiveEntry, r: &mut dyn Read| {
        if !entry.is_directory && entry.has_stream && is_image_entry_raw(entry.name.as_bytes()) {
            let mut buf = Vec::with_capacity(entry.size as usize);
            if r.read_to_end(&mut buf).is_ok() {
                out.insert(entry.name.clone(), buf);
            }
        }
        Ok(true) // 全エントリを処理する
    });
    out
}

/// ディスク上の7zファイルを開いて `extract_all_images_7z` を実行する
pub fn extract_all_images_7z_path(path: &Path) -> HashMap<String, Vec<u8>> {
    let Ok(file) = std::fs::File::open(path) else {
        return HashMap::new();
    };
    extract_all_images_7z(file)
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
    entry.read_to_end(&mut buf).ok()?;
    let display_name = decode_zip_name(entry.name_raw());
    let img = decode_image(&buf, &display_name)?;
    Some(img)
}

/// ZIP/CBZ の先頭画像1枚をデコードして返す（サムネイル用）。
/// まず Local File Header を先頭から順読みして試みる（ネットワーク帯域節約）。
/// Data Descriptor フラグ等で順読み不可の場合は ZipArchive 経由にフォールバックする。
pub fn load_first_image(path: &Path) -> Option<image::DynamicImage> {
    load_first_image_sequential(path).or_else(|| load_first_image_via_archive(path))
}

/// Local File Header を先頭から順読みする（セントラルディレクトリ・末尾シーク不要）。
/// Data Descriptor フラグ付き DEFLATE エントリはストリーム展開で処理する。
fn load_first_image_sequential(path: &Path) -> Option<image::DynamicImage> {
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
                    let name = decode_zip_name(&fname);
                    if let Some(img) = decode_image(&raw, &name) {
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
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        if let Some(img) = decode_image(&buf, &display_name) {
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
    use std::io::Write;
    use std::path::PathBuf;

    const MB: usize = 1024 * 1024;
    /// テスト用のリング先読み枚数(下限, 上限)。実運用のデフォルト(4, 32)に合わせる。
    const TEST_RING_BOUNDS: (usize, usize) = (4, 32);

    fn test_zip() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/testarchive.zip")
    }

    /// 指定した寸法のPNGを1枚ずつ格納したZIPを一時ファイルとして作成する。
    fn build_zip_with_pngs(dims: &[(u32, u32)]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("nekoviewer_test_estimate_{}_{n}.zip", std::process::id()));

        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (i, (w, h)) in dims.iter().enumerate() {
            writer.start_file(format!("page{:03}.png", i + 1), options).unwrap();
            writer.write_all(&encode_png(*w, *h)).unwrap();
        }
        writer.finish().unwrap();
        path
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

    fn test_7z() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/test7z.7z")
    }

    #[test]
    fn test_list_images_7z_count() {
        let entries = list_images(&test_7z());
        assert_eq!(entries.len(), 3, "7z画像エントリ数が想定と異なる");
    }

    #[test]
    fn test_list_images_7z_sorted_and_named() {
        let entries = list_images(&test_7z());
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "display_name がソートされていない");
        assert!(names.iter().any(|n| n.ends_with(".webp")));
    }

    #[test]
    fn test_extract_all_images_7z_decodes_pages() {
        let entries = list_images(&test_7z());
        assert_eq!(entries.len(), 3);
        let map = extract_all_images_7z_path(&test_7z());
        assert_eq!(map.len(), 3, "一括展開後のエントリ数が一覧と一致しない");
        for e in &entries {
            let buf = map.get(&e.entry_name).expect("展開済みマップにエントリが無い");
            let img = decode_image_bytes(buf, &e.display_name);
            assert!(img.is_some(), "{} のデコードに失敗", e.display_name);
        }
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

    fn encode_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::new(w, h);
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn encode_gif_frames(w: u32, h: u32, frame_count: usize) -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::Delay;
        let mut buf = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut buf);
            for _ in 0..frame_count {
                let img = image::RgbaImage::new(w, h);
                let frame = image::Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(10, 1));
                encoder.encode_frame(frame).unwrap();
            }
        }
        buf
    }

    #[test]
    fn test_estimate_static_decoded_bytes_reads_header_only() {
        let buf = encode_png(100, 50);
        let bytes = estimate_static_decoded_bytes(&buf);
        assert_eq!(bytes, Some(100 * 50 * 4));
    }

    #[test]
    fn test_estimate_static_decoded_bytes_invalid_header() {
        let bytes = estimate_static_decoded_bytes(&[0u8; 4]);
        assert_eq!(bytes, None, "不正なヘッダはNoneでフォールバックを示すべき");
    }

    #[test]
    fn test_estimate_anim_sample_bytes_plain_png_is_not_animated() {
        let buf = encode_png(10, 10);
        let result = estimate_anim_sample_bytes(&buf, "png", 10 * MB, TEST_RING_BOUNDS);
        assert!(matches!(result, AnimSampleEstimate::NotAnimated), "非APNGはNotAnimatedであるべき");
    }

    #[test]
    fn test_estimate_anim_sample_bytes_gif_within_budget() {
        let buf = encode_gif_frames(10, 10, 3);
        let result = estimate_anim_sample_bytes(&buf, "gif", 10 * MB, TEST_RING_BOUNDS);
        match result {
            AnimSampleEstimate::Bytes(n) => assert_eq!(n, 10 * 10 * 4 * 3),
            _ => panic!("3フレームGIFはBytesであるべき"),
        }
    }

    #[test]
    fn test_estimate_anim_sample_bytes_gif_over_budget() {
        let buf = encode_gif_frames(10, 10, 3);
        // 1フレーム分(400byte)未満の予算 → 先頭フレームの時点でハードリミット超過
        let result = estimate_anim_sample_bytes(&buf, "gif", 100, TEST_RING_BOUNDS);
        assert!(matches!(result, AnimSampleEstimate::OverBudget), "予算未満のGIFはOverBudgetであるべき");
    }

    #[test]
    fn test_select_sample_indices_boundaries() {
        assert_eq!(select_sample_indices(0), Vec::<usize>::new());
        assert_eq!(select_sample_indices(1), vec![0]);
        assert_eq!(select_sample_indices(3), vec![0]);
        assert_eq!(select_sample_indices(4), vec![0, 3]);
        assert_eq!(select_sample_indices(10), vec![0, 9]);
        assert_eq!(select_sample_indices(11), vec![0, 5, 10]);
        assert_eq!(select_sample_indices(100), vec![0, 50, 99]);
    }

    #[test]
    fn test_estimate_archive_memory_ok_within_budget() {
        let path = build_zip_with_pngs(&[(10, 10), (10, 10), (10, 10)]);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 3);
        let result = estimate_archive_memory(&path, &entries, 10 * MB, TEST_RING_BOUNDS);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[test]
    fn test_estimate_archive_memory_aggregate_over_budget() {
        // 各サンプル単体はbudget以内(400,000byte)だが、"平均×総ページ数"では超える設定。
        let dims: Vec<(u32, u32)> = (0..20).map(|_| (100, 100)).collect(); // 40,000byte/枚
        let path = build_zip_with_pngs(&dims);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 20);
        // 40,000 * 20 = 800,000 > budget(500,000) だが 40,000 < budget 単体は超えない
        let result = estimate_archive_memory(&path, &entries, 500_000, TEST_RING_BOUNDS);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::OverBudget);
    }

    #[test]
    fn test_estimate_archive_memory_single_sample_short_circuits() {
        // 先頭ページ(サンプル対象)単体が既にbudgetを超過 → 末尾サンプルを見る前に即OverBudget
        let path = build_zip_with_pngs(&[(2000, 2000), (10, 10), (10, 10), (10, 10), (10, 10)]);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 5); // 5枚 → 先頭・末尾2サンプル
        let result = estimate_archive_memory(&path, &entries, MB, TEST_RING_BOUNDS);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::OverBudget);
    }

    #[test]
    fn test_estimate_archive_memory_empty_entries_is_ok() {
        let path = test_zip();
        let result = estimate_archive_memory(&path, &[], 1, TEST_RING_BOUNDS);
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[test]
    #[ignore]
    fn debug_probe_real_zip() {
        let path = PathBuf::from("test/神聖モテモテ王国 01巻.zip");
        let entries = list_images(&path);
        eprintln!("entries.len() = {}", entries.len());
        let (page_max, _page_min, _file_max) = crate::cache::resolve_cache_budgets(None);
        eprintln!("page_max(budget) = {} bytes ({} MB)", page_max, page_max / (1024*1024));
        let result = estimate_archive_memory(&path, &entries, page_max, TEST_RING_BOUNDS);
        eprintln!("result = {:?}", result);
    }

    #[test]
    #[ignore]
    fn debug_probe_testwebp_zip() {
        let path = PathBuf::from("/tmp/testwebp.zip");
        let entries = list_images(&path);
        eprintln!("entries.len() = {}", entries.len());
        let (page_max, _page_min, _file_max) = crate::cache::resolve_cache_budgets(None);
        eprintln!("page_max(budget) = {} bytes ({} MB)", page_max, page_max / (1024*1024));

        // select_sample_indices が実際にどのサンプルを選ぶか、各サンプルの見積もりも個別に見る。
        for idx in select_sample_indices(entries.len()) {
            let file = std::fs::File::open(&path).unwrap();
            let mut archive = zip::ZipArchive::new(file).unwrap();
            match estimate_entry_bytes(&mut archive, &entries[idx].entry_name, page_max, TEST_RING_BOUNDS) {
                Some(EntryEstimate::Bytes(n)) => eprintln!("sample[{idx}] = Bytes({n}, {}MB)", n/(1024*1024)),
                Some(EntryEstimate::OverBudget) => eprintln!("sample[{idx}] = OverBudget"),
                None => eprintln!("sample[{idx}] = None(read failure)"),
            }
        }

        let result = estimate_archive_memory(&path, &entries, page_max, TEST_RING_BOUNDS);
        eprintln!("result = {:?}", result);
    }
}
