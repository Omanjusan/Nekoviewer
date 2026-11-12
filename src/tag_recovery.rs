//! タグのリカバリポイント（固定枠1つ）。タグマスター（JSON）とデータベース全体を、対にして保存する。主にデバッグ用。
//!
//! - 置き場所は `tag_master_gen/recovery/`（`tags.json` と `nekoviewer_spread.redb`）。世代のローテーションの対象外
//! - 設定は、いまのマスターのファイルとDBを丸ごとコピーする。作り直しは別の場所に組み立ててから差し替えるので、
//!   途中で失敗しても前のリカバリポイントは壊れない
//! - 復元は予約して、次回起動時（DBを開く前）にマスターとDBをまとめて適用する。起動中にマスターだけ戻すと、
//!   DBのタグ付けと食い違うため。適用の前に、いまのDBを `backup_auto/before-restore-*.redb` へ退避し、
//!   いまのマスターを世代に残す
//! - 世代（`db_backup`）の復元予約とは同時に持たない（後から予約した方が有効）

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::Database;

use crate::dev_db_backup::{copy_db_atomic, copy_file_atomic, DevBackupError};

const DIR_NAME: &str = "recovery";
const STAGING_NAME: &str = "recovery.tmp";
const OLD_NAME: &str = "recovery.old";
const MASTER_FILE: &str = "tags.json";
const DB_FILE: &str = "nekoviewer_spread.redb";
const PENDING_FILE: &str = "nekoviewer_spread.restore-pending-recovery";

/// リカバリポイントの情報（表示用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryInfo {
    /// 設定した日時（unix秒）。
    pub modified_unix: i64,
    pub db_size: u64,
    pub master_size: u64,
}

fn base_dir(root: &Path) -> PathBuf {
    root.join("tag_master_gen")
}

fn dir(root: &Path) -> PathBuf {
    base_dir(root).join(DIR_NAME)
}

fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_FILE)
}

/// 設定済みのリカバリポイントの情報。マスターかDBのどちらかが欠けていれば無いものとして扱う。
pub(crate) fn info(root: &Path) -> Option<RecoveryInfo> {
    let d = dir(root);
    let master = std::fs::metadata(d.join(MASTER_FILE)).ok().filter(|m| m.is_file())?;
    let db = std::fs::metadata(d.join(DB_FILE)).ok().filter(|m| m.is_file())?;
    let modified_unix = master
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    Some(RecoveryInfo { modified_unix, db_size: db.len(), master_size: master.len() })
}

/// いまのマスター（保存済みのファイル）とDBを、リカバリポイントとして保存する。既にあれば置き換える。
/// DBのコピー中はDBのMutexを保持する。
pub(crate) fn set(db: &Arc<Mutex<Database>>, root: &Path) -> Result<RecoveryInfo, DevBackupError> {
    let master_src = crate::tag_manager::real_path(root);
    if !master_src.is_file() {
        return Err(DevBackupError::Io("tag master file is missing".into()));
    }
    let base = base_dir(root);
    let (staging, current, old) = (base.join(STAGING_NAME), dir(root), base.join(OLD_NAME));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let built = (|| -> Result<(), DevBackupError> {
        copy_file_atomic(&master_src, &staging.join(MASTER_FILE))?;
        copy_db_atomic(db, root, &staging.join(DB_FILE), false)?;
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    // 前のものを退けてから、組み立てたものを置く。置けなければ前のものへ戻す。
    let _ = std::fs::remove_dir_all(&old);
    let had_old = current.exists();
    if had_old {
        std::fs::rename(&current, &old)?;
    }
    if let Err(e) = std::fs::rename(&staging, &current) {
        if had_old {
            let _ = std::fs::rename(&old, &current);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old);
    info(root).ok_or_else(|| DevBackupError::Io("recovery point missing after save".into()))
}

/// リカバリポイントを削除する（復元の予約も取り消す）。無ければ何もしない。
pub(crate) fn delete(root: &Path) -> std::io::Result<()> {
    cancel_restore(root)?;
    match std::fs::remove_dir_all(dir(root)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 復元が予約されているか。
pub(crate) fn restore_pending(root: &Path) -> bool {
    pending_path(root).is_file()
}

/// 復元の予約を取り消す。予約が無ければ何もしない。
pub(crate) fn cancel_restore(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(pending_path(root)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// リカバリポイントへの復元を予約する（適用は次回起動時）。世代の復元の予約があれば取り消す。
pub(crate) fn request_restore(root: &Path) -> Result<(), DevBackupError> {
    if info(root).is_none() {
        return Err(DevBackupError::NoBaseline);
    }
    let _ = crate::db_backup::cancel_restore(root);
    std::fs::write(pending_path(root), b"")?;
    Ok(())
}

/// 予約済みの復元を適用する。`open_spread_db` より前（DBが開かれていない状態）で呼ぶ。戻り値は適用したかどうか。
/// 手順: ①リカバリポイントが揃っているか確認（欠けていれば予約を捨てる）②いまのマスターを世代に残す③DBを差し替える
/// （失敗したら何も変えず、予約を残す）④予約を消す⑤試用の仮マスターがあれば捨てる⑥マスターを置き換える。
pub(crate) fn apply_pending(root: &Path, master_keep: usize) -> Result<bool, DevBackupError> {
    if !restore_pending(root) {
        return Ok(false);
    }
    let Some(_) = info(root) else {
        let _ = cancel_restore(root);
        return Err(DevBackupError::NoBaseline);
    };
    let d = dir(root);
    crate::tag_master_gen::snapshot_before_save(root, master_keep, true);
    crate::db_backup::replace_db_with(root, &d.join(DB_FILE), true)?;
    let _ = cancel_restore(root);
    // 復元したDBに、試用中のタグ付けは無い。仮マスターと記録は捨てる。
    if crate::tag_manager::trial_exists(root) {
        let _ = crate::tag_manager::discard_trial(root);
    }
    crate::tag_trial::TrialRecord::remove(root);
    copy_file_atomic(&d.join(MASTER_FILE), &crate::tag_manager::real_path(root))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_manager::{save_slot, Slot};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_tag_recovery_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn save_master(root: &Path, next: u64) {
        let (cats, _) = crate::tag_manager::default_state();
        save_slot(root, Slot::Real, &cats, next);
    }

    fn master_next(root: &Path) -> u64 {
        crate::tag_manager::load_path(&crate::tag_manager::real_path(root)).unwrap().1
    }

    /// DBを開いたまま、マーカーのレコードを1件書く（復元で戻る/戻らないの判定用）。
    fn db_with_rating(root: &Path, stars: u8) -> Arc<Mutex<Database>> {
        let db = crate::spread_state::open_spread_db(root).expect("db");
        crate::spread_state::write_archive_rating(&db, Path::new("/x"), "a.zip", stars);
        db
    }

    #[test]
    fn set_needs_a_saved_master_and_info_needs_both_files() {
        let t = TempRoot::new("set_needs");
        let db = db_with_rating(&t.0, 3);
        assert!(set(&db, &t.0).is_err(), "マスターが無ければ設定できない");
        assert!(info(&t.0).is_none());
        save_master(&t.0, 10);
        let i = set(&db, &t.0).expect("set");
        assert!(i.db_size > 0 && i.master_size > 0);
        assert_eq!(info(&t.0), Some(i));
        // 片方が欠けたら無いものとして扱う。
        std::fs::remove_file(dir(&t.0).join(DB_FILE)).unwrap();
        assert!(info(&t.0).is_none());
        assert!(request_restore(&t.0).is_err());
    }

    #[test]
    fn set_replaces_the_previous_point_and_delete_clears_it() {
        let t = TempRoot::new("replace");
        let db = db_with_rating(&t.0, 3);
        save_master(&t.0, 10);
        set(&db, &t.0).unwrap();
        save_master(&t.0, 20);
        set(&db, &t.0).unwrap();
        let kept = crate::tag_manager::load_path(&dir(&t.0).join(MASTER_FILE)).unwrap().1;
        assert_eq!(kept, 20, "新しい方で置き換わる");
        assert!(!base_dir(&t.0).join(OLD_NAME).exists() && !base_dir(&t.0).join(STAGING_NAME).exists());
        request_restore(&t.0).unwrap();
        assert!(restore_pending(&t.0));
        delete(&t.0).unwrap();
        assert!(info(&t.0).is_none() && !restore_pending(&t.0), "削除で予約も消える");
        delete(&t.0).unwrap();
    }

    #[test]
    fn apply_restores_master_and_db_together_and_keeps_the_point() {
        let t = TempRoot::new("apply");
        {
            let db = db_with_rating(&t.0, 3);
            save_master(&t.0, 10);
            set(&db, &t.0).unwrap();
            // 設定後に、マスターもDBも変える。
            save_master(&t.0, 99);
            crate::spread_state::write_archive_rating(&db, Path::new("/x"), "a.zip", 5);
        }
        assert!(!apply_pending(&t.0, 5).unwrap(), "予約が無ければ何もしない");
        request_restore(&t.0).unwrap();
        // DBが開かれていない状態（起動時）で適用する。
        assert!(apply_pending(&t.0, 5).unwrap());
        assert!(!restore_pending(&t.0), "適用したら予約は消える");
        assert_eq!(master_next(&t.0), 10, "マスターが戻る");
        let db = crate::spread_state::open_spread_db(&t.0).expect("db");
        assert_eq!(crate::spread_state::read_archive_rating(&db, Path::new("/x"), "a.zip").map(|r| r.rating_half), Some(3), "DBが戻る");
        assert!(info(&t.0).is_some(), "リカバリポイントは残る");
        // 適用前のマスターは世代に残り、DBは退避される。
        let gens = crate::tag_master_gen::list(&t.0);
        assert_eq!(gens.len(), 1);
        assert_eq!(crate::tag_master_gen::read_for_restore(&t.0, &gens[0].name, 0).unwrap().1, 99);
        let evacuated = std::fs::read_dir(crate::dev_db_backup::auto_backup_dir(&t.0))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("before-restore-"));
        assert!(evacuated);
    }

    #[test]
    fn requesting_one_kind_of_restore_cancels_the_other() {
        let t = TempRoot::new("exclusive");
        let db = db_with_rating(&t.0, 3);
        save_master(&t.0, 10);
        set(&db, &t.0).unwrap();
        let generation = crate::db_backup::create_generation(&db, &t.0, 3).unwrap();
        request_restore(&t.0).unwrap();
        crate::db_backup::request_restore(&t.0, &generation.name).unwrap();
        assert!(crate::db_backup::restore_pending(&t.0));
        assert!(!restore_pending(&t.0), "世代の予約が後なので、リカバリポイントの予約は消える");
        request_restore(&t.0).unwrap();
        assert!(restore_pending(&t.0));
        assert!(!crate::db_backup::restore_pending(&t.0), "リカバリポイントの予約が後なので、世代の予約は消える");
    }
}
