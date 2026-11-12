//! 一括タグ付けの候補生成（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 走査で集めた情報（形式・info.txt・ファイル名）と、現在のタグマスターから、「どのタグ要素を何件のファイルに付けるか」の
//! 候補を作る。確認ウィンドウ（2枚）と適用が、この結果を読む。
//!
//! - 取得元の優先: アーカイブ内の info.txt（タイトル行・Language・Category）が正。ファイル名は、無い部分だけ補う
//! - 候補は「種類（`Role`）＋完全一致キー」ごとに1つ。同じ名前を持つファイルは、1つの候補にまとまる
//! - 既存タグとの照合は `tag_similarity`（完全一致キーと類似キー）。完全一致だけが初期採用、類似は提案として並べて未選択
//! - `[A(B)]` は一般系（外=グループ・内=作者）を基本に、既存タグとの類似度の合計が高い向きへ補正する
//! - 窓1は作者・グループ以外、窓2は作者・グループ。新規要素数が窓ごとに1000で警告、3000でエラー

// 候補の補助情報（表記の一覧・ファイル数など）は、確認ウィンドウの表示や今後の拡張用に保持している。
#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::i18n::Lang;
use crate::tag_corpus::Label;
use crate::tag_meta::{resolve, ArchiveMeta, Resolved};
use crate::tag_model::Model;
use crate::tag_rules::{evaluate_rules, parse_dash_title, RuleInput, RuleOutcome, RuleRole, TagRule};
use crate::tag_master::{alias_keys, escape_alias_sep, join_aliases, normalize_key, overlapping_elements, split_aliases, SystemKind};
use crate::tag_similarity::{cluster, exact_key, keys, normalize_width, similarity, similarity_at_least, Keys, MatchOptions};
use crate::tag_source::{
    canonical_language, language_english_name, parse_info_txt, parse_name_parts, split_archive_extension, split_title_group, Creator,
    FormatTag, InfoTxt, NameParts,
};
use crate::view_explorer::TagManagerCategoryUi;

/// 上限に達したとき、1回の「拡張」で増やす新規要素数（カテゴリごと）。上限の既定値は設定（`AppConfig::tag_new_element_limit`）。
pub(crate) const LIMIT_EXPAND_STEP: usize = 5000;
/// 1候補に並べる類似提案の最大数。
const MAX_SUGGESTIONS: usize = 5;

/// 候補の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    Format,
    Main,
    Language,
    Event,
    Author,
    Group,
    /// 演者（cosplay）。大ルールで有効にしたときだけ取る。
    Performer,
    /// キャラ（cosplay）。大ルールで有効にしたときだけ取る。
    Character,
    /// パロディ元。大ルールで有効にしたときだけ取る。
    Parody,
    /// 属性タグ（ComicInfo.xml の Tags・meta.json の type=tag）。「正の情報」があるときだけ取る。
    Attribute,
    /// 年月（`2023-12`。月が無ければ `2023`）。ComicInfo.xml の Year・Month から、「正の情報」があるときだけ取る。
    YearMonth,
}

/// 確認ウィンドウ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Window {
    /// 作者・グループ名を除く項目。
    One,
    /// 作者・グループ名。
    Two,
}

impl Role {
    pub(crate) fn window(self) -> Window {
        match self {
            // 人名の種類は窓2。
            Role::Author | Role::Group | Role::Performer => Window::Two,
            _ => Window::One,
        }
    }

    /// 窓の中の並び順（一覧・適用の順）。
    pub(crate) fn order(self) -> u8 {
        match self {
            Role::Format => 0,
            Role::Main => 1,
            Role::Language => 2,
            Role::Event => 3,
            Role::Parody => 4,
            Role::Character => 5,
            Role::Author => 6,
            Role::Group => 7,
            Role::Performer => 8,
            Role::YearMonth => 9,
            Role::Attribute => 10,
        }
    }

    /// 新規カテゴリを作るときの名前。
    pub(crate) fn new_category_name(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Role::Author, Lang::English) => "Author",
            (Role::Author, _) => "作者",
            (Role::Group, Lang::English) => "Group",
            (Role::Group, Lang::Chinese) => "团体",
            (Role::Group, _) => "グループ",
            (Role::Language, Lang::English) => "Language",
            (Role::Language, Lang::Chinese) => "语言",
            (Role::Language, _) => "言語",
            (Role::Event, Lang::English) => "Event",
            (Role::Event, Lang::Chinese) => "活动",
            (Role::Event, _) => "イベント",
            (Role::Performer, Lang::English) => "Performer",
            (Role::Performer, Lang::Chinese) => "演员",
            (Role::Performer, _) => "演者",
            (Role::Character, Lang::English) => "Character",
            (Role::Character, Lang::Chinese) => "角色",
            (Role::Character, _) => "キャラクター",
            (Role::Parody, Lang::English) => "Parody",
            (Role::Parody, Lang::Chinese) => "原作",
            (Role::Parody, _) => "パロディ元",
            (Role::Attribute, Lang::English) => "Attribute",
            (Role::Attribute, _) => "属性",
            (Role::YearMonth, Lang::English) => "Year-Month",
            (Role::YearMonth, _) => "年月",
            // 形式とメインは常に既存のカテゴリがある。
            (Role::Format, _) => "Archive format",
            (Role::Main, _) => "Main",
        }
    }

    /// 既存カテゴリを名前で探すときの別名（全言語分。正規化して比べる）。
    fn category_aliases(self) -> &'static [&'static str] {
        match self {
            Role::Author => &["作者", "author", "artist", "漫画家"],
            Role::Group => &["グループ", "サークル", "group", "circle", "团体", "社团"],
            Role::Language => &["言語", "language", "语言"],
            Role::Event => &["イベント", "event", "活动"],
            Role::Performer => &["演者", "performer", "cosplayer", "演员", "モデル"],
            Role::Character => &["キャラクター", "キャラ", "character", "角色"],
            Role::Parody => &["パロディ元", "パロディ", "parody", "原作", "作品", "series"],
            Role::Attribute => &["属性", "attribute", "attributes", "タグ", "tag", "tags"],
            Role::YearMonth => &["年月", "year-month", "yearmonth", "year month"],
            Role::Format | Role::Main => &[],
        }
    }
}

/// 候補を入れるカテゴリ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CatRef {
    /// 既存のカテゴリ（`TagManagerCategoryUi` の配列の番号）。
    Existing(usize),
    /// まだ無いカテゴリ（適用時に `Role::new_category_name` の名前で作る）。
    New,
}

/// 既存タグへの類似提案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suggestion {
    pub cat_idx: usize,
    pub tier_id: u64,
    /// 要素の全体名（別名つきなら `A|B|C`）。
    pub name: String,
    /// 最も近かった別名（別名の無い要素では `name` と同じ）。
    pub alias: String,
    pub score: u8,
}

/// 候補の扱い。初期値は生成時の規則で決まり、確認ウィンドウで変えられる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// 既存の要素に付ける（採用）。
    Existing { cat_idx: usize, tier_id: u64 },
    /// 新しい要素を作って付ける（採用）。
    New,
    /// 既存の要素の別名に、この候補の名前を足して付ける（採用）。要素名が `Kouji` → `Kouji|Koji` に書き換わる。
    Alias { cat_idx: usize, tier_id: u64 },
    /// 付けない（未選択）。
    Skip,
}

#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub role: Role,
    /// 新規作成するときの登録名。
    pub name: String,
    /// 見つかった表記（重複を除く）。登録名もこの中から選ぶ。
    pub forms: Vec<String>,
    /// 完全一致キー（候補の同一性）。
    pub key: String,
    /// このタグを付ける対象のファイル（入力の番号。昇順・重複なし）。
    pub files: Vec<usize>,
    /// 新規要素を作る場合のカテゴリ。
    pub category: CatRef,
    pub choice: Choice,
    /// 生成時の初期の扱い（確認ウィンドウで「採用」に戻すときの基準）。
    pub initial: Choice,
    /// 既存タグへの類似提案（類似度の降順）。
    pub suggestions: Vec<Suggestion>,
    /// 作者・グループとして確証が無い（既存タグに完全一致せず、`[X]` 単独や括弧の向きが推測のもの）。
    pub unconfirmed: bool,
    /// 類似品の束の番号（同じ種類の中で、類似度がしきい値以上のものが同じ番号）。
    pub cluster: usize,
    /// 手動でまとめた候補の別名（先頭が主表記。`|` は全角に置換済み）。まとめていない候補は空。
    pub aliases: Vec<String>,
    /// まとめる前の候補（分解で元に戻す）。まとめていない候補は空。
    pub members: Vec<Candidate>,
    /// まとめの番号（再生成後も同じまとめを復元するための識別）。
    pub bundle_id: Option<u64>,
    /// 学習モデルが「タグにしない」と判定したため、未採用にして残している（要確認）。
    pub review: bool,
    /// 対にした別の見出し行の表記（英語⇄日本語）。登録するとき、`name` の別名として足す。
    pub alt_forms: Vec<String>,
    /// ルールで無効にされたため、未採用にして残している（要確認）。無効にしたルールの名前（「、」でつなぐ）。
    pub rule_reason: Option<String>,
}

impl Candidate {
    /// 付ける（採用）か。
    pub(crate) fn is_adopted(&self) -> bool {
        !matches!(self.choice, Choice::Skip)
    }

    /// 要素として登録する名前。まとめた候補は別名を `|` でつないだ名前、そうでなければ名前の `|` を全角にしたもの。
    pub(crate) fn registration_name(&self) -> String {
        if self.aliases.is_empty() {
            let mut all = vec![escape_alias_sep(&self.name)];
            all.extend(self.alt_forms.iter().map(|a| escape_alias_sep(a)));
            join_aliases(&all)
        } else {
            join_aliases(&self.aliases)
        }
    }

    /// 既存要素へ足す別名の一覧（まとめていなければ登録名の1つだけ）。
    pub(crate) fn alias_list(&self) -> Vec<String> {
        if self.aliases.is_empty() {
            let mut all = vec![escape_alias_sep(&self.name)];
            all.extend(self.alt_forms.iter().map(|a| escape_alias_sep(a)));
            all
        } else {
            self.aliases.clone()
        }
    }

    /// 採用にする。初期が既存の完全一致（まとめた候補では別名追加）ならそれ、類似提案があれば最上位、無ければ新規作成。
    pub(crate) fn adopt(&mut self) {
        if self.is_adopted() {
            return;
        }
        self.choice = match (&self.initial, self.suggestions.first()) {
            (Choice::Existing { .. } | Choice::Alias { .. }, _) => self.initial.clone(),
            (_, Some(s)) => Choice::Existing { cat_idx: s.cat_idx, tier_id: s.tier_id },
            _ => Choice::New,
        };
    }

    /// 不採用（付けない）にする。
    pub(crate) fn unadopt(&mut self) {
        self.choice = Choice::Skip;
    }

    /// 新規要素を作る必要がある（既存の完全一致ではない）か。要素数の警告・エラー判定に使う。
    pub(crate) fn needs_new_element(&self) -> bool {
        !matches!(self.choice, Choice::Existing { .. } | Choice::Alias { .. })
    }
}

/// 候補生成の入力（1ファイル）。
#[derive(Clone, Debug)]
pub(crate) struct CandidateInput {
    /// 拡張子つきのファイル名（フォルダ本はフォルダ名）。
    pub file_name: String,
    pub format: Option<FormatTag>,
    /// info.txt の解析結果。無い・対象外なら None。
    pub info: Option<InfoTxt>,
    /// ComicInfo.xml・meta.json（アーカイブ内の「正の情報」）。無ければ None。info.txt・ファイル名より優先する。
    pub meta: Option<ArchiveMeta>,
}

/// 窓ごとの新規要素数（集計表示用）。上限の判定はカテゴリ（種類）ごとに別で行う（`over_limit`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LimitReport {
    pub window1_new: usize,
    pub window2_new: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateSet {
    pub candidates: Vec<Candidate>,
    /// 成立したルールの名前と、成立したファイル数（ルールの並び順）。
    pub rule_hits: Vec<(String, usize)>,
    pub limits: LimitReport,
    pub file_count: usize,
    pub files_with_info: usize,
}

impl CandidateSet {
    pub(crate) fn count_by_role(&self, role: Role) -> usize {
        self.candidates.iter().filter(|c| c.role == role).count()
    }
}

// ── 既存カテゴリの検索 ────────────────────────────────────────────────

fn find_category(cats: &[TagManagerCategoryUi], role: Role) -> CatRef {
    let idx = match role {
        Role::Format => cats.iter().position(|c| c.system_kind == Some(SystemKind::ArchiveFormat)),
        Role::Main => cats.iter().position(|c| c.is_main),
        _ => {
            let aliases: Vec<String> = role.category_aliases().iter().map(|a| normalize_key(a)).collect();
            cats.iter().position(|c| c.system_kind.is_none() && !c.is_main && aliases.contains(&normalize_key(&c.display_name())))
        }
    };
    idx.map_or(CatRef::New, CatRef::Existing)
}

/// カテゴリの要素（tier_id・名前・照合キー）。照合のたびにキーを作り直さないよう、先に作っておく。
struct ElementIndex {
    cat_idx: usize,
    /// 別名ごとに1件（tier_id・要素の全体名・別名・照合キー）。別名の無い要素は1件。
    items: Vec<(u64, String, String, Keys)>,
}

fn element_index(cats: &[TagManagerCategoryUi], cat: CatRef, opts: &MatchOptions) -> Option<ElementIndex> {
    let CatRef::Existing(idx) = cat else { return None };
    let mut items = Vec::new();
    for t in &cats[idx].tiers {
        let Some(e) = t.element.as_ref() else { continue };
        let aliases = split_aliases(e);
        if aliases.is_empty() {
            // 空白や `|` だけの名前。従来どおり全体を1件として扱う。
            items.push((t.id, e.clone(), e.clone(), keys(e, opts)));
        }
        for a in aliases {
            items.push((t.id, e.clone(), a.to_string(), keys(a, opts)));
        }
    }
    Some(ElementIndex { cat_idx: idx, items })
}

impl ElementIndex {
    /// 完全一致する要素。
    fn exact(&self, k: &Keys) -> Option<(u64, &str)> {
        if k.exact.is_empty() {
            return None;
        }
        self.items.iter().find(|(_, _, _, ek)| ek.exact == k.exact).map(|(id, n, _, _)| (*id, n.as_str()))
    }

    /// しきい値以上の類似要素（完全一致を除く）。要素ごとに、最も近い別名の類似度を使う。
    fn similar(&self, k: &Keys, threshold: u8) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = Vec::new();
        for (id, name, alias, ek) in &self.items {
            if ek.exact == k.exact {
                continue;
            }
            let Some(score) = similarity_at_least(k, ek, threshold) else { continue };
            match out.iter_mut().find(|s| s.tier_id == *id) {
                Some(s) if s.score >= score => {}
                Some(s) => {
                    s.score = score;
                    s.alias = alias.clone();
                }
                None => out.push(Suggestion { cat_idx: self.cat_idx, tier_id: *id, name: name.clone(), alias: alias.clone(), score }),
            }
        }
        // 別名のどれかが完全一致する要素は、完全一致として扱う（提案に出さない）。
        out.retain(|s| !self.items.iter().any(|(id, _, _, ek)| *id == s.tier_id && ek.exact == k.exact));
        out.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
        out
    }

    /// 向きの判定に使う、最も近い要素の類似度（しきい値未満は 0）。
    fn evidence(&self, k: &Keys, threshold: u8) -> u32 {
        if self.exact(k).is_some() {
            return 100;
        }
        self.similar(k, threshold).first().map_or(0, |s| s.score as u32)
    }
}

// ── 1ファイルから拾う言及 ─────────────────────────────────────────────

/// ファイルから拾った1件の言及。
#[derive(Clone, Debug)]
struct Mention {
    role: Role,
    /// 表記（登録名の元）。
    name: String,
    /// `[X]` 単独に由来する（作者かグループか未確定）。
    from_single: bool,
    /// 括弧の向きを推測で決めた（既存タグの裏づけが無い）。
    guessed: bool,
    /// 学習モデルが「タグにしない（無視）」と判定した（役割は、現行ルールが当てたもの）。
    ignored: bool,
    /// info.txt の別の見出し行（英語行など）の同じ位置にあった名前。対にして別名として登録する。
    alt: Option<String>,
    /// この言及を無効にしたルールの名前（`ignored` も true になる）。
    rule: Option<String>,
}

fn file_stem(input: &CandidateInput) -> &str {
    split_archive_extension(&input.file_name).0
}

fn format_name(tag: &FormatTag, lang: Lang) -> String {
    match tag {
        FormatTag::Extension(e) => e.clone(),
        FormatTag::Folder => match lang {
            Lang::English => "Folder",
            Lang::Chinese => "文件夹",
            Lang::Japanese => "フォルダ",
        }
        .to_string(),
    }
}

/// info.txt を優先し、無い部分だけファイル名で補った、作者・グループ・イベント・言語の元。
struct Sources {
    event: Option<String>,
    creator: Option<Creator>,
    language: Option<&'static str>,
    /// info.txt の別の見出し行（`title` 以外）のうち、イベント・作者の構造が同じ行の、同じ位置の名前。
    alt_event: Option<String>,
    alt_creator: Option<Creator>,
}

/// 2つの作者表記が同じ構造（単独どうし、または外側・内側どうし）か。
fn same_creator_shape(a: &Creator, b: &Creator) -> bool {
    matches!((a, b), (Creator::Single(_), Creator::Single(_)) | (Creator::Pair { .. }, Creator::Pair { .. }))
}

fn gather_sources(input: &CandidateInput) -> Sources {
    let name_parts: NameParts = parse_name_parts(file_stem(input));
    let info_parts: Option<NameParts> = input.info.as_ref().and_then(|i| i.title.as_deref()).map(parse_name_parts);
    let event = info_parts.as_ref().and_then(|p| p.event.clone()).or(name_parts.event.clone());
    let creator = info_parts.as_ref().and_then(|p| p.creator.clone()).or(name_parts.creator.clone());
    let language = input
        .info
        .as_ref()
        .and_then(InfoTxt::info_language)
        .or_else(|| info_parts.as_ref().and_then(NameParts::language))
        .or_else(|| name_parts.language());
    // 別の見出し行（英語行など）で、構造が同じものがあれば、同じ位置の名前を対にする。
    let (mut alt_event, mut alt_creator) = (None, None);
    if let (Some(info), Some(base)) = (input.info.as_ref(), info_parts.as_ref()) {
        for line in &info.alt_titles {
            let alt = parse_name_parts(line);
            let creator_ok = match (&base.creator, &alt.creator) {
                (Some(a), Some(b)) => same_creator_shape(a, b),
                (None, None) => true,
                _ => false,
            };
            if creator_ok && base.event.is_some() == alt.event.is_some() {
                alt_event = alt.event;
                alt_creator = alt.creator;
                break;
            }
        }
    }
    Sources { event, creator, language, alt_event, alt_creator }
}

/// 「正の情報」（ComicInfo.xml・meta.json）があれば、題名・メイン・言語を info.txt より優先して上書きした入力にする
/// （XML → JSON → info.txt の順。以降の抽出は、この入力を info.txt と同じに扱う）。統合した値も返す。
fn effective_input(input: &CandidateInput) -> (Cow<'_, CandidateInput>, Option<Resolved>) {
    let Some(meta) = &input.meta else { return (Cow::Borrowed(input), None) };
    let r = resolve(meta);
    let mut eff = input.clone();
    let mut info = eff.info.take().unwrap_or_default();
    let (title, alts) = split_title_group(&r.titles);
    if title.is_some() {
        info.title = title;
        info.alt_titles = alts;
    }
    if let Some(c) = r.categories.first() {
        info.category = Some(c.clone());
    }
    if let Some(l) = r.languages.first() {
        info.language = Some(l.clone());
    }
    eff.info = Some(info);
    (Cow::Owned(eff), Some(r))
}

/// 題名から拾った名前（`m.name` と、対にした別の行の表記 `m.alt`）が、「正の情報」の名前 `known` と同じ人・団体か。
/// 完全一致キーが同じか、類似度がしきい値以上。
fn same_name(m: &Mention, known: &str, opts: &MatchOptions) -> bool {
    let kk = exact_key(known, opts);
    [Some(&m.name), m.alt.as_ref()].into_iter().flatten().any(|n| exact_key(n, opts) == kk || similarity(n, known, opts) >= opts.threshold)
}

/// 「正の情報」の作者・グループ・パロディ元・キャラを、言及に反映する（1ファイルぶん）。
/// - 作者・グループが分かっているとき、題名の括弧から推測した作者・グループの言及は捨て、「正の情報」の名前を使う。
///   題名の括弧に同じ名前（表記違い・翻訳違いを含む）があれば、その表記を登録名にして、「正の情報」の表記を対の別名にする
/// - 「正の情報」と一致しなかった括弧は、その種類の名前が「正の情報」に無いときだけ残す（`[Hawk Bit (Kouji)]` で作者だけ分かっていれば、
///   残りの `Hawk Bit` はグループの候補として残る）
/// - パロディ元・キャラは、ルールで有効にしていなくても取る（「正の情報」だから）。ルールが無効にしたものは、後段で未採用・要確認になる
fn apply_authority(file_mentions: Vec<Mention>, r: &Resolved, opts: &MatchOptions) -> Vec<Mention> {
    let has_creators = !r.authors.is_empty() || !r.groups.is_empty();
    let mut out: Vec<Mention> = Vec::new();
    let mut tokens: Vec<Mention> = Vec::new();
    for m in file_mentions {
        if has_creators && matches!(m.role, Role::Author | Role::Group) {
            tokens.push(m);
        } else {
            out.push(m);
        }
    }
    let mut used = vec![false; tokens.len()];
    let plain = |role: Role, name: &str, alt: Option<String>| Mention {
        role,
        name: name.to_string(),
        from_single: false,
        guessed: false,
        ignored: false,
        alt,
        rule: None,
    };
    for (role, names) in [(Role::Author, &r.authors), (Role::Group, &r.groups)] {
        for known in names {
            let hit = (0..tokens.len()).find(|&i| !used[i] && same_name(&tokens[i], known, opts));
            match hit {
                Some(i) => {
                    used[i] = true;
                    let t = &tokens[i];
                    // 題名側の表記を登録名にし、「正の情報」の表記が別なら、対の別名にする。
                    let alt = t.alt.clone().or_else(|| (exact_key(known, opts) != exact_key(&t.name, opts)).then(|| known.clone()));
                    out.push(plain(role, &t.name, alt));
                }
                None => out.push(plain(role, known, None)),
            }
        }
    }
    for (i, mut t) in tokens.into_iter().enumerate() {
        if used[i] {
            continue;
        }
        // 一致しなかった括弧。その種類の名前が「正の情報」にあれば、矛盾するので捨てる。無ければ、もう一方の種類として残す（推測）。
        let known_for_role = match t.role {
            Role::Author => !r.authors.is_empty(),
            _ => !r.groups.is_empty(),
        };
        if known_for_role {
            let other_known = match t.role {
                Role::Author => !r.groups.is_empty(),
                _ => !r.authors.is_empty(),
            };
            if other_known {
                continue;
            }
            t.role = if t.role == Role::Author { Role::Group } else { Role::Author };
            t.guessed = true;
        }
        out.push(t);
    }
    for p in &r.parodies {
        out.push(plain(Role::Parody, p, None));
    }
    for c in &r.characters {
        out.push(plain(Role::Character, c, None));
    }
    // 属性タグ・年月は、「正の情報」があるときだけ取る（題名・txt だけのファイルからは取らない）。
    for a in &r.attributes {
        out.push(plain(Role::Attribute, a, None));
    }
    if let Some(ym) = &r.year_month {
        out.push(plain(Role::YearMonth, &ym.element_name(), None));
    }
    out
}

/// ` - ` 区切りの題名（cosplay の `演者 - キャラ (パロディ元)` / `演者 - キャラ - パロディ元` / `演者 - キャラ`）から、
/// 演者・キャラ・パロディ元の言及を作る。取るのは、ルールが有効にした種類だけ。
/// 題名は、info.txt の見出し（内部照合の行）が ` - ` 区切りならそれ、でなければファイル名。
/// info.txt の別の見出し行が同じ形なら、同じ位置の名前を対にする。
fn dash_mentions(input: &CandidateInput, outcome: &RuleOutcome) -> Vec<Mention> {
    let wanted = [RuleRole::Performer, RuleRole::Character, RuleRole::Parody];
    if !wanted.iter().any(|r| outcome.is_role_enabled(*r)) {
        return Vec::new();
    }
    let from_info = input.info.as_ref().and_then(|i| i.title.as_deref()).and_then(parse_dash_title);
    let main = match from_info.clone().or_else(|| parse_dash_title(file_stem(input))) {
        Some(d) => d,
        None => return Vec::new(),
    };
    let alt = from_info.and_then(|d| {
        input.info.as_ref()?.alt_titles.iter().filter_map(|l| parse_dash_title(l)).find(|a| a.shape == d.shape)
    });
    let mention = |role: Role, rule_role: RuleRole, name: &str, alt: Option<&String>| {
        outcome.is_role_enabled(rule_role).then(|| Mention {
            role,
            name: name.to_string(),
            from_single: false,
            guessed: false,
            ignored: false,
            alt: alt.cloned(),
            rule: None,
        })
    };
    let mut out: Vec<Mention> = Vec::new();
    out.extend(mention(Role::Performer, RuleRole::Performer, &main.first, alt.as_ref().map(|a| &a.first)));
    out.extend(mention(Role::Character, RuleRole::Character, &main.second, alt.as_ref().map(|a| &a.second)));
    if let Some(third) = &main.third {
        out.extend(mention(Role::Parody, RuleRole::Parody, third, alt.as_ref().and_then(|a| a.third.as_ref())));
    }
    out
}

/// モデルの確信度がこれ未満の作者・グループは、向き・種別を推測で決めた（未確定）ものとして扱う。
const MODEL_CONFIDENT: f64 = 0.75;

/// 学習モデルで、ファイル名（info.txt のタイトルがあればそちら）の括弧から、作者・グループ・イベント・言語の言及を作る。
/// - 既存マスターに完全一致（作者・グループ）があれば、モデルよりマスターを優先する。`[A(B)]` の向きも、マスターの裏づけが
///   ある（向きで差がつく）ときはマスターの向きにする
/// - 「無視」と判定された括弧は、現行ルールが役割を当てていたものだけ、`ignored` つきで残す（未採用・要確認にするため）
/// - 言語は info.txt の Language を優先し、無ければ辞書に合う括弧だけを言語にする
fn model_mentions(
    input: &CandidateInput,
    model: &Model,
    author_idx: &Option<ElementIndex>,
    group_idx: &Option<ElementIndex>,
    opts: &MatchOptions,
    lang: Lang,
) -> Vec<Mention> {
    let rules = gather_sources(input);
    let info_title = input.info.as_ref().and_then(|i| i.title.as_deref()).filter(|t| !crate::tag_features::tokenize(t).is_empty());
    let source = info_title.unwrap_or(&input.file_name);
    let preds = model.predict_file(source);
    // info.txt の別の見出し行（英語行など）で、括弧の構造が同じものがあれば、同じ位置の名前を対にする。
    let mut alt_texts: Vec<Option<String>> = vec![None; preds.len()];
    if info_title.is_some() {
        for line in input.info.iter().flat_map(|i| i.alt_titles.iter()) {
            let toks = crate::tag_features::tokenize(line);
            let same = toks.len() == preds.len()
                && toks.iter().zip(&preds).all(|(a, p)| a.bracket == p.token.bracket && a.slot == p.token.slot && a.nesting == p.token.nesting);
            if same {
                alt_texts = toks.into_iter().map(|t| Some(t.text)).collect();
                break;
            }
        }
    }

    // 現行ルールが、この文字列に当てていた役割（無視と判定されたものを残す根拠）。
    let rule_role = |text: &str| -> Option<Role> {
        if rules.event.as_deref() == Some(text) {
            return Some(Role::Event);
        }
        match &rules.creator {
            Some(Creator::Single(x)) if x == text => Some(Role::Author),
            Some(Creator::Pair { outer, .. }) if outer == text => Some(Role::Group),
            Some(Creator::Pair { inner, .. }) if inner == text => Some(Role::Author),
            _ => None,
        }
    };
    let exact_in = |idx: &Option<ElementIndex>, k: &Keys| idx.as_ref().is_some_and(|ix| ix.exact(k).is_some());
    let evidence = |idx: &Option<ElementIndex>, n: &str| idx.as_ref().map_or(0, |ix| ix.evidence(&keys(n, opts), opts.threshold));
    let mention = |role: Role, name: &str, from_single: bool, guessed: bool, ignored: bool| Mention {
        role,
        name: name.to_string(),
        from_single,
        guessed,
        ignored,
        alt: None,
        rule: None,
    };

    let mut out: Vec<Mention> = Vec::new();
    // info.txt の Language があれば、それが正（括弧が言語と判定されても重ねない）。
    let info_language = input.info.as_ref().and_then(InfoTxt::info_language);
    if let Some(canon) = info_language {
        let name = if lang == Lang::Japanese { canon } else { language_english_name(canon) };
        out.push(mention(Role::Language, name, false, false, false));
    }

    let mut i = 0;
    while i < preds.len() {
        let p = &preds[i];
        // 外側・内側の組は、向きをまとめて決める。
        let pair = p.token.nesting == crate::tag_features::Nesting::Outer
            && preds.get(i + 1).is_some_and(|q| q.token.nesting == crate::tag_features::Nesting::Inner);
        if pair {
            let q = &preds[i + 1];
            let (mut lo, mut li) = (p.label, q.label);
            let mut guessed = p.confidence < MODEL_CONFIDENT;
            let creator_pair = matches!((lo, li), (Label::Author, Label::Group) | (Label::Group, Label::Author));
            if creator_pair {
                // マスターの裏づけがあり、向きで差がつくなら、マスターの向き。
                let normal = evidence(group_idx, &p.token.text) + evidence(author_idx, &q.token.text);
                let swapped = evidence(author_idx, &p.token.text) + evidence(group_idx, &q.token.text);
                if normal != swapped {
                    (lo, li) = if swapped > normal { (Label::Author, Label::Group) } else { (Label::Group, Label::Author) };
                    guessed = false;
                } else if normal != 0 {
                    guessed = false;
                }
            }
            for (n, (tok, label)) in [(&p.token, lo), (&q.token, li)].into_iter().enumerate() {
                let alt = alt_texts[i + n].as_deref();
                push_labeled(&mut out, tok.text.as_str(), label, false, guessed, alt, &rule_role, &mention, lang, info_language.is_some());
            }
            i += 2;
            continue;
        }
        // 1つだけの括弧。作者・グループは、マスターに完全一致があればその側。
        let mut label = p.label;
        let mut from_single = false;
        let mut guessed = false;
        if matches!(label, Label::Author | Label::Group) {
            let k = keys(&p.token.text, opts);
            match (exact_in(author_idx, &k), exact_in(group_idx, &k)) {
                (true, _) => label = Label::Author,
                (false, true) => label = Label::Group,
                (false, false) => {
                    from_single = p.confidence < MODEL_CONFIDENT;
                    guessed = from_single;
                }
            }
        }
        push_labeled(&mut out, p.token.text.as_str(), label, from_single, guessed, alt_texts[i].as_deref(), &rule_role, &mention, lang, info_language.is_some());
        i += 1;
    }
    out
}

/// 種別が決まった括弧1つを、言及として足す。
#[allow(clippy::too_many_arguments)]
fn push_labeled(
    out: &mut Vec<Mention>,
    text: &str,
    label: Label,
    from_single: bool,
    guessed: bool,
    alt: Option<&str>,
    rule_role: &dyn Fn(&str) -> Option<Role>,
    mention: &dyn Fn(Role, &str, bool, bool, bool) -> Mention,
    lang: Lang,
    info_language_given: bool,
) {
    match label {
        Label::Author | Label::Group | Label::Event => {
            let role = match label {
                Label::Author => Role::Author,
                Label::Group => Role::Group,
                _ => Role::Event,
            };
            let single = from_single && role != Role::Event;
            let mut m = mention(role, text, single, if role == Role::Event { false } else { guessed }, false);
            m.alt = alt.map(str::to_string);
            out.push(m);
        }
        Label::Language => {
            // 辞書に合う言語だけ。info.txt の Language があれば、それを優先して重ねない。
            if !info_language_given {
                if let Some(canon) = canonical_language(text) {
                    let name = if lang == Lang::Japanese { canon } else { language_english_name(canon) };
                    out.push(mention(Role::Language, name, false, false, false));
                }
            }
        }
        Label::Ignore => {
            if let Some(role) = rule_role(text) {
                let single = matches!(role, Role::Author | Role::Group) && from_single;
                out.push(mention(role, text, single, false, true));
            }
        }
    }
}

/// 提案を、要素ごとに最も高い類似度の1件にして、類似度の降順に並べる。
fn dedupe_suggestions(mut v: Vec<Suggestion>) -> Vec<Suggestion> {
    v.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    let mut seen: Vec<u64> = Vec::new();
    v.retain(|s| {
        if seen.contains(&s.tier_id) {
            false
        } else {
            seen.push(s.tier_id);
            true
        }
    });
    v
}

// ── 候補生成 ──────────────────────────────────────────────────────────

/// 候補を作る（学習モデルなし。現行ルールだけ）。`cats` は現在のタグマスター（読み取りのみ）。
pub(crate) fn build_candidates(
    inputs: &[CandidateInput],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
) -> CandidateSet {
    build_candidates_with(inputs, cats, opts, lang, None)
}

/// 候補を作る（学習モデルつき・ルールなし）。
pub(crate) fn build_candidates_with(
    inputs: &[CandidateInput],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    model: Option<&Model>,
) -> CandidateSet {
    build_candidates_full(inputs, cats, opts, lang, lang, model, &[])
}

/// 候補を作る。`model` があれば、括弧の種別（作者・グループ・イベント・言語・無視）を学習モデルで判定する。
/// 既存マスターに完全一致があるものは、モデルよりマスターを優先する。
/// `name_lang` は、新規要素の主表記を選ぶ言語（登録表記の設定。表示言語に合わせるときは `lang` と同じ）。
/// `rules`（大ルール）は抽出の下処理: ファイルごとに成立したルールが無効にした種類の言及は、候補にして未採用・要確認で残す
/// （既存マスターに完全一致があっても、ルールを優先する）。
pub(crate) fn build_candidates_full(
    inputs: &[CandidateInput],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    name_lang: Lang,
    model: Option<&Model>,
    rules: &[TagRule],
) -> CandidateSet {
    let opts = opts.clamped();
    let cat_of = |role: Role| find_category(cats, role);
    let (author_cat, group_cat) = (cat_of(Role::Author), cat_of(Role::Group));
    let author_idx = element_index(cats, author_cat, &opts);
    let group_idx = element_index(cats, group_cat, &opts);

    // 1) ファイルごとの言及を集める。
    let mut mentions: Vec<(usize, Mention)> = Vec::new();
    let mut files_with_info = 0usize;
    let mut outcomes: Vec<RuleOutcome> = Vec::new();
    let mut resolved_all: Vec<Option<Resolved>> = Vec::new();
    for (fi, input_ref) in inputs.iter().enumerate() {
        // 「正の情報」（ComicInfo.xml・meta.json）があれば、題名・メイン・言語を info.txt より優先する。
        let (effective, resolved) = effective_input(input_ref);
        let input: &CandidateInput = &effective;
        resolved_all.push(resolved);
        if !rules.is_empty() {
            outcomes.push(evaluate_rules(rules, &RuleInput::new(&input.file_name, input.info.as_ref()), &opts));
            // 大ルールが有効にした新種類（演者・キャラ・パロディ元）は、` - ` 区切りの題名から取る。
            let dash = dash_mentions(input, &outcomes[fi]);
            mentions.extend(dash.into_iter().map(|m| (fi, m)));
        }
        if input.info.is_some() {
            files_with_info += 1;
        }
        if let Some(tag) = &input.format {
            mentions.push((fi, Mention { role: Role::Format, name: format_name(tag, lang), from_single: false, guessed: false, ignored: false, alt: None, rule: None }));
        }
        if let Some(cat) = input.info.as_ref().and_then(|i| i.category.clone()) {
            mentions.push((fi, Mention { role: Role::Main, name: cat, from_single: false, guessed: false, ignored: false, alt: None, rule: None }));
        }
        if let Some(model) = model {
            mentions.extend(model_mentions(input, model, &author_idx, &group_idx, &opts, lang).into_iter().map(|m| (fi, m)));
            continue;
        }
        let src = gather_sources(input);
        if let Some(canon) = src.language {
            let name = if lang == Lang::Japanese { canon } else { language_english_name(canon) };
            mentions.push((fi, Mention { role: Role::Language, name: name.to_string(), from_single: false, guessed: false, ignored: false, alt: None, rule: None }));
        }
        // 日本語行のイベント・作者と、別の見出し行の同じ位置の名前を対にする（info.txt の見出しがあるときだけ）。
        let alt_ev = src.alt_event.clone();
        let (alt_outer, alt_inner, alt_single) = match &src.alt_creator {
            Some(Creator::Pair { outer, inner }) => (Some(outer.clone()), Some(inner.clone()), None),
            Some(Creator::Single(x)) => (None, None, Some(x.clone())),
            None => (None, None, None),
        };
        if let Some(ev) = src.event {
            mentions.push((fi, Mention { role: Role::Event, name: ev, from_single: false, guessed: false, ignored: false, alt: alt_ev, rule: None }));
        }
        match src.creator {
            Some(Creator::Pair { outer, inner }) => {
                let ev = |idx: &Option<ElementIndex>, n: &str| idx.as_ref().map_or(0, |ix| ix.evidence(&keys(n, &opts), opts.threshold));
                let normal = ev(&group_idx, &outer) + ev(&author_idx, &inner);
                let swapped = ev(&author_idx, &outer) + ev(&group_idx, &inner);
                let swap = swapped > normal;
                let (outer_role, inner_role) = if swap { (Role::Author, Role::Group) } else { (Role::Group, Role::Author) };
                let guessed = normal == 0 && swapped == 0;
                mentions.push((fi, Mention { role: outer_role, name: outer, from_single: false, guessed, ignored: false, alt: alt_outer, rule: None }));
                mentions.push((fi, Mention { role: inner_role, name: inner, from_single: false, guessed, ignored: false, alt: alt_inner, rule: None }));
            }
            Some(Creator::Single(x)) => {
                // 既存の作者・グループに完全一致があれば、その側。無ければ作者の候補（未確定）。
                let k = keys(&x, &opts);
                let in_author = author_idx.as_ref().is_some_and(|ix| ix.exact(&k).is_some());
                let in_group = group_idx.as_ref().is_some_and(|ix| ix.exact(&k).is_some());
                let role = if !in_author && in_group { Role::Group } else { Role::Author };
                mentions.push((fi, Mention { role, name: x, from_single: true, guessed: false, ignored: false, alt: alt_single, rule: None }));
            }
            None => {}
        }
    }

    // 1a) 「正の情報」の作者・グループ・パロディ元・キャラを反映する。
    if resolved_all.iter().any(Option::is_some) {
        let mut adjusted: Vec<(usize, Mention)> = Vec::with_capacity(mentions.len());
        let mut old = std::mem::take(&mut mentions).into_iter().peekable();
        for (fi, resolved) in resolved_all.iter().enumerate() {
            let mut mine: Vec<Mention> = Vec::new();
            while old.peek().is_some_and(|(f, _)| *f == fi) {
                mine.push(old.next().unwrap().1);
            }
            let out = match resolved {
                Some(r) => apply_authority(mine, r, &opts),
                None => mine,
            };
            adjusted.extend(out.into_iter().map(|m| (fi, m)));
        }
        mentions = adjusted;
    }

    // 1b) ルールが無効にした種類の言及は、無効にしたルールの名前を付けて「無視」扱いにする（未採用・要確認で残す）。
    let mut rule_hits: Vec<(String, usize)> = Vec::new();
    if !rules.is_empty() {
        for rule in rules {
            let n = outcomes.iter().filter(|o| o.applied.contains(&rule.name)).count();
            if n > 0 && !rule_hits.iter().any(|(name, _)| *name == rule.name) {
                rule_hits.push((rule.name.clone(), n));
            }
        }
        for (fi, m) in &mut mentions {
            let role = match m.role {
                Role::Format => RuleRole::Format,
                Role::Main => RuleRole::Main,
                Role::Language => RuleRole::Language,
                Role::Event => RuleRole::Event,
                Role::Author => RuleRole::Author,
                Role::Group => RuleRole::Group,
                Role::Performer => RuleRole::Performer,
                Role::Character => RuleRole::Character,
                Role::Parody => RuleRole::Parody,
                Role::Attribute => RuleRole::Attribute,
                Role::YearMonth => RuleRole::YearMonth,
            };
            if let Some(reason) = outcomes[*fi].disabled_reason(role) {
                m.ignored = true;
                m.rule = Some(reason);
            }
        }
    }

    // 属性タグの照合用の索引は、属性タグの言及があるときだけ作る。
    let has_attribute_mentions = mentions.iter().any(|(_, m)| m.role == Role::Attribute);

    // 2) 種類＋完全一致キーで候補にまとめる。
    struct Acc {
        role: Role,
        key: String,
        forms: Vec<(String, usize)>,
        files: Vec<usize>,
        from_single: bool,
        guessed: bool,
        /// すべての言及が、モデルに「無視」と判定されたもの。
        ignored: bool,
        /// 対にした別の見出し行の表記（重複を除く）。
        alts: Vec<String>,
        /// 言及を無効にしたルールの名前（重複を除く）。
        rule_reasons: Vec<String>,
    }
    let mut order: Vec<(Role, String)> = Vec::new();
    let mut accs: HashMap<(Role, String), Acc> = HashMap::new();
    for (fi, m) in mentions {
        let k = keys(&m.name, &opts);
        // 言語は辞書の正規名で、同じ言語を1つにまとめる（英語名・日本語名の表記違いを統一）。
        let ident = if m.role == Role::Language {
            canonical_language(&m.name).map_or(k.exact.clone(), |c| c.to_string())
        } else {
            k.exact.clone()
        };
        if ident.is_empty() {
            continue;
        }
        let acc = accs.entry((m.role, ident.clone())).or_insert_with(|| {
            order.push((m.role, ident.clone()));
            Acc { role: m.role, key: ident.clone(), forms: Vec::new(), files: Vec::new(), from_single: false, guessed: false, ignored: true, alts: Vec::new(), rule_reasons: Vec::new() }
        });
        match acc.forms.iter_mut().find(|(f, _)| *f == m.name) {
            Some((_, n)) => *n += 1,
            None => acc.forms.push((m.name.clone(), 1)),
        }
        if acc.files.last() != Some(&fi) && !acc.files.contains(&fi) {
            acc.files.push(fi);
        }
        acc.from_single |= m.from_single;
        acc.guessed |= m.guessed;
        acc.ignored &= m.ignored;
        if let Some(r) = &m.rule {
            if !acc.rule_reasons.contains(r) {
                acc.rule_reasons.push(r.clone());
            }
        }
        if let Some(a) = &m.alt {
            if !acc.alts.contains(a) {
                acc.alts.push(a.clone());
            }
        }
    }

    // 3) 候補ごとに、既存タグとの照合・初期の扱い・登録名を決める。
    let format_ix = element_index(cats, cat_of(Role::Format), &opts);
    let main_ix = element_index(cats, cat_of(Role::Main), &opts);
    let lang_ix = element_index(cats, cat_of(Role::Language), &opts);
    let event_ix = element_index(cats, cat_of(Role::Event), &opts);
    let performer_ix = element_index(cats, cat_of(Role::Performer), &opts);
    let character_ix = element_index(cats, cat_of(Role::Character), &opts);
    let parody_ix = element_index(cats, cat_of(Role::Parody), &opts);
    let year_month_ix = element_index(cats, cat_of(Role::YearMonth), &opts);
    // 属性タグは、メイン・システム以外のどのカテゴリにある要素にも完全一致で付ける（無ければ「属性」カテゴリに新規作成）。
    let attribute_cat = cat_of(Role::Attribute);
    let attribute_ixs: Vec<ElementIndex> = if has_attribute_mentions {
        (0..cats.len())
            .filter(|&i| !cats[i].is_main && cats[i].system_kind.is_none())
            .filter_map(|i| element_index(cats, CatRef::Existing(i), &opts))
            .collect()
    } else {
        Vec::new()
    };
    let mut candidates: Vec<Candidate> = Vec::new();
    for (role, ident) in order {
        let acc = accs.remove(&(role, ident)).unwrap();
        // 対にした別の見出し行の表記（英語⇄日本語）も含めて、UIの言語に合う表記を登録名にし、残りは別名にする。
        let mut forms_all = acc.forms.clone();
        for a in &acc.alts {
            let ak = keys(a, &opts).exact;
            if !forms_all.iter().any(|(f, _)| keys(f, &opts).exact == ak) {
                forms_all.push((a.clone(), 1));
            }
        }
        let picked = pick_registration_name(&forms_all, name_lang);
        // 全角半角の正規化がONなら、登録名も正規化する（半角カナ・全角英数をそのまま登録しない）。
        let norm = |s: &str| if opts.width { normalize_width(s) } else { s.to_string() };
        let name = norm(&picked);
        let k = keys(&name, &opts);
        let mut alt_forms: Vec<String> = Vec::new();
        if matches!(role, Role::Author | Role::Group | Role::Event | Role::Performer | Role::Character | Role::Parody) {
            let mut seen = vec![k.exact.clone()];
            for (f, _) in &forms_all {
                let (nf, fk) = (norm(f), keys(f, &opts).exact);
                if !fk.is_empty() && !seen.contains(&fk) {
                    seen.push(fk);
                    alt_forms.push(nf);
                }
            }
        }
        // 既存タグの照合に使うキー（登録名と、対にした別の表記）。
        let mut match_keys: Vec<Keys> = vec![k.clone()];
        match_keys.extend(alt_forms.iter().map(|f| keys(f, &opts)));
        let category = cat_of(role);
        let mut suggestions: Vec<Suggestion> = Vec::new();
        let mut existing: Option<(usize, u64)> = None;

        match role {
            Role::Language => {
                // 辞書の正規名が同じ要素（日本語名でも英語名でも）を、既存の言語タグとみなす。
                if let Some(ix) = &lang_ix {
                    existing = ix
                        .items
                        .iter()
                        .find(|(_, _, a, ek)| canonical_language(a).is_some_and(|c| c == acc.key) || ek.exact == k.exact)
                        .map(|(id, _, _, _)| (ix.cat_idx, *id));
                }
            }
            Role::Attribute => {
                // どのカテゴリにあっても、完全一致する要素に付ける。見つからなければ新規作成（提案は出さない）。
                existing = attribute_ixs.iter().find_map(|ix| match_keys.iter().find_map(|mk| ix.exact(mk).map(|(id, _)| (ix.cat_idx, id))));
            }
            Role::Format | Role::Main | Role::Event | Role::Performer | Role::Character | Role::Parody | Role::YearMonth => {
                let ix = match role {
                    Role::Format => &format_ix,
                    Role::Main => &main_ix,
                    Role::Performer => &performer_ix,
                    Role::Character => &character_ix,
                    Role::Parody => &parody_ix,
                    Role::YearMonth => &year_month_ix,
                    _ => &event_ix,
                };
                if let Some(ix) = ix {
                    existing = match_keys.iter().find_map(|mk| ix.exact(mk).map(|(id, _)| (ix.cat_idx, id)));
                    // 形式・年月は完全一致だけ（年月は似た文字列ばかりなので、類似を提案しない）。それ以外は類似も提案する。
                    if existing.is_none() && !matches!(role, Role::Format | Role::YearMonth) {
                        for mk in &match_keys {
                            suggestions.extend(ix.similar(mk, opts.threshold));
                        }
                        suggestions = dedupe_suggestions(suggestions);
                    }
                }
            }
            Role::Author | Role::Group => {
                // 単独の `[X]` は作者・グループの両方から探す。括弧つきは、決まった側だけ。
                let own = if role == Role::Author { &author_idx } else { &group_idx };
                let other = if role == Role::Author { &group_idx } else { &author_idx };
                let mut pools: Vec<&ElementIndex> = own.iter().collect();
                if acc.from_single {
                    pools.extend(other.iter());
                }
                for ix in &pools {
                    if existing.is_none() {
                        existing = match_keys.iter().find_map(|mk| ix.exact(mk).map(|(id, _)| (ix.cat_idx, id)));
                    }
                }
                if existing.is_none() {
                    for ix in &pools {
                        for mk in &match_keys {
                            suggestions.extend(ix.similar(mk, opts.threshold));
                        }
                    }
                    suggestions = dedupe_suggestions(suggestions);
                }
            }
        }
        suggestions.truncate(MAX_SUGGESTIONS);
        // 対にした表記があり、既存の要素がそのすべての表記を別名に持っていなければ、足りない表記を別名として足す候補にする。
        let alias_needed = !alt_forms.is_empty()
            && existing.is_some_and(|(cat_idx, tier_id)| {
                [&author_idx, &group_idx, &event_ix, &performer_ix, &character_ix, &parody_ix].into_iter().flatten().find(|ix| ix.cat_idx == cat_idx).is_some_and(|ix| {
                    !match_keys.iter().all(|mk| ix.items.iter().any(|(id, _, _, ek)| *id == tier_id && ek.exact == mk.exact))
                })
            });
        let choice = match (existing, suggestions.is_empty()) {
            (Some((cat_idx, tier_id)), _) if alias_needed => Choice::Alias { cat_idx, tier_id },
            (Some((cat_idx, tier_id)), _) => Choice::Existing { cat_idx, tier_id },
            (None, false) => Choice::Skip,
            (None, true) => Choice::New,
        };
        // モデルが「無視」と判定したものは、既存タグの完全一致が無い限り、未採用にして残す（要確認）。
        // ルールが無効にしたものは、既存マスターに一致していても、ルールを優先して未採用にする。
        let rule_disabled = acc.ignored && !acc.rule_reasons.is_empty();
        let review = rule_disabled || (acc.ignored && !matches!(choice, Choice::Existing { .. }));
        let choice = if review { Choice::Skip } else { choice };
        let rule_reason = rule_disabled.then(|| acc.rule_reasons.join("、"));
        let unconfirmed = matches!(role, Role::Author | Role::Group)
            && !matches!(choice, Choice::Existing { .. })
            && (acc.from_single || acc.guessed);
        candidates.push(Candidate {
            role,
            name,
            forms: acc.forms.iter().map(|(f, _)| f.clone()).collect(),
            key: acc.key,
            files: acc.files,
            category,
            initial: choice.clone(),
            choice,
            suggestions,
            unconfirmed,
            cluster: 0,
            aliases: Vec::new(),
            members: Vec::new(),
            bundle_id: None,
            review,
            alt_forms,
            rule_reason,
        });
    }

    // 4) 類似品の束（同じ種類の中で）。番号は種類をまたいで一意にする。
    let mut next_cluster = 0usize;
    for role in [Role::Format, Role::Main, Role::Language, Role::Event, Role::Parody, Role::Character, Role::Author, Role::Group, Role::Performer] {
        let idxs: Vec<usize> = (0..candidates.len()).filter(|&i| candidates[i].role == role).collect();
        let names: Vec<String> = idxs.iter().map(|&i| candidates[i].name.clone()).collect();
        let ids = cluster(&names, &opts);
        let base = next_cluster;
        for (pos, &i) in idxs.iter().enumerate() {
            candidates[i].cluster = base + ids[pos];
            next_cluster = next_cluster.max(base + ids[pos] + 1);
        }
    }

    // 属性タグ・年月は、似た名前でも束ねない（候補ごとに別の番号にする）。
    for c in candidates.iter_mut().filter(|c| matches!(c.role, Role::Attribute | Role::YearMonth)) {
        c.cluster = next_cluster;
        next_cluster += 1;
    }

    let limits = check_limits(&candidates);
    CandidateSet { candidates, rule_hits, limits, file_count: inputs.len(), files_with_info }
}

/// 登録名を表記の中から選ぶ。UIが日本語なら日本語（ASCII以外）を含む表記を、それ以外は英字だけの表記を優先し、
/// 同じ条件の中では出現数の多いものを選ぶ。
fn pick_registration_name(forms: &[(String, usize)], lang: Lang) -> String {
    let prefers_japanese = lang == Lang::Japanese;
    let is_japanese = |s: &str| !s.is_ascii();
    forms
        .iter()
        .max_by(|(a, na), (b, nb)| {
            let pa = is_japanese(a) == prefers_japanese;
            let pb = is_japanese(b) == prefers_japanese;
            pa.cmp(&pb).then(na.cmp(nb)).then(b.cmp(a))
        })
        .map(|(f, _)| f.clone())
        .unwrap_or_default()
}

/// 採否を変えたあとの集計をやり直す（確認ウィンドウ用）。
pub(crate) fn recount_limits(candidates: &[Candidate]) -> LimitReport {
    check_limits(candidates)
}

/// 窓ごとの新規要素数を数える。
fn check_limits(candidates: &[Candidate]) -> LimitReport {
    let count = |w: Window| candidates.iter().filter(|c| c.role.window() == w && c.needs_new_element()).count();
    LimitReport { window1_new: count(Window::One), window2_new: count(Window::Two) }
}

/// 上限（`limit`）を超えているカテゴリ（種類）と、その新規要素数。種類の並び順。
pub(crate) fn over_limit(candidates: &[Candidate], limit: usize) -> Vec<(Role, usize)> {
    let mut counts: HashMap<Role, usize> = HashMap::new();
    for c in candidates.iter().filter(|c| c.needs_new_element()) {
        *counts.entry(c.role).or_default() += 1;
    }
    let mut over: Vec<(Role, usize)> = counts.into_iter().filter(|&(_, n)| n > limit).collect();
    over.sort_by_key(|&(r, _)| r.order());
    over
}

impl CandidateSet {
    /// 上限を超えているカテゴリごとに、新規要素のうち該当ファイル数の多いものから `limit` 個だけ残し、残りを候補から除く
    /// （同数なら名前順）。既存の要素に一致する候補は数えず、触らない。除いた数を種類ごとに返す（除いたものが無ければ空）。
    pub(crate) fn truncate_to_limit(&mut self, limit: usize) -> Vec<(Role, usize)> {
        let mut removed: Vec<(Role, usize)> = Vec::new();
        for (role, _) in over_limit(&self.candidates, limit) {
            let mut idx: Vec<usize> =
                (0..self.candidates.len()).filter(|&i| self.candidates[i].role == role && self.candidates[i].needs_new_element()).collect();
            idx.sort_by(|&a, &b| {
                let (ca, cb) = (&self.candidates[a], &self.candidates[b]);
                cb.files.len().cmp(&ca.files.len()).then(ca.name.to_lowercase().cmp(&cb.name.to_lowercase())).then(a.cmp(&b))
            });
            let drop: HashSet<usize> = idx.into_iter().skip(limit).collect();
            removed.push((role, drop.len()));
            let mut i = 0;
            self.candidates.retain(|_| {
                let keep = !drop.contains(&i);
                i += 1;
                keep
            });
        }
        self.limits = check_limits(&self.candidates);
        removed
    }

    /// 窓ごとの候補の番号（種類順、束の最初の出現順、名前順）。
    pub(crate) fn indices_in_window(&self, w: Window) -> Vec<usize> {
        let order = |r: Role| r.order();
        let mut idx: Vec<usize> = (0..self.candidates.len()).filter(|&i| self.candidates[i].role.window() == w).collect();
        idx.sort_by(|&a, &b| {
            let (ca, cb) = (&self.candidates[a], &self.candidates[b]);
            order(ca.role)
                .cmp(&order(cb.role))
                .then(ca.cluster.cmp(&cb.cluster))
                .then(ca.name.to_lowercase().cmp(&cb.name.to_lowercase()))
        });
        idx
    }

    /// 再生成した候補 `fresh` に、ユーザーが変えた扱い（`touched`: 種類＋キー → 扱い）を引き継ぐ。
    /// 同じ種類・キーの候補が無ければ捨てる。
    pub(crate) fn carry_over_choices(&self, fresh: &mut CandidateSet) {
        let old: HashMap<(Role, &str), &Candidate> = self.candidates.iter().map(|c| ((c.role, c.key.as_str()), c)).collect();
        for c in &mut fresh.candidates {
            if let Some(o) = old.get(&(c.role, c.key.as_str())) {
                if o.choice != o.initial {
                    c.choice = o.choice.clone();
                }
            }
        }
        fresh.limits = check_limits(&fresh.candidates);
    }
}

// ── 手動のまとめ（別名つき1要素） ──────────────────────────────────────

/// まとめを拒否する理由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MergeError {
    /// 2件未満。
    TooFew,
    /// 種類が違う候補が混じっている。
    MixedRoles,
    /// 異なる既存要素に一致する候補が複数ある（既存要素どうしの統合は非対応）。
    MultipleExisting { first: String, second: String },
    /// 別名が他の既存要素の別名と重なる。
    AliasOverlap { alias: String, element: String },
}

/// まとめの記録（再生成した候補へ同じまとめを復元するための情報）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bundle {
    pub id: u64,
    pub role: Role,
    /// まとめた候補の表記（再生成後に、どの候補をまとめるかを探す手がかり）。
    pub forms: Vec<String>,
    pub aliases: Vec<String>,
}

fn element_of(cats: &[TagManagerCategoryUi], cat_idx: usize, tier_id: u64) -> Option<&str> {
    cats.get(cat_idx)?.tiers.iter().find(|t| t.id == tier_id).and_then(|t| t.element.as_deref())
}

impl CandidateSet {
    /// 候補 `idxs` を1つにまとめ、まとめた候補の番号を返す。
    /// 既存の完全一致が1つあればその要素への別名追加、無ければ別名つきの新規要素。`aliases` が None なら各候補の名前を並べる。
    pub(crate) fn merge_candidates(
        &mut self,
        idxs: &[usize],
        aliases: Option<Vec<String>>,
        bundle_id: u64,
        cats: &[TagManagerCategoryUi],
    ) -> Result<usize, MergeError> {
        let mut idxs: Vec<usize> = idxs.iter().copied().filter(|&i| i < self.candidates.len()).collect();
        idxs.sort_unstable();
        idxs.dedup();
        if idxs.len() < 2 {
            return Err(MergeError::TooFew);
        }
        let role = self.candidates[idxs[0]].role;
        if idxs.iter().any(|&i| self.candidates[i].role != role) {
            return Err(MergeError::MixedRoles);
        }
        // 元の候補（既にまとめたものは中身に展開する）。
        let mut leaves: Vec<Candidate> = Vec::new();
        for &i in &idxs {
            let c = &self.candidates[i];
            if c.members.is_empty() {
                leaves.push(c.clone());
            } else {
                leaves.extend(c.members.iter().cloned());
            }
        }
        // 既存の完全一致（まとめた候補の別名追加も含む）の対象。
        let mut target: Option<(usize, u64)> = None;
        for l in &leaves {
            if let Choice::Existing { cat_idx, tier_id } | Choice::Alias { cat_idx, tier_id } = l.initial {
                match target {
                    None => target = Some((cat_idx, tier_id)),
                    Some((_, t)) if t == tier_id => {}
                    Some((c0, t0)) => {
                        let name = |c: usize, t: u64| element_of(cats, c, t).map_or_else(|| format!("#{t}"), String::from);
                        return Err(MergeError::MultipleExisting { first: name(c0, t0), second: name(cat_idx, tier_id) });
                    }
                }
            }
        }
        // 別名の並び（重複は除く）。
        let aliases = aliases.unwrap_or_else(|| {
            let mut v: Vec<String> = Vec::new();
            for l in &leaves {
                let a = escape_alias_sep(l.name.trim());
                if !a.is_empty() && !v.iter().any(|x| alias_keys(x) == alias_keys(&a)) {
                    v.push(a);
                }
            }
            v
        });
        // 別名が、対象以外の既存要素の別名と重ならないこと。
        let cat_idx = match (target, leaves[0].category) {
            (Some((c, _)), _) => Some(c),
            (None, CatRef::Existing(c)) => Some(c),
            _ => None,
        };
        if let Some(ci) = cat_idx {
            for a in &aliases {
                for (id, element) in overlapping_elements(cats, ci, a) {
                    if target.map(|(_, t)| t) != Some(id) {
                        return Err(MergeError::AliasOverlap { alias: a.clone(), element });
                    }
                }
            }
        }
        let covered = target.is_some_and(|(c, t)| {
            let have = element_of(cats, c, t).map(alias_keys).unwrap_or_default();
            aliases.iter().all(|a| alias_keys(a).iter().all(|k| have.contains(k)))
        });
        let initial = match target {
            Some((cat_idx, tier_id)) if covered => Choice::Existing { cat_idx, tier_id },
            Some((cat_idx, tier_id)) => Choice::Alias { cat_idx, tier_id },
            None => Choice::New,
        };
        let all_skipped = leaves.iter().all(|l| !l.is_adopted());
        let mut forms: Vec<String> = Vec::new();
        let mut files: Vec<usize> = Vec::new();
        for l in &leaves {
            for f in &l.forms {
                if !forms.contains(f) {
                    forms.push(f.clone());
                }
            }
            files.extend(l.files.iter().copied());
        }
        files.sort_unstable();
        files.dedup();
        let merged = Candidate {
            role,
            name: join_aliases(&aliases),
            forms,
            key: format!("bundle:{bundle_id}"),
            files,
            category: leaves[0].category,
            choice: if all_skipped { Choice::Skip } else { initial.clone() },
            initial,
            suggestions: Vec::new(),
            unconfirmed: leaves.iter().all(|l| l.unconfirmed),
            cluster: leaves.iter().map(|l| l.cluster).min().unwrap_or(0),
            aliases,
            review: leaves.iter().all(|l| l.review),
            alt_forms: Vec::new(),
            rule_reason: leaves.iter().all(|l| l.rule_reason.is_some()).then(|| {
                let mut names: Vec<String> = Vec::new();
                for r in leaves.iter().filter_map(|l| l.rule_reason.as_deref()) {
                    if !names.iter().any(|n| n == r) {
                        names.push(r.to_string());
                    }
                }
                names.join("、")
            }),
            members: leaves,
            bundle_id: Some(bundle_id),
        };
        for &i in idxs.iter().rev() {
            self.candidates.remove(i);
        }
        self.candidates.push(merged);
        self.limits = check_limits(&self.candidates);
        Ok(self.candidates.len() - 1)
    }

    /// まとめた候補を元の候補に分解する。まとめていなければ false。
    pub(crate) fn split_candidate(&mut self, idx: usize) -> bool {
        if self.candidates.get(idx).is_none_or(|c| c.members.is_empty()) {
            return false;
        }
        let c = self.candidates.remove(idx);
        self.candidates.extend(c.members);
        self.limits = check_limits(&self.candidates);
        true
    }

    /// まとめた候補の、先頭見出しにする別名を `pos` 番目に変える。
    pub(crate) fn set_primary_alias(&mut self, idx: usize, pos: usize) {
        let Some(c) = self.candidates.get_mut(idx) else { return };
        if pos == 0 || pos >= c.aliases.len() {
            return;
        }
        let a = c.aliases.remove(pos);
        c.aliases.insert(0, a);
        c.name = join_aliases(&c.aliases);
    }

    /// いまのまとめの記録。
    pub(crate) fn snapshot_bundles(&self) -> Vec<Bundle> {
        self.candidates
            .iter()
            .filter_map(|c| {
                c.bundle_id.map(|id| Bundle { id, role: c.role, forms: c.forms.clone(), aliases: c.aliases.clone() })
            })
            .collect()
    }

    /// 再生成した候補に、記録したまとめを復元する（表記が重なる、まとめていない同じ種類の候補を集める）。
    /// 2件未満になった・拒否されたまとめは復元しない。
    pub(crate) fn apply_bundles(&mut self, bundles: &[Bundle], cats: &[TagManagerCategoryUi]) {
        for b in bundles {
            let idxs: Vec<usize> = (0..self.candidates.len())
                .filter(|&i| {
                    let c = &self.candidates[i];
                    c.bundle_id.is_none() && c.role == b.role && c.forms.iter().any(|f| b.forms.contains(f))
                })
                .collect();
            let _ = self.merge_candidates(&idxs, Some(b.aliases.clone()), b.id, cats);
        }
    }
}

/// info.txt の本文から入力を作る（走査結果の変換用）。本文が空でも `Some`（info.txt はあった）。
pub(crate) fn input_from_info_text(file_name: &str, format: Option<FormatTag>, info_text: Option<&str>) -> CandidateInput {
    CandidateInput { file_name: file_name.to_string(), format, info: info_text.map(parse_info_txt), meta: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_explorer::TagManagerTierUi;

    fn tier(id: u64, name: &str) -> TagManagerTierUi {
        TagManagerTierUi { id, tier_no: id as i32, negative: false, element: Some(name.to_string()) }
    }

    fn cat(name: &str, is_main: bool, kind: Option<SystemKind>, tiers: Vec<TagManagerTierUi>) -> TagManagerCategoryUi {
        TagManagerCategoryUi {
            name: name.to_string(),
            tiers,
            color: egui::Color32::GRAY,
            is_main,
            single_select: is_main,
            system_kind: kind,
        }
    }

    /// メイン(漫画) / アーカイブ形式(zip,cbz) / 作者(Kouji) / グループ(Hawk Bit) / 言語(日本語)
    fn master() -> Vec<TagManagerCategoryUi> {
        vec![
            cat("main", true, None, vec![tier(1, "漫画"), tier(2, "Doujinshi")]),
            cat("fmt", false, Some(SystemKind::ArchiveFormat), vec![tier(10, "zip"), tier(11, "cbz")]),
            cat("作者", false, None, vec![tier(20, "Kouji")]),
            cat("グループ", false, None, vec![tier(30, "Hawk Bit")]),
            cat("言語", false, None, vec![tier(40, "日本語")]),
        ]
    }

    fn input(name: &str, fmt: &str, info: Option<&str>) -> CandidateInput {
        input_from_info_text(name, Some(FormatTag::Extension(fmt.to_string())), info)
    }

    const SAMPLE_INFO: &str =
        "(C105) [Hawk Bit (Kouji)] Takahama-kun wa DoM\n<download url>\n\nCategory: Doujinshi\nUploader: x\nLanguage: Japanese\n";

    fn find<'a>(set: &'a CandidateSet, role: Role, name: &str) -> &'a Candidate {
        set.candidates.iter().find(|c| c.role == role && c.name == name).unwrap_or_else(|| panic!("{role:?} {name} が無い: {:?}", set.candidates.iter().map(|c| (c.role, &c.name)).collect::<Vec<_>>()))
    }

    #[test]
    fn sample_matches_existing_tags_and_proposes_new_ones() {
        let inputs = [input("x.zip", "zip", Some(SAMPLE_INFO))];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::Japanese);
        // 形式・メイン・言語・作者・グループは既存と完全一致で、初期採用。
        assert_eq!(find(&set, Role::Format, "zip").choice, Choice::Existing { cat_idx: 1, tier_id: 10 });
        assert_eq!(find(&set, Role::Main, "Doujinshi").choice, Choice::Existing { cat_idx: 0, tier_id: 2 });
        assert_eq!(find(&set, Role::Language, "日本語").choice, Choice::Existing { cat_idx: 4, tier_id: 40 });
        assert_eq!(find(&set, Role::Group, "Hawk Bit").choice, Choice::Existing { cat_idx: 3, tier_id: 30 });
        assert_eq!(find(&set, Role::Author, "Kouji").choice, Choice::Existing { cat_idx: 2, tier_id: 20 });
        // イベントは、イベントカテゴリが無いので新規（カテゴリも新規）。
        let ev = find(&set, Role::Event, "C105");
        assert_eq!((ev.choice.clone(), ev.category), (Choice::New, CatRef::New));
        assert_eq!(set.files_with_info, 1);
        assert_eq!(set.limits.window1_new, 1);
        assert_eq!(set.limits.window2_new, 0);
    }

    #[test]
    fn info_text_wins_over_the_filename_and_filename_fills_the_gaps() {
        // ファイル名は別の作者・言語。info.txt の値が正。
        let inputs = [input("[Other (Someone)] T [English].zip", "zip", Some(SAMPLE_INFO))];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().all(|c| c.name != "Someone" && c.name != "Other"), "infoの作者が優先");
        assert_eq!(find(&set, Role::Language, "日本語").files, vec![0]);
        assert!(set.candidates.iter().all(|c| !(c.role == Role::Language && c.name == "英語")));
        // info.txt が無い（あっても作者・言語が無い）ときは、ファイル名で補う。
        let inputs = [
            input("[Circle (Writer)] T [English].zip", "zip", None),
            input("[Circle2 (Writer2)] T.zip", "zip", Some("Category: Manga\n")),
        ];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::Japanese);
        find(&set, Role::Group, "Circle");
        find(&set, Role::Author, "Writer");
        find(&set, Role::Language, "英語");
        find(&set, Role::Group, "Circle2");
        find(&set, Role::Main, "Manga");
    }

    #[test]
    fn pair_direction_follows_existing_tags() {
        // 一般系の向き: 外=グループ、内=作者。
        let set = build_candidates(&[input("[NewCircle (NewWriter)] T.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::Japanese);
        assert_eq!(find(&set, Role::Group, "NewCircle").choice, Choice::New);
        assert!(find(&set, Role::Group, "NewCircle").unconfirmed, "既存の裏づけが無い向きは未確定");
        find(&set, Role::Author, "NewWriter");
        // 既存タグが逆（外=作者に Kouji、内=グループに Hawk Bit）を示すなら、向きを入れ替える。
        let set = build_candidates(&[input("[Kouji (Hawk Bit)] T.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::Japanese);
        assert_eq!(find(&set, Role::Author, "Kouji").choice, Choice::Existing { cat_idx: 2, tier_id: 20 });
        assert_eq!(find(&set, Role::Group, "Hawk Bit").choice, Choice::Existing { cat_idx: 3, tier_id: 30 });
        // 一般系どおりで既存と合うなら、そのまま。
        let set = build_candidates(&[input("[Hawk Bit (Kouji)] T.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::Japanese);
        assert!(!find(&set, Role::Group, "Hawk Bit").unconfirmed);
    }

    #[test]
    fn single_bracket_uses_existing_side_then_similar_suggestions_then_new_author() {
        let m = master();
        let o = MatchOptions::default();
        // グループにだけ完全一致 → グループとして既存を採用。
        let set = build_candidates(&[input("[Hawk-Bit] T.zip", "zip", None)], &m, &o, Lang::Japanese);
        assert_eq!(find(&set, Role::Group, "Hawk-Bit").choice, Choice::Existing { cat_idx: 3, tier_id: 30 });
        // 類似（ローマ字ゆれ）だけ → 提案つきで未選択。
        let set = build_candidates(&[input("[Koji] T.zip", "zip", None)], &m, &o, Lang::Japanese);
        let c = find(&set, Role::Author, "Koji");
        assert_eq!(c.choice, Choice::Skip);
        assert_eq!(c.suggestions.first().map(|s| (s.tier_id, s.name.as_str())), Some((20, "Kouji")));
        assert!(c.suggestions[0].score >= 80 && c.suggestions[0].score <= 99);
        assert!(c.unconfirmed);
        // どれにも類似しない → 作者の新規候補（未確定）。
        let set = build_candidates(&[input("[Zzzzqq] T.zip", "zip", None)], &m, &o, Lang::Japanese);
        let c = find(&set, Role::Author, "Zzzzqq");
        assert_eq!((c.choice.clone(), c.unconfirmed), (Choice::New, true));
        // しきい値を100にすると、類似提案は出ない。
        let strict = MatchOptions { threshold: 100, ..o };
        let set = build_candidates(&[input("[Koji] T.zip", "zip", None)], &m, &strict, Lang::Japanese);
        assert_eq!(find(&set, Role::Author, "Koji").choice, Choice::New);
    }

    #[test]
    fn spelling_variants_are_merged_by_exact_key_and_clustered_when_similar() {
        let inputs = [
            input("[Foo (Bar Baz)] a.zip", "zip", None),
            input("[foo (bar-baz)] b.zip", "zip", None),
            input("[Foo (Barr Baz)] c.zip", "zip", None),
        ];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        let authors: Vec<&Candidate> = set.candidates.iter().filter(|c| c.role == Role::Author).collect();
        assert_eq!(authors.len(), 2, "Bar Baz と bar-baz は1つ、Barr Baz は別の候補");
        let merged = authors.iter().find(|c| c.files.len() == 2).unwrap();
        assert_eq!(merged.files, vec![0, 1]);
        assert_eq!(merged.forms.len(), 2);
        assert_eq!(authors[0].cluster, authors[1].cluster, "類似品は同じ束");
        let groups = set.candidates.iter().filter(|c| c.role == Role::Group).count();
        assert_eq!(groups, 1);
    }

    #[test]
    fn existing_elements_match_by_any_alias() {
        let mut cats = master();
        cats[2].tiers[0].element = Some("Kouji|Tarou|コウタ".to_string());
        // 2番目の別名に完全一致 → 既存の採用。
        let set = build_candidates(&[input("[Circ (tarou)] a.zip", "zip", None)], &cats, &MatchOptions::default(), Lang::English);
        let c = find(&set, Role::Author, "tarou");
        assert_eq!(c.choice, Choice::Existing { cat_idx: 2, tier_id: 20 });
        assert!(c.suggestions.is_empty());
        // 別名どうしが類似（ローマ字ゆれ）→ 提案。全体名と、近かった別名が出る。
        let set = build_candidates(&[input("[Circ (Kouta)] a.zip", "zip", None)], &cats, &MatchOptions::default(), Lang::English);
        let c = find(&set, Role::Author, "Kouta");
        assert_eq!(c.choice, Choice::Skip);
        assert_eq!(c.suggestions.len(), 1, "要素ごとに1件");
        assert_eq!((c.suggestions[0].tier_id, c.suggestions[0].name.as_str(), c.suggestions[0].alias.as_str()), (20, "Kouji|Tarou|コウタ", "コウタ"));
        // 別名に完全一致する要素は、類似提案に出さない（完全一致が優先）。
        let set = build_candidates(&[input("[Circ (Kouji)] a.zip", "zip", None)], &cats, &MatchOptions::default(), Lang::English);
        let c = find(&set, Role::Author, "Kouji");
        assert_eq!(c.choice, Choice::Existing { cat_idx: 2, tier_id: 20 });
    }

    #[test]
    fn alias_choice_counts_as_adopted_and_needs_no_new_element() {
        let set = build_candidates(&[input("[Circ (Koji)] a.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::English);
        let mut c = find(&set, Role::Author, "Koji").clone();
        c.choice = Choice::Alias { cat_idx: 2, tier_id: 20 };
        assert!(c.is_adopted());
        assert!(!c.needs_new_element());
    }

    fn authors_set(cats: &[TagManagerCategoryUi]) -> CandidateSet {
        let inputs = [
            input("[Circ (Kouji)] a.zip", "zip", None),
            input("[Circ2 (Koji)] b.zip", "zip", None),
            input("[Circ3 (Taro)] c.zip", "zip", None),
        ];
        build_candidates(&inputs, cats, &MatchOptions::default(), Lang::English)
    }

    fn idx_of(set: &CandidateSet, role: Role, name: &str) -> usize {
        set.candidates.iter().position(|c| c.role == role && c.name == name).unwrap()
    }

    #[test]
    fn merge_with_an_exact_existing_member_adds_aliases_to_that_element() {
        let cats = master();
        let mut set = authors_set(&cats);
        let total_before = set.candidates.len();
        let (a, b) = (idx_of(&set, Role::Author, "Kouji"), idx_of(&set, Role::Author, "Koji"));
        let m = set.merge_candidates(&[a, b], None, 1, &cats).unwrap();
        let c = &set.candidates[m];
        assert_eq!(c.aliases, ["Kouji", "Koji"]);
        assert_eq!(c.name, "Kouji|Koji");
        assert_eq!(c.choice, Choice::Alias { cat_idx: 2, tier_id: 20 }, "Kouji は既存。Koji を別名に足す");
        assert!(c.is_adopted() && !c.needs_new_element());
        assert_eq!(c.files, vec![0, 1]);
        assert_eq!(c.members.len(), 2);
        assert_eq!(c.key, "bundle:1");
        assert_eq!(c.registration_name(), "Kouji|Koji");
        assert_eq!(set.candidates.len(), total_before - 1, "元の2件が1件に");
    }

    #[test]
    fn merge_without_an_existing_member_makes_one_new_aliased_element() {
        let cats = master();
        let mut set = authors_set(&cats);
        let before = set.limits.window2_new;
        let (a, b) = (idx_of(&set, Role::Author, "Taro"), idx_of(&set, Role::Group, "Circ"));
        assert_eq!(set.merge_candidates(&[a, b], None, 2, &cats), Err(MergeError::MixedRoles));
        let g1 = idx_of(&set, Role::Group, "Circ");
        let g2 = idx_of(&set, Role::Group, "Circ2");
        let m = set.merge_candidates(&[g1, g2], None, 3, &cats).unwrap();
        assert_eq!(set.candidates[m].choice, Choice::New);
        assert_eq!(set.candidates[m].registration_name(), "Circ|Circ2");
        assert_eq!(set.limits.window2_new, before - 1, "新規要素が1つ減る");
        assert_eq!(set.merge_candidates(&[m], None, 4, &cats), Err(MergeError::TooFew));
    }

    #[test]
    fn merge_is_rejected_for_multiple_existing_elements_or_overlapping_aliases() {
        let mut cats = master();
        cats[2].tiers.push(tier(21, "Taro"));
        let mut set = authors_set(&cats);
        let (a, b) = (idx_of(&set, Role::Author, "Kouji"), idx_of(&set, Role::Author, "Taro"));
        assert_eq!(
            set.merge_candidates(&[a, b], None, 1, &cats),
            Err(MergeError::MultipleExisting { first: "Kouji".into(), second: "Taro".into() })
        );
        // 新規どうしの別名が、既存要素の別名と重なる（照合の設定で完全一致にならなかった場合）。
        let mut cats = master();
        cats[2].tiers.push(tier(21, "Koji"));
        let mut opts = MatchOptions::default();
        opts.romaji = false;
        let inputs = [input("[Circ (Kouji)] a.zip", "zip", None), input("[Circ2 (Koji!)] b.zip", "zip", None)];
        let mut set = build_candidates(&inputs, &cats, &opts, Lang::English);
        // "Koji!" は記号を無視した完全一致で既存 Koji（21）、"Kouji" は既存 Kouji（20）→ 複数の既存。
        let (a, b) = (idx_of(&set, Role::Author, "Kouji"), idx_of(&set, Role::Author, "Koji!"));
        assert!(matches!(set.merge_candidates(&[a, b], None, 1, &cats), Err(MergeError::MultipleExisting { .. })));
    }

    #[test]
    fn merged_candidate_skip_split_and_primary_alias() {
        let cats = master();
        let mut set = authors_set(&cats);
        let (a, b) = (idx_of(&set, Role::Group, "Circ"), idx_of(&set, Role::Group, "Circ2"));
        set.candidates[a].unadopt();
        set.candidates[b].unadopt();
        let m = set.merge_candidates(&[a, b], None, 1, &cats).unwrap();
        assert!(!set.candidates[m].is_adopted(), "メンバー全部が不採用なら不採用");
        set.candidates[m].adopt();
        assert_eq!(set.candidates[m].choice, Choice::New);
        set.set_primary_alias(m, 1);
        assert_eq!(set.candidates[m].name, "Circ2|Circ");
        assert_eq!(set.candidates[m].registration_name(), "Circ2|Circ");
        assert!(set.split_candidate(m));
        assert!(set.candidates.iter().any(|c| c.name == "Circ") && set.candidates.iter().any(|c| c.name == "Circ2"));
        assert!(!set.split_candidate(0), "まとめていない候補は分解できない");
    }

    #[test]
    fn merging_a_merged_candidate_flattens_and_bundles_are_restored_after_rebuild() {
        let cats = master();
        let mut set = authors_set(&cats);
        let g: Vec<usize> = ["Circ", "Circ2", "Circ3"].iter().map(|n| idx_of(&set, Role::Group, n)).collect();
        let m = set.merge_candidates(&g[..2], None, 7, &cats).unwrap();
        let g3 = idx_of(&set, Role::Group, "Circ3");
        let m = set.merge_candidates(&[m, g3], None, 8, &cats).unwrap();
        assert_eq!(set.candidates[m].members.len(), 3, "展開して3件");
        assert_eq!(set.candidates[m].aliases.len(), 3);
        let bundles = set.snapshot_bundles();
        assert_eq!(bundles.len(), 1);
        // 再生成（まとめ無し）→ 復元。
        let mut fresh = authors_set(&cats);
        assert!(fresh.candidates.iter().all(|c| c.bundle_id.is_none()));
        fresh.apply_bundles(&bundles, &cats);
        let restored = fresh.candidates.iter().find(|c| c.bundle_id == Some(8)).expect("復元される");
        assert_eq!(restored.name, set.candidates[m].name);
        assert_eq!(restored.members.len(), 3);
        // ユーザーの採否の引き継ぎ（まとめた候補のキーは番号で安定）。
        let mut old = set.clone();
        let mi = old.candidates.iter().position(|c| c.bundle_id == Some(8)).unwrap();
        old.candidates[mi].unadopt();
        old.carry_over_choices(&mut fresh);
        assert!(!fresh.candidates.iter().find(|c| c.bundle_id == Some(8)).unwrap().is_adopted());
    }

    #[test]
    fn registration_name_follows_the_ui_language() {
        let forms = vec![("コウジ".to_string(), 1), ("Kouji".to_string(), 1)];
        assert_eq!(pick_registration_name(&forms, Lang::Japanese), "コウジ");
        assert_eq!(pick_registration_name(&forms, Lang::English), "Kouji");
        assert_eq!(pick_registration_name(&[("Kouji".to_string(), 3)], Lang::Japanese), "Kouji", "日本語の表記が無ければあるものを使う");
        // 言語タグも、UIが日本語以外なら英語名で登録する（既存の日本語名タグとは辞書で同一視する）。
        let inputs = [input("a.zip", "zip", Some("Language: Japanese\n"))];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        assert_eq!(set.candidates.iter().find(|c| c.role == Role::Language).map(|c| c.choice.clone()), Some(Choice::Existing { cat_idx: 4, tier_id: 40 }));
        let set = build_candidates(&inputs, &[cat("main", true, None, vec![])], &MatchOptions::default(), Lang::English);
        assert_eq!(find(&set, Role::Language, "Japanese").choice, Choice::New);
    }

    #[test]
    fn unknown_extensions_and_folders_become_format_candidates() {
        let inputs = [
            input("a.rar", "rar", None),
            input_from_info_text("book", Some(FormatTag::Folder), None),
            input("b.ZIP", "zip", None),
        ];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::Japanese);
        assert_eq!(find(&set, Role::Format, "rar").choice, Choice::New);
        assert_eq!(find(&set, Role::Format, "rar").category, CatRef::Existing(1));
        assert_eq!(find(&set, Role::Format, "フォルダ").choice, Choice::New, "初期要素に無ければ新規（起動時は初期要素に入る）");
        assert_eq!(find(&set, Role::Format, "zip").files, vec![2]);
    }

    #[test]
    fn over_limit_is_judged_per_category() {
        let o = MatchOptions::default();
        let many = |n: usize, prefix: &str| -> Vec<CandidateInput> {
            (0..n).map(|i| input(&format!("[{prefix}{i:05}] t.zip"), "zip", None)).collect()
        };
        // 作者1500件は、上限1000なら作者だけが超える。窓2の集計は1500。
        let set = build_candidates(&many(1500, "Writer"), &master(), &o, Lang::English);
        assert_eq!(set.limits.window2_new, 1500);
        assert_eq!(over_limit(&set.candidates, 1000), vec![(Role::Author, 1500)]);
        assert!(over_limit(&set.candidates, 1500).is_empty(), "ちょうど上限は超えない");
        // イベント（窓1）と作者（窓2）は別カテゴリ。それぞれの数で判定し、合算しない。
        let mut inputs = many(600, "Writer");
        inputs.extend((0..600).map(|i| input(&format!("(Event{i:05}) t.zip"), "zip", None)));
        let set = build_candidates(&inputs, &master(), &o, Lang::English);
        assert!(over_limit(&set.candidates, 1000).is_empty(), "合算では1200だが、カテゴリ別では超えない");
        let over = over_limit(&set.candidates, 500);
        assert_eq!(over.iter().map(|(r, _)| *r).collect::<Vec<_>>(), vec![Role::Event, Role::Author], "種類の並び順");
        // 既存に一致するものは新規に数えない。
        let set = build_candidates(&[input("[Hawk Bit (Kouji)] t.zip", "zip", None)], &master(), &o, Lang::English);
        assert_eq!((set.limits.window1_new, set.limits.window2_new), (0, 0));
        assert!(over_limit(&set.candidates, 0).is_empty());
    }

    #[test]
    fn truncate_keeps_the_most_used_and_reports_the_removed_count() {
        let o = MatchOptions::default();
        // 作者 A は3ファイル、B は2、C〜E は1。上限3なら A・B と、同数1件のうち名前順で先頭の C が残る。
        let mut inputs = Vec::new();
        for (name, n) in [("Aaa", 3), ("Bbb", 2), ("Ccc", 1), ("Ddd", 1), ("Eee", 1)] {
            inputs.extend((0..n).map(|i| input(&format!("[{name}] t{i}.zip"), "zip", None)));
        }
        let mut set = build_candidates(&inputs, &master(), &o, Lang::English);
        let authors = |s: &CandidateSet| {
            let mut v: Vec<String> = s.candidates.iter().filter(|c| c.role == Role::Author).map(|c| c.name.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(authors(&set).len(), 5);
        let removed = set.truncate_to_limit(3);
        assert_eq!(removed, vec![(Role::Author, 2)]);
        assert_eq!(authors(&set), vec!["Aaa", "Bbb", "Ccc"]);
        assert!(over_limit(&set.candidates, 3).is_empty());
        assert_eq!(set.limits.window2_new, 3, "集計もやり直す");
        // 上限内なら何も除かない。
        assert!(set.truncate_to_limit(3).is_empty());
    }

    #[test]
    fn truncate_does_not_touch_existing_matches_or_other_categories() {
        let o = MatchOptions::default();
        let mut inputs: Vec<CandidateInput> = (0..4).map(|i| input(&format!("[W{i}] t.zip"), "zip", None)).collect();
        inputs.push(input("[Hawk Bit (Kouji)] u.zip", "zip", None));
        inputs.push(input("(C105) v.zip", "zip", None));
        let mut set = build_candidates(&inputs, &master(), &o, Lang::English);
        let before_other: Vec<String> = set.candidates.iter().filter(|c| c.role != Role::Author).map(|c| c.name.clone()).collect();
        set.truncate_to_limit(1);
        let after_other: Vec<String> = set.candidates.iter().filter(|c| c.role != Role::Author).map(|c| c.name.clone()).collect();
        assert_eq!(before_other, after_other);
        assert!(set.candidates.iter().any(|c| c.name == "Kouji" && !c.needs_new_element()), "既存一致は残る");
    }

    #[test]
    fn adopt_and_unadopt_restore_the_sensible_choice() {
        let set = build_candidates(&[input("[Koji] t.zip", "zip", None), input("[Hawk Bit (Kouji)] u.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::Japanese);
        let mut koji = find(&set, Role::Author, "Koji").clone();
        assert_eq!(koji.choice, Choice::Skip);
        assert!(!koji.is_adopted());
        koji.adopt();
        assert_eq!(koji.choice, Choice::Existing { cat_idx: 2, tier_id: 20 }, "類似提案の最上位");
        koji.choice = Choice::New;
        koji.unadopt();
        assert_eq!(koji.choice, Choice::Skip);
        let mut kouji = find(&set, Role::Author, "Kouji").clone();
        kouji.unadopt();
        kouji.adopt();
        assert_eq!(kouji.choice, kouji.initial, "完全一致は初期の既存に戻る");
        // 提案も既存も無い候補は、新規作成。
        let set = build_candidates(&[input("[Zzzz (Qqqq)] t.zip", "zip", None)], &master(), &MatchOptions::default(), Lang::Japanese);
        let mut c = find(&set, Role::Author, "Qqqq").clone();
        c.unadopt();
        c.adopt();
        assert_eq!(c.choice, Choice::New);
    }

    #[test]
    fn indices_in_window_split_roles_and_group_clusters() {
        let inputs = [
            input("(E1) [Aaa (Bbb)] t.zip", "zip", Some("Category: Manga\n")),
            input("[Aaa (Bbc)] t.zip", "zip", None),
        ];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        let w1 = set.indices_in_window(Window::One);
        let w2 = set.indices_in_window(Window::Two);
        assert!(w1.iter().all(|&i| set.candidates[i].role.window() == Window::One));
        assert!(w2.iter().all(|&i| matches!(set.candidates[i].role, Role::Author | Role::Group)));
        assert_eq!(w1.len() + w2.len(), set.candidates.len());
        let roles: Vec<Role> = w1.iter().map(|&i| set.candidates[i].role).collect();
        let mut sorted = roles.clone();
        sorted.sort_by_key(|r| match r { Role::Format => 0, Role::Main => 1, Role::Language => 2, _ => 3 });
        assert_eq!(roles, sorted, "種類順");
    }

    #[test]
    fn carry_over_keeps_user_changes_for_candidates_that_still_exist() {
        let inputs = [input("[Koji] t.zip", "zip", None), input("[Zzzz] u.zip", "zip", None)];
        let o = MatchOptions::default();
        let mut set = build_candidates(&inputs, &master(), &o, Lang::English);
        // ユーザーが Koji を採用、Zzzz を不採用に変えた。
        let koji = set.candidates.iter().position(|c| c.name == "Koji").unwrap();
        set.candidates[koji].adopt();
        let zzzz = set.candidates.iter().position(|c| c.name == "Zzzz").unwrap();
        set.candidates[zzzz].unadopt();
        let strict = MatchOptions { threshold: 100, ..o };
        let mut fresh = build_candidates(&inputs, &master(), &strict, Lang::English);
        assert_eq!(find(&fresh, Role::Author, "Koji").choice, Choice::New, "しきい値100では提案が消える");
        set.carry_over_choices(&mut fresh);
        assert_eq!(find(&fresh, Role::Author, "Koji").choice, Choice::Existing { cat_idx: 2, tier_id: 20 }, "変えた扱いを引き継ぐ");
        assert_eq!(find(&fresh, Role::Author, "Zzzz").choice, Choice::Skip);
        assert_eq!(fresh.limits.window2_new, 1, "新規の集計も引き継ぎ後でやり直す（Zzzz は Skip でも新規の数に入る）");
    }

    #[test]
    fn registration_name_is_width_normalized_when_enabled() {
        let inputs = [input("[ﾎｰｸﾋﾞｯﾄ (ｺｳｼﾞ)] t.zip", "zip", None)];
        let set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().any(|c| c.role == Role::Author && c.name == "コウジ"));
        let off = MatchOptions { width: false, ..MatchOptions::default() };
        let set = build_candidates(&inputs, &[], &off, Lang::Japanese);
        assert!(set.candidates.iter().any(|c| c.role == Role::Author && c.name == "ｺｳｼﾞ"), "OFFのときはそのまま");
    }

    #[test]
    fn empty_and_odd_inputs_do_not_panic() {
        let set = build_candidates(&[], &master(), &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.is_empty());
        assert_eq!((set.file_count, set.limits.window1_new, set.limits.window2_new), (0, 0, 0));
        let weird = [input("[] ().zip", "zip", Some("")), input("[(A)] t.zip", "zip", Some("Category:   \n"))];
        let set = build_candidates(&weird, &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().any(|c| c.role == Role::Author && c.name == "A"));
    }

    // ── 2行タイトル（英語行・日本語行）の対応づけ ──

    const PAIRED_INFO: &str = "[Ebizoridan (Shien)] Senpai Book 2 [Digital]\n[えびぞり団 (しえん)] 先輩の本 2 [DL版]\nhttps://example.invalid/\n\nCategory: Doujinshi\nLanguage: Japanese\n";

    fn names_of(set: &CandidateSet, role: Role) -> Vec<(String, Vec<String>, String)> {
        set.candidates.iter().filter(|c| c.role == role).map(|c| (c.name.clone(), c.alt_forms.clone(), c.registration_name())).collect()
    }

    #[test]
    fn two_line_title_pairs_names_by_position_and_registers_with_aliases() {
        let inputs = [input("x.zip", "zip", Some(PAIRED_INFO))];
        // UIが日本語なら、日本語の表記が主、英語の表記が別名。照合は日本語行で行う。
        let ja = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(names_of(&ja, Role::Group), vec![("えびぞり団".to_string(), vec!["Ebizoridan".to_string()], "えびぞり団|Ebizoridan".to_string())]);
        assert_eq!(names_of(&ja, Role::Author), vec![("しえん".to_string(), vec!["Shien".to_string()], "しえん|Shien".to_string())]);
        // UIが英語なら、英語の表記が主。
        let en = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::English);
        assert_eq!(names_of(&en, Role::Group)[0].2, "Ebizoridan|えびぞり団");
        assert_eq!(names_of(&en, Role::Author)[0].2, "Shien|しえん");
    }

    #[test]
    fn identical_names_in_both_lines_get_no_alias_and_structure_mismatch_is_not_paired() {
        // 同じ表記（イベントコード）は別名にしない。
        let info = "(C105) [Hawk Bit (Kouji)] A\n(C105) [ホークビット (こうじ)] あ\n\nCategory: Doujinshi\n";
        let set = build_candidates(&[input("x.zip", "zip", Some(info))], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(names_of(&set, Role::Event), vec![("C105".to_string(), vec![], "C105".to_string())]);
        assert_eq!(names_of(&set, Role::Author)[0].1, vec!["Kouji".to_string()]);
        // 括弧の構造が違う（英語行は作者が単独）なら、対にしない。
        let info = "[Hawk Bit] A\n[ホークビット (こうじ)] あ\n\nCategory: Doujinshi\n";
        let set = build_candidates(&[input("x.zip", "zip", Some(info))], &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().all(|c| c.alt_forms.is_empty()));
    }

    #[test]
    fn existing_element_found_by_either_form_gets_an_alias_candidate_adopted_by_default() {
        // マスターに英語の表記だけがある（Ebizoridan / Shien）。日本語の表記を別名として足す候補になる。
        let master = vec![
            cat("作者", false, None, vec![tier(20, "Shien")]),
            cat("グループ", false, None, vec![tier(30, "Ebizoridan")]),
        ];
        let set = build_candidates(&[input("x.zip", "zip", Some(PAIRED_INFO))], &master, &MatchOptions::default(), Lang::Japanese);
        let g = set.candidates.iter().find(|c| c.role == Role::Group).unwrap();
        assert_eq!(g.choice, Choice::Alias { cat_idx: 1, tier_id: 30 });
        assert!(g.is_adopted());
        assert_eq!(g.alias_list(), vec!["えびぞり団".to_string(), "Ebizoridan".to_string()]);
        let a = set.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert_eq!(a.choice, Choice::Alias { cat_idx: 0, tier_id: 20 });
        // 既存の要素がすべての表記を別名に持っているなら、そのまま既存に付ける。
        let full = vec![
            cat("作者", false, None, vec![tier(20, "Shien|しえん")]),
            cat("グループ", false, None, vec![tier(30, "Ebizoridan|えびぞり団")]),
        ];
        let set = build_candidates(&[input("x.zip", "zip", Some(PAIRED_INFO))], &full, &MatchOptions::default(), Lang::Japanese);
        assert_eq!(set.candidates.iter().find(|c| c.role == Role::Group).unwrap().choice, Choice::Existing { cat_idx: 1, tier_id: 30 });
        assert_eq!(set.candidates.iter().find(|c| c.role == Role::Author).unwrap().choice, Choice::Existing { cat_idx: 0, tier_id: 20 });
    }

    #[test]
    fn registration_name_setting_chooses_the_main_form_only_for_element_names() {
        use crate::config::TagRegisterName;
        let inputs = [input("x.zip", "zip", Some(PAIRED_INFO))];
        let group_of = |ui: Lang, pref: TagRegisterName| {
            let set = build_candidates_full(&inputs, &[], &MatchOptions::default(), ui, pref.name_lang(ui), None, &[]);
            let g = set.candidates.iter().find(|c| c.role == Role::Group).unwrap().clone();
            (g.name, g.alt_forms)
        };
        let ja = ("えびぞり団".to_string(), vec!["Ebizoridan".to_string()]);
        let en = ("Ebizoridan".to_string(), vec!["えびぞり団".to_string()]);
        // 表示言語に合わせる: 日本語表示=日本語、英語表示=英字（従来どおり）。
        assert_eq!(group_of(Lang::Japanese, TagRegisterName::FollowUi), ja);
        assert_eq!(group_of(Lang::English, TagRegisterName::FollowUi), en);
        // 日本語優先・英語優先は、表示言語に関わらず固定。もう一方は別名になる。
        assert_eq!(group_of(Lang::English, TagRegisterName::Japanese), ja);
        assert_eq!(group_of(Lang::Japanese, TagRegisterName::English), en);
        // 要素名だけが対象: 形式名・言語名は、設定に関わらず表示言語で決まる（`lang` を使い続ける）。
        let lang_inputs = [input("a.zip", "zip", Some("Language: Japanese\n"))];
        let set = build_candidates_full(&lang_inputs, &[], &MatchOptions::default(), Lang::English, Lang::Japanese, None, &[]);
        assert!(set.candidates.iter().any(|c| c.role == Role::Language && c.name == "Japanese"));
    }

    #[test]
    fn alias_pairs_merge_across_files_and_survive_single_line_files() {
        // 1件目は対あり、2件目は日本語行だけ。同じ日本語名なら1つの候補にまとまり、別名は1件目から引き継ぐ。
        let second = "[えびぞり団 (しえん)] 別の本\n\nCategory: Doujinshi\n";
        let set = build_candidates(&[input("a.zip", "zip", Some(PAIRED_INFO)), input("b.zip", "zip", Some(second))], &[], &MatchOptions::default(), Lang::Japanese);
        let g: Vec<_> = set.candidates.iter().filter(|c| c.role == Role::Group).collect();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].files.clone(), g[0].alt_forms.clone()), (vec![0, 1], vec!["Ebizoridan".to_string()]));
    }

    #[test]
    fn single_line_titles_and_filename_only_files_are_unchanged() {
        let set = build_candidates(&[input("[Hawk Bit (Kouji)] t.zip", "zip", None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().all(|c| c.alt_forms.is_empty()));
        let set = build_candidates(&[input("x.zip", "zip", Some(SAMPLE_INFO))], &master(), &MatchOptions::default(), Lang::Japanese);
        assert_eq!(find(&set, Role::Group, "Hawk Bit").choice, Choice::Existing { cat_idx: 3, tier_id: 30 });
    }

    // ── 大ルール（下処理） ──

    use crate::tag_rules::{Preset, TagRule};

    fn preset_rule(p: Preset, id: u64) -> TagRule {
        let mut r = p.to_rule(id);
        if r.effect == crate::tag_rules::Effect::default() {
            r.effect.disable.insert(RuleRole::Event);
        }
        r
    }

    fn build_with_rules(inputs: &[CandidateInput], cats: &[TagManagerCategoryUi], rules: &[TagRule]) -> CandidateSet {
        build_candidates_full(inputs, cats, &MatchOptions::default(), Lang::Japanese, Lang::Japanese, None, rules)
    }

    #[test]
    fn rule_disables_roles_and_keeps_them_as_unadopted_review_candidates_with_the_reason() {
        let inputs = [input("(C105) [Hawk Bit (Kouji)] Book [AI Generated].zip", "zip", None)];
        let none = build_with_rules(&inputs, &[], &[]);
        assert!(none.candidates.iter().all(|c| !c.review && c.is_adopted()), "ルール無しなら従来どおり");
        let set = build_with_rules(&inputs, &[], &[preset_rule(Preset::AiGenerated, 1)]);
        for role in [Role::Author, Role::Group, Role::Event] {
            let c = set.candidates.iter().find(|c| c.role == role).unwrap();
            assert!(c.review && !c.is_adopted(), "{role:?} は未採用・要確認で残る");
            assert_eq!(c.rule_reason.as_deref(), Some("AI generated"));
        }
        // 効果で触れていない種類（形式・言語）は、そのまま採用される。
        let fmt = set.candidates.iter().find(|c| c.role == Role::Format).unwrap();
        assert!(!fmt.review && fmt.is_adopted());
        assert_eq!(set.rule_hits, vec![("AI generated".to_string(), 1)]);
    }

    #[test]
    fn rule_wins_over_an_exact_master_match() {
        let mut r = preset_rule(Preset::Anthology, 1);
        r.effect.disable = [RuleRole::Author, RuleRole::Group].into_iter().collect();
        let set = build_with_rules(&[input("Anthology x.zip", "zip", Some(SAMPLE_INFO))], &master(), &[r]);
        let a = set.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert_eq!(a.initial, Choice::Skip, "既存マスターに一致していても、ルールが無効にしたら未採用");
        assert!(a.review);
    }

    #[test]
    fn rule_applies_per_file_and_a_name_used_by_an_unaffected_file_stays_adopted() {
        // 1件目はAI generated（作者・グループ無効）、2件目は通常。同じ作者名なら、2件目の分があるので候補は採用のまま。
        let inputs = [input("[Hawk Bit (Kouji)] A [AI Generated].zip", "zip", None), input("[Hawk Bit (Kouji)] B.zip", "zip", None)];
        let set = build_with_rules(&inputs, &[], &[preset_rule(Preset::AiGenerated, 1)]);
        let a = set.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert!(!a.review && a.is_adopted());
        // 2件ともAI generatedなら、未採用・要確認。
        let inputs = [input("[Hawk Bit (Kouji)] A [AI Generated].zip", "zip", None), input("[Hawk Bit (Kouji)] B ai generated.zip", "zip", None)];
        let set = build_with_rules(&inputs, &[], &[preset_rule(Preset::AiGenerated, 1)]);
        assert!(set.candidates.iter().find(|c| c.role == Role::Author).unwrap().review);
        assert_eq!(set.rule_hits, vec![("AI generated".to_string(), 2)]);
    }

    #[test]
    fn rule_can_disable_system_roles_and_reasons_are_joined_across_rules() {
        let mut ai = preset_rule(Preset::AiGenerated, 1);
        ai.effect.disable.insert(RuleRole::Format);
        let anth = preset_rule(Preset::Anthology, 2);
        let set = build_with_rules(&[input("[Hawk Bit (Kouji)] Anthology AI generated.zip", "zip", None)], &[], &[ai, anth]);
        let fmt = set.candidates.iter().find(|c| c.role == Role::Format).unwrap();
        assert_eq!(fmt.rule_reason.as_deref(), Some("AI generated"));
        let author = set.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert_eq!(author.rule_reason.as_deref(), Some("AI generated、anthology"));
    }

    #[test]
    fn rules_read_the_info_txt_category_and_two_line_titles() {
        let info = "[Ebizoridan (Shien)] Cosplay Book\n[えびぞり団 (しえん)] コスプレ本\n\nCategory: Doujinshi\n";
        let set = build_with_rules(&[input("x.zip", "zip", Some(info))], &[], &[preset_rule(Preset::Cosplay, 1)]);
        assert!(set.candidates.iter().find(|c| c.role == Role::Group).unwrap().review, "info.txt の見出し行（日本語行・英語行）に cosplay/コスプレ");
        let set = build_with_rules(&[input("x.zip", "zip", Some("[A (B)] T\n\nCategory: Cosplay\n"))], &[], &[preset_rule(Preset::Cosplay, 1)]);
        assert!(set.candidates.iter().find(|c| c.role == Role::Author).unwrap().review, "Category: Cosplay");
    }

    #[test]
    fn rules_apply_on_the_learned_model_path_too() {
        use crate::tag_corpus::{CorpusEntry, Label};
        use crate::tag_learning::LearnedSet;
        let entries = vec![CorpusEntry { file_name: "[A (B)] T.zip".into(), labels: vec![(Label::Group, "A".into()), (Label::Author, "B".into())], hard: false }];
        let model = Model::train(&LearnedSet { version: 1, entries }.training_data(&MatchOptions::default())).unwrap();
        let rules = [preset_rule(Preset::AiGenerated, 1)];
        let set = build_candidates_full(&[input("[Hawk Bit (Kouji)] Book AI generated.zip", "zip", None)], &[], &MatchOptions::default(), Lang::Japanese, Lang::Japanese, Some(&model), &rules);
        assert!(set.candidates.iter().find(|c| c.role == Role::Author).unwrap().review);
    }

    #[test]
    fn disabled_rules_do_nothing() {
        let mut r = preset_rule(Preset::AiGenerated, 1);
        r.enabled = false;
        let set = build_with_rules(&[input("[Hawk Bit (Kouji)] Book AI generated.zip", "zip", None)], &[], &[r]);
        assert!(set.candidates.iter().all(|c| !c.review) && set.rule_hits.is_empty());
    }

    // ── 新種類（演者・キャラ・パロディ元）と cosplay の文法 ──

    use crate::tag_rules::{CondTarget, Condition, NameShape};

    fn shape_cosplay_rule(shape: NameShape) -> TagRule {
        let mut r = Preset::Cosplay.to_rule(1);
        r.conditions = vec![Condition::Shape { targets: vec![CondTarget::FileName, CondTarget::InfoTitle], shape }];
        r
    }

    fn name_of(set: &CandidateSet, role: Role) -> Option<String> {
        set.candidates.iter().find(|c| c.role == role).map(|c| c.name.clone())
    }

    #[test]
    fn cosplay_filename_without_info_gives_performer_character_and_parody() {
        let inputs = [input("Tiny Asa アサ - Castorice (Honkai-Star Rail).zip", "zip", None)];
        let set = build_with_rules(&inputs, &[], &[shape_cosplay_rule(NameShape::Dash2Paren)]);
        assert_eq!(name_of(&set, Role::Performer).as_deref(), Some("Tiny Asa アサ"));
        assert_eq!(name_of(&set, Role::Character).as_deref(), Some("Castorice"));
        assert_eq!(name_of(&set, Role::Parody).as_deref(), Some("Honkai-Star Rail"));
        assert!(set.candidates.iter().filter(|c| matches!(c.role, Role::Performer | Role::Character | Role::Parody)).all(|c| c.is_adopted() && !c.review));
        // 窓の割り当て: 演者（人名）は窓2、キャラ・パロディ元は窓1。
        assert_eq!((Role::Performer.window(), Role::Character.window(), Role::Parody.window()), (Window::Two, Window::One, Window::One));
        // ルールが無ければ、新種類は取らない。
        let none = build_with_rules(&inputs, &[], &[]);
        assert!(none.candidates.iter().all(|c| !matches!(c.role, Role::Performer | Role::Character | Role::Parody)));
    }

    #[test]
    fn cosplay_info_txt_three_part_title_and_category() {
        let info = "希尔薇 - w旗袍 - Arknights\n\nhttps://example.invalid/\n\nCategory: Cosplay\nLanguage: Japanese\n";
        let set = build_with_rules(&[input("x.zip", "zip", Some(info))], &[], &[Preset::Cosplay.to_rule(1)]);
        assert_eq!(name_of(&set, Role::Performer).as_deref(), Some("希尔薇"));
        assert_eq!(name_of(&set, Role::Character).as_deref(), Some("w旗袍"));
        assert_eq!(name_of(&set, Role::Parody).as_deref(), Some("Arknights"));
        // 既存のルール（作者・グループ・イベントを無効）はそのまま。形式・言語は採用のまま。
        assert!(set.candidates.iter().find(|c| c.role == Role::Language).unwrap().is_adopted());
        assert!(set.candidates.iter().find(|c| c.role == Role::Format).unwrap().is_adopted());
    }

    #[test]
    fn two_part_title_has_no_parody_and_non_dash_titles_give_nothing() {
        let set = build_with_rules(&[input("Asa - Chara.zip", "zip", None)], &[], &[shape_cosplay_rule(NameShape::Dash2)]);
        assert_eq!((name_of(&set, Role::Performer).as_deref(), name_of(&set, Role::Character).as_deref()), (Some("Asa"), Some("Chara")));
        assert_eq!(name_of(&set, Role::Parody), None, "最小単位は演者＋キャラ。パロディ元は任意");
        // cosplay の Category だが、題名が ` - ` 区切りでなければ、何も取らない（作者・グループ等は無効のまま）。
        let set = build_with_rules(&[input("x.zip", "zip", Some("[Circle (Author)] Title\n\nCategory: Cosplay\n"))], &[], &[Preset::Cosplay.to_rule(1)]);
        assert!(set.candidates.iter().all(|c| !matches!(c.role, Role::Performer | Role::Character | Role::Parody)));
        assert!(set.candidates.iter().find(|c| c.role == Role::Author).unwrap().review);
    }

    #[test]
    fn dash_title_names_are_paired_with_the_other_line_as_aliases() {
        let info = "Asa - Castorice (Honkai Star Rail)\nアサ - カストリス (崩壊：スターレイル)\n\nCategory: Cosplay\n";
        let set = build_with_rules(&[input("x.zip", "zip", Some(info))], &[], &[Preset::Cosplay.to_rule(1)]);
        let c = set.candidates.iter().find(|c| c.role == Role::Character).unwrap();
        assert_eq!((c.name.as_str(), c.alt_forms.clone()), ("カストリス", vec!["Castorice".to_string()]));
        assert_eq!(c.registration_name(), "カストリス|Castorice");
        let p = set.candidates.iter().find(|c| c.role == Role::Parody).unwrap();
        assert_eq!(p.registration_name(), "崩壊:スターレイル|Honkai Star Rail", "全角半角の正規化（既定ON）で、全角コロンは半角になる");
    }

    #[test]
    fn new_roles_match_existing_categories_by_alias_names() {
        let master = vec![
            cat("演者", false, None, vec![tier(50, "Tiny Asa アサ")]),
            cat("キャラクター", false, None, vec![tier(51, "Castorice")]),
            cat("パロディ元", false, None, vec![tier(52, "Honkai-Star Rail")]),
        ];
        let set = build_with_rules(&[input("Tiny Asa アサ - Castorice (Honkai-Star Rail).zip", "zip", None)], &master, &[shape_cosplay_rule(NameShape::Dash2Paren)]);
        let choice = |role| set.candidates.iter().find(|c| c.role == role).unwrap().choice.clone();
        assert_eq!(choice(Role::Performer), Choice::Existing { cat_idx: 0, tier_id: 50 });
        assert_eq!(choice(Role::Character), Choice::Existing { cat_idx: 1, tier_id: 51 });
        assert_eq!(choice(Role::Parody), Choice::Existing { cat_idx: 2, tier_id: 52 });
    }

    #[test]
    fn another_rule_can_still_disable_a_new_role() {
        let mut parody_off = Preset::AiGenerated.to_rule(2);
        parody_off.conditions = vec![Condition::Shape { targets: vec![CondTarget::FileName], shape: NameShape::Dash2Paren }];
        parody_off.effect.disable = [RuleRole::Parody].into_iter().collect();
        parody_off.effect.enable.clear();
        let set = build_with_rules(&[input("Asa - Chara (Series).zip", "zip", None)], &[], &[shape_cosplay_rule(NameShape::Dash2Paren), parody_off]);
        let parody = set.candidates.iter().find(|c| c.role == Role::Parody);
        assert!(parody.is_none() || parody.is_some_and(|c| c.review), "無効にされたパロディ元は取らないか、要確認で残す");
        assert!(set.candidates.iter().find(|c| c.role == Role::Performer).unwrap().is_adopted());
    }

    // ── 正の情報（ComicInfo.xml・meta.json）の統合 ──

    const XML_SAMPLE: &str = "<?xml version=\"1.0\"?><ComicInfo><Title>[Kirome] Elf Mama-San no manga</Title><Series>original</Series>\
        <Writer>kirome</Writer><Tags>very long hair, bald, elf</Tags><Year>2023</Year><Month>12</Month><LanguageISO>ja</LanguageISO></ComicInfo>";
    const JSON_SAMPLE: &str = r#"{"title":{"english":"[Kayumidome] Valerie Monogatari 3","japanese":"[かゆみ止め] ヴァレリー物語(3)"},"tags":[{"type":"language","name":"japanese"},{"type":"category","name":"manga"},{"type":"tag","name":"group"},{"type":"artist","name":"kayumidome"}]}"#;

    fn meta_input(name: &str, xml: Option<&str>, json: Option<&str>) -> CandidateInput {
        let info = xml.and_then(|x| crate::fs::archive::meta::parse_comic_info(x.as_bytes()));
        CandidateInput {
            file_name: name.to_string(),
            format: Some(FormatTag::Extension("zip".into())),
            info: None,
            meta: ArchiveMeta::from_sources(info.as_ref(), json),
        }
    }

    fn cand<'a>(set: &'a CandidateSet, role: Role) -> Option<&'a Candidate> {
        set.candidates.iter().find(|c| c.role == role)
    }

    #[test]
    fn writer_confirms_a_single_bracket_as_author_without_unconfirmed() {
        // 題名 `[Kirome]` は、単独の括弧なので従来は「未確定」。Writer（kirome）で作者に確定する。
        let plain = build_candidates(&[input("[Kirome] Elf Mama-San no manga.zip", "zip", None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert!(cand(&plain, Role::Author).unwrap().unconfirmed, "正の情報が無ければ、従来どおり未確定");
        let set = build_candidates(&[meta_input("[Kirome] Elf Mama-San no manga.zip", Some(XML_SAMPLE), None)], &[], &MatchOptions::default(), Lang::Japanese);
        let a = cand(&set, Role::Author).unwrap();
        assert_eq!(a.name, "Kirome", "題名側の表記を登録名にする（大文字小文字違いは同じ名前）");
        assert!(!a.unconfirmed && a.is_adopted());
        assert!(cand(&set, Role::Group).is_none());
        // XML の Series: original はパロディ元にしない。LanguageISO: ja は言語になる。
        assert!(cand(&set, Role::Parody).is_none());
        assert_eq!(cand(&set, Role::Language).unwrap().name, "日本語");
    }

    #[test]
    fn json_artist_is_paired_with_the_japanese_title_bracket_through_the_two_titles() {
        let set = build_candidates(&[meta_input("whatever.zip", None, Some(JSON_SAMPLE))], &[], &MatchOptions::default(), Lang::Japanese);
        let a = cand(&set, Role::Author).unwrap();
        // 漢字の `かゆみ止め` と、ローマ字の `kayumidome` は、2つの題名の括弧の位置で対になる。
        assert_eq!(a.name, "かゆみ止め");
        assert_eq!(a.registration_name(), "かゆみ止め|Kayumidome");
        assert!(!a.unconfirmed && a.is_adopted());
        assert_eq!(cand(&set, Role::Main).unwrap().name, "manga", "JSON の category がメイン");
        assert_eq!(cand(&set, Role::Language).unwrap().name, "日本語");
        // type: tag の `group` は、グループではない。
        assert!(cand(&set, Role::Group).is_none());
    }

    #[test]
    fn xml_wins_over_json_and_json_fills_the_gaps() {
        // 同じ本の XML と JSON。作者は1つにまとまり、題名の括弧（日本語行）の表記が登録名、英語の表記が別名になる。
        let xml = "<ComicInfo><Title>[Kayumidome] Valerie Monogatari 3</Title><Writer>kayumidome</Writer></ComicInfo>";
        let set = build_candidates(&[meta_input("x.zip", Some(xml), Some(JSON_SAMPLE))], &[], &MatchOptions::default(), Lang::Japanese);
        let authors: Vec<&Candidate> = set.candidates.iter().filter(|c| c.role == Role::Author).collect();
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].registration_name(), "かゆみ止め|Kayumidome");
        assert_eq!(cand(&set, Role::Main).unwrap().name, "manga", "XML に Genre が無いので JSON の category で埋める");
        // XML と JSON で作者が違うときは、XML だけを使う（JSON の artist は使わない）。
        let xml = "<ComicInfo><Writer>Other Person</Writer></ComicInfo>";
        let set = build_candidates(&[meta_input("x.zip", Some(xml), Some(JSON_SAMPLE))], &[], &MatchOptions::default(), Lang::Japanese);
        let names: Vec<&str> = set.candidates.iter().filter(|c| c.role == Role::Author).map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Other Person"]);
    }

    #[test]
    fn authority_fixes_the_direction_of_a_pair_and_keeps_the_other_as_group() {
        let xml = "<ComicInfo><Writer>Kouji</Writer></ComicInfo>";
        // 題名が `[作者 (サークル)]` の向き（従来のルールは外側=グループと推測して逆にする）でも、Writer で正しくなる。
        let set = build_candidates(&[meta_input("[Kouji (Hawk Bit)] T.zip", Some(xml), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!((cand(&set, Role::Author).unwrap().name.as_str(), cand(&set, Role::Group).unwrap().name.as_str()), ("Kouji", "Hawk Bit"));
        // 通常の向きでも、作者が確定し、残りの括弧はグループとして残る。
        let set = build_candidates(&[meta_input("[Hawk Bit (Kouji)] T.zip", Some(xml), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!((cand(&set, Role::Author).unwrap().name.as_str(), cand(&set, Role::Group).unwrap().name.as_str()), ("Kouji", "Hawk Bit"));
    }

    #[test]
    fn publisher_gives_the_group_and_a_name_missing_from_the_title_is_used_as_is() {
        let xml = "<ComicInfo><Writer>Kouji</Writer><Publisher>Hawk Bit</Publisher></ComicInfo>";
        let set = build_candidates(&[meta_input("plain title.zip", Some(xml), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(cand(&set, Role::Author).unwrap().name, "Kouji");
        assert_eq!(cand(&set, Role::Group).unwrap().name, "Hawk Bit");
        assert!(!cand(&set, Role::Author).unwrap().unconfirmed);
    }

    #[test]
    fn writer_name_variants_are_matched_to_the_title_token_by_similarity() {
        // 翻訳・誤字のブレ（Koji / Kouji）。題名の括弧に同じ名前があるとみなして、その表記を登録名にする。
        let xml = "<ComicInfo><Writer>Koji</Writer></ComicInfo>";
        let set = build_candidates(&[meta_input("[Kouji] T.zip", Some(xml), None)], &[], &MatchOptions::default(), Lang::Japanese);
        let a: Vec<_> = set.candidates.iter().filter(|c| c.role == Role::Author).collect();
        assert_eq!(a.len(), 1, "同じ人は1つにまとまる");
        let mut both: Vec<String> = std::iter::once(a[0].name.clone()).chain(a[0].alt_forms.clone()).collect();
        both.sort();
        assert_eq!(both, vec!["Koji".to_string(), "Kouji".to_string()], "題名の表記と Writer の表記が、登録名と別名になる");
    }

    #[test]
    fn parody_and_character_are_taken_without_a_rule_but_original_is_not() {
        let xml = "<ComicInfo><Series>Touhou Project</Series></ComicInfo>";
        let json = r#"{"tags":[{"type":"character","name":"Reimu Hakurei"},{"type":"parody","name":"original"}]}"#;
        let set = build_candidates(&[meta_input("t.zip", Some(xml), Some(json))], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(cand(&set, Role::Parody).unwrap().name, "Touhou Project");
        assert_eq!(cand(&set, Role::Character).unwrap().name, "Reimu Hakurei");
        assert!(cand(&set, Role::Parody).unwrap().is_adopted());
    }

    #[test]
    fn rules_still_apply_to_authority_names() {
        let set = build_with_rules(&[meta_input("[Kirome] AI generated Book.zip", Some(XML_SAMPLE), None)], &[], &[preset_rule(Preset::AiGenerated, 1)]);
        let a = cand(&set, Role::Author).unwrap();
        assert!(a.review && !a.is_adopted(), "XML 由来の作者も、ルールが無効にしたら未採用・要確認");
        assert_eq!(a.rule_reason.as_deref(), Some("AI generated"));
    }

    #[test]
    fn xml_title_replaces_the_info_txt_title_and_files_without_meta_are_unchanged() {
        // info.txt の見出しは `[Wrong (Name)]` だが、XML の Title と Writer を優先する。
        let info = parse_info_txt("[Wrong (Name)] Book\n\nCategory: Doujinshi\n");
        let mut inp = meta_input("a.zip", Some("<ComicInfo><Title>[Hawk Bit (Kouji)] Book</Title><Writer>Kouji</Writer></ComicInfo>"), None);
        inp.info = Some(info);
        let set = build_candidates(&[inp], &[], &MatchOptions::default(), Lang::Japanese);
        let names: Vec<&str> = set.candidates.iter().filter(|c| matches!(c.role, Role::Author | Role::Group)).map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Kouji") && names.contains(&"Hawk Bit") && !names.contains(&"Wrong") && !names.contains(&"Name"), "{names:?}");
        // メイン（Category）は、XML に Genre が無いので info.txt のまま。
        assert_eq!(cand(&set, Role::Main).unwrap().name, "Doujinshi");
        // meta の無いファイルは、従来どおり。
        let set = build_candidates(&[input("x.zip", "zip", Some(SAMPLE_INFO))], &master(), &MatchOptions::default(), Lang::Japanese);
        assert_eq!(find(&set, Role::Group, "Hawk Bit").choice, Choice::Existing { cat_idx: 3, tier_id: 30 });
    }

    // ── 属性タグ・年月（「正の情報」があるときだけ） ──

    fn attr_names(set: &CandidateSet) -> Vec<String> {
        let mut v: Vec<String> = set.candidates.iter().filter(|c| c.role == Role::Attribute).map(|c| c.name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn xml_tags_and_year_month_become_attribute_and_year_month_candidates() {
        let set = build_candidates(&[meta_input("a.zip", Some(XML_SAMPLE), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(attr_names(&set), vec!["bald", "elf", "very long hair"]);
        let ym = cand(&set, Role::YearMonth).unwrap();
        assert_eq!(ym.name, "2023-12");
        assert!(ym.is_adopted() && set.candidates.iter().filter(|c| c.role == Role::Attribute).all(|c| c.is_adopted() && c.choice == Choice::New));
        assert_eq!((Role::Attribute.window(), Role::YearMonth.window()), (Window::One, Window::One));
        // 新規カテゴリの名前（既存が無いとき）。
        assert_eq!((Role::Attribute.new_category_name(Lang::Japanese), Role::YearMonth.new_category_name(Lang::Japanese)), ("属性", "年月"));
    }

    #[test]
    fn json_tags_are_attributes_too_and_a_tag_named_group_is_just_a_tag() {
        let set = build_candidates(&[meta_input("a.zip", None, Some(JSON_SAMPLE))], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(attr_names(&set), vec!["group"]);
        assert!(cand(&set, Role::Group).is_none());
        assert!(cand(&set, Role::YearMonth).is_none(), "JSON だけでは年月は取らない");
    }

    #[test]
    fn year_without_month_is_year_only_and_month_without_year_is_dropped() {
        let set = build_candidates(&[meta_input("a.zip", Some("<ComicInfo><Year>2020</Year></ComicInfo>"), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert_eq!(cand(&set, Role::YearMonth).unwrap().name, "2020");
        let set = build_candidates(&[meta_input("a.zip", Some("<ComicInfo><Month>5</Month><Tags>x</Tags></ComicInfo>"), None)], &[], &MatchOptions::default(), Lang::Japanese);
        assert!(cand(&set, Role::YearMonth).is_none());
    }

    #[test]
    fn no_attributes_or_year_month_without_authority_data() {
        // info.txt・ファイル名だけのファイルからは、取らない。
        let set = build_candidates(&[input("[A (B)] t.zip", "zip", Some("[A (B)] t\n\nCategory: Doujinshi\nTags:\n> female: elf\n"))], &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().all(|c| !matches!(c.role, Role::Attribute | Role::YearMonth)));
    }

    #[test]
    fn attributes_attach_to_an_existing_element_in_any_non_main_category() {
        let master = vec![
            cat("main", true, None, vec![tier(1, "Manga")]),
            cat("種族", false, None, vec![tier(60, "Elf"), tier(61, "Dwarf")]),
            cat("年月", false, None, vec![tier(70, "2023-12")]),
        ];
        let set = build_candidates(&[meta_input("a.zip", Some(XML_SAMPLE), None)], &master, &MatchOptions::default(), Lang::Japanese);
        let choice = |name: &str| set.candidates.iter().find(|c| c.role == Role::Attribute && c.name == name).unwrap().choice.clone();
        assert_eq!(choice("elf"), Choice::Existing { cat_idx: 1, tier_id: 60 }, "別カテゴリ（種族）の Elf に付ける");
        assert_eq!(choice("bald"), Choice::New, "どこにも無ければ新規作成");
        assert_eq!(cand(&set, Role::YearMonth).unwrap().choice, Choice::Existing { cat_idx: 2, tier_id: 70 });
        // 属性タグは類似の提案を出さない（`elves` が `elf` に似ていても、新規のまま）。
        let json = r#"{"tags":[{"type":"tag","name":"elves"}]}"#;
        let set = build_candidates(&[meta_input("a.zip", None, Some(json))], &master, &MatchOptions::default(), Lang::Japanese);
        let c = set.candidates.iter().find(|c| c.role == Role::Attribute).unwrap();
        assert!(c.suggestions.is_empty() && c.choice == Choice::New);
    }

    #[test]
    fn year_months_are_never_suggested_as_similar_and_attribute_clusters_are_unique() {
        let master = vec![cat("年月", false, None, vec![tier(70, "2023-02")])];
        let set = build_candidates(&[meta_input("a.zip", Some("<ComicInfo><Year>2023</Year><Month>12</Month></ComicInfo>"), None)], &master, &MatchOptions::default(), Lang::Japanese);
        let ym = cand(&set, Role::YearMonth).unwrap();
        assert!(ym.suggestions.is_empty() && ym.choice == Choice::New && ym.is_adopted());
        // 束（類似品）の番号は、種類をまたいで重ならない。属性・年月は束ねない。
        let set = build_candidates(&[meta_input("a.zip", Some(XML_SAMPLE), Some(JSON_SAMPLE))], &[], &MatchOptions::default(), Lang::Japanese);
        let mut seen = std::collections::HashMap::new();
        for c in &set.candidates {
            let e = seen.entry(c.cluster).or_insert(c.role);
            assert_eq!(*e, c.role, "束 {} が種類をまたいでいる", c.cluster);
        }
        let attr_clusters: Vec<usize> = set.candidates.iter().filter(|c| c.role == Role::Attribute).map(|c| c.cluster).collect();
        let mut uniq = attr_clusters.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), attr_clusters.len());
    }

    #[test]
    fn a_rule_can_disable_attributes_and_year_month() {
        let mut r = preset_rule(Preset::AiGenerated, 1);
        r.effect.disable = [RuleRole::Attribute, RuleRole::YearMonth].into_iter().collect();
        r.effect.enable.clear();
        let set = build_with_rules(&[meta_input("AI generated.zip", Some(XML_SAMPLE), None)], &[], &[r]);
        assert!(set.candidates.iter().filter(|c| matches!(c.role, Role::Attribute | Role::YearMonth)).all(|c| c.review && !c.is_adopted()));
        assert!(cand(&set, Role::Author).unwrap().is_adopted(), "無効にしていない種類はそのまま");
    }
}
