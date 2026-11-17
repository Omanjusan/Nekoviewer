//! 自動付与の記録。一括タグ取得で自動的に付けたタグIDを、ファイルごとに残す。
//!
//! - 目的: 再抽出のとき、「前回の自動付与分だけ」を外して付け直せるようにする（手動で付けたタグには触れない）。
//! - 記録するのは「自動付与で**足した**ID」だけ（付ける前から付いていたタグは含めない）。試用の確定のときに、
//!   試用の記録（`TrialRecord`）から移す。破棄したときは、タグごと外すので記録しない
//! - 保存先はDBの新規テーブル `auto_tag_added_v1`。キーは既存のファイルID（ID+FP照合で、移動・改名に追従する）。
//!   値はタグIDの並び。世代バックアップ・リカバリポイントはDBごと保存するので、そのまま同梱される
//! - 同じファイルへ再び足したときは、前の記録に和集合で足す（前回分がまだ付いているため）
//! - 手動で付けた分は記録から外す（`remove_ids`。タグパネルの手動の保存が呼ぶ）。付け直し（P3）は、記録のIDだけを外す

use std::path::Path;
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase, TableDefinition, TableHandle, WriteTransaction};

use crate::tag_trial::TrialRecord;

pub(crate) const AUTO_TAG_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("auto_tag_added_v1");

fn encode(ids: &[u64]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + ids.len() * 8);
    b.extend((ids.len() as u32).to_le_bytes());
    for id in ids {
        b.extend(id.to_le_bytes());
    }
    b
}

fn decode(bytes: &[u8]) -> Vec<u64> {
    let Some(head) = bytes.get(..4) else { return Vec::new() };
    let n = u32::from_le_bytes(head.try_into().unwrap_or([0; 4])) as usize;
    (0..n).filter_map(|i| bytes.get(4 + i * 8..12 + i * 8)).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))).collect()
}

/// ファイルIDで引く。記録が無い・テーブルが無いときは空。
pub(crate) fn read_by_id(db: &Database, id: u64) -> Vec<u64> {
    let Ok(tx) = db.begin_read() else { return Vec::new() };
    let Ok(t) = tx.open_table(AUTO_TAG_TABLE) else { return Vec::new() };
    t.get(id).ok().flatten().map(|g| decode(g.value())).unwrap_or_default()
}

/// ファイルの記録を返す。実体からIDを解決し直すので、移動・改名のあとでも引ける。IDを解決できないときは空。
pub(crate) fn read_resolved(db: &Arc<Mutex<Database>>, dir: &Path, name: &str) -> Vec<u64> {
    let Some(rec) = crate::file_identity::ensure_record(db, dir, name) else { return Vec::new() };
    let Ok(guard) = db.lock() else { return Vec::new() };
    read_by_id(&guard, rec.id)
}

/// ファイルの記録を返す。ファイルにIDがまだ無い（追えない）ときは空。IDは作らない。
#[cfg(test)]
pub(crate) fn read(db: &Arc<Mutex<Database>>, dir: &Path, name: &str) -> Vec<u64> {
    let key = crate::spread_state::make_key(dir, name);
    let Some(rec) = crate::file_identity::lookup(db, &key) else { return Vec::new() };
    let Ok(guard) = db.lock() else { return Vec::new() };
    read_by_id(&guard, rec.id)
}

fn write_ids(db: &Arc<Mutex<Database>>, id: u64, ids: &[u64]) -> bool {
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let ok = (|| {
        let mut t = tx.open_table(AUTO_TAG_TABLE).ok()?;
        if ids.is_empty() {
            t.remove(id).ok()?;
        } else {
            t.insert(id, encode(ids).as_slice()).ok()?;
        }
        Some(())
    })()
    .is_some();
    ok && tx.commit().is_ok()
}

/// 自動付与で足したIDを記録する（前の記録との和集合）。ファイルのIDを解決できないときは false。
pub(crate) fn add(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, ids: &[u64]) -> bool {
    if ids.is_empty() {
        return true;
    }
    let Some(rec) = crate::file_identity::ensure_record(db, dir, name) else { return false };
    let mut all = match db.lock() {
        Ok(guard) => read_by_id(&guard, rec.id),
        Err(_) => return false,
    };
    all.extend_from_slice(ids);
    all.sort_unstable();
    all.dedup();
    write_ids(db, rec.id, &all)
}

/// 記録から指定のIDを外す（手動で付けたタグを、自動付与の記録から外すとき）。記録が無ければ何もしない。成否を返す。
pub(crate) fn remove_ids(db: &Arc<Mutex<Database>>, dir: &Path, name: &str, ids: &[u64]) -> bool {
    let key = crate::spread_state::make_key(dir, name);
    let Some(rec) = crate::file_identity::lookup(db, &key) else { return true };
    let current = match db.lock() {
        Ok(guard) => read_by_id(&guard, rec.id),
        Err(_) => return false,
    };
    let kept: Vec<u64> = current.iter().copied().filter(|id| !ids.contains(id)).collect();
    if kept == current {
        return true;
    }
    write_ids(db, rec.id, &kept)
}

/// 全ファイルの記録を消す（「タグを全部クリア」のあと。タグが無いので記録も意味を失う）。成否を返す。
#[cfg(test)]
pub(crate) fn clear_all(db: &Arc<Mutex<Database>>) -> bool {
    let Ok(guard) = db.lock() else { return false };
    let Ok(tx) = guard.begin_write() else { return false };
    let ok = tx.delete_table(AUTO_TAG_TABLE).is_ok() && tx.open_table(AUTO_TAG_TABLE).is_ok();
    ok && tx.commit().is_ok()
}

/// IDが消える時の連動（`delete_id` の書き込みトランザクションの中で呼ぶ）。テーブルが無ければ何もしない。Some = 成功。
pub(crate) fn forget_id_tx(tx: &WriteTransaction, id: u64) -> Option<()> {
    let exists = tx.list_tables().ok()?.any(|h| h.name() == AUTO_TAG_TABLE.name());
    if !exists {
        return Some(());
    }
    tx.open_table(AUTO_TAG_TABLE).ok()?.remove(id).ok()?;
    Some(())
}

/// 記録した件数と、記録できなかった件数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecordReport {
    pub recorded: usize,
    pub failed: usize,
}

/// 試用の記録（適用で足したID）を、自動付与の記録へ移す。試用の確定のときに呼ぶ。
pub(crate) fn record_from_trial(db: &Arc<Mutex<Database>>, record: &TrialRecord) -> RecordReport {
    let mut r = RecordReport::default();
    for f in &record.files {
        if f.added.is_empty() && f.removed.is_empty() {
            continue;
        }
        // 付け直しで外した前回分は、記録から外してから、今回足した分を記録する（外して付け直したIDは、改めて記録される）。
        if remove_ids(db, &f.dir, &f.name, &f.removed) && add(db, &f.dir, &f.name, &f.added) {
            r.recorded += 1;
        } else {
            r.failed += 1;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_trial::TrialFile;
    use std::path::PathBuf;

    fn real_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("nekoviewer_auto_tag_test_{}_{}_{}", std::process::id(), nonce, tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn real_file(dir: &Path, name: &str, len: usize) {
        let p = dir.join(name);
        std::fs::write(&p, (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>()).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
    }

    fn new_db(dir: &Path) -> Arc<Mutex<Database>> {
        let db = crate::spread_state::open_spread_db(dir).unwrap();
        crate::favorites::init_favorite_tables(&db).unwrap();
        db
    }

    #[test]
    fn codec_round_trips_and_tolerates_garbage() {
        assert_eq!(decode(&encode(&[3, 9, u64::MAX])), vec![3, 9, u64::MAX]);
        assert_eq!(decode(&encode(&[])), Vec::<u64>::new());
        assert_eq!(decode(&[]), Vec::<u64>::new());
        assert_eq!(decode(&[5, 0, 0, 0, 1]), Vec::<u64>::new(), "件数に足りない壊れた値は、読める分（0件）だけ");
    }

    #[test]
    fn add_unions_removes_and_follows_the_file_id() {
        let dir = real_dir("add");
        real_file(&dir, "a.zip", 400);
        let db = new_db(&dir);
        assert!(read(&db, &dir, "a.zip").is_empty(), "IDが無いうちは空");
        assert!(add(&db, &dir, "a.zip", &[7, 3]));
        assert_eq!(read(&db, &dir, "a.zip"), vec![3, 7]);
        // 再び足すと和集合（重複なし）。
        assert!(add(&db, &dir, "a.zip", &[7, 11]));
        assert_eq!(read(&db, &dir, "a.zip"), vec![3, 7, 11]);
        // 手動で付けたIDを外す。無いIDを外しても壊れない。
        assert!(remove_ids(&db, &dir, "a.zip", &[7, 99]));
        assert_eq!(read(&db, &dir, "a.zip"), vec![3, 11]);
        assert!(remove_ids(&db, &dir, "a.zip", &[3, 11]));
        assert!(read(&db, &dir, "a.zip").is_empty());
        // 記録の無いファイルに対する remove は成功扱い。
        assert!(remove_ids(&db, &dir, "none.zip", &[1]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_survives_a_rename_of_the_file() {
        let dir = real_dir("rename");
        real_file(&dir, "old.zip", 600);
        let db = new_db(&dir);
        assert!(add(&db, &dir, "old.zip", &[5]));
        std::fs::rename(dir.join("old.zip"), dir.join("new.zip")).unwrap();
        // 実体のIDを解決し直すと、改名後も同じ記録が引ける。
        crate::file_identity::ensure_record(&db, &dir, "new.zip").expect("resolved");
        assert_eq!(read(&db, &dir, "new.zip"), vec![5]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_from_trial_moves_added_ids_and_counts_unresolvable_files() {
        let dir = real_dir("trial");
        real_file(&dir, "a.zip", 300);
        real_file(&dir, "b.zip", 500);
        let db = new_db(&dir);
        let rec = TrialRecord {
            files: vec![
                TrialFile { dir: dir.clone(), name: "a.zip".into(), added: vec![1, 2], removed: vec![] },
                TrialFile { dir: dir.clone(), name: "b.zip".into(), added: vec![], removed: vec![] },
                TrialFile { dir: dir.clone(), name: "gone.zip".into(), added: vec![4], removed: vec![] },
            ],
        };
        let r = record_from_trial(&db, &rec);
        assert_eq!(r, RecordReport { recorded: 1, failed: 1 });
        assert_eq!(read(&db, &dir, "a.zip"), vec![1, 2]);
        assert!(read(&db, &dir, "b.zip").is_empty(), "足したIDが無いファイルは記録しない");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_id_and_clear_all_drop_the_records() {
        let dir = real_dir("gc");
        real_file(&dir, "a.zip", 300);
        real_file(&dir, "b.zip", 500);
        let db = new_db(&dir);
        assert!(add(&db, &dir, "a.zip", &[1]));
        assert!(add(&db, &dir, "b.zip", &[2]));
        let id_a = crate::file_identity::lookup(&db, &crate::spread_state::make_key(&dir, "a.zip")).unwrap().id;
        crate::file_identity::delete_id(&db, id_a).unwrap();
        assert!(read_by_id(&db.lock().unwrap(), id_a).is_empty(), "IDを消すと記録も消える");
        assert_eq!(read(&db, &dir, "b.zip"), vec![2]);
        // 「タグを全部クリア」（clear_all_tags）も、同じトランザクションで記録を消す。
        assert!(crate::file_settings::clear_all_tags(&db).is_some());
        assert!(read(&db, &dir, "b.zip").is_empty());
        // クリア後も、また足せる（テーブルは作り直されている）。
        assert!(add(&db, &dir, "b.zip", &[8]));
        assert_eq!(read(&db, &dir, "b.zip"), vec![8]);
        assert!(clear_all(&db));
        assert!(read(&db, &dir, "b.zip").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
