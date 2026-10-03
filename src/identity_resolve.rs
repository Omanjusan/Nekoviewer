// 本実装のR4（ワーカー結線）・R5（解決UI）で接続するまでの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

//! 未解決（`identity_pending`）の項目を、ユーザーの選択どおりに解決する操作。
//!
//! - 引き継ぎ元の選択: 選んだ候補のデータを、対象の新ファイルへ複製する（お気に入りは複製しない）
//! - 移動元の選択: 選んだ候補（消えているID）が、対象の新ファイルのパスへ移る（お気に入りを含む全データが付く）
//! - お気に入りの引き継ぎ: 実体の無いお気に入りの所属を、選んだ現存ファイルへ移す
//! - 「引き継がない」: 記録を消すだけ（データは何も動かさない）
//!
//! 適用の前に、前提が崩れていないか（対象にデータが付いていないか、選んだ候補が候補の一覧にあるか、
//! IDが消えていないか）を確かめ、崩れていれば何もせずエラーを返す。成功したら未解決の記録を消す。

use std::sync::{Arc, Mutex};

use redb::{Database, ReadableDatabase};

use crate::file_identity::{adopt_orphan, now_unix, record_by_id_tx, FileRecord};
use crate::file_settings::{modify_for_record, read_effective_for_record, Slot};
use crate::identity_pending::{self as pending, PendingKind, PendingRecord};
use crate::spread_state::{clone_inheritable_data, id_has_user_data, CloneError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// この候補（ファイルID）を参照元として確定する。
    Source(u64),
    /// 引き継がない（空のまま）。
    Skip,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// 未解決の記録が無い（既に解決された・取り下げられた）。
    NotFound,
    /// 選んだIDが、その記録の候補にない。
    NotACandidate,
    /// 対象のIDが消えた。
    SubjectGone,
    /// 選んだ候補のIDが消えた（日数による削除など）。
    SourceGone,
    /// 対象に既にデータが付いている（解決を待つ間に評価などを付けた）。潰さないので適用しない。
    SubjectHasData,
    /// 移し元に、移すお気に入りが無い。
    NoFavorite,
    Db,
}

fn record(db: &Arc<Mutex<Database>>, id: u64) -> Option<FileRecord> {
    let guard = db.lock().ok()?;
    let tx = guard.begin_read().ok()?;
    record_by_id_tx(&tx, id)
}

/// お気に入りの所属を合流させる（昇順・重複なし）。未整理（空）同士は未整理のまま。
fn merge_favorites(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// お気に入りの所属を `from` から `to` へ移す。`to` が既にお気に入りなら所属を合流させる。
/// 先に `to` へ書いてから `from` を外すので、途中で失敗しても、お気に入りが失われることはない
/// （重複して残る可能性はある）。
pub fn transfer_favorite(db: &Arc<Mutex<Database>>, from: &FileRecord, to: &FileRecord) -> Result<(), ResolveError> {
    let moving = read_effective_for_record(db, from)
        .and_then(|e| e.favorite)
        .ok_or(ResolveError::NoFavorite)?;
    let existing = read_effective_for_record(db, to).and_then(|e| e.favorite).unwrap_or_default();
    let merged = merge_favorites(&existing, &moving);
    // 「移し先に既に所属がある」場合の合流で、未整理（空）と所属ありが混ざったら、所属ありを優先する。
    if modify_for_record(db, to, |s| s.favorite = Slot::Set(merged)) != Some(true) {
        return Err(ResolveError::Db);
    }
    if modify_for_record(db, from, |s| s.favorite = Slot::Cleared) != Some(true) {
        return Err(ResolveError::Db);
    }
    Ok(())
}

fn clone_error(e: CloneError) -> ResolveError {
    match e {
        CloneError::SourceGone => ResolveError::SourceGone,
        CloneError::TargetGone => ResolveError::SubjectGone,
        CloneError::TargetHasData => ResolveError::SubjectHasData,
        CloneError::Db => ResolveError::Db,
    }
}

/// 未解決の記録 `pending_id` を、選択どおりに解決する。成功したら記録を消す。
pub fn apply(db: &Arc<Mutex<Database>>, pending_id: u64, choice: Choice) -> Result<(), ResolveError> {
    let rec: PendingRecord = pending::get(db, pending_id).ok_or(ResolveError::NotFound)?;
    let Choice::Source(source_id) = choice else {
        // 引き継がない: データは何も動かさず、記録だけ消す。
        pending::remove(db, pending_id);
        return Ok(());
    };
    if !rec.candidates.contains(&source_id) {
        return Err(ResolveError::NotACandidate);
    }
    let subject = record(db, rec.subject).ok_or(ResolveError::SubjectGone)?;
    let source = record(db, source_id).ok_or(ResolveError::SourceGone)?;
    match rec.kind {
        PendingKind::CopySource => {
            clone_inheritable_data(db, source.id, subject.id).map_err(clone_error)?;
        }
        PendingKind::MoveTarget => {
            // 新ファイルのIDにデータが付いていたら、統合で潰してしまうので適用しない。
            if id_has_user_data(db, subject.id) {
                return Err(ResolveError::SubjectHasData);
            }
            adopt_orphan(db, subject.id, source.id, now_unix()).map_err(|_| ResolveError::Db)?;
        }
        PendingKind::FavoriteHandover => {
            // 対象＝実体の無いお気に入り、候補＝移し先の現存ファイル。
            transfer_favorite(db, &subject, &source)?;
        }
    }
    pending::remove(db, pending_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::favorites;
    use crate::file_identity::ensure_record;
    use crate::identity_pending::AddOutcome;
    use crate::spread_state::{
        open_spread_db, read_archive_rating, read_archive_sort, read_archive_tags, read_bookmark, read_spread,
        read_thumbnail_selection, record_archive_visit, write_archive_rating, write_archive_sort,
        write_archive_tags, write_bookmark_enabled, write_bookmark_position, write_spread,
        write_thumbnail_selection, ThumbnailSelection, ThumbnailSourceKind,
    };
    use crate::types::{PageMode, ReaderSortKey};

    struct Env {
        root: PathBuf,
        db: Arc<Mutex<Database>>,
    }

    impl Env {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("nekoviewer_identity_resolve_test_{}_{}_{}", std::process::id(), nonce, tag));
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
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn selection() -> ThumbnailSelection {
        ThumbnailSelection { entry_name: "p/003.jpg".to_owned(), source_kind: ThumbnailSourceKind::LeftHalf }
    }

    /// データを一通り付ける（お気に入りを含む）。
    fn fill_everything(env: &Env, dir: &Path, name: &str) {
        let db = &env.db;
        assert!(write_archive_rating(db, dir, name, 8));
        assert!(record_archive_visit(db, dir, name));
        assert!(record_archive_visit(db, dir, name));
        assert!(write_bookmark_enabled(db, dir, name, true));
        write_bookmark_position(db, dir, name, "p/010.jpg", 123);
        assert!(write_spread(db, dir, name, PageMode::SpreadLeft, 1));
        assert!(write_archive_sort(db, dir, name, ReaderSortKey::Date, true));
        write_thumbnail_selection(db, dir, name, &selection());
        assert!(write_archive_tags(db, dir, name, &[3, 1]));
        favorites::set_membership(db, dir, name, &[2, 5]);
    }

    fn new_pending(env: &Env, kind: PendingKind, subject: u64, candidates: &[u64]) -> u64 {
        let Some(AddOutcome::Created(id)) = pending::add(&env.db, kind, subject, candidates, 1) else {
            panic!("pending not created")
        };
        id
    }

    // ---- 引き継ぎ元の選択（複製）----

    #[test]
    fn copy_source_clones_everything_except_favorites_and_visits() {
        let env = Env::new("copy");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        fill_everything(&env, &d1, "a.zip");
        let subject = env.file(&d2, "a_copy.zip", 1);
        let other = env.file(&d1, "b.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id, other.id]);

        assert_eq!(apply(&env.db, pid, Choice::Source(source.id)), Ok(()));

        let r = read_archive_rating(&env.db, &d2, "a_copy.zip").unwrap();
        assert_eq!((r.rating_half, r.visit_count, r.last_visit_at), (8, 0, 0), "訪問回数は0に戻す");
        let b = read_bookmark(&env.db, &d2, "a_copy.zip").unwrap();
        assert_eq!((b.enabled, b.last_entry_name.as_str()), (true, "p/010.jpg"));
        assert!(b.archive_fp.is_some(), "FPも付くので、しおりの有効判定が通る");
        assert_eq!(read_spread(&env.db, &d2, "a_copy.zip"), Some((PageMode::SpreadLeft, 1)));
        assert_eq!(read_archive_sort(&env.db, &d2, "a_copy.zip"), Some((ReaderSortKey::Date, true)));
        assert_eq!(read_thumbnail_selection(&env.db, &d2, "a_copy.zip"), Some(selection()));
        assert_eq!(read_archive_tags(&env.db, &d2, "a_copy.zip"), vec![3, 1]);
        assert_eq!(favorites::get_membership(&env.db, &d2, "a_copy.zip"), None, "お気に入りは複製しない");
        // 元は無傷（お気に入りも訪問回数も残る）。
        assert_eq!(favorites::get_membership(&env.db, &d1, "a.zip"), Some(vec![2, 5]));
        assert_eq!(read_archive_rating(&env.db, &d1, "a.zip").unwrap().visit_count, 2);
        // 記録は消える。
        assert_eq!(pending::count(&env.db), 0);
    }

    #[test]
    fn copy_source_clones_legacy_only_values_of_the_source() {
        let env = Env::new("copy_legacy");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        // IDだけ作られ、評価・タグは旧v1のまま（未移行）。
        let key = crate::spread_state::make_key(&d1, "a.zip");
        {
            let g = env.db.lock().unwrap();
            let tx = g.begin_write().unwrap();
            {
                let mut t = tx.open_table(crate::spread_state::ARCHIVE_RATING_TABLE_V1).unwrap();
                t.insert(key.as_str(), (6u8, 3u32, 9i64)).unwrap();
                let mut tags = tx.open_table(crate::spread_state::ARCHIVE_TAGS_TABLE_V1).unwrap();
                tags.insert(key.as_str(), 7u64.to_le_bytes().as_slice()).unwrap();
            }
            tx.commit().unwrap();
        }
        let subject = env.file(&d2, "copy.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id]);
        assert_eq!(apply(&env.db, pid, Choice::Source(source.id)), Ok(()));
        assert_eq!(read_archive_rating(&env.db, &d2, "copy.zip").unwrap().rating_half, 6);
        assert_eq!(read_archive_tags(&env.db, &d2, "copy.zip"), vec![7]);
    }

    #[test]
    fn copy_source_refuses_when_the_subject_already_has_data() {
        let env = Env::new("copy_has_data");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "a.zip", 8));
        let subject = env.file(&d2, "copy.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id]);
        // 解決を待つ間に、対象へ評価を付けた。
        assert!(write_archive_rating(&env.db, &d2, "copy.zip", 2));
        assert_eq!(apply(&env.db, pid, Choice::Source(source.id)), Err(ResolveError::SubjectHasData));
        assert_eq!(read_archive_rating(&env.db, &d2, "copy.zip").unwrap().rating_half, 2, "潰さない");
        assert_eq!(pending::count(&env.db), 1, "記録は残る（取り下げは prune の役目）");
    }

    // ---- 移動元の選択（統合）----

    #[test]
    fn move_target_moves_the_chosen_orphan_to_the_new_path_with_everything() {
        let env = Env::new("move");
        let (d1, d2) = (env.dir("old"), env.dir("new"));
        let a = env.file(&d1, "a.zip", 1);
        fill_everything(&env, &d1, "a.zip");
        let b = env.file(&d1, "b.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "b.zip", 2));
        // 2つとも消えた（移動された）。新しいファイルが同内容で現れた。
        std::fs::remove_file(d1.join("a.zip")).unwrap();
        std::fs::remove_file(d1.join("b.zip")).unwrap();
        let subject = env.file(&d2, "c.zip", 1);
        let pid = new_pending(&env, PendingKind::MoveTarget, subject.id, &[a.id, b.id]);

        assert_eq!(apply(&env.db, pid, Choice::Source(a.id)), Ok(()));

        // 新しいパスは孤児Aのものになり、お気に入りを含む全データが付いて来る。
        let moved = crate::file_identity::lookup(&env.db, &crate::spread_state::make_key(&d2, "c.zip")).unwrap();
        assert_eq!(moved.id, a.id);
        assert!(moved.history.iter().any(|h| h.ends_with("a.zip")));
        assert_eq!(read_archive_rating(&env.db, &d2, "c.zip").unwrap().rating_half, 8);
        assert_eq!(read_archive_rating(&env.db, &d2, "c.zip").unwrap().visit_count, 2, "移動なので訪問も付く");
        assert_eq!(favorites::get_membership(&env.db, &d2, "c.zip"), Some(vec![2, 5]));
        assert_eq!(read_archive_tags(&env.db, &d2, "c.zip"), vec![3, 1]);
        // 空だった新しいID（subject）は消え、選ばなかった候補Bはそのまま残る。
        assert!(record(&env.db, subject.id).is_none());
        assert!(record(&env.db, b.id).is_some());
        assert_eq!(pending::count(&env.db), 0);
    }

    #[test]
    fn move_target_refuses_when_the_new_file_already_has_data() {
        let env = Env::new("move_has_data");
        let (d1, d2) = (env.dir("old"), env.dir("new"));
        let a = env.file(&d1, "a.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "a.zip", 8));
        let b = env.file(&d1, "b.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "b.zip", 2));
        // 移動元の候補が2つ（データが食い違う）で、どちらも消えた。新しいファイルは空のIDで作られる。
        std::fs::remove_file(d1.join("a.zip")).unwrap();
        std::fs::remove_file(d1.join("b.zip")).unwrap();
        let subject = env.file(&d2, "c.zip", 1);
        assert_ne!(subject.id, a.id);
        let pid = new_pending(&env, PendingKind::MoveTarget, subject.id, &[a.id, b.id]);
        assert!(favorites::get_membership(&env.db, &d2, "c.zip").is_none());
        favorites::set_membership(&env.db, &d2, "c.zip", &[1]);
        assert_eq!(apply(&env.db, pid, Choice::Source(a.id)), Err(ResolveError::SubjectHasData));
        // 何も動いていない。
        assert_eq!(favorites::get_membership(&env.db, &d2, "c.zip"), Some(vec![1]));
        assert!(record(&env.db, subject.id).is_some());
    }

    // ---- お気に入りの引き継ぎ ----

    #[test]
    fn favorite_handover_moves_the_membership_to_the_chosen_file() {
        let env = Env::new("fav");
        let (d1, d2) = (env.dir("old"), env.dir("alive"));
        let orphan = env.file(&d1, "d.zip", 1);
        favorites::set_membership(&env.db, &d1, "d.zip", &[2, 3]);
        // 「コピーしてから元を消す」順。元が生きている間にコピーが現れ、別IDになる。
        let alive = env.file(&d2, "d_copy.zip", 1);
        assert_ne!(alive.id, orphan.id);
        std::fs::remove_file(d1.join("d.zip")).unwrap();
        let pid = new_pending(&env, PendingKind::FavoriteHandover, orphan.id, &[alive.id]);

        assert_eq!(apply(&env.db, pid, Choice::Source(alive.id)), Ok(()));

        assert_eq!(favorites::get_membership(&env.db, &d2, "d_copy.zip"), Some(vec![2, 3]));
        assert_eq!(favorites::get_membership(&env.db, &d1, "d.zip"), None, "移し元からは外れる");
        assert_eq!(pending::count(&env.db), 0);
    }

    #[test]
    fn favorite_handover_merges_with_the_existing_membership_of_the_target() {
        let env = Env::new("fav_merge");
        let (d1, d2) = (env.dir("old"), env.dir("alive"));
        let orphan = env.file(&d1, "d.zip", 1);
        favorites::set_membership(&env.db, &d1, "d.zip", &[2]);
        let alive = env.file(&d2, "x.zip", 1);
        favorites::set_membership(&env.db, &d2, "x.zip", &[3]);
        transfer_favorite(&env.db, &orphan, &alive).unwrap();
        assert_eq!(favorites::get_membership(&env.db, &d2, "x.zip"), Some(vec![2, 3]));
        // 未整理（空）同士は未整理のまま。
        let o2 = env.file(&d1, "e.zip", 2);
        favorites::set_membership(&env.db, &d1, "e.zip", &[]);
        let a2 = env.file(&d2, "y.zip", 2);
        transfer_favorite(&env.db, &o2, &a2).unwrap();
        assert_eq!(favorites::get_membership(&env.db, &d2, "y.zip"), Some(vec![]));
    }

    #[test]
    fn favorite_handover_without_a_favorite_is_rejected_and_changes_nothing() {
        let env = Env::new("fav_none");
        let (d1, d2) = (env.dir("old"), env.dir("alive"));
        let orphan = env.file(&d1, "d.zip", 1);
        let alive = env.file(&d2, "x.zip", 1);
        favorites::set_membership(&env.db, &d2, "x.zip", &[4]);
        assert_eq!(transfer_favorite(&env.db, &orphan, &alive), Err(ResolveError::NoFavorite));
        assert_eq!(favorites::get_membership(&env.db, &d2, "x.zip"), Some(vec![4]));
    }

    // ---- 共通の確認 ----

    #[test]
    fn skip_removes_only_the_pending_record() {
        let env = Env::new("skip");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "a.zip", 8));
        let subject = env.file(&d2, "copy.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id]);
        assert_eq!(apply(&env.db, pid, Choice::Skip), Ok(()));
        assert_eq!(pending::count(&env.db), 0);
        assert_eq!(read_archive_rating(&env.db, &d2, "copy.zip"), None, "空のまま");
        assert_eq!(read_archive_rating(&env.db, &d1, "a.zip").unwrap().rating_half, 8);
    }

    #[test]
    fn invalid_requests_are_rejected_without_side_effects() {
        let env = Env::new("invalid");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "a.zip", 8));
        let subject = env.file(&d2, "copy.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id]);
        assert_eq!(apply(&env.db, 9999, Choice::Skip), Err(ResolveError::NotFound));
        assert_eq!(apply(&env.db, pid, Choice::Source(subject.id)), Err(ResolveError::NotACandidate));
        assert_eq!(apply(&env.db, pid, Choice::Source(424242)), Err(ResolveError::NotACandidate));
        assert_eq!(pending::count(&env.db), 1);
        assert_eq!(read_archive_rating(&env.db, &d2, "copy.zip"), None);
        // 候補のIDが消えた（日数による削除など）場合。
        let gone = new_pending(&env, PendingKind::CopySource, subject.id + 1000, &[source.id]);
        assert_eq!(apply(&env.db, gone, Choice::Source(source.id)), Err(ResolveError::SubjectGone));
    }

    #[test]
    fn resolving_twice_reports_not_found_the_second_time() {
        let env = Env::new("twice");
        let (d1, d2) = (env.dir("src"), env.dir("dst"));
        let source = env.file(&d1, "a.zip", 1);
        assert!(write_archive_rating(&env.db, &d1, "a.zip", 8));
        let subject = env.file(&d2, "copy.zip", 1);
        let pid = new_pending(&env, PendingKind::CopySource, subject.id, &[source.id]);
        assert_eq!(apply(&env.db, pid, Choice::Source(source.id)), Ok(()));
        assert_eq!(apply(&env.db, pid, Choice::Source(source.id)), Err(ResolveError::NotFound));
    }
}
