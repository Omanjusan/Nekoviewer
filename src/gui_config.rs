//! GUI設定ダイアログ・セッション状態(state ファイル)まわり。
//! config.rs の AppConfig(ハードコード既定値、起動時に確定)とは異なり、こちらは
//! アプリ実行中に設定ダイアログ/ビューアー操作から書き換えられ、都度 state ファイルへ
//! 永続化される値（ウィンドウ位置・ソート順・言語・ビューア設定・隠しファイル表示・
//! 設定ダイアログ経由の AppConfig 上書き値）を扱う。

use std::path::{Path, PathBuf};

use crate::card_date_format::CardDateFormat;
use crate::config::{AppConfig, ResizeFilter, filter_to_str, parse_filter};
use crate::image_filter::{
    ImageFilterSettings, color_filter_mode_to_str, filter_order_to_str, parse_color_filter_mode,
    parse_filter_order,
};
use crate::toolbar::{BAR_ITEM_COUNT, DEFAULT_BAR_ORDER, ViewerBarItem, bar_order_to_str, parse_bar_order};
use crate::translate::TranslateConfig;
use crate::tool_palette::{PaletteState, PaletteSlotContent, SLOT_COUNT, slot_content_to_id, slot_content_from_id};

// ── State ファイル（動的状態: 最後のディレクトリ・ウィンドウサイズ）────────────

pub struct SortState {
    pub key: String,
    pub ascending: bool,
    /// 第2ソートセット（スコア／訪問回数）
    pub rating: crate::explorer_sort::RatingSort,
}

impl Default for SortState {
    fn default() -> Self {
        Self { key: "name".to_string(), ascending: true, rating: Default::default() }
    }
}

/// アーカイブ内サムネイルバーの配置。ビューアー画面を軸とした表示位置。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThumbbarPos {
    Left,
    Right,
    Top,
    Bottom,
    None,
}

pub(crate) fn parse_thumbbar_pos(s: &str) -> ThumbbarPos {
    match s {
        "left"   => ThumbbarPos::Left,
        "right"  => ThumbbarPos::Right,
        "top"    => ThumbbarPos::Top,
        "bottom" => ThumbbarPos::Bottom,
        _        => ThumbbarPos::None,
    }
}

pub fn thumbbar_pos_to_str(p: ThumbbarPos) -> &'static str {
    match p {
        ThumbbarPos::Left   => "left",
        ThumbbarPos::Right  => "right",
        ThumbbarPos::Top    => "top",
        ThumbbarPos::Bottom => "bottom",
        ThumbbarPos::None   => "none",
    }
}

/// スライドショー用トランジション種類（設定ダイアログのスライドショータブから選択）。
/// 実際の描画切り替えは別フェーズで各PageMode描画パスに接続する。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionKind {
    /// トランジション無し。即時切り替え（アニメーションをスキップする）
    None,
    /// 既存の見開き位置スライド（デフォルト、従来からのページ送り演出）
    HorizontalSlide,
    /// 旧ページ→新ページのクロスフェード
    CrossFade,
    /// 時計回りワイプ（境界フェザー付き）
    ClockwiseWipe,
}

/// トランジション遷移時間(ms)の下限/上限。設定ダイアログのスライダーもこの範囲。
pub const TRANSITION_DURATION_FLOOR_MS: u64 = 100;
pub const TRANSITION_DURATION_CEILING_MS: u64 = 1000;

pub(crate) fn parse_transition_kind(s: &str) -> TransitionKind {
    match s {
        "none"           => TransitionKind::None,
        "cross_fade"     => TransitionKind::CrossFade,
        "clockwise_wipe" => TransitionKind::ClockwiseWipe,
        _                => TransitionKind::HorizontalSlide,
    }
}

pub fn transition_kind_to_str(k: TransitionKind) -> &'static str {
    match k {
        TransitionKind::None            => "none",
        TransitionKind::HorizontalSlide => "horizontal_slide",
        TransitionKind::CrossFade       => "cross_fade",
        TransitionKind::ClockwiseWipe   => "clockwise_wipe",
    }
}

/// スライドショー送り間隔(ms)の下限/上限。設定ダイアログのスライダーもこの範囲。
pub const SLIDESHOW_INTERVAL_FLOOR_MS: u64 = 1000;
pub const SLIDESHOW_INTERVAL_CEILING_MS: u64 = 60000;

/// スライドショー中にユーザーが手動でページ送りした場合の挙動。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlideshowManualBehavior {
    /// タイマーをリセットしてスライドショーを継続する
    ResetTimer,
    /// 手動操作の時点でスライドショーを停止する
    Stop,
}

pub(crate) fn parse_slideshow_manual_behavior(s: &str) -> SlideshowManualBehavior {
    match s {
        "stop" => SlideshowManualBehavior::Stop,
        _      => SlideshowManualBehavior::ResetTimer,
    }
}

pub fn slideshow_manual_behavior_to_str(b: SlideshowManualBehavior) -> &'static str {
    match b {
        SlideshowManualBehavior::ResetTimer => "reset_timer",
        SlideshowManualBehavior::Stop        => "stop",
    }
}

/// ファイルをまたいで維持するビューア設定（ウィンドウを開き直しても保持）
/// tool_paletteがカスタム名称(String)を持つためCopyは実装できない（Cloneのみ）。
#[derive(Clone)]
pub struct ViewerConfig {
    /// true = 1:1等倍表示、false = ウィンドウフィット
    pub zoom_actual: bool,
    /// フルスクリーン状態
    pub fullscreen: bool,
    /// フェーズ6: ウィンドウリサイズ/zoom_actual切替時に元データから再デコードするか
    pub redecode_on_resize: bool,
    /// フェーズ6: リサイズ→再デコードまでのデバウンス時間(ms)。100刻みで100〜1000をループ
    pub resize_debounce_ms: u64,
    /// フェーズ6: リサイズ/zoom_actual切替のたびに増分する世代カウンタ（非永続・実行時のみ）。
    /// winit_app.rs / view_reader.rs から更新され、NekoviewApp 側で変化検知にのみ使う。
    pub redecode_trigger_seq: u64,
    /// アーカイブ内サムネイルバーの配置。単一ファイル/1ファイル格納アーカイブでは
    /// この設定に関わらず非表示にする（呼び出し側で判定）。
    pub thumbbar_pos: ThumbbarPos,
    /// サムネイル1枚の長辺サイズ（px）
    pub thumbbar_thumb_size: u32,
    /// ページ操作停滞後、サムネバーを自動で消すまでの待機時間(ms)。0 = 常時表示
    pub thumbbar_idle_hide_ms: u64,
    /// true = サムネバーを本画像の前面にオーバーレイ表示（本画像はサムネバー領域を意識せず描画）
    pub thumbbar_overlap: bool,
    /// 現在地マーカーの色(R)
    pub thumbbar_marker_r: u8,
    /// 現在地マーカーの色(G)
    pub thumbbar_marker_g: u8,
    /// 現在地マーカーの色(B)
    pub thumbbar_marker_b: u8,
    /// 現在地マーカーの不透明度(0〜100%)
    pub thumbbar_marker_a: u8,
    /// 手動回転角度の引き継ぎトグル。true = ページ送りをまたいで回転角度を維持する。
    /// 非永続・実行時のみ（アプリセッション中は保持、再起動でリセット）。
    pub rotation_carry_over: bool,
    /// rotation_carry_over が true のとき、全ページ共通で使う回転角度(度)。
    /// 非永続・実行時のみ。
    pub rotation_session_angle: i32,
    /// 項目(D): Exif Orientation自動回転(A)の適用有無。true = 従来通り自動回転を適用。
    /// false = デコード時のOrientation適用をスキップする（誤ったOrientationタグ対策）。
    /// 永続設定（save_state/load_state対象）。ビューアーのみに効き、サムネイルには影響しない。
    pub exif_orientation_enabled: bool,
    /// 画像情報（解像度・ページ数）の右下オーバーレイを表示するか。既定ON。
    /// 永続設定。ツールパレットのトグル「画像情報表示」で切り替える。
    pub image_info_visible: bool,
    /// アーカイブ末尾の評価オーバーレイ（スコアリング）を出すか。既定ON。
    /// 永続設定。エクスプローラーのメニューバーのボタンで切り替える。OFFでも訪問回数の記録・
    /// サムネの評価帯・score filter・保存済みの評価値は変わらない。
    pub rating_overlay_enabled: bool,
    /// ビューアーツールバーの項目並び順（全項目の順列、toolbar.rs 参照）。
    /// 永続設定。現時点で編集UIは無く実質固定（state を直接編集すれば並べ替え可能）。
    pub bar_order: [ViewerBarItem; BAR_ITEM_COUNT],
    /// 通常時（スライドショー実行中以外）のトランジション種類。永続設定
    /// （設定ダイアログのスライドショータブで編集）。
    pub transition_kind: TransitionKind,
    /// 通常時のトランジション遷移時間(ms)。永続設定。TRANSITION_DURATION_FLOOR_MS〜CEILING_MSの範囲。
    pub transition_duration_ms: u64,
    /// スライドショー送り間隔(ms)。永続設定。SLIDESHOW_INTERVAL_FLOOR_MS〜CEILING_MSの範囲。
    pub slideshow_interval_ms: u64,
    /// スライドショー中の手動ページ送り時の挙動。永続設定。
    pub slideshow_manual_behavior: SlideshowManualBehavior,
    /// スライドショー実行中のトランジション種類。永続設定。通常時(transition_kind)とは
    /// 独立して選べる（例: 通常時クロスフェード／スライドショー時横スライド）。
    pub slideshow_transition_kind: TransitionKind,
    /// スライドショー実行中のトランジション遷移時間(ms)。永続設定。
    /// TRANSITION_DURATION_FLOOR_MS〜CEILING_MSの範囲（通常時と同じ範囲を共用）。
    pub slideshow_transition_duration_ms: u64,
    /// 画像処理フィルター（ブルーライトカット／セピア／モノクロ／ガンマ／ブライトネス／
    /// シャープネス）の設定一式。永続設定。設定画面・ツールバー等の複数導線から同じ値を書き換える。
    pub image_filter: ImageFilterSettings,
    /// ビューアー内ツールパレット（オーバーレイ）の座標・LOCK・透過度・マスサイズ・
    /// 可視性・マス内容。永続設定。ViewerState側の実行時キャッシュから500msデバウンスで反映される。
    pub tool_palette: PaletteState,
    /// 虫眼鏡（ホイール拡縮）モードのON/OFF。非永続・実行時のみ（ツールパレットのマスで切替）。
    pub magnifier_on: bool,
    /// 疑似コマ送りモードのON/OFF。非永続・実行時のみ（ツールパレットのマスで切替）。
    /// 虫眼鏡モードの上のサブモードなので、虫眼鏡がOFFの間は常にOFF。
    pub koma_on: bool,
    /// 虫眼鏡モードの設定値（上限倍率・バー幅など）。現状は既定値のみで永続化しない。
    pub magnifier: crate::magnifier::MagnifierConfig,
    /// GPUテクスチャを保持するページ窓のVRAM予算など。非永続（現状は既定値のみ）。
    pub texture_window: crate::texture_window::TextureWindowConfig,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            zoom_actual: false,
            fullscreen: false,
            redecode_on_resize: true,
            resize_debounce_ms: 300,
            redecode_trigger_seq: 0,
            thumbbar_pos: ThumbbarPos::None,
            thumbbar_thumb_size: 96,
            thumbbar_idle_hide_ms: 3000,
            thumbbar_overlap: false,
            thumbbar_marker_r: 230,
            thumbbar_marker_g: 169,
            thumbbar_marker_b: 79,
            thumbbar_marker_a: 35,
            rotation_carry_over: false,
            rotation_session_angle: 0,
            exif_orientation_enabled: true,
            image_info_visible: true,
            rating_overlay_enabled: true,
            bar_order: DEFAULT_BAR_ORDER,
            transition_kind: TransitionKind::HorizontalSlide,
            transition_duration_ms: 400,
            slideshow_interval_ms: 5000,
            slideshow_manual_behavior: SlideshowManualBehavior::ResetTimer,
            slideshow_transition_kind: TransitionKind::HorizontalSlide,
            slideshow_transition_duration_ms: 1000,
            image_filter: ImageFilterSettings::default(),
            tool_palette: PaletteState::default(),
            magnifier_on: false,
            koma_on: false,
            magnifier: crate::magnifier::MagnifierConfig::default(),
            texture_window: crate::texture_window::TextureWindowConfig::default(),
        }
    }
}

/// resize_debounce_ms サイクルボタンの次の値を返す（100刻み、100〜1000をループ）。
pub fn next_debounce_ms(current: u64) -> u64 {
    if current >= 1000 { 100 } else { current + 100 }
}

/// ビューアウィンドウの位置・サイズスロット（論理ピクセル）
#[derive(Clone, Copy)]
pub struct WindowSlot {
    /// outer_rect の左上 x 座標
    pub x: i32,
    /// outer_rect の左上 y 座標
    pub y: i32,
    /// inner_rect の幅（コンテンツ領域）
    pub w: u32,
    /// inner_rect の高さ（コンテンツ領域）
    pub h: u32,
}

/// お気に入りタブが最後に開いていた選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoritePosition {
    /// 未整理のお気に入り
    Unsorted,
    /// 定義済みお気に入りフォルダ（id）
    Folder(u8),
}

impl FavoritePosition {
    fn to_state_str(self) -> String {
        match self {
            Self::Unsorted => "unsorted".to_string(),
            Self::Folder(id) => format!("folder:{id}"),
        }
    }

    fn from_state_str(s: &str) -> Option<Self> {
        match s.trim() {
            "unsorted" => Some(Self::Unsorted),
            v => v.strip_prefix("folder:")?.trim().parse().ok().map(Self::Folder),
        }
    }
}

/// 最後に選んでいた左ペインのタブ。起動時にこのタブを開く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedTab {
    RealTree,
    Favorites,
    Search,
    VirtualFolders,
}

impl SavedTab {
    fn to_state_str(self) -> &'static str {
        match self {
            Self::RealTree => "real",
            Self::Favorites => "favorites",
            Self::Search => "search",
            Self::VirtualFolders => "virtual",
        }
    }

    fn from_state_str(s: &str) -> Option<Self> {
        match s.trim() {
            "real" => Some(Self::RealTree),
            "favorites" => Some(Self::Favorites),
            "search" => Some(Self::Search),
            "virtual" => Some(Self::VirtualFolders),
            _ => None,
        }
    }
}

/// 仮想フォルダタブが最後に開いていたノード。idの再利用で別のノードを復元しないよう、
/// 保存時の実パスも一緒に持ち、復元時に照合する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualPosition {
    pub id: u32,
    pub path: PathBuf,
}

/// フォルダ系タブのうち、お気に入り・検索・仮想フォルダが最後にいた位置。
/// 実ツリータブの位置は従来どおり `AppState::last_dir`。未保存（None）や、復元時に検証で外れた
/// 場合は、各タブの既定の位置になる（お気に入り=未整理、検索=実ツリータブの現在地、仮想=`/`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabPositions {
    /// 最後に選んでいたタブ（未保存=実ツリータブで起動）
    pub active: Option<SavedTab>,
    pub favorites: Option<FavoritePosition>,
    /// ユーザーが選んだ検索対象フォルダ（既定のPWDは保存しない）
    pub search_dir: Option<PathBuf>,
    pub virtual_node: Option<VirtualPosition>,
}

impl TabPositions {
    /// state ファイルに書く `tab_*` の行（Some のものだけ）。
    fn state_lines(&self) -> String {
        let mut out = String::new();
        if let Some(t) = self.active {
            out.push_str(&format!("tab_active={}\n", t.to_state_str()));
        }
        if let Some(f) = self.favorites {
            out.push_str(&format!("tab_favorites={}\n", f.to_state_str()));
        }
        if let Some(dir) = &self.search_dir {
            out.push_str(&format!("tab_search_dir={}\n", dir.to_string_lossy()));
        }
        if let Some(v) = &self.virtual_node {
            out.push_str(&format!("tab_virtual_node={}\ntab_virtual_path={}\n", v.id, v.path.to_string_lossy()));
        }
        out
    }
}

pub struct AppState {
    pub last_dir: Option<PathBuf>,
    /// (width, height) in logical pixels
    pub window_size: Option<(u32, u32)>,
    /// ビューアウィンドウの位置・サイズスロット（F5〜F8 対応）
    pub viewer_slots: [Option<WindowSlot>; 4],
    pub sort_state: SortState,
    /// UI言語コード: "ja" / "en" / "cn"
    pub lang: String,
    /// ファイル切替後も維持するビューア設定
    pub viewer_cfg: ViewerConfig,
    /// 隠しファイル/フォルダを一覧に表示するか（設定ダイアログの共通タブで編集）
    pub show_hidden: bool,
    /// サムネカード下部の情報帯モード: "off" / "name" / "name_date" / "name_date_size"。
    /// メニューバーの1ボタン循環トグルで切り替え、値は CardInfoMode 側で解釈する。
    pub card_info_mode: String,
    /// サムネカード下段の評価帯モード（info2）: "off" / "stars" / "visits" / "stars_visits"。
    /// メニューバーの1ボタン循環トグルで切り替え、値は CardRatingMode 側で解釈する。
    pub card_rating_mode: String,
    /// サムネカード情報帯の「更新日時」表示に使う日付書式。設定ダイアログの
    /// エクスプローラータブで編集し、state には card_date_* の6キーに分割して保存する。
    pub card_date_format: CardDateFormat,
    /// 設定ダイアログから編集された AppConfig 上書き値。
    /// None のものは config.rs のハードコード既定値をそのまま使う。一度でもダイアログで
    /// 変更するとこの state 側の値が以後その既定値より優先される（次回起動反映）。
    pub app_cache_total_mb: Option<u64>,
    pub app_anim_ring_min_frames: Option<usize>,
    pub app_anim_ring_max_frames: Option<usize>,
    pub app_anim_frame_hard_limit_mb: Option<usize>,
    pub app_viewer_filter: Option<ResizeFilter>,
    pub app_max_decode_edge: Option<u32>,
    /// デバッグタブで編集するログ設定。None は未設定（ハードコード既定=false を使う）。
    pub app_log_perf: Option<bool>,
    pub app_log_key: Option<bool>,
    pub app_log_common: Option<bool>,
    /// その他タブで編集する起動時フォルダ設定。
    pub app_startup_use_last_dir: Option<bool>,
    pub app_startup_fixed_dir: Option<PathBuf>,
    /// フェーズ4a: thumb_size/thumb_filterもconfig.ini廃止に伴いこちらへ移行。
    pub app_thumb_filter: Option<ResizeFilter>,
    pub app_thumb_size: Option<u32>,
    /// フェーズ4b: decode_threads/default_slotもGUI編集可能にしこちらへ統合。
    pub app_decode_threads: Option<usize>,
    /// 虫眼鏡の拡大縮小の割り当てを知らせ済みか（1度だけ表示するための記録）。
    pub app_magnifier_zoom_notice_shown: Option<bool>,
    /// `max_decode_edge` の既定値底上げ（1920→4000）の確認を済ませたか。
    pub app_max_decode_edge_prompt_answered: Option<bool>,
    /// 外側Noneはキー未記載（ハードコード既定値を使う）、内側Noneはユーザーが明示的に選んだ「なし」。
    pub app_default_slot: Option<Option<usize>>,
    /// 翻訳機能(実験的)の接続先・オーバーレイ設定。
    pub translate_cfg: TranslateConfig,
    /// お気に入り・検索・仮想フォルダの各タブが最後にいた位置。
    pub tab_positions: TabPositions,
    /// ツリー（仮想・実）の並び条件。位置と違い、`use_last_dir` の影響を受けない。
    pub tree_sorts: crate::tree_sort::TreeSorts,
    /// タグ機能・レイアウト器: 左ツリーパネルを折りたたんでいるか。
    pub tree_panel_collapsed: bool,
    /// タグ機能・レイアウト器: 右タグパネルを開いているか。
    pub tag_panel_open: bool,
    /// タグ機能・レイアウト器: 右タグパネルの幅（開いている時、D&Dリサイズ対象）。
    pub tag_panel_width: f32,
    /// タグ機能・レイアウト器: タグパネルの編集モード（上側ツマミでさらに中央側へ展開）。
    pub tag_panel_edit_expanded: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_dir: None,
            window_size: None,
            viewer_slots: [None; 4],
            sort_state: SortState::default(),
            lang: "ja".to_string(),
            viewer_cfg: ViewerConfig::default(),
            show_hidden: false,
            card_info_mode: "off".to_string(),
            card_rating_mode: "off".to_string(),
            card_date_format: CardDateFormat::default(),
            app_cache_total_mb: None,
            app_anim_ring_min_frames: None,
            app_anim_ring_max_frames: None,
            app_anim_frame_hard_limit_mb: None,
            app_viewer_filter: None,
            app_max_decode_edge: None,
            app_log_perf: None,
            app_log_key: None,
            app_log_common: None,
            app_startup_use_last_dir: None,
            app_startup_fixed_dir: None,
            app_thumb_filter: None,
            app_thumb_size: None,
            app_decode_threads: None,
            app_magnifier_zoom_notice_shown: None,
            app_max_decode_edge_prompt_answered: None,
            app_default_slot: None,
            translate_cfg: TranslateConfig::default(),
            tab_positions: TabPositions::default(),
            tree_sorts: crate::tree_sort::TreeSorts::default(),
            tree_panel_collapsed: false,
            tag_panel_open: false,
            tag_panel_width: 280.0,
            tag_panel_edit_expanded: false,
        }
    }
}

fn state_path(root: &Path) -> PathBuf {
    root.join("nekoviewer.state")
}

fn state_bak_path(root: &Path) -> PathBuf {
    root.join("nekoviewer.state.bak")
}

fn state_tmp_path(root: &Path) -> PathBuf {
    root.join("nekoviewer.state.tmp")
}

/// root（config.rsが解決したconf置き場所）の nekoviewer.state を読み込む。
pub fn load_state(root: &Path) -> AppState {
    if let Some(state) = parse_state_file(&state_path(root)) {
        return state;
    }
    // メインが読めなければ bak を試みる
    if let Some(state) = parse_state_file(&state_bak_path(root)) {
        crate::log_common!("[state] メイン読み込み失敗 → bak から復元");
        return state;
    }
    AppState::default()
}

fn parse_state_file(path: &Path) -> Option<AppState> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut last_dir: Option<PathBuf> = None;
    let mut tab_active: Option<SavedTab> = None;
    let mut tab_favorites: Option<FavoritePosition> = None;
    let mut tab_search_dir: Option<PathBuf> = None;
    let mut tab_virtual_id: Option<u32> = None;
    let mut tab_virtual_path: Option<PathBuf> = None;
    let mut tree_sort_virtual: Option<crate::tree_sort::TreeSort> = None;
    let mut tree_sort_real: Option<crate::tree_sort::TreeSort> = None;
    let mut window_width: Option<u32> = None;
    let mut window_height: Option<u32> = None;
    let mut slot_x: [Option<i32>; 4] = [None; 4];
    let mut slot_y: [Option<i32>; 4] = [None; 4];
    let mut slot_w: [Option<u32>; 4] = [None; 4];
    let mut slot_h: [Option<u32>; 4] = [None; 4];
    let mut sort_key: Option<String> = None;
    let mut sort_ascending: Option<bool> = None;
    let mut rating_sort_key: Option<String> = None;
    let mut rating_sort_ascending: Option<bool> = None;
    let mut lang: Option<String> = None;
    let mut viewer_fullscreen: Option<bool> = None;
    let mut redecode_on_resize: Option<bool> = None;
    let mut show_hidden: Option<bool> = None;
    let mut card_info_mode: Option<String> = None;
    let mut card_rating_mode: Option<String> = None;
    // カード日付書式の6キー（未記載は CardDateFormat::from_state 側で各既定へフォールバック）
    let mut card_date_mode = String::new();
    let mut card_date_auto_style = String::new();
    let mut card_date_order = String::new();
    let mut card_date_sep = String::new();
    let mut card_date_year = String::new();
    let mut card_date_month = String::new();
    let mut resize_debounce_ms: Option<u64> = None;
    let mut app_cache_total_mb: Option<u64> = None;
    let mut app_anim_ring_min_frames: Option<usize> = None;
    let mut app_anim_ring_max_frames: Option<usize> = None;
    let mut app_anim_frame_hard_limit_mb: Option<usize> = None;
    let mut app_viewer_filter: Option<ResizeFilter> = None;
    let mut app_max_decode_edge: Option<u32> = None;
    let mut app_log_perf: Option<bool> = None;
    let mut app_log_key: Option<bool> = None;
    let mut app_log_common: Option<bool> = None;
    let mut app_startup_use_last_dir: Option<bool> = None;
    let mut app_startup_fixed_dir: Option<PathBuf> = None;
    let mut app_thumb_filter: Option<ResizeFilter> = None;
    let mut app_thumb_size: Option<u32> = None;
    let mut app_decode_threads: Option<usize> = None;
    let mut app_magnifier_zoom_notice_shown: Option<bool> = None;
    let mut app_max_decode_edge_prompt_answered: Option<bool> = None;
    let mut app_default_slot: Option<Option<usize>> = None;
    let mut thumbbar_pos: Option<ThumbbarPos> = None;
    let mut thumbbar_thumb_size: Option<u32> = None;
    let mut thumbbar_idle_hide_ms: Option<u64> = None;
    let mut thumbbar_overlap: Option<bool> = None;
    let mut thumbbar_marker_r: Option<u8> = None;
    let mut thumbbar_marker_g: Option<u8> = None;
    let mut thumbbar_marker_b: Option<u8> = None;
    let mut thumbbar_marker_a: Option<u8> = None;
    let mut exif_orientation_enabled: Option<bool> = None;
    let mut image_info_visible: Option<bool> = None;
    let mut rating_overlay_enabled: Option<bool> = None;
    let mut magnifier_notch_step: Option<crate::magnifier::NotchStep> = None;
    let mut magnifier_detail_ticks: Option<bool> = None;
    let mut magnifier_koma_height_rel: Option<f32> = None;
    let mut magnifier_koma_shrink_x: Option<bool> = None;
    let mut magnifier_koma_shrink_y: Option<bool> = None;
    let mut magnifier_koma_shrink_x_pct: Option<u32> = None;
    let mut magnifier_koma_shrink_y_pct: Option<u32> = None;
    let mut magnifier_koma_shrink_hide_ask: Option<bool> = None;
    let mut viewer_bar_order: Option<[ViewerBarItem; BAR_ITEM_COUNT]> = None;
    let mut transition_kind: Option<TransitionKind> = None;
    let mut transition_duration_ms: Option<u64> = None;
    let mut slideshow_interval_ms: Option<u64> = None;
    let mut slideshow_manual_behavior: Option<SlideshowManualBehavior> = None;
    let mut slideshow_transition_kind: Option<TransitionKind> = None;
    let mut slideshow_transition_duration_ms: Option<u64> = None;
    let mut translate_base_url: Option<String> = None;
    // 旧キー(単一モデル)。新キー未設定時にocr_model/translation_modelへ後方互換で引き継ぐ。
    let mut translate_model_legacy: Option<String> = None;
    let mut translate_ocr_model: Option<String> = None;
    let mut translate_translation_model: Option<String> = None;
    let mut translate_overlay_width: Option<u32> = None;
    let mut image_filter_color_mode: Option<crate::image_filter::ColorFilterMode> = None;
    let mut image_filter_blc_temp_k: Option<u32> = None;
    let mut image_filter_gamma: Option<f32> = None;
    let mut image_filter_gamma_enabled: Option<bool> = None;
    let mut image_filter_brightness: Option<f32> = None;
    let mut image_filter_brightness_enabled: Option<bool> = None;
    let mut image_filter_sharpness: Option<f32> = None;
    let mut image_filter_sharpness_enabled: Option<bool> = None;
    let mut image_filter_order: Option<[crate::image_filter::FilterStage; crate::image_filter::FILTER_STAGE_COUNT]> = None;
    let mut tool_palette_pos_x: Option<f32> = None;
    let mut tool_palette_pos_y: Option<f32> = None;
    let mut tool_palette_locked: Option<bool> = None;
    let mut tool_palette_auto_hide_locked: Option<bool> = None;
    let mut tool_palette_opacity_pct: Option<u8> = None;
    let mut tool_palette_visible: Option<bool> = None;
    let mut tool_palette_slot_size_idx: Option<usize> = None;
    let mut tool_palette_slots: Option<[PaletteSlotContent; SLOT_COUNT]> = None;
    // マス毎のカスタム名称。キー無し = None（デフォルトラベルを使う）。空文字は「明示的に空欄」。
    let mut tool_palette_labels: [Option<String>; SLOT_COUNT] = [(); SLOT_COUNT].map(|_| None);
    let mut has_kv = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some((k, v)) = line.split_once('=') {
            has_kv = true;
            match k.trim() {
                "last_dir" => {
                    let v = v.trim();
                    if !v.is_empty() { last_dir = Some(PathBuf::from(v)); }
                }
                "tab_active" => { tab_active = SavedTab::from_state_str(v); }
                "tab_favorites" => { tab_favorites = FavoritePosition::from_state_str(v); }
                "tab_search_dir" => {
                    let v = v.trim();
                    if !v.is_empty() { tab_search_dir = Some(PathBuf::from(v)); }
                }
                "tree_sort_virtual" => { tree_sort_virtual = crate::tree_sort::TreeSort::from_state_str(v); }
                // 実ツリーに「登録順」は無いので、それは不正値として捨てる
                "tree_sort_real" => {
                    tree_sort_real = crate::tree_sort::TreeSort::from_state_str(v)
                        .filter(|s| s.key != crate::tree_sort::TreeSortKey::Registration);
                }
                "tab_virtual_node" => { tab_virtual_id = v.trim().parse().ok(); }
                "tab_virtual_path" => {
                    let v = v.trim();
                    if !v.is_empty() { tab_virtual_path = Some(PathBuf::from(v)); }
                }
                "window_width"  => { window_width  = v.trim().parse().ok(); }
                "window_height" => { window_height = v.trim().parse().ok(); }
                "slot1_x" => { slot_x[0] = v.trim().parse().ok(); }
                "slot1_y" => { slot_y[0] = v.trim().parse().ok(); }
                "slot1_w" => { slot_w[0] = v.trim().parse().ok(); }
                "slot1_h" => { slot_h[0] = v.trim().parse().ok(); }
                "slot2_x" => { slot_x[1] = v.trim().parse().ok(); }
                "slot2_y" => { slot_y[1] = v.trim().parse().ok(); }
                "slot2_w" => { slot_w[1] = v.trim().parse().ok(); }
                "slot2_h" => { slot_h[1] = v.trim().parse().ok(); }
                "slot3_x" => { slot_x[2] = v.trim().parse().ok(); }
                "slot3_y" => { slot_y[2] = v.trim().parse().ok(); }
                "slot3_w" => { slot_w[2] = v.trim().parse().ok(); }
                "slot3_h" => { slot_h[2] = v.trim().parse().ok(); }
                "slot4_x" => { slot_x[3] = v.trim().parse().ok(); }
                "slot4_y" => { slot_y[3] = v.trim().parse().ok(); }
                "slot4_w" => { slot_w[3] = v.trim().parse().ok(); }
                "slot4_h" => { slot_h[3] = v.trim().parse().ok(); }
                "sort_key" => {
                    let v = v.trim();
                    if matches!(v, "name" | "date" | "size") {
                        sort_key = Some(v.to_string());
                    }
                }
                "sort_ascending" => { sort_ascending = v.trim().parse().ok(); }
                "rating_sort_key" => { rating_sort_key = Some(v.trim().to_string()); }
                "rating_sort_ascending" => { rating_sort_ascending = v.trim().parse().ok(); }
                "lang" => {
                    let v = v.trim();
                    if matches!(v, "ja" | "en" | "cn") {
                        lang = Some(v.to_string());
                    }
                }
                "viewer_fullscreen" => { viewer_fullscreen = v.trim().parse().ok(); }
                "redecode_on_resize" => { redecode_on_resize = v.trim().parse().ok(); }
                "show_hidden" => { show_hidden = v.trim().parse().ok(); }
                "card_info_mode" => { card_info_mode = Some(v.trim().to_string()); }
                "card_rating_mode" => { card_rating_mode = Some(v.trim().to_string()); }
                "card_date_mode" => { card_date_mode = v.trim().to_string(); }
                "card_date_auto_style" => { card_date_auto_style = v.trim().to_string(); }
                "card_date_order" => { card_date_order = v.trim().to_string(); }
                "card_date_sep" => { card_date_sep = v.trim().to_string(); }
                "card_date_year" => { card_date_year = v.trim().to_string(); }
                "card_date_month" => { card_date_month = v.trim().to_string(); }
                "resize_debounce_ms" => {
                    resize_debounce_ms = v.trim().parse::<u64>().ok()
                        .filter(|n| (100..=1000).contains(n) && n % 100 == 0);
                }
                "app_cache_total_mb" => { app_cache_total_mb = v.trim().parse().ok(); }
                "app_anim_ring_min_frames" => { app_anim_ring_min_frames = v.trim().parse().ok(); }
                "app_anim_ring_max_frames" => { app_anim_ring_max_frames = v.trim().parse().ok(); }
                "app_anim_frame_hard_limit_mb" => { app_anim_frame_hard_limit_mb = v.trim().parse().ok(); }
                "app_viewer_filter" => {
                    let v = v.trim();
                    if !v.is_empty() { app_viewer_filter = Some(parse_filter(v)); }
                }
                "app_max_decode_edge" => { app_max_decode_edge = v.trim().parse().ok(); }
                "app_log_perf" => { app_log_perf = v.trim().parse().ok(); }
                "app_log_key" => { app_log_key = v.trim().parse().ok(); }
                "app_log_common" => { app_log_common = v.trim().parse().ok(); }
                "app_startup_use_last_dir" => { app_startup_use_last_dir = v.trim().parse().ok(); }
                "app_startup_fixed_dir" => {
                    let v = v.trim();
                    if !v.is_empty() { app_startup_fixed_dir = Some(PathBuf::from(v)); }
                }
                "app_thumb_filter" => {
                    let v = v.trim();
                    if !v.is_empty() { app_thumb_filter = Some(parse_filter(v)); }
                }
                "app_thumb_size" => { app_thumb_size = v.trim().parse().ok(); }
                "app_decode_threads" => { app_decode_threads = v.trim().parse().ok(); }
                "app_magnifier_zoom_notice_shown" => { app_magnifier_zoom_notice_shown = v.trim().parse().ok(); }
                "app_max_decode_edge_prompt_answered" => { app_max_decode_edge_prompt_answered = v.trim().parse().ok(); }
                "app_default_slot" => {
                    app_default_slot = Some(match v.trim() {
                        "5" => Some(0),
                        "6" => Some(1),
                        "7" => Some(2),
                        "8" => Some(3),
                        _   => None,
                    });
                }
                "thumbbar_pos" => { thumbbar_pos = Some(parse_thumbbar_pos(v.trim())); }
                "thumbbar_thumb_size" => { thumbbar_thumb_size = v.trim().parse().ok(); }
                "thumbbar_idle_hide_ms" => { thumbbar_idle_hide_ms = v.trim().parse().ok(); }
                "thumbbar_overlap" => { thumbbar_overlap = v.trim().parse().ok(); }
                "thumbbar_marker_r" => { thumbbar_marker_r = v.trim().parse().ok(); }
                "thumbbar_marker_g" => { thumbbar_marker_g = v.trim().parse().ok(); }
                "thumbbar_marker_b" => { thumbbar_marker_b = v.trim().parse().ok(); }
                "thumbbar_marker_a" => { thumbbar_marker_a = v.trim().parse().ok(); }
                "exif_orientation_enabled" => { exif_orientation_enabled = v.trim().parse().ok(); }
                "image_info_visible" => { image_info_visible = v.trim().parse().ok(); }
                "rating_overlay_enabled" => { rating_overlay_enabled = v.trim().parse().ok(); }
                "magnifier_notch_step" => { magnifier_notch_step = crate::magnifier::NotchStep::from_state_str(v); }
                "magnifier_detail_ticks" => { magnifier_detail_ticks = v.trim().parse().ok(); }
                "magnifier_koma_height_rel" => { magnifier_koma_height_rel = v.trim().parse().ok(); }
                "magnifier_koma_shrink_x" => { magnifier_koma_shrink_x = v.trim().parse().ok(); }
                "magnifier_koma_shrink_y" => { magnifier_koma_shrink_y = v.trim().parse().ok(); }
                "magnifier_koma_shrink_x_pct" => { magnifier_koma_shrink_x_pct = v.trim().parse().ok(); }
                "magnifier_koma_shrink_y_pct" => { magnifier_koma_shrink_y_pct = v.trim().parse().ok(); }
                "magnifier_koma_shrink_hide_ask" => { magnifier_koma_shrink_hide_ask = v.trim().parse().ok(); }
                "viewer_bar_order" => { viewer_bar_order = Some(parse_bar_order(v)); }
                "transition_kind" => { transition_kind = Some(parse_transition_kind(v.trim())); }
                "transition_duration_ms" => {
                    transition_duration_ms = v.trim().parse::<u64>().ok()
                        .map(|n| n.clamp(TRANSITION_DURATION_FLOOR_MS, TRANSITION_DURATION_CEILING_MS));
                }
                "slideshow_interval_ms" => {
                    slideshow_interval_ms = v.trim().parse::<u64>().ok()
                        .map(|n| n.clamp(SLIDESHOW_INTERVAL_FLOOR_MS, SLIDESHOW_INTERVAL_CEILING_MS));
                }
                "slideshow_manual_behavior" => {
                    slideshow_manual_behavior = Some(parse_slideshow_manual_behavior(v.trim()));
                }
                "slideshow_transition_kind" => { slideshow_transition_kind = Some(parse_transition_kind(v.trim())); }
                "slideshow_transition_duration_ms" => {
                    slideshow_transition_duration_ms = v.trim().parse::<u64>().ok()
                        .map(|n| n.clamp(TRANSITION_DURATION_FLOOR_MS, TRANSITION_DURATION_CEILING_MS));
                }
                "translate_base_url" => {
                    let v = v.trim();
                    if !v.is_empty() { translate_base_url = Some(v.to_string()); }
                }
                "translate_model" => {
                    let v = v.trim();
                    if !v.is_empty() { translate_model_legacy = Some(v.to_string()); }
                }
                "translate_ocr_model" => {
                    let v = v.trim();
                    if !v.is_empty() { translate_ocr_model = Some(v.to_string()); }
                }
                "translate_translation_model" => {
                    let v = v.trim();
                    if !v.is_empty() { translate_translation_model = Some(v.to_string()); }
                }
                "translate_overlay_width" => {
                    translate_overlay_width = v.trim().parse::<u32>().ok()
                        .map(|n| n.clamp(crate::translate::OVERLAY_WIDTH_FLOOR, crate::translate::OVERLAY_WIDTH_CEILING));
                }
                // "translate_overlay_corner"は廃止済み(EXPERIMENTAL配置オプション撤去)。
                // 旧state ファイルに残っていても単に無視される。
                "image_filter_color_mode" => {
                    image_filter_color_mode = Some(parse_color_filter_mode(v.trim()));
                }
                "image_filter_blc_temp_k" => {
                    image_filter_blc_temp_k = v.trim().parse::<u32>().ok()
                        .map(|n| n.clamp(crate::image_filter::BLC_TEMP_FLOOR_K, crate::image_filter::BLC_TEMP_CEILING_K));
                }
                "image_filter_gamma" => {
                    image_filter_gamma = v.trim().parse::<f32>().ok()
                        .map(|n| n.clamp(crate::image_filter::GAMMA_FLOOR, crate::image_filter::GAMMA_CEILING));
                }
                "image_filter_brightness" => {
                    image_filter_brightness = v.trim().parse::<f32>().ok()
                        .map(|n| n.clamp(crate::image_filter::BRIGHTNESS_FLOOR, crate::image_filter::BRIGHTNESS_CEILING));
                }
                "image_filter_sharpness" => {
                    image_filter_sharpness = v.trim().parse::<f32>().ok()
                        .map(|n| n.clamp(crate::image_filter::SHARPNESS_FLOOR, crate::image_filter::SHARPNESS_CEILING));
                }
                "image_filter_order" => { image_filter_order = Some(parse_filter_order(v)); }
                "image_filter_gamma_enabled" => { image_filter_gamma_enabled = v.trim().parse().ok(); }
                "image_filter_brightness_enabled" => { image_filter_brightness_enabled = v.trim().parse().ok(); }
                "image_filter_sharpness_enabled" => { image_filter_sharpness_enabled = v.trim().parse().ok(); }
                "tool_palette_pos_x" => { tool_palette_pos_x = v.trim().parse().ok(); }
                "tool_palette_pos_y" => { tool_palette_pos_y = v.trim().parse().ok(); }
                "tool_palette_locked" => { tool_palette_locked = v.trim().parse().ok(); }
                "tool_palette_auto_hide_locked" => { tool_palette_auto_hide_locked = v.trim().parse().ok(); }
                "tool_palette_opacity_pct" => {
                    tool_palette_opacity_pct = v.trim().parse::<u8>().ok()
                        .map(|n| n.clamp(crate::tool_palette::OPACITY_FLOOR_PCT, crate::tool_palette::OPACITY_CEILING_PCT));
                }
                "tool_palette_visible" => { tool_palette_visible = v.trim().parse().ok(); }
                "tool_palette_slot_size_idx" => {
                    tool_palette_slot_size_idx = v.trim().parse::<usize>().ok()
                        .map(|n| n.min(crate::tool_palette::SLOT_SIZE_STEPS_PX.len() - 1));
                }
                "tool_palette_slots" => {
                    // 前方互換: 未知IDはEmpty扱い、要素が足りない/多い場合はSLOT_COUNT基準で埋める/切り捨てる。
                    let parsed: Vec<PaletteSlotContent> = v.split(',').map(slot_content_from_id).collect();
                    let mut slots = [PaletteSlotContent::Empty; SLOT_COUNT];
                    for (i, s) in parsed.into_iter().take(SLOT_COUNT).enumerate() {
                        slots[i] = s;
                    }
                    tool_palette_slots = Some(slots);
                }
                k if k.starts_with("tool_palette_label_") => {
                    if let Ok(idx) = k["tool_palette_label_".len()..].parse::<usize>()
                        && idx < SLOT_COUNT
                    {
                        tool_palette_labels[idx] = Some(v.trim().to_string());
                    }
                }
                _ => {}
            }
        }
    }

    // 旧フォーマット互換: key=value がなければ全体をパスとして扱う
    if !has_kv {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            last_dir = Some(PathBuf::from(trimmed));
        }
    }

    let window_size = match (window_width, window_height) {
        (Some(w), Some(h)) if w >= 200 && h >= 150 => Some((w, h)),
        _ => None,
    };

    let mut viewer_slots: [Option<WindowSlot>; 4] = [None; 4];
    for i in 0..4 {
        if let (Some(x), Some(y), Some(w), Some(h)) = (slot_x[i], slot_y[i], slot_w[i], slot_h[i]) {
            if w >= 100 && h >= 100 {
                viewer_slots[i] = Some(WindowSlot { x, y, w, h });
            }
        }
    }

    let sort_state = SortState {
        key: sort_key.unwrap_or_else(|| "name".to_string()),
        ascending: sort_ascending.unwrap_or(true),
        rating: crate::explorer_sort::RatingSort::from_state(rating_sort_key.as_deref(), rating_sort_ascending),
    };

    Some(AppState {
        last_dir,
        window_size,
        viewer_slots,
        sort_state,
        lang: lang.unwrap_or_else(|| "ja".to_string()),
        viewer_cfg: ViewerConfig {
            // 起動時は常にフィット表示から始める（原寸表示は前回終了時の状態を引き継がない）。
            zoom_actual: false,
            fullscreen: viewer_fullscreen.unwrap_or(false),
            redecode_on_resize: redecode_on_resize.unwrap_or(true),
            resize_debounce_ms: resize_debounce_ms.unwrap_or(300),
            redecode_trigger_seq: 0,
            thumbbar_pos: thumbbar_pos.unwrap_or(ThumbbarPos::None),
            thumbbar_thumb_size: thumbbar_thumb_size.unwrap_or(96),
            thumbbar_idle_hide_ms: thumbbar_idle_hide_ms.unwrap_or(3000),
            thumbbar_overlap: thumbbar_overlap.unwrap_or(false),
            thumbbar_marker_r: thumbbar_marker_r.unwrap_or(230),
            thumbbar_marker_g: thumbbar_marker_g.unwrap_or(169),
            thumbbar_marker_b: thumbbar_marker_b.unwrap_or(79),
            thumbbar_marker_a: thumbbar_marker_a.unwrap_or(35),
            rotation_carry_over: false,
            rotation_session_angle: 0,
            exif_orientation_enabled: exif_orientation_enabled.unwrap_or(true),
            image_info_visible: image_info_visible.unwrap_or(true),
            rating_overlay_enabled: rating_overlay_enabled.unwrap_or(true),
            bar_order: viewer_bar_order.unwrap_or(DEFAULT_BAR_ORDER),
            transition_kind: transition_kind.unwrap_or(TransitionKind::HorizontalSlide),
            transition_duration_ms: transition_duration_ms.unwrap_or(400),
            slideshow_interval_ms: slideshow_interval_ms.unwrap_or(5000),
            slideshow_manual_behavior: slideshow_manual_behavior.unwrap_or(SlideshowManualBehavior::ResetTimer),
            slideshow_transition_kind: slideshow_transition_kind.unwrap_or(TransitionKind::HorizontalSlide),
            slideshow_transition_duration_ms: slideshow_transition_duration_ms.unwrap_or(1000),
            image_filter: ImageFilterSettings {
                color_filter_mode: image_filter_color_mode.unwrap_or(crate::image_filter::ColorFilterMode::None),
                blc_color_temperature_k: image_filter_blc_temp_k.unwrap_or(crate::image_filter::BLC_TEMP_DEFAULT_K),
                gamma: image_filter_gamma.unwrap_or(crate::image_filter::GAMMA_DEFAULT),
                gamma_enabled: image_filter_gamma_enabled.unwrap_or(true),
                brightness: image_filter_brightness.unwrap_or(crate::image_filter::BRIGHTNESS_DEFAULT),
                brightness_enabled: image_filter_brightness_enabled.unwrap_or(true),
                sharpness: image_filter_sharpness.unwrap_or(crate::image_filter::SHARPNESS_DEFAULT),
                sharpness_enabled: image_filter_sharpness_enabled.unwrap_or(true),
                filter_order: image_filter_order.unwrap_or(crate::image_filter::DEFAULT_FILTER_ORDER),
            },
            tool_palette: {
                let default = PaletteState::default();
                PaletteState {
                    pos: (tool_palette_pos_x.unwrap_or(default.pos.0), tool_palette_pos_y.unwrap_or(default.pos.1)),
                    locked: tool_palette_locked.unwrap_or(default.locked),
                    auto_hide_locked: tool_palette_auto_hide_locked.unwrap_or(default.auto_hide_locked),
                    opacity_pct: tool_palette_opacity_pct.unwrap_or(default.opacity_pct),
                    visible: tool_palette_visible.unwrap_or(default.visible),
                    slot_size_idx: tool_palette_slot_size_idx.unwrap_or(default.slot_size_idx),
                    slots: tool_palette_slots.unwrap_or(default.slots),
                    custom_labels: tool_palette_labels,
                }
            },
            magnifier_on: false,
            koma_on: false,
            magnifier: {
                // 不正値は捨てて既定値。永続化するのはノッチ倍率・目盛りの詳細/簡易・コマ送りの基準倍率だけ。
                let mut m = crate::magnifier::MagnifierConfig::default();
                if let Some(step) = magnifier_notch_step { m.set_notch_step(step); }
                if let Some(detail) = magnifier_detail_ticks { m.set_detail_ticks(detail); }
                if let Some(rel) = magnifier_koma_height_rel { m.set_koma_height_rel(rel); }
                if let Some(on) = magnifier_koma_shrink_x { m.set_koma_shrink_x(on); }
                if let Some(on) = magnifier_koma_shrink_y { m.set_koma_shrink_y(on); }
                if let Some(pct) = magnifier_koma_shrink_x_pct { m.set_koma_shrink_x_pct(pct); }
                if let Some(pct) = magnifier_koma_shrink_y_pct { m.set_koma_shrink_y_pct(pct); }
                if let Some(hide) = magnifier_koma_shrink_hide_ask { m.set_koma_shrink_hide_ask(hide); }
                m
            },
            texture_window: crate::texture_window::TextureWindowConfig::default(),
        },
        show_hidden: show_hidden.unwrap_or(false),
        card_info_mode: card_info_mode.unwrap_or_else(|| "off".to_string()),
        card_rating_mode: card_rating_mode.unwrap_or_else(|| "off".to_string()),
        card_date_format: CardDateFormat::from_state(
            &card_date_mode,
            &card_date_auto_style,
            &card_date_order,
            &card_date_sep,
            &card_date_year,
            &card_date_month,
        ),
        app_cache_total_mb,
        app_anim_ring_min_frames,
        app_anim_ring_max_frames,
        app_anim_frame_hard_limit_mb,
        app_viewer_filter,
        app_max_decode_edge,
        app_log_perf,
        app_log_key,
        app_log_common,
        app_startup_use_last_dir,
        app_startup_fixed_dir,
        app_thumb_filter,
        app_thumb_size,
        app_decode_threads,
        app_magnifier_zoom_notice_shown,
        app_max_decode_edge_prompt_answered,
        app_default_slot,
        translate_cfg: TranslateConfig {
            base_url: translate_base_url.unwrap_or_default(),
            translation_model: translate_translation_model.clone().or_else(|| translate_model_legacy.clone()).unwrap_or_default(),
            ocr_model: translate_ocr_model.or(translate_translation_model).or(translate_model_legacy).unwrap_or_default(),
            overlay_width: translate_overlay_width.unwrap_or(360),
        },
        tab_positions: TabPositions {
            active: tab_active,
            favorites: tab_favorites,
            search_dir: tab_search_dir,
            // idと実パスの両方がそろっているときだけ有効（片方だけなら照合できないので破棄）
            virtual_node: match (tab_virtual_id, tab_virtual_path) {
                (Some(id), Some(path)) => Some(VirtualPosition { id, path }),
                _ => None,
            },
        },
        tree_sorts: crate::tree_sort::TreeSorts { virtual_tree: tree_sort_virtual, real_tree: tree_sort_real },
        // タグ機能・レイアウト器: フェーズ0時点では永続化未配線、既定値のみ
        tree_panel_collapsed: false,
        tag_panel_open: false,
        tag_panel_width: 280.0,
        tag_panel_edit_expanded: false,
    })
}

#[allow(clippy::too_many_arguments)]
/// 虫眼鏡・コマ送りの永続設定の state 行（`key=value\n` の連なり）。
fn magnifier_state_lines(m: &crate::magnifier::MagnifierConfig) -> String {
    let mut lines = format!(
        "magnifier_notch_step={}\nmagnifier_detail_ticks={}\n",
        m.notch_step().to_state_str(),
        m.detail_ticks(),
    );
    if let Some(rel) = m.koma_height_rel() {
        lines.push_str(&format!("magnifier_koma_height_rel={rel}\n"));
    }
    lines.push_str(&format!(
        "magnifier_koma_shrink_x={}\nmagnifier_koma_shrink_y={}\nmagnifier_koma_shrink_x_pct={}\nmagnifier_koma_shrink_y_pct={}\nmagnifier_koma_shrink_hide_ask={}\n",
        m.koma_shrink_x(), m.koma_shrink_y(), m.koma_shrink_x_pct(), m.koma_shrink_y_pct(), m.koma_shrink_hide_ask(),
    ));
    lines
}

pub fn save_state(root: &Path, dir: &Path, window_size: (u32, u32), viewer_slots: &[Option<WindowSlot>; 4], sort_state: &SortState, lang: &str, viewer_cfg: &ViewerConfig, show_hidden: bool, card_info_mode: &str, card_rating_mode: &str, card_date_format: &CardDateFormat, app_cfg: &AppConfig, translate_cfg: &TranslateConfig, tab_positions: &TabPositions, tree_sorts: &crate::tree_sort::TreeSorts) {
    let _ = std::fs::create_dir_all(root);
    let (path, bak, tmp) = (state_path(root), state_bak_path(root), state_tmp_path(root));

    let mut content = format!(
        "last_dir={}\nwindow_width={}\nwindow_height={}\nsort_key={}\nsort_ascending={}\nlang={}\nviewer_zoom={}\nviewer_fullscreen={}\nredecode_on_resize={}\nresize_debounce_ms={}\nshow_hidden={}\ncard_info_mode={}\ncard_rating_mode={}\nrating_sort_key={}\nrating_sort_ascending={}\n",
        dir.to_string_lossy(), window_size.0, window_size.1, sort_state.key, sort_state.ascending, lang,
        viewer_cfg.zoom_actual, viewer_cfg.fullscreen,
        viewer_cfg.redecode_on_resize, viewer_cfg.resize_debounce_ms, show_hidden, card_info_mode, card_rating_mode,
        sort_state.rating.state_key(), sort_state.rating.ascending,
    );
    // フォルダ系タブ（お気に入り・検索・仮想フォルダ）の最後の位置（Some のものだけ）
    content.push_str(&tab_positions.state_lines());
    // ツリーの並び条件（Some のものだけ）
    content.push_str(&tree_sorts.state_lines());
    // カード日付書式: 可読性優先で6キーに分割。auto_style 未指定は空文字で書く（＝言語追従）。
    content.push_str(&format!(
        "card_date_mode={}\ncard_date_auto_style={}\ncard_date_order={}\ncard_date_sep={}\ncard_date_year={}\ncard_date_month={}\n",
        card_date_format.mode_str(), card_date_format.auto_style_str(), card_date_format.order_str(),
        card_date_format.sep_str(), card_date_format.year_str(), card_date_format.month_str(),
    ));
    content.push_str(&format!(
        "thumbbar_pos={}\nthumbbar_thumb_size={}\nthumbbar_idle_hide_ms={}\nthumbbar_overlap={}\nthumbbar_marker_r={}\nthumbbar_marker_g={}\nthumbbar_marker_b={}\nthumbbar_marker_a={}\n",
        thumbbar_pos_to_str(viewer_cfg.thumbbar_pos), viewer_cfg.thumbbar_thumb_size, viewer_cfg.thumbbar_idle_hide_ms,
        viewer_cfg.thumbbar_overlap, viewer_cfg.thumbbar_marker_r, viewer_cfg.thumbbar_marker_g,
        viewer_cfg.thumbbar_marker_b, viewer_cfg.thumbbar_marker_a,
    ));
    content.push_str(&format!(
        "exif_orientation_enabled={}\n",
        viewer_cfg.exif_orientation_enabled,
    ));
    content.push_str(&format!(
        "image_info_visible={}\n",
        viewer_cfg.image_info_visible,
    ));
    content.push_str(&format!(
        "rating_overlay_enabled={}\n",
        viewer_cfg.rating_overlay_enabled,
    ));
    content.push_str(&magnifier_state_lines(&viewer_cfg.magnifier));
    content.push_str(&format!(
        "viewer_bar_order={}\n",
        bar_order_to_str(&viewer_cfg.bar_order),
    ));
    content.push_str(&format!(
        "transition_kind={}\ntransition_duration_ms={}\n",
        transition_kind_to_str(viewer_cfg.transition_kind), viewer_cfg.transition_duration_ms,
    ));
    content.push_str(&format!(
        "slideshow_interval_ms={}\nslideshow_manual_behavior={}\n",
        viewer_cfg.slideshow_interval_ms, slideshow_manual_behavior_to_str(viewer_cfg.slideshow_manual_behavior),
    ));
    content.push_str(&format!(
        "slideshow_transition_kind={}\nslideshow_transition_duration_ms={}\n",
        transition_kind_to_str(viewer_cfg.slideshow_transition_kind), viewer_cfg.slideshow_transition_duration_ms,
    ));
    content.push_str(&format!(
        "translate_base_url={}\ntranslate_ocr_model={}\ntranslate_translation_model={}\ntranslate_overlay_width={}\n",
        translate_cfg.base_url, translate_cfg.ocr_model, translate_cfg.translation_model, translate_cfg.overlay_width,
    ));
    content.push_str(&format!(
        "image_filter_color_mode={}\nimage_filter_blc_temp_k={}\nimage_filter_gamma={}\nimage_filter_brightness={}\nimage_filter_sharpness={}\nimage_filter_order={}\n",
        color_filter_mode_to_str(viewer_cfg.image_filter.color_filter_mode),
        viewer_cfg.image_filter.blc_color_temperature_k,
        viewer_cfg.image_filter.gamma,
        viewer_cfg.image_filter.brightness,
        viewer_cfg.image_filter.sharpness,
        filter_order_to_str(&viewer_cfg.image_filter.filter_order),
    ));
    content.push_str(&format!(
        "image_filter_gamma_enabled={}\nimage_filter_brightness_enabled={}\nimage_filter_sharpness_enabled={}\n",
        viewer_cfg.image_filter.gamma_enabled,
        viewer_cfg.image_filter.brightness_enabled,
        viewer_cfg.image_filter.sharpness_enabled,
    ));
    content.push_str(&format!(
        "tool_palette_pos_x={}\ntool_palette_pos_y={}\ntool_palette_locked={}\ntool_palette_auto_hide_locked={}\ntool_palette_opacity_pct={}\ntool_palette_visible={}\ntool_palette_slot_size_idx={}\ntool_palette_slots={}\n",
        viewer_cfg.tool_palette.pos.0,
        viewer_cfg.tool_palette.pos.1,
        viewer_cfg.tool_palette.locked,
        viewer_cfg.tool_palette.auto_hide_locked,
        viewer_cfg.tool_palette.opacity_pct,
        viewer_cfg.tool_palette.visible,
        viewer_cfg.tool_palette.slot_size_idx,
        viewer_cfg.tool_palette.slots.iter().map(|s| slot_content_to_id(*s)).collect::<Vec<_>>().join(","),
    ));
    // マス毎のカスタム名称。キー無し = デフォルトラベルを使う（Noneのマスは書かない）。
    for (i, label) in viewer_cfg.tool_palette.custom_labels.iter().enumerate() {
        if let Some(label) = label {
            content.push_str(&format!("tool_palette_label_{i}={label}\n"));
        }
    }
    // 設定ダイアログ（共通/アニメタブ）が編集する AppConfig 系の値。次回起動から反映されるため、
    // ここでは現在の有効値をそのまま state に書き戻すだけでよい（即時のワーカー再構築は不要）。
    content.push_str(&format!(
        "app_cache_total_mb={}\napp_anim_ring_min_frames={}\napp_anim_ring_max_frames={}\napp_anim_frame_hard_limit_mb={}\napp_viewer_filter={}\napp_max_decode_edge={}\n",
        app_cfg.cache_total_mb.map(|v| v.to_string()).unwrap_or_default(),
        app_cfg.anim_ring_min_frames,
        app_cfg.anim_ring_max_frames,
        app_cfg.anim_frame_hard_limit_mb,
        filter_to_str(app_cfg.viewer_filter),
        app_cfg.max_decode_edge,
    ));
    // デバッグタブが編集するログ設定。現在有効な値（グローバルなconfig::log()）をそのまま書き戻す。
    let log_cfg = crate::config::log();
    content.push_str(&format!(
        "app_log_perf={}\napp_log_key={}\napp_log_common={}\n",
        log_cfg.perf, log_cfg.key, log_cfg.common,
    ));
    // その他タブが編集する起動時フォルダ設定。
    content.push_str(&format!(
        "app_startup_use_last_dir={}\napp_startup_fixed_dir={}\n",
        app_cfg.startup.use_last_dir,
        app_cfg.startup.fixed_dir.as_deref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
    ));
    // フェーズ4a: thumb_size/thumb_filter（旧config.ini直接保存分）もここへ統合。
    content.push_str(&format!(
        "app_thumb_filter={}\napp_thumb_size={}\n",
        filter_to_str(app_cfg.thumb_filter), app_cfg.thumb_size,
    ));
    // フェーズ4b: decode_threads/default_slotもここへ統合。
    let default_slot_str = match app_cfg.default_slot {
        Some(0) => "5", Some(1) => "6", Some(2) => "7", Some(3) => "8", _ => "",
    };
    content.push_str(&format!(
        "app_decode_threads={}\napp_default_slot={}\napp_magnifier_zoom_notice_shown={}\napp_max_decode_edge_prompt_answered={}\n",
        app_cfg.decode_threads, default_slot_str, app_cfg.magnifier_zoom_notice_shown, app_cfg.max_decode_edge_prompt_answered,
    ));
    for (i, slot) in viewer_slots.iter().enumerate() {
        if let Some(s) = slot {
            content.push_str(&format!(
                "slot{n}_x={x}\nslot{n}_y={y}\nslot{n}_w={w}\nslot{n}_h={h}\n",
                n = i + 1, x = s.x, y = s.y, w = s.w, h = s.h
            ));
        }
    }

    // アトミック書き込み: tmp に書いてから rename
    if std::fs::write(&tmp, &content).is_err() { return; }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // 書き込み成功を確認してから bak に同内容をミラー
    let _ = std::fs::write(&bak, &content);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト専用の一時ディレクトリに content を state として書き、パースして返す。
    fn parse_state_text(tag: &str, content: &str) -> AppState {
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_tabpos_{}_{tag}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), content).unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        let _ = std::fs::remove_dir_all(&root);
        parsed
    }

    #[test]
    fn max_decode_edge_prompt_flag_parses_and_defaults_to_unset() {
        let parsed = parse_state_text("edge_set", "app_max_decode_edge_prompt_answered=true\napp_max_decode_edge=1920\n");
        assert_eq!(parsed.app_max_decode_edge_prompt_answered, Some(true));
        assert_eq!(parsed.app_max_decode_edge, Some(1920));
        let parsed = parse_state_text("edge_bad", "app_max_decode_edge_prompt_answered=maybe\n");
        assert_eq!(parsed.app_max_decode_edge_prompt_answered, None);
        let parsed = parse_state_text("edge_none", "lang=ja\n");
        assert_eq!(parsed.app_max_decode_edge_prompt_answered, None);
    }

    #[test]
    fn magnifier_zoom_notice_flag_parses_and_defaults_to_unset() {
        let parsed = parse_state_text("notice_set", "app_magnifier_zoom_notice_shown=true\n");
        assert_eq!(parsed.app_magnifier_zoom_notice_shown, Some(true));
        let parsed = parse_state_text("notice_bad", "app_magnifier_zoom_notice_shown=yes\n");
        assert_eq!(parsed.app_magnifier_zoom_notice_shown, None);
        let parsed = parse_state_text("notice_none", "lang=ja\n");
        assert_eq!(parsed.app_magnifier_zoom_notice_shown, None);
    }

    #[test]
    fn image_info_visible_parses_and_defaults_to_on() {
        assert!(ViewerConfig::default().image_info_visible);
        let parsed = parse_state_text("info_off", "image_info_visible=false\n");
        assert!(!parsed.viewer_cfg.image_info_visible);
        let parsed = parse_state_text("info_on", "image_info_visible=true\n");
        assert!(parsed.viewer_cfg.image_info_visible);
        // 不正値・キーなし（旧state）は既定のON。
        let parsed = parse_state_text("info_bad", "image_info_visible=maybe\n");
        assert!(parsed.viewer_cfg.image_info_visible);
        let parsed = parse_state_text("info_none", "lang=ja\n");
        assert!(parsed.viewer_cfg.image_info_visible);
    }

    #[test]
    fn magnifier_keys_parse_and_fall_back_to_defaults() {
        let parsed = parse_state_text("mag_ok", "magnifier_notch_step=1.5\nmagnifier_detail_ticks=true\n");
        assert_eq!(parsed.viewer_cfg.magnifier.notch_step(), crate::magnifier::NotchStep::X1_5);
        assert!(parsed.viewer_cfg.magnifier.detail_ticks());

        // 不正値は捨てて既定値（×1.25・簡易）。
        let parsed = parse_state_text("mag_bad", "magnifier_notch_step=7\nmagnifier_detail_ticks=maybe\n");
        assert_eq!(parsed.viewer_cfg.magnifier.notch_step(), crate::magnifier::NotchStep::X1_25);
        assert!(!parsed.viewer_cfg.magnifier.detail_ticks());

        // キーなし（旧state）も既定値。
        let parsed = parse_state_text("mag_none", "lang=ja\n");
        assert_eq!(parsed.viewer_cfg.magnifier, crate::magnifier::MagnifierConfig::default());
    }

    #[test]
    fn koma_shrink_settings_roundtrip_through_state_lines() {
        let mut m = crate::magnifier::MagnifierConfig::default();
        m.set_koma_shrink_x(true);
        m.set_koma_shrink_x_pct(6);
        m.set_koma_shrink_y_pct(30);
        m.set_koma_shrink_hide_ask(true);
        m.set_koma_height_rel(1.5);
        let parsed = parse_state_text("koma_shrink_rt", &magnifier_state_lines(&m));
        assert_eq!(parsed.viewer_cfg.magnifier, m);
        assert!(parsed.viewer_cfg.magnifier.koma_shrink_x());
        assert!(!parsed.viewer_cfg.magnifier.koma_shrink_y());
        assert_eq!(parsed.viewer_cfg.magnifier.koma_shrink_x_pct(), 6);
        assert_eq!(parsed.viewer_cfg.magnifier.koma_shrink_y_pct(), 30);
        assert!(parsed.viewer_cfg.magnifier.koma_shrink_hide_ask());
        // 既定値のままでも往復する。
        let d = crate::magnifier::MagnifierConfig::default();
        assert_eq!(parse_state_text("koma_shrink_rt_d", &magnifier_state_lines(&d)).viewer_cfg.magnifier, d);
    }

    #[test]
    fn koma_shrink_keys_round_clamp_and_fall_back() {
        let parsed = parse_state_text("shrink_round", "magnifier_koma_shrink_x_pct=7\nmagnifier_koma_shrink_y_pct=999\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_shrink_x_pct(), 8, "刻み(2%)へ丸める");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_shrink_y_pct(), 30, "上限へ丸める");
        let parsed = parse_state_text("shrink_low", "magnifier_koma_shrink_x_pct=0\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_shrink_x_pct(), 2, "下限へ丸める");
        // 不正値・キーなしは既定値（OFF・10%・ダイアログ表示あり）。
        let parsed = parse_state_text("shrink_bad", "magnifier_koma_shrink_x=yes\nmagnifier_koma_shrink_y_pct=-5\nmagnifier_koma_shrink_hide_ask=1\n");
        let m = &parsed.viewer_cfg.magnifier;
        assert!(!m.koma_shrink_x() && !m.koma_shrink_y() && !m.koma_shrink_hide_ask());
        assert_eq!((m.koma_shrink_x_pct(), m.koma_shrink_y_pct()), (10, 10));
    }

    #[test]
    fn koma_height_rel_parses_clamps_and_falls_back() {
        let parsed = parse_state_text("koma_ok", "magnifier_koma_height_rel=1.75\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_height_rel(), Some(1.75));

        // 範囲外は丸め、不正値・キーなしは未設定。
        let parsed = parse_state_text("koma_big", "magnifier_koma_height_rel=9999\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_height_rel(), Some(64.0));
        let parsed = parse_state_text("koma_bad", "magnifier_koma_height_rel=abc\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_height_rel(), None);
        let parsed = parse_state_text("koma_nan", "magnifier_koma_height_rel=NaN\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_height_rel(), None);
        let parsed = parse_state_text("koma_none", "lang=ja\n");
        assert_eq!(parsed.viewer_cfg.magnifier.koma_height_rel(), None);
        assert!(!parsed.viewer_cfg.koma_on);
    }

    #[test]
    fn tab_positions_roundtrip_through_state_text() {
        let tp = TabPositions {
            active: Some(SavedTab::VirtualFolders),
            favorites: Some(FavoritePosition::Folder(3)),
            search_dir: Some(PathBuf::from("/data/search base")),
            virtual_node: Some(VirtualPosition { id: 12, path: PathBuf::from("/data/manga") }),
        };
        let parsed = parse_state_text("roundtrip", &format!("last_dir=/real\n{}", tp.state_lines()));
        assert_eq!(parsed.tab_positions, tp);
        // 実ツリータブの位置（last_dir）は従来どおり別キー
        assert_eq!(parsed.last_dir, Some(PathBuf::from("/real")));
    }

    #[test]
    fn tree_sorts_roundtrip_and_ignore_invalid_values() {
        use crate::tree_sort::{TreeSort, TreeSortKey};
        let sorts = crate::tree_sort::TreeSorts {
            virtual_tree: Some(TreeSort { key: TreeSortKey::Date, ascending: false }),
            real_tree: Some(TreeSort { key: TreeSortKey::Name, ascending: false }),
        };
        let parsed = parse_state_text("tree_sort", &sorts.state_lines());
        assert_eq!(parsed.tree_sorts, sorts);
        assert_eq!(parse_state_text("tree_sort_none", "last_dir=/x\n").tree_sorts, Default::default());
        assert_eq!(parse_state_text("tree_sort_bad", "tree_sort_virtual=size:up\n").tree_sorts, Default::default());
        // 実ツリーに登録順は無い
        assert_eq!(parse_state_text("tree_sort_real_reg", "tree_sort_real=registration:asc\n").tree_sorts, Default::default());
    }

    #[test]
    fn saved_tab_roundtrips_and_ignores_invalid_values() {
        for t in [SavedTab::RealTree, SavedTab::Favorites, SavedTab::Search, SavedTab::VirtualFolders] {
            let tp = TabPositions { active: Some(t), ..Default::default() };
            assert_eq!(parse_state_text("saved_tab", &tp.state_lines()).tab_positions.active, Some(t));
        }
        assert_eq!(TabPositions { active: Some(SavedTab::Search), ..Default::default() }.state_lines(), "tab_active=search\n");
        for bad in ["", "Real", "tree", "virtual_folders", "0"] {
            assert_eq!(SavedTab::from_state_str(bad), None, "{bad:?}");
        }
        // 旧stateにはキーが無い＝未保存（実ツリータブで起動）
        assert_eq!(parse_state_text("saved_tab_old", "last_dir=/x\n").tab_positions.active, None);
    }

    #[test]
    fn favorites_unsorted_roundtrips() {
        let tp = TabPositions { favorites: Some(FavoritePosition::Unsorted), ..Default::default() };
        let parsed = parse_state_text("unsorted", &tp.state_lines());
        assert_eq!(parsed.tab_positions.favorites, Some(FavoritePosition::Unsorted));
    }

    #[test]
    fn state_lines_omits_unset_positions() {
        assert_eq!(TabPositions::default().state_lines(), "");
        let only_search = TabPositions { search_dir: Some(PathBuf::from("/s")), ..Default::default() };
        assert_eq!(only_search.state_lines(), "tab_search_dir=/s\n");
    }

    #[test]
    fn old_state_without_tab_keys_yields_defaults() {
        let parsed = parse_state_text("old", "last_dir=/tmp/x\nlang=ja\n");
        assert_eq!(parsed.tab_positions, TabPositions::default());
    }

    #[test]
    fn malformed_tab_values_are_ignored() {
        let parsed = parse_state_text(
            "malformed",
            "tab_favorites=weird\ntab_search_dir=\ntab_virtual_node=abc\ntab_virtual_path=/x\n",
        );
        assert_eq!(parsed.tab_positions, TabPositions::default());
        // お気に入りフォルダidは u8（範囲外・空・不正はNone）
        for bad in ["folder:999", "folder:", "folder:x", "Unsorted", ""] {
            assert_eq!(FavoritePosition::from_state_str(bad), None, "{bad:?}");
        }
        assert_eq!(FavoritePosition::from_state_str("folder:255"), Some(FavoritePosition::Folder(255)));
    }

    #[test]
    fn virtual_position_needs_both_id_and_path() {
        let only_id = parse_state_text("only_id", "tab_virtual_node=5\n");
        assert_eq!(only_id.tab_positions.virtual_node, None);
        let only_path = parse_state_text("only_path", "tab_virtual_path=/p\n");
        assert_eq!(only_path.tab_positions.virtual_node, None);
    }

    #[test]
    fn tab_keys_coexist_with_unknown_keys() {
        // 将来のキーや古いバージョンが書いたキーがあっても、tab_* は読める（未知キーは無視される）
        let parsed = parse_state_text(
            "unknown",
            "future_key=1\ntab_favorites=folder:2\nanother_future=xyz\ntab_search_dir=/s\n",
        );
        assert_eq!(parsed.tab_positions.favorites, Some(FavoritePosition::Folder(2)));
        assert_eq!(parsed.tab_positions.search_dir, Some(PathBuf::from("/s")));
    }

    #[test]
    fn viewer_defaults_to_window_size_following() {
        let config = ViewerConfig::default();
        assert!(!config.zoom_actual);
        assert!(config.redecode_on_resize);
    }

    #[test]
    fn rating_overlay_enabled_key_parses_and_defaults_to_on() {
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_scoring_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\nrating_overlay_enabled=false\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert!(!parsed.viewer_cfg.rating_overlay_enabled);

        // 旧stateにキーがなければ ON（従来どおりオーバーレイを出す）
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert!(parsed.viewer_cfg.rating_overlay_enabled);
        // 壊れた値も ON へ戻る
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\nrating_overlay_enabled=bogus\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert!(parsed.viewer_cfg.rating_overlay_enabled);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn card_rating_mode_key_round_trips_and_defaults_to_off() {
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_rating_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\ncard_rating_mode=stars_visits\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.card_rating_mode, "stars_visits");

        // 旧stateにキーがなければ off
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.card_rating_mode, "off");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rating_sort_keys_round_trip_and_default_to_off_descending() {
        use crate::explorer_sort::{RatingSort, RatingSortKey};

        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_rating_sort_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\nrating_sort_key=visits\nrating_sort_ascending=true\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.sort_state.rating, RatingSort { key: Some(RatingSortKey::Visits), ascending: true });

        // 旧stateにキーがなければ OFF・降順
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.sort_state.rating, RatingSort::default());

        // 未知値・壊れた向きも OFF・降順
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\nrating_sort_key=bogus\nrating_sort_ascending=zzz\n").unwrap();
        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.sort_state.rating, RatingSort::default());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn card_date_keys_parse_into_format() {
        use crate::card_date_format::{AutoStyle, CardDateFormat, CardDateMode};

        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        // save_state が書き出すのと同じ6キー名で custom 書式を復元できること。
        std::fs::write(
            state_path(&root),
            "last_dir=/tmp/x\nlang=ja\n\
             card_date_mode=custom\ncard_date_auto_style=\ncard_date_order=dmy\n\
             card_date_sep=dot\ncard_date_year=y2\ncard_date_month=en\n",
        )
        .unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert_eq!(parsed.card_date_format.mode, CardDateMode::Custom);
        assert_eq!(parsed.card_date_format.auto_style, None);
        assert_eq!(
            parsed.card_date_format,
            CardDateFormat::from_state("custom", "", "dmy", "dot", "y2", "en")
        );
        // 参照: auto_style を持つケースも往復する
        assert_eq!(
            AutoStyle::MdySlash,
            CardDateFormat::from_state("auto", "mdy_slash", "", "", "", "")
                .auto_style
                .unwrap()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_card_date_keys_fall_back_to_default() {
        // card_date_* を一切含まない旧 state 断片でも既定（自動）へ寄る。
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_legacy_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\nshow_hidden=false\n").unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("legacy state parses");
        assert_eq!(parsed.card_date_format, CardDateFormat::default());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tool_palette_keys_roundtrip_through_state_file() {
        use crate::tool_palette::{ActionKind, DialogKind, PaletteSlotContent, ToggleKind};

        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_tool_palette_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(
            state_path(&root),
            "last_dir=/tmp/x\nlang=ja\n\
             tool_palette_pos_x=120.5\ntool_palette_pos_y=64\n\
             tool_palette_locked=true\ntool_palette_opacity_pct=40\n\
             tool_palette_visible=false\ntool_palette_slot_size_idx=1\n\
             tool_palette_slots=toggle:blue_light_cut,dialog:image_filter,action:next_page,action:prev_page,empty,empty,empty,empty,empty,empty\n",
        )
        .unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        let tp = parsed.viewer_cfg.tool_palette;
        assert_eq!(tp.pos, (120.5, 64.0));
        assert!(tp.locked);
        assert_eq!(tp.opacity_pct, 40);
        assert!(!tp.visible);
        assert_eq!(tp.slot_size_idx, 1);
        assert_eq!(tp.slots[0], PaletteSlotContent::Toggle(ToggleKind::BlueLightCut));
        assert_eq!(tp.slots[1], PaletteSlotContent::Dialog(DialogKind::ImageFilter));
        assert_eq!(tp.slots[2], PaletteSlotContent::Action(ActionKind::NextPage));
        assert_eq!(tp.slots[3], PaletteSlotContent::Action(ActionKind::PrevPage));
        assert!(tp.slots[4..].iter().all(|s| *s == PaletteSlotContent::Empty));
        // auto_hide_locked未指定時はデフォルト(true=常時表示)にフォールバックする。
        assert!(tp.auto_hide_locked);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tool_palette_auto_hide_locked_roundtrips_through_state_file() {
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_tool_palette_auto_hide_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(
            state_path(&root),
            "last_dir=/tmp/x\nlang=ja\ntool_palette_auto_hide_locked=false\n",
        )
        .unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        assert!(!parsed.viewer_cfg.tool_palette.auto_hide_locked);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tool_palette_labels_roundtrip_through_state_file() {
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_tool_palette_label_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(
            state_path(&root),
            "last_dir=/tmp/x\nlang=ja\n\
             tool_palette_label_0=GC\ntool_palette_label_1=\n",
        )
        .unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("state file parses");
        let labels = parsed.viewer_cfg.tool_palette.custom_labels;
        assert_eq!(labels[0].as_deref(), Some("GC"));
        // 空文字での確定 = 「明示的に空欄」であり、キー自体はNoneと区別する。
        assert_eq!(labels[1].as_deref(), Some(""));
        assert!(labels[2..].iter().all(|l| l.is_none()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_tool_palette_keys_fall_back_to_default() {
        // tool_palette_* を一切含まない旧 state 断片でも既定値（全マス空欄）へ寄る。
        let root = std::env::temp_dir()
            .join(format!("nekoviewer_state_tool_palette_legacy_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(state_path(&root), "last_dir=/tmp/x\nlang=ja\n").unwrap();

        let parsed = parse_state_file(&state_path(&root)).expect("legacy state parses");
        assert_eq!(parsed.viewer_cfg.tool_palette, PaletteState::default());

        let _ = std::fs::remove_dir_all(&root);
    }
}
