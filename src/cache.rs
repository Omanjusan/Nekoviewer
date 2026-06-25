use crate::{log_perf};
use crate::anim::{AnimatedImage, AnimFrame};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType as FirFilter, PixelType, ResizeAlg, ResizeOptions, Resizer};

const RAM_RATIO_PCT: usize = 30;
const MIN_RATIO_PCT: usize = 40; // max_bytes に対する min_bytes の割合
const FALLBACK_MAX_BYTES: usize = 500 * 1024 * 1024; // sysinfo 失敗時フォールバック

pub fn resolve_cache_budget(cache_max_mb: Option<u64>) -> (usize, usize) {
    let max_bytes = match cache_max_mb {
        Some(mb) => (mb as usize) * 1024 * 1024,
        None => {
            let total = {
                let mut sys = sysinfo::System::new();
                sys.refresh_memory();
                sys.total_memory() as usize
            };
            if total > 0 {
                total * RAM_RATIO_PCT / 100
            } else {
                FALLBACK_MAX_BYTES
            }
        }
    };
    let min_bytes = max_bytes * MIN_RATIO_PCT / 100;
    (max_bytes, min_bytes)
}
const MAX_DISPLAY_W: u32 = 1920;
const MAX_DISPLAY_H: u32 = 1080;

// ワーカースレッドへのロード要求
pub struct LoadRequest {
    pub archive_path: PathBuf,
    pub index: usize,
    pub entry_name: String,
    /// true のとき archive_path は ZIP ではなく生画像ファイル（entry_name 不使用）
    pub is_raw_file: bool,
}

/// ページキャッシュの値型。静止画とアニメーション（全フレーム展開済み）を統一して扱う
pub enum PageContent {
    Static(image::RgbaImage),
    Animated(AnimatedImage),
}

// ワーカースレッドからの結果
pub struct LoadResult {
    pub archive_path: PathBuf,
    pub index: usize,
    pub content: PageContent,
}

/// バックグラウンドデコードワーカーを `num_threads` 本起動する。
/// 返り値: (要求送信側, 結果受信側)
pub fn spawn_worker(filter: image::imageops::FilterType, num_threads: usize) -> (mpsc::Sender<LoadRequest>, mpsc::Receiver<LoadResult>) {
    let (req_tx, req_rx) = mpsc::channel::<LoadRequest>();
    let (res_tx, res_rx) = mpsc::channel::<LoadResult>();

    // Receiver を Arc<Mutex> で包んで複数スレッドに共有する
    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..num_threads {
        let req_rx = Arc::clone(&req_rx);
        let res_tx = res_tx.clone();
        std::thread::spawn(move || {
            // 直前に開いたアーカイブをキープオープンする
            let mut open_archive: Option<(PathBuf, zip::ZipArchive<std::fs::File>)> = None;

            loop {
                // ロックはメッセージ取り出しのみに使用し、デコード中は解放される
                let req = match req_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break, // Sender が drop されたら終了
                };

                let t_total = std::time::Instant::now();
                let content = if req.is_raw_file {
                    load_raw_file_content(&req.archive_path, filter)
                } else {
                    // 別のアーカイブに切り替わったら開き直す
                    let is_same = open_archive.as_ref().map_or(false, |(p, _)| p == &req.archive_path);
                    if !is_same {
                        open_archive = std::fs::File::open(&req.archive_path)
                            .ok()
                            .and_then(|f| zip::ZipArchive::new(f).ok())
                            .map(|a| (req.archive_path.clone(), a));
                    }
                    open_archive.as_mut().and_then(|(_, a)| load_page_content(a, &req.entry_name, filter))
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
fn load_raw_file_content(path: &std::path::Path, filter: image::imageops::FilterType) -> Option<PageContent> {
    let buf = std::fs::read(path).ok()?;
    let lower = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if lower == "gif" {
        if let Some(anim) = AnimatedImage::from_gif(&buf) {
            if anim.frames.len() > 1 {
                return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
            }
            let img = image::DynamicImage::ImageRgba8(anim.frames.into_iter().next()?.image);
            return Some(PageContent::Static(resize_for_display(img, filter)));
        }
    }
    if lower == "webp" {
        if let Some(anim) = AnimatedImage::from_webp(&buf) {
            if anim.frames.len() > 1 {
                return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
            }
            let img = image::DynamicImage::ImageRgba8(anim.frames.into_iter().next()?.image);
            return Some(PageContent::Static(resize_for_display(img, filter)));
        }
    }
    if lower == "png" {
        if let Some(anim) = AnimatedImage::from_apng(&buf) {
            return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
        }
    }

    let display_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let img = crate::fs::archive::decode_image_bytes(&buf, display_name)?;
    Some(PageContent::Static(resize_for_display(img, filter)))
}

/// アーカイブエントリを PageContent としてデコードする。GIF/WebP/APNGはアニメーション展開、それ以外は静止画。
fn load_page_content(
    archive: &mut zip::ZipArchive<std::fs::File>,
    entry_name: &str,
    filter: image::imageops::FilterType,
) -> Option<PageContent> {
    let (buf, display_name) = crate::fs::archive::load_bytes_from_archive(archive, entry_name)?;

    let lower = entry_name.to_ascii_lowercase();

    if lower.ends_with(".gif") {
        if let Some(anim) = AnimatedImage::from_gif(&buf) {
            if anim.frames.len() > 1 {
                return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
            }
            let img = image::DynamicImage::ImageRgba8(anim.frames.into_iter().next()?.image);
            return Some(PageContent::Static(resize_for_display(img, filter)));
        }
    }

    if lower.ends_with(".webp") {
        if let Some(anim) = AnimatedImage::from_webp(&buf) {
            if anim.frames.len() > 1 {
                return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
            }
            let img = image::DynamicImage::ImageRgba8(anim.frames.into_iter().next()?.image);
            return Some(PageContent::Static(resize_for_display(img, filter)));
        }
    }

    if lower.ends_with(".png") {
        if let Some(anim) = AnimatedImage::from_apng(&buf) {
            return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
        }
    }

    let img = crate::fs::archive::decode_image_bytes(&buf, &display_name)?;
    Some(PageContent::Static(resize_for_display(img, filter)))
}

/// AnimatedImage の全フレームを最大表示サイズに縮小する（拡大はしない）
fn resize_anim_for_display(anim: AnimatedImage, filter: image::imageops::FilterType) -> AnimatedImage {
    let (w, h) = {
        let f = &anim.frames[0];
        (f.image.width(), f.image.height())
    };
    let scale = (MAX_DISPLAY_W as f32 / w as f32)
        .min(MAX_DISPLAY_H as f32 / h as f32)
        .min(1.0);

    if scale >= 1.0 {
        return anim;
    }

    let nw = ((w as f32 * scale) as u32).max(1);
    let nh = ((h as f32 * scale) as u32).max(1);

    let frames = anim.frames.into_iter().map(|f| {
        let dynamic = image::DynamicImage::ImageRgba8(f.image);
        AnimFrame { image: fir_resize(dynamic, nw, nh, filter), delay: f.delay }
    }).collect();

    AnimatedImage { frames, loop_count: anim.loop_count }
}

/// 最大表示サイズ（1920×1080）に縮小する（拡大はしない）
pub fn resize_for_display(img: image::DynamicImage, filter: image::imageops::FilterType) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let scale = (MAX_DISPLAY_W as f32 / w as f32)
        .min(MAX_DISPLAY_H as f32 / h as f32)
        .min(1.0);
    if scale < 1.0 {
        let nw = ((w as f32 * scale) as u32).max(1);
        let nh = ((h as f32 * scale) as u32).max(1);
        fir_resize(img, nw, nh, filter)
    } else {
        img.to_rgba8()
    }
}

pub struct PageCache {
    entries: HashMap<(PathBuf, usize), PageContent>,
    total_bytes: usize,
    max_bytes: usize,
    min_bytes: usize,
}

impl PageCache {
    pub fn new(max_bytes: usize, min_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            total_bytes: 0,
            max_bytes,
            min_bytes,
        }
    }

    pub fn total_bytes(&self) -> usize { self.total_bytes }
    pub fn max_bytes(&self) -> usize { self.max_bytes }

    pub fn contains(&self, path: &PathBuf, index: usize) -> bool {
        self.entries.contains_key(&(path.clone(), index))
    }

    pub fn get(&self, path: &PathBuf, index: usize) -> Option<&PageContent> {
        self.entries.get(&(path.clone(), index))
    }

    /// キャッシュに追加する。予算超過時は最遠エントリを evict する。
    pub fn insert(
        &mut self,
        path: PathBuf,
        index: usize,
        content: PageContent,
        current_path: &PathBuf,
        current_index: usize,
    ) {
        let incoming = content_bytes(&content);
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
                self.total_bytes -= content_bytes(&content);
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
        PageContent::Animated(anim) => anim.frames.iter().map(|f| rgba_bytes(&f.image)).sum(),
    }
}

// ── サムネイルワーカー ──────────────────────────────────────────────────────

pub struct ThumbRequest {
    pub archive_path: PathBuf,
    /// ディスクキャッシュJPEGのパス。存在すればZIPを開かずそこから読む。
    pub thumb_path: Option<PathBuf>,
    /// true のとき archive_path は ZIP ではなく生画像ファイル
    pub is_raw_file: bool,
}

pub struct ThumbResult {
    pub path: PathBuf,
    pub rgba: Option<image::RgbaImage>,
}

pub fn spawn_thumb_worker(filter: image::imageops::FilterType, num_threads: usize) -> (mpsc::SyncSender<ThumbRequest>, mpsc::Receiver<ThumbResult>) {
    let capacity = num_threads * 2;
    let (req_tx, req_rx) = mpsc::sync_channel::<ThumbRequest>(capacity);
    let (res_tx, res_rx) = mpsc::channel::<ThumbResult>();

    let req_rx = Arc::new(Mutex::new(req_rx));

    for _ in 0..num_threads {
        let req_rx = Arc::clone(&req_rx);
        let res_tx = res_tx.clone();
        std::thread::spawn(move || {
            loop {
                let req = match req_rx.lock().unwrap().recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                // 失敗（None）でも必ず返送し、呼び元が thumb_pending を解放できるようにする
                let rgba = resolve_thumb(&req, filter);
                let _ = res_tx.send(ThumbResult { path: req.archive_path, rgba });
            }
        });
    }

    (req_tx, res_rx)
}

/// SMB（gvfs）パスのZIPをバックグラウンドスレッドで読み込む。
/// ワーカースレッド自体はブロックされないため次のリクエストを処理できる。
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
    if req.is_raw_file {
        // ディスクキャッシュがあれば使用（元ファイルより新しい場合のみ）
        if let Some(ref tp) = req.thumb_path {
            if tp.exists() && !is_source_newer(&req.archive_path, tp) {
                return image::open(tp).ok().map(|img| img.to_rgba8());
            }
        }
        // 生ファイルを直接読んでリサイズ
        let buf = std::fs::read(&req.archive_path).ok()?;
        let display_name = req.archive_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let img = crate::fs::archive::decode_image_bytes(&buf, display_name)?;
        let rgba = resize_thumbnail(img, filter);
        if let Some(ref tp) = req.thumb_path {
            save_thumb_jpeg(&rgba, tp);
        }
        return Some(rgba);
    }

    let is_smb = crate::fs::dir::is_gvfs_path(&req.archive_path);

    if let Some(ref tp) = req.thumb_path {
        if tp.exists() && !is_source_newer(&req.archive_path, tp) {
            // ディスクキャッシュから読み込み（元ファイルより新しい場合のみ）
            let t0 = std::time::Instant::now();
            let result = image::open(tp).ok().map(|img| img.to_rgba8());
            log_perf!(
                "[perf/thumb] disk_cache={:.1}ms",
                t0.elapsed().as_secs_f64() * 1000.0,
            );
            result
        } else {
            // ZIPから生成してディスクに保存
            let t_total = std::time::Instant::now();
            let img = if is_smb {
                load_first_image_smb(req.archive_path.clone())?
            } else {
                crate::fs::archive::load_first_image(&req.archive_path)?
            };
            let t_load = t_total.elapsed();
            let t2 = std::time::Instant::now();
            let rgba = resize_thumbnail(img, filter);
            let t_resize = t2.elapsed();
            log_perf!(
                "[perf/thumb] load={:.1}ms resize={:.1}ms total={:.1}ms",
                t_load.as_secs_f64() * 1000.0,
                t_resize.as_secs_f64() * 1000.0,
                t_total.elapsed().as_secs_f64() * 1000.0,
            );
            save_thumb_jpeg(&rgba, tp);
            Some(rgba)
        }
    } else {
        // ディスクキャッシュ無効: ZIPから生成のみ
        let t_total = std::time::Instant::now();
        let img = if is_smb {
            load_first_image_smb(req.archive_path.clone())?
        } else {
            crate::fs::archive::load_first_image(&req.archive_path)?
        };
        let t_load = t_total.elapsed();
        let t2 = std::time::Instant::now();
        let rgba = resize_thumbnail(img, filter);
        let t_resize = t2.elapsed();
        log_perf!(
            "[perf/thumb] load={:.1}ms resize={:.1}ms total={:.1}ms",
            t_load.as_secs_f64() * 1000.0,
            t_resize.as_secs_f64() * 1000.0,
            t_total.elapsed().as_secs_f64() * 1000.0,
        );
        Some(rgba)
    }
}

/// 元ファイルの更新時刻がサムネキャッシュより新しいかどうかを返す。
/// 時刻取得に失敗した場合は再生成を促すため true を返す。
fn is_source_newer(source: &std::path::Path, thumb: &std::path::Path) -> bool {
    let source_mtime = std::fs::metadata(source).and_then(|m| m.modified());
    let thumb_mtime = std::fs::metadata(thumb).and_then(|m| m.modified());
    match (source_mtime, thumb_mtime) {
        (Ok(s), Ok(t)) => s > t,
        _ => true,
    }
}

fn save_thumb_jpeg(rgba: &image::RgbaImage, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // JPEGはアルファチャンネル非対応のためRGBに変換
    let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
    let _ = rgb.save(path);
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
