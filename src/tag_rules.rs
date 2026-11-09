//! タグ自動取得の「大ルール」（穴埋め式スクリプト）の型・保存・評価（画面・DBに触れない純関数）。
//!
//! 抽出の前の下処理。ファイルごとに条件を判定し、成立したルールの効果（種類ごとの無効／有効）を集める。
//! 例: 条件「題名に AI generated を含む」→ 作者・グループ・イベントを無効、パロディ元を有効。
//!
//! - **条件**: 対象（ファイル名／info.txt の見出し行／info.txt の Category／題名＝括弧を除く部分）の文字列に、
//!   「文字列内一致」「前方一致」「完全一致」するか。または、ファイル名の形（`X - Y` `X - Y (Z)` `X - Y - Z`）。
//!   文字列の照合は既存の完全一致キー（全角半角・空白・ハイフン・記号・大小を同一視）
//! - **効果**: 無効にする種類、有効にする種類。種類は抽出の6つ（形式・メイン・言語・イベント・作者・グループ）と、
//!   新種類（演者・キャラ・パロディ元）。新種類は抽出が既定では取らないので、有効にしたルールがあるときだけ取る
//! - **競合**: 成立したルールをすべて適用する。無効は和集合。有効は、無効の側に入っていないものだけが残る
//! - ルールは設定フォルダの `nekoviewer_tag_rules.json` に順序つきで保存する（順序は表示と理由の並びだけに効く）

// 抽出への接続（R2）・編集UI（R4）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tag_similarity::{exact_key, MatchOptions};
use crate::tag_source::{
    collapse_spaces, is_close_round, is_close_square, is_open_round, is_open_square, split_archive_extension, take_trailing_group,
    InfoTxt,
};

const FILE_NAME: &str = "nekoviewer_tag_rules.json";
const VERSION: u32 = 1;

// ── 型 ────────────────────────────────────────────────────────────────

/// ルールで指定する種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuleRole {
    Format,
    Main,
    Language,
    Event,
    Author,
    Group,
    /// 新種類: 演者（cosplay）。
    Performer,
    /// 新種類: キャラ（cosplay）。
    Character,
    /// 新種類: パロディ元。
    Parody,
    /// 新種類: 属性タグ（ComicInfo.xml の Tags・meta.json の tag）。「正の情報」があるときに取る。
    Attribute,
    /// 新種類: 年月（ComicInfo.xml の Year・Month）。「正の情報」があるときに取る。
    YearMonth,
}

impl RuleRole {
    /// 既存の6種類（抽出が常に取るもの）。
    pub(crate) const EXISTING: [RuleRole; 6] =
        [RuleRole::Format, RuleRole::Main, RuleRole::Language, RuleRole::Event, RuleRole::Author, RuleRole::Group];
    /// 新種類（有効にしたルールがあるときだけ取る）。
    pub(crate) const OPT_IN: [RuleRole; 3] = [RuleRole::Performer, RuleRole::Character, RuleRole::Parody];
    /// 「正の情報」（ComicInfo.xml・meta.json）があるときに取る種類（ルールで有効にしなくても取る。無効にはできる）。
    pub(crate) const FROM_AUTHORITY: [RuleRole; 2] = [RuleRole::Attribute, RuleRole::YearMonth];
    pub(crate) const ALL: [RuleRole; 11] = [
        RuleRole::Format,
        RuleRole::Main,
        RuleRole::Language,
        RuleRole::Event,
        RuleRole::Author,
        RuleRole::Group,
        RuleRole::Performer,
        RuleRole::Character,
        RuleRole::Parody,
        RuleRole::Attribute,
        RuleRole::YearMonth,
    ];

    pub(crate) fn is_opt_in(self) -> bool {
        Self::OPT_IN.contains(&self)
    }
}

/// 条件が見る文字列。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CondTarget {
    /// 拡張子を除いたファイル名。
    FileName,
    /// info.txt の見出し行（複数行なら、どの行でも）。
    InfoTitle,
    /// info.txt の `Category:` の値。
    InfoCategory,
    /// 題名: ファイル名・見出し行から、括弧 `(...)` `[...]` の部分を除いたもの。
    Title,
}

/// 文字列の照合の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TextMatch {
    /// 文字列内一致。
    Contains,
    /// 前方一致。
    Prefix,
    /// 完全一致。
    Exact,
}

/// ファイル名の形（区切りは、前後に半角スペースのある ` - `）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NameShape {
    /// `X - Y`
    Dash2,
    /// `X - Y (Z)`
    Dash2Paren,
    /// `X - Y - Z`
    Dash3,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Condition {
    /// 対象のどれかの文字列が、`text` に一致する。
    Text { targets: Vec<CondTarget>, how: TextMatch, text: String },
    /// 対象のどれかの文字列が、ファイル名の形に合う。
    Shape { targets: Vec<CondTarget>, shape: NameShape },
}

/// ルールの効果。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Effect {
    /// 無効にする種類（その種類の言及は、候補にしないか、未採用・要確認にする）。
    #[serde(default)]
    pub disable: BTreeSet<RuleRole>,
    /// 有効にする種類（新種類は、有効にしたときだけ取る）。
    #[serde(default)]
    pub enable: BTreeSet<RuleRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TagRule {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub conditions: Vec<Condition>,
    /// true なら条件のすべてを満たしたとき、false ならどれかを満たしたとき成立。
    #[serde(default)]
    pub match_all: bool,
    pub effect: Effect,
}

impl TagRule {
    /// 保存・適用の前の検証。問題があれば理由を返す。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("ルール名が空".into());
        }
        if self.conditions.is_empty() {
            return Err("条件が無い".into());
        }
        for c in &self.conditions {
            match c {
                Condition::Text { targets, text, .. } => {
                    if targets.is_empty() {
                        return Err("条件の対象が無い".into());
                    }
                    if text.trim().is_empty() {
                        return Err("条件の文字列が空".into());
                    }
                }
                Condition::Shape { targets, .. } => {
                    if targets.is_empty() {
                        return Err("条件の対象が無い".into());
                    }
                }
            }
        }
        if self.effect.disable.is_empty() && self.effect.enable.is_empty() {
            return Err("効果が無い".into());
        }
        if let Some(r) = self.effect.disable.intersection(&self.effect.enable).next() {
            return Err(format!("同じ種類を無効と有効の両方にはできない: {r:?}"));
        }
        Ok(())
    }
}

// ── 条件の判定 ────────────────────────────────────────────────────────

/// 判定の入力（1ファイル）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuleInput {
    /// 拡張子を除いたファイル名。
    pub file_stem: String,
    /// info.txt の見出し行（内部照合に使う行と、その他の行）。
    pub info_titles: Vec<String>,
    pub info_category: Option<String>,
}

impl RuleInput {
    pub(crate) fn new(file_name: &str, info: Option<&InfoTxt>) -> Self {
        let mut info_titles = Vec::new();
        if let Some(i) = info {
            info_titles.extend(i.title.clone());
            info_titles.extend(i.alt_titles.iter().cloned());
        }
        Self {
            file_stem: split_archive_extension(file_name).0.to_string(),
            info_titles,
            info_category: info.and_then(|i| i.category.clone()),
        }
    }

    fn texts(&self, target: CondTarget) -> Vec<String> {
        match target {
            CondTarget::FileName => vec![self.file_stem.clone()],
            CondTarget::InfoTitle => self.info_titles.clone(),
            CondTarget::InfoCategory => self.info_category.iter().cloned().collect(),
            CondTarget::Title => {
                let mut v = vec![strip_groups(&self.file_stem)];
                v.extend(self.info_titles.iter().map(|t| strip_groups(t)));
                v
            }
        }
    }
}

/// 括弧 `(...)` `[...]`（全角を含む）の部分を除いて、空白をまとめる。閉じていない括弧があれば、そのまま返す。
pub(crate) fn strip_groups(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        if is_open_round(c) || is_open_square(c) {
            depth += 1;
        } else if (is_close_round(c) || is_close_square(c)) && depth > 0 {
            depth -= 1;
        } else if depth == 0 {
            out.push(c);
        }
    }
    if depth > 0 {
        return collapse_spaces(s);
    }
    collapse_spaces(&out)
}

/// ` - ` で区切った題名（cosplay の `演者 - キャラ (パロディ元)` など）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DashTitle {
    pub shape: NameShape,
    pub first: String,
    pub second: String,
    pub third: Option<String>,
}

/// ` - ` で区切った題名を読む。先頭が括弧で始まるもの（`[サークル] …` `(イベント) …`）は対象外。
/// `Honkai-Star Rail` のような、スペースの無いハイフンは区切りにしない。
pub(crate) fn parse_dash_title(text: &str) -> Option<DashTitle> {
    let t = collapse_spaces(text);
    let first_char = t.chars().next()?;
    if is_open_round(first_char) || is_open_square(first_char) {
        return None;
    }
    let parts: Vec<&str> = t.split(" - ").map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty() || p.starts_with('-')) {
        return None;
    }
    match parts.as_slice() {
        [a, b, c] => Some(DashTitle { shape: NameShape::Dash3, first: a.to_string(), second: b.to_string(), third: Some(c.to_string()) }),
        [a, b] => match take_trailing_group(b, is_open_round, is_close_round) {
            Some((before, inside)) if !before.trim().is_empty() && !inside.trim().is_empty() => Some(DashTitle {
                shape: NameShape::Dash2Paren,
                first: a.to_string(),
                second: before.trim().to_string(),
                third: Some(inside.trim().to_string()),
            }),
            _ => Some(DashTitle { shape: NameShape::Dash2, first: a.to_string(), second: b.to_string(), third: None }),
        },
        _ => None,
    }
}

fn text_matches(target: &str, how: TextMatch, needle_key: &str, opts: &MatchOptions) -> bool {
    if needle_key.is_empty() {
        return false;
    }
    let key = exact_key(target, opts);
    match how {
        TextMatch::Contains => key.contains(needle_key),
        TextMatch::Prefix => key.starts_with(needle_key),
        TextMatch::Exact => key == needle_key,
    }
}

impl Condition {
    pub(crate) fn matches(&self, input: &RuleInput, opts: &MatchOptions) -> bool {
        match self {
            Condition::Text { targets, how, text } => {
                let needle = exact_key(text, opts);
                targets.iter().any(|t| input.texts(*t).iter().any(|s| text_matches(s, *how, &needle, opts)))
            }
            Condition::Shape { targets, shape } => {
                targets.iter().any(|t| input.texts(*t).iter().any(|s| parse_dash_title(s).is_some_and(|d| d.shape == *shape)))
            }
        }
    }
}

impl TagRule {
    /// このファイルでルールが成立するか（無効なルールは成立しない）。
    pub(crate) fn applies_to(&self, input: &RuleInput, opts: &MatchOptions) -> bool {
        if !self.enabled || self.conditions.is_empty() {
            return false;
        }
        if self.match_all {
            self.conditions.iter().all(|c| c.matches(input, opts))
        } else {
            self.conditions.iter().any(|c| c.matches(input, opts))
        }
    }
}

// ── 評価 ──────────────────────────────────────────────────────────────

/// 1ファイルに成立したルールの効果の集計。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuleOutcome {
    /// 無効にする種類（成立したルールの和集合）。
    pub disabled: BTreeSet<RuleRole>,
    /// 有効にする種類（成立したルールの和集合から、無効の側に入っているものを除いたもの）。
    pub enabled: BTreeSet<RuleRole>,
    /// 成立したルールの名前（ルールの並び順）。
    pub applied: Vec<String>,
    /// 無効にした理由。種類ごとに、無効にしたルールの名前。確認ウィンドウの「ルール『…』で無効」の表示に使う。
    pub disabled_by: BTreeMap<RuleRole, Vec<String>>,
}

impl RuleOutcome {
    /// この種類の言及を取るか。無効にされていれば取らない。新種類は、有効にしたルールがあるときだけ取る。
    pub(crate) fn is_role_enabled(&self, role: RuleRole) -> bool {
        if self.disabled.contains(&role) {
            return false;
        }
        !role.is_opt_in() || self.enabled.contains(&role)
    }

    /// 無効にした理由（ルール名を「、」でつないだもの）。無効でなければ None。
    pub(crate) fn disabled_reason(&self, role: RuleRole) -> Option<String> {
        self.disabled_by.get(&role).map(|names| names.join("、"))
    }

    /// 成立したルールが1つも無いか。
    pub(crate) fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }
}

/// ルールを順に評価し、成立したものの効果を集める。
pub(crate) fn evaluate_rules(rules: &[TagRule], input: &RuleInput, opts: &MatchOptions) -> RuleOutcome {
    let mut out = RuleOutcome::default();
    for rule in rules.iter().filter(|r| r.applies_to(input, opts)) {
        out.applied.push(rule.name.clone());
        for r in &rule.effect.disable {
            out.disabled.insert(*r);
            out.disabled_by.entry(*r).or_default().push(rule.name.clone());
        }
        out.enabled.extend(rule.effect.enable.iter().copied());
    }
    out.enabled = out.enabled.difference(&out.disabled).copied().collect();
    out
}

// ── プリセット ────────────────────────────────────────────────────────

/// 条件のプリセット（ドロップダウンの初期の選択肢）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Preset {
    AiGenerated,
    Cosplay,
    ArtBook,
    Cg3d,
    Doujinshi,
    Manga,
    Anthology,
}

impl Preset {
    pub(crate) const ALL: [Preset; 7] =
        [Preset::AiGenerated, Preset::Cosplay, Preset::ArtBook, Preset::Cg3d, Preset::Doujinshi, Preset::Manga, Preset::Anthology];

    /// 表示名（ルール名の初期値にもなる）。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Preset::AiGenerated => "AI generated",
            Preset::Cosplay => "cosplay",
            Preset::ArtBook => "画集・イラスト集",
            Preset::Cg3d => "3D / CG集",
            Preset::Doujinshi => "doujinshi",
            Preset::Manga => "manga",
            Preset::Anthology => "anthology",
        }
    }

    /// 条件の文字列（どれかに一致すれば成立）。
    fn texts(self) -> &'static [&'static str] {
        match self {
            Preset::AiGenerated => &["AI generated", "AI生成", "AIイラスト"],
            Preset::Cosplay => &["cosplay", "コスプレ"],
            Preset::ArtBook => &["artbook", "art book", "画集", "イラスト集"],
            Preset::Cg3d => &["3dcg", "CG集", "CG set"],
            Preset::Doujinshi => &["doujinshi", "同人誌"],
            Preset::Manga => &["manga", "漫画"],
            Preset::Anthology => &["anthology", "アンソロジー"],
        }
    }

    /// 条件の対象。Category を見るプリセットは Category だけ、それ以外は題名・ファイル名・見出し行・Category。
    fn targets(self) -> Vec<CondTarget> {
        match self {
            Preset::Doujinshi | Preset::Manga => vec![CondTarget::InfoCategory],
            Preset::Cosplay => vec![CondTarget::InfoCategory, CondTarget::Title, CondTarget::FileName, CondTarget::InfoTitle],
            _ => vec![CondTarget::Title, CondTarget::FileName, CondTarget::InfoTitle, CondTarget::InfoCategory],
        }
    }

    /// 既定の効果。効果が決まっているものだけ（他は、条件だけのプリセットで、効果はユーザーが選ぶ）。
    fn default_effect(self) -> Effect {
        let set = |v: &[RuleRole]| v.iter().copied().collect::<BTreeSet<_>>();
        match self {
            Preset::AiGenerated => Effect { disable: set(&[RuleRole::Author, RuleRole::Group, RuleRole::Event]), enable: set(&[RuleRole::Parody]) },
            Preset::Cosplay => Effect {
                disable: set(&[RuleRole::Author, RuleRole::Group, RuleRole::Event]),
                enable: set(&[RuleRole::Performer, RuleRole::Character, RuleRole::Parody]),
            },
            Preset::Anthology => Effect { disable: set(&[RuleRole::Author, RuleRole::Group]), enable: BTreeSet::new() },
            _ => Effect::default(),
        }
    }

    /// プリセットからルールを作る（`id` は呼び出し側が振る）。効果が無いプリセットは、効果が空のまま（保存前にユーザーが決める）。
    pub(crate) fn to_rule(self, id: u64) -> TagRule {
        let targets = self.targets();
        TagRule {
            id,
            name: self.label().to_string(),
            enabled: true,
            conditions: self
                .texts()
                .iter()
                .map(|t| Condition::Text { targets: targets.clone(), how: TextMatch::Contains, text: t.to_string() })
                .collect(),
            match_all: false,
            effect: self.default_effect(),
        }
    }
}

// ── 保存 ──────────────────────────────────────────────────────────────

/// 保存するルール一式。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RuleSet {
    pub version: u32,
    /// 次に振るルールID（削除しても戻さない）。
    pub next_id: u64,
    pub rules: Vec<TagRule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self { version: VERSION, next_id: 1, rules: Vec::new() }
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

impl RuleSet {
    /// ルールを末尾に足す（IDは振り直す）。検証に通らなければ理由を返す。
    pub(crate) fn add(&mut self, mut rule: TagRule) -> Result<u64, String> {
        rule.id = self.next_id;
        rule.validate()?;
        self.next_id += 1;
        let id = rule.id;
        self.rules.push(rule);
        Ok(id)
    }

    /// 同じIDのルールを置き換える。検証に通らない・IDが無いときは理由を返す。
    pub(crate) fn update(&mut self, rule: TagRule) -> Result<(), String> {
        rule.validate()?;
        match self.rules.iter_mut().find(|r| r.id == rule.id) {
            Some(slot) => {
                *slot = rule;
                Ok(())
            }
            None => Err("ルールが無い".into()),
        }
    }

    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        self.rules.len() != before
    }

    /// 1つ上（`up`）または下へ動かす。動かせたら true。
    pub(crate) fn move_rule(&mut self, id: u64, up: bool) -> bool {
        let Some(i) = self.rules.iter().position(|r| r.id == id) else { return false };
        let j = if up { i.checked_sub(1) } else { (i + 1 < self.rules.len()).then_some(i + 1) };
        match j {
            Some(j) => {
                self.rules.swap(i, j);
                true
            }
            None => false,
        }
    }

    /// 保存する（tmpに書いてからrename）。保存できたら true。
    pub(crate) fn save(&self, root: &Path) -> bool {
        let Ok(content) = serde_json::to_string_pretty(self) else { return false };
        let _ = std::fs::create_dir_all(root);
        let tmp = root.join(format!("{FILE_NAME}.tmp"));
        if std::fs::write(&tmp, content).is_err() {
            return false;
        }
        if std::fs::rename(&tmp, path(root)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        true
    }

    /// 読む。無い・壊れている・版が新しいときは None。
    pub(crate) fn load(root: &Path) -> Option<Self> {
        let set: Self = serde_json::from_str(&std::fs::read_to_string(path(root)).ok()?).ok()?;
        (set.version <= VERSION).then_some(set)
    }

    /// ファイルのルールを、評価の入力にする。
    pub(crate) fn evaluate(&self, input: &RuleInput, opts: &MatchOptions) -> RuleOutcome {
        evaluate_rules(&self.rules, input, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_source::parse_info_txt;

    fn opts() -> MatchOptions {
        MatchOptions::default()
    }

    fn input(file: &str, info: Option<&str>) -> RuleInput {
        RuleInput::new(file, info.map(parse_info_txt).as_ref())
    }

    fn rule_for(p: Preset) -> TagRule {
        let mut r = p.to_rule(1);
        if r.effect == Effect::default() {
            r.effect.disable.insert(RuleRole::Event);
        }
        r
    }

    // ── 題名・形 ──

    #[test]
    fn strip_groups_removes_brackets_but_keeps_the_title() {
        assert_eq!(strip_groups("(C105) [Hawk Bit (Kouji)] Title Anthology [日本語]"), "Title Anthology");
        assert_eq!(strip_groups("［ＡＩ］ 題名（副題）"), "題名");
        assert_eq!(strip_groups("Title [broken"), "Title [broken", "閉じていない括弧は触らない");
    }

    #[test]
    fn dash_title_shapes() {
        let d = parse_dash_title("Tiny Asa アサ - Castorice (Honkai-Star Rail)").unwrap();
        assert_eq!((d.shape, d.first.as_str(), d.second.as_str(), d.third.as_deref()), (NameShape::Dash2Paren, "Tiny Asa アサ", "Castorice", Some("Honkai-Star Rail")));
        let d = parse_dash_title("希尔薇 - w旗袍 - Arknights").unwrap();
        assert_eq!((d.shape, d.second.as_str(), d.third.as_deref()), (NameShape::Dash3, "w旗袍", Some("Arknights")));
        let d = parse_dash_title("Asa - Castorice").unwrap();
        assert_eq!((d.shape, d.third), (NameShape::Dash2, None));
        // スペースの無いハイフンは区切りにしない。括弧で始まるもの・4つ以上は対象外。
        assert_eq!(parse_dash_title("Honkai-Star Rail"), None);
        assert_eq!(parse_dash_title("[Circle] A - B (C)"), None);
        assert_eq!(parse_dash_title("(C105) A - B"), None);
        assert_eq!(parse_dash_title("A - B - C - D"), None);
        assert_eq!(parse_dash_title("A -  - B"), None);
    }

    // ── 条件 ──

    #[test]
    fn text_condition_uses_the_exact_key_so_spelling_variants_match() {
        let r = rule_for(Preset::AiGenerated);
        for name in ["[AI Generated] Book.zip", "Book (ai-generated).zip", "ＡＩ　ｇｅｎｅｒａｔｅｄ Book.zip", "Book [AI_Generated].cbz"] {
            assert!(r.applies_to(&input(name, None), &opts()), "{name}");
        }
        assert!(!r.applies_to(&input("Plain Book.zip", None), &opts()));
        assert!(r.applies_to(&input("a.zip", Some("AI生成 作品\n\nCategory: Doujinshi\n")), &opts()), "info.txt の見出し行");
    }

    #[test]
    fn prefix_and_exact_matching_and_empty_text() {
        let cond = |how, text: &str| Condition::Text { targets: vec![CondTarget::FileName], how, text: text.into() };
        let i = input("Cosplay Collection 3.zip", None);
        assert!(cond(TextMatch::Prefix, "cosplay").matches(&i, &opts()));
        assert!(!cond(TextMatch::Prefix, "collection").matches(&i, &opts()));
        assert!(cond(TextMatch::Contains, "collection").matches(&i, &opts()));
        assert!(cond(TextMatch::Exact, "Cosplay Collection 3").matches(&i, &opts()));
        assert!(!cond(TextMatch::Exact, "Cosplay").matches(&i, &opts()));
        assert!(!cond(TextMatch::Contains, "").matches(&i, &opts()), "空の文字列は何にも一致しない");
        assert!(!cond(TextMatch::Contains, " - ").matches(&i, &opts()), "記号だけの文字列も一致しない");
    }

    #[test]
    fn category_and_title_targets() {
        let info = "[Ebizoridan (Shien)] Anthology Vol 2\n[えびぞり団 (しえん)] アンソロジー 2\n\nCategory: Doujinshi\n";
        let i = input("[Circle (Author)] Some Book.zip", Some(info));
        let by = |t: CondTarget, text: &str| Condition::Text { targets: vec![t], how: TextMatch::Contains, text: text.into() };
        assert!(by(CondTarget::InfoCategory, "doujinshi").matches(&i, &opts()));
        assert!(by(CondTarget::Title, "アンソロジー").matches(&i, &opts()), "題名＝括弧を除いた部分（日本語行）");
        assert!(by(CondTarget::Title, "anthology").matches(&i, &opts()), "題名（英語行）");
        assert!(!by(CondTarget::Title, "Circle").matches(&i, &opts()), "括弧の中身（サークル名）は題名に含めない");
        assert!(by(CondTarget::FileName, "Circle").matches(&i, &opts()));
        assert!(!by(CondTarget::InfoCategory, "anthology").matches(&i, &opts()));
    }

    #[test]
    fn shape_condition_finds_cosplay_style_names_without_info_txt() {
        let shape = |s| Condition::Shape { targets: vec![CondTarget::FileName], shape: s };
        let i = input("Tiny Asa アサ - Castorice (Honkai-Star Rail).zip", None);
        assert!(shape(NameShape::Dash2Paren).matches(&i, &opts()));
        assert!(!shape(NameShape::Dash3).matches(&i, &opts()));
        assert!(!shape(NameShape::Dash2Paren).matches(&input("[Circle (Author)] Title.zip", None), &opts()));
    }

    #[test]
    fn match_all_and_match_any() {
        let mut r = Preset::Cosplay.to_rule(1);
        r.conditions = vec![
            Condition::Shape { targets: vec![CondTarget::FileName], shape: NameShape::Dash2Paren },
            Condition::Text { targets: vec![CondTarget::InfoCategory], how: TextMatch::Exact, text: "Cosplay".into() },
        ];
        let name = "Asa - Chara (Series).zip";
        let with_cat = input(name, Some("Asa - Chara (Series)\n\nCategory: Cosplay\n"));
        let without = input(name, None);
        r.match_all = false;
        assert!(r.applies_to(&with_cat, &opts()) && r.applies_to(&without, &opts()), "どれかを満たせば成立");
        r.match_all = true;
        assert!(r.applies_to(&with_cat, &opts()) && !r.applies_to(&without, &opts()), "すべてを満たしたとき成立");
        r.enabled = false;
        assert!(!r.applies_to(&with_cat, &opts()), "無効なルールは成立しない");
    }

    // ── 評価・競合 ──

    #[test]
    fn effects_union_and_enable_is_overridden_by_disable() {
        let ai = rule_for(Preset::AiGenerated);
        let cosplay = rule_for(Preset::Cosplay);
        let anth = rule_for(Preset::Anthology);
        let i = input("[AI Generated] Cosplay Anthology.zip", None);
        let out = evaluate_rules(&[ai, cosplay, anth], &i, &opts());
        assert_eq!(out.applied, vec!["AI generated", "cosplay", "anthology"]);
        assert_eq!(out.disabled, [RuleRole::Event, RuleRole::Author, RuleRole::Group].into_iter().collect());
        assert_eq!(out.enabled, [RuleRole::Performer, RuleRole::Character, RuleRole::Parody].into_iter().collect());
        assert!(!out.is_role_enabled(RuleRole::Author));
        assert!(out.is_role_enabled(RuleRole::Language), "効果で触れていない既存の種類は有効のまま");
        assert!(out.is_role_enabled(RuleRole::Parody));
        assert_eq!(out.disabled_reason(RuleRole::Author).as_deref(), Some("AI generated、cosplay、anthology"));
        assert_eq!(out.disabled_reason(RuleRole::Language), None);
    }

    #[test]
    fn disable_beats_enable_across_rules_and_new_roles_are_off_by_default() {
        let mut a = rule_for(Preset::Cosplay);
        a.effect.enable = [RuleRole::Performer, RuleRole::Parody].into_iter().collect();
        let mut b = rule_for(Preset::AiGenerated);
        b.id = 2;
        b.effect.disable.insert(RuleRole::Parody);
        b.effect.enable.clear();
        let out = evaluate_rules(&[a, b], &input("cosplay AI generated.zip", None), &opts());
        assert!(!out.is_role_enabled(RuleRole::Parody), "別のルールが無効にした種類は、有効にしても残らない");
        assert!(out.is_role_enabled(RuleRole::Performer));
        let none = evaluate_rules(&[], &input("x.zip", None), &opts());
        assert!(none.is_empty());
        assert!(!none.is_role_enabled(RuleRole::Performer), "新種類は、有効にしたルールが無ければ取らない");
        assert!(none.is_role_enabled(RuleRole::Author));
    }

    #[test]
    fn presets_build_valid_rules_and_cosplay_matches_category_info() {
        for p in Preset::ALL {
            assert_eq!(rule_for(p).validate(), Ok(()), "{p:?}");
        }
        let cosplay = Preset::Cosplay.to_rule(1);
        let i = input("a.zip", Some("希尔薇 - w旗袍 - Arknights\n\nCategory: Cosplay\n"));
        assert!(cosplay.applies_to(&i, &opts()));
        // 条件だけのプリセット（効果は未選択）は、効果を選ぶまで検証に通らない。
        assert!(Preset::Manga.to_rule(1).validate().is_err());
        // doujinshi の条件は Category だけを見る。題名に doujinshi とあっても成立しない。
        let dj = rule_for(Preset::Doujinshi);
        assert!(dj.applies_to(&input("a.zip", Some("T\n\nCategory: Doujinshi\n")), &opts()));
        assert!(!dj.applies_to(&input("doujinshi collection.zip", None), &opts()));
    }

    // ── 検証・保存 ──

    #[test]
    fn validation_reports_problems() {
        let ok = rule_for(Preset::Cosplay);
        let mut r = ok.clone();
        r.name = " ".into();
        assert!(r.validate().is_err());
        let mut r = ok.clone();
        r.conditions.clear();
        assert!(r.validate().is_err());
        let mut r = ok.clone();
        r.conditions = vec![Condition::Text { targets: vec![], how: TextMatch::Contains, text: "x".into() }];
        assert!(r.validate().is_err());
        let mut r = ok.clone();
        r.conditions = vec![Condition::Text { targets: vec![CondTarget::Title], how: TextMatch::Contains, text: "  ".into() }];
        assert!(r.validate().is_err());
        let mut r = ok.clone();
        r.effect = Effect::default();
        assert!(r.validate().is_err());
        let mut r = ok;
        r.effect.enable.insert(RuleRole::Author);
        assert!(r.validate().unwrap_err().contains("両方"), "同じ種類を無効と有効の両方にはできない");
    }

    #[test]
    fn rule_set_add_update_remove_and_reorder() {
        let mut s = RuleSet::default();
        let a = s.add(rule_for(Preset::AiGenerated)).unwrap();
        let b = s.add(rule_for(Preset::Cosplay)).unwrap();
        let c = s.add(rule_for(Preset::Anthology)).unwrap();
        assert_eq!((a, b, c), (1, 2, 3));
        assert!(s.add(Preset::Manga.to_rule(0)).is_err(), "検証に通らないものは足せない");
        assert_eq!(s.next_id, 4, "失敗したときはIDを進めない");
        assert!(s.move_rule(c, true) && s.rules[1].id == c);
        assert!(!s.move_rule(a, true), "先頭は上へ動かせない");
        assert!(!s.move_rule(b, false) || s.rules[2].id == b);
        let mut upd = s.rules[0].clone();
        upd.name = "改名".into();
        assert!(s.update(upd).is_ok() && s.rules[0].name == "改名");
        assert!(s.update(TagRule { id: 99, ..s.rules[0].clone() }).is_err());
        assert!(s.remove(a) && !s.remove(a));
        assert_eq!(s.add(rule_for(Preset::Cg3d)).unwrap(), 4, "削除したIDは再利用しない");
    }

    #[test]
    fn save_and_load_round_trip_and_survive_corruption() {
        let dir = std::env::temp_dir().join(format!("nekoviewer_rules_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(RuleSet::load(&dir), None);
        let mut s = RuleSet::default();
        s.add(rule_for(Preset::AiGenerated)).unwrap();
        let mut shaped = rule_for(Preset::Cosplay);
        shaped.conditions.push(Condition::Shape { targets: vec![CondTarget::FileName], shape: NameShape::Dash2Paren });
        shaped.match_all = true;
        s.add(shaped).unwrap();
        assert!(s.save(&dir));
        assert_eq!(RuleSet::load(&dir), Some(s));
        assert!(!dir.join("nekoviewer_tag_rules.json.tmp").exists());
        std::fs::write(dir.join("nekoviewer_tag_rules.json"), "{broken").unwrap();
        assert_eq!(RuleSet::load(&dir), None);
        std::fs::write(dir.join("nekoviewer_tag_rules.json"), r#"{"version":99,"next_id":1,"rules":[]}"#).unwrap();
        assert_eq!(RuleSet::load(&dir), None, "新しい版は読まない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rule_set_evaluate_uses_the_rules_in_order() {
        let mut s = RuleSet::default();
        s.add(rule_for(Preset::Anthology)).unwrap();
        let out = s.evaluate(&input("Great Anthology.zip", None), &opts());
        assert_eq!(out.applied, vec!["anthology"]);
        assert!(!out.is_role_enabled(RuleRole::Group));
    }

    /// 目視用: 効果の既定があるプリセット3つ（AI generated / cosplay / anthology）と、cosplay のファイル名の形のルールを、設定フォルダ `NEKO_RULES_OUT` に
    /// 書き出す。編集UI（R4）ができる前に、抽出への接続を実機で確かめるために使う
    /// （`NEKO_RULES_OUT=<設定フォルダ> cargo test export_default_preset_rules -- --ignored --nocapture`）。既存のルールは上書きされる。
    #[test]
    #[ignore]
    fn export_default_preset_rules() {
        let Ok(out) = std::env::var("NEKO_RULES_OUT") else { return };
        let mut s = RuleSet::default();
        for p in [Preset::AiGenerated, Preset::Cosplay, Preset::Anthology] {
            s.add(p.to_rule(0)).unwrap();
        }
        // info.txt が無い cosplay（`演者 - キャラ (パロディ元)` / `演者 - キャラ - パロディ元`）をファイル名の形で拾うルール。
        let mut shaped = Preset::Cosplay.to_rule(0);
        shaped.name = "cosplay（ファイル名の形）".into();
        shaped.conditions = [NameShape::Dash2Paren, NameShape::Dash3]
            .into_iter()
            .map(|shape| Condition::Shape { targets: vec![CondTarget::FileName], shape })
            .collect();
        s.add(shaped).unwrap();
        assert!(s.save(Path::new(&out)));
        println!("{} 件のルールを {out}/{FILE_NAME} に書いた", s.rules.len());
    }
}
