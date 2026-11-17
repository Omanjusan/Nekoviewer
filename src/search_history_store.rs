//! 検索条件の履歴の保存（検索結果ではなく、条件だけを残す）。
//!
//! 履歴の行を選んだときに、条件を復元して再走査するための入口を残すのが目的。
//! - 形式: JSON Lines。1行=1条件。新しい順（先頭が最新）。新モードと旧モードは別ファイルに分け、混線させない。
//!   - `search_history_new.jsonl`    : `{"v":1,"t":保存時刻(unix秒),"text":…,"size_min":…,"size_max":…}`
//!   - `search_history_legacy.jsonl` : `{"v":1,"t":…,"base_dir":…,"name":…,"subdirs":…,"size_min":…,"size_max":…,"date_after":…,"date_before":…}`
//! - 上限はファイル（モード）ごとに `MAX_ENTRIES` 件。超えた古い行から消える。同じ条件でも、検索のたびに1行ずつ積む。
//! - 書き込みは一時ファイルへ書いて入れ替える（途中で落ちても、元のファイルは壊れない）。
//!   読み込みでは、壊れた行・知らない版の行を飛ばす（他の行は読める）。
//! - 保存はごく小さいので同期で行うが、UI を止めないよう呼び出し側は別スレッドから呼ぶ。同時に呼ばれても、
//!   ファイル単位の排他（`LOCK`）で、読み→書きが混ざらないようにしてある。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// 1ファイル（モード）あたりの保存件数の上限。
pub const MAX_ENTRIES: usize = 50;

const VERSION: u64 = 1;

/// 読み→書きを混ぜないための排他。保存はまれで小さいので、全ファイルで1本にしてある。
static LOCK: Mutex<()> = Mutex::new(());

/// 保存する条件。モードごとに持つ欄が違う。画面の入力は未パースの文字列のまま持つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    New {
        text: String,
        size_min: String,
        size_max: String,
    },
    Legacy {
        base_dir: Option<String>,
        name: String,
        subdirs: bool,
        size_min: String,
        size_max: String,
        date_after: String,
        date_before: String,
    },
}

/// どちらのファイルか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    New,
    Legacy,
}

impl Condition {
    pub fn kind(&self) -> Kind {
        match self {
            Condition::New { .. } => Kind::New,
            Condition::Legacy { .. } => Kind::Legacy,
        }
    }
}

/// 保存された1行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 保存した時刻（unix秒）。同じ秒に積まれても、ファイル内の並びが新しさの順。
    pub saved_at: i64,
    pub cond: Condition,
}

/// モードごとの保存ファイルのパス。
pub fn file_path(dir: &Path, kind: Kind) -> PathBuf {
    dir.join(match kind {
        Kind::New => "search_history_new.jsonl",
        Kind::Legacy => "search_history_legacy.jsonl",
    })
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}

fn encode(e: &Entry) -> String {
    let v = match &e.cond {
        Condition::New { text, size_min, size_max } => json!({
            "v": VERSION, "t": e.saved_at, "text": text, "size_min": size_min, "size_max": size_max,
        }),
        Condition::Legacy { base_dir, name, subdirs, size_min, size_max, date_after, date_before } => json!({
            "v": VERSION, "t": e.saved_at, "base_dir": base_dir, "name": name, "subdirs": subdirs,
            "size_min": size_min, "size_max": size_max, "date_after": date_after, "date_before": date_before,
        }),
    };
    v.to_string()
}

fn decode(kind: Kind, line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("v")?.as_u64()? != VERSION {
        return None;
    }
    let saved_at = v.get("t")?.as_i64()?;
    let cond = match kind {
        Kind::New => Condition::New { text: s(&v, "text")?, size_min: s(&v, "size_min")?, size_max: s(&v, "size_max")? },
        Kind::Legacy => Condition::Legacy {
            base_dir: match v.get("base_dir")? {
                Value::Null => None,
                other => Some(other.as_str()?.to_string()),
            },
            name: s(&v, "name")?,
            subdirs: v.get("subdirs")?.as_bool()?,
            size_min: s(&v, "size_min")?,
            size_max: s(&v, "size_max")?,
            date_after: s(&v, "date_after")?,
            date_before: s(&v, "date_before")?,
        },
    };
    Some(Entry { saved_at, cond })
}

/// 保存済みの条件を、新しい順に読む。ファイルが無い・読めない場合は空。壊れた行は飛ばし、上限を超えた分は読まない。
pub fn load(dir: &Path, kind: Kind) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(file_path(dir, kind)) else { return Vec::new() };
    text.lines().filter_map(|l| decode(kind, l)).take(MAX_ENTRIES).collect()
}

fn write_all(dir: &Path, kind: Kind, entries: &[Entry]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = file_path(dir, kind);
    let tmp = path.with_extension("jsonl.tmp");
    let mut body = String::new();
    for e in entries {
        body.push_str(&encode(e));
        body.push('\n');
    }
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// 条件を先頭（最新）に積む。`replace_saved_at` が Some なら、その時刻の行を先に消す
/// （保存済みの行を選んで再実行したときに、同じ行を重複させず先頭へ移すため）。
/// 上限を超えた古い行は消える。
pub fn push(dir: &Path, entry: Entry, replace_saved_at: Option<i64>) -> std::io::Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let kind = entry.cond.kind();
    let mut entries = load(dir, kind);
    if let Some(t) = replace_saved_at {
        if let Some(pos) = entries.iter().position(|e| e.saved_at == t && e.cond == entry.cond) {
            entries.remove(pos);
        }
    }
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
    write_all(dir, kind, &entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_search_history_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn new_cond(text: &str) -> Condition {
        Condition::New { text: text.to_string(), size_min: String::new(), size_max: String::new() }
    }

    fn legacy_cond(name: &str) -> Condition {
        Condition::Legacy {
            base_dir: Some("/data/my dir".to_string()),
            name: name.to_string(),
            subdirs: true,
            size_min: "1".to_string(),
            size_max: String::new(),
            date_after: "2025-01-02".to_string(),
            date_before: String::new(),
        }
    }

    fn entry(t: i64, cond: Condition) -> Entry {
        Entry { saved_at: t, cond }
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = temp_dir("missing");
        assert!(load(&dir, Kind::New).is_empty());
        assert!(load(&dir, Kind::Legacy).is_empty());
    }

    #[test]
    fn new_entries_go_to_the_top_and_round_trip() {
        let dir = temp_dir("order");
        push(&dir, entry(1, new_cond("first")), None).unwrap();
        push(&dir, entry(2, new_cond("second \"quoted phrase\" 全角　スペース")), None).unwrap();
        let loaded = load(&dir, Kind::New);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], entry(2, new_cond("second \"quoted phrase\" 全角　スペース")), "新しいものが先頭");
        assert_eq!(loaded[1], entry(1, new_cond("first")));
    }

    #[test]
    fn same_condition_is_stacked_every_time() {
        let dir = temp_dir("dups");
        for t in 1..=3 {
            push(&dir, entry(t, new_cond("same")), None).unwrap();
        }
        assert_eq!(load(&dir, Kind::New).len(), 3, "同じ条件でも、検索のたびに1行ずつ積む");
    }

    #[test]
    fn rotation_keeps_only_the_newest_fifty_and_drops_the_oldest() {
        let dir = temp_dir("rotation");
        for t in 1..=(MAX_ENTRIES as i64 + 7) {
            push(&dir, entry(t, new_cond(&format!("q{t}"))), None).unwrap();
        }
        let loaded = load(&dir, Kind::New);
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert_eq!(loaded[0].saved_at, MAX_ENTRIES as i64 + 7, "最新が先頭");
        assert_eq!(loaded.last().unwrap().saved_at, 8, "古い7件が消えた");
        // ファイル自体も50行
        let text = std::fs::read_to_string(file_path(&dir, Kind::New)).unwrap();
        assert_eq!(text.lines().count(), MAX_ENTRIES);
    }

    #[test]
    fn new_and_legacy_use_separate_files_and_separate_limits() {
        let dir = temp_dir("separate");
        for t in 1..=60 {
            push(&dir, entry(t, new_cond(&format!("n{t}"))), None).unwrap();
        }
        push(&dir, entry(100, legacy_cond("old.zip")), None).unwrap();
        assert_eq!(load(&dir, Kind::New).len(), MAX_ENTRIES);
        let legacy = load(&dir, Kind::Legacy);
        assert_eq!(legacy, vec![entry(100, legacy_cond("old.zip"))], "旧モードの行は新モードの上限に巻き込まれない");
        assert_ne!(file_path(&dir, Kind::New), file_path(&dir, Kind::Legacy));
    }

    #[test]
    fn legacy_round_trips_including_a_missing_base_dir() {
        let dir = temp_dir("legacy");
        let none_base = Condition::Legacy {
            base_dir: None,
            name: String::new(),
            subdirs: false,
            size_min: String::new(),
            size_max: "5".to_string(),
            date_after: String::new(),
            date_before: "2026-03-04".to_string(),
        };
        push(&dir, entry(1, legacy_cond("a")), None).unwrap();
        push(&dir, entry(2, none_base.clone()), None).unwrap();
        let loaded = load(&dir, Kind::Legacy);
        assert_eq!(loaded[0].cond, none_base);
        assert_eq!(loaded[1].cond, legacy_cond("a"));
    }

    #[test]
    fn broken_lines_and_unknown_versions_are_skipped_without_losing_the_rest() {
        let dir = temp_dir("broken");
        let good = encode(&entry(5, new_cond("good")));
        let body = format!("not json\n{{\"v\":99,\"t\":1,\"text\":\"future\",\"size_min\":\"\",\"size_max\":\"\"}}\n{good}\n{{\"v\":1,\"t\":2}}\n\n");
        std::fs::write(file_path(&dir, Kind::New), body).unwrap();
        assert_eq!(load(&dir, Kind::New), vec![entry(5, new_cond("good"))]);
        // 壊れた行があっても、続けて積める（壊れた行はこのとき整理される）
        push(&dir, entry(6, new_cond("next")), None).unwrap();
        let loaded = load(&dir, Kind::New);
        assert_eq!(loaded.iter().map(|e| e.saved_at).collect::<Vec<_>>(), vec![6, 5]);
    }

    #[test]
    fn replacing_a_stored_row_moves_it_to_the_top_without_a_duplicate() {
        let dir = temp_dir("replace");
        push(&dir, entry(1, new_cond("a")), None).unwrap();
        push(&dir, entry(2, new_cond("b")), None).unwrap();
        // 保存済みの「a」(t=1) を選んで再実行 → 古い a を消して、新しい時刻で先頭へ
        push(&dir, entry(3, new_cond("a")), Some(1)).unwrap();
        let loaded = load(&dir, Kind::New);
        assert_eq!(loaded, vec![entry(3, new_cond("a")), entry(2, new_cond("b"))]);
    }

    #[test]
    fn failed_write_leaves_the_existing_file_intact() {
        let dir = temp_dir("intact");
        push(&dir, entry(1, new_cond("keep")), None).unwrap();
        // 一時ファイルの場所をフォルダで塞いで、書き込みを失敗させる
        std::fs::create_dir_all(dir.join("search_history_new.jsonl.tmp")).unwrap();
        assert!(push(&dir, entry(2, new_cond("lost")), None).is_err());
        assert_eq!(load(&dir, Kind::New), vec![entry(1, new_cond("keep"))], "失敗しても元のファイルは無傷");
    }

    #[test]
    fn concurrent_pushes_do_not_lose_rows() {
        let dir = temp_dir("concurrent");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for j in 0..5 {
                        push(&dir, entry(i * 10 + j, new_cond(&format!("t{i}_{j}"))), None).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(load(&dir, Kind::New).len(), 40, "同時に積んでも、読み→書きが混ざって行を失わない");
    }
}
