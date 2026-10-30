//! サムネ比較用の知覚ハッシュ（dHash 64bit）。「似ている画像」の「？」マーカーの判定に使う。
//!
//! サムネ（長辺固定のJPEG）を 9×8 のグレーに縮め、隣り合うセルの明暗の大小を 64 ビットにする。
//! 元ファイルのサイズや形式は見ない。再圧縮・リサイズ・軽い画質差には強く、構図が違えば距離が大きくなる。
//! 色は見ない（色違いの別物は拾う。必要になったら別の特徴量を足す。保存側は `HASH_VERSION` で世代を分ける）。
//! 類似度(%)は 1 − ハミング距離/64。UIのしきい値（70〜100%）は `max_distance_for_pct` で距離に直す。

/// ハッシュの世代。計算方法を変えたら上げる（保存済みの値は世代が違えば捨てて作り直す）。
pub const HASH_VERSION: u8 = 1;

/// ハッシュのビット数（= 距離の最大値）。
pub const HASH_BITS: u32 = 64;

/// 縮小後の格子。横は比較用に1つ多い。
const GRID_W: usize = 9;
const GRID_H: usize = 8;

/// 全セルの明るさの幅がこれ未満の画像は「のっぺり」とみなし、ハッシュを作らない
/// （真っ白と真っ黒がどちらも全ビット0で一致してしまうのを避ける）。
const FLAT_RANGE: u8 = 3;

/// 8bitグレー画像（行優先、`width * height` バイト）から dHash を求める。
/// 画像が小さすぎる・のっぺりしているときは None。
pub fn dhash_from_gray(width: usize, height: usize, gray: &[u8]) -> Option<u64> {
    if width < GRID_W || height < GRID_H || gray.len() < width * height {
        return None;
    }
    let cells = downsample(width, height, gray);
    let (lo, hi) = cells.iter().flatten().fold((u8::MAX, u8::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if hi - lo < FLAT_RANGE {
        return None;
    }
    let mut bits = 0_u64;
    for (y, row) in cells.iter().enumerate() {
        for x in 0..GRID_W - 1 {
            if row[x] > row[x + 1] {
                bits |= 1 << (y * (GRID_W - 1) + x);
            }
        }
    }
    Some(bits)
}

/// サムネのJPEG（などの画像バイト列）から dHash を求める。デコードできない・のっぺりしていれば None。
pub fn dhash_from_image_bytes(bytes: &[u8]) -> Option<u64> {
    let gray = image::load_from_memory(bytes).ok()?.into_luma8();
    dhash_from_gray(gray.width() as usize, gray.height() as usize, gray.as_raw())
}

/// 各セルが覆う範囲の平均（面積平均）で 9×8 に縮める。
fn downsample(width: usize, height: usize, gray: &[u8]) -> [[u8; GRID_W]; GRID_H] {
    let mut out = [[0_u8; GRID_W]; GRID_H];
    for (cy, row) in out.iter_mut().enumerate() {
        let (y0, y1) = span(cy, GRID_H, height);
        for (cx, cell) in row.iter_mut().enumerate() {
            let (x0, x1) = span(cx, GRID_W, width);
            let mut sum = 0_u64;
            for y in y0..y1 {
                sum += gray[y * width + x0..y * width + x1].iter().map(|&v| u64::from(v)).sum::<u64>();
            }
            *cell = (sum / ((y1 - y0) * (x1 - x0)) as u64) as u8;
        }
    }
    out
}

/// `n` 分割したうちの `i` 番目が覆う `[開始, 終了)`。最低1画素は含める。
fn span(i: usize, n: usize, len: usize) -> (usize, usize) {
    let start = i * len / n;
    let end = ((i + 1) * len / n).max(start + 1);
    (start, end)
}

/// 2つのハッシュのハミング距離（0〜64）。
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// 距離から類似度(%)。0 で 100、64 で 0（四捨五入）。
pub fn similarity_pct(distance: u32) -> u8 {
    let d = distance.min(HASH_BITS);
    (((HASH_BITS - d) * 100 + HASH_BITS / 2) / HASH_BITS) as u8
}

/// しきい値(%)を満たす最大の距離。例: 90% → 6、95% → 3、100% → 0。
/// `similarity_pct(d) >= pct` となる最大の d（丸めと整合させるため、類似度側で判定して求める）。
pub fn max_distance_for_pct(pct: u8) -> u32 {
    (0..=HASH_BITS).rev().find(|&d| similarity_pct(d) >= pct).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 256;
    const H: usize = 180;

    /// 16×16 のブロックごとにランダムな明るさを置いた画像（写真のような低周波の構図の代用）。
    fn blocks(seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let (bw, bh) = (W / 16, H.div_ceil(16));
        let levels: Vec<u8> = (0..bw * bh).map(|_| (next() % 256) as u8).collect();
        (0..W * H).map(|i| levels[(i / W / 16) * bw + (i % W) / 16]).collect()
    }

    fn encode_jpeg(gray: &[u8], w: usize, h: usize, quality: u8) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode(gray, w as u32, h as u32, image::ExtendedColorType::L8)
            .unwrap();
        out
    }

    fn resized(gray: &[u8], w: usize, h: usize, nw: usize, nh: usize) -> Vec<u8> {
        let img = image::GrayImage::from_raw(w as u32, h as u32, gray.to_vec()).unwrap();
        image::imageops::resize(&img, nw as u32, nh as u32, image::imageops::FilterType::Triangle).into_raw()
    }

    #[test]
    fn same_image_has_distance_zero() {
        let a = dhash_from_gray(W, H, &blocks(1)).unwrap();
        assert_eq!(distance(a, a), 0);
        assert_eq!(similarity_pct(0), 100);
    }

    #[test]
    fn recompression_and_resize_stay_close() {
        let base = blocks(2);
        let h0 = dhash_from_gray(W, H, &base).unwrap();
        for q in [90, 60, 30] {
            let jpeg = encode_jpeg(&base, W, H, q);
            let d = distance(h0, dhash_from_image_bytes(&jpeg).unwrap());
            assert!(d <= 6, "jpeg q{q}: distance {d}");
        }
        let small = resized(&base, W, H, 160, 112);
        let d = distance(h0, dhash_from_gray(160, 112, &small).unwrap());
        assert!(d <= 8, "resized: distance {d}");
    }

    #[test]
    fn unrelated_images_are_far_apart() {
        let h0 = dhash_from_gray(W, H, &blocks(3)).unwrap();
        let far = (10..30).filter(|&s| distance(h0, dhash_from_gray(W, H, &blocks(s)).unwrap()) > 16).count();
        assert!(far >= 18, "unrelated images too close: only {far}/20 were far");
    }

    #[test]
    fn inverted_image_is_maximally_different() {
        let base = blocks(4);
        let inv: Vec<u8> = base.iter().map(|v| 255 - v).collect();
        let d = distance(dhash_from_gray(W, H, &base).unwrap(), dhash_from_gray(W, H, &inv).unwrap());
        assert!(d > 40, "inverted distance {d}");
    }

    #[test]
    fn flat_or_tiny_images_have_no_hash() {
        assert_eq!(dhash_from_gray(W, H, &vec![255; W * H]), None);
        assert_eq!(dhash_from_gray(W, H, &vec![0; W * H]), None);
        assert_eq!(dhash_from_gray(4, 4, &[0; 16]), None);
        assert_eq!(dhash_from_gray(W, H, &[0; 10]), None, "buffer shorter than width*height");
        assert_eq!(dhash_from_image_bytes(b"not an image"), None);
    }

    #[test]
    fn pct_and_distance_convert_consistently() {
        assert_eq!(similarity_pct(64), 0);
        assert_eq!(similarity_pct(32), 50);
        let expected = [(100, 0), (95, 3), (90, 6), (85, 9), (80, 13), (75, 16), (70, 19)];
        for (pct, d) in expected {
            assert_eq!(max_distance_for_pct(pct), d, "pct {pct}");
            assert!(similarity_pct(d) >= pct);
            assert!(similarity_pct(d + 1) < pct, "pct {pct}: distance {} should fall below", d + 1);
        }
    }
}
