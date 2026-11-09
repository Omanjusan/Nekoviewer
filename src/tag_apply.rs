//! 一括タグ付けの適用の中核（画面に触れない）。
//!
//! - `plan_application`: 採用された候補から、マスターへの追加（新規カテゴリ・新規要素）と、ファイルごとに付けたいタグIDを作る。
//!   マスターは渡されたコピーに対して変更する（実際の保存・反映は呼び出し側）
//! - `run_apply`: DBのバックアップ → マスターの保存 → ファイルごとのタグ書き込み、を順に行う。バックアップに失敗したら、
//!   何も変更せずに中止する。既存のタグは消さず、排他の規則を守って足す（`tag_master::merge_file_tags_in`）

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use redb::Database;

use crate::i18n::Lang;
use crate::tag_candidates::{CandidateSet, CatRef, Choice, Role};
use crate::tag_trial::{TrialFile, TrialRecord};
use crate::tag_master::{alias_keys, join_aliases, merge_file_tags_in, split_aliases, TagMaster};
use crate::view_explorer::TagManagerCategoryUi;

/// 適用の計画。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// ファイルの番号（走査結果・候補の `files` と同じ番号）→ 付けたいタグID（候補の順）。
    pub file_additions: HashMap<usize, Vec<u64>>,
    pub new_categories: usize,
    pub new_elements: usize,
    /// 別名を足して要素名を書き換えたもの（カテゴリ番号, tier_id, 書き換え前, 書き換え後）。
    pub renames: Vec<(usize, u64, String, String)>,
    /// 別名が他の要素の別名と重なるため、書き換えずにタグだけ付けた件数。
    pub alias_conflicts: usize,
}

fn role_order(r: Role) -> u8 {
    r.order()
}

/// 採用された候補を、マスター（`cats`・`next_tier_id`。呼び出し側の写し）への追加と、ファイルごとのタグIDにする。
/// 「付けない」の候補は何もしない。既存の要素を指す候補が、いまのマスターに無ければ飛ばす。
pub(crate) fn plan_application(
    set: &CandidateSet,
    cats: &mut Vec<TagManagerCategoryUi>,
    next_tier_id: &mut u64,
    lang: Lang,
) -> Plan {
    let mut plan = Plan::default();
    let mut created_cat_for_role: HashMap<Role, usize> = HashMap::new();
    let mut order: Vec<usize> = (0..set.candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (ca, cb) = (&set.candidates[a], &set.candidates[b]);
        role_order(ca.role).cmp(&role_order(cb.role)).then(ca.name.to_lowercase().cmp(&cb.name.to_lowercase()))
    });
    for i in order {
        let c = &set.candidates[i];
        let tier_id = match &c.choice {
            Choice::Skip => continue,
            Choice::Existing { tier_id, .. } => {
                let exists = cats.iter().any(|cat| cat.tiers.iter().any(|t| t.id == *tier_id && t.element.is_some()));
                if !exists {
                    continue;
                }
                *tier_id
            }
            Choice::Alias { cat_idx, tier_id } => {
                match add_aliases_to_element(cats, next_tier_id, *cat_idx, *tier_id, &c.alias_list()) {
                    AliasOutcome::Renamed(old, new) => plan.renames.push((*cat_idx, *tier_id, old, new)),
                    AliasOutcome::AlreadyHas => {}
                    AliasOutcome::Conflict => plan.alias_conflicts += 1,
                    AliasOutcome::MissingTarget => continue,
                }
                *tier_id
            }
            Choice::New => {
                let mut master = TagMaster::new(cats, next_tier_id);
                let cat_idx = match c.category {
                    CatRef::Existing(idx) if idx < master.len() => idx,
                    _ => match created_cat_for_role.get(&c.role) {
                        Some(&idx) => idx,
                        None => {
                            let idx = master.add_category(c.role.new_category_name(lang).to_string(), false, None);
                            created_cat_for_role.insert(c.role, idx);
                            plan.new_categories += 1;
                            idx
                        }
                    },
                };
                let Some((id, created)) = master.add_element_unique(cat_idx, &c.registration_name()) else { continue };
                if created {
                    plan.new_elements += 1;
                }
                id
            }
        };
        for &f in &c.files {
            let list = plan.file_additions.entry(f).or_default();
            if !list.contains(&tier_id) {
                list.push(tier_id);
            }
        }
    }
    plan
}

enum AliasOutcome {
    /// 要素名を（書き換え前, 書き換え後）にした。
    Renamed(String, String),
    /// 既に同じ別名を持っている（書き換え不要）。
    AlreadyHas,
    /// 別の要素の別名と重なるので書き換えない。
    Conflict,
    /// 対象の要素がマスターに無い。
    MissingTarget,
}

/// `cat_idx` の `tier_id` の要素に別名 `alias` を足す。重なる別名が他の要素にあれば書き換えない。
fn add_aliases_to_element(
    cats: &mut Vec<TagManagerCategoryUi>,
    next_tier_id: &mut u64,
    cat_idx: usize,
    tier_id: u64,
    aliases: &[String],
) -> AliasOutcome {
    let master = TagMaster::new(cats, next_tier_id);
    let Some(old) = master.element_name(cat_idx, tier_id) else { return AliasOutcome::MissingTarget };
    let own = alias_keys(&old);
    // 要素がまだ持っていない別名だけを足す。
    let mut added: Vec<&String> = Vec::new();
    for a in aliases {
        let keys = alias_keys(a);
        if keys.is_empty() || keys.iter().all(|k| own.contains(k)) || added.iter().any(|x| alias_keys(x) == keys) {
            continue;
        }
        added.push(a);
    }
    if added.is_empty() {
        return AliasOutcome::AlreadyHas;
    }
    if added.iter().any(|a| master.find_elements(cat_idx, a).iter().any(|&id| id != tier_id)) {
        return AliasOutcome::Conflict;
    }
    let mut parts: Vec<String> = split_aliases(&old).into_iter().map(String::from).collect();
    parts.extend(added.into_iter().map(|a| a.trim().to_string()));
    let new = join_aliases(&parts);
    for t in cats[cat_idx].tiers.iter_mut().filter(|t| t.id == tier_id) {
        t.element = Some(new.clone());
    }
    AliasOutcome::Renamed(old, new)
}

/// 適用の結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ApplyReport {
    /// タグが増えたファイルの数。
    pub tagged: usize,
    /// 既に付いていて変化が無かったファイルの数。
    pub unchanged: usize,
    /// 書き込みに失敗したファイルの数。
    pub failed: usize,
    pub new_categories: usize,
    pub new_elements: usize,
    /// 別名を足して名前を書き換えた要素の数。
    pub renamed_elements: usize,
    /// 別名が他の要素と重なるため、名前を書き換えずにタグだけ付けた候補の数。
    pub alias_conflicts: usize,
    /// 作ったバックアップ世代の名前。
    pub backup: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApplyError {
    /// バックアップに失敗したので、何も変更せずに中止した。
    BackupFailed,
    /// 試用の記録を保存できなかったので、マスターもタグも変えずに中止した。
    RecordFailed,
}

/// 1ファイルのタグの変更（書き込み前に決める）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub dir: PathBuf,
    pub name: String,
    /// 書き込み前のタグ。
    pub existing: Vec<u64>,
    /// 合成後のタグ。
    pub merged: Vec<u64>,
}

impl FileChange {
    /// 適用で足すタグ（`existing` に無く `merged` にあるもの）。
    pub(crate) fn added(&self) -> Vec<u64> {
        self.merged.iter().copied().filter(|id| !self.existing.contains(id)).collect()
    }
}

/// ファイルごとの変更を決める（まだ書き込まない）。戻り値は（変更の一覧, パスが引けず失敗した件数）。
/// 既存のタグはDBから読み直して合成する（走査から時間が経っていても、後から付けたタグを消さない）。
pub(crate) fn plan_file_changes(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    file_additions: &HashMap<usize, Vec<u64>>,
    master: &[TagManagerCategoryUi],
) -> (Vec<FileChange>, usize) {
    let mut failed = 0;
    let mut changes = Vec::new();
    let mut files: Vec<(&usize, &Vec<u64>)> = file_additions.iter().collect();
    files.sort_by_key(|(i, _)| **i);
    for (&fi, additions) in files {
        let Some(path) = paths.get(fi) else {
            failed += 1;
            continue;
        };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            failed += 1;
            continue;
        };
        let existing = crate::spread_state::read_archive_tags(db, dir, name);
        let merged = merge_file_tags_in(master, &existing, additions);
        changes.push(FileChange { dir: dir.to_path_buf(), name: name.to_string(), existing, merged });
    }
    (changes, failed)
}

/// 決めた変更を書き込む。戻り値は（増えた, 変化なし, 失敗）。
pub(crate) fn write_file_changes(db: &Arc<Mutex<Database>>, changes: &[FileChange]) -> (usize, usize, usize) {
    let (mut tagged, mut unchanged, mut failed) = (0, 0, 0);
    for c in changes {
        if c.merged == c.existing {
            unchanged += 1;
        } else if crate::spread_state::write_archive_tags(db, &c.dir, &c.name, &c.merged) {
            tagged += 1;
        } else {
            failed += 1;
        }
    }
    (tagged, unchanged, failed)
}

/// ファイルごとにタグを足して書き込む。戻り値は（増えた, 変化なし, 失敗）。
/// 本番の適用は `plan_file_changes` と `write_file_changes` を分けて呼ぶ（間で記録を保存するため）。テスト用のまとめ版。
#[cfg(test)]
pub(crate) fn write_file_tags(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    file_additions: &HashMap<usize, Vec<u64>>,
    master: &[TagManagerCategoryUi],
) -> (usize, usize, usize) {
    let (changes, unresolved) = plan_file_changes(db, paths, file_additions, master);
    let (tagged, unchanged, failed) = write_file_changes(db, &changes);
    (tagged, unchanged, failed + unresolved)
}

/// 足したタグがあるファイルの記録（破棄で外す元）。
pub(crate) fn trial_record_of(changes: &[FileChange]) -> TrialRecord {
    TrialRecord {
        files: changes
            .iter()
            .filter_map(|c| {
                let added = c.added();
                (!added.is_empty()).then(|| TrialFile { dir: c.dir.clone(), name: c.name.clone(), added })
            })
            .collect(),
    }
}

/// 適用を実行する。`backup` はバックアップを取り、成功したら世代名を返す（失敗は None）。
/// `save_master` はマスターを保存する。`save_record` は、足すタグの記録を保存する（保存できたら true）。
/// 順序は バックアップ → 変更の決定 → 記録の保存 → マスター保存 → タグ書き込み。
/// 記録を保存できなかったら、マスターもタグも変えずに `RecordFailed` で中止する（試用の破棄で戻せなくなるため）。
pub(crate) fn run_apply(
    db: &Arc<Mutex<Database>>,
    paths: &[PathBuf],
    plan: &Plan,
    master: &[TagManagerCategoryUi],
    backup: &dyn Fn() -> Option<String>,
    save_master: &dyn Fn(),
    save_record: &dyn Fn(&TrialRecord) -> bool,
) -> Result<ApplyReport, ApplyError> {
    let backup_name = backup().ok_or(ApplyError::BackupFailed)?;
    let (changes, unresolved) = plan_file_changes(db, paths, &plan.file_additions, master);
    // 足すタグの記録は、書き込みの前に保存する。途中で落ちても、破棄で外せる。
    if !save_record(&trial_record_of(&changes)) {
        return Err(ApplyError::RecordFailed);
    }
    // タグのIDがマスターに存在してから紐付けるため、タグを書く前にマスターを保存する。
    save_master();
    let (tagged, unchanged, failed) = write_file_changes(db, &changes);
    let failed = failed + unresolved;
    Ok(ApplyReport {
        tagged,
        unchanged,
        failed,
        new_categories: plan.new_categories,
        new_elements: plan.new_elements,
        renamed_elements: plan.renames.len(),
        alias_conflicts: plan.alias_conflicts,
        backup: backup_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_candidates::{build_candidates, input_from_info_text};
    use crate::tag_master::SystemKind;
    use crate::tag_similarity::MatchOptions;
    use crate::tag_source::FormatTag;
    use crate::view_explorer::TagManagerTierUi;
    use std::cell::Cell;

    fn tier(id: u64, name: &str) -> TagManagerTierUi {
        TagManagerTierUi { id, tier_no: id as i32, negative: false, element: Some(name.to_string()) }
    }

    fn cat(name: &str, is_main: bool, kind: Option<SystemKind>, tiers: Vec<TagManagerTierUi>) -> TagManagerCategoryUi {
        TagManagerCategoryUi { name: name.to_string(), tiers, color: egui::Color32::GRAY, is_main, single_select: is_main, system_kind: kind }
    }

    fn master() -> Vec<TagManagerCategoryUi> {
        vec![
            cat("main", true, None, vec![tier(1, "漫画")]),
            cat("fmt", false, Some(SystemKind::ArchiveFormat), vec![tier(10, "zip"), tier(11, "cbz")]),
            cat("作者", false, None, vec![tier(20, "Kouji")]),
        ]
    }

    fn input(name: &str, fmt: &str, info: Option<&str>) -> crate::tag_candidates::CandidateInput {
        input_from_info_text(name, Some(FormatTag::Extension(fmt.to_string())), info)
    }

    #[test]
    fn plan_creates_missing_categories_once_and_dedupes_elements() {
        let inputs = [
            input("(C1) [Circ (Kouji)] a [日本語].zip", "zip", Some("Category: Manga\n")),
            input("(C1) [Circ (New Writer)] b [日本語].zip", "zip", None),
        ];
        let set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::Japanese);
        let mut cats = master();
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::Japanese);
        // 新規カテゴリ: 言語・イベント・グループ（作者は既存）。メインの Manga はメインカテゴリへ。
        assert_eq!(plan.new_categories, 3, "言語・イベント・グループ");
        let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"言語") && names.contains(&"イベント") && names.contains(&"グループ"));
        assert_eq!(cats.iter().filter(|c| c.name == "グループ").count(), 1, "同じ種類のカテゴリは1つだけ");
        // 新規要素: Manga・日本語・C1・Circ・New Writer。
        assert_eq!(plan.new_elements, 5);
        let main = cats.iter().find(|c| c.is_main).unwrap();
        assert!(main.tiers.iter().any(|t| t.element.as_deref() == Some("Manga")));
        // ファイルごと: 2ファイルとも イベント・言語・グループ・形式(既存zip)。0番だけ Manga と既存の作者 Kouji。
        let f0 = &plan.file_additions[&0];
        let f1 = &plan.file_additions[&1];
        assert!(f0.contains(&10) && f0.contains(&20), "既存の zip と Kouji: {f0:?}");
        assert!(f1.contains(&10));
        assert!(!f1.contains(&20));
        assert_eq!(f0.len(), 6, "形式・Manga・言語・イベント・グループ・作者");
        assert_eq!(f1.len(), 5, "形式・言語・イベント・グループ・作者(New Writer)");
        assert_eq!(next, 105, "新規要素の数だけ採番");
    }

    #[test]
    fn alias_choice_renames_the_element_and_tags_with_it() {
        let inputs = [input("[Circ (Koji)] a.zip", "zip", None), input("[Circ (Koji)] b.zip", "zip", None)];
        let mut set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        let c = set.candidates.iter_mut().find(|c| c.role == Role::Author).unwrap();
        c.choice = Choice::Alias { cat_idx: 2, tier_id: 20 };
        let mut cats = master();
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        assert_eq!(plan.renames, vec![(2, 20, "Kouji".to_string(), "Kouji|Koji".to_string())]);
        assert_eq!(cats[2].tiers[0].element.as_deref(), Some("Kouji|Koji"));
        assert!(plan.file_additions[&0].contains(&20) && plan.file_additions[&1].contains(&20));
        assert!(cats[2].tiers.len() == 1, "要素は増えない");
        // 次回は別名への完全一致で自動採用される。
        let again = build_candidates(&inputs, &cats, &MatchOptions::default(), Lang::English);
        let c = again.candidates.iter().find(|c| c.role == Role::Author).unwrap();
        assert_eq!(c.choice, Choice::Existing { cat_idx: 2, tier_id: 20 });
    }

    #[test]
    fn alias_choice_is_not_applied_when_it_overlaps_another_element_or_exists() {
        let inputs = [input("[Circ (Koji)] a.zip", "zip", None)];
        let mut set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        set.candidates.iter_mut().find(|c| c.role == Role::Author).unwrap().choice = Choice::Alias { cat_idx: 2, tier_id: 20 };
        // 別の要素 Koji が既にある → 書き換えず、タグだけ付ける。
        let mut cats = master();
        cats[2].tiers.push(tier(21, "Koji"));
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        assert!(plan.renames.is_empty());
        assert_eq!(plan.alias_conflicts, 1);
        assert_eq!(cats[2].tiers[0].element.as_deref(), Some("Kouji"));
        assert!(plan.file_additions[&0].contains(&20));
        // 既に同じ別名を持つ → 書き換えない（衝突でもない）。
        let mut cats = master();
        cats[2].tiers[0].element = Some("Kouji|koji".to_string());
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        assert!(plan.renames.is_empty() && plan.alias_conflicts == 0);
        assert_eq!(cats[2].tiers[0].element.as_deref(), Some("Kouji|koji"));
        // 対象の要素が無い → 飛ばす。
        let mut cats = master();
        cats[2].tiers.clear();
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        assert!(plan.renames.is_empty());
        assert!(!plan.file_additions.get(&0).is_some_and(|v| v.contains(&20)));
    }

    #[test]
    fn merged_candidates_register_one_aliased_element_or_extend_the_existing_one() {
        let inputs = [input("[Circ (Kouji)] a.zip", "zip", None), input("[Circ2 (Koji)] b.zip", "zip", None)];
        let mut set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        let pos = |set: &CandidateSet, r: Role, n: &str| set.candidates.iter().position(|c| c.role == r && c.name == n).unwrap();
        let (a, b) = (pos(&set, Role::Author, "Kouji"), pos(&set, Role::Author, "Koji"));
        set.merge_candidates(&[a, b], None, 1, &master()).unwrap();
        let (g1, g2) = (pos(&set, Role::Group, "Circ"), pos(&set, Role::Group, "Circ2"));
        set.merge_candidates(&[g1, g2], None, 2, &master()).unwrap();
        let mut cats = master();
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        assert_eq!(cats[2].tiers[0].element.as_deref(), Some("Kouji|Koji"), "既存の作者に別名を足す");
        assert_eq!(plan.renames.len(), 1);
        let groups = cats.iter().find(|c| c.name == "Group").expect("グループカテゴリを新規作成");
        assert_eq!(groups.tiers.len(), 1);
        assert_eq!(groups.tiers[0].element.as_deref(), Some("Circ|Circ2"));
        assert_eq!(plan.new_elements, 1 + 0 + 0 + set.candidates.iter().filter(|c| c.role == Role::Event && c.needs_new_element()).count());
        // 両ファイルとも、まとめた作者(20)とグループにタグが付く。
        let group_id = groups.tiers[0].id;
        assert!(plan.file_additions[&0].contains(&20) && plan.file_additions[&1].contains(&20));
        assert!(plan.file_additions[&0].contains(&group_id) && plan.file_additions[&1].contains(&group_id));
    }

    #[test]
    fn done_toast_mentions_renamed_elements_only_when_present() {
        let mut r = ApplyReport { tagged: 3, backup: "g1".into(), ..ApplyReport::default() };
        assert!(!Lang::Japanese.tag_apply_done(&r).contains("別名"));
        r.renamed_elements = 2;
        r.alias_conflicts = 1;
        for l in [Lang::Japanese, Lang::English, Lang::Chinese] {
            let msg = l.tag_apply_done(&r);
            assert!(msg.contains('2') && msg.contains('1') && msg.contains("g1"), "{msg}");
        }
        assert!(Lang::Japanese.tag_apply_done(&r).contains("別名を追加した既存要素2個"));
    }

    #[test]
    fn record_holds_only_added_ids_and_a_failed_record_stops_everything() {
        let (db, root, paths) = seed_db("rec", &[("a.zip", &[1]), ("b.zip", &[10, 20])]);
        let mut adds: HashMap<usize, Vec<u64>> = HashMap::new();
        adds.insert(0, vec![10, 20]);
        adds.insert(1, vec![10, 20]); // b は既に両方持っている
        let (changes, unresolved) = plan_file_changes(&db, &paths, &adds, &master());
        assert_eq!(unresolved, 0);
        let rec = trial_record_of(&changes);
        assert_eq!(rec.files.len(), 1, "変化の無い b は記録しない");
        assert_eq!((rec.files[0].name.as_str(), rec.files[0].added.clone()), ("a.zip", vec![10, 20]));
        assert_eq!(changes[0].existing, vec![1], "メインが設定済みなので、既存は残す");
        // 記録を保存できなければ、マスターもタグも変えずに中止する。
        let plan = Plan { file_additions: adds, ..Plan::default() };
        let saved = Cell::new(false);
        let r = run_apply(&db, &paths, &plan, &master(), &|| Some("g".into()), &|| saved.set(true), &|_| false);
        assert_eq!(r, Err(ApplyError::RecordFailed));
        assert!(!saved.get());
        assert_eq!(crate::spread_state::read_archive_tags(&db, &root, "a.zip"), vec![1]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discard_removes_only_the_added_ids_and_keeps_later_manual_edits() {
        use crate::tag_trial::{remove_added_tags, DiscardReport};
        let (db, root, paths) = seed_db("disc", &[("a.zip", &[1]), ("b.zip", &[]), ("c.zip", &[5]), ("d.zip", &[])]);
        let mut adds: HashMap<usize, Vec<u64>> = HashMap::new();
        for i in 0..4 {
            adds.insert(i, vec![10, 20]);
        }
        let (changes, _) = plan_file_changes(&db, &paths, &adds, &master());
        let rec = trial_record_of(&changes);
        let (tagged, _, _) = write_file_changes(&db, &changes);
        assert_eq!(tagged, 4);
        // 適用のあとに手で変えた分: b に 7 を足す、c の 10 を手で外す。d は消す（移動したことにする）。
        let read = |n: &str| crate::spread_state::read_archive_tags(&db, &root, n);
        let mut b = read("b.zip");
        b.push(7);
        assert!(crate::spread_state::write_archive_tags(&db, &root, "b.zip", &b));
        let c: Vec<u64> = read("c.zip").into_iter().filter(|&x| x != 10).collect();
        assert!(crate::spread_state::write_archive_tags(&db, &root, "c.zip", &c));
        std::fs::remove_file(root.join("d.zip")).unwrap();
        assert!(crate::spread_state::write_archive_tags(&db, &root, "d.zip", &[]));
        let r = remove_added_tags(&db, &rec);
        assert_eq!(read("a.zip"), vec![1], "適用前の状態に戻る");
        assert_eq!(read("b.zip"), vec![7], "手で足したタグは残る");
        assert_eq!(read("c.zip"), vec![5], "c は 20 だけ外れる。手で外した 10 は戻さない");
        assert_eq!(r, DiscardReport { restored: 3, unchanged: 0, missing: 1, failed: 0 });
        // もう一度外しても変化なし。
        let r = remove_added_tags(&db, &rec);
        assert_eq!((r.restored, r.unchanged, r.missing), (0, 3, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_skips_rejected_candidates_and_missing_existing_targets() {
        let inputs = [input("[Circ (Wr)] a.zip", "zip", None)];
        let mut set = build_candidates(&inputs, &master(), &MatchOptions::default(), Lang::English);
        for c in &mut set.candidates {
            if c.name == "Wr" {
                c.unadopt();
            }
            if c.name == "zip" {
                // マスターから消えた要素を指している。
                c.choice = Choice::Existing { cat_idx: 1, tier_id: 999 };
            }
        }
        let mut cats = master();
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::English);
        let f = &plan.file_additions[&0];
        assert_eq!(f.len(), 1, "グループ Circ だけ: {f:?}");
        assert_eq!(plan.new_elements, 1);
        assert!(cats.iter().all(|c| c.tiers.iter().all(|t| t.element.as_deref() != Some("Wr"))), "不採用はマスターにも作らない");
    }

    fn seed_db(tag: &str, files: &[(&str, &[u64])]) -> (Arc<Mutex<Database>>, std::path::PathBuf, Vec<PathBuf>) {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("nekoviewer_tag_apply_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&root).unwrap();
        let db = crate::spread_state::open_spread_db(&root).unwrap();
        let mut paths = Vec::new();
        for (i, (name, tags)) in files.iter().enumerate() {
            let p = root.join(name);
            std::fs::write(&p, (0..3000u32).map(|n| ((n as usize + i * 13) % 251) as u8).collect::<Vec<u8>>()).unwrap();
            crate::file_identity::ensure_record(&db, &root, name).unwrap();
            if !tags.is_empty() {
                assert!(crate::spread_state::write_archive_tags(&db, &root, name, tags));
            }
            paths.push(p);
        }
        (db, root, paths)
    }

    #[test]
    fn write_file_tags_adds_without_removing_and_respects_exclusive_categories() {
        // 0: タグなし / 1: 既存のメイン(1)と作者(20) / 2: 追加が無い
        let (db, root, paths) = seed_db("write", &[("a.zip", &[]), ("b.zip", &[1, 20]), ("c.zip", &[])]);
        let mut adds: HashMap<usize, Vec<u64>> = HashMap::new();
        adds.insert(0, vec![1, 10, 20]);
        adds.insert(1, vec![1, 10, 11, 21]);
        let mut cats = master();
        cats.push(cat("グループ", false, None, vec![tier(21, "Circ")]));
        let (tagged, unchanged, failed) = write_file_tags(&db, &paths, &adds, &cats);
        assert_eq!((tagged, unchanged, failed), (2, 0, 0));
        let a = crate::spread_state::read_archive_tags(&db, &root, "a.zip");
        assert_eq!(a, vec![1, 10, 20]);
        let b = crate::spread_state::read_archive_tags(&db, &root, "b.zip");
        assert_eq!(b, vec![1, 20, 10, 21], "既存を残し、メインは既にあるので足さず、形式は1つだけ");
        // 同じ内容で再実行すると、変化なし。
        let (tagged, unchanged, _) = write_file_tags(&db, &paths, &adds, &cats);
        assert_eq!((tagged, unchanged), (0, 2));
        assert!(crate::spread_state::read_archive_tags(&db, &root, "c.zip").is_empty());
        // 範囲外のファイル番号は失敗として数える。
        adds.insert(99, vec![1]);
        let (_, _, failed) = write_file_tags(&db, &paths, &adds, &cats);
        assert_eq!(failed, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn run_apply_aborts_without_changes_when_the_backup_fails_and_orders_steps() {
        let (db, root, paths) = seed_db("run", &[("a.zip", &[])]);
        let mut adds: HashMap<usize, Vec<u64>> = HashMap::new();
        adds.insert(0, vec![10]);
        let plan = Plan { file_additions: adds, new_categories: 1, new_elements: 2, ..Plan::default() };
        let saved = Cell::new(false);
        let r = run_apply(&db, &paths, &plan, &master(), &|| None, &|| saved.set(true), &|_| true);
        assert_eq!(r, Err(ApplyError::BackupFailed));
        assert!(!saved.get(), "バックアップに失敗したらマスターも保存しない");
        assert!(crate::spread_state::read_archive_tags(&db, &root, "a.zip").is_empty(), "タグも書かない");

        let order = std::cell::RefCell::new(Vec::new());
        let r = run_apply(
            &db,
            &paths,
            &plan,
            &master(),
            &|| {
                order.borrow_mut().push("backup");
                Some("gen-1".to_string())
            },
            &|| order.borrow_mut().push("save"),
            &|r| {
                order.borrow_mut().push("record");
                assert_eq!(r.files.len(), 1);
                assert_eq!(r.files[0].added, vec![10]);
                true
            },
        )
        .unwrap();
        assert_eq!(*order.borrow(), vec!["backup", "record", "save"]);
        assert_eq!((r.tagged, r.unchanged, r.failed, r.new_categories, r.new_elements), (1, 0, 0, 1, 2));
        assert_eq!(r.backup, "gen-1");
        assert_eq!(crate::spread_state::read_archive_tags(&db, &root, "a.zip"), vec![10]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paired_two_line_names_are_registered_as_one_element_with_aliases() {
        let info = "[Ebizoridan (Shien)] A [Digital]\n[えびぞり団 (しえん)] あ [DL版]\nhttps://x\n\nCategory: Doujinshi\n";
        let inputs = [input_from_info_text("a.zip", Some(FormatTag::Extension("zip".into())), Some(info))];
        let set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::Japanese);
        let (mut cats, mut next) = (Vec::new(), 100u64);
        let plan = plan_application(&set, &mut cats, &mut next, Lang::Japanese);
        let names: Vec<String> = cats.iter().flat_map(|c| c.tiers.iter().filter_map(|t| t.element.clone())).collect();
        assert!(names.contains(&"えびぞり団|Ebizoridan".to_string()), "{names:?}");
        assert!(names.contains(&"しえん|Shien".to_string()), "{names:?}");
        assert!(plan.new_elements >= 2);
    }

    #[test]
    fn alias_candidate_adds_the_missing_form_to_the_existing_element() {
        let info = "[Ebizoridan (Shien)] A\n[えびぞり団 (しえん)] あ\n\nCategory: Doujinshi\n";
        let inputs = [input_from_info_text("a.zip", None, Some(info))];
        let mut cats = vec![crate::view_explorer::TagManagerCategoryUi {
            name: "グループ".into(),
            tiers: vec![tier(30, "Ebizoridan")],
            color: egui::Color32::GRAY,
            is_main: false,
            single_select: false,
            system_kind: None,
        }];
        let set = build_candidates(&inputs, &cats, &MatchOptions::default(), Lang::Japanese);
        let mut next = 100u64;
        let plan = plan_application(&set, &mut cats, &mut next, Lang::Japanese);
        assert_eq!(cats[0].tiers[0].element.as_deref(), Some("Ebizoridan|えびぞり団"), "別名が足され、要素名が書き換わる");
        assert_eq!(plan.renames.len(), 1, "書き換わる既存要素の一覧に出る");
    }

    #[test]
    fn new_roles_create_their_own_categories_and_are_applied_in_role_order() {
        use crate::tag_rules::{CondTarget, Condition, NameShape, Preset};
        let mut rule = Preset::Cosplay.to_rule(1);
        rule.conditions = vec![Condition::Shape { targets: vec![CondTarget::FileName], shape: NameShape::Dash2Paren }];
        let inputs = [input_from_info_text("Tiny Asa アサ - Castorice (Honkai-Star Rail).zip", Some(FormatTag::Extension("zip".into())), None)];
        let set = crate::tag_candidates::build_candidates_full(&inputs, &[], &MatchOptions::default(), Lang::Japanese, None, &[rule]);
        let (mut cats, mut next) = (Vec::new(), 100u64);
        let plan = plan_application(&set, &mut cats, &mut next, Lang::Japanese);
        let find = |cat: &str| cats.iter().find(|c| c.name == cat).map(|c| c.tiers.iter().filter_map(|t| t.element.clone()).collect::<Vec<_>>());
        assert_eq!(find("演者"), Some(vec!["Tiny Asa アサ".to_string()]));
        assert_eq!(find("キャラクター"), Some(vec!["Castorice".to_string()]));
        assert_eq!(find("パロディ元"), Some(vec!["Honkai-Star Rail".to_string()]));
        assert_eq!(plan.new_categories, 4, "形式（zip）＋演者・キャラクター・パロディ元");
        assert_eq!(plan.file_additions.get(&0).map(|v| v.len()), Some(4));
    }

    #[test]
    fn attributes_and_year_month_create_their_own_categories_and_elements() {
        let xml = "<ComicInfo><Writer>kirome</Writer><Tags>elf, bald</Tags><Year>2023</Year><Month>12</Month></ComicInfo>";
        let meta = crate::tag_meta::ArchiveMeta::from_sources(crate::fs::archive::meta::parse_comic_info(xml.as_bytes()).as_ref(), None);
        let inputs = [crate::tag_candidates::CandidateInput { file_name: "[Kirome] t.zip".into(), format: None, info: None, meta }];
        let set = build_candidates(&inputs, &[], &MatchOptions::default(), Lang::Japanese);
        let (mut cats, mut next) = (Vec::new(), 100u64);
        let plan = plan_application(&set, &mut cats, &mut next, Lang::Japanese);
        let find = |cat: &str| cats.iter().find(|c| c.name == cat).map(|c| c.tiers.iter().filter_map(|t| t.element.clone()).collect::<Vec<_>>());
        assert_eq!(find("属性").map(|mut v| { v.sort(); v }), Some(vec!["bald".to_string(), "elf".to_string()]));
        assert_eq!(find("年月"), Some(vec!["2023-12".to_string()]));
        assert_eq!(find("作者"), Some(vec!["Kirome".to_string()]));
        assert_eq!(plan.file_additions.get(&0).map(|v| v.len()), Some(4), "作者1＋属性2＋年月1");
    }
}
