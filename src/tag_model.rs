//! 括弧トークンの種別（作者／グループ／イベント／言語／無視）を当てる、学習モデル（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! - モデルは多クラスのロジスティック回帰（softmax）。入力は `tag_features` の特徴量（文字列の集合）。外部クレートは使わない
//! - 学習は全バッチの勾配降下で、乱数を使わない（同じ学習データなら必ず同じモデルになる）。30件前後（約100トークン）なら一瞬で終わる
//! - `A (B)` の外側・内側は、互いに違う種別になる組み合わせの中から、確率の積が最大のものを選ぶ（向きの取り違えを1つの判断として扱う）
//! - 学習データが空なら、モデルは作らない（`train` は None）。その場合、呼び出し側は現行ルールだけで動く

// UI・設定への配線（P6以降）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use std::collections::HashMap;

use crate::tag_corpus::Label;
use crate::tag_features::{tokenize, Nesting, Token};
use crate::tag_learning::TrainingData;

/// 種別の並び（確率・重みの添字）。
pub(crate) const CLASSES: [Label; 5] = [Label::Author, Label::Group, Label::Event, Label::Language, Label::Ignore];
const N: usize = CLASSES.len();

/// 勾配降下の回数・刻み・正則化。小さな学習データでも過剰に尖らないよう、L2 を強めにかけている。
const ITERATIONS: usize = 400;
const LEARNING_RATE: f64 = 0.8;
const L2: f64 = 0.02;

fn class_index(l: Label) -> usize {
    CLASSES.iter().position(|c| *c == l).unwrap()
}

#[derive(Clone, Debug)]
pub(crate) struct Model {
    /// 特徴量ごとの、種別別の重み。
    weights: HashMap<String, [f64; N]>,
    /// 学習に使ったトークン数。
    pub trained_on: usize,
}

/// 1トークンの予測。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TokenPrediction {
    pub token: Token,
    pub label: Label,
    /// 選んだ種別の確率（外側・内側の組は、組み合わせ全体の確率）。
    pub confidence: f64,
}

fn softmax(logits: [f64; N]) -> [f64; N] {
    let max = logits.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut e = [0.0; N];
    let mut sum = 0.0;
    for (i, l) in logits.iter().enumerate() {
        e[i] = (l - max).exp();
        sum += e[i];
    }
    for v in &mut e {
        *v /= sum;
    }
    e
}

impl Model {
    /// 学習する。例が1つも無ければ None。
    pub(crate) fn train(data: &TrainingData) -> Option<Model> {
        if data.examples.is_empty() {
            return None;
        }
        // 特徴量の番号づけは出現順（決定的）。
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut names: Vec<&str> = Vec::new();
        for ex in &data.examples {
            for f in &ex.features {
                index.entry(f.as_str()).or_insert_with(|| {
                    names.push(f.as_str());
                    names.len() - 1
                });
            }
        }
        let rows: Vec<(Vec<usize>, usize)> = data
            .examples
            .iter()
            .map(|ex| (ex.features.iter().map(|f| index[f.as_str()]).collect(), class_index(ex.label)))
            .collect();
        let n = rows.len() as f64;
        let mut w = vec![[0.0f64; N]; names.len()];
        for _ in 0..ITERATIONS {
            let mut grad = vec![[0.0f64; N]; names.len()];
            for (feats, y) in &rows {
                let mut logits = [0.0; N];
                for &f in feats {
                    for c in 0..N {
                        logits[c] += w[f][c];
                    }
                }
                let p = softmax(logits);
                for &f in feats {
                    for c in 0..N {
                        grad[f][c] += p[c] - if c == *y { 1.0 } else { 0.0 };
                    }
                }
            }
            for (f, g) in grad.iter().enumerate() {
                for c in 0..N {
                    // bias は正則化しない（先頭の特徴量名で判定）。
                    let reg = if names[f] == "bias" { 0.0 } else { L2 * w[f][c] };
                    w[f][c] -= LEARNING_RATE * (g[c] / n + reg);
                }
            }
        }
        let weights = names.iter().zip(w).map(|(name, w)| (name.to_string(), w)).collect();
        Some(Model { weights, trained_on: rows.len() })
    }

    /// 特徴量から、種別ごとの確率（`CLASSES` の順）。知らない特徴量は無視する。
    pub(crate) fn probabilities(&self, features: &[String]) -> [f64; N] {
        let mut logits = [0.0; N];
        for f in features {
            if let Some(w) = self.weights.get(f) {
                for c in 0..N {
                    logits[c] += w[c];
                }
            }
        }
        softmax(logits)
    }

    /// ファイル名の括弧すべてについて、種別を予測する（出現順）。
    pub(crate) fn predict_file(&self, file_name: &str) -> Vec<TokenPrediction> {
        self.predict_tokens(tokenize(file_name))
    }

    pub(crate) fn predict_tokens(&self, tokens: Vec<Token>) -> Vec<TokenPrediction> {
        let probs: Vec<[f64; N]> = tokens.iter().map(|t| self.probabilities(&t.features())).collect();
        let mut out: Vec<TokenPrediction> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            let is_pair = tokens[i].nesting == Nesting::Outer && tokens.get(i + 1).is_some_and(|t| t.nesting == Nesting::Inner);
            if is_pair {
                let (pa, pb) = (&probs[i], &probs[i + 1]);
                // 外側・内側が同じ種別（無視どうしを除く）になる組は除く。残りの組の確率で正規化する。
                let (mut best, mut best_p, mut total) = ((0usize, 0usize), -1.0f64, 0.0f64);
                for a in 0..N {
                    for b in 0..N {
                        if a == b && CLASSES[a] != Label::Ignore {
                            continue;
                        }
                        let p = pa[a] * pb[b];
                        total += p;
                        if p > best_p {
                            best_p = p;
                            best = (a, b);
                        }
                    }
                }
                let conf = if total > 0.0 { best_p / total } else { 0.0 };
                out.push(TokenPrediction { token: tokens[i].clone(), label: CLASSES[best.0], confidence: conf });
                out.push(TokenPrediction { token: tokens[i + 1].clone(), label: CLASSES[best.1], confidence: conf });
                i += 2;
            } else {
                let p = &probs[i];
                let c = (0..N).max_by(|&x, &y| p[x].partial_cmp(&p[y]).unwrap().then(y.cmp(&x))).unwrap();
                out.push(TokenPrediction { token: tokens[i].clone(), label: CLASSES[c], confidence: p[c] });
                i += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_corpus::{parse_corpus, CorpusEntry};
    use crate::tag_learning::LearnedSet;
    use crate::tag_similarity::MatchOptions;

    fn data_of(entries: Vec<CorpusEntry>) -> TrainingData {
        LearnedSet { version: 1, entries }.training_data(&MatchOptions::default())
    }

    fn labels_of(model: &Model, name: &str) -> Vec<Label> {
        model.predict_file(name).into_iter().map(|p| p.label).collect()
    }

    #[test]
    fn empty_training_data_gives_no_model() {
        assert!(Model::train(&TrainingData::default()).is_none());
    }

    #[test]
    fn training_is_deterministic() {
        let entries = parse_corpus(include_str!("../tests/data/tag_corpus_synthetic.tsv")).unwrap();
        let (a, b) = (Model::train(&data_of(entries.clone())).unwrap(), Model::train(&data_of(entries)).unwrap());
        let f = tokenize("(C105) [Hawk Bit (Kouji)] T [日本語].zip")[1].features();
        assert_eq!(a.probabilities(&f), b.probabilities(&f));
    }

    #[test]
    fn learns_the_swapped_pair_convention_from_a_few_examples() {
        // 「外側が作者・内側がサークル」の流儀で10件。名前は毎回違うので、位置と形から学ぶしかない。
        let names = ["Aoi", "Bell", "Cato", "Dune", "Echo", "Fumi", "Gale", "Haru", "Iris", "Jun"];
        let circles = ["Blue Moon", "Red Sun", "Green Hill", "Gray Sea", "Pink Sky", "Gold Sand", "Iron Gate", "Salt Lake", "Dry Leaf", "Old Pine"];
        let entries: Vec<CorpusEntry> = names
            .iter()
            .zip(circles)
            .map(|(n, c)| CorpusEntry {
                file_name: format!("[{n} ({c})] Title.zip"),
                labels: vec![(Label::Author, n.to_string()), (Label::Group, c.to_string())],
                hard: false,
            })
            .collect();
        let model = Model::train(&data_of(entries)).unwrap();
        assert_eq!(labels_of(&model, "[Kai (Rock Fall)] Another.zip"), vec![Label::Author, Label::Group]);
    }

    #[test]
    fn learns_to_ignore_genre_brackets_and_keep_events() {
        let entries = parse_corpus(include_str!("../tests/data/tag_corpus_synthetic.tsv")).unwrap();
        let model = Model::train(&data_of(entries)).unwrap();
        assert_eq!(labels_of(&model, "(成年コミック) [Zed] T.zip"), vec![Label::Ignore, Label::Author]);
        assert_eq!(labels_of(&model, "(C77) [Zed] T.zip")[0], Label::Event);
    }

    #[test]
    fn pair_gets_two_different_labels_and_shares_confidence() {
        let entries = parse_corpus(include_str!("../tests/data/tag_corpus_synthetic.tsv")).unwrap();
        let model = Model::train(&data_of(entries)).unwrap();
        let p = model.predict_file("[Foo Bar (Baz)] T.zip");
        assert_eq!(p.len(), 2);
        assert_ne!(p[0].label, p[1].label);
        assert_eq!(p[0].confidence, p[1].confidence);
        assert!((0.0..=1.0).contains(&p[0].confidence));
    }

    #[test]
    fn unseen_features_are_ignored() {
        let entries = parse_corpus("[A (B)] T.zip\tgroup=A\tauthor=B\n").unwrap();
        let model = Model::train(&data_of(entries)).unwrap();
        let p = model.probabilities(&["never-seen".to_string()]);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }
}
