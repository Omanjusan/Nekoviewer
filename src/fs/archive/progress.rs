//! アーカイブオープン時の進捗report型。
//! zip/7zは`archive.len()`/`archive.files.len()`で全件数が事前確定するため
//! `Determinate`で件数ベースの進捗を出せるが、tarは中央ディレクトリを持たない
//! 逐次ストリームで全件数が事前にわからない（かつ圧縮tarは二度読みが高コスト）ため
//! `Indeterminate`（「読み込み中」表示のみ）とする。

/// アーカイブオープン処理の進捗状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOpenProgress {
    /// 全件数が判明している（zip/7z）。`current`は処理済み件数（0-based、次に処理する件数-1）。
    Determinate { current: usize, total: usize },
    /// 全件数が不明（tar）。読み込み中であることのみを示す。
    Indeterminate,
}

/// 進捗コールバック。1エントリ処理するたびに呼ばれる。
/// 戻り値が`false`ならオープン処理を打ち切る（キャンセル要求）。
pub type ProgressCallback<'a> = dyn FnMut(ArchiveOpenProgress) -> bool + 'a;
