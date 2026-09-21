//! 仮想フォルダの右クリック「実ツリーと同期」。選んだ仮想ノードの実パスまで、実ツリーを展開して選択表示にする。
//!
//! 実ツリーは仮想タブの横ペインとFoldersタブで状態を共有しており、ペインが閉じていても状態は更新できる。
//! そのため展開は「見えないまま」行い、ペインを開いたときに対象の位置へスクロールされる。
//! 中央のカード欄（仮想ノードの表示）は変えない。同じ仕組みで「フォルダタブで開く」も担う
//! （こちらはタブを移り、カード欄もその実フォルダに切り替わる）。実ツリーが別ドライブのときだけ、ツリーのルートを
//! 対象のドライブへ切り替える（トーストで知らせる）。切り替えたら、実ツリータブ自身の位置の退避は捨てる
//! （実ペイン内の実操作は新しい実位置になる、という既存のドライブ切替と同じ扱い）。

use std::path::{Path, PathBuf};

use crate::fs::mount::MountEntry;
use crate::i18n;

use super::*;

/// 実ツリーのルートに対する、同期先パスの位置づけ。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RootPlan {
    /// 今のルート配下。ルートはそのまま
    InTree,
    /// 今のルート配下ではないが、`drives[i]` の配下。ルートをそのドライブへ切り替える
    SwitchDrive(usize),
    /// どのドライブにも属さない
    NoDrive,
}

/// `target` を含むドライブのうち、パスが最も長い（最も深いマウントの）もの。
pub(super) fn drive_index_for(drives: &[MountEntry], target: &Path) -> Option<usize> {
    drives
        .iter()
        .enumerate()
        .filter(|(_, d)| target.starts_with(&d.path))
        .max_by_key(|(_, d)| d.path.components().count())
        .map(|(i, _)| i)
}

pub(super) fn plan_root(tree_root: &Path, drives: &[MountEntry], target: &Path) -> RootPlan {
    if target.starts_with(tree_root) {
        return RootPlan::InTree;
    }
    match drive_index_for(drives, target) {
        Some(i) => RootPlan::SwitchDrive(i),
        None => RootPlan::NoDrive,
    }
}

/// `tree_root` から `target` までの経路に、`.` で始まるフォルダ名があるか（隠し表示OFFのツリーでは行が出ない）。
pub(super) fn hidden_on_path(tree_root: &Path, target: &Path) -> bool {
    let Ok(rel) = target.strip_prefix(tree_root) else { return false };
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => s.to_str().is_some_and(|s| s.starts_with('.')),
        _ => false,
    })
}

/// `ensure_tree_root_for` の結果。
enum RootOutcome {
    /// ルートはそのまま
    Kept,
    /// 別ドライブへ切り替えた（ドライブの表示名）
    Switched(String),
    /// どのドライブにも属さず、ルートはそのまま
    NoDrive,
}

impl NekoviewApp {
    /// 仮想ノード `id` の実パスまで実ツリーを展開して選択表示にする。失敗・注意はトーストで知らせる。
    /// 成功時（同一ドライブで隠しフォルダも経由しない）は何も出さない。
    pub(super) fn sync_real_tree(&mut self, id: u32) {
        let Some(real) = self.node_real_path(id) else { return };
        // ツリーの状態を変える前に到達可否を見る（届かないなら何も変えない）
        if !self.path_reachable(&real) {
            self.set_toast(i18n::t().virtual_folder_unreachable());
            return;
        }
        match self.ensure_tree_root_for(&real) {
            RootOutcome::NoDrive => self.set_toast(i18n::t().virtual_sync_no_drive()),
            RootOutcome::Kept => self.follow_in_tree(real, None),
            RootOutcome::Switched(label) => {
                self.persist_state();
                self.follow_in_tree(real, Some(label));
            }
        }
    }

    /// 仮想ノード `id` の実フォルダを、フォルダタブ（実ツリー）で選択済みにして開く。
    /// 別ドライブならツリーのルートも切り替える（タブを移るので通知はしない）。
    pub(super) fn open_in_folders_tab(&mut self, id: u32) {
        let Some(real) = self.node_real_path(id) else { return };
        if !self.path_reachable(&real) {
            self.set_toast(i18n::t().virtual_folder_unreachable());
            return;
        }
        let outcome = self.ensure_tree_root_for(&real);
        // 実位置の退避は捨てる。表示中のノードが対象なら、仮想表示を終える再スキャンがそのまま本番のスキャンになる
        self.real_dir_stash.clear();
        self.switch_folder_tab(FolderPaneTab::RealTree);
        self.focused_pane = FocusPane::TreeTab;
        if self.current_dir != real {
            // 対象が表示中のノードでなかった（通常は右クリックで選択済みなので起きない）ときだけ、あらためて開く
            self.navigate_to(real.clone(), DirectoryNavigationSource::ItemPane);
        }
        self.persist_state();
        match outcome {
            RootOutcome::NoDrive => self.set_toast(i18n::t().virtual_open_no_drive()),
            RootOutcome::Kept | RootOutcome::Switched(_) => self.follow_in_tree(real, None),
        }
    }

    fn node_real_path(&self, id: u32) -> Option<PathBuf> {
        self.virtual_state.nodes.iter().find(|n| n.id == id).map(|n| n.real.clone())
    }

    /// 実ツリーのルートを `real` を含むものにする。今のルート配下ならそのまま、別ドライブなら
    /// ツリーだけそのドライブへ切り替える（表示中のフォルダには触れない。実位置の退避は捨てる）。
    fn ensure_tree_root_for(&mut self, real: &Path) -> RootOutcome {
        match plan_root(&self.tree_root, &self.drives, real) {
            RootPlan::InTree => RootOutcome::Kept,
            RootPlan::NoDrive => RootOutcome::NoDrive,
            RootPlan::SwitchDrive(i) => {
                let drive = self.drives[i].clone();
                self.real_dir_stash.clear();
                self.reset_tree_root(drive.path);
                RootOutcome::Switched(drive.label)
            }
        }
    }

    /// 実ツリーを `real` まで展開して選択表示にし、見えない・見つからない場合はトーストで知らせる。
    /// `switch_notice` はドライブを切り替えたときの通知用（警告が出るときは警告を優先する）。
    fn follow_in_tree(&mut self, real: PathBuf, switch_notice: Option<String>) {
        let hidden = !self.show_hidden && hidden_on_path(&self.tree_root, &real);
        self.start_tree_autofocus(real);
        if hidden {
            // 展開自体は進むが行が描かれない。ドライブ切替の通知より、見えない理由を優先する
            self.set_toast(i18n::t().virtual_tree_hidden_on_path());
            return;
        }
        // 経路の途中で見つからず打ち切られたら、そのとき通知する（非同期に判明するため）
        self.tree_autofocus_notify_abort = self.tree_autofocus.is_some();
        if let Some(label) = switch_notice {
            self.set_toast(i18n::t().virtual_sync_drive_switched(&label));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn drive(path: &str) -> MountEntry {
        MountEntry { label: path.to_string(), path: PathBuf::from(path) }
    }

    #[test]
    fn drive_index_prefers_the_deepest_mount() {
        let drives = [drive("/"), drive("/mnt/data"), drive("/mnt")];
        assert_eq!(drive_index_for(&drives, Path::new("/mnt/data/pics")), Some(1));
        assert_eq!(drive_index_for(&drives, Path::new("/mnt/other")), Some(2));
        assert_eq!(drive_index_for(&drives, Path::new("/home/x")), Some(0));
    }

    #[test]
    fn drive_index_is_none_outside_every_drive() {
        let drives = [drive("/mnt/data")];
        assert_eq!(drive_index_for(&drives, Path::new("/home/x")), None);
        // 名前の前方一致だけでは属さない（コンポーネント単位で見る）
        assert_eq!(drive_index_for(&drives, Path::new("/mnt/database")), None);
    }

    #[test]
    fn plan_keeps_the_root_when_target_is_inside_it() {
        let drives = [drive("/"), drive("/mnt/data")];
        assert_eq!(plan_root(Path::new("/"), &drives, Path::new("/mnt/data/x")), RootPlan::InTree);
        assert_eq!(plan_root(Path::new("/mnt/data"), &drives, Path::new("/mnt/data")), RootPlan::InTree);
    }

    #[test]
    fn plan_switches_to_the_drive_holding_the_target() {
        let drives = [drive("/home"), drive("/mnt/data")];
        assert_eq!(plan_root(Path::new("/home"), &drives, Path::new("/mnt/data/x")), RootPlan::SwitchDrive(1));
    }

    #[test]
    fn plan_reports_no_drive_for_paths_outside_all_drives() {
        let drives = [drive("/home"), drive("/mnt/data")];
        assert_eq!(plan_root(Path::new("/home"), &drives, Path::new("/opt/x")), RootPlan::NoDrive);
    }

    #[test]
    fn hidden_on_path_checks_only_below_the_root() {
        let root = Path::new("/mnt/data");
        assert!(hidden_on_path(root, Path::new("/mnt/data/.cache/pics")));
        assert!(hidden_on_path(root, Path::new("/mnt/data/a/.b")));
        assert!(!hidden_on_path(root, Path::new("/mnt/data/a/b")));
        // ルート自体が隠し名でも、ルートより上は見ない
        assert!(!hidden_on_path(Path::new("/mnt/.data"), Path::new("/mnt/.data/a")));
        // 配下でなければ判定しない
        assert!(!hidden_on_path(root, Path::new("/other/.x")));
    }
}
