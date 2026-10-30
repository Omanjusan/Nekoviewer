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

use std::collections::HashMap;
use std::path::Path;
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
    /// ユーザーが正常として消した（下段フィルタの「消去済」で呼び戻せる）。
    pub dismissed: bool,
    /// 重複先と更新日時が一致していた（再取得の補助表示）。
    pub mtime_match: bool,
    /// 検出日時（unix秒）。
    pub detected_at: i64,
}

impl MarkerRecord {
    fn encode(&self) -> Vec<u8> {
        // bit3 は旧版が「完全一致を確認済み」に使っていた。今は使わず、読むときも無視する。
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

/// フォルダ内の1ファイルぶんの表示用情報。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirMarker {
    /// そのファイル自身のマーカー（移動・再取得。消去済みも含む。表示側で除く）。
    pub own: Option<MarkerRecord>,
    /// このファイルを重複先に持つ再取得マーカーのファイルID（昇順。消去済みは除く）。
    /// 空でなければ、旧ファイル側の「他に重複あり」を示す。
    pub referenced_by: Vec<u64>,
}

/// `dir` 直下のファイルのマーカーを一括で返す（キー = ファイル名）。マーカーも被参照も無いファイルは含めない。
/// テーブルが無ければ（旧DB・マーカー未検出）、IDを引かずに空を返す。
pub fn load_dir(db: &Arc<Mutex<Database>>, dir: &Path) -> HashMap<String, DirMarker> {
    let mut out = HashMap::new();
    let Ok(guard) = db.lock() else { return out };
    let Ok(tx) = guard.begin_read() else { return out };
    let (Ok(table), Ok(rev)) = (tx.open_table(MARKER_TABLE), tx.open_multimap_table(MARKER_TARGET_TABLE)) else {
        return out;
    };
    let prefix = crate::spread_state::dir_prefix(dir);
    for rec in crate::file_identity::dir_records_tx(&tx, &prefix) {
        let own = table.get(rec.id).ok().flatten().and_then(|g| MarkerRecord::decode(g.value()));
        let mut referenced_by: Vec<u64> = rev
            .get(rec.id)
            .map(|values| values.flatten().map(|g| g.value()).collect())
            .unwrap_or_default();
        referenced_by.retain(|owner| {
            let marker = table.get(*owner).ok().flatten().and_then(|g| MarkerRecord::decode(g.value()));
            marker.is_some_and(|m| !m.dismissed)
        });
        referenced_by.sort_unstable();
        if own.is_some() || !referenced_by.is_empty() {
            let name = rec.path_key[prefix.len()..].to_string();
            out.insert(name, DirMarker { own, referenced_by });
        }
    }
    out
}

/// `dir` 内の「表示領域に入った印が付いた移動マーカー」を消す。フォルダを開き直した時（再訪）に呼ぶ。
/// 消した件数を返す。
pub fn purge_seen_moved(db: &Arc<Mutex<Database>>, dir: &Path) -> usize {
    let ids: Vec<u64> = load_dir(db, dir)
        .into_values()
        .filter_map(|m| m.own)
        .filter(|r| r.kind == MarkerKind::Moved && r.in_viewport)
        .map(|r| r.id)
        .collect();
    if ids.is_empty() {
        return 0;
    }
    let Ok(guard) = db.lock() else { return 0 };
    let Ok(tx) = guard.begin_write() else { return 0 };
    let removed = ids.iter().filter(|id| forget_owner(&tx, **id) == Some(true)).count();
    if tx.commit().is_err() {
        return 0;
    }
    removed
}

/// 再取得マーカーを「正常として消す」。行は残し、消去済みにする（下段フィルタの「消去済」で呼び戻せる）。
/// 移動マーカー（再訪で自然に消える）と、既に消去済み・無いものは何もしない。消去済みにしたら true。
pub fn dismiss(db: &Arc<Mutex<Database>>, id: u64) -> bool {
    write(db, |table, _rev| {
        let rec = table.get(id).ok()?.and_then(|g| MarkerRecord::decode(g.value()));
        let Some(rec) = rec.filter(|r| r.kind == MarkerKind::Reacquired && !r.dismissed) else {
            return Some(false);
        };
        table.insert(id, MarkerRecord { dismissed: true, ..rec }.encode().as_slice()).ok()?;
        Some(true)
    })
}

/// 表示領域に入った移動マーカーに印を付ける（フォルダ離脱・終了時にまとめて呼ぶ）。印が付いた件数を返す。
/// 移動以外（再取得は永続）と、既に印のあるもの・無いものは何もしない。
pub fn mark_in_viewport(db: &Arc<Mutex<Database>>, ids: &[u64]) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let Ok(guard) = db.lock() else { return 0 };
    let Ok(tx) = guard.begin_write() else { return 0 };
    let mut marked = 0;
    {
        // 台帳が無い（移動を一度も検出していない）DBでは、テーブルを作らずに戻る。
        if !tx.list_tables().is_ok_and(|mut t| t.any(|h| h.name() == MARKER_TABLE.name())) {
            return 0;
        }
        let Ok(mut table) = tx.open_table(MARKER_TABLE) else { return 0 };
        for id in ids {
            let rec = table.get(*id).ok().flatten().and_then(|g| MarkerRecord::decode(g.value()));
            let Some(rec) = rec.filter(|r| r.kind == MarkerKind::Moved && !r.in_viewport) else { continue };
            let flagged = MarkerRecord { in_viewport: true, ..rec };
            if table.insert(*id, flagged.encode().as_slice()).is_ok() {
                marked += 1;
            }
        }
    }
    if tx.commit().is_err() {
        return 0;
    }
    marked
}

/// 1つのマーカーに持たせる重複先の上限（IDの小さい順）。同じ内容のファイルが大量にあっても、台帳が膨らまない。
const MAX_TARGETS: usize = 16;

/// `dir` 直下のファイルについて、同じ内容（FP一致）で現存する別のファイルを探し、再取得マーカーを付ける
/// （遡及検出）。導入前に登録済みのファイルや、データの無い同内容ファイルは、解決時には記録されないため。
/// - 同じ内容の現存ファイルが2つ以上あれば、IDの大きい方（後から登録された方）が再取得側になる
/// - 重複先は、その持ち主よりIDの小さい現存ファイル。他のフォルダの持ち主にも付ける
/// - 消去済みの印は消さない（ユーザーが正常と判断した後に、また警告しない）
/// 新しく行ができた件数を返す（既存のマーカーに重複先を足しただけの場合は数えない）。
pub fn detect_in_dir(
    db: &Arc<Mutex<Database>>,
    dir: &Path,
    probe: &dyn Fn(&crate::file_identity::FileRecord) -> crate::file_identity::Presence,
    now: i64,
) -> usize {
    use crate::file_identity::{FileRecord, FileStatus, Presence};
    // 1. 読み取り: フォルダの各ファイルについて、同じFPのレコードを集める（複数あるものだけ）。
    let prefix = crate::spread_state::dir_prefix(dir);
    let mut groups: HashMap<crate::file_identity::Fp, Vec<FileRecord>> = HashMap::new();
    {
        let Ok(guard) = db.lock() else { return 0 };
        let Ok(tx) = guard.begin_read() else { return 0 };
        for rec in crate::file_identity::dir_records_tx(&tx, &prefix) {
            let Some(fp) = rec.fp.filter(|_| rec.status == FileStatus::Confirmed) else { continue };
            if groups.contains_key(&fp) {
                continue;
            }
            let mut members = crate::file_identity::records_with_fp_tx(&tx, &fp);
            // 同じパスを指す古いレコードは、別のファイルとは数えない。
            members.sort_by_key(|m| m.id);
            members.dedup_by(|a, b| a.path_key == b.path_key);
            if members.len() > 1 {
                groups.insert(fp, members);
            }
        }
    }
    // 2. ロックの外で、現存を確認する（オフライン・消失は重複と見なさない）。
    let mut presence: HashMap<u64, Presence> = HashMap::new();
    let mut created = 0;
    for members in groups.into_values() {
        let live: Vec<&FileRecord> = members
            .iter()
            .filter(|m| *presence.entry(m.id).or_insert_with(|| probe(m)) == Presence::Present)
            .collect();
        // 3. 書き込み: 2番目以降の現存ファイルを、それより小さいIDの現存ファイルの再取得として記録する。
        for (i, owner) in live.iter().enumerate().skip(1) {
            let targets: Vec<u64> = live[..i].iter().take(MAX_TARGETS).map(|m| m.id).collect();
            let mtime_match = live[0].mtime == owner.mtime;
            let existed = get(db, owner.id).is_some();
            if record_reacquired(db, owner.id, &targets, mtime_match, now) && !existed {
                created += 1;
            }
        }
    }
    created
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

    /// フォルダ内の実ファイルを登録してIDを返す（FPが違うので互いに統合されない）。
    fn register(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, fill: u8) -> u64 {
        std::fs::write(dir.join(name), vec![fill; 64]).unwrap();
        // 書いたばかりのファイルは書き込み中を疑ってFPを取らないので、古い更新日時にする。
        let f = std::fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        crate::file_identity::ensure_record(db, dir, name).unwrap().id
    }

    fn set_viewport(db: &Arc<Mutex<Database>>, id: u64) {
        let mut rec = get(db, id).unwrap();
        rec.in_viewport = true;
        let g = db.lock().unwrap();
        let tx = g.begin_write().unwrap();
        tx.open_table(MARKER_TABLE).unwrap().insert(id, rec.encode().as_slice()).unwrap();
        tx.commit().unwrap();
    }

    fn dirs(t: &TempRoot) -> (PathBuf, PathBuf) {
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        (a, b)
    }

    fn real_probe(rec: &crate::file_identity::FileRecord) -> crate::file_identity::Presence {
        crate::file_identity::probe_record(rec)
    }

    #[test]
    fn detect_marks_the_later_registered_file_even_without_user_data() {
        let t = TempRoot::new("detect_basic");
        let db = new_db(&t);
        let (da, dbb) = dirs(&t);
        let old = register(&db, &da, "x.zip", 7);
        let new = register(&db, &dbb, "y.zip", 7);
        assert!(new > old);
        assert!(get(&db, new).is_none(), "データが無いので解決時には記録されない");
        assert_eq!(detect_in_dir(&db, &dbb, &real_probe, 100), 1);
        let m = get(&db, new).unwrap();
        assert_eq!((m.kind, m.targets.clone(), m.detected_at), (MarkerKind::Reacquired, vec![old], 100));
        assert!(get(&db, old).is_none(), "古い方には自身のマーカーを付けない");
        assert_eq!(owners_of(&db, old), vec![new]);
        assert_eq!(detect_in_dir(&db, &dbb, &real_probe, 200), 0, "二度目は何も変えない");
    }

    #[test]
    fn detect_from_the_older_files_folder_marks_the_newer_file_in_another_folder() {
        let t = TempRoot::new("detect_other_folder");
        let db = new_db(&t);
        let (da, dbb) = dirs(&t);
        let old = register(&db, &da, "x.zip", 7);
        let new = register(&db, &dbb, "y.zip", 7);
        assert_eq!(detect_in_dir(&db, &da, &real_probe, 1), 1);
        assert_eq!(get(&db, new).unwrap().targets, vec![old]);
    }

    #[test]
    fn detect_ignores_missing_duplicates_and_different_content() {
        let t = TempRoot::new("detect_none");
        let db = new_db(&t);
        let (da, dbb) = dirs(&t);
        register(&db, &da, "x.zip", 7);
        register(&db, &dbb, "y.zip", 8);
        assert_eq!(detect_in_dir(&db, &dbb, &real_probe, 1), 0, "中身が違う");
        register(&db, &dbb, "z.zip", 7);
        std::fs::remove_file(da.join("x.zip")).unwrap();
        assert_eq!(detect_in_dir(&db, &dbb, &real_probe, 1), 0, "相手が消えている");
        assert!(list(&db).is_empty());
    }

    #[test]
    fn detect_chains_three_copies_and_keeps_the_dismissed_flag() {
        let t = TempRoot::new("detect_three");
        let db = new_db(&t);
        let (da, dbb) = dirs(&t);
        let a = register(&db, &da, "a.zip", 7);
        let b = register(&db, &dbb, "b.zip", 7);
        assert_eq!(detect_in_dir(&db, &da, &real_probe, 1), 1);
        assert!(dismiss(&db, b));
        let c = register(&db, &dbb, "c.zip", 7);
        assert_eq!(detect_in_dir(&db, &dbb, &real_probe, 2), 1, "新しい行はcだけ");
        assert_eq!(get(&db, c).unwrap().targets, vec![a, b]);
        assert!(get(&db, b).unwrap().dismissed, "消去済みはそのまま");
        assert_eq!(get(&db, b).unwrap().targets, vec![a]);
    }

    #[test]
    fn detect_does_not_probe_files_with_a_unique_fingerprint() {
        let t = TempRoot::new("detect_unique");
        let db = new_db(&t);
        let (da, _) = dirs(&t);
        for (i, name) in ["a.zip", "b.zip", "c.zip"].iter().enumerate() {
            register(&db, &da, name, i as u8 + 1);
        }
        let probes = std::cell::Cell::new(0);
        let counting = |_: &crate::file_identity::FileRecord| {
            probes.set(probes.get() + 1);
            crate::file_identity::Presence::Present
        };
        assert_eq!(detect_in_dir(&db, &da, &counting, 1), 0);
        assert_eq!(probes.get(), 0, "重複の疑いが無いファイルは存在確認もしない");
    }

    #[test]
    fn load_dir_returns_own_and_referenced_markers_by_file_name() {
        let t = TempRoot::new("load_dir");
        let db = new_db(&t);
        let dir = t.0.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        let other = t.0.join("e");
        std::fs::create_dir_all(&other).unwrap();
        let old = register(&db, &dir, "old.zip", 1);
        let new = register(&db, &other, "new.zip", 2);
        let plain = register(&db, &dir, "plain.zip", 3);
        record_reacquired(&db, new, &[old], false, 10);

        let d = load_dir(&db, &dir);
        assert_eq!(d.keys().cloned().collect::<Vec<_>>(), vec!["old.zip".to_owned()], "{d:?}");
        assert_eq!(d["old.zip"].referenced_by, vec![new]);
        assert!(d["old.zip"].own.is_none());
        let e = load_dir(&db, &other);
        assert_eq!(e["new.zip"].own.as_ref().unwrap().kind, MarkerKind::Reacquired);
        assert!(e["new.zip"].referenced_by.is_empty());
        assert!(!d.contains_key("plain.zip") && plain != 0);
    }

    #[test]
    fn load_dir_hides_references_from_dismissed_markers() {
        let t = TempRoot::new("load_dir_dismissed");
        let db = new_db(&t);
        let dir = t.0.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        let old = register(&db, &dir, "old.zip", 1);
        let new = register(&db, &dir, "new.zip", 2);
        record_reacquired(&db, new, &[old], false, 10);
        {
            let mut rec = get(&db, new).unwrap();
            rec.dismissed = true;
            let g = db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            tx.open_table(MARKER_TABLE).unwrap().insert(new, rec.encode().as_slice()).unwrap();
            tx.commit().unwrap();
        }
        let d = load_dir(&db, &dir);
        assert!(!d.contains_key("old.zip"), "消去済みの再取得は旧側にも出さない");
        assert!(d["new.zip"].own.as_ref().unwrap().dismissed, "自身の行は消去済みとして残る");
    }

    #[test]
    fn load_dir_on_a_legacy_db_is_empty_and_creates_nothing() {
        let t = TempRoot::new("load_dir_lazy");
        let db = new_db(&t);
        let dir = t.0.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        register(&db, &dir, "a.zip", 1);
        assert!(load_dir(&db, &dir).is_empty());
        assert_eq!(purge_seen_moved(&db, &dir), 0);
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name().starts_with("duplicate_marker")));
    }

    #[test]
    fn purge_seen_moved_removes_only_flagged_moved_markers_in_that_dir() {
        let t = TempRoot::new("purge");
        let db = new_db(&t);
        let dir = t.0.join("d");
        let other = t.0.join("e");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let seen = register(&db, &dir, "seen.zip", 1);
        let unseen = register(&db, &dir, "unseen.zip", 2);
        let re = register(&db, &dir, "re.zip", 3);
        let elsewhere = register(&db, &other, "x.zip", 4);
        for id in [seen, unseen, elsewhere] {
            record_moved(&db, id, 1);
        }
        record_reacquired(&db, re, &[unseen], false, 1);
        for id in [seen, re, elsewhere] {
            set_viewport(&db, id);
        }
        assert_eq!(purge_seen_moved(&db, &dir), 1);
        assert!(get(&db, seen).is_none(), "見た移動マーカーは消える");
        assert!(get(&db, unseen).is_some(), "まだ見ていない移動マーカーは残る");
        assert!(get(&db, re).is_some(), "再取得はVPフラグがあっても消えない");
        assert!(get(&db, elsewhere).is_some(), "別フォルダは触らない");
    }

    #[test]
    fn legacy_verified_bit_is_ignored_when_reading() {
        let rec = MarkerRecord {
            id: 7,
            kind: MarkerKind::Reacquired,
            targets: vec![1],
            in_viewport: false,
            dismissed: false,
            mtime_match: false,
            detected_at: 3,
        };
        let mut bytes = rec.encode();
        bytes[2] |= 8;
        assert_eq!(MarkerRecord::decode(&bytes), Some(rec), "旧版が立てた bit3 は無視して読める");
    }

    #[test]
    fn dismiss_keeps_the_row_but_only_for_reacquired() {
        let t = TempRoot::new("dismiss");
        let db = new_db(&t);
        record_reacquired(&db, 9, &[2], false, 1);
        record_moved(&db, 8, 1);
        assert!(!dismiss(&db, 8), "移動は対象外");
        assert!(!dismiss(&db, 77), "無いものは何もしない");
        assert!(dismiss(&db, 9));
        assert!(!dismiss(&db, 9), "二重には消去しない");
        let rec = get(&db, 9).unwrap();
        assert!(rec.dismissed);
        assert_eq!(rec.targets, vec![2], "重複先は残す（呼び戻し用）");
        assert_eq!(owners_of(&db, 2), vec![9]);
    }

    #[test]
    fn mark_in_viewport_flags_only_unflagged_moved_markers() {
        let t = TempRoot::new("mark_vp");
        let db = new_db(&t);
        assert_eq!(mark_in_viewport(&db, &[1]), 0, "台帳が無ければ何もしない");
        record_moved(&db, 1, 10);
        record_moved(&db, 2, 10);
        record_reacquired(&db, 3, &[1], false, 10);
        assert_eq!(mark_in_viewport(&db, &[1, 3, 99]), 1, "再取得と未登録は対象外");
        assert!(get(&db, 1).unwrap().in_viewport);
        assert!(!get(&db, 2).unwrap().in_viewport);
        assert!(!get(&db, 3).unwrap().in_viewport);
        assert_eq!(mark_in_viewport(&db, &[1]), 0, "二重には付けない");
        let g = db.lock().unwrap();
        let tx = g.begin_read().unwrap();
        assert_eq!(MarkerRecord::decode(tx.open_table(MARKER_TABLE).unwrap().get(1).unwrap().unwrap().value()).unwrap().detected_at, 10);
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
