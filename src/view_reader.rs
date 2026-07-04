use crate::gui_config::{ThumbbarPos, ViewerConfig};
use crate::controller::{ViewerNav, ViewerOutput};
use crate::i18n;
use crate::log_key;
use crate::types::ReaderSortKey as ViewerSortKey;
pub use crate::types::{PageMode, ViewerEntry};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::cache::{PageCache, PageContent};
use crate::gui_config::WindowSlot;
use crate::fs::archive;
use crate::spread_offset::SpreadOffset;

const SCROLL_THRESHOLD: f32 = 50.0;
const ANIM_SECS: f32 = 0.4;
const FULL_UV: egui::Rect =
    egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(1.0, 1.0) };

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// `bounds` の中に `img_size` を縦横比を保ったまま収める（contain-fit）矩形を返す。
/// サムネイルバーの正方形枠に、実際のサムネイル画像(縦長/横長)を収めるのに使う。
fn fit_rect_contain(bounds: egui::Rect, img_size: egui::Vec2) -> egui::Rect {
    if img_size.x <= 0.0 || img_size.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width() / img_size.x).min(bounds.height() / img_size.y);
    let size = img_size * scale;
    egui::Rect::from_center_size(bounds.center(), size)
}

/// GIF等アニメーション再生状態（ページごとに保持）
struct AnimState {
    frame_index: usize,
    last_frame_at: Instant,
}

/// show() の先頭で ctx.input を1回だけ呼び、フレーム全体で使い回す入力スナップショット
struct FrameInput {
    // キー入力
    key_left: bool,
    key_right: bool,
    key_up: bool,
    key_down: bool,
    key_space: bool,
    esc: bool,
    zoom_key: bool,
    fs_key: bool,
    mode1: bool,
    mode2: bool,
    mode3: bool,
    shift4: bool,
    shift5: bool,
    shift_nav_up: bool,
    shift_nav_down: bool,
    slot_apply: Option<usize>,
    // スクロール
    scroll_delta: f32,
    shift_scroll_delta: f32,
    // ポインタ
    hover_pos: Option<egui::Pos2>,
    middle_clicked: bool,
    // viewport
    outer_rect: Option<egui::Rect>,
    inner_rect: Option<egui::Rect>,
    monitor_size: Option<egui::Vec2>,
    viewport_rect: egui::Rect,
    // Wayland 専用: OS ネイティブ最大化を擬似フルスクへ合流させる判定に使う。
    // Windows では参照しないため dead_code 警告を抑制する。
    #[cfg_attr(windows, allow(dead_code))]
    os_maximized: bool,
    close_requested: bool,
    // 時刻
    dt: f32,
    time: f64,
}

impl FrameInput {
    fn collect(ctx: &egui::Context, zoom_actual: bool) -> Self {
        ctx.input(|i| {
            let sh = i.modifiers.shift;
            let raw = if zoom_actual { 0.0 } else {
                let sd = i.smooth_scroll_delta();
                sd.y + if sh { sd.x } else { 0.0 }
            };
            let slot_apply =
                if      i.key_pressed(egui::Key::F5) { Some(0) }
                else if i.key_pressed(egui::Key::F6) { Some(1) }
                else if i.key_pressed(egui::Key::F7) { Some(2) }
                else if i.key_pressed(egui::Key::F8) { Some(3) }
                else { None };
            let vp = i.viewport();
            Self {
                key_left:           i.key_pressed(egui::Key::ArrowLeft)  && !sh,
                key_right:          i.key_pressed(egui::Key::ArrowRight) && !sh,
                key_up:             i.key_pressed(egui::Key::ArrowUp)    && !sh,
                key_down:           i.key_pressed(egui::Key::ArrowDown)  && !sh,
                key_space:          i.key_pressed(egui::Key::Space),
                esc:                i.key_pressed(egui::Key::Escape),
                zoom_key:           i.key_pressed(egui::Key::Enter) && !i.modifiers.alt,
                fs_key:             i.key_pressed(egui::Key::Enter) &&  i.modifiers.alt,
                mode1:              i.key_pressed(egui::Key::Num1),
                mode2:              i.key_pressed(egui::Key::Num2),
                mode3:              i.key_pressed(egui::Key::Num3),
                shift4:             i.key_pressed(egui::Key::Num4),
                shift5:             i.key_pressed(egui::Key::Num5),
                shift_nav_up:       i.key_pressed(egui::Key::ArrowUp)   && sh,
                shift_nav_down:     i.key_pressed(egui::Key::ArrowDown) && sh,
                slot_apply,
                scroll_delta:       raw,
                shift_scroll_delta: if sh { raw } else { 0.0 },
                hover_pos:          i.pointer.hover_pos(),
                middle_clicked:     i.pointer.button_clicked(egui::PointerButton::Middle),
                outer_rect:         vp.outer_rect,
                inner_rect:         vp.inner_rect,
                monitor_size:       vp.monitor_size,
                viewport_rect:      i.viewport_rect(),
                os_maximized:       vp.maximized.unwrap_or(false),
                close_requested:    vp.close_requested(),
                dt:                 i.unstable_dt,
                time:               i.time,
            }
        })
    }
}

/// 自然数ソート比較: 数字列は数値として比較、それ以外は文字列として比較
fn nat_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let na = eat_digits(&mut ai);
                    let nb = eat_digits(&mut bi);
                    let ord = na.cmp(&nb);
                    if ord != std::cmp::Ordering::Equal { return ord; }
                } else {
                    ai.next();
                    bi.next();
                    let ord = ac.cmp(&bc);
                    if ord != std::cmp::Ordering::Equal { return ord; }
                }
            }
        }
    }
}

fn eat_digits(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut n = 0u64;
    while let Some(&c) = iter.peek() {
        if c.is_ascii_digit() {
            iter.next();
            n = n.saturating_mul(10).saturating_add(c as u64 - b'0' as u64);
        } else {
            break;
        }
    }
    n
}

struct RenderFrame {
    tex_lo:      Option<egui::TextureHandle>,
    tex_hi:      Option<egui::TextureHandle>,
    prev_tex_lo: Option<egui::TextureHandle>,
    prev_tex_hi: Option<egui::TextureHandle>,
    animating:   bool,
    t:           f32,
    anim_dir_f:  f32,
    page_mode:   PageMode,
    zoom_actual: bool,
    monitor:     Option<egui::Vec2>,
}

pub struct ViewerState {
    archive_path: PathBuf,
    entries: Vec<ViewerEntry>,
    /// 見開き基点ページ（常に偶数。単ページ時はそのままページ番号）
    spread_base: i32,
    /// オフセット状態。spread_lo() = spread_base + offset.value()
    offset: SpreadOffset,
    textures: HashMap<usize, egui::TextureHandle>,
    open: bool,
    page_mode: PageMode,
    scroll_acc: f32,
    /// アニメーション検出用: 前フレームの spread_lo()
    prev_spread_lo: i32,
    /// アニメーション開始時点の旧 spread_lo（退場側テクスチャ取得用）
    anim_from_lo: i32,
    /// +1=新ページが右からIN（左綴じ前進）、-1=左からIN（右綴じ前進）
    anim_dir: i32,
    /// アニメーション進捗 0.0=開始 1.0=完了
    anim_progress: f32,
    anim_active: bool,
    /// ウィンドウ位置・サイズスロット（F5〜F8 で適用、ボタンで保存）
    slots: [Option<WindowSlot>; 4],
    /// conf 由来の既定スロット index（0..3）。None = デフォルト無し
    default_slot: Option<usize>,
    /// 既定スロットの初回フレーム適用を一度だけ行うためのフラグ
    default_slot_applied: bool,
    /// スロット保存後に app 側へ永続化を要求するフラグ
    /// 前フレームの outer_rect 左上座標（保存用、1フレーム遅れ許容）
    outer_pos: Option<egui::Pos2>,
    /// 左エントリリストパネルの表示状態（マウスホバーで on/off）
    entry_list_visible: bool,
    /// フルスクリーン時ソートバーの表示状態（上端ホバーで on/off）
    fs_sort_bar_visible: bool,
    sort_key: ViewerSortKey,
    sort_ascending: bool,
    /// アニメーションページの再生状態（original_index → AnimState）
    anim_states: HashMap<usize, AnimState>,
    /// true のとき生画像ファイルを直接表示中（見開きモード封印）
    is_raw_file: bool,
    /// Shift+スクロールの蓄積値（ファイル間ナビゲーション用）
    shift_scroll_acc: f32,
    /// トーストメッセージ: (テキスト, 消去予定のegui時刻) None=非表示
    toast: Option<(String, Option<f64>)>,
    /// フェーズ6: 直近フレームで観測したウィンドウ描画領域サイズ（物理px）。
    /// リサイズ再デコードのターゲットサイズ算出に使う。
    content_px: (u32, u32),
    /// アーカイブ内サムネイルバー用テクスチャ(original_index → texture)。
    /// メインの textures とは別解像度で保持するため独立させる。
    thumb_textures: HashMap<usize, egui::TextureHandle>,
    /// サムネイル読み込み要求済み・未完了の original_index 集合（重複要求防止）。
    thumb_pending: HashSet<usize>,
    /// サムネイルバー自動非表示用: 直近のページ操作(ナビゲーション入力)時刻。
    thumbbar_last_activity: Instant,
    /// サムネイルバーを最後にセンタリングした spread_lo。ページが実際に変わった
    /// フレームでだけ scroll_to_rect を呼ぶための重複防止フラグ（毎フレーム呼ぶと
    /// クリップ矩形サイズが不安定な瞬間に delta が収束せず request_repaint が
    /// 連打され続ける恐れがあるため）。
    thumbbar_scrolled_lo: Option<i32>,
}

impl ViewerState {
    // ── 読み取り専用アクセサ ─────────────────────────────────────────────────
    pub fn archive_path(&self) -> &PathBuf { &self.archive_path }
    pub fn entries(&self) -> &[ViewerEntry] { &self.entries }
    pub fn is_raw_file(&self) -> bool { self.is_raw_file }
    pub fn page_mode(&self) -> PageMode { self.page_mode }

    /// フェーズ6: 現在表示中のページ(見開き時は2枚)の original_index を返す。
    pub fn visible_original_indices(&self) -> Vec<usize> {
        let lo = self.spread_lo();
        let total = self.entries.len() as i32;
        let hi = if self.page_mode == PageMode::Single { lo } else { lo + 1 };
        (lo..=hi)
            .filter(|&i| i >= 0 && i < total)
            .map(|i| self.entries[i as usize].original_index)
            .collect()
    }

    /// サムネイルバー用: まだテクスチャが無く、要求も出していない original_index 一覧。
    pub fn thumbbar_missing_indices(&self) -> Vec<usize> {
        self.entries.iter()
            .map(|e| e.original_index)
            .filter(|i| !self.thumb_textures.contains_key(i) && !self.thumb_pending.contains(i))
            .collect()
    }

    /// サムネイルバー用: 指定 original_index に対応する entry_name を引く（要求作成用）。
    pub fn entry_name_for(&self, original_index: usize) -> Option<&str> {
        self.entries.iter()
            .find(|e| e.original_index == original_index)
            .map(|e| e.entry_name.as_str())
    }

    /// サムネイルバー用: 要求送信済みとしてマークする（重複送信防止）。
    pub fn mark_thumb_pending(&mut self, original_index: usize) {
        self.thumb_pending.insert(original_index);
    }

    /// サムネイルバー用: ワーカーからの結果を反映する。デコード失敗時(None)もpendingは解除する。
    pub fn set_thumb_result(&mut self, ctx: &egui::Context, original_index: usize, rgba: Option<image::RgbaImage>) {
        self.thumb_pending.remove(&original_index);
        if let Some(img) = rgba {
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            let tex = ctx.load_texture(
                format!("thumb_{original_index}"),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            self.thumb_textures.insert(original_index, tex);
        }
    }

    /// サムネイルバー描画用: original_index に対応するテクスチャ（未取得なら None）。
    pub fn thumb_texture(&self, original_index: usize) -> Option<&egui::TextureHandle> {
        self.thumb_textures.get(&original_index)
    }

    /// フェーズ6: リサイズ/zoom_actual切替後の再デコード先ターゲットサイズ。
    /// zoom_actual時は無制限(原寸)、それ以外は直近の描画領域サイズ(物理px)を上限にする。
    pub fn current_decode_target(&self, zoom_actual: bool) -> Option<(u32, u32)> {
        if zoom_actual { None } else { Some(self.content_px) }
    }

    /// フェーズ6: 再デコード発火時に、指定ページのテクスチャ・アニメ再生状態を破棄する。
    /// 次の update_textures() で PageCache から作り直させる（アニメはフレーム0から再生し直す）。
    pub fn invalidate_pages(&mut self, orig_indices: &[usize]) {
        for orig_i in orig_indices {
            self.textures.remove(orig_i);
            self.anim_states.remove(orig_i);
        }
    }

    pub fn new(archive_path: PathBuf, slots: [Option<WindowSlot>; 4], default_slot: Option<usize>) -> Option<Self> {
        let image_entries = archive::list_images(&archive_path);
        if image_entries.is_empty() {
            return None;
        }
        let entries: Vec<ViewerEntry> = image_entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| ViewerEntry {
                entry_name: e.entry_name,
                display_name: e.display_name,
                date_key: e.date_key,
                original_index: i,
            })
            .collect();
        Some(Self {
            archive_path,
            entries,
            spread_base: 0,
            offset: SpreadOffset::new(),
            textures: HashMap::new(),
            open: true,
            page_mode: PageMode::Single,
            scroll_acc: 0.0,
            prev_spread_lo: 0,
            anim_from_lo: 0,
            anim_dir: 1,
            anim_progress: 1.0,
            anim_active: false,
            slots,
            default_slot,
            default_slot_applied: false,
            outer_pos: None,
            entry_list_visible: false,
            fs_sort_bar_visible: false,
            sort_key: ViewerSortKey::Name,
            sort_ascending: true,
            anim_states: HashMap::new(),
            is_raw_file: false,
            shift_scroll_acc: 0.0,
            toast: None,
            content_px: crate::cache::DEFAULT_DECODE_TARGET,
            thumb_textures: HashMap::new(),
            thumb_pending: HashSet::new(),
            thumbbar_last_activity: Instant::now(),
            thumbbar_scrolled_lo: None,
        })
    }

    /// 生画像ファイル（ZIP非対応・1ファイル固定）用コンストラクタ
    pub fn new_raw(path: PathBuf, slots: [Option<WindowSlot>; 4], default_slot: Option<usize>) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        let entries = vec![ViewerEntry {
            entry_name: String::new(),
            display_name: name,
            date_key: 0,
            original_index: 0,
        }];
        Self {
            archive_path: path,
            entries,
            spread_base: 0,
            offset: SpreadOffset::new(),
            textures: HashMap::new(),
            open: true,
            page_mode: PageMode::Single,
            scroll_acc: 0.0,
            prev_spread_lo: 0,
            anim_from_lo: 0,
            anim_dir: 1,
            anim_progress: 1.0,
            anim_active: false,
            slots,
            default_slot,
            default_slot_applied: false,
            outer_pos: None,
            entry_list_visible: false,
            fs_sort_bar_visible: false,
            sort_key: ViewerSortKey::Name,
            sort_ascending: true,
            anim_states: HashMap::new(),
            is_raw_file: true,
            shift_scroll_acc: 0.0,
            toast: None,
            content_px: crate::cache::DEFAULT_DECODE_TARGET,
            thumb_textures: HashMap::new(),
            thumb_pending: HashSet::new(),
            thumbbar_last_activity: Instant::now(),
            thumbbar_scrolled_lo: None,
        }
    }

    /// トーストメッセージをセット（3秒後に自動消去）
    pub fn set_toast(&mut self, msg: String) {
        self.toast = Some((msg, None));
    }

    /// 現在の表示基点インデックス（spread_base + offset）
    pub fn spread_lo(&self) -> i32 {
        self.spread_base + self.offset.value()
    }

    /// オフセットがずれているか（UI表示用）
    pub fn is_spread_offset(&self) -> bool {
        self.offset.is_nonzero()
    }

    pub fn can_shift_forward(&self) -> bool {
        self.offset.can_advance()
    }

    pub fn can_shift_backward(&self) -> bool {
        self.offset.can_retreat()
    }

    pub fn shift_offset_forward(&mut self) {
        self.offset.advance();
    }

    pub fn shift_offset_backward(&mut self) {
        self.offset.retreat();
    }

    /// ページモードを切り替え、spread_base とオフセットを整合させる
    pub fn set_page_mode(&mut self, mode: PageMode, cfg: &mut ViewerConfig) {
        match mode {
            PageMode::Single => {
                self.page_mode = mode;
                self.spread_base = self.spread_lo().max(0);
                self.offset.reset();
            }
            PageMode::SpreadLeft | PageMode::SpreadRight => {
                // 生ファイル表示中は見開き封印
                if self.is_raw_file { return; }
                if self.page_mode != mode {
                    self.page_mode = mode;
                    cfg.zoom_actual = false;
                    self.spread_base = self.spread_lo().max(0) & !1;
                    self.offset.reset();
                }
            }
        }
    }

    /// spread_lo を基に lo/hi テクスチャを返す（original_index でキャッシュ参照）
    fn page_textures_for(&self, lo: i32) -> (Option<egui::TextureHandle>, Option<egui::TextureHandle>) {
        let total = self.entries.len() as i32;
        let get = |idx: i32| -> Option<egui::TextureHandle> {
            if idx >= 0 && idx < total {
                let orig = self.entries[idx as usize].original_index;
                self.textures.get(&orig).cloned()
            } else {
                None
            }
        };
        (get(lo), get(lo + 1))
    }

    fn sort_entries(&mut self) {
        let asc = self.sort_ascending;
        match self.sort_key {
            ViewerSortKey::Name => {
                self.entries.sort_by(|a, b| {
                    let c = a.display_name.cmp(&b.display_name);
                    if asc { c } else { c.reverse() }
                });
            }
            ViewerSortKey::Natural => {
                self.entries.sort_by(|a, b| {
                    let c = nat_cmp(&a.display_name, &b.display_name);
                    if asc { c } else { c.reverse() }
                });
            }
            ViewerSortKey::Date => {
                self.entries.sort_by(|a, b| {
                    let c = a.date_key.cmp(&b.date_key)
                        .then_with(|| a.display_name.cmp(&b.display_name));
                    if asc { c } else { c.reverse() }
                });
            }
        }
        // ソート変更時は先頭に戻してアニメーションもリセット
        self.spread_base = 0;
        self.offset.reset();
        self.anim_active = false;
        self.anim_progress = 1.0;
        self.prev_spread_lo = 0;
    }

    pub fn title(&self) -> String {
        self.archive_path.file_name().and_then(|n| n.to_str()).unwrap_or(i18n::t().viewer_fallback()).to_string()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, page_cache: &PageCache, cfg: &mut ViewerConfig) -> ViewerOutput {
        let ctx = ui.ctx().clone();
        let viewer_style = ui.style().clone();
        if !self.open || self.entries.is_empty() {
            return ViewerOutput { nav: ViewerNav::None, close_requested: !self.open, save_slots: None };
        }

        // ── フレーム入力を一括収集（ctx.input はこの1回のみ）────────────────
        let input = FrameInput::collect(&ctx, cfg.zoom_actual);

        // フェーズ6: リサイズ再デコードのターゲットサイズ算出用に、現在の描画領域サイズ（物理px）を記録する。
        let screen = ctx.content_rect().size() * ctx.pixels_per_point();
        self.content_px = (screen.x.max(1.0) as u32, screen.y.max(1.0) as u32);

        // 既定スロットを初回フレームで一度だけ適用（クランプ付き）。
        self.apply_default_slot(&ctx, input.monitor_size);

        let (animating, t) = self.update_animation(&ctx, input.dt);

        self.update_textures(&ctx, page_cache);

        let total = self.entries.len();
        let (tex_lo, tex_hi) = self.page_textures_for(self.spread_lo());
        let (prev_tex_lo, prev_tex_hi) = if animating {
            self.page_textures_for(self.anim_from_lo)
        } else {
            (None, None)
        };

        // outer_rect の左上座標を毎フレーム記録（保存ボタン用、1フレーム遅れ許容）
        if let Some(outer) = input.outer_rect {
            self.outer_pos = Some(outer.min);
        }

        // OSネイティブの最大化（タイトルバーあり最大化）を検知したら擬似フルスクに合流する。
        // Wayland は Fullscreen 中に Close を無視する上、本物のFullscreenはGNOME等で
        // 専用ワークスペースへ移動する挙動があり他窓へフォーカスを移すとビューアーが
        // 消えて見える（実験2で確認）。Maximized(true)+Decorations(false)の擬似フルスクに
        // 統一してこれらを避ける。Wayland固有の問題のためWindowsでは行わない。
        #[cfg(not(windows))]
        if input.os_maximized && !cfg.fullscreen {
            cfg.fullscreen = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        }

        // ── 入力読み取り（FrameInput から展開）────────────────────────────────
        let key_left         = input.key_left;
        let key_right        = input.key_right;
        let key_up           = input.key_up;
        let key_down         = input.key_down;
        let key_space        = input.key_space;
        let esc              = input.esc;
        let zoom_key         = input.zoom_key;
        let fs_key           = input.fs_key;
        let mode1            = input.mode1;
        let mode2            = input.mode2;
        let mode3            = input.mode3;
        let shift4           = input.shift4;
        let shift5           = input.shift5;
        let shift_nav_up     = input.shift_nav_up;
        let shift_nav_down   = input.shift_nav_down;
        let scroll_delta_raw = input.scroll_delta;
        let shift_scroll_delta = input.shift_scroll_delta;
        let scroll_delta = scroll_delta_raw;

        // サムネイルバー自動非表示用: ページ送りに関わる入力があった時刻を記録する。
        if key_left || key_right || key_up || key_down || key_space
            || shift_nav_up || shift_nav_down
            || scroll_delta != 0.0 || shift_scroll_delta != 0.0
        {
            self.thumbbar_last_activity = Instant::now();
        }

        if key_left || key_right || key_up || key_down || key_space || esc || zoom_key || fs_key
            || mode1 || mode2 || mode3 || shift4 || shift5
            || shift_nav_up || shift_nav_down
            || scroll_delta != 0.0 || shift_scroll_delta != 0.0
        {
            log_key!(
                "[key] left={} right={} up={} down={} space={} esc={} zoom={} fs={} \
                 mode1={} mode2={} mode3={} shift4={} shift5={} \
                 shift_nav_up={} shift_nav_down={} scroll={:.1} shift_scroll={:.1}",
                key_left, key_right, key_up, key_down, key_space, esc, zoom_key, fs_key,
                mode1, mode2, mode3, shift4, shift5,
                shift_nav_up, shift_nav_down, scroll_delta, shift_scroll_delta
            );
        }

        // ── ページモード切り替え ──────────────────────────────────────────────
        if mode1 { self.set_page_mode(PageMode::Single, cfg); }
        // 生ファイル表示中は見開きキーを無効化（set_page_mode 内でも封印済みだが念のため）
        if !self.is_raw_file {
            if mode2 { self.set_page_mode(PageMode::SpreadLeft, cfg); }
            if mode3 { self.set_page_mode(PageMode::SpreadRight, cfg); }
        }

        let is_spread = self.page_mode != PageMode::Single;
        let step = if is_spread { 2i32 } else { 1i32 };

        // タイトルを独立ウィンドウのタイトルバーに反映
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));

        let save_slots = self.draw_top_bar(ui, &ctx, &input, &viewer_style, cfg);

        // ── 左エントリリスト ──────────────────────────────────────────────────
        self.draw_entry_list(ui, &ctx, &viewer_style, input.hover_pos, input.viewport_rect);

        // ── アーカイブ内サムネイルバー ────────────────────────────────────────
        // 単一ファイル/1ファイル格納アーカイブでは常に非表示（設定に関わらず）。
        // idle_hide_ms > 0 のとき、ページ操作停滞がその時間を超えたら自動的に隠す(0=常時表示)。
        let idle_ms = cfg.thumbbar_idle_hide_ms;
        let idle_elapsed_ms = self.thumbbar_last_activity.elapsed().as_millis() as u64;
        let auto_hidden = idle_ms > 0 && idle_elapsed_ms >= idle_ms;
        if idle_ms > 0 && !auto_hidden {
            // 残り時間ちょうどで再描画させ、入力が無くても自動的に隠れるようにする。
            ctx.request_repaint_after(Duration::from_millis((idle_ms - idle_elapsed_ms).max(1)));
        }
        let show_thumbbar = cfg.thumbbar_pos != ThumbbarPos::None && total > 1 && !auto_hidden;
        if show_thumbbar && !cfg.thumbbar_overlap {
            self.draw_thumbbar_panel(ui, cfg, cfg.thumbbar_pos);
        }
        let viewport_before_central = ui.max_rect();

        let frame = RenderFrame {
            tex_lo, tex_hi, prev_tex_lo, prev_tex_hi,
            animating,
            t,
            anim_dir_f:  self.anim_dir as f32,
            page_mode:   self.page_mode,
            zoom_actual: cfg.zoom_actual,
            monitor:     input.monitor_size,
        };
        let double_clicked = self.draw_central_panel(ui, &frame);

        if show_thumbbar && cfg.thumbbar_overlap {
            self.draw_thumbbar_overlay(ui, cfg, cfg.thumbbar_pos, viewport_before_central);
        }

        let nav = self.process_navigation(&input, is_spread, step, total);

        let close_self = self.process_misc_input(&ctx, &input, is_spread, double_clicked, cfg);

        self.tick_toast(&ctx, input.time);

        ViewerOutput { nav, close_requested: close_self, save_slots }
    }

    /// ビューアーを開いた直後（初回フレーム）に conf 既定スロットを一度だけ適用する。
    /// F5〜F8 と同じく `clamp_slot_position_inner` で画面外補正してから位置・サイズを送る。
    fn apply_default_slot(&mut self, ctx: &egui::Context, monitor_size: Option<egui::Vec2>) {
        if self.default_slot_applied {
            return;
        }
        self.default_slot_applied = true;

        let Some(slot) = crate::controller::resolve_default_slot(self.default_slot, &self.slots)
        else { return };

        let (cx, cy) = if let Some(m) = monitor_size {
            Self::clamp_slot_position_inner(slot.x, slot.y, slot.w, slot.h, m)
        } else {
            (slot.x, slot.y)
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
            egui::pos2(cx as f32, cy as f32),
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            egui::vec2(slot.w as f32, slot.h as f32),
        ));
        log_key!("[slot] apply default → pos=({},{}) size={}x{}", cx, cy, slot.w, slot.h);
    }

    fn update_animation(&mut self, ctx: &egui::Context, dt: f32) -> (bool, f32) {
        let current_lo = self.spread_lo();
        if current_lo != self.prev_spread_lo {
            let delta = current_lo - self.prev_spread_lo;
            self.anim_dir = match self.page_mode {
                PageMode::SpreadRight => if delta > 0 { -1 } else { 1 },
                _                     => if delta > 0 {  1 } else { -1 },
            };
            self.anim_from_lo = self.prev_spread_lo;
            self.anim_progress = 0.0;
            self.anim_active = true;
            self.prev_spread_lo = current_lo;
        }

        if self.anim_active {
            self.anim_progress = (self.anim_progress + dt / ANIM_SECS).min(1.0);
            if self.anim_progress >= 1.0 { self.anim_active = false; }
            ctx.request_repaint();
        }

        (self.anim_active, ease_out(self.anim_progress))
    }

    fn draw_top_bar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        input: &FrameInput,
        style: &egui::Style,
        cfg: &ViewerConfig,
    ) -> Option<[Option<WindowSlot>; 4]> {
        let mut save_slots = None;

        // ── スロット適用（F5〜F8）────────────────────────────────────────────
        if let Some(idx) = input.slot_apply {
            if let Some(slot) = self.slots[idx] {
                let (cx, cy) = if let Some(m) = input.monitor_size {
                    Self::clamp_slot_position_inner(slot.x, slot.y, slot.w, slot.h, m)
                } else {
                    (slot.x, slot.y)
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                    egui::pos2(cx as f32, cy as f32),
                ));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                    egui::vec2(slot.w as f32, slot.h as f32),
                ));
                log_key!("[slot] apply slot{} → pos=({},{}) size={}x{}", idx + 1, cx, cy, slot.w, slot.h);
            }
        }

        // ── メニューバー（フルスクリーン時は非表示）────────────────────────────
        if !cfg.fullscreen {
            egui::Panel::top("slot_bar")
                .frame(egui::Frame::side_top_panel(style))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        self.draw_sort_buttons(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for i in (0..4usize).rev() {
                                let label = i18n::t().slot_label(i + 5);
                                let has_slot = self.slots[i].is_some();
                                if ui.selectable_label(has_slot, &label).clicked() {
                                    if let (Some(pos), Some(inner)) = (self.outer_pos, input.inner_rect) {
                                        self.slots[i] = Some(WindowSlot {
                                            x: pos.x as i32,
                                            y: pos.y as i32,
                                            w: inner.width() as u32,
                                            h: inner.height() as u32,
                                        });
                                        save_slots = Some(self.slots);
                                        log_key!("[slot] save slot{} → pos=({},{}) size={}x{}",
                                            i + 1, pos.x as i32, pos.y as i32,
                                            inner.width() as u32, inner.height() as u32);
                                    }
                                }
                            }
                        });
                    });
                });
        }

        // ── フルスクリーン時ソートバー（上端ホバーでポップアップ）────────────
        if cfg.fullscreen {
            const FS_TRIGGER_H: f32 = 40.0;
            const FS_BAR_H: f32 = 32.0;
            const FS_HIDE_MARGIN: f32 = 10.0;

            let screen_top = input.viewport_rect.min.y;
            if let Some(pos) = input.hover_pos {
                if !self.fs_sort_bar_visible && pos.y < screen_top + FS_TRIGGER_H {
                    self.fs_sort_bar_visible = true;
                    ctx.request_repaint();
                } else if self.fs_sort_bar_visible && pos.y > screen_top + FS_BAR_H + FS_HIDE_MARGIN {
                    self.fs_sort_bar_visible = false;
                    ctx.request_repaint();
                }
            }

            if self.fs_sort_bar_visible {
                egui::Panel::top("fs_sort_bar")
                    .frame(egui::Frame::side_top_panel(style))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| { self.draw_sort_buttons(ui); });
                    });
            }
        }

        save_slots
    }

    fn process_navigation(
        &mut self,
        input: &FrameInput,
        is_spread: bool,
        step: i32,
        total: usize,
    ) -> ViewerNav {
        let key_next = input.key_space || input.key_down || input.shift_nav_down;
        let key_prev = input.key_up || input.shift_nav_up;
        let (shift_dec, shift_inc) = match self.page_mode {
            PageMode::SpreadRight => (input.shift5, input.shift4),
            _                     => (input.shift4, input.shift5),
        };

        // ── ファイル間ナビゲーション（Shift+↑↓ or Shift+スクロール）────────────
        self.shift_scroll_acc += input.shift_scroll_delta;
        let shift_scroll_prev = self.shift_scroll_acc >  SCROLL_THRESHOLD;
        let shift_scroll_next = self.shift_scroll_acc < -SCROLL_THRESHOLD;
        if shift_scroll_prev { self.shift_scroll_acc -= SCROLL_THRESHOLD; }
        if shift_scroll_next { self.shift_scroll_acc += SCROLL_THRESHOLD; }

        let total_i = total as i32;
        let off = self.offset.value();
        let at_first = self.spread_lo() <= if is_spread { -1 } else { 0 };
        let at_last  = self.spread_base + step + off > total_i - 1;

        let mut nav = ViewerNav::None;
        if (input.shift_nav_up   || shift_scroll_prev) && at_first { nav = ViewerNav::PrevFile; }
        if (input.shift_nav_down || shift_scroll_next) && at_last  { nav = ViewerNav::NextFile; }
        if input.key_left { nav = ViewerNav::PrevFile; }
        if input.key_right { nav = ViewerNav::NextFile; }

        // ── ページ送り ───────────────────────────────────────────────────────
        self.scroll_acc += input.scroll_delta;
        let scroll_next = self.scroll_acc < -SCROLL_THRESHOLD;
        let scroll_prev = self.scroll_acc >  SCROLL_THRESHOLD;
        if scroll_next { self.scroll_acc += SCROLL_THRESHOLD; }
        if scroll_prev { self.scroll_acc -= SCROLL_THRESHOLD; }

        if key_next || scroll_next {
            let next_base = self.spread_base + step;
            if next_base + off <= total_i - 1 { self.spread_base = next_base; }
        }
        if key_prev || scroll_prev {
            let prev_base = self.spread_base - step;
            let min_lo = if is_spread { -1 } else { 0 };
            if prev_base + off >= min_lo { self.spread_base = prev_base; }
        }

        // ナビゲーション後の末尾仮想フラグ更新（オフセットシフト前に確定させる）
        self.offset.update_virtual_right(is_spread && self.spread_lo() + 1 >= total_i);

        // ── 見開き 1P シフト（4/5）──────────────────────────────────────────
        if is_spread {
            if shift_inc { self.shift_offset_forward(); }
            if shift_dec { self.shift_offset_backward(); }
            self.offset.update_virtual_right(self.spread_lo() + 1 >= total_i);
        }

        nav
    }

    fn draw_central_panel(&mut self, ui: &mut egui::Ui, frame: &RenderFrame) -> bool {
        let mut double_clicked = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let clip   = ui.clip_rect();
            let avail  = ui.available_size();
            let origin = ui.cursor().left_top();

            if !frame.animating || frame.zoom_actual {
                // ── 通常レンダリング ──────────────────────────────────────────
                match frame.page_mode {
                    PageMode::Single => {
                        self.render_single(ui, &frame.tex_lo, frame.zoom_actual, &mut double_clicked);
                    }
                    PageMode::SpreadLeft => {
                        Self::render_spread(ui, &frame.tex_lo, &frame.tex_hi, frame.monitor);
                    }
                    PageMode::SpreadRight => {
                        Self::render_spread(ui, &frame.tex_hi, &frame.tex_lo, frame.monitor);
                    }
                }
            } else {
                // ── スライドアニメーション ────────────────────────────────────
                let full_rect = egui::Rect::from_min_size(origin, avail);
                let resp = ui.allocate_rect(full_rect, egui::Sense::click());
                if resp.double_clicked() { double_clicked = true; }

                let painter = ui.painter().with_clip_rect(clip);
                let off_old = avail.x * frame.t * (-frame.anim_dir_f);
                let off_new = avail.x * (1.0 - frame.t) * frame.anim_dir_f;

                match frame.page_mode {
                    PageMode::Single => {
                        Self::paint_single_at(&painter, &frame.prev_tex_lo, avail, origin, off_old);
                        Self::paint_single_at(&painter, &frame.tex_lo,      avail, origin, off_new);
                    }
                    PageMode::SpreadLeft => {
                        let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_lo, &frame.prev_tex_hi, frame.monitor);
                        Self::paint_page(&painter, &frame.prev_tex_lo, rl.translate(egui::vec2(off_old, 0.0)));
                        Self::paint_page(&painter, &frame.prev_tex_hi, rr.translate(egui::vec2(off_old, 0.0)));
                        let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_lo, &frame.tex_hi, frame.monitor);
                        Self::paint_page(&painter, &frame.tex_lo, rl.translate(egui::vec2(off_new, 0.0)));
                        Self::paint_page(&painter, &frame.tex_hi, rr.translate(egui::vec2(off_new, 0.0)));
                    }
                    PageMode::SpreadRight => {
                        let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_hi, &frame.prev_tex_lo, frame.monitor);
                        Self::paint_page(&painter, &frame.prev_tex_hi, rl.translate(egui::vec2(off_old, 0.0)));
                        Self::paint_page(&painter, &frame.prev_tex_lo, rr.translate(egui::vec2(off_old, 0.0)));
                        let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_hi, &frame.tex_lo, frame.monitor);
                        Self::paint_page(&painter, &frame.tex_hi, rl.translate(egui::vec2(off_new, 0.0)));
                        Self::paint_page(&painter, &frame.tex_lo, rr.translate(egui::vec2(off_new, 0.0)));
                    }
                }
            }

            // ── 右下ページ数オーバーレイ ──────────────────────────────────────
            let page_text = format!("{}/{}", self.spread_lo().max(0) + 1, self.entries.len());
            let font_id = egui::FontId::proportional(14.0);
            let text_color = egui::Color32::WHITE;
            let shadow_color = egui::Color32::from_black_alpha(180);
            let panel_rect = ui.clip_rect();
            let painter = ui.painter();
            let galley = painter.layout_no_wrap(page_text, font_id, text_color);
            let text_size = galley.size();
            let margin = egui::vec2(8.0, 6.0);
            let text_pos = panel_rect.right_bottom() - text_size - margin;
            painter.text(text_pos + egui::vec2(1.0, 1.0), egui::Align2::LEFT_TOP, &galley.text().to_string(), egui::FontId::proportional(14.0), shadow_color);
            painter.galley(text_pos, galley, text_color);

            // ── トーストオーバーレイ（下部中央）──────────────────────────────
            if let Some((msg, Some(_))) = &self.toast {
                let toast_font = egui::FontId::proportional(16.0);
                let tg = ui.painter().layout_no_wrap(msg.clone(), toast_font, egui::Color32::WHITE);
                let pad = egui::vec2(16.0, 8.0);
                let bg_size = tg.size() + pad * 2.0;
                let bg_pos = egui::pos2(
                    panel_rect.center().x - bg_size.x / 2.0,
                    panel_rect.bottom() - bg_size.y - 20.0,
                );
                let bg_rect = egui::Rect::from_min_size(bg_pos, bg_size);
                let p = ui.painter();
                p.rect_filled(bg_rect, 6.0, egui::Color32::from_black_alpha(200));
                p.galley(bg_pos + pad, tg, egui::Color32::WHITE);
            }
        });
        double_clicked
    }

    /// サムネイルバー: 本画像の領域を圧迫する形（配置に応じて Panel で領域確保）。
    fn draw_thumbbar_panel(&mut self, ui: &mut egui::Ui, cfg: &ViewerConfig, pos: ThumbbarPos) {
        let outer = cfg.thumbbar_thumb_size as f32 + 16.0;
        let frame = egui::Frame::side_top_panel(ui.style());
        match pos {
            ThumbbarPos::Left => {
                egui::Panel::left("thumbbar_panel").exact_size(outer).resizable(false).frame(frame)
                    .show(ui, |ui| self.draw_thumbbar_contents(ui, cfg, false));
            }
            ThumbbarPos::Right => {
                egui::Panel::right("thumbbar_panel").exact_size(outer).resizable(false).frame(frame)
                    .show(ui, |ui| self.draw_thumbbar_contents(ui, cfg, false));
            }
            ThumbbarPos::Top => {
                egui::Panel::top("thumbbar_panel").exact_size(outer).resizable(false).frame(frame)
                    .show(ui, |ui| self.draw_thumbbar_contents(ui, cfg, true));
            }
            ThumbbarPos::Bottom => {
                egui::Panel::bottom("thumbbar_panel").exact_size(outer).resizable(false).frame(frame)
                    .show(ui, |ui| self.draw_thumbbar_contents(ui, cfg, true));
            }
            ThumbbarPos::None => {}
        }
    }

    /// サムネイルバー: 本画像の前面にオーバーレイ表示（領域は確保しない）。
    /// `viewport` は central panel 描画前に記録した全体領域。
    fn draw_thumbbar_overlay(&mut self, ui: &mut egui::Ui, cfg: &ViewerConfig, pos: ThumbbarPos, viewport: egui::Rect) {
        let thickness = cfg.thumbbar_thumb_size as f32 + 16.0;
        const MARGIN: f32 = 10.0;
        let horizontal = matches!(pos, ThumbbarPos::Top | ThumbbarPos::Bottom);
        let rect = match pos {
            ThumbbarPos::Left => egui::Rect::from_min_size(
                viewport.min + egui::vec2(MARGIN, MARGIN),
                egui::vec2(thickness, viewport.height() - MARGIN * 2.0),
            ),
            ThumbbarPos::Right => egui::Rect::from_min_size(
                egui::pos2(viewport.max.x - MARGIN - thickness, viewport.min.y + MARGIN),
                egui::vec2(thickness, viewport.height() - MARGIN * 2.0),
            ),
            ThumbbarPos::Top => egui::Rect::from_min_size(
                viewport.min + egui::vec2(MARGIN, MARGIN),
                egui::vec2(viewport.width() - MARGIN * 2.0, thickness),
            ),
            ThumbbarPos::Bottom => egui::Rect::from_min_size(
                egui::pos2(viewport.min.x + MARGIN, viewport.max.y - MARGIN - thickness),
                egui::vec2(viewport.width() - MARGIN * 2.0, thickness),
            ),
            ThumbbarPos::None => return,
        };

        ui.painter().rect_filled(rect, 4.0, egui::Color32::from_black_alpha(160));
        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("thumbbar_overlay_child").max_rect(rect));
        self.draw_thumbbar_contents(&mut child, cfg, horizontal);
    }

    /// サムネイルバーの中身。指定 Ui の領域いっぱいにスクロール可能な帯としてサムネを並べ、
    /// 現在地(見開きなら2枚)に半透明ボックスを重ねる。
    fn draw_thumbbar_contents(&mut self, ui: &mut egui::Ui, cfg: &ViewerConfig, horizontal: bool) {
        let edge = cfg.thumbbar_thumb_size as f32;
        let lo = self.spread_lo();
        let hi = if self.page_mode == PageMode::Single { lo } else { lo + 1 };
        let marker = egui::Color32::from_rgba_unmultiplied(
            cfg.thumbbar_marker_r,
            cfg.thumbbar_marker_g,
            cfg.thumbbar_marker_b,
            (cfg.thumbbar_marker_a as f32 / 100.0 * 255.0).round() as u8,
        );

        egui::ScrollArea::new([horizontal, !horizontal])
            .id_salt("thumbbar_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let layout = if horizontal {
                    egui::Layout::left_to_right(egui::Align::Center)
                } else {
                    egui::Layout::top_down(egui::Align::Min)
                };
                ui.with_layout(layout, |ui| {
                    let mut current_rect: Option<egui::Rect> = None;
                    for (i, entry) in self.entries.iter().enumerate() {
                        let orig = entry.original_index;
                        let (rect, _resp) = ui.allocate_exact_size(egui::vec2(edge, edge), egui::Sense::hover());
                        if let Some(tex) = self.thumb_texture(orig) {
                            let fit = fit_rect_contain(rect, tex.size_vec2());
                            ui.painter().image(tex.id(), fit, FULL_UV, egui::Color32::WHITE);
                        } else {
                            ui.painter().rect_filled(rect, 3.0, egui::Color32::from_gray(60));
                        }
                        let is_current = i as i32 == lo || i as i32 == hi;
                        if is_current {
                            ui.painter().rect_filled(rect, 3.0, marker);
                            current_rect = Some(current_rect.map_or(rect, |r| r.union(rect)));
                        }
                    }
                    // 見開きは2枚分の範囲をまとめて1回だけセンタリングする（現在地は動かさず、
                    // サムネの方をスクロールさせる）。先頭/終端付近では egui が自動的に
                    // クランプするため、そこだけマーカーが中央から端へ寄る（仕様通りの挙動）。
                    // ページが実際に変わった時だけ呼ぶ（毎フレーム呼ぶと、リサイズ直後など
                    // クリップ矩形が安定しない間 delta が収束せず request_repaint が連打され
                    // 続けるおそれがあるため。フルスクリーン切替直後の操作停滞の一因だった）。
                    if let Some(r) = current_rect {
                        if self.thumbbar_scrolled_lo != Some(lo) {
                            ui.scroll_to_rect(r, Some(egui::Align::Center));
                            self.thumbbar_scrolled_lo = Some(lo);
                        }
                    }
                });
            });
    }

    fn process_misc_input(
        &mut self,
        ctx: &egui::Context,
        input: &FrameInput,
        is_spread: bool,
        double_clicked: bool,
        cfg: &mut ViewerConfig,
    ) -> bool {
        if (input.zoom_key || double_clicked) && !is_spread {
            cfg.zoom_actual = !cfg.zoom_actual;
            // フェーズ6: 表示ターゲットサイズが変わるイベントとして再デコードのデバウンス対象にする
            cfg.redecode_trigger_seq += 1;
        }

        if input.fs_key || input.middle_clicked {
            cfg.fullscreen = !cfg.fullscreen;
            if cfg.fullscreen {
                #[cfg(windows)]
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                #[cfg(not(windows))]
                {
                    // 本物のFullscreenはGNOME等で専用ワークスペースに移り、他窓へ
                    // フォーカスを移すとビューアーが消えて見える上ESCも届かなくなる
                    // (実験2で確認)。Maximized(true)+Decorations(false)の擬似フルスクに戻す。
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
                }
            } else {
                #[cfg(windows)]
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                #[cfg(not(windows))]
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                }
            }
            log_key!("[key] fullscreen → {}", cfg.fullscreen);
        }

        if input.close_requested || input.esc {
            if cfg.fullscreen {
                #[cfg(windows)]
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                #[cfg(not(windows))]
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                }
            }
            // Windows では非表示にするとゴーストが残るため、フルスク・ウィンドウ問わず最小化で代替する
            #[cfg(windows)]
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            self.open = false;
            cfg.fullscreen = false;
            return true;
        }

        false
    }

    fn tick_toast(&mut self, ctx: &egui::Context, time: f64) {
        match &mut self.toast {
            Some((_, expires @ None)) => {
                *expires = Some(time + 3.0);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            Some((_, Some(exp))) if time >= *exp => {
                self.toast = None;
            }
            Some(_) => {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            None => {}
        }
    }

    /// 表示ウィンドウ付近のページをキャッシュからテクスチャに変換し、
    /// ウィンドウ外のテクスチャを破棄する。GIF アニメーションのフレーム送りも担う。
    fn update_textures(&mut self, ctx: &egui::Context, page_cache: &PageCache) {
        let total = self.entries.len();
        let anchor = self.spread_lo().max(0) as usize;
        let start = anchor.saturating_sub(5);
        let end = (anchor + 10 + 1).min(total);

        let now = Instant::now();
        let mut min_repaint_after = Duration::MAX;

        for i in start..end {
            let orig_i = self.entries[i].original_index;
            match page_cache.get(&self.archive_path, orig_i) {
                Some(PageContent::Static(img)) => {
                    if !self.textures.contains_key(&orig_i) {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [img.width() as usize, img.height() as usize],
                            img.as_raw(),
                        );
                        let tex = ctx.load_texture(
                            format!("page_{orig_i}"),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.textures.insert(orig_i, tex);
                    }
                }
                Some(PageContent::Animated(ring)) => {
                    // フェーズ3/3.5: GIF/APNG/AVIF/WebP。全フレーム常駐ではなく逐次デコード+リングバッファ。
                    // デコーダが終端(None)を返した時点をループ境界とみなし restart() する
                    // (この再デコードによる一瞬のフリーズは許容する設計上の割り切り)。
                    let state = self.anim_states.entry(orig_i).or_insert_with(|| AnimState {
                        frame_index: 0,
                        last_frame_at: now,
                    });
                    let elapsed = now.duration_since(state.last_frame_at);
                    let current_delay = ring
                        .with_frame(state.frame_index, |f| f.delay)
                        .unwrap_or(Duration::from_millis(100));

                    let mut needs_upload = !self.textures.contains_key(&orig_i);
                    if elapsed >= current_delay {
                        let next_index = state.frame_index + 1;
                        if ring.with_frame(next_index, |_| ()).is_some() {
                            state.frame_index = next_index;
                        } else {
                            ring.restart();
                            state.frame_index = 0;
                        }
                        state.last_frame_at = now;
                        needs_upload = true;
                    }

                    if needs_upload {
                        let payload = ring.with_frame(state.frame_index, |f| {
                            (f.image.width(), f.image.height(), f.image.as_raw().clone())
                        });
                        if let Some((w, h, raw)) = payload {
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                [w as usize, h as usize],
                                &raw,
                            );
                            let tex = ctx.load_texture(
                                format!("page_{orig_i}"),
                                color_image,
                                egui::TextureOptions::LINEAR,
                            );
                            self.textures.insert(orig_i, tex);
                        }
                    }

                    // デコード/リサイズ/アップロードに要した実時間を差し引くため、ここで時刻を取り直す
                    // (loop開始時の `now` を使うと、上記処理のコストが remaining に反映されず
                    //  次のrepaintが実処理時間分だけ遅延し、アニメ全体が一様に遅く見える)
                    let now2 = Instant::now();
                    let elapsed_after_upload = now2.duration_since(state.last_frame_at);
                    let next_delay = ring
                        .with_frame(state.frame_index, |f| f.delay)
                        .unwrap_or(Duration::from_millis(100));
                    let remaining = next_delay.saturating_sub(elapsed_after_upload);
                    min_repaint_after = min_repaint_after.min(remaining);
                }
                None => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }
        }

        let window_orig: HashSet<usize> = (start..end)
            .map(|i| self.entries[i].original_index)
            .collect();
        self.textures.retain(|orig_i, _| window_orig.contains(orig_i));
        self.anim_states.retain(|orig_i, _| window_orig.contains(orig_i));

        if min_repaint_after < Duration::MAX {
            ctx.request_repaint_after(min_repaint_after);
        }
    }

    /// 左エントリリストパネル（ホバー制御 + 描画）
    fn draw_entry_list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        viewer_style: &egui::Style,
        hover_pos: Option<egui::Pos2>,
        viewport_rect: egui::Rect,
    ) {
        const ENTRY_PANEL_W: f32 = 180.0;
        const TRIGGER_W: f32 = 40.0;
        const HIDE_MARGIN: f32 = 20.0;

        let screen_left = viewport_rect.min.x;
        if let Some(pos) = hover_pos {
            if !self.entry_list_visible && pos.x < screen_left + TRIGGER_W {
                self.entry_list_visible = true;
                ctx.request_repaint();
            } else if self.entry_list_visible && pos.x > screen_left + ENTRY_PANEL_W + HIDE_MARGIN {
                self.entry_list_visible = false;
                ctx.request_repaint();
            }
        }

        if self.entry_list_visible {
            let current_lo = self.spread_lo().max(0) as usize;
            let is_spread = self.page_mode != PageMode::Single;
            let entries_snap = self.entries.clone();

            egui::Panel::left("entry_list_panel")
                .exact_size(ENTRY_PANEL_W)
                .frame(egui::Frame::side_top_panel(viewer_style))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let total = entries_snap.len();
                            for (i, entry) in entries_snap.iter().enumerate() {
                                let is_cur = i == current_lo
                                    || (is_spread && current_lo + 1 < total && i == current_lo + 1);
                                let _ = ui.selectable_label(is_cur, &entry.display_name);
                            }
                        });
                });
        }
    }

    /// ソートキー切り替えボタン群（通常バーとフルスクリーンバーで共用）
    fn draw_sort_buttons(&mut self, ui: &mut egui::Ui) {
        let mut sort_changed = false;
        let t = i18n::t();
        for (key, label) in [
            (ViewerSortKey::Name,    t.sort_name()),
            (ViewerSortKey::Natural, t.sort_natural()),
            (ViewerSortKey::Date,    t.sort_date()),
        ] {
            let active = self.sort_key == key;
            if ui.selectable_label(active, label).clicked() {
                self.sort_key = key;
                sort_changed = true;
            }
        }
        ui.label(":");
        let order_label = if self.sort_ascending { t.sort_asc() } else { t.sort_desc() };
        if ui.button(order_label).clicked() {
            self.sort_ascending = !self.sort_ascending;
            sort_changed = true;
        }
        if sort_changed { self.sort_entries(); }
    }

    fn render_single(
        &mut self,
        ui: &mut egui::Ui,
        tex: &Option<egui::TextureHandle>,
        zoom_actual: bool,
        double_clicked: &mut bool,
    ) {
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            if zoom_actual {
                // ビューポートより画像が小さい場合は中央寄せ、大きい場合はスクロール領域いっぱいに
                // 敷いて従来どおりの原寸表示にする。
                let outer_available = ui.available_size();
                egui::ScrollArea::both().show(ui, |ui| {
                    let img_size = egui::vec2(img_w as f32, img_h as f32);
                    let content_size = img_size.max(outer_available);
                    let (content_rect, resp) = ui.allocate_exact_size(content_size, egui::Sense::click());
                    let img_rect = egui::Rect::from_min_size(
                        content_rect.min + (content_size - img_size) / 2.0,
                        img_size,
                    );
                    ui.painter().image(tex.id(), img_rect, FULL_UV, egui::Color32::WHITE);
                    if resp.double_clicked() { *double_clicked = true; }
                });
            } else {
                let available = ui.available_size();
                let scale = (available.x / img_w as f32).min(available.y / img_h as f32);
                if !scale.is_finite() || scale <= 0.0 {
                    return;
                }
                let size  = egui::vec2(img_w as f32 * scale, img_h as f32 * scale);
                let tl    = ui.cursor().left_top() + (available - size) / 2.0;
                let rect  = egui::Rect::from_min_size(tl, size);
                let resp  = ui.allocate_rect(rect, egui::Sense::click());
                ui.painter().image(tex.id(), rect, FULL_UV, egui::Color32::WHITE);
                if resp.double_clicked() { *double_clicked = true; }
            }
        } else {
            let rect = egui::Rect::from_min_size(ui.cursor().left_top(), ui.available_size());
            ui.allocate_rect(rect, egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_gray(40));
        }
    }

    fn render_spread(
        ui: &mut egui::Ui,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        monitor: Option<egui::Vec2>,
    ) {
        let available = ui.available_size();
        let origin = ui.cursor().left_top();

        let full_rect = egui::Rect::from_min_size(origin, available);
        ui.allocate_rect(full_rect, egui::Sense::click());

        let (rect_l, rect_r) = Self::spread_rects(available, origin, tex_left, tex_right, monitor);
        let painter = ui.painter();
        Self::paint_page(painter, tex_left,  rect_l);
        Self::paint_page(painter, tex_right, rect_r);
    }

    /// 見開き2ページのレイアウト計算（左右の Rect を返す）
    fn spread_rects(
        available: egui::Vec2,
        origin: egui::Pos2,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        _monitor: Option<egui::Vec2>,
    ) -> (egui::Rect, egui::Rect) {
        if !available.x.is_finite() || !available.y.is_finite()
            || available.x < 1.0 || available.y < 1.0 {
            return (egui::Rect::NOTHING, egui::Rect::NOTHING);
        }

        let page_size = |tex: &Option<egui::TextureHandle>| -> egui::Vec2 {
            tex.as_ref()
                .map(|t| { let [w, h] = t.size(); egui::vec2(w as f32, h as f32) })
                .unwrap_or_else(|| egui::vec2(1.0, std::f32::consts::SQRT_2))
        };
        let sl = page_size(tex_left);
        let sr = page_size(tex_right);
        let ratio_sum = sl.x / sl.y + sr.x / sr.y;
        if !ratio_sum.is_finite() || ratio_sum < 0.01 {
            return (egui::Rect::NOTHING, egui::Rect::NOTHING);
        }

        let h  = (available.x / ratio_sum).min(available.y);
        if !h.is_finite() || h <= 0.0 {
            return (egui::Rect::NOTHING, egui::Rect::NOTHING);
        }
        let w_l = sl.x / sl.y * h;
        let w_r = sr.x / sr.y * h;
        let x0 = origin.x + (available.x - (w_l + w_r)) / 2.0;
        let y0 = origin.y + (available.y - h) / 2.0;
        let rect_l = egui::Rect::from_min_size(egui::pos2(x0,        y0), egui::vec2(w_l, h));
        let rect_r = egui::Rect::from_min_size(egui::pos2(x0 + w_l,  y0), egui::vec2(w_r, h));
        (rect_l, rect_r)
    }

    fn paint_page(painter: &egui::Painter, tex: &Option<egui::TextureHandle>, rect: egui::Rect) {
        match tex {
            Some(t) => { painter.image(t.id(), rect, FULL_UV, egui::Color32::WHITE); }
            None    => { painter.rect_filled(rect, 0.0, egui::Color32::from_gray(40)); }
        }
    }

    /// スロット位置をモニター内に収まるようクランプする（少なくとも 100px は画面内に残す）
    fn clamp_slot_position_inner(x: i32, y: i32, w: u32, _h: u32, monitor: egui::Vec2) -> (i32, i32) {
        let min_visible = 100.0_f32;
        let cx = x.max(-(w as i32) + min_visible as i32)
                  .min((monitor.x - min_visible) as i32);
        let cy = y.max(0).min((monitor.y - min_visible) as i32);
        (cx, cy)
    }

    /// 単ページを offset_x だけ横にずらして描画（アニメーション用）
    fn paint_single_at(
        painter: &egui::Painter,
        tex: &Option<egui::TextureHandle>,
        avail: egui::Vec2,
        origin: egui::Pos2,
        offset_x: f32,
    ) {
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            let scale = (avail.x / img_w as f32).min(avail.y / img_h as f32);
            let size  = egui::vec2(img_w as f32 * scale, img_h as f32 * scale);
            let tl    = origin + (avail - size) / 2.0 + egui::vec2(offset_x, 0.0);
            painter.image(tex.id(), egui::Rect::from_min_size(tl, size), FULL_UV, egui::Color32::WHITE);
        }
    }
}
