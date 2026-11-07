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

use std::collections::HashMap;

use crate::i18n::Lang;
use crate::tag_master::{normalize_key, SystemKind};
use crate::tag_similarity::{cluster, keys, normalize_width, similarity_at_least, Keys, MatchOptions};
use crate::tag_source::{
    canonical_language, language_english_name, parse_info_txt, parse_name_parts, split_archive_extension, Creator,
    FormatTag, InfoTxt, NameParts,
};
use crate::view_explorer::TagManagerCategoryUi;

/// 新規要素数の警告・エラーのしきい値（窓ごと）。
pub(crate) const WARN_NEW_ELEMENTS: usize = 1000;
pub(crate) const ERROR_NEW_ELEMENTS: usize = 3000;
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
            Role::Author | Role::Group => Window::Two,
            _ => Window::One,
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
    pub name: String,
    pub score: u8,
}

/// 候補の扱い。初期値は生成時の規則で決まり、確認ウィンドウで変えられる。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// 既存の要素に付ける（採用）。
    Existing { cat_idx: usize, tier_id: u64 },
    /// 新しい要素を作って付ける（採用）。
    New,
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
}

impl Candidate {
    /// 付ける（採用）か。
    pub(crate) fn is_adopted(&self) -> bool {
        !matches!(self.choice, Choice::Skip)
    }

    /// 採用にする。初期が既存の完全一致ならそれ、類似提案があれば最上位、無ければ新規作成。
    pub(crate) fn adopt(&mut self) {
        if self.is_adopted() {
            return;
        }
        self.choice = match (&self.initial, self.suggestions.first()) {
            (Choice::Existing { .. }, _) => self.initial.clone(),
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
        !matches!(self.choice, Choice::Existing { .. })
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
}

/// 窓ごとの新規要素数と、警告・エラーの判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LimitReport {
    pub window1_new: usize,
    pub window2_new: usize,
    pub window1_warn: bool,
    pub window2_warn: bool,
    /// どちらか一方でも上限を超えたとき、超えた側の新規要素数（両方なら多い方）。
    pub error_count: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidateSet {
    pub candidates: Vec<Candidate>,
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
    items: Vec<(u64, String, Keys)>,
}

fn element_index(cats: &[TagManagerCategoryUi], cat: CatRef, opts: &MatchOptions) -> Option<ElementIndex> {
    let CatRef::Existing(idx) = cat else { return None };
    let items = cats[idx]
        .tiers
        .iter()
        .filter_map(|t| t.element.as_ref().map(|e| (t.id, e.clone(), keys(e, opts))))
        .collect();
    Some(ElementIndex { cat_idx: idx, items })
}

impl ElementIndex {
    /// 完全一致する要素。
    fn exact(&self, k: &Keys) -> Option<(u64, &str)> {
        if k.exact.is_empty() {
            return None;
        }
        self.items.iter().find(|(_, _, ek)| ek.exact == k.exact).map(|(id, n, _)| (*id, n.as_str()))
    }

    /// しきい値以上の類似要素（完全一致を除く）。
    fn similar(&self, k: &Keys, threshold: u8) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = self
            .items
            .iter()
            .filter(|(_, _, ek)| ek.exact != k.exact)
            .filter_map(|(id, name, ek)| {
                similarity_at_least(k, ek, threshold)
                    .map(|score| Suggestion { cat_idx: self.cat_idx, tier_id: *id, name: name.clone(), score })
            })
            .collect();
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
    Sources { event, creator, language }
}

// ── 候補生成 ──────────────────────────────────────────────────────────

/// 候補を作る。`cats` は現在のタグマスター（読み取りのみ）。
pub(crate) fn build_candidates(
    inputs: &[CandidateInput],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
) -> CandidateSet {
    let opts = opts.clamped();
    let cat_of = |role: Role| find_category(cats, role);
    let (author_cat, group_cat) = (cat_of(Role::Author), cat_of(Role::Group));
    let author_idx = element_index(cats, author_cat, &opts);
    let group_idx = element_index(cats, group_cat, &opts);

    // 1) ファイルごとの言及を集める。
    let mut mentions: Vec<(usize, Mention)> = Vec::new();
    let mut files_with_info = 0usize;
    for (fi, input) in inputs.iter().enumerate() {
        if input.info.is_some() {
            files_with_info += 1;
        }
        if let Some(tag) = &input.format {
            mentions.push((fi, Mention { role: Role::Format, name: format_name(tag, lang), from_single: false, guessed: false }));
        }
        if let Some(cat) = input.info.as_ref().and_then(|i| i.category.clone()) {
            mentions.push((fi, Mention { role: Role::Main, name: cat, from_single: false, guessed: false }));
        }
        let src = gather_sources(input);
        if let Some(canon) = src.language {
            let name = if lang == Lang::Japanese { canon } else { language_english_name(canon) };
            mentions.push((fi, Mention { role: Role::Language, name: name.to_string(), from_single: false, guessed: false }));
        }
        if let Some(ev) = src.event {
            mentions.push((fi, Mention { role: Role::Event, name: ev, from_single: false, guessed: false }));
        }
        match src.creator {
            Some(Creator::Pair { outer, inner }) => {
                let ev = |idx: &Option<ElementIndex>, n: &str| idx.as_ref().map_or(0, |ix| ix.evidence(&keys(n, &opts), opts.threshold));
                let normal = ev(&group_idx, &outer) + ev(&author_idx, &inner);
                let swapped = ev(&author_idx, &outer) + ev(&group_idx, &inner);
                let swap = swapped > normal;
                let (outer_role, inner_role) = if swap { (Role::Author, Role::Group) } else { (Role::Group, Role::Author) };
                let guessed = normal == 0 && swapped == 0;
                mentions.push((fi, Mention { role: outer_role, name: outer, from_single: false, guessed }));
                mentions.push((fi, Mention { role: inner_role, name: inner, from_single: false, guessed }));
            }
            Some(Creator::Single(x)) => {
                // 既存の作者・グループに完全一致があれば、その側。無ければ作者の候補（未確定）。
                let k = keys(&x, &opts);
                let in_author = author_idx.as_ref().is_some_and(|ix| ix.exact(&k).is_some());
                let in_group = group_idx.as_ref().is_some_and(|ix| ix.exact(&k).is_some());
                let role = if !in_author && in_group { Role::Group } else { Role::Author };
                mentions.push((fi, Mention { role, name: x, from_single: true, guessed: false }));
            }
            None => {}
        }
    }

    // 2) 種類＋完全一致キーで候補にまとめる。
    struct Acc {
        role: Role,
        key: String,
        forms: Vec<(String, usize)>,
        files: Vec<usize>,
        from_single: bool,
        guessed: bool,
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
            Acc { role: m.role, key: ident.clone(), forms: Vec::new(), files: Vec::new(), from_single: false, guessed: false }
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
    }

    // 3) 候補ごとに、既存タグとの照合・初期の扱い・登録名を決める。
    let format_ix = element_index(cats, cat_of(Role::Format), &opts);
    let main_ix = element_index(cats, cat_of(Role::Main), &opts);
    let lang_ix = element_index(cats, cat_of(Role::Language), &opts);
    let event_ix = element_index(cats, cat_of(Role::Event), &opts);
    let mut candidates: Vec<Candidate> = Vec::new();
    for (role, ident) in order {
        let acc = accs.remove(&(role, ident)).unwrap();
        let picked = pick_registration_name(&acc.forms, lang);
        // 全角半角の正規化がONなら、登録名も正規化する（半角カナ・全角英数をそのまま登録しない）。
        let name = if opts.width { normalize_width(&picked) } else { picked };
        let k = keys(&name, &opts);
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
                        .find(|(_, n, ek)| canonical_language(n).is_some_and(|c| c == acc.key) || ek.exact == k.exact)
                        .map(|(id, _, _)| (ix.cat_idx, *id));
                }
            }
            Role::Format | Role::Main | Role::Event => {
                let ix = match role {
                    Role::Format => &format_ix,
                    Role::Main => &main_ix,
                    _ => &event_ix,
                };
                if let Some(ix) = ix {
                    existing = ix.exact(&k).map(|(id, _)| (ix.cat_idx, id));
                    // 形式は拡張子の完全一致だけ。メイン・イベントは類似も提案する。
                    if existing.is_none() && role != Role::Format {
                        suggestions = ix.similar(&k, opts.threshold);
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
                        existing = ix.exact(&k).map(|(id, _)| (ix.cat_idx, id));
                    }
                }
                if existing.is_none() {
                    for ix in &pools {
                        suggestions.extend(ix.similar(&k, opts.threshold));
                    }
                    suggestions.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
                }
            }
        }
        suggestions.truncate(MAX_SUGGESTIONS);
        let choice = match (existing, suggestions.is_empty()) {
            (Some((cat_idx, tier_id)), _) => Choice::Existing { cat_idx, tier_id },
            (None, false) => Choice::Skip,
            (None, true) => Choice::New,
        };
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
        });
    }

    // 4) 類似品の束（同じ種類の中で）。番号は種類をまたいで一意にする。
    let mut next_cluster = 0usize;
    for role in [Role::Format, Role::Main, Role::Language, Role::Event, Role::Author, Role::Group] {
        let idxs: Vec<usize> = (0..candidates.len()).filter(|&i| candidates[i].role == role).collect();
        let names: Vec<String> = idxs.iter().map(|&i| candidates[i].name.clone()).collect();
        let ids = cluster(&names, &opts);
        let base = next_cluster;
        for (pos, &i) in idxs.iter().enumerate() {
            candidates[i].cluster = base + ids[pos];
            next_cluster = next_cluster.max(base + ids[pos] + 1);
        }
    }

    let limits = check_limits(&candidates);
    CandidateSet { candidates, limits, file_count: inputs.len(), files_with_info }
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

/// 窓ごとの新規要素数を数え、警告・エラーを判定する。
fn check_limits(candidates: &[Candidate]) -> LimitReport {
    let count = |w: Window| candidates.iter().filter(|c| c.role.window() == w && c.needs_new_element()).count();
    let (w1, w2) = (count(Window::One), count(Window::Two));
    let over: Vec<usize> = [w1, w2].into_iter().filter(|&n| n > ERROR_NEW_ELEMENTS).collect();
    LimitReport {
        window1_new: w1,
        window2_new: w2,
        window1_warn: w1 > WARN_NEW_ELEMENTS,
        window2_warn: w2 > WARN_NEW_ELEMENTS,
        error_count: over.into_iter().max(),
    }
}

impl CandidateSet {
    /// 窓ごとの候補の番号（種類順、束の最初の出現順、名前順）。
    pub(crate) fn indices_in_window(&self, w: Window) -> Vec<usize> {
        let order = |r: Role| match r {
            Role::Format => 0,
            Role::Main => 1,
            Role::Language => 2,
            Role::Event => 3,
            Role::Author => 4,
            Role::Group => 5,
        };
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

/// info.txt の本文から入力を作る（走査結果の変換用）。本文が空でも `Some`（info.txt はあった）。
pub(crate) fn input_from_info_text(file_name: &str, format: Option<FormatTag>, info_text: Option<&str>) -> CandidateInput {
    CandidateInput { file_name: file_name.to_string(), format, info: info_text.map(parse_info_txt) }
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
    fn limits_warn_per_window_and_error_when_either_exceeds() {
        let o = MatchOptions::default();
        let many = |n: usize, prefix: &str| -> Vec<CandidateInput> {
            (0..n).map(|i| input(&format!("[{prefix}{i:05}] t.zip"), "zip", None)).collect()
        };
        // 作者1500件: 窓2が警告（エラーではない）。
        let set = build_candidates(&many(1500, "Writer"), &master(), &o, Lang::English);
        assert_eq!((set.limits.window2_new, set.limits.window2_warn, set.limits.error_count), (1500, true, None));
        assert!(!set.limits.window1_warn);
        // 作者3001件: エラー。Nは超過した側の数。
        let set = build_candidates(&many(3001, "Writer"), &master(), &o, Lang::English);
        assert_eq!(set.limits.error_count, Some(3001));
        // 窓1だけが超える場合も（イベントを3001個）エラー。
        let events: Vec<CandidateInput> = (0..3001).map(|i| input(&format!("(Event{i:05}) t.zip"), "zip", None)).collect();
        let set = build_candidates(&events, &master(), &o, Lang::English);
        assert_eq!((set.limits.window1_new, set.limits.error_count), (3001, Some(3001)));
        // 既存に一致するものは新規に数えない。
        let set = build_candidates(&[input("[Hawk Bit (Kouji)] t.zip", "zip", None)], &master(), &o, Lang::English);
        assert_eq!((set.limits.window1_new, set.limits.window2_new), (0, 0));
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
        assert_eq!(fresh.limits.window2_new, 1, "上限の集計も引き継ぎ後でやり直す（Zzzz は Skip でも新規の数に入る）");
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
        assert_eq!((set.file_count, set.limits.error_count), (0, None));
        let weird = [input("[] ().zip", "zip", Some("")), input("[(A)] t.zip", "zip", Some("Category:   \n"))];
        let set = build_candidates(&weird, &[], &MatchOptions::default(), Lang::Japanese);
        assert!(set.candidates.iter().any(|c| c.role == Role::Author && c.name == "A"));
    }
}
