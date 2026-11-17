//! アーカイブ内の `.txt` を、画像ページとして扱うための組版・描画。
//!
//! 流れ:
//! - 読み出し側（zip の `load_bytes_from_archive`）が、TXT の本文を上限サイズまで読み、目印・ページ番号・組版条件を付けた
//!   「TXTペイロード」に包んで返す。以降はキャッシュ・サムネ・見積もりまで、ふつうの画像のバイト列と同じ経路を通る
//! - `decode_image_bytes` が先頭の目印を見て、ここの `render` で白地に黒文字の画像にする
//!
//! 組版は等幅グリッド（全角=2マス・半角=1マス）。ページ数が本文だけから決まるので、
//! 一覧取得の時点で「ページ分割」の正確なページ数が出せる。横書きのみ（縦書き・禁則・文字の結合形は非対応）。
//!
//! 安全方針: 読み込みサイズ・文字数・ページ数・画像の高さに上限を持つ。ネットワークは使わない。
//! 本文は `meta::decode_text_bytes` で文字コード判定と安全化（制御文字・BiDi・過剰な結合文字の除去）を済ませてから使う。

use std::sync::{OnceLock, RwLock};

use ab_glyph::{Font, FontArc, GlyphId, PxScale};
use image::{DynamicImage, RgbaImage};

use super::meta::{decode_text_bytes, is_combining};

/// ペイロードの目印（8バイト）。
const MAGIC: &[u8; 8] = b"NEKOTXT1";
/// 目印(8) + ページ(4) + 高さ上限(4) + フラグ(1)。
const HEADER_LEN: usize = 17;
const FLAG_SPLIT: u8 = 1;
const FLAG_SIZE_TRUNCATED: u8 = 2;

/// 分割モードで、仮想エントリ名に付ける区切り（`<元の名前>\u{1f}<ページ番号>`）。
pub(crate) const PAGE_SEP: char = '\u{1f}';

/// ページの幅（px）。
pub const PAGE_W: u32 = 1200;
const MARGIN: u32 = 48;
const FONT_PX: f32 = 30.0;
/// 半角1マスの幅。Hack（等幅）の送り幅 0.6em に合わせる。
const UNIT: f32 = FONT_PX * 0.6;
const LINE_H: u32 = 45;
/// 1ページの横のマス数。
const COLS: usize = ((PAGE_W - MARGIN * 2) as f32 / UNIT) as usize;

/// 画像の高さの上限（GPUテクスチャの1辺上限に合わせる）と、設定で選べる下限。
pub const HEIGHT_LIMIT_MAX: u32 = 8192;
pub const HEIGHT_LIMIT_MIN: u32 = 600;
/// 読み込みサイズの上限と下限（バイト）。
pub const READ_LIMIT_MAX: usize = 4 * 1024 * 1024;
pub const READ_LIMIT_MIN: usize = 64 * 1024;
/// 1つのTXTを分割できる最大ページ数。
const MAX_PAGES: usize = 500;
/// 組版する最大文字数（読み込み上限の範囲でも、改行だらけの本文などで行数が膨れないように）。
const MAX_CHARS: usize = 600_000;

/// TXTページの設定。実行中は全体で1つを共有する（`set_options` / `options`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPageOptions {
    /// TXT を画像ページとして開くか。
    pub enabled: bool,
    /// 画像の高さの上限（px）。
    pub height_limit: u32,
    /// true=上限を超えたらページ分割、false=切り詰め。
    pub split: bool,
    /// 読み込むサイズの上限（バイト）。超えた分は読まない。
    pub read_limit: usize,
    /// ページ数（リストの「ページ数」列）にTXTページを含めるか。
    pub count_in_pages: bool,
}

impl Default for TextPageOptions {
    fn default() -> Self {
        Self { enabled: true, height_limit: 4000, split: false, read_limit: 1024 * 1024, count_in_pages: false }
    }
}

impl TextPageOptions {
    /// state ファイル用の文字列（有効,高さ,分割,読込KB,ページ数に含める）。
    pub fn to_state_str(self) -> String {
        format!(
            "{},{},{},{},{}",
            u8::from(self.enabled),
            self.height_limit,
            u8::from(self.split),
            self.read_limit / 1024,
            u8::from(self.count_in_pages),
        )
    }

    /// `to_state_str` の逆。個数や数値が不正なら None（呼び出し側は既定値のまま）。範囲外は丸める。
    pub fn from_state_str(s: &str) -> Option<Self> {
        let p: Vec<&str> = s.trim().split(',').map(str::trim).collect();
        let [enabled, height, split, read_kb, count] = p.as_slice() else { return None };
        let flag = |v: &str| match v {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        };
        Some(
            Self {
                enabled: flag(enabled)?,
                height_limit: height.parse().ok()?,
                split: flag(split)?,
                read_limit: read_kb.parse::<usize>().ok()?.checked_mul(1024)?,
                count_in_pages: flag(count)?,
            }
            .clamped(),
        )
    }

    /// 範囲外の値を丸める。
    pub fn clamped(mut self) -> Self {
        self.height_limit = self.height_limit.clamp(HEIGHT_LIMIT_MIN, HEIGHT_LIMIT_MAX);
        self.read_limit = self.read_limit.clamp(READ_LIMIT_MIN, READ_LIMIT_MAX);
        self
    }
}

static OPTIONS: RwLock<TextPageOptions> =
    RwLock::new(TextPageOptions { enabled: true, height_limit: 4000, split: false, read_limit: 1024 * 1024, count_in_pages: false });

pub fn set_options(opts: TextPageOptions) {
    if let Ok(mut g) = OPTIONS.write() {
        *g = opts.clamped();
    }
}

pub fn options() -> TextPageOptions {
    OPTIONS.read().map(|g| *g).unwrap_or_default()
}

/// 生のエントリ名が `.txt` か（大文字小文字は区別しない）。
pub(crate) fn is_text_entry_raw(raw: &[u8]) -> bool {
    raw.len() > 4 && raw[raw.len() - 4..].eq_ignore_ascii_case(b".txt")
}

/// エントリ名（仮想ページ名を含む）がTXTページか。ページ数の集計から外す判定に使う。
pub fn is_text_entry_name(entry_name: &str) -> bool {
    let base = entry_name.split(PAGE_SEP).next().unwrap_or(entry_name);
    is_text_entry_raw(base.as_bytes())
}

/// 仮想エントリ名を（元の名前, ページ番号 0始まり）に分ける。区切りが無ければページ 0。
pub(crate) fn split_entry_name(entry_name: &str) -> (&str, u32) {
    match entry_name.split_once(PAGE_SEP) {
        Some((base, page)) => (base, page.parse().unwrap_or(0)),
        None => (entry_name, 0),
    }
}

pub(crate) fn page_entry_name(base: &str, page: usize) -> String {
    if page == 0 { base.to_owned() } else { format!("{base}{PAGE_SEP}{page}") }
}

/// 分割した2ページ目以降の表示名（`a.txt` → `a_p02.txt`）。
pub(crate) fn page_display_name(display: &str, page: usize) -> String {
    if page == 0 {
        return display.to_owned();
    }
    match display.rfind('.') {
        Some(dot) => format!("{}_p{:02}{}", &display[..dot], page + 1, &display[dot..]),
        None => format!("{display}_p{:02}", page + 1),
    }
}

// ── 組版 ──────────────────────────────────────────────────────────────────

fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// 文字が占めるマス数（結合文字は 0、全角は 2、それ以外は 1）。
fn char_units(c: char) -> usize {
    if is_combining(c) {
        0
    } else if is_wide(c as u32) {
        2
    } else {
        1
    }
}

/// 本文を `cols` マスで折り返した行にする。空行も1行として残す。`max_chars` 文字で打ち切る。
pub(crate) fn wrap_lines(text: &str, cols: usize, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut budget = max_chars;
    'outer: for logical in text.split('\n') {
        let mut cur = String::new();
        let mut units = 0usize;
        for c in logical.chars() {
            if budget == 0 {
                lines.push(std::mem::take(&mut cur));
                break 'outer;
            }
            budget -= 1;
            if c == '\t' {
                // 4マス区切りの位置まで空白で埋める。
                let pad = 4 - units % 4;
                for _ in 0..pad {
                    if units + 1 > cols {
                        lines.push(std::mem::take(&mut cur));
                        units = 0;
                    }
                    cur.push(' ');
                    units += 1;
                }
                continue;
            }
            let u = char_units(c);
            if units + u > cols && units > 0 {
                lines.push(std::mem::take(&mut cur));
                units = 0;
            }
            cur.push(c);
            units += u;
        }
        lines.push(cur);
    }
    lines
}

/// 1ページに入る行数。
fn lines_per_page(height_limit: u32) -> usize {
    (((height_limit.saturating_sub(MARGIN * 2)) / LINE_H) as usize).max(1)
}

/// 本文のバイト列を、組版済みの行にする。`size_truncated` なら末尾に省略記号の行を足す。
pub(crate) fn prepare_lines(body: &[u8], size_truncated: bool) -> Vec<String> {
    let text = decode_text_bytes(body);
    let mut lines = wrap_lines(&text, COLS, MAX_CHARS);
    // 末尾の空行（最終改行の分）は落とす。
    while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if size_truncated {
        lines.push("…".to_owned());
    }
    lines
}

/// ページ数。分割モードで行数が上限を超えた分だけ増える。切り詰めモードは常に1。
pub(crate) fn page_count_for(line_count: usize, split: bool, height_limit: u32) -> usize {
    if !split {
        return 1;
    }
    line_count.div_ceil(lines_per_page(height_limit)).clamp(1, MAX_PAGES)
}

/// 一覧取得時に使う: 本文からページ数を求める（切り詰めモードなら読まずに 1）。
pub(crate) fn count_pages(body: &[u8], size_truncated: bool, opts: &TextPageOptions) -> usize {
    if !opts.split {
        return 1;
    }
    page_count_for(prepare_lines(body, size_truncated).len(), true, opts.height_limit)
}

// ── ペイロード ────────────────────────────────────────────────────────────

/// 読み出した本文を、描画に必要な条件つきのペイロードに包む。
pub(crate) fn encode_payload(body: &[u8], page: u32, opts: &TextPageOptions, size_truncated: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER_LEN + body.len());
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&page.to_le_bytes());
    v.extend_from_slice(&opts.height_limit.to_le_bytes());
    v.push(u8::from(opts.split) * FLAG_SPLIT | u8::from(size_truncated) * FLAG_SIZE_TRUNCATED);
    v.extend_from_slice(body);
    v
}

pub fn is_payload(buf: &[u8]) -> bool {
    buf.len() >= HEADER_LEN && &buf[..8] == MAGIC
}

/// 本文を上限まで読み、ペイロードにして返す。`declared_larger` は元のサイズが上限を超えていたか。
pub(crate) fn wrap_entry(body: Vec<u8>, page: u32, declared_larger: bool) -> Vec<u8> {
    let opts = options();
    let truncated = declared_larger || body.len() >= opts.read_limit;
    encode_payload(&body, page, &opts, truncated)
}

// ── 描画 ──────────────────────────────────────────────────────────────────

struct Fonts {
    /// 半角の英数記号を優先して引くフォント（等幅の Hack）。
    ascii: Vec<FontArc>,
    /// それ以外を引くフォントの順（CJK → 簡体字 → 標準 → 絵文字）。
    other: Vec<FontArc>,
}

fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let defs = egui::FontDefinitions::default();
        let load = |name: &str| -> Option<FontArc> {
            let fd = defs.font_data.get(name)?;
            FontArc::try_from_vec(fd.font.to_vec()).ok()
        };
        let load_bytes = |bytes: Vec<u8>| FontArc::try_from_vec(bytes).ok();
        let mut other: Vec<FontArc> = Vec::new();
        if let Some(f) = crate::japanese_font_data().and_then(load_bytes) {
            other.push(f);
        }
        if let Some(f) = crate::simplified_chinese_font_data().and_then(load_bytes) {
            other.push(f);
        }
        let hack = load("Hack");
        other.extend(load("Ubuntu-Light"));
        other.extend(hack.clone());
        other.extend(load("NotoEmoji-Regular"));
        Fonts { ascii: hack.into_iter().collect(), other }
    })
}

/// 文字を持つ最初のフォント（なければ None）。
fn pick_font(c: char, chain: &[&[FontArc]]) -> Option<FontArc> {
    chain
        .iter()
        .flat_map(|fs| fs.iter())
        .find(|f| f.glyph_id(c) != GlyphId(0))
        .cloned()
}

fn px_scale(font: &FontArc) -> PxScale {
    let upem = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(FONT_PX * font.height_unscaled() / upem)
}

fn draw_glyph(img: &mut RgbaImage, font: &FontArc, c: char, x: f32, baseline: f32) {
    let scale = px_scale(font);
    let glyph = font.glyph_id(c).with_scale_and_position(scale, ab_glyph::point(x, baseline));
    let Some(outlined) = font.outline_glyph(glyph) else { return };
    let b = outlined.px_bounds();
    let (w, h) = (img.width() as i32, img.height() as i32);
    outlined.draw(|gx, gy, cov| {
        let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
        if px < 0 || py < 0 || px >= w || py >= h || cov <= 0.0 {
            return;
        }
        let p = img.get_pixel_mut(px as u32, py as u32);
        let v = (255.0 * (1.0 - cov.clamp(0.0, 1.0))) as u8;
        p.0[0] = p.0[0].min(v);
        p.0[1] = p.0[1].min(v);
        p.0[2] = p.0[2].min(v);
    });
}

/// 行を描く。`row` は画像内の何行目か。
fn draw_line(img: &mut RgbaImage, line: &str, row: usize) {
    let f = fonts();
    let baseline = (MARGIN + row as u32 * LINE_H) as f32 + LINE_H as f32 * 0.5 + FONT_PX * 0.35;
    let mut units = 0usize;
    // 結合文字は直前の文字のマスに重ねる。
    let (mut prev_x, mut prev_w) = (MARGIN as f32, UNIT);
    for c in line.chars() {
        let u = char_units(c);
        let cell_x = MARGIN as f32 + units as f32 * UNIT;
        let cell_w = (u.max(1)) as f32 * UNIT;
        let (anchor_x, anchor_w) = if u == 0 { (prev_x, prev_w) } else { (cell_x, cell_w) };
        if c != ' ' {
            let chain: [&[FontArc]; 2] = if c.is_ascii() { [&f.ascii, &f.other] } else { [&f.other, &f.ascii] };
            // 持っているフォントが無い文字は、四角（□）で代替する。それも無ければ空白。
            let picked = pick_font(c, &chain);
            let (font, ch) = match picked {
                Some(font) => (Some(font), c),
                None => (pick_font('□', &chain), '□'),
            };
            if let Some(font) = font {
                let scale = px_scale(&font);
                let adv = font.as_scaled_advance(ch, scale);
                // マスの中央に置く（はみ出す場合は左端に寄せる）。
                let x = anchor_x + ((anchor_w - adv) / 2.0).max(0.0);
                draw_glyph(img, &font, ch, x, baseline);
            }
        }
        if u > 0 {
            prev_x = cell_x;
            prev_w = cell_w;
        }
        units += u;
    }
}

/// `Font` に送り幅の取得を足す小さな拡張。
trait AdvanceExt {
    fn as_scaled_advance(&self, c: char, scale: PxScale) -> f32;
}

impl AdvanceExt for FontArc {
    fn as_scaled_advance(&self, c: char, scale: PxScale) -> f32 {
        use ab_glyph::ScaleFont;
        self.as_scaled(scale).h_advance(self.glyph_id(c))
    }
}

/// ペイロードを画像にする。ペイロードでなければ None。
pub fn render(payload: &[u8]) -> Option<DynamicImage> {
    if !is_payload(payload) {
        return None;
    }
    let page = u32::from_le_bytes(payload[8..12].try_into().ok()?) as usize;
    let height_limit = u32::from_le_bytes(payload[12..16].try_into().ok()?).clamp(HEIGHT_LIMIT_MIN, HEIGHT_LIMIT_MAX);
    let flags = payload[16];
    let split = flags & FLAG_SPLIT != 0;
    let size_truncated = flags & FLAG_SIZE_TRUNCATED != 0;

    let lines = prepare_lines(&payload[HEADER_LEN..], size_truncated);
    let lpp = lines_per_page(height_limit);
    let (start, mut shown): (usize, Vec<String>) = if split {
        let start = page.min(MAX_PAGES - 1) * lpp;
        let end = (start + lpp).min(lines.len());
        (start, lines.get(start..end).unwrap_or(&[]).to_vec())
    } else if lines.len() > lpp {
        // 切り詰め: 最終行を省略記号に置き換える。
        let mut v = lines[..lpp - 1].to_vec();
        v.push("…".to_owned());
        (0, v)
    } else {
        (0, lines)
    };
    let _ = start;
    if shown.is_empty() {
        shown.push(String::new());
    }
    let height = (MARGIN * 2 + shown.len() as u32 * LINE_H).min(height_limit);
    let mut img = RgbaImage::from_pixel(PAGE_W, height, image::Rgba([255, 255, 255, 255]));
    for (row, line) in shown.iter().enumerate() {
        draw_line(&mut img, line, row);
    }
    Some(DynamicImage::ImageRgba8(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_name_helpers() {
        assert!(is_text_entry_raw(b"a/b.TXT"));
        assert!(!is_text_entry_raw(b".txt"), "拡張子だけの名前は対象外");
        assert!(!is_text_entry_raw(b"a.txt.bak"));
        assert!(is_text_entry_name(&page_entry_name("n/a.txt", 3)));
        assert_eq!(split_entry_name(&page_entry_name("n/a.txt", 3)), ("n/a.txt", 3));
        assert_eq!(split_entry_name("n/a.txt"), ("n/a.txt", 0));
        assert_eq!(page_display_name("a.txt", 0), "a.txt");
        assert_eq!(page_display_name("a.txt", 1), "a_p02.txt");
        assert_eq!(page_display_name("note", 2), "note_p03");
    }

    #[test]
    fn unit_widths_follow_east_asian_width() {
        assert_eq!(char_units('a'), 1);
        assert_eq!(char_units('あ'), 2);
        assert_eq!(char_units('漢'), 2);
        assert_eq!(char_units('ｱ'), 1, "半角カナは1マス");
        assert_eq!(char_units('\u{0301}'), 0, "結合文字は0マス");
    }

    #[test]
    fn wrap_breaks_by_cell_width_and_keeps_blank_lines() {
        let lines = wrap_lines("abcdef", 4, 1000);
        assert_eq!(lines, ["abcd", "ef"]);
        let lines = wrap_lines("あいう", 4, 1000);
        assert_eq!(lines, ["あい", "う"], "全角は2マスずつ");
        let lines = wrap_lines("a\n\nb", 10, 1000);
        assert_eq!(lines, ["a", "", "b"]);
        // 幅より広い1文字だけの行でも無限ループしない。
        assert_eq!(wrap_lines("あ", 1, 1000), ["あ"]);
    }

    #[test]
    fn wrap_expands_tabs_and_caps_chars() {
        assert_eq!(wrap_lines("a\tb", 10, 1000), ["a   b"]);
        let lines = wrap_lines(&"x".repeat(100), 10, 25);
        assert_eq!(lines.iter().map(|l| l.chars().count()).sum::<usize>(), 25, "文字数の上限で打ち切る");
    }

    #[test]
    fn page_count_is_one_unless_splitting() {
        assert_eq!(page_count_for(10_000, false, 4000), 1);
        let lpp = lines_per_page(4000);
        assert_eq!(page_count_for(lpp, true, 4000), 1);
        assert_eq!(page_count_for(lpp + 1, true, 4000), 2);
        assert_eq!(page_count_for(0, true, 4000), 1);
        assert_eq!(page_count_for(usize::MAX / 2, true, 4000), MAX_PAGES, "ページ数に上限");
    }

    #[test]
    fn payload_round_trips_and_rejects_foreign_bytes() {
        let opts = TextPageOptions { split: true, height_limit: 1500, ..Default::default() };
        let p = encode_payload(b"hello", 3, &opts, true);
        assert!(is_payload(&p));
        assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 1500);
        assert_eq!(p[16], FLAG_SPLIT | FLAG_SIZE_TRUNCATED);
        assert_eq!(&p[HEADER_LEN..], b"hello");
        assert!(!is_payload(b"\xff\xd8\xff jpeg"));
        assert!(!is_payload(b"NEKOTXT1"), "ヘッダー未満は不正");
        assert!(render(b"not a payload").is_none());
    }

    #[test]
    fn render_makes_a_white_page_with_dark_ink_and_respects_the_height_limit() {
        let opts = TextPageOptions::default();
        let img = render(&encode_payload("Hello, Nekoviewer!\nsecond line".as_bytes(), 0, &opts, false)).unwrap().to_rgba8();
        assert_eq!(img.width(), PAGE_W);
        assert_eq!(img.height(), MARGIN * 2 + 2 * LINE_H);
        assert_eq!(img.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert!(img.pixels().any(|p| p.0[0] < 100), "文字のインクがある");

        // 長い本文の切り詰め: 高さは上限どおり。
        let long = "line\n".repeat(5000);
        let img = render(&encode_payload(long.as_bytes(), 0, &opts, false)).unwrap();
        assert_eq!(img.height(), MARGIN * 2 + lines_per_page(opts.height_limit) as u32 * LINE_H);
        assert!(img.height() <= opts.height_limit);
    }

    #[test]
    fn split_pages_cover_all_lines_and_the_last_page_is_shorter() {
        let opts = TextPageOptions { split: true, height_limit: 1000, ..Default::default() };
        let lpp = lines_per_page(1000);
        let body = (0..lpp * 2 + 3).map(|i| format!("row{i}")).collect::<Vec<_>>().join("\n");
        let n = count_pages(body.as_bytes(), false, &opts);
        assert_eq!(n, 3);
        let sizes: Vec<u32> = (0..n as u32)
            .map(|p| render(&encode_payload(body.as_bytes(), p, &opts, false)).unwrap().height())
            .collect();
        assert_eq!(sizes[0], sizes[1]);
        assert_eq!(sizes[2], MARGIN * 2 + 3 * LINE_H, "最終ページは余った行だけ");
        // 範囲外のページ番号でも落ちない（空ページの高さで返る）。
        assert!(render(&encode_payload(body.as_bytes(), 99, &opts, false)).is_some());
    }

    #[test]
    fn hostile_text_is_sanitized_before_layout() {
        let body = "a\u{202E}b\x07c\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}\u{0301}d";
        let lines = prepare_lines(body.as_bytes(), false);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('\u{202E}') && !lines[0].contains('\x07'));
        assert!(lines[0].chars().filter(|c| is_combining(*c)).count() <= 3);
    }

    #[test]
    fn size_truncated_adds_an_ellipsis_line() {
        let lines = prepare_lines(b"abc", true);
        assert_eq!(lines.last().map(String::as_str), Some("…"));
    }

    /// 目視用: `NEKO_DUMP_DIR` に見本のページをPNGで書き出す（`cargo test dump_sample_pages -- --ignored`）。
    #[test]
    #[ignore]
    fn dump_sample_pages() {
        let Ok(dir) = std::env::var("NEKO_DUMP_DIR") else { return };
        let body = "吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。\nThe quick brown fox jumps over the lazy dog. 0123456789\n\n\tタブ字下げ Tab indent\n漢字・ひらがな・カタカナ・ＡＢＣ・abc・記号!?（）「」『』～…\n这是中文测试：今天天气很好。\n한글 테스트: 안녕하세요\ne\u{0301} 結合文字 emoji \u{1F600} 長い行はマス数で折り返されます。長い行はマス数で折り返されます。長い行はマス数で折り返されます。";
        let opts = TextPageOptions::default();
        let img = render(&encode_payload(body.as_bytes(), 0, &opts, false)).unwrap();
        img.save(std::path::Path::new(&dir).join("sample_text_page.png")).unwrap();
    }

    #[test]
    fn options_state_string_round_trips_and_rejects_garbage() {
        let o = TextPageOptions { enabled: false, height_limit: 5000, split: true, read_limit: 2048 * 1024, count_in_pages: true };
        assert_eq!(o.to_state_str(), "0,5000,1,2048,1");
        assert_eq!(TextPageOptions::from_state_str(&o.to_state_str()), Some(o));
        assert_eq!(TextPageOptions::default().to_state_str(), "1,4000,0,1024,0");
        for bad in ["", "1,2,3", "1,4000,0,1024", "x,4000,0,1024,0", "1,4000,2,1024,0", "1,abc,0,1024,0", "1,4000,0,99999999999999999999,0"] {
            assert_eq!(TextPageOptions::from_state_str(bad), None, "{bad}");
        }
        assert_eq!(TextPageOptions::from_state_str("1,10,0,1,0").map(|o| (o.height_limit, o.read_limit)), Some((HEIGHT_LIMIT_MIN, READ_LIMIT_MIN)), "範囲外は丸める");
    }

    #[test]
    fn options_are_clamped() {
        let o = TextPageOptions { height_limit: 10, read_limit: usize::MAX, ..Default::default() }.clamped();
        assert_eq!((o.height_limit, o.read_limit), (HEIGHT_LIMIT_MIN, READ_LIMIT_MAX));
    }
}
