//! 実体の無いお気に入り（ゴーストID）を、ユーザーが指したフォルダの中の同内容ファイルへ再紐付けする。
//!
//! 自動の移動追従は「移動先のフォルダを開いた時」にしか働かない。移動先を開いていない・追従が曖昧だった
//! ゴーストを、ユーザーの指定で拾うための汎用層（お気に入り専用ではなく、IDレコード単位）。
//!
//! - 照合: ゴーストのサイズで絞ってからFPを計算し、FPが一致するファイルを探す。
//! - 移し先1つ・ゴースト1つ以上:
//!   - 移し先にデータが無ければ、ゴーストIDを丸ごと移し先のパスへ移す（お気に入りを含む全データが付く）。
//!   - 移し先にデータがあれば、移し先を正としてお気に入りだけを合流させ（所属の和集合）、ゴーストIDは削除する。
//! - 移し先が複数（同内容のファイルが複数）: 決められないので適用せず、お気に入りの引き継ぎ
//!   （`PendingKind::FavoriteHandover`）として未解決に登録する。
//! - 実在するパスのID（ゴーストでない）は対象外。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{
    adopt_orphan, delete_id, lookup, now_unix, observe, probe_record, record_by_id_tx, resolve_observed_with_probe,
    FileRecord, Fp, Observation, Observed, Presence,
};
use crate::identity_pending::{self as pending, PendingKind};
use crate::identity_resolve::{transfer_favorite, ResolveError};
use crate::spread_state::{id_has_user_data, make_key};

/// 再紐付けの結果（件数はゴーストID単位）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RelinkReport {
    /// ゴーストIDを丸ごと移し先へ移した。
    pub moved: usize,
    /// 移し先を正としてお気に入りを合流し、ゴーストIDを削除した。
    pub merged: usize,
    /// 移し先の候補が複数で決められず、未解決（お気に入りの引き継ぎ）に登録した。
    pub ambiguous: usize,
    /// フォルダの中に同内容のファイルが無かった（FPなしの0バイトを含む）。
    pub not_found: usize,
    /// 対象外（実在するパス、または既に無いID、お気に入りでないID）。
    pub skipped: usize,
    /// DBエラーなどで適用できなかった。
    pub failed: usize,
}

fn record(db: &Arc<Mutex<Database>>, id: u64) -> Option<FileRecord> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    record_by_id_tx(&tx, id)
}

/// `folder`（`recursive` ならサブフォルダも）の中の、サイズが `sizes` のどれかに一致するファイル。
/// シンボリックリンクのフォルダはたどらない（循環を避ける）。
fn collect_candidates(folder: &Path, recursive: bool, sizes: &HashSet<u64>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if recursive {
                    stack.push(entry.path());
                }
            } else if ft.is_file() && entry.metadata().is_ok_and(|m| sizes.contains(&m.len())) {
                out.push(entry.path());
            }
        }
    }
    out
}

/// ゴースト `ghost_ids` を、`folder` の中の同内容ファイルへ再紐付けする。
pub fn relink(db: &Arc<Mutex<Database>>, ghost_ids: &[u64], folder: &Path, recursive: bool) -> RelinkReport {
    let mut report = RelinkReport::default();

    // 1. 対象のゴーストを確定する（実在するパスのIDは外す）。FPごとにまとめる。
    let mut seen = HashSet::new();
    let mut by_fp: HashMap<Fp, Vec<FileRecord>> = HashMap::new();
    for &id in ghost_ids {
        if !seen.insert(id) {
            continue;
        }
        let Some(rec) = record(db, id) else {
            report.skipped += 1;
            continue;
        };
        if probe_record(&rec) == Presence::Present {
            report.skipped += 1;
            continue;
        }
        match rec.fp {
            Some(fp) => by_fp.entry(fp).or_default().push(rec),
            None => report.not_found += 1,
        }
    }
    if by_fp.is_empty() {
        return report;
    }
    let ghost_set: HashSet<u64> = by_fp.values().flatten().map(|r| r.id).collect();

    // 2. サイズで絞ってからFPを計算し、ゴーストのFPに一致するファイルを集める。
    let sizes: HashSet<u64> = by_fp.values().flatten().map(|r| r.size).collect();
    let now = now_unix();
    let mut found: HashMap<Fp, Vec<(PathBuf, Observation)>> = HashMap::new();
    for path in collect_candidates(folder, recursive, &sizes) {
        if let Observed::Ready(obs) = observe(&path, now)
            && let Some(fp) = obs.fp
            && by_fp.contains_key(&fp)
        {
            found.entry(fp).or_default().push((path, obs));
        }
    }

    // 3. FPごとに適用する。新ファイルのIDは、ゴーストを移動元・複製元の候補から外した状態で作り、
    //    付け替えは自分で決める（解決の自動判定に任せない）。
    let probe = |rec: &FileRecord| {
        if ghost_set.contains(&rec.id) { Presence::Offline } else { probe_record(rec) }
    };
    for (fp, ghosts) in &by_fp {
        let files = found.get(fp).map(Vec::as_slice).unwrap_or_default();
        match files {
            [] => report.not_found += ghosts.len(),
            [(path, obs)] => {
                let key = file_key(path);
                for ghost in ghosts {
                    // 前の付け替えでIDが入れ替わりうるので、毎回、移し先のIDを引き直す。
                    let target = lookup(db, &key).or_else(|| resolve_observed_with_probe(db, &key, obs, &probe));
                    let Some(target) = target else {
                        report.failed += 1;
                        continue;
                    };
                    apply_one(db, ghost, &target, &mut report);
                }
            }
            many => {
                let candidates: Vec<u64> = many
                    .iter()
                    .filter_map(|(path, obs)| {
                        let key = file_key(path);
                        lookup(db, &key).or_else(|| resolve_observed_with_probe(db, &key, obs, &probe))
                    })
                    .map(|r| r.id)
                    .collect();
                for ghost in ghosts {
                    match pending::add(db, PendingKind::FavoriteHandover, ghost.id, &candidates, now) {
                        Some(_) => report.ambiguous += 1,
                        None => report.failed += 1,
                    }
                }
            }
        }
    }

    // 4. 付け替えで意味を失った未解決の記録を除く。
    pending::prune_real(db);
    report
}

fn file_key(path: &Path) -> String {
    let dir = path.parent().unwrap_or(Path::new(""));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    make_key(dir, &name)
}

/// ゴースト1つを移し先 `target` へ付け替える。
fn apply_one(db: &Arc<Mutex<Database>>, ghost: &FileRecord, target: &FileRecord, report: &mut RelinkReport) {
    if target.id == ghost.id {
        // 解決の途中で同じIDが移し先に付いていた（既に追従済み）。
        report.moved += 1;
    } else if !id_has_user_data(db, target.id) {
        match adopt_orphan(db, target.id, ghost.id, now_unix()) {
            Ok(()) => report.moved += 1,
            Err(_) => report.failed += 1,
        }
    } else {
        match transfer_favorite(db, ghost, target) {
            Ok(()) => match delete_id(db, ghost.id) {
                Ok(()) => report.merged += 1,
                Err(_) => report.failed += 1,
            },
            // ゴーストにお気に入りが無い（呼び出し側の指定ミス）。何も動かさない。
            Err(ResolveError::NoFavorite) => report.skipped += 1,
            Err(_) => report.failed += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites;
    use crate::file_identity::ensure_record;
    use crate::file_settings::read_effective_for_record;
    use crate::spread_state::{open_spread_db, read_archive_rating, write_archive_rating};

    struct Env {
        root: PathBuf,
        db: Arc<Mutex<Database>>,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir()
                .join(format!("nekoviewer_identity_relink_test_{}_{}_{}", std::process::id(), nonce, tag));
            std::fs::create_dir_all(&root).unwrap();
            let db = open_spread_db(&root).unwrap();
            favorites::init_favorite_tables(&db).unwrap();
            Self { root, db }
        }

        fn dir(&self, name: &str) -> PathBuf {
            let d = self.root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        /// 十分古いmtimeのファイルを作り、IDを解決して返す。内容は `seed` で決まる（同じ seed は同内容）。
        fn file(&self, dir: &Path, name: &str, seed: u8) -> FileRecord {
            let p = dir.join(name);
            std::fs::write(&p, vec![seed; 3000 + seed as usize]).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
            ensure_record(&self.db, dir, name).unwrap()
        }

        /// お気に入り一覧（全フォルダ横断・未整理含む）のファイル名を昇順で返す。
        fn favorite_names(&self, folder_id: u8) -> Vec<String> {
            let mut v: Vec<String> =
                favorites::list_files_in_folder(&self.db, folder_id).into_iter().map(|(d, n)| format!("{}/{}", d.file_name().unwrap().to_string_lossy(), n)).collect();
            v.sort();
            v
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn single_ghost_moves_whole_id_when_target_has_no_data() {
        let env = Env::new("move");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let ghost = env.file(&a, "x.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        assert!(write_archive_rating(&env.db, &a, "x.zip", 7));
        std::fs::rename(a.join("x.zip"), b.join("x.zip")).unwrap();

        let report = relink(&env.db, &[ghost.id], &b, false);
        assert_eq!(report, RelinkReport { moved: 1, ..Default::default() });
        assert_eq!(env.favorite_names(2), vec!["b/x.zip".to_owned()]);
        // IDごと移ったので、★も付いてくる。
        assert_eq!(read_archive_rating(&env.db, &b, "x.zip").map(|r| r.rating_half), Some(7));
        assert_eq!(lookup(&env.db, &make_key(&b, "x.zip")).map(|r| r.id), Some(ghost.id));
    }

    #[test]
    fn existing_target_data_wins_and_favorites_are_merged_and_ghost_is_deleted() {
        let env = Env::new("merge");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let ghost = env.file(&a, "x.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        assert!(write_archive_rating(&env.db, &a, "x.zip", 9));
        // 同内容の新ファイル（コピー）に、別のデータとお気に入りが既にある。
        let target = env.file(&b, "x.zip", 1);
        assert_ne!(target.id, ghost.id);
        favorites::set_membership(&env.db, &b, "x.zip", &[5]);
        assert!(write_archive_rating(&env.db, &b, "x.zip", 3));
        std::fs::remove_file(a.join("x.zip")).unwrap();

        let report = relink(&env.db, &[ghost.id], &b, false);
        assert_eq!(report, RelinkReport { merged: 1, ..Default::default() });
        // 正は移し先。★は上書きされず、お気に入りは和集合になる。
        assert_eq!(read_archive_rating(&env.db, &b, "x.zip").map(|r| r.rating_half), Some(3));
        assert_eq!(env.favorite_names(2), vec!["b/x.zip".to_owned()]);
        assert_eq!(env.favorite_names(5), vec!["b/x.zip".to_owned()]);
        // ゴーストIDは消え、お気に入りタブにもv1の行として戻らない。
        assert!(record(&env.db, ghost.id).is_none());
        assert_eq!(favorites::list_files_in_folder(&env.db, 2).len(), 1);
    }

    #[test]
    fn cleared_target_favorite_is_merged_like_empty() {
        let env = Env::new("cleared");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let ghost = env.file(&a, "x.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        let target = env.file(&b, "x.zip", 1);
        assert!(write_archive_rating(&env.db, &b, "x.zip", 3));
        crate::file_settings::modify_for_record(&env.db, &target, |s| s.favorite = crate::file_settings::Slot::Cleared);
        std::fs::remove_file(a.join("x.zip")).unwrap();

        let report = relink(&env.db, &[ghost.id], &b, false);
        assert_eq!(report.merged, 1);
        let fav = read_effective_for_record(&env.db, &lookup(&env.db, &make_key(&b, "x.zip")).unwrap())
            .and_then(|e| e.favorite);
        assert_eq!(fav, Some(vec![2]));
    }

    #[test]
    fn two_ghosts_with_the_same_content_all_merge_into_one_file() {
        let env = Env::new("many_ghosts");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let g1 = env.file(&a, "x.zip", 1);
        let g2 = env.file(&a, "y.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        favorites::set_membership(&env.db, &a, "y.zip", &[3]);
        std::fs::rename(a.join("x.zip"), b.join("n.zip")).unwrap();
        std::fs::remove_file(a.join("y.zip")).unwrap();

        let report = relink(&env.db, &[g1.id, g2.id], &b, false);
        assert_eq!(report.moved + report.merged, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(env.favorite_names(2), vec!["b/n.zip".to_owned()]);
        assert_eq!(env.favorite_names(3), vec!["b/n.zip".to_owned()]);
        // ゴーストは残らない（お気に入りの一覧は1件のみ）。
        assert_eq!(favorites::list_files_in_folder(&env.db, 2).len() + favorites::list_files_in_folder(&env.db, 3).len(), 2);
    }

    #[test]
    fn several_same_content_files_become_a_pending_favorite_handover() {
        let env = Env::new("ambiguous");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let ghost = env.file(&a, "x.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        std::fs::remove_file(a.join("x.zip")).unwrap();
        // 未訪問（IDなし）の同内容ファイルが2つ。
        for n in ["p.zip", "q.zip"] {
            std::fs::write(b.join(n), vec![1u8; 3001]).unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(b.join(n)).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(1000)).unwrap();
        }

        let report = relink(&env.db, &[ghost.id], &b, false);
        assert_eq!(report, RelinkReport { ambiguous: 1, ..Default::default() });
        let list = pending::list(&env.db);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, PendingKind::FavoriteHandover);
        assert_eq!(list[0].subject, ghost.id);
        assert_eq!(list[0].candidates.len(), 2);
        // 自動では何も動かさない（ゴーストはそのまま、候補にお気に入りは付かない）。
        assert!(record(&env.db, ghost.id).is_some());
        assert!(favorites::list_files_in_folder(&env.db, 2).iter().all(|(_, n)| n == "x.zip"));
    }

    #[test]
    fn present_files_and_missing_matches_are_counted_not_touched() {
        let env = Env::new("skips");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let present = env.file(&a, "keep.zip", 4);
        favorites::set_membership(&env.db, &a, "keep.zip", &[2]);
        let ghost = env.file(&a, "gone.zip", 5);
        favorites::set_membership(&env.db, &a, "gone.zip", &[2]);
        std::fs::remove_file(a.join("gone.zip")).unwrap();

        let report = relink(&env.db, &[present.id, ghost.id, 9999], &b, false);
        assert_eq!(report, RelinkReport { skipped: 2, not_found: 1, ..Default::default() });
        assert_eq!(favorites::list_files_in_folder(&env.db, 2).len(), 2);
    }

    #[test]
    fn subfolders_are_searched_only_when_requested() {
        let env = Env::new("recursive");
        let (a, b) = (env.dir("a"), env.dir("b"));
        let sub = env.dir("b/deep/er");
        let ghost = env.file(&a, "x.zip", 1);
        favorites::set_membership(&env.db, &a, "x.zip", &[2]);
        std::fs::rename(a.join("x.zip"), sub.join("x.zip")).unwrap();

        assert_eq!(relink(&env.db, &[ghost.id], &b, false), RelinkReport { not_found: 1, ..Default::default() });
        assert_eq!(relink(&env.db, &[ghost.id], &b, true), RelinkReport { moved: 1, ..Default::default() });
        assert_eq!(env.favorite_names(2), vec!["er/x.zip".to_owned()]);
    }
}
