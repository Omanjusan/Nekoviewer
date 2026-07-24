use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use crate::cache::{FileCache, FileCacheEntry, LoadRequest, LoadResult, PageCache, ThumbRequest, ThumbResult, EntryThumbRequest, EntryThumbResult, spawn_worker, spawn_thumb_worker, spawn_entry_thumb_worker, spawn_file_cache_worker};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderPaneTab {
    RealTree,
    Favorites,
}

/// キーボード操作のフォーカス巡回順（Tab/Shift+Tabで一周する）。
/// 順序: TreeTab(初期値) → Grid → Filter → Drives → MenuBar → FavoriteTab → (先頭に戻る)
/// TreeTab/FavoriteTab は左ペインのタブ切替を兼ねる。Drives は実ツリー配下の
/// ドライブ一覧のみを指し、Favorites表示中でも巡回上は残る（着地時に実ツリーへ
/// 自動復帰する）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusPane {
    TreeTab,
    FavoriteTab,
    Drives,
    Grid,
    Filter,
    MenuBar,
}

impl FocusPane {
    fn next(self) -> Self {
        match self {
            Self::TreeTab => Self::Grid,
            Self::Grid => Self::Filter,
            Self::Filter => Self::Drives,
            Self::Drives => Self::MenuBar,
            Self::MenuBar => Self::FavoriteTab,
            Self::FavoriteTab => Self::TreeTab,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::TreeTab => Self::FavoriteTab,
            Self::Grid => Self::TreeTab,
            Self::Filter => Self::Grid,
            Self::Drives => Self::Filter,
            Self::MenuBar => Self::Drives,
            Self::FavoriteTab => Self::MenuBar,
        }
    }
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
    SortName,
    SortDate,
    SortSize,
    SortOrder,
    StatusToggle,
    Settings,
}

/// 表示順そのもの（draw_menu_barの描画順と一致させること）。
/// 見開き・ページモード群はビューアーツールバーへ移設した（toolbar.rs 参照）。
pub(crate) const MENU_BAR_ORDER: [MenuBarButton; 6] = [
    MenuBarButton::SortName,
    MenuBarButton::SortDate,
    MenuBarButton::SortSize,
    MenuBarButton::SortOrder,
    MenuBarButton::Settings,
    MenuBarButton::StatusToggle,
];

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

/// 7zのFileCache展開待ちで保留したページ/サムネ要求。
/// FileCache結果が届いた時点でこれをまとめて実際のワーカーへ送出する。
enum DeferredArchiveRequest {
    Page(LoadRequest),
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
    /// true: カーソルはツリー本体ではなくTreeTabボタン自体にいる（上下キーでの
    /// タブ⇄本体の行き来を表現する。本体先頭ノードでUp、またはこの状態でDownで切替）
    tree_at_tab: bool,
    /// ドライブ一覧内のプレターゲティングカーソル。Favoritesタブ経由でDrivesへ
    /// 移動した際、実ツリー側にいた頃のこの値を復元する（無効ならフォールバック）
    drive_cursor: Option<PathBuf>,
    /// お気に入りタブ内のプレターゲティングカーソル（[未整理, フォルダ...]の並び）
    favorite_cursor: Option<FavoriteSelection>,
    /// true: カーソルは本体リストではなくFavoriteTabボタン自体にいる（tree_at_tabと同様）
    favorite_at_tab: bool,
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
    /// CD/LSディレクトリのサマリーキャッシュ (path, saved_thumbs, total_archives)
    cd_summary: Option<(PathBuf, usize, usize)>,
    /// バックグラウンドで計算中のサマリー結果受信チャンネル
    cd_summary_rx: Option<mpsc::Receiver<(PathBuf, usize, usize)>>,
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
    /// 現在ディレクトリ内のお気に入り登録状態 (filename -> 所属folder_id一覧、空Vec=未整理)
    favorite_states: HashMap<String, Vec<u8>>,
    /// お気に入り一覧表示中のマーカー情報 (フルパス -> 所属folder_id一覧)。
    /// ディレクトリ横断のため favorite_states とは別にフルパスキーで持つ。
    favorite_view_markers: HashMap<PathBuf, Vec<u8>>,
    /// 到達不能と判定済みのネットワークマウント大元（定期ポーリングはしない）
    network_unreachable_mounts: HashSet<PathBuf>,
    /// バックグラウンドで進行中のマウント到達可否チェック
    mount_check_pending: Vec<(PathBuf, mpsc::Receiver<(PathBuf, bool)>)>,
    thumbnails: HashMap<PathBuf, egui::TextureHandle>,
    thumb_req_tx: mpsc::SyncSender<ThumbRequest>,
    thumb_res_rx: mpsc::Receiver<ThumbResult>,
    thumb_pending: HashSet<PathBuf>,
    /// アーカイブ内サムネイルバー用（フォルダグリッドの thumb_req_tx とは別系統）
    entry_thumb_req_tx: mpsc::Sender<EntryThumbRequest>,
    entry_thumb_res_rx: mpsc::Receiver<EntryThumbResult>,
    viewer: Arc<Mutex<Option<ViewerState>>>,
    /// ファイル切替後も維持するビューア設定（zoom・fullscreen 等）
    pub(crate) viewer_cfg: Arc<Mutex<ViewerConfig>>,
    drives: Vec<MountEntry>,
    page_cache: Arc<Mutex<PageCache>>,
    file_cache: FileCache,
    file_cache_req_tx: mpsc::Sender<std::path::PathBuf>,
    file_cache_res_rx: mpsc::Receiver<(std::path::PathBuf, Option<FileCacheEntry>)>,
    file_cache_pending: HashSet<PathBuf>,
    /// 7zがFileCacheへの展開待ちの間、ページ/サムネ要求を送らずここに溜めておく。
    /// FileCache結果が届いた時点でまとめてフラッシュする（デコードワーカー側での
    /// スレッドごとの重複展開を避けるため）。
    deferred_archive_requests: HashMap<PathBuf, Vec<DeferredArchiveRequest>>,
    req_tx: mpsc::Sender<LoadRequest>,
    res_rx: Arc<Mutex<mpsc::Receiver<LoadResult>>>,
    pending_loads: Arc<Mutex<HashSet<(PathBuf, usize)>>>,
    scan_state: ScanState,
    tree_scan_pending: Option<TreeScanPending>,
    /// フレームごとに更新されるウィンドウサイズ（論理ピクセル）
    window_size: (u32, u32),
    /// ビューアウィンドウの位置・サイズスロット（viewer と共有して永続化）
    viewer_slots: [Option<WindowSlot>; 4],
    /// archives のうち生画像ファイルのセット（赤枠表示・シングルクリック開封用）
    raw_image_files: std::collections::HashSet<PathBuf>,
    /// 無効確定済みZIP（画像エントリなし）のセット（現ディレクトリセッション中に保持）
    invalid_archives: std::collections::HashSet<PathBuf>,
    /// サムネイル生成に失敗したファイルのセット（DB非永続・セッション中のみ。
    /// 無限リトライを止めるためのマーカーで、invalid_archives とは異なり
    /// 次回スキャンでの一覧除外は行わない）
    thumb_failed: std::collections::HashSet<PathBuf>,
    /// アプリレベルのトーストメッセージ（3秒で自動消去）
    app_toast: Option<(String, std::time::Instant)>,
    /// フェーズ2: ページキャッシュ予算（見積もりゲートの閾値。resolve_cache_budgetsのpage_max）
    cache_budget_bytes: usize,
    /// フェーズ4: アニメリングバッファ先読み枚数の(下限, 上限)。見積もりゲートも同じ値を使う。
    anim_ring_bounds: (usize, usize),
    /// フェーズ2: メモリ見積もり超過を知らせる確認ダイアログの表示状態
    memory_warning_open: bool,
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
    /// 直近に子ウィンドウの原文/翻訳先言語を保存済みメタから同期したアーカイブパス。
    /// アーカイブが変わった時だけ復元処理を行うためのキャッシュ。
    pub(crate) translate_child_lang_synced_for: Option<PathBuf>,
    /// 直近に自動オープン判定を行ったアーカイブパス（同一アーカイブ内での毎フレーム
    /// 再チェックを避けるためのキャッシュ）。
    pub(crate) translate_window_autocheck_done_for: Option<PathBuf>,
    /// ビューアウィンドウをフォーカス前面に出すフラグ
    viewer_focus_requested: bool,
    pub(crate) show_hidden: bool,
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
mod status;
mod nav_icons;

#[cfg(test)]
mod glyph_audit;


impl NekoviewApp {
    pub fn new(start_dir: PathBuf, config: AppConfig, viewer_slots: [Option<WindowSlot>; 4], sort_state: SortState, viewer_cfg: ViewerConfig, show_hidden: bool, translate_cfg: crate::translate::TranslateConfig, ctx: egui::Context) -> Self {
        let (cache_max, cache_min, file_cache_max) = crate::cache::resolve_cache_budgets(config.cache_total_mb);
        let ring_bounds = (config.anim_ring_min_frames, config.anim_ring_max_frames);
        let frame_hard_limit_bytes = config.anim_frame_hard_limit_mb * 1024 * 1024;
        // 長辺px上限のみ指定し、正方形の箱として resize_for_display に渡す。
        // fit-within(縦横比維持)なので短辺は箱の中に自動的に収まる。
        let max_decode_target = (config.max_decode_edge, config.max_decode_edge);
        let settings_draft = SettingsDraft::from_current(&config, &viewer_cfg, show_hidden, &translate_cfg);
        let (req_tx, res_rx) = spawn_worker(config.viewer_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone(), cache_max, ring_bounds, frame_hard_limit_bytes);
        let (thumb_req_tx, thumb_res_rx) = spawn_thumb_worker(config.thumb_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone());
        let (entry_thumb_req_tx, entry_thumb_res_rx) = spawn_entry_thumb_worker(config.thumb_filter.to_image_filter(), config.resolved_decode_threads(), ctx.clone());
        let (file_cache_req_tx, file_cache_res_rx) = spawn_file_cache_worker(ctx.clone(), file_cache_max);
        let mut drives = list_local_drives();
        drives.extend(list_gvfs_smb_mounts());

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
            tree_at_tab: false,
            drive_cursor: None,
            favorite_cursor: None,
            favorite_at_tab: false,
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
                let db = crate::spread_state::open_spread_db();
                if let Some(db) = &db {
                    crate::favorites::init_favorite_tables(db);
                    // 候補刷新で廃止した空洞・豆腐マーカーを塗り版へ一括移行
                    crate::favorites::migrate_markers(db, FAVORITE_MARKER_MIGRATION);
                }
                db
            },
            spread_states: HashMap::new(),
            favorite_states: HashMap::new(),
            favorite_view_markers: HashMap::new(),
            network_unreachable_mounts: HashSet::new(),
            mount_check_pending: Vec::new(),
            thumbnails: HashMap::new(),
            thumb_req_tx,
            thumb_res_rx,
            thumb_pending: HashSet::new(),
            entry_thumb_req_tx,
            entry_thumb_res_rx,
            viewer: Arc::new(Mutex::new(None)),
            viewer_cfg: Arc::new(Mutex::new(viewer_cfg)),
            drives,
            page_cache: Arc::new(Mutex::new(PageCache::new(cache_max, cache_min))),
            file_cache: FileCache::new(file_cache_max),
            file_cache_req_tx,
            file_cache_res_rx,
            file_cache_pending: HashSet::new(),
            deferred_archive_requests: HashMap::new(),
            req_tx,
            res_rx: Arc::new(Mutex::new(res_rx)),
            pending_loads: Arc::new(Mutex::new(HashSet::new())),
            scan_state: ScanState::Idle,
            tree_scan_pending,
            window_size: (1024, 768),
            viewer_slots,
            raw_image_files: std::collections::HashSet::new(),
            invalid_archives: std::collections::HashSet::new(),
            thumb_failed: std::collections::HashSet::new(),
            app_toast: None,
            cache_budget_bytes: cache_max,
            anim_ring_bounds: ring_bounds,
            memory_warning_open: false,
            settings_open: false,
            settings_tab: SettingsTab::Common,
            settings_draft,
            translate_cfg,
            translate_conn_rx: None,
            translate_conn_status: None,
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
            translate_child_lang_synced_for: None,
            translate_window_autocheck_done_for: None,
            translate_ocr_inflight_key: None,
            translate_ocr_queue: std::collections::VecDeque::new(),
            viewer_focus_requested: false,
            show_hidden,
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
            exif_orientation_enabled_last_seen: viewer_cfg.exif_orientation_enabled,
        };
        app.start_scan();
        app.refresh_favorite_folders();
        app
    }

    /// カレントディレクトリ・ウィンドウ状態・ソート順・言語・ビューア設定・設定ダイアログで
    /// 編集されうる AppConfig 値をまとめて state ファイルへ書き戻す。
    pub(crate) fn persist_state(&self) {
        crate::gui_config::save_state(
            &self.current_dir, self.window_size, &self.viewer_slots,
            &SortState { key: self.sort_key.as_state_key().to_string(), ascending: self.sort_ascending },
            i18n::lang_code(),
            &*self.viewer_cfg.lock().unwrap(),
            self.show_hidden,
            &self.config,
            &self.translate_cfg,
        );
    }
}
