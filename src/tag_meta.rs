//! ComicInfo.xml・meta.json（アーカイブ内の「正の情報」）の型と、優先順位に従った統合（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! タグ自動取得の取得元の優先順位は、**ComicInfo.xml → meta.json（空欄を埋める）→ info.txt → タイトル（ファイル名）**。
//! ここは先頭の2つ。XML と JSON は「完全に正」の情報として扱い、info.txt・ファイル名より優先する。
//!
//! - ComicInfo.xml: `Title` `Series` `Writer` `Publisher` `Imprint` `Genre` `Tags` `Year` `Month` `LanguageISO`
//!   （`fs::archive::meta::parse_comic_info` が拾った項目）
//! - meta.json: `title.english` / `title.japanese`、`tags[]`（`type` つき: language / category / tag / artist / group / parody / character）。
//!   `type: tag` の名前が `group` でも、それは普通のタグ名（種類の group ではない）
//! - 統合の規則（`resolve`）: 項目ごとに、XML に値があれば XML、無ければ JSON。タグ（属性）だけは両方の和集合。
//!   Series の `original` / `オリジナル` は元作品ではないので、パロディ元にしない
//! - 作者名のブレ（英語話者の翻訳・誤字）は、ここでは直さない（既存の類似照合に任せる）

// 候補への統合（X2）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use serde::Deserialize;

use crate::fs::archive::meta::ComicInfo;
use crate::tag_features::split_multi;
use crate::tag_similarity::{exact_key, MatchOptions};
use crate::tag_source::collapse_spaces;

// ── ComicInfo.xml ─────────────────────────────────────────────────────

/// ComicInfo.xml から、タグ取得に使う項目を取り出したもの。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComicMeta {
    pub title: Option<String>,
    pub series: Option<String>,
    /// `Writer`（複数なら分割）。
    pub writers: Vec<String>,
    /// `Publisher` と `Imprint`（分割して、重複を除く）。
    pub publishers: Vec<String>,
    /// `Genre`（`,` 区切りなら分割）。
    pub genres: Vec<String>,
    /// `Tags`（`,` 区切り）。
    pub tags: Vec<String>,
    pub year: Option<u32>,
    pub month: Option<u8>,
    /// `LanguageISO`。
    pub language: Option<String>,
}

/// `,` 区切りの文字列を、空を除いた項目にする。
fn split_commas(s: &str) -> Vec<String> {
    s.split([',', '、', '，', ';', '；']).map(collapse_spaces).filter(|x| !x.is_empty()).collect()
}

/// 重複（完全一致キーが同じ）を除く。出現順を保つ。
fn dedupe(v: Vec<String>) -> Vec<String> {
    let opts = MatchOptions::default();
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for s in v {
        let k = exact_key(&s, &opts);
        if !k.is_empty() && !seen.contains(&k) {
            seen.push(k);
            out.push(s);
        }
    }
    out
}

fn parse_year(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok().filter(|y| (1000..=2999).contains(y))
}

fn parse_month(s: &str) -> Option<u8> {
    s.trim().parse::<u8>().ok().filter(|m| (1..=12).contains(m))
}

impl ComicMeta {
    pub(crate) fn from_comic_info(info: &ComicInfo) -> Self {
        let get = |tag: &str| info.fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str());
        let owned = |tag: &str| get(tag).map(collapse_spaces).filter(|s| !s.is_empty());
        let mut publishers: Vec<String> = Vec::new();
        for tag in ["Publisher", "Imprint"] {
            if let Some(v) = get(tag) {
                publishers.extend(split_multi(v));
            }
        }
        Self {
            title: owned("Title"),
            series: owned("Series"),
            writers: get("Writer").map(split_multi).unwrap_or_default(),
            publishers: dedupe(publishers),
            genres: get("Genre").map(split_commas).unwrap_or_default(),
            tags: get("Tags").map(|t| dedupe(split_commas(t))).unwrap_or_default(),
            year: get("Year").and_then(parse_year),
            month: get("Month").and_then(parse_month),
            language: owned("LanguageISO"),
        }
    }
}

// ── meta.json ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RawTitle {
    #[serde(default)]
    english: Option<String>,
    #[serde(default)]
    japanese: Option<String>,
}

#[derive(Deserialize)]
struct RawTag {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawJson {
    #[serde(default)]
    title: Option<RawTitle>,
    #[serde(default)]
    tags: Vec<RawTag>,
}

/// meta.json から、タグ取得に使う項目を取り出したもの。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct JsonMeta {
    pub title_english: Option<String>,
    pub title_japanese: Option<String>,
    pub languages: Vec<String>,
    /// `type: category`（メインタグの元）。
    pub categories: Vec<String>,
    pub artists: Vec<String>,
    pub groups: Vec<String>,
    pub parodies: Vec<String>,
    pub characters: Vec<String>,
    /// `type: tag`（属性タグ）。名前が `group` でも、普通のタグ。
    pub tags: Vec<String>,
}

impl JsonMeta {
    /// meta.json の本文を読む。JSON として読めない・拾える項目が1つも無ければ None。知らない `type` は読み飛ばす。
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let raw: RawJson = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        let mut out = JsonMeta::default();
        let clean = |s: Option<String>| s.map(|s| collapse_spaces(&s)).filter(|s| !s.is_empty());
        if let Some(t) = raw.title {
            out.title_english = clean(t.english);
            out.title_japanese = clean(t.japanese);
        }
        for tag in raw.tags {
            let name = collapse_spaces(&tag.name);
            if name.is_empty() {
                continue;
            }
            match tag.kind.trim().to_ascii_lowercase().as_str() {
                "language" => out.languages.push(name),
                "category" => out.categories.push(name),
                "artist" => out.artists.push(name),
                "group" => out.groups.push(name),
                "parody" => out.parodies.push(name),
                "character" => out.characters.push(name),
                "tag" => out.tags.push(name),
                _ => {}
            }
        }
        for v in [&mut out.languages, &mut out.categories, &mut out.artists, &mut out.groups, &mut out.parodies, &mut out.characters, &mut out.tags] {
            *v = dedupe(std::mem::take(v));
        }
        let empty = out == JsonMeta::default();
        (!empty).then_some(out)
    }
}

// ── 統合 ──────────────────────────────────────────────────────────────

/// アーカイブ内の「正の情報」（ComicInfo.xml と meta.json）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArchiveMeta {
    pub comic: Option<ComicMeta>,
    pub json: Option<JsonMeta>,
}

impl ArchiveMeta {
    /// どちらも無ければ None。
    pub(crate) fn from_sources(comic_info: Option<&ComicInfo>, meta_json: Option<&str>) -> Option<Self> {
        let m = Self { comic: comic_info.map(ComicMeta::from_comic_info), json: meta_json.and_then(JsonMeta::parse) };
        (m.comic.is_some() || m.json.is_some()).then_some(m)
    }

    pub(crate) fn has_xml(&self) -> bool {
        self.comic.is_some()
    }

    pub(crate) fn has_json(&self) -> bool {
        self.json.is_some()
    }
}

/// 年月（`2023-12`）。月が無ければ年だけ（月0として並ぶ）。月のデータは書かない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct YearMonth {
    pub year: u32,
    pub month: Option<u8>,
}

impl YearMonth {
    /// 要素の名前。`2023-12`、月が無ければ `2023`。文字列として整列すると、年→月の順に並ぶ（年だけのものは、その年の先頭）。
    pub(crate) fn element_name(&self) -> String {
        match self.month {
            Some(m) => format!("{:04}-{m:02}", self.year),
            None => format!("{:04}", self.year),
        }
    }
}

/// 優先順位に従って統合した、「正の情報」の値。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// 題名の行（XML の Title、JSON の japanese・english の順。重複を除く）。内部照合に使う行は `tag_source::split_title_group` で選ぶ。
    pub titles: Vec<String>,
    pub authors: Vec<String>,
    pub groups: Vec<String>,
    /// パロディ元。`original` は除く。
    pub parodies: Vec<String>,
    pub characters: Vec<String>,
    /// 言語（`ja` / `japanese` など。辞書で正規化するのは呼び出し側）。
    pub languages: Vec<String>,
    /// メインタグ（カテゴリ）の元。
    pub categories: Vec<String>,
    /// 属性タグ（XML の Tags と JSON の tag の和集合）。
    pub attributes: Vec<String>,
    pub year_month: Option<YearMonth>,
}

/// シリーズ名が「元作品なし」（`original` / `オリジナル` など）か。
pub(crate) fn is_original_series(s: &str) -> bool {
    let k = exact_key(s, &MatchOptions::default());
    matches!(k.as_str(), "original" | "オリジナル" | "オリジナル作品" | "originalwork" | "originalseries" | "原創" | "原创" | "原作なし")
}

fn first_nonempty(a: Vec<String>, b: Vec<String>) -> Vec<String> {
    if a.is_empty() {
        b
    } else {
        a
    }
}

/// XML → JSON の順に、項目ごとに空欄を埋めて統合する。
pub(crate) fn resolve(meta: &ArchiveMeta) -> Resolved {
    let empty_c = ComicMeta::default();
    let empty_j = JsonMeta::default();
    let (c, j) = (meta.comic.as_ref().unwrap_or(&empty_c), meta.json.as_ref().unwrap_or(&empty_j));

    let mut titles: Vec<String> = Vec::new();
    titles.extend(c.title.clone());
    titles.extend(j.title_japanese.clone());
    titles.extend(j.title_english.clone());

    let xml_parodies: Vec<String> = c.series.iter().filter(|s| !is_original_series(s)).cloned().collect();
    let json_parodies: Vec<String> = j.parodies.iter().filter(|s| !is_original_series(s)).cloned().collect();

    let mut attributes = c.tags.clone();
    attributes.extend(j.tags.iter().cloned());

    // 年月は、年が分かるときだけ（年が無く月だけは取らない）。
    let year_month = c.year.map(|year| YearMonth { year, month: c.month });

    Resolved {
        titles: dedupe(titles),
        authors: first_nonempty(c.writers.clone(), j.artists.clone()),
        groups: first_nonempty(c.publishers.clone(), j.groups.clone()),
        parodies: first_nonempty(xml_parodies, json_parodies),
        characters: j.characters.clone(),
        languages: first_nonempty(c.language.iter().cloned().collect(), j.languages.clone()),
        categories: first_nonempty(c.genres.clone(), j.categories.clone()),
        attributes: dedupe(attributes),
        year_month,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::archive::meta::parse_comic_info;

    /// ユーザー提供のサンプル（ComicInfo.xml）。
    const XML: &str = "<?xml version=\"1.0\"?><ComicInfo><Title>[Kirome] Elf Mama-San no manga</Title><Series>original</Series>\
        <Writer>kirome</Writer><Tags>very long hair, bald, elf, dwarf, closed eyes</Tags><Year>2023</Year><Month>12</Month>\
        <LanguageISO>ja</LanguageISO><PageCount>26</PageCount><Web>https://example.invalid/</Web></ComicInfo>";

    /// ユーザー提供のサンプル（meta.json）。
    const JSON: &str = r#"{"id":487084,"title":{"english":"[Kayumidome] Valerie Monogatari 3 〜Suki nishi te ii no yo〜","japanese":"[かゆみ止め] ヴァレリー物語(3) 〜好きにしていいのよ♡〜"},"upload_date":1703286936,"num_pages":29,"num_favorites":4770,"scanlator":"","tags":[{"id":6346,"type":"language","name":"japanese"},{"id":33173,"type":"category","name":"manga"},{"id":8010,"type":"tag","name":"group"},{"id":32341,"type":"tag","name":"shota"},{"id":21572,"type":"tag","name":"multi-work series"},{"id":89344,"type":"artist","name":"kayumidome"}]}"#;

    fn comic(xml: &str) -> ComicMeta {
        ComicMeta::from_comic_info(&parse_comic_info(xml.as_bytes()).unwrap())
    }

    #[test]
    fn comic_info_sample_is_read_into_typed_fields() {
        let c = comic(XML);
        assert_eq!(c.title.as_deref(), Some("[Kirome] Elf Mama-San no manga"));
        assert_eq!(c.series.as_deref(), Some("original"));
        assert_eq!(c.writers, vec!["kirome"]);
        assert_eq!(c.tags, vec!["very long hair", "bald", "elf", "dwarf", "closed eyes"]);
        assert_eq!((c.year, c.month, c.language.as_deref()), (Some(2023), Some(12), Some("ja")));
    }

    #[test]
    fn comic_info_splits_multiple_writers_publishers_and_ignores_bad_dates() {
        let c = comic("<ComicInfo><Writer>A, B x C</Writer><Publisher>Hawk Bit</Publisher><Imprint>Hawk Bit</Imprint><Genre>Manga, Doujinshi</Genre><Year>20x3</Year><Month>13</Month></ComicInfo>");
        assert_eq!(c.writers, vec!["A", "B", "C"]);
        assert_eq!(c.publishers, vec!["Hawk Bit"], "重複は除く");
        assert_eq!(c.genres, vec!["Manga", "Doujinshi"]);
        assert_eq!((c.year, c.month), (None, None), "不正な年・月は取らない");
    }

    #[test]
    fn meta_json_sample_is_read_with_typed_tags() {
        let j = JsonMeta::parse(JSON).unwrap();
        assert_eq!(j.title_japanese.as_deref(), Some("[かゆみ止め] ヴァレリー物語(3) 〜好きにしていいのよ♡〜"));
        assert_eq!(j.title_english.as_deref(), Some("[Kayumidome] Valerie Monogatari 3 〜Suki nishi te ii no yo〜"));
        assert_eq!(j.languages, vec!["japanese"]);
        assert_eq!(j.categories, vec!["manga"]);
        assert_eq!(j.artists, vec!["kayumidome"]);
        assert!(j.groups.is_empty(), "type: tag の name が group でも、種類の group ではない");
        assert_eq!(j.tags, vec!["group", "shota", "multi-work series"]);
    }

    #[test]
    fn meta_json_handles_other_types_bom_and_bad_input() {
        let j = JsonMeta::parse("\u{feff}{\"tags\":[{\"type\":\"group\",\"name\":\"Hawk Bit\"},{\"type\":\"PARODY\",\"name\":\"Touhou\"},{\"type\":\"character\",\"name\":\"Reimu\"},{\"type\":\"mystery\",\"name\":\"x\"},{\"type\":\"tag\",\"name\":\"  \"}]}").unwrap();
        assert_eq!((j.groups, j.parodies, j.characters), (vec!["Hawk Bit".to_string()], vec!["Touhou".to_string()], vec!["Reimu".to_string()]));
        assert!(j.tags.is_empty(), "空の名前と未知の種類は読み飛ばす");
        assert!(JsonMeta::parse("{broken").is_none());
        assert!(JsonMeta::parse("{}").is_none(), "拾える項目が無い");
        assert!(JsonMeta::parse("[1,2]").is_none());
        assert!(JsonMeta::parse(r#"{"title":{"english":"  "},"tags":[]}"#).is_none());
    }

    #[test]
    fn from_sources_needs_at_least_one_usable_source() {
        assert!(ArchiveMeta::from_sources(None, None).is_none());
        assert!(ArchiveMeta::from_sources(None, Some("{broken")).is_none());
        let m = ArchiveMeta::from_sources(parse_comic_info(XML.as_bytes()).as_ref(), None).unwrap();
        assert!(m.has_xml() && !m.has_json());
    }

    #[test]
    fn resolve_prefers_xml_and_fills_empty_fields_from_json() {
        let both = ArchiveMeta::from_sources(parse_comic_info(XML.as_bytes()).as_ref(), Some(JSON)).unwrap();
        let r = resolve(&both);
        assert_eq!(r.authors, vec!["kirome"], "XML の Writer が優先（JSON の artist は使わない）");
        assert_eq!(r.languages, vec!["ja"]);
        assert_eq!(r.categories, vec!["manga"], "XML に Genre が無いので JSON の category で埋める");
        assert!(r.parodies.is_empty(), "Series: original は元作品ではない");
        assert_eq!(r.attributes, vec!["very long hair", "bald", "elf", "dwarf", "closed eyes", "group", "shota", "multi-work series"], "タグは和集合");
        assert_eq!(r.year_month, Some(YearMonth { year: 2023, month: Some(12) }));
        assert_eq!(
            r.titles,
            vec![
                "[Kirome] Elf Mama-San no manga",
                "[かゆみ止め] ヴァレリー物語(3) 〜好きにしていいのよ♡〜",
                "[Kayumidome] Valerie Monogatari 3 〜Suki nishi te ii no yo〜",
            ]
        );
    }

    #[test]
    fn resolve_from_json_only_uses_typed_tags() {
        let m = ArchiveMeta::from_sources(None, Some(JSON)).unwrap();
        let r = resolve(&m);
        assert_eq!((r.authors, r.languages, r.categories), (vec!["kayumidome".to_string()], vec!["japanese".to_string()], vec!["manga".to_string()]));
        assert_eq!(r.year_month, None, "JSON の upload_date はアップロード日なので、年月にしない");
        assert_eq!(r.attributes, vec!["group", "shota", "multi-work series"]);
    }

    #[test]
    fn series_original_is_excluded_but_a_real_series_is_a_parody() {
        for s in ["original", "Original", "オリジナル", "ORIGINAL"] {
            assert!(is_original_series(s), "{s}");
        }
        assert!(!is_original_series("Honkai-Star Rail"));
        let m = ArchiveMeta::from_sources(parse_comic_info(b"<ComicInfo><Series>Touhou Project</Series></ComicInfo>").as_ref(), None).unwrap();
        assert_eq!(resolve(&m).parodies, vec!["Touhou Project"]);
        // XML が original なら、JSON の parody で埋める。
        let m = ArchiveMeta::from_sources(
            parse_comic_info(b"<ComicInfo><Series>original</Series></ComicInfo>").as_ref(),
            Some(r#"{"tags":[{"type":"parody","name":"Arknights"},{"type":"parody","name":"original"}]}"#),
        )
        .unwrap();
        assert_eq!(resolve(&m).parodies, vec!["Arknights"]);
    }

    #[test]
    fn year_month_names_sort_as_strings_and_year_alone_is_month_zero() {
        let names: Vec<String> = [
            YearMonth { year: 2023, month: Some(12) },
            YearMonth { year: 2023, month: None },
            YearMonth { year: 2023, month: Some(2) },
            YearMonth { year: 2022, month: Some(12) },
        ]
        .iter()
        .map(YearMonth::element_name)
        .collect();
        assert_eq!(names, vec!["2023-12", "2023", "2023-02", "2022-12"]);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["2022-12", "2023", "2023-02", "2023-12"], "年だけは、その年の先頭に並ぶ（月0扱い）");
    }

    #[test]
    fn year_without_month_and_month_without_year() {
        let r = resolve(&ArchiveMeta::from_sources(parse_comic_info(b"<ComicInfo><Year>2020</Year></ComicInfo>").as_ref(), None).unwrap());
        assert_eq!(r.year_month.map(|y| y.element_name()), Some("2020".to_string()));
        let r = resolve(&ArchiveMeta::from_sources(parse_comic_info(b"<ComicInfo><Month>5</Month><Title>T</Title></ComicInfo>").as_ref(), None).unwrap());
        assert_eq!(r.year_month, None, "年が無く月だけは取らない");
    }
}
