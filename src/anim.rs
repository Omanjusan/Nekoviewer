use std::time::Duration;
use image::RgbaImage;

const DEFAULT_DELAY_MS: u64 = 100;
const MIN_DELAY_MS: u64 = 10;

pub struct AnimFrame {
    pub image: RgbaImage,
    pub delay: Duration,
}

pub struct AnimatedImage {
    pub frames: Vec<AnimFrame>,
    /// 0 = 無限ループ
    pub loop_count: u32,
}

impl AnimatedImage {
    /// `limit_bytes` を超えた時点でデコードを打ち切り None を返す（インクリメンタルガード）。
    /// 全フレームをデコードし終えてからサイズ判定する「後追い」実装だと、
    /// 1枚で巨大に展開されるアニメーションに対してガードが手遅れになるため。
    pub fn from_gif(data: &[u8], limit_bytes: usize, label: &str) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).ok()?;
        let mut frames = Vec::new();
        let mut total: usize = 0;
        for frame_result in decoder.into_frames() {
            let frame = frame_result.ok()?;
            let delay = delay_from_image(frame.delay());
            let image = frame.into_buffer();
            total += frame_bytes(&image);
            if total > limit_bytes {
                log_anim_too_large(label, total, limit_bytes);
                return None;
            }
            frames.push(AnimFrame { image, delay });
        }
        if frames.is_empty() { return None; }
        Some(Self { frames, loop_count: 0 })
    }

    pub fn from_apng(data: &[u8], limit_bytes: usize, label: &str) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(data)).ok()?;
        if !decoder.is_apng().ok()? { return None; }
        let mut frames = Vec::new();
        let mut total: usize = 0;
        for frame_result in decoder.apng().ok()?.into_frames() {
            let frame = frame_result.ok()?;
            let delay = delay_from_image(frame.delay());
            let image = frame.into_buffer();
            total += frame_bytes(&image);
            if total > limit_bytes {
                log_anim_too_large(label, total, limit_bytes);
                return None;
            }
            frames.push(AnimFrame { image, delay });
        }
        if frames.is_empty() { return None; }
        Some(Self { frames, loop_count: 0 })
    }

    pub fn from_avif(data: &[u8], limit_bytes: usize, label: &str) -> Option<Self> {
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
            let mut frames = Vec::with_capacity((*decoder).imageCount as usize);
            let mut total: usize = 0;

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
                    total += frame_bytes(&frame.image);
                    if total > limit_bytes {
                        log_anim_too_large(label, total, limit_bytes);
                        return None;
                    }
                    frames.push(frame);
                }
            }

            if frames.is_empty() { return None; }
            Some(Self { frames, loop_count: 0 })
        }
    }

    /// webp::AnimDecoder::decode() は libwebp 内部で全フレームをデコードしてから返すため、
    /// フレーム単位の途中打ち切りができない。代わりにデコード前にヘッダ情報
    /// (キャンバスサイズ・フレーム数)だけを取得し、展開後サイズを事前見積もりして
    /// 閾値超過ならデコード自体を行わずに打ち切る。
    pub fn from_webp(data: &[u8], limit_bytes: usize, label: &str) -> Option<Self> {
        if let Some(projected) = webp_projected_bytes(data) {
            if projected > limit_bytes {
                log_anim_too_large(label, projected, limit_bytes);
                return None;
            }
        }

        let anim = webp::AnimDecoder::new(data).decode().ok()?;
        if !anim.has_animation() { return None; }
        let loop_count = anim.loop_count;

        // webp のタイムスタンプは累積ms。フレーム間の差分をディレイとして使う。
        let webp_frames: Vec<_> = anim.into_iter().collect();
        let frames: Vec<AnimFrame> = webp_frames
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
