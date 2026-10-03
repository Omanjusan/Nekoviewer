
//! 開発用のDBバックアップ/リストアと、移行前の自動バックアップ導線。
//!
//! FP管理への移行を「旧パス仕様DBへ戻して繰り返し検証する」ためのもの。対象は
//! `nekoviewer_spread.redb` の1ファイルだけ（評価・タグ・しおり・お気に入り・仮想フォルダ等が
//! 全て入っている）。ディレクトリ別の `cache.redb` は作り直せるので対象外。
//!
//! - 基準バックアップ: `dev_backup/baseline.redb`（1スロット）。FP仕様のDBは拒否する
//! - リストア: 起動中のDBは差し替えず、`restore-pending` を置いて次回起動時
//!   （`open_spread_db` の前）に適用する。適用時は現DBを `dev_backup/failed/` へ退避する
//! - 自動バックアップ導線: `ensure_pre_migration_backup`。DBを開く前にファイルを丸ごとコピーし
//!   （開いたDBは Windows でロックされるため）、`backup_auto/` に1回だけ取る。失敗は `Err` で返す
//!
//! 実リリース時はモジュールごと `cfg(debug_assertions)` へ戻すこと（自動バックアップ導線を
//! 製品側へ紐付ける場合はその部分だけ残す）。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::Database;

use crate::spread_state::is_identity_spec_in;

pub const DB_FILE_NAME: &str = "nekoviewer_spread.redb";
const PENDING_FILE_NAME: &str = "nekoviewer_spread.redb.restore-pending";
const DEV_DIR: &str = "dev_backup";
const BASELINE_FILE_NAME: &str = "baseline.redb";
const FAILED_DIR: &str = "failed";
const AUTO_DIR: &str = "backup_auto";

/// 世代管理の上限。ファイル数か合計サイズのどちらかを超えたら古い順に削除する。
pub const MAX_BACKUP_FILES: usize = 5;
pub const MAX_BACKUP_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum DevBackupError {
    /// FP仕様のDBはバックアップ対象外（旧パス仕様へ戻すための基準にならないため）。
    IdentitySpec,
    /// 基準バックアップが無い。
    NoBaseline,
    /// DBのMutexが取れない（poison）。
    DbBusy,
    Io(String),
}

impl From<io::Error> for DevBackupError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineInfo {
    /// 更新日時（unix秒）。
    pub modified_unix: i64,
    pub size: u64,
}

pub fn db_path(root: &Path) -> PathBuf {
    root.join(DB_FILE_NAME)
}

pub fn baseline_path(root: &Path) -> PathBuf {
    root.join(DEV_DIR).join(BASELINE_FILE_NAME)
}

pub fn pending_path(root: &Path) -> PathBuf {
    root.join(PENDING_FILE_NAME)
}

pub fn failed_dir(root: &Path) -> PathBuf {
    root.join(DEV_DIR).join(FAILED_DIR)
}

pub fn auto_backup_dir(root: &Path) -> PathBuf {
    root.join(AUTO_DIR)
}

pub fn baseline_info(root: &Path) -> Option<BaselineInfo> {
    let meta = std::fs::metadata(baseline_path(root)).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    Some(BaselineInfo { modified_unix, size: meta.len() })
}

pub fn pending_exists(root: &Path) -> bool {
    pending_path(root).is_file()
}

/// 現DBを基準バックアップとして保存する。FP仕様のDBは `IdentitySpec` で拒否する。
/// 既存の基準は上書きする（確認ダイアログは呼び出し側）。
pub fn backup(db: &Arc<Mutex<Database>>, root: &Path) -> Result<BaselineInfo, DevBackupError> {
    copy_db_atomic(db, root, &baseline_path(root), true)?;
    baseline_info(root).ok_or_else(|| DevBackupError::Io("baseline missing after backup".into()))
}

/// 基準バックアップの復元を予約する。実際の差し替えは次回起動時の `apply_pending_restore`。
pub fn request_restore(root: &Path) -> Result<(), DevBackupError> {
    let baseline = baseline_path(root);
    if !baseline.is_file() {
        return Err(DevBackupError::NoBaseline);
    }
    copy_file_atomic(&baseline, &pending_path(root))?;
    Ok(())
}

/// 予約済みの復元を適用する。`open_spread_db` より前（DBが開かれていない状態）で呼ぶ。
/// 戻り値は適用したかどうか。現DBの退避に失敗したら復元を中止し、現DBとpendingを残す。
pub fn apply_pending_restore(root: &Path) -> Result<bool, DevBackupError> {
    let pending = pending_path(root);
    if !pending.is_file() {
        return Ok(false);
    }
    let db = db_path(root);
    let mut evacuated: Option<PathBuf> = None;
    if db.is_file() {
        let failed = failed_dir(root);
        std::fs::create_dir_all(&failed)?;
        let dest = unique_dest(&failed, "failed", "redb");
        move_file(&db, &dest)?;
        evacuated = Some(dest);
        let _ = rotate(&failed, MAX_BACKUP_FILES, MAX_BACKUP_BYTES);
    }
    if let Err(e) = std::fs::rename(&pending, &db) {
        // 退避済みで本DBが無い状態を残さない（ベストエフォートで元へ戻す）。
        if let Some(dest) = evacuated {
            let _ = move_file(&dest, &db);
        }
        return Err(e.into());
    }
    Ok(true)
}

/// `ensure_pre_migration_backup` の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum PreMigrationBackup {
    /// 取った。値はバックアップのパス。
    Created(PathBuf),
    /// DBファイルが無い（新規インストール）ので取らなかった。
    NoDb,
    /// `backup_auto/` に既にバックアップがあるので取らなかった。
    AlreadyDone,
}

/// 移行前の自動バックアップ。DBを開く前に呼ぶ（開いたDBは Windows でロックされるため、ファイルの
/// コピーだけで済ませる）。DBファイルがあり、`backup_auto/` にバックアップが1つも無いときだけ、
/// 日時付きでコピーする（FP仕様への移行は1回きりなので、世代管理はしない）。失敗は `Err`。
pub fn ensure_pre_migration_backup(root: &Path) -> Result<PreMigrationBackup, DevBackupError> {
    let db = db_path(root);
    if !db.is_file() {
        return Ok(PreMigrationBackup::NoDb);
    }
    let dir = auto_backup_dir(root);
    if has_backup(&dir) {
        return Ok(PreMigrationBackup::AlreadyDone);
    }
    std::fs::create_dir_all(&dir)?;
    let dest = unique_dest(&dir, "nekoviewer_spread", "redb");
    copy_file_atomic(&db, &dest)?;
    Ok(PreMigrationBackup::Created(dest))
}

/// `dir` 直下にバックアップ（`.redb`）があるか。コピー途中の一時ファイルは数えない。
fn has_backup(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            e.metadata().is_ok_and(|m| m.is_file()) && e.path().extension().is_some_and(|x| x == "redb")
        })
    })
}

/// `dir` 直下のファイルを、ファイル数か合計サイズの上限を超える間、更新日時の古い順に削除する。
/// 最新の1件は上限を超えていても消さない。削除した件数を返す。
pub fn rotate(dir: &Path, max_files: usize, max_bytes: u64) -> io::Result<usize> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut files: Vec<(std::time::SystemTime, std::ffi::OsString, u64)> = Vec::new();
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        files.push((modified, entry.file_name(), meta.len()));
    }
    // 更新日時が同じ場合はファイル名（日時入りで昇順になる）で決める。
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    let mut remaining = files.len();
    let mut deleted = 0;
    for (_, name, len) in &files {
        if remaining <= 1 || (remaining <= max_files && total <= max_bytes) {
            break;
        }
        std::fs::remove_file(dir.join(name))?;
        total = total.saturating_sub(*len);
        remaining -= 1;
        deleted += 1;
    }
    Ok(deleted)
}

/// Mutexを保持したままDBファイルをコピーして `dest` へ確定する（一時ファイル経由で、
/// 失敗しても既存の `dest` を壊さない）。書き込みは全てMutex経由なので、保持中は
/// コミット済みの整合した状態をコピーできる。
fn copy_db_atomic(
    db: &Arc<Mutex<Database>>,
    root: &Path,
    dest: &Path,
    reject_identity: bool,
) -> Result<(), DevBackupError> {
    let guard = db.lock().map_err(|_| DevBackupError::DbBusy)?;
    if reject_identity && is_identity_spec_in(&guard) {
        return Err(DevBackupError::IdentitySpec);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    copy_file_atomic(&db_path(root), dest)?;
    drop(guard);
    Ok(())
}

fn copy_file_atomic(src: &Path, dest: &Path) -> io::Result<()> {
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if let Err(e) = std::fs::copy(src, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// rename、別ファイルシステム等で失敗したらコピー＋削除で移す。
fn move_file(src: &Path, dest: &Path) -> io::Result<()> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dest)?;
    std::fs::remove_file(src)
}

/// `{stem}-{日時}.{ext}`。同名が既にあれば連番を付ける。
fn unique_dest(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let ts = timestamp_string();
    let first = dir.join(format!("{stem}-{ts}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{ts}-{n}.{ext}")))
        .find(|p| !p.exists())
        .expect("unbounded range")
}

fn timestamp_string() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

#[cfg(test)]
mod tests {
    use redb::{ReadableDatabase, TableHandle};

    use super::*;
    use crate::spread_state::{
        is_identity_spec, open_spread_db, read_archive_rating, set_identity_spec,
        write_archive_rating,
    };

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_dev_db_backup_test_{}_{}_{}",
                std::process::id(),
                nonce,
                tag
            ));
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
    fn backup_restore_round_trip_returns_to_original_content() {
        let t = TempRoot::new("roundtrip");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        backup(&db, &t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 9));
        assert_eq!(rating_of(&db), Some(9));

        request_restore(&t.0).unwrap();
        assert!(pending_exists(&t.0));
        // 起動時適用はDBが開かれていない状態で行う。
        drop(db);
        assert_eq!(apply_pending_restore(&t.0), Ok(true));
        assert!(!pending_exists(&t.0));

        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(4));
    }

    #[test]
    fn identity_spec_db_is_rejected_and_baseline_untouched() {
        let t = TempRoot::new("guard");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        let before = backup(&db, &t.0).unwrap();
        let before_bytes = std::fs::read(baseline_path(&t.0)).unwrap();

        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 7));
        assert!(set_identity_spec(&db, true));
        assert!(is_identity_spec(&db));
        assert_eq!(backup(&db, &t.0), Err(DevBackupError::IdentitySpec));
        assert_eq!(std::fs::read(baseline_path(&t.0)).unwrap(), before_bytes);
        assert_eq!(baseline_info(&t.0).unwrap().size, before.size);

        // 解除すれば再びバックアップできる。
        assert!(set_identity_spec(&db, false));
        assert!(!is_identity_spec(&db));
        assert!(backup(&db, &t.0).is_ok());
    }

    #[test]
    fn open_spread_db_does_not_add_identity_table_or_marker() {
        let t = TempRoot::new("marker_readonly");
        let db = open_spread_db(&t.0).unwrap();
        assert!(!is_identity_spec(&db));
        {
            let guard = db.lock().unwrap();
            let tx = guard.begin_read().unwrap();
            let names: Vec<String> = tx
                .list_tables()
                .unwrap()
                .map(|h| h.name().to_string())
                .collect();
            assert!(!names.iter().any(|n| n.starts_with("identity")), "{names:?}");
        }
        // 判定を繰り返しても書き込みは起きない（テーブルは作られないまま）。
        assert!(!is_identity_spec(&db));
        let guard = db.lock().unwrap();
        let tx = guard.begin_read().unwrap();
        assert!(!tx.list_tables().unwrap().any(|h| h.name().starts_with("identity")));
    }

    #[test]
    fn restore_moves_current_db_to_failed_dir() {
        let t = TempRoot::new("evacuate");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 2));
        backup(&db, &t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 8));
        request_restore(&t.0).unwrap();
        drop(db);
        assert_eq!(apply_pending_restore(&t.0), Ok(true));

        let failed: Vec<_> = std::fs::read_dir(failed_dir(&t.0)).unwrap().flatten().collect();
        assert_eq!(failed.len(), 1);
        // 退避されたDBには、リストア前（バグ入り想定）の値が残っている。
        let evacuated = Database::open(failed[0].path()).unwrap();
        let evacuated = Arc::new(Mutex::new(evacuated));
        assert_eq!(rating_of(&evacuated), Some(8));
    }

    #[test]
    fn request_restore_without_baseline_fails() {
        let t = TempRoot::new("nobaseline");
        let _db = open_spread_db(&t.0).unwrap();
        assert_eq!(request_restore(&t.0), Err(DevBackupError::NoBaseline));
        assert!(!pending_exists(&t.0));
        assert_eq!(apply_pending_restore(&t.0), Ok(false));
    }

    #[test]
    fn restore_aborts_and_keeps_current_db_when_evacuation_fails() {
        let t = TempRoot::new("evac_fail");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 3));
        backup(&db, &t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 6));
        request_restore(&t.0).unwrap();
        drop(db);
        // 退避先 `failed` をファイルにして、ディレクトリ作成を失敗させる。
        let failed = failed_dir(&t.0);
        std::fs::create_dir_all(failed.parent().unwrap()).unwrap();
        std::fs::write(&failed, b"not a dir").unwrap();

        assert!(matches!(apply_pending_restore(&t.0), Err(DevBackupError::Io(_))));
        // 現DBは無傷で、pendingも残る（原因を直せば再適用できる）。
        assert!(pending_exists(&t.0));
        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(6));
    }

    fn write_sized(dir: &Path, name: &str, len: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), vec![0u8; len]).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn rotate_keeps_at_most_max_files_dropping_oldest_first() {
        let t = TempRoot::new("rotate_count");
        let dir = t.0.join("r");
        for i in 1..=7 {
            write_sized(&dir, &format!("b-{i:02}.redb"), 10);
        }
        assert_eq!(rotate(&dir, 5, u64::MAX).unwrap(), 2);
        assert_eq!(
            names_in(&dir),
            ["b-03.redb", "b-04.redb", "b-05.redb", "b-06.redb", "b-07.redb"]
        );
        // 上限ちょうどなら何も消さない。
        assert_eq!(rotate(&dir, 5, u64::MAX).unwrap(), 0);
    }

    #[test]
    fn rotate_drops_oldest_when_total_size_exceeds_limit() {
        let t = TempRoot::new("rotate_size");
        let dir = t.0.join("r");
        for i in 1..=4 {
            write_sized(&dir, &format!("b-{i:02}.redb"), 100);
        }
        // 合計400バイトで上限250バイト → 古い2件を削除して200バイト。
        assert_eq!(rotate(&dir, 5, 250).unwrap(), 2);
        assert_eq!(names_in(&dir), ["b-03.redb", "b-04.redb"]);
    }

    #[test]
    fn rotate_never_deletes_the_newest_even_if_it_exceeds_the_limit() {
        let t = TempRoot::new("rotate_newest");
        let dir = t.0.join("r");
        write_sized(&dir, "b-01.redb", 100);
        write_sized(&dir, "b-02.redb", 500);
        assert_eq!(rotate(&dir, 5, 50).unwrap(), 1);
        assert_eq!(names_in(&dir), ["b-02.redb"]);
        // ディレクトリ不在は何もしない。
        assert_eq!(rotate(&t.0.join("none"), 5, 50).unwrap(), 0);
    }

    fn auto_backups(root: &Path) -> Vec<String> {
        names_in(&auto_backup_dir(root))
    }

    #[test]
    fn pre_migration_backup_copies_legacy_db_once() {
        let t = TempRoot::new("auto");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 5));
        drop(db);
        let PreMigrationBackup::Created(dest) = ensure_pre_migration_backup(&t.0).unwrap() else {
            panic!("backup should be created");
        };
        assert_eq!(auto_backups(&t.0).len(), 1);
        let copy = Arc::new(Mutex::new(Database::open(&dest).unwrap()));
        assert_eq!(rating_of(&copy), Some(5));
        assert!(!is_identity_spec(&copy));
        drop(copy);

        // 2回目以降は、DBが変わっても取らない（1回きり）。
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 9));
        drop(db);
        assert_eq!(ensure_pre_migration_backup(&t.0), Ok(PreMigrationBackup::AlreadyDone));
        assert_eq!(auto_backups(&t.0).len(), 1);
    }

    #[test]
    fn pre_migration_backup_skips_when_no_db_exists() {
        let t = TempRoot::new("auto_nodb");
        assert_eq!(ensure_pre_migration_backup(&t.0), Ok(PreMigrationBackup::NoDb));
        assert!(!auto_backup_dir(&t.0).exists());
    }

    #[test]
    fn pre_migration_backup_ignores_stray_tmp_files_and_retries() {
        let t = TempRoot::new("auto_tmp");
        drop(open_spread_db(&t.0).unwrap());
        write_sized(&auto_backup_dir(&t.0), "nekoviewer_spread-x.redb.tmp", 10);
        assert!(matches!(ensure_pre_migration_backup(&t.0), Ok(PreMigrationBackup::Created(_))));
    }

    #[test]
    fn pre_migration_backup_reports_failure_so_caller_can_stop_migration() {
        let t = TempRoot::new("auto_fail");
        drop(open_spread_db(&t.0).unwrap());
        // 保存先をファイルにして、ディレクトリ作成を失敗させる。
        std::fs::write(auto_backup_dir(&t.0), b"not a dir").unwrap();
        assert!(matches!(ensure_pre_migration_backup(&t.0), Err(DevBackupError::Io(_))));
        // 失敗しても元のDBは無傷。
        assert!(open_spread_db(&t.0).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn pre_migration_backup_fails_on_read_only_dir_and_leaves_no_partial_file() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempRoot::new("auto_ro");
        drop(open_spread_db(&t.0).unwrap());
        let dir = auto_backup_dir(&t.0);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::File::create(dir.join("probe")).is_ok();
        let result = ensure_pre_migration_backup(&t.0);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            return; // root 等は権限を無視するので検証できない。
        }
        assert!(matches!(result, Err(DevBackupError::Io(_))));
        assert!(auto_backups(&t.0).is_empty());
    }
}
