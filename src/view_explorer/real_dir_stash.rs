//! 実ツリータブ自身の位置（実ディレクトリ）の退避。
//!
//! 仮想タブでノードを選ぶと、既存のスキャン基盤を使うため `current_dir` がそのノードの実パスに変わる。
//! そのままだと `persist_state` が `last_dir` に仮想ノードの実パスを書き、次回の起動位置や、
//! 仮想タブから実ツリータブへ戻ったときの位置が、仮想側に引っ張られる。
//! そこで、仮想側へ移る直前の実位置をここに退避し、`last_dir` と復帰にはこちらを使う。

use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(super) struct RealDirStash(Option<PathBuf>);

impl RealDirStash {
    /// 仮想ノードの実パスへ `current_dir` を移す直前に呼ぶ。すでに退避済みなら何もしない
    /// （仮想ノード間の移動では最初の実位置を保つ）。退避が空のとき `current` は実位置。
    pub(super) fn stash_if_empty(&mut self, current: &Path) {
        if self.0.is_none() {
            self.0 = Some(current.to_path_buf());
        }
    }

    /// 実ディレクトリへ移動した（実位置が更新された）ので、退避は不要になる。
    pub(super) fn clear(&mut self) {
        self.0 = None;
    }

    /// 仮想表示を終えるとき、実位置に戻す先を取り出す。
    pub(super) fn take(&mut self) -> Option<PathBuf> {
        self.0.take()
    }

    /// 実ツリータブの位置。退避中はその位置、そうでなければ現在の `current_dir`。
    pub(super) fn effective<'a>(&'a self, current: &'a Path) -> &'a Path {
        self.0.as_deref().unwrap_or(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_is_current_dir_when_nothing_stashed() {
        let s = RealDirStash::default();
        assert_eq!(s.effective(Path::new("/real")), Path::new("/real"));
    }

    #[test]
    fn virtual_moves_keep_the_first_real_location() {
        let mut s = RealDirStash::default();
        // 実 /real → 仮想ノードX(/x) → 仮想ノードY(/y)
        s.stash_if_empty(Path::new("/real"));
        s.stash_if_empty(Path::new("/x"));
        assert_eq!(s.effective(Path::new("/y")), Path::new("/real"));
        // 仮想表示を終えると、最初の実位置に戻る
        assert_eq!(s.take(), Some(PathBuf::from("/real")));
        assert_eq!(s.effective(Path::new("/real")), Path::new("/real"));
    }

    #[test]
    fn real_navigation_discards_the_stash() {
        let mut s = RealDirStash::default();
        s.stash_if_empty(Path::new("/real"));
        // 仮想タブ内の実ツリーペインで別の実フォルダへ移動: それが新しい実位置
        s.clear();
        assert_eq!(s.effective(Path::new("/other")), Path::new("/other"));
        assert_eq!(s.take(), None);
    }

    #[test]
    fn take_without_stash_leaves_current_dir_alone() {
        let mut s = RealDirStash::default();
        assert_eq!(s.take(), None);
    }
}
