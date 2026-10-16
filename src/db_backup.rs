//! 製品用のDBバックアップ世代と、GUIからのリストア予約。
//!
//! 対象は `nekoviewer_spread.redb` の1ファイルだけ。保存先は移行前バックアップと同じ `backup_auto/` で、
//! ファイル名の頭で区別する（移行前バックアップは世代管理の対象外で、ここでは消さない）。
//!
//! - 世代: `gen-<日時>.redb`。ユーザーの操作や掃除の直前に作り、設定の世代数を超えた分を古い順に消す
//! - 復元前の退避: `before-restore-<日時>.redb`。復元で置き換える直前の現DB。直近2件だけ残す
//! - リストア: 起動中のDBは差し替えず、予約ファイルを置いて次回起動時（DBを開く前）に適用する
//!
//! 開発用DBツール（`dev_db_backup.rs`）の復元予約とは別のファイル・別の退避先を使う。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::Database;

use crate::dev_db_backup::{
    auto_backup_dir, copy_db_atomic, copy_file_atomic, db_path, move_file, DevBackupError, MAX_BACKUP_BYTES,
};

const GEN_STEM: &str = "gen";
const BEFORE_RESTORE_STEM: &str = "before-restore";
const RESTORE_PENDING_FILE: &str = "nekoviewer_spread.redb.restore-pending-gen";
/// 復元前の退避を残す件数。
const BEFORE_RESTORE_KEEP: usize = 2;
/// `YYYYMMDD-HHMMSS` の文字数。
const TIMESTAMP_LEN: usize = 15;

/// 世代1件の情報（一覧表示用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    /// `backup_auto/` 内のファイル名。
    pub name: String,
    /// 更新日時（unix秒）。
    pub modified_unix: i64,
    pub size: u64,
}

pub fn restore_pending_path(root: &Path) -> PathBuf {
    root.join(RESTORE_PENDING_FILE)
}

/// 復元が予約されているか。
pub fn restore_pending(root: &Path) -> bool {
    restore_pending_path(root).is_file()
}

/// 復元の予約を取り消す。予約が無ければ何もしない。
pub fn cancel_restore(root: &Path) -> io::Result<()> {
    match std::fs::remove_file(restore_pending_path(root)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 現DBを新しい世代としてコピーし、`keep` 世代を超えた古い世代を消す。コピー中はDBのMutexを保持する。
pub fn create_generation(
    db: &Arc<Mutex<Database>>,
    root: &Path,
    keep: usize,
) -> Result<Generation, DevBackupError> {
    let dir = auto_backup_dir(root);
    std::fs::create_dir_all(&dir)?;
    let dest = next_dest(&dir, GEN_STEM);
    copy_db_atomic(db, root, &dest, false)?;
    // 古い世代の削除に失敗しても、作った世代は有効（次回に持ち越す）。
    let _ = rotate_generations(&dir, keep);
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    generation_info(&dir, &name).ok_or_else(|| DevBackupError::Io("generation missing after backup".into()))
}

/// 世代の一覧（新しい順）。
pub fn list_generations(root: &Path) -> Vec<Generation> {
    let dir = auto_backup_dir(root);
    let mut names = file_names_with_stem(&dir, GEN_STEM);
    names.sort_by_key(|n| std::cmp::Reverse(sort_key(n, GEN_STEM)));
    names.iter().filter_map(|n| generation_info(&dir, n)).collect()
}

/// 世代 `name`（`list_generations` の `name`）への復元を予約する。実際の差し替えは次回起動時。
pub fn request_restore(root: &Path, name: &str) -> Result<(), DevBackupError> {
    if !is_stem_file(name, GEN_STEM) {
        return Err(DevBackupError::Io(format!("not a generation: {name}")));
    }
    let src = auto_backup_dir(root).join(name);
    if !src.is_file() {
        return Err(DevBackupError::NoBaseline);
    }
    copy_file_atomic(&src, &restore_pending_path(root))?;
    Ok(())
}

/// 予約済みの復元を適用する。`open_spread_db` より前（DBが開かれていない状態）で呼ぶ。
/// 現DBは `backup_auto/before-restore-*.redb` へ退避する。退避に失敗したら復元を中止し、現DBと予約を残す。
/// 戻り値は適用したかどうか。
pub fn apply_pending_restore(root: &Path) -> Result<bool, DevBackupError> {
    let pending = restore_pending_path(root);
    if !pending.is_file() {
        return Ok(false);
    }
    let db = db_path(root);
    let mut evacuated: Option<PathBuf> = None;
    if db.is_file() {
        let dir = auto_backup_dir(root);
        std::fs::create_dir_all(&dir)?;
        let dest = next_dest(&dir, BEFORE_RESTORE_STEM);
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
    let _ = rotate_stem(&auto_backup_dir(root), BEFORE_RESTORE_STEM, BEFORE_RESTORE_KEEP, u64::MAX);
    Ok(true)
}

/// `dir` の世代を、`keep` 世代か合計サイズの上限を超える間、古い順に削除する。最新の1件は消さない。
fn rotate_generations(dir: &Path, keep: usize) -> io::Result<usize> {
    rotate_stem(dir, GEN_STEM, keep.max(1), MAX_BACKUP_BYTES)
}

fn rotate_stem(dir: &Path, stem: &str, keep: usize, max_bytes: u64) -> io::Result<usize> {
    let mut names = file_names_with_stem(dir, stem);
    names.sort_by_key(|n| sort_key(n, stem));
    let sizes: Vec<u64> = names
        .iter()
        .map(|n| std::fs::metadata(dir.join(n)).map(|m| m.len()).unwrap_or(0))
        .collect();
    let mut total: u64 = sizes.iter().sum();
    let mut remaining = names.len();
    let mut deleted = 0;
    for (name, len) in names.iter().zip(&sizes) {
        if remaining <= 1 || (remaining <= keep && total <= max_bytes) {
            break;
        }
        std::fs::remove_file(dir.join(name))?;
        total = total.saturating_sub(*len);
        remaining -= 1;
        deleted += 1;
    }
    Ok(deleted)
}

/// `dir` 直下の `{stem}-*.redb`（コピー途中の一時ファイルは含まない）のファイル名。
fn file_names_with_stem(dir: &Path, stem: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.metadata().is_ok_and(|m| m.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_stem_file(n, stem))
        .collect()
}

/// `{stem}-{日時}.redb` または `{stem}-{日時}-{連番}.redb` の形か。パス区切りを含む名前は拒否する。
fn is_stem_file(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem).and_then(|r| r.strip_prefix('-')) else { return false };
    let Some(body) = rest.strip_suffix(".redb") else { return false };
    let (ts, tail) = body.split_at_checked(TIMESTAMP_LEN).unwrap_or((body, "x"));
    let ts_ok = ts.len() == TIMESTAMP_LEN
        && ts.chars().enumerate().all(|(i, c)| if i == 8 { c == '-' } else { c.is_ascii_digit() });
    let tail_ok = tail.is_empty()
        || tail.strip_prefix('-').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    ts_ok && tail_ok
}

/// 次のファイル名。既存の最新より必ず後ろに並ぶ名前にする（古いものを消した後に同じ名前を再利用すると、
/// 新しいのに最古として扱われて消されるため）。時計が戻っていても、既存の最新の連番を進めて使う。
fn next_dest(dir: &Path, stem: &str) -> PathBuf {
    let now = timestamp_now();
    let latest = file_names_with_stem(dir, stem).iter().map(|n| sort_key(n, stem)).max();
    let (ts, seq) = match latest {
        Some((ts, seq)) if ts >= now => (ts, seq + 1),
        _ => (now, 1),
    };
    let name = if seq <= 1 { format!("{stem}-{ts}.redb") } else { format!("{stem}-{ts}-{seq}.redb") };
    dir.join(name)
}

fn timestamp_now() -> String {
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

/// 古い順に並べるキー（日時、同一秒内の連番）。連番なしは1番目として扱う。
fn sort_key(name: &str, stem: &str) -> (String, u32) {
    let body = name
        .strip_prefix(stem)
        .and_then(|r| r.strip_prefix('-'))
        .and_then(|r| r.strip_suffix(".redb"))
        .unwrap_or("");
    let (ts, tail) = body.split_at_checked(TIMESTAMP_LEN).unwrap_or((body, ""));
    let seq = tail.strip_prefix('-').and_then(|n| n.parse().ok()).unwrap_or(1);
    (ts.to_owned(), seq)
}

fn generation_info(dir: &Path, name: &str) -> Option<Generation> {
    let meta = std::fs::metadata(dir.join(name)).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    Some(Generation { name: name.to_owned(), modified_unix, size: meta.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_db_backup::{ensure_pre_migration_backup, PreMigrationBackup};
    use crate::spread_state::{open_spread_db, read_archive_rating, write_archive_rating};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_db_backup_test_{}_{}_{}",
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

    fn names(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(auto_backup_dir(root))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    fn put(dir: &Path, name: &str, len: usize) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), vec![0u8; len]).unwrap();
    }

    #[test]
    fn stem_file_names_are_recognised_strictly() {
        assert!(is_stem_file("gen-20261016-130501.redb", "gen"));
        assert!(is_stem_file("gen-20261016-130501-2.redb", "gen"));
        assert!(!is_stem_file("gen-20261016-130501.redb.tmp", "gen"));
        assert!(!is_stem_file("gen-20261016-130501-.redb", "gen"));
        assert!(!is_stem_file("gen-20261016-130501-x.redb", "gen"));
        assert!(!is_stem_file("gen-2026.redb", "gen"));
        assert!(!is_stem_file("nekoviewer_spread-20261016-130501.redb", "gen"));
        assert!(!is_stem_file("gen-../../x.redb", "gen"));
        assert!(!is_stem_file("gen-20261016-130501.redb", "before-restore"));
    }

    #[test]
    fn next_dest_never_reuses_a_name_that_sorts_before_existing_ones() {
        let t = TempRoot::new("next_dest");
        let dir = auto_backup_dir(&t.0);
        // 未来の日時（時計が戻った場合など）が既にあっても、それより後ろに並ぶ名前になる。
        put(&dir, "gen-29990101-000000.redb", 1);
        let next = next_dest(&dir, GEN_STEM);
        assert_eq!(next.file_name().unwrap().to_string_lossy(), "gen-29990101-000000-2.redb");
        assert!(sort_key("gen-29990101-000000-2.redb", GEN_STEM) > sort_key("gen-29990101-000000.redb", GEN_STEM));
    }

    #[test]
    fn same_second_sequence_sorts_after_the_plain_name() {
        let a = sort_key("gen-20261016-130501.redb", "gen");
        let b = sort_key("gen-20261016-130501-2.redb", "gen");
        let c = sort_key("gen-20261016-130502.redb", "gen");
        assert!(a < b && b < c);
    }

    #[test]
    fn create_generation_copies_the_db_and_lists_newest_first() {
        let t = TempRoot::new("create");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        let g1 = create_generation(&db, &t.0, 3).unwrap();
        let g2 = create_generation(&db, &t.0, 3).unwrap();
        assert!(g1.size > 0);
        let listed = list_generations(&t.0);
        assert_eq!(listed.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), vec![g2.name.as_str(), g1.name.as_str()]);
    }

    #[test]
    fn generations_beyond_keep_are_dropped_oldest_first() {
        let t = TempRoot::new("rotate");
        let db = open_spread_db(&t.0).unwrap();
        let mut made = Vec::new();
        for _ in 0..4 {
            made.push(create_generation(&db, &t.0, 2).unwrap().name);
        }
        let left: Vec<String> = list_generations(&t.0).into_iter().map(|g| g.name).collect();
        assert_eq!(left, vec![made[3].clone(), made[2].clone()]);
    }

    #[test]
    fn keep_zero_still_leaves_the_newest_generation() {
        let t = TempRoot::new("keep0");
        let db = open_spread_db(&t.0).unwrap();
        create_generation(&db, &t.0, 0).unwrap();
        create_generation(&db, &t.0, 0).unwrap();
        assert_eq!(list_generations(&t.0).len(), 1);
    }

    #[test]
    fn rotation_never_touches_pre_migration_or_before_restore_files() {
        let t = TempRoot::new("protect");
        let dir = auto_backup_dir(&t.0);
        put(&dir, "nekoviewer_spread-20200101-000000.redb", 10);
        put(&dir, "before-restore-20200101-000000.redb", 10);
        put(&dir, "gen-20200101-000001.redb", 10);
        put(&dir, "gen-20200101-000002.redb", 10);
        put(&dir, "gen-20200101-000003.redb", 10);
        assert_eq!(rotate_generations(&dir, 1).unwrap(), 2);
        assert_eq!(
            names(&t.0),
            vec![
                "before-restore-20200101-000000.redb",
                "gen-20200101-000003.redb",
                "nekoviewer_spread-20200101-000000.redb",
            ]
        );
    }

    #[test]
    fn generations_do_not_count_as_the_pre_migration_backup() {
        let t = TempRoot::new("pre_mig");
        let db = open_spread_db(&t.0).unwrap();
        create_generation(&db, &t.0, 3).unwrap();
        drop(db);
        // 世代しか無い状態でも、移行前バックアップはまだ取られていないと判定される。
        assert!(matches!(ensure_pre_migration_backup(&t.0), Ok(PreMigrationBackup::Created(_))));
        assert!(matches!(ensure_pre_migration_backup(&t.0), Ok(PreMigrationBackup::AlreadyDone)));
    }

    #[test]
    fn restore_round_trip_returns_to_the_chosen_generation() {
        let t = TempRoot::new("restore");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        let g = create_generation(&db, &t.0, 3).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 9));
        assert!(!restore_pending(&t.0));
        request_restore(&t.0, &g.name).unwrap();
        assert!(restore_pending(&t.0));
        drop(db);

        assert!(apply_pending_restore(&t.0).unwrap());
        assert!(!restore_pending(&t.0));
        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(4));
        // 復元前（評価9）のDBが退避されている。
        let evac: Vec<String> = names(&t.0).into_iter().filter(|n| n.starts_with("before-restore-")).collect();
        assert_eq!(evac.len(), 1);
    }

    #[test]
    fn apply_without_a_reservation_does_nothing() {
        let t = TempRoot::new("noop");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        drop(db);
        assert!(!apply_pending_restore(&t.0).unwrap());
        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(4));
        assert!(names(&t.0).is_empty());
    }

    #[test]
    fn cancel_removes_the_reservation_and_is_idempotent() {
        let t = TempRoot::new("cancel");
        let db = open_spread_db(&t.0).unwrap();
        let g = create_generation(&db, &t.0, 3).unwrap();
        request_restore(&t.0, &g.name).unwrap();
        cancel_restore(&t.0).unwrap();
        assert!(!restore_pending(&t.0));
        cancel_restore(&t.0).unwrap();
        drop(db);
        assert!(!apply_pending_restore(&t.0).unwrap());
    }

    #[test]
    fn request_restore_rejects_unknown_and_foreign_names() {
        let t = TempRoot::new("reject");
        let dir = auto_backup_dir(&t.0);
        put(&dir, "nekoviewer_spread-20200101-000000.redb", 10);
        assert_eq!(request_restore(&t.0, "gen-20200101-000000.redb"), Err(DevBackupError::NoBaseline));
        // 移行前バックアップと、パスを含む名前は世代として扱わない。
        assert!(matches!(request_restore(&t.0, "nekoviewer_spread-20200101-000000.redb"), Err(DevBackupError::Io(_))));
        assert!(matches!(request_restore(&t.0, "../nekoviewer_spread.redb"), Err(DevBackupError::Io(_))));
        assert!(!restore_pending(&t.0));
    }

    #[test]
    fn before_restore_keeps_only_the_latest_two() {
        let t = TempRoot::new("evac_keep");
        let dir = auto_backup_dir(&t.0);
        put(&dir, "before-restore-20200101-000001.redb", 10);
        put(&dir, "before-restore-20200101-000002.redb", 10);
        let db = open_spread_db(&t.0).unwrap();
        let g = create_generation(&db, &t.0, 3).unwrap();
        request_restore(&t.0, &g.name).unwrap();
        drop(db);
        assert!(apply_pending_restore(&t.0).unwrap());
        let evac: Vec<String> = names(&t.0).into_iter().filter(|n| n.starts_with("before-restore-")).collect();
        assert_eq!(evac.len(), 2);
        assert!(!evac.contains(&"before-restore-20200101-000001.redb".to_owned()));
    }

    #[test]
    fn restore_aborts_and_keeps_current_db_when_evacuation_fails() {
        let t = TempRoot::new("evac_fail");
        let db = open_spread_db(&t.0).unwrap();
        assert!(write_archive_rating(&db, Path::new("/x"), "a.zip", 4));
        let g = create_generation(&db, &t.0, 3).unwrap();
        request_restore(&t.0, &g.name).unwrap();
        drop(db);
        // 退避先のフォルダを作れない状態にする（backup_auto を、世代ごと消してファイルに置き換える）。
        std::fs::remove_dir_all(auto_backup_dir(&t.0)).unwrap();
        std::fs::write(auto_backup_dir(&t.0), b"not a dir").unwrap();
        assert!(apply_pending_restore(&t.0).is_err());
        assert!(restore_pending(&t.0), "予約は残る");
        let db = open_spread_db(&t.0).unwrap();
        assert_eq!(rating_of(&db), Some(4), "現DBはそのまま");
    }
}
