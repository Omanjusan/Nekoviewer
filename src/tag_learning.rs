//! タグ自動取得の「学習データ」の保存と、学習用の例（特徴量＋正解ラベル）への変換。
//!
//! - 保存するのは「ファイル名＋正解ラベル」（`CorpusEntry`）。特徴量やモデルは保存しない（P5で毎回作り直す）ので、
//!   特徴量の設計を変えても保存済みの学習データはそのまま使える
//! - 保存先は設定フォルダの `nekoviewer_tag_learning.json`（tmpに書いてからrename）。全フォルダ共通で1つ
//! - 括弧にラベルが書かれていないものは「無視」（`ignore`）として学習する。ラベル付けでは、ファイル名の括弧をすべて見て
//!   判断した前提のため
//! - 学習の例は、括弧のトークン（`tag_features::tokenize`）ごとに1つ。ラベルの文字列とトークンの対応は、完全一致キー
//!   （全角半角・空白・記号・大小を同一視。言語は辞書の正規名）で取る。`A, B` のような併記のトークンは、各部分が同じ
//!   ラベルに対応するときだけ、そのラベルとして扱う

// 学習UI・モデル（P5以降）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tag_corpus::{token_key, CorpusEntry, Label};
use crate::tag_features::{split_multi, tokenize, Token};
use crate::tag_similarity::MatchOptions;

const FILE_NAME: &str = "nekoviewer_tag_learning.json";
const VERSION: u32 = 1;

/// 保存する学習データ。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LearnedSet {
    pub version: u32,
    /// 追加順。同じファイル名は1件だけ（後から足したもので置き換える）。
    pub entries: Vec<CorpusEntry>,
}

impl Default for LearnedSet {
    fn default() -> Self {
        Self { version: VERSION, entries: Vec::new() }
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

impl LearnedSet {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 1件を足す。同じファイル名があれば置き換える（位置は元のまま）。置き換えたら true。
    pub(crate) fn add(&mut self, entry: CorpusEntry) -> bool {
        match self.entries.iter_mut().find(|e| e.file_name == entry.file_name) {
            Some(slot) => {
                *slot = entry;
                true
            }
            None => {
                self.entries.push(entry);
                false
            }
        }
    }

    /// ファイル名で1件（そのファイルのラベルすべて）を消す。消せたら true。
    pub(crate) fn remove(&mut self, file_name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.file_name != file_name);
        self.entries.len() != before
    }

    /// 1ファイルの中の1ラベルだけを消す。消せたら true（ファイル自体は残る）。
    pub(crate) fn remove_label(&mut self, file_name: &str, index: usize) -> bool {
        match self.entries.iter_mut().find(|e| e.file_name == file_name) {
            Some(e) if index < e.labels.len() => {
                e.labels.remove(index);
                true
            }
            _ => false,
        }
    }

    /// 全消去。
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// 保存する（tmpに書いてからrename）。保存できたら true。
    pub(crate) fn save(&self, root: &Path) -> bool {
        let Ok(content) = serde_json::to_string_pretty(self) else { return false };
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

    /// 読む。無い・壊れている・版が新しいときは None。
    pub(crate) fn load(root: &Path) -> Option<Self> {
        let set: Self = serde_json::from_str(&std::fs::read_to_string(path(root)).ok()?).ok()?;
        (set.version <= VERSION).then_some(set)
    }

    /// 学習の例に変換する。
    pub(crate) fn training_data(&self, opts: &MatchOptions) -> TrainingData {
        let mut data = TrainingData::default();
        for e in &self.entries {
            let (examples, unmatched) = examples_for_entry(e, opts);
            data.examples.extend(examples);
            data.unmatched.extend(unmatched.into_iter().map(|(l, t)| (e.file_name.clone(), l, t)));
        }
        data
    }
}

/// 学習の例1つ（括弧のトークン1つ）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Example {
    pub file_name: String,
    pub token: Token,
    pub features: Vec<String>,
    pub label: Label,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrainingData {
    pub examples: Vec<Example>,
    /// ラベルの文字列が、ファイル名のどの括弧にも対応しなかったもの（ファイル名, 種別, 文字列）。
    /// ラベルの書き間違い、または括弧の外にある名前。学習には使えない。
    pub unmatched: Vec<(String, Label, String)>,
}

impl TrainingData {
    /// 種別ごとの例の数（`Label` の宣言順）。
    pub(crate) fn counts(&self) -> Vec<(Label, usize)> {
        [Label::Author, Label::Group, Label::Event, Label::Language, Label::Ignore]
            .into_iter()
            .map(|l| (l, self.examples.iter().filter(|e| e.label == l).count()))
            .collect()
    }
}

/// トークンに対応するラベルを探す。見つかったラベルと、使ったラベルの番号（部分で使った分も含む）を返す。
fn label_for_token(token: &Token, labels: &[(Label, String)], opts: &MatchOptions) -> Option<(Label, Vec<usize>)> {
    // 1) トークン全体が、ラベルの文字列と一致する。
    for (i, (l, text)) in labels.iter().enumerate() {
        if token_key(*l, &token.text, opts) == token_key(*l, text, opts) && !token_key(*l, text, opts).is_empty() {
            return Some((*l, vec![i]));
        }
    }
    // 2) 併記（`A, B`）。各部分が同じ種別のラベルに対応するときだけ。
    let parts = split_multi(&token.text);
    if parts.len() > 1 {
        let mut used: Vec<usize> = Vec::new();
        let mut common: Option<Label> = None;
        for p in &parts {
            let hit = labels.iter().enumerate().find(|(_, (l, text))| {
                let k = token_key(*l, text, opts);
                !k.is_empty() && token_key(*l, p, opts) == k
            });
            let Some((i, (l, _))) = hit else { return None };
            if common.is_some_and(|c| c != *l) {
                return None;
            }
            common = Some(*l);
            used.push(i);
        }
        return common.map(|l| (l, used));
    }
    None
}

/// 1ファイルぶんの学習の例と、対応しなかったラベル。
pub(crate) fn examples_for_entry(e: &CorpusEntry, opts: &MatchOptions) -> (Vec<Example>, Vec<(Label, String)>) {
    let mut used = vec![false; e.labels.len()];
    let mut examples = Vec::new();
    for token in tokenize(&e.file_name) {
        let label = match label_for_token(&token, &e.labels, opts) {
            Some((l, idxs)) => {
                for i in idxs {
                    used[i] = true;
                }
                l
            }
            // ラベルが付いていない括弧は「無視」。
            None => Label::Ignore,
        };
        let features = token.features();
        examples.push(Example { file_name: e.file_name.clone(), token, features, label });
    }
    let unmatched = e.labels.iter().enumerate().filter(|(i, _)| !used[*i]).map(|(_, (l, t))| (*l, t.clone())).collect();
    (examples, unmatched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_corpus::parse_corpus;

    const SYNTHETIC: &str = include_str!("../tests/data/tag_corpus_synthetic.tsv");

    fn entry(name: &str, labels: &[(Label, &str)]) -> CorpusEntry {
        CorpusEntry { file_name: name.into(), labels: labels.iter().map(|(l, t)| (*l, t.to_string())).collect(), hard: false }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nekoviewer_learn_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unlabeled_brackets_become_ignore_examples() {
        let e = entry("(成年コミック) [Kouji] T [DL版].zip", &[(Label::Author, "Kouji")]);
        let (ex, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        let labels: Vec<_> = ex.iter().map(|x| (x.token.text.as_str(), x.label)).collect();
        assert_eq!(labels, vec![("成年コミック", Label::Ignore), ("Kouji", Label::Author), ("DL版", Label::Ignore)]);
        assert!(unmatched.is_empty());
    }

    #[test]
    fn pair_tokens_get_their_own_labels_even_when_swapped() {
        let e = entry("[Kouji (Hawk Bit)] T.zip", &[(Label::Author, "Kouji"), (Label::Group, "Hawk Bit")]);
        let (ex, _) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!((ex[0].label, ex[1].label), (Label::Author, Label::Group));
        assert!(ex[0].features.contains(&"pos:square/lead0/outer".to_string()));
        assert!(ex[1].features.contains(&"pos:square/lead0/inner".to_string()));
    }

    #[test]
    fn multi_author_token_is_labeled_when_parts_share_a_label() {
        let e = entry("[Hawk Bit (Kouji, Tsuki Mori)] T.zip", &[(Label::Group, "Hawk Bit"), (Label::Author, "Kouji"), (Label::Author, "Tsuki Mori")]);
        let (ex, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!(ex[1].label, Label::Author);
        assert!(unmatched.is_empty());
        // 部分のラベルが食い違うなら、併記トークンは無視として扱い、ラベルは未対応に残る。
        let e = entry("[Hawk Bit (Kouji, Tsuki Mori)] T.zip", &[(Label::Author, "Kouji"), (Label::Group, "Tsuki Mori")]);
        let (ex, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!(ex[1].label, Label::Ignore);
        assert_eq!(unmatched.len(), 2 - 0);
    }

    #[test]
    fn unmatched_labels_are_reported() {
        let e = entry("[Kouji] T.zip", &[(Label::Author, "Kouji"), (Label::Event, "C105")]);
        let (_, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!(unmatched, vec![(Label::Event, "C105".to_string())]);
    }

    #[test]
    fn matching_ignores_width_case_and_language_spelling() {
        let e = entry("[ＫＯＵＪＩ] T [Eng].zip", &[(Label::Author, "kouji"), (Label::Language, "English")]);
        let (ex, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!(ex[0].label, Label::Author);
        assert!(unmatched.len() <= 1);
        let e = entry("T [English].zip", &[(Label::Language, "英語")]);
        let (ex, unmatched) = examples_for_entry(&e, &MatchOptions::default());
        assert_eq!(ex[0].label, Label::Language);
        assert!(unmatched.is_empty());
    }

    #[test]
    fn add_replaces_same_file_and_remove_works() {
        let mut s = LearnedSet::default();
        assert!(!s.add(entry("a.zip", &[(Label::Author, "A")])));
        assert!(!s.add(entry("b.zip", &[])));
        assert!(s.add(entry("a.zip", &[(Label::Group, "A")])), "同じファイル名は置き換え");
        assert_eq!((s.len(), s.entries[0].labels[0].0), (2, Label::Group));
        assert!(s.remove_label("a.zip", 0));
        assert!(s.entries[0].labels.is_empty());
        assert!(!s.remove_label("a.zip", 0) && !s.remove_label("zzz.zip", 0));
        assert!(s.remove("b.zip") && !s.remove("b.zip"));
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn save_and_load_round_trip_and_survive_corruption() {
        let dir = temp_dir("rt");
        assert_eq!(LearnedSet::load(&dir), None, "無ければ None");
        let mut s = LearnedSet::default();
        s.add(entry("[A (B)] T.zip", &[(Label::Group, "A"), (Label::Author, "B"), (Label::Ignore, "x")]));
        assert!(s.save(&dir));
        assert_eq!(LearnedSet::load(&dir), Some(s));
        assert!(!dir.join("nekoviewer_tag_learning.json.tmp").exists());
        std::fs::write(dir.join("nekoviewer_tag_learning.json"), "{broken").unwrap();
        assert_eq!(LearnedSet::load(&dir), None, "壊れていれば None");
        std::fs::write(dir.join("nekoviewer_tag_learning.json"), r#"{"version":99,"entries":[]}"#).unwrap();
        assert_eq!(LearnedSet::load(&dir), None, "新しい版は読まない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_corpus_converts_without_unmatched_labels() {
        let set = LearnedSet { version: VERSION, entries: parse_corpus(SYNTHETIC).unwrap() };
        let data = set.training_data(&MatchOptions::default());
        assert!(data.unmatched.is_empty(), "対応しないラベルがある: {:?}", data.unmatched);
        let counts = data.counts();
        println!("例の数: {counts:?} 合計={}", data.examples.len());
        for (l, n) in counts {
            assert!(n > 0, "{l:?} の例が無い");
        }
    }
}
