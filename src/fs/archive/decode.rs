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

/// WebPのRIFFコンテナからEXIFチャンクを探してOrientationを返す（無ければNoTransforms）。
/// `webp` crateにexif APIが無いため、RIFFチャンク構造（FourCC 4byte + size 4byte LE +
/// データ、偶数バイトにパディング）を自前で走査する。WebPのEXIFチャンクはJPEGのAPP1と
/// 違い"Exif\0\0"プレフィックスを持たず、TIFFヘッダから直接始まるため`from_exif_chunk`に
/// そのまま渡せる。
pub(crate) fn webp_exif_orientation(buf: &[u8]) -> image::metadata::Orientation {
    use image::metadata::Orientation;

    if !has_webp_signature(buf) {
        return Orientation::NoTransforms;
    }
    let mut pos = 12usize;
    while pos + 8 <= buf.len() {
        let fourcc = &buf[pos..pos + 4];
        let size = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(size) else { break };
        if data_end > buf.len() {
            break;
        }
        if fourcc == b"EXIF" {
            return Orientation::from_exif_chunk(&buf[data_start..data_end])
                .unwrap_or(Orientation::NoTransforms);
        }
        // チャンクは偶数バイトにパディングされる
        pos = data_end + (size % 2);
    }
    Orientation::NoTransforms
}

/// バイト列から静止画をデコードする（外部から呼び出し可能）。
/// 拡張子ではなく先頭バイトのシグネチャで実フォーマットを判定するため、
/// 拡張子と中身が食い違うファイルでも正しいデコーダへ振り分けられる。
pub fn decode_image_bytes(buf: &[u8]) -> Option<image::DynamicImage> {
    if has_webp_signature(buf) {
        // このパスは常に単一の静的画像を返す（アニメ再生自体は別経路のRingAnimationが担う）
        // ため、どちらの分岐で取れたフレームに対してもExif Orientationを適用してよい。
        let orientation = webp_exif_orientation(buf);
        // 静止画デコードを先に試みる
        if let Some(mut img) = webp::Decoder::new(buf).decode().map(|w| w.to_image()) {
            img.apply_orientation(orientation);
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
                let mut img = rgba.map(image::DynamicImage::ImageRgba8)?;
                img.apply_orientation(orientation);
                return Some(img);
            }
        }
        None
    } else if has_avif_signature(buf) {
        decode_avif(buf)
    } else {
        decode_native_with_orientation(buf)
    }
}

/// image crateネイティブ対応フォーマット（JPEG/PNG/TIFF/BMP/GIF等）用。
/// Exif Orientationタグを検出し、デコード直後に画素へ適用する。以降の呼び出し元
/// （サムネ生成・ビューアーのページ表示、いずれもここを通る）は回転後の寸法を
/// そのまま使えばよく、個別の回転対応は不要になる。
/// WebP/AVIFは別デコーダ（webp crate / libavif）経由のため対象外（未対応、既知の制限）。
fn decode_native_with_orientation(buf: &[u8]) -> Option<image::DynamicImage> {
    use image::ImageDecoder;

    let reader = image::ImageReader::new(std::io::Cursor::new(buf))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let exif_chunk: Option<Vec<u8>> = decoder.exif_metadata().ok().flatten();
    let orientation = exif_chunk
        .and_then(|chunk| image::metadata::Orientation::from_exif_chunk(&chunk))
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img)
}

/// AVIFコンテナ自身が持つ`irot`(90度単位回転)/`imir`(ミラー)変換と、埋め込みExif Orientation
/// タグの2系統から最終的な向きを決定して適用する。両方あった場合はコンテナ側の`irot`/`imir`を
/// 優先しExifタグは無視する（仕様上Exifは冗長・レガシー扱いで、実際のエンコーダの多くは
/// irot/imirの方を使うため）。`irot`は反時計回りangle*90度、`imir`はaxis=0で上下反転・
/// axis=1で左右反転、この順（回転→ミラー）で適用する。
fn apply_avif_orientation(img: image::DynamicImage, avif_image: *const libavif_sys::avifImage) -> image::DynamicImage {
    use libavif_sys::{AVIF_TRANSFORM_IMIR, AVIF_TRANSFORM_IROT};

    let (transform_flags, irot_angle, imir_axis, exif) = unsafe {
        ((*avif_image).transformFlags, (*avif_image).irot.angle, (*avif_image).imir.axis, (*avif_image).exif)
    };
    let has_irot = transform_flags & AVIF_TRANSFORM_IROT != 0;
    let has_imir = transform_flags & AVIF_TRANSFORM_IMIR != 0;

    if has_irot || has_imir {
        let mut img = img;
        if has_irot {
            // irot.angle: 反時計回りangle*90度。image crateのrotate90/270は時計回りのため対応が逆になる。
            img = match irot_angle {
                1 => img.rotate270(),
                2 => img.rotate180(),
                3 => img.rotate90(),
                _ => img,
            };
        }
        if has_imir {
            img = if imir_axis == 0 { img.flipv() } else { img.fliph() };
        }
        return img;
    }

    if !exif.data.is_null() && exif.size > 0 {
        let exif_bytes = unsafe { std::slice::from_raw_parts(exif.data, exif.size) };
        if let Some(orientation) = image::metadata::Orientation::from_exif_chunk(exif_bytes) {
            let mut img = img;
            img.apply_orientation(orientation);
            return img;
        }
    }
    img
}

/// `libavif`クレート(安全ラッパー)にはExif/irot/imirへのアクセスが無いため、生FFI
/// (`libavif-sys`)で直接デコードする。`anim.rs`の`AvifSeqState`と同系統のFFI呼び出しパターン。
fn decode_avif(buf: &[u8]) -> Option<image::DynamicImage> {
    use libavif_sys::*;

    struct DecoderGuard(*mut avifDecoder);
    impl Drop for DecoderGuard {
        fn drop(&mut self) {
            unsafe { avifDecoderDestroy(self.0) };
        }
    }

    unsafe {
        let decoder = avifDecoderCreate();
        if decoder.is_null() {
            return None;
        }
        let _guard = DecoderGuard(decoder);

        if avifDecoderSetIOMemory(decoder, buf.as_ptr(), buf.len()) != AVIF_RESULT_OK {
            return None;
        }
        if avifDecoderParse(decoder) != AVIF_RESULT_OK {
            return None;
        }
        if avifDecoderNextImage(decoder) != AVIF_RESULT_OK {
            return None;
        }
        let avif_image = (*decoder).image;
        if avif_image.is_null() {
            return None;
        }

        let w = (*avif_image).width;
        let h = (*avif_image).height;

        let mut rgb: avifRGBImage = std::mem::zeroed();
        avifRGBImageSetDefaults(&mut rgb, avif_image);
        rgb.format = AVIF_RGB_FORMAT_RGBA;
        rgb.depth = 8;
        if avifRGBImageAllocatePixels(&mut rgb) != AVIF_RESULT_OK {
            return None;
        }
        let ok = avifImageYUVToRGB(avif_image, &mut rgb) == AVIF_RESULT_OK;
        let pixels = if ok {
            let pixels_len = (rgb.rowBytes * h) as usize;
            Some(std::slice::from_raw_parts(rgb.pixels, pixels_len).to_vec())
        } else {
            None
        };
        avifRGBImageFreePixels(&mut rgb);

        let img = image::RgbaImage::from_raw(w, h, pixels?).map(image::DynamicImage::ImageRgba8)?;
        Some(apply_avif_orientation(img, avif_image))
    }
}

/// フェーズ2: 静止画1エントリのデコード後サイズ(RGBA, byte)をヘッダ情報のみから推定する。
/// ピクセルデータは一切デコードしないため、寸法を偽装したデコンプレッションボム的な
/// エントリが来ても本体デコードは発生しない。ヘッダ解析に失敗した場合は None を返し、
/// 呼び出し側でフルデコードへのフォールバックを判断する。
/// 実際の保持サイズは表示リサイズ後（resize_for_display）なので、見積もりも
/// `max_decode_edge` の箱に収めた縮小後サイズで計算する（原寸基準だと過大になる）。
pub fn estimate_static_decoded_bytes(buf: &[u8], max_decode_edge: u32) -> Option<usize> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(buf))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let (rw, rh) = crate::anim::fit_within(w, h, max_decode_edge, max_decode_edge);
    Some((rw as usize) * (rh as usize) * 4)
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

/// フェーズ2: アニメーション拡張子のエントリの「PageCacheへの計上額」を見積もる。
/// 実デコード時（`cache.rs::RingAnimation::from_source`）と同じ式で
/// 「リング容量 × 表示リサイズ後フレームサイズ」＝予約計上額を算出する。
/// 帳簿（content_bytes）・実常駐上限・見積もりの三者が同一値になる。
/// フレーム0のリサイズ後サイズが単体で budget_bytes を超えるなら即 OverBudget。
///
/// 「非アニメーション」と「予算超過」の区別のため、事前に構造的な非アニメーション判定
/// （PNGのacTLチャンク有無、WebPの静止画デコード可否）を行い、`NotAnimated` を先に弾く。
pub fn estimate_anim_sample_bytes(buf: &[u8], ext: &str, budget_bytes: usize, ring_bounds: (usize, usize), max_decode_edge: u32) -> AnimSampleEstimate {
    use crate::anim::{fit_within, AnimFormat, SequentialAnimDecoder, resolve_ring_capacity};
    use crate::cache::ANIM_RING_BUDGET_PCT;

    /// リング容量・フレームサイズとも実デコード時と同じくリサイズ後基準で算出する。
    /// フレーム0と1の2枚だけデコードすれば予約額が確定する（旧実装のように
    /// リング容量ぶん全フレームをデコードする必要がない）。
    /// 実質1フレームなら NotAnimated（decode_ring_anim の SingleFrame 判定と同じ）。
    fn ring_bounded_estimate(format: AnimFormat, buf: &[u8], budget_bytes: usize, ring_bounds: (usize, usize), max_decode_edge: u32) -> AnimSampleEstimate {
        let Some(mut decoder) = SequentialAnimDecoder::new(format, std::sync::Arc::from(buf)) else {
            return AnimSampleEstimate::NotAnimated;
        };
        let Some(frame0) = decoder.next_frame() else {
            return AnimSampleEstimate::NotAnimated;
        };
        let (w, h) = (frame0.image.width(), frame0.image.height());
        let (rw, rh) = fit_within(w, h, max_decode_edge, max_decode_edge);
        let resized_frame_bytes = (rw as usize) * (rh as usize) * 4;
        if resized_frame_bytes > budget_bytes {
            return AnimSampleEstimate::OverBudget;
        }

        if decoder.next_frame().is_none() {
            // 実質1フレームしかない = 静止画相当
            return AnimSampleEstimate::NotAnimated;
        }

        let ring_budget_bytes = budget_bytes * ANIM_RING_BUDGET_PCT / 100;
        let (min_frames, max_frames) = ring_bounds;
        let capacity = resolve_ring_capacity(resized_frame_bytes, ring_budget_bytes, min_frames, max_frames);
        AnimSampleEstimate::Bytes(capacity.saturating_mul(resized_frame_bytes))
    }

    match ext {
        "gif" => ring_bounded_estimate(AnimFormat::Gif, buf, budget_bytes, ring_bounds, max_decode_edge),
        "webp" => {
            // 静止画WebPはAnimDecoderがデコード失敗またはhas_animation()==falseを返すことが
            // あり、budget_bytes超過とは無関係にNoneになりうる。先に静止画デコードを試して
            // 弾くことで、typicalな静止画WebPページの誤検知(OverBudget誤判定)を避ける。
            if webp::Decoder::new(buf).decode().is_some() {
                return AnimSampleEstimate::NotAnimated;
            }
            ring_bounded_estimate(AnimFormat::Webp, buf, budget_bytes, ring_bounds, max_decode_edge)
        }
        "png" => {
            let is_apng = image::codecs::png::PngDecoder::new(std::io::Cursor::new(buf))
                .ok()
                .and_then(|d| d.is_apng().ok())
                .unwrap_or(false);
            if !is_apng {
                return AnimSampleEstimate::NotAnimated;
            }
            ring_bounded_estimate(AnimFormat::Apng, buf, budget_bytes, ring_bounds, max_decode_edge)
        }
        "avif" => ring_bounded_estimate(AnimFormat::Avif, buf, budget_bytes, ring_bounds, max_decode_edge),
        _ => AnimSampleEstimate::NotAnimated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_image_bytes_reads_tiff() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255])));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Tiff).unwrap();
        let bytes = buf.into_inner();

        let decoded = decode_image_bytes(&bytes).expect("tiff should decode");
        assert_eq!((decoded.width(), decoded.height()), (4, 3));

        let estimated = estimate_static_decoded_bytes(&bytes, 1920).expect("tiff header should be readable");
        assert_eq!(estimated, 4 * 3 * 4);
    }

    /// Orientationタグ1個だけを持つ最小TIFF形式Exifチャンクを組み立てる（値はintel/LE）。
    fn build_minimal_exif_tiff(orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II"); // little-endian
        tiff.extend_from_slice(&0x002Au16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_le_bytes()); // entry count
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // tag: Orientation
        tiff.extend_from_slice(&3u16.to_le_bytes()); // type: SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes()); // count
        tiff.extend_from_slice(&(orientation as u32).to_le_bytes()); // inline value
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset
        tiff
    }

    /// Exif Orientationタグ(6 = 時計回り90度回転)を持つ最小JPEG APP1セグメントを組み立てる。
    fn build_exif_orientation_app1(orientation: u16) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&build_minimal_exif_tiff(orientation));

        let mut app1 = vec![0xFF, 0xE1];
        app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        app1.extend_from_slice(&payload);
        app1
    }

    /// EXIFチャンク（Orientation付き）を1つだけ持つ最小RIFF/WEBPバイト列を組み立てる。
    /// 実際のVP8/VP8Xピクセルデータは含まない（`webp_exif_orientation`はチャンク走査のみ行うため不要）。
    fn build_minimal_webp_with_exif(orientation: u16) -> Vec<u8> {
        let tiff = build_minimal_exif_tiff(orientation);
        let mut exif_chunk = Vec::new();
        exif_chunk.extend_from_slice(b"EXIF");
        exif_chunk.extend_from_slice(&(tiff.len() as u32).to_le_bytes());
        exif_chunk.extend_from_slice(&tiff);
        if tiff.len() % 2 != 0 {
            exif_chunk.push(0); // パディング
        }

        let mut riff_body = Vec::new();
        riff_body.extend_from_slice(b"WEBP");
        riff_body.extend_from_slice(&exif_chunk);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(riff_body.len() as u32).to_le_bytes());
        out.extend_from_slice(&riff_body);
        out
    }

    #[test]
    fn webp_exif_orientation_reads_orientation_tag() {
        let buf = build_minimal_webp_with_exif(6);
        assert_eq!(webp_exif_orientation(&buf), image::metadata::Orientation::Rotate90);
    }

    #[test]
    fn webp_exif_orientation_defaults_to_no_transforms_without_exif_chunk() {
        // testフィクスチャの実webp: VP8チャンクのみでEXIFチャンクを持たない
        let buf = std::fs::read("test/IMG_20260626_134522.jpg.webp").unwrap();
        assert_eq!(webp_exif_orientation(&buf), image::metadata::Orientation::NoTransforms);
    }

    #[test]
    fn decode_image_bytes_applies_jpeg_exif_orientation() {
        // 4x2の非正方形画像で回転による寸法入れ替えを検出できるようにする
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 2, image::Rgb([200, 10, 10])));
        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut jpeg_buf, image::ImageFormat::Jpeg).unwrap();
        let jpeg_bytes = jpeg_buf.into_inner();

        // SOI(FFD8)直後にExif Orientation=6(時計回り90度)のAPP1セグメントを挿入する
        let mut with_exif = Vec::new();
        with_exif.extend_from_slice(&jpeg_bytes[0..2]);
        with_exif.extend_from_slice(&build_exif_orientation_app1(6));
        with_exif.extend_from_slice(&jpeg_bytes[2..]);

        let decoded = decode_image_bytes(&with_exif).expect("exif付きjpegはデコードできるはず");
        assert_eq!((decoded.width(), decoded.height()), (2, 4), "90度回転で幅高さが入れ替わるはず");
    }
}
