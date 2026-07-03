use std::path::PathBuf;
use crate::spread_offset::SpreadOffset;

// ── 共有型定義 ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum PageMode {
    Single,
    SpreadLeft,
    SpreadRight,
}

/// reader（ZIP内）ページのソートキー
#[derive(Clone, Copy, PartialEq)]
pub enum ReaderSortKey {
    Name,
    Natural,
    Date,
}

/// explorer（ディレクトリ内ファイル群）のソートキー
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExplorerSortKey {
    Name,
    Date,
    Size,
}

/// ソート済みエントリ。original_index はキャッシュキーに使い、ソートで変化しない。
#[derive(Clone)]
pub struct ViewerEntry {
    pub entry_name: String,
    pub display_name: String,
    pub date_key: u64,
    pub original_index: usize,
}

// ── モデル構造体 ────────────────────────────────────────────────────────────

/// ZIP ファイル1件分の純粋なドメイン状態（テクスチャ・アニメ等は含まない）
pub struct ArchiveModel {
    pub archive_path: PathBuf,
    pub entries: Vec<ViewerEntry>,
    pub spread_base: i32,
    pub offset: SpreadOffset,
    pub page_mode: PageMode,
    pub sort_key: ReaderSortKey,
    pub sort_ascending: bool,
    pub is_raw_file: bool,
}

/// ディレクトリブラウザの純粋なドメイン状態
pub struct DirModel {
    pub current_dir: PathBuf,
    /// sort 済みファイルリスト（archives + raw images）
    pub file_list: Vec<PathBuf>,
    /// 現在選択中のインデックス。左右キーはここを ±1 する
    pub list_cursor: Option<usize>,
    pub sort_key: ExplorerSortKey,
    pub sort_ascending: bool,
}

/// アプリ全体の単一モデル（唯一の真実の源）
pub struct AppModel {
    pub dir: DirModel,
    /// 現在開いているファイルのモデル。None = reader 未表示
    pub reader: Option<ArchiveModel>,
}
