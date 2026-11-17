//! 右クリック「タグ検索」用の、検索の切り口になる名前の解決（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 種類（`SEARCH_ROLES`）ごとに、①ファイルに付いているタグ（DB。tier_id → その種類のカテゴリの要素名）を優先し、
//! ②DBにその種類が無いものだけ、ファイル名からの抽出（学習モデル・ルールつきの候補生成を1件分だけ回す）で補う。
//! 別名つきの要素（`A|B|C`）は1名として扱い、検索語は主表記（先頭の別名）を使う。
//! 演者・キャラクター・パロディ元は、ファイル名からはタグ抽出ルールで有効にしたときだけ出る（DBのタグからは常に取れる）。
//! 検索語の結び方は `search_text`（スペース区切り＝新モード検索のOR）。

use crate::i18n::Lang;
use crate::tag_candidates::{build_candidates_full, find_category, CandidateInput, CandidateSet, CatRef, Choice, Role};
use crate::tag_master::{normalize_key, primary_alias};
use crate::tag_model::Model;
use crate::tag_rules::TagRule;
use crate::tag_similarity::MatchOptions;
use crate::view_explorer::TagManagerCategoryUi;

/// 検索の切り口にする種類と、メニューに並べる順。形式・メイン・属性は含めない。
pub(crate) const SEARCH_ROLES: [Role; 8] =
    [Role::Group, Role::Author, Role::Performer, Role::Character, Role::Parody, Role::Event, Role::Language, Role::YearMonth];

/// 名前の出どころ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeySource {
    /// ファイルに付いているタグ（DB）。
    Db,
    /// ファイル名からの抽出（学習データ・ルールつき）。
    FileName,
}

/// 検索の切り口になる名前。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyName {
    /// `SEARCH_ROLES` のどれか。
    pub role: Role,
    /// 表示と検索に使う主表記（別名つきなら先頭の別名）。
    pub name: String,
    /// 別名つきの要素名全体（`A|B|C`）。別名が無ければ `name` と同じ。ホバー表示用。
    pub full: String,
    pub source: KeySource,
}

/// 重複（同じ種類で、正規化した主表記が同じ）を除いて足す。
fn push_unique(out: &mut Vec<KeyName>, item: KeyName) {
    let key = normalize_key(&item.name);
    if key.is_empty() || out.iter().any(|c| c.role == item.role && normalize_key(&c.name) == key) {
        return;
    }
    out.push(item);
}

fn make(role: Role, full: &str, source: KeySource) -> KeyName {
    let name = primary_alias(full).to_string();
    KeyName { role, name, full: full.to_string(), source }
}

/// ある種類の名前だけを取り出す（並びは保つ）。
pub(crate) fn of_role(names: &[KeyName], role: Role) -> Vec<KeyName> {
    names.iter().filter(|n| n.role == role).cloned().collect()
}

/// ファイルに付いているタグ（tier_id）から、各種類の名前を取る。種類の並びは `SEARCH_ROLES`、各種類の中はタグの並び順。
/// その種類のカテゴリが無い、またはそのカテゴリのタグが付いていなければ、その種類は出ない。
pub(crate) fn from_db_tags(tag_ids: &[u64], cats: &[TagManagerCategoryUi]) -> Vec<KeyName> {
    let mut out = Vec::new();
    for role in SEARCH_ROLES {
        let CatRef::Existing(idx) = find_category(cats, role) else { continue };
        for id in tag_ids {
            if let Some(full) = cats[idx].tiers.iter().find(|t| t.id == *id).and_then(|t| t.element.as_deref()) {
                push_unique(&mut out, make(role, full, KeySource::Db));
            }
        }
    }
    out
}

/// 1ファイル分の候補生成の結果（`inputs` は1件）から、各種類の名前を取る。
/// 「付けない」と判定されたもの（学習モデルが無視と判定・ルールで無効）は除く。
/// 既存の要素に当たったものはその要素名（別名つき）、新規のものは抽出した表記を使う。
pub(crate) fn from_candidates(set: &CandidateSet, cats: &[TagManagerCategoryUi]) -> Vec<KeyName> {
    let mut out = Vec::new();
    for role in SEARCH_ROLES {
        for cand in set.candidates.iter().filter(|c| c.role == role && c.files.contains(&0)) {
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
            push_unique(&mut out, make(role, &full, KeySource::FileName));
        }
    }
    out
}

/// ファイル名だけから名前を取る（学習モデル・ルールつき。zip は開かない）。
pub(crate) fn from_file_name(
    file_name: &str,
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    name_lang: Lang,
    model: Option<&Model>,
    rules: &[TagRule],
) -> Vec<KeyName> {
    let input = CandidateInput { file_name: file_name.to_string(), format: None, info: None, meta: None };
    let set = build_candidates_full(&[input], cats, opts, lang, name_lang, model, rules);
    from_candidates(&set, cats)
}

/// 名前の解決。種類ごとに、DBにあればDB、無ければファイル名。
/// 全種類がDBで埋まっているときは、重いファイル名抽出を呼ばない。
pub(crate) fn resolve_keys(
    tag_ids: &[u64],
    file_name: &str,
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    name_lang: Lang,
    model: Option<&Model>,
    rules: &[TagRule],
) -> Vec<KeyName> {
    let mut out = from_db_tags(tag_ids, cats);
    if SEARCH_ROLES.iter().all(|r| out.iter().any(|n| n.role == *r)) {
        return out;
    }
    let from_file = from_file_name(file_name, cats, opts, lang, name_lang, model, rules);
    let db_roles: Vec<Role> = out.iter().map(|n| n.role).collect();
    out.extend(from_file.into_iter().filter(|n| !db_roles.contains(&n.role)));
    // 並びを種類順に直す（同じ種類の中の順は保つ）。
    out.sort_by_key(|n| SEARCH_ROLES.iter().position(|r| *r == n.role));
    out
}

/// 主要タグの取得状況（メニューのステータスボタンの表示と、案内ダイアログの出し分けに使う）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyStatus {
    /// メタデータ（ComicInfo.xml・meta.json）から付与されたタグで取得できている（押せない表示）。
    Precise,
    /// ファイル名からの簡易取得を含む、または詳細フラグが「正の情報」を持たない（案内を開ける）。
    Simple,
    /// どちらでも取得できない（案内を開ける）。
    Unavailable,
}

/// キャッシュの詳細フラグが、高精度の根拠になるメタデータ（ComicInfo.xml または meta.json）を持つか。
/// 未確認・対象外・info.txt のみは持たない扱い（info.txt は「正の情報」より優先度が低いため簡易扱い）。
pub(crate) fn has_precise_meta(flag: crate::neko_dir::DetailFlag) -> bool {
    matches!(flag, crate::neko_dir::DetailFlag::Checked(k) if k.xml || k.json)
}

/// 取得状況を決める。名前が1つも無ければ取得不可。全種類がDBのタグ由来で、かつ詳細フラグが高精度の根拠を持つときだけ高精度。
/// 種類ごとに取得元が混ざるときは、一番低い精度（簡易）に合わせる。
pub(crate) fn key_status(names: &[KeyName], precise_meta: bool) -> KeyStatus {
    if names.is_empty() {
        KeyStatus::Unavailable
    } else if precise_meta && names.iter().all(|n| n.source == KeySource::Db) {
        KeyStatus::Precise
    } else {
        KeyStatus::Simple
    }
}

/// 新モード検索の検索文字列。語はスペース区切り（＝OR）で、空白・引用符を含む名前は `"…"` で囲む。
/// 引用符そのものは語の区切りになるので、名前に含まれていたら空白に置き換える。
pub(crate) fn search_text(names: &[&KeyName]) -> String {
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
            cat("演者", false, None, vec![tier(50, "Mika")]),
        ]
    }

    fn names(v: &[KeyName]) -> Vec<(Role, &str)> {
        v.iter().map(|c| (c.role, c.name.as_str())).collect()
    }

    fn file_name_keys(name: &str) -> Vec<KeyName> {
        from_file_name(name, &master(), &MatchOptions::default(), Lang::Japanese, Lang::Japanese, None, &[])
    }

    #[test]
    fn db_tags_follow_the_role_order_with_primary_alias() {
        // タグの並びに依らず、種類の順（グループ→作者→演者→…→言語）。別名つきは主表記を名前にする。
        let v = from_db_tags(&[40, 50, 20, 30], &master());
        assert_eq!(names(&v), vec![(Role::Group, "Hawk Bit"), (Role::Author, "Kouji"), (Role::Performer, "Mika"), (Role::Language, "日本語")]);
        assert_eq!(v[1].full, "Kouji|Koji");
        assert!(v.iter().all(|c| c.source == KeySource::Db));
    }

    #[test]
    fn db_tags_exclude_main_and_unrelated_ids() {
        // メイン（漫画）は対象の種類ではない。未知のIDも無視。
        assert!(from_db_tags(&[1, 999], &master()).is_empty());
        assert!(from_db_tags(&[], &master()).is_empty());
        // 該当カテゴリが無いマスターでも落ちない。
        let no_cat = vec![cat("main", true, None, vec![tier(1, "漫画")])];
        assert!(from_db_tags(&[1, 20], &no_cat).is_empty());
    }

    #[test]
    fn file_name_matches_existing_elements_and_uses_their_full_name() {
        let v = file_name_keys("(C105) [Hawk Bit (Kouji)] Title.zip");
        assert!(names(&v).contains(&(Role::Group, "Hawk Bit")), "{v:?}");
        let author = v.iter().find(|c| c.role == Role::Author).expect("author");
        assert_eq!((author.name.as_str(), author.full.as_str()), ("Kouji", "Kouji|Koji"));
        // イベントも切り口として出る（新規の表記）。
        assert!(names(&v).contains(&(Role::Event, "C105")), "{v:?}");
        assert!(v.iter().all(|c| c.source == KeySource::FileName));
    }

    #[test]
    fn file_name_new_creator_uses_the_extracted_form() {
        let v = file_name_keys("[Newbie] Title.zip");
        assert!(v.iter().any(|c| c.name == "Newbie"), "{v:?}");
    }

    #[test]
    fn file_name_without_brackets_gives_nothing() {
        assert!(file_name_keys("plain title.zip").is_empty());
    }

    #[test]
    fn resolve_mixes_db_and_file_name_per_role() {
        let opts = MatchOptions::default();
        // 作者はDBにあるので、ファイル名の作者（Kouji）は見ない。グループはDBに無いので、ファイル名から補う。
        let v = resolve_keys(&[21], "[Hawk Bit (Kouji)] T.zip", &master(), &opts, Lang::Japanese, Lang::Japanese, None, &[]);
        assert_eq!(names(&v), vec![(Role::Group, "Hawk Bit"), (Role::Author, "Other")]);
        assert_eq!((v[0].source, v[1].source), (KeySource::FileName, KeySource::Db));
        // DBに何も無ければ、ファイル名だけ。
        let v = resolve_keys(&[], "[Hawk Bit (Kouji)] T.zip", &master(), &opts, Lang::Japanese, Lang::Japanese, None, &[]);
        assert_eq!(names(&v), vec![(Role::Group, "Hawk Bit"), (Role::Author, "Kouji")]);
    }

    #[test]
    fn of_role_picks_only_that_role() {
        let v = from_db_tags(&[40, 20, 21, 30], &master());
        assert_eq!(names(&of_role(&v, Role::Author)), vec![(Role::Author, "Kouji"), (Role::Author, "Other")]);
        assert!(of_role(&v, Role::Parody).is_empty());
    }

    #[test]
    fn status_is_precise_only_when_all_db_and_precise_meta() {
        let mk = |source| KeyName { role: Role::Author, name: "A".into(), full: "A".into(), source };
        let (db, file) = (mk(KeySource::Db), mk(KeySource::FileName));
        // 名前が無ければ取得不可（詳細フラグに依らない）。
        assert_eq!(key_status(&[], true), KeyStatus::Unavailable);
        assert_eq!(key_status(&[], false), KeyStatus::Unavailable);
        // 全部DB由来＋XML/JSONあり → 高精度。
        assert_eq!(key_status(&[db.clone()], true), KeyStatus::Precise);
        // 全部DB由来でも、詳細フラグが根拠を持たない（手動タグ・info.txtのみ・未確認）→ 簡易。
        assert_eq!(key_status(&[db.clone()], false), KeyStatus::Simple);
        // ファイル名由来が1つでも混ざれば簡易（一番低い精度に合わせる）。
        assert_eq!(key_status(&[db.clone(), file.clone()], true), KeyStatus::Simple);
        assert_eq!(key_status(&[file], true), KeyStatus::Simple);
    }

    #[test]
    fn precise_meta_needs_xml_or_json_in_a_checked_flag() {
        use crate::fs::archive::meta::DetailKinds;
        use crate::neko_dir::DetailFlag;
        let kinds = |xml, json, txt| DetailFlag::Checked(DetailKinds { xml, json, txt });
        assert!(has_precise_meta(kinds(true, false, false)));
        assert!(has_precise_meta(kinds(false, true, false)));
        assert!(has_precise_meta(kinds(true, true, true)));
        // info.txt（付属テキスト）のみ・何も無し・未確認・対象外は根拠にならない。
        assert!(!has_precise_meta(kinds(false, false, true)));
        assert!(!has_precise_meta(kinds(false, false, false)));
        assert!(!has_precise_meta(DetailFlag::Unknown));
        assert!(!has_precise_meta(DetailFlag::NotApplicable));
    }

    #[test]
    fn search_text_quotes_spaces_and_dedupes() {
        let mk = |role, name: &str| KeyName { role, name: name.to_string(), full: name.to_string(), source: KeySource::Db };
        let (a, g, dup, q) = (mk(Role::Author, "Kouji"), mk(Role::Group, "Hawk Bit"), mk(Role::Group, "Kouji"), mk(Role::Author, "a\"b"));
        assert_eq!(search_text(&[&a, &g]), "Kouji \"Hawk Bit\"");
        assert_eq!(search_text(&[&a, &dup]), "Kouji");
        assert_eq!(search_text(&[&q]), "\"a b\"");
        assert_eq!(search_text(&[]), "");
    }
}
