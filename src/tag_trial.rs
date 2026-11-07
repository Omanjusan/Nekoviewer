//! 一括タグ付けの「試用中」の記録。適用で各ファイルに足したタグIDを残し、破棄のときに外す元にする。
//!
//! - 保存先は `nekoviewer_tags.trial.added.json`（仮マスター `nekoviewer_tags.trial.json` と同じ設定フォルダ）。
//!   アプリを閉じても残り、次回起動の試用中に引き継がれる
//! - 記録するのは「適用で足したID」だけ（適用前に既にあったタグは含めない）。破棄は、この分だけを各ファイルから外し、
//!   適用のあとに手で付けた・外したタグには触らない
//! - 記録は、ファイルのタグを書く**前**に保存する。途中で落ちても、足したかもしれないIDを破棄で外せる

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 1ファイルに足したID。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrialFile {
    /// ファイルのあるフォルダ。
    pub dir: PathBuf,
    /// ファイル名（フォルダ本はフォルダ名）。
    pub name: String,
    /// 適用で足したタグID。
    pub added: Vec<u64>,
}

/// 試用中の記録。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrialRecord {
    pub files: Vec<TrialFile>,
}

fn record_path(root: &Path) -> PathBuf {
    root.join("nekoviewer_tags.trial.added.json")
}

impl TrialRecord {
    /// 記録を保存する（tmpに書いてからrename）。保存できたら true。
    pub(crate) fn save(&self, root: &Path) -> bool {
        let Ok(content) = serde_json::to_string(self) else { return false };
        let _ = std::fs::create_dir_all(root);
        let path = record_path(root);
        let tmp = root.join("nekoviewer_tags.trial.added.json.tmp");
        if std::fs::write(&tmp, content).is_err() {
            return false;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        true
    }

    /// 記録を読む。無い・壊れているときは None。
    pub(crate) fn load(root: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(record_path(root)).ok()?).ok()
    }

    /// 記録のファイルを消す（確定・破棄のあと）。
    pub(crate) fn remove(root: &Path) {
        let _ = std::fs::remove_file(record_path(root));
    }

    /// 記録に残っているID の総数（件数の報告用）。
    pub(crate) fn added_total(&self) -> usize {
        self.files.iter().map(|f| f.added.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nekoviewer_trial_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn record_round_trips_and_can_be_removed() {
        let root = temp_root("round");
        assert_eq!(TrialRecord::load(&root), None);
        let rec = TrialRecord {
            files: vec![
                TrialFile { dir: PathBuf::from("/a/b"), name: "x.zip".into(), added: vec![3, 4] },
                TrialFile { dir: PathBuf::from("/a/c"), name: "日本語.cbz".into(), added: vec![9] },
            ],
        };
        assert!(rec.save(&root));
        assert_eq!(TrialRecord::load(&root), Some(rec.clone()));
        assert_eq!(rec.added_total(), 3);
        // 上書き保存。
        let rec2 = TrialRecord { files: vec![rec.files[1].clone()] };
        assert!(rec2.save(&root));
        assert_eq!(TrialRecord::load(&root), Some(rec2));
        TrialRecord::remove(&root);
        assert_eq!(TrialRecord::load(&root), None);
        // 壊れたファイルは None。
        std::fs::write(record_path(&root), "{ broken").unwrap();
        assert_eq!(TrialRecord::load(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
