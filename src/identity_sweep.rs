//! 期限を過ぎた「未確認」ID（実体が見つからないまま）をDBから掃除する。
//!
//! 掃除は2段に分ける。呼び出し側は、`find_expired` で候補を出し、ユーザーデータ付きのIDが含まれるなら
//! バックアップを取ってから、`delete_expired` で消す。
//!
//! - 期限の判定は呼び出し側が渡す（`is_expired`）。ボリュームのオンライン時間で数える方式にも、
//!   単純な経過日数にも使えるようにするため。
//! - 候補は「未確認」かつ、実ファイルの確認で「ボリュームは見えているのにファイルが無い（Missing）」もの。
//!   現存（復活した）・オフライン・確認できないIDは対象外。消す直前にも状態を確かめ直す。
//! - 削除は `file_identity::delete_id`（ID層・設定・評価・しおり・お気に入りの行。他の旧v1の行は残す）。
//!   消した後に、そのIDを指す未解決の記録を `identity_pending::prune_real` で整理する。

use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{all_records_tx, delete_id, probe_record, record_by_id_tx, FileRecord, FileStatus, Presence};
use crate::spread_state::id_has_user_data;

/// 掃除の候補1件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired {
    pub id: u64,
    /// 評価・しおり・設定（タグ・お気に入りを含む）のいずれかが付いている。
    pub has_user_data: bool,
}

/// 掃除の結果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// 消したID。
    pub deleted: usize,
    /// うち、ユーザーデータが付いていたID。
    pub deleted_with_data: usize,
    /// 消す直前に状態が変わっていた（現存・オフライン・既に無い等）ので消さなかったID。
    pub skipped: usize,
    /// DBエラーなどで消せなかったID。
    pub failed: usize,
}

fn is_candidate(rec: &FileRecord, is_expired: &dyn Fn(&FileRecord) -> bool, probe: &dyn Fn(&FileRecord) -> Presence) -> bool {
    rec.status == FileStatus::Unconfirmed && is_expired(rec) && probe(rec) == Presence::Missing
}

/// 掃除の候補を探す（何も消さない）。
pub fn find_expired_with(
    db: &Arc<Mutex<Database>>,
    is_expired: &dyn Fn(&FileRecord) -> bool,
    probe: &dyn Fn(&FileRecord) -> Presence,
    has_user_data: &dyn Fn(u64) -> bool,
) -> Vec<Expired> {
    let records = {
        let Ok(guard) = db.lock() else { return Vec::new() };
        let Ok(tx) = guard.begin_read() else { return Vec::new() };
        all_records_tx(&tx)
    };
    // 実ファイルの確認（stat）はロックの外で行う。
    records
        .iter()
        .filter(|rec| rec.status == FileStatus::Unconfirmed && is_expired(rec))
        .filter(|rec| probe(rec) == Presence::Missing)
        .map(|rec| Expired { id: rec.id, has_user_data: has_user_data(rec.id) })
        .collect()
}

/// `find_expired_with` の実ファイル・実DB版。
pub fn find_expired(db: &Arc<Mutex<Database>>, is_expired: &dyn Fn(&FileRecord) -> bool) -> Vec<Expired> {
    find_expired_with(db, is_expired, &probe_record, &|id| id_has_user_data(db, id))
}

/// 候補を消す。消す直前に、未確認のまま・期限切れ・ファイルが無いことを確かめ直す。
/// 消したIDがあれば、そのIDを指す未解決の記録も整理する。
pub fn delete_expired_with(
    db: &Arc<Mutex<Database>>,
    list: &[Expired],
    is_expired: &dyn Fn(&FileRecord) -> bool,
    probe: &dyn Fn(&FileRecord) -> Presence,
) -> SweepReport {
    let mut report = SweepReport::default();
    for item in list {
        let rec = {
            let Ok(guard) = db.lock() else {
                report.failed += 1;
                continue;
            };
            let Ok(tx) = guard.begin_read() else {
                report.failed += 1;
                continue;
            };
            record_by_id_tx(&tx, item.id)
        };
        let Some(rec) = rec else {
            report.skipped += 1;
            continue;
        };
        if !is_candidate(&rec, is_expired, probe) {
            report.skipped += 1;
            continue;
        }
        match delete_id(db, item.id) {
            Ok(()) => {
                report.deleted += 1;
                if item.has_user_data {
                    report.deleted_with_data += 1;
                }
            }
            Err(_) => report.failed += 1,
        }
    }
    if report.deleted > 0 {
        crate::identity_pending::prune_real(db);
    }
    report
}

/// `delete_expired_with` の実ファイル版。
pub fn delete_expired(db: &Arc<Mutex<Database>>, list: &[Expired], is_expired: &dyn Fn(&FileRecord) -> bool) -> SweepReport {
    delete_expired_with(db, list, is_expired, &probe_record)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::favorites;
    use crate::file_identity::{ensure_record, lookup, mark_unconfirmed};
    use crate::identity_pending::{self as pending, PendingKind};
    use crate::spread_state::{make_key, open_spread_db, read_archive_rating, write_archive_rating};

    struct Env {
        root: PathBuf,
        db: Arc<Mutex<Database>>,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("nekoviewer_identity_sweep_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            let db = open_spread_db(&root).unwrap();
            favorites::init_favorite_tables(&db).unwrap();
            Self { root, db }
        }

        /// 十分古いmtimeのファイルを作り、IDを解決して返す。
        fn file(&self, name: &str, seed: u8) -> FileRecord {
            let p = self.root.join(name);
            std::fs::write(&p, vec![seed; 3000 + seed as usize]).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
            ensure_record(&self.db, &self.root, name).unwrap()
        }

        /// ファイルを消し、一覧で消えたことを確認した状態（未確認）にする。
        fn vanish(&self, name: &str, at: i64) {
            std::fs::remove_file(self.root.join(name)).unwrap();
            assert!(mark_unconfirmed(&self.db, &make_key(&self.root, name), at));
        }

        fn exists(&self, name: &str) -> bool {
            lookup(&self.db, &make_key(&self.root, name)).is_some()
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn older_than(limit: i64) -> impl Fn(&FileRecord) -> bool {
        move |rec: &FileRecord| rec.last_seen < limit
    }

    #[test]
    fn expired_unconfirmed_missing_ids_are_found_with_their_data_flag() {
        let env = Env::new("find");
        env.file("data.zip", 1);
        env.file("blank.zip", 2);
        env.file("fresh.zip", 3);
        env.file("alive.zip", 4);
        assert!(write_archive_rating(&env.db, &env.root, "data.zip", 6));
        env.vanish("data.zip", 100);
        env.vanish("blank.zip", 100);
        env.vanish("fresh.zip", 900); // 期限内
        // alive.zip は確認済みのまま（期限判定の対象外）。

        let mut found = find_expired(&env.db, &older_than(500));
        found.sort_by_key(|e| e.id);
        let id_of = |n: &str| lookup(&env.db, &make_key(&env.root, n)).unwrap().id;
        assert_eq!(
            found,
            vec![
                Expired { id: id_of("data.zip"), has_user_data: true },
                Expired { id: id_of("blank.zip"), has_user_data: false },
            ]
        );
    }

    #[test]
    fn present_or_offline_or_confirmed_ids_are_never_candidates() {
        let env = Env::new("exclude");
        env.file("back.zip", 1);
        env.file("off.zip", 2);
        env.vanish("back.zip", 100);
        env.vanish("off.zip", 100);
        // back.zip は復活した（現存）。off.zip はボリュームがオフライン想定。
        std::fs::write(env.root.join("back.zip"), b"x").unwrap();
        let off_id = lookup(&env.db, &make_key(&env.root, "off.zip")).unwrap().id;
        let probe = |rec: &FileRecord| if rec.id == off_id { Presence::Offline } else { probe_record(rec) };
        let found = find_expired_with(&env.db, &older_than(500), &probe, &|_| false);
        assert!(found.is_empty());
    }

    #[test]
    fn delete_expired_removes_id_data_and_reports_counts() {
        let env = Env::new("delete");
        env.file("data.zip", 1);
        env.file("blank.zip", 2);
        assert!(write_archive_rating(&env.db, &env.root, "data.zip", 6));
        favorites::set_membership(&env.db, &env.root, "data.zip", &[2]);
        env.vanish("data.zip", 100);
        env.vanish("blank.zip", 100);

        let found = find_expired(&env.db, &older_than(500));
        let report = delete_expired(&env.db, &found, &older_than(500));
        assert_eq!(report, SweepReport { deleted: 2, deleted_with_data: 1, skipped: 0, failed: 0 });
        assert!(!env.exists("data.zip") && !env.exists("blank.zip"));
        assert_eq!(read_archive_rating(&env.db, &env.root, "data.zip"), None);
        assert!(favorites::list_files_in_folder(&env.db, 2).is_empty());
    }

    #[test]
    fn delete_rechecks_state_and_skips_ids_that_came_back_or_changed() {
        let env = Env::new("recheck");
        env.file("back.zip", 1);
        env.file("kept.zip", 2);
        env.vanish("back.zip", 100);
        env.vanish("kept.zip", 100);
        let found = find_expired(&env.db, &older_than(500));
        assert_eq!(found.len(), 2);
        // 候補を出した後に、back.zip が戻ってきた（現存）。
        std::fs::write(env.root.join("back.zip"), b"x").unwrap();
        // さらに kept.zip の期限が延びた（判定を厳しくした）。
        let id_kept = lookup(&env.db, &make_key(&env.root, "kept.zip")).unwrap().id;
        let still_fresh = move |rec: &FileRecord| rec.id != id_kept && rec.last_seen < 500;
        let report = delete_expired(&env.db, &found, &still_fresh);
        assert_eq!(report, SweepReport { skipped: 2, ..Default::default() });
        assert!(env.exists("back.zip") && env.exists("kept.zip"));
    }

    #[test]
    fn delete_expired_skips_ids_that_no_longer_exist() {
        let env = Env::new("gone");
        env.file("a.zip", 1);
        env.vanish("a.zip", 100);
        let found = find_expired(&env.db, &older_than(500));
        assert_eq!(delete_expired(&env.db, &found, &older_than(500)).deleted, 1);
        // 同じ候補を2回渡しても、2回目は何もしない。
        assert_eq!(delete_expired(&env.db, &found, &older_than(500)), SweepReport { skipped: 1, ..Default::default() });
    }

    #[test]
    fn long_lived_file_deleted_today_is_not_instantly_expired() {
        // 作成時刻が古くても、消えたと分かった時刻が起点なので、期限内なら候補にならない。
        let env = Env::new("stamp");
        let rec = env.file("old.zip", 1);
        assert!(rec.last_seen > 0);
        let vanished_at = rec.last_seen + 365 * 24 * 3600;
        env.vanish("old.zip", vanished_at);
        let found = find_expired(&env.db, &|r: &FileRecord| vanished_at - r.last_seen >= 0 && r.last_seen < vanished_at - 60 * 24 * 3600);
        assert!(found.is_empty());
    }

    #[test]
    fn pending_records_about_deleted_ids_are_pruned() {
        let env = Env::new("prune");
        let ghost = env.file("ghost.zip", 1);
        let twin = env.file("twin.zip", 1); // 同内容
        favorites::set_membership(&env.db, &env.root, "ghost.zip", &[2]);
        env.vanish("ghost.zip", 100);
        pending::add(&env.db, PendingKind::FavoriteHandover, ghost.id, &[twin.id], 100);
        assert_eq!(pending::count(&env.db), 1);

        let found = find_expired(&env.db, &older_than(500));
        assert_eq!(found.len(), 1);
        delete_expired(&env.db, &found, &older_than(500));
        assert_eq!(pending::count(&env.db), 0, "消えたIDを対象にした未解決は整理される");
    }
}

