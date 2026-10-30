//! 仮想フォルダへの実フォルダ登録（評価 → 非同期スキャン → DB登録）。
//!
//! 入口は `begin_register(src, dest)` の1つ。右クリック→実フォルダピッカー、実ツリー右クリック→
//! 行き先ピッカー、将来のD&D登録が同じ確認ダイアログ → 評価 → 登録の流れに合流する。
//! 評価・スキャンはUIを止めないよう別スレッドで行い、完了は毎フレーム `poll_virtual_register` で拾う。

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::i18n;
use crate::virtual_folder_scan::{scan_subtree_limited, ScanResult, IMPORT_CONFIRM_THRESHOLD, SCAN_HARD_CAP};
use crate::virtual_folders::{self, SubtreeSpec, VirtualFolderError, VirtualNode, MAX_NODES};

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
    probe_and_scan_limited(root, hard_cap, None)
}

/// `probe_and_scan` の深さ制限版（`max_depth` は `scan_subtree_limited` と同じ）。
pub(super) fn probe_and_scan_limited(root: &Path, hard_cap: usize, max_depth: Option<usize>) -> ScanOutcome {
    match std::fs::metadata(root) {
        Err(_) => ScanOutcome::NotFound,
        Ok(m) if !m.is_dir() => ScanOutcome::NotDir,
        Ok(_) => {
            if std::fs::read_dir(root).is_err() {
                ScanOutcome::Unreadable
            } else {
                ScanOutcome::Scanned(scan_subtree_limited(root, hard_cap, max_depth))
            }
        }
    }
}

/// スキャン結果をDBに登録する。戻り値は登録したノード数。
/// 打ち切り（`capped`）は途中までのスナップショットを黙って登録しないよう中止する。
pub(super) fn register_outcome(
    outcome: ScanOutcome,
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    dest: u32,
    kind: RegisterKind,
) -> Result<usize, RegisterError> {
    match outcome {
        ScanOutcome::NotFound => Err(RegisterError::Unreachable),
        ScanOutcome::NotDir => Err(RegisterError::NotDir),
        ScanOutcome::Unreadable => Err(RegisterError::Unreadable),
        ScanOutcome::Scanned(scan) => {
            // 打ち切りは確認なしに登録しない（通常は呼び出し前に大量登録ダイアログへ回す）
            if scan.capped {
                return Err(RegisterError::TooLarge);
            }
            commit_kind(db, kind, dest, &scan.spec)
        }
    }
}

fn commit_spec(
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    dest: u32,
    spec: &SubtreeSpec,
) -> Result<usize, RegisterError> {
    virtual_folders::add_subtree(db, dest, spec)
        .map(|nodes| nodes.len())
        .map_err(error_from_add)
}

/// 既存ノード `node_id` の配下へ、スナップショットのうち未登録の分だけを足す。戻り値は追加数。
fn commit_merge(
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    node_id: u32,
    spec: &SubtreeSpec,
) -> Result<usize, RegisterError> {
    virtual_folders::merge_snapshot(db, node_id, spec).map_err(|e| match e {
        VirtualFolderError::NotFound => RegisterError::DestMissing,
        other => error_from_add(other),
    })
}

/// 登録の種類。`dest` の意味が変わる。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegisterKind {
    /// 実フォルダを `dest` の子として新規登録する
    New,
    /// `dest` 自身の実フォルダのサブフォルダのうち、未登録の分を `dest` の配下へ足す
    Merge,
}

fn commit_kind(
    db: &std::sync::Arc<std::sync::Mutex<redb::Database>>,
    kind: RegisterKind,
    dest: u32,
    spec: &SubtreeSpec,
) -> Result<usize, RegisterError> {
    match kind {
        RegisterKind::New => commit_spec(db, dest, spec),
        RegisterKind::Merge => commit_merge(db, dest, spec),
    }
}

/// 大量取り込み（`ScanResult::needs_confirmation`）でユーザーが選ぶ取り込み方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImportChoice {
    All,
    Shallow,
}

/// 「浅く」で取り込む範囲。根を深さ0として、`depth` 階層目までの `count` 件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ShallowPlan {
    pub depth: usize,
    pub count: usize,
}

/// 大量取り込みダイアログに出す選択肢の評価結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ImportPlan {
    /// スキャンできたノード総数（`capped` のときは完全に走査できた深さまで）
    pub total: usize,
    /// 走査を打ち切った（`total` は全体ではない）
    pub capped: bool,
    /// 「全部」を選べるか。打ち切りのときと、残り容量を超えるときは選べない。
    pub all_allowed: bool,
    /// 「浅く」の範囲。根だけでも予算に収まらなければ None（選べない）。
    pub shallow: Option<ShallowPlan>,
}

impl ImportPlan {
    /// 全体を走査できたうえで、残り容量を超えている（ダイアログで赤字の警告を出す）。
    pub fn over_limit(&self, remaining: usize) -> bool {
        !self.capped && self.total > remaining
    }
}

/// 「浅く」の予算は、確認閾値と残り容量の小さいほう。
pub(super) fn plan_large_import(scan: &ScanResult, remaining: usize) -> ImportPlan {
    plan_from_counts(&scan.spec.depth_counts(), scan.capped, remaining)
}

/// 深さ別の「登録される件数」から選択肢を評価する。追加0件の深さ（merge の根）は「浅く」に選ばせない。
pub(super) fn plan_from_counts(counts: &[usize], capped: bool, remaining: usize) -> ImportPlan {
    let total: usize = counts.iter().sum();
    let budget = IMPORT_CONFIRM_THRESHOLD.min(remaining);
    let shallow = virtual_folders::deepest_depth_within_counts(counts, budget)
        .map(|depth| ShallowPlan { depth, count: counts.iter().take(depth + 1).sum::<usize>() })
        .filter(|s| s.count > 0);
    ImportPlan { total, capped, all_allowed: !capped && total <= remaining, shallow }
}

/// 選択に対応する登録用スナップショット。選べない選択肢なら None。
pub(super) fn spec_for_choice(scan: &ScanResult, plan: &ImportPlan, choice: ImportChoice) -> Option<SubtreeSpec> {
    match choice {
        ImportChoice::All => plan.all_allowed.then(|| scan.spec.clone()),
        ImportChoice::Shallow => plan.shallow.map(|s| scan.spec.truncated_to_depth(s.depth)),
    }
}

/// 「実サブフォルダを一括登録」の確認ダイアログの状態。
pub(super) struct SubsConfirm {
    pub id: u32,
    /// サブフォルダの子（孫以降）まで見るか
    pub deep: bool,
}

/// 評価・スキャン中の登録。1件ずつしか走らせない。
pub(super) struct PendingRegister {
    dest: u32,
    kind: RegisterKind,
    rx: mpsc::Receiver<ScanOutcome>,
}

/// 取り込み方の選択待ちの大量登録（1件ずつ。`register_pending` と同時には存在しない）。
pub(super) struct LargeImport {
    dest: u32,
    kind: RegisterKind,
    scan: ScanResult,
    plan: ImportPlan,
    /// ダイアログを開いた時点の残り容量（赤字の警告表示用）
    remaining: usize,
}

impl NekoviewApp {
    /// 登録の入口。既存ノードとの重複関係を調べて確認ダイアログを開く（OKで `start_virtual_register`）。
    pub(super) fn begin_register(&mut self, src: PathBuf, dest: u32) {
        if self.virtual_state.register_pending.is_some() || self.virtual_state.large_import.is_some() {
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
        if self.virtual_state.register_pending.is_some() || self.virtual_state.large_import.is_some() {
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
        self.virtual_state.register_pending = Some(PendingRegister { dest: c.dest, kind: RegisterKind::New, rx });
        self.set_toast(i18n::t().virtual_register_progress());
    }

    /// 仮想ノード `node_id` の実フォルダのサブフォルダを列挙し、未登録の分を `node_id` の子として登録する。
    /// `deep` が false なら直下のサブフォルダだけ、true ならその子孫まで。評価・スキャンは別スレッド。
    pub(super) fn start_merge_subfolders(&mut self, node_id: u32, deep: bool) {
        if self.virtual_state.register_pending.is_some() || self.virtual_state.large_import.is_some() {
            return;
        }
        let Some(real) = self.virtual_state.nodes.iter().find(|n| n.id == node_id).map(|n| n.real.clone()) else {
            self.set_register_failed(&RegisterError::DestMissing);
            return;
        };
        if node_id == ROOT {
            self.set_register_failed(&RegisterError::DestMissing);
            return;
        }
        if !self.path_reachable(&real) {
            self.set_register_failed(&RegisterError::Unreachable);
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let max_depth = if deep { None } else { Some(1) };
        std::thread::spawn(move || {
            let _ = tx.send(probe_and_scan_limited(&real, SCAN_HARD_CAP, max_depth));
            ctx.request_repaint();
        });
        self.virtual_state.register_pending = Some(PendingRegister { dest: node_id, kind: RegisterKind::Merge, rx });
        self.set_toast(i18n::t().virtual_register_progress());
    }

    /// 毎フレーム呼ぶ。登録中は「登録中…」を出し続け、完了したらDBに登録してトーストを出す。
    pub(super) fn poll_virtual_register(&mut self) {
        let Some(pending) = &self.virtual_state.register_pending else { return };
        let (dest, kind) = (pending.dest, pending.kind);
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
        // 大量取り込み（1000件超・打ち切り）は、登録せずに取り込み方の選択を待つ
        if let ScanOutcome::Scanned(scan) = &outcome {
            let remaining = virtual_folders::remaining_capacity(&db);
            let (added, plan) = match kind {
                RegisterKind::New => (scan.spec.node_count(), plan_large_import(scan, remaining)),
                RegisterKind::Merge => {
                    let counts =
                        virtual_folders::merge_depth_counts(&virtual_folders::list_nodes(&db), dest, &scan.spec);
                    (counts.iter().sum(), plan_from_counts(&counts, scan.capped, remaining))
                }
            };
            if scan.capped || added > IMPORT_CONFIRM_THRESHOLD {
                self.virtual_state.large_import =
                    Some(LargeImport { dest, kind, scan: scan.clone(), plan, remaining });
                return;
            }
        }
        let result = register_outcome(outcome, &db, dest, kind);
        self.finish_register(dest, kind, result);
    }

    fn finish_register(&mut self, dest: u32, kind: RegisterKind, result: Result<usize, RegisterError>) {
        match result {
            Ok(added) => {
                self.refresh_virtual_nodes();
                self.virtual_state.expanded.insert(dest);
                self.set_toast(match kind {
                    RegisterKind::New => i18n::t().virtual_register_ok().to_string(),
                    RegisterKind::Merge if added == 0 => i18n::t().virtual_register_subs_none().to_string(),
                    RegisterKind::Merge => i18n::t().virtual_register_subs_ok(added),
                });
            }
            Err(e) => self.set_register_failed(&e),
        }
    }

    /// 「実サブフォルダを一括登録」の確認ダイアログ。OKで `start_merge_subfolders`。
    pub(super) fn draw_virtual_register_subs(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.virtual_state.subs_confirm else { return };
        let (id, mut deep) = (s.id, s.deep);
        let Some(real) = self.virtual_state.nodes.iter().find(|n| n.id == id).map(|n| n.real.clone()) else {
            // 開いている間にノードが消えた
            self.virtual_state.subs_confirm = None;
            return;
        };
        let virtual_path = self.virtual_state.virtual_path(id);
        let t = i18n::t();
        let outcome = draw_confirm_dialog(
            ctx,
            ConfirmDialogSpec {
                id_salt: "virtual_register_subs_window",
                title: t.virtual_subs_title(),
                ok_label: t.virtual_ok(),
                cancel_label: t.favorite_dialog_cancel(),
                foreground: true,
            },
            |ui| {
                ui.label(t.virtual_subs_body());
                ui.add_space(6.0);
                ui.label(t.virtual_subs_target(&virtual_path));
                ui.label(t.virtual_subs_real(&real.display().to_string()));
                ui.add_space(6.0);
                ui.checkbox(&mut deep, t.virtual_subs_deep());
                ui.label(t.virtual_subs_note());
            },
        );
        match outcome {
            ConfirmOutcome::Cancel => self.virtual_state.subs_confirm = None,
            ConfirmOutcome::Ok => {
                self.virtual_state.subs_confirm = None;
                self.start_merge_subfolders(id, deep);
            }
            ConfirmOutcome::None => {
                if let Some(s) = &mut self.virtual_state.subs_confirm {
                    s.deep = deep;
                }
            }
        }
    }

    /// 大量取り込みの固定ダイアログ。全部／浅く／キャンセル。選べない選択肢は無効にする。
    pub(super) fn draw_virtual_large_import(&mut self, ctx: &egui::Context) {
        let Some(li) = &self.virtual_state.large_import else { return };
        let plan = li.plan.clone();
        let remaining = li.remaining;
        let t = i18n::t();
        let mut choice = None;
        let mut cancel = false;
        egui::Window::new(t.virtual_large_title())
            .id(egui::Id::new("virtual_large_import_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(if plan.capped {
                    t.virtual_large_body_capped(SCAN_HARD_CAP)
                } else {
                    t.virtual_large_body(plan.total)
                });
                if plan.over_limit(remaining) || plan.shallow.is_none() {
                    ui.colored_label(ui.visuals().error_fg_color, t.virtual_large_over_limit(remaining));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let all = t.virtual_large_all(plan.total, plan.capped);
                    if ui.add_enabled(plan.all_allowed, egui::Button::new(all)).clicked() {
                        choice = Some(ImportChoice::All);
                    }
                    let shallow = plan.shallow.map_or_else(
                        || t.virtual_large_shallow(0, 0),
                        |s| t.virtual_large_shallow(s.depth, s.count),
                    );
                    if ui.add_enabled(plan.shallow.is_some(), egui::Button::new(shallow)).clicked() {
                        choice = Some(ImportChoice::Shallow);
                    }
                    cancel = ui.button(t.favorite_dialog_cancel()).clicked();
                });
            });
        if cancel {
            self.virtual_state.large_import = None;
        } else if let Some(choice) = choice {
            let Some(li) = self.virtual_state.large_import.take() else { return };
            let Some(spec) = spec_for_choice(&li.scan, &li.plan, choice) else { return };
            let result = match self.spread_db.clone() {
                Some(db) => commit_kind(&db, li.kind, li.dest, &spec),
                None => Err(RegisterError::Db),
            };
            self.finish_register(li.dest, li.kind, result);
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
        let n = register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap();
        assert_eq!(n, 4);
        assert_eq!(virtual_folders::list_nodes(&db).len(), 4);
    }

    #[test]
    fn register_same_folder_twice_under_same_dest_is_duplicate() {
        let t = TempTree::new(&["a"]);
        let db = temp_db();
        register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap();
        let err = register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap_err();
        assert_eq!(err, RegisterError::Duplicate);
        // 失敗しても既存ノードはそのまま（追加も削除もされない）
        assert_eq!(virtual_folders::list_nodes(&db).len(), 2);
    }

    #[test]
    fn register_same_folder_under_different_dest_is_allowed() {
        let t = TempTree::new(&[]);
        let db = temp_db();
        let root_child = virtual_folders::add_node(&db, virtual_folders::ROOT_ID, Path::new("/x"), "x").unwrap();
        register_outcome(scanned(&t, 100), &db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap();
        register_outcome(scanned(&t, 100), &db, root_child.id, RegisterKind::New).unwrap();
        assert_eq!(virtual_folders::list_nodes(&db).len(), 3);
    }

    #[test]
    fn register_capped_scan_is_rejected_without_adding() {
        let t = TempTree::new(&["a", "b", "c", "d"]);
        let db = temp_db();
        let err = register_outcome(scanned(&t, 3), &db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap_err();
        assert_eq!(err, RegisterError::TooLarge);
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    #[test]
    fn register_under_missing_dest_is_rejected() {
        let t = TempTree::new(&[]);
        let db = temp_db();
        let err = register_outcome(scanned(&t, 100), &db, 999, RegisterKind::New).unwrap_err();
        assert_eq!(err, RegisterError::DestMissing);
        assert!(virtual_folders::list_nodes(&db).is_empty());
    }

    #[test]
    fn register_probe_failures_map_to_reasons_without_touching_db() {
        let db = temp_db();
        assert_eq!(register_outcome(ScanOutcome::NotFound, &db, 0, RegisterKind::New).unwrap_err(), RegisterError::Unreachable);
        assert_eq!(register_outcome(ScanOutcome::NotDir, &db, 0, RegisterKind::New).unwrap_err(), RegisterError::NotDir);
        assert_eq!(register_outcome(ScanOutcome::Unreadable, &db, 0, RegisterKind::New).unwrap_err(), RegisterError::Unreadable);
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

    /// 根 + 子`kids`件、各子の下に孫`grand`件（深さ別 [1, kids, kids*grand]）
    fn three_level(kids: usize, grand: usize) -> SubtreeSpec {
        let children = (0..kids)
            .map(|k| SubtreeSpec {
                real_path: PathBuf::from(format!("/r/{k}")),
                name: k.to_string(),
                children: (0..grand).map(|g| SubtreeSpec::leaf(format!("/r/{k}/{g}"), g.to_string())).collect(),
            })
            .collect();
        SubtreeSpec { real_path: PathBuf::from("/r"), name: "r".into(), children }
    }

    #[test]
    fn plan_large_import_shallow_stops_at_threshold_depth() {
        let scan = ScanResult { spec: three_level(10, 200), capped: false };
        let plan = plan_large_import(&scan, MAX_NODES);
        assert_eq!(plan.total, 2011);
        assert!(plan.all_allowed);
        assert!(!plan.over_limit(MAX_NODES));
        // 深さ1まで（1+10=11件）。深さ2を足すと2011件で閾値1000を超える
        assert_eq!(plan.shallow, Some(ShallowPlan { depth: 1, count: 11 }));
    }

    #[test]
    fn plan_large_import_disallows_all_over_remaining_capacity() {
        let scan = ScanResult { spec: three_level(10, 200), capped: false };
        let plan = plan_large_import(&scan, 1500);
        assert!(!plan.all_allowed);
        assert!(plan.over_limit(1500));
        assert_eq!(plan.shallow, Some(ShallowPlan { depth: 1, count: 11 }));
    }

    #[test]
    fn plan_large_import_shallow_budget_is_capped_by_remaining() {
        let scan = ScanResult { spec: three_level(10, 200), capped: false };
        // 残り5件では根（1件）だけ入る
        assert_eq!(plan_large_import(&scan, 5).shallow, Some(ShallowPlan { depth: 0, count: 1 }));
        // 残り0件では何も入らない
        assert_eq!(plan_large_import(&scan, 0).shallow, None);
    }

    #[test]
    fn plan_large_import_capped_disallows_all_even_if_it_fits() {
        let scan = ScanResult { spec: three_level(10, 5), capped: true };
        let plan = plan_large_import(&scan, MAX_NODES);
        assert!(!plan.all_allowed);
        // 打ち切りは「上限超過」ではないので赤字の対象にしない
        assert!(!plan.over_limit(MAX_NODES));
        assert_eq!(plan.shallow, Some(ShallowPlan { depth: 2, count: 61 }));
    }

    #[test]
    fn spec_for_choice_returns_full_or_truncated_snapshot() {
        let scan = ScanResult { spec: three_level(10, 200), capped: false };
        let plan = plan_large_import(&scan, MAX_NODES);
        assert_eq!(spec_for_choice(&scan, &plan, ImportChoice::All).unwrap().node_count(), 2011);
        assert_eq!(spec_for_choice(&scan, &plan, ImportChoice::Shallow).unwrap().node_count(), 11);

        let limited = plan_large_import(&scan, 1500);
        assert!(spec_for_choice(&scan, &limited, ImportChoice::All).is_none());
        let none = plan_large_import(&scan, 0);
        assert!(spec_for_choice(&scan, &none, ImportChoice::Shallow).is_none());
    }

    #[test]
    fn commit_shallow_choice_registers_only_truncated_nodes() {
        let db = temp_db();
        let scan = ScanResult { spec: three_level(10, 200), capped: false };
        let plan = plan_large_import(&scan, virtual_folders::remaining_capacity(&db));
        let spec = spec_for_choice(&scan, &plan, ImportChoice::Shallow).unwrap();
        assert_eq!(commit_spec(&db, virtual_folders::ROOT_ID, &spec), Ok(11));
        assert_eq!(virtual_folders::list_nodes(&db).len(), 11);
    }

    /// 仮想ノードとして `t` を ROOT 直下に登録し、そのidを返す。
    fn register_root_of(t: &TempTree, db: &std::sync::Arc<std::sync::Mutex<redb::Database>>) -> u32 {
        register_outcome(scanned(t, 100), db, virtual_folders::ROOT_ID, RegisterKind::New).unwrap();
        virtual_folders::list_nodes(db).into_iter().find(|n| n.real_path == t.0).unwrap().id
    }

    fn merge_scan(t: &TempTree, max_depth: Option<usize>) -> ScanOutcome {
        probe_and_scan_limited(&t.0, 100, max_depth)
    }

    #[test]
    fn merge_adds_only_unregistered_subfolders() {
        let t = TempTree::new(&["a"]);
        let db = temp_db();
        let id = register_root_of(&t, &db);
        std::fs::create_dir_all(t.0.join("b")).unwrap();
        std::fs::create_dir_all(t.0.join("c")).unwrap();
        let n = register_outcome(merge_scan(&t, Some(1)), &db, id, RegisterKind::Merge).unwrap();
        assert_eq!(n, 2);
        // 根 + a + b + c。aは重複登録されない
        assert_eq!(virtual_folders::list_nodes(&db).len(), 4);
        // もう一度やっても追加0件
        assert_eq!(register_outcome(merge_scan(&t, Some(1)), &db, id, RegisterKind::Merge), Ok(0));
    }

    #[test]
    fn merge_shallow_skips_grandchildren_and_deep_includes_them() {
        let t = TempTree::new(&["a"]);
        let db = temp_db();
        let id = register_root_of(&t, &db);
        std::fs::create_dir_all(t.0.join("a/x")).unwrap();
        std::fs::create_dir_all(t.0.join("b/y")).unwrap();
        // 直下のみ: bは入るが、aの下のxとbの下のyは入らない
        assert_eq!(register_outcome(merge_scan(&t, Some(1)), &db, id, RegisterKind::Merge), Ok(1));
        // 子孫まで: 登録済みのaの下のxと、bの下のyが入る
        assert_eq!(register_outcome(merge_scan(&t, None), &db, id, RegisterKind::Merge), Ok(2));
        assert_eq!(virtual_folders::list_nodes(&db).len(), 5);
    }

    #[test]
    fn merge_depth_counts_ignore_registered_and_count_by_depth() {
        let t = TempTree::new(&["a"]);
        let db = temp_db();
        let id = register_root_of(&t, &db);
        std::fs::create_dir_all(t.0.join("a/x/z")).unwrap();
        std::fs::create_dir_all(t.0.join("b")).unwrap();
        let ScanOutcome::Scanned(scan) = merge_scan(&t, None) else { panic!("scanned expected") };
        let counts = virtual_folders::merge_depth_counts(&virtual_folders::list_nodes(&db), id, &scan.spec);
        // 深さ1: b（aは登録済み）、深さ2: a/x、深さ3: a/x/z
        assert_eq!(counts, vec![0, 1, 1, 1]);
    }

    #[test]
    fn merge_into_missing_node_is_dest_missing() {
        let t = TempTree::new(&[]);
        let db = temp_db();
        let err = register_outcome(merge_scan(&t, Some(1)), &db, 999, RegisterKind::Merge).unwrap_err();
        assert_eq!(err, RegisterError::DestMissing);
    }

    #[test]
    fn plan_from_counts_merge_never_offers_zero_addition_shallow() {
        // 直下に1500件: 閾値1000に収まる深さは根(0件)だけ。0件の「浅く」は選べない
        let plan = plan_from_counts(&[0, 1500], false, MAX_NODES);
        assert_eq!(plan.total, 1500);
        assert!(plan.all_allowed);
        assert_eq!(plan.shallow, None);
        // 直下10件＋孫2000件: 深さ1（10件）まで
        let plan = plan_from_counts(&[0, 10, 2000], false, MAX_NODES);
        assert_eq!(plan.shallow, Some(ShallowPlan { depth: 1, count: 10 }));
    }
}
