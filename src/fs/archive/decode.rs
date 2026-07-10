//! フォーマット非依存の画像バイト処理。
//! シグネチャ判定・静止画/アニメーションのデコードとデコード後サイズ見積もりを担う。

/// バイト列から静止画をデコードする（内部ラッパ）。サムネイル生成専用の経路のため、
/// 項目(D)のExif Orientation ON/OFF設定に関わらず常時Orientationを適用する
/// （サムネイルはビューアーの設定と独立、常にEXIF自動回転ON固定）。
pub(crate) fn decode_image(buf: &[u8]) -> Option<image::DynamicImage> {
    decode_image_bytes(buf, true)
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

/// 項目(D)のON→OFF切替時、手動回転(B)の角度補正用にOrientationだけを検出する
/// （デコードはしない、軽量パス）。AVIFは向き判定にフルデコード相当のFFI呼び出しが
/// 必要でコストに見合わないため対象外とし、常にNoTransforms扱いとする（既知の制限、
/// 該当ページのみON→OFF切替時に見た目が一瞬ズレることを許容する）。
pub(crate) fn detect_orientation_for_toggle(buf: &[u8]) -> image::metadata::Orientation {
    if has_webp_signature(buf) {
        webp_exif_orientation(buf)
    } else if has_avif_signature(buf) {
        image::metadata::Orientation::NoTransforms
    } else {
        native_exif_orientation(buf)
    }
}

/// バイト列から静止画をデコードする（外部から呼び出し可能）。
/// 拡張子ではなく先頭バイトのシグネチャで実フォーマットを判定するため、
/// 拡張子と中身が食い違うファイルでも正しいデコーダへ振り分けられる。
/// `exif_enabled`: 項目(D)。falseならOrientation検出自体は行うが適用をスキップする
/// （誤ったOrientationタグが埋め込まれたアーカイブ向けの逃げ道）。
pub fn decode_image_bytes(buf: &[u8], exif_enabled: bool) -> Option<image::DynamicImage> {
    if has_webp_signature(buf) {
        // このパスは常に単一の静的画像を返す（アニメ再生自体は別経路のRingAnimationが担う）
        // ため、どちらの分岐で取れたフレームに対してもExif Orientationを適用してよい。
        let orientation = if exif_enabled { webp_exif_orientation(buf) } else { image::metadata::Orientation::NoTransforms };
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
        decode_avif(buf, exif_enabled)
    } else {
        decode_native_with_orientation(buf, exif_enabled)
    }
}

/// image crateネイティブ対応フォーマット（JPEG/PNG/TIFF/BMP/GIF等）のExif Orientationタグを
/// 検出する（デコードはしない）。ピクセルへの適用は呼び出し側の責務。
/// 項目(D)のON/OFF切替時、手動回転(B)の角度補正のためにOrientationだけ知りたい場面
/// （`crate::rotation::orientation_rotation_degrees`と組み合わせる）でも使う。
pub(crate) fn native_exif_orientation(buf: &[u8]) -> image::metadata::Orientation {
    use image::ImageDecoder;

    (|| -> Option<image::metadata::Orientation> {
        let reader = image::ImageReader::new(std::io::Cursor::new(buf))
            .with_guessed_format()
            .ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        let exif_chunk: Option<Vec<u8>> = decoder.exif_metadata().ok().flatten();
        exif_chunk.and_then(|chunk| image::metadata::Orientation::from_exif_chunk(&chunk))
    })()
    .unwrap_or(image::metadata::Orientation::NoTransforms)
}

/// image crateネイティブ対応フォーマット（JPEG/PNG/TIFF/BMP/GIF等）用。
/// Exif Orientationタグを検出し、デコード直後に画素へ適用する。以降の呼び出し元
/// （サムネ生成・ビューアーのページ表示、いずれもここを通る）は回転後の寸法を
/// そのまま使えばよく、個別の回転対応は不要になる。
/// WebP/AVIFは別デコーダ（webp crate / libavif）経由のため対象外（未対応、既知の制限）。
fn decode_native_with_orientation(buf: &[u8], exif_enabled: bool) -> Option<image::DynamicImage> {
    let orientation = if exif_enabled { native_exif_orientation(buf) } else { image::metadata::Orientation::NoTransforms };
    let reader = image::ImageReader::new(std::io::Cursor::new(buf))
        .with_guessed_format()
        .ok()?;
    let decoder = reader.into_decoder().ok()?;
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img)
}

/// AVIFコンテナ自身が持つ`irot`(90度単位回転)/`imir`(ミラー)変換と、埋め込みExif Orientation
/// タグの2系統から最終的な向きを1つの`Orientation`に統合する。両方あった場合はコンテナ側の
/// `irot`/`imir`を優先しExifタグは無視する（仕様上Exifは冗長・レガシー扱いで、実際のエンコーダの
/// 多くはirot/imirの方を使うため）。
///
/// `irot`は反時計回りangle*90度、`imir`はaxis=0で上下反転・axis=1で左右反転、この順
/// （回転→ミラー）で適用される。image crateの`Orientation`は「回転→左右反転」の合成でしか
/// 8通りを表現できない（上下反転単体はFlipVerticalとして持つが、回転+上下反転の組は無い）ため、
/// axis=0(上下反転)のケースは「180度回転を上乗せしてから左右反転」に正規化してから当てはめる
/// （180度回転してから左右反転 = 上下反転、という恒等式を利用）。生成される`Orientation`を返す
/// 純粋関数にしておくことで、静止画パス(`decode_avif`)とアニメーション単一フレーム確定時
/// （`cache.rs::RingAnimation::from_source`）の両方から共有できる。
pub(crate) fn avif_container_orientation(transform_flags: u32, irot_angle: u8, imir_axis: u8, exif: Option<&[u8]>) -> image::metadata::Orientation {
    use image::metadata::Orientation;
    use libavif_sys::{AVIF_TRANSFORM_IMIR, AVIF_TRANSFORM_IROT};

    let has_irot = transform_flags & AVIF_TRANSFORM_IROT != 0;
    let has_imir = transform_flags & AVIF_TRANSFORM_IMIR != 0;

    if has_irot || has_imir {
        // 反時計回りangle*90度 → 時計回りquarter-turn数へ変換
        let k = if has_irot { (4 - (irot_angle as u32 % 4)) % 4 } else { 0 };
        if has_imir {
            let k = if imir_axis == 0 { (k + 2) % 4 } else { k };
            return match k {
                0 => Orientation::FlipHorizontal,
                1 => Orientation::Rotate90FlipH,
                2 => Orientation::FlipVertical,
                _ => Orientation::Rotate270FlipH,
            };
        }
        return match k {
            0 => Orientation::NoTransforms,
            1 => Orientation::Rotate90,
            2 => Orientation::Rotate180,
            _ => Orientation::Rotate270,
        };
    }

    exif.and_then(Orientation::from_exif_chunk).unwrap_or(Orientation::NoTransforms)
}

/// `avifImage`の生ポインタから向き判定に必要な値を読み出し、`avif_container_orientation`に渡す。
/// Exifバイト列は`avifImage`（＝デコーダ）が生きている間しか有効でないため、ここで判定まで
/// 完結させる（呼び出し元へ生ポインタや借用を持ち出させない）。
///
/// # Safety
/// `avif_image`は`avifDecoderNextImage`成功後に得た有効な`avifImage`ポインタでなければならない。
pub(crate) unsafe fn avif_orientation_from_image(avif_image: *const libavif_sys::avifImage) -> image::metadata::Orientation {
    let (transform_flags, irot_angle, imir_axis, exif) =
        unsafe { ((*avif_image).transformFlags, (*avif_image).irot.angle, (*avif_image).imir.axis, (*avif_image).exif) };
    let exif_bytes = if !exif.data.is_null() && exif.size > 0 {
        Some(unsafe { std::slice::from_raw_parts(exif.data, exif.size) })
    } else {
        None
    };
    avif_container_orientation(transform_flags, irot_angle, imir_axis, exif_bytes)
}

/// `libavif`クレート(安全ラッパー)にはExif/irot/imirへのアクセスが無いため、生FFI
/// (`libavif-sys`)で直接デコードする。`anim.rs`の`AvifSeqState`と同系統のFFI呼び出しパターン。
fn decode_avif(buf: &[u8], exif_enabled: bool) -> Option<image::DynamicImage> {
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

        let mut img = image::RgbaImage::from_raw(w, h, pixels?).map(image::DynamicImage::ImageRgba8)?;
        let orientation = if exif_enabled { avif_orientation_from_image(avif_image) } else { image::metadata::Orientation::NoTransforms };
        img.apply_orientation(orientation);
        Some(img)
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

    /// `avif_container_orientation`が「irot→imir逐次適用」と等価であることを、
    /// image crateの生の回転・反転関数を使った素朴なシミュレーションと突き合わせて検証する。
    /// 特にimir.axis==0(上下反転)を「180度回転+左右反転」に正規化する変換が正しいかどうかが要。
    #[test]
    fn avif_container_orientation_matches_sequential_irot_then_imir() {
        use libavif_sys::{AVIF_TRANSFORM_IMIR, AVIF_TRANSFORM_IROT};

        // 非対称な3x2画像（回転・反転すると別物になる）
        let base = image::RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8, y as u8, 0, 255]));

        for irot_angle in 0u8..4 {
            for imir_axis in [None, Some(0u8), Some(1u8)] {
                let mut transform_flags = AVIF_TRANSFORM_IROT;
                if imir_axis.is_some() {
                    transform_flags |= AVIF_TRANSFORM_IMIR;
                }

                // 素朴なシミュレーション: irotを反時計回りangle*90度→rotate90/270へ変換して適用、
                // その後imirをaxis通りに適用する。
                let mut expected = image::DynamicImage::ImageRgba8(base.clone());
                expected = match irot_angle % 4 {
                    1 => expected.rotate270(),
                    2 => expected.rotate180(),
                    3 => expected.rotate90(),
                    _ => expected,
                };
                if let Some(axis) = imir_axis {
                    expected = if axis == 0 { expected.flipv() } else { expected.fliph() };
                }

                let orientation = avif_container_orientation(transform_flags, irot_angle, imir_axis.unwrap_or(0), None);
                let mut actual = image::DynamicImage::ImageRgba8(base.clone());
                actual.apply_orientation(orientation);

                assert_eq!(
                    actual.to_rgba8(), expected.to_rgba8(),
                    "irot_angle={irot_angle}, imir_axis={imir_axis:?} で不一致"
                );
            }
        }
    }

    #[test]
    fn decode_image_bytes_reads_tiff() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255])));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Tiff).unwrap();
        let bytes = buf.into_inner();

        let decoded = decode_image_bytes(&bytes, true).expect("tiff should decode");
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

        let decoded = decode_image_bytes(&with_exif, true).expect("exif付きjpegはデコードできるはず");
        assert_eq!((decoded.width(), decoded.height()), (2, 4), "90度回転で幅高さが入れ替わるはず");
    }

    /// 項目(D)ON→OFF切替の角度補正用軽量パスが、JPEG/WebPではデコードなしで
    /// Exif Orientationを正しく検出できることを確認する。
    #[test]
    fn detect_orientation_for_toggle_reads_jpeg_exif() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 2, image::Rgb([200, 10, 10])));
        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut jpeg_buf, image::ImageFormat::Jpeg).unwrap();
        let jpeg_bytes = jpeg_buf.into_inner();

        let mut with_exif = Vec::new();
        with_exif.extend_from_slice(&jpeg_bytes[0..2]);
        with_exif.extend_from_slice(&build_exif_orientation_app1(6)); // 6 = 時計回り90度
        with_exif.extend_from_slice(&jpeg_bytes[2..]);

        assert_eq!(detect_orientation_for_toggle(&with_exif), image::metadata::Orientation::Rotate90);
    }

    #[test]
    fn detect_orientation_for_toggle_reads_webp_exif() {
        let buf = build_minimal_webp_with_exif(6);
        assert_eq!(detect_orientation_for_toggle(&buf), image::metadata::Orientation::Rotate90);
    }

    #[test]
    fn detect_orientation_for_toggle_defaults_without_exif() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255])));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Tiff).unwrap();
        assert_eq!(detect_orientation_for_toggle(&buf.into_inner()), image::metadata::Orientation::NoTransforms);
    }

    /// 生FFI(libavif-sys)で実際のAVIFバイト列をエンコードする。RIFF/WebPと違いISOBMFF boxの
    /// 自前組み立ては現実的でないため、libavifのエンコーダを直接叩く（フェーズ3・B案）。
    /// irot/imir・Exifは`libavif`の安全ラッパーには無いフィールドで、デコード側(`decode_avif`)
    /// と同じ生FFI操作でしかセットできない。可逆設定でエンコードし、幅高さの入れ替わりで
    /// 回転が実際に反映されたことを確認する（AV1は非可逆コーデックのためピクセル完全一致は見ない）。
    fn build_test_avif(w: u32, h: u32, rgba: &[u8], irot_angle: Option<u8>, imir_axis: Option<u8>, exif: Option<&[u8]>) -> Vec<u8> {
        use libavif_sys::*;
        unsafe {
            let image = avifImageCreate(w, h, 8, AVIF_PIXEL_FORMAT_YUV444);
            assert!(!image.is_null());

            if let Some(angle) = irot_angle {
                (*image).transformFlags |= AVIF_TRANSFORM_IROT;
                (*image).irot.angle = angle;
            }
            if let Some(axis) = imir_axis {
                (*image).transformFlags |= AVIF_TRANSFORM_IMIR;
                (*image).imir.axis = axis;
            }

            let mut rgb: avifRGBImage = std::mem::zeroed();
            avifRGBImageSetDefaults(&mut rgb, image);
            rgb.format = AVIF_RGB_FORMAT_RGBA;
            rgb.depth = 8;
            assert_eq!(avifRGBImageAllocatePixels(&mut rgb), AVIF_RESULT_OK);
            for row in 0..h as usize {
                let src = &rgba[row * (w as usize) * 4..(row + 1) * (w as usize) * 4];
                let dst = std::slice::from_raw_parts_mut(rgb.pixels.add(row * rgb.rowBytes as usize), (w as usize) * 4);
                dst.copy_from_slice(src);
            }
            assert_eq!(avifImageRGBToYUV(image, &rgb), AVIF_RESULT_OK);
            avifRGBImageFreePixels(&mut rgb);

            // Exifは`avifImageSetMetadataExif`がirot/imirも自動解釈して上書きするため、
            // irot/imirを明示指定するテストケースとは排他で使う。
            if let Some(exif_bytes) = exif {
                assert_eq!(avifImageSetMetadataExif(image, exif_bytes.as_ptr(), exif_bytes.len()), AVIF_RESULT_OK);
            }

            let encoder = avifEncoderCreate();
            assert!(!encoder.is_null());
            (*encoder).speed = 10;
            (*encoder).quality = AVIF_QUALITY_LOSSLESS as i32;
            (*encoder).minQuantizer = AVIF_QUANTIZER_LOSSLESS as i32;
            (*encoder).maxQuantizer = AVIF_QUANTIZER_LOSSLESS as i32;

            assert_eq!(
                avifEncoderAddImage(encoder, image, 1, AVIF_ADD_IMAGE_FLAG_SINGLE),
                AVIF_RESULT_OK
            );
            let mut output: avifRWData = std::mem::zeroed();
            assert_eq!(avifEncoderFinish(encoder, &mut output), AVIF_RESULT_OK);
            let bytes = std::slice::from_raw_parts(output.data, output.size).to_vec();

            avifRWDataFree(&mut output);
            avifEncoderDestroy(encoder);
            avifImageDestroy(image);
            bytes
        }
    }

    /// 4x2の非対称RGBA画像（座標(x,y)をそのままR,Gに埋め込む）を作る。
    fn asymmetric_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[x as u8 * 40, y as u8 * 40, 0, 255]);
            }
        }
        out
    }

    #[test]
    fn decode_avif_applies_irot_orientation() {
        let rgba = asymmetric_rgba(4, 2);
        // irot.angle=3 (反時計回り270度=時計回り90度) → 幅高さが入れ替わるはず
        let buf = build_test_avif(4, 2, &rgba, Some(3), None, None);

        let decoded = decode_image_bytes(&buf, true).expect("自作AVIF(irot)はデコードできるはず");
        assert_eq!((decoded.width(), decoded.height()), (2, 4), "irot反映で幅高さが入れ替わるはず");
    }

    #[test]
    fn decode_avif_applies_imir_orientation() {
        let rgba = asymmetric_rgba(4, 2);
        // imir単体（回転なし）は寸法を変えないため、ピクセル内容で左右反転を検証する
        let buf = build_test_avif(4, 2, &rgba, None, Some(1), None);

        let decoded = decode_image_bytes(&buf, true).expect("自作AVIF(imir)はデコードできるはず");
        let decoded = decoded.to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (4, 2));
        // axis=1(左右反転)適用後は元画像の右端列(x=3)が復号後の左端(x=0)に来るはず
        // （YUV往復による丸め誤差を避けるため、位置を表すR/Gチャンネルのみ見る）
        let px = decoded.get_pixel(0, 0).0;
        assert_eq!([px[0], px[1]], [3 * 40, 0]);
    }

    #[test]
    fn decode_avif_falls_back_to_exif_orientation_without_irot_imir() {
        let rgba = asymmetric_rgba(4, 2);
        let exif_tiff = build_minimal_exif_tiff(6); // 6 = 時計回り90度
        let buf = build_test_avif(4, 2, &rgba, None, None, Some(&exif_tiff));

        let decoded = decode_image_bytes(&buf, true).expect("自作AVIF(exif)はデコードできるはず");
        assert_eq!((decoded.width(), decoded.height()), (2, 4), "Exif Orientationフォールバックで幅高さが入れ替わるはず");
    }

    /// 項目(D)ON→OFF切替の角度補正: AVIFはirotを持っていても軽量パスでは常にNoTransforms扱い
    /// にする既知の制限（フルデコード相当のFFI呼び出しコストを避けるための割り切り）。
    #[test]
    fn detect_orientation_for_toggle_ignores_avif_irot() {
        let rgba = asymmetric_rgba(4, 2);
        let buf = build_test_avif(4, 2, &rgba, Some(3), None, None);
        assert_eq!(detect_orientation_for_toggle(&buf), image::metadata::Orientation::NoTransforms);
    }
}
