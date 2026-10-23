//! 重複マーカーの台帳。同じ内容のファイル（FP一致）を「移動」「再取得」としてユーザーへ示すための記録層。
//! 表示・消去・フィルタは後続フェーズ。ここは記録と引き当てだけを持つ。
//!
//! - 種別は2つ。移動（旧パスが消えている。IDごと移るので重複先は持たない）と、
//!   再取得（旧パスが現存。複製も区別しない。重複先＝元のファイルID群）
//! - 1ファイル最大1マーカー。移動と再取得が重なったら再取得が優先（永続の側が一時の側を飲み込む）
//! - 本体表 `duplicate_marker_v1`（キー = マーカー持ちのファイルID）と、逆引き表
//!   `duplicate_marker_target_v1`（重複先のID → マーカー持ちのID）は、常に同じトランザクションで書く。
//!   旧ファイル側カードの「他に重複あり」を、全件走査なしで判定するための索引
//! - テーブルは最初の記録で作る（旧DBの構造を起動だけで変えない）。読み取りと `forget_id_tx` は作らない
//! - IDが消えたら（`file_identity::delete_id` など）行と索引を連動して消す。再取得は重複先が尽きたら消える

use std::sync::{Arc, Mutex};

use redb::{
    Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, TableDefinition,
    TableHandle, WriteTransaction,
};

use crate::file_identity::Reader;

const MARKER_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("duplicate_marker_v1");
const MARKER_TARGET_TABLE: MultimapTableDefinition<u64, u64> = MultimapTableDefinition::new("duplicate_marker_target_v1");
const RECORD_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    /// 旧パスが消えていて、IDごと新しいパスへ移った（一時のマーカー）。
    Moved = 1,
    /// 旧パスが現存したまま、同じ内容のファイルが増えた（永続のマーカー）。
    Reacquired = 2,
}

impl MarkerKind {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Moved),
            2 => Some(Self::Reacquired),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerRecord {
    /// マーカーを持つファイルのID。
    pub id: u64,
    pub kind: MarkerKind,
    /// 重複先のファイルID（昇順・重複なし・自分を含まない）。移動は空。
    pub targets: Vec<u64>,
    /// 表示領域に入ったか（移動マーカーの消去の起点）。
    pub in_viewport: bool,
    /// ユーザーが正常として消した（一覧の「消去済みも表示」で呼び戻せる）。
    pub dismissed: bool,
    /// 重複先と更新日時が一致していた（再取得の補助表示）。
    pub mtime_match: bool,
    /// 検出日時（unix秒）。
    pub detected_at: i64,
}

impl MarkerRecord {
    fn encode(&self) -> Vec<u8> {
        let flags = u8::from(self.in_viewport) | u8::from(self.dismissed) << 1 | u8::from(self.mtime_match) << 2;
        let mut b = vec![RECORD_VERSION, self.kind as u8, flags];
        b.extend(self.id.to_le_bytes());
        b.extend(self.detected_at.to_le_bytes());
        b.extend((self.targets.len() as u32).to_le_bytes());
        for t in &self.targets {
            b.extend(t.to_le_bytes());
        }
        b
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { b: bytes, pos: 0 };
        if r.u8()? != RECORD_VERSION {
            return None;
        }
        let kind = MarkerKind::from_u8(r.u8()?)?;
        let flags = r.u8()?;
        let id = r.u64()?;
        let detected_at = r.i64()?;
        let n = r.u32()? as usize;
        let targets = (0..n).map(|_| r.u64()).collect::<Option<Vec<u64>>>()?;
        Some(Self {
            id,
            kind,
            targets,
            in_viewport: flags & 1 != 0,
            dismissed: flags & 2 != 0,
            mtime_match: flags & 4 != 0,
            detected_at,
        })
    }
}

/// 重複先を整える（昇順・重複なし・自分を除く）。
fn normalize_targets(id: u64, targets: &[u64]) -> Vec<u64> {
    let mut v: Vec<u64> = targets.iter().copied().filter(|t| *t != id).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 移動を記録する。既にマーカーがあれば何もしない（再取得は移動より優先、移動は最初の検出を残す）。
/// 新しく記録したら true。
pub fn record_moved(db: &Arc<Mutex<Database>>, id: u64, now: i64) -> bool {
    write(db, |table, _rev| {
        if table.get(id).ok()?.is_some() {
            return Some(false);
        }
        let rec = MarkerRecord {
            id,
            kind: MarkerKind::Moved,
            targets: Vec::new(),
            in_viewport: false,
            dismissed: false,
            mtime_match: false,
            detected_at: now,
        };
        table.insert(id, rec.encode().as_slice()).ok()?;
        Some(true)
    })
}

/// 再取得を記録する。移動マーカーがあれば再取得で置き換える。再取得が既にあれば重複先を足す
/// （消去済みと検出日時は最初のまま）。重複先が空なら記録しない。何か変えたら true。
pub fn record_reacquired(
    db: &Arc<Mutex<Database>>,
    id: u64,
    targets: &[u64],
    mtime_match: bool,
    now: i64,
) -> bool {
    let targets = normalize_targets(id, targets);
    if targets.is_empty() {
        return false;
    }
    write(db, |table, rev| {
        let existing = table.get(id).ok()?.and_then(|g| MarkerRecord::decode(g.value()));
        let rec = match existing {
            Some(old) if old.kind == MarkerKind::Reacquired => {
                let mut merged = old.targets.clone();
                merged.extend(&targets);
                merged.sort_unstable();
                merged.dedup();
                let next = MarkerRecord { targets: merged, mtime_match, ..old.clone() };
                if next == old {
                    return Some(false);
                }
                next
            }
            // 無い、または移動。永続の再取得が一時の移動を飲み込む（VPフラグも捨てる）。
            _ => MarkerRecord {
                id,
                kind: MarkerKind::Reacquired,
                targets,
                in_viewport: false,
                dismissed: false,
                mtime_match,
                detected_at: now,
            },
        };
        for t in &rec.targets {
            rev.insert(*t, id).ok()?;
        }
        table.insert(id, rec.encode().as_slice()).ok()?;
        Some(true)
    })
}

/// 本体表と逆引き表を開いて書き込み、コミットする。失敗は false。
fn write(
    db: &Arc<Mutex<Database>>,
    f: impl FnOnce(&mut redb::Table<'_, u64, &'static [u8]>, &mut redb::MultimapTable<'_, u64, u64>) -> Option<bool>,
) -> bool {
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let changed = {
        let (Ok(mut table), Ok(mut rev)) = (tx.open_table(MARKER_TABLE), tx.open_multimap_table(MARKER_TARGET_TABLE))
        else {
            return false;
        };
        match f(&mut table, &mut rev) {
            Some(changed) => changed,
            None => return false,
        }
    };
    tx.commit().is_ok() && changed
}

/// そのIDのマーカー。無ければ（テーブルが無くても）None。
#[allow(dead_code)] // 表示側（D2）で使う。
pub fn get(db: &Arc<Mutex<Database>>, id: u64) -> Option<MarkerRecord> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    let table = tx.open_table(MARKER_TABLE).ok()?;
    let rec = table.get(id).ok()??;
    MarkerRecord::decode(rec.value())
}

/// 全マーカー（検出日時の新しい順、同じならIDの大きい順）。テーブルが無ければ空。
#[allow(dead_code)] // 一覧・フィルタ（D3）で使う。
pub fn list(db: &Arc<Mutex<Database>>) -> Vec<MarkerRecord> {
    let Ok(guard) = db.lock() else { return Vec::new() };
    let Ok(tx) = guard.begin_read() else { return Vec::new() };
    let Ok(table) = tx.open_table(MARKER_TABLE) else { return Vec::new() };
    let Ok(iter) = table.iter() else { return Vec::new() };
    let mut out: Vec<MarkerRecord> = iter.flatten().filter_map(|(_, v)| MarkerRecord::decode(v.value())).collect();
    out.sort_by(|a, b| b.detected_at.cmp(&a.detected_at).then(b.id.cmp(&a.id)));
    out
}

/// `target` を重複先に持つマーカーのファイルID（昇順）。旧ファイル側の「他に重複あり」の判定用。
#[allow(dead_code)] // 表示側（D2）で使う。
pub fn owners_of(db: &Arc<Mutex<Database>>, target: u64) -> Vec<u64> {
    let Ok(guard) = db.lock() else { return Vec::new() };
    let Ok(tx) = guard.begin_read() else { return Vec::new() };
    let Ok(rev) = tx.open_multimap_table(MARKER_TARGET_TABLE) else { return Vec::new() };
    let Ok(values) = rev.get(target) else { return Vec::new() };
    values.flatten().map(|g| g.value()).collect()
}

/// そのIDのマーカーを消す（本体と逆引き）。消したら true。
#[allow(dead_code)] // 移動マーカーの消去・正常として消す操作（D2以降）で使う。
pub fn remove(db: &Arc<Mutex<Database>>, id: u64) -> bool {
    // 無いものを消すために、書き込み（＝テーブルの作成）はしない。
    if get(db, id).is_none() {
        return false;
    }
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let removed = forget_owner(&tx, id).is_some_and(|r| r);
    tx.commit().is_ok() && removed
}

/// IDが消える時の連動（`delete_id` などの書き込みトランザクションの中で呼ぶ）。
/// そのIDのマーカーを消し、そのIDを重複先に持つ他のマーカーから外す（尽きた再取得は消す）。
/// マーカーのテーブルが無ければ何もしない。Some = 成功。
pub(crate) fn forget_id_tx(tx: &WriteTransaction, id: u64) -> Option<()> {
    let exists = tx.list_tables().ok()?.any(|h| h.name() == MARKER_TABLE.name());
    if !exists {
        return Some(());
    }
    forget_owner(tx, id)?;
    let mut table = tx.open_table(MARKER_TABLE).ok()?;
    let mut rev = tx.open_multimap_table(MARKER_TARGET_TABLE).ok()?;
    let owners: Vec<u64> = rev.remove_all(id).ok()?.flatten().map(|g| g.value()).collect();
    for owner in owners {
        let Some(mut rec) = table.get(owner).ok()?.and_then(|g| MarkerRecord::decode(g.value())) else {
            continue;
        };
        rec.targets.retain(|t| *t != id);
        if rec.targets.is_empty() && rec.kind == MarkerKind::Reacquired {
            table.remove(owner).ok()?;
        } else {
            table.insert(owner, rec.encode().as_slice()).ok()?;
        }
    }
    Some(())
}

/// マーカー持ち `id` の行と、その重複先の逆引きを消す。行があって消したら Some(true)。
fn forget_owner(tx: &WriteTransaction, id: u64) -> Option<bool> {
    let mut table = tx.open_table(MARKER_TABLE).ok()?;
    let mut rev = tx.open_multimap_table(MARKER_TARGET_TABLE).ok()?;
    let old = table.remove(id).ok()?.and_then(|g| MarkerRecord::decode(g.value()));
    let Some(old) = old else { return Some(false) };
    for t in &old.targets {
        rev.remove(*t, id).ok()?;
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use redb::MultimapTableHandle;

    use super::*;
    use crate::spread_state::open_spread_db;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root =
                std::env::temp_dir().join(format!("nekoviewer_dup_marker_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn new_db(t: &TempRoot) -> Arc<Mutex<Database>> {
        open_spread_db(&t.0).unwrap()
    }

    fn forget(db: &Arc<Mutex<Database>>, id: u64) -> bool {
        let g = db.lock().unwrap();
        let tx = g.begin_write().unwrap();
        let ok = forget_id_tx(&tx, id).is_some();
        tx.commit().unwrap();
        ok
    }

    #[test]
    fn record_round_trips_and_rejects_corrupt_bytes() {
        let rec = MarkerRecord {
            id: 7,
            kind: MarkerKind::Reacquired,
            targets: vec![1, 2, u64::MAX],
            in_viewport: true,
            dismissed: false,
            mtime_match: true,
            detected_at: -5,
        };
        assert_eq!(MarkerRecord::decode(&rec.encode()), Some(rec.clone()));
        let bytes = rec.encode();
        assert_eq!(MarkerRecord::decode(&bytes[..bytes.len() - 1]), None);
        let mut bad_kind = bytes.clone();
        bad_kind[1] = 99;
        assert_eq!(MarkerRecord::decode(&bad_kind), None);
        let mut bad_version = bytes;
        bad_version[0] = 9;
        assert_eq!(MarkerRecord::decode(&bad_version), None);
        assert_eq!(MarkerRecord::decode(&[]), None);
    }

    #[test]
    fn reads_and_forget_never_create_tables_in_a_legacy_db() {
        let t = TempRoot::new("lazy");
        let db = new_db(&t);
        assert!(get(&db, 1).is_none());
        assert!(list(&db).is_empty());
        assert!(owners_of(&db, 1).is_empty());
        assert!(!remove(&db, 1));
        assert!(forget(&db, 1));
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name().starts_with("duplicate_marker")));
        assert!(!tx.list_multimap_tables().unwrap().any(|h| h.name().starts_with("duplicate_marker")));
    }

    #[test]
    fn moved_is_recorded_once_with_no_targets() {
        let t = TempRoot::new("moved");
        let db = new_db(&t);
        assert!(record_moved(&db, 5, 100));
        assert!(!record_moved(&db, 5, 200), "最初の検出を残す");
        let rec = get(&db, 5).unwrap();
        assert_eq!((rec.kind, rec.detected_at, rec.targets.is_empty()), (MarkerKind::Moved, 100, true));
        assert!(!rec.in_viewport && !rec.dismissed);
    }

    #[test]
    fn reacquired_normalizes_targets_and_fills_the_reverse_index() {
        let t = TempRoot::new("reacq");
        let db = new_db(&t);
        assert!(!record_reacquired(&db, 9, &[], false, 1), "重複先が無ければ記録しない");
        assert!(!record_reacquired(&db, 9, &[9], false, 1), "自分だけでも記録しない");
        assert!(record_reacquired(&db, 9, &[4, 2, 4, 9], true, 100));
        let rec = get(&db, 9).unwrap();
        assert_eq!((rec.kind, rec.targets.clone(), rec.mtime_match), (MarkerKind::Reacquired, vec![2, 4], true));
        assert_eq!(owners_of(&db, 2), vec![9]);
        assert_eq!(owners_of(&db, 4), vec![9]);
        assert!(owners_of(&db, 9).is_empty());
    }

    #[test]
    fn reacquired_overrides_moved_and_moved_never_overrides_reacquired() {
        let t = TempRoot::new("priority");
        let db = new_db(&t);
        record_moved(&db, 5, 100);
        // 移動のVPフラグは再取得で捨てる。
        {
            let mut rec = get(&db, 5).unwrap();
            rec.in_viewport = true;
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            tx.open_table(MARKER_TABLE).unwrap().insert(5, rec.encode().as_slice()).unwrap();
            tx.commit().unwrap();
        }
        assert!(record_reacquired(&db, 5, &[1], false, 200));
        let rec = get(&db, 5).unwrap();
        assert_eq!((rec.kind, rec.in_viewport, rec.detected_at), (MarkerKind::Reacquired, false, 200));
        assert!(!record_moved(&db, 5, 300), "再取得が優先");
        assert_eq!(get(&db, 5).unwrap().kind, MarkerKind::Reacquired);
    }

    #[test]
    fn reacquired_again_adds_targets_but_keeps_detection_and_dismissal() {
        let t = TempRoot::new("merge");
        let db = new_db(&t);
        record_reacquired(&db, 9, &[2], false, 100);
        {
            let mut rec = get(&db, 9).unwrap();
            rec.dismissed = true;
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            tx.open_table(MARKER_TABLE).unwrap().insert(9, rec.encode().as_slice()).unwrap();
            tx.commit().unwrap();
        }
        assert!(!record_reacquired(&db, 9, &[2], false, 500), "同じなら変えない");
        assert!(record_reacquired(&db, 9, &[3], true, 500));
        let rec = get(&db, 9).unwrap();
        assert_eq!((rec.targets.clone(), rec.dismissed, rec.detected_at, rec.mtime_match), (vec![2, 3], true, 100, true));
        assert_eq!(owners_of(&db, 3), vec![9]);
    }

    #[test]
    fn list_is_newest_first() {
        let t = TempRoot::new("list");
        let db = new_db(&t);
        record_moved(&db, 1, 100);
        record_reacquired(&db, 2, &[1], false, 300);
        record_moved(&db, 3, 200);
        assert_eq!(list(&db).iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 3, 1]);
    }

    #[test]
    fn remove_clears_row_and_reverse_entries() {
        let t = TempRoot::new("remove");
        let db = new_db(&t);
        record_reacquired(&db, 9, &[2, 3], false, 1);
        assert!(remove(&db, 9));
        assert!(!remove(&db, 9));
        assert!(get(&db, 9).is_none());
        assert!(owners_of(&db, 2).is_empty() && owners_of(&db, 3).is_empty());
    }

    #[test]
    fn forgetting_an_owner_removes_its_row_and_index() {
        let t = TempRoot::new("forget_owner");
        let db = new_db(&t);
        record_reacquired(&db, 9, &[2], false, 1);
        record_moved(&db, 8, 1);
        assert!(forget(&db, 9));
        assert!(get(&db, 9).is_none());
        assert!(owners_of(&db, 2).is_empty());
        assert!(get(&db, 8).is_some(), "他のマーカーは残る");
    }

    #[test]
    fn forgetting_a_target_trims_owners_and_drops_exhausted_reacquired() {
        let t = TempRoot::new("forget_target");
        let db = new_db(&t);
        record_reacquired(&db, 9, &[2, 3], false, 1);
        record_reacquired(&db, 10, &[2], false, 1);
        assert!(forget(&db, 2));
        // 9 は重複先が 3 だけに減って残る。10 は尽きて消える。
        assert_eq!(get(&db, 9).unwrap().targets, vec![3]);
        assert!(get(&db, 10).is_none());
        assert!(owners_of(&db, 2).is_empty());
        assert_eq!(owners_of(&db, 3), vec![9]);
        // 最後の重複先も消えたら9も消える。
        assert!(forget(&db, 3));
        assert!(get(&db, 9).is_none());
    }
}
