//! 「この作者／グループのファイルを検索」用の、作者・グループ名の解決（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 取得の優先順: ①ファイルに付いているタグ（DB。tier_id → 作者／グループのカテゴリの要素名）
//! ②DBに作者・グループが無いときだけ、ファイル名からの抽出（学習モデル・ルールつきの候補生成を1件分だけ回す）。
//! 別名つきの要素（`A|B|C`）は1名として扱い、検索語は主表記（先頭の別名）を使う。
//! 検索語の結び方は `search_text`（スペース区切り＝新モード検索のOR）。

use crate::i18n::Lang;
use crate::tag_candidates::{build_candidates_full, find_category, CandidateInput, CandidateSet, CatRef, Choice, Role};
use crate::tag_master::{normalize_key, primary_alias};
use crate::tag_model::Model;
use crate::tag_rules::TagRule;
use crate::tag_similarity::MatchOptions;
use crate::view_explorer::TagManagerCategoryUi;

/// 名前の出どころ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreatorSource {
    /// ファイルに付いているタグ（DB）。
    Db,
    /// ファイル名からの抽出（学習データ・ルールつき）。
    FileName,
}

/// 検索の候補になる作者名・グループ名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CreatorName {
    /// `Role::Author` か `Role::Group` のどちらか。
    pub role: Role,
    /// 表示と検索に使う主表記（別名つきなら先頭の別名）。
    pub name: String,
    /// 別名つきの要素名全体（`A|B|C`）。別名が無ければ `name` と同じ。ホバー表示用。
    pub full: String,
    pub source: CreatorSource,
}

fn is_creator_role(role: Role) -> bool {
    matches!(role, Role::Author | Role::Group)
}

/// 重複（同じ種類で、正規化した主表記が同じ）を除いて足す。
fn push_unique(out: &mut Vec<CreatorName>, item: CreatorName) {
    let key = normalize_key(&item.name);
    if key.is_empty() || out.iter().any(|c| c.role == item.role && normalize_key(&c.name) == key) {
        return;
    }
    out.push(item);
}

fn make(role: Role, full: &str, source: CreatorSource) -> CreatorName {
    let name = primary_alias(full).to_string();
    CreatorName { role, name, full: full.to_string(), source }
}

/// ファイルに付いているタグ（tier_id）から、作者・グループの名前を取る。
/// 作者・グループのカテゴリが無い、またはそのカテゴリのタグが付いていなければ空。並びは作者→グループ、各カテゴリ内はタグの並び順。
pub(crate) fn from_db_tags(tag_ids: &[u64], cats: &[TagManagerCategoryUi]) -> Vec<CreatorName> {
    let mut out = Vec::new();
    for role in [Role::Author, Role::Group] {
        let CatRef::Existing(idx) = find_category(cats, role) else { continue };
        for id in tag_ids {
            if let Some(full) = cats[idx].tiers.iter().find(|t| t.id == *id).and_then(|t| t.element.as_deref()) {
                push_unique(&mut out, make(role, full, CreatorSource::Db));
            }
        }
    }
    out
}

/// 1ファイル分の候補生成の結果（`inputs` は1件）から、作者・グループの名前を取る。
/// 「付けない」と判定されたもの（学習モデルが無視と判定・ルールで無効）は除く。
/// 既存の要素に当たったものはその要素名（別名つき）、新規のものは抽出した表記を使う。
pub(crate) fn from_candidates(set: &CandidateSet, cats: &[TagManagerCategoryUi]) -> Vec<CreatorName> {
    let mut out = Vec::new();
    // 作者→グループの順に並べる（候補の並びに依らない）。
    for role in [Role::Author, Role::Group] {
        for cand in set.candidates.iter().filter(|c| c.role == role && is_creator_role(c.role) && c.files.contains(&0)) {
            let full: String = match &cand.choice {
                Choice::Skip => continue,
                Choice::New => cand.name.clone(),
                Choice::Existing { cat_idx, tier_id } | Choice::Alias { cat_idx, tier_id } => {
                    match cats.get(*cat_idx).and_then(|c| c.tiers.iter().find(|t| t.id == *tier_id)).and_then(|t| t.element.clone()) {
                        Some(e) => e,
                        None => cand.name.clone(),
                    }
                }
            };
            push_unique(&mut out, make(role, &full, CreatorSource::FileName));
        }
    }
    out
}

/// ファイル名だけから作者・グループの名前を取る（学習モデル・ルールつき。zip は開かない）。
pub(crate) fn from_file_name(
    file_name: &str,
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    name_lang: Lang,
    model: Option<&Model>,
    rules: &[TagRule],
) -> Vec<CreatorName> {
    let input = CandidateInput { file_name: file_name.to_string(), format: None, info: None, meta: None };
    let set = build_candidates_full(&[input], cats, opts, lang, name_lang, model, rules);
    from_candidates(&set, cats)
}

/// 名前の解決。DBに作者・グループがあればそれだけを返し、無いときだけ `from_file_name` を呼ぶ（重い方を避ける）。
pub(crate) fn resolve_creators(
    tag_ids: &[u64],
    file_name: &str,
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    name_lang: Lang,
    model: Option<&Model>,
    rules: &[TagRule],
) -> Vec<CreatorName> {
    let db = from_db_tags(tag_ids, cats);
    if !db.is_empty() {
        return db;
    }
    from_file_name(file_name, cats, opts, lang, name_lang, model, rules)
}

/// 新モード検索の検索文字列。語はスペース区切り（＝OR）で、空白・引用符を含む名前は `"…"` で囲む。
/// 引用符そのものは語の区切りになるので、名前に含まれていたら空白に置き換える。
pub(crate) fn search_text(names: &[&CreatorName]) -> String {
    let mut terms: Vec<String> = Vec::new();
    for n in names {
        let clean: String = n.name.chars().map(|c| if matches!(c, '"' | '\u{201C}' | '\u{201D}') { ' ' } else { c }).collect();
        let t = clean.trim();
        if t.is_empty() {
            continue;
        }
        let term = if t.contains([' ', '\u{3000}']) { format!("\"{t}\"") } else { t.to_string() };
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_master::SystemKind;
    use crate::view_explorer::TagManagerTierUi;

    fn tier(id: u64, name: &str) -> TagManagerTierUi {
        TagManagerTierUi { id, tier_no: id as i32, negative: false, element: Some(name.to_string()) }
    }

    fn cat(name: &str, is_main: bool, kind: Option<SystemKind>, tiers: Vec<TagManagerTierUi>) -> TagManagerCategoryUi {
        TagManagerCategoryUi { name: name.to_string(), tiers, color: egui::Color32::GRAY, is_main, single_select: is_main, system_kind: kind }
    }

    fn master() -> Vec<TagManagerCategoryUi> {
        vec![
            cat("main", true, None, vec![tier(1, "漫画")]),
            cat("作者", false, None, vec![tier(20, "Kouji|Koji"), tier(21, "Other")]),
            cat("グループ", false, None, vec![tier(30, "Hawk Bit")]),
            cat("言語", false, None, vec![tier(40, "日本語")]),
        ]
    }

    fn names(v: &[CreatorName]) -> Vec<(Role, &str)> {
        v.iter().map(|c| (c.role, c.name.as_str())).collect()
    }

    #[test]
    fn db_tags_give_author_then_group_with_primary_alias() {
        // タグの並びが 言語・グループ・作者 でも、作者→グループの順。別名つきは主表記を名前にする。
        let v = from_db_tags(&[40, 30, 20], &master());
        assert_eq!(names(&v), vec![(Role::Author, "Kouji"), (Role::Group, "Hawk Bit")]);
        assert_eq!(v[0].full, "Kouji|Koji");
        assert!(v.iter().all(|c| c.source == CreatorSource::Db));
    }

    #[test]
    fn db_tags_without_creators_are_empty() {
        assert!(from_db_tags(&[1, 40], &master()).is_empty());
        assert!(from_db_tags(&[], &master()).is_empty());
        // 作者・グループのカテゴリが無いマスターでも落ちない。
        let no_cat = vec![cat("main", true, None, vec![tier(1, "漫画")])];
        assert!(from_db_tags(&[1, 20], &no_cat).is_empty());
    }

    #[test]
    fn file_name_matches_existing_elements_and_uses_their_full_name() {
        let v = from_file_name("(C105) [Hawk Bit (Kouji)] Title.zip", &master(), &MatchOptions::default(), Lang::Japanese, Lang::Japanese, None, &[]);
        assert_eq!(names(&v), vec![(Role::Author, "Kouji"), (Role::Group, "Hawk Bit")]);
        assert_eq!(v[0].full, "Kouji|Koji");
        assert!(v.iter().all(|c| c.source == CreatorSource::FileName));
    }

    #[test]
    fn file_name_new_creator_uses_the_extracted_form() {
        let v = from_file_name("[Newbie] Title.zip", &master(), &MatchOptions::default(), Lang::Japanese, Lang::Japanese, None, &[]);
        assert!(v.iter().any(|c| c.name == "Newbie"), "{v:?}");
    }

    #[test]
    fn file_name_without_brackets_gives_nothing() {
        let v = from_file_name("plain title.zip", &master(), &MatchOptions::default(), Lang::Japanese, Lang::Japanese, None, &[]);
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn resolve_prefers_db_and_falls_back_to_file_name() {
        let opts = MatchOptions::default();
        // DBに作者があれば、ファイル名の別の作者は見ない。
        let v = resolve_creators(&[21], "[Hawk Bit (Kouji)] T.zip", &master(), &opts, Lang::Japanese, Lang::Japanese, None, &[]);
        assert_eq!(names(&v), vec![(Role::Author, "Other")]);
        // DBに作者・グループが無ければ、ファイル名から。
        let v = resolve_creators(&[40], "[Hawk Bit (Kouji)] T.zip", &master(), &opts, Lang::Japanese, Lang::Japanese, None, &[]);
        assert_eq!(names(&v), vec![(Role::Author, "Kouji"), (Role::Group, "Hawk Bit")]);
    }

    #[test]
    fn search_text_quotes_spaces_and_dedupes() {
        let mk = |role, name: &str| CreatorName { role, name: name.to_string(), full: name.to_string(), source: CreatorSource::Db };
        let (a, g, dup, q) = (mk(Role::Author, "Kouji"), mk(Role::Group, "Hawk Bit"), mk(Role::Group, "Kouji"), mk(Role::Author, "a\"b"));
        assert_eq!(search_text(&[&a, &g]), "Kouji \"Hawk Bit\"");
        assert_eq!(search_text(&[&a, &dup]), "Kouji");
        assert_eq!(search_text(&[&q]), "\"a b\"");
        assert_eq!(search_text(&[]), "");
    }
}
