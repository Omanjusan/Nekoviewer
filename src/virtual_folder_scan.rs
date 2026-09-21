// フェーズ3（仮想ビュー）以降で接続するまでの暫定。接続後にこの行を外すこと。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::fs::dir::list_subdirs;
use crate::virtual_folders::{SubtreeSpec, MAX_NODES};

/// スキャン自体を打ち切るノード数。ドライブ直下などの誤登録で走査が暴走しないための上限。
pub const SCAN_HARD_CAP: usize = MAX_NODES * 4;
/// 1回の登録で取り込むノード数がこれを超えたらユーザーに確認する。
pub const IMPORT_CONFIRM_THRESHOLD: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    pub spec: SubtreeSpec,
    /// `SCAN_HARD_CAP` で走査を打ち切った。true のとき `spec` は「完全に走査できた深さまで」
    /// の部分木で、それより深い階層は含まない（途中まで走査した階層は捨てている）。
    pub capped: bool,
}

impl ScanResult {
    /// 取り込み前にユーザー確認（全部／浅く／キャンセル）が要るか。
    pub fn needs_confirmation(&self) -> bool {
        self.capped || self.spec.node_count() > IMPORT_CONFIRM_THRESHOLD
    }
}

struct Entry {
    path: PathBuf,
    name: String,
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// `root` 配下の実サブフォルダ構造を幅優先で走査してスナップショットにする。
/// - シンボリックリンクのフォルダは `list_subdirs` が除外するためループしない。
/// - 読めないフォルダは子なしとして扱う（panic しない）。
/// - 各階層の子は `list_subdirs` のパス順。
pub fn scan_subtree(root: &Path, hard_cap: usize) -> ScanResult {
    let mut arena = vec![Entry { path: root.to_path_buf(), name: display_name(root) }];
    let mut parents: Vec<Option<usize>> = vec![None];
    let mut level: Vec<usize> = vec![0];
    let mut capped = false;

    'levels: while !level.is_empty() {
        let mut pending: Vec<(usize, PathBuf)> = Vec::new();
        for &idx in &level {
            for child in list_subdirs(&arena[idx].path) {
                if arena.len() + pending.len() >= hard_cap {
                    capped = true;
                    break 'levels;
                }
                pending.push((idx, child));
            }
        }
        level = pending
            .into_iter()
            .map(|(parent, path)| {
                arena.push(Entry { name: display_name(&path), path });
                parents.push(Some(parent));
                arena.len() - 1
            })
            .collect();
    }

    ScanResult { spec: build_spec(arena, &parents), capped }
}

/// 幅優先で積んだ arena（子の添字は必ず親より大きい）を、再帰なしで部分木に組み上げる。
fn build_spec(arena: Vec<Entry>, parents: &[Option<usize>]) -> SubtreeSpec {
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); arena.len()];
    for (i, parent) in parents.iter().enumerate() {
        if let Some(p) = parent {
            kids[*p].push(i);
        }
    }
    let mut specs: Vec<Option<SubtreeSpec>> = (0..arena.len()).map(|_| None).collect();
    for (i, entry) in arena.into_iter().enumerate().rev() {
        let children = kids[i]
            .iter()
            .map(|&k| specs[k].take().expect("子は親より先に組み上がっている"))
            .collect();
        specs[i] = Some(SubtreeSpec { real_path: entry.path, name: entry.name, children });
    }
    specs[0].take().expect("根は必ず存在する")
}

/// `scan_subtree` をバックグラウンドで実行する。ユーザーが別操作に移った場合は受信側を
/// 破棄すればよい（他のスキャンと同じく結果を捨てる方式）。
/// `wake` は結果送信後に1回呼ばれる（UI を起こすため。fs層と同様に egui 非依存）。
pub fn spawn_scan_subtree(
    root: PathBuf,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<ScanResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(scan_subtree(&root, SCAN_HARD_CAP));
        wake();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// テスト専用の一時ディレクトリ。Drop で削除する。
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(dirs: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nekoviewer_vf_scan_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            for d in dirs {
                std::fs::create_dir_all(root.join(d)).unwrap();
            }
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn names(spec: &SubtreeSpec) -> Vec<&str> {
        spec.children.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn scan_builds_nested_snapshot_in_path_order() {
        let t = TempTree::new(&["b/c/d", "a", "b/e"]);
        let r = scan_subtree(&t.0, SCAN_HARD_CAP);
        assert!(!r.capped);
        assert_eq!(r.spec.real_path, t.0);
        assert_eq!(names(&r.spec), ["a", "b"]);
        let b = &r.spec.children[1];
        assert_eq!(names(b), ["c", "e"]);
        assert_eq!(names(&b.children[0]), ["d"]);
        assert_eq!(r.spec.node_count(), 6);
        assert_eq!(b.children[0].children[0].real_path, t.0.join("b/c/d"));
    }

    #[test]
    fn scan_ignores_files_and_handles_leaf_or_missing_root() {
        let t = TempTree::new(&["a"]);
        std::fs::write(t.0.join("file.txt"), b"x").unwrap();
        std::fs::write(t.0.join("a/img.png"), b"x").unwrap();
        let r = scan_subtree(&t.0, SCAN_HARD_CAP);
        assert_eq!(r.spec.node_count(), 2);
        // 存在しない根は子なしの1ノードになる（呼び出し側で is_dir を確認する）
        let missing = scan_subtree(&t.0.join("nope"), SCAN_HARD_CAP);
        assert_eq!(missing.spec.node_count(), 1);
        assert!(!missing.capped);
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_directory_symlinks() {
        let t = TempTree::new(&["a"]);
        std::os::unix::fs::symlink(&t.0, t.0.join("a/loop")).unwrap();
        let r = scan_subtree(&t.0, SCAN_HARD_CAP);
        assert!(!r.capped);
        assert_eq!(r.spec.node_count(), 2);
    }

    #[test]
    fn scan_hard_cap_keeps_only_fully_scanned_levels() {
        // 深さ1: a,b,c / 深さ2: a配下2つ + b配下2つ（合計 1+3+4=8）
        let t = TempTree::new(&["a/x", "a/y", "b/x", "b/y", "c"]);
        let full = scan_subtree(&t.0, SCAN_HARD_CAP);
        assert_eq!(full.spec.node_count(), 8);
        // 上限=8ちょうどなら打ち切りなし
        assert!(!scan_subtree(&t.0, 8).capped);
        // 上限=6だと深さ2の途中で超過 → 深さ2は捨てて深さ1(4ノード)まで
        let capped = scan_subtree(&t.0, 6);
        assert!(capped.capped);
        assert_eq!(capped.spec.node_count(), 4);
        assert!(capped.spec.children.iter().all(|c| c.children.is_empty()));
        // 上限=2だと深さ1の途中で超過 → 根のみ
        let root_only = scan_subtree(&t.0, 2);
        assert!(root_only.capped);
        assert_eq!(root_only.spec.node_count(), 1);
    }

    #[test]
    fn needs_confirmation_on_cap_or_large_import() {
        let small = ScanResult { spec: SubtreeSpec::leaf("/vt/a", "a"), capped: false };
        assert!(!small.needs_confirmation());
        assert!(ScanResult { capped: true, ..small.clone() }.needs_confirmation());
        let children = (0..IMPORT_CONFIRM_THRESHOLD).map(|i| SubtreeSpec::leaf(format!("/vt/a/{i}"), "x")).collect();
        let big = ScanResult {
            spec: SubtreeSpec { real_path: "/vt/a".into(), name: "a".into(), children },
            capped: false,
        };
        // 根+1000子 = 1001 > 閾値
        assert!(big.needs_confirmation());
        // 「浅く取込」の深さを閾値から決められる
        assert_eq!(big.spec.deepest_depth_within(IMPORT_CONFIRM_THRESHOLD), Some(0));
    }

    #[test]
    fn spawn_scan_delivers_result_and_wakes_once() {
        let t = TempTree::new(&["a/b"]);
        let (wake_tx, wake_rx) = mpsc::channel();
        let rx = spawn_scan_subtree(t.0.clone(), move || {
            let _ = wake_tx.send(());
        });
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(r.spec.node_count(), 3);
        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(wake_rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
