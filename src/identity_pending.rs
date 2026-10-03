// 本実装のR4（ワーカー結線）・R5（解決UI）で接続するまでの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

//! 「未解決」の記録層。自動では決められなかった重複・曖昧の項目を、`nekoviewer_spread.redb` に覚えておく。
//! UI（解決ダイアログ）はこの一覧を表示し、解決したら記録を消す。解決の操作そのものはR3。
//!
//! - 記録 = (種別, 対象のファイルID, 候補のファイルID一覧, 検出日時)。同じ (種別, 対象) は1件にまとめる
//! - テーブル `identity_pending_v1` は最初の追加で作る（旧DBの構造を起動だけで変えない）
//! - 状況が変わって意味を失った記録は `prune` で自動的に除く（対象が消えた・対象に記録が付いた・
//!   候補が尽きた、など）。日数による削除でIDが消えた場合も、候補が尽きた項目はここで消える

use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::file_identity::{
    probe_record, record_by_id_tx, FileRecord, Presence, Reader, IDENTITY_COUNTER_TABLE,
};

/// 未解決の記録（キー = 記録ID、値 = `PendingRecord::encode`）。
const PENDING_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("identity_pending_v1");
const NEXT_PENDING_KEY: &str = "next_pending_id";
const RECORD_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingKind {
    /// 新しいファイルと同内容の現存ファイルが複数あり、データが食い違う。引き継ぎ元を選ぶ。
    CopySource = 1,
    /// 新しいファイルと同内容の「消えているファイル」が複数あり、移動元を決められない。
    MoveTarget = 2,
    /// 実体の無いお気に入りに、同内容の現存ファイルがある。お気に入りの移し先を選ぶ。
    FavoriteHandover = 3,
}

impl PendingKind {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::CopySource),
            2 => Some(Self::MoveTarget),
            3 => Some(Self::FavoriteHandover),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRecord {
    pub id: u64,
    pub kind: PendingKind,
    /// 対象のファイルID（引き継ぎ元の選択・移動元の選択は新しいファイル、
    /// お気に入りの引き継ぎは実体の無いお気に入りのID）。
    pub subject: u64,
    /// 候補のファイルID（昇順・重複なし・対象を含まない）。
    pub candidates: Vec<u64>,
    /// 検出日時（unix秒）。
    pub detected_at: i64,
}

impl PendingRecord {
    fn encode(&self) -> Vec<u8> {
        let mut b = vec![RECORD_VERSION, self.kind as u8];
        b.extend(self.id.to_le_bytes());
        b.extend(self.subject.to_le_bytes());
        b.extend(self.detected_at.to_le_bytes());
        b.extend((self.candidates.len() as u32).to_le_bytes());
        for c in &self.candidates {
            b.extend(c.to_le_bytes());
        }
        b
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { b: bytes, pos: 0 };
        if r.u8()? != RECORD_VERSION {
            return None;
        }
        let kind = PendingKind::from_u8(r.u8()?)?;
        let id = r.u64()?;
        let subject = r.u64()?;
        let detected_at = r.i64()?;
        let n = r.u32()? as usize;
        let candidates = (0..n).map(|_| r.u64()).collect::<Option<Vec<u64>>>()?;
        Some(Self { id, kind, subject, candidates, detected_at })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddOutcome {
    /// 新しい未解決として記録した（通知の対象）。
    Created(u64),
    /// 同じ (種別, 対象) が既にあり、候補が変わったので更新した。
    Updated(u64),
    /// 同じ (種別, 対象) が既にあり、候補も同じ。何も変えていない。
    Unchanged(u64),
}

/// 候補を整える（昇順・重複なし・対象を除く）。
fn normalize_candidates(subject: u64, candidates: &[u64]) -> Vec<u64> {
    let mut v: Vec<u64> = candidates.iter().copied().filter(|c| *c != subject).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 未解決を追加する。同じ (種別, 対象) があれば1件にまとめる（候補が変われば更新。検出日時は最初のまま）。
/// 候補が空になる場合は記録しない（None）。
pub fn add(
    db: &Arc<Mutex<Database>>,
    kind: PendingKind,
    subject: u64,
    candidates: &[u64],
    now: i64,
) -> Option<AddOutcome> {
    let candidates = normalize_candidates(subject, candidates);
    if candidates.is_empty() {
        return None;
    }
    let guard = db.lock().ok()?;
    let tx = guard.begin_write().ok()?;
    let outcome = {
        let mut table = tx.open_table(PENDING_TABLE).ok()?;
        let existing: Option<PendingRecord> = table
            .iter()
            .ok()?
            .flatten()
            .filter_map(|(_, v)| PendingRecord::decode(v.value()))
            .find(|r| r.kind == kind && r.subject == subject);
        match existing {
            Some(rec) if rec.candidates == candidates => AddOutcome::Unchanged(rec.id),
            Some(rec) => {
                let updated = PendingRecord { candidates, ..rec };
                table.insert(updated.id, updated.encode().as_slice()).ok()?;
                AddOutcome::Updated(updated.id)
            }
            None => {
                let mut counter = tx.open_table(IDENTITY_COUNTER_TABLE).ok()?;
                let id = {
                    let next = counter.get(NEXT_PENDING_KEY).ok()?.map(|g| g.value());
                    next.unwrap_or(1)
                };
                counter.insert(NEXT_PENDING_KEY, id + 1).ok()?;
                let rec = PendingRecord { id, kind, subject, candidates, detected_at: now };
                table.insert(id, rec.encode().as_slice()).ok()?;
                AddOutcome::Created(id)
            }
        }
    };
    tx.commit().ok()?;
    Some(outcome)
}

/// 未解決の一覧（検出日時の新しい順、同じなら記録IDの大きい順）。テーブルが無ければ空。
pub fn list(db: &Arc<Mutex<Database>>) -> Vec<PendingRecord> {
    let Ok(guard) = db.lock() else { return Vec::new() };
    list_in(&guard)
}

fn list_in(db: &Database) -> Vec<PendingRecord> {
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(PENDING_TABLE) else { return Vec::new() };
    let Ok(iter) = table.iter() else { return Vec::new() };
    let mut out: Vec<PendingRecord> = iter
        .flatten()
        .filter_map(|(_, v)| PendingRecord::decode(v.value()))
        .collect();
    out.sort_by(|a, b| b.detected_at.cmp(&a.detected_at).then(b.id.cmp(&a.id)));
    out
}

/// 未解決の件数（メニューバーのボタンの数字）。
pub fn count(db: &Arc<Mutex<Database>>) -> usize {
    list(db).len()
}

/// 記録を1件消す（解決した・「引き継がない」を選んだ時）。消したら true。
pub fn remove(db: &Arc<Mutex<Database>>, id: u64) -> bool {
    let Ok(guard) = db.lock() else { return false };
    // 無いものを消すために、書き込み（＝テーブルの作成）はしない。
    if list_in(&guard).iter().all(|r| r.id != id) {
        return false;
    }
    let Ok(tx) = guard.begin_write() else { return false };
    let removed = {
        let Ok(mut table) = tx.open_table(PENDING_TABLE) else { return false };
        table.remove(id).ok().flatten().is_some()
    };
    tx.commit().is_ok() && removed
}

/// 対象に紐づく記録を全て消す（対象が別の経路で解決された時）。消した件数を返す。
pub fn remove_for_subject(db: &Arc<Mutex<Database>>, kind: PendingKind, subject: u64) -> usize {
    let Ok(guard) = db.lock() else { return 0 };
    if !list_in(&guard).iter().any(|r| r.kind == kind && r.subject == subject) {
        return 0;
    }
    let Ok(tx) = guard.begin_write() else { return 0 };
    let mut removed = 0;
    {
        let Ok(mut table) = tx.open_table(PENDING_TABLE) else { return 0 };
        let ids: Vec<u64> = match table.iter() {
            Ok(iter) => iter
                .flatten()
                .filter_map(|(_, v)| PendingRecord::decode(v.value()))
                .filter(|r| r.kind == kind && r.subject == subject)
                .map(|r| r.id)
                .collect(),
            Err(_) => return 0,
        };
        for id in ids {
            if table.remove(id).ok().flatten().is_some() {
                removed += 1;
            }
        }
    }
    if tx.commit().is_ok() { removed } else { 0 }
}

/// 掃除の判定に使う、ファイルの現況の問い合わせ。ファイルを見に行く関数は、DBロックの外で呼ばれる。
pub struct PendingEnv<'a> {
    pub probe: &'a dyn Fn(&FileRecord) -> Presence,
    /// そのIDにユーザーデータ（評価・訪問・しおり・設定）があるか。
    pub has_user_data: &'a dyn Fn(u64) -> bool,
    /// そのIDが今、お気に入りに入っているか（未整理を含む）。
    pub is_favorite: &'a dyn Fn(&FileRecord) -> bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// 記録ごと除いた件数。
    pub removed: usize,
    /// 候補を減らして残した件数。
    pub updated: usize,
}

/// 記録を残すか、残すなら候補をどう減らすかを決める。None は記録ごと除く。
fn decide(
    rec: &PendingRecord,
    subject: Option<&FileRecord>,
    candidates: &[FileRecord],
    env: &PendingEnv,
) -> Option<Vec<u64>> {
    // 対象のIDが消えた（日数による削除など）なら、解決する相手がいない。
    let subject = subject?;
    match rec.kind {
        PendingKind::CopySource | PendingKind::MoveTarget => {
            // 対象に既に記録が付いた（解決を待つ間に評価などを付けた）なら、潰さないよう取り下げる。
            if (env.has_user_data)(subject.id) {
                return None;
            }
            // 対象のファイル自体が消えたなら、記録を付ける先が無い。
            if (env.probe)(subject) == Presence::Missing {
                return None;
            }
        }
        PendingKind::FavoriteHandover => {
            // お気に入りが外された、または実体が戻ってきたなら、移す必要がない。
            if !(env.is_favorite)(subject) || (env.probe)(subject) == Presence::Present {
                return None;
            }
        }
    }
    let kept: Vec<u64> = candidates
        .iter()
        .filter(|c| c.id != subject.id)
        .filter(|c| {
            let presence = (env.probe)(c);
            match rec.kind {
                // 引き継ぎ元は、中身を読める（現存、または一時的に確認できない）もの。
                PendingKind::CopySource => presence != Presence::Missing,
                // 移動元の候補は、まだ見つからないままのもの（戻ってきたファイルは移動元ではない）。
                PendingKind::MoveTarget => presence != Presence::Present,
                // お気に入りの移し先は、実在するファイルだけ。
                PendingKind::FavoriteHandover => presence == Presence::Present,
            }
        })
        .map(|c| c.id)
        .collect();
    (!kept.is_empty()).then_some(kept)
}

/// 意味を失った記録を除き、候補が減った記録は更新する。
/// 読み取り→（ロック外で）ファイルの現況確認→書き込み、の3段階でDBロックを短く握る。
pub fn prune(db: &Arc<Mutex<Database>>, env: &PendingEnv) -> PruneReport {
    // 1. 読み取り: 記録と、対象・候補のIDレコードを集める。
    let snapshot: Vec<(PendingRecord, Option<FileRecord>, Vec<FileRecord>)> = {
        let Ok(guard) = db.lock() else { return PruneReport::default() };
        let records = list_in(&guard);
        let Ok(tx) = guard.begin_read() else { return PruneReport::default() };
        records
            .into_iter()
            .map(|rec| {
                let subject = record_by_id_tx(&tx, rec.subject);
                let candidates = rec.candidates.iter().filter_map(|c| record_by_id_tx(&tx, *c)).collect();
                (rec, subject, candidates)
            })
            .collect()
    };
    if snapshot.is_empty() {
        return PruneReport::default(); // 記録が無ければ何もしない（テーブルも作らない）。
    }
    // 2. ロックの外で判定する（ファイルの存在確認を含む）。
    let decisions: Vec<(PendingRecord, Option<Vec<u64>>)> = snapshot
        .into_iter()
        .map(|(rec, subject, candidates)| {
            let decision = decide(&rec, subject.as_ref(), &candidates, env);
            (rec, decision)
        })
        .collect();
    // 3. 書き込み。
    let mut report = PruneReport::default();
    let Ok(guard) = db.lock() else { return report };
    let Ok(tx) = guard.begin_write() else { return report };
    {
        let Ok(mut table) = tx.open_table(PENDING_TABLE) else { return report };
        for (rec, decision) in decisions {
            match decision {
                None => {
                    if table.remove(rec.id).ok().flatten().is_some() {
                        report.removed += 1;
                    }
                }
                Some(kept) if kept != rec.candidates => {
                    let updated = PendingRecord { candidates: kept, ..rec };
                    if table.insert(updated.id, updated.encode().as_slice()).is_ok() {
                        report.updated += 1;
                    }
                }
                Some(_) => {}
            }
        }
    }
    if tx.commit().is_err() {
        return PruneReport::default();
    }
    report
}

/// 実ファイル・実DBで掃除する。
pub fn prune_real(db: &Arc<Mutex<Database>>) -> PruneReport {
    let has_user_data = |id: u64| crate::spread_state::id_has_user_data(db, id);
    let is_favorite = |rec: &FileRecord| {
        crate::file_settings::read_effective_for_record(db, rec).is_some_and(|e| e.favorite.is_some())
    };
    prune(db, &PendingEnv { probe: &probe_record, has_user_data: &has_user_data, is_favorite: &is_favorite })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use redb::TableHandle;

    use super::*;
    use crate::file_identity::{resolve_path, Observation, ResolveEnv};
    use crate::spread_state::open_spread_db;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("nekoviewer_identity_pending_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 偽のIDを作る（FPをIDごとに変えるので、互いに移動・統合されない）。
    fn make_id(db: &Arc<Mutex<Database>>, key: &str, fp: u8) -> u64 {
        let obs = Observation { size: 10, mtime: 5, volume: "/".to_owned(), fp: Some([fp; 16]) };
        let probe = |_: &FileRecord| Presence::Present;
        let has = |_: u64| false;
        let env = ResolveEnv { now: 1, probe: &probe, has_user_data: &has };
        resolve_path(db, key, &obs, &env).unwrap();
        crate::file_identity::lookup(db, key).unwrap().id
    }

    #[derive(Default)]
    struct Fake {
        presence: HashMap<u64, Presence>,
        user_data: HashSet<u64>,
        favorites: HashSet<u64>,
    }

    fn prune_fake(db: &Arc<Mutex<Database>>, fake: &Fake) -> PruneReport {
        let probe = |r: &FileRecord| fake.presence.get(&r.id).copied().unwrap_or(Presence::Present);
        let has = |id: u64| fake.user_data.contains(&id);
        let fav = |r: &FileRecord| fake.favorites.contains(&r.id);
        prune(db, &PendingEnv { probe: &probe, has_user_data: &has, is_favorite: &fav })
    }

    fn new_db(t: &TempRoot) -> Arc<Mutex<Database>> {
        open_spread_db(&t.0).unwrap()
    }

    #[test]
    fn record_round_trips_and_rejects_corrupt_bytes() {
        let rec = PendingRecord {
            id: 7,
            kind: PendingKind::MoveTarget,
            subject: 3,
            candidates: vec![1, 2, u64::MAX],
            detected_at: -5,
        };
        assert_eq!(PendingRecord::decode(&rec.encode()), Some(rec.clone()));
        let bytes = rec.encode();
        assert_eq!(PendingRecord::decode(&bytes[..bytes.len() - 1]), None);
        let mut bad_kind = bytes.clone();
        bad_kind[1] = 99;
        assert_eq!(PendingRecord::decode(&bad_kind), None);
        let mut bad_version = bytes;
        bad_version[0] = 9;
        assert_eq!(PendingRecord::decode(&bad_version), None);
        assert_eq!(PendingRecord::decode(&[]), None);
    }

    #[test]
    fn reads_never_create_the_table_in_a_legacy_db() {
        let t = TempRoot::new("lazy");
        let db = new_db(&t);
        assert!(list(&db).is_empty());
        assert_eq!(count(&db), 0);
        assert!(!remove(&db, 1));
        assert_eq!(prune_fake(&db, &Fake::default()), PruneReport::default());
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name().starts_with("identity_pending")));
    }

    #[test]
    fn add_list_count_remove() {
        let t = TempRoot::new("basic");
        let db = new_db(&t);
        let first = add(&db, PendingKind::CopySource, 10, &[3, 1], 100).unwrap();
        let second = add(&db, PendingKind::MoveTarget, 11, &[2], 200).unwrap();
        let (AddOutcome::Created(a), AddOutcome::Created(b)) = (first, second) else { panic!("{first:?} {second:?}") };
        assert_ne!(a, b);
        assert_eq!(count(&db), 2);
        // 検出日時の新しい順。候補は昇順に整う。
        let listed = list(&db);
        assert_eq!(listed.iter().map(|r| r.subject).collect::<Vec<_>>(), vec![11, 10]);
        assert_eq!(listed[1].candidates, vec![1, 3]);
        assert!(remove(&db, a));
        assert!(!remove(&db, a), "二重に消せない");
        assert_eq!(count(&db), 1);
    }

    #[test]
    fn same_kind_and_subject_are_merged_into_one_record() {
        let t = TempRoot::new("merge");
        let db = new_db(&t);
        let AddOutcome::Created(id) = add(&db, PendingKind::CopySource, 10, &[1, 2], 100).unwrap() else { panic!() };
        assert_eq!(add(&db, PendingKind::CopySource, 10, &[2, 1], 999), Some(AddOutcome::Unchanged(id)));
        assert_eq!(add(&db, PendingKind::CopySource, 10, &[1, 2, 3], 999), Some(AddOutcome::Updated(id)));
        let listed = list(&db);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].candidates, vec![1, 2, 3]);
        assert_eq!(listed[0].detected_at, 100, "検出日時は最初のまま");
        // 種別が違えば別の記録。
        assert!(matches!(add(&db, PendingKind::MoveTarget, 10, &[1], 5), Some(AddOutcome::Created(_))));
        assert_eq!(count(&db), 2);
    }

    #[test]
    fn candidates_are_normalized_and_empty_ones_are_not_recorded() {
        let t = TempRoot::new("normalize");
        let db = new_db(&t);
        assert_eq!(add(&db, PendingKind::CopySource, 5, &[], 1), None);
        assert_eq!(add(&db, PendingKind::CopySource, 5, &[5], 1), None, "対象自身は候補にならない");
        add(&db, PendingKind::CopySource, 5, &[9, 5, 9, 2], 1).unwrap();
        assert_eq!(list(&db)[0].candidates, vec![2, 9]);
    }

    #[test]
    fn counter_keeps_counting_across_removals() {
        let t = TempRoot::new("counter");
        let db = new_db(&t);
        let AddOutcome::Created(a) = add(&db, PendingKind::CopySource, 1, &[2], 1).unwrap() else { panic!() };
        assert!(remove(&db, a));
        let AddOutcome::Created(b) = add(&db, PendingKind::CopySource, 1, &[2], 1).unwrap() else { panic!() };
        assert!(b > a, "消した記録IDを再利用しない");
    }

    #[test]
    fn remove_for_subject_clears_all_records_of_that_subject() {
        let t = TempRoot::new("by_subject");
        let db = new_db(&t);
        add(&db, PendingKind::CopySource, 1, &[2], 1).unwrap();
        add(&db, PendingKind::MoveTarget, 1, &[3], 1).unwrap();
        add(&db, PendingKind::CopySource, 9, &[2], 1).unwrap();
        assert_eq!(remove_for_subject(&db, PendingKind::CopySource, 1), 1);
        assert_eq!(count(&db), 2);
        assert_eq!(remove_for_subject(&db, PendingKind::CopySource, 1), 0);
    }

    #[test]
    fn prune_drops_copy_source_when_subject_is_gone_or_has_data() {
        let t = TempRoot::new("prune_copy");
        let db = new_db(&t);
        let (subject, a, b) = (make_id(&db, "/d\0new", 1), make_id(&db, "/d\0a", 2), make_id(&db, "/d\0b", 3));
        add(&db, PendingKind::CopySource, subject, &[a, b], 1).unwrap();
        // 何も変わらなければ残る。
        assert_eq!(prune_fake(&db, &Fake::default()), PruneReport::default());
        assert_eq!(count(&db), 1);
        // 解決を待つ間に、対象へ記録が付いたら取り下げる（既にあるデータを潰さない）。
        let mut fake = Fake::default();
        fake.user_data.insert(subject);
        assert_eq!(prune_fake(&db, &fake), PruneReport { removed: 1, updated: 0 });
        assert_eq!(count(&db), 0);
        // 対象のファイル自体が消えた場合も取り下げる。
        add(&db, PendingKind::CopySource, subject, &[a, b], 1).unwrap();
        let mut fake = Fake::default();
        fake.presence.insert(subject, Presence::Missing);
        assert_eq!(prune_fake(&db, &fake).removed, 1);
        // 対象のIDがDBから消えた場合も。
        add(&db, PendingKind::CopySource, 9999, &[a], 1).unwrap();
        assert_eq!(prune_fake(&db, &Fake::default()).removed, 1);
    }

    #[test]
    fn prune_narrows_copy_source_candidates_to_readable_files() {
        let t = TempRoot::new("prune_copy_cand");
        let db = new_db(&t);
        let (subject, a, b, c) = (
            make_id(&db, "/d\0new", 1),
            make_id(&db, "/d\0a", 2),
            make_id(&db, "/d\0b", 3),
            make_id(&db, "/d\0c", 4),
        );
        add(&db, PendingKind::CopySource, subject, &[a, b, c], 1).unwrap();
        let mut fake = Fake::default();
        fake.presence.insert(a, Presence::Missing); // 消えた候補は引き継ぎ元にできない
        fake.presence.insert(b, Presence::Offline); // 一時的に確認できないものは残す
        assert_eq!(prune_fake(&db, &fake), PruneReport { removed: 0, updated: 1 });
        assert_eq!(list(&db)[0].candidates, vec![b, c]);
        // 候補が全て消えたら記録ごと除く。
        fake.presence.insert(b, Presence::Missing);
        fake.presence.insert(c, Presence::Missing);
        assert_eq!(prune_fake(&db, &fake).removed, 1);
    }

    #[test]
    fn prune_keeps_only_still_missing_move_candidates() {
        let t = TempRoot::new("prune_move");
        let db = new_db(&t);
        let (subject, a, b) = (make_id(&db, "/d\0new", 1), make_id(&db, "/d\0a", 2), make_id(&db, "/d\0b", 3));
        add(&db, PendingKind::MoveTarget, subject, &[a, b], 1).unwrap();
        let mut fake = Fake::default();
        fake.presence.insert(a, Presence::Missing);
        fake.presence.insert(b, Presence::Present); // 戻ってきたファイルは移動元ではない
        assert_eq!(prune_fake(&db, &fake), PruneReport { removed: 0, updated: 1 });
        assert_eq!(list(&db)[0].candidates, vec![a]);
        // 日数による削除で候補のIDが消えた場合（DBに無い）も、候補が尽きた項目は消える。
        let missing_id = 424242;
        add(&db, PendingKind::MoveTarget, make_id(&db, "/d\0other", 9), &[missing_id], 1).unwrap();
        let report = prune_fake(&db, &fake);
        assert_eq!(report.removed, 1);
        assert_eq!(count(&db), 1);
    }

    #[test]
    fn prune_favorite_handover_needs_a_missing_favorite_and_present_targets() {
        let t = TempRoot::new("prune_fav");
        let db = new_db(&t);
        let (orphan, alive, other) = (make_id(&db, "/d\0old", 1), make_id(&db, "/d\0alive", 2), make_id(&db, "/d\0other", 3));
        add(&db, PendingKind::FavoriteHandover, orphan, &[alive, other], 1).unwrap();
        let mut fake = Fake::default();
        fake.favorites.insert(orphan);
        fake.presence.insert(orphan, Presence::Missing);
        fake.presence.insert(other, Presence::Missing); // 消えたファイルは移し先にならない
        assert_eq!(prune_fake(&db, &fake), PruneReport { removed: 0, updated: 1 });
        assert_eq!(list(&db)[0].candidates, vec![alive]);
        // 実体が戻ってきたら、移す必要がない。
        fake.presence.insert(orphan, Presence::Present);
        assert_eq!(prune_fake(&db, &fake).removed, 1);
        // お気に入りが外されていても取り下げる。
        add(&db, PendingKind::FavoriteHandover, orphan, &[alive], 1).unwrap();
        let mut fake = Fake::default();
        fake.presence.insert(orphan, Presence::Missing);
        assert_eq!(prune_fake(&db, &fake).removed, 1, "お気に入りでなくなった");
    }

    #[test]
    fn prune_real_works_on_real_files_and_user_data() {
        let t = TempRoot::new("prune_real");
        let db = new_db(&t);
        let dir = t.0.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, len: usize| {
            let p = dir.join(name);
            std::fs::write(&p, vec![len as u8; len]).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
            crate::file_identity::ensure_record(&db, &dir, name).unwrap().id
        };
        let (subject, a, b) = (mk("new.zip", 3000), mk("a.zip", 3100), mk("b.zip", 3200));
        add(&db, PendingKind::CopySource, subject, &[a, b], 1).unwrap();
        assert_eq!(prune_real(&db), PruneReport::default());
        // 候補の実ファイルが消えると、候補が減る。
        std::fs::remove_file(dir.join("a.zip")).unwrap();
        assert_eq!(prune_real(&db), PruneReport { removed: 0, updated: 1 });
        assert_eq!(list(&db)[0].candidates, vec![b]);
        // 対象に評価が付くと、取り下げる。
        assert!(crate::spread_state::write_archive_rating(&db, &dir, "new.zip", 6));
        assert_eq!(prune_real(&db).removed, 1);
        assert_eq!(count(&db), 0);
    }
}
