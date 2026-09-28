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

/// カテゴリ一覧から、メインタグドラム用の選択肢（メインカテゴリの要素、tier順）と、
/// 属性タグパレット用の選択肢（それ以外の全カテゴリの要素をフラットに集約）を導出する。
/// タグ管理ページでの編集が確定するたびにこれで再計算し、タグ付けUIに反映する。
pub(crate) fn derive_tag_options(categories: &[TagManagerCategoryUi]) -> (Vec<String>, Vec<String>) {
    let main_options = categories
        .iter()
        .find(|c| c.is_main)
        .map(|c| c.tiers.iter().filter_map(|t| t.element.clone()).collect())
        .unwrap_or_default();
    let attr_options = categories
        .iter()
        .filter(|c| !c.is_main)
        .flat_map(|c| c.tiers.iter().filter_map(|t| t.element.clone()))
        .collect();
    (main_options, attr_options)
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
