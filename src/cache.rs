use crate::{log_perf};
use crate::anim::{AnimFrame, AnimFormat, SequentialAnimDecoder, FrameRingBuffer, resolve_ring_capacity};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType as FirFilter, PixelType, ResizeAlg, ResizeOptions, Resizer};

const KB: usize = 1024;
const MB: usize = 1024 * KB;
const GB: usize = 1024 * MB;

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
const MAX_DISPLAY_W: u32 = 1920;
const MAX_DISPLAY_H: u32 = 1080;
/// フェーズ6: リサイズ再デコード未接続時（起動直後など）のデコード上限既定値。
/// 従来の固定 MAX_DISPLAY_W/H と同じ値を使う。
pub const DEFAULT_DECODE_TARGET: (u32, u32) = (MAX_DISPLAY_W, MAX_DISPLAY_H);

// ワーカースレッドへのロード要求
pub struct LoadRequest {
    pub archive_path: PathBuf,
    pub index: usize,
    pub entry_name: String,
    /// true のとき archive_path は ZIP ではなく生画像ファイル（entry_name 不使用）
    pub is_raw_file: bool,
    /// FileCache ヒット時のファイルバイト列。Some のときディスクI/Oをスキップしてメモリから読む。
    pub file_bytes: Option<Arc<[u8]>>,
    /// フェーズ6: デコード時の表示ターゲットサイズ上限（拡大はしない）。
    /// None は無制限（zoom_actual時、原寸）を意味する。
    pub target_size: Option<(u32, u32)>,
}

/// ワーカースレッド内で保持する開きっぱなしアーカイブ（ディスク版・メモリ版を統合）
enum OpenArchive {
    Disk(zip::ZipArchive<std::fs::File>),
    Mem(zip::ZipArchive<std::io::Cursor<Arc<[u8]>>>),
}

impl OpenArchive {
    fn load_page(&mut self, entry_name: &str, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> Option<PageContent> {
        match self {
            Self::Disk(a) => load_page_content(a, entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
            Self::Mem(a)  => load_page_content(a, entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
        }
    }
}

/// ページキャッシュの値型。静止画とアニメーションを統一して扱う。
/// アニメーション(GIF/APNG/AVIF/WebP)は全フレーム一括保持をやめ、
/// 逐次デコード+リングバッファ(`RingAnimation`)で保持する（フェーズ3/3.5）。
pub enum PageContent {
    Static(image::RgbaImage),
    Animated(RingAnimation),
}

// ワーカースレッドからの結果
pub struct LoadResult {
    pub archive_path: PathBuf,
    pub index: usize,
    pub content: PageContent,
}

/// バックグラウンドデコードワーカーを `num_threads` 本起動する。
/// `cache_budget_bytes` はページキャッシュの予算（PageCache::max_bytes）。
/// これを超えて展開されるアニメーションは先頭フレームのみの静止画にフォールバックする。
/// `ring_bounds` はフェーズ4のリング先読み枚数の(下限, 上限)。
/// `frame_hard_limit_bytes` はフェーズ5: 1フレームあたりの生デコードサイズ上限。超過フレームはその場で縮小する。
/// 返り値: (要求送信側, 結果受信側)
pub fn spawn_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize) -> (mpsc::Sender<LoadRequest>, mpsc::Receiver<LoadResult>) {
    let (req_tx, req_rx) = mpsc::channel::<LoadRequest>();
    let (res_tx, res_rx) = mpsc::channel::<LoadResult>();

    // Receiver を Arc<Mutex> で包んで複数スレッドに共有する
    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..num_threads {
        let req_rx = Arc::clone(&req_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // 直前に開いたアーカイブをキープオープンする（ディスク版・メモリ版を統合）
            let mut open_archive: Option<(PathBuf, OpenArchive)> = None;

            loop {
                // ロックはメッセージ取り出しのみに使用し、デコード中は解放される
                let req = match req_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break, // Sender が drop されたら終了
                };

                let t_total = std::time::Instant::now();
                let target_size = req.target_size;
                let content = if req.is_raw_file {
                    if let Some(bytes) = req.file_bytes {
                        load_raw_content_from_bytes(&bytes, &req.archive_path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size)
                    } else {
                        load_raw_file_content(&req.archive_path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size)
                    }
                } else if let Some(bytes) = req.file_bytes {
                    // FileCache ヒット: メモリからアーカイブを開く
                    let is_same_mem = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Mem(_))
                    });
                    if !is_same_mem {
                        open_archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
                            .ok()
                            .map(|a| (req.archive_path.clone(), OpenArchive::Mem(a)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size))
                } else {
                    // FileCache ミス: ディスクから開く（従来の動作）
                    let is_same_disk = open_archive.as_ref().map_or(false, |(p, a)| {
                        p == &req.archive_path && matches!(a, OpenArchive::Disk(_))
                    });
                    if !is_same_disk {
                        open_archive = std::fs::File::open(&req.archive_path)
                            .ok()
                            .and_then(|f| zip::ZipArchive::new(f).ok())
                            .map(|a| (req.archive_path.clone(), OpenArchive::Disk(a)));
                    }
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size))
                };

                if let Some(content) = content {
                    log_perf!(
                        "[perf/page] total={:.1}ms entry={}",
                        t_total.elapsed().as_secs_f64() * 1000.0,
                        req.entry_name,
                    );
                    let _ = res_tx.send(LoadResult {
                        archive_path: req.archive_path,
                        index: req.index,
                        content,
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(8));
                }
            }
        });
    }

    (req_tx, res_rx)
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
fn load_raw_file_content(path: &std::path::Path, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> Option<PageContent> {
    let buf = std::fs::read(path).ok()?;
    load_raw_content_from_bytes(&buf, path, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size)
}

/// GIF/APNG/AVIF/WebP（フェーズ3/3.5）をリングバッファ方式でデコードし PageContent に変換する。
/// 実質1フレームしか無い場合は静止画として扱う。対象フォーマットでない場合は None。
fn decode_ring_anim(buf: &[u8], format: AnimFormat, filter: image::imageops::FilterType, ring_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> Option<PageContent> {
    match RingAnimation::from_source(format, std::sync::Arc::from(buf), filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size) {
        RingDecodeOutcome::NotThisFormat => None,
        RingDecodeOutcome::SingleFrame(frame) => {
            Some(PageContent::Static(frame.image))
        }
        RingDecodeOutcome::Animated(ring) => {
            Some(PageContent::Animated(ring))
        }
    }
}

/// 拡張子（ドットなし小文字）からアニメーションデコードを試みて PageContent を返す。
/// 対象外の拡張子や静止画は None。
/// `cache_budget_bytes` はページキャッシュ全体の予算（page_max）。フェーズ4では
/// その一部（ANIM_RING_BUDGET_PCT）をアニメ1本のリング予算として使う。
/// `target_size` はフェーズ6: デコード時の表示上限（Noneは無制限=原寸）。
fn decode_anim_from_ext(buf: &[u8], ext: &str, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> Option<PageContent> {
    let ring_budget_bytes = cache_budget_bytes * ANIM_RING_BUDGET_PCT / 100;
    match ext {
        "gif"  => decode_ring_anim(buf, AnimFormat::Gif, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
        "png"  => decode_ring_anim(buf, AnimFormat::Apng, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
        "avif" => decode_ring_anim(buf, AnimFormat::Avif, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
        "webp" => decode_ring_anim(buf, AnimFormat::Webp, filter, ring_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size),
        _      => None,
    }
}

/// バイト列から生画像を PageContent としてデコードする（FileCache ヒット時用）
fn load_raw_content_from_bytes(buf: &[u8], path: &std::path::Path, filter: image::imageops::FilterType, cache_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> Option<PageContent> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if let Some(c) = decode_anim_from_ext(buf, &ext, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size) {
        return Some(c);
    }

    let display_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let img = crate::fs::archive::decode_image_bytes(buf, display_name)?;
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
) -> Option<PageContent> {
    let (buf, display_name) = crate::fs::archive::load_bytes_from_archive(archive, entry_name)?;

    let lower = entry_name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

    if let Some(c) = decode_anim_from_ext(&buf, ext, filter, cache_budget_bytes, ring_bounds, frame_hard_limit_bytes, target_size) {
        return Some(c);
    }

    let img = crate::fs::archive::decode_image_bytes(&buf, &display_name)?;
    Some(PageContent::Static(resize_for_display(img, filter, target_size)))
}

/// `target` に縮小する（拡大はしない）。`target` が None のときは無制限（原寸のまま）。
/// フェーズ6: 従来の固定 MAX_DISPLAY_W/H から、リサイズ再デコード対応のため呼び出し側指定に変更。
pub fn resize_for_display(img: image::DynamicImage, filter: image::imageops::FilterType, target: Option<(u32, u32)>) -> image::RgbaImage {
    let Some((tw, th)) = target else {
        return img.to_rgba8();
    };
    let (w, h) = (img.width(), img.height());
    let scale = (tw as f32 / w as f32)
        .min(th as f32 / h as f32)
        .min(1.0);
    if scale < 1.0 {
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
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
    decoder: SequentialAnimDecoder,
    ring: FrameRingBuffer,
    /// 次に decoder.next_frame() で得られるフレームに割り振るインデックス
    next_index: usize,
    resize_to: Option<(u32, u32)>,
    filter: image::imageops::FilterType,
    /// フェーズ5: 1フレームあたりの生デコードサイズ上限（超過フレームはその場で縮小）
    frame_hard_limit_bytes: usize,
}

/// 全フレームを一括保持せず、逐次デコード+リングバッファで保持するアニメーション。
/// 再生は前進のみを前提とし、デコーダが終端(None)を返した時点がループ境界の合図になる。
/// その際は `restart()` でデコーダを先頭から作り直す（この再デコードによる一瞬のフリーズは許容する）。
pub struct RingAnimation {
    state: Mutex<RingAnimState>,
}

impl RingAnimation {
    /// `ring_budget_bytes` はこのアニメ1本に割り当てるリング予算、`ring_bounds` は(下限, 上限)の先読み枚数。
    /// `frame_hard_limit_bytes` はフェーズ5: 1フレームの生デコードサイズ上限。frame0・中間フレームの
    /// どちらも超過時はそのフレームだけ縮小して継続する（同一アニメ内で解像度が変則的なファイル対策）。
    /// フレーム0のバイト数から通常表示用のリサイズ先・リング容量を算出し、以降の再生中は変更しない（フェーズ4）。
    /// `target_size` はフェーズ6: 表示上限（Noneは無制限=原寸のまま）。
    fn from_source(format: AnimFormat, source: Arc<[u8]>, filter: image::imageops::FilterType, ring_budget_bytes: usize, ring_bounds: (usize, usize), frame_hard_limit_bytes: usize, target_size: Option<(u32, u32)>) -> RingDecodeOutcome {
        let mut decoder = match SequentialAnimDecoder::new(format, source) {
            Some(d) => d,
            None => return RingDecodeOutcome::NotThisFormat,
        };
        let frame0 = match decoder.next_frame() {
            Some(f) => f,
            None => return RingDecodeOutcome::NotThisFormat,
        };
        let frame0 = Self::guard_frame_size(frame0, frame_hard_limit_bytes, filter, 0);

        let (w, h) = (frame0.image.width(), frame0.image.height());
        let resize_to = target_size.and_then(|(tw, th)| {
            let scale = (tw as f32 / w as f32).min(th as f32 / h as f32).min(1.0);
            if scale < 1.0 {
                Some((((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1)))
            } else {
                None
            }
        });

        let frame0 = Self::apply_resize(frame0, resize_to, filter);

        let frame1 = match decoder.next_frame() {
            Some(f) => f,
            None => return RingDecodeOutcome::SingleFrame(frame0),
        };
        let frame1 = Self::guard_frame_size(frame1, frame_hard_limit_bytes, filter, 1);
        let frame1 = Self::apply_resize(frame1, resize_to, filter);

        // 容量算出はresize後のフレームサイズ基準（実際にリングへ乗るバイト数と一致させるため）。
        let resized_frame_bytes = (frame0.image.width() as usize) * (frame0.image.height() as usize) * 4;
        let (min_frames, max_frames) = ring_bounds;
        let capacity = resolve_ring_capacity(resized_frame_bytes, ring_budget_bytes, min_frames, max_frames);
        let mut ring = FrameRingBuffer::new(capacity);
        ring.push(0, frame0);
        ring.push(1, frame1);

        let state = RingAnimState { decoder, ring, next_index: 2, resize_to, filter, frame_hard_limit_bytes };
        RingDecodeOutcome::Animated(Self { state: Mutex::new(state) })
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

    /// index番目のフレームが手に入るまでデコードを進め、見つかったフレームへの参照でfを呼ぶ
    /// (RGBAバッファの不要なコピーを避けるため)。デコーダが終端に達し index が存在しないと
    /// わかった場合は None（呼び出し側はループ境界として扱い `restart()` を呼ぶ）。
    pub fn with_frame<R>(&self, index: usize, f: impl FnOnce(&AnimFrame) -> R) -> Option<R> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(frame) = state.ring.get(index) {
                return Some(f(frame));
            }
            if index < state.next_index {
                // 前進専用のためエビクト済みフレームへは戻れない。
                return None;
            }
            let next = state.decoder.next_frame()?;
            let idx = state.next_index;
            state.next_index += 1;
            let next = Self::guard_frame_size(next, state.frame_hard_limit_bytes, state.filter, idx);
            let resized = Self::apply_resize(next, state.resize_to, state.filter);
            state.ring.push(idx, resized);
        }
    }

    /// ループ境界（最終フレーム→先頭）: デコーダを元データから作り直す。
    pub fn restart(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.decoder.restart() {
            state.ring.clear();
            state.next_index = 0;
            true
        } else {
            false
        }
    }

    /// 現在リングバッファに乗っている分だけの推定バイト数（アニメ全体ではない）。
    pub fn resident_bytes(&self) -> usize {
        self.state.lock().unwrap().ring.total_bytes()
    }
}

pub struct PageCache {
    entries: HashMap<(PathBuf, usize), PageContent>,
    total_bytes: usize,
    max_bytes: usize,
    min_bytes: usize,
    /// LRU 予算を超える単一アイテムを表示のためだけに保持するスロット（1件のみ）
    bypass: Option<((PathBuf, usize), PageContent)>,
    /// 一度でも予算超過(bypass)と判定された(path, index)の記憶。
    /// bypass スロットから追い出された後も先読みが再要求しないようにするためのもので、
    /// 中身は保持しない（キャッシュを汚染しない）。
    known_bypass: HashSet<(PathBuf, usize)>,
}

impl PageCache {
    pub fn new(max_bytes: usize, min_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            total_bytes: 0,
            max_bytes,
            min_bytes,
            bypass: None,
            known_bypass: HashSet::new(),
        }
    }

    pub fn total_bytes(&self) -> usize { self.total_bytes }
    pub fn max_bytes(&self) -> usize { self.max_bytes }

    pub fn contains(&self, path: &PathBuf, index: usize) -> bool {
        self.entries.contains_key(&(path.clone(), index))
            || self.bypass.as_ref()
                .map_or(false, |((bp, bi), _)| bp == path && *bi == index)
    }

    /// この(path, index)が過去に予算超過(bypass)と判定されたことがあるか。
    /// 先読みウィンドウが同じページを何度もデコードし直すループを防ぐために使う。
    pub fn is_known_bypass(&self, path: &PathBuf, index: usize) -> bool {
        self.known_bypass.contains(&(path.clone(), index))
    }

    pub fn get(&self, path: &PathBuf, index: usize) -> Option<&PageContent> {
        self.entries.get(&(path.clone(), index)).or_else(|| {
            self.bypass.as_ref().and_then(|((bp, bi), c)| {
                if bp == path && *bi == index { Some(c) } else { None }
            })
        })
    }

    /// フェーズ6: 再デコードのため既存エントリを強制的に破棄する（bypassスロットも対象）。
    /// 次の insert() で新しいデコード結果を通常どおり入れ直す前提。
    pub fn remove(&mut self, path: &PathBuf, index: usize) {
        if let Some(content) = self.entries.remove(&(path.clone(), index)) {
            self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
        }
        if let Some(((bp, bi), _)) = &self.bypass {
            if bp == path && *bi == index {
                self.bypass = None;
            }
        }
        self.known_bypass.remove(&(path.clone(), index));
    }

    /// キャッシュに追加する。予算超過時は最遠エントリを evict する。
    /// 単一アイテムが予算全体を超える場合は LRU を汚さず bypass スロットに格納する。
    pub fn insert(
        &mut self,
        path: PathBuf,
        index: usize,
        content: PageContent,
        current_path: &PathBuf,
        current_index: usize,
    ) {
        let incoming = content_bytes(&content);

        if incoming >= self.max_bytes {
            eprintln!(
                "[cache] bypass: {:?}[{}] {}MB > budget {}MB",
                path, index,
                incoming / MB,
                self.max_bytes / MB,
            );
            self.known_bypass.insert((path.clone(), index));
            self.bypass = Some(((path, index), content));
            return;
        }

        // 現在位置が変わっていたら stale な bypass エントリを解放する
        if let Some(((bp, bi), _)) = &self.bypass {
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
        self.entries.insert((path, index), content);
    }

    /// 現在ページから最も遠いエントリを1件解放する。
    /// 別アーカイブのエントリは同アーカイブより常に遠いとみなす。
    fn evict_furthest(&mut self, current_path: &PathBuf, current_index: usize) {
        let key = self
            .entries
            .keys()
            .max_by_key(|(path, idx)| {
                let other_archive = path != current_path;
                let dist = idx.abs_diff(current_index);
                (other_archive, dist)
            })
            .cloned();

        if let Some(key) = key {
            if let Some(content) = self.entries.remove(&key) {
                self.total_bytes = self.total_bytes.saturating_sub(content_bytes(&content));
            }
        }
    }
}

// ── ファイル単位キャッシュ ──────────────────────────────────────────────────────

/// 圧縮済みファイルバイト列をまるごとメモリに保持するキャッシュ。
/// ヒット時はストレージI/Oをスキップしてメモリからデコードできる。
pub struct FileCache {
    entries: HashMap<PathBuf, std::sync::Arc<[u8]>>,
    total_bytes: usize,
    max_bytes: usize,
}

impl FileCache {
    pub fn new(max_bytes: usize) -> Self {
        Self { entries: HashMap::new(), total_bytes: 0, max_bytes }
    }

    pub fn max_bytes(&self) -> usize { self.max_bytes }
    pub fn total_bytes(&self) -> usize { self.total_bytes }

    pub fn get(&self, path: &PathBuf) -> Option<std::sync::Arc<[u8]>> {
        self.entries.get(path).cloned()
    }

    pub fn contains(&self, path: &PathBuf) -> bool {
        self.entries.contains_key(path)
    }

    /// ファイルバイト列をキャッシュに追加する。
    /// 予算超過時は `all_paths` 上の位置距離が最も遠いエントリを evict する。
    pub fn insert(
        &mut self,
        path: PathBuf,
        bytes: Arc<[u8]>,
        current_path: &PathBuf,
        all_paths: &[PathBuf],
    ) {
        let incoming = bytes.len();
        while self.total_bytes + incoming > self.max_bytes && !self.entries.is_empty() {
            self.evict_furthest(current_path, all_paths);
        }
        self.total_bytes += incoming;
        self.entries.insert(path, bytes);
    }

    fn evict_furthest(&mut self, current_path: &PathBuf, all_paths: &[PathBuf]) {
        let current_pos = all_paths.iter().position(|p| p == current_path).unwrap_or(0);
        let key = self.entries.keys().max_by_key(|path| {
            all_paths.iter().position(|p| p == *path)
                .map(|pos| (pos as isize - current_pos as isize).unsigned_abs())
                .unwrap_or(usize::MAX)
        }).cloned();

        if let Some(key) = key {
            if let Some(bytes) = self.entries.remove(&key) {
                self.total_bytes -= bytes.len();
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
        PageContent::Animated(ring) => ring.resident_bytes(),
    }
}

// ── ファイルキャッシュワーカー ─────────────────────────────────────────────────

/// ファイルバイト列をバックグラウンドで読み込む単一スレッドのワーカーを起動する。
/// 返り値: (要求送信側, 結果受信側)
pub fn spawn_file_cache_worker(ctx: egui::Context) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<(PathBuf, Arc<[u8]>)>) {
    let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
    let (res_tx, res_rx) = mpsc::channel::<(PathBuf, Arc<[u8]>)>();
    std::thread::spawn(move || {
        while let Ok(path) = req_rx.recv() {
            if let Ok(bytes) = std::fs::read(&path) {
                let arc: Arc<[u8]> = Arc::from(bytes);
                let _ = res_tx.send((path, arc));
                // ROOT を起こして poll_workers に結果を回収させる
                ctx.request_repaint();
            }
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
}

pub struct ThumbResult {
    pub path: PathBuf,
    pub rgba: Option<image::RgbaImage>,
}

pub fn spawn_thumb_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context) -> (mpsc::SyncSender<ThumbRequest>, mpsc::Receiver<ThumbResult>) {
    let capacity = num_threads * 2;
    let (req_tx, req_rx) = mpsc::sync_channel::<ThumbRequest>(capacity);
    let (res_tx, res_rx) = mpsc::channel::<ThumbResult>();

    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..num_threads {
        let req_rx = Arc::clone(&req_rx);
        let res_tx = res_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            loop {
                let req = match req_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                // 失敗（None）でも必ず返送し、呼び元が thumb_pending を解放できるようにする
                let rgba = resolve_thumb(&req, filter);
                let _ = res_tx.send(ThumbResult { path: req.archive_path, rgba });
                // ROOT を起こして poll_workers に結果を回収させる
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

/// 1件のサムネイルリクエストを処理する。失敗時は None を返す（スレッドは死なない）。
fn resolve_thumb(req: &ThumbRequest, filter: image::imageops::FilterType) -> Option<image::RgbaImage> {
    let filename = req.archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned();
    let source_mtime = crate::neko_dir::file_mtime(&req.archive_path);

    // DBキャッシュを試みる
    if let Some(ref db) = req.db {
        if let Some(jpeg) = crate::neko_dir::read_thumb(db, &filename, source_mtime) {
            let t0 = std::time::Instant::now();
            let result = image::load_from_memory(&jpeg).ok().map(|img| img.to_rgba8());
            log_perf!("[perf/thumb] db_cache={:.1}ms", t0.elapsed().as_secs_f64() * 1000.0);
            return result;
        }
    }

    // キャッシュミス: 元ファイルから生成
    let rgba = if req.is_raw_file {
        let buf = std::fs::read(&req.archive_path).ok()?;
        let display_name = req.archive_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let img = crate::fs::archive::decode_image_bytes(&buf, display_name)?;
        resize_thumbnail(img, filter)
    } else {
        let t_total = std::time::Instant::now();
        let is_smb = crate::fs::dir::is_gvfs_path(&req.archive_path);
        let img = if is_smb {
            load_first_image_smb(req.archive_path.clone())?
        } else {
            crate::fs::archive::load_first_image(&req.archive_path)?
        };
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

    // DBに保存
    if let Some(ref db) = req.db {
        if let Some(jpeg) = encode_jpeg(&rgba) {
            crate::neko_dir::write_thumb(db, &filename, source_mtime, &jpeg);
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

    /// テスト用: 十分大きい予算を与え、容量は常に上限(32)に張り付かせる
    /// （フェーズ3.6時点の固定容量32枚での期待値をそのまま維持するため）。
    const TEST_RING_BUDGET_BYTES: usize = 10 * GB;
    const TEST_RING_BOUNDS: (usize, usize) = (4, 32);
    const TEST_RING_MAX: usize = TEST_RING_BOUNDS.1;
    /// フェーズ5: 実際のデフォルト(100MB)と同じ値。テストフィクスチャは全て十分小さいので影響しない。
    const TEST_FRAME_HARD_LIMIT_BYTES: usize = 100 * MB;

    /// フェーズ3.6: 実物の大きいGIF(test/nouka.gif, 640x360 1316フレーム, 全展開なら約1.2GB)で
    /// リングバッファが実際に「全フレーム常駐にならず一定量に収まる」ことを確認する結合テスト。
    #[test]
    fn ring_anim_stays_bounded_on_real_large_gif() {
        let path = std::path::Path::new("test/nouka.gif");
        let buf = std::fs::read(path).expect("test/nouka.gif が見つからない");

        let content = decode_ring_anim(&buf, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)))
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
    }

    /// フェーズ3.6: ループ境界(終端→restart→先頭)が実際に機能することを確認する。
    #[test]
    fn ring_anim_restart_replays_from_head_on_real_gif() {
        let path = std::path::Path::new("test/nouka.gif");
        let buf = std::fs::read(path).expect("test/nouka.gif が見つからない");

        let content = decode_ring_anim(&buf, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)))
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

        let content = decode_ring_anim(&buf, AnimFormat::Webp, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)))
            .expect("WebPとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("243フレームあるので Animated になるはず（Static ではない）");
        };

        // 表示サイズ縮小後の1フレーム分バイト数を、エビクトされる前(ループ前)に取得しておく
        // (960x1376はMAX_DISPLAY_H(1080)を超えるため縮小されている前提)。
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

        let content = decode_ring_anim(&buf, AnimFormat::Webp, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, TEST_FRAME_HARD_LIMIT_BYTES, Some((1920, 1080)))
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

    /// フェーズ5: 同一アニメ内でframe0より大幅に大きい中間フレームに遭遇しても、
    /// そのフレームだけ縮小されてhard_limit以内に収まり、再生が継続できることを確認する。
    #[test]
    fn ring_anim_downscales_oversized_mid_stream_frame() {
        let bytes = encode_gif_frames_mixed(&[(10, 10), (4000, 4000), (10, 10)]);
        let hard_limit_bytes = 1000; // 4000x4000の生サイズ(64,000,000 bytes)を大きく下回る極小値

        let content = decode_ring_anim(&bytes, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, hard_limit_bytes, Some((1920, 1080)))
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

        let content = decode_ring_anim(&bytes, AnimFormat::Gif, image::imageops::FilterType::Triangle, TEST_RING_BUDGET_BYTES, TEST_RING_BOUNDS, hard_limit_bytes, Some((1920, 1080)))
            .expect("GIFとしてデコードできるはず");
        let PageContent::Animated(ring) = content else {
            panic!("2フレームあるので Animated になるはず（静止画フォールバックしない）");
        };

        let (w, h) = ring.with_frame(0, |f| (f.image.width(), f.image.height()))
            .expect("frame0が取得できるはず");
        assert!((w as usize) * (h as usize) * 4 <= hard_limit_bytes, "frame0も縮小されhard_limitに収まるはず: {w}x{h}");
    }
}
