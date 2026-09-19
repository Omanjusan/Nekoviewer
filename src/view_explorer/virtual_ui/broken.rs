//! 仮想ノードのリンク切れ（実パスがフォルダとして開けない）の一括判定。
//!
//! 全ノードを別スレッドで確認して、ツリー行とフォルダカードの目印に使う。ネットワークマウント配下は
//! I/Oを行わず、確認済みの到達可否（`network_unreachable_mounts`）だけを見る（固まらないための既存方針）。
//! 同じ確認（`metadata`）で、実フォルダの更新日時も集める（フォルダカードの日付ソート用。ネットワーク配下は取らない）。
//! 判定は仮想タブに入るたび・登録や削除のあと・リロード時（`refresh_virtual_nodes`）に走り直す。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::SystemTime;

use super::*;

/// 判定中の一括チェック。世代番号が古い結果は捨てる。
pub(super) struct BrokenCheck {
    generation: u64,
    rx: mpsc::Receiver<(u64, HashSet<u32>, HashMap<u32, SystemTime>)>,
}

/// ノードを「すぐ判定できるネットワーク配下」と「スレッドで確認するローカル」に振り分ける。
/// 戻り値は（不通と分かっているマウント配下＝切れているノードのid、スレッドで確認する対象）。
/// 到達可否が未確認のネットワーク配下は、切れていない扱い（楽観）で確認対象からも外す。
pub(super) fn split_by_network(
    nodes: &[(u32, PathBuf)],
    mount_root: impl Fn(&Path) -> Option<PathBuf>,
    unreachable: &HashSet<PathBuf>,
) -> (HashSet<u32>, Vec<(u32, PathBuf)>) {
    let mut broken = HashSet::new();
    let mut probe = Vec::new();
    for (id, path) in nodes {
        match mount_root(path) {
            Some(root) => {
                if unreachable.contains(&root) {
                    broken.insert(*id);
                }
            }
            None => probe.push((*id, path.clone())),
        }
    }
    (broken, probe)
}

/// 対象の実パスがフォルダとして開けないノードのid（リンク切れ）と、開けたノードの更新日時を返す
/// （スレッド上で呼ぶ）。更新日時が取れないフォルダは、切れてはいないが日時の表には載らない。
pub(super) fn probe_local(targets: &[(u32, PathBuf)]) -> (HashSet<u32>, HashMap<u32, SystemTime>) {
    let mut broken = HashSet::new();
    let mut mtimes = HashMap::new();
    for (id, path) in targets {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => {
                if let Ok(t) = m.modified() {
                    mtimes.insert(*id, t);
                }
            }
            _ => {
                broken.insert(*id);
            }
        }
    }
    (broken, mtimes)
}

impl NekoviewApp {
    /// 現在の仮想ノードの全件を、リンク切れ判定し直す（前の判定が走っていれば捨てる）。
    pub(super) fn start_broken_check(&mut self) {
        self.virtual_state.broken_gen += 1;
        let generation = self.virtual_state.broken_gen;
        let nodes: Vec<(u32, PathBuf)> = self
            .virtual_state
            .nodes
            .iter()
            .map(|n| (n.id, n.real.clone()))
            .collect();
        if nodes.is_empty() {
            self.virtual_state.broken.clear();
            self.virtual_state.mtimes.clear();
            self.virtual_state.broken_check = None;
            return;
        }
        let (net_broken, local) = split_by_network(
            &nodes,
            |p| self.network_mount_root_cached(p),
            &self.network_unreachable_mounts,
        );
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let (mut broken, mtimes) = probe_local(&local);
            broken.extend(net_broken);
            let _ = tx.send((generation, broken, mtimes));
            ctx.request_repaint();
        });
        self.virtual_state.broken_check = Some(BrokenCheck { generation, rx });
    }

    /// 毎フレーム呼ぶ。判定が終わっていれば結果を取り込む。
    pub(super) fn poll_broken_check(&mut self) {
        let Some(check) = &self.virtual_state.broken_check else { return };
        match check.rx.try_recv() {
            Ok((generation, broken, mtimes)) => {
                if generation == check.generation && generation == self.virtual_state.broken_gen {
                    self.virtual_state.broken = broken;
                    self.virtual_state.mtimes = mtimes;
                }
                self.virtual_state.broken_check = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.virtual_state.broken_check = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_vf_broken_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(root.join("dir")).unwrap();
            std::fs::write(root.join("file.txt"), b"x").unwrap();
            Self(root)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn probe_local_marks_missing_and_non_directory_paths() {
        let t = TempTree::new();
        let targets = vec![
            (1, t.0.join("dir")),
            (2, t.0.join("missing")),
            (3, t.0.join("file.txt")),
        ];
        let (broken, mtimes) = probe_local(&targets);
        assert_eq!(broken, HashSet::from([2, 3]));
        // 更新日時が載るのは開けたフォルダだけ
        assert_eq!(mtimes.keys().copied().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn split_by_network_never_probes_network_paths() {
        let nodes = vec![
            (1, PathBuf::from("/net/dead/a")),
            (2, PathBuf::from("/net/alive/b")),
            (3, PathBuf::from("/local/c")),
        ];
        let mount_root = |p: &Path| {
            ["/net/dead", "/net/alive"].iter().map(PathBuf::from).find(|r| p.starts_with(r))
        };
        let unreachable = HashSet::from([PathBuf::from("/net/dead")]);
        let (broken, probe) = split_by_network(&nodes, mount_root, &unreachable);
        // 不通と分かっているマウント配下は即「切れ」、到達できるネットワーク配下は切れていない扱いで
        // 確認対象にもならない。I/Oで確認するのはローカルだけ。
        assert_eq!(broken, HashSet::from([1]));
        assert_eq!(probe, vec![(3, PathBuf::from("/local/c"))]);
    }
}
