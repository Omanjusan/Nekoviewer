//! タグ自動取得の学習結果（`tag_learning` / `tag_model`）の保持と、タグパネルの状態表示。
//!
//! - 学習データは起動時に設定フォルダから読み、モデルを作る（学習データが空ならモデルは無し）
//! - 「学習結果を使う」が OFF のとき、または学習データが無いときは、候補生成にモデルを渡さない（現行ルールのみ）
//! - 学習窓・ラベル窓は P6-2 以降。ここは配線と、状態テキスト＋使う/使わないの切替だけ

use std::sync::Arc;



use crate::i18n;
use crate::tag_learning::LearnedSet;
use crate::tag_model::Model;
use crate::tag_similarity::MatchOptions;

use super::NekoviewApp;

/// 設定フォルダの学習データを読む。無い・壊れているときは空。
pub(super) fn load_learned(root: &std::path::Path) -> LearnedSet {
    LearnedSet::load(root).unwrap_or_default()
}

/// 学習データからモデルを作る（空なら None）。
pub(super) fn train_model(set: &LearnedSet, opts: &MatchOptions) -> Option<Arc<Model>> {
    Model::train(&set.training_data(opts)).map(Arc::new)
}

impl NekoviewApp {
    /// 候補生成に渡すモデル。学習結果を使わない設定のとき、学習データが無いときは None（現行ルールのみ）。
    pub(super) fn active_tag_model(&self) -> Option<Arc<Model>> {
        if self.config.tag_learning_use {
            self.tag_model.clone()
        } else {
            None
        }
    }

    /// 学習データが変わった（追加・削除）あとに、モデルを作り直す。
    #[allow(dead_code)] // 学習窓（P6-2）から呼ぶ
    pub(super) fn rebuild_tag_model(&mut self) {
        self.tag_model = train_model(&self.tag_learned, &self.config.tag_match);
    }

    /// タグパネルの状態テキスト: 学習済みの件数と、学習結果を使う/使わないの切替。
    pub(super) fn draw_tag_learning_status(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        let n = self.tag_learned.len();
        ui.vertical_centered(|ui| {
            ui.horizontal(|ui| {
                ui.weak(t.tag_learn_status(n)).on_hover_text(t.tag_learn_status_hover());
                // 使う/使わない。学習データが無いときは切り替えても結果が変わらないので、押せなくする。
                let on = self.config.tag_learning_use;
                let resp = ui.add_enabled(n > 0, egui::Button::selectable(on, t.tag_learn_use_toggle(on)).small());
                let resp = if n > 0 { resp.on_hover_text(t.tag_learn_use_hover()) } else { resp.on_disabled_hover_text(t.tag_learn_use_disabled_hover()) };
                if resp.clicked() {
                    self.config.tag_learning_use = !on;
                    self.persist_state();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_corpus::{CorpusEntry, Label};

    fn set_of(names: &[(&str, &[(Label, &str)])]) -> LearnedSet {
        LearnedSet {
            version: 1,
            entries: names
                .iter()
                .map(|(n, l)| CorpusEntry { file_name: n.to_string(), labels: l.iter().map(|(a, b)| (*a, b.to_string())).collect(), hard: false })
                .collect(),
        }
    }

    #[test]
    fn empty_learned_set_gives_no_model() {
        assert!(train_model(&LearnedSet::default(), &MatchOptions::default()).is_none());
    }

    #[test]
    fn learned_set_gives_a_model() {
        let s = set_of(&[("[A (B)] T.zip", &[(Label::Group, "A"), (Label::Author, "B")])]);
        assert!(train_model(&s, &MatchOptions::default()).is_some());
    }

    #[test]
    fn missing_or_broken_file_loads_as_empty() {
        let d = std::env::temp_dir().join(format!("nekoviewer_learn_ui_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(load_learned(&d).is_empty());
        std::fs::write(d.join("nekoviewer_tag_learning.json"), "{broken").unwrap();
        assert!(load_learned(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
