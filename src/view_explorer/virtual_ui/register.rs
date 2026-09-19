//! 仮想フォルダへの実フォルダ登録（評価 → 非同期スキャン → DB登録）。
//!
//! 入口は `begin_register(src, dest)` の1つ。右クリック→実フォルダピッカー、実ツリー右クリック→
//! 行き先ピッカー、将来のD&D登録が同じ確認ダイアログ → 評価 → 登録の流れに合流する。
//! 評価・スキャンはUIを止めないよう別スレッドで行い、完了は毎フレーム `poll_virtual_register` で拾う。

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::i18n;
use crate::virtual_folder_scan::{scan_subtree, ScanResult, SCAN_HARD_CAP};
use crate::virtual_folders::{self, VirtualFolderError, VirtualNode, MAX_NODES};

use super::*;

/// 登録前警告用。既存ノードとの重複関係の件数（拒否ではなく確認ダイアログの表示材料）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OverlapInfo {
    /// 同じ実パスの既存ノード数（全体）
    pub same: usize,
    /// そのうち登録先と同じ親の直下にあるもの（この場合 `add_subtree` が拒否する）
    pub same_here: bool,
    /// 候補の祖先フォルダを指す既存ノード数
    pub ancestors: usize,
    /// 候補の子孫フォルダを指す既存ノード数
    pub descendants: usize,
}

pub(super) fn summarize_overlaps(nodes: &[VirtualNode], candidate: &Path, dest: u32) -> OverlapInfo {
    let o = virtual_folders::find_overlaps(nodes, candidate);
    let same_here = o
        .same
        .iter()
        .any(|id| nodes.iter().any(|n| n.id == *id && n.parent_id == dest));
    OverlapInfo {
        same: o.same.len(),
        same_here,
        ancestors: o.ancestors.len(),
        descendants: o.descendants.len(),
    }
}

/// 登録が中止される理由。トーストの文言に対応する。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RegisterError {
    Unreachable,
    NotDir,
    Unreadable,
    TooLarge,
    Duplicate,
    Limit,
    DestMissing,
    NameInvalid,
    Db,
}

impl RegisterError {
    fn message(&self) -> String {
        let t = i18n::t();
        match self {
            Self::Unreachable => t.virtual_reason_unreachable().to_string(),
            Self::NotDir => t.virtual_reason_not_dir().to_string(),
            Self::Unreadable => t.virtual_reason_unreadable().to_string(),
            Self::TooLarge => t.virtual_reason_too_large().to_string(),
            Self::Duplicate => t.virtual_reason_duplicate().to_string(),
            Self::Limit => t.virtual_reason_limit(MAX_NODES),
            Self::DestMissing => t.virtual_reason_dest_missing().to_string(),
            Self::NameInvalid => t.virtual_reason_name_invalid().to_string(),
            Self::Db => t.virtual_reason_db().to_string(),
        }
    }
}

pub(super) fn error_from_add(e: VirtualFolderError) -> RegisterError {
    match e {
        VirtualFolderError::LimitReached => RegisterError::Limit,
        VirtualFolderError::DuplicateSibling => RegisterError::Duplicate,
        VirtualFolderError::ParentNotFound => RegisterError::DestMissing,
        VirtualFolderError::NameEmpty | VirtualFolderError::NameTooLong => RegisterError::NameInvalid,
        VirtualFolderError::NotFound | VirtualFolderError::CycleDetected | VirtualFolderError::Db => {
            RegisterError::Db
        }
    }
}

/// スレッド上での評価とスキャンの結果。
pub(super) enum ScanOutcome {
    Scanned(ScanResult),
    NotFound,
    NotDir,
    Unreadable,
}

/// `root` が存在するフォルダで読めることを確かめてから、配下の構造をスナップショットにする。
/// ネットワーク配下でも固まらないよう、UIスレッドでは呼ばない。
pub(super) fn probe_and_scan(root: &Path, hard_cap: usize) -> ScanOutcome {
    match std::fs::metadata(root) {
        Err(_) => ScanOutcome::NotFound,
        Ok(m) if !m.is_dir() => ScanOutcome::NotDir,
        Ok(_) => {
            if std::fs::read_dir(root).is_err() {
                ScanOutcome::Unreadable
            } else {
                ScanOutcome::Scanned(scan_subtree(root, hard_cap))
            }
        }
    }
}

/// スキャン結果をDBに登録する。戻り値は登録したノード数。
/// 打ち切り（`capped`）は途中までのスナップショットを黙って登録しないよう中止する
/// （1000件超の確認ダイアログは未実装のため）。
pub(super) fn register_outcome(
    outcome: ScanOutcome,
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    dest: u32,
) -> Result<usize, RegisterError> {
    match outcome {
        ScanOutcome::NotFound => Err(RegisterError::Unreachable),
        ScanOutcome::NotDir => Err(RegisterError::NotDir),
        ScanOutcome::Unreadable => Err(RegisterError::Unreadable),
        ScanOutcome::Scanned(scan) => {
            if scan.capped {
                return Err(RegisterError::TooLarge);
            }
            virtual_folders::add_subtree(db, dest, &scan.spec)
                .map(|nodes| nodes.len())
                .map_err(error_from_add)
        }
    }
}

/// 評価・スキャン中の登録。1件ずつしか走らせない。
pub(super) struct PendingRegister {
    dest: u32,
    rx: mpsc::Receiver<ScanOutcome>,
}

impl NekoviewApp {
    /// 登録の入口。既存ノードとの重複関係を調べて確認ダイアログを開く（OKで `start_virtual_register`）。
    pub(super) fn begin_register(&mut self, src: PathBuf, dest: u32) {
        if self.virtual_state.register_pending.is_some() {
            return;
        }
        let Some(db) = self.spread_db.clone() else {
            self.set_register_failed(&RegisterError::Db);
            return;
        };
        let nodes = virtual_folders::list_nodes(&db);
        let overlaps = summarize_overlaps(&nodes, &src, dest);
        self.virtual_state.confirm = Some(Confirm { src, dest, overlaps });
    }

    /// 確認ダイアログのOK。到達可否を先に確かめ、以降の評価とスキャンは別スレッドで行う。
    pub(super) fn start_virtual_register(&mut self, c: Confirm) {
        if self.virtual_state.register_pending.is_some() {
            return;
        }
        // ネットワークマウント配下は確認済みの到達可否だけを見る（同期I/Oなし）
        if !self.path_reachable(&c.src) {
            self.set_register_failed(&RegisterError::Unreachable);
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let src = c.src;
        std::thread::spawn(move || {
            let _ = tx.send(probe_and_scan(&src, SCAN_HARD_CAP));
            ctx.request_repaint();
        });
        self.virtual_state.register_pending = Some(PendingRegister { dest: c.dest, rx });
        self.set_toast(i18n::t().virtual_register_progress());
    }

    /// 毎フレーム呼ぶ。登録中は「登録中…」を出し続け、完了したらDBに登録してトーストを出す。
    pub(super) fn poll_virtual_register(&mut self) {
        let Some(pending) = &self.virtual_state.register_pending else { return };
        let dest = pending.dest;
        let outcome = match pending.rx.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => {
                // トーストは3秒で消えるため、待っている間は出し直す
                self.set_toast(i18n::t().virtual_register_progress());
                self.egui_ctx.request_repaint_after(std::time::Duration::from_millis(500));
                return;
            }
            // スレッドが結果を返さず終わった: 読み取れなかった扱いで中止する
            Err(mpsc::TryRecvError::Disconnected) => ScanOutcome::Unreadable,
        };
        self.virtual_state.register_pending = None;
        let Some(db) = self.spread_db.clone() else {
            self.set_register_failed(&RegisterError::Db);
            return;
        };
        match register_outcome(outcome, &db, dest) {
            Ok(_) => {
                self.refresh_virtual_nodes();
                self.virtual_state.expanded.insert(dest);
                self.set_toast(i18n::t().virtual_register_ok());
            }
            Err(e) => self.set_register_failed(&e),
        }
    }

    fn set_register_failed(&mut self, e: &RegisterError) {
        self.set_toast(i18n::t().virtual_register_failed(&e.message()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト専用の一時ディレクトリ。Drop で削除する。
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(dirs: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_vf_register_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            for d in dirs {
                std::fs::create_dir_all(root.join(d)).unwrap();
            }
            Self(root)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_db() -> std::sync::Arc<std::sync::Mutex<redb::Database>> {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "nekoviewer_vf_register_test_{}_{}.redb",
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

    fn scanned(t: &TempTree, cap: usize) -> ScanOutcome {
        probe_and_scan(&t.0, cap)
    }

    #[test]
    fn register_adds_snapshot_under_dest() {
        let t = TempTree::new(&["a/b", "c"]);
        let db = temp_db();
        let n = register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID).unwrap();
        assert_eq!(n, 4);
        assert_eq!(virtual_folders::list_nodes(&db).len(), 4);
    }

    #[test]
    fn register_same_folder_twice_under_same_dest_is_duplicate() {
        let t = TempTree::new(&["a"]);
        let db = temp_db();
        register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID).unwrap();
        let err = register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID).unwrap_err();
        assert_eq!(err, RegisterError::Duplicate);
        // 失敗しても既存ノードはそのまま（追加も削除もされない）
        assert_eq!(virtual_folders::list_nodes(&db).len(), 2);
    }

    #[test]
    fn register_same_folder_under_different_dest_is_allowed() {
        let t = TempTree::new(&[]);
        let db = temp_db();
        let root_child = virtual_folders::add_node(&db, virtual_folders::ROOT_ID, Path::new("/x"), "x").unwrap();
        register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID).unwrap();
        register_outcome(scanned(&t, 100), &db, root_child.id).unwrap();
        assert_eq!(virtual_folders::list_nodes(&db).len(), 3);
    }

    #[test]
    fn register_capped_scan_is_rejected_without_adding() {
        let t = TempTree::new(&["a", "b", "c", "d"]);
        let db = temp_db();
        let err = register_outcome(scanned(&t, 3), &db, virtual_folders::ROOT_ID).unwrap_err();
        assert_eq!(err, RegisterError::TooLarge);
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    #[test]
    fn register_under_missing_dest_is_rejected() {
        let t = TempTree::new(&[]);
        let db = temp_db();
        let err = register_outcome(scanned(&t, 100), &db, 999).unwrap_err();
        assert_eq!(err, RegisterError::DestMissing);
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    #[test]
    fn register_probe_failures_map_to_reasons_without_touching_db() {
        let db = temp_db();
        assert_eq!(register_outcome(ScanOutcome::NotFound, &db, 0).unwrap_err(), RegisterError::Unreachable);
        assert_eq!(register_outcome(ScanOutcome::NotDir, &db, 0).unwrap_err(), RegisterError::NotDir);
        assert_eq!(register_outcome(ScanOutcome::Unreadable, &db, 0).unwrap_err(), RegisterError::Unreadable);
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    fn vnode(id: u32, parent_id: u32, real: &str) -> VirtualNode {
        VirtualNode {
            id,
            parent_id,
            real_path: PathBuf::from(real),
            name: real.rsplit('/').next().unwrap_or("").to_string(),
            order: 0,
        }
    }

    #[test]
    fn probe_missing_path_is_not_found() {
        let t = TempTree::new(&[]);
        assert!(matches!(probe_and_scan(&t.0.join("nope"), 100), ScanOutcome::NotFound));
    }

    #[test]
    fn probe_file_is_not_dir() {
        let t = TempTree::new(&[]);
        let f = t.0.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(matches!(probe_and_scan(&f, 100), ScanOutcome::NotDir));
    }

    #[test]
    fn probe_dir_scans_subfolder_structure() {
        let t = TempTree::new(&["a/b", "c"]);
        match probe_and_scan(&t.0, 100) {
            ScanOutcome::Scanned(scan) => {
                assert!(!scan.capped);
                // ルート + a + a/b + c
                assert_eq!(scan.spec.node_count(), 4);
            }
            _ => panic!("scanned expected"),
        }
    }

    #[test]
    fn probe_over_hard_cap_reports_capped() {
        let t = TempTree::new(&["a", "b", "c", "d"]);
        match probe_and_scan(&t.0, 3) {
            ScanOutcome::Scanned(scan) => assert!(scan.capped),
            _ => panic!("scanned expected"),
        }
    }

    #[test]
    fn add_errors_map_to_register_errors() {
        assert_eq!(error_from_add(VirtualFolderError::LimitReached), RegisterError::Limit);
        assert_eq!(error_from_add(VirtualFolderError::DuplicateSibling), RegisterError::Duplicate);
        assert_eq!(error_from_add(VirtualFolderError::ParentNotFound), RegisterError::DestMissing);
        assert_eq!(error_from_add(VirtualFolderError::NameTooLong), RegisterError::NameInvalid);
        assert_eq!(error_from_add(VirtualFolderError::NameEmpty), RegisterError::NameInvalid);
        assert_eq!(error_from_add(VirtualFolderError::Db), RegisterError::Db);
    }

    #[test]
    fn overlaps_same_under_same_parent_is_flagged_here() {
        let nodes = vec![vnode(1, 0, "/m/manga"), vnode(2, 5, "/m/manga")];
        let o = summarize_overlaps(&nodes, Path::new("/m/manga"), 0);
        assert_eq!(o.same, 2);
        assert!(o.same_here);
        let o = summarize_overlaps(&nodes, Path::new("/m/manga"), 9);
        assert_eq!(o.same, 2);
        assert!(!o.same_here);
    }

    #[test]
    fn overlaps_ancestor_and_descendant_counts() {
        let nodes = vec![vnode(1, 0, "/m"), vnode(2, 0, "/m/manga/a"), vnode(3, 0, "/other")];
        let o = summarize_overlaps(&nodes, Path::new("/m/manga"), 0);
        assert_eq!(o.same, 0);
        assert_eq!(o.ancestors, 1);
        assert_eq!(o.descendants, 1);
        assert!(!o.same_here);
    }
}
