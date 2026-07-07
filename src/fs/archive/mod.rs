//! アーカイブ抽象化層。フォーマット判定に基づき各バックエンド(zip/sevenz)へ
//! 公開APIをディスパッチする。フォーマット非依存の共通型・共有見積もりロジックもここに置く。

use std::collections::HashMap;
use std::path::Path;

pub mod decode;
pub mod detect;
#[cfg(feature = "fmt-7z")]
mod sevenz;
#[cfg(feature = "fmt-tar")]
mod tar;
mod zip;

// 既存の呼び出し元が使う `crate::fs::archive::NAME` パスを維持するための再エクスポート。
pub use decode::decode_image_bytes;
pub use detect::{detect_format, is_7z_path, is_supported_image_file, ArchiveFormat};
#[cfg(feature = "fmt-7z")]
pub use sevenz::extract_all_images_7z_path;
#[cfg(feature = "fmt-tar")]
pub use tar::extract_all_images_tar_path;
pub use zip::load_bytes_from_archive;

/// ZIP/CBZ/7z/CB7 内の画像エントリ
pub struct ImageEntry {
    /// ソート・表示用のファイル名（衝突時は "stem_01.ext" 形式）
    pub display_name: String,
    /// アーカイブ内でのフルパス（読み込みに使用）
    pub entry_name: String,
    /// 日付ソートキー（年月日時分秒を1桁ずつパックした u64）
    pub date_key: u64,
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
pub(crate) enum EntryEstimate {
    /// budget_bytes 以内でのデコード後（推定）サイズ。
    Bytes(usize),
    /// このエントリ単体で budget_bytes を超過することが確定した。
    OverBudget,
}

/// フェーズ2: 展開済みバイト列1件の「PageCacheへの計上額」を見積もる（ZIP/7z共通処理）。
/// 実際の保持サイズ（表示リサイズ後。アニメはリング予約額）と同じ式で計算する。
/// アニメーション拡張子ならまずガード付きデコードを試し、非アニメーションと
/// 判定された場合は静止画のヘッダ読みにフォールバックする。ヘッダ解析にも失敗した場合の
/// 最終手段としてフルデコードして実寸法を使う（通常のページ読み込みと同じコスト）。
/// デコード自体に失敗した場合は None（このサンプルは無視する）。
pub(crate) fn estimate_bytes_for_entry(buf: &[u8], display_name: &str, budget_bytes: usize, ring_bounds: (usize, usize), max_decode_edge: u32) -> Option<EntryEstimate> {
    let ext = display_name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();

    if matches!(ext.as_str(), "gif" | "webp" | "png" | "avif") {
        match decode::estimate_anim_sample_bytes(buf, &ext, budget_bytes, ring_bounds, max_decode_edge) {
            decode::AnimSampleEstimate::Bytes(n) => return Some(EntryEstimate::Bytes(n)),
            decode::AnimSampleEstimate::OverBudget => return Some(EntryEstimate::OverBudget),
            decode::AnimSampleEstimate::NotAnimated => {} // 静止画推定へフォールバック
        }
    }

    if let Some(n) = decode::estimate_static_decoded_bytes(buf, max_decode_edge) {
        return Some(to_entry_estimate(n, budget_bytes));
    }

    // ヘッダ解析失敗時の最終手段: フルデコードして実寸法を使う。
    let img = decode::decode_image_bytes(buf)?;
    let (rw, rh) = crate::anim::fit_within(img.width(), img.height(), max_decode_edge, max_decode_edge);
    let n = (rw as usize) * (rh as usize) * 4;
    Some(to_entry_estimate(n, budget_bytes))
}

pub(crate) fn to_entry_estimate(bytes: usize, budget_bytes: usize) -> EntryEstimate {
    if bytes > budget_bytes {
        EntryEstimate::OverBudget
    } else {
        EntryEstimate::Bytes(bytes)
    }
}

fn average(values: &[usize]) -> Option<usize> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<usize>() / values.len())
}

/// 先読みウィンドウ幅の連続区間で最大となる計上額合計（同時常駐の最悪ケース）。
/// 全エントリの見積もりが手に入る7z/tar用。読み込み失敗エントリは0として渡す。
#[cfg(any(feature = "fmt-7z", feature = "fmt-tar"))]
pub(crate) fn worst_window_total(per_entry: &[usize], window: usize) -> usize {
    let window = window.clamp(1, per_entry.len().max(1));
    if per_entry.is_empty() {
        return 0;
    }
    per_entry
        .windows(window)
        .map(|w| w.iter().sum::<usize>())
        .max()
        .unwrap_or(0)
}

/// フェーズ2: アーカイブ全体の推定結果。
#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveMemoryEstimate {
    /// 同時常駐推定（先読みウィンドウぶんの計上額）が budget_bytes 以内。
    Ok,
    /// budget_bytes を超過（サンプル単体超過、または同時常駐推定超過のいずれか）。
    OverBudget,
}

/// フェーズ2改: `list_images` の結果に対してサンプリングを行い、閲覧中に同時常駐しうる
/// 推定サイズ（先読みウィンドウ `cache::PREFETCH_WINDOW` ページぶんの計上額）が
/// `budget_bytes` に収まるかを判定する。旧実装の「平均×総ページ数」は全ページ同時常駐を
/// 仮定した過大見積もりで、リングバッファ・evictの実態と乖離していたため置き換えた。
/// サンプル1枚でも単体で budget_bytes を超えた時点で残りのサンプリングを打ち切り、
/// 即座に `OverBudget` と判定する（サンプル単体チェック + 同時常駐チェックの二重構成）。
/// 全サンプルの読み込みに失敗した場合（判定不能）は `Ok` を返し、通常のオープンを妨げない。
///
/// `max_decode_edge` は表示リサイズのガードレール値（config.max_decode_edge）。
/// 見積もりは全てこの箱に収めたリサイズ後サイズで行い、実際の保持量と一致させる。
///
/// 7z(ソリッド圧縮)はサンプリングでの軽量見積もりが成立しない（1件だけ取り出すのにブロック
/// 先頭からの再展開が必要になるため）。そのため7zは `estimate_archive_memory_7z` に委譲し、
/// 一括展開結果を使った全件計算（最悪連続ウィンドウ）で判定する。
pub fn estimate_archive_memory(
    path: &Path,
    entries: &[ImageEntry],
    budget_bytes: usize,
    ring_bounds: (usize, usize),
    max_decode_edge: u32,
) -> ArchiveMemoryEstimate {
    if entries.is_empty() {
        return ArchiveMemoryEstimate::Ok;
    }
    match detect::detect_format(path) {
        #[cfg(feature = "fmt-7z")]
        ArchiveFormat::SevenZ => sevenz::estimate_archive_memory_7z(path, entries, budget_bytes, ring_bounds, max_decode_edge),
        #[cfg(feature = "fmt-tar")]
        ArchiveFormat::Tar => tar::estimate_archive_memory_tar(path, entries, budget_bytes, ring_bounds, max_decode_edge),
        ArchiveFormat::Zip => zip::estimate_archive_memory_zip(path, entries, budget_bytes, ring_bounds, max_decode_edge),
    }
}

/// アーカイブ(ZIP/CBZ/7z/CB7/TAR/CBT)内の全画像をフラット化して返す。
/// ディレクトリ構造を無視し、ファイル名の衝突は "stem_01.ext" 形式で回避する。
pub fn list_images(path: &Path) -> Vec<ImageEntry> {
    match detect::detect_format(path) {
        #[cfg(feature = "fmt-7z")]
        ArchiveFormat::SevenZ => sevenz::list_images_7z(path),
        #[cfg(feature = "fmt-tar")]
        ArchiveFormat::Tar => tar::list_images_tar(path),
        ArchiveFormat::Zip => zip::list_images_zip(path),
    }
}

/// アーカイブ(ZIP/CBZ/7z/CB7/TAR/CBT)の先頭画像1枚をデコードして返す（サムネイル用）。
/// ZIPはまず Local File Header を先頭から順読みして試みる（ネットワーク帯域節約）。
/// Data Descriptor フラグ等で順読み不可の場合は ZipArchive 経由にフォールバックする。
pub fn load_first_image(path: &Path) -> Option<image::DynamicImage> {
    match detect::detect_format(path) {
        #[cfg(feature = "fmt-7z")]
        ArchiveFormat::SevenZ => sevenz::load_first_image_7z(path),
        #[cfg(feature = "fmt-tar")]
        ArchiveFormat::Tar => tar::load_first_image_tar(path),
        ArchiveFormat::Zip => {
            zip::load_first_image_sequential(path).or_else(|| zip::load_first_image_via_archive(path))
        }
    }
}

/// UNIXエポック秒を、ZIP版と同じ日付ソートキー(年月日時分秒を1桁ずつパックしたu64)に変換する。
/// Howard Hinnant の civil_from_days アルゴリズム(days-since-epoch -> 暦日)を使う。7z/tar 共通。
#[cfg(any(feature = "fmt-7z", feature = "fmt-tar"))]
pub(crate) fn unix_secs_to_date_key(secs: u64) -> u64 {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

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
    use super::decode::{estimate_anim_sample_bytes, estimate_static_decoded_bytes, AnimSampleEstimate};
    use super::zip::estimate_entry_bytes;
    use super::{
        estimate_archive_memory, list_images, load_first_image,
        select_sample_indices, ArchiveMemoryEstimate, EntryEstimate,
    };
    #[cfg(feature = "fmt-7z")]
    use super::{decode_image_bytes, extract_all_images_7z_path};
    use std::io::Write;
    use std::path::PathBuf;

    const MB: usize = 1024 * 1024;
    /// テスト用のリング先読み枚数(下限, 上限)。実運用のデフォルト(4, 32)に合わせる。
    const TEST_RING_BOUNDS: (usize, usize) = (4, 32);
    /// テスト用の表示リサイズガードレール長辺。実運用のデフォルト(max_decode_edge=1920)に合わせる。
    const TEST_DECODE_EDGE: u32 = 1920;

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

    #[cfg(feature = "fmt-7z")]
    fn test_7z() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/test7z.7z")
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_list_images_7z_count() {
        let entries = list_images(&test_7z());
        assert_eq!(entries.len(), 3, "7z画像エントリ数が想定と異なる");
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_list_images_7z_sorted_and_named() {
        let entries = list_images(&test_7z());
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "display_name がソートされていない");
        assert!(names.iter().any(|n| n.ends_with(".webp")));
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_extract_all_images_7z_decodes_pages() {
        let entries = list_images(&test_7z());
        assert_eq!(entries.len(), 3);
        let map = extract_all_images_7z_path(&test_7z());
        assert_eq!(map.len(), 3, "一括展開後のエントリ数が一覧と一致しない");
        for e in &entries {
            let buf = map.get(&e.entry_name).expect("展開済みマップにエントリが無い");
            let img = decode_image_bytes(buf);
            assert!(img.is_some(), "{} のデコードに失敗", e.display_name);
        }
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_load_first_image_7z_returns_some() {
        let img = load_first_image(&test_7z());
        assert!(img.is_some(), "7zの先頭画像の読み込みに失敗");
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_estimate_archive_memory_7z_ok_within_budget() {
        // test7z.7z の3画像（最大4096x3072）はリサイズ後基準（1920箱）だと1枚≈11MB以下。
        // 100MB予算なら余裕で収まる。
        let entries = list_images(&test_7z());
        let result = estimate_archive_memory(&test_7z(), &entries, 100 * MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[cfg(feature = "fmt-7z")]
    #[test]
    fn test_estimate_archive_memory_7z_over_budget() {
        // 最大の1枚(4096x3072)はリサイズ後(1920x1440)でも約11MB。8MB予算なら単体超過でOverBudget。
        let entries = list_images(&test_7z());
        let result = estimate_archive_memory(&test_7z(), &entries, 8 * MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        assert_eq!(result, ArchiveMemoryEstimate::OverBudget);
    }

    #[test]
    fn test_load_first_image_returns_some() {
        let img = load_first_image(&test_zip());
        assert!(img.is_some(), "先頭画像の読み込みに失敗");
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
        let bytes = estimate_static_decoded_bytes(&buf, TEST_DECODE_EDGE);
        assert_eq!(bytes, Some(100 * 50 * 4));
    }

    #[test]
    fn test_estimate_static_decoded_bytes_fits_to_decode_edge() {
        // 原寸4000x2000はガードレール(1920箱)に収まるよう縮小されて保持されるため、
        // 見積もりも縮小後サイズ(1920x960)で返すべき。
        let buf = encode_png(4000, 2000);
        let bytes = estimate_static_decoded_bytes(&buf, TEST_DECODE_EDGE);
        assert_eq!(bytes, Some(1920 * 960 * 4));
    }

    #[test]
    fn test_estimate_static_decoded_bytes_invalid_header() {
        let bytes = estimate_static_decoded_bytes(&[0u8; 4], TEST_DECODE_EDGE);
        assert_eq!(bytes, None, "不正なヘッダはNoneでフォールバックを示すべき");
    }

    #[test]
    fn test_estimate_anim_sample_bytes_plain_png_is_not_animated() {
        let buf = encode_png(10, 10);
        let result = estimate_anim_sample_bytes(&buf, "png", 10 * MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        assert!(matches!(result, AnimSampleEstimate::NotAnimated), "非APNGはNotAnimatedであるべき");
    }

    #[test]
    fn test_estimate_anim_sample_bytes_gif_returns_ring_reservation() {
        // アニメの見積もりは「リング容量×リサイズ後フレームサイズ」の予約計上額。
        // 10x10(400byte/枚)、予算10MB → リング予算2.5MBに対しフレームが小さいので
        // 容量は上限32枚に張り付き、400×32=12800byte。
        let buf = encode_gif_frames(10, 10, 3);
        let result = estimate_anim_sample_bytes(&buf, "gif", 10 * MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        match result {
            AnimSampleEstimate::Bytes(n) => assert_eq!(n, 10 * 10 * 4 * TEST_RING_BOUNDS.1),
            _ => panic!("3フレームGIFはBytesであるべき"),
        }
    }

    #[test]
    fn test_estimate_anim_sample_bytes_gif_over_budget() {
        let buf = encode_gif_frames(10, 10, 3);
        // 1フレーム分(400byte)未満の予算 → 先頭フレームの時点でハードリミット超過
        let result = estimate_anim_sample_bytes(&buf, "gif", 100, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
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
        let result = estimate_archive_memory(&path, &entries, 10 * MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[test]
    fn test_estimate_archive_memory_window_over_budget() {
        // 各サンプル単体はbudget以内(40,000byte)だが、"平均×先読みウィンドウ(16)"では超える設定。
        let dims: Vec<(u32, u32)> = (0..20).map(|_| (100, 100)).collect(); // 40,000byte/枚
        let path = build_zip_with_pngs(&dims);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 20);
        // 40,000 × 16 = 640,000 > budget(500,000) だが 40,000 < budget 単体は超えない
        let result = estimate_archive_memory(&path, &entries, 500_000, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::OverBudget);
    }

    #[test]
    fn test_estimate_archive_memory_many_pages_bounded_by_window() {
        // 旧実装の「平均×総ページ数」なら 40,000×100=4,000,000 でOverBudgetになる規模でも、
        // 同時常駐は先読みウィンドウ(16ページ)ぶんしかないため 40,000×16=640,000 で判定し Ok。
        let dims: Vec<(u32, u32)> = (0..100).map(|_| (100, 100)).collect(); // 40,000byte/枚
        let path = build_zip_with_pngs(&dims);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 100);
        let result = estimate_archive_memory(&path, &entries, 700_000, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[test]
    fn test_estimate_archive_memory_single_sample_short_circuits() {
        // 先頭ページ(サンプル対象)単体がリサイズ後(1920x1920=14.7MB)でもbudget超過
        // → 末尾サンプルを見る前に即OverBudget
        let path = build_zip_with_pngs(&[(2000, 2000), (10, 10), (10, 10), (10, 10), (10, 10)]);
        let entries = list_images(&path);
        assert_eq!(entries.len(), 5); // 5枚 → 先頭・末尾2サンプル
        let result = estimate_archive_memory(&path, &entries, MB, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        std::fs::remove_file(&path).ok();
        assert_eq!(result, ArchiveMemoryEstimate::OverBudget);
    }

    #[test]
    fn test_estimate_archive_memory_empty_entries_is_ok() {
        let path = test_zip();
        let result = estimate_archive_memory(&path, &[], 1, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        assert_eq!(result, ArchiveMemoryEstimate::Ok);
    }

    #[cfg(any(feature = "fmt-7z", feature = "fmt-tar"))]
    #[test]
    fn test_worst_window_total_picks_densest_run() {
        // 窓幅3: 最大の連続区間は [5,6,7]=18
        assert_eq!(super::worst_window_total(&[1, 2, 5, 6, 7, 1], 3), 18);
        // 窓幅がエントリ数を超える場合は全合計
        assert_eq!(super::worst_window_total(&[1, 2, 3], 16), 6);
        assert_eq!(super::worst_window_total(&[], 16), 0);
    }

    #[test]
    #[ignore]
    fn debug_probe_real_zip() {
        let path = PathBuf::from("test/神聖モテモテ王国 01巻.zip");
        let entries = list_images(&path);
        eprintln!("entries.len() = {}", entries.len());
        let (page_max, _page_min, _file_max) = crate::cache::resolve_cache_budgets(None);
        eprintln!("page_max(budget) = {} bytes ({} MB)", page_max, page_max / (1024*1024));
        let result = estimate_archive_memory(&path, &entries, page_max, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
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
            match estimate_entry_bytes(&mut archive, &entries[idx].entry_name, page_max, TEST_RING_BOUNDS, TEST_DECODE_EDGE) {
                Some(EntryEstimate::Bytes(n)) => eprintln!("sample[{idx}] = Bytes({n}, {}MB)", n/(1024*1024)),
                Some(EntryEstimate::OverBudget) => eprintln!("sample[{idx}] = OverBudget"),
                None => eprintln!("sample[{idx}] = None(read failure)"),
            }
        }

        let result = estimate_archive_memory(&path, &entries, page_max, TEST_RING_BOUNDS, TEST_DECODE_EDGE);
        eprintln!("result = {:?}", result);
    }
}
