use crate::{log_perf};
use crate::anim::{AnimatedImage, AnimFrame};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{FilterType as FirFilter, PixelType, ResizeAlg, ResizeOptions, Resizer};

const PAGE_CACHE_RAM_PCT: usize = 25;
const FILE_CACHE_RAM_PCT: usize = 5;
const MIN_RATIO_PCT: usize = 40; // page_max に対する page_min の割合
const FALLBACK_TOTAL_BYTES: usize = 500 * 1024 * 1024; // sysinfo 失敗時フォールバック（旧30%相当）

/// ページキャッシュ・ファイルキャッシュの予算を一括解決する。
/// 返り値: (page_max, page_min, file_max)
pub fn resolve_cache_budgets(
    page_cache_max_mb: Option<u64>,
    file_cache_max_mb: Option<u64>,
) -> (usize, usize, usize) {
    let total_ram = {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.total_memory() as usize
    };

    let page_max = match page_cache_max_mb {
        Some(mb) => (mb as usize) * 1024 * 1024,
        None => if total_ram > 0 {
            total_ram * PAGE_CACHE_RAM_PCT / 100
        } else {
            FALLBACK_TOTAL_BYTES * PAGE_CACHE_RAM_PCT / (PAGE_CACHE_RAM_PCT + FILE_CACHE_RAM_PCT)
        },
    };

    let file_max = match file_cache_max_mb {
        Some(mb) => (mb as usize) * 1024 * 1024,
        None => if total_ram > 0 {
            total_ram * FILE_CACHE_RAM_PCT / 100
        } else {
            FALLBACK_TOTAL_BYTES * FILE_CACHE_RAM_PCT / (PAGE_CACHE_RAM_PCT + FILE_CACHE_RAM_PCT)
        },
    };

    let page_min = page_max * MIN_RATIO_PCT / 100;
    (page_max, page_min, file_max)
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
    /// FileCache ヒット時のファイルバイト列。Some のときディスクI/Oをスキップしてメモリから読む。
    pub file_bytes: Option<Arc<[u8]>>,
}

/// ワーカースレッド内で保持する開きっぱなしアーカイブ（ディスク版・メモリ版を統合）
enum OpenArchive {
    Disk(zip::ZipArchive<std::fs::File>),
    Mem(zip::ZipArchive<std::io::Cursor<Arc<[u8]>>>),
}

impl OpenArchive {
    fn load_page(&mut self, entry_name: &str, filter: image::imageops::FilterType) -> Option<PageContent> {
        match self {
            Self::Disk(a) => load_page_content(a, entry_name, filter),
            Self::Mem(a)  => load_page_content(a, entry_name, filter),
        }
    }
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
pub fn spawn_worker(filter: image::imageops::FilterType, num_threads: usize, ctx: egui::Context) -> (mpsc::Sender<LoadRequest>, mpsc::Receiver<LoadResult>) {
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
                let content = if req.is_raw_file {
                    if let Some(bytes) = req.file_bytes {
                        load_raw_content_from_bytes(&bytes, &req.archive_path, filter)
                    } else {
                        load_raw_file_content(&req.archive_path, filter)
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
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter))
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
                    open_archive.as_mut().and_then(|(_, a)| a.load_page(&req.entry_name, filter))
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
fn load_raw_file_content(path: &std::path::Path, filter: image::imageops::FilterType) -> Option<PageContent> {
    let buf = std::fs::read(path).ok()?;
    load_raw_content_from_bytes(&buf, path, filter)
}

/// バイト列から生画像を PageContent としてデコードする（FileCache ヒット時用）
fn load_raw_content_from_bytes(buf: &[u8], path: &std::path::Path, filter: image::imageops::FilterType) -> Option<PageContent> {
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
    if lower == "avif" {
        if let Some(anim) = AnimatedImage::from_avif(&buf) {
            return Some(PageContent::Animated(resize_anim_for_display(anim, filter)));
        }
    }

    let display_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let img = crate::fs::archive::decode_image_bytes(&buf, display_name)?;
    Some(PageContent::Static(resize_for_display(img, filter)))
}

/// アーカイブエントリを PageContent としてデコードする。GIF/WebP/APNGはアニメーション展開、それ以外は静止画。
fn load_page_content<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
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
    if lower.ends_with(".avif") {
        if let Some(anim) = AnimatedImage::from_avif(&buf) {
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
        PageContent::Animated(anim) => anim.frames.iter().map(|f| rgba_bytes(&f.image)).sum(),
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
