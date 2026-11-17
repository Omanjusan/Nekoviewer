//! 計測フィクスチャー（`perf_fixture`）が使うDBの退避・復元と、フォルダのID層クリア。開発専用。
//!
//! - 退避: クリアの直前に spread DB を丸ごと `perf_fixture/before.redb` へコピーする。**復元するまで初回のみ保持**
//!   （サムネのみ→フルと続けて計測しても、「最初のクリア前」に戻れる）
//! - 復元: 起動中のDBは差し替えず、予約ファイルを置いて次回起動時（DBを開く前）に適用する。
//!   適用時は現DBを `perf_fixture/after-measure.redb` へ退避し、退避の `before.redb` は消す
//! - クリア: 表示中フォルダ直下のファイルのIDを `delete_id` で消す（評価・しおり・お気に入り・重複マーカー・dHashも連動して消える）

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::dev_db_backup::{copy_db_atomic, copy_file_atomic, db_path, move_file, DevBackupError};

const DIR: &str = "perf_fixture";
const BEFORE_FILE: &str = "before.redb";
const AFTER_FILE: &str = "after-measure.redb";
const PENDING_FILE: &str = "nekoviewer_spread.redb.restore-pending-perf";

fn dir_path(root: &Path) -> PathBuf {
    root.join(DIR)
}

pub fn backup_path(root: &Path) -> PathBuf {
    dir_path(root).join(BEFORE_FILE)
}

fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_FILE)
}

pub fn has_backup(root: &Path) -> bool {
    backup_path(root).is_file()
}

pub fn restore_pending(root: &Path) -> bool {
    pending_path(root).is_file()
}

/// クリア前の退避。既に退避があれば何もしない（`Ok(false)`）。取ったら `Ok(true)`。
pub fn ensure_backup(db: &Arc<Mutex<Database>>, root: &Path) -> Result<bool, DevBackupError> {
    if has_backup(root) {
        return Ok(false);
    }
    copy_db_atomic(db, root, &backup_path(root), false)?;
    Ok(true)
}

/// 退避への復元を予約する。実際の差し替えは次回起動時。
pub fn request_restore(root: &Path) -> Result<(), DevBackupError> {
    if !has_backup(root) {
        return Err(DevBackupError::NoBaseline);
    }
    copy_file_atomic(&backup_path(root), &pending_path(root))?;
    Ok(())
}

/// 予約済みの復元を適用する。`open_spread_db` より前（DBが開かれていない状態）で呼ぶ。
/// 現DBの退避に失敗したら復元を中止し、現DBと予約を残す。戻り値は適用したかどうか。
pub fn apply_pending_restore(root: &Path) -> Result<bool, DevBackupError> {
    let pending = pending_path(root);
    if !pending.is_file() {
        return Ok(false);
    }
    let db = db_path(root);
    let mut evacuated: Option<PathBuf> = None;
    if db.is_file() {
        std::fs::create_dir_all(dir_path(root))?;
        let dest = dir_path(root).join(AFTER_FILE);
        let _ = std::fs::remove_file(&dest);
        move_file(&db, &dest)?;
        evacuated = Some(dest);
    }
    if let Err(e) = std::fs::rename(&pending, &db) {
        // 退避済みで本DBが無い状態を残さない（ベストエフォートで元へ戻す）。
        if let Some(dest) = evacuated {
            let _ = move_file(&dest, &db);
        }
        return Err(e.into());
    }
    // 復元が済んだので、次のクリアでは改めて退避を取る。
    let _ = std::fs::remove_file(backup_path(root));
    Ok(true)
}

/// フォルダ `dir` 直下のファイルのIDを全部消す（サブフォルダは対象外）。消した件数を返す。
/// 未解決リスト（`identity_pending`）の行も消す。ID層が未使用なら 0。
pub fn clear_dir_ids(db: &Arc<Mutex<Database>>, dir: &Path) -> usize {
    let prefix = crate::spread_state::dir_prefix(dir);
    let ids: Vec<u64> = {
        let Ok(guard) = db.lock() else { return 0 };
        let Ok(tx) = guard.begin_read() else { return 0 };
        crate::file_identity::dir_records_tx(&tx, &prefix).into_iter().map(|r| r.id).collect()
    };
    let mut cleared = 0;
    for id in ids {
        if crate::file_identity::delete_id(db, id).is_ok() {
            crate::identity_pending::remove(db, id);
            cleared += 1;
        }
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spread_state::{open_spread_db, read_archive_rating, write_archive_rating};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_perf_fixture_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rating_of(db: &Arc<Mutex<Database>>) -> Option<u8> {
        read_archive_rating(db, Path::new("/x"), "a.zip").map(|r| r.rating_half)
    }

    #[test]
    fn backup_is_kept_from_first_clear_until_restore() {
        let t = TempRoot::new("keep");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        assert_eq!(ensure_backup(&db, &t.0), Ok(true));
        // 2回目（サムネのみ→フルの連続計測）は上書きしない。
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 9));
        assert_eq!(ensure_backup(&db, &t.0), Ok(false));

        request_restore(&t.0).unwrap();
        assert!(restore_pending(&t.0));
        drop(db);
        assert_eq!(apply_pending_restore(&t.0), Ok(true));
        assert!(!restore_pending(&t.0));
        assert!(!has_backup(&t.0), "復元後は次のクリアで改めて退避する");
        assert!(dir_path(&t.0).join(AFTER_FILE).is_file());

        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(4), "最初のクリア前に戻る");
    }

    #[test]
    fn restore_without_backup_is_refused_and_apply_is_noop() {
        let t = TempRoot::new("nobackup");
        assert_eq!(request_restore(&t.0), Err(DevBackupError::NoBaseline));
        assert_eq!(apply_pending_restore(&t.0), Ok(false));
    }
}
