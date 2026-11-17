//! タグ自動取得の判定を測るための「サンプルコーパス」（画面・DB・ファイルI/Oに触れない純関数）。
//!
//! 学習（`P4`以降）とチューニングの土台。ファイル名ごとに「正解のラベル」（どの括弧の中身が作者・グループ・イベント・言語か）を持ち、
//! 現行ルール（`build_candidates`）の出力と突き合わせて一致率を出す。
//!
//! # ファイル形式（TSV・1行1ファイル）
//! ```text
//! # 先頭が # の行と空行は無視
//! (C105) [Hawk Bit (Kouji)] Title [日本語].zip<TAB>event=C105<TAB>group=Hawk Bit<TAB>author=Kouji<TAB>language=日本語
//! [Writer A (Circle A)] Title.zip<TAB>author=Writer A<TAB>group=Circle A<TAB>!hard
//! (成年コミック) [X] Title.zip<TAB>ignore=成年コミック<TAB>author=X
//! ```
//! - 1列目はファイル名（拡張子つき）。2列目以降は `種別=文字列`（種別: author / group / event / language / ignore）
//! - `ignore` は「括弧だが、タグにしない」もの。判定の採点には使わない（予測側が出すと誤り＝余分、として数える）
//! - `!hard` は難問の印（ルールで当たりにくい・向きが曖昧なもの）。採点を難問だけに絞れる
//! - 採点は、種別＋完全一致キー（全角半角・空白・記号・大小を同一視。言語は辞書の正規名）の集合で比べる
//!
//! # 置き場所
//! - 合成の最小セット（コミットする）: `tests/data/tag_corpus_synthetic.tsv`
//! - 実ライブラリのファイル名（コミットしない）: `test/tag_corpus/` 以下（`/test` は gitignore 済み）

// 学習（P4以降）が使うまで、公開APIの一部は未使用。
#![allow(dead_code)]

use crate::i18n::Lang;
use crate::tag_candidates::{build_candidates, CandidateInput, Role};
use crate::tag_similarity::{exact_key, MatchOptions};
use crate::tag_source::canonical_language;
use crate::view_explorer::TagManagerCategoryUi;

/// 正解（または予測）のラベル種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Label {
    Author,
    Group,
    Event,
    Language,
    /// 括弧だが、タグにしないもの（`DL版` `(成年コミック)` など）。
    Ignore,
}

impl Label {
    pub(crate) const SCORED: [Label; 4] = [Label::Author, Label::Group, Label::Event, Label::Language];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Label::Author => "author",
            Label::Group => "group",
            Label::Event => "event",
            Label::Language => "language",
            Label::Ignore => "ignore",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Label> {
        match s.trim().to_ascii_lowercase().as_str() {
            "author" => Some(Label::Author),
            "group" => Some(Label::Group),
            "event" => Some(Label::Event),
            "language" => Some(Label::Language),
            "ignore" => Some(Label::Ignore),
            _ => None,
        }
    }

    fn from_role(role: Role) -> Option<Label> {
        match role {
            Role::Author => Some(Label::Author),
            Role::Group => Some(Label::Group),
            Role::Event => Some(Label::Event),
            Role::Language => Some(Label::Language),
            Role::Format | Role::Main | Role::Performer | Role::Character | Role::Parody | Role::Attribute | Role::YearMonth => None,
        }
    }
}

/// コーパスの1行。
#[derive(Clone, Debug, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CorpusEntry {
    pub file_name: String,
    /// 正解のラベル（出現順。文字列は書かれた表記のまま）。
    pub labels: Vec<(Label, String)>,
    /// 難問の印。
    pub hard: bool,
}

/// コーパスを読む。形式の誤りは「行番号: 理由」で返す。
pub(crate) fn parse_corpus(text: &str) -> Result<Vec<CorpusEntry>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let file_name = cols.next().unwrap_or("").trim().to_string();
        if file_name.is_empty() {
            return Err(format!("{}行目: ファイル名が空", i + 1));
        }
        let mut entry = CorpusEntry { file_name, labels: Vec::new(), hard: false };
        for col in cols {
            let col = col.trim();
            if col.is_empty() {
                continue;
            }
            if col == "!hard" {
                entry.hard = true;
                continue;
            }
            let Some((k, v)) = col.split_once('=') else {
                return Err(format!("{}行目: `{col}` は 種別=文字列 でも !hard でもない", i + 1));
            };
            let Some(label) = Label::parse(k) else {
                return Err(format!("{}行目: 未知の種別 `{k}`", i + 1));
            };
            let v = v.trim();
            if v.is_empty() {
                return Err(format!("{}行目: `{k}=` の文字列が空", i + 1));
            }
            entry.labels.push((label, v.to_string()));
        }
        out.push(entry);
    }
    Ok(out)
}

/// 1行ぶんをファイル形式に直す（`parse_corpus` で読み戻せる）。
pub(crate) fn format_entry(e: &CorpusEntry) -> String {
    let mut s = e.file_name.clone();
    for (l, t) in &e.labels {
        s.push('\t');
        s.push_str(l.as_str());
        s.push('=');
        s.push_str(t);
    }
    if e.hard {
        s.push_str("\t!hard");
    }
    s
}

// ── 採点 ─────────────────────────────────────────────────────────────

/// 比較用のキー。言語は辞書の正規名、それ以外は完全一致キー。
pub(crate) fn token_key(label: Label, text: &str, opts: &MatchOptions) -> String {
    if label == Label::Language {
        if let Some(c) = canonical_language(text) {
            return c.to_string();
        }
    }
    exact_key(text, opts)
}

/// 正解を比較用の（種別, キー）の集合にする（`ignore` は除く。重複なし・整列）。
pub(crate) fn expected_tokens(e: &CorpusEntry, opts: &MatchOptions) -> Vec<(Label, String)> {
    let mut v: Vec<(Label, String)> = e
        .labels
        .iter()
        .filter(|(l, _)| *l != Label::Ignore)
        .map(|(l, t)| (*l, token_key(*l, t, opts)))
        .filter(|(_, k)| !k.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 現行ルールの予測（ファイルごとの（種別, キー）の集合）。マスターは渡したものを使う
/// （根拠の無い空のマスターなら、ルール単独の力が測れる）。1ファイルずつ生成するので、ファイル間の束ねは起きない。
pub(crate) fn predict_with_rules(
    entries: &[CorpusEntry],
    cats: &[TagManagerCategoryUi],
    opts: &MatchOptions,
    lang: Lang,
) -> Vec<Vec<(Label, String)>> {
    entries
        .iter()
        .map(|e| {
            let input = CandidateInput { file_name: e.file_name.clone(), format: None, info: None, meta: None };
            let set = build_candidates(std::slice::from_ref(&input), cats, opts, lang);
            let mut v: Vec<(Label, String)> = set
                .candidates
                .iter()
                .filter_map(|c| Label::from_role(c.role).map(|l| (l, c.key.clone())))
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .collect()
}

/// 種別ごとの集計。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LabelScore {
    pub expected: usize,
    pub predicted: usize,
    pub correct: usize,
}

impl LabelScore {
    /// 再現率（正解のうち当てた割合）。正解が無ければ None。
    pub(crate) fn recall(&self) -> Option<f64> {
        (self.expected > 0).then(|| self.correct as f64 / self.expected as f64)
    }
    /// 適合率（予測のうち当たった割合）。予測が無ければ None。
    pub(crate) fn precision(&self) -> Option<f64> {
        (self.predicted > 0).then(|| self.correct as f64 / self.predicted as f64)
    }
}

/// 外れたファイル1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mismatch {
    pub file_name: String,
    pub hard: bool,
    /// 正解にあるのに予測に無い。
    pub missing: Vec<(Label, String)>,
    /// 予測にあるのに正解に無い。
    pub extra: Vec<(Label, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Score {
    pub files: usize,
    /// 正解と予測の集合が完全に一致したファイル数。
    pub files_exact: usize,
    pub by_label: Vec<(Label, LabelScore)>,
    pub mismatches: Vec<Mismatch>,
}

impl Score {
    pub(crate) fn exact_rate(&self) -> Option<f64> {
        (self.files > 0).then(|| self.files_exact as f64 / self.files as f64)
    }

    pub(crate) fn total(&self) -> LabelScore {
        self.by_label.iter().fold(LabelScore::default(), |a, (_, s)| LabelScore {
            expected: a.expected + s.expected,
            predicted: a.predicted + s.predicted,
            correct: a.correct + s.correct,
        })
    }

    /// 人が読む1画面ぶんの要約。
    pub(crate) fn report(&self) -> String {
        let pct = |v: Option<f64>| v.map_or("-".to_string(), |x| format!("{:.1}%", x * 100.0));
        let mut s = format!(
            "files={} exact={} ({})\n",
            self.files,
            self.files_exact,
            pct(self.exact_rate())
        );
        for (l, sc) in self.by_label.iter().chain(std::iter::once(&(Label::Ignore, self.total()))) {
            let name = if *l == Label::Ignore { "total" } else { l.as_str() };
            s.push_str(&format!(
                "  {name:<8} 正解={:<4} 予測={:<4} 一致={:<4} 再現率={} 適合率={}\n",
                sc.expected,
                sc.predicted,
                sc.correct,
                pct(sc.recall()),
                pct(sc.precision())
            ));
        }
        s
    }
}

/// 予測を採点する。`only_hard` が Some なら、難問の印が一致するファイルだけを対象にする。
pub(crate) fn evaluate(
    entries: &[CorpusEntry],
    predicted: &[Vec<(Label, String)>],
    opts: &MatchOptions,
    only_hard: Option<bool>,
) -> Score {
    let mut score = Score {
        by_label: Label::SCORED.iter().map(|l| (*l, LabelScore::default())).collect(),
        ..Score::default()
    };
    for (e, pred) in entries.iter().zip(predicted) {
        if only_hard.is_some_and(|h| h != e.hard) {
            continue;
        }
        let exp = expected_tokens(e, opts);
        score.files += 1;
        for (l, ls) in &mut score.by_label {
            ls.expected += exp.iter().filter(|(x, _)| x == l).count();
            ls.predicted += pred.iter().filter(|(x, _)| x == l).count();
            ls.correct += exp.iter().filter(|t| t.0 == *l && pred.contains(t)).count();
        }
        let missing: Vec<_> = exp.iter().filter(|t| !pred.contains(t)).cloned().collect();
        let extra: Vec<_> = pred.iter().filter(|t| !exp.contains(t)).cloned().collect();
        if missing.is_empty() && extra.is_empty() {
            score.files_exact += 1;
        } else {
            score.mismatches.push(Mismatch { file_name: e.file_name.clone(), hard: e.hard, missing, extra });
        }
    }
    score
}

// ── ラベル雛形 ───────────────────────────────────────────────────────

/// ファイル名の一覧から、現行ルールの結果を初期値にしたコーパス（TSV）を作る。
/// ユーザーは間違った行だけを直せばよい（`ignore` や `!hard` は手で足す）。
pub(crate) fn template_from_names(names: &[String], cats: &[TagManagerCategoryUi], opts: &MatchOptions, lang: Lang) -> String {
    let mut out = String::from("# タグ自動取得 サンプルコーパス（現行ルールの結果を初期値にした雛形。間違った行を直す）\n");
    for name in names {
        let input = CandidateInput { file_name: name.clone(), format: None, info: None, meta: None };
        let set = build_candidates(std::slice::from_ref(&input), cats, opts, lang);
        let mut labels: Vec<(Label, String)> = Vec::new();
        for role in [Role::Event, Role::Group, Role::Author, Role::Language] {
            for c in set.candidates.iter().filter(|c| c.role == role) {
                if let (Some(l), Some(f)) = (Label::from_role(role), c.forms.first()) {
                    labels.push((l, f.clone()));
                }
            }
        }
        out.push_str(&format_entry(&CorpusEntry { file_name: name.clone(), labels, hard: false }));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTHETIC: &str = include_str!("../tests/data/tag_corpus_synthetic.tsv");

    /// 根拠の無い空のマスター（メイン＋アーカイブ形式だけ）。ルール単独の力を測る。
    fn empty_master() -> Vec<TagManagerCategoryUi> {
        crate::tag_manager::default_state().0
    }

    #[test]
    fn parses_labels_hard_marks_and_comments() {
        let text = "# c\n\n(C1) [A (B)] T.zip\tevent=C1\tgroup=A\tauthor=B\t!hard\r\nplain.zip\n";
        let v = parse_corpus(text).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].labels, vec![(Label::Event, "C1".into()), (Label::Group, "A".into()), (Label::Author, "B".into())]);
        assert!(v[0].hard);
        assert!(v[1].labels.is_empty() && !v[1].hard);
    }

    #[test]
    fn reports_malformed_lines_with_line_numbers() {
        assert!(parse_corpus("a.zip\tfoo=x").unwrap_err().starts_with("1行目"));
        assert!(parse_corpus("ok.zip\n\na.zip\tauthor").unwrap_err().starts_with("3行目"));
        assert!(parse_corpus("a.zip\tauthor=").unwrap_err().contains("空"));
    }

    #[test]
    fn format_then_parse_round_trips() {
        let v = parse_corpus(SYNTHETIC).unwrap();
        let text: String = v.iter().map(|e| format_entry(e) + "\n").collect();
        assert_eq!(parse_corpus(&text).unwrap(), v);
    }

    #[test]
    fn comparison_ignores_width_case_and_language_spelling() {
        let opts = MatchOptions::default();
        let e = CorpusEntry {
            file_name: "x".into(),
            labels: vec![(Label::Author, "ＫＯＵＪＩ".into()), (Label::Language, "English".into()), (Label::Ignore, "DL版".into())],
            hard: false,
        };
        let t = expected_tokens(&e, &opts);
        assert_eq!(t.len(), 2, "ignore は採点に使わない");
        assert!(t.contains(&(Label::Author, exact_key("kouji", &opts))));
        assert!(t.contains(&(Label::Language, token_key(Label::Language, "英語", &opts))));
    }

    #[test]
    fn evaluate_counts_exact_missing_and_extra() {
        let opts = MatchOptions::default();
        let entries = parse_corpus("a.zip\tauthor=A\tgroup=G\nb.zip\tevent=C1\n").unwrap();
        let k = |l, t: &str| (l, token_key(l, t, &opts));
        // a: 作者だけ当てて、グループを取りこぼし、余分なイベントを出した。b: 完全一致。
        let pred = vec![vec![k(Label::Author, "A"), k(Label::Event, "X")], vec![k(Label::Event, "C1")]];
        let s = evaluate(&entries, &pred, &opts, None);
        assert_eq!((s.files, s.files_exact), (2, 1));
        assert_eq!(s.mismatches.len(), 1);
        assert_eq!(s.mismatches[0].missing, vec![k(Label::Group, "G")]);
        assert_eq!(s.mismatches[0].extra, vec![k(Label::Event, "X")]);
        let author = s.by_label.iter().find(|(l, _)| *l == Label::Author).unwrap().1;
        assert_eq!((author.expected, author.predicted, author.correct), (1, 1, 1));
        assert_eq!(s.total().correct, 2);
    }

    #[test]
    fn evaluate_can_filter_by_hard_mark() {
        let opts = MatchOptions::default();
        let entries = parse_corpus("a.zip\tauthor=A\t!hard\nb.zip\tauthor=B\n").unwrap();
        let pred = predict_with_rules(&entries, &empty_master(), &opts, Lang::Japanese);
        assert_eq!(evaluate(&entries, &pred, &opts, Some(true)).files, 1);
        assert_eq!(evaluate(&entries, &pred, &opts, Some(false)).files, 1);
        assert_eq!(evaluate(&entries, &pred, &opts, None).files, 2);
    }

    #[test]
    fn template_round_trips_through_parse_and_scores_itself_perfectly() {
        let opts = MatchOptions::default();
        let names: Vec<String> = ["(C105) [Hawk Bit (Kouji)] Title [日本語].zip", "[Solo] Book.cbz", "no brackets.zip"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let text = template_from_names(&names, &empty_master(), &opts, Lang::Japanese);
        let entries = parse_corpus(&text).unwrap();
        assert_eq!(entries.len(), 3);
        let pred = predict_with_rules(&entries, &empty_master(), &opts, Lang::Japanese);
        let s = evaluate(&entries, &pred, &opts, None);
        assert_eq!(s.files_exact, 3, "雛形は現行ルールの出力そのものなので全件一致する: {}", s.report());
    }

    #[test]
    fn synthetic_corpus_is_well_formed_and_covers_every_label() {
        let entries = parse_corpus(SYNTHETIC).unwrap();
        assert!(entries.len() >= 40, "合成コーパスは40件以上: {}", entries.len());
        for l in [Label::Author, Label::Group, Label::Event, Label::Language, Label::Ignore] {
            assert!(entries.iter().any(|e| e.labels.iter().any(|(x, _)| *x == l)), "{l:?} の例が無い");
        }
        assert!(entries.iter().any(|e| e.hard), "難問の印つきの例が無い");
        let mut names: Vec<&str> = entries.iter().map(|e| e.file_name.as_str()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "ファイル名の重複");
    }

    /// 目視用: 合成コーパスに対する現行ルールの一致率（ベースライン）を表示する
    /// （`cargo test baseline_on_synthetic -- --nocapture`）。数値は固定せず、ルール調整のたびに見比べる。
    #[test]
    fn baseline_on_synthetic() {
        let opts = MatchOptions::default();
        let entries = parse_corpus(SYNTHETIC).unwrap();
        let pred = predict_with_rules(&entries, &empty_master(), &opts, Lang::Japanese);
        for (title, filter) in [("全体", None), ("難問のみ", Some(true)), ("難問以外", Some(false))] {
            let s = evaluate(&entries, &pred, &opts, filter);
            println!("== {title} ==\n{}", s.report());
            if filter.is_none() {
                for m in &s.mismatches {
                    println!("  外れ{} {}  不足={:?} 余分={:?}", if m.hard { "(難)" } else { "" }, m.file_name, m.missing, m.extra);
                }
            }
        }
    }

    /// 目視用: `NEKO_CORPUS` のコーパスに対する現行ルールの一致率と外れ一覧を表示する
    /// （`NEKO_CORPUS=<file> cargo test baseline_on_file -- --ignored --nocapture`）。
    #[test]
    #[ignore]
    fn baseline_on_file() {
        let Ok(path) = std::env::var("NEKO_CORPUS") else { return };
        let opts = MatchOptions::default();
        let entries = parse_corpus(&std::fs::read_to_string(path).unwrap()).unwrap();
        let pred = predict_with_rules(&entries, &empty_master(), &opts, Lang::Japanese);
        let s = evaluate(&entries, &pred, &opts, None);
        println!("{}", s.report());
        for m in &s.mismatches {
            println!("外れ{} {}\n    不足={:?}\n    余分={:?}", if m.hard { "(難)" } else { "" }, m.file_name, m.missing, m.extra);
        }
    }

    /// 目視用: `NEKO_SCAN_DIR` のファイル名から、ラベル雛形を `NEKO_CORPUS_OUT` に書き出す
    /// （`NEKO_SCAN_DIR=<dir> NEKO_CORPUS_OUT=<file> cargo test make_corpus_template -- --ignored --nocapture`）。
    /// 出力先を省略すると標準出力に出す。出力先は `test/tag_corpus/` 以下（gitignore 済み）を推奨。
    #[test]
    #[ignore]
    fn make_corpus_template() {
        let Ok(dir) = std::env::var("NEKO_SCAN_DIR") else { return };
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        let text = template_from_names(&names, &empty_master(), &MatchOptions::default(), Lang::Japanese);
        match std::env::var("NEKO_CORPUS_OUT") {
            Ok(out) => {
                if let Some(parent) = std::path::Path::new(&out).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&out, text).unwrap();
                println!("{} 件を {out} に書いた", names.len());
            }
            Err(_) => println!("{text}"),
        }
    }
}
