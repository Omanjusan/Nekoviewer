//! タグマネージャー（カテゴリ/tier/色のマスタ定義）のJSON永続化。
//!
//! 保存先は2つ（`Slot`）。本物の `nekoviewer_tags.json` と、一括タグ付けの適用後に使う試用中の仮マスター
//! `nekoviewer_tags.trial.json`。試用中は仮マスターだけを読み書きし、本物は確定（`promote_trial`）まで変えない。
//! 破棄（`discard_trial`）は仮マスターを捨てて本物に戻す（`next_tier_id` は大きい方を引き継ぐ）。
//!
//! 対象は「タグ管理」画面で編集するマスタ定義のみで、各ファイル/アーカイブへの
//! 実際のタグ付け（紐付け）はスコープ外（別途検討）。保存は操作確定ごとの即時保存
//! （呼び出し側が編集確定のたびに`save`を呼ぶ）で、専用の待避タイミングは持たない。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::view_explorer::{TagManagerCategoryUi, TagManagerTierUi};


#[derive(Serialize, Deserialize)]
struct TierSaveData {
    id: u64,
    tier_no: i32,
    negative: bool,
    element: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CategorySaveData {
    name: String,
    is_main: bool,
    /// 単一選択（排他）カテゴリかどうか。旧JSON（このフィールド追加前の保存data）には
    /// 存在しないため、`#[serde(default)]`でfalse（複数選択）として読み込む。
    #[serde(default)]
    single_select: bool,
    /// システムカテゴリの種別キー（`tag_master::SystemKind::as_key`）。旧JSON・通常カテゴリは未設定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_kind: Option<String>,
    color: (u8, u8, u8),
    tiers: Vec<TierSaveData>,
}

#[derive(Serialize, Deserialize)]
struct SaveDataV1 {
    next_tier_id: u64,
    categories: Vec<CategorySaveData>,
}

/// マスターの保存先。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// 本物。
    Real,
    /// 試用中の仮マスター。
    Trial,
}

fn file_name(slot: Slot) -> &'static str {
    match slot {
        Slot::Real => "nekoviewer_tags.json",
        Slot::Trial => "nekoviewer_tags.trial.json",
    }
}

fn data_path(root: &Path, slot: Slot) -> PathBuf {
    root.join(file_name(slot))
}

fn data_tmp_path(root: &Path, slot: Slot) -> PathBuf {
    root.join(format!("{}.tmp", file_name(slot)))
}

/// 試用中の仮マスターのファイルがあるか（アプリを閉じても残るので、起動時に試用中かを判定する）。
pub(crate) fn trial_exists(root: &Path) -> bool {
    data_path(root, Slot::Trial).is_file()
}

/// 仮マスターを本物に昇格する（アトミックな置き換え）。仮マスターが無い・置き換えに失敗したら false。
pub(crate) fn promote_trial(root: &Path) -> bool {
    let (from, to) = (data_path(root, Slot::Trial), data_path(root, Slot::Real));
    from.is_file() && std::fs::rename(&from, &to).is_ok()
}

/// 仮マスターを捨てて、本物を読み直して返す。`next_tier_id` は、本物と仮マスターの大きい方を引き継ぎ、本物にも保存する
/// （仮マスターが新規要素に振った番号が、DBに残った孤立IDと再び衝突しないようにするため）。
/// 仮マスターが壊れていて読めないときは、そのまま捨てる。本物も無ければ初期状態。
pub(crate) fn discard_trial(root: &Path) -> (Vec<TagManagerCategoryUi>, u64) {
    let trial_next = load_slot(root, Slot::Trial).map(|(_, n)| n);
    let (cats, real_next) = load_slot(root, Slot::Real).unwrap_or_else(default_state);
    let next = real_next.max(trial_next.unwrap_or(0));
    if next != real_next {
        save_slot(root, Slot::Real, &cats, next);
    }
    let _ = std::fs::remove_file(data_path(root, Slot::Trial));
    let _ = std::fs::remove_file(data_tmp_path(root, Slot::Trial));
    (cats, next)
}

/// 保存ファイルが無い（初回起動）場合の初期状態: メインカテゴリ1つ＋補填用要素1つのみ。
pub(crate) fn default_state() -> (Vec<TagManagerCategoryUi>, u64) {
    let categories = vec![TagManagerCategoryUi {
        // 保存名は表示に使わない（`TagManagerCategoryUi::display_name`が言語別へ差し替える）。
        name: crate::i18n::t().tag_main_category_name().to_string(),
        tiers: vec![TagManagerTierUi {
            id: 1,
            tier_no: 1,
            negative: false,
            element: Some(crate::i18n::t().tag_main_default_element_name().to_string()),
        }],
        color: crate::view_explorer::pick_distinct_tag_color(&[]),
        is_main: true,
        single_select: true,
        system_kind: None,
    }];
    (categories, 2)
}

/// メインカテゴリの要素が1つも無い状態を許さないガード。削除操作の直後・保存直前に呼ぶ。
/// メインカテゴリが存在しない場合は何もしない（呼び出し側の状態が壊れているとみなす）。
/// 補填要素を追加した場合はtrueを返す（呼び出し側がトーストで知らせる）。
pub(crate) fn ensure_main_category_nonempty(categories: &mut [TagManagerCategoryUi], next_tier_id: &mut u64) -> bool {
    let Some(main) = categories.iter_mut().find(|c| c.is_main) else { return false };
    if main.tiers.iter().all(|t| t.element.is_none()) {
        let id = *next_tier_id;
        *next_tier_id += 1;
        main.tiers.push(TagManagerTierUi {
            id,
            tier_no: main.tiers.len() as i32 + 1,
            negative: false,
            element: Some(crate::i18n::t().tag_main_default_element_name().to_string()),
        });
        return true;
    }
    false
}

/// root配下の本物の保存ファイルを読み込む。無い・壊れている場合はNone（呼び出し側で`default_state`を使う）。
pub(crate) fn load(root: &Path) -> Option<(Vec<TagManagerCategoryUi>, u64)> {
    load_slot(root, Slot::Real)
}

/// 保存先を指定して読み込む。無い・壊れている場合はNone。
pub(crate) fn load_slot(root: &Path, slot: Slot) -> Option<(Vec<TagManagerCategoryUi>, u64)> {
    let content = std::fs::read_to_string(data_path(root, slot)).ok()?;
    let data: SaveDataV1 = serde_json::from_str(&content).ok()?;
    let categories = data
        .categories
        .into_iter()
        .map(|c| TagManagerCategoryUi {
            name: c.name,
            is_main: c.is_main,
            // メインカテゴリは常に単一選択固定（保存データが壊れていても矯正する）。
            single_select: c.is_main || c.single_select,
            // 未知の種別キー（将来版のJSON）は通常カテゴリとして読む。
            system_kind: c.system_kind.as_deref().and_then(crate::tag_master::SystemKind::from_key),
            color: egui::Color32::from_rgb(c.color.0, c.color.1, c.color.2),
            tiers: c
                .tiers
                .into_iter()
                .map(|t| TagManagerTierUi {
                    id: t.id,
                    tier_no: t.tier_no,
                    negative: t.negative,
                    element: t.element,
                })
                .collect(),
        })
        .collect();
    Some((categories, data.next_tier_id))
}

/// メインタグドラムの「未選択」を表す仮想tier_id。実tierの採番は1から始まる
/// （`default_state`/`load`参照）ため0とは衝突しない。ファイルに保存済みのメインタグが
/// 無い場合、ドラムはこの仮想エントリの位置から始まる。DBへはこの値を書き込まない
/// （`commit_tag_main_edit`側で除外する）。
pub(crate) const TAG_MAIN_UNSET_ID: u64 = 0;

/// カテゴリ一覧から、メインタグドラム用の選択肢（先頭に「未設定」の仮想エントリ、
/// 続いてメインカテゴリの要素をtier_id＋要素名でtier順）と、属性タグの妥当性チェック用
/// tier_id一覧（それ以外の全カテゴリの要素をフラットに集約）を導出する。タグ管理
/// ページでの編集が確定するたびにこれで再計算し、タグ付けUIに反映する。選択・紐付けの
/// 同一性判定はtier_id（リネームで変わらない不変ID）で行い、要素名は表示専用。
pub(crate) fn derive_tag_options(categories: &[TagManagerCategoryUi]) -> (Vec<(u64, String)>, Vec<u64>) {
    let mut main_options = vec![(TAG_MAIN_UNSET_ID, crate::i18n::t().tag_main_unset_label().to_string())];
    main_options.extend(
        categories
            .iter()
            .find(|c| c.is_main)
            .map(|c| {
                // 別名つきの要素は、ドラム等の表示には先頭の別名だけを使う。
                c.tiers
                    .iter()
                    .filter_map(|t| t.element.as_deref().map(|name| (t.id, crate::tag_master::display_name(name).to_string())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    let attr_options = categories
        .iter()
        .filter(|c| !c.is_main)
        .flat_map(|c| c.tiers.iter().filter(|t| t.element.is_some()).map(|t| t.id))
        .collect();
    (main_options, attr_options)
}

/// 単一選択カテゴリ（メインカテゴリを除く）の有効な選択が2個以上にならないよう分離する。
/// カテゴリ定義（要素追加・削除・単一/複数選択の切替）が変わるたび、およびファイルの
/// 保存済みタグ読み込み時に呼ぶ。**データは消さない**（非破壊）:
/// - 複数選択カテゴリの選択はそのまま維持する
/// - 単一選択カテゴリ内で選択済みが2個以上あれば、登録順（tier順）で最も若い1個だけを
///   `selected`に残し、他は戻り値（休眠値）として返す。呼び出し側は休眠値をDBへ保存する
///   ときに必ず書き戻し、複数選択へ戻したときに再び有効化できるようにする
/// - 選択が0個の場合は何もしない（未タグ付けは「未選択」のままが正しい状態）
pub(crate) fn enforce_single_select(categories: &[TagManagerCategoryUi], selected: &mut Vec<u64>) -> Vec<u64> {
    let mut dormant = Vec::new();
    for cat in categories.iter().filter(|c| !c.is_main && c.single_select) {
        let elements: Vec<u64> = cat.tiers.iter().filter(|t| t.element.is_some()).map(|t| t.id).collect();
        let Some(keep) = elements.iter().copied().find(|id| selected.contains(id)) else {
            continue;
        };
        selected.retain(|s| {
            if *s != keep && elements.contains(s) {
                dormant.push(*s);
                false
            } else {
                true
            }
        });
    }
    dormant
}

/// 複数選択カテゴリの一括入力欄向けパーサー。`,`を区切りとして要素名に分割する。
/// `,,`（連続カンマ）はリテラルの`,`1文字として要素名に含める（左から2個ずつ消費）。
/// 各要素は前後の空白（全角スペース含む）をtrimし、trim後に空文字になる要素は捨てる。
pub(crate) fn parse_bulk_elements(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            if chars.peek() == Some(&',') {
                chars.next();
                current.push(',');
            } else {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    result.push(trimmed.to_string());
                }
                current.clear();
            }
        } else {
            current.push(c);
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        result.push(trimmed.to_string());
    }
    result
}

/// 本物へのアトミック保存（tmpに書いてからrename）。gui_config::save_stateと同じ方式。
/// 本番は保存先を選ぶ `save_slot` を使う。これはテスト用の短縮形。
#[cfg(test)]
pub(crate) fn save(root: &Path, categories: &[TagManagerCategoryUi], next_tier_id: u64) {
    save_slot(root, Slot::Real, categories, next_tier_id);
}

/// 保存先を指定したアトミック保存。
pub(crate) fn save_slot(root: &Path, slot: Slot, categories: &[TagManagerCategoryUi], next_tier_id: u64) {
    let data = SaveDataV1 {
        next_tier_id,
        categories: categories
            .iter()
            .map(|c| {
                let [r, g, b, _a] = c.color.to_array();
                CategorySaveData {
                    name: c.name.clone(),
                    is_main: c.is_main,
                    single_select: c.single_select,
                    system_kind: c.system_kind.map(|k| k.as_key().to_string()),
                    color: (r, g, b),
                    tiers: c
                        .tiers
                        .iter()
                        .map(|t| TierSaveData {
                            id: t.id,
                            tier_no: t.tier_no,
                            negative: t.negative,
                            element: t.element.clone(),
                        })
                        .collect(),
                }
            })
            .collect(),
    };
    let Ok(content) = serde_json::to_string_pretty(&data) else { return };
    let _ = std::fs::create_dir_all(root);
    let path = data_path(root, slot);
    let tmp = data_tmp_path(root, slot);
    if std::fs::write(&tmp, &content).is_err() {
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(single: bool, ids: &[u64]) -> TagManagerCategoryUi {
        TagManagerCategoryUi {
            name: "c".into(),
            is_main: false,
            single_select: single,
            system_kind: None,
            color: egui::Color32::WHITE,
            tiers: ids
                .iter()
                .map(|&id| TagManagerTierUi { id, tier_no: id as i32, negative: false, element: Some(format!("e{id}")) })
                .collect(),
        }
    }

    #[test]
    fn main_options_show_only_the_first_alias() {
        let mut main = cat(true, &[1, 2]);
        main.is_main = true;
        main.tiers[0].element = Some("漫画|Manga|Comic".to_string());
        let (opts, _) = derive_tag_options(&[main]);
        assert_eq!(opts[1], (1, "漫画".to_string()));
        assert_eq!(opts[2], (2, "e2".to_string()));
    }

    #[test]
    fn trial_master_is_separate_until_promoted() {
        let root = temp_root("trial_promote");
        let real = vec![cat(false, &[1, 2])];
        save(&root, &real, 10);
        assert!(!trial_exists(&root));
        let mut trial = vec![cat(false, &[1, 2, 3])];
        trial[0].tiers[0].element = Some("Kouji|Koji".into());
        save_slot(&root, Slot::Trial, &trial, 11);
        assert!(trial_exists(&root));
        // 試用中も、本物は変わらない。
        let (r, rn) = load(&root).unwrap();
        assert_eq!((r[0].tiers.len(), rn), (2, 10));
        let (t, tn) = load_slot(&root, Slot::Trial).unwrap();
        assert_eq!((t[0].tiers.len(), tn, t[0].tiers[0].element.as_deref()), (3, 11, Some("Kouji|Koji")));
        // 昇格で本物が仮マスターの内容になり、仮マスターのファイルは無くなる。
        assert!(promote_trial(&root));
        assert!(!trial_exists(&root));
        let (r, rn) = load(&root).unwrap();
        assert_eq!((r[0].tiers.len(), rn), (3, 11));
        assert!(!promote_trial(&root), "仮マスターが無ければ false");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discard_returns_to_the_real_master_but_keeps_the_larger_next_id() {
        let root = temp_root("trial_discard");
        save(&root, &[cat(false, &[1, 2])], 10);
        save_slot(&root, Slot::Trial, &[cat(false, &[1, 2, 3, 4])], 15);
        let (cats, next) = discard_trial(&root);
        assert_eq!(cats[0].tiers.len(), 2, "本物の内容に戻る");
        assert_eq!(next, 15, "仮マスターが振った番号は巻き戻さない");
        assert!(!trial_exists(&root));
        // 本物のファイルにも、飛ばした番号が保存されている。
        let (r, rn) = load(&root).unwrap();
        assert_eq!((r[0].tiers.len(), rn), (2, 15));
        // 仮マスターのほうが小さい番号なら、本物の番号のまま。
        save_slot(&root, Slot::Trial, &[cat(false, &[1])], 5);
        assert_eq!(discard_trial(&root).1, 15);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discard_handles_a_broken_trial_file_and_a_missing_real_file() {
        let root = temp_root("trial_broken");
        save(&root, &[cat(false, &[1])], 7);
        std::fs::write(data_path(&root, Slot::Trial), "{ broken").unwrap();
        let (cats, next) = discard_trial(&root);
        assert_eq!((cats[0].tiers.len(), next), (1, 7));
        assert!(!trial_exists(&root));
        // 本物が無いときは初期状態。
        let root2 = temp_root("trial_no_real");
        save_slot(&root2, Slot::Trial, &[cat(false, &[1, 2, 3])], 40);
        let (cats, next) = discard_trial(&root2);
        assert!(cats.iter().any(|c| c.is_main), "初期状態（メインカテゴリ）");
        assert_eq!(next, 40);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nekoviewer_tagmgr_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn system_kind_round_trips_through_json_and_old_json_loads_as_normal() {
        let root = temp_root("sys");
        let mut c = cat(false, &[1, 2]);
        c.system_kind = Some(crate::tag_master::SystemKind::ArchiveFormat);
        let normal = cat(false, &[3]);
        save(&root, &[c, normal], 10);
        let (loaded, next) = load(&root).unwrap();
        assert_eq!(next, 10);
        assert_eq!(loaded[0].system_kind, Some(crate::tag_master::SystemKind::ArchiveFormat));
        assert_eq!(loaded[1].system_kind, None);
        // 通常カテゴリのJSONには system_kind のキーを出さない（旧版と同じ形のまま）。
        let text = std::fs::read_to_string(data_path(&root, Slot::Real)).unwrap();
        assert_eq!(text.matches("system_kind").count(), 1);

        // system_kind を持たない旧JSON・未知の種別キーは通常カテゴリとして読む。
        let old = r#"{"next_tier_id":3,"categories":[{"name":"x","is_main":false,"single_select":false,"color":[1,2,3],"tiers":[]},
            {"name":"y","is_main":false,"system_kind":"future_kind","color":[1,2,3],"tiers":[]}]}"#;
        std::fs::write(data_path(&root, Slot::Real), old).unwrap();
        let (loaded, _) = load(&root).unwrap();
        assert!(loaded.iter().all(|c| c.system_kind.is_none()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn single_keeps_earliest_registered_and_returns_rest_as_dormant() {
        let cats = [cat(true, &[1, 2, 3])];
        let mut sel = vec![3, 2];
        let dormant = enforce_single_select(&cats, &mut sel);
        assert_eq!(sel, vec![2]);
        assert_eq!(dormant, vec![3]);
    }

    #[test]
    fn multi_category_is_untouched() {
        let cats = [cat(false, &[1, 2, 3])];
        let mut sel = vec![3, 1];
        assert!(enforce_single_select(&cats, &mut sel).is_empty());
        assert_eq!(sel, vec![3, 1]);
    }

    #[test]
    fn other_categories_and_empty_selection_are_left_alone() {
        let cats = [cat(true, &[1, 2]), cat(false, &[10, 11])];
        let mut sel = vec![11, 2, 10];
        let dormant = enforce_single_select(&cats, &mut sel);
        assert_eq!(sel, vec![11, 2, 10]);
        assert!(dormant.is_empty());
        let mut none = Vec::new();
        assert!(enforce_single_select(&cats, &mut none).is_empty());
    }

    #[test]
    fn round_trip_single_then_multi_restores_all() {
        let mut cats = [cat(false, &[1, 2, 3])];
        let mut sel = vec![1, 3];
        cats[0].single_select = true;
        let dormant = enforce_single_select(&cats, &mut sel);
        assert_eq!(sel, vec![1]);
        cats[0].single_select = false;
        let mut all = sel.clone();
        all.extend(dormant);
        assert!(enforce_single_select(&cats, &mut all).is_empty());
        all.sort();
        assert_eq!(all, vec![1, 3]);
    }
}
