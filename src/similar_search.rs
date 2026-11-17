//! 検索結果（新モード）の「？」（サムネが似ている）を、**保存済みのハッシュだけ**で照合する。
//!
//! フォルダ表示の照合（[thumb_similar.rs](crate::thumb_similar)）との違い:
//! - 新しくハッシュを計算しない。保存済みのハッシュ（`thumb_hash_store`）が無いファイルは、判定できないので何も出さない。
//! - 相手の現存確認は stat ではなく、台帳の記録（前回の確認で見つからなかった相手は除く）で済ませる。I/O が要らない。
//! - 対象は呼び出し側が決める（画面に出ている行だけ）。相手は、保存済みのハッシュの全件。
//! - 画面のしきい値に対応する距離までを、最初から絞って集める。70%まで全部集めると、無関係な相手が偶然
//!   （ハッシュの総数が50万なら、1ファイルあたり約400件）混ざって、後の絞り込みが重くなるため。
//! - 1ファイルあたりの相手は、近い順に `MAX_PARTNERS` 件まで。
//!
//! 計算量: 対象数 × ハッシュの総数 の XOR と bit 数え。実測（最適化ビルド・1スレッド）で、対象50件 × 100万件が約55ms。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{self, FileStatus};
use crate::thumb_hash::{self, HASH_VERSION};
use crate::thumb_hash_store::{self, HashRow};
use crate::thumb_similar::SimilarMatch;

/// 1ファイルあたりに返す相手の上限（近い順）。
pub const MAX_PARTNERS: usize = 20;

/// 保存済みのハッシュの表。セッション中は持ち回って使う（読み込みは1回につき DB 全件）。
pub struct SimilarTable {
    /// 現在の版のハッシュを持つ（ID, ハッシュ）。照合の相手の全体。
    hashes: Vec<(u64, u64)>,
    by_id: HashMap<u64, u64>,
    /// 「正常として消した」ペア（小さいID, 大きいID）。
    dismissed: HashSet<(u64, u64)>,
}

impl SimilarTable {
    /// 保存済みの行から作る。版が違う行・ハッシュを作れなかった行は除く。
    pub fn from_rows(rows: &HashMap<u64, HashRow>, dismissed: HashSet<(u64, u64)>) -> Self {
        let mut hashes: Vec<(u64, u64)> = rows
            .iter()
            .filter(|(_, r)| r.version == HASH_VERSION)
            .filter_map(|(&id, r)| r.hash.map(|h| (id, h)))
            .collect();
        hashes.sort_unstable();
        let by_id = hashes.iter().copied().collect();
        Self { hashes, by_id, dismissed }
    }

    /// DB から全件読む（重いので、専用スレッドで呼ぶ）。
    pub fn load(db: &Arc<Mutex<Database>>) -> Self {
        Self::from_rows(&thumb_hash_store::all_rows(db), thumb_hash_store::dismissed_pairs(db))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// そのファイルのハッシュ（無ければ None＝判定できない）。
    pub fn hash_of(&self, id: u64) -> Option<u64> {
        self.by_id.get(&id).copied()
    }

    /// `subject` のハッシュに対して、距離が `max_distance` 以下の相手（自分自身を除く）を、近い順（同じ距離なら小さいID）で
    /// 最大 `limit` 件。総当たり。
    pub fn nearest(&self, subject: u64, max_distance: u32, limit: usize) -> Vec<(u64, u8)> {
        let Some(shash) = self.hash_of(subject) else { return Vec::new() };
        let mut found: Vec<(u64, u8)> = Vec::new();
        for &(oid, ohash) in &self.hashes {
            if oid == subject {
                continue;
            }
            let d = thumb_hash::distance(shash, ohash);
            if d <= max_distance {
                found.push((oid, d as u8));
                // 偶然の相手が大量にあっても溜め込まないよう、時々近い順に刈る。
                if found.len() >= limit.saturating_mul(8).max(64) {
                    found.sort_unstable_by_key(|&(id, d)| (d, id));
                    found.truncate(limit);
                }
            }
        }
        found.sort_unstable_by_key(|&(id, d)| (d, id));
        found.truncate(limit);
        found
    }
}

/// `subjects` の各ファイルについて、似ている相手を集める。戻りは（対象のID, 相手）。ハッシュが無い対象や、
/// 相手が1件も無い対象は含めない。`keep_going` が false になったら、そこまでの結果を返す。
///
/// 相手から除くもの: 同じパスの古い記録／FPが一致するもの（重複マーカーの担当）／前回の確認で見つからなかったもの。
/// 台帳のレコードは、DB を1回だけ読んでまとめて引く（ロックは短く）。
pub fn find_similar(
    db: &Arc<Mutex<Database>>,
    table: &SimilarTable,
    subjects: &[u64],
    max_distance: u32,
    keep_going: &dyn Fn() -> bool,
) -> Vec<(u64, Vec<SimilarMatch>)> {
    // 先に、ハッシュだけで候補を集める（DB に触れない）。フィルタで落ちる分を見込んで、上限の数倍まで持つ。
    let mut cands: Vec<(u64, Vec<(u64, u8)>)> = Vec::new();
    for &sid in subjects {
        if !keep_going() {
            return Vec::new();
        }
        let near = table.nearest(sid, max_distance, MAX_PARTNERS * 3);
        if !near.is_empty() {
            cands.push((sid, near));
        }
    }
    if cands.is_empty() {
        return Vec::new();
    }
    // 関係するレコードを、1回のトランザクションでまとめて読む。
    let ids: HashSet<u64> = cands.iter().flat_map(|(s, near)| std::iter::once(*s).chain(near.iter().map(|n| n.0))).collect();
    let records: HashMap<u64, file_identity::FileRecord> = {
        let Ok(guard) = db.lock() else { return Vec::new() };
        let Ok(tx) = guard.begin_read() else { return Vec::new() };
        ids.iter().filter_map(|&id| file_identity::record_by_id_tx(&tx, id).map(|r| (id, r))).collect()
    };
    let mut out = Vec::new();
    for (sid, near) in cands {
        let Some(subject) = records.get(&sid) else { continue };
        let mut matches: Vec<SimilarMatch> = Vec::new();
        for (oid, distance) in near {
            let Some(other) = records.get(&oid) else { continue };
            if other.path_key == subject.path_key
                || (subject.fp.is_some() && subject.fp == other.fp)
                || other.status != FileStatus::Confirmed
            {
                continue;
            }
            matches.push(SimilarMatch {
                other: oid,
                distance,
                dismissed: table.dismissed.contains(&thumb_hash_store::pair(sid, oid)),
            });
            if matches.len() >= MAX_PARTNERS {
                break;
            }
        }
        if !matches.is_empty() {
            out.push((sid, matches));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spread_state::open_spread_db;
    use std::path::{Path, PathBuf};

    fn rows(items: &[(u64, Option<u64>, u8)]) -> HashMap<u64, HashRow> {
        items.iter().map(|&(id, hash, version)| (id, HashRow { version, thumb_mtime: 0, hash })).collect()
    }

    fn table(items: &[(u64, u64)]) -> SimilarTable {
        let r: Vec<(u64, Option<u64>, u8)> = items.iter().map(|&(id, h)| (id, Some(h), HASH_VERSION)).collect();
        SimilarTable::from_rows(&rows(&r), HashSet::new())
    }

    #[test]
    fn table_keeps_only_current_version_hashes_that_exist() {
        let t = SimilarTable::from_rows(
            &rows(&[(1, Some(0xFF), HASH_VERSION), (2, None, HASH_VERSION), (3, Some(0xAA), HASH_VERSION.wrapping_add(1))]),
            HashSet::new(),
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t.hash_of(1), Some(0xFF));
        assert_eq!(t.hash_of(2), None, "ハッシュを作れなかったものは判定できない");
        assert_eq!(t.hash_of(3), None, "版が違うものは使わない");
    }

    #[test]
    fn nearest_returns_close_partners_in_distance_then_id_order_without_the_subject() {
        // 1 のハッシュ 0。2 は距離1、3 は距離1（IDが大きい）、4 は距離3、5 は距離20（遠い）
        let t = table(&[(1, 0), (4, 0b111), (3, 0b10), (2, 0b1), (5, (1u64 << 20) - 1)]);
        assert_eq!(t.nearest(1, 6, 10), vec![(2, 1), (3, 1), (4, 3)]);
        assert_eq!(t.nearest(1, 0, 10), Vec::<(u64, u8)>::new(), "距離0以下に限れば、近い相手は無い（自分は除く）");
        assert_eq!(t.nearest(1, 6, 2), vec![(2, 1), (3, 1)], "上限件数まで");
        assert!(t.nearest(99, 6, 10).is_empty(), "ハッシュの無い対象は空");
    }

    #[test]
    fn nearest_keeps_the_best_even_when_many_far_partners_pile_up() {
        // 距離10の相手を大量に作り、最後に距離1の相手を置く。刈り込みで、近い相手を失わない。
        let mut items: Vec<(u64, u64)> = vec![(1, 0)];
        for i in 0..500u64 {
            items.push((100 + i, 0b11_1111_1111)); // 距離10
        }
        items.push((9999, 1)); // 距離1
        let t = table(&items);
        let got = t.nearest(1, 12, 5);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], (9999, 1), "最も近い相手が先頭に残る");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_similar_search_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_file(dir: &Path, name: &str, seed: u8, len: usize) {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| ((i as u8).wrapping_mul(seed).wrapping_add(seed)) ^ (i >> 3) as u8).collect::<Vec<u8>>()).unwrap();
        // 書き込み直後のファイルは、FP を作らずに記録される（書き込み中の扱い）。十分古い更新日時にしておく。
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
    }

    /// 名前ごとに（内容の種, ハッシュ）を持つファイルを台帳に登録する。IDを名前順に返す。
    fn seed_db(tag: &str, files: &[(&str, u8, u64)]) -> (Arc<Mutex<Database>>, PathBuf, Vec<u64>, SimilarTable) {
        let root = temp_dir(tag);
        let db = open_spread_db(&root).unwrap();
        let mut ids = Vec::new();
        let mut put = Vec::new();
        for (i, (name, seed, hash)) in files.iter().enumerate() {
            make_file(&root, name, *seed, 3000 + i * 11);
            let rec = file_identity::ensure_record(&db, &root, name).unwrap();
            ids.push(rec.id);
            put.push((rec.id, HashRow { version: HASH_VERSION, thumb_mtime: 0, hash: Some(*hash) }));
        }
        assert!(thumb_hash_store::put_all(&db, &put));
        let t = SimilarTable::load(&db);
        (db, root, ids, t)
    }

    fn go() -> bool {
        true
    }

    #[test]
    fn find_similar_returns_close_partners_with_distances() {
        // a と b は距離2、c は遠い
        let (db, _r, ids, t) = seed_db("basic", &[("a.png", 1, 0b0), ("b.png", 2, 0b11), ("c.png", 3, u64::MAX)]);
        let found = find_similar(&db, &t, &[ids[0]], 6, &go);
        assert_eq!(found.len(), 1);
        let (sid, matches) = &found[0];
        assert_eq!(*sid, ids[0]);
        assert_eq!(matches.iter().map(|m| (m.other, m.distance, m.dismissed)).collect::<Vec<_>>(), vec![(ids[1], 2, false)]);
        // 逆向きにも見つかる
        let back = find_similar(&db, &t, &[ids[1]], 6, &go);
        assert_eq!(back[0].1[0].other, ids[0]);
        // ハッシュの無い対象・近い相手の無い対象は含まれない
        assert!(find_similar(&db, &t, &[ids[2]], 6, &go).is_empty());
        assert!(find_similar(&db, &t, &[424242], 6, &go).is_empty());
    }

    #[test]
    fn partners_with_the_same_fp_are_left_to_the_duplicate_marker() {
        // 内容（種と長さ）が同じファイルは FP が一致する。同じ内容を2つ作る。
        let root = temp_dir("fp");
        let db = open_spread_db(&root).unwrap();
        make_file(&root, "x.png", 7, 4000);
        std::fs::copy(root.join("x.png"), root.join("x_copy.png")).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(root.join("x_copy.png")).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        make_file(&root, "y.png", 9, 5000);
        let x = file_identity::ensure_record(&db, &root, "x.png").unwrap();
        let xc = file_identity::ensure_record(&db, &root, "x_copy.png").unwrap();
        let y = file_identity::ensure_record(&db, &root, "y.png").unwrap();
        assert!(x.fp.is_some() && x.fp == xc.fp, "前提: コピーはFPが一致（Some）");
        let put = vec![
            (x.id, HashRow { version: HASH_VERSION, thumb_mtime: 0, hash: Some(0) }),
            (xc.id, HashRow { version: HASH_VERSION, thumb_mtime: 0, hash: Some(1) }),
            (y.id, HashRow { version: HASH_VERSION, thumb_mtime: 0, hash: Some(0b11) }),
        ];
        assert!(thumb_hash_store::put_all(&db, &put));
        let t = SimilarTable::load(&db);
        let found = find_similar(&db, &t, &[x.id], 6, &go);
        let others: Vec<u64> = found[0].1.iter().map(|m| m.other).collect();
        assert_eq!(others, vec![y.id], "FP一致のコピーは除く（重複マーカーの担当）");
    }

    #[test]
    fn partners_the_ledger_marks_as_missing_are_left_out() {
        let (db, root, ids, t) = seed_db("missing", &[("a.png", 1, 0), ("b.png", 2, 0b1), ("c.png", 3, 0b11)]);
        let key = crate::spread_state::make_key(&root, "b.png");
        assert!(file_identity::mark_unconfirmed(&db, &key, file_identity::now_unix()));
        let found = find_similar(&db, &t, &[ids[0]], 6, &go);
        let others: Vec<u64> = found[0].1.iter().map(|m| m.other).collect();
        assert_eq!(others, vec![ids[2]], "前回の確認で見つからなかった相手は除く");
    }

    #[test]
    fn dismissed_pairs_are_flagged_not_dropped() {
        let (db, _r, ids, _t) = seed_db("dismissed", &[("a.png", 1, 0), ("b.png", 2, 0b1)]);
        assert!(thumb_hash_store::dismiss_pair(&db, ids[0], ids[1], 100));
        let t = SimilarTable::load(&db);
        let found = find_similar(&db, &t, &[ids[0]], 6, &go);
        assert!(found[0].1[0].dismissed, "消した組は、旗をつけて結果に残す");
    }

    #[test]
    fn keep_going_false_stops_the_search() {
        let (db, _r, ids, t) = seed_db("stop", &[("a.png", 1, 0), ("b.png", 2, 0b1)]);
        assert!(find_similar(&db, &t, &[ids[0]], 6, &|| false).is_empty(), "打ち切られたら何も返さない");
    }

    /// 計測用（通常は走らせない）。`cargo test --release -- --ignored similar_search_timing --nocapture`
    #[test]
    #[ignore = "timing measurement"]
    fn similar_search_timing() {
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for n in [100_000u64, 500_000, 1_000_000] {
            let items: Vec<(u64, u64)> = (0..n).map(|i| (i + 1, next())).collect();
            let t = table(&items);
            for subjects in [50usize, 200] {
                let start = std::time::Instant::now();
                let mut total = 0;
                for s in 0..subjects as u64 {
                    total += t.nearest(s + 1, 6, MAX_PARTNERS).len() + t.nearest(s + 1, 19, MAX_PARTNERS).len();
                }
                println!("table={n} subjects={subjects} (距離6と19の2回): {:?} (found {total})", start.elapsed());
            }
        }
    }
}
