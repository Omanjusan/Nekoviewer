//! 学習モデルの精度を測る（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! - `predict_with_model`: 学習モデルつきの `build_candidates` の予測（`tag_corpus::evaluate` で採点できる形）
//! - `holdout`: 学習用とテスト用に分けて、現行ルールとモデルを同じテスト用で比べる
//! - `learning_curve`: 学習件数（10/20/30…）ごとに、ランダムな抽出を繰り返して平均する。乱数は固定シードの疑似乱数で、結果は毎回同じ
//! - `k_fold`: 学習データそのものでの交差検証。学習後に出す「目安の一致率」に使う（学習データが少ないと揺れが大きい）

// UI（P6以降）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use crate::i18n::Lang;
use crate::tag_candidates::{build_candidates_with, CandidateInput};
use crate::tag_corpus::{evaluate, predict_with_rules, CorpusEntry, Label, Score};
use crate::tag_learning::LearnedSet;
use crate::tag_model::Model;
use crate::tag_similarity::MatchOptions;
use crate::view_explorer::TagManagerCategoryUi;

/// 根拠の無い空のマスター（メイン＋アーカイブ形式だけ）。ルールとモデル単独の力を測る。
pub(crate) fn empty_master() -> Vec<TagManagerCategoryUi> {
    crate::tag_manager::default_state().0
}

/// モデルつきの予測（ファイルごとの（種別, キー）の集合）。モデルが「無視」と判定して未採用に残した候補は含めない。
pub(crate) fn predict_with_model(
    entries: &[CorpusEntry],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
    model: &Model,
) -> Vec<Vec<(Label, String)>> {
    entries
        .iter()
        .map(|e| {
            let input = CandidateInput { file_name: e.file_name.clone(), format: None, info: None };
            let set = build_candidates_with(std::slice::from_ref(&input), cats, opts, lang, Some(model));
            let mut v: Vec<(Label, String)> = set
                .candidates
                .iter()
                .filter(|c| !c.review)
                .filter_map(|c| {
                    let l = match c.role {
                        crate::tag_candidates::Role::Author => Label::Author,
                        crate::tag_candidates::Role::Group => Label::Group,
                        crate::tag_candidates::Role::Event => Label::Event,
                        crate::tag_candidates::Role::Language => Label::Language,
                        _ => return None,
                    };
                    Some((l, c.key.clone()))
                })
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .collect()
}

/// 学習用とテスト用で、ルールとモデルを比べた結果。
#[derive(Clone, Debug)]
pub(crate) struct Holdout {
    pub rules: Score,
    pub model: Score,
}

/// `train` で学習し、`test` で現行ルール（空マスター）とモデルを採点する。学習用が空ならモデルは作れず None。
pub(crate) fn holdout(train: &[CorpusEntry], test: &[CorpusEntry], opts: &MatchOptions, lang: Lang) -> Option<Holdout> {
    let data = LearnedSet { version: 1, entries: train.to_vec() }.training_data(opts);
    let model = Model::train(&data)?;
    let cats = empty_master();
    let rules = evaluate(test, &predict_with_rules(test, &cats, opts, lang), opts, None);
    let modeled = evaluate(test, &predict_with_model(test, &cats, opts, lang, &model), opts, None);
    Some(Holdout { rules, model: modeled })
}

/// 固定シードの疑似乱数（xorshift64）。外部クレートを使わず、結果を再現できるようにする。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn shuffled(entries: &[CorpusEntry], seed: u64) -> Vec<CorpusEntry> {
    let mut v = entries.to_vec();
    let mut rng = Rng::new(seed);
    for i in (1..v.len()).rev() {
        v.swap(i, (rng.next() % (i as u64 + 1)) as usize);
    }
    v
}

/// 学習件数ごとの平均（ルールとモデル）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CurvePoint {
    /// 学習に使った件数。
    pub n: usize,
    pub trials: usize,
    /// テストに使った件数（1回あたり）。
    pub test_size: usize,
    pub rules_exact: f64,
    pub model_exact: f64,
    /// 正解のラベルのうち当てた割合。
    pub rules_recall: f64,
    pub model_recall: f64,
    /// 余分な予測も数えた適合率。
    pub rules_precision: f64,
    pub model_precision: f64,
}

/// 学習曲線。`sizes` の各件数について、ランダムに `n` 件を学習用に選び、残りで採点するのを `trials` 回繰り返して平均する。
/// コーパスが `n` 件より小さい（テスト用が5件未満になる）サイズは飛ばす。
pub(crate) fn learning_curve(entries: &[CorpusEntry], sizes: &[usize], trials: usize, opts: &MatchOptions, lang: Lang) -> Vec<CurvePoint> {
    let mut out = Vec::new();
    for &n in sizes {
        if n == 0 || entries.len() < n + 5 {
            continue;
        }
        let (mut re, mut me, mut rr, mut mr, mut rp, mut mp) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut done = 0usize;
        for t in 0..trials {
            let all = shuffled(entries, (n * 1000 + t) as u64 + 1);
            let (train, test) = all.split_at(n);
            let Some(h) = holdout(train, test, opts, lang) else { continue };
            re += h.rules.exact_rate().unwrap_or(0.0);
            me += h.model.exact_rate().unwrap_or(0.0);
            rr += h.rules.total().recall().unwrap_or(0.0);
            mr += h.model.total().recall().unwrap_or(0.0);
            rp += h.rules.total().precision().unwrap_or(0.0);
            mp += h.model.total().precision().unwrap_or(0.0);
            done += 1;
        }
        if done == 0 {
            continue;
        }
        let d = done as f64;
        out.push(CurvePoint {
            n,
            trials: done,
            test_size: entries.len() - n,
            rules_exact: re / d,
            model_exact: me / d,
            rules_recall: rr / d,
            model_recall: mr / d,
            rules_precision: rp / d,
            model_precision: mp / d,
        });
    }
    out
}

/// `k` 分割の交差検証（学習データだけで、モデルの一致率の目安を出す）。ルールの結果も同じ分割で出す。
/// 件数が `k` 未満なら None。
pub(crate) fn k_fold(entries: &[CorpusEntry], k: usize, opts: &MatchOptions, lang: Lang) -> Option<Holdout> {
    if k < 2 || entries.len() < k {
        return None;
    }
    let all = shuffled(entries, 7);
    let (mut rules_pred, mut model_pred, mut tests): (Vec<_>, Vec<_>, Vec<CorpusEntry>) = (Vec::new(), Vec::new(), Vec::new());
    let cats = empty_master();
    for fold in 0..k {
        let test: Vec<CorpusEntry> = all.iter().enumerate().filter(|(i, _)| i % k == fold).map(|(_, e)| e.clone()).collect();
        let train: Vec<CorpusEntry> = all.iter().enumerate().filter(|(i, _)| i % k != fold).map(|(_, e)| e.clone()).collect();
        let data = LearnedSet { version: 1, entries: train }.training_data(opts);
        let Some(model) = Model::train(&data) else { continue };
        rules_pred.extend(predict_with_rules(&test, &cats, opts, lang));
        model_pred.extend(predict_with_model(&test, &cats, opts, lang, &model));
        tests.extend(test);
    }
    if tests.is_empty() {
        return None;
    }
    Some(Holdout { rules: evaluate(&tests, &rules_pred, opts, None), model: evaluate(&tests, &model_pred, opts, None) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_corpus::parse_corpus;

    const SYNTHETIC: &str = include_str!("../tests/data/tag_corpus_synthetic.tsv");

    // ── 試験用の擬似ライブラリ（合成）。命名の流儀（外側が作者か）を変えて、学習の効き方を見る ──

    const SYLLABLES: [&str; 16] = ["ka", "mi", "to", "ra", "su", "ne", "ho", "yu", "ri", "ko", "ma", "ta", "no", "se", "ki", "hi"];
    const ADJ: [&str; 12] = ["Blue", "Red", "Gray", "Gold", "Iron", "Soft", "Night", "Wild", "Calm", "Dry", "Old", "Bright"];
    const NOUN: [&str; 12] = ["Moon", "Hill", "Sea", "Gate", "Leaf", "Fox", "Lake", "Wind", "Garden", "Harbor", "Tree", "Stone"];
    const KANJI: [&str; 12] = ["月", "森", "海", "風", "花", "空", "石", "川", "星", "雪", "春", "光"];
    const GENRES: [&str; 4] = ["成年コミック", "一般コミック", "同人誌", "同人CG集"];
    const LANGS: [&str; 4] = ["日本語", "English", "中文", "Korean"];

    fn author_name(r: &mut Rng) -> String {
        match r.next() % 3 {
            0 => {
                let (a, b) = (SYLLABLES[(r.next() % 16) as usize], SYLLABLES[(r.next() % 16) as usize]);
                let n = format!("{a}{b}");
                n[..1].to_uppercase() + &n[1..]
            }
            1 => format!("{}{}", KANJI[(r.next() % 12) as usize], KANJI[(r.next() % 12) as usize]),
            _ => format!("{}{}", SYLLABLES[(r.next() % 16) as usize], SYLLABLES[(r.next() % 16) as usize]),
        }
    }

    fn circle_name(r: &mut Rng) -> String {
        match r.next() % 4 {
            0 | 1 => format!("{} {}", ADJ[(r.next() % 12) as usize], NOUN[(r.next() % 12) as usize]),
            2 => format!("{}の{}", KANJI[(r.next() % 12) as usize], KANJI[(r.next() % 12) as usize]),
            _ => author_name(r),
        }
    }

    /// 擬似ライブラリ。`swap_rate` は `[作者 (サークル)]` の向きが逆（外側が作者）の割合。
    fn synth_library(n: usize, swap_rate: f64, seed: u64) -> Vec<CorpusEntry> {
        let mut r = Rng::new(seed);
        let pct = |r: &mut Rng| (r.next() % 1000) as f64 / 1000.0;
        let mut out: Vec<CorpusEntry> = Vec::new();
        let mut serial = 0;
        while out.len() < n {
            serial += 1;
            let kind = r.next() % 100;
            let (a, c) = (author_name(&mut r), circle_name(&mut r));
            let lang = LANGS[(r.next() % 4) as usize];
            let ev = format!("C{}", 60 + r.next() % 50);
            let title = format!("Title {serial}");
            let mut labels: Vec<(Label, String)> = Vec::new();
            let mut name = String::new();
            let has_event = r.next() % 100 < 45;
            let has_lang = r.next() % 100 < 40;
            if kind < 55 {
                // (イベント) [サークル (作者)] 題名 [言語]
                if has_event {
                    name += &format!("({ev}) ");
                    labels.push((Label::Event, ev));
                }
                if pct(&mut r) < swap_rate {
                    name += &format!("[{a} ({c})] ");
                    labels.push((Label::Author, a));
                    labels.push((Label::Group, c));
                } else {
                    name += &format!("[{c} ({a})] ");
                    labels.push((Label::Group, c));
                    labels.push((Label::Author, a));
                }
            } else if kind < 68 {
                name += &format!("[{a}] ");
                labels.push((Label::Author, a));
            } else if kind < 76 {
                name += &format!("[{c}] ");
                labels.push((Label::Group, c));
            } else if kind < 86 {
                let g = GENRES[(r.next() % 4) as usize];
                name += &format!("({g}) [{a}] ");
                labels.push((Label::Ignore, g.to_string()));
                labels.push((Label::Author, a));
            } else if kind < 92 {
                name += &format!("[{ev}] [{a}] ");
                labels.push((Label::Event, ev));
                labels.push((Label::Author, a));
            } else if kind < 96 {
                name += &title;
                name += &format!(" [{a}].zip");
                labels.push((Label::Author, a));
                out.push(CorpusEntry { file_name: name, labels, hard: false });
                continue;
            }
            name += &title;
            if has_lang {
                name += &format!(" [{lang}]");
                labels.push((Label::Language, lang.to_string()));
            }
            name += ".zip";
            out.push(CorpusEntry { file_name: name, labels, hard: false });
        }
        out
    }

    fn print_curve(title: &str, entries: &[CorpusEntry], sizes: &[usize]) {
        let opts = MatchOptions::default();
        println!("== {title}（{}件）: 完全一致率 / 再現率 / 適合率  ルール → モデル ==", entries.len());
        for p in learning_curve(entries, sizes, 20, &opts, Lang::Japanese) {
            println!(
                "  学習{:>3}件(テスト{}件×{}回): 完全一致 {:>5.1}% → {:>5.1}%   再現率 {:>5.1}% → {:>5.1}%   適合率 {:>5.1}% → {:>5.1}%",
                p.n,
                p.test_size,
                p.trials,
                p.rules_exact * 100.0,
                p.model_exact * 100.0,
                p.rules_recall * 100.0,
                p.model_recall * 100.0,
                p.rules_precision * 100.0,
                p.model_precision * 100.0
            );
        }
    }

    #[test]
    fn rng_and_shuffle_are_deterministic() {
        let e = synth_library(30, 0.5, 1);
        assert_eq!(shuffled(&e, 3), shuffled(&e, 3));
        assert_ne!(shuffled(&e, 3), shuffled(&e, 4));
        assert_eq!(synth_library(30, 0.5, 1), e);
    }

    #[test]
    fn synthetic_library_labels_match_their_tokens() {
        // 擬似ライブラリの正解ラベルが、すべて括弧に対応する（対応しないラベルがあると、学習曲線が歪む）。
        let e = synth_library(150, 0.5, 2);
        let data = LearnedSet { version: 1, entries: e }.training_data(&MatchOptions::default());
        assert!(data.unmatched.is_empty(), "{:?}", &data.unmatched[..data.unmatched.len().min(5)]);
    }

    #[test]
    fn model_beats_rules_on_a_swapped_convention_library() {
        let opts = MatchOptions::default();
        let lib = synth_library(120, 0.9, 5);
        let (train, test) = lib.split_at(30);
        let h = holdout(train, test, &opts, Lang::Japanese).unwrap();
        assert!(
            h.model.exact_rate().unwrap() > h.rules.exact_rate().unwrap() + 0.1,
            "逆向きが主流の流儀では、学習が効く: ルール{:?} モデル{:?}",
            h.rules.exact_rate(),
            h.model.exact_rate()
        );
    }

    #[test]
    fn holdout_needs_training_data() {
        let lib = synth_library(20, 0.1, 1);
        assert!(holdout(&[], &lib, &MatchOptions::default(), Lang::Japanese).is_none());
    }

    #[test]
    fn k_fold_works_and_needs_enough_entries() {
        let opts = MatchOptions::default();
        let lib = synth_library(40, 0.9, 3);
        let h = k_fold(&lib, 5, &opts, Lang::Japanese).unwrap();
        assert_eq!(h.model.files, 40, "全件が1回ずつテストに回る");
        assert!(k_fold(&lib[..3], 5, &opts, Lang::Japanese).is_none());
    }

    #[test]
    fn model_marks_ignored_brackets_for_review_instead_of_dropping_them() {
        let opts = MatchOptions::default();
        let lib = synth_library(60, 0.1, 9);
        let data = LearnedSet { version: 1, entries: lib }.training_data(&opts);
        let model = Model::train(&data).unwrap();
        let input = CandidateInput { file_name: "(成年コミック) [Zed] T.zip".into(), format: None, info: None };
        let set = build_candidates_with(&[input], &empty_master(), &opts, Lang::Japanese, Some(&model));
        let genre = set.candidates.iter().find(|c| c.name == "成年コミック").expect("無視と判定されても候補は残る");
        assert!(genre.review && !genre.is_adopted(), "未採用・要確認で残す");
        let author = set.candidates.iter().find(|c| c.name == "Zed").unwrap();
        assert!(!author.review);
    }

    #[test]
    fn master_match_wins_over_the_model() {
        // 逆向きの流儀で学習していても、マスターに「作者 Hawk Bit」の裏づけがあれば、その向きになる。
        use crate::tag_candidates::{Choice, Role};
        let opts = MatchOptions::default();
        let lib = synth_library(60, 0.9, 11);
        let model = Model::train(&LearnedSet { version: 1, entries: lib }.training_data(&opts)).unwrap();
        let mut cats = empty_master();
        let mut next = 100u64;
        {
            let mut m = crate::tag_master::TagMaster::new(&mut cats, &mut next);
            m.ensure_system_categories();
            let c = m.add_category("グループ".to_string(), false, None);
            m.push_element(c, "Hawk Bit");
            let a = m.add_category("作者".to_string(), false, None);
            m.push_element(a, "Kouji");
        }
        let input = CandidateInput { file_name: "[Hawk Bit (Kouji)] T.zip".into(), format: None, info: None };
        let set = build_candidates_with(&[input], &cats, &opts, Lang::Japanese, Some(&model));
        let g = set.candidates.iter().find(|c| c.name == "Hawk Bit").unwrap();
        assert_eq!(g.role, Role::Group);
        assert!(matches!(g.choice, Choice::Existing { .. }));
    }

    /// 目視用: 擬似ライブラリと合成コーパスの学習曲線（`cargo test learning_curve_on_synthetic -- --nocapture`）。
    /// 擬似ライブラリは、このテストの作者が想定した命名規則から作ったもので、実ライブラリの精度ではない。
    #[test]
    fn learning_curve_on_synthetic() {
        let sizes = [10, 20, 30, 40, 50];
        print_curve("擬似ライブラリA（[サークル(作者)]が主流・逆向き10%）", &synth_library(150, 0.1, 21), &sizes);
        print_curve("擬似ライブラリB（[作者(サークル)]が主流・逆向き90%）", &synth_library(150, 0.9, 22), &sizes);
        print_curve("擬似ライブラリC（混在・逆向き50%）", &synth_library(150, 0.5, 23), &sizes);
        let corpus = parse_corpus(SYNTHETIC).unwrap();
        print_curve("手書きの合成コーパス（難問多め）", &corpus, &[10, 20, 30]);
    }

    /// 目視用: `NEKO_CORPUS` の実コーパスの学習曲線（`NEKO_CORPUS=<file> cargo test learning_curve_on_file -- --ignored --nocapture`）。
    #[test]
    #[ignore]
    fn learning_curve_on_file() {
        let Ok(path) = std::env::var("NEKO_CORPUS") else { return };
        let entries = parse_corpus(&std::fs::read_to_string(path).unwrap()).unwrap();
        print_curve("実コーパス", &entries, &[10, 20, 30, 40, 50]);
        if let Some(h) = k_fold(&entries, 5, &MatchOptions::default(), Lang::Japanese) {
            println!("5分割交差検証: ルール {:?} → モデル {:?}", h.rules.exact_rate(), h.model.exact_rate());
        }
    }
}
