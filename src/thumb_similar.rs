//! サムネのdHashによる「似ている画像」の照合。「？」マーカーと似ている画像ビューの元データを作る。
//!
//! - 相手は、これまでに開いてハッシュが保存されたファイル全て（`thumb_hash_store`）。同じフォルダ内の相手も含む
//! - 距離が `max_distance_for_pct(MIN_PCT)` 以下の組を、距離付きで返す。UIのしきい値（70〜100%）での絞り込みは
//!   表示側が行う（スライダーを動かしても照合し直さない）。件数の上限はない
//! - 除く相手: FPが一致するファイル（重複マーカーの担当）、「正常として消した」ペア、
//!   消失・オフラインのファイル（現存する相手だけを根拠にする）、自分と同じパスの古い記録
//! - 結果は保存しない（派生情報）。フォルダを開くたび・サムネ生成が一段落するたびに作り直す

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{self, FileRecord, Presence};
use crate::thumb_hash::{self, HASH_VERSION};
use crate::thumb_hash_store::{self, HashRow};

/// UIのしきい値の下限(%)。照合はこの類似度までを集める。
pub const MIN_PCT: u8 = 70;

/// 似ている相手1件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimilarMatch {
    /// 相手のファイルID。
    pub other: u64,
    /// ハミング距離（0〜`max_distance_for_pct(MIN_PCT)`）。
    pub distance: u8,
}

impl SimilarMatch {
    /// 類似度(%)。
    pub fn pct(&self) -> u8 {
        thumb_hash::similarity_pct(u32::from(self.distance))
    }
}

/// 距離順の相手 `matches` のうち、類似度が `min_pct` 以上のものの (いちばん近い相手の類似度(%), 件数)。
/// 1件も満たさなければ None。「？」のホバー説明と、出す・出さないの判定に使う。
pub fn summarize(matches: &[SimilarMatch], min_pct: u8) -> Option<(u8, usize)> {
    let pct = matches.first()?.pct();
    (pct >= min_pct).then(|| (pct, matches.iter().take_while(|m| m.pct() >= min_pct).count()))
}

/// `subjects` の各ハッシュに対して、`universe` の中で距離が `max_distance` 以下のもの（自分自身を除く）。
/// 戻り値は (対象ID, 相手ID, 距離)。総当たり。
pub fn candidate_pairs(subjects: &[(u64, u64)], universe: &[(u64, u64)], max_distance: u32) -> Vec<(u64, u64, u8)> {
    let mut out = Vec::new();
    for &(sid, shash) in subjects {
        for &(oid, ohash) in universe {
            if sid == oid {
                continue;
            }
            let d = thumb_hash::distance(shash, ohash);
            if d <= max_distance {
                out.push((sid, oid, d as u8));
            }
        }
    }
    out
}

/// 相手の現存確認の結果を、短い期限つきでメモリに持つ（依頼をまたいで使い回す）。
/// 期限の間は、相手が消えても「現存」のままになる。最後の置き換えの時に期限が過ぎていれば直る。
pub struct PresenceCache {
    entries: HashMap<u64, (Instant, bool)>,
    ttl: Duration,
}

/// 現存確認の結果を持つ期間。
pub const PRESENCE_TTL: Duration = Duration::from_secs(30);

impl PresenceCache {
    pub fn new(ttl: Duration) -> Self {
        Self { entries: HashMap::new(), ttl }
    }

    /// `id` の相手が現存するか。期限内の結果があればそれを返し、無ければ `check` で確認して持つ。
    fn is_present(&mut self, id: u64, now: Instant, check: impl FnOnce() -> bool) -> bool {
        if let Some(&(at, present)) = self.entries.get(&id)
            && now.saturating_duration_since(at) < self.ttl
        {
            return present;
        }
        let present = check();
        self.entries.insert(id, (now, present));
        present
    }
}

/// フォルダ `dir` の `names` について、似ている相手を集める。キーはファイル名。相手が無いファイルは含めない。
/// 保存済みのハッシュと消したペアを毎回読み、現存確認の結果は持ち越さない版（テスト用。本番は `scan_dir`）。
///
/// - `probe` は相手の現存確認（DBロックの外で呼ぶ）
/// - `keep_going` が false になったら打ち切り、そこまでの結果を返す
#[cfg(test)]
pub fn match_dir(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    names: &[String],
    probe: &dyn Fn(&FileRecord) -> Presence,
    keep_going: &dyn Fn() -> bool,
) -> HashMap<String, Vec<SimilarMatch>> {
    let rows = thumb_hash_store::all_rows(db);
    let dismissed = thumb_hash_store::dismissed_pairs(db);
    let mut presence = PresenceCache::new(Duration::ZERO);
    match_names(db, dir, names, &rows, &dismissed, &mut presence, Instant::now(), probe, keep_going)
}

/// `match_dir` の本体。ハッシュの表 `rows`、消したペア `dismissed`、現存確認の結果 `presence` は呼び出し側が持つ。
/// `names` の一部（チャンク）だけを渡してもよい。相手は `rows` の全て。
#[allow(clippy::too_many_arguments)]
pub fn match_names(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    names: &[String],
    rows: &HashMap<u64, HashRow>,
    dismissed: &HashSet<(u64, u64)>,
    presence: &mut PresenceCache,
    now: Instant,
    probe: &dyn Fn(&FileRecord) -> Presence,
    keep_going: &dyn Fn() -> bool,
) -> HashMap<String, Vec<SimilarMatch>> {
    let hashed = |id: u64| rows.get(&id).filter(|r| r.version == HASH_VERSION).and_then(|r| r.hash);
    let prefix = crate::spread_state::dir_prefix(dir);

    // 対象: このフォルダのファイルのうち、ハッシュがあるもの。
    let mut subjects: Vec<(u64, u64)> = Vec::new();
    let mut name_of: HashMap<u64, &str> = HashMap::new();
    for name in names {
        let Some(rec) = file_identity::lookup(db, &format!("{prefix}{name}")) else { continue };
        if let Some(hash) = hashed(rec.id) {
            subjects.push((rec.id, hash));
            name_of.insert(rec.id, name);
        }
    }
    if subjects.is_empty() {
        return HashMap::new();
    }
    let universe: Vec<(u64, u64)> = rows.keys().filter_map(|&id| hashed(id).map(|h| (id, h))).collect();
    let pairs: Vec<(u64, u64, u8)> =
        candidate_pairs(&subjects, &universe, thumb_hash::max_distance_for_pct(MIN_PCT))
            .into_iter()
            .filter(|(s, o, _)| !dismissed.contains(&thumb_hash_store::pair(*s, *o)))
            .collect();
    if pairs.is_empty() {
        return HashMap::new();
    }

    // 関係するレコードを、ロックの中でまとめて読む。
    let ids: HashSet<u64> = pairs.iter().flat_map(|(s, o, _)| [*s, *o]).collect();
    let records: HashMap<u64, FileRecord> = {
        let Ok(guard) = db.lock() else { return HashMap::new() };
        let Ok(tx) = guard.begin_read() else { return HashMap::new() };
        ids.iter().filter_map(|id| file_identity::record_by_id_tx(&tx, *id).map(|r| (*id, r))).collect()
    };

    // ここからロックの外。相手の現存を確認しながら絞る（相手ごとに1回。期限内の結果は使い回す）。
    let mut out: HashMap<String, Vec<SimilarMatch>> = HashMap::new();
    for (sid, oid, distance) in pairs {
        if !keep_going() {
            break;
        }
        let (Some(subject), Some(other)) = (records.get(&sid), records.get(&oid)) else { continue };
        if other.path_key == subject.path_key {
            continue;
        }
        // FP一致は重複マーカーの担当。
        if subject.fp.is_some() && subject.fp == other.fp {
            continue;
        }
        if !presence.is_present(oid, now, || probe(other) == Presence::Present) {
            continue;
        }
        out.entry(name_of[&sid].to_owned()).or_default().push(SimilarMatch { other: oid, distance });
    }
    for matches in out.values_mut() {
        matches.sort_by_key(|m| (m.distance, m.other));
    }
    out
}

/// 1回の走査（フォルダ1つぶん）で返すもの。
#[derive(Debug, PartialEq, Eq)]
pub enum ScanEvent {
    /// 1チャンクぶんの結果。そのチャンクの名前だけを含む（相手が無い名前は含まない）。足し込んで使う。
    Partial(HashMap<String, Vec<SimilarMatch>>),
    /// 走査の最後に全名前をやり直した結果。これまでの結果を置き換える
    /// （後のチャンクでハッシュができた相手との組が、前のチャンクでは取りこぼされるため）。
    Final(HashMap<String, Vec<SimilarMatch>>),
}

/// 1チャンクの名前の数。
pub const SCAN_CHUNK: usize = 100;

/// 依頼をまたいで持つ、走査の状態。ハッシュの表は最初の走査で一度だけDBから読み、あとは書いた分を足していく。
pub struct ScanState {
    rows: Option<HashMap<u64, HashRow>>,
    pub presence: PresenceCache,
}

impl ScanState {
    pub fn new() -> Self {
        Self { rows: None, presence: PresenceCache::new(PRESENCE_TTL) }
    }
}

/// フォルダ `dir` の `names` を `chunk` 件ずつ、ハッシュの計算 → 照合 → `emit(Partial)` の順で処理し、
/// 最後に全名前をやり直して `emit(Final)` する。`keep_going` が false になったら、そこで打ち切る（Final は出さない）。
/// 結果が空のチャンクは Partial を出さない。
#[allow(clippy::too_many_arguments)]
pub fn scan_dir(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    names: &[String],
    chunk: usize,
    state: &mut ScanState,
    read_thumb: &dyn Fn(&str) -> Option<(i64, Vec<u8>)>,
    probe: &dyn Fn(&FileRecord) -> Presence,
    keep_going: &dyn Fn() -> bool,
    emit: &mut dyn FnMut(ScanEvent),
) {
    let dismissed = thumb_hash_store::dismissed_pairs(db);
    let rows = state.rows.get_or_insert_with(|| thumb_hash_store::all_rows(db));
    let now = Instant::now();
    for part in names.chunks(chunk.max(1)) {
        if !keep_going() {
            return;
        }
        let written = thumb_hash_store::hash_files(db, dir, part, read_thumb, keep_going, Some(rows));
        rows.extend(written);
        let matches = match_names(db, dir, part, rows, &dismissed, &mut state.presence, now, probe, keep_going);
        if !keep_going() {
            return;
        }
        if !matches.is_empty() {
            emit(ScanEvent::Partial(matches));
        }
    }
    // 最後に、全名前を現在のハッシュの表でやり直す。
    let all = match_names(db, dir, names, rows, &dismissed, &mut state.presence, now, probe, keep_going);
    if keep_going() {
        emit(ScanEvent::Final(all));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::spread_state::open_spread_db;
    use crate::thumb_hash_store::{HashRow, put_all};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root =
                std::env::temp_dir().join(format!("nekoviewer_thumb_similar_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn sub(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::create_dir_all(&p).unwrap();
            p
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 内容が `fill` で決まるファイルを登録する（同じ `fill` ならFPも同じ）。
    fn register(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, fill: u8) -> u64 {
        std::fs::write(dir.join(name), vec![fill; 64]).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        file_identity::ensure_record(db, dir, name).unwrap().id
    }

    fn set_hash(db: &Arc<Mutex<Database>>, id: u64, hash: Option<u64>) {
        assert!(put_all(db, &[(id, HashRow { version: HASH_VERSION, thumb_mtime: 1, hash })]));
    }

    /// 下位 `bits` ビットだけが違うハッシュ（基準 0 からの距離が `bits`）。
    fn bits(n: u32) -> u64 {
        if n == 0 { 0 } else { u64::MAX >> (64 - n) }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    fn present(_: &FileRecord) -> Presence {
        Presence::Present
    }

    fn run(db: &Arc<Mutex<Database>>, dir: &Path, list: &[&str]) -> HashMap<String, Vec<SimilarMatch>> {
        match_dir(db, dir, &names(list), &present, &|| true)
    }

    #[test]
    fn candidate_pairs_include_the_boundary_and_skip_self() {
        let max = thumb_hash::max_distance_for_pct(MIN_PCT);
        assert_eq!(max, 19);
        let subjects = [(1, 0_u64)];
        let universe = [(1, 0_u64), (2, bits(19)), (3, bits(20)), (4, 0)];
        let got = candidate_pairs(&subjects, &universe, max);
        assert_eq!(got, vec![(1, 2, 19), (1, 4, 0)]);
    }

    #[test]
    fn summarize_counts_only_partners_at_or_above_the_threshold() {
        let m = |distance: u8| SimilarMatch { other: u64::from(distance), distance };
        // 距離 0 / 3 / 6 / 13 / 19 は、類似度 100 / 95 / 91 / 80 / 70 %。
        let list = [m(0), m(3), m(6), m(13), m(19)];
        assert_eq!(summarize(&list, 100), Some((100, 1)));
        assert_eq!(summarize(&list, 90), Some((100, 3)));
        assert_eq!(summarize(&list, 80), Some((100, 4)));
        assert_eq!(summarize(&list, 70), Some((100, 5)));
        assert_eq!(summarize(&[m(3), m(6)], 100), None, "最も近い相手が届かなければ出さない");
        assert_eq!(summarize(&[], 70), None);
    }

    #[test]
    fn files_in_the_same_folder_match_each_other_with_the_distance() {
        let t = TempRoot::new("same");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        set_hash(&db, a, Some(0));
        set_hash(&db, b, Some(bits(3)));
        let got = run(&db, &t.0, &["a.zip", "b.zip"]);
        assert_eq!(got["a.zip"], vec![SimilarMatch { other: b, distance: 3 }]);
        assert_eq!(got["b.zip"], vec![SimilarMatch { other: a, distance: 3 }], "双方に付く");
        assert_eq!(got["a.zip"][0].pct(), 95);
    }

    #[test]
    fn a_file_in_another_folder_is_a_partner_but_is_not_reported_itself() {
        let t = TempRoot::new("cross");
        let db = open_spread_db(&t.0).unwrap();
        let (d1, d2) = (t.sub("d1"), t.sub("d2"));
        let (a, b) = (register(&db, &d1, "a.zip", 1), register(&db, &d2, "b.zip", 2));
        set_hash(&db, a, Some(0));
        set_hash(&db, b, Some(bits(8)));
        let got = run(&db, &d1, &["a.zip"]);
        assert_eq!(got.keys().collect::<Vec<_>>(), vec!["a.zip"]);
        assert_eq!(got["a.zip"], vec![SimilarMatch { other: b, distance: 8 }]);
    }

    #[test]
    fn matches_are_sorted_by_distance_and_have_no_cap() {
        let t = TempRoot::new("sorted");
        let db = open_spread_db(&t.0).unwrap();
        let a = register(&db, &t.0, "a.zip", 0);
        set_hash(&db, a, Some(0));
        let mut expected = Vec::new();
        for i in 1..=40_u8 {
            let id = register(&db, &t.0, &format!("p{i:02}.zip"), i);
            let d = 19 - u32::from(i % 20).min(19);
            set_hash(&db, id, Some(bits(d)));
            expected.push((d as u8, id));
        }
        expected.sort();
        let got = run(&db, &t.0, &["a.zip"]);
        let got: Vec<(u8, u64)> = got["a.zip"].iter().map(|m| (m.distance, m.other)).collect();
        assert_eq!(got.len(), 40, "件数の上限なし");
        assert_eq!(got, expected);
    }

    #[test]
    fn too_far_or_unhashable_files_do_not_match() {
        let t = TempRoot::new("far");
        let db = open_spread_db(&t.0).unwrap();
        let a = register(&db, &t.0, "a.zip", 1);
        let far = register(&db, &t.0, "far.zip", 2);
        let flat = register(&db, &t.0, "flat.zip", 3);
        set_hash(&db, a, Some(0));
        set_hash(&db, far, Some(bits(20)));
        set_hash(&db, flat, None);
        assert!(run(&db, &t.0, &["a.zip", "far.zip", "flat.zip"]).get("a.zip").is_none());
    }

    #[test]
    fn rows_of_another_hash_version_are_ignored() {
        let t = TempRoot::new("version");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        set_hash(&db, a, Some(0));
        assert!(put_all(&db, &[(b, HashRow { version: HASH_VERSION + 1, thumb_mtime: 1, hash: Some(0) })]));
        assert!(run(&db, &t.0, &["a.zip", "b.zip"]).is_empty());
    }

    #[test]
    fn files_with_the_same_fingerprint_are_left_to_the_duplicate_marker() {
        let t = TempRoot::new("fp");
        let db = open_spread_db(&t.0).unwrap();
        let (d1, d2) = (t.sub("d1"), t.sub("d2"));
        let (a, same) = (register(&db, &d1, "a.zip", 7), register(&db, &d2, "copy.zip", 7));
        let other = register(&db, &d2, "other.zip", 8);
        assert_eq!(file_identity::lookup(&db, &format!("{}copy.zip", crate::spread_state::dir_prefix(&d2))).unwrap().fp,
            file_identity::lookup(&db, &format!("{}a.zip", crate::spread_state::dir_prefix(&d1))).unwrap().fp);
        for id in [a, same, other] {
            set_hash(&db, id, Some(0));
        }
        let got = run(&db, &d1, &["a.zip"]);
        assert_eq!(got["a.zip"], vec![SimilarMatch { other, distance: 0 }], "FP一致のcopy.zipは除く");
    }

    #[test]
    fn dismissed_pairs_are_excluded_in_both_directions_only_for_that_pair() {
        let t = TempRoot::new("dismissed");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b, c) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2), register(&db, &t.0, "c.zip", 3));
        for id in [a, b, c] {
            set_hash(&db, id, Some(0));
        }
        assert!(thumb_hash_store::dismiss_pair(&db, b, a, 100), "順序に依らない");
        let got = run(&db, &t.0, &["a.zip", "b.zip", "c.zip"]);
        assert_eq!(got["a.zip"], vec![SimilarMatch { other: c, distance: 0 }]);
        assert_eq!(got["b.zip"], vec![SimilarMatch { other: c, distance: 0 }]);
        assert_eq!(got["c.zip"].len(), 2);
    }

    #[test]
    fn missing_and_offline_partners_are_not_counted() {
        let t = TempRoot::new("presence");
        let db = open_spread_db(&t.0).unwrap();
        let (a, gone, off, ok) = (
            register(&db, &t.0, "a.zip", 1),
            register(&db, &t.0, "gone.zip", 2),
            register(&db, &t.0, "off.zip", 3),
            register(&db, &t.0, "ok.zip", 4),
        );
        for id in [a, gone, off, ok] {
            set_hash(&db, id, Some(0));
        }
        let probe = move |rec: &FileRecord| match rec.id {
            id if id == gone => Presence::Missing,
            id if id == off => Presence::Offline,
            _ => Presence::Present,
        };
        let got = match_dir(&db, &t.0, &names(&["a.zip"]), &probe, &|| true);
        assert_eq!(got["a.zip"], vec![SimilarMatch { other: ok, distance: 0 }]);
    }

    #[test]
    fn stops_when_asked() {
        let t = TempRoot::new("stop");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        set_hash(&db, a, Some(0));
        set_hash(&db, b, Some(0));
        let got = match_dir(&db, &t.0, &names(&["a.zip"]), &present, &|| false);
        assert!(got.is_empty());
    }

    #[test]
    fn dismissal_table_follows_id_deletion_and_is_not_created_by_reads() {
        let t = TempRoot::new("table");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b, c) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2), register(&db, &t.0, "c.zip", 3));
        assert!(thumb_hash_store::dismissed_pairs(&db).is_empty());
        file_identity::delete_id(&db, c).unwrap();
        {
            use redb::TableHandle;
            let guard = db.lock().unwrap();
            let tx = guard.begin_write().unwrap();
            assert!(!tx.list_tables().unwrap().any(|h| h.name().starts_with("thumb_")), "読みと連動削除は表を作らない");
        }
        assert!(thumb_hash_store::dismiss_pair(&db, a, b, 100));
        assert!(thumb_hash_store::dismiss_pair(&db, a, b, 100), "二重記録しても1組");
        assert_eq!(thumb_hash_store::dismissed_pairs(&db), HashSet::from([(a.min(b), a.max(b))]));
        file_identity::delete_id(&db, b).unwrap();
        assert!(thumb_hash_store::dismissed_pairs(&db).is_empty(), "IDが消えたらペアも消える");
    }

    #[test]
    fn presence_cache_reuses_a_fresh_answer_and_rechecks_after_the_ttl() {
        let mut cache = PresenceCache::new(Duration::from_secs(30));
        let t0 = Instant::now();
        let calls = std::cell::Cell::new(0);
        let mut ask = |cache: &mut PresenceCache, now: Instant, answer: bool| {
            cache.is_present(7, now, || {
                calls.set(calls.get() + 1);
                answer
            })
        };
        assert!(ask(&mut cache, t0, true));
        assert!(ask(&mut cache, t0 + Duration::from_secs(29), false), "期限内は前の答えのまま");
        assert_eq!(calls.get(), 1);
        assert!(!ask(&mut cache, t0 + Duration::from_secs(31), false), "期限が過ぎたら確認し直す");
        assert_eq!(calls.get(), 2);
        let mut none = PresenceCache::new(Duration::ZERO);
        none.is_present(1, t0, || true);
        none.is_present(1, t0, || true);
        // 期限0は常に確認する。
        assert_eq!(none.entries.len(), 1);
    }

    /// `scan_dir` を実行して、出たイベントを集める。
    fn scan(
        db: &Arc<Mutex<Database>>,
        dir: &Path,
        list: &[&str],
        chunk: usize,
        state: &mut ScanState,
        thumbs: &HashMap<String, (i64, Vec<u8>)>,
        keep_going: &dyn Fn() -> bool,
    ) -> Vec<ScanEvent> {
        let mut events = Vec::new();
        scan_dir(db, dir, &names(list), chunk, state, &|n| thumbs.get(n).cloned(), &present, keep_going, &mut |e| {
            events.push(e)
        });
        events
    }

    /// 構図のあるJPEG（`seed` で模様が変わる）。
    fn jpeg(seed: u8) -> Vec<u8> {
        let (w, h) = (96_u32, 64_u32);
        let raw: Vec<u8> =
            (0..w * h).map(|i| ((i % w / 12) * 37 + (i / w / 8) * 91 + u32::from(seed) * 53) as u8).collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode(&raw, w, h, image::ExtendedColorType::L8)
            .unwrap();
        out
    }

    #[test]
    fn scan_emits_partials_per_chunk_then_a_final_that_equals_a_one_shot_match() {
        let t = TempRoot::new("scan");
        let db = open_spread_db(&t.0).unwrap();
        let list = ["a.zip", "b.zip", "c.zip", "d.zip"];
        let mut thumbs = HashMap::new();
        for (i, name) in list.iter().enumerate() {
            register(&db, &t.0, name, i as u8 + 1);
            // 全部同じ絵 → 全員が互いに距離0。
            thumbs.insert((*name).to_owned(), (100, jpeg(1)));
        }
        let mut state = ScanState::new();
        let events = scan(&db, &t.0, &list, 2, &mut state, &thumbs, &|| true);
        assert_eq!(events.len(), 3, "チャンク2つ + 最後の置き換え: {events:?}");
        let ScanEvent::Partial(first) = &events[0] else { panic!("{events:?}") };
        let ScanEvent::Partial(second) = &events[1] else { panic!("{events:?}") };
        let ScanEvent::Final(last) = &events[2] else { panic!("{events:?}") };
        assert_eq!(first.keys().collect::<HashSet<_>>(), HashSet::from([&"a.zip".to_owned(), &"b.zip".to_owned()]));
        assert_eq!(second.keys().collect::<HashSet<_>>(), HashSet::from([&"c.zip".to_owned(), &"d.zip".to_owned()]));
        // 最初のチャンクの時点では、まだ後ろのファイルのハッシュが無いので、相手は同じチャンクの1件だけ。
        assert_eq!(first["a.zip"].len(), 1);
        // 最後の置き換えで、全員が他の3件を相手に持つ（取りこぼしが直る）。
        for name in list {
            assert_eq!(last[name].len(), 3, "{name}");
        }
        assert_eq!(*last, match_dir(&db, &t.0, &names(&list), &present, &|| true), "一括の照合と一致する");
    }

    #[test]
    fn a_second_scan_reuses_the_in_memory_rows_and_hashes_nothing_again() {
        let t = TempRoot::new("rescan");
        let db = open_spread_db(&t.0).unwrap();
        let (a, b) = (register(&db, &t.0, "a.zip", 1), register(&db, &t.0, "b.zip", 2));
        let thumbs = HashMap::from([("a.zip".to_owned(), (100, jpeg(1))), ("b.zip".to_owned(), (100, jpeg(1)))]);
        let mut state = ScanState::new();
        scan(&db, &t.0, &["a.zip", "b.zip"], 100, &mut state, &thumbs, &|| true);
        let reads = std::cell::Cell::new(0);
        let counting = |n: &str| {
            reads.set(reads.get() + 1);
            thumbs.get(n).cloned()
        };
        let mut events = Vec::new();
        scan_dir(&db, &t.0, &names(&["a.zip", "b.zip"]), 100, &mut state, &counting, &present, &|| true, &mut |e| events.push(e));
        // サムネは読む（更新日時の確認のため）が、計算済みなので行は書き直さない。
        assert_eq!(thumb_hash_store::get(&db, a).unwrap().thumb_mtime, 100);
        assert!(matches!(events.last(), Some(ScanEvent::Final(m)) if m["a.zip"][0].other == b));
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn a_cancelled_scan_emits_no_final() {
        let t = TempRoot::new("cancel");
        let db = open_spread_db(&t.0).unwrap();
        for (i, n) in ["a.zip", "b.zip", "c.zip"].iter().enumerate() {
            register(&db, &t.0, n, i as u8 + 1);
        }
        let thumbs: HashMap<String, (i64, Vec<u8>)> =
            ["a.zip", "b.zip", "c.zip"].iter().map(|n| ((*n).to_owned(), (100, jpeg(1)))).collect();
        let calls = std::cell::Cell::new(0);
        let stop_after_first_chunk = || {
            calls.set(calls.get() + 1);
            calls.get() <= 3
        };
        let mut state = ScanState::new();
        let events = scan(&db, &t.0, &["a.zip", "b.zip", "c.zip"], 1, &mut state, &thumbs, &stop_after_first_chunk);
        assert!(events.iter().all(|e| matches!(e, ScanEvent::Partial(_))), "{events:?}");
        assert!(!events.is_empty() || calls.get() > 3);
    }

    #[test]
    fn files_without_a_thumbnail_yield_no_events_until_one_exists() {
        let t = TempRoot::new("nothumb");
        let db = open_spread_db(&t.0).unwrap();
        register(&db, &t.0, "a.zip", 1);
        register(&db, &t.0, "b.zip", 2);
        let mut state = ScanState::new();
        let events = scan(&db, &t.0, &["a.zip", "b.zip"], 100, &mut state, &HashMap::new(), &|| true);
        assert_eq!(events, vec![ScanEvent::Final(HashMap::new())]);
    }
}
