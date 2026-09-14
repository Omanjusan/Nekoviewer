use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::sync::atomic::AtomicU64;

use crate::cache::{FileCache, FileCacheEntry, LoadRequest, LoadResult, PageCache, ThumbRequest, ThumbResult, ThumbResultStage, EntryThumbRequest, EntryThumbResult, spawn_worker, spawn_thumb_worker, spawn_entry_thumb_worker, spawn_file_cache_worker};
use crate::decode_jobs::{DecodeJobQueue, DesiredDecodeJob};
use crate::config::AppConfig;
use crate::gui_config::{SortState, ViewerConfig, WindowSlot};
use crate::view_gui_config::{SettingsDraft, SettingsTab};
use crate::i18n;
use crate::types::ExplorerSortKey;
use crate::fs::{dir, mount::{list_gvfs_smb_mounts, list_local_drives, MountEntry}};
use crate::view_reader::ViewerState;

impl ExplorerSortKey {
    fn label(self) -> &'static str {
        let t = i18n::t();
        match self {
            Self::Name => t.sort_name(),
            Self::Date => t.sort_date(),
            Self::Size => t.sort_size(),
        }
    }

    fn as_state_key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Date => "date",
            Self::Size => "size",
        }
    }

    fn from_state_key(s: &str) -> Self {
        match s {
            "date" => Self::Date,
            "size" => Self::Size,
            _ => Self::Name,
        }
    }
}

enum TreeAction {
    None,
    ToggleExpand(PathBuf),
    Navigate(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FolderPaneTab {
    RealTree,
    Favorites,
    Search,
}

/// キーボード操作のフォーカス巡回順（Tab/Shift+Tabで一周する）。今どのタブ（folder_pane_tab）を
/// 選んでいるかで経路が変わる（本体の中身がタブごとに違うため）:
///   RealTree:  FolderTabBar → TreeTab(本体) → Drives → Grid → Filter → MenuBar → (戻る)
///   Favorites: FolderTabBar → FavoriteTab(本体) → Grid → Filter → MenuBar → (戻る)  ※Drivesなし
///   Search:    FolderTabBar → SearchForm(各項目) → SearchHistory → TreeTab → Drives → Grid → Filter → MenuBar → (戻る)
/// FolderTabBar はタブ切替バー自体（左右キーでswitch_folder_tab、Tab/Shift+Tabでは巡回の
/// 起点/終点として1箇所だけ現れる）。TreeTab/Drives は実ツリー選択時とSearch選択時の両方で
/// 使われる（アイテムペイン内のツリー/ドライブと表示・状態を共有する二重の顔を持つ）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusPane {
    FolderTabBar,
    TreeTab,
    FavoriteTab,
    SearchForm,
    SearchHistory,
    Drives,
    Grid,
    Filter,
    MenuBar,
}

impl FocusPane {
    fn next(self, tab: FolderPaneTab) -> Self {
        match self {
            Self::FolderTabBar => match tab {
                FolderPaneTab::RealTree => Self::TreeTab,
                FolderPaneTab::Favorites => Self::FavoriteTab,
                FolderPaneTab::Search => Self::SearchForm,
            },
            Self::TreeTab => Self::Drives,
            Self::SearchForm => Self::SearchHistory,
            Self::SearchHistory => Self::TreeTab,
            Self::Drives => Self::Grid,
            Self::FavoriteTab => Self::Grid,
            Self::Grid => Self::Filter,
            Self::Filter => Self::MenuBar,
            Self::MenuBar => Self::FolderTabBar,
        }
    }

    fn prev(self, tab: FolderPaneTab) -> Self {
        match self {
            Self::FolderTabBar => Self::MenuBar,
            Self::TreeTab => match tab {
                FolderPaneTab::Search => Self::SearchHistory,
                _ => Self::FolderTabBar,
            },
            Self::SearchForm => Self::FolderTabBar,
            Self::SearchHistory => Self::SearchForm,
            Self::Drives => Self::TreeTab,
            Self::FavoriteTab => Self::FolderTabBar,
            Self::Grid => match tab {
                FolderPaneTab::Favorites => Self::FavoriteTab,
                _ => Self::Drives,
            },
            Self::Filter => Self::Grid,
            Self::MenuBar => Self::Filter,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SearchFormFocus {
    NamePattern, IncludeSubdirs, SizeMin, SizeMax, DateAfter, DateBefore, Start, Clear,
}

impl SearchFormFocus {
    fn next(self) -> Option<Self> { Some(match self {
        Self::NamePattern => Self::IncludeSubdirs, Self::IncludeSubdirs => Self::SizeMin,
        Self::SizeMin => Self::SizeMax, Self::SizeMax => Self::DateAfter,
        Self::DateAfter => Self::DateBefore, Self::DateBefore => Self::Start,
        Self::Start => Self::Clear, Self::Clear => return None,
    }) }
    fn prev(self) -> Option<Self> { Some(match self {
        Self::NamePattern => return None, Self::IncludeSubdirs => Self::NamePattern,
        Self::SizeMin => Self::IncludeSubdirs, Self::SizeMax => Self::SizeMin,
        Self::DateAfter => Self::SizeMax, Self::DateBefore => Self::DateAfter,
        Self::Start => Self::DateBefore, Self::Clear => Self::Start,
    }) }
}

#[cfg(test)]
mod search_focus_tests {
    use super::SearchFormFocus::*;

    #[test]
    fn search_form_focus_visits_every_field_in_confirmed_order() {
        let mut current = NamePattern;
        let mut visited = vec![current];
        while let Some(next) = current.next() {
            visited.push(next);
            current = next;
        }
        assert_eq!(visited, vec![NamePattern, IncludeSubdirs, SizeMin, SizeMax,
            DateAfter, DateBefore, Start, Clear]);
        assert_eq!(NamePattern.prev(), None);
        assert_eq!(Clear.next(), None);
    }
}

/// 1回の検索実行結果。左ペインの検索結果リストに1行として表示される
/// （検索された順で最上位に追加され、下へ送られていく）。
#[derive(Clone)]
pub(crate) struct SearchResultEntry {
    /// リスト表示用ラベル（検索ファイル名の表示ができるだけの文字数）
    pub label: String,
    /// ヒットしたファイルのフルパス一覧
    pub hits: Vec<PathBuf>,
    /// 検索開始時点のフォーム入力。履歴を選択した際にフォームへ復元する。
    pub form: SearchFormState,
}

/// 検索結果を履歴の先頭に追加する（新しい実行が最上位に来て、既存分は下に送られる）。
pub(crate) fn push_search_result(history: &mut Vec<SearchResultEntry>, entry: SearchResultEntry) {
    history.insert(0, entry);
}

/// 検索条件フォームの入力状態。テキスト欄はすべて未パース文字列のまま保持し、
/// 実行時（Phase3）にパースする。
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchFormState {
    /// 検索の基点ディレクトリ。None のうちは検索タブ初回入場時に current_dir で初期化される
    /// （switch_folder_tab参照）。以降はアイテムペイン内ツリー/ドライブのクリックで更新され、
    /// タブを行き来しても保持される。
    pub base_dir: Option<PathBuf>,
    pub name_pattern: String,
    pub include_subdirs: bool,
    pub size_min_mb: String,
    pub size_max_mb: String,
    pub date_after: String,
    pub date_before: String,
}

/// サムネグリッドの「↑・サブフォルダ・アーカイブファイル」を貫通する統一カーソル位置。
/// draw_archive_grid内で実際に描画される順序（↑→サブフォルダ→フィルタ後アーカイブ）と
/// 一致させること（grid_entries()参照）。
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum GridEntry {
    Up(PathBuf),
    Subdir(PathBuf),
    /// archives へのインデックス（実インデックス、filtered_indices経由ではない）
    Archive(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FavoriteSelection {
    None,
    /// 未整理のお気に入り（どのフォルダにも紐付かないテンポラリお気に入り群）
    Unsorted,
    Folder(u8),
}

/// メニューバー内のボタンをインデックス化した識別子（左から右への表示順そのもの）。
/// キーボードでの左右移動・Enter確定（handle_menu_bar_keys）の対象になる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuBarButton {
    Reload,
    SortName,
    SortDate,
    SortSize,
    SortOrder,
    CardInfoToggle,
    StatusToggle,
    Settings,
}

/// 表示順そのもの（draw_menu_barの描画順と一致させること）。
/// 見開き・ページモード群はビューアーツールバーへ移設した（toolbar.rs 参照）。
pub(crate) const MENU_BAR_ORDER: [MenuBarButton; 8] = [
    MenuBarButton::Reload,
    MenuBarButton::SortName,
    MenuBarButton::SortDate,
    MenuBarButton::SortSize,
    MenuBarButton::SortOrder,
    MenuBarButton::CardInfoToggle,
    MenuBarButton::Settings,
    MenuBarButton::StatusToggle,
];

#[cfg(test)]
mod menu_bar_order_tests {
    use super::{MenuBarButton, MENU_BAR_ORDER};

    #[test]
    fn settings_and_status_keep_the_visual_right_end_order() {
        assert_eq!(
            &MENU_BAR_ORDER[6..],
            &[
                MenuBarButton::Settings,
                MenuBarButton::StatusToggle,
            ],
        );
    }
}

/// サムネカード下部の情報オーバーレイ帯の表示量。メニューバーの1ボタンで循環する。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum CardInfoMode {
    /// 何も表示しない
    #[default]
    Off,
    /// ファイル名のみ
    Name,
    /// ファイル名 + 更新日時
    NameDate,
    /// ファイル名 + 更新日時 + サイズ
    NameDateSize,
}

impl CardInfoMode {
    /// 押下ごとの循環順: Off → Name → NameDate → NameDateSize → Off
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Off => Self::Name,
            Self::Name => Self::NameDate,
            Self::NameDate => Self::NameDateSize,
            Self::NameDateSize => Self::Off,
        }
    }

    /// 帯に描画する行数（0..=3）
    pub(crate) fn line_count(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Name => 1,
            Self::NameDate => 2,
            Self::NameDateSize => 3,
        }
    }

    /// nekoviewer.state への保存キー
    pub(crate) fn as_state_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Name => "name",
            Self::NameDate => "name_date",
            Self::NameDateSize => "name_date_size",
        }
    }

    /// nekoviewer.state からの復元（未知値は Off）
    pub(crate) fn from_state_str(s: &str) -> Self {
        match s {
            "name" => Self::Name,
            "name_date" => Self::NameDate,
            "name_date_size" => Self::NameDateSize,
            _ => Self::Off,
        }
    }
}

/// 情報帯の見た目。将来 GUI 設定から供給する想定で、今は Default 固定。
#[derive(Clone, Copy)]
pub(crate) struct CardInfoStyle {
    /// 帯の背景色（透過度込み。画像の上にオーバーレイ合成される）
    pub band_color: egui::Color32,
    /// 文字色
    pub text_color: egui::Color32,
    /// 基準文字サイズ(px)。実サイズは cell_h 連動で clamp する。
    pub text_size: f32,
}

impl Default for CardInfoStyle {
    fn default() -> Self {
        Self {
            band_color: egui::Color32::from_black_alpha(150),
            text_color: egui::Color32::from_rgb(240, 240, 240),
            text_size: 13.0,
        }
    }
}

#[derive(Clone, Copy)]
enum FavoriteDialogMode {
    Create,
    Rename(u8),
}

#[derive(Clone)]
struct FavoriteDialogState {
    mode: FavoriteDialogMode,
    name: String,
    marker: String,
    color: egui::Color32,
    error: Option<String>,
}

/// お気に入りマーカーの固定候補セット。
/// マーカーはグリフの単色アルファマスクに色を乗せて描くため、候補は「塗り（solid）
/// グリフ」または「線画（インク全面に色が乗る）グリフ」に限定する。空洞グリフ（☆等）は
/// 内部に色が乗らないため除外。収録・塗り率は glyph_audit テストで機械検証しており、
/// リストを変更したら Windows / Linux 両方で `cargo test glyph -- --nocapture` を通すこと。
const FAVORITE_MARKER_CANDIDATES: &[&str] = &[
    // 星・スート・スパーク
    "★", "✪", "✱", "♥", "❤", "❥", "♦", "♣", "♠",
    // 幾何図形
    "●", "■", "▲", "▼", "◀", "▶", "◆", "◢", "◥", "⬟",
    // 花・記号
    "✿", "✚", "✖", "✔",
    // 音符（線画）
    "♪", "♫", "♬",
    // 物・シンボル
    "☂", "✈", "⚑", "♨", "☎", "✉", "⌛", "☯", "☮",
    // チェス駒
    "♚", "♛", "♜", "♝", "♞", "♟",
];

/// 廃止した空洞・豆腐マーカーから塗り版への移行対応表。
/// 塗りペアが存在しない文字は既定の ★ に寄せる（DB読込時に適用・書き戻し）。
const FAVORITE_MARKER_MIGRATION: &[(&str, &str)] = &[
    ("☆", "★"), // 塗りペア
    ("⚐", "⚑"), // 塗りペア
    ("☀", "★"),
    ("☁", "★"),
    ("☺", "★"),
    ("☻", "★"), // Linux ではフォント未収録（豆腐）
    ("✂", "★"),
    ("⌚", "⌛"), // 同モチーフの塗り版
];

/// ビューアー右クリック「お気に入り詳細設定」ダイアログの状態。
/// 左＝定義済みお気に入りフォルダ一覧、右＝対象ファイルの登録先（デュアルリストボックス）。
struct FavoriteDetailDialogState {
    /// 対象ファイルの絶対パス。単一選択時は1件、複数選択時は選択集合全件。
    targets: Vec<PathBuf>,
    favorite_enabled: bool,
    /// ダイアログを開いた時点での対象ファイル全員の所属フォルダの積集合（共通部分）。
    /// 単一選択時はそのファイルの実際の所属そのものと一致する。
    /// 決定時、この共通部分と `assigned`（ユーザー操作後の右リスト）の差分だけを
    /// 各ファイルの実際の所属に対して加減算適用する（表示されない個別所属を保持するため）。
    common: Vec<u8>,
    assigned: Vec<u8>,
    left_selected: HashSet<u8>,
    right_selected: HashSet<u8>,
    /// 複数選択時のみ使用: チェックボックスOFF（全削除）決定後にもう一段の確認を挟むためのフラグ
    pending_overwrite_confirm: bool,
}

fn default_favorite_color() -> egui::Color32 {
    egui::Color32::from_rgb(255, 204, 0)
}

fn color32_to_rgba_u32(c: egui::Color32) -> u32 {
    let [r, g, b, a] = c.to_array();
    u32::from_be_bytes([r, g, b, a])
}

fn rgba_u32_to_color32(v: u32) -> egui::Color32 {
    let [r, g, b, a] = v.to_be_bytes();
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// エクスプローラーのディレクトリスキャン状態
enum ScanState {
    /// アイドル（未スキャン）
    Idle,
    /// バックグラウンドでスキャン中
    Loading {
        dir: PathBuf,
        rx: mpsc::Receiver<(Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>)>,
        started_at: std::time::Instant,
    },
    /// スキャン完了
    Done,
}

/// ツリー展開のバックグラウンドスキャン状態
struct TreeScanPending {
    path: PathBuf,
    rx: mpsc::Receiver<Vec<PathBuf>>,
}

/// ディレクトリツリーの自動追従（現在地までの祖先チェーンを1階層ずつ展開していく）の進行状態。
/// root から target までの経路は既知の一本道なので、探索ではなく構築として扱う
/// （兄弟ディレクトリの中身には踏み込まない）。
struct TreeAutoFocus {
    /// 最終的にカーソル・選択状態を合わせる対象パス
    target: PathBuf,
    /// これから展開すべき残りの path component（root寄りが先頭）
    remaining: std::collections::VecDeque<std::ffi::OsString>,
    /// 現時点で到達済みのノード（この直下から remaining の先頭を探す）
    current: PathBuf,
}

/// ディレクトリ遷移を開始したUI／内部処理。
/// ツリー内の選択は既に可視なノードを対象とするため、自動スクロールの対象外にする。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryNavigationSource {
    Tree,
    ItemPane,
    System,
}

/// リロードボタンによるツリー一括再取得の待ち状態（スレッド1本で全対象を処理）
struct TreeReloadPending {
    rx: mpsc::Receiver<Vec<(PathBuf, Vec<PathBuf>)>>,
}

/// 7zのFileCache展開待ちで保留したページ/サムネ要求。
/// FileCache結果が届いた時点でこれをまとめて実際のワーカーへ送出する。
enum DeferredArchiveRequest {
    Page(DesiredDecodeJob<LoadRequest>),
    Thumb(EntryThumbRequest),
}

pub struct NekoviewApp {
    pub(crate) config: AppConfig,
    current_dir: PathBuf,
    subdirs: Vec<PathBuf>,
    archives: Vec<PathBuf>,
    tree_root: PathBuf,
    tree_expanded: HashSet<PathBuf>,
    tree_children: HashMap<PathBuf, Vec<PathBuf>>,
    /// 左ペイン: 実フォルダツリー / お気に入りペインの切替状態
    folder_pane_tab: FolderPaneTab,
    /// キーボードフォーカスが現在どの領域にあるか（Tab/Shift+Tabで巡回）
    pub(crate) focused_pane: FocusPane,
    /// 実ツリー内のプレターゲティングカーソル（Enterで確定navigate）
    tree_cursor: Option<PathBuf>,
    /// ドライブ一覧内のプレターゲティングカーソル。Favoritesタブ経由でDrivesへ
    /// 移動した際、実ツリー側にいた頃のこの値を復元する（無効ならフォールバック）
    drive_cursor: Option<PathBuf>,
    /// お気に入りタブ内のプレターゲティングカーソル（[未整理, フォルダ...]の並び）
    favorite_cursor: Option<FavoriteSelection>,
    /// MenuBar内のプレターゲティングカーソル（MENU_BAR_ORDER上のインデックス）
    menu_cursor: usize,
    /// 定義済みお気に入りフォルダ一覧のキャッシュ（DB操作の都度リフレッシュ）
    favorite_folders: Vec<crate::favorites::FavoriteFolder>,
    favorite_selected: FavoriteSelection,
    favorite_dialog: Option<FavoriteDialogState>,
    /// 削除確認待ちのお気に入りフォルダID
    favorite_delete_confirm: Option<u8>,
    /// ビューアー右クリック「お気に入り詳細設定」ダイアログの状態
    favorite_detail_dialog: Option<FavoriteDetailDialogState>,
    /// Some(_) の間、中央グリッドは実ディレクトリではなく選択中のお気に入り
    /// （フォルダ横断）一覧を表示している。
    viewing_favorites: Option<FavoriteSelection>,
    viewing_dir: Option<PathBuf>,
    /// 現PWDのサムネイル進捗 (path, current, total, replacing_old)。
    cd_summary: Option<(PathBuf, usize, usize, bool)>,
    /// バックグラウンドで計算中のサマリー結果受信チャンネル
    cd_summary_rx: Option<mpsc::Receiver<(PathBuf, usize, usize, bool)>>,
    cd_summary_updated_at: Option<std::time::Instant>,
    /// 現在ディレクトリの redb キャッシュDB（キャッシュ無効なら None）
    cache_db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
    /// 現在ディレクトリに対応するキャッシュディレクトリのパス。
    /// DB未作成のフォルダで対象ファイルが見つかった時点の遅延作成に使う。
    cache_neko_dir: Option<PathBuf>,
    /// exe横の見開き状態DB（アプリ起動時に一度だけ開き、使い回す）
    spread_db: Option<std::sync::Arc<std::sync::Mutex<redb::Database>>>,
    /// 現在ディレクトリ内で保存済みの見開き状態 (filename -> (mode, offset, page_index))
    spread_states: HashMap<String, (crate::types::PageMode, i32)>,
    /// 現在ディレクトリ内で保存済みのアーカイブ内ソート条件
    archive_sort_states: HashMap<String, (crate::types::ReaderSortKey, bool)>,
    /// 現在ディレクトリ内のお気に入り登録状態 (filename -> 所属folder_id一覧、空Vec=未整理)
    favorite_states: HashMap<String, Vec<u8>>,
    /// お気に入り・検索の横断一覧表示中のマーカー情報 (フルパス -> 所属folder_id一覧)。
    /// ディレクトリ横断のため favorite_states とは別にフルパスキーで持つ。
    cross_view_favorite_markers: HashMap<PathBuf, Vec<u8>>,
    /// サムネイル上へ表示する保存設定状態。通常・お気に入り・検索をフルパスで共通管理する。
    saved_archive_settings: HashMap<PathBuf, crate::spread_state::SavedArchiveSettings>,
    /// 到達不能と判定済みのネットワークマウント大元（定期ポーリングはしない）
    network_unreachable_mounts: HashSet<PathBuf>,
    /// バックグラウンドで進行中のマウント到達可否チェック
    mount_check_pending: Vec<(PathBuf, mpsc::Receiver<(PathBuf, bool)>)>,
    thumbnails: HashMap<PathBuf, egui::TextureHandle>,
    thumb_req_tx: mpsc::SyncSender<ThumbRequest>,
    thumb_res_rx: mpsc::Receiver<ThumbResult>,
    thumb_session: Arc<AtomicU64>,
    thumb_pending: HashSet<PathBuf>,
    thumb_display_requested: HashSet<PathBuf>,
    thumb_queue: VecDeque<PathBuf>,
    thumb_priority_queue: VecDeque<PathBuf>,
    thumb_queued: HashSet<PathBuf>,
    thumb_missing_queued: HashSet<PathBuf>,
    thumb_priority_queued: HashSet<PathBuf>,
    thumb_last_user_activity: std::time::Instant,
    /// 直近フレームで確定した可視範囲（filtered_indices順のposition）。
    /// スクロール方向の判定にのみ使う（[[update_thumbnail_lookahead]]）。
    thumb_visible_order_range: Option<(usize, usize)>,
    /// サムネキューを最後に作り直した（＝フォルダを開いた）時刻。並列度ウォームアップの起点。
    thumb_queue_built_at: std::time::Instant,
    /// 現PWDのRDBプロファイルに基づく、サムネイル生成の許可状態と競合防止世代。
    thumb_generation_state: crate::neko_dir::ThumbnailGenerationState,
    /// アーカイブ内サムネイルバー用（フォルダグリッドの thumb_req_tx とは別系統）
    entry_thumb_req_tx: mpsc::Sender<EntryThumbRequest>,
    entry_thumb_res_rx: mpsc::Receiver<EntryThumbResult>,
    viewer: Arc<Mutex<Option<ViewerState>>>,
    /// ファイル切替後も維持するビューア設定（zoom・fullscreen 等）
    pub(crate) viewer_cfg: Arc<Mutex<ViewerConfig>>,
    drives: Vec<MountEntry>,
    /// 既知のGVFS SMBマウント一覧（到達可否に関係なく列挙時点の全件）。
    /// panels.rs等でパス単位の判定に使う際、毎フレーム read_dir("/run/user/uid/gvfs")
    /// を避けるためのキャッシュ。reload_current() 側で「進行中の到達可否チェックが
    /// 無い時だけ」readdirして更新する（進行中チェックと同時にreaddirすると
    /// gvfsd内部でロック競合してメインスレッドがブロックされるため）。
    gvfs_mount_entries: Vec<MountEntry>,
    page_cache: Arc<Mutex<PageCache>>,
    file_cache: FileCache,
    file_cache_req_tx: mpsc::Sender<std::path::PathBuf>,
    file_cache_res_rx: mpsc::Receiver<(std::path::PathBuf, Option<FileCacheEntry>)>,
    file_cache_pending: HashSet<PathBuf>,
    /// 7zがFileCacheへの展開待ちの間、ページ/サムネ要求を送らずここに溜めておく。
    /// FileCache結果が届いた時点でまとめてフラッシュする（デコードワーカー側での
    /// スレッドごとの重複展開を避けるため）。
    deferred_archive_requests: HashMap<PathBuf, Vec<DeferredArchiveRequest>>,
    req_tx: DecodeJobQueue<LoadRequest>,
    res_rx: Arc<Mutex<mpsc::Receiver<LoadResult>>>,
    pending_loads: Arc<Mutex<HashSet<(PathBuf, usize)>>>,
    /// デコード失敗ページ。毎フレームの無限再要求を防ぎ、世代変更・再オープン・
    /// FileCache準備完了時には解除して再試行可能にする。
    failed_loads: HashSet<crate::decode_jobs::DecodeJobKey>,
    scan_state: ScanState,
    tree_scan_pending: Option<TreeScanPending>,
    tree_reload_pending: Option<TreeReloadPending>,
    /// ディレクトリツリーの自動追従の進行状態。手動トグル展開（tree_scan_pending）とは
    /// 別レーンで動かし、互いのロード結果を潰さないようにする。
    tree_autofocus: Option<TreeAutoFocus>,
    /// 自動追従が発行した子ディレクトリロードの待ち状態（tree_scan_pendingとは独立）
    tree_autofocus_pending: Option<TreeScanPending>,
    /// 自動追従が完了した直後の1フレームだけtrueにし、対象ノード描画時にスクロールを行わせる
    tree_autofocus_scroll_pending: bool,
    /// フレームごとに更新されるウィンドウサイズ（論理ピクセル）
    window_size: (u32, u32),
    /// ビューアウィンドウの位置・サイズスロット（viewer と共有して永続化）
    viewer_slots: [Option<WindowSlot>; 4],
    /// archives のうち生画像ファイルのセット（赤枠表示・シングルクリック開封用）
    raw_image_files: std::collections::HashSet<PathBuf>,
    /// 起動時にCLIでファイル指定された場合の自動オープン対象。
    /// 初回スキャン完了時（poll_scan）に一度だけ試行し、成否に関わらずNoneへ戻す。
    pending_open_target: Option<PathBuf>,
    /// 無効確定済みZIP（画像エントリなし）のセット（現ディレクトリセッション中に保持）
    invalid_archives: std::collections::HashSet<PathBuf>,
    /// サムネイル生成に失敗したファイルのセット（DB非永続・セッション中のみ。
    /// 無限リトライを止めるためのマーカーで、invalid_archives とは異なり
    /// 次回スキャンでの一覧除外は行わない）
    thumb_failed: std::collections::HashSet<PathBuf>,
    /// アプリレベルのトーストメッセージ（3秒で自動消去）
    pub(crate) app_toast: Option<(String, std::time::Instant)>,
    /// フェーズ2: ページキャッシュ予算（見積もりゲートの閾値。resolve_cache_budgetsのpage_max）
    cache_budget_bytes: usize,
    /// フェーズ4: アニメリングバッファ先読み枚数の(下限, 上限)。見積もりゲートも同じ値を使う。
    anim_ring_bounds: (usize, usize),
    /// フェーズ2: メモリ見積もり超過を知らせる確認ダイアログの表示状態
    memory_warning_open: bool,
    /// エクスプローラーからのアーカイブオープン非同期処理。Some の間は
    /// 中央オーバーレイでプログレスバー＋キャンセルボタンを表示し、
    /// エクスプローラー側の他操作（キーボードショートカット等）を止める。
    pending_open: Option<open_progress::PendingOpen>,
    /// 設定ダイアログの表示状態・選択中タブ・編集用下書き
    pub(crate) settings_open: bool,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) settings_draft: SettingsDraft,
    /// 翻訳機能(実験的)の永続設定。設定ダイアログの[反映]でのみ書き換わる。
    pub(crate) translate_cfg: crate::translate::TranslateConfig,
    /// 接続テストの進行中受信チャンネル（ダイアログを閉じたら破棄）。
    pub(crate) translate_conn_rx: Option<mpsc::Receiver<crate::translate::ConnCheckMsg>>,
    /// 直近の接続テスト結果表示用（疎通/vision結果の文字列、または失敗理由）。
    pub(crate) translate_conn_status: Option<String>,
    /// ツールバーの翻訳トグルボタンを有効化してよいか（セッション単位、非永続）。
    /// [反映]時に、直前の疎通チェック成功URLと実際に反映されるURLが一致する場合だけtrueになる。
    pub(crate) translate_conn_verified: bool,
    /// 直近に疎通チェックが成功した時点のURL（[反映]時のURL一致検証に使う）。
    pub(crate) translate_conn_verified_url: Option<String>,
    pub(crate) translate_ocr_rx: Option<mpsc::Receiver<crate::translate::OcrMsg>>,
    pub(crate) translate_ocr_status: Option<String>,
    /// 実行中のOCRリクエストがどのページ宛てか。結果到着時にそのページ用のtxtへ保存する
    /// （待っている間にユーザーがページ送りしても、要求時点のページへ正しく保存するため）。
    pub(crate) translate_ocr_inflight_key: Option<(PathBuf, usize)>,
    /// 見開き時、2ページぶんを1リクエストに結合せず個別に逐次実行するための残りキュー。
    /// モデルに「どこまでが左ページか」を自己申告させると信頼できないため、ページの
    /// 切り分け・ラベル付けは常にアプリ側(この構造)で行い、モデルには単独ページとして
    /// 1枚ずつ渡す。
    pub(crate) translate_ocr_queue: std::collections::VecDeque<(PathBuf, usize)>,
    /// OCR/翻訳子ウィンドウ（独立OS窓）の表示状態。既存txtが1P分でも残っていれば
    /// 自動で開き、無ければビューアー部の[翻訳]ボタンでユーザーが開く。
    pub(crate) translate_window_open: bool,
    /// OCR/翻訳子ウィンドウの最前面固定トグル。低解像度モニターでウィンドウが混線する
    /// 環境向け。ONのときは本体窓の操作を奪ってよい（子側優先の設計）。
    pub(crate) translate_window_always_on_top: bool,
    /// OCR/翻訳子ウィンドウが現在フォーカスしている単一ページ。見開き中は親の可視2ページの
    /// うちどちらかを指す。親の可視集合に含まれなくなったら(=親が別に動いた)先頭へ再同期する。
    pub(crate) translate_child_cursor: Option<(PathBuf, usize)>,
    /// 子ウィンドウ左ペイン(OCR原文)の表示内容。`translate_child_cursor`のページ分のtxt。
    pub(crate) translate_child_ocr_lines: Vec<String>,
    /// 子ウィンドウで選択中の原文言語。未設定(None)ならOCR/翻訳プロンプトへは反映しない。
    pub(crate) translate_child_source_lang: Option<crate::translate::TranslateLang>,
    /// 子ウィンドウで選択中の翻訳先言語。未設定(None)なら翻訳を実行できない。
    pub(crate) translate_child_target_lang: Option<crate::translate::TranslateLang>,
    /// 保存済み翻訳データが記録している言語ペア（アーカイブを開いた/翻訳を保存した時点の
    /// スナップショット）。現在UIで選択中の言語ペアとの食い違いをUIへ警告表示するために使う。
    pub(crate) translate_child_saved_lang_meta: Option<(crate::translate::TranslateLang, crate::translate::TranslateLang)>,
    /// 子ウィンドウ右ペイン(翻訳結果)の表示内容。OCRとは完全に独立した処理単位・状態。
    pub(crate) translate_child_translation_lines: Vec<String>,
    pub(crate) translate_translate_rx: Option<mpsc::Receiver<crate::translate::TranslateMsg>>,
    pub(crate) translate_translate_status: Option<String>,
    /// 実行中の翻訳リクエストがどのページ宛てか。OCRのinflight_keyと同じ理由で保持する。
    pub(crate) translate_translate_inflight_key: Option<(PathBuf, usize)>,
    /// 実行中の翻訳リクエストで使った言語ペア。完了時にアーカイブ単位の言語メタとして
    /// 保存するために、リクエスト開始時点の値を保持しておく。
    pub(crate) translate_translate_inflight_lang: Option<(crate::translate::TranslateLang, crate::translate::TranslateLang)>,
    /// 原文言語判定リクエストの受信チャネル・ステータス表示。[言語判定]ボタンが
    /// 押された時だけ発火する（自動実行はしない）。
    pub(crate) translate_lang_detect_rx: Option<mpsc::Receiver<crate::translate::LangDetectMsg>>,
    pub(crate) translate_lang_detect_status: Option<String>,
    /// 判定結果が現在の原文/翻訳先設定と食い違う場合の確認待ち状態。Some中はUIに
    /// 「判定結果を設定」「現在の設定を維持」の選択を出し、ユーザーが選ぶまで反映しない。
    pub(crate) translate_lang_detect_pending: Option<crate::translate::TranslateLang>,
    /// 直近に子ウィンドウの原文/翻訳先言語を保存済みメタから同期したアーカイブパス。
    /// アーカイブが変わった時だけ復元処理を行うためのキャッシュ。
    pub(crate) translate_child_lang_synced_for: Option<PathBuf>,
    /// 直近に自動オープン判定を行ったアーカイブパス（同一アーカイブ内での毎フレーム
    /// 再チェックを避けるためのキャッシュ）。
    pub(crate) translate_window_autocheck_done_for: Option<PathBuf>,
    /// ビューアウィンドウをフォーカス前面に出すフラグ
    viewer_focus_requested: bool,
    pub(crate) show_hidden: bool,
    /// サムネカード下部の情報帯の表示量（メニューバーの1ボタンで循環）。
    pub(crate) card_info_mode: CardInfoMode,
    /// 情報帯の「更新日時」行に使う日付書式。設定ダイアログのエクスプローラータブで編集。
    pub(crate) card_date_format: crate::card_date_format::CardDateFormat,
    /// 情報帯の見た目（背景色・透過度・文字色・文字サイズ）。今は Default 固定。
    pub(crate) card_info_style: CardInfoStyle,
    /// 情報帯の行が帯幅を超えた時のホバー横スクロール状態: (対象パス, ホバー開始時刻)。
    pub(crate) card_info_hover: Option<(PathBuf, std::time::Instant)>,
    /// 可視カードぶんだけ遅延取得するファイルメタデータのキャッシュ: パス → (更新日時, サイズbytes)。
    /// スキャンで archives を作り直すたびにクリアする。
    pub(crate) archive_meta_cache: HashMap<PathBuf, (std::time::SystemTime, u64)>,
    sort_key: ExplorerSortKey,
    sort_ascending: bool,
    /// サムネグリッドの統一カーソル位置（↑/サブフォルダ/アーカイブを貫通）
    grid_cursor: Option<GridEntry>,
    selected_archive_index: Option<usize>,
    selected_archive_meta: Option<(std::time::SystemTime, u64)>,
    /// Ctrl/Shift併用による複数選択の集合（archivesへのインデックス）。
    /// 空 = 単一選択モード。非空時は selected_archive_index も含めて保持する。
    multi_selected: std::collections::HashSet<usize>,
    /// Shift範囲選択の起点インデックス。
    select_anchor: Option<usize>,
    /// サムネフィルタ: 有効フラグ・入力文字列・絞り込み後の archives インデックス一覧
    filter_enabled: bool,
    filter_text: String,
    filtered_indices: Vec<usize>,
    /// 検索結果の履歴（新しい実行が先頭。セッション内のみ保持）
    search_history: Vec<SearchResultEntry>,
    /// 履歴内で選択中の位置（Someなら中央ペインにその結果を表示）
    search_selected: Option<usize>,
    /// 検索条件フォームの入力状態
    search_form: SearchFormState,
    search_form_focus: SearchFormFocus,
    search_form_focus_request: bool,
    search_date_start_calendar: calendar_gui::CalendarGui,
    search_date_end_calendar: calendar_gui::CalendarGui,
    search_calendar_today: calendar_gui::LocalDate,
    /// true: 検索実行中（完了までは多重実行不可、検索開始ボタンを無効化する）
    search_running: bool,
    /// 検索ワーカーからの結果受信チャンネル（実行中のみSome）
    search_pending: Option<mpsc::Receiver<Vec<PathBuf>>>,
    /// 実行中の検索を開始した時点のフォーム入力。
    search_pending_form: Option<SearchFormState>,
    /// Some(_) の間、中央グリッドは実ディレクトリではなく選択中の検索結果
    /// （search_history[idx]）のフラット一覧を表示している。
    viewing_search: Option<usize>,
    explorer_cols: usize,
    explorer_scroll_offset: f32,
    explorer_viewport_h: f32,
    /// フォルダ名ラベルの1秒ホバー救済用: (対象パス, ホバー開始時刻)
    folder_label_hover: Option<(PathBuf, std::time::Instant)>,
    /// ステータスウィンドウ表示フラグ（[?] ボタンでトグル）
    show_status_window: bool,
    status_window_data: Arc<Mutex<crate::view_status::StatusData>>,
    /// ステータスデータを最後に更新した時刻（1秒間隔制御用）
    last_status_update: std::time::Instant,
    /// 各 View から controller 経由でセットされる即時更新要求フラグ
    status_update_requested: Arc<std::sync::atomic::AtomicBool>,
    /// バックグラウンドワーカーから ROOT を起こす（イベント駆動再描画）ために保持する ctx
    egui_ctx: egui::Context,
    /// ビューアー窓自身の ctx（render_viewer 毎フレーム更新）。OCR/翻訳子ウィンドウから
    /// ページ送りで共有 ViewerState を書き換えた際、独立 Context のビューアー窓を
    /// 起こす（request_repaint）ために保持する。
    viewer_egui_ctx: Option<egui::Context>,
    /// OCR/翻訳子ウィンドウ自身の ctx（render_translate_window 毎フレーム更新）。
    /// ビューアー窓側の通常のページ送り（子ウィンドウの操作を介さない）で子ウィンドウを
    /// 起こす（request_repaint）ために保持する。
    translate_egui_ctx: Option<egui::Context>,
    /// 直近にrender_viewerが観測した親の可視ページ集合。変化を検知したら子ウィンドウを起こす。
    translate_last_seen_parent_keys: Vec<(PathBuf, usize)>,
    /// フェーズ6: viewer_cfg.redecode_trigger_seq のうち処理済みの値（変化検知用）
    resize_redecode_last_seq: u64,
    /// フェーズ6: デバウンス期限（この時刻を過ぎたら再デコード発火）。None = 待ち無し
    resize_redecode_deadline: Option<std::time::Instant>,
    /// フェーズ6: 直近の再デコードで決まった、以降のデコード要求(先読み含む)に使うターゲットサイズ。
    /// None = 無制限(原寸、zoom_actual時)。起動直後の既定値は従来の固定上限と同じ。
    decode_target: Option<(u32, u32)>,
    /// 画面表示のフォールバックとして採用する確定世代。
    active_decode_generation: u64,
    /// 最新サイズを準備中の世代。完成ページはactiveより優先表示する。
    preparing_decode_generation: Option<u64>,
    /// 新しい世代番号の発行元。通常はpreparing、未準備時はactiveと同じ。
    decode_generation: u64,
    /// 項目(D): viewer_cfg.exif_orientation_enabled の変化検知用（設定ダイアログ・
    /// ビューアーツールバーのチェックボックス、どちらの経路で変更されても拾えるようにする）。
    exif_orientation_enabled_last_seen: bool,
}

mod scan;
mod workers;
mod viewer_host;
mod input;
mod panels;
mod favorites_ui;
mod search_ui;
mod search;
mod status;
mod nav_icons;
mod calendar_gui;
mod open_progress;

#[cfg(test)]
mod glyph_audit;


impl NekoviewApp {
    pub fn new(start_dir: PathBuf, config: AppConfig, viewer_slots: [Option<WindowSlot>; 4], sort_state: SortState, viewer_cfg: ViewerConfig, show_hidden: bool, card_info_mode: &str, card_date_format: crate::card_date_format::CardDateFormat, translate_cfg: crate::translate::TranslateConfig, open_target: Option<PathBuf>, ctx: egui::Context) -> Self {
        // timeのローカルオフセット取得は、Unixでは他スレッド起動前に行う必要がある。
        let local_today = calendar_gui::LocalDate::today_local();
        let (cache_max, cache_min, file_cache_max) = crate::cache::resolve_cache_budgets(config.cache_total_mb);
        let ring_bounds = (config.anim_ring_min_frames, config.anim_ring_max_frames);
        let frame_hard_limit_bytes = config.anim_frame_hard_limit_mb * 1024 * 1024;
        // 長辺px上限のみ指定し、正方形の箱として resize_for_display に渡す。
        // fit-within(縦横比維持)なので短辺は箱の中に自動的に収まる。
        let max_decode_target = (config.max_decode_edge, config.max_decode_edge);
        let config_root = config.config_root.clone();
        let settings_draft = SettingsDraft::from_current(&config, &viewer_cfg, show_hidden, card_date_format, &translate_cfg);
        let (req_tx, res_rx) = spawn_worker(config.viewer_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone(), cache_max, ring_bounds, frame_hard_limit_bytes);
        let (thumb_req_tx, thumb_res_rx, thumb_session) =
            spawn_thumb_worker(config.resolved_decode_threads(), ctx.clone());
        let (entry_thumb_req_tx, entry_thumb_res_rx) = spawn_entry_thumb_worker(config.thumb_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone());
        let (file_cache_req_tx, file_cache_res_rx) = spawn_file_cache_worker(ctx.clone(), file_cache_max);
        let mut drives = list_local_drives();
        let gvfs_mounts = list_gvfs_smb_mounts();
        let gvfs_mount_entries = gvfs_mounts.clone();
        drives.extend(gvfs_mounts);

        // start_dir を含むドライブのパスをツリーのルートにする
        let tree_root = drives
            .iter()
            .filter(|d| start_dir.starts_with(&d.path))
            .max_by_key(|d| d.path.components().count())
            .map(|d| d.path.clone())
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/"))
            });

        // ツリールートのサブディレクトリをバックグラウンドで取得
        let tree_scan_pending = Some(TreeScanPending {
            path: tree_root.clone(),
            rx: dir::spawn_scan_subdirs(tree_root.clone(), {
                let c = ctx.clone();
                move || c.request_repaint()
            }),
        });

        // 段階5: 旧 1Hz ティッカースレッド＋ROOT 外部ウェイクは撤去。debug のステータス窓は
        // 独立 OS 窓になり、render_status 内の request_repaint_after(1s) で自分自身を 1Hz で
        // 起こし続ける（winit ループがその予定で WaitUntil する）。

        let initial_thumb_size = config.thumb_size;
        let initial_thumb_filter = config.thumb_filter.thumbnail_cache_id();
        let mut app = Self {
            config,
            current_dir: start_dir,
            subdirs: Vec::new(),
            archives: Vec::new(),
            tree_root,
            tree_expanded: HashSet::new(),
            tree_children: HashMap::new(),
            folder_pane_tab: FolderPaneTab::RealTree,
            focused_pane: FocusPane::TreeTab,
            tree_cursor: None,
            drive_cursor: None,
            favorite_cursor: None,
            menu_cursor: 0,
            favorite_folders: Vec::new(),
            favorite_selected: FavoriteSelection::None,
            favorite_dialog: None,
            favorite_delete_confirm: None,
            favorite_detail_dialog: None,
            viewing_favorites: None,
            viewing_dir: None,
            cd_summary: None,
            cd_summary_rx: None,
            cd_summary_updated_at: None,
            cache_db: None,
            cache_neko_dir: None,
            spread_db: {
                let db = crate::spread_state::open_spread_db(&config_root);
                if let Some(db) = &db {
                    crate::favorites::init_favorite_tables(db);
                    // 候補刷新で廃止した空洞・豆腐マーカーを塗り版へ一括移行
                    crate::favorites::migrate_markers(db, FAVORITE_MARKER_MIGRATION);
                }
                db
            },
            spread_states: HashMap::new(),
            archive_sort_states: HashMap::new(),
            favorite_states: HashMap::new(),
            cross_view_favorite_markers: HashMap::new(),
            saved_archive_settings: HashMap::new(),
            network_unreachable_mounts: HashSet::new(),
            mount_check_pending: Vec::new(),
            thumbnails: HashMap::new(),
            thumb_req_tx,
            thumb_res_rx,
            thumb_session,
            thumb_pending: HashSet::new(),
            thumb_display_requested: HashSet::new(),
            thumb_queue: VecDeque::new(),
            thumb_priority_queue: VecDeque::new(),
            thumb_queued: HashSet::new(),
            thumb_missing_queued: HashSet::new(),
            thumb_priority_queued: HashSet::new(),
            thumb_last_user_activity: std::time::Instant::now(),
            thumb_visible_order_range: None,
            thumb_queue_built_at: std::time::Instant::now(),
            thumb_generation_state: crate::neko_dir::ThumbnailGenerationState {
                requested_edge: initial_thumb_size,
                requested_filter: initial_thumb_filter,
            },
            entry_thumb_req_tx,
            entry_thumb_res_rx,
            viewer: Arc::new(Mutex::new(None)),
            viewer_cfg: Arc::new(Mutex::new(viewer_cfg)),
            drives,
            gvfs_mount_entries,
            page_cache: Arc::new(Mutex::new(PageCache::new(cache_max, cache_min))),
            file_cache: FileCache::new(file_cache_max),
            file_cache_req_tx,
            file_cache_res_rx,
            file_cache_pending: HashSet::new(),
            deferred_archive_requests: HashMap::new(),
            req_tx,
            res_rx: Arc::new(Mutex::new(res_rx)),
            pending_loads: Arc::new(Mutex::new(HashSet::new())),
            failed_loads: HashSet::new(),
            scan_state: ScanState::Idle,
            tree_scan_pending,
            tree_reload_pending: None,
            tree_autofocus: None,
            tree_autofocus_pending: None,
            tree_autofocus_scroll_pending: false,
            window_size: (1024, 768),
            viewer_slots,
            raw_image_files: std::collections::HashSet::new(),
            pending_open_target: open_target,
            invalid_archives: std::collections::HashSet::new(),
            thumb_failed: std::collections::HashSet::new(),
            app_toast: None,
            cache_budget_bytes: cache_max,
            anim_ring_bounds: ring_bounds,
            memory_warning_open: false,
            pending_open: None,
            settings_open: false,
            settings_tab: SettingsTab::Common,
            settings_draft,
            translate_cfg,
            translate_conn_rx: None,
            translate_conn_status: None,
            translate_conn_verified: false,
            translate_conn_verified_url: None,
            translate_ocr_rx: None,
            translate_ocr_status: None,
            translate_window_open: false,
            translate_window_always_on_top: false,
            translate_child_cursor: None,
            translate_child_ocr_lines: Vec::new(),
            translate_child_source_lang: None,
            translate_child_target_lang: None,
            translate_child_saved_lang_meta: None,
            translate_child_translation_lines: Vec::new(),
            translate_translate_rx: None,
            translate_translate_status: None,
            translate_translate_inflight_key: None,
            translate_translate_inflight_lang: None,
            translate_lang_detect_rx: None,
            translate_lang_detect_status: None,
            translate_lang_detect_pending: None,
            translate_child_lang_synced_for: None,
            translate_window_autocheck_done_for: None,
            translate_ocr_inflight_key: None,
            translate_ocr_queue: std::collections::VecDeque::new(),
            viewer_focus_requested: false,
            show_hidden,
            card_info_mode: CardInfoMode::from_state_str(card_info_mode),
            card_date_format,
            card_info_style: CardInfoStyle::default(),
            card_info_hover: None,
            archive_meta_cache: HashMap::new(),
            sort_key: ExplorerSortKey::from_state_key(&sort_state.key),
            sort_ascending: sort_state.ascending,
            grid_cursor: None,
            selected_archive_index: None,
            selected_archive_meta: None,
            multi_selected: std::collections::HashSet::new(),
            select_anchor: None,
            filter_enabled: true,
            filter_text: String::new(),
            filtered_indices: Vec::new(),
            search_history: Vec::new(),
            search_selected: None,
            search_form: SearchFormState::default(),
            search_form_focus: SearchFormFocus::NamePattern,
            search_form_focus_request: false,
            search_date_start_calendar: calendar_gui::CalendarGui::new(local_today),
            search_date_end_calendar: calendar_gui::CalendarGui::new(local_today),
            search_calendar_today: local_today,
            search_running: false,
            search_pending: None,
            search_pending_form: None,
            viewing_search: None,
            explorer_cols: 1,
            explorer_scroll_offset: 0.0,
            explorer_viewport_h: 0.0,
            folder_label_hover: None,
            show_status_window: false,
            status_window_data: Arc::new(Mutex::new(crate::view_status::StatusData::default())),
            last_status_update: std::time::Instant::now(),
            status_update_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            egui_ctx: ctx,
            viewer_egui_ctx: None,
            translate_egui_ctx: None,
            translate_last_seen_parent_keys: Vec::new(),
            resize_redecode_last_seq: viewer_cfg.redecode_trigger_seq,
            resize_redecode_deadline: None,
            decode_target: Some(max_decode_target),
            active_decode_generation: 0,
            preparing_decode_generation: None,
            decode_generation: 0,
            exif_orientation_enabled_last_seen: viewer_cfg.exif_orientation_enabled,
        };
        app.start_scan();
        app.refresh_favorite_folders();
        // 起動フォルダをディレクトリツリー側にも同期する。ドライブルート→現在地までを
        // 1階層ずつ逐次展開し、現在地ノードに現在地マーカー（赤反転）を点け、ツリー
        // ビューの外にあればビューポート内へ寄せる（poll_tree_autofocus が毎フレーム
        // 進める）。現在地が tree_root 配下でなければ start_tree_autofocus 側で no-op。
        app.viewing_dir = Some(app.current_dir.clone());
        app.start_tree_autofocus(app.current_dir.clone());
        // 起動時点でGVFSマウントの到達可否確認を仕込んでおく。
        // ユーザーが最初にリロードを押す頃には判定が終わっている見込みが立ち、
        // 「初回リロードでは切断先が消えない」体感を和らげる。
        for mount in app.gvfs_mount_entries.clone() {
            app.spawn_mount_check_if_needed(mount.path);
        }
        app
    }

    /// カレントディレクトリ・ウィンドウ状態・ソート順・言語・ビューア設定・設定ダイアログで
    /// 編集されうる AppConfig 値をまとめて state ファイルへ書き戻す。
    pub(crate) fn persist_state(&self) {
        crate::gui_config::save_state(
            &self.config.config_root,
            &self.current_dir, self.window_size, &self.viewer_slots,
            &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending },
            i18n::lang_code(),
            &*self.viewer_cfg.lock().unwrap(),
            self.show_hidden,
            self.card_info_mode.as_state_str(),
            &self.card_date_format,
            &self.config,
            &self.translate_cfg,
        );
    }
}

#[cfg(test)]
mod search_result_tests {
    use super::{push_search_result, SearchResultEntry};

    #[test]
    fn push_search_result_inserts_newest_at_top() {
        let mut history: Vec<SearchResultEntry> = Vec::new();

        push_search_result(&mut history, SearchResultEntry { label: "1回目".to_string(), hits: vec![], form: Default::default() });
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].label, "1回目");

        push_search_result(&mut history, SearchResultEntry { label: "2回目".to_string(), hits: vec![], form: Default::default() });
        assert_eq!(history.len(), 2);
        // 新しい実行が最上位、既存分は下に送られる
        assert_eq!(history[0].label, "2回目");
        assert_eq!(history[1].label, "1回目");

        push_search_result(&mut history, SearchResultEntry { label: "3回目".to_string(), hits: vec![], form: Default::default() });
        assert_eq!(
            history.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
            vec!["3回目", "2回目", "1回目"],
        );
    }
}
