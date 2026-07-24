//! GUI設定ダイアログ・セッション状態(state ファイル)まわり。
//! config.rs の AppConfig(config.ini, 起動時読み込み専用)とは異なり、こちらは
//! アプリ実行中に設定ダイアログ/ビューアー操作から書き換えられ、都度 state ファイルへ
//! 永続化される値（ウィンドウ位置・ソート順・言語・ビューア設定・隠しファイル表示・
//! 設定ダイアログ経由の AppConfig 上書き値）を扱う。

use std::path::{Path, PathBuf};

use crate::config::{AppConfig, ResizeFilter, filter_to_str, parse_filter};
use crate::toolbar::{BAR_ITEM_COUNT, DEFAULT_BAR_ORDER, ViewerBarItem, bar_order_to_str, parse_bar_order};
use crate::translate::TranslateConfig;

// ── State ファイル（動的状態: 最後のディレクトリ・ウィンドウサイズ）────────────

pub struct SortState {
    pub key: String,
    pub ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self { key: "name".to_string(), ascending: true }
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

/// ファイルをまたいで維持するビューア設定（ウィンドウを開き直しても保持）
#[derive(Clone, Copy)]
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
    /// ビューアーツールバーの項目並び順（全項目の順列、toolbar.rs 参照）。
    /// 永続設定。現時点で編集UIは無く実質固定（state を直接編集すれば並べ替え可能）。
    pub bar_order: [ViewerBarItem; BAR_ITEM_COUNT],
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            zoom_actual: false,
            fullscreen: false,
            redecode_on_resize: false,
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
            bar_order: DEFAULT_BAR_ORDER,
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
    /// 設定ダイアログ（共通/アニメタブ）から編集された AppConfig 上書き値。
    /// None のものは config.ini の値をそのまま使う。一度でもダイアログで変更すると
    /// この state 側の値が以後 config.ini より優先される（次回起動反映）。
    pub app_cache_total_mb: Option<u64>,
    pub app_anim_ring_min_frames: Option<usize>,
    pub app_anim_ring_max_frames: Option<usize>,
    pub app_anim_frame_hard_limit_mb: Option<usize>,
    pub app_viewer_filter: Option<ResizeFilter>,
    pub app_max_decode_edge: Option<u32>,
    /// 翻訳機能(実験的)の接続先・オーバーレイ設定。
    pub translate_cfg: TranslateConfig,
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
            app_cache_total_mb: None,
            app_anim_ring_min_frames: None,
            app_anim_ring_max_frames: None,
            app_anim_frame_hard_limit_mb: None,
            app_viewer_filter: None,
            app_max_decode_edge: None,
            translate_cfg: TranslateConfig::default(),
        }
    }
}

fn state_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state")))
}

fn state_bak_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state.bak")))
}

fn state_tmp_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("nekoviewer.state.tmp")))
}

pub fn load_state() -> AppState {
    if let Some(path) = state_path() {
        if let Some(state) = parse_state_file(&path) {
            return state;
        }
    }
    // メインが読めなければ bak を試みる
    if let Some(bak) = state_bak_path() {
        if let Some(state) = parse_state_file(&bak) {
            crate::log_common!("[state] メイン読み込み失敗 → bak から復元");
            return state;
        }
    }
    AppState::default()
}

fn parse_state_file(path: &Path) -> Option<AppState> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut last_dir: Option<PathBuf> = None;
    let mut window_width: Option<u32> = None;
    let mut window_height: Option<u32> = None;
    let mut slot_x: [Option<i32>; 4] = [None; 4];
    let mut slot_y: [Option<i32>; 4] = [None; 4];
    let mut slot_w: [Option<u32>; 4] = [None; 4];
    let mut slot_h: [Option<u32>; 4] = [None; 4];
    let mut sort_key: Option<String> = None;
    let mut sort_ascending: Option<bool> = None;
    let mut lang: Option<String> = None;
    let mut viewer_fullscreen: Option<bool> = None;
    let mut redecode_on_resize: Option<bool> = None;
    let mut show_hidden: Option<bool> = None;
    let mut resize_debounce_ms: Option<u64> = None;
    let mut app_cache_total_mb: Option<u64> = None;
    let mut app_anim_ring_min_frames: Option<usize> = None;
    let mut app_anim_ring_max_frames: Option<usize> = None;
    let mut app_anim_frame_hard_limit_mb: Option<usize> = None;
    let mut app_viewer_filter: Option<ResizeFilter> = None;
    let mut app_max_decode_edge: Option<u32> = None;
    let mut thumbbar_pos: Option<ThumbbarPos> = None;
    let mut thumbbar_thumb_size: Option<u32> = None;
    let mut thumbbar_idle_hide_ms: Option<u64> = None;
    let mut thumbbar_overlap: Option<bool> = None;
    let mut thumbbar_marker_r: Option<u8> = None;
    let mut thumbbar_marker_g: Option<u8> = None;
    let mut thumbbar_marker_b: Option<u8> = None;
    let mut thumbbar_marker_a: Option<u8> = None;
    let mut exif_orientation_enabled: Option<bool> = None;
    let mut viewer_bar_order: Option<[ViewerBarItem; BAR_ITEM_COUNT]> = None;
    let mut translate_base_url: Option<String> = None;
    // 旧キー(単一モデル)。新キー未設定時にocr_model/translation_modelへ後方互換で引き継ぐ。
    let mut translate_model_legacy: Option<String> = None;
    let mut translate_ocr_model: Option<String> = None;
    let mut translate_translation_model: Option<String> = None;
    let mut translate_overlay_width: Option<u32> = None;
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
                "lang" => {
                    let v = v.trim();
                    if matches!(v, "ja" | "en" | "cn") {
                        lang = Some(v.to_string());
                    }
                }
                "viewer_fullscreen" => { viewer_fullscreen = v.trim().parse().ok(); }
                "redecode_on_resize" => { redecode_on_resize = v.trim().parse().ok(); }
                "show_hidden" => { show_hidden = v.trim().parse().ok(); }
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
                "thumbbar_pos" => { thumbbar_pos = Some(parse_thumbbar_pos(v.trim())); }
                "thumbbar_thumb_size" => { thumbbar_thumb_size = v.trim().parse().ok(); }
                "thumbbar_idle_hide_ms" => { thumbbar_idle_hide_ms = v.trim().parse().ok(); }
                "thumbbar_overlap" => { thumbbar_overlap = v.trim().parse().ok(); }
                "thumbbar_marker_r" => { thumbbar_marker_r = v.trim().parse().ok(); }
                "thumbbar_marker_g" => { thumbbar_marker_g = v.trim().parse().ok(); }
                "thumbbar_marker_b" => { thumbbar_marker_b = v.trim().parse().ok(); }
                "thumbbar_marker_a" => { thumbbar_marker_a = v.trim().parse().ok(); }
                "exif_orientation_enabled" => { exif_orientation_enabled = v.trim().parse().ok(); }
                "viewer_bar_order" => { viewer_bar_order = Some(parse_bar_order(v)); }
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
            redecode_on_resize: redecode_on_resize.unwrap_or(false),
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
            bar_order: viewer_bar_order.unwrap_or(DEFAULT_BAR_ORDER),
        },
        show_hidden: show_hidden.unwrap_or(false),
        app_cache_total_mb,
        app_anim_ring_min_frames,
        app_anim_ring_max_frames,
        app_anim_frame_hard_limit_mb,
        app_viewer_filter,
        app_max_decode_edge,
        translate_cfg: TranslateConfig {
            base_url: translate_base_url.unwrap_or_default(),
            translation_model: translate_translation_model.clone().or_else(|| translate_model_legacy.clone()).unwrap_or_default(),
            ocr_model: translate_ocr_model.or(translate_translation_model).or(translate_model_legacy).unwrap_or_default(),
            overlay_width: translate_overlay_width.unwrap_or(360),
        },
    })
}

pub fn save_state(dir: &Path, window_size: (u32, u32), viewer_slots: &[Option<WindowSlot>; 4], sort_state: &SortState, lang: &str, viewer_cfg: &ViewerConfig, show_hidden: bool, app_cfg: &AppConfig, translate_cfg: &TranslateConfig) {
    let (Some(path), Some(bak), Some(tmp)) =
        (state_path(), state_bak_path(), state_tmp_path())
    else { return; };

    let mut content = format!(
        "last_dir={}\nwindow_width={}\nwindow_height={}\nsort_key={}\nsort_ascending={}\nlang={}\nviewer_zoom={}\nviewer_fullscreen={}\nredecode_on_resize={}\nresize_debounce_ms={}\nshow_hidden={}\n",
        dir.to_string_lossy(), window_size.0, window_size.1, sort_state.key, sort_state.ascending, lang,
        viewer_cfg.zoom_actual, viewer_cfg.fullscreen,
        viewer_cfg.redecode_on_resize, viewer_cfg.resize_debounce_ms, show_hidden,
    );
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
        "viewer_bar_order={}\n",
        bar_order_to_str(&viewer_cfg.bar_order),
    ));
    content.push_str(&format!(
        "translate_base_url={}\ntranslate_ocr_model={}\ntranslate_translation_model={}\ntranslate_overlay_width={}\n",
        translate_cfg.base_url, translate_cfg.ocr_model, translate_cfg.translation_model, translate_cfg.overlay_width,
    ));
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
