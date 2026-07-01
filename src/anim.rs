use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use image::RgbaImage;

const DEFAULT_DELAY_MS: u64 = 100;
const MIN_DELAY_MS: u64 = 10;

#[derive(Clone)]
pub struct AnimFrame {
    pub image: RgbaImage,
    pub delay: Duration,
}

pub struct AnimatedImage {
    pub frames: Vec<AnimFrame>,
    /// 0 = 無限ループ
    pub loop_count: u32,
}

/// フレームを1枚ずつ受け取り、2段階のサイズ判定を行うアキュムレータ。
/// - `hard_limit_bytes` 超過: アニメーション全体を破棄（`push` が Err を返す）。
/// - `cache_budget_bytes` 超過: アニメーションとしての再生を諦め、
///   既に確定している先頭1フレームだけを残して打ち切る（静止画フォールバック）。
///   ページキャッシュに載り切らないアニメーションを毎回全フレーム展開してから
///   bypass するのは無駄が大きく、ループ再生でユーザー体験も損なうため、
///   「載らないと分かった時点」で以降のデコードをやめて先頭フレームの静止画に倒す。
struct FrameAccumulator<'a> {
    frames: Vec<AnimFrame>,
    total: usize,
    hard_limit_bytes: usize,
    cache_budget_bytes: usize,
    label: &'a str,
}

impl<'a> FrameAccumulator<'a> {
    fn new(hard_limit_bytes: usize, cache_budget_bytes: usize, label: &'a str) -> Self {
        Self { frames: Vec::new(), total: 0, hard_limit_bytes, cache_budget_bytes, label }
    }

    /// `Ok(true)`: 継続, `Ok(false)`: 静止画フォールバックとして打ち切り, `Err(())`: 全体を破棄。
    fn push(&mut self, frame: AnimFrame) -> Result<bool, ()> {
        self.total += frame_bytes(&frame.image);
        if self.total > self.hard_limit_bytes {
            log_anim_too_large(self.label, self.total, self.hard_limit_bytes);
            return Err(());
        }
        if self.frames.is_empty() {
            // 先頭フレームは静止画フォールバック用に必ず確保する。
            self.frames.push(frame);
            return Ok(true);
        }
        if self.total <= self.cache_budget_bytes {
            self.frames.push(frame);
            Ok(true)
        } else {
            log_anim_truncated_to_static(self.label, self.total, self.cache_budget_bytes);
            self.frames.truncate(1);
            Ok(false)
        }
    }

    fn finish(self) -> Option<AnimatedImage> {
        if self.frames.is_empty() { return None; }
        Some(AnimatedImage { frames: self.frames, loop_count: 0 })
    }
}

impl AnimatedImage {
    /// 全フレームの合計デコード後バイト数（フェーズ2のサンプル見積もり用）。
    pub fn total_bytes(&self) -> usize {
        self.frames.iter().map(|f| frame_bytes(&f.image)).sum()
    }

    pub fn from_gif(data: &[u8], hard_limit_bytes: usize, cache_budget_bytes: usize, label: &str) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).ok()?;
        let mut acc = FrameAccumulator::new(hard_limit_bytes, cache_budget_bytes, label);
        for frame_result in decoder.into_frames() {
            let frame = frame_result.ok()?;
            let delay = delay_from_image(frame.delay());
            let image = frame.into_buffer();
            match acc.push(AnimFrame { image, delay }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(()) => return None,
            }
        }
        acc.finish()
    }

    pub fn from_apng(data: &[u8], hard_limit_bytes: usize, cache_budget_bytes: usize, label: &str) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(data)).ok()?;
        if !decoder.is_apng().ok()? { return None; }
        let mut acc = FrameAccumulator::new(hard_limit_bytes, cache_budget_bytes, label);
        for frame_result in decoder.apng().ok()?.into_frames() {
            let frame = frame_result.ok()?;
            let delay = delay_from_image(frame.delay());
            let image = frame.into_buffer();
            match acc.push(AnimFrame { image, delay }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(()) => return None,
            }
        }
        acc.finish()
    }

    pub fn from_avif(data: &[u8], hard_limit_bytes: usize, cache_budget_bytes: usize, label: &str) -> Option<Self> {
        use libavif_sys::*;

        struct DecoderGuard(*mut avifDecoder);
        impl Drop for DecoderGuard {
            fn drop(&mut self) {
                unsafe { avifDecoderDestroy(self.0); }
            }
        }

        unsafe {
            let decoder = avifDecoderCreate();
            if decoder.is_null() { return None; }
            let _guard = DecoderGuard(decoder);

            if avifDecoderSetIOMemory(decoder, data.as_ptr(), data.len()) != AVIF_RESULT_OK {
                return None;
            }
            if avifDecoderParse(decoder) != AVIF_RESULT_OK {
                return None;
            }

            // アニメーションのみ対象（静止画は decode_avif で処理済み）
            if (*decoder).imageCount <= 1 { return None; }

            let timescale = (*decoder).timescale as f64;
            let mut acc = FrameAccumulator::new(hard_limit_bytes, cache_budget_bytes, label);

            while avifDecoderNextImage(decoder) == AVIF_RESULT_OK {
                let avif_image = (*decoder).image;
                if avif_image.is_null() { break; }

                let w = (*avif_image).width;
                let h = (*avif_image).height;

                let mut rgb: avifRGBImage = std::mem::zeroed();
                avifRGBImageSetDefaults(&mut rgb, avif_image);
                rgb.format = AVIF_RGB_FORMAT_RGBA;
                rgb.depth = 8;

                if avifRGBImageAllocatePixels(&mut rgb) != AVIF_RESULT_OK { continue; }

                let ok = avifImageYUVToRGB(avif_image, &mut rgb) == AVIF_RESULT_OK;
                let mut pushed_frame: Option<AnimFrame> = None;
                if ok {
                    let pixels_len = (rgb.rowBytes * h) as usize;
                    let pixels = std::slice::from_raw_parts(rgb.pixels, pixels_len).to_vec();

                    let duration_in_timescales = (*decoder).imageTiming.durationInTimescales as f64;
                    let delay_ms = if timescale > 0.0 {
                        ((duration_in_timescales / timescale * 1000.0) as u64).max(MIN_DELAY_MS)
                    } else {
                        DEFAULT_DELAY_MS
                    };

                    if let Some(image) = RgbaImage::from_raw(w, h, pixels) {
                        pushed_frame = Some(AnimFrame { image, delay: std::time::Duration::from_millis(delay_ms) });
                    }
                }
                avifRGBImageFreePixels(&mut rgb);

                if let Some(frame) = pushed_frame {
                    match acc.push(frame) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(()) => return None,
                    }
                }
            }

            acc.finish()
        }
    }

    /// webp::AnimDecoder::decode() は libwebp 内部で全フレームをデコードしてから返すため、
    /// フレーム単位の途中打ち切りができない。まずヘッダ情報(キャンバスサイズ・フレーム数)だけで
    /// ハードリミット超過が確定しているものだけデコード自体を回避し、残りは通常通りデコードした後に
    /// 実サイズでキャッシュ予算を判定して先頭フレームのみへ切り詰める。
    pub fn from_webp(data: &[u8], hard_limit_bytes: usize, cache_budget_bytes: usize, label: &str) -> Option<Self> {
        if let Some(projected) = webp_projected_bytes(data) {
            if projected > hard_limit_bytes {
                log_anim_too_large(label, projected, hard_limit_bytes);
                return None;
            }
        }

        let anim = webp::AnimDecoder::new(data).decode().ok()?;
        if !anim.has_animation() { return None; }
        let loop_count = anim.loop_count;

        // webp のタイムスタンプは累積ms。フレーム間の差分をディレイとして使う。
        let webp_frames: Vec<_> = anim.into_iter().collect();
        let mut frames: Vec<AnimFrame> = webp_frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let ts = f.get_time_ms().max(0) as u64;
                let prev_ts = if i == 0 {
                    0
                } else {
                    webp_frames[i - 1].get_time_ms().max(0) as u64
                };
                let delay_ms = ts.saturating_sub(prev_ts).max(MIN_DELAY_MS);
                let image = rgba_from_webp_frame(f);
                AnimFrame { image, delay: Duration::from_millis(delay_ms) }
            })
            .collect();
        if frames.is_empty() { return None; }

        let total: usize = frames.iter().map(|f| frame_bytes(&f.image)).sum();
        if total > hard_limit_bytes {
            log_anim_too_large(label, total, hard_limit_bytes);
            return None;
        }
        if total > cache_budget_bytes && frames.len() > 1 {
            log_anim_truncated_to_static(label, total, cache_budget_bytes);
            frames.truncate(1);
        }

        Some(Self { frames, loop_count })
    }
}

fn frame_bytes(img: &RgbaImage) -> usize {
    (img.width() as usize) * (img.height() as usize) * 4
}

fn log_anim_too_large(label: &str, total: usize, limit: usize) {
    const MB: usize = 1024 * 1024;
    eprintln!(
        "[cache] {} animation too large ({}MB > {}MB limit), skipping",
        label,
        total / MB,
        limit / MB,
    );
}

fn log_anim_truncated_to_static(label: &str, total: usize, budget: usize) {
    const MB: usize = 1024 * 1024;
    eprintln!(
        "[cache] {} animation exceeds cache budget ({}MB > {}MB), falling back to first frame as static",
        label,
        total / MB,
        budget / MB,
    );
}

/// WebPAnimDecoderGetInfo だけを呼んでキャンバスサイズとフレーム数を取得し、
/// 全フレームデコード後の推定バイト数（キャンバスサイズ×フレーム数×4）を返す。
/// フレームは常にキャンバス全面のRGBAバッファとして返されるため、この見積もりは正確な上限になる。
/// 取得に失敗した場合は None（呼び出し側は事前チェックをスキップし、通常のデコードに進む）。
fn webp_projected_bytes(data: &[u8]) -> Option<usize> {
    use libwebp_sys::*;

    unsafe {
        let mut dec_options: WebPAnimDecoderOptions = std::mem::zeroed();
        dec_options.color_mode = WEBP_CSP_MODE::MODE_RGBA;
        if WebPAnimDecoderOptionsInitInternal(&mut dec_options, WebPGetDemuxABIVersion()) == 0 {
            return None;
        }

        let webp_data = WebPData { bytes: data.as_ptr(), size: data.len() };
        let dec = WebPAnimDecoderNewInternal(&webp_data, &dec_options, WebPGetDemuxABIVersion());
        if dec.is_null() { return None; }

        let mut anim_info: WebPAnimInfo = std::mem::zeroed();
        let ok = WebPAnimDecoderGetInfo(dec, &mut anim_info);
        WebPAnimDecoderDelete(dec);
        if ok == 0 { return None; }

        let per_frame = (anim_info.canvas_width as usize) * (anim_info.canvas_height as usize) * 4;
        Some(per_frame.saturating_mul(anim_info.frame_count as usize))
    }
}

fn delay_from_image(d: image::Delay) -> Duration {
    let (numer, denom) = d.numer_denom_ms();
    let ms = if denom == 0 {
        DEFAULT_DELAY_MS
    } else {
        ((numer as u64) / (denom as u64)).max(MIN_DELAY_MS)
    };
    Duration::from_millis(ms)
}

fn rgba_from_webp_frame(f: &webp::AnimFrame) -> RgbaImage {
    let (w, h) = (f.width(), f.height());
    let raw = f.get_image();
    match f.get_layout() {
        webp::PixelLayout::Rgba => {
            RgbaImage::from_raw(w, h, raw.to_vec()).unwrap_or_else(|| RgbaImage::new(w, h))
        }
        webp::PixelLayout::Rgb => {
            let rgba: Vec<u8> = raw.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            RgbaImage::from_raw(w, h, rgba).unwrap_or_else(|| RgbaImage::new(w, h))
        }
    }
}

// ---- フェーズ3: リングバッファ（GIF/APNG/AVIF）----
//
// 再生は前進のみ・ループはモジュロという前提のもと、アニメの全フレームを
// 一括デコードして常駐させるのをやめ、「1フレームずつ逐次デコードするデコーダ」を
// 持ち回してリングバッファに載せる。GIF/APNG/AVIFはフレーム間差分合成(dispose/blend)方式で
// 任意フレームへのランダムアクセスができないため、ループ境界(最終フレーム→先頭)でのみ
// `restart()` でデコーダを元データから作り直す（この再デコードによる一瞬のフリーズは許容する）。

/// フェーズ3のリングバッファが対象にするフォーマット。WebPはフェーズ3.5で別途対応。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnimFormat {
    Gif,
    Apng,
    Avif,
    Webp,
}

enum SeqDecoderKind {
    /// GIF/APNGは image クレートの Frames イテレータをそのまま持ち回す。
    ImageCrate(image::Frames<'static>),
    Avif(AvifSeqState),
    Webp(WebpSeqState),
}

/// 1フレームずつ逐次デコードする状態。ランダムアクセス不可・前進のみ。
pub struct SequentialAnimDecoder {
    kind: SeqDecoderKind,
    source: Arc<[u8]>,
    format: AnimFormat,
}

impl SequentialAnimDecoder {
    pub fn new(format: AnimFormat, source: Arc<[u8]>) -> Option<Self> {
        let kind = Self::build_kind(format, &source)?;
        Some(Self { kind, source, format })
    }

    fn build_kind(format: AnimFormat, source: &Arc<[u8]>) -> Option<SeqDecoderKind> {
        use image::AnimationDecoder;
        match format {
            AnimFormat::Gif => {
                let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(Arc::clone(source))).ok()?;
                Some(SeqDecoderKind::ImageCrate(decoder.into_frames()))
            }
            AnimFormat::Apng => {
                let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(Arc::clone(source))).ok()?;
                if !decoder.is_apng().ok()? { return None; }
                Some(SeqDecoderKind::ImageCrate(decoder.apng().ok()?.into_frames()))
            }
            AnimFormat::Avif => {
                let state = unsafe { AvifSeqState::create(source) }?;
                Some(SeqDecoderKind::Avif(state))
            }
            AnimFormat::Webp => {
                let state = unsafe { WebpSeqState::create(source) }?;
                Some(SeqDecoderKind::Webp(state))
            }
        }
    }

    /// 次の1フレームをデコードする。アニメ終端に達したら None。
    pub fn next_frame(&mut self) -> Option<AnimFrame> {
        match &mut self.kind {
            SeqDecoderKind::ImageCrate(it) => {
                let frame = it.next()?.ok()?;
                let delay = delay_from_image(frame.delay());
                Some(AnimFrame { image: frame.into_buffer(), delay })
            }
            SeqDecoderKind::Avif(state) => state.next_frame(),
            SeqDecoderKind::Webp(state) => state.next_frame(),
        }
    }

    /// ループ境界（最終フレーム→先頭）: デコーダを元データから作り直し、フレーム0から再開する。
    /// 失敗時は既存の状態を保持したまま false を返す。
    pub fn restart(&mut self) -> bool {
        match Self::build_kind(self.format, &self.source) {
            Some(kind) => { self.kind = kind; true }
            None => false,
        }
    }
}

// SAFETY: `SeqDecoderKind::ImageCrate` は image クレートの GifDecoder/PngDecoder が
// `Cursor<Arc<[u8]>>` の上に構築する純粋な安全なRustの状態（スレッド固有のものは無い）だが、
// `Box<dyn Iterator + 'a>` の型消去により Send が自動導出されない。
// `SeqDecoderKind::Avif`/`SeqDecoderKind::Webp` の生ポインタ(`*mut avifDecoder`/`*mut WebPAnimDecoder`)は
// libavif/libwebp が確保するヒープ上の自己完結したオブジェクトで、生成スレッドに紐付く状態を持たない。
// いずれも「複数スレッドから同時アクセスされない(所有権の移動のみ)」という利用条件下でのみ
// 安全であり、実際の呼び出し元(cache.rs)ではデコード完了後にチャネルで1回だけ他スレッドへ
// 移動し、以降は `Mutex` 経由の排他アクセスのみが行われる。
unsafe impl Send for SequentialAnimDecoder {}

struct AvifSeqState {
    decoder: *mut libavif_sys::avifDecoder,
    timescale: f64,
}

impl AvifSeqState {
    unsafe fn create(source: &[u8]) -> Option<Self> {
        use libavif_sys::*;
        unsafe {
            let decoder = avifDecoderCreate();
            if decoder.is_null() { return None; }
            if avifDecoderSetIOMemory(decoder, source.as_ptr(), source.len()) != AVIF_RESULT_OK {
                avifDecoderDestroy(decoder);
                return None;
            }
            if avifDecoderParse(decoder) != AVIF_RESULT_OK {
                avifDecoderDestroy(decoder);
                return None;
            }
            if (*decoder).imageCount <= 1 {
                avifDecoderDestroy(decoder);
                return None;
            }
            let timescale = (*decoder).timescale as f64;
            Some(Self { decoder, timescale })
        }
    }

    fn next_frame(&mut self) -> Option<AnimFrame> {
        use libavif_sys::*;
        unsafe {
            if avifDecoderNextImage(self.decoder) != AVIF_RESULT_OK {
                return None;
            }
            let avif_image = (*self.decoder).image;
            if avif_image.is_null() { return None; }

            let w = (*avif_image).width;
            let h = (*avif_image).height;

            let mut rgb: avifRGBImage = std::mem::zeroed();
            avifRGBImageSetDefaults(&mut rgb, avif_image);
            rgb.format = AVIF_RGB_FORMAT_RGBA;
            rgb.depth = 8;

            if avifRGBImageAllocatePixels(&mut rgb) != AVIF_RESULT_OK { return None; }

            let ok = avifImageYUVToRGB(avif_image, &mut rgb) == AVIF_RESULT_OK;
            let result = if ok {
                let pixels_len = (rgb.rowBytes * h) as usize;
                let pixels = std::slice::from_raw_parts(rgb.pixels, pixels_len).to_vec();

                let duration_in_timescales = (*self.decoder).imageTiming.durationInTimescales as f64;
                let delay_ms = if self.timescale > 0.0 {
                    ((duration_in_timescales / self.timescale * 1000.0) as u64).max(MIN_DELAY_MS)
                } else {
                    DEFAULT_DELAY_MS
                };

                RgbaImage::from_raw(w, h, pixels)
                    .map(|image| AnimFrame { image, delay: Duration::from_millis(delay_ms) })
            } else {
                None
            };
            avifRGBImageFreePixels(&mut rgb);
            result
        }
    }
}

impl Drop for AvifSeqState {
    fn drop(&mut self) {
        unsafe { libavif_sys::avifDecoderDestroy(self.decoder); }
    }
}

/// フェーズ3.5: WebPアニメの逐次デコード状態。libwebpの`WebPAnimDecoderGetNext`は
/// もともと1フレームずつ取り出す逐次イテレータとして設計されている
/// （`webp`クレートの`AnimDecoder::decode()`は内部でこれを全フレーム分ループしているだけ）。
/// ランダムシークは不可（前進のみ）で、`WebPAnimDecoderReset`で先頭に巻き戻せる。
struct WebpSeqState {
    decoder: *mut libwebp_sys::WebPAnimDecoder,
    width: u32,
    height: u32,
    /// webpの累積タイムスタンプ(ms)からフレーム間差分をdelayとして算出するための直前値。
    prev_ts_ms: u64,
}

impl WebpSeqState {
    unsafe fn create(source: &[u8]) -> Option<Self> {
        use libwebp_sys::*;
        unsafe {
            let mut dec_options: WebPAnimDecoderOptions = std::mem::zeroed();
            dec_options.color_mode = WEBP_CSP_MODE::MODE_RGBA;
            if WebPAnimDecoderOptionsInitInternal(&mut dec_options, WebPGetDemuxABIVersion()) == 0 {
                return None;
            }

            let webp_data = WebPData { bytes: source.as_ptr(), size: source.len() };
            let dec = WebPAnimDecoderNewInternal(&webp_data, &dec_options, WebPGetDemuxABIVersion());
            if dec.is_null() { return None; }

            let mut info: WebPAnimInfo = std::mem::zeroed();
            if WebPAnimDecoderGetInfo(dec, &mut info) == 0 {
                WebPAnimDecoderDelete(dec);
                return None;
            }
            if info.frame_count <= 1 {
                WebPAnimDecoderDelete(dec);
                return None;
            }

            Some(Self { decoder: dec, width: info.canvas_width, height: info.canvas_height, prev_ts_ms: 0 })
        }
    }

    fn next_frame(&mut self) -> Option<AnimFrame> {
        use libwebp_sys::*;
        unsafe {
            if WebPAnimDecoderHasMoreFrames(self.decoder) == 0 {
                return None;
            }
            let mut buf: *mut u8 = std::ptr::null_mut();
            let mut timestamp: std::os::raw::c_int = 0;
            if WebPAnimDecoderGetNext(self.decoder, &mut buf, &mut timestamp) == 0 {
                return None;
            }
            if buf.is_null() { return None; }

            let pixels_len = (self.width as usize) * (self.height as usize) * 4;
            let pixels = std::slice::from_raw_parts(buf, pixels_len).to_vec();

            let ts_ms = timestamp.max(0) as u64;
            let delay_ms = ts_ms.saturating_sub(self.prev_ts_ms).max(MIN_DELAY_MS);
            self.prev_ts_ms = ts_ms;

            let image = RgbaImage::from_raw(self.width, self.height, pixels)?;
            Some(AnimFrame { image, delay: Duration::from_millis(delay_ms) })
        }
    }
}

impl Drop for WebpSeqState {
    fn drop(&mut self) {
        unsafe { libwebp_sys::WebPAnimDecoderDelete(self.decoder); }
    }
}

/// フェーズ4: リング容量（先読み枚数）を1フレームあたりのバイト数と予算から算出する。
/// アニメ開始時に1回だけ呼ばれ、以降そのアニメーションの再生中は変更しない。
/// `frame_bytes` が0以下、または `budget_bytes` が0の場合は `min_frames` にフォールバックする。
pub fn resolve_ring_capacity(frame_bytes: usize, budget_bytes: usize, min_frames: usize, max_frames: usize) -> usize {
    let min_frames = min_frames.max(1);
    let max_frames = max_frames.max(min_frames);
    if frame_bytes == 0 || budget_bytes == 0 {
        return min_frames;
    }
    (budget_bytes / frame_bytes).clamp(min_frames, max_frames)
}

/// 直近デコードしたフレームだけを保持するリングバッファ。
/// 容量超過時は最も古い(最初にpushされた)フレームから捨てる。
pub struct FrameRingBuffer {
    capacity: usize,
    frames: VecDeque<(usize, AnimFrame)>,
}

impl FrameRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), frames: VecDeque::new() }
    }

    pub fn push(&mut self, index: usize, frame: AnimFrame) {
        if self.frames.len() >= self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back((index, frame));
    }

    pub fn get(&self, index: usize) -> Option<&AnimFrame> {
        self.frames.iter().find(|(i, _)| *i == index).map(|(_, f)| f)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// 現在保持している分だけの合計デコード後バイト数（全フレームではなくリング内のみ）。
    pub fn total_bytes(&self) -> usize {
        self.frames.iter().map(|(_, f)| frame_bytes(&f.image)).sum()
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    fn dummy_frame() -> AnimFrame {
        AnimFrame { image: RgbaImage::new(2, 2), delay: Duration::from_millis(10) }
    }

    #[test]
    fn ring_buffer_evicts_oldest_beyond_capacity() {
        let mut ring = FrameRingBuffer::new(3);
        for i in 0..5 {
            ring.push(i, dummy_frame());
        }
        assert_eq!(ring.len(), 3);
        assert!(ring.get(0).is_none());
        assert!(ring.get(1).is_none());
        assert!(ring.get(2).is_some());
        assert!(ring.get(4).is_some());
    }

    #[test]
    fn ring_buffer_clear_empties_all() {
        let mut ring = FrameRingBuffer::new(2);
        ring.push(0, dummy_frame());
        ring.push(1, dummy_frame());
        ring.clear();
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn resolve_ring_capacity_uses_budget_within_bounds() {
        // 1フレーム1MB、予算10MB → 10枚（下限4/上限32の範囲内）
        let cap = resolve_ring_capacity(1024 * 1024, 10 * 1024 * 1024, 4, 32);
        assert_eq!(cap, 10);
    }

    #[test]
    fn resolve_ring_capacity_clamps_to_min_for_large_frames() {
        // 1フレーム100MB、予算10MB → 0枚相当だが下限4にクランプ
        let cap = resolve_ring_capacity(100 * 1024 * 1024, 10 * 1024 * 1024, 4, 32);
        assert_eq!(cap, 4);
    }

    #[test]
    fn resolve_ring_capacity_clamps_to_max_for_tiny_frames() {
        // 1フレーム1KB、予算10MB → 上限32にクランプ
        let cap = resolve_ring_capacity(1024, 10 * 1024 * 1024, 4, 32);
        assert_eq!(cap, 32);
    }

    #[test]
    fn resolve_ring_capacity_falls_back_to_min_when_zero_input() {
        assert_eq!(resolve_ring_capacity(0, 10 * 1024 * 1024, 4, 32), 4);
        assert_eq!(resolve_ring_capacity(1024, 0, 4, 32), 4);
    }

    #[test]
    fn resolve_ring_capacity_handles_inverted_min_max() {
        // maxがminより小さい不正設定でもクランプが破綻しない
        let cap = resolve_ring_capacity(1024, 10 * 1024 * 1024, 32, 4);
        assert_eq!(cap, 32);
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
    fn sequential_gif_decoder_yields_all_frames_then_none() {
        let bytes: Arc<[u8]> = encode_gif_frames(4, 4, 3).into();
        let mut dec = SequentialAnimDecoder::new(AnimFormat::Gif, bytes).unwrap();
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_none());
    }

    #[test]
    fn sequential_gif_decoder_restart_replays_from_head() {
        let bytes: Arc<[u8]> = encode_gif_frames(4, 4, 2).into();
        let mut dec = SequentialAnimDecoder::new(AnimFormat::Gif, bytes).unwrap();
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_none());

        assert!(dec.restart());
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_some());
        assert!(dec.next_frame().is_none());
    }
}
