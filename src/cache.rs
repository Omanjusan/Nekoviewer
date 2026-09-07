use crate::{log_perf};
use crate::anim::{AnimFrame, AnimFormat, SequentialAnimDecoder, FrameRingBuffer, resolve_ring_capacity};
use crate::decode_jobs::{DecodeJobOutcome, DecodeJobQueue};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType as FirFilter, PixelType, ResizeAlg, ResizeOptions, Resizer};

const KB: usize = 1024;
const MB: usize = 1024 * KB;

/// 合計キャッシュ予算のうちページキャッシュに回す割合。リングバッファ導入(フェーズ3/3.5)で
/// アニメーションによるページキャッシュ占有が下がったぶん、ファイルキャッシュに厚めに配分する。
const PAGE_CACHE_SHARE_PCT: usize = 70;
const FILE_CACHE_SHARE_PCT: usize = 100 - PAGE_CACHE_SHARE_PCT;
/// 合計予算が未指定(自動)のときに使う、システムRAMに対する割合。
const TOTAL_CACHE_RAM_PCT: usize = 30;
const MIN_RATIO_PCT: usize = 40; // page_max に対する page_min の割合
/// フェーズ4: アニメ1本のリングバッファに割り当てる予算の、page_max に対する割合。
/// フェーズ2の見積もりゲート(fs/archive.rs)も同じ値を使い、実際のリング容量算出と整合させる。
pub(crate) const ANIM_RING_BUDGET_PCT: usize = 25;
const FALLBACK_TOTAL_BYTES: usize = 500 * MB; // sysinfo 失敗時フォールバック（旧30%相当）
/// アニメのデコード済み・表示リサイズ前フレームを保持する小容量FIFO。
/// リサイズ遅延を検出した後はFIFOを最新1枚へ畳み込む。
const ANIM_RAW_QUEUE_CAPACITY: usize = 2;

/// ビューアーの先読みウィンドウ: 現在ページの後方（戻り側）に保持する枚数。
pub const PREFETCH_BEHIND: usize = 5;
/// ビューアーの先読みウィンドウ: 前方（進み側）に先読みする枚数。
pub const PREFETCH_AHEAD: usize = 10;
/// 同時にデコード常駐しうるページ数（後方 + 現在ページ + 前方）。
/// view_explorer の prefetch_pages と、fs/archive のメモリ見積もりゲートが
/// 同じウィンドウ幅を共有するための単一定義。
pub const PREFETCH_WINDOW: usize = PREFETCH_BEHIND + 1 + PREFETCH_AHEAD;

/// システム総RAM量をMB単位で返す（sysinfo失敗時は0）。設定ダイアログの表示にも使う。
pub fn system_total_ram_mb() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.total_memory() / (MB as u64)
}

/// 合計予算が未指定(自動)のときの既定MB値（システムRAMの30%、取得失敗時はフォールバック）。
pub fn default_cache_total_mb() -> u64 {
    let ram_mb = system_total_ram_mb();
    if ram_mb > 0 {
        ram_mb * TOTAL_CACHE_RAM_PCT as u64 / 100
    } else {
        (FALLBACK_TOTAL_BYTES / MB) as u64
    }
}

/// ページキャッシュ・ファイルキャッシュの予算を一括解決する。
/// `cache_total_mb` はページ+ファイル合計の上限（None = システムRAMの30%を自動使用）。
/// 合計を PAGE_CACHE_SHARE_PCT : FILE_CACHE_SHARE_PCT で分配する。
/// 返り値: (page_max, page_min, file_max)
pub fn resolve_cache_budgets(cache_total_mb: Option<u64>) -> (usize, usize, usize) {
    let total_mb = cache_total_mb.unwrap_or_else(default_cache_total_mb);
    let total_bytes = (total_mb as usize) * MB;

    let page_max = total_bytes * PAGE_CACHE_SHARE_PCT / 100;
    let file_max = total_bytes * FILE_CACHE_SHARE_PCT / 100;
    let page_min = page_max * MIN_RATIO_PCT / 100;
    (page_max, page_min, file_max)
}
/// FileCache が保持する「準備済みデータ」。7z(および将来のtar)はソリッド圧縮で
/// ランダムアクセスできないため、開いた時点で全画像エントリを展開したもの(`Extracted`)を
/// 1アーカイブにつき1回だけ作り、ZIP/生画像は従来通りの生バイト列(`Raw`)を持つ。
/// どちらも `Arc` でスレッド間に安く共有する。
#[derive(Clone)]
pub enum FileCacheEntry {
    Raw(Arc<[u8]>),
    /// 型自体は std のみ（展開関数のみ 7z/tar 依存）。両 feature 無効時のみ未構築のデッドコード。
    #[cfg_attr(not(any(feature = "fmt-7z", feature = "fmt-tar")), allow(dead_code))]
    Extracted(Arc<HashMap<String, Vec<u8>>>),
}

impl FileCacheEntry {
    fn size_bytes(&self) -> usize {
        match self {
            Self::Raw(b) => b.len(),
            Self::Extracted(m) => m.values().map(|v| v.len()).sum(),
        }
    }
}

// ワーカースレッドへのロード要求
pub struct LoadRequest {
    pub archive_path: PathBuf,
    pub index: usize,
    pub entry_name: String,
    /// true のとき archive_path は ZIP ではなく生画像ファイル（entry_name 不使用）
    pub is_raw_file: bool,
    /// FileCache ヒット時の準備済みデータ。Some のときディスクI/Oも7z展開もスキップできる。
    pub file_cache_entry: Option<FileCacheEntry>,
    /// フェーズ6: デコード時の表示ターゲットサイズ上限（拡大はしない）。
    /// None は無制限（zoom_actual時、原寸）を意味する。
    pub target_size: Option<(u32, u32)>,
    /// 項目(D): Exif Orientation自動回転をデコード時に適用するか（ViewerConfigから都度取得）。
    pub exif_enabled: bool,
    /// 表示解像度・Orientation等、デコード条件の世代。結果回収時に現行世代と一致しない
    /// 結果を破棄し、リサイズ前の遅い要求が新しいキャッシュを上書きするのを防ぐ。
    pub generation: u64,
}

/// ワーカースレッド内で保持する開きっぱなしアーカイブ（ディスク版・メモリ版を統合）
enum OpenArchive {
    Disk(zip::ZipArchive<std::fs::File>),
    Mem(zip::ZipArchive<std::io::Cursor<Arc<[u8]>>>),
    /// 7z/tar のソリッド圧縮は開いた時点で画像を一括展開済み。
    /// ページ送りは再展開せずここから引く。
    /// `Arc`はFileCache側が保持するものと共有しており、スレッドごとの再展開が起きない。
    /// 型自体は std のみ（展開関数のみ 7z/tar 依存）なので variant は常時持ち、
    /// 両 feature 無効時のみ未構築のデッドコードとして許容する。
    #[cfg_attr(not(any(feature = "fmt-7z", feature = "fmt-tar")), allow(dead_code))]
    Extracted(Arc<HashMap<String, Vec<u8>>>),
}

/// FileCache ミス時にディスクからアーカイブを開く（zipはランダムアクセス、7z/tarは一括展開）。
/// spawn_worker と spawn_entry_thumb_worker の FileCache ミス経路の共通処理。
/// 通常は FileCache 側が先出しするためミスはほぼ発生しない安全弁。
fn open_archive_from_disk(path: &std::path::Path) -> Option<OpenArchive> {
    match crate::fs::archive::detect_format(path) {
        #[cfg(feature = "fmt-7z")]
        crate::fs::archive::ArchiveFormat::SevenZ => {
            let map = Arc::new(crate::fs::archive::extract_all_images_7z_path(path));
            Some(OpenArchive::Extracted(map))
        }
        #[cfg(feature = "fmt-tar")]
        crate::fs::archive::ArchiveFormat::Tar => {
            let map = Arc::new(crate::fs::archive::extract_all_images_tar_path(path));
            Some(OpenArchive::Extracted(map))
        }
        crate::fs::archive::ArchiveFormat::Zip => std::fs::File::open(path)
            .ok()
            .and_then(|f| zip::ZipArchive::new(f).ok())
            .map(OpenArchive::Disk),
    }
}

impl OpenArchive {
    fn load_page(&mut self, entry_name: &str, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> Option<PageContent> {
        match self {
            Self::Disk(a) => load_page_content(a, entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
            Self::Mem(a)  => load_page_content(a, entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
            Self::Extracted(map) => {
                let buf = map.get(entry_name)?;
                decode_bytes_to_content(buf, entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled)
            }
        }
    }
}

/// OCR用: 表示解像度に依存しない原寸デコード。通常のページキャッシュ(target_size付き、
/// ビューアー窓の実描画サイズに縮小される)とは別経路で、アーカイブから直接1ページぶんだけ
/// 都度デコードする。呼び出し頻度が低いOCR専用なので通常のprefetch/キャッシュ経路には乗せない。
/// アニメーションページは対象外（先頭フレームのみでは意味が薄いため）。
pub fn decode_full_res_static_page(
    archive_path: &std::path::Path,
    entry_name: &str,
    filter: image::imageops::FilterType,
    cache_budget_bytes: usize,
    ring_bounds: (usize, usize),
    frame_hard_limit_bytes: usize,
    exif_enabled: bool,
) -> Option<image::RgbaImage> {
    let mut archive = open_archive_from_disk(archive_path)?;
    match archive.load_page(entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, None, exif_enabled)? {
        PageContent::Static(rgba) => Some(rgba),
        PageContent::Animated(_) => None,
    }
}

/// ページキャッシュの値型。静止画とアニメーションを統一して扱う。
/// アニメーション(GIF/APNG/AVIF/WebP)は全フレーム一括保持をやめ、
/// 逐次デコード+リングバッファ(`RingAnimation`)で保持する（フェーズ3/3.5）。
pub enum PageContent {
    Static(image::RgbaImage),
    Animated(Arc<RingAnimation>),
}

// ワーカースレッドからの結果
pub struct LoadResult {
    pub archive_path: PathBuf,
    pub index: usize,
    pub outcome: DecodeJobOutcome<PageContent>,
    pub generation: u64,
}

/// バックグラウンドデコードワーカーを `num_threads` 本起動する。
/// `cache_budget_bytes` はページキャッシュの予算（PageCache::max_bytes）。
/// これを超えて展開されるアニメーションは先頭フレームのみの静止画にフォールバックする。
/// `ring_bounds` はフェーズ4のリング先読み枚数の(下限, 上限)。
/// `frame_hard_limit_bytes` はフェーズ5: 1フレームあたりの生デコードサイズ上限。超過フレームはその場で縮小する。
/// 返り値: (要求送信側, 結果受信側)
pub fn spawn_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize) -> (DecodeJobQueue<LoadRequest>, mpsc::Receiver<LoadResult>) {
    let job_queue = DecodeJobQueue::<LoadRequest>::new(0);
    let (res_tx, res_rx) = mpsc::channel::<LoadResult>();

    for _ in 0..num_threads {
        let worker_queue = job_queue.clone();
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // 直前に開いたアーカイブをキープオープンする（ディスク版・メモリ版を統合）
            let mut open_archive: Option<(PathBuf, OpenArchive)> = None;

            loop {
                let scheduled = match worker_queue.wait_take() {
                    Some(job) => job,
                    None => break,
                };
                if scheduled.is_cancelled() {
                    worker_queue.finish(scheduled.id);
                    continue;
                }
                let job_id = scheduled.id;
                let req = scheduled.payload;

                let target_size = req.target_size;
                let exif_enabled = req.exif_enabled;
                let content = if req.is_raw_file {
                    match req.file_cache_entry {
                        Some(FileCacheEntry::Raw(bytes)) => {
                            load_raw_content_from_bytes(&bytes, &req.archive_path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled)
                        }
                        _ => load_raw_file_content(&req.archive_path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
                    }
                } else if let Some(FileCacheEntry::Extracted(map)) = req.file_cache_entry.clone() {
                    // FileCache が既に展開済み(7z等): スレッドローカルの再展開はせず共有Arcを使う
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Extracted(_))
                    });
                    if !is_same {
                        open_archive = Some((req.archive_path.clone(), OpenArchive::Extracted(map)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled))
                } else if let Some(FileCacheEntry::Raw(bytes)) = req.file_cache_entry {
                    // FileCache ヒット(ZIP): メモリからアーカイブを開く
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Mem(_))
                    });
                    if !is_same {
                        open_archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                            .ok()
                            .map(|a| (req.archive_path.clone(), OpenArchive::Mem(a)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled))
                } else {
                    // FileCache ミス: ディスクから開く（従来の動作。ソリッド形式はここに来た場合のみ
                    // スレッドローカルに展開する安全弁で、通常はFileCache側の先出しにより
                    // ほぼ発生しない）
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Disk(_) | OpenArchive::Extracted(_))
                    });
                    if !is_same {
                        open_archive = open_archive_from_disk(&req.archive_path)
                            .map(|a| (req.archive_path.clone(), a));
                    }
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled))
                };

                if worker_queue.finish(job_id) {
                    let outcome = match content {
                        Some(content) => DecodeJobOutcome::Ready(content),
                        None => DecodeJobOutcome::Failed,
                    };
                    let _ = res_tx.send(LoadResult {
                        archive_path: req.archive_path,
                        index: req.index,
                        outcome,
                        generation: req.generation,
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(8));
                }
            }
        });
    }

    (job_queue, res_rx)
}

fn to_fir_alg(filter: image::imageops::FilterType) -> ResizeAlg {
    match filter {
        image::imageops::FilterType::Nearest    => ResizeAlg::Nearest,
        image::imageops::FilterType::Triangle   => ResizeAlg::Convolution(FirFilter::Bilinear),
        image::imageops::FilterType::CatmullRom => ResizeAlg::Convolution(FirFilter::CatmullRom),
        image::imageops::FilterType::Gaussian   => ResizeAlg::Convolution(FirFilter::Bilinear),
        image::imageops::FilterType::Lanczos3   => ResizeAlg::Convolution(FirFilter::Lanczos3),
    }
}

// RGB 画像は RGB のまま resize し、小さくなった出力だけ RGBA に変換する。
// デバッグモードでは大きな入力を to_rgba8() するコストが支配的になるため。
fn fir_resize(img: image::DynamicImage, nw: u32, nh: u32, filter: image::imageops::FilterType) -> image::RgbaImage {
    let options = ResizeOptions::new().resize_alg(to_fir_alg(filter));
    match img {
        image::DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let src = FirImage::from_vec_u8(w, h, rgb.into_raw(), PixelType::U8x3).unwrap();
            let mut dst = FirImage::new(nw, nh, PixelType::U8x3);
            Resizer::new().resize(&src, &mut dst, &options).unwrap();
            let rgb_out = image::RgbImage::from_raw(nw, nh, dst.into_vec()).unwrap();
            image::DynamicImage::ImageRgb8(rgb_out).to_rgba8()
        }
        _ => {
            let rgba = img.into_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let src = FirImage::from_vec_u8(w, h, rgba.into_raw(), PixelType::U8x4).unwrap();
            let mut dst = FirImage::new(nw, nh, PixelType::U8x4);
            Resizer::new().resize(&src, &mut dst, &options).unwrap();
            image::RgbaImage::from_raw(nw, nh, dst.into_vec()).unwrap()
        }
    }
}

/// 生画像ファイルを PageContent としてデコードする（ZIP不使用）
fn load_raw_file_content(path: &std::path::Path, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> Option<PageContent> {
    let buf = std::fs::read(path).ok()?;
    load_raw_content_from_bytes(&buf, path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled)
}

/// GIF/APNG/AVIF/WebP（フェーズ3/3.5）をリングバッファ方式でデコードし PageContent に変換する。
/// 実質1フレームしか無い場合は静止画として扱う。対象フォーマットでない場合は None。
fn decode_ring_anim(buf: &[u8], format: AnimFormat, filter: image::imageops::FilterType, ring_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> Option<PageContent> {
    match RingAnimation::from_source(format, std::sync::Arc::from(buf), filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled) {
        RingDecodeOutcome::NotThisFormat => None,
        RingDecodeOutcome::SingleFrame(frame) => {
            Some(PageContent::Static(frame.image))
        }
        RingDecodeOutcome::Animated(ring) => {
            Some(PageContent::Animated(Arc::new(ring)))
        }
    }
}

/// 拡張子（ドットなし小文字）からアニメーションデコードを試みて PageContent を返す。
/// 対象外の拡張子や静止画は None。
/// `cache_budget_bytes` はページキャッシュ全体の予算（page_max）。フェーズ4では
/// その一部（ANIM_RING_BUDGET_PCT）をアニメ1本のリング予算として使う。
/// `target_size` はフェーズ6: デコード時の表示上限（Noneは無制限=原寸）。
fn decode_anim_from_ext(buf: &[u8], ext: &str, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> Option<PageContent> {
    let ring_budget_bytes = cache_budget_bytes * ANIM_RING_BUDGET_PCT / 100;
    match ext {
        "gif"  => decode_ring_anim(buf, AnimFormat::Gif, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
        "png"  => decode_ring_anim(buf, AnimFormat::Apng, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
        "avif" => decode_ring_anim(buf, AnimFormat::Avif, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
        "webp" => decode_ring_anim(buf, AnimFormat::Webp, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled),
        _      => None,
    }
}

/// バイト列から生画像を PageContent としてデコードする（FileCache ヒット時用）
fn load_raw_content_from_bytes(buf: &[u8], path: &std::path::Path, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> Option<PageContent> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if let Some(c) = decode_anim_from_ext(buf, &ext, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled) {
        return Some(c);
    }

    let img = crate::fs::archive::decode_image_bytes(buf, exif_enabled)?;
    Some(PageContent::Static(resize_for_display(img, filter, target_size)))
}

/// アーカイブエントリを PageContent としてデコードする。GIF/WebP/APNGはアニメーション展開、それ以外は静止画。
fn load_page_content<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
    filter: image::imageops::FilterType,
    cache_budget_bytes: usize,
    ring_bounds: (usize, usize),
    frame_hard_limit_bytes: usize,
    target_size: Option<(u32, u32)>,
    exif_enabled: bool,
) -> Option<PageContent> {
    let (buf, display_name) = crate::fs::archive::load_bytes_from_archive(archive, entry_name)?;
    decode_bytes_to_content(&buf, &display_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled)
}

/// 展開済みバイト列を PageContent としてデコードする（ZIP/7z共通処理）。
/// GIF/WebP/APNGはアニメーション展開、それ以外は静止画。
fn decode_bytes_to_content(
    buf: &[u8],
    entry_name: &str,
    filter: image::imageops::FilterType,
    cache_budget_bytes: usize,
    ring_bounds: (usize, usize),
    frame_hard_limit_bytes: usize,
    target_size: Option<(u32, u32)>,
    exif_enabled: bool,
) -> Option<PageContent> {
    let lower = entry_name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

    if let Some(c) = decode_anim_from_ext(buf, ext, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size, exif_enabled) {
        return Some(c);
    }

    let img = crate::fs::archive::decode_image_bytes(buf, exif_enabled)?;
    Some(PageContent::Static(resize_for_display(img, filter, target_size)))
}

/// `target` に縮小する（拡大はしない）。`target` が None のときは無制限（原寸のまま）。
/// フェーズ6: 従来の固定上限(1920x1080)から、リサイズ再デコード対応のため呼び出し側指定に変更。
pub fn resize_for_display(img: image::DynamicImage, filter: image::imageops::FilterType, target: Option<(u32, u32)>) -> image::RgbaImage {
    let Some((tw, th)) = target else {
        return img.to_rgba8();
    };
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = crate::anim::fit_within(w, h, tw, th);
    if (nw, nh) != (w, h) {
        fir_resize(img, nw, nh, filter)
    } else {
        img.to_rgba8()
    }
}

// ── フェーズ3: リングバッファ式アニメーション（GIF/APNG/AVIF）───────────────────

/// `RingAnimation::from_source` の結果。
enum RingDecodeOutcome {
    /// このフォーマットのデコーダとして構築できなかった（例: apng拡張子ではない通常PNG）。
    NotThisFormat,
    /// 実質1フレームしか無い。呼び出し側は静止画として扱う。
    SingleFrame(AnimFrame),
    /// 2フレーム以上ある本物のアニメーション。
    Animated(RingAnimation),
}

struct RingAnimState {
    decoder: Option<SequentialAnimDecoder>,
    ring: FrameRingBuffer,
    /// 次に decoder.next_frame() で得られるフレームに割り振るインデックス
    next_decode_index: usize,
    /// アニメ判定のために同期デコード済みだが、まだ表示解像度へ変換していないframe1。
    /// 初期ページロードをframe1の重いリサイズで塞がないため、最初の要求時にパイプラインへ渡す。
    pending_raw: Option<RawAnimFrame>,
    resize_to: Option<(u32, u32)>,
    resize_epoch: u64,
    /// リサイズで旧サイズの未来フレームを捨てた直後だけ、次の新epoch完成フレームへの
    /// 欠番ジャンプを許可する。
    resize_transition_from: Option<usize>,
    source_dimensions: (u32, u32),
    ring_budget_bytes: usize,
    ring_bounds: (usize, usize),
    filter: image::imageops::FilterType,
    /// フェーズ5: 1フレームあたりの生デコードサイズ上限（超過フレームはその場で縮小）
    frame_hard_limit_bytes: usize,
}

struct RawAnimFrame {
    index: usize,
    frame: AnimFrame,
    source_size: (u32, u32),
    decode_elapsed: std::time::Duration,
}

fn commit_resized_frame(
    state: &Mutex<RingAnimState>,
    expected_epoch: u64,
    index: usize,
    frame: AnimFrame,
) -> bool {
    let mut state = state.lock().unwrap();
    if state.resize_epoch != expected_epoch {
        return false;
    }
    state.ring.push(index, frame);
    true
}

#[derive(Default)]
struct AnimationPipelineCommand {
    requested_through: Option<usize>,
    shutdown: bool,
}

struct AnimationPipelineControl {
    command: Mutex<AnimationPipelineCommand>,
    wake: Condvar,
    /// 通常は順序を守る小容量FIFO。リサイズ遅延検出後は最新1枚へ畳み込む。
    raw_queue: Mutex<VecDeque<RawAnimFrame>>,
    raw_wake: Condvar,
    raw_space: Condvar,
    drop_stale_raw: AtomicBool,
}

/// 表示世代とは独立した、RingAnimationインスタンスそのものの同一性。
/// リサイズ世代が変わっても同じ値を維持し、本当に再生成された場合だけ変わる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AnimationInstanceId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnimationFrameDiagnostic {
    Busy,
    Missing {
        ring_range: Option<(usize, usize)>,
        next_decode_index: usize,
        capacity: usize,
        resize_epoch: u64,
    },
}

#[cfg(test)]
impl AnimationInstanceId {
    pub(crate) fn for_test(value: u64) -> Self {
        Self(value)
    }
}

static NEXT_ANIMATION_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn next_animation_instance_id() -> AnimationInstanceId {
    AnimationInstanceId(NEXT_ANIMATION_INSTANCE_ID.fetch_add(1, Ordering::Relaxed))
}

impl AnimationPipelineControl {
    fn push_raw(&self, raw: RawAnimFrame) -> bool {
        let mut queue = self.raw_queue.lock().unwrap();
        loop {
            if self.command.lock().unwrap().shutdown {
                return false;
            }
            if self.drop_stale_raw.load(Ordering::Acquire) {
                queue.clear();
                queue.push_back(raw);
                self.raw_wake.notify_one();
                return true;
            }
            if queue.len() < ANIM_RAW_QUEUE_CAPACITY {
                queue.push_back(raw);
                self.raw_wake.notify_one();
                return true;
            }
            queue = self.raw_space.wait(queue).unwrap();
        }
    }
}

/// 全フレームを一括保持せず、逐次デコード+リングバッファで保持するアニメーション。
/// 再生は前進のみを前提とし、デコーダが終端(None)を返した時点がループ境界の合図になる。
/// その際は `restart()` でデコーダを先頭から作り直す（この再デコードによる一瞬のフリーズは許容する）。
pub struct RingAnimation {
    instance_id: AnimationInstanceId,
    state: Arc<Mutex<RingAnimState>>,
    /// 初回フレーム要求時にだけ起動する、アニメ専用の常駐デコード/リサイズパイプライン。
    pipeline_started: AtomicBool,
    pipeline: Arc<AnimationPipelineControl>,
    format: AnimFormat,
    /// PageCache への計上額（リング容量 × リサイズ後フレームサイズ）。構築時に確定し不変。
    /// 挿入時点の実常駐（2フレーム分）で計上すると、再生でリングが容量まで育ったとき
    /// 帳簿が実態を大幅に過小評価して evict が動かなくなるため、
    /// 「育ちうる最大量」を予約方式で先取り計上する（常に 帳簿 ≥ 実常駐 を保証）。
    reserved_bytes: AtomicUsize,
}

impl RingAnimation {
    pub(crate) fn instance_id(&self) -> AnimationInstanceId {
        self.instance_id
    }

    /// `ring_budget_bytes` はこのアニメ1本に割り当てるリング予算、`ring_bounds` は(下限, 上限)の先読み枚数。
    /// `frame_hard_limit_bytes` はフェーズ5: 1フレームの生デコードサイズ上限。frame0・中間フレームの
    /// どちらも超過時はそのフレームだけ縮小して継続する（同一アニメ内で解像度が変則的なファイル対策）。
    /// フレーム0のバイト数から通常表示用のリサイズ先・リング容量を算出し、以降の再生中は変更しない（フェーズ4）。
    /// `target_size` はフェーズ6: 表示上限（Noneは無制限=原寸のまま）。
    fn from_source(format: AnimFormat, source: Arc<[u8]>, filter: image::imageops::FilterType, ring_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>, exif_enabled: bool) -> RingDecodeOutcome {
        // WebPのExif Orientationは静止画（1フレームしかない）確定時のみ適用する。アニメ全体への
        // 一律適用は未対応のため、先にframe0だけ回転してしまうと「先頭フレームだけ回転済み・
        // 残りは無回転」という不整合が起きる。そのため判定（frame1の有無）より前には触らない。
        let webp_orientation = if format == AnimFormat::Webp && exif_enabled {
            crate::fs::archive::decode::webp_exif_orientation(&source)
        } else {
            image::metadata::Orientation::NoTransforms
        };

        let mut decoder = match SequentialAnimDecoder::new(format, source) {
            Some(d) => d,
            None => return RingDecodeOutcome::NotThisFormat,
        };
        let frame0 = match decoder.next_frame() {
            Some(f) => f,
            None => return RingDecodeOutcome::NotThisFormat,
        };
        // AVIFはWebPと違い、向き（Exif/irot/imir）がframe0デコード後のデコーダ状態からしか
        // 読めない（コンテナのRIFF/box自前パースのような事前スキャンができない）ため、
        // frame0が獲れた直後、かつ静止画/アニメ判定と同じ理由でframe1取得より前に確定させる。
        let orientation = if format == AnimFormat::Avif {
            if exif_enabled { decoder.avif_orientation() } else { image::metadata::Orientation::NoTransforms }
        } else {
            webp_orientation
        };
        let frame1 = decoder.next_frame();

        if frame1.is_none() {
            let mut frame0 = frame0;
            if orientation != image::metadata::Orientation::NoTransforms {
                let mut img = image::DynamicImage::ImageRgba8(frame0.image);
                img.apply_orientation(orientation);
                frame0.image = img.into_rgba8();
            }
            let frame0 = Self::guard_frame_size(frame0, frame_hard_limit_bytes, filter, 0);
            let (w, h) = (frame0.image.width(), frame0.image.height());
            let resize_to = target_size.and_then(|(tw, th)| {
                let (nw, nh) = crate::anim::fit_within(w, h, tw, th);
                if (nw, nh) == (w, h) { None } else { Some((nw, nh)) }
            });
            let frame0 = Self::apply_resize(frame0, resize_to, filter);
            return RingDecodeOutcome::SingleFrame(frame0);
        }

        let frame0 = Self::guard_frame_size(frame0, frame_hard_limit_bytes, filter, 0);

        let (w, h) = (frame0.image.width(), frame0.image.height());
        let resize_to = target_size.and_then(|(tw, th)| {
            let (nw, nh) = crate::anim::fit_within(w, h, tw, th);
            if (nw, nh) == (w, h) { None } else { Some((nw, nh)) }
        });

        let frame0 = Self::apply_resize(frame0, resize_to, filter);

        let frame1 = frame1.unwrap();
        let frame1_source_size = frame1.image.dimensions();
        // rawキューへ長時間保持されうるため、表示リサイズは遅延してもhard limitだけは先に適用する。
        let frame1 = Self::guard_frame_size(frame1, frame_hard_limit_bytes, filter, 1);

        // 容量算出はresize後のフレームサイズ基準（実際にリングへ乗るバイト数と一致させるため）。
        let resized_frame_bytes = (frame0.image.width() as usize) * (frame0.image.height() as usize) * 4;
        let (min_frames, max_frames) = ring_bounds;
        let capacity = resolve_ring_capacity(resized_frame_bytes, ring_budget_bytes, min_frames, max_frames);
        let reserved_bytes = capacity.saturating_mul(resized_frame_bytes);
        let mut ring = FrameRingBuffer::new(capacity);
        ring.push(0, frame0);

        let state = RingAnimState {
            decoder: Some(decoder),
            ring,
            next_decode_index: 2,
            pending_raw: Some(RawAnimFrame {
                index: 1,
                frame: frame1,
                source_size: frame1_source_size,
                decode_elapsed: std::time::Duration::ZERO,
            }),
            resize_to,
            resize_epoch: 0,
            resize_transition_from: None,
            source_dimensions: (w, h),
            ring_budget_bytes,
            ring_bounds,
            filter,
            frame_hard_limit_bytes,
        };
        RingDecodeOutcome::Animated(Self {
            instance_id: next_animation_instance_id(),
            state: Arc::new(Mutex::new(state)),
            pipeline_started: AtomicBool::new(false),
            pipeline: Arc::new(AnimationPipelineControl {
                command: Mutex::new(AnimationPipelineCommand::default()),
                wake: Condvar::new(),
                raw_queue: Mutex::new(VecDeque::new()),
                raw_wake: Condvar::new(),
                raw_space: Condvar::new(),
                drop_stale_raw: AtomicBool::new(false),
            }),
            format,
            reserved_bytes: AtomicUsize::new(reserved_bytes),
        })
    }

    /// フレームの生デコードサイズ(リサイズ前、w*h*4)が`hard_limit_bytes`を超える場合、
    /// 収まる比率までそのフレームだけ縮小する（フェーズ5: 同一アニメ内の解像度異常フレーム対策）。
    fn guard_frame_size(frame: AnimFrame, hard_limit_bytes: usize, filter: image::imageops::FilterType, index: usize) -> AnimFrame {
        let (w, h) = (frame.image.width(), frame.image.height());
        let raw_bytes = (w as usize) * (h as usize) * 4;
        if hard_limit_bytes == 0 || raw_bytes <= hard_limit_bytes {
            return frame;
        }
        let scale = ((hard_limit_bytes as f64) / (raw_bytes as f64)).sqrt() as f32;
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
        eprintln!(
            "[cache] anim frame {} auto-downscaled: {}x{} -> {}x{} (raw {}MB > limit {}MB)",
            index, w, h, nw, nh, raw_bytes / MB, hard_limit_bytes / MB,
        );
        let dynamic = image::DynamicImage::ImageRgba8(frame.image);
        AnimFrame { image: fir_resize(dynamic, nw, nh, filter), delay: frame.delay }
    }

    fn apply_resize(frame: AnimFrame, resize_to: Option<(u32, u32)>, filter: image::imageops::FilterType) -> AnimFrame {
        match resize_to {
            None => frame,
            Some((nw, nh)) => {
                let dynamic = image::DynamicImage::ImageRgba8(frame.image);
                AnimFrame { image: fir_resize(dynamic, nw, nh, filter), delay: frame.delay }
            }
        }
    }

    fn resize_raw_frame(
        raw: RawAnimFrame,
        resize_to: Option<(u32, u32)>,
        filter: image::imageops::FilterType,
        frame_hard_limit_bytes: usize,
        format: AnimFormat,
    ) -> (usize, AnimFrame, std::time::Duration) {
        let resize_started = std::time::Instant::now();
        let guarded = Self::guard_frame_size(raw.frame, frame_hard_limit_bytes, filter, raw.index);
        let resized = Self::apply_resize(guarded, resize_to, filter);
        let resize_elapsed = resize_started.elapsed();
        if raw.decode_elapsed >= std::time::Duration::from_millis(8)
            || resize_elapsed >= std::time::Duration::from_millis(8)
        {
            log_perf!(
                "[diag/anim-frame] format={:?} frame={} source={}x{} output={}x{} decode={:.1}ms resize={:.1}ms delay={:.1}ms worker=pipeline",
                format,
                raw.index,
                raw.source_size.0,
                raw.source_size.1,
                resized.image.width(),
                resized.image.height(),
                raw.decode_elapsed.as_secs_f64() * 1000.0,
                resize_elapsed.as_secs_f64() * 1000.0,
                resized.delay.as_secs_f64() * 1000.0,
            );
        }
        (raw.index, resized, resize_elapsed)
    }

    /// index番目のフレームが手に入るまでデコードを進め、見つかったフレームへの参照でfを呼ぶ
    /// (RGBAバッファの不要なコピーを避けるため)。デコーダが終端に達し index が存在しないと
    /// わかった場合は None（呼び出し側はループ境界として扱い `restart()` を呼ぶ）。
    pub fn with_frame<R>(&self, index: usize, f: impl FnOnce(&AnimFrame) -> R) -> Option<R> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(frame) = state.ring.get(index) {
                return Some(f(frame));
            }
            if let Some(raw) = state.pending_raw.take() {
                let resize_to = state.resize_to;
                let filter = state.filter;
                let frame_hard_limit_bytes = state.frame_hard_limit_bytes;
                let (idx, resized, _) = Self::resize_raw_frame(
                    raw,
                    resize_to,
                    filter,
                    frame_hard_limit_bytes,
                    self.format,
                );
                state.ring.push(idx, resized);
                continue;
            }
            if index < state.next_decode_index {
                // 前進専用のためエビクト済みフレームへは戻れない。
                return None;
            }
            let decode_started = std::time::Instant::now();
            let next = state.decoder.as_mut()?.next_frame()?;
            let decode_elapsed = decode_started.elapsed();
            let idx = state.next_decode_index;
            state.next_decode_index += 1;
            let source_size = next.image.dimensions();
            let resize_to = state.resize_to;
            let filter = state.filter;
            let frame_hard_limit_bytes = state.frame_hard_limit_bytes;
            let (_, resized, _) = Self::resize_raw_frame(
                RawAnimFrame { index: idx, frame: next, source_size, decode_elapsed },
                resize_to,
                filter,
                frame_hard_limit_bytes,
                self.format,
            );
            state.ring.push(idx, resized);
        }
    }

    /// UIスレッド用の非ブロッキング参照。バックグラウンド生成中にMutexを待たず、
    /// まだ使える旧フレームを描画し続けられるようにする。
    pub fn try_with_frame<R>(&self, index: usize, f: impl FnOnce(&AnimFrame) -> R) -> Option<R> {
        let state = self.state.try_lock().ok()?;
        state.ring.get(index).map(f)
    }

    /// UIスレッド用: 欠番を許容し、`index`より後で完成済みの最新フレーム番号を返す。
    #[cfg(test)]
    pub fn latest_ready_after(&self, index: usize) -> Option<usize> {
        let state = self.state.try_lock().ok()?;
        state.ring.latest_after(index).map(|(ready_index, _)| ready_index)
    }

    /// 通常再生では次の連番フレームを返す。実測リサイズ遅延によってdrop modeへ
    /// 移行した後だけ、欠番を許容して完成済みの最新フレームを返す。
    pub fn playback_ready_after(&self, index: usize) -> Option<usize> {
        let mut state = self.state.try_lock().ok()?;
        if state.resize_transition_from == Some(index) {
            if let Some((ready_index, _)) = state.ring.latest_after(index) {
                state.resize_transition_from = None;
                return Some(ready_index);
            }
            return None;
        }
        if self.pipeline.drop_stale_raw.load(Ordering::Acquire) {
            state.ring.latest_after(index).map(|(ready_index, _)| ready_index)
        } else {
            let next_index = index.saturating_add(1);
            state.ring.get(next_index).map(|_| next_index)
        }
    }

    /// 現在の出力サイズから決まるリング容量。表示側の先読み要求をこの範囲内に制限する。
    pub fn ring_capacity(&self) -> usize {
        self.state.lock().unwrap().ring.capacity()
    }

    /// フレーム取得失敗時だけ使う非ブロッキング診断。単なるMutex競合と、前進済みで
    /// 要求フレームがリングから消えている状態を区別する。
    pub(crate) fn diagnose_missing_frame(&self, index: usize) -> Option<AnimationFrameDiagnostic> {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(_) => return Some(AnimationFrameDiagnostic::Busy),
        };
        if state.ring.get(index).is_some() {
            return None;
        }
        Some(AnimationFrameDiagnostic::Missing {
            ring_range: state.ring.index_range(),
            next_decode_index: state.next_decode_index,
            capacity: state.ring.capacity(),
            resize_epoch: state.resize_epoch,
        })
    }

    /// ViewerStateを作り直した際、以前のframe_indexが既にevict済みなら、現在リング内で
    /// 取得できる最新フレームを返す。Mutex競合時は次tickで再試行できるようNone。
    pub(crate) fn reconnect_frame_index(&self, preferred: usize) -> Option<usize> {
        let state = self.state.try_lock().ok()?;
        if state.ring.get(preferred).is_some() {
            Some(preferred)
        } else {
            state.ring.index_range().map(|(_, latest)| latest)
        }
    }

    fn ensure_pipeline_started(&self) {
        if self.pipeline_started.compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        ).is_err() {
            return;
        }

        let decode_state = Arc::clone(&self.state);
        let decode_pipeline = Arc::clone(&self.pipeline);
        std::thread::spawn(move || {
            loop {
                let requested_through = {
                    let mut command = decode_pipeline.command.lock().unwrap();
                    loop {
                        if command.shutdown {
                            return;
                        }
                        let next_index = {
                            let state = decode_state.lock().unwrap();
                            state.pending_raw.as_ref().map_or(state.next_decode_index, |raw| raw.index)
                        };
                        if command.requested_through.is_some_and(|target| target >= next_index) {
                            break command.requested_through.unwrap();
                        }
                        command = decode_pipeline.wake.wait(command).unwrap();
                    }
                };

                let (pending, decoder_failed) = {
                    let mut state = decode_state.lock().unwrap();
                    if let Some(raw) = state.pending_raw.take() {
                        (Some(raw), false)
                    } else if state.next_decode_index <= requested_through {
                        let frame_index = state.next_decode_index;
                        let Some(mut decoder) = state.decoder.take() else {
                            continue;
                        };
                        drop(state);

                        let decode_started = std::time::Instant::now();
                        let mut next = decoder.next_frame();
                        if next.is_none() && decoder.restart() {
                            next = decoder.next_frame();
                        }
                        let decode_elapsed = decode_started.elapsed();

                        let mut state = decode_state.lock().unwrap();
                        state.decoder = Some(decoder);
                        match next {
                            Some(frame) => {
                                state.next_decode_index = frame_index + 1;
                                let source_size = frame.image.dimensions();
                                (
                                    Some(RawAnimFrame {
                                        index: frame_index,
                                        frame,
                                        source_size,
                                        decode_elapsed,
                                    }),
                                    false,
                                )
                            }
                            None => (None, true),
                        }
                    } else {
                        (None, false)
                    }
                };

                if decoder_failed {
                    return;
                }
                if let Some(raw) = pending {
                    if decode_pipeline.command.lock().unwrap().shutdown {
                        return;
                    }
                    // 通常は小容量FIFOで順序を守る。resize遅延が検出された後だけ、
                    // 未処理フレームを最新1枚へ畳み込み、高コストなresize前に捨てる。
                    if !decode_pipeline.push_raw(raw) {
                        return;
                    }
                }
            }
        });

        let resize_state = Arc::clone(&self.state);
        let resize_pipeline = Arc::clone(&self.pipeline);
        let format = self.format;
        std::thread::spawn(move || {
            loop {
                let raw = {
                    let mut raw_queue = resize_pipeline.raw_queue.lock().unwrap();
                    loop {
                        if resize_pipeline.command.lock().unwrap().shutdown {
                            return;
                        }
                        if let Some(raw) = raw_queue.pop_front() {
                            resize_pipeline.raw_space.notify_one();
                            break raw;
                        }
                        raw_queue = resize_pipeline.raw_wake.wait(raw_queue).unwrap();
                    }
                };
                let (resize_to, filter, frame_hard_limit_bytes, resize_epoch) = {
                    let state = resize_state.lock().unwrap();
                    (state.resize_to, state.filter, state.frame_hard_limit_bytes, state.resize_epoch)
                };
                let (index, resized, resize_elapsed) = Self::resize_raw_frame(
                    raw,
                    resize_to,
                    filter,
                    frame_hard_limit_bytes,
                    format,
                );
                if resize_elapsed > resized.delay {
                    resize_pipeline.drop_stale_raw.store(true, Ordering::Release);
                    resize_pipeline.raw_space.notify_all();
                }
                if resize_pipeline.command.lock().unwrap().shutdown {
                    continue;
                }
                commit_resized_frame(&resize_state, resize_epoch, index, resized);
            }
        });
    }

    /// `index` が未生成なら、アニメ専用の常駐パイプラインへ生成要求を送る。
    /// 要求は単調な到達点に畳み込み、resize遅延中だけ未処理の生RGBAを最新1枚へ畳み込む。
    /// 終端ではデコーダを巻き戻し、単調増加する表示indexへ次ループの先頭を割り当てる。
    pub fn request_frame(&self, index: usize) -> bool {
        if let Ok(state) = self.state.try_lock() {
            if state.ring.get(index).is_some() {
                return true;
            }
        }
        self.ensure_pipeline_started();
        let mut command = self.pipeline.command.lock().unwrap();
        command.requested_through = Some(command.requested_through.map_or(index, |old| old.max(index)));
        self.pipeline.wake.notify_one();
        false
    }

    /// 【非推奨・呼び出し禁止】`PageCache::update_animation_resize` からのみ呼ばれる。
    /// リサイズ経路は `drop_animation_for_redecode` + 通常再デコードへ移行済み。フェーズ4で撤去予定。
    ///
    /// 同じアニメ担当を維持したまま表示サイズだけ切り替える。
    /// 現在表示中の旧サイズフレームは残し、完成済み未来フレームと待機中rawを破棄する。
    /// 既にresize中の旧epoch結果は、完了時のepoch照合で投入されない。
    #[allow(dead_code)] // フェーズ4で update_animation_resize ごと撤去予定
    pub fn update_resize_target(
        &self,
        target_size: Option<(u32, u32)>,
        current_frame_index: usize,
    ) -> (usize, usize) {
        let (old_reserved, new_reserved, changed) = {
            let mut state = self.state.lock().unwrap();
            let (w, h) = state.source_dimensions;
            let resize_to = target_size.and_then(|(tw, th)| {
                let (nw, nh) = crate::anim::fit_within(w, h, tw, th);
                if (nw, nh) == (w, h) { None } else { Some((nw, nh)) }
            });
            let old_reserved = self.reserved_bytes.load(Ordering::Acquire);
            if state.resize_to == resize_to {
                (old_reserved, old_reserved, false)
            } else {
                state.resize_to = resize_to;
                state.resize_epoch = state.resize_epoch.wrapping_add(1);
                state.resize_transition_from = Some(current_frame_index);
                state.ring.retain_only(current_frame_index);

                let (out_w, out_h) = resize_to.unwrap_or((w, h));
                let frame_bytes = (out_w as usize)
                    .saturating_mul(out_h as usize)
                    .saturating_mul(4);
                let (min_frames, max_frames) = state.ring_bounds;
                let capacity = resolve_ring_capacity(
                    frame_bytes,
                    state.ring_budget_bytes,
                    min_frames,
                    max_frames,
                );
                state.ring.set_capacity(capacity);
                let new_reserved = capacity.saturating_mul(frame_bytes);
                self.reserved_bytes.store(new_reserved, Ordering::Release);
                (old_reserved, new_reserved, true)
            }
        };

        if changed {
            self.pipeline.raw_queue.lock().unwrap().clear();
            self.pipeline.raw_space.notify_all();
        }
        (old_reserved, new_reserved)
    }

    /// ループ境界（最終フレーム→先頭）: デコーダを元データから作り直す。
    #[cfg(test)]
    pub fn restart(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(decoder) = state.decoder.as_mut() else { return false };
        if decoder.restart() {
            state.ring.clear();
            state.next_decode_index = 0;
            state.pending_raw = None;
            true
        } else {
            false
        }
    }

    /// 現在リングバッファに乗っている分だけの推定バイト数（アニメ全体ではない）。
    /// テスト専用（実常駐が予約額 reserved_bytes を超えないことの検証用）。
    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.state.lock().unwrap().ring.total_bytes()
    }

    /// PageCache への計上額（リング容量 × リサイズ後フレームサイズ、構築時に確定）。
    /// insert/evict の両方でこの同一値を使うことで帳簿の足し引きが対称になる。
    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes.load(Ordering::Acquire)
    }
}

impl Drop for RingAnimation {
    fn drop(&mut self) {
        let mut command = self.pipeline.command.lock().unwrap();
        command.shutdown = true;
        self.pipeline.wake.notify_all();
        self.pipeline.raw_wake.notify_all();
        self.pipeline.raw_space.notify_all();
    }
}

pub struct PageCache {
    /// 表示解像度の世代とは独立して、1ページにつき1つだけ保持するアニメ担当。
    /// リサイズ世代が進んでも同じ RingAnimation を返し続ける。
    animations: HashMap<(PathBuf, usize), PageContent>,
    entries: HashMap<(PathBuf, usize, u64), PageContent>,
    total_bytes: usize,
    max_bytes: usize,
    min_bytes: usize,
    /// LRU 予算を超える単一アイテムを表示のためだけに保持するスロット（1件のみ）
    bypass: Option<((PathBuf, usize, u64), PageContent)>,
    /// 予算を単体で超えるアニメ用の、世代非依存bypassスロット。
    animation_bypass: Option<((PathBuf, usize), PageContent)>,
    /// 一度でも予算超過(bypass)と判定された(path, index)の記憶。
    /// bypass スロットから追い出された後も先読みが再要求しないようにするためのもので、
    /// 中身は保持しない（キャッシュを汚染しない）。
    known_bypass: HashSet<(PathBuf, usize, u64)>,
    known_animation_bypass: HashSet<(PathBuf, usize)>,
}

impl PageCache {
    pub fn new(max_bytes: usize, min_bytes: usize) -> Self {
        Self {
            animations: HashMap::new(),
            entries: HashMap::new(),
            total_bytes: 0,
            max_bytes,
            min_bytes,
            bypass: None,
            animation_bypass: None,
            known_bypass: HashSet::new(),
            known_animation_bypass: HashSet::new(),
        }
    }

    pub fn total_bytes(&self) -> usize { self.total_bytes }
    pub fn max_bytes(&self) -> usize { self.max_bytes }

    /// 表示解像度の世代変更時に、旧ターゲットで作られた全ページを破棄する。
    /// FileCache（圧縮済み/展開済みの元データ）は別層なので影響しない。
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.animations.clear();
        self.entries.clear();
        self.total_bytes = 0;
        self.bypass = None;
        self.animation_bypass = None;
        self.known_bypass.clear();
        self.known_animation_bypass.clear();
    }

    pub fn contains(&self, path: &PathBuf, index: usize, generation: u64) -> bool {
        self.contains_animation(path, index)
            || self.entries.contains_key(&(path.clone(), index, generation))
            || self.bypass.as_ref()
                .map_or(false, |((bp, bi, bg), _)| bp == path && *bi == index && *bg == generation)
    }

    /// 表示世代に依存しないアニメ担当が、このページに既に存在するか。
    pub fn contains_animation(&self, path: &PathBuf, index: usize) -> bool {
        self.animations.contains_key(&(path.clone(), index))
            || self.animation_bypass.as_ref()
                .is_some_and(|((bp, bi), _)| bp == path && *bi == index)
    }

    /// 【非推奨・呼び出し禁止】その場リサイズ機構。前進専用デコーダの `next_decode_index` を
    /// 巻き戻さないまま `ring.retain_only()` でリングを切り詰めるため、再生位置より先へ
    /// デコード要求が届かずアニメが静止画化するバグがあった。リサイズ/原寸切替/フルスクリーンは
    /// `drop_animation_for_redecode()` + 通常再デコードへ移行済み。フェーズ4でこの関数と
    /// `RingAnimation::update_resize_target` ごと撤去予定。
    ///
    /// 世代非依存のアニメ担当へ新しい表示上限を通知し、通常キャッシュに計上している
    /// 予約額を差分更新する。bypassアニメは元からtotal_bytesの帳簿外なので差分計上しない。
    #[allow(dead_code)] // フェーズ4で update_resize_target ごと撤去予定
    pub fn update_animation_resize(
        &mut self,
        path: &PathBuf,
        index: usize,
        target_size: Option<(u32, u32)>,
        current_frame_index: usize,
    ) -> bool {
        let key = (path.clone(), index);
        let (ring, accounted) = if let Some(PageContent::Animated(ring)) = self.animations.get(&key) {
            (Arc::clone(ring), true)
        } else if let Some((_, PageContent::Animated(ring))) = self.animation_bypass.as_ref()
            .filter(|((p, i), _)| p == path && *i == index)
        {
            (Arc::clone(ring), false)
        } else {
            return false;
        };

        let (old_reserved, new_reserved) =
            ring.update_resize_target(target_size, current_frame_index);
        if accounted {
            if new_reserved >= old_reserved {
                self.total_bytes = self.total_bytes.saturating_add(new_reserved - old_reserved);
            } else {
                self.total_bytes = self.total_bytes.saturating_sub(old_reserved - new_reserved);
            }
        }
        true
    }

    /// リサイズ/原寸切替/フルスクリーンでの再デコード時に、稼働中のアニメ担当を破棄する。
    /// `update_animation_resize`（その場リサイズ）と違い、呼び出し元はこの直後に新しい
    /// `target_size` で通常の `LoadRequest` を投げ、別 `instance_id` の `RingAnimation` を
    /// 先頭フレームから作り直させる（`insert_animation` の `contains_animation` ガードに
    /// 弾かれないよう、再デコード結果が届く前にここで席を空けておく）。
    /// 通常キャッシュへ計上済みの予約額を `total_bytes` から戻し、bypassスロット・既知bypass
    /// 記憶も掃除する。アニメを持たない `(path, index)` に対しては何もしない。
    pub fn drop_animation_for_redecode(&mut self, path: &PathBuf, index: usize) {
        if let Some(content) = self.animations.remove(&(path.clone(), index)) {
            self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
        }
        if self.animation_bypass.as_ref().is_some_and(|((p, i), _)| p == path && *i == index) {
            self.animation_bypass = None;
        }
        self.known_animation_bypass.remove(&(path.clone(), index));
    }

    /// この(path, index)が過去に予算超過(bypass)と判定されたことがあるか。
    /// 先読みウィンドウが同じページを何度もデコードし直すループを防ぐために使う。
    pub fn is_known_bypass(&self, path: &PathBuf, index: usize, generation: u64) -> bool {
        self.known_animation_bypass.contains(&(path.clone(), index))
            || self.known_bypass.contains(&(path.clone(), index, generation))
    }

    /// preparingが完成済みなら優先し、未完成ならactiveへフォールバックする。
    pub fn get_best(
        &self,
        path: &PathBuf,
        index: usize,
        active: u64,
        preparing: Option<u64>,
    ) -> Option<(u64, &PageContent)> {
        self.get_animation(path, index)
            .map(|content| (preparing.unwrap_or(active), content))
            .or_else(|| preparing
            .and_then(|generation| self.get_generation(path, index, generation).map(|c| (generation, c)))
            .or_else(|| self.get_generation(path, index, active).map(|c| (active, c))))
    }

    fn get_animation(&self, path: &PathBuf, index: usize) -> Option<&PageContent> {
        self.animations.get(&(path.clone(), index)).or_else(|| {
            self.animation_bypass.as_ref().and_then(|((bp, bi), content)| {
                if bp == path && *bi == index { Some(content) } else { None }
            })
        })
    }

    fn get_generation(&self, path: &PathBuf, index: usize, generation: u64) -> Option<&PageContent> {
        self.entries.get(&(path.clone(), index, generation)).or_else(|| {
            self.bypass.as_ref().and_then(|((bp, bi, bg), c)| {
                if bp == path && *bi == index && *bg == generation { Some(c) } else { None }
            })
        })
    }

    /// フェーズ6: 再デコードのため既存エントリを強制的に破棄する（bypassスロットも対象）。
    /// 次の insert() で新しいデコード結果を通常どおり入れ直す前提。
    #[cfg(test)]
    pub fn remove(&mut self, path: &PathBuf, index: usize, generation: u64) {
        if let Some(content) = self.animations.remove(&(path.clone(), index)) {
            self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
        }
        if self.animation_bypass.as_ref().is_some_and(|((p, i), _)| p == path && *i == index) {
            self.animation_bypass = None;
        }
        self.known_animation_bypass.remove(&(path.clone(), index));
        if let Some(content) = self.entries.remove(&(path.clone(), index, generation)) {
            self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
        }
        if let Some(((bp, bi, bg), _)) = &self.bypass {
            if bp == path && *bi == index && *bg == generation {
                self.bypass = None;
            }
        }
        self.known_bypass.remove(&(path.clone(), index, generation));
    }

    /// 新GPUテクスチャへの交換後、同じページの古いCPU世代だけを解放する。
    pub fn remove_older_versions(&mut self, path: &PathBuf, index: usize, keep_generation: u64) {
        let stale: Vec<_> = self.entries.keys()
            .filter(|(p, i, g)| p == path && *i == index && *g != keep_generation)
            .cloned()
            .collect();
        for (p, i, g) in stale {
            if let Some(content) = self.entries.remove(&(p, i, g)) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
        if self.bypass.as_ref().is_some_and(|((p, i, g), _)| {
            p == path && *i == index && *g != keep_generation
        }) {
            self.bypass = None;
        }
        self.known_bypass.retain(|(p, i, g)| {
            p != path || *i != index || *g == keep_generation
        });
    }

    /// 連続リサイズ時にactive/preparing以外の世代を一括破棄する。
    pub fn retain_generations(&mut self, active: u64, preparing: Option<u64>) {
        let stale: Vec<_> = self.entries.keys()
            .filter(|(_, _, g)| *g != active && Some(*g) != preparing)
            .cloned()
            .collect();
        for key in stale {
            if let Some(content) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
        if self.bypass.as_ref().is_some_and(|((_, _, g), _)| {
            *g != active && Some(*g) != preparing
        }) {
            self.bypass = None;
        }
        self.known_bypass.retain(|(_, _, g)| *g == active || Some(*g) == preparing);
    }

    /// 項目(D): Exif Orientation ON/OFF切替時に、指定アーカイブの全エントリ（bypassスロット・
    /// known_bypass記憶も含む）を破棄する。ページ単位の`remove`と違い、可視ページに限らず
    /// アーカイブ全体を対象にする（先読み済みページが古いOrientationのまま残るのを防ぐため）。
    pub fn remove_all_for_path(&mut self, path: &PathBuf) {
        let stale_animations: Vec<_> = self.animations.keys()
            .filter(|(p, _)| p == path)
            .cloned()
            .collect();
        for key in stale_animations {
            if let Some(content) = self.animations.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
        let stale_keys: Vec<(PathBuf, usize, u64)> = self.entries.keys()
            .filter(|(p, _, _)| p == path)
            .cloned()
            .collect();
        for key in stale_keys {
            if let Some(content) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
        if let Some(((bp, _, _), _)) = &self.bypass {
            if bp == path {
                self.bypass = None;
            }
        }
        if self.animation_bypass.as_ref().is_some_and(|((p, _), _)| p == path) {
            self.animation_bypass = None;
        }
        self.known_bypass.retain(|(p, _, _)| p != path);
        self.known_animation_bypass.retain(|(p, _)| p != path);
    }

    /// キャッシュに追加する。予算超過時は最遠エントリを evict する。
    /// 単一アイテムが予算全体を超える場合は LRU を汚さず bypass スロットに格納する。
    pub fn insert(
        &mut self,
        path: PathBuf,
        index: usize,
        generation: u64,
        content: PageContent,
        current_path: &PathBuf,
        current_index: usize,
    ) {
        if matches!(content, PageContent::Animated(_)) {
            self.insert_animation(path, index, content, current_path, current_index);
            return;
        }

        let incoming = content_bytes(&content);

        // 同一世代・同一ページの再投入は置換として扱い、帳簿を二重加算しない。
        // 通常はジョブ重複排除で起きないが、結果配送と再要求が競合しても安全にする。
        if let Some(previous) = self.entries.remove(&(path.clone(), index, generation)) {
            self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&previous));
        }

        if incoming >= self.max_bytes {
            eprintln!(
                "[cache] bypass: {:?}[{}] {}MB > budget {}MB",
                path, index,
                incoming / MB,
                self.max_bytes / MB,
            );
            self.known_bypass.insert((path.clone(), index, generation));
            self.bypass = Some(((path, index, generation), content));
            return;
        }

        // 現在位置が変わっていたら stale な bypass エントリを解放する
        if let Some(((bp, bi, _), _)) = &self.bypass {
            if bp != current_path || *bi != current_index {
                self.bypass = None;
            }
        }

        while self.total_bytes + incoming > self.max_bytes
            && self.total_bytes > self.min_bytes
            && !self.entries.is_empty()
        {
            self.evict_furthest(current_path, current_index);
        }
        self.total_bytes += incoming;
        self.entries.insert((path, index, generation), content);
    }

    fn insert_animation(
        &mut self,
        path: PathBuf,
        index: usize,
        content: PageContent,
        current_path: &PathBuf,
        current_index: usize,
    ) {
        // リサイズ世代から届いた再デコード結果より、稼働中の担当を優先する。
        if self.contains_animation(&path, index) {
            return;
        }

        let incoming = content_bytes(&content);
        if incoming >= self.max_bytes {
            eprintln!(
                "[cache] animation bypass: {:?}[{}] {}MB > budget {}MB",
                path, index, incoming / MB, self.max_bytes / MB,
            );
            self.known_animation_bypass.insert((path.clone(), index));
            self.animation_bypass = Some(((path, index), content));
            return;
        }

        if self.animation_bypass.as_ref().is_some_and(|((p, i), _)| {
            p != current_path || *i != current_index
        }) {
            self.animation_bypass = None;
        }

        while self.total_bytes + incoming > self.max_bytes
            && self.total_bytes > self.min_bytes
            && (!self.entries.is_empty() || !self.animations.is_empty())
        {
            self.evict_furthest(current_path, current_index);
        }
        self.total_bytes += incoming;
        self.animations.insert((path, index), content);
    }

    /// 現在ページから最も遠いエントリを1件解放する。
    /// 別アーカイブのエントリは同アーカイブより常に遠いとみなす。
    fn evict_furthest(&mut self, current_path: &PathBuf, current_index: usize) {
        let static_key = self
            .entries
            .keys()
            .max_by_key(|(path, idx, _)| {
                let other_archive = path != current_path;
                let dist = idx.abs_diff(current_index);
                (other_archive, dist)
            })
            .cloned();

        let animation_key = self.animations.keys().max_by_key(|(path, idx)| {
            let other_archive = path != current_path;
            let dist = idx.abs_diff(current_index);
            (other_archive, dist)
        }).cloned();

        let static_distance = static_key.as_ref().map(|(path, idx, _)| {
            (path != current_path, idx.abs_diff(current_index))
        });
        let animation_distance = animation_key.as_ref().map(|(path, idx)| {
            (path != current_path, idx.abs_diff(current_index))
        });

        if animation_distance > static_distance {
            if let Some(key) = animation_key {
                if let Some(content) = self.animations.remove(&key) {
                    self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
                }
            }
        } else if let Some(key) = static_key {
            if let Some(content) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
    }
}

// ── ファイル単位キャッシュ ──────────────────────────────────────────────────────

/// アーカイブ単位の「準備済みデータ」をまるごとメモリに保持するキャッシュ。
/// ZIP/生画像は生バイト列(`FileCacheEntry::Raw`)、7zは開いた時点で全画像を展開した
/// 結果(`FileCacheEntry::Extracted`)を保持する。ヒット時はストレージI/Oも
/// 7z展開もスキップでき、かつ`Arc`で全ワーカースレッドに安く共有できる。
pub struct FileCache {
    entries: HashMap<PathBuf, FileCacheEntry>,
    total_bytes: usize,
    max_bytes: usize,
}

impl FileCache {
    pub fn new(max_bytes: usize) -> Self {
        Self { entries: HashMap::new(), total_bytes: 0, max_bytes }
    }

    pub fn max_bytes(&self) -> usize { self.max_bytes }
    pub fn total_bytes(&self) -> usize { self.total_bytes }

    pub fn get(&self, path: &PathBuf) -> Option<FileCacheEntry> {
        self.entries.get(path).cloned()
    }

    pub fn contains(&self, path: &PathBuf) -> bool {
        self.entries.contains_key(path)
    }

    /// 準備済みデータをキャッシュに追加する。
    /// 予算超過時は `all_paths` 上の位置距離が最も遠いエントリを evict する。
    pub fn insert(
        &mut self,
        path: PathBuf,
        entry: FileCacheEntry,
        current_path: &PathBuf,
        all_paths: &[PathBuf],
    ) {
        let incoming = entry.size_bytes();
        while self.total_bytes + incoming > self.max_bytes && !self.entries.is_empty() {
            self.evict_furthest(current_path, all_paths);
        }
        self.total_bytes += incoming;
        self.entries.insert(path, entry);
    }

    fn evict_furthest(&mut self, current_path: &PathBuf, all_paths: &[PathBuf]) {
        let current_pos = all_paths.iter().position(|p| p == current_path).unwrap_or(0);
        let key = self.entries.keys().max_by_key(|path| {
            all_paths.iter().position(|p| p == *path)
                .map(|pos| (pos as isize - current_pos as isize).unsigned_abs())
                .unwrap_or(usize::MAX)
        }).cloned();

        if let Some(key) = key {
            if let Some(entry) = self.entries.remove(&key) {
                self.total_bytes -= entry.size_bytes();
            }
        }
    }
}

fn rgba_bytes(img: &image::RgbaImage) -> usize {
    (img.width() * img.height() * 4) as usize
}

fn content_bytes(content: &PageContent) -> usize {
    match content {
        PageContent::Static(img) => rgba_bytes(img),
        // アニメは実常駐ではなく予約額で計上する。実常駐(resident_bytes)は挿入後に
        // リングが育って増えるため、挿入時点の値で計上すると帳簿が過小評価になり
        // evict が動かなくなる（reserved_bytes のコメント参照）。
        PageContent::Animated(ring) => ring.reserved_bytes(),
    }
}

// ── ファイルキャッシュワーカー ─────────────────────────────────────────────────

/// ファイルをバックグラウンドで読み込む単一スレッドのワーカーを起動する。
/// 7z(ソリッド圧縮)は開いた時点で全画像エントリを一括展開して`Extracted`として返す
/// （ランダムアクセスできないための一括展開。プロセス全体でこの1回きりになり、
/// 以降デコードワーカー側は共有Arcを参照するだけで済む）。
/// それ以外(ZIP/生画像)は従来通り生バイト列(`Raw`)のまま返す。
///
/// `max_bytes` はFileCacheの予算。メタデータ見積もりで予算を超えるファイルは
/// 読み込み・展開自体を行わず None を返す（隣接アーカイブの先読みはビューアーの
/// メモリゲートを通らないため、ここで丸読みによるスパイクを防ぐ。呼び出し側は
/// キャッシュせず、ページ読み込みはディスク直読みにフォールバックする）。
/// 返り値: (要求送信側, 結果受信側)。結果 None は「予算超過につきキャッシュ対象外」。
pub fn spawn_file_cache_worker(ctx: egui::Context, max_bytes: usize) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<(PathBuf, Option<FileCacheEntry>)>) {
    let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
    let (res_tx, res_rx) = mpsc::channel::<(PathBuf, Option<FileCacheEntry>)>();
    std::thread::spawn(move || {
        while let Ok(path) = req_rx.recv() {
            if crate::fs::archive::estimate_file_cache_bytes(&path) > max_bytes as u64 {
                eprintln!(
                    "[cache] file cache skip (over budget {}MB): {:?}",
                    max_bytes / MB, path,
                );
                let _ = res_tx.send((path, None));
                ctx.request_repaint();
                continue;
            }
            let entry = match crate::fs::archive::detect_format(&path) {
                #[cfg(feature = "fmt-7z")]
                crate::fs::archive::ArchiveFormat::SevenZ => {
                    let map = crate::fs::archive::extract_all_images_7z_path(&path);
                    Some(FileCacheEntry::Extracted(Arc::new(map)))
                }
                #[cfg(feature = "fmt-tar")]
                crate::fs::archive::ArchiveFormat::Tar => {
                    let map = crate::fs::archive::extract_all_images_tar_path(&path);
                    Some(FileCacheEntry::Extracted(Arc::new(map)))
                }
                crate::fs::archive::ArchiveFormat::Zip => {
                    std::fs::read(&path).ok().map(|bytes| FileCacheEntry::Raw(Arc::from(bytes)))
                }
            };
            // 読み込み失敗(None)でも必ず返送し、呼び出し側が pending を解放できるようにする
            let _ = res_tx.send((path, entry));
            // ROOT を起こして poll_workers に結果を回収させる
            ctx.request_repaint();
        }
    });
    (req_tx, res_rx)
}

// ── サムネイルワーカー ──────────────────────────────────────────────────────

pub struct ThumbRequest {
    pub archive_path: PathBuf,
    /// DB が利用可能な場合に渡す。None のときはメモリ生成のみ。
    pub db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
    /// true のとき archive_path は ZIP ではなく生画像ファイル
    pub is_raw_file: bool,
    /// Noneは従来どおり先頭画像、Someは登録済みentry_nameから生成する。
    pub thumbnail_entry_name: Option<String>,
}

pub struct ThumbResult {
    pub path: PathBuf,
    pub rgba: Option<image::RgbaImage>,
}

/// プローブレーンのスレッド数。ローカルDB読み＋JPEGデコードのみで軽いため少数で足りる。
const THUMB_PROBE_THREADS: usize = 2;

/// ネットワーク（gvfs）パスの生成並列度。CPU数ぶん並列でSMBに読みをかけると
/// 帯域が飽和してプローブのstatやビューア本体の読み込みまで巻き添えになるため絞る。
const THUMB_NET_GEN_THREADS: usize = 2;

/// ミス分をパス種別に応じた生成レーンへ振り分ける。
fn forward_thumb_gen(req: ThumbRequest, local_tx: &mpsc::Sender<ThumbRequest>, net_tx: &mpsc::Sender<ThumbRequest>) {
    let tx = if crate::fs::dir::is_gvfs_path(&req.archive_path) { net_tx } else { local_tx };
    let _ = tx.send(req);
}

/// 生成レーンのスレッドプールを起動する。
fn spawn_thumb_gen_pool(
    gen_rx: Arc<Mutex<mpsc::Receiver<ThumbRequest>>>,
    num_threads: usize,
    filter: image::imageops::FilterType,
    res_tx: mpsc::Sender<ThumbResult>,
    ctx: egui::Context,
) {
    for _ in 0..num_threads {
        let gen_rx = Arc::clone(&gen_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            loop {
                let req = match gen_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                // 失敗（None）でも必ず返送し、呼び元が thumb_pending を解放できるようにする
                let rgba = generate_thumb(&req, filter);
                let _ = res_tx.send(ThumbResult { path: req.archive_path, rgba });
                // ROOT を起こして poll_workers に結果を回収させる
                ctx.request_repaint();
            }
        });
    }
}

/// サムネイルワーカーを多レーン構成で起動する。
/// - プローブレーン: ローカルDBだけを見てキャッシュ済みサムネを即返す（ネットワークI/Oなし）。
///   ミス分と要再生成分は生成レーンへ転送する。
/// - 生成レーン（ローカル/ネットワーク別）: 元ファイルからの生成。
///   ネットワーク側は並列度を THUMB_NET_GEN_THREADS に制限する。
/// キャッシュ済みサムネが未格納分の生成待ち行列に並ばされて遅延するのを防ぐ。
pub fn spawn_thumb_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context) -> (mpsc::SyncSender<ThumbRequest>, mpsc::Receiver<ThumbResult>) {
    let capacity = (num_threads * 2).max(16);
    let (req_tx, probe_rx) = mpsc::sync_channel::<ThumbRequest>(capacity);
    let (res_tx, res_rx) = mpsc::channel::<ThumbResult>();
    let (gen_tx, gen_rx) = mpsc::channel::<ThumbRequest>();
    let (net_gen_tx, net_gen_rx) = mpsc::channel::<ThumbRequest>();

    let probe_rx = Arc::new(Mutex::new(probe_rx));

    for _ in 0..THUMB_PROBE_THREADS {
        let probe_rx = Arc::clone(&probe_rx);
        let gen_tx = gen_tx.clone();
        let net_gen_tx = net_gen_tx.clone();
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            loop {
                let req = match probe_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                match probe_cached_thumb(&req) {
                    Some((rgba, stored_mtime)) => {
                        // キャッシュヒット: statを待たずに先に表示へ回す
                        let _ = res_tx.send(ThumbResult { path: req.archive_path.clone(), rgba: Some(rgba) });
                        ctx.request_repaint();
                        // 後追い検証: statが成功してmtimeが変わっていた場合のみ再生成へ。
                        // stat失敗（ネットワーク不調）はキャッシュ表示のまま維持する。
                        let current_mtime = crate::neko_dir::file_mtime(&req.archive_path);
                        if current_mtime != 0 && current_mtime != stored_mtime {
                            forward_thumb_gen(req, &gen_tx, &net_gen_tx);
                        }
                    }
                    None => {
                        forward_thumb_gen(req, &gen_tx, &net_gen_tx);
                    }
                }
            }
        });
    }
    // 全送信側（プローブスレッド保持分）が閉じると生成レーンも終了する
    drop(gen_tx);
    drop(net_gen_tx);

    spawn_thumb_gen_pool(Arc::new(Mutex::new(gen_rx)), num_threads, filter, res_tx.clone(), ctx.clone());
    spawn_thumb_gen_pool(Arc::new(Mutex::new(net_gen_rx)), THUMB_NET_GEN_THREADS, filter, res_tx, ctx);

    (req_tx, res_rx)
}

// ── アーカイブ内エントリ単位のサムネイルワーカー（サムネイルバー用）────────────
// フォルダグリッド用の spawn_thumb_worker（アーカイブ先頭1枚だけ）とは別に、開いている
// アーカイブの全エントリを小さくデコードする。LoadRequest 用の OpenArchive/load_page を
// そのまま流用し、zip/7z/生画像を共通に扱う。結果はメインの page_cache とは別に保持する
// （解像度が異なるため同じキーで衝突させない）。
const ENTRY_THUMB_RING_BUDGET_BYTES: usize = 32 * 1024 * 1024;
const ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES: usize = 32 * 1024 * 1024;

pub struct EntryThumbRequest {
    pub archive_path: PathBuf,
    pub entry_name: String,
    pub original_index: usize,
    pub is_raw_file: bool,
    /// 長辺の目標サイズ(px)
    pub edge: u32,
    /// FileCache ヒット時の準備済みデータ。spawn_worker と同じくSome なら
    /// ディスクI/Oも7z展開もスキップできる。
    pub file_cache_entry: Option<FileCacheEntry>,
}

pub struct EntryThumbResult {
    pub archive_path: PathBuf,
    pub original_index: usize,
    pub rgba: Option<image::RgbaImage>,
}

pub fn spawn_entry_thumb_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context) -> (mpsc::Sender<EntryThumbRequest>, mpsc::Receiver<EntryThumbResult>) {
    let (req_tx, req_rx) = mpsc::channel::<EntryThumbRequest>();
    let (res_tx, res_rx) = mpsc::channel::<EntryThumbResult>();
    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..num_threads.max(1) {
        let req_rx = Arc::clone(&req_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // 直前に開いたアーカイブをキープオープンする（spawn_worker と同様）
            let mut open_archive: Option<(PathBuf, OpenArchive)> = None;

            loop {
                let req = match req_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };

                let target = Some((req.edge, req.edge));
                // サムネ用途でもframe0の保持を保証し、ビューアー用リング設定と独立させる。
                let ring_bounds = (2, 2);
                // アーカイブ内サムネイルはビューアーのExif ON/OFF設定(D)と独立、常時EXIF自動回転を適用する。
                let content = if req.is_raw_file {
                    match &req.file_cache_entry {
                        Some(FileCacheEntry::Raw(bytes)) => {
                            load_raw_content_from_bytes(bytes, &req.archive_path, filter, ENTRY_THUMB_RING_BUDGET_BYTES, ring_bounds, ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES, target, true)
                        }
                        _ => load_raw_file_content(&req.archive_path, filter, ENTRY_THUMB_RING_BUDGET_BYTES, ring_bounds, ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES, target, true),
                    }
                } else if let Some(FileCacheEntry::Extracted(map)) = req.file_cache_entry.clone() {
                    // FileCache が既に展開済み(7z等): スレッドローカルの再展開はせず共有Arcを使う
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Extracted(_))
                    });
                    if !is_same {
                        open_archive = Some((req.archive_path.clone(), OpenArchive::Extracted(map)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| {
                        a.load_page(&req.entry_name, filter, ENTRY_THUMB_RING_BUDGET_BYTES, ring_bounds, ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES, target, true)
                    })
                } else if let Some(FileCacheEntry::Raw(bytes)) = req.file_cache_entry {
                    // FileCache ヒット(ZIP): メモリからアーカイブを開く
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Mem(_))
                    });
                    if !is_same {
                        open_archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                            .ok()
                            .map(|a| (req.archive_path.clone(), OpenArchive::Mem(a)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| {
                        a.load_page(&req.entry_name, filter, ENTRY_THUMB_RING_BUDGET_BYTES, ring_bounds, ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES, target, true)
                    })
                } else {
                    // FileCache ミス: ディスクから開く（従来の動作。ソリッド形式はここに来た場合のみ
                    // スレッドローカルに展開する安全弁で、通常はFileCache側の先出しにより
                    // ほぼ発生しない）
                    let is_same = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Disk(_) | OpenArchive::Extracted(_))
                    });
                    if !is_same {
                        open_archive = open_archive_from_disk(&req.archive_path)
                            .map(|a| (req.archive_path.clone(), a));
                    }
                    open_archive.as_mut().and_then(|(_, a)| {
                        a.load_page(&req.entry_name, filter, ENTRY_THUMB_RING_BUDGET_BYTES, ring_bounds, ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES, target, true)
                    })
                };

                let rgba = match content {
                    Some(PageContent::Static(img)) => Some(img),
                    Some(PageContent::Animated(ring)) => ring.with_frame(0, |f| f.image.clone()),
                    None => None,
                };

                let _ = res_tx.send(EntryThumbResult {
                    archive_path: req.archive_path,
                    original_index: req.original_index,
                    rgba,
                });
                ctx.request_repaint();
            }
        });
    }

    (req_tx, res_rx)
}

/// SMB（gvfs）パスのZIPをバックグラウンドスレッドで読み込む。
fn load_first_image_smb(path: PathBuf) -> Option<image::DynamicImage> {
    let timeout = std::time::Duration::from_secs(30);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::fs::archive::load_first_image(&path));
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// ローカルDBだけを見てキャッシュ済みサムネを返す（ネットワークI/Oなし）。
/// 戻り値は (デコード済みRGBA, 保存時のsource_mtime)。mtime検証は呼び出し側が後追いで行う。
fn probe_cached_thumb(req: &ThumbRequest) -> Option<(image::RgbaImage, i64)> {
    let db = req.db.as_ref()?;
    let filename = req.archive_path.file_name().and_then(|n| n.to_str())?;
    let (stored_mtime, jpeg) = crate::neko_dir::read_thumb_unchecked(db, filename)?;
    let stored_source = crate::neko_dir::read_thumb_source(db, filename);
    let source_matches = match req.thumbnail_entry_name.as_deref() {
        Some(expected) => stored_source.as_deref() == Some(expected),
        None => stored_source.as_deref().map_or(true, str::is_empty),
    };
    if !source_matches {
        return None;
    }
    let t0 = std::time::Instant::now();
    let rgba = image::load_from_memory(&jpeg).ok()?.to_rgba8();
    log_perf!("[perf/thumb] db_cache={:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);
    Some((rgba, stored_mtime))
}

/// 元ファイルからサムネを生成してDBへ保存する。失敗時は None を返す（スレッドは死なない）。
fn generate_thumb(req: &ThumbRequest, filter: image::imageops::FilterType) -> Option<image::RgbaImage> {
    let filename = req.archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned();
    let source_mtime = crate::neko_dir::file_mtime(&req.archive_path);
    let mut generated_source: Option<String> = None;

    let rgba = if req.is_raw_file {
        let buf = std::fs::read(&req.archive_path).ok()?;
        // サムネイルはビューアーのExif ON/OFF設定(D)と独立、常時EXIF自動回転を適用する。
        let img = crate::fs::archive::decode_image_bytes(&buf, true)?;
        resize_thumbnail(img, filter)
    } else {
        let t_total = std::time::Instant::now();
        let is_smb = crate::fs::dir::is_gvfs_path(&req.archive_path);
        let load_default = || {
            if is_smb {
                load_first_image_smb(req.archive_path.clone())
            } else {
                crate::fs::archive::load_first_image(&req.archive_path)
            }
        };
        let registered = req.thumbnail_entry_name.as_deref().and_then(|entry_name| {
            let mut archive = open_archive_from_disk(&req.archive_path)?;
            match archive.load_page(
                entry_name,
                filter,
                ENTRY_THUMB_RING_BUDGET_BYTES,
                (2, 2),
                ENTRY_THUMB_FRAME_HARD_LIMIT_BYTES,
                Some((256, 256)),
                true,
            )? {
                PageContent::Static(rgba) => Some(image::DynamicImage::ImageRgba8(rgba)),
                PageContent::Animated(ring) => ring.with_frame(0, |f| {
                    image::DynamicImage::ImageRgba8(f.image.clone())
                }),
            }
        });
        // 登録先がアーカイブ更新等で消えていても、グリッド自体を壊さず先頭画像へ戻す。
        if registered.is_some() {
            generated_source = req.thumbnail_entry_name.clone();
        }
        let img = registered.or_else(load_default)?;
        let t_load = t_total.elapsed();
        let t2 = std::time::Instant::now();
        let result = resize_thumbnail(img, filter);
        log_perf!(
            "[perf/thumb] load={:.1}ms resize={:.1}ms total={:.1}ms",
            t_load.as_secs_f64() * 1000.0,
            t2.elapsed().as_secs_f64() * 1000.0,
            t_total.elapsed().as_secs_f64() * 1000.0,
        );
        result
    };

    // DBに保存（サムネ本体＋検索用ファイル索引）
    if let Some(ref db) = req.db {
        if let Some(jpeg) = encode_jpeg(&rgba) {
            crate::neko_dir::write_thumb(db, &filename, source_mtime, &jpeg);
            crate::neko_dir::write_thumb_source(db, &filename, generated_source.as_deref());
            let size = crate::neko_dir::file_size(&req.archive_path);
            crate::neko_dir::write_file_record(db, &filename, source_mtime, size);
        }
    }

    Some(rgba)
}

fn encode_jpeg(rgba: &image::RgbaImage) -> Option<Vec<u8>> {
    let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
    let mut buf = std::io::Cursor::new(Vec::new());
    rgb.write_to(&mut buf, image::ImageFormat::Jpeg).ok()?;
    Some(buf.into_inner())
}

pub fn resize_thumbnail(img: image::DynamicImage, filter: image::imageops::FilterType) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = if w >= h {
        (256, (256 * h / w).max(1))
    } else {
        ((256 * w / h).max(1), 256)
    };
    fir_resize(img, nw, nh, filter)
}

#[cfg(test)]
mod ring_integration_tests {
    use super::*;

    const GB: usize = 1024 * MB;
    /// テスト用: 十分大きい予算を与え、容量は常に上限(32)に張り付かせる
    /// （フェーズ3.6時点の固定容量32枚での期待値をそのまま維持するため）。
    const TEST_RING_BUDGET_BYTES: usize = 10 * GB;
    const TEST_RING_BOUNDS: (usize, usize) = (4, 32);
    const TEST_RING_MAX: usize = TEST_RING_BOUNDS.1;
    /// フェーズ5: 実際のデフォルト(100MB)と同じ値。テストフィクスチャは全て十分小さいので影響しない。
    const TEST_FRAME_HARD_LIMIT_BYTES: usize = 100 * MB;

    #[test]
    fn registered_archive_entry_generates_a_grid_thumbnail() {
        let archive_path = PathBuf::from("test/testarchive.zip");
        let entries = crate::fs::archive::list_images(&archive_path);
        let selected = entries.last().expect("test archive has images").entry_name.clone();
        let req = ThumbRequest {
            archive_path,
            db: None,
            is_raw_file: false,
            thumbnail_entry_name: Some(selected),
        };
        let rgba = generate_thumb(&req, image::imageops::FilterType::Triangle)
            .expect("registered entry should generate a thumbnail");
        assert!(rgba.width() <= 256 && rgba.height() <= 256);
        assert!(rgba.width() > 0 && rgba.height() > 0);
    }

    /// フェーズ3.6: 実物の大きいGIF(test/nouka.gif, 640x360 1316フレーム, 全展開なら約1.2GB)で
    /// リングバッファが実際に「全フレーム常駐にならず一定量に収まる」ことを確認する結合テスト。
    #[test]
    fn ring_anim_stays_bounded_on_real_large_gif() {
        let path = std::path::Path::new("test/nouka.gif");
        let buf = std::fs::read(path).expect("test/nouka.gif が見つからない");

        let content = decode_ring_anim(&buf, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)), true)
            .expect("GIFとしてデコードできるはず");

        let PageContent::Animated(ring) = content else {
            panic!("1316フレームあるので Animated になるはず（Static ではない）");
        };

        // 再生をシミュレート: リングバッファ容量(32)を大きく超える200フレーム分進める。
        for i in 0..200 {
            let ok = ring.with_frame(i, |_| ()).is_some();
            assert!(ok, "frame {i} が取得できるはず（1316フレーム中なので終端に達していない）");
        }

        // 200フレーム分デコードした後も、常駐量は「全フレーム分(約1.2GB)」ではなく
        // リングバッファ容量相当(数十MB)に収まっているはず。
        let resident = ring.resident_bytes();
        let one_frame_bytes = 640 * 360 * 4;
        let full_bytes = one_frame_bytes * 1316;
        assert!(
            resident < full_bytes / 4,
            "resident_bytes={resident} が全フレーム分({full_bytes})に対して大きすぎる(リングバッファが効いていない)",
        );
        assert!(
            resident <= one_frame_bytes * (TEST_RING_MAX + 1),
            "resident_bytes={resident} がリング容量{TEST_RING_MAX}枚分を大きく超えている",
        );
        // 帳簿の予約額は実常駐の上限であり続けるはず（帳簿 ≥ 実常駐 の保証）。
        assert!(
            resident <= ring.reserved_bytes(),
            "resident_bytes={resident} が予約計上額{}を超えている(帳簿が過小評価になる)",
            ring.reserved_bytes(),
        );
    }

    /// PageCacheの帳簿がアニメを予約額(容量×フレームサイズ)で計上し、
    /// 再生でリングが育っても帳簿が変わらず、remove時に同額が引かれて
    /// ゼロに戻る（足し引きの対称性）ことを確認する。
    #[test]
    fn page_cache_accounts_anim_by_reservation_symmetrically() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(&bytes, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)), true)
            .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ref ring) = content else {
            panic!("3フレームあるので Animated になるはず");
        };
        // 予算が十分大きいので容量は上限(32)に張り付き、予約額 = 32 × 10×10×4。
        let reserved = ring.reserved_bytes();
        assert_eq!(reserved, 10 * 10 * 4 * TEST_RING_MAX);

        let mut cache = PageCache::new(10 * MB, 0);
        let path = std::path::PathBuf::from("test.zip");
        cache.insert(path.clone(), 0, 0, content, &path, 0);
        assert_eq!(cache.total_bytes(), reserved, "挿入時点で予約額が計上されるべき");

        // 再生を進めてリングを育てても帳簿は不変（挿入時確定の予約方式）。
        if let Some((0, PageContent::Animated(ring))) = cache.get_best(&path, 0, 0, None) {
            for i in 0..3 {
                let _ = ring.with_frame(i, |_| ());
            }
            assert!(ring.resident_bytes() <= reserved);
        }
        assert_eq!(cache.total_bytes(), reserved, "リングが育っても帳簿は変わらないべき");

        // removeで同額が引かれゼロに戻る（挿入と削除の対称性）。
        cache.remove(&path, 0, 0);
        assert_eq!(cache.total_bytes(), 0, "予約額と同額が引かれてゼロに戻るべき");
    }

    #[test]
    fn page_cache_keeps_one_animation_instance_across_display_generations() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let decode = || {
            decode_ring_anim(
                &bytes,
                AnimFormat::Gif,
                image::imageops::FilterType::Triangle,
                TEST_RING_BUDGET_BYTES,
                TEST_RING_BOUNDS,
                TEST_FRAME_HARD_LIMIT_BYTES,
                Some((1920, 1080)),
                true,
            )
            .expect("GIFとしてデコードできるはず")
        };
        let path = PathBuf::from("animation.zip");
        let mut cache = PageCache::new(10 * MB, 0);

        cache.insert(path.clone(), 0, 10, decode(), &path, 0);
        let first_id = match cache.get_best(&path, 0, 10, None).unwrap().1 {
            PageContent::Animated(ring) => ring.instance_id(),
            PageContent::Static(_) => panic!("animation expected"),
        };

        // リサイズ世代から重複結果が届いても、稼働中の担当を置換しない。
        cache.insert(path.clone(), 0, 11, decode(), &path, 0);
        cache.retain_generations(10, Some(11));
        cache.remove_older_versions(&path, 0, 11);

        let (generation, content) = cache.get_best(&path, 0, 10, Some(11)).unwrap();
        assert_eq!(generation, 11, "表示側には最新表示世代として返す");
        let PageContent::Animated(ring) = content else {
            panic!("animation expected");
        };
        assert_eq!(ring.instance_id(), first_id, "RingAnimation担当は同一のまま");
        assert!(cache.contains(&path, 0, 12), "アニメの存在判定は表示世代に依存しない");
    }

    #[test]
    fn animation_resize_keeps_current_frame_and_jumps_to_first_new_epoch_frame() {
        let bytes = encode_gif_frames_mixed(&[(100, 100), (100, 100), (100, 100), (100, 100)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((50, 50)),
            true,
        )
        .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("animation expected");
        };

        assert_eq!(ring.with_frame(1, |f| f.image.dimensions()), Some((50, 50)));
        assert_eq!(ring.with_frame(2, |f| f.image.dimensions()), Some((50, 50)));

        ring.update_resize_target(Some((25, 25)), 1);

        assert!(
            !commit_resized_frame(
                &ring.state,
                0,
                99,
                AnimFrame {
                    image: image::RgbaImage::new(50, 50),
                    delay: std::time::Duration::from_millis(40),
                },
            ),
            "旧epochで処理完了したリサイズ結果はリングへ戻さない",
        );

        assert_eq!(
            ring.try_with_frame(1, |f| f.image.dimensions()),
            Some((50, 50)),
            "現在表示中の旧サイズフレームは差し替え完了まで残す",
        );
        assert!(ring.try_with_frame(2, |_| ()).is_none(), "旧サイズの未来フレームは破棄する");
        assert_eq!(ring.with_frame(3, |f| f.image.dimensions()), Some((25, 25)));
        assert_eq!(
            ring.playback_ready_after(1),
            Some(3),
            "前進専用decoderで再取得不能な欠番を越えて新epochへ移る",
        );
    }

    #[test]
    fn missing_frame_diagnostic_distinguishes_evicted_frame() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            (2, 2),
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((1920, 1080)),
            true,
        )
        .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("animation expected");
        };
        assert!(ring.with_frame(2, |_| ()).is_some());

        assert_eq!(
            ring.diagnose_missing_frame(0),
            Some(AnimationFrameDiagnostic::Missing {
                ring_range: Some((1, 2)),
                next_decode_index: 3,
                capacity: 2,
                resize_epoch: 0,
            }),
        );
        assert_eq!(ring.diagnose_missing_frame(2), None);
        assert_eq!(ring.reconnect_frame_index(0), Some(2));
        assert_eq!(ring.reconnect_frame_index(1), Some(1));
    }

    #[test]
    fn page_cache_resize_updates_animation_reservation_by_delta() {
        let bytes = encode_gif_frames_mixed(&[(100, 100), (100, 100), (100, 100)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((50, 50)),
            true,
        )
        .expect("GIFとしてデコードできるはず");
        let path = PathBuf::from("animation.zip");
        let mut cache = PageCache::new(10 * MB, 0);
        cache.insert(path.clone(), 0, 10, content, &path, 0);
        let old_total = cache.total_bytes();

        assert!(cache.update_animation_resize(&path, 0, Some((25, 25)), 0));
        let new_reserved = match cache.get_best(&path, 0, 10, Some(11)).unwrap().1 {
            PageContent::Animated(ring) => ring.reserved_bytes(),
            PageContent::Static(_) => panic!("animation expected"),
        };

        assert!(new_reserved < old_total);
        assert_eq!(cache.total_bytes(), new_reserved);
        cache.remove_all_for_path(&path);
        assert_eq!(cache.total_bytes(), 0, "動的変更後もevict側の減算と対称になる");
    }

    #[test]
    fn remove_all_for_path_releases_stable_animation_instance() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((1920, 1080)),
            true,
        )
        .expect("GIFとしてデコードできるはず");
        let path = PathBuf::from("animation.zip");
        let mut cache = PageCache::new(10 * MB, 0);
        cache.insert(path.clone(), 0, 10, content, &path, 0);

        cache.remove_all_for_path(&path);

        assert!(!cache.contains_animation(&path, 0));
        assert!(cache.get_best(&path, 0, 10, Some(11)).is_none());
        assert_eq!(cache.total_bytes(), 0);
    }

    /// リサイズ/原寸切替/フルスクリーンの再デコード経路: `drop_animation_for_redecode` が
    /// 稼働中アニメの席と予約計上を完全に解放し、直後の再デコード結果を別 `instance_id` の
    /// 新しい `RingAnimation` として先頭から座らせ直せることを確認する
    /// （静止画化バグの修正。`insert_animation` の `contains_animation` ガードに弾かれない）。
    #[test]
    fn redecode_drop_reseats_animation_from_scratch_with_new_instance() {
        // ソースを十分大きく取り、target ごとに実際の縮小サイズ＝予約額が変わるようにする。
        let bytes = encode_gif_frames_mixed(&[(100, 100), (100, 100), (100, 100)]);
        let decode = |target| {
            decode_ring_anim(
                &bytes,
                AnimFormat::Gif,
                image::imageops::FilterType::Triangle,
                TEST_RING_BUDGET_BYTES,
                TEST_RING_BOUNDS,
                TEST_FRAME_HARD_LIMIT_BYTES,
                target,
                true,
            )
            .expect("GIFとしてデコードできるはず")
        };
        let instance_of = |content: &PageContent| match content {
            PageContent::Animated(ring) => ring.instance_id(),
            PageContent::Static(_) => panic!("animation expected"),
        };

        let path = PathBuf::from("animation.zip");
        let mut cache = PageCache::new(10 * MB, 0);

        // アニメ未保持のページに対しては no-op（total_bytes を減算し過ぎない）。
        cache.drop_animation_for_redecode(&path, 0);
        assert_eq!(cache.total_bytes(), 0);

        cache.insert(path.clone(), 0, 10, decode(Some((50, 50))), &path, 0);
        let first_id = instance_of(cache.get_best(&path, 0, 10, None).unwrap().1);
        let reserved_before = cache.total_bytes();
        assert!(reserved_before > 0);
        assert!(cache.contains_animation(&path, 0));

        // リサイズ再デコード発火相当: 稼働中アニメを破棄。
        cache.drop_animation_for_redecode(&path, 0);
        assert!(!cache.contains_animation(&path, 0));
        assert!(cache.get_best(&path, 0, 10, Some(11)).is_none());
        assert_eq!(cache.total_bytes(), 0, "予約計上ぶんは完全に戻す");

        // 新しい target_size での再デコード結果を投入 → contains_animation ガードに弾かれず着席。
        cache.insert(path.clone(), 0, 11, decode(Some((25, 25))), &path, 0);
        let second_id = instance_of(cache.get_best(&path, 0, 11, None).unwrap().1);

        assert_ne!(first_id, second_id, "別 RingAnimation として作り直される（先頭フレームから再生）");
        assert!(cache.total_bytes() > 0);
        assert!(cache.total_bytes() < reserved_before, "25x25 の新予約は 50x50 より小さい");

        cache.remove_all_for_path(&path);
        assert_eq!(cache.total_bytes(), 0, "再デコード後もevict側の減算と対称");
    }

    /// bypass 扱い（単体で予算超過）のアニメも、`drop_animation_for_redecode` で
    /// bypassスロットと既知bypass記憶ごと掃除され、再デコードをやり直せる。
    #[test]
    fn redecode_drop_clears_animation_bypass_slot_and_known_flag() {
        let bytes = encode_gif_frames_mixed(&[(100, 100), (100, 100), (100, 100)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            None,
            true,
        )
        .expect("GIFとしてデコードできるはず");

        let path = PathBuf::from("huge-anim.zip");
        // アニメの予約額（32枚 x 100x100x4 ≒ 1.28MB）が収まらない予算 → animation_bypass 行き。
        let mut cache = PageCache::new(1_000_000, 0);
        cache.insert(path.clone(), 0, 10, content, &path, 0);

        assert!(cache.contains_animation(&path, 0), "bypassスロット経由でも保持中扱い");
        assert!(cache.is_known_bypass(&path, 0, 10), "既知bypassとして記録される");
        assert_eq!(cache.total_bytes(), 0, "bypassは帳簿外");

        cache.drop_animation_for_redecode(&path, 0);

        assert!(!cache.contains_animation(&path, 0));
        assert!(!cache.is_known_bypass(&path, 0, 10), "既知bypass記憶も消す（再デコードを抑止しない）");
        assert_eq!(cache.total_bytes(), 0);
        assert!(cache.get_best(&path, 0, 10, Some(11)).is_none());
    }

    /// 表示解像度の世代変更では通常エントリだけでなく、予算超過bypassと
    /// 再要求抑止記録もまとめて初期化されるべき。
    #[test]
    fn page_cache_clear_removes_all_resolution_dependent_state() {
        let mut cache = PageCache::new(1024, 0);
        let normal_path = PathBuf::from("normal.png");
        cache.insert(
            normal_path.clone(),
            0,
            0,
            PageContent::Static(image::RgbaImage::new(10, 10)),
            &normal_path,
            0,
        );

        let bypass_path = PathBuf::from("oversized.png");
        cache.insert(
            bypass_path.clone(),
            0,
            0,
            PageContent::Static(image::RgbaImage::new(20, 20)),
            &normal_path,
            0,
        );
        assert!(cache.contains(&normal_path, 0, 0));
        assert!(cache.contains(&bypass_path, 0, 0));
        assert!(cache.is_known_bypass(&bypass_path, 0, 0));

        cache.clear();

        assert_eq!(cache.total_bytes(), 0);
        assert!(!cache.contains(&normal_path, 0, 0));
        assert!(!cache.contains(&bypass_path, 0, 0));
        assert!(!cache.is_known_bypass(&bypass_path, 0, 0));
    }

    #[test]
    fn page_cache_prefers_preparing_then_falls_back_to_active() {
        let mut cache = PageCache::new(1024, 0);
        let path = PathBuf::from("page.png");
        cache.insert(
            path.clone(), 0, 10,
            PageContent::Static(image::RgbaImage::new(1, 1)),
            &path, 0,
        );

        let (generation, _) = cache.get_best(&path, 0, 10, Some(11)).unwrap();
        assert_eq!(generation, 10, "新世代の完成前は現世代へフォールバックする");

        cache.insert(
            path.clone(), 0, 11,
            PageContent::Static(image::RgbaImage::new(2, 2)),
            &path, 0,
        );
        let (generation, content) = cache.get_best(&path, 0, 10, Some(11)).unwrap();
        assert_eq!(generation, 11, "新世代が完成したページは新世代を優先する");
        assert!(matches!(content, PageContent::Static(img) if img.width() == 2));

        cache.remove_older_versions(&path, 0, 11);
        assert!(!cache.contains(&path, 0, 10));
        assert!(cache.contains(&path, 0, 11));
    }

    #[test]
    fn page_cache_repeated_resize_retains_only_active_and_preparing() {
        let mut cache = PageCache::new(1024, 0);
        let path = PathBuf::from("page.png");
        for generation in 10..=12 {
            cache.insert(
                path.clone(), 0, generation,
                PageContent::Static(image::RgbaImage::new(1, 1)),
                &path, 0,
            );
        }

        cache.retain_generations(11, Some(12));

        assert!(!cache.contains(&path, 0, 10));
        assert!(cache.contains(&path, 0, 11));
        assert!(cache.contains(&path, 0, 12));
    }

    #[test]
    fn page_cache_replacing_same_generation_keeps_accounting_symmetric() {
        let mut cache = PageCache::new(1024, 0);
        let path = PathBuf::from("page.png");
        cache.insert(
            path.clone(), 0, 1,
            PageContent::Static(image::RgbaImage::new(2, 2)),
            &path, 0,
        );
        cache.insert(
            path.clone(), 0, 1,
            PageContent::Static(image::RgbaImage::new(3, 3)),
            &path, 0,
        );

        assert_eq!(cache.total_bytes(), 3 * 3 * 4);
    }

    #[test]
    fn page_worker_reports_decode_failure_instead_of_leaving_pending_forever() {
        let ctx = egui::Context::default();
        let (queue, results) = spawn_worker(
            image::imageops::FilterType::Triangle,
            1,
            ctx,
            16 * 1024 * 1024,
            (1, 2),
            16 * 1024 * 1024,
        );
        let path = PathBuf::from("definitely-missing-page.png");
        let key = crate::decode_jobs::DecodeJobKey {
            archive_path: path.clone(),
            page_index: 0,
            generation: 0,
        };
        assert!(queue.submit(crate::decode_jobs::DesiredDecodeJob {
            key,
            class: crate::decode_jobs::PagePriorityClass::Visible,
            distance: 0,
            payload: LoadRequest {
                archive_path: path,
                index: 0,
                entry_name: String::new(),
                is_raw_file: true,
                file_cache_entry: None,
                target_size: Some((800, 600)),
                exif_enabled: true,
                generation: 0,
            },
        }));

        let result = results
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("失敗結果が返るはず");
        assert!(matches!(result.outcome, DecodeJobOutcome::Failed));
        assert_eq!(queue.pending_count(), 0);
        queue.shutdown();
    }

    /// フェーズ3.6: ループ境界(終端→restart→先頭)が実際に機能することを確認する。
    #[test]
    fn ring_anim_restart_replays_from_head_on_real_gif() {
        let path = std::path::Path::new("test/nouka.gif");
        let buf = std::fs::read(path).expect("test/nouka.gif が見つからない");

        let content = decode_ring_anim(&buf, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)), true)
            .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("Animated になるはず");
        };

        // 終端(1316番目、存在しない)は None のはず。
        assert!(ring.with_frame(1316, |_| ()).is_none());

        // ループ境界: restart() して先頭からまた取得できることを確認する。
        assert!(ring.restart());
        assert!(ring.with_frame(0, |_| ()).is_some());
        assert!(ring.with_frame(1, |_| ()).is_some());
    }

    /// フェーズ3.6(WebP): /tmp/testwebp.zip 内のアニメWebP(960x1376, 243フレーム,
    /// 全展開なら約1.28GB)で、リングバッファが機能することを確認する結合テスト。
    /// パスが環境依存(ユーザーの実機の一時ファイル)のため #[ignore]。
    fn load_webp_entry_from_test_zip() -> Vec<u8> {
        let zip_path = "/tmp/testwebp.zip";
        let file = std::fs::File::open(zip_path).expect("/tmp/testwebp.zip が見つからない");
        let mut archive = zip::ZipArchive::new(file).expect("zip open failed");
        let mut entry = archive
            .by_name("3696790_aab7483a45/11_1_001.webp")
            .expect("entry not found");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        buf
    }

    #[test]
    #[ignore]
    fn ring_anim_webp_stays_bounded_on_real_large_file() {
        let buf = load_webp_entry_from_test_zip();

        let content = decode_ring_anim(&buf, AnimFormat::Webp, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)), true)
            .expect("WebPとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("243フレームあるので Animated になるはず（Static ではない）");
        };

        // 表示サイズ縮小後の1フレーム分バイト数を、エビクトされる前(ループ前)に取得しておく
        // (960x1376は表示ターゲット高さ(1080)を超えるため縮小されている前提)。
        let one_frame_bytes = ring
            .with_frame(0, |f| (f.image.width() as usize) * (f.image.height() as usize) * 4)
            .expect("frame 0 は取得できるはず");

        // 再生をシミュレート: リングバッファ容量(32)を大きく超える100フレーム分進める。
        for i in 0..100 {
            assert!(ring.with_frame(i, |_| ()).is_some(), "frame {i} が取得できるはず");
        }

        let full_bytes = one_frame_bytes * 243;
        let resident = ring.resident_bytes();
        assert!(
            resident < full_bytes / 4,
            "resident_bytes={resident} が全フレーム分({full_bytes})に対して大きすぎる(リングバッファが効いていない)",
        );
        assert!(
            resident <= one_frame_bytes * (TEST_RING_MAX + 1),
            "resident_bytes={resident} がリング容量{TEST_RING_MAX}枚分を大きく超えている",
        );
    }

    #[test]
    #[ignore]
    fn ring_anim_webp_restart_replays_from_head() {
        let buf = load_webp_entry_from_test_zip();

        let content = decode_ring_anim(&buf, AnimFormat::Webp, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)), true)
            .expect("WebPとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("Animated になるはず");
        };

        // 終端(243番目、存在しない)は None のはず。
        assert!(ring.with_frame(243, |_| ()).is_none());

        // ループ境界: restart() して先頭からまた取得できることを確認する。
        assert!(ring.restart());
        assert!(ring.with_frame(0, |_| ()).is_some());
        assert!(ring.with_frame(1, |_| ()).is_some());
    }

    /// フレームごとに解像度が異なる合成GIFを作る（フェーズ5: 変則ファイルの再現用）。
    fn encode_gif_frames_mixed(dims: &[(u32, u32)]) -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::Delay;
        let mut buf = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut buf);
            for &(w, h) in dims {
                let img = image::RgbaImage::new(w, h);
                let frame = image::Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(10, 1));
                encoder.encode_frame(frame).unwrap();
            }
        }
        buf
    }

    fn wait_for_async_frame(ring: &RingAnimation, index: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if ring.try_with_frame(index, |_| ()).is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("background frame {index} was not produced before timeout");
    }

    #[test]
    fn ring_anim_background_request_produces_next_frame_without_sync_decode() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((10, 10)),
            true,
        )
        .expect("GIF should decode");
        let PageContent::Animated(ring) = content else { panic!("expected animation") };

        assert!(ring.try_with_frame(0, |_| ()).is_some());
        assert!(!ring.request_frame(2), "new frame should be produced asynchronously");
        wait_for_async_frame(&ring, 2);
        assert!(ring.request_frame(2), "completed frame should report ready");
    }

    #[test]
    fn ring_anim_defers_frame1_resize_and_pipeline_start_until_requested() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((10, 10)),
            true,
        )
        .expect("GIF should decode");
        let PageContent::Animated(ring) = content else { panic!("expected animation") };

        assert!(ring.try_with_frame(0, |_| ()).is_some());
        assert!(ring.try_with_frame(1, |_| ()).is_none());
        assert!(!ring.pipeline_started.load(Ordering::Acquire));

        assert!(!ring.request_frame(1));
        wait_for_async_frame(&ring, 1);
        assert!(ring.pipeline_started.load(Ordering::Acquire));
    }

    #[test]
    fn ring_anim_pipeline_reaches_requested_target_with_sparse_ready_frames() {
        let bytes = encode_gif_frames_mixed(&[
            (10, 10),
            (10, 10),
            (10, 10),
            (10, 10),
            (10, 10),
        ]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((10, 10)),
            true,
        )
        .expect("GIF should decode");
        let PageContent::Animated(ring) = content else { panic!("expected animation") };

        assert!(!ring.request_frame(4));
        wait_for_async_frame(&ring, 4);
        assert!(ring.try_with_frame(0, |_| ()).is_some());
        assert!(ring.try_with_frame(4, |_| ()).is_some());
        assert_eq!(ring.latest_ready_after(0), Some(4));
    }

    #[test]
    fn animation_pipeline_raw_queue_preserves_normal_frames_then_drops_stale() {
        let pipeline = AnimationPipelineControl {
            command: Mutex::new(AnimationPipelineCommand::default()),
            wake: Condvar::new(),
            raw_queue: Mutex::new(VecDeque::new()),
            raw_wake: Condvar::new(),
            raw_space: Condvar::new(),
            drop_stale_raw: AtomicBool::new(false),
        };
        let make_raw = |index| RawAnimFrame {
            index,
            frame: AnimFrame {
                image: image::RgbaImage::new(2, 2),
                delay: std::time::Duration::from_millis(10),
            },
            source_size: (2, 2),
            decode_elapsed: std::time::Duration::ZERO,
        };

        assert!(pipeline.push_raw(make_raw(1)));
        assert!(pipeline.push_raw(make_raw(2)));
        assert_eq!(
            pipeline.raw_queue.lock().unwrap().iter().map(|raw| raw.index).collect::<Vec<_>>(),
            vec![1, 2],
        );

        pipeline.drop_stale_raw.store(true, Ordering::Release);
        assert!(pipeline.push_raw(make_raw(4)));
        assert_eq!(
            pipeline.raw_queue.lock().unwrap().iter().map(|raw| raw.index).collect::<Vec<_>>(),
            vec![4],
        );
    }

    #[test]
    fn animation_playback_uses_next_frame_until_drop_mode_is_enabled() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((10, 10)),
            true,
        )
        .expect("GIF should decode");
        let PageContent::Animated(ring) = content else { panic!("expected animation") };

        assert!(ring.with_frame(2, |_| ()).is_some());
        assert_eq!(ring.playback_ready_after(0), Some(1));

        ring.pipeline.drop_stale_raw.store(true, Ordering::Release);
        assert_eq!(ring.playback_ready_after(0), Some(2));
    }

    #[test]
    fn animation_instance_id_is_stable_for_arc_clones_and_unique_for_redecode() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10)]);
        let decode = || {
            let content = decode_ring_anim(
                &bytes,
                AnimFormat::Gif,
                image::imageops::FilterType::Triangle,
                TEST_RING_BUDGET_BYTES,
                TEST_RING_BOUNDS,
                TEST_FRAME_HARD_LIMIT_BYTES,
                Some((10, 10)),
                true,
            )
            .expect("GIF should decode");
            let PageContent::Animated(ring) = content else { panic!("expected animation") };
            ring
        };

        let first = decode();
        let shared = Arc::clone(&first);
        let replacement = decode();

        assert!(Arc::ptr_eq(&first, &shared));
        assert_eq!(first.instance_id(), shared.instance_id());
        assert_ne!(first.instance_id(), replacement.instance_id());
    }

    #[test]
    fn ring_anim_background_request_continues_across_loop_boundary() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (10, 10), (10, 10)]);
        let content = decode_ring_anim(
            &bytes,
            AnimFormat::Gif,
            image::imageops::FilterType::Triangle,
            TEST_RING_BUDGET_BYTES,
            TEST_RING_BOUNDS,
            TEST_FRAME_HARD_LIMIT_BYTES,
            Some((10, 10)),
            true,
        )
        .expect("GIF should decode");
        let PageContent::Animated(ring) = content else { panic!("expected animation") };

        ring.request_frame(2);
        wait_for_async_frame(&ring, 2);
        ring.request_frame(3);
        wait_for_async_frame(&ring, 3);
    }

    /// フェーズ5: 同一アニメ内でframe0より大幅に大きい中間フレームに遭遇しても、
    /// そのフレームだけ縮小されてhard_limit以内に収まり、再生が継続できることを確認する。
    #[test]
    fn ring_anim_downscales_oversized_mid_stream_frame() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (4000, 4000), (10, 10)]);
        let hard_limit_bytes = 1000; // 4000x4000の生サイズ(64,000,000 bytes)を大きく下回る極小値

        let content = decode_ring_anim(&bytes, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, hard_limit_bytes, Some((1920, 1080)), true)
            .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("3フレームあるので Animated になるはず");
        };

        let (w, h) = ring.with_frame(1, |f| (f.image.width(), f.image.height()))
            .expect("frame1が取得できるはず");
        assert!((w as usize) * (h as usize) * 4 <= hard_limit_bytes, "縮小後もhard_limitに収まるはず: {w}x{h}");

        // 縮小後もframe2(通常サイズに戻る)へ普通に進行できることを確認する。
        assert!(ring.with_frame(2, |_| ()).is_some());
    }

    /// フェーズ5: frame0自体が超過しても、静止画に丸ごとフォールバックせず
    /// 縮小した上でアニメーションとして続行することを確認する。
    #[test]
    fn ring_anim_downscales_oversized_frame0_and_stays_animated() {
        let bytes = encode_gif_frames_mixed(&[(4000, 4000), (10, 10)]);
        let hard_limit_bytes = 1000;

        let content = decode_ring_anim(&bytes, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, hard_limit_bytes, Some((1920, 1080)), true)
            .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("2フレームあるので Animated になるはず（静止画フォールバックしない）");
        };

        let (w, h) = ring.with_frame(0, |f| (f.image.width(), f.image.height()))
            .expect("frame0が取得できるはず");
        assert!((w as usize) * (h as usize) * 4 <= hard_limit_bytes, "frame0も縮小されhard_limitに収まるはず: {w}x{h}");
    }
}
