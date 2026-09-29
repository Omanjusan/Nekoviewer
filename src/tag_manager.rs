//! タグマネージャー（カテゴリ/tier/色のマスタ定義）のJSON永続化。
//!
//! 対象は「タグ管理」画面で編集するマスタ定義のみで、各ファイル/アーカイブへの
//! 実際のタグ付け（紐付け）はスコープ外（別途検討）。保存は操作確定ごとの即時保存
//! （呼び出し側が編集確定のたびに`save`を呼ぶ）で、専用の待避タイミングは持たない。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::view_explorer::{TagManagerCategoryUi, TagManagerTierUi};

/// メインカテゴリの要素が0個になった時に補充する仮要素名。
const DEFAULT_ELEMENT_NAME: &str = "デフォルト値";

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
    color: (u8, u8, u8),
    tiers: Vec<TierSaveData>,
}

#[derive(Serialize, Deserialize)]
struct SaveDataV1 {
    next_tier_id: u64,
    categories: Vec<CategorySaveData>,
}

fn data_path(root: &Path) -> PathBuf {
    root.join("nekoviewer_tags.json")
}

fn data_tmp_path(root: &Path) -> PathBuf {
    root.join("nekoviewer_tags.json.tmp")
}

/// 保存ファイルが無い（初回起動）場合の初期状態: メインカテゴリ1つ＋デフォルト値要素1つのみ。
pub(crate) fn default_state() -> (Vec<TagManagerCategoryUi>, u64) {
    let categories = vec![TagManagerCategoryUi {
        name: "メイン".to_string(),
        tiers: vec![TagManagerTierUi {
            id: 1,
            tier_no: 1,
            negative: false,
            element: Some(DEFAULT_ELEMENT_NAME.to_string()),
        }],
        color: crate::view_explorer::pick_distinct_tag_color(&[]),
        is_main: true,
        single_select: true,
    }];
    (categories, 2)
}

/// メインカテゴリの要素が1つも無い状態を許さないガード。削除操作の直後・保存直前に呼ぶ。
/// メインカテゴリが存在しない場合は何もしない（呼び出し側の状態が壊れているとみなす）。
pub(crate) fn ensure_main_category_nonempty(categories: &mut [TagManagerCategoryUi], next_tier_id: &mut u64) {
    let Some(main) = categories.iter_mut().find(|c| c.is_main) else { return };
    if main.tiers.iter().all(|t| t.element.is_none()) {
        let id = *next_tier_id;
        *next_tier_id += 1;
        main.tiers.push(TagManagerTierUi {
            id,
            tier_no: main.tiers.len() as i32 + 1,
            negative: false,
            element: Some(DEFAULT_ELEMENT_NAME.to_string()),
        });
    }
}

/// root配下の保存ファイルを読み込む。無い・壊れている場合はNone（呼び出し側で`default_state`を使う）。
pub(crate) fn load(root: &Path) -> Option<(Vec<TagManagerCategoryUi>, u64)> {
    let content = std::fs::read_to_string(data_path(root)).ok()?;
    let data: SaveDataV1 = serde_json::from_str(&content).ok()?;
    let categories = data
        .categories
        .into_iter()
        .map(|c| TagManagerCategoryUi {
            name: c.name,
            is_main: c.is_main,
            // メインカテゴリは常に単一選択固定（保存データが壊れていても矯正する）。
            single_select: c.is_main || c.single_select,
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

/// メインタグドラムの「未選択」を表す仮想エントリの表示名。
const TAG_MAIN_UNSET_LABEL: &str = "（未設定）";

/// カテゴリ一覧から、メインタグドラム用の選択肢（先頭に「未設定」の仮想エントリ、
/// 続いてメインカテゴリの要素をtier_id＋要素名でtier順）と、属性タグの妥当性チェック用
/// tier_id一覧（それ以外の全カテゴリの要素をフラットに集約）を導出する。タグ管理
/// ページでの編集が確定するたびにこれで再計算し、タグ付けUIに反映する。選択・紐付けの
/// 同一性判定はtier_id（リネームで変わらない不変ID）で行い、要素名は表示専用。
pub(crate) fn derive_tag_options(categories: &[TagManagerCategoryUi]) -> (Vec<(u64, String)>, Vec<u64>) {
    let mut main_options = vec![(TAG_MAIN_UNSET_ID, TAG_MAIN_UNSET_LABEL.to_string())];
    main_options.extend(
        categories
            .iter()
            .find(|c| c.is_main)
            .map(|c| c.tiers.iter().filter_map(|t| t.element.clone().map(|name| (t.id, name))).collect::<Vec<_>>())
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

/// アトミック保存（tmpに書いてからrename）。gui_config::save_stateと同じ方式。
pub(crate) fn save(root: &Path, categories: &[TagManagerCategoryUi], next_tier_id: u64) {
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
    let path = data_path(root);
    let tmp = data_tmp_path(root);
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
            color: egui::Color32::WHITE,
            tiers: ids
                .iter()
                .map(|&id| TagManagerTierUi { id, tier_no: id as i32, negative: false, element: Some(format!("e{id}")) })
                .collect(),
        }
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
