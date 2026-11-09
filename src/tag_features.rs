//! ファイル名の括弧を「トークン」に分け、学習用の特徴量を作る（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 学習（`tag_learning`）とモデル（P5）の土台。判定の対象は、ファイル名中の括弧 `(...)` `[...]` の中身1つ1つ。
//! 各トークンに「作者／グループ／イベント／言語／無視」のどれかを当てるのが学習の目的で、名前そのものは覚えない
//! （特徴量は括弧の種類・位置・文字種・長さ・形・辞書語のみ。名前の文字列は特徴量に入れない）。
//!
//! - トークンは、先頭側（`Lead`）・末尾側（`Trail`）・題名の途中（`Mid`）の括弧すべて。`parse_name_parts` が拾えない
//!   `[C105] [Kouji]` のような並びや、題名の後ろの `[作者]` も対象にする
//! - 角括弧の中身が `A (B)` の形なら、外側 `A` と内側 `B` の2トークン（`Nesting::Outer`/`Inner`）。互いを相手（`partner`）として持つ
//! - 特徴量は文字列の集合（スパース）。数値の重みづけはモデル側（P5）で行う

// 学習（P5以降）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use crate::tag_source::{
    canonical_language, collapse_spaces, is_close_round, is_close_square, is_open_round, is_open_square, split_archive_extension,
    take_leading_group, take_trailing_group,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bracket {
    Round,
    Square,
}

/// トークンの位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// 題名より前の括弧。先頭からの番号（0始まり）。
    Lead(usize),
    /// 題名より後ろの括弧。末尾からの番号（0が最後）。
    Trail(usize),
    /// 題名の途中の括弧。
    Mid,
}

/// 括弧の中身の構造。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nesting {
    /// 中身がそのまま1つのトークン。
    Whole,
    /// `A (B)` の `A`。
    Outer,
    /// `A (B)` の `B`。
    Inner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub text: String,
    pub bracket: Bracket,
    pub slot: Slot,
    pub nesting: Nesting,
    /// `Outer`/`Inner` の相手の文字列。
    pub partner: Option<String>,
    /// 先頭側の括弧の数（トークンではなく括弧の数）。
    pub lead_count: usize,
    /// 末尾側の括弧の数。
    pub trail_count: usize,
    /// 題名（括弧の外の文字）があるか。
    pub has_title: bool,
}

enum Seg<'a> {
    Group(Bracket, &'a str),
    /// 括弧の外の文字。空白以外を含むか。
    Text(bool),
}

/// 括弧の外側の文字と括弧を、出現順に分ける（括弧は入れ子対応。閉じていない開き括弧は文字として扱う）。
fn segments(stem: &str) -> Vec<Seg<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < stem.len() {
        let rest = &stem[i..];
        let c = rest.chars().next().unwrap();
        let group = if is_open_round(c) {
            take_leading_group(rest, is_open_round, is_close_round).map(|g| (Bracket::Round, g))
        } else if is_open_square(c) {
            take_leading_group(rest, is_open_square, is_close_square).map(|g| (Bracket::Square, g))
        } else {
            None
        };
        match group {
            Some((b, (content, after))) => {
                out.push(Seg::Group(b, content));
                i = stem.len() - after.len();
            }
            None => {
                let non_ws = !c.is_whitespace();
                match out.last_mut() {
                    Some(Seg::Text(flag)) => *flag |= non_ws,
                    _ => out.push(Seg::Text(non_ws)),
                }
                i += c.len_utf8();
            }
        }
    }
    out
}

/// ファイル名（拡張子つき）から、括弧のトークンを出現順に取り出す。
pub(crate) fn tokenize(file_name: &str) -> Vec<Token> {
    let (stem, _) = split_archive_extension(file_name);
    let segs = segments(stem);
    let first_text = segs.iter().position(|s| matches!(s, Seg::Text(true)));
    let last_text = segs.iter().rposition(|s| matches!(s, Seg::Text(true)));
    let has_title = first_text.is_some();

    // 括弧ごとの位置を決める（題名が無ければ、すべて先頭側）。
    let group_idx: Vec<usize> = segs.iter().enumerate().filter(|(_, s)| matches!(s, Seg::Group(..))).map(|(i, _)| i).collect();
    let is_lead = |i: usize| first_text.map_or(true, |f| i < f);
    let is_trail = |i: usize| last_text.is_some_and(|l| i > l);
    let lead_count = group_idx.iter().filter(|&&i| is_lead(i)).count();
    let trail_count = group_idx.iter().filter(|&&i| !is_lead(i) && is_trail(i)).count();

    let mut out = Vec::new();
    let (mut lead_n, mut trail_n) = (0usize, trail_count);
    for (i, seg) in segs.iter().enumerate() {
        let Seg::Group(bracket, content) = seg else { continue };
        let slot = if is_lead(i) {
            lead_n += 1;
            Slot::Lead(lead_n - 1)
        } else if is_trail(i) {
            trail_n -= 1;
            Slot::Trail(trail_n)
        } else {
            Slot::Mid
        };
        let content = collapse_spaces(content);
        if content.is_empty() {
            continue;
        }
        let mk = |text: String, nesting: Nesting, partner: Option<String>| Token {
            text,
            bracket: *bracket,
            slot,
            nesting,
            partner,
            lead_count,
            trail_count,
            has_title,
        };
        // 角括弧の中身が `A (B)` なら、外側と内側に分ける。
        if *bracket == Bracket::Square {
            if let Some((before, inside)) = take_trailing_group(&content, is_open_round, is_close_round) {
                let (outer, inner) = (before.trim(), inside.trim());
                if !outer.is_empty() && !inner.is_empty() {
                    out.push(mk(outer.to_string(), Nesting::Outer, Some(inner.to_string())));
                    out.push(mk(inner.to_string(), Nesting::Inner, Some(outer.to_string())));
                    continue;
                }
                let only = if outer.is_empty() { inner } else { outer };
                if !only.is_empty() {
                    out.push(mk(only.to_string(), Nesting::Whole, None));
                    continue;
                }
            }
        }
        out.push(mk(content, Nesting::Whole, None));
    }
    out
}

// ── 特徴量 ────────────────────────────────────────────────────────────

/// イベント名らしい語。
const EVENT_WORDS: &[&str] =
    &["コミケ", "コミティア", "例大祭", "サンクリ", "オンリー", "スパーク", "ブルコミ", "comic1", "comitia", "comiket", "コミックマーケット", "sunshine", "サンシャイン"];
/// ジャンル・区分を表す語（イベントでも作者でもない括弧に出やすい）。
const GENRE_WORDS: &[&str] = &["成年", "一般", "同人誌", "同人cg", "同人音声", "18禁", "r18", "r-18", "画集", "雑誌", "単行本", "商業", "アンソロ"];
/// 配布形態・修正などの付記（言語でもない括弧）。
const RELEASE_WORDS: &[&str] =
    &["dl版", "dl", "無修正", "decensored", "uncensored", "digital", "デジタル", "修正", "高画質", "fixed", "raw", "scan", "スキャン", "完全版", "全頁"];

/// 括弧の中身が複数の名前（作者の併記）に見えるときの区切りで分ける。
pub(crate) fn split_multi(text: &str) -> Vec<String> {
    let normalized = text.replace(" x ", ",").replace(" X ", ",").replace(" × ", ",");
    normalized
        .split([',', '、', '&', '＆', '×', '／'])
        .map(|p| collapse_spaces(p))
        .filter(|p| !p.is_empty())
        .collect()
}

fn is_event_code(text: &str) -> bool {
    let t = text.trim();
    let mut chars = t.chars();
    matches!(chars.next(), Some('C' | 'c')) && t.len() >= 3 && t[1..].chars().all(|c| c.is_ascii_digit())
}

fn has_word(text: &str, words: &[&str]) -> bool {
    let lower = text.to_lowercase();
    words.iter().any(|w| lower.contains(w))
}

fn is_volume(text: &str) -> bool {
    let lower = text.to_lowercase();
    let rest = lower.strip_prefix("vol").or_else(|| lower.strip_prefix("v"));
    rest.is_some_and(|r| {
        let r = r.trim_start_matches(['.', ' ']);
        !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())
    }) || (lower.starts_with('第') && lower.chars().any(|c| c.is_ascii_digit()))
}

/// 文字種の集合（`latin` `kana` `halfkana` `kanji` `hangul` `digit` `fullwidth` `symbol`）。
fn scripts(text: &str) -> Vec<&'static str> {
    let mut set: Vec<&'static str> = Vec::new();
    let mut add = |s: &'static str| {
        if !set.contains(&s) {
            set.push(s);
        }
    };
    for c in text.chars() {
        match c {
            'a'..='z' | 'A'..='Z' => add("latin"),
            '0'..='9' => add("digit"),
            '\u{3040}'..='\u{30FF}' => add("kana"),
            '\u{FF66}'..='\u{FF9F}' => add("halfkana"),
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => add("kanji"),
            '\u{AC00}'..='\u{D7AF}' => add("hangul"),
            '\u{FF01}'..='\u{FF5E}' => add("fullwidth"),
            c if c.is_whitespace() => {}
            _ => add("symbol"),
        }
    }
    set
}

fn len_bucket(n: usize) -> &'static str {
    match n {
        0..=2 => "1-2",
        3..=5 => "3-5",
        6..=10 => "6-10",
        11..=20 => "11-20",
        _ => "21+",
    }
}

fn text_shape(prefix: &str, text: &str, out: &mut Vec<String>) {
    for s in scripts(text) {
        out.push(format!("{prefix}sc:{s}"));
    }
    out.push(format!("{prefix}len:{}", len_bucket(text.chars().count())));
    if text.contains(' ') || text.contains('\u{3000}') {
        out.push(format!("{prefix}space"));
    }
    if split_multi(text).len() > 1 {
        out.push(format!("{prefix}multi"));
    }
}

impl Token {
    /// 学習用の特徴量（文字列の集合）。名前そのものは含めない。
    pub(crate) fn features(&self) -> Vec<String> {
        let mut f: Vec<String> = vec!["bias".to_string()];
        let br = match self.bracket {
            Bracket::Round => "round",
            Bracket::Square => "square",
        };
        let slot = match self.slot {
            Slot::Lead(n) => format!("lead{}", n.min(2)),
            Slot::Trail(n) => format!("trail{}", n.min(2)),
            Slot::Mid => "mid".to_string(),
        };
        let nest = match self.nesting {
            Nesting::Whole => "whole",
            Nesting::Outer => "outer",
            Nesting::Inner => "inner",
        };
        f.push(format!("br:{br}"));
        f.push(format!("slot:{slot}"));
        f.push(format!("nest:{nest}"));
        f.push(format!("pos:{br}/{slot}/{nest}"));
        f.push(format!("nlead:{}", self.lead_count.min(3)));
        f.push(format!("ntrail:{}", self.trail_count.min(3)));
        f.push(format!("title:{}", if self.has_title { "yes" } else { "no" }));
        text_shape("", &self.text, &mut f);

        let t = self.text.as_str();
        if is_event_code(t) {
            f.push("pat:event_code".into());
        }
        if has_word(t, EVENT_WORDS) {
            f.push("kw:event".into());
        }
        if has_word(t, GENRE_WORDS) {
            f.push("kw:genre".into());
        }
        if has_word(t, RELEASE_WORDS) {
            f.push("kw:release".into());
        }
        if canonical_language(t).is_some() {
            f.push("dict:language".into());
        }
        if is_volume(t) {
            f.push("pat:volume".into());
        }
        if t.chars().any(|c| c.is_ascii_digit()) {
            f.push("has_digit".into());
        }
        if t.chars().all(|c| c.is_ascii_digit()) {
            f.push("only_digit".into());
        }
        let letters: Vec<char> = t.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        if !letters.is_empty() && letters.iter().all(|c| c.is_ascii_uppercase()) {
            f.push("caps".into());
        }

        // 相手（外側⇄内側）との比較。向き（サークルと作者の取り違え）の手がかり。
        if let Some(p) = &self.partner {
            f.push("pair".into());
            text_shape("p_", p, &mut f);
            let (a, b) = (t.chars().count(), p.chars().count());
            f.push(format!("lencmp:{}", if a < b { "shorter" } else if a > b { "longer" } else { "equal" }));
            let (own, other) = (scripts(t), scripts(p));
            f.push(format!("scripteq:{}", own == other));
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(name: &str) -> Vec<(String, Slot, Nesting)> {
        tokenize(name).into_iter().map(|t| (t.text, t.slot, t.nesting)).collect()
    }

    #[test]
    fn splits_standard_name_into_event_pair_and_language() {
        let v = texts("(C105) [Hawk Bit (Kouji)] Title [日本語].zip");
        assert_eq!(
            v,
            vec![
                ("C105".into(), Slot::Lead(0), Nesting::Whole),
                ("Hawk Bit".into(), Slot::Lead(1), Nesting::Outer),
                ("Kouji".into(), Slot::Lead(1), Nesting::Inner),
                ("日本語".into(), Slot::Trail(0), Nesting::Whole),
            ]
        );
    }

    #[test]
    fn handles_squares_that_parse_name_parts_cannot() {
        let v = texts("[C105] [Kouji] Event In Square.zip");
        assert_eq!(v[0].1, Slot::Lead(0));
        assert_eq!(v[1].1, Slot::Lead(1));
        let v = texts("Title With Trailing Author [Kouji].zip");
        assert_eq!(v, vec![("Kouji".into(), Slot::Trail(0), Nesting::Whole)]);
        let v = texts("Title (Sub Title) Rest.zip");
        assert_eq!(v, vec![("Sub Title".into(), Slot::Mid, Nesting::Whole)]);
    }

    #[test]
    fn trailing_groups_are_numbered_from_the_end() {
        let v = texts("Mixed Title [DL版] [English].zip");
        assert_eq!(v[0].1, Slot::Trail(1));
        assert_eq!(v[1].1, Slot::Trail(0));
        assert_eq!(tokenize("Mixed Title [DL版] [English].zip")[0].trail_count, 2);
    }

    #[test]
    fn fullwidth_brackets_and_no_brackets() {
        let v = texts("［Hawk Bit（Kouji）］全角括弧の本.zip");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "Hawk Bit");
        assert!(tokenize("Plain Title No Brackets.zip").is_empty());
        assert!(tokenize("Vol.01 Numbered Title.zip").is_empty());
    }

    #[test]
    fn name_made_only_of_brackets_has_no_title_and_all_lead() {
        let t = tokenize("[A (B)] (C1).zip");
        assert!(t.iter().all(|t| matches!(t.slot, Slot::Lead(_)) && !t.has_title));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn unclosed_bracket_is_plain_text() {
        assert!(tokenize("Title [broken.zip").is_empty());
        let v = texts("[Circle (Writer] Book.zip");
        assert_eq!(v, vec![("Circle (Writer".into(), Slot::Lead(0), Nesting::Whole)]);
    }

    #[test]
    fn pair_tokens_know_each_other() {
        let t = tokenize("[Circle Name (Writer)] Book.zip");
        assert_eq!(t[0].partner.as_deref(), Some("Writer"));
        assert_eq!(t[1].partner.as_deref(), Some("Circle Name"));
        let f = t[0].features();
        assert!(f.contains(&"pair".to_string()) && f.contains(&"lencmp:longer".to_string()));
    }

    #[test]
    fn features_capture_shape_without_the_name() {
        let t = &tokenize("(C105) [Kouji] T.zip")[0];
        let f = t.features();
        assert!(f.contains(&"pat:event_code".to_string()));
        assert!(f.contains(&"pos:round/lead0/whole".to_string()));
        assert!(f.iter().all(|x| !x.contains("C105")), "名前は特徴量に入れない: {f:?}");
        let genre = &tokenize("(成年コミック) [Kouji] T.zip")[0];
        assert!(genre.features().contains(&"kw:genre".to_string()));
        assert!(!genre.features().contains(&"kw:event".to_string()));
        let lang = &tokenize("T [English].zip")[0];
        assert!(lang.features().contains(&"dict:language".to_string()));
        let rel = &tokenize("T [DL版].zip")[0];
        assert!(rel.features().contains(&"kw:release".to_string()));
    }

    #[test]
    fn split_multi_handles_common_separators() {
        assert_eq!(split_multi("Kouji, Tsuki Mori"), vec!["Kouji", "Tsuki Mori"]);
        assert_eq!(split_multi("Umi Kaze x Aka Kitsune"), vec!["Umi Kaze", "Aka Kitsune"]);
        assert_eq!(split_multi("A・B"), vec!["A・B"], "中黒は名前の一部として分けない");
    }
}
