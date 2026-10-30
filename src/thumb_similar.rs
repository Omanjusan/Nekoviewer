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

use redb::{Database, ReadableDatabase};

use crate::file_identity::{self, FileRecord, Presence};
use crate::thumb_hash::{self, HASH_VERSION};
use crate::thumb_hash_store;

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

/// フォルダ `dir` の `names` について、似ている相手を集める。キーはファイル名。相手が無いファイルは含めない。
///
/// - `probe` は相手の現存確認（DBロックの外で呼ぶ）
/// - `keep_going` が false になったら打ち切り、そこまでの結果を返す
pub fn match_dir(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    names: &[String],
    probe: &dyn Fn(&FileRecord) -> Presence,
    keep_going: &dyn Fn() -> bool,
) -> HashMap<String, Vec<SimilarMatch>> {
    let rows = thumb_hash_store::all_rows(db);
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
    let dismissed = thumb_hash_store::dismissed_pairs(db);
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

    // ここからロックの外。相手の現存を確認しながら絞る（相手ごとに1回）。
    let mut presence: HashMap<u64, bool> = HashMap::new();
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
        let present = *presence.entry(oid).or_insert_with(|| probe(other) == Presence::Present);
        if !present {
            continue;
        }
        out.entry(name_of[&sid].to_owned()).or_default().push(SimilarMatch { other: oid, distance });
    }
    for matches in out.values_mut() {
        matches.sort_by_key(|m| (m.distance, m.other));
    }
    out
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
        assert!(thumb_hash_store::dismiss_pair(&db, b, a), "順序に依らない");
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
        assert!(thumb_hash_store::dismiss_pair(&db, a, b));
        assert!(thumb_hash_store::dismiss_pair(&db, a, b), "二重記録しても1組");
        assert_eq!(thumb_hash_store::dismissed_pairs(&db), HashSet::from([(a.min(b), a.max(b))]));
        file_identity::delete_id(&db, b).unwrap();
        assert!(thumb_hash_store::dismissed_pairs(&db).is_empty(), "IDが消えたらペアも消える");
    }
}
