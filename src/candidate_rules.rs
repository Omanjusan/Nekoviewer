//! 一括タグ取得の確認窓で、ユーザーが決めた扱いを次回以降に引き継ぐ「候補ルール」の型と保存（画面・DBに触れない）。
//!
//! - 保存先は設定フォルダの `nekoviewer_candidate_rules.json`（tmp→rename）。`{"version":1,"rules":[…]}`。
//!   ルールは `kind` つきのリスト。知らない種別は捨てずに、そのまま書き戻す（前方互換。種別を足しても形式は変わらない）
//! - 現在の種別: `move_category`（カテゴリ振替。「この種類のこの名前は、このカテゴリの要素にする」）
//! - 照合は、種類と名前の完全一致（`tag_master::normalize_key` で全角半角・大文字小文字・空白記号を同一視）
//! - カテゴリは番号ではなく名前で持つ（マスターのロールバックやカテゴリの並べ替えで壊れないように）。
//!   名前のカテゴリが見つからないときは、使わずに知らせる（`CandidateSet::apply_category_rules`）
//! - ファイルが壊れている・新しい版のときは、上書きしない（保存を拒否する）

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tag_master::normalize_key;
use crate::tag_rules::RuleRole;

const FILE_NAME: &str = "nekoviewer_candidate_rules.json";
const VERSION: u32 = 1;
const KIND_MOVE: &str = "move_category";

fn path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

/// カテゴリ振替ルール。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MoveRule {
    /// 候補の種類（抽出された種類）。
    pub role: RuleRole,
    /// 候補の名前（表示用。照合は `key`）。
    pub name: String,
    /// 名前の照合キー（`normalize_key`）。
    pub key: String,
    /// 移し先のカテゴリの名前（表示名）。
    pub category: String,
}

/// 保存している候補ルールの全体。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CandidateRules {
    pub moves: Vec<MoveRule>,
    /// 知らない種別のルール（そのまま書き戻す）。
    unknown: Vec<Value>,
    /// ファイルが壊れている・新しい版で読めなかった。true の間は保存しない（元のファイルを守る）。
    read_only: bool,
}

/// 種類の日本語名（一覧表示用）。
pub(crate) fn rule_role_name(role: RuleRole) -> &'static str {
    match role {
        RuleRole::Format => "アーカイブ形式",
        RuleRole::Main => "メイン",
        RuleRole::Language => "言語",
        RuleRole::Event => "イベント",
        RuleRole::Author => "作者",
        RuleRole::Group => "グループ",
        RuleRole::Performer => "演者",
        RuleRole::Character => "キャラクター",
        RuleRole::Parody => "パロディ元",
        RuleRole::Attribute => "属性",
        RuleRole::YearMonth => "年月",
    }
}

impl CandidateRules {
    /// 読む。ファイルが無ければ空。壊れている・新しい版なら、空で、保存を拒否する状態にする。
    pub(crate) fn load(root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path(root)) else { return Self::default() };
        Self::parse(&text).unwrap_or(Self { read_only: true, ..Self::default() })
    }

    fn parse(text: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text).ok()?;
        if v.get("version")?.as_u64()? > u64::from(VERSION) {
            return None;
        }
        let mut out = Self::default();
        for r in v.get("rules")?.as_array()? {
            if r.get("kind").and_then(Value::as_str) == Some(KIND_MOVE)
                && let Ok(m) = serde_json::from_value::<MoveRule>(r.clone())
            {
                out.moves.push(m);
            } else {
                out.unknown.push(r.clone());
            }
        }
        Some(out)
    }

    /// 保存する（tmpに書いてからrename）。保存できたら true。読めなかったファイルは上書きしない。
    pub(crate) fn save(&self, root: &Path) -> bool {
        if self.read_only {
            return false;
        }
        let mut rules: Vec<Value> = Vec::new();
        for m in &self.moves {
            let Ok(mut v) = serde_json::to_value(m) else { return false };
            if let Some(o) = v.as_object_mut() {
                o.insert("kind".to_string(), Value::String(KIND_MOVE.to_string()));
            }
            rules.push(v);
        }
        rules.extend(self.unknown.iter().cloned());
        let doc = serde_json::json!({ "version": VERSION, "rules": rules });
        let Ok(content) = serde_json::to_string_pretty(&doc) else { return false };
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

    /// 保存を拒否している状態か（ファイルが読めなかった）。
    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// 種類と名前に合うカテゴリ振替ルール。
    pub(crate) fn find_move(&self, role: RuleRole, name: &str) -> Option<&MoveRule> {
        let key = normalize_key(name);
        self.moves.iter().find(|r| r.role == role && r.key == key)
    }

    /// カテゴリ振替ルールを足す。同じ種類・同じ名前のルールがあれば、移し先を置き換える。
    pub(crate) fn upsert_move(&mut self, role: RuleRole, name: &str, category: &str) {
        let key = normalize_key(name);
        match self.moves.iter_mut().find(|r| r.role == role && r.key == key) {
            Some(r) => {
                r.name = name.to_string();
                r.category = category.to_string();
            }
            None => self.moves.push(MoveRule { role, name: name.to_string(), key, category: category.to_string() }),
        }
    }

    /// 種類と名前に合うカテゴリ振替ルールを消す。消せたら true。
    pub(crate) fn remove_move(&mut self, role: RuleRole, name: &str) -> bool {
        let key = normalize_key(name);
        let before = self.moves.len();
        self.moves.retain(|r| !(r.role == role && r.key == key));
        self.moves.len() != before
    }

    /// 並びの番号のカテゴリ振替ルールを消す（管理画面の削除ボタン）。
    pub(crate) fn remove_move_at(&mut self, index: usize) -> bool {
        if index < self.moves.len() {
            self.moves.remove(index);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("nekoviewer_cand_rules_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn upsert_find_remove_use_normalized_names() {
        let mut r = CandidateRules::default();
        r.upsert_move(RuleRole::Event, "同人ＣＧ集", "ジャンル");
        // 全角半角・大文字小文字・空白記号の違いは同じ名前。
        assert_eq!(r.find_move(RuleRole::Event, "同人CG集").map(|m| m.category.as_str()), Some("ジャンル"));
        assert_eq!(r.find_move(RuleRole::Event, "同人 cg 集").map(|m| m.name.as_str()), Some("同人ＣＧ集"));
        assert!(r.find_move(RuleRole::Author, "同人CG集").is_none(), "種類が違えば別");
        // 同じ種類・名前は置き換え。
        r.upsert_move(RuleRole::Event, "同人CG集", "作品種別");
        assert_eq!(r.moves.len(), 1);
        assert_eq!((r.moves[0].name.as_str(), r.moves[0].category.as_str()), ("同人CG集", "作品種別"));
        assert!(r.remove_move(RuleRole::Event, "同人cg集"));
        assert!(!r.remove_move(RuleRole::Event, "同人cg集"));
        r.upsert_move(RuleRole::Event, "a", "x");
        assert!(r.remove_move_at(0));
        assert!(!r.remove_move_at(0));
    }

    #[test]
    fn save_and_load_round_trip_keeps_unknown_kinds() {
        let dir = tmp("rt");
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"version":1,"rules":[{"kind":"move_category","role":"event","name":"青年コミック","key":"青年コミック","category":"ジャンル"},{"kind":"future_kind","x":1}]}"#,
        )
        .unwrap();
        let mut r = CandidateRules::load(&dir);
        assert_eq!(r.moves.len(), 1);
        assert!(!r.is_read_only());
        r.upsert_move(RuleRole::Author, "Koji", "別名");
        assert!(r.save(&dir));
        assert!(!dir.join(format!("{FILE_NAME}.tmp")).exists());
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(text.contains("future_kind"), "知らない種別も書き戻す: {text}");
        let again = CandidateRules::load(&dir);
        assert_eq!(again.moves, r.moves);
        assert_eq!(again.moves.len(), 2);
    }

    #[test]
    fn missing_file_is_empty_and_broken_or_newer_file_is_protected() {
        let dir = tmp("mi");
        let empty = CandidateRules::load(&dir);
        assert!(empty.moves.is_empty() && !empty.is_read_only());
        assert!(empty.save(&dir), "ファイルが無ければ作れる");
        std::fs::write(dir.join(FILE_NAME), "not json").unwrap();
        let broken = CandidateRules::load(&dir);
        assert!(broken.is_read_only());
        assert!(!broken.save(&dir), "壊れたファイルは上書きしない");
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "not json");
        std::fs::write(dir.join(FILE_NAME), r#"{"version":99,"rules":[]}"#).unwrap();
        let newer = CandidateRules::load(&dir);
        assert!(newer.is_read_only() && !newer.save(&dir), "新しい版は上書きしない");
    }

    #[test]
    fn role_names_are_japanese() {
        assert_eq!(rule_role_name(RuleRole::Event), "イベント");
        assert_eq!(rule_role_name(RuleRole::Performer), "演者");
    }
}
