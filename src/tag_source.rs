//! 一括タグ付けの「取得元」の解析（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! - `parse_info_txt`: アーカイブ内の info.txt から、タイトル・カテゴリ・言語を取り出す。
//!   `Tags:` 節は解析しない（仕様）。他の項目（Uploader・Posted など）も読み飛ばす
//! - `parse_name_parts`: ファイル名（拡張子を除いた部分）を、先頭の `(イベント)`、先頭の `[グループ(作者)]`、
//!   タイトル、末尾の `[...]`（言語など）に分ける
//! - `canonical_language`: 言語名の辞書引き。info.txt の `Language:` とファイル名末尾の `[言語]` で共用する
//! - `split_archive_extension` / `format_tag_for`: ファイル名から、アーカイブ形式タグの元になる拡張子を決める
//!
//! 作者かグループかの判定（既存の確定タグとの照合）は、ここでは行わない。`[A(B)]` は `Pair` のまま返し、
//! 解釈は候補生成の側が既存マスターを見て決める。

use crate::tag_master::normalize_key;

// ── info.txt ──────────────────────────────────────────────────────────

/// info.txt から取り出した項目。無い項目は None。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InfoTxt {
    /// 先頭の見出し行（タイトル）。ファイル名と同じ形式なら `parse_name_parts` に渡せる。
    pub title: Option<String>,
    /// `Category:`（Doujinshi / Manga など）。メインタグの元。
    pub category: Option<String>,
    /// `Language:` の値そのまま（`Japanese` / `Chinese  TR` など）。辞書引きは `info_language` を使う。
    pub language: Option<String>,
}

impl InfoTxt {
    /// `Language:` の値を、辞書の正規名にする。`Chinese  TR` のように末尾に注記があっても、最初の語で引く。
    pub(crate) fn info_language(&self) -> Option<&'static str> {
        let raw = self.language.as_deref()?;
        canonical_language(raw).or_else(|| raw.split_whitespace().next().and_then(canonical_language))
    }
}

/// 行頭のキーとして認める項目名（小文字）。これ以外の `名前: 値` の行は、見出し行にも項目にもしない。
const INFO_KEYS: [&str; 10] =
    ["category", "uploader", "posted", "parent", "visible", "language", "file size", "length", "favorited", "rating"];

/// `キー: 値` の行を（小文字のキー, 値）に分ける。既知のキーだけ。
fn split_info_line(line: &str) -> Option<(String, &str)> {
    let (key, value) = line.split_once(':')?;
    let key = key.trim().to_lowercase();
    INFO_KEYS.contains(&key.as_str()).then(|| (key, value.trim()))
}

/// 値が `<...>` のプレースホルダ（`<download url>` など）か。
fn is_placeholder(line: &str) -> bool {
    let l = line.trim();
    l.starts_with('<') && l.ends_with('>')
}

/// info.txt の本文を解析する。本文は `meta::decode_text_bytes` で安全化済みであること。
/// 見出し行（タイトル）は、最初の空でない行のうち、既知のキー行でもプレースホルダでもないもの。
pub(crate) fn parse_info_txt(text: &str) -> InfoTxt {
    let mut out = InfoTxt::default();
    let mut seen_first_content = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = split_info_line(line) {
            seen_first_content = true;
            if value.is_empty() {
                continue;
            }
            match key.as_str() {
                "category" if out.category.is_none() => out.category = Some(collapse_spaces(value)),
                "language" if out.language.is_none() => out.language = Some(collapse_spaces(value)),
                _ => {}
            }
            continue;
        }
        if !seen_first_content && out.title.is_none() && !is_placeholder(line) {
            out.title = Some(collapse_spaces(line));
        }
        seen_first_content = true;
    }
    out
}

/// 連続する空白（全角空白を含む）を1つの半角空白にして、前後を落とす。
pub(crate) fn collapse_spaces(s: &str) -> String {
    s.split(|c: char| c.is_whitespace()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

// ── 言語 ──────────────────────────────────────────────────────────────

/// 言語の正規名と別名。別名は `normalize_key` で比べる（大文字小文字・空白・記号の違いを吸収）。
/// 2文字の略号は、混同しやすいもの（id・it・es・de・pt・th・vi・pl・ru・fr）は載せない。
const LANGUAGES: &[(&str, &[&str])] = &[
    ("日本語", &["日本語", "日本語版", "日本", "日語", "日文", "japanese", "jp", "jpn", "ja"]),
    ("英語", &["英語", "英語版", "英訳", "英文", "english", "english translated", "en", "eng"]),
    (
        "中国語",
        &[
            "中国語", "中国語版", "中国語翻訳", "中国翻訳", "中国", "中文", "汉化", "漢化", "简体中文", "簡体中文", "繁體中文",
            "繁体中文", "chinese", "chinese translated", "zh", "chn",
        ],
    ),
    ("韓国語", &["韓国語", "韓国語版", "韓国語翻訳", "韓国翻訳", "韓国", "한국어", "韩语", "korean", "ko", "kor"]),
    ("フランス語", &["フランス語", "仏語", "french"]),
    ("ドイツ語", &["ドイツ語", "独語", "german"]),
    ("スペイン語", &["スペイン語", "西語", "spanish"]),
    ("イタリア語", &["イタリア語", "伊語", "italian"]),
    ("ロシア語", &["ロシア語", "露語", "russian"]),
    ("ポルトガル語", &["ポルトガル語", "portuguese"]),
    ("タイ語", &["タイ語", "thai"]),
    ("ベトナム語", &["ベトナム語", "vietnamese"]),
    ("インドネシア語", &["インドネシア語", "indonesian"]),
    ("ポーランド語", &["ポーランド語", "polish"]),
];

/// 言語名（または別名）を、辞書の正規名にする。辞書に無ければ None（`[DL版]` などを言語にしないため）。
pub(crate) fn canonical_language(raw: &str) -> Option<&'static str> {
    let key = normalize_key(raw);
    if key.is_empty() {
        return None;
    }
    LANGUAGES
        .iter()
        .find(|(_, aliases)| aliases.iter().any(|a| normalize_key(a) == key))
        .map(|(canon, _)| *canon)
}

/// 言語の正規名（`canonical_language` の戻り値）に対応する英語名。登録名を英語にする場合に使う。
pub(crate) fn language_english_name(canonical: &str) -> &'static str {
    match canonical {
        "日本語" => "Japanese",
        "英語" => "English",
        "中国語" => "Chinese",
        "韓国語" => "Korean",
        "フランス語" => "French",
        "ドイツ語" => "German",
        "スペイン語" => "Spanish",
        "イタリア語" => "Italian",
        "ロシア語" => "Russian",
        "ポルトガル語" => "Portuguese",
        "タイ語" => "Thai",
        "ベトナム語" => "Vietnamese",
        "インドネシア語" => "Indonesian",
        "ポーランド語" => "Polish",
        _ => "Other",
    }
}

// ── ファイル名 ────────────────────────────────────────────────────────

/// 先頭の `[...]` の中身。括弧書きが付いていれば `Pair`（外側, 内側）、無ければ `Single`。
/// 外側と内側のどちらが作者でどちらがグループかは、ここでは決めない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Creator {
    Single(String),
    Pair { outer: String, inner: String },
}

/// ファイル名（拡張子を除く）の分解結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NameParts {
    /// 先頭の `(C105)` の中身。
    pub event: Option<String>,
    /// 先頭の `[グループ(作者)]`。
    pub creator: Option<Creator>,
    pub title: String,
    /// 末尾の `[...]` の中身（出現順）。言語・`DL版` など。
    pub trailing: Vec<String>,
}

impl NameParts {
    /// 末尾の `[...]` のうち、言語辞書に合うもの（複数あれば最後のもの）の正規名。
    pub(crate) fn language(&self) -> Option<&'static str> {
        self.trailing.iter().rev().find_map(|t| canonical_language(t))
    }
}

pub(crate) fn is_open_round(c: char) -> bool {
    c == '(' || c == '（'
}
pub(crate) fn is_close_round(c: char) -> bool {
    c == ')' || c == '）'
}
pub(crate) fn is_open_square(c: char) -> bool {
    c == '[' || c == '［'
}
pub(crate) fn is_close_square(c: char) -> bool {
    c == ']' || c == '］'
}

/// 先頭の `open` で始まる括弧の、対応する閉じ括弧までの中身と、その後ろの残りを返す（入れ子対応）。
pub(crate) fn take_leading_group(s: &str, is_open: fn(char) -> bool, is_close: fn(char) -> bool) -> Option<(&str, &str)> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !is_open(first) {
        return None;
    }
    let start = first.len_utf8();
    let mut depth = 1usize;
    for (i, c) in chars {
        if is_open(c) {
            depth += 1;
        } else if is_close(c) {
            depth -= 1;
            if depth == 0 {
                return Some((&s[start..i], &s[i + c.len_utf8()..]));
            }
        }
    }
    None
}

/// 末尾の閉じ括弧で終わる括弧の、対応する開き括弧より前の部分と、その中身を返す（入れ子対応）。
pub(crate) fn take_trailing_group(s: &str, is_open: fn(char) -> bool, is_close: fn(char) -> bool) -> Option<(&str, &str)> {
    let last = s.chars().next_back()?;
    if !is_close(last) {
        return None;
    }
    let end = s.len() - last.len_utf8();
    let mut depth = 1usize;
    for (i, c) in s[..end].char_indices().rev() {
        if is_close(c) {
            depth += 1;
        } else if is_open(c) {
            depth -= 1;
            if depth == 0 {
                return Some((&s[..i], &s[i + c.len_utf8()..end]));
            }
        }
    }
    None
}

/// `[...]` の中身を作者・グループ候補にする。末尾が括弧書きなら `Pair`。
fn parse_creator(inner: &str) -> Option<Creator> {
    let inner = collapse_spaces(inner);
    if inner.is_empty() {
        return None;
    }
    if let Some((before, inside)) = take_trailing_group(&inner, is_open_round, is_close_round) {
        let outer = before.trim();
        let paren = inside.trim();
        return Some(match (outer.is_empty(), paren.is_empty()) {
            (false, false) => Creator::Pair { outer: outer.to_string(), inner: paren.to_string() },
            (false, true) => Creator::Single(outer.to_string()),
            (true, false) => Creator::Single(paren.to_string()),
            (true, true) => return None,
        });
    }
    Some(Creator::Single(inner))
}

/// ファイル名（拡張子を除く）を分解する。
/// 形: `(イベント) [グループ(作者)] タイトル [言語][DL版]`。どの部分も省略できる。
pub(crate) fn parse_name_parts(stem: &str) -> NameParts {
    let mut out = NameParts::default();
    let mut rest = stem.trim();

    if let Some((event, after)) = take_leading_group(rest, is_open_round, is_close_round) {
        let event = collapse_spaces(event);
        if !event.is_empty() && event.chars().count() <= 40 {
            out.event = Some(event);
            rest = after.trim_start();
        }
    }
    if let Some((inner, after)) = take_leading_group(rest, is_open_square, is_close_square) {
        if let Some(creator) = parse_creator(inner) {
            out.creator = Some(creator);
            rest = after.trim_start();
        }
    }
    // 末尾の `[...]` を、後ろから順に剥がす。
    let mut trailing: Vec<String> = Vec::new();
    loop {
        let trimmed = rest.trim_end();
        match take_trailing_group(trimmed, is_open_square, is_close_square) {
            Some((before, inside)) => {
                let inside = collapse_spaces(inside);
                if !inside.is_empty() {
                    trailing.push(inside);
                }
                rest = before;
            }
            None => {
                rest = trimmed;
                break;
            }
        }
    }
    trailing.reverse();
    out.trailing = trailing;
    out.title = collapse_spaces(rest);
    out
}

// ── 拡張子・アーカイブ形式 ────────────────────────────────────────────

/// 複合拡張子（先に判定する）。
const COMPOUND_EXTENSIONS: [&str; 2] = ["tar.gz", "tar.zst"];

/// ファイル名を（拡張子を除いた名前, 小文字の拡張子）に分ける。拡張子が無ければ拡張子は None。
/// `tar.gz` のような複合拡張子を先に見る。拡張子は英数字だけで8文字以下のものに限る
/// （`Vol.1 title` のようなドット入りの名前を拡張子と誤認しない）。
pub(crate) fn split_archive_extension(file_name: &str) -> (&str, Option<String>) {
    let lower = file_name.to_lowercase();
    for ext in COMPOUND_EXTENSIONS {
        let suffix = format!(".{ext}");
        if lower.len() > suffix.len() && lower.ends_with(&suffix) {
            // 小文字化で長さが変わる文字を含む名前でも、末尾 ASCII の長さぶんだけ切る。
            let cut = file_name.len() - suffix.len();
            if file_name.is_char_boundary(cut) {
                return (&file_name[..cut], Some(ext.to_string()));
            }
        }
    }
    match file_name.rfind('.') {
        Some(dot) if dot > 0 => {
            let ext = &file_name[dot + 1..];
            if !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                (&file_name[..dot], Some(ext.to_ascii_lowercase()))
            } else {
                (file_name, None)
            }
        }
        _ => (file_name, None),
    }
}

/// アーカイブ形式タグの元。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FormatTag {
    /// フォルダ本。
    Folder,
    /// 小文字の拡張子（`zip` / `cbz` / `tar.gz` …）。
    Extension(String),
}

/// ファイル名とフォルダかどうかから、形式タグを決める。拡張子の無いファイルは None。
/// 中身（マジックバイト）ではなく、見えているファイル名で決める。
pub(crate) fn format_tag_for(file_name: &str, is_dir: bool) -> Option<FormatTag> {
    if is_dir {
        return Some(FormatTag::Folder);
    }
    split_archive_extension(file_name).1.map(FormatTag::Extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "(C105) [Hawk Bit (Kouji)] Takahama-kun  wa DoM\n<download url>\n\nCategory: Doujinshi\nUploader: bbay1201  \nPosted: 2026-03-15 19:05\nParent: None\nVisible: Yes\nLanguage: Japanese  \nFile Size: 290.2 MiB\nLength: 74 pages\nFavorited: 522 times\nRating: 4.29\n";

    #[test]
    fn info_txt_sample_is_parsed() {
        let info = parse_info_txt(SAMPLE);
        assert_eq!(info.title.as_deref(), Some("(C105) [Hawk Bit (Kouji)] Takahama-kun wa DoM"));
        assert_eq!(info.category.as_deref(), Some("Doujinshi"));
        assert_eq!(info.language.as_deref(), Some("Japanese"));
        assert_eq!(info.info_language(), Some("日本語"));
    }

    #[test]
    fn info_txt_tolerates_missing_and_odd_input() {
        assert_eq!(parse_info_txt(""), InfoTxt::default());
        // 見出しが無く、キー行から始まる。
        let info = parse_info_txt("Category: Manga\nLanguage: Chinese  TR\n");
        assert_eq!(info.title, None);
        assert_eq!(info.category.as_deref(), Some("Manga"));
        assert_eq!(info.info_language(), Some("中国語"), "末尾の注記があっても最初の語で引く");
        // キーの大文字小文字、空の値、重複キー（最初を採る）、Tags節は読み飛ばす。
        let info = parse_info_txt("T\nCATEGORY:   \ncategory: A\nCategory: B\nTags:\n> female: x\nartist: someone\n");
        assert_eq!(info.title.as_deref(), Some("T"));
        assert_eq!(info.category.as_deref(), Some("A"));
        // 見出しは先頭の1行だけ。キー行より後の自由文は見出しにしない。
        let info = parse_info_txt("Category: Manga\nfree text line\n");
        assert_eq!(info.title, None);
        // CRLF・前後の空白。
        let info = parse_info_txt("  Title line  \r\nCategory: Doujinshi\r\n");
        assert_eq!((info.title.as_deref(), info.category.as_deref()), (Some("Title line"), Some("Doujinshi")));
    }

    #[test]
    fn language_dictionary_matches_aliases_and_rejects_others() {
        for (raw, want) in [
            ("日本語", "日本語"), ("Japanese", "日本語"), ("JP", "日本語"), ("english", "英語"), ("English Translated", "英語"),
            ("中国翻訳", "中国語"), ("中文", "中国語"), ("Chinese", "中国語"), ("汉化", "中国語"), ("한국어", "韓国語"),
            ("ＪＡＰＡＮＥＳＥ", "日本語"), ("french", "フランス語"),
        ] {
            assert_eq!(canonical_language(raw), Some(want), "{raw}");
        }
        for raw in ["DL版", "無修正", "Digital", "", "  ", "ID", "it", "R-18", "Vol.1"] {
            assert_eq!(canonical_language(raw), None, "{raw}");
        }
    }

    #[test]
    fn name_with_event_pair_creator_and_language() {
        let p = parse_name_parts("(C105) [Hawk Bit (Kouji)] Takahama-kun wa DoM [日本語]");
        assert_eq!(p.event.as_deref(), Some("C105"));
        assert_eq!(p.creator, Some(Creator::Pair { outer: "Hawk Bit".into(), inner: "Kouji".into() }));
        assert_eq!(p.title, "Takahama-kun wa DoM");
        assert_eq!(p.trailing, vec!["日本語"]);
        assert_eq!(p.language(), Some("日本語"));
    }

    #[test]
    fn name_with_single_creator_and_no_event() {
        let p = parse_name_parts("[Kouji] Some Title [English]");
        assert_eq!(p.event, None);
        assert_eq!(p.creator, Some(Creator::Single("Kouji".into())));
        assert_eq!(p.title, "Some Title");
        assert_eq!(p.language(), Some("英語"));
    }

    #[test]
    fn name_handles_fullwidth_brackets_and_nested_parens() {
        let p = parse_name_parts("（C104）［サークル（作者）］ タイトル（副題） ［中国翻訳］");
        assert_eq!(p.event.as_deref(), Some("C104"));
        assert_eq!(p.creator, Some(Creator::Pair { outer: "サークル".into(), inner: "作者".into() }));
        assert_eq!(p.title, "タイトル（副題）", "タイトル中の括弧は残す");
        assert_eq!(p.language(), Some("中国語"));
        // 入れ子: 括弧の中にさらに括弧。
        let p = parse_name_parts("[A (B (C))] T");
        assert_eq!(p.creator, Some(Creator::Pair { outer: "A".into(), inner: "B (C)".into() }));
    }

    #[test]
    fn name_trailing_brackets_keep_order_and_language_uses_dictionary_only() {
        let p = parse_name_parts("[X] Title [DL版] [日本語] [無修正]");
        assert_eq!(p.trailing, vec!["DL版", "日本語", "無修正"]);
        assert_eq!(p.language(), Some("日本語"), "辞書に合うものだけが言語");
        let p = parse_name_parts("[X] Title [DL版]");
        assert_eq!(p.language(), None);
        // 中間の括弧はタイトルに残る。
        let p = parse_name_parts("[X] Part [1] of Title [English]");
        assert_eq!(p.title, "Part [1] of Title");
    }

    #[test]
    fn name_edge_cases_do_not_panic_or_invent_parts() {
        assert_eq!(parse_name_parts(""), NameParts::default());
        let p = parse_name_parts("Plain title without brackets");
        assert_eq!((p.event, p.creator, p.title.as_str(), p.trailing.len()), (None, None, "Plain title without brackets", 0));
        // 閉じていない括弧は無視してタイトルのまま。
        let p = parse_name_parts("[unclosed title");
        assert_eq!((p.creator, p.title.as_str()), (None, "[unclosed title"));
        let p = parse_name_parts("(unclosed [A] title");
        assert_eq!(p.event, None);
        // 空の括弧・括弧だけの名前。
        let p = parse_name_parts("[] [ ] Title");
        assert_eq!(p.creator, None);
        let p = parse_name_parts("[Only]");
        assert_eq!((p.creator, p.title.as_str()), (Some(Creator::Single("Only".into())), ""));
        // `[A()]` `[(B)]` は、空の側を捨てて単独にする。
        assert_eq!(parse_name_parts("[A()] t").creator, Some(Creator::Single("A".into())));
        assert_eq!(parse_name_parts("[(B)] t").creator, Some(Creator::Single("B".into())));
        // 長すぎる先頭括弧はイベントにしない。
        let long = format!("({}) [A] t", "x".repeat(60));
        assert_eq!(parse_name_parts(&long).event, None);
        // 絵文字・結合文字が混ざっても落ちない。
        let _ = parse_name_parts("[😀 (e\u{0301})] 😀 t [😀]");
    }

    #[test]
    fn extension_split_handles_compound_and_dotted_names() {
        assert_eq!(split_archive_extension("a.zip"), ("a", Some("zip".into())));
        assert_eq!(split_archive_extension("A.CBZ"), ("A", Some("cbz".into())));
        assert_eq!(split_archive_extension("x.tar.gz"), ("x", Some("tar.gz".into())));
        assert_eq!(split_archive_extension("X.TAR.ZST"), ("X", Some("tar.zst".into())));
        assert_eq!(split_archive_extension("x.tgz"), ("x", Some("tgz".into())));
        assert_eq!(split_archive_extension("Vol.1 title"), ("Vol.1 title", None), "空白入りは拡張子にしない");
        assert_eq!(split_archive_extension("noext"), ("noext", None));
        assert_eq!(split_archive_extension(".hidden"), (".hidden", None), "先頭のドットだけ");
        assert_eq!(split_archive_extension("日本語.cbz"), ("日本語", Some("cbz".into())));
        assert_eq!(split_archive_extension("trail."), ("trail.", None));
        assert_eq!(split_archive_extension(".tar.gz"), (".tar", Some("gz".into())), "複合拡張子だけの名前は単純な拡張子に落とす");
    }

    #[test]
    fn format_tag_prefers_folder_and_uses_visible_extension() {
        assert_eq!(format_tag_for("book", true), Some(FormatTag::Folder));
        assert_eq!(format_tag_for("book.zip", true), Some(FormatTag::Folder), "フォルダは名前に関わらずフォルダ");
        assert_eq!(format_tag_for("a.CBZ", false), Some(FormatTag::Extension("cbz".into())));
        assert_eq!(format_tag_for("a.rar", false), Some(FormatTag::Extension("rar".into())), "未知の拡張子も候補にする");
        assert_eq!(format_tag_for("noext", false), None);
    }
}
