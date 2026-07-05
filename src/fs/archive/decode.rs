//! フォーマット非依存の画像バイト処理。
//! シグネチャ判定・静止画/アニメーションのデコードとデコード後サイズ見積もりを担う。

/// バイト列から静止画をデコードする（内部ラッパ）。
pub(crate) fn decode_image(buf: &[u8]) -> Option<image::DynamicImage> {
    decode_image_bytes(buf)
}

/// RIFF/WEBP シグネチャを先頭バイトから判定する
fn has_webp_signature(buf: &[u8]) -> bool {
    buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP"
}

/// ftypボックスのブランドが avif/avis かどうかで AVIF シグネチャを判定する
fn has_avif_signature(buf: &[u8]) -> bool {
    buf.len() >= 12 && &buf[4..8] == b"ftyp" && matches!(&buf[8..12], b"avif" | b"avis")
}

/// バイト列から静止画をデコードする（外部から呼び出し可能）。
/// 拡張子ではなく先頭バイトのシグネチャで実フォーマットを判定するため、
/// 拡張子と中身が食い違うファイルでも正しいデコーダへ振り分けられる。
pub fn decode_image_bytes(buf: &[u8]) -> Option<image::DynamicImage> {
    if has_webp_signature(buf) {
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
    } else if has_avif_signature(buf) {
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
