//! 仮想フォルダの削除。子孫は常に連動して消える（`remove_node`）。実フォルダには一切触れない。
//!
//! 実パスの存否を見ないので、リンク切れのノードも通常どおり削除できる。
//! 失敗しうるのはDBエラーと「ダイアログを開いた後に対象が別のノードに変わった」場合だけで、
//! すでに削除済み（`NotFound`）は成功として静かに終える。

use crate::i18n;
use crate::virtual_folders::{self, VirtualFolderError, VirtualNode};

use super::*;

/// 削除ダイアログを開いた時点の対象。OK時に同じノードか確かめるため、idと一緒に実パスと名前を持つ。
/// （`next_id = max+1` のため、最大idのノードを消すとidが再利用されうる）
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeleteTarget {
    pub id: u32,
    pub real: PathBuf,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DeleteError {
    /// ダイアログを開いた後に、同じidが別のノードになっていた
    Changed,
    Db,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SameNode {
    Same,
    /// すでにDBに無い（削除済み）
    Gone,
    Changed,
}

pub(super) fn same_node(current: Option<&VirtualNode>, target: &DeleteTarget) -> SameNode {
    match current {
        None => SameNode::Gone,
        Some(n) if n.real_path == target.real && n.name == target.name => SameNode::Same,
        Some(_) => SameNode::Changed,
    }
}

/// 対象を削除する。戻り値は削除したノード数（すでに削除済みなら0）。
pub(super) fn delete_virtual_node(
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    target: &DeleteTarget,
) -> Result<usize, DeleteError> {
    match same_node(virtual_folders::get_node(db, target.id).as_ref(), target) {
        SameNode::Gone => return Ok(0),
        SameNode::Changed => return Err(DeleteError::Changed),
        SameNode::Same => {}
    }
    match virtual_folders::remove_node(db, target.id) {
        Ok(n) => Ok(n),
        // 確認の直後にすでに消えていた場合も、削除済みとして終える
        Err(VirtualFolderError::NotFound) => Ok(0),
        Err(_) => Err(DeleteError::Db),
    }
}

impl NekoviewApp {
    /// 削除ダイアログのOK。表示中のノードが削除される枝の中にあれば、削除した最上位ノードの親を開く
    /// （親の実フォルダが開けなければファイルなしの表示、`/` なら仮想ルート）。
    pub(super) fn run_virtual_delete(&mut self, target: DeleteTarget) {
        let Some(db) = self.spread_db.clone() else {
            self.set_delete_failed(&DeleteError::Db);
            return;
        };
        let branch = self.virtual_state.subtree_ids(target.id);
        let parent = self
            .virtual_state
            .nodes
            .iter()
            .find(|n| n.id == target.id)
            .map_or(ROOT, |n| n.parent);
        let viewing_inside = self.viewing_virtual_node.is_some_and(|v| branch.contains(&v));

        match delete_virtual_node(&db, &target) {
            Ok(_) => {
                if viewing_inside {
                    // 読み直しで「表示中のノードが消えた」と実表示に戻されないよう、先に親へ寄せる
                    self.viewing_virtual_node = Some(parent);
                }
                self.refresh_virtual_nodes();
                if viewing_inside {
                    self.enter_virtual_node(parent);
                }
                self.set_toast(i18n::t().virtual_delete_ok());
            }
            Err(e) => {
                // 対象が変わっていた場合は、表示が古い可能性があるので読み直す
                self.refresh_virtual_nodes();
                self.set_delete_failed(&e);
            }
        }
    }

    fn set_delete_failed(&mut self, e: &DeleteError) {
        let reason = match e {
            DeleteError::Changed => i18n::t().virtual_delete_reason_changed(),
            DeleteError::Db => i18n::t().virtual_reason_db(),
        };
        self.set_toast(i18n::t().virtual_delete_failed(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> std::sync::Arc<std::sync::Mutex<redb::Database>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nekoviewer_vf_delete_test_{}_{}.redb",
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

    fn target_of(n: &VirtualNode) -> DeleteTarget {
        DeleteTarget { id: n.id, real: n.real_path.clone(), name: n.name.clone() }
    }

    fn vnode(id: u32, real: &str, name: &str) -> VirtualNode {
        VirtualNode { id, parent_id: 0, real_path: PathBuf::from(real), name: name.to_string(), order: 0 }
    }

    #[test]
    fn same_node_distinguishes_same_gone_changed() {
        let n = vnode(3, "/a", "a");
        let t = target_of(&n);
        assert_eq!(same_node(Some(&n), &t), SameNode::Same);
        assert_eq!(same_node(None, &t), SameNode::Gone);
        // idが再利用されて別の実パス／別の名前のノードになっている
        assert_eq!(same_node(Some(&vnode(3, "/b", "a")), &t), SameNode::Changed);
        assert_eq!(same_node(Some(&vnode(3, "/a", "renamed")), &t), SameNode::Changed);
    }

    #[test]
    fn delete_removes_whole_branch_and_reports_count() {
        let db = temp_db();
        let root = virtual_folders::add_node(&db, 0, Path::new("/r"), "r").unwrap();
        let child = virtual_folders::add_node(&db, root.id, Path::new("/r/c"), "c").unwrap();
        virtual_folders::add_node(&db, child.id, Path::new("/r/c/g"), "g").unwrap();
        let other = virtual_folders::add_node(&db, 0, Path::new("/o"), "o").unwrap();
        assert_eq!(delete_virtual_node(&db, &target_of(&root)), Ok(3));
        let left = virtual_folders::list_nodes(&db);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, other.id);
    }

    #[test]
    fn delete_works_for_broken_link_node() {
        // 実パスが存在しなくても（リンク切れでも）削除できる
        let db = temp_db();
        let n = virtual_folders::add_node(&db, 0, Path::new("/definitely/not/exist/xyz"), "x").unwrap();
        assert!(virtual_folders::is_link_broken(&n));
        assert_eq!(delete_virtual_node(&db, &target_of(&n)), Ok(1));
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    #[test]
    fn delete_already_gone_finishes_quietly() {
        let db = temp_db();
        let n = virtual_folders::add_node(&db, 0, Path::new("/g"), "g").unwrap();
        let t = target_of(&n);
        assert_eq!(delete_virtual_node(&db, &t), Ok(1));
        // 二重削除はエラーにせず0件で終える
        assert_eq!(delete_virtual_node(&db, &t), Ok(0));
    }

    #[test]
    fn delete_rejects_reused_id_without_touching_db() {
        let db = temp_db();
        let old = virtual_folders::add_node(&db, 0, Path::new("/old"), "old").unwrap();
        let stale = target_of(&old);
        virtual_folders::remove_node(&db, old.id).unwrap();
        // 最大idの削除後の追加で、同じidが別のノードに再利用される
        let reused = virtual_folders::add_node(&db, 0, Path::new("/new"), "new").unwrap();
        assert_eq!(reused.id, old.id);
        assert_eq!(delete_virtual_node(&db, &stale), Err(DeleteError::Changed));
        assert_eq!(virtual_folders::list_nodes(&db).len(), 1);
    }
}
