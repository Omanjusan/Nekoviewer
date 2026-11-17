//! ビューアー静止画デコードの実測ハーネス（開発専用・#[ignore]）。
//!
//! 実行: `cargo test --profile fastrel decode_bench -- --ignored --nocapture`
//! 各段（EXIF向き検出・デコード・縮小・RGBA化・egui変換）を分けて中央値(ms)を出す。
//! 実写JPEGは /usr/share/backgrounds（3840x2560 プログレッシブ）を使い、
//! そこから 非プログレッシブJPEG・PNG・WebP を作って比べる。

use std::hint::black_box;
use std::time::Instant;

const RUNS: usize = 7;

fn median_ms<T>(mut f: impl FnMut() -> T) -> f64 {
    let mut v: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            black_box(f());
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[RUNS / 2]
}

fn encode_jpeg_baseline(img: &image::DynamicImage, q: u8) -> Vec<u8> {
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q)
        .encode_image(&rgb)
        .unwrap();
    out
}

fn encode_png(img: &image::DynamicImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn encode_webp(img: &image::DynamicImage, q: f32) -> Vec<u8> {
    let rgba = img.to_rgba8();
    webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height()).encode(q).to_vec()
}

/// libwebp の縮小デコード（use_scaling）。縮小デコードの見込みを見るための参考値。
fn webp_scaled_decode(buf: &[u8], nw: i32, nh: i32) -> Option<usize> {
    use libwebp_sys::*;
    unsafe {
        let mut cfg: WebPDecoderConfig = std::mem::zeroed();
        if !WebPInitDecoderConfig(&mut cfg) {
            return None;
        }
        cfg.options.use_scaling = 1;
        cfg.options.scaled_width = nw;
        cfg.options.scaled_height = nh;
        cfg.output.colorspace = WEBP_CSP_MODE::MODE_RGBA;
        let st = WebPDecode(buf.as_ptr(), buf.len(), &mut cfg);
        let n = cfg.output.width as usize * cfg.output.height as usize * 4;
        WebPFreeDecBuffer(&mut cfg.output);
        (st == VP8StatusCode::VP8_STATUS_OK).then_some(n)
    }
}

fn bench_one(label: &str, buf: &[u8], is_webp: bool) {
    use crate::cache::resize_for_display;
    use image::imageops::FilterType;

    let kb = buf.len() / 1024;
    let exif = median_ms(|| crate::fs::archive::decode::native_exif_orientation(buf));
    let total = median_ms(|| crate::fs::archive::decode_image_bytes(buf, true).unwrap());
    let img = crate::fs::archive::decode_image_bytes(buf, true).unwrap();
    let (w, h) = (img.width(), img.height());
    println!("\n== {label}  {w}x{h}  {kb}KB  color={:?}", img.color());
    println!("  EXIF向き検出(JPEG/PNG等のみ有意)  {exif:7.2} ms");
    println!("  decode_image_bytes 合計        {total:7.2} ms");

    for &(tw, th) in &[(1920u32, 1080u32), (2560, 1440), (3840, 2160)] {
        for (fname, f) in [("CatmullRom", FilterType::CatmullRom), ("Lanczos3", FilterType::Lanczos3)] {
            // clone は計測に入れない: 先に作ってから move する
            let mut pool: Vec<Option<image::DynamicImage>> = (0..RUNS).map(|_| Some(img.clone())).collect();
            let mut i = 0;
            let ms = median_ms(|| {
                let im = pool[i].take().unwrap();
                i += 1;
                resize_for_display(im, f, Some((tw, th)))
            });
            let (nw, nh) = crate::anim::fit_within(w, h, tw, th);
            println!("  縮小 →{nw}x{nh} ({tw}x{th}枠) {fname:<10} {ms:7.2} ms");
        }
    }

    // 縮小なし経路（拡大しない・原寸以下）。to_rgba8 の参照コピー vs into_rgba8
    let mut pool: Vec<Option<image::DynamicImage>> = (0..RUNS).map(|_| Some(img.clone())).collect();
    let mut i = 0;
    let no_resize = median_ms(|| {
        let im = pool[i].take().unwrap();
        i += 1;
        resize_for_display(im, FilterType::CatmullRom, Some((u32::MAX / 2, u32::MAX / 2)))
    });
    let mut pool: Vec<Option<image::DynamicImage>> = (0..RUNS).map(|_| Some(img.clone())).collect();
    let mut i = 0;
    let into_ms = median_ms(|| {
        let im = pool[i].take().unwrap();
        i += 1;
        im.into_rgba8()
    });
    println!("  縮小なし resize_for_display(to_rgba8)  {no_resize:7.2} ms / into_rgba8 {into_ms:7.2} ms");

    // egui変換（UIスレッドで毎ページ行う）: 1920x1080 相当の出力で比較
    let out = resize_for_display(img.clone(), FilterType::CatmullRom, Some((1920, 1080)));
    let (ow, oh) = (out.width() as usize, out.height() as usize);
    let un = median_ms(|| egui::ColorImage::from_rgba_unmultiplied([ow, oh], out.as_raw()));
    let pre = median_ms(|| egui::ColorImage::from_rgba_premultiplied([ow, oh], out.as_raw()));
    println!("  egui ColorImage {ow}x{oh}: unmultiplied {un:6.2} ms / premultiplied {pre:6.2} ms");

    if is_webp {
        let (nw, nh) = crate::anim::fit_within(w, h, 1920, 1080);
        let sc = median_ms(|| webp_scaled_decode(buf, nw as i32, nh as i32).unwrap());
        println!("  [参考] libwebp 縮小デコード→{nw}x{nh}（デコード+縮小を一括） {sc:7.2} ms  （比較対象: 合計+縮小CatmullRom 1920枠）");
    }
}

#[test]
#[ignore]
fn decode_bench() {
    let dir = std::path::Path::new("/usr/share/backgrounds");
    let src = std::fs::read(dir.join("Beach-by-Troy-T.jpg")).expect("実写JPEGが無い");
    println!("RUNS={RUNS}  threads={}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0));

    bench_one("実写JPEG(プログレッシブ)", &src, false);

    let img = image::load_from_memory(&src).unwrap();
    bench_one("JPEG baseline q90 3840x2560", &encode_jpeg_baseline(&img, 90), false);
    // 縦長ページ相当（2000x3000）
    let page = img.resize_to_fill(2000, 3000, image::imageops::FilterType::Triangle);
    bench_one("JPEG baseline q85 2000x3000(縦長ページ)", &encode_jpeg_baseline(&page, 85), false);
    let gray = image::DynamicImage::ImageLuma8(page.to_luma8());
    bench_one("JPEG baseline q85 2000x3000 グレー", &encode_jpeg_baseline(&gray, 85), false);

    bench_one("PNG RGB 3840x2560", &encode_png(&img), false);
    bench_one("PNG RGBA 3840x2560", &encode_png(&image::DynamicImage::ImageRgba8(img.to_rgba8())), false);
    bench_one("PNG 2000x3000 グレー", &encode_png(&gray), false);

    bench_one("WebP lossy q80 3840x2560", &encode_webp(&img, 80.0), true);
    if let Ok(b) = std::fs::read("test/IMG_20260626_134522.jpg.webp") {
        bench_one("WebP 実ファイル(test/)", &b, true);
    }
}
