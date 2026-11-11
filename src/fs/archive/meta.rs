//! アーカイブの詳細テキスト（ComicInfo.xml・内部の .nfo / info.txt / readme.txt・zipコメント）の抽出と、表示用に安全化する純関数。
//!
//! 安全方針（外部の不正なファイルを読む前提）:
//! - 読み出しは `take(上限)` で打ち切る。宣言サイズは信用しない（zip爆弾対策）。
//! - ディスクには何も展開しない。エントリ名は検索にだけ使う。
//! - 文字は制御文字・BiDi制御・過剰な結合文字を落としてから使う（表示の偽装・崩れ防止）。
//! - XMLパーサは使わない。必要なタグの中身だけを取り出し、実体は基本5種と数値参照だけ解く（DTD・外部実体は無関係）。
//! - 暗号化エントリ・壊れたエントリは黙って飛ばす。パニックさせない。

use std::io::Read;
use std::path::Path;

use super::detect::{detect_format, ArchiveFormat};

/// ComicInfo.xml・.nfo 1件あたりの読み出し上限（バイト）。
pub const DETAIL_READ_LIMIT: u64 = 256 * 1024;
/// zipコメントの上限（バイト）。zip仕様上の最大は 65535。
const COMMENT_LIMIT: usize = 64 * 1024;
/// 拾う付属テキスト（.nfo / info.txt / readme.txt）の最大件数。
const MAX_NFO_FILES: usize = 4;
/// 連続して許す結合文字の数（これを超える分は落とす。zalgo対策）。
const MAX_COMBINING_RUN: usize = 3;

/// リストの「ページ数」列に出す文字列。未取得（0）は空、999 を超えたら `>999`。
pub fn format_page_count(n: u32) -> String {
    match n {
        0 => String::new(),
        1..=999 => n.to_string(),
        _ => ">999".to_string(),
    }
}

pub(crate) fn is_combining(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// 表示に出してはいけない文字（改行・タブ以外の制御文字、BiDi制御、BOM、ゼロ幅スペース）。
fn is_forbidden(c: char) -> bool {
    match c as u32 {
        0x0A | 0x09 => false,
        0x00..=0x1F | 0x7F..=0x9F => true,
        0x061C | 0x200B | 0x200E | 0x200F | 0xFEFF => true,
        0x202A..=0x202E | 0x2066..=0x2069 => true,
        _ => false,
    }
}

/// 表示用に安全化する。改行は `\n` に統一し、禁止文字と過剰な結合文字を落とす。
pub fn sanitize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cr = false;
    let mut combining_run = 0usize;
    for c in s.chars() {
        // \r\n と単独の \r、U+2028/2029 は改行1つにまとめる。
        if c == '\r' || c == '\u{2028}' || c == '\u{2029}' {
            out.push('\n');
            prev_cr = c == '\r';
            combining_run = 0;
            continue;
        }
        if c == '\n' && prev_cr {
            prev_cr = false;
            continue;
        }
        prev_cr = false;
        if is_forbidden(c) {
            continue;
        }
        if is_combining(c) {
            combining_run += 1;
            if combining_run > MAX_COMBINING_RUN {
                continue;
            }
        } else {
            combining_run = 0;
        }
        out.push(c);
    }
    out
}

/// 上限で打ち切られて末尾が欠けた UTF-8 を、欠けた分だけ捨てて読めるようにする。
fn utf8_prefix(bytes: &[u8]) -> Option<&str> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(s),
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()]).ok(),
        Err(_) => None,
    }
}

/// バイト列を文字コード判定してデコードし、安全化して返す（lossy。パニックしない）。
/// 判定順: BOM（UTF-8/16）→ UTF-8 → Shift-JIS → GBK → EUC-KR → Windows-1252。
pub fn decode_text_bytes(bytes: &[u8]) -> String {
    let decoded = decode_raw(bytes);
    sanitize_text(&decoded)
}

fn decode_raw(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode_without_bom_handling(&bytes[2..]).0.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode_without_bom_handling(&bytes[2..]).0.into_owned();
    }
    if let Some(s) = utf8_prefix(bytes) {
        return s.to_owned();
    }
    for enc in [encoding_rs::SHIFT_JIS, encoding_rs::GBK, encoding_rs::EUC_KR] {
        if let Some(s) = enc.decode_without_bom_handling_and_without_replacement(bytes) {
            return s.into_owned();
        }
    }
    encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0.into_owned()
}

/// XMLの実体参照を解く（基本5種と数値参照のみ。それ以外はそのまま残す）。
fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `<tag>中身</tag>` の中身を取り出す（最初の1件。属性なしのタグのみ）。CDATA は中身をそのまま使う。
fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&close)?;
    let inner = xml[start..start + len].trim();
    let value = match inner.strip_prefix("<![CDATA[").and_then(|t| t.strip_suffix("]]>")) {
        Some(raw) => raw.to_owned(),
        None => unescape_xml(inner),
    };
    let value = sanitize_text(&value).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

/// ComicInfo.xml から拾う項目（タグ名, 表示順）。値が空の項目は結果に入れない。
const COMIC_INFO_TAGS: [&str; 16] = [
    "Title", "Series", "Number", "Volume", "Writer", "Penciller", "Publisher", "Imprint", "Genre", "Tags",
    "Year", "Month", "LanguageISO", "PageCount", "Web", "Summary",
];

/// ComicInfo.xml の内容。`fields` は（タグ名, 値）を `COMIC_INFO_TAGS` の順で持つ。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ComicInfo {
    pub fields: Vec<(&'static str, String)>,
}

impl ComicInfo {
    /// タグ名で値を引く（テスト用。画面は `fields` の並びをそのまま使う）。
    #[cfg(test)]
    pub fn get(&self, tag: &str) -> Option<&str> {
        self.fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str())
    }
}

/// ComicInfo.xml のバイト列を読む。拾える項目が1つも無ければ None。
pub fn parse_comic_info(bytes: &[u8]) -> Option<ComicInfo> {
    let xml = decode_raw(bytes);
    let fields: Vec<(&'static str, String)> = COMIC_INFO_TAGS
        .iter()
        .filter_map(|&tag| extract_tag(&xml, tag).map(|v| (tag, v)))
        .collect();
    (!fields.is_empty()).then_some(ComicInfo { fields })
}

/// 付属テキスト（.nfo / info.txt / readme.txt）1件。info.txt・readme.txt は画像ページ（TXTページ）としても残る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfoText {
    pub name: String,
    pub text: String,
}

/// 詳細の種別（リストの「詳細」列のアイコンに対応する）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DetailKinds {
    /// ComicInfo.xml。
    pub xml: bool,
    /// meta.json。
    pub json: bool,
    /// 付属テキスト（.nfo / info.txt / readme.txt）とzipコメント。
    pub txt: bool,
}

impl DetailKinds {
    pub fn any(self) -> bool {
        self.xml || self.json || self.txt
    }

    pub fn to_bits(self) -> u8 {
        u8::from(self.xml) | (u8::from(self.json) << 1) | (u8::from(self.txt) << 2)
    }

    pub fn from_bits(bits: u8) -> Self {
        Self { xml: bits & 1 != 0, json: bits & 2 != 0, txt: bits & 4 != 0 }
    }
}

/// アーカイブの詳細テキスト一式。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DetailMeta {
    pub comic_info: Option<ComicInfo>,
    pub nfo: Vec<NfoText>,
    /// zipコメント（zip/cbzのみ）。
    pub comment: String,
    /// meta.json の本文（安全化済み。読み出し上限 `META_JSON_LIMIT`）。
    pub meta_json: Option<String>,
    /// info.txt の `Rating:` 行の値を100倍した整数（4.82 → 482）。info.txt が無い・行が無い・範囲外は None。
    pub info_rating: Option<u32>,
}

impl DetailMeta {
    /// 種別ごとの有無。
    pub fn kinds(&self) -> DetailKinds {
        DetailKinds {
            xml: self.comic_info.is_some(),
            json: self.meta_json.as_ref().is_some_and(|s| !s.trim().is_empty()),
            txt: !self.nfo.is_empty() || !self.comment.trim().is_empty(),
        }
    }

    /// 詳細ウィンドウに出せる内容があるか（リストの「詳細」列の判定）。
    pub fn has_detail(&self) -> bool {
        self.kinds().any()
    }
}

/// info.txt の本文から `Rating:` 行を読み、100倍した整数にして返す（`Rating: 4.82` → 482）。
/// 行頭のキーが `rating`（大文字小文字は区別しない）の最初の行を使う。0.00〜5.00の範囲外・数値でない値は None。
/// 0.00 は有効な値として返す（`Some(0)`）。小数第3位以下は四捨五入。
pub fn parse_info_rating(text: &str) -> Option<u32> {
    let line = text.lines().find(|l| l.split_once(':').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("rating")))?;
    let value = line.split_once(':')?.1.trim();
    // 「4.82」のほか、末尾に注記があっても先頭の語だけを見る（`4.82 (123 votes)` など）。
    let token = value.split_whitespace().next()?;
    let n: f64 = token.parse().ok()?;
    if !n.is_finite() || !(0.0..=5.0).contains(&n) {
        return None;
    }
    Some((n * 100.0).round() as u32)
}

/// 詳細テキストの抽出に対応している形式か（zip/cbzのみ。7z・tar・フォルダは対応外＝リストでは N/A）。
pub fn detail_supported(path: &Path) -> bool {
    !path.is_dir() && matches!(detect_format(path), ArchiveFormat::Zip)
}

/// エントリ名の最後の要素（`/` と `\` の両方で切る）。
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

fn is_comic_info_name(name: &str) -> bool {
    base_name(name).eq_ignore_ascii_case("comicinfo.xml")
}

/// 詳細として拾う付属テキストの名前か。`*.nfo` と、ファイル名が `info.txt` / `readme.txt` のもの
/// （大文字小文字は区別せず、サブフォルダ内も可）。ほかの `.txt` は拾わない（TXTページとしてだけ扱う）。
fn is_detail_text_name(name: &str) -> bool {
    let base = base_name(name);
    if base.eq_ignore_ascii_case("info.txt") || base.eq_ignore_ascii_case("readme.txt") {
        return true;
    }
    base.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("nfo"))
}

/// 読み出し上限つきでバイト列を読む。
fn read_limited<R: Read>(r: R, limit: u64) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(limit).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// zip/cbz から詳細テキストを読む。開けない・zipでない場合は None。
/// 詳細が無くても `Some(空のDetailMeta)` を返す（「確認済みで無し」と「未確認」を区別するため）。
pub fn read_zip_detail(path: &Path) -> Option<DetailMeta> {
    read_zip_bundle(path).map(|b| b.detail)
}

/// zip/cbz 内の `info.txt`（ファイル名が完全一致。大文字小文字は区別せず、サブフォルダ内も可）の本文を読む。
/// zipとして開けなければ None。開けて info.txt が無ければ `Some(None)`。複数あれば最初のもの。
/// 本文は安全化済み（`decode_text_bytes`）で、読み出しは `DETAIL_READ_LIMIT` までに切る。
pub fn read_zip_info_txt(path: &Path) -> Option<Option<String>> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = ::zip::ZipArchive::new(file).ok()?;
    let index = (0..archive.len()).find(|&i| {
        archive.name_for_index(i).is_some_and(|n| !n.ends_with('/') && base_name(n).eq_ignore_ascii_case("info.txt"))
    });
    let Some(index) = index else { return Some(None) };
    // 暗号化・壊れたエントリは「無し」として扱う（走査全体を止めない）。
    let Ok(entry) = archive.by_index(index) else { return Some(None) };
    Some(read_limited(entry, DETAIL_READ_LIMIT).map(|b| decode_text_bytes(&b)))
}

/// meta.json 1件あたりの読み出し上限（バイト）。タグが多いものも入る大きさ。
pub const META_JSON_LIMIT: u64 = 1024 * 1024;

/// zip/cbz 内の、タグ取得に使う付属データ（info.txt・ComicInfo.xml・meta.json）。無いものは None。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TagSourceFiles {
    /// info.txt の本文（安全化済み）。
    pub info_txt: Option<String>,
    pub comic_info: Option<ComicInfo>,
    /// meta.json の本文（安全化済み。JSONとしての解析は呼び出し側）。
    pub meta_json: Option<String>,
}

fn is_meta_json_name(name: &str) -> bool {
    base_name(name).eq_ignore_ascii_case("meta.json")
}

/// zip/cbz を1回だけ開いて読んだ、詳細表示とタグ取得の両方の元。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ZipBundle {
    /// 詳細ウィンドウ・リストの「詳細」列の元。
    pub detail: DetailMeta,
    /// タグ取得の元（info.txt・ComicInfo.xml・meta.json）。
    pub tag: TagSourceFiles,
    /// 画像拡張子のエントリ数（TXTページは含まない。ページ数列に使えるが、開いたときの数と細部が違うことがある）。
    pub image_count: usize,
}

/// zip/cbz を1回だけ開いて、詳細テキスト（ComicInfo.xml・meta.json・.nfo/info.txt/readme.txt・zipコメント）と
/// タグ取得の元、画像の数をまとめて読む。ファイル名は完全一致（大文字小文字は区別せず、サブフォルダ内も可。複数あれば最初のもの。
/// 付属テキストは最大4件）。zipとして開けなければ None。開けて、どれも無ければ空の `ZipBundle`。
/// 暗号化・壊れたエントリは「無し」として扱う（走査全体を止めない）。
/// 名前の分類はzipの中央ディレクトリ（名前一覧）だけで行い、本体を読むのは対象のエントリだけ。
pub fn read_zip_bundle(path: &Path) -> Option<ZipBundle> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = ::zip::ZipArchive::new(file).ok()?;

    let comment_raw = archive.comment();
    let comment_raw = &comment_raw[..comment_raw.len().min(COMMENT_LIMIT)];
    let mut out = ZipBundle::default();
    out.detail.comment = decode_text_bytes(comment_raw).trim().to_owned();

    let (mut info_idx, mut comic_idx, mut json_idx) = (None, None, None);
    let mut nfo_idxs: Vec<usize> = Vec::new();
    for i in 0..archive.len() {
        let Some(name) = archive.name_for_index(i) else { continue };
        if name.ends_with('/') {
            continue;
        }
        if super::detect::is_image_entry_raw(name.as_bytes()) {
            out.image_count += 1;
            continue;
        }
        if info_idx.is_none() && base_name(name).eq_ignore_ascii_case("info.txt") {
            info_idx = Some(i);
        }
        if comic_idx.is_none() && is_comic_info_name(name) {
            comic_idx = Some(i);
        } else if json_idx.is_none() && is_meta_json_name(name) {
            json_idx = Some(i);
        }
        if nfo_idxs.len() < MAX_NFO_FILES && is_detail_text_name(name) {
            nfo_idxs.push(i);
        }
    }

    if let Some(i) = info_idx
        && let Ok(entry) = archive.by_index(i)
    {
        out.tag.info_txt = read_limited(entry, DETAIL_READ_LIMIT).map(|b| decode_text_bytes(&b));
        out.detail.info_rating = out.tag.info_txt.as_deref().and_then(parse_info_rating);
    }
    if let Some(i) = comic_idx
        && let Ok(entry) = archive.by_index(i)
    {
        out.tag.comic_info = read_limited(entry, DETAIL_READ_LIMIT).and_then(|b| parse_comic_info(&b));
        out.detail.comic_info = out.tag.comic_info.clone();
    }
    if let Some(i) = json_idx
        && let Ok(entry) = archive.by_index(i)
    {
        out.tag.meta_json = read_limited(entry, META_JSON_LIMIT).map(|b| decode_text_bytes(&b));
        out.detail.meta_json = out.tag.meta_json.clone();
    }
    for i in nfo_idxs {
        let Ok(entry) = archive.by_index(i) else { continue };
        let name = super::zip::decode_zip_name(entry.name_raw());
        if let Some(bytes) = read_limited(entry, DETAIL_READ_LIMIT) {
            let text = decode_text_bytes(&bytes).trim().to_owned();
            if !text.is_empty() {
                out.detail.nfo.push(NfoText { name: base_name(&name).to_owned(), text });
            }
        }
    }
    Some(out)
}

/// zip/cbz のタグ取得の元（`read_zip_bundle` の一部）。zipとして開けなければ None。
pub fn read_zip_tag_sources(path: &Path) -> Option<TagSourceFiles> {
    read_zip_bundle(path).map(|b| b.tag)
}

/// 形式を判定して、詳細テキスト・タグ取得の元・画像数をまとめて読む。当面は zip/cbz のみ対応（他形式は None＝対象外）。
pub fn read_bundle(path: &Path) -> Option<ZipBundle> {
    match detect_format(path) {
        ArchiveFormat::Zip => read_zip_bundle(path),
        _ => None,
    }
}

/// 形式を判定して info.txt の本文を読む。当面は zip/cbz のみ対応（他形式は None＝対象外）。
pub fn read_info_txt(path: &Path) -> Option<Option<String>> {
    match detect_format(path) {
        ArchiveFormat::Zip => read_zip_info_txt(path),
        _ => None,
    }
}

/// 形式を判定して詳細テキストを読む。当面は zip/cbz のみ対応（他形式は None）。
pub fn read_detail(path: &Path) -> Option<DetailMeta> {
    match detect_format(path) {
        ArchiveFormat::Zip => read_zip_detail(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn page_count_text_hides_zero_and_caps_at_999() {
        assert_eq!(format_page_count(0), "");
        assert_eq!(format_page_count(1), "1");
        assert_eq!(format_page_count(999), "999");
        assert_eq!(format_page_count(1000), ">999");
        assert_eq!(format_page_count(u32::MAX), ">999");
    }

    #[test]
    fn sanitize_drops_control_and_bidi_and_unifies_newlines() {
        assert_eq!(sanitize_text("a\u{202E}b\u{0007}c\u{FEFF}d"), "abcd");
        assert_eq!(sanitize_text("x\u{1b}[31my"), "x[31my", "ESCは落とす（残りは通常の文字）");
        assert_eq!(sanitize_text("a\r\nb\rc\u{2028}d"), "a\nb\nc\nd");
        assert_eq!(sanitize_text("tab\there"), "tab\there");
    }

    #[test]
    fn sanitize_caps_combining_marks() {
        let zalgo = format!("a{}", "\u{0301}".repeat(50));
        assert_eq!(sanitize_text(&zalgo).chars().count(), 1 + MAX_COMBINING_RUN);
        // 通常の濁点付きなどは影響を受けない。
        assert_eq!(sanitize_text("e\u{0301}"), "e\u{0301}");
    }

    #[test]
    fn decode_handles_bom_and_legacy_encodings() {
        assert_eq!(decode_text_bytes(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(decode_text_bytes(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode_text_bytes(&[0xFE, 0xFF, 0, b'h', 0, b'i']), "hi");
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のテキスト");
        assert_eq!(decode_text_bytes(&sjis), "日本語のテキスト");
        assert_eq!(decode_text_bytes("あいう".as_bytes()), "あいう");
    }

    #[test]
    fn decode_tolerates_truncated_utf8_and_garbage() {
        let full = "あいう".as_bytes();
        assert_eq!(decode_text_bytes(&full[..full.len() - 1]), "あい", "末尾の欠けた1文字だけ捨てる");
        // どの経路でも落ちない。
        let _ = decode_text_bytes(&[0x81, 0x00, 0xFF, 0xFE, 0xFD]);
        assert_eq!(decode_text_bytes(&[]), "");
    }

    #[test]
    fn comic_info_picks_fields_with_entities_and_cdata() {
        let xml = "<?xml version=\"1.0\"?><ComicInfo><Title>A &amp; B &#x3042;</Title>\
                   <Series>S</Series><Summary><![CDATA[x < y]]></Summary><Writer>  </Writer>\
                   <Unknown>zzz</Unknown></ComicInfo>";
        let info = parse_comic_info(xml.as_bytes()).unwrap();
        assert_eq!(info.get("Title"), Some("A & B あ"));
        assert_eq!(info.get("Series"), Some("S"));
        assert_eq!(info.get("Summary"), Some("x < y"));
        assert_eq!(info.get("Writer"), None, "空白だけの項目は入れない");
        assert_eq!(info.get("Unknown"), None);
        assert_eq!(info.fields[0].0, "Title", "並びはタグ表の順");
    }

    #[test]
    fn comic_info_ignores_entity_bombs_and_empty() {
        let bomb = "<!DOCTYPE x [<!ENTITY a \"&b;&b;\">]><ComicInfo><Title>&a;</Title></ComicInfo>";
        let info = parse_comic_info(bomb.as_bytes()).unwrap();
        assert_eq!(info.get("Title"), Some("&a;"), "独自実体は展開しない");
        assert!(parse_comic_info(b"<ComicInfo></ComicInfo>").is_none());
        assert!(parse_comic_info(b"").is_none());
    }

    #[test]
    fn name_matching_is_case_insensitive_and_path_aware() {
        assert!(is_comic_info_name("ComicInfo.xml"));
        assert!(is_comic_info_name("sub/COMICINFO.XML"));
        assert!(!is_comic_info_name("ComicInfo.xml.bak"));
        assert!(is_detail_text_name("readme.NFO"));
        assert!(is_detail_text_name("a\\b\\info.nfo"));
        assert!(!is_detail_text_name(".nfo"));
        assert!(is_detail_text_name("info.txt"));
        assert!(is_detail_text_name("sub/README.TXT"));
        assert!(is_detail_text_name("x\\Info.Txt"));
        assert!(!is_detail_text_name("info.txt.bak"));
        assert!(!is_detail_text_name("myinfo.txt"), "完全一致のみ");
        assert!(!is_detail_text_name("notes.txt"));
    }

    fn build_zip(name: &str, files: &[(&str, &[u8])], comment: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("nekoviewer_meta_test_{}_{name}.zip", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let mut w = ::zip::ZipWriter::new(file);
        w.set_comment(comment);
        for (n, data) in files {
            w.start_file(*n, ::zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        path
    }

    #[test]
    fn zip_tag_sources_read_info_comic_info_and_meta_json_in_one_pass() {
        let path = build_zip(
            "tagsrc",
            &[
                ("001.jpg", b"img"),
                ("sub/Info.TXT", "Title\nCategory: Manga\n".as_bytes()),
                ("ComicInfo.xml", b"<ComicInfo><Title>T</Title><Writer>kirome</Writer></ComicInfo>"),
                ("META.JSON", br#"{"id":1,"tags":[]}"#),
                ("meta.json.bak", b"not this"),
            ],
            "",
        );
        let got = read_zip_tag_sources(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got.info_txt.as_deref(), Some("Title\nCategory: Manga\n"));
        assert_eq!(got.comic_info.as_ref().and_then(|c| c.get("Writer")), Some("kirome"));
        assert_eq!(got.meta_json.as_deref(), Some(r#"{"id":1,"tags":[]}"#), "名前は完全一致（meta.json.bak は対象外）");
    }

    #[test]
    fn zip_tag_sources_are_none_inside_and_none_outside() {
        let path = build_zip("tagsrc_none", &[("001.jpg", b"img")], "");
        let got = read_zip_tag_sources(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, TagSourceFiles::default(), "zip だが付属データなし");
        let not_zip = std::env::temp_dir().join(format!("nekoviewer_meta_test_{}_notzip.zip", std::process::id()));
        std::fs::write(&not_zip, b"plain text").unwrap();
        assert!(read_zip_tag_sources(&not_zip).is_none(), "zip として開けない");
        let _ = std::fs::remove_file(&not_zip);
    }

    #[test]
    fn zip_tag_sources_cap_the_json_size_and_skip_empty_comic_info() {
        let mut big = br#"{"tags":["#.to_vec();
        big.extend(std::iter::repeat(b' ').take(META_JSON_LIMIT as usize * 2));
        let path = build_zip("tagsrc_big", &[("meta.json", &big), ("ComicInfo.xml", b"<ComicInfo></ComicInfo>")], "");
        let got = read_zip_tag_sources(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got.meta_json.as_ref().map(|s| s.len() as u64), Some(META_JSON_LIMIT));
        assert!(got.comic_info.is_none(), "拾える項目が無い ComicInfo.xml は無し扱い");
    }

    #[test]
    fn bundle_reads_detail_tag_sources_and_image_count_in_one_open() {
        let path = build_zip(
            "bundle",
            &[
                ("001.jpg", b"i"),
                ("sub/002.PNG", b"i"),
                ("notes.txt", b"page, not detail"),
                ("Info.txt", "T\nCategory: Manga\n".as_bytes()),
                ("ComicInfo.xml", b"<ComicInfo><Title>T</Title></ComicInfo>"),
                ("meta.json", br#"{"tags":[]}"#),
            ],
            "",
        );
        let b = read_zip_bundle(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(b.image_count, 2, "画像だけ数える");
        assert_eq!(b.detail.kinds(), DetailKinds { xml: true, json: true, txt: true });
        assert_eq!(b.detail.nfo.len(), 1);
        assert_eq!(b.tag.info_txt.as_deref(), Some("T\nCategory: Manga\n"));
        assert_eq!(b.tag.meta_json.as_deref(), Some(r#"{"tags":[]}"#));
        assert_eq!(b.detail.meta_json, b.tag.meta_json);
    }

    #[test]
    fn info_rating_reads_two_decimals_and_rejects_bad_values() {
        assert_eq!(parse_info_rating("Category: Manga\nFavorited: 442 times\nRating: 4.82\n"), Some(482));
        assert_eq!(parse_info_rating("rating:5"), Some(500));
        assert_eq!(parse_info_rating("RATING : 0.00"), Some(0), "0.00 は有効な値");
        assert_eq!(parse_info_rating("Rating: 4.8 (123 votes)"), Some(480));
        assert_eq!(parse_info_rating("Rating: 4.999"), Some(500), "第3位は四捨五入");
        assert_eq!(parse_info_rating("Rating: 5.01"), None, "範囲外");
        assert_eq!(parse_info_rating("Rating: -1"), None);
        assert_eq!(parse_info_rating("Rating: NaN"), None);
        assert_eq!(parse_info_rating("Rating: good"), None);
        assert_eq!(parse_info_rating("Rating:"), None);
        assert_eq!(parse_info_rating("Title only"), None);
        assert_eq!(parse_info_rating("Rating: x\nRating: 4.0"), None, "最初の Rating 行だけを見る");
    }

    #[test]
    fn bundle_carries_the_info_rating_only_from_info_txt() {
        let with = build_zip("rating_with", &[("1.jpg", b"i"), ("info.txt", b"T\nRating: 4.82\n")], "");
        let readme = build_zip("rating_readme", &[("1.jpg", b"i"), ("readme.txt", b"Rating: 4.82\n")], "");
        let a = read_zip_bundle(&with).unwrap();
        let b = read_zip_bundle(&readme).unwrap();
        let _ = std::fs::remove_file(&with);
        let _ = std::fs::remove_file(&readme);
        assert_eq!(a.detail.info_rating, Some(482));
        assert_eq!(b.detail.info_rating, None, "readme.txt の Rating は見ない");
    }

    #[test]
    fn json_only_zip_counts_as_having_detail() {
        let path = build_zip("jsononly", &[("001.jpg", b"i"), ("meta.json", br#"{"a":1}"#)], "");
        let d = read_zip_detail(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(d.kinds(), DetailKinds { xml: false, json: true, txt: false });
        assert!(d.has_detail());
    }

    #[test]
    fn comment_only_is_txt_and_blank_json_is_nothing() {
        let path = build_zip("commentonly", &[("001.jpg", b"i"), ("meta.json", b"  \n")], "from site");
        let d = read_zip_detail(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(d.kinds(), DetailKinds { xml: false, json: false, txt: true });
    }

    #[test]
    fn detail_kinds_bits_round_trip_and_support_check() {
        for bits in 0..8u8 {
            assert_eq!(DetailKinds::from_bits(bits).to_bits(), bits);
        }
        assert!(!detail_supported(&std::env::temp_dir()), "フォルダは対応外");
    }

    #[test]
    fn zip_detail_reads_comic_info_nfo_and_comment() {
        let path = build_zip(
            "full",
            &[
                ("001.jpg", b"not really an image"),
                ("ComicInfo.xml", b"<ComicInfo><Title>T</Title></ComicInfo>"),
                ("note/info.nfo", "メモ\r\n2行目".as_bytes()),
                ("notes.txt", b"other txt is a page, not a detail"),
                ("Info.TXT", "付属のinfo.txt\r\n2行目".as_bytes()),
            ],
            "from site",
        );
        let meta = read_zip_detail(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(meta.has_detail());
        assert_eq!(meta.comic_info.unwrap().get("Title"), Some("T"));
        assert_eq!(meta.nfo.len(), 2, "nfo と info.txt。notes.txt は拾わない");
        assert_eq!(meta.nfo[0].name, "info.nfo");
        assert_eq!(meta.nfo[0].text, "メモ\n2行目");
        assert_eq!(meta.nfo[1].name, "Info.TXT");
        assert_eq!(meta.nfo[1].text, "付属のinfo.txt\n2行目");
        assert_eq!(meta.comment, "from site");
    }

    #[test]
    fn info_txt_reader_distinguishes_found_absent_and_not_zip() {
        let found = build_zip("info_found", &[("0001.jpg", b"x"), ("sub/INFO.TXT", "Category: Manga\r\n".as_bytes())], "");
        assert_eq!(read_zip_info_txt(&found), Some(Some("Category: Manga\n".to_string())));
        let absent = build_zip("info_absent", &[("0001.jpg", b"x"), ("myinfo.txt", b"no"), ("info.txt.bak", b"no")], "");
        assert_eq!(read_zip_info_txt(&absent), Some(None), "完全一致だけが info.txt");
        let bad = std::env::temp_dir().join(format!("nekoviewer_meta_test_{}_info_bad.zip", std::process::id()));
        std::fs::write(&bad, b"not a zip").unwrap();
        assert_eq!(read_zip_info_txt(&bad), None);
        assert_eq!(read_zip_info_txt(Path::new("/nonexistent/x.zip")), None);
        for p in [found, absent, bad] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn zip_detail_is_empty_when_nothing_and_none_when_not_zip() {
        let path = build_zip("plain", &[("1.jpg", b"x"), ("2.txt", b"y")], "");
        let meta = read_zip_detail(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!meta.has_detail(), "txt は詳細に含めない");

        let bad = std::env::temp_dir().join(format!("nekoviewer_meta_test_{}_bad.zip", std::process::id()));
        std::fs::write(&bad, b"this is not a zip").unwrap();
        assert!(read_zip_detail(&bad).is_none());
        let _ = std::fs::remove_file(&bad);
        assert!(read_zip_detail(Path::new("/nonexistent/file.zip")).is_none());
    }

    #[test]
    fn zip_detail_caps_read_size_of_huge_nfo() {
        let huge = vec![b'a'; (DETAIL_READ_LIMIT as usize) * 3];
        let path = build_zip("huge", &[("big.nfo", &huge)], "");
        let meta = read_zip_detail(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(meta.nfo[0].text.len() as u64, DETAIL_READ_LIMIT);
    }
}
