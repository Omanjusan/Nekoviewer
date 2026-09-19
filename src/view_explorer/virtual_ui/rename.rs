//! 仮想フォルダの表示名の変更。変えるのは仮想側の名前だけで、実フォルダ名・実パスには触れない。
//!
//! 入口（右クリックのメニューとF2）は同じ関数から `RenameTarget` を作ってここへ合流する。
//! 失敗は入力欄では防げないものだけ（DBエラー・対象が消えた・対象が別のノードに変わった）で、
//! トーストで知らせる。成功時は何も出さない。

// フェーズB（UI接続）までの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

use crate::i18n;
use crate::virtual_folders::{self, VirtualFolderError};

use super::delete::{same_node, DeleteTarget, SameNode};

/// ダイアログを開いた時点の対象（id・実パス・名前）。削除と同じ照合用のスナップショット。
/// （`next_id = max+1` のため、idが再利用されて別のノードになりうる）
pub(super) type RenameTarget = DeleteTarget;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RenameOutcome {
    Renamed,
    /// 整形後の名前が今の名前と同じ。DBは書かない。
    Unchanged,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RenameError {
    /// ダイアログを開いた後に、対象がDBから消えた
    NotFound,
    /// ダイアログを開いた後に、同じidが別のノードになっていた
    Changed,
    /// 整形後に空、または長すぎる（UIは入力制限とOK無効化で防ぐ）
    NameInvalid,
    Db,
}

impl RenameError {
    pub(super) fn message(&self) -> String {
        let t = i18n::t();
        let reason = match self {
            Self::NotFound => t.virtual_rename_reason_not_found(),
            Self::Changed => t.virtual_delete_reason_changed(),
            Self::NameInvalid => t.virtual_reason_name_invalid(),
            Self::Db => t.virtual_reason_db(),
        };
        t.virtual_rename_failed(reason)
    }
}

/// 入力欄の文字列を名前にする。前後の空白を削り、空なら None（OKボタンを無効にする）。
pub(super) fn normalize_name(input: &str) -> Option<String> {
    let name = input.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// 対象の表示名を変更する。開いた時点と同じノードでなければDBを変えずに中止する。
pub(super) fn rename_virtual_node(
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    target: &RenameTarget,
    input: &str,
) -> Result<RenameOutcome, RenameError> {
    let Some(name) = normalize_name(input) else {
        return Err(RenameError::NameInvalid);
    };
    match same_node(virtual_folders::get_node(db, target.id).as_ref(), target) {
        SameNode::Gone => return Err(RenameError::NotFound),
        SameNode::Changed => return Err(RenameError::Changed),
        SameNode::Same => {}
    }
    if name == target.name {
        return Ok(RenameOutcome::Unchanged);
    }
    match virtual_folders::rename_node(db, target.id, &name) {
        Ok(()) => Ok(RenameOutcome::Renamed),
        Err(VirtualFolderError::NotFound) => Err(RenameError::NotFound),
        Err(VirtualFolderError::NameEmpty | VirtualFolderError::NameTooLong) => Err(RenameError::NameInvalid),
        Err(_) => Err(RenameError::Db),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::virtual_folders::{VirtualNode, MAX_NAME_CHARS};

    fn temp_db() -> std::sync::Arc<std::sync::Mutex<redb::Database>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nekoviewer_vf_rename_test_{}_{}.redb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = std::sync::Arc::new(std::sync::Mutex::new(redb::Database::create(&path).unwrap()));
        virtual_folders::init_virtual_folder_tables(&db).unwrap();
        db
    }

    fn target_of(n: &VirtualNode) -> RenameTarget {
        RenameTarget { id: n.id, real: n.real_path.clone(), name: n.name.clone() }
    }

    #[test]
    fn normalize_trims_and_rejects_blank() {
        assert_eq!(normalize_name("  漫画 \t").as_deref(), Some("漫画"));
        assert_eq!(normalize_name("a b").as_deref(), Some("a b"));
        assert_eq!(normalize_name(""), None);
        assert_eq!(normalize_name(" \u{3000} "), None);
    }

    #[test]
    fn rename_changes_only_the_virtual_name() {
        let db = temp_db();
        let n = virtual_folders::add_node(&db, 0, Path::new("/real/a"), "a").unwrap();
        let other = virtual_folders::add_node(&db, 0, Path::new("/real/b"), "b").unwrap();
        assert_eq!(rename_virtual_node(&db, &target_of(&n), "  新しい名前 "), Ok(RenameOutcome::Renamed));
        let after = virtual_folders::get_node(&db, n.id).unwrap();
        assert_eq!(after.name, "新しい名前");
        assert_eq!(after.real_path, PathBuf::from("/real/a"));
        assert_eq!(virtual_folders::get_node(&db, other.id).unwrap().name, "b");
    }

    #[test]
    fn rename_allows_same_name_as_a_sibling() {
        let db = temp_db();
        virtual_folders::add_node(&db, 0, Path::new("/real/a"), "a").unwrap();
        let b = virtual_folders::add_node(&db, 0, Path::new("/real/b"), "b").unwrap();
        assert_eq!(rename_virtual_node(&db, &target_of(&b), "a"), Ok(RenameOutcome::Renamed));
    }

    #[test]
    fn rename_to_same_name_is_unchanged() {
        let db = temp_db();
        let n = virtual_folders::add_node(&db, 0, Path::new("/real/a"), "a").unwrap();
        assert_eq!(rename_virtual_node(&db, &target_of(&n), " a "), Ok(RenameOutcome::Unchanged));
    }

    #[test]
    fn rename_rejects_invalid_names_without_touching_db() {
        let db = temp_db();
        let n = virtual_folders::add_node(&db, 0, Path::new("/real/a"), "a").unwrap();
        let t = target_of(&n);
        assert_eq!(rename_virtual_node(&db, &t, "  "), Err(RenameError::NameInvalid));
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(rename_virtual_node(&db, &t, &long), Err(RenameError::NameInvalid));
        assert_eq!(virtual_folders::get_node(&db, n.id).unwrap().name, "a");
    }

    #[test]
    fn rename_rejects_reused_id_and_missing_target() {
        let db = temp_db();
        let old = virtual_folders::add_node(&db, 0, Path::new("/old"), "old").unwrap();
        let stale = target_of(&old);
        virtual_folders::remove_node(&db, old.id).unwrap();
        assert_eq!(rename_virtual_node(&db, &stale, "x"), Err(RenameError::NotFound));
        // 最大idの削除後の追加で、同じidが別のノードに再利用される
        let reused = virtual_folders::add_node(&db, 0, Path::new("/new"), "new").unwrap();
        assert_eq!(reused.id, old.id);
        assert_eq!(rename_virtual_node(&db, &stale, "x"), Err(RenameError::Changed));
        assert_eq!(virtual_folders::get_node(&db, reused.id).unwrap().name, "new");
    }
}
