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
    pub fn from_gif(data: &[u8]) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)).ok()?;
        let frames: Vec<AnimFrame> = decoder
            .into_frames()
            .collect_frames()
            .ok()?
            .into_iter()
            .map(|f| AnimFrame {
                delay: delay_from_image(f.delay()),
                image: f.into_buffer(),
            })
            .collect();
        if frames.is_empty() { return None; }
        Some(Self { frames, loop_count: 0 })
    }

    pub fn from_apng(data: &[u8]) -> Option<Self> {
        use image::AnimationDecoder;
        let decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(data)).ok()?;
        if !decoder.is_apng().ok()? { return None; }
        let frames: Vec<AnimFrame> = decoder
            .apng().ok()?
            .into_frames()
            .collect_frames()
            .ok()?
            .into_iter()
            .map(|f| AnimFrame {
                delay: delay_from_image(f.delay()),
                image: f.into_buffer(),
            })
            .collect();
        if frames.is_empty() { return None; }
        Some(Self { frames, loop_count: 0 })
    }

    pub fn from_webp(data: &[u8]) -> Option<Self> {
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
                let delay_ms = (ts - prev_ts).max(MIN_DELAY_MS);
                let image = rgba_from_webp_frame(f);
                AnimFrame { image, delay: Duration::from_millis(delay_ms) }
            })
            .collect();
        if frames.is_empty() { return None; }
        Some(Self { frames, loop_count })
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
