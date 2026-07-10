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
use crate::rotation::{self, RotationState};

const SCROLL_THRESHOLD: f32 = 50.0;
/// content_px の初回フレーム前プレースホルダ。draw() 冒頭で毎フレーム実測値に
/// 上書きされるため、実際のデコードターゲットには事実上使われない。
const CONTENT_PX_PLACEHOLDER: (u32, u32) = (1920, 1080);
const ANIM_SECS: f32 = 0.4;
/// アニメ再生のキャッチアップ: 1tickで進める最大フレーム数。
/// フレーム送りはUIスレッド上の同期デコード(RingAnimation::with_frame)を伴うため、
/// 上限なしで追走するとrepaintが長時間ブロックしてUIが固まる。
const MAX_CATCHUP_FRAMES: usize = 4;
/// サムネイルバー: 現在ページを中心にこの枚数分だけ先取り要求する（暫定固定値）。
/// フェーズ2で実際の可視範囲ベースに置き換え予定。
const THUMBBAR_ENQUEUE_WINDOW: i32 = 40;
const FULL_UV: egui::Rect =
    egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(1.0, 1.0) };

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// リングバッファ上の指定フレームをテクスチャとして登録する。
/// フレームが手に入らない（エビクト済み等）場合は None。
fn upload_ring_frame(
    ctx: &egui::Context,
    orig_i: usize,
    ring: &crate::cache::RingAnimation,
    index: usize,
) -> Option<egui::TextureHandle> {
    let (w, h, raw) = ring.with_frame(index, |f| {
        (f.image.width(), f.image.height(), f.image.as_raw().clone())
    })?;
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &raw);
    Some(ctx.load_texture(
        format!("page_{orig_i}"),
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

/// `bounds` の中に `img_size` を縦横比を保ったまま収める（contain-fit）矩形を返す。
/// サムネイルバーの正方形枠に、実際のサムネイル画像(縦長/横長)を収めるのに使う。
pub(crate) fn fit_rect_contain(bounds: egui::Rect, img_size: egui::Vec2) -> egui::Rect {
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
    /// 非可視ページとして凍結中か。フレーム送り(UIスレッド同期デコード)は可視ページ
    /// 限定のため、裏に回ったアニメはこのフラグを立てて位置を凍結し、再可視化時に
    /// 基準時刻を取り直して続きから再開する（凍結中の経過時間を追走させない）。
    paused: bool,
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
    key_home: bool,
    key_end: bool,
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
                key_home:           i.key_pressed(egui::Key::Home),
                key_end:            i.key_pressed(egui::Key::End),
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
    /// TODO項目B: シングルページ表示に適用する手動回転角度(0/90/180/270)
    rotation_angle: i32,
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
    /// サムネイル生成に失敗した original_index 集合。失敗を記録しないと
    /// thumbbar_missing_indices が毎フレーム同じエントリを再要求し、
    /// デコードワーカーが失敗デコードを永久に繰り返す（破損画像等への保険）。
    thumb_failed: HashSet<usize>,
    /// サムネイルバー自動非表示用: 直近のページ操作(ナビゲーション入力)時刻。
    thumbbar_last_activity: Instant,
    /// サムネイルバーを最後にセンタリングした spread_lo。ページが実際に変わった
    /// フレームでだけ scroll_to_rect を呼ぶための重複防止フラグ（毎フレーム呼ぶと
    /// クリップ矩形サイズが不安定な瞬間に delta が収束せず request_repaint が
    /// 連打され続ける恐れがあるため）。
    thumbbar_scrolled_lo: Option<i32>,
    /// フェーズ2: 直近フレームで実描画したサムネイルバーの可視インデックス範囲
    /// (原始インデックス、両端含む)。enqueue の優先範囲としても使う。
    /// None の間は仮想化描画がまだ一度も走っていない（起動直後の1フレーム分）。
    thumbbar_visible_range: Option<(i32, i32)>,
    /// 保存済み見開き状態のキャッシュ（app側がopen_viewer時にセット/操作後に更新）
    saved_spread: Option<(PageMode, i32)>,
    /// 保存メニューでのユーザー操作要求（1フレームで消費してViewerOutputへ渡す）
    pending_spread_action: Option<crate::controller::SpreadSaveAction>,
    /// 右クリックメニュー「お気に入り詳細設定」が押されたか（1フレームで消費）
    pending_open_favorite_dialog: bool,
    /// TODO項目B: 現在ページの手動回転状態。rotation_carry_over が true の間は
    /// この値ではなく ViewerConfig::rotation_session_angle を使う（呼び出し側判断）。
    rotation: RotationState,
    /// TODO項目D相当のダミーフラグ。D本体（設定UI・永続化）は未実装のため常時true固定
    /// （EXIF自動回転は既にデコード時にピクセルへ焼き込み済みのため、Bの範囲では
    /// このフラグを実際の分岐には使わない）。
    #[allow(dead_code)]
    exif_enabled: bool,
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

    /// サムネイルバー用: 現在ページ近傍でまだテクスチャが無く、要求も出していない
    /// original_index 一覧。開いた直後や大きくジャンプした直後に全ページ分を一括で
    /// キューへ積まないよう、直近フレームで実描画した可視範囲(`thumbbar_visible_range`)
    /// に絞る。まだ一度も描画されていない最初のフレームは `THUMBBAR_ENQUEUE_WINDOW`
    /// を暫定の窓として使う。
    pub fn thumbbar_missing_indices(&self) -> Vec<usize> {
        let lo = self.spread_lo();
        let total = self.entries.len() as i32;
        let (win_lo, win_hi) = self.thumbbar_visible_range.unwrap_or((
            lo - THUMBBAR_ENQUEUE_WINDOW,
            lo + THUMBBAR_ENQUEUE_WINDOW,
        ));
        let win_lo = win_lo.max(0);
        let win_hi = win_hi.min(total - 1);
        if win_lo > win_hi {
            return Vec::new();
        }
        (win_lo..=win_hi)
            .map(|i| self.entries[i as usize].original_index)
            .filter(|i| {
                !self.thumb_textures.contains_key(i)
                    && !self.thumb_pending.contains(i)
                    && !self.thumb_failed.contains(i)
            })
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

    /// サムネイルバー用: ワーカーからの結果を反映する。デコード失敗時(None)は
    /// 失敗として記録し、以降は再要求しない（グレーカードのまま表示を継続する）。
    pub fn set_thumb_result(&mut self, ctx: &egui::Context, original_index: usize, rgba: Option<image::RgbaImage>) {
        self.thumb_pending.remove(&original_index);
        if rgba.is_none() {
            self.thumb_failed.insert(original_index);
        }
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
    /// 項目(D): Exif Orientation ON/OFF切替時に、開いているアーカイブの全ページのテクスチャ・
    /// アニメ再生状態を破棄する。`invalidate_pages`(可視ページのみ)と違い、先読みウィンドウ内
    /// (update_textures の表示ウィンドウ)で既にテクスチャを持つ裏ページも古いOrientationの
    /// まま残ってしまうため、開いているアーカイブ全体を対象にする。
    pub fn invalidate_all_pages(&mut self) {
        self.textures.clear();
        self.anim_states.clear();
    }

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
            content_px: CONTENT_PX_PLACEHOLDER,
            thumb_textures: HashMap::new(),
            thumb_pending: HashSet::new(),
            thumb_failed: HashSet::new(),
            thumbbar_last_activity: Instant::now(),
            thumbbar_scrolled_lo: None,
            thumbbar_visible_range: None,
            saved_spread: None,
            pending_spread_action: None,
            pending_open_favorite_dialog: false,
            rotation: RotationState::new(),
            exif_enabled: true,
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
            content_px: CONTENT_PX_PLACEHOLDER,
            thumb_textures: HashMap::new(),
            thumb_pending: HashSet::new(),
            thumb_failed: HashSet::new(),
            thumbbar_last_activity: Instant::now(),
            thumbbar_scrolled_lo: None,
            thumbbar_visible_range: None,
            saved_spread: None,
            pending_spread_action: None,
            pending_open_favorite_dialog: false,
            rotation: RotationState::new(),
            exif_enabled: true,
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

    /// 保存済み見開き状態を復元する（ビューアを開いた直後に一度だけ呼ぶ想定）。
    /// 復帰は常にファイル先頭固定で、保存されたオフセット値だけを先頭に適用する。
    pub fn restore_saved_spread(&mut self, mode: PageMode, offset_value: i32, cfg: &mut ViewerConfig) {
        if self.is_raw_file || mode == PageMode::Single { return; }
        self.set_page_mode(mode, cfg);
        self.spread_base = 0;
        match offset_value {
            v if v < 0 => self.offset.force_virtual_left(),
            v if v > 0 => { self.offset.reset(); self.offset.advance(); }
            _ => self.offset.reset(),
        }
    }

    pub fn set_saved_spread(&mut self, v: Option<(PageMode, i32)>) { self.saved_spread = v; }

    /// 現在の表示状態を保存キー用の (mode, offset) 形式で返す
    pub fn current_spread_snapshot(&self) -> (PageMode, i32) {
        (self.page_mode, self.offset.value())
    }

    /// 保存トグル（チェックボックス）を操作可能か（Single中は保存対象外）
    pub fn spread_save_toggle_enabled(&self) -> bool {
        self.page_mode != PageMode::Single
    }

    pub fn spread_save_toggle_on(&self) -> bool {
        self.saved_spread.is_some()
    }

    /// 「上書き保存」ボタンを操作可能か（保存済みかつ現在のmode/offsetが保存値と異なる場合のみ）
    pub fn spread_overwrite_enabled(&self) -> bool {
        match self.saved_spread {
            None => false,
            Some((mode, offset)) => mode != self.page_mode || offset != self.offset.value(),
        }
    }

    /// メニュー操作で立てられた保存要求を取り出す（1フレームで消費）
    pub fn take_spread_action(&mut self) -> Option<crate::controller::SpreadSaveAction> {
        self.pending_spread_action.take()
    }

    /// 右クリックメニュー「お気に入り詳細設定」の要求を取り出す（1フレームで消費）
    pub fn take_favorite_dialog_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_open_favorite_dialog)
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
        // 表示ページが変わるため手動回転もリセット（carry_over中はcfg側の共有角度を使うため無害）
        self.rotation.reset();
    }

    /// TODO項目B: carry_overトグルの状態に応じて「今どちらの回転値が有効か」を切り替える。
    fn manual_rotation_angle(&self, cfg: &ViewerConfig) -> i32 {
        rotation::manual_angle(cfg.rotation_carry_over, &self.rotation, cfg.rotation_session_angle)
    }

    fn rotate_cw(&mut self, cfg: &mut ViewerConfig) {
        rotation::rotate(cfg.rotation_carry_over, &mut self.rotation, &mut cfg.rotation_session_angle, true);
    }

    fn rotate_ccw(&mut self, cfg: &mut ViewerConfig) {
        rotation::rotate(cfg.rotation_carry_over, &mut self.rotation, &mut cfg.rotation_session_angle, false);
    }

    /// 項目(D)OFF→ON切替時: 「EXIF値のみを見る」という意思表示になるため、
    /// 手動回転の加算分を破棄する（carry_over中はセッション共有角度も0に戻す）。
    pub fn on_exif_orientation_enabled(&mut self, cfg: &mut ViewerConfig) {
        self.rotation.on_exif_enabled();
        cfg.rotation_session_angle = 0;
    }

    /// 項目(D)ON→OFF切替時: デコード時のEXIF焼き込みが無くなる分、見た目維持のため
    /// EXIF回転角度ぶんを手動回転角度へ加算補正する（carry_over中はセッション共有角度側）。
    pub fn on_exif_orientation_disabled(&mut self, cfg: &mut ViewerConfig, exif_deg: i32) {
        if cfg.rotation_carry_over {
            cfg.rotation_session_angle = rotation::normalize_360(cfg.rotation_session_angle + exif_deg);
        } else {
            self.rotation.on_exif_disabled(exif_deg);
        }
    }

    /// 項目(D)ON→OFF切替の角度補正で基準にするページの(original_index, entry_name)。
    /// 見開き時のEXIF基準点決定（仮想ページでない方を優先→両方実ページならインデックスが
    /// 若い方）と同じ優先順位を使う（Bの確定仕様）。
    pub fn rotation_correction_reference_page(&self) -> Option<(usize, String)> {
        let total = self.entries.len() as i32;
        if total == 0 {
            return None;
        }
        let lo = self.spread_lo();
        let pick = |i: i32| -> Option<(usize, String)> {
            if i < 0 || i >= total { return None; }
            self.entries.get(i as usize).map(|e| (e.original_index, e.entry_name.clone()))
        };
        if self.page_mode == PageMode::Single {
            return pick(lo.clamp(0, total - 1));
        }
        let hi = lo + 1;
        let virtual_left = lo < 0 || lo >= total;
        let virtual_right = hi < 0 || hi >= total;
        match (virtual_left, virtual_right) {
            (true, true) => None,
            (true, false) => pick(hi),
            (false, true) => pick(lo),
            (false, false) => pick(lo), // lo < hi は常に成立するので若い方=lo
        }
    }

    pub fn title(&self) -> String {
        self.archive_path.file_name().and_then(|n| n.to_str()).unwrap_or(i18n::t().viewer_fallback()).to_string()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, page_cache: &PageCache, cfg: &mut ViewerConfig) -> ViewerOutput {
        let ctx = ui.ctx().clone();
        let viewer_style = ui.style().clone();
        if !self.open || self.entries.is_empty() {
            return ViewerOutput { nav: ViewerNav::None, close_requested: !self.open, save_slots: None, spread_save_action: None, open_favorite_dialog: false };
        }

        // ── フレーム入力を一括収集（ctx.input はこの1回のみ）────────────────
        let input = FrameInput::collect(&ctx, cfg.zoom_actual);

        // フェーズ6: リサイズ再デコードのターゲットサイズ算出用に、現在の描画領域サイズ（物理px）を記録する。
        let screen = ctx.content_rect().size() * ctx.pixels_per_point();
        self.content_px = (screen.x.max(1.0) as u32, screen.y.max(1.0) as u32);

        // 既定スロットを初回フレームで一度だけ適用（クランプ付き）。
        self.apply_default_slot(&ctx, input.monitor_size);

        let (animating, t) = self.update_animation(&ctx, input.dt, cfg);

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
        // key_left/key_right（←→）はファイル切替専用でこのビューア内のページ送りではなく、
        // 端（先頭/末尾ファイル）で移動先が無い場合は何も動かずトーストが出るだけ。その場合は
        // ここで無条件にタイマーを更新すると「何も動いていないのにバーだけ反応する」矛盾が
        // 起きるため対象から外す。移動が成功した場合は新規 ViewerState 生成時に
        // thumbbar_last_activity が Instant::now() で初期化されるため、そちらで表示される。
        if key_up || key_down || key_space
            || shift_nav_up || shift_nav_down || input.key_home || input.key_end
            || scroll_delta != 0.0 || shift_scroll_delta != 0.0
        {
            self.thumbbar_last_activity = Instant::now();
        }

        if key_left || key_right || key_up || key_down || key_space || esc || zoom_key || fs_key
            || mode1 || mode2 || mode3 || shift4 || shift5
            || shift_nav_up || shift_nav_down || input.key_home || input.key_end
            || scroll_delta != 0.0 || shift_scroll_delta != 0.0
        {
            log_key!(
                "[key] left={} right={} up={} down={} space={} esc={} zoom={} fs={} \
                 mode1={} mode2={} mode3={} shift4={} shift5={} \
                 shift_nav_up={} shift_nav_down={} home={} end={} scroll={:.1} shift_scroll={:.1}",
                key_left, key_right, key_up, key_down, key_space, esc, zoom_key, fs_key,
                mode1, mode2, mode3, shift4, shift5,
                shift_nav_up, shift_nav_down, input.key_home, input.key_end, scroll_delta, shift_scroll_delta
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

        let rotation_angle = self.manual_rotation_angle(cfg);
        let frame = RenderFrame {
            tex_lo, tex_hi, prev_tex_lo, prev_tex_hi,
            animating,
            t,
            anim_dir_f:  self.anim_dir as f32,
            page_mode:   self.page_mode,
            zoom_actual: cfg.zoom_actual,
            monitor:     input.monitor_size,
            rotation_angle,
        };
        let (double_clicked, single_clicked) = self.draw_central_panel(ui, &frame);

        // メイン画像シングルクリックでサムネバーの自動非表示タイマーを早送りし、即座に隠す。
        // idle_hide_ms == 0（常時表示設定）のときは早送り対象のタイマー自体が存在しないため何もしない。
        if single_clicked && idle_ms > 0 {
            self.thumbbar_last_activity = Instant::now() - Duration::from_millis(idle_ms);
            ctx.request_repaint();
        }

        if show_thumbbar && cfg.thumbbar_overlap {
            self.draw_thumbbar_overlay(ui, cfg, cfg.thumbbar_pos, viewport_before_central);
        }

        let nav = self.process_navigation(&input, is_spread, step, total);

        let close_self = self.process_misc_input(&ctx, &input, is_spread, double_clicked, cfg);

        self.tick_toast(&ctx, input.time);

        let spread_save_action = self.take_spread_action();
        let open_favorite_dialog = self.take_favorite_dialog_request();
        ViewerOutput { nav, close_requested: close_self, save_slots, spread_save_action, open_favorite_dialog }
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

    fn update_animation(&mut self, ctx: &egui::Context, dt: f32, cfg: &ViewerConfig) -> (bool, f32) {
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
            // 表示画像が差し替わったので手動回転をリセット（角度引き継ぎトグルONの間は
            // cfg側の共有角度をそのまま使い続けるため、ここではリセットしない）
            if !cfg.rotation_carry_over {
                self.rotation.reset();
            }
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
        cfg: &mut ViewerConfig,
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
                        ui.separator();
                        self.draw_rotation_buttons(ui, cfg);
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
                        ui.horizontal(|ui| {
                            self.draw_sort_buttons(ui);
                            ui.separator();
                            self.draw_rotation_buttons(ui, cfg);
                        });
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

        // ── Home/End: アーカイブ内先頭/末尾へ絶対ジャンプ ────────────────────
        // 通常のページ送りを限界まで行った状態と同じ内部状態を再現する
        // （以降の戻る/進む操作が通常ナビゲーションと同様に振る舞うように）。
        if input.key_home {
            self.scroll_acc = 0.0;
            self.shift_scroll_acc = 0.0;
            self.spread_base = 0;
            if is_spread {
                // オフセットは維持する。ただし維持したままだと先頭実ページ(0)が
                // 欠落してしまう場合（ShiftedOne等）だけ、仮想左側に倒して補正する。
                if self.spread_lo() > 0 {
                    self.offset.force_virtual_left();
                }
            } else {
                self.offset.reset();
            }
            self.offset.update_virtual_right(is_spread && self.spread_lo() + 1 >= total_i);
        }
        if input.key_end {
            self.scroll_acc = 0.0;
            self.shift_scroll_acc = 0.0;
            if is_spread {
                // オフセットは維持する。通常のページ送りを限界までやった時と同じ
                // spread_base（offsetを保ったまま到達できる最大値）を直接計算する。
                let off = self.offset.value();
                let target = total_i - 1 - off;
                let k = if target >= 0 { target / step } else { 0 };
                self.spread_base = (k * step).max(0);
            } else {
                self.spread_base = (total_i - 1).max(0);
                self.offset.reset();
            }
            self.offset.update_virtual_right(is_spread && self.spread_lo() + 1 >= total_i);
        }

        nav
    }

    fn draw_central_panel(&mut self, ui: &mut egui::Ui, frame: &RenderFrame) -> (bool, bool) {
        let mut double_clicked = false;
        let mut single_clicked = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let clip   = ui.clip_rect();
            let avail  = ui.available_size();
            let origin = ui.cursor().left_top();

            if !frame.animating || frame.zoom_actual {
                // ── 通常レンダリング ──────────────────────────────────────────
                match frame.page_mode {
                    PageMode::Single => {
                        self.render_single(ui, &frame.tex_lo, frame.zoom_actual, frame.rotation_angle, &mut double_clicked, &mut single_clicked);
                    }
                    PageMode::SpreadLeft => {
                        self.render_spread(ui, &frame.tex_lo, &frame.tex_hi, frame.monitor, frame.rotation_angle, &mut single_clicked);
                    }
                    PageMode::SpreadRight => {
                        self.render_spread(ui, &frame.tex_hi, &frame.tex_lo, frame.monitor, frame.rotation_angle, &mut single_clicked);
                    }
                }
            } else {
                // ── スライドアニメーション ────────────────────────────────────
                let full_rect = egui::Rect::from_min_size(origin, avail);
                let resp = ui.allocate_rect(full_rect, egui::Sense::click());
                if resp.double_clicked() { double_clicked = true; }
                if resp.clicked() && !resp.double_clicked() { single_clicked = true; }

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
        (double_clicked, single_clicked)
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
    /// フェーズ2: ページ数が多いアーカイブでも重くならないよう、可視範囲＋マージン分だけ
    /// 実際にレイアウト・描画する（仮想化）。可視範囲は `thumbbar_visible_range` に記録し、
    /// enqueue（サムネ生成要求）の優先範囲としても使う。
    fn draw_thumbbar_contents(&mut self, ui: &mut egui::Ui, cfg: &ViewerConfig, horizontal: bool) {
        const MARGIN_ITEMS: i32 = 8;
        let edge = cfg.thumbbar_thumb_size as f32;
        let spacing = ui.spacing().item_spacing;
        let spacing_axis = if horizontal { spacing.x } else { spacing.y };
        let step = edge + spacing_axis;
        let lo = self.spread_lo();
        let hi = if self.page_mode == PageMode::Single { lo } else { lo + 1 };
        let total = self.entries.len() as i32;
        let marker = egui::Color32::from_rgba_unmultiplied(
            cfg.thumbbar_marker_r,
            cfg.thumbbar_marker_g,
            cfg.thumbbar_marker_b,
            (cfg.thumbbar_marker_a as f32 / 100.0 * 255.0).round() as u8,
        );

        // 現在地が仮想ページ(-1 or total)にはみ出している場合、サムネバー側にも
        // その仮想ページ用のスロットを1枠追加する（現在地マーカーを実ページ単独では
        // なく本来の見開きペアとして表示するため）。仮想ページは常に先頭(-1)か
        // 末尾(total)のどちらか片方にしか出ないので、両方同時に足す必要はない。
        let virtual_left = self.page_mode != PageMode::Single && lo == -1;
        let virtual_right = self.page_mode != PageMode::Single && hi == total;
        let base = if virtual_left { 1 } else { 0 };
        let slot_count = total + base + if virtual_right { 1 } else { 0 };

        egui::ScrollArea::new([horizontal, !horizontal])
            .id_salt("thumbbar_scroll")
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                if slot_count <= 0 {
                    self.thumbbar_visible_range = Some((0, -1));
                    return;
                }
                let content_len = (slot_count as f32 * step - spacing_axis).max(0.0);
                if horizontal {
                    ui.set_width(content_len);
                } else {
                    ui.set_height(content_len);
                }

                let (view_min, view_max) = if horizontal {
                    (viewport.min.x, viewport.max.x)
                } else {
                    (viewport.min.y, viewport.max.y)
                };
                let first = ((view_min / step).floor() as i32 - MARGIN_ITEMS).max(0);
                let last = ((view_max / step).ceil() as i32 + MARGIN_ITEMS).min(slot_count - 1);
                // enqueue優先範囲は実ページのみを対象にするため、仮想スロット分は除いて記録する。
                self.thumbbar_visible_range = Some(((first - base).max(0), (last - base).min(total - 1)));

                let origin = ui.max_rect().min;
                let item_rect = |s: i32| -> egui::Rect {
                    let offset = s as f32 * step;
                    if horizontal {
                        egui::Rect::from_min_size(origin + egui::vec2(offset, 0.0), egui::vec2(edge, edge))
                    } else {
                        egui::Rect::from_min_size(origin + egui::vec2(0.0, offset), egui::vec2(edge, edge))
                    }
                };

                let mut current_rect: Option<egui::Rect> = None;
                if first <= last {
                    for s in first..=last {
                        let page = s - base;
                        let rect = item_rect(s);
                        if page < 0 || page >= total {
                            // 仮想ページ: メイン表示側の空白カードと同じ色のダミーを描く。
                            ui.painter().rect_filled(rect, 3.0, egui::Color32::from_gray(40));
                        } else {
                            let orig = self.entries[page as usize].original_index;
                            if let Some(tex) = self.thumb_texture(orig) {
                                let fit = fit_rect_contain(rect, tex.size_vec2());
                                ui.painter().image(tex.id(), fit, FULL_UV, egui::Color32::WHITE);
                            } else {
                                ui.painter().rect_filled(rect, 3.0, egui::Color32::from_gray(60));
                            }
                        }
                        let is_current = page == lo || page == hi;
                        if is_current {
                            ui.painter().rect_filled(rect, 3.0, marker);
                            current_rect = Some(current_rect.map_or(rect, |r| r.union(rect)));
                        }
                        if page >= 0 && page < total {
                            // ページ番号(1-indexed)。ドロップシャドウは右下ページ数オーバーレイと同じ手法。
                            let font_size = (edge * 0.22).clamp(9.0, 20.0);
                            let font_id = egui::FontId::proportional(font_size);
                            let text_color = egui::Color32::WHITE;
                            let shadow_color = egui::Color32::from_black_alpha(180);
                            let galley = ui.painter().layout_no_wrap((page + 1).to_string(), font_id, text_color);
                            let text_pos = egui::pos2(rect.left() + 2.0, rect.bottom() - galley.size().y - 2.0);
                            ui.painter().text(
                                text_pos + egui::vec2(1.0, 1.0),
                                egui::Align2::LEFT_TOP,
                                galley.text(),
                                egui::FontId::proportional(font_size),
                                shadow_color,
                            );
                            ui.painter().galley(text_pos, galley, text_color);
                        }
                    }
                }
                // 現在地(lo/hi)が可視範囲外（マージンの外）でも、位置計算だけで
                // scroll_to_rect の対象矩形を求める。見開きは2枚分の範囲をまとめて
                // 1回だけセンタリングする（現在地は動かさず、サムネの方をスクロールさせる）。
                // ページが実際に変わった時だけ呼ぶ（毎フレーム呼ぶと、リサイズ直後など
                // クリップ矩形が安定しない間 delta が収束せず request_repaint が連打され
                // 続けるおそれがあるため。フルスクリーン切替直後の操作停滞の一因だった）。
                if self.thumbbar_scrolled_lo != Some(lo) {
                    let r = current_rect.unwrap_or_else(|| {
                        let lo_s = (lo + base).clamp(0, slot_count - 1);
                        let hi_s = (hi + base).clamp(0, slot_count - 1);
                        item_rect(lo_s).union(item_rect(hi_s))
                    });
                    ui.scroll_to_rect(r, Some(egui::Align::Center));
                    self.thumbbar_scrolled_lo = Some(lo);
                }
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

        // フレーム送り(UIスレッド同期デコード+アップロード)は可視ページに限定する。
        // 先読みウィンドウ内の裏ページまで毎tickデコードすると、アニメ主体のアーカイブで
        // UIスレッドが飽和し、可視アニメ自身のtickが追走上限に張り付いて再生全体が遅くなる。
        let visible_orig = self.visible_original_indices();

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
                    if !visible_orig.contains(&orig_i) {
                        // 裏ページ: 位置を凍結（tickしない）。ページ送り時の白フラッシュ防止に、
                        // テクスチャ未保有時のみ凍結位置のフレームを1回だけアップロードする。
                        if let Some(state) = self.anim_states.get_mut(&orig_i) {
                            state.paused = true;
                        }
                        if !self.textures.contains_key(&orig_i) {
                            let frozen_index =
                                self.anim_states.get(&orig_i).map_or(0, |s| s.frame_index);
                            if let Some(tex) = upload_ring_frame(ctx, orig_i, ring, frozen_index) {
                                self.textures.insert(orig_i, tex);
                            }
                        }
                        continue;
                    }
                    let state = self.anim_states.entry(orig_i).or_insert_with(|| AnimState {
                        frame_index: 0,
                        last_frame_at: now,
                        paused: false,
                    });
                    // 凍結明け: 凍結中の経過時間を再生遅延として追走しないよう基準時刻を取り直し、
                    // 凍結位置から等速で再開する。
                    if state.paused {
                        state.paused = false;
                        state.last_frame_at = now;
                    }
                    let mut needs_upload = !self.textures.contains_key(&orig_i);
                    // 遅れが1フレーム分を超えていたら複数フレーム進めて追いつく
                    // (テクスチャアップロードは最後の1枚だけ)。スキップ分のデコードも
                    // UIスレッドで走るため、上限 MAX_CATCHUP_FRAMES で打ち切る。
                    let mut advanced = false;
                    for _ in 0..MAX_CATCHUP_FRAMES {
                        let current_delay = ring
                            .with_frame(state.frame_index, |f| f.delay)
                            .unwrap_or(Duration::from_millis(100));
                        if now.duration_since(state.last_frame_at) < current_delay {
                            break;
                        }
                        let next_index = state.frame_index + 1;
                        if ring.with_frame(next_index, |_| ()).is_some() {
                            state.frame_index = next_index;
                            // 超過分(elapsed - delay)を次フレームへ繰り越して蓄積誤差を防ぐ
                            state.last_frame_at += current_delay;
                        } else {
                            // ループ境界: restart()はリング全クリア+先頭からの再デコードで
                            // コストが読めないため、境界を跨ぐ追走はせずフレーム0から仕切り直す。
                            ring.restart();
                            state.frame_index = 0;
                            state.last_frame_at = now;
                            advanced = true;
                            break;
                        }
                        advanced = true;
                    }
                    if advanced {
                        needs_upload = true;
                        // 上限まで進めてもまだ1フレーム分以上遅れている場合
                        // (デコードが再生速度に追いつかない高速アニメ、最小化からの復帰直後など)は
                        // 追走を諦めて now に切り直し「遅いなり再生」に落とす(無限追走スパイラル防止)。
                        let current_delay = ring
                            .with_frame(state.frame_index, |f| f.delay)
                            .unwrap_or(Duration::from_millis(100));
                        if now.duration_since(state.last_frame_at) >= current_delay {
                            state.last_frame_at = now;
                        }
                    }

                    if needs_upload {
                        let frame_index = state.frame_index;
                        if let Some(tex) = upload_ring_frame(ctx, orig_i, ring, frame_index) {
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
            // 仮想ページ(-1 or total)を握りつぶさないよう、クランプ前の生の lo/hi で
            // ペア判定してから、実ページ範囲内のものだけハイライトする。
            let lo = self.spread_lo();
            let is_spread = self.page_mode != PageMode::Single;
            let hi = if is_spread { lo + 1 } else { lo };
            let entries_snap = self.entries.clone();

            egui::Panel::left("entry_list_panel")
                .exact_size(ENTRY_PANEL_W)
                .frame(egui::Frame::side_top_panel(viewer_style))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, entry) in entries_snap.iter().enumerate() {
                                let i = i as i32;
                                let is_cur = i == lo || i == hi;
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

    /// TODO項目B: 手動回転CW/CCWボタン（アイコンのみ、i18n対象外）＋角度引き継ぎトグル
    fn draw_rotation_buttons(&mut self, ui: &mut egui::Ui, cfg: &mut ViewerConfig) {
        if ui.button("⟲").on_hover_text(i18n::t().rotate_ccw()).clicked() {
            self.rotate_ccw(cfg);
        }
        if ui.button("⟳").on_hover_text(i18n::t().rotate_cw()).clicked() {
            self.rotate_cw(cfg);
        }
        ui.checkbox(&mut cfg.rotation_carry_over, i18n::t().rotation_carry_over_label());
    }

    /// 画像本体の右クリックメニュー（見開き設定の保存トグル／上書き保存）を描画する
    fn spread_save_context_menu(
        ui: &mut egui::Ui,
        toggle_enabled: bool,
        toggle_on_init: bool,
        overwrite_enabled: bool,
        action: &mut Option<crate::controller::SpreadSaveAction>,
        open_favorite_dialog: &mut bool,
    ) {
        let t = i18n::t();
        let mut toggle_on = toggle_on_init;
        ui.add_enabled_ui(toggle_enabled, |ui| {
            if ui.checkbox(&mut toggle_on, t.spread_save_toggle_label()).changed() {
                *action = Some(if toggle_on {
                    crate::controller::SpreadSaveAction::Enable
                } else {
                    crate::controller::SpreadSaveAction::Disable
                });
                ui.close();
            }
        });
        ui.add_enabled_ui(overwrite_enabled, |ui| {
            if ui.button(t.spread_save_overwrite_label()).clicked() {
                *action = Some(crate::controller::SpreadSaveAction::Overwrite);
                ui.close();
            }
        });
        ui.separator();
        if ui.button(t.favorite_detail_menu()).clicked() {
            *open_favorite_dialog = true;
            ui.close();
        }
    }

    fn render_single(
        &mut self,
        ui: &mut egui::Ui,
        tex: &Option<egui::TextureHandle>,
        zoom_actual: bool,
        angle_deg: i32,
        double_clicked: &mut bool,
        single_clicked: &mut bool,
    ) {
        let toggle_enabled = self.spread_save_toggle_enabled();
        let toggle_on = self.spread_save_toggle_on();
        let overwrite_enabled = self.spread_overwrite_enabled();
        let action = &mut self.pending_spread_action;
        let open_favorite_dialog = &mut self.pending_open_favorite_dialog;
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            if zoom_actual {
                // ビューポートより画像が小さい場合は中央寄せ、大きい場合はスクロール領域いっぱいに
                // 敷いて従来どおりの原寸表示にする。90/270度時は回転後の外接サイズで
                // スクロール範囲を確保してから、その中心を軸に回転させる（等倍・拡縮なし）。
                let outer_available = ui.available_size();
                egui::ScrollArea::both().show(ui, |ui| {
                    let img_size = egui::vec2(img_w as f32, img_h as f32);
                    let rotated_size = if angle_deg == 90 || angle_deg == 270 {
                        egui::vec2(img_size.y, img_size.x)
                    } else {
                        img_size
                    };
                    let content_size = rotated_size.max(outer_available);
                    let (content_rect, resp) = ui.allocate_exact_size(content_size, egui::Sense::click());
                    let bbox = egui::Rect::from_min_size(
                        content_rect.min + (content_size - rotated_size) / 2.0,
                        rotated_size,
                    );
                    if angle_deg == 0 {
                        ui.painter().image(tex.id(), bbox, FULL_UV, egui::Color32::WHITE);
                    } else {
                        Self::paint_texture_rotated_at(ui.painter(), tex, bbox.center(), 1.0, angle_deg);
                    }
                    if resp.double_clicked() { *double_clicked = true; }
                    if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
                    resp.context_menu(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, open_favorite_dialog));
                });
            } else {
                let available = ui.available_size();
                let bounds = egui::Rect::from_min_size(ui.cursor().left_top(), available);
                let fit = Self::paint_page_rotated(ui.painter(), tex, bounds, angle_deg);
                let resp  = ui.allocate_rect(fit, egui::Sense::click());
                if resp.double_clicked() { *double_clicked = true; }
                if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
                resp.context_menu(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, open_favorite_dialog));
            }
        } else {
            let rect = egui::Rect::from_min_size(ui.cursor().left_top(), ui.available_size());
            ui.allocate_rect(rect, egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_gray(40));
        }
    }

    fn render_spread(
        &mut self,
        ui: &mut egui::Ui,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        monitor: Option<egui::Vec2>,
        angle_deg: i32,
        single_clicked: &mut bool,
    ) {
        let toggle_enabled = self.spread_save_toggle_enabled();
        let toggle_on = self.spread_save_toggle_on();
        let overwrite_enabled = self.spread_overwrite_enabled();
        let action = &mut self.pending_spread_action;
        let open_favorite_dialog = &mut self.pending_open_favorite_dialog;
        let available = ui.available_size();
        let origin = ui.cursor().left_top();

        let full_rect = egui::Rect::from_min_size(origin, available);
        let resp = ui.allocate_rect(full_rect, egui::Sense::click());
        if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
        resp.context_menu(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, open_favorite_dialog));

        if angle_deg == 0 {
            let (rect_l, rect_r) = Self::spread_rects(available, origin, tex_left, tex_right, monitor);
            let painter = ui.painter();
            Self::paint_page(painter, tex_left,  rect_l);
            Self::paint_page(painter, tex_right, rect_r);
        } else {
            Self::paint_spread_rotated(ui.painter(), full_rect, tex_left, tex_right, angle_deg);
        }
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

        let sl = Self::spread_page_size(tex_left);
        let sr = Self::spread_page_size(tex_right);
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

    /// テクスチャ未取得時のプレースホルダを含むページサイズ取得（spread_rects/spread_local_rects共通）
    fn spread_page_size(tex: &Option<egui::TextureHandle>) -> egui::Vec2 {
        tex.as_ref()
            .map(|t| { let [w, h] = t.size(); egui::vec2(w as f32, h as f32) })
            .unwrap_or_else(|| egui::vec2(1.0, std::f32::consts::SQRT_2))
    }

    /// 見開き2ページの「回転前・高さ1.0基準」ローカル矩形(rect_l, rect_r)を返す。
    /// 原点(0,0)基準、spread_rectsと同じ比率ロジックを流用（横並び幅のみが違う）。
    fn spread_local_rects(
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
    ) -> (egui::Rect, egui::Rect) {
        Self::local_rects_from_sizes(Self::spread_page_size(tex_left), Self::spread_page_size(tex_right))
    }

    /// spread_local_rectsの核となる幾何計算（テクスチャサイズを直接受け取る版、単体テスト用）。
    fn local_rects_from_sizes(sl: egui::Vec2, sr: egui::Vec2) -> (egui::Rect, egui::Rect) {
        let h = 1.0;
        let w_l = sl.x / sl.y * h;
        let w_r = sr.x / sr.y * h;
        let rect_l = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w_l, h));
        let rect_r = egui::Rect::from_min_size(egui::pos2(w_l,  0.0), egui::vec2(w_r, h));
        (rect_l, rect_r)
    }

    /// 見開き回転の幾何計算だけを抜き出した純粋関数（TextureHandle非依存、単体テスト可能）。
    /// `local_l`/`local_r` は高さ1.0基準のローカル矩形。左右ページ中心の画面座標と、
    /// ローカル単位→画面px換算率(scale)を返す（scale自体はテクスチャ実ピクセルには未換算）。
    /// 直近の見開き回転バグ（ローカル単位のscaleをテクスチャ実ピクセルへ直接適用して破綻した件）を
    /// 構造的に防ぐため、テクスチャピクセルサイズは一切扱わない。
    fn spread_rotation_fit(
        local_l: egui::Rect,
        local_r: egui::Rect,
        bounds: egui::Rect,
        angle_deg: i32,
    ) -> Option<(egui::Pos2, egui::Pos2, f32)> {
        let footprint = local_l.union(local_r);
        let footprint_center = footprint.center();
        let rotated_size = if angle_deg == 90 || angle_deg == 270 {
            egui::vec2(footprint.height(), footprint.width())
        } else {
            footprint.size()
        };
        let fit = fit_rect_contain(bounds, rotated_size);
        let scale = (fit.width() / rotated_size.x).min(fit.height() / rotated_size.y);
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let rot = egui::emath::Rot2::from_angle((angle_deg as f32).to_radians());
        let center_l = fit.center() + rot * ((local_l.center() - footprint_center) * scale);
        let center_r = fit.center() + rot * ((local_r.center() - footprint_center) * scale);
        Some((center_l, center_r, scale))
    }

    /// 見開き全体（左右2ページ）を1つの剛体として `bounds` にcontain-fitしつつ
    /// `angle_deg` 度回転させて描画する（TODO項目B）。EXIFは既にデコード時にピクセルへ
    /// 焼き込み済みのため、基準点判定は不要で手動回転角度のみを外接矩形に適用する。
    fn paint_spread_rotated(
        painter: &egui::Painter,
        bounds: egui::Rect,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        angle_deg: i32,
    ) {
        let (local_l, local_r) = Self::spread_local_rects(tex_left, tex_right);
        let Some((center_l, center_r, scale)) =
            Self::spread_rotation_fit(local_l, local_r, bounds, angle_deg)
        else {
            return;
        };
        for (tex, local_rect, center) in [(tex_left, local_l, center_l), (tex_right, local_r, center_r)] {
            match tex {
                Some(t) => {
                    // local_rect は高さ1.0基準の正規化座標なので、scale(ローカル単位→画面px)を
                    // そのままテクスチャの実ピクセルサイズに掛けると単位が合わず破綻する。
                    // テクスチャ高さ→ローカル単位1.0への換算(pixel_scale)を挟む。
                    let [_, tex_h] = t.size();
                    if tex_h > 0 {
                        let pixel_scale = scale / tex_h as f32;
                        Self::paint_texture_rotated_at(painter, t, center, pixel_scale, angle_deg);
                    }
                }
                None => {
                    let half = local_rect.size() * scale / 2.0;
                    let points = Self::rotated_quad_points(center, half, angle_deg);
                    painter.add(egui::Shape::convex_polygon(
                        points.to_vec(),
                        egui::Color32::from_gray(40),
                        egui::Stroke::NONE,
                    ));
                }
            }
        }
    }

    fn paint_page(painter: &egui::Painter, tex: &Option<egui::TextureHandle>, rect: egui::Rect) {
        match tex {
            Some(t) => { painter.image(t.id(), rect, FULL_UV, egui::Color32::WHITE); }
            None    => { painter.rect_filled(rect, 0.0, egui::Color32::from_gray(40)); }
        }
    }

    /// 中心点 `center`・半径(半幅半高) `half`・`angle_deg` 度で回転させた矩形の4頂点
    /// （左上→右上→右下→左下の順）を返す共通ヘルパー。
    fn rotated_quad_points(center: egui::Pos2, half: egui::Vec2, angle_deg: i32) -> [egui::Pos2; 4] {
        let rot = egui::emath::Rot2::from_angle((angle_deg as f32).to_radians());
        [
            center + rot * egui::vec2(-half.x, -half.y),
            center + rot * egui::vec2( half.x, -half.y),
            center + rot * egui::vec2( half.x,  half.y),
            center + rot * egui::vec2(-half.x,  half.y),
        ]
    }

    /// テクスチャ全体を中心点 `center` 周りに `scale` 倍・`angle_deg` 度回転させて描画する
    /// 共通ヘルパー（実ピクセル合成はしない、頂点座標の回転のみ）。
    fn paint_texture_rotated_at(
        painter: &egui::Painter,
        tex: &egui::TextureHandle,
        center: egui::Pos2,
        scale: f32,
        angle_deg: i32,
    ) {
        let [tw, th] = tex.size();
        let half = egui::vec2(tw as f32, th as f32) * scale / 2.0;
        let corners = Self::rotated_quad_points(center, half, angle_deg);
        let uvs = [
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
        ];
        let mut mesh = egui::Mesh::with_texture(tex.id());
        for i in 0..4 {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: corners[i],
                uv: uvs[i],
                color: egui::Color32::WHITE,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }

    /// 手動回転(TODO項目B)を適用したテクスチャ描画（contain-fitモード用）。
    /// `bounds` にはcontain-fit前の利用可能領域を渡す。90/270度時は縦横が入れ替わった
    /// 外接サイズでcontain-fitしてから、その中心を軸にテクスチャ矩形を回転させる。
    /// クリック判定用に、実際に使ったfit矩形(回転後の外接矩形)を返す。
    fn paint_page_rotated(
        painter: &egui::Painter,
        tex: &egui::TextureHandle,
        bounds: egui::Rect,
        angle_deg: i32,
    ) -> egui::Rect {
        let [tw, th] = tex.size();
        let (tw, th) = (tw as f32, th as f32);
        let rotated_size = if angle_deg == 90 || angle_deg == 270 {
            egui::vec2(th, tw)
        } else {
            egui::vec2(tw, th)
        };
        let fit = fit_rect_contain(bounds, rotated_size);
        if angle_deg == 0 {
            painter.image(tex.id(), fit, FULL_UV, egui::Color32::WHITE);
            return fit;
        }
        let scale = (fit.width() / rotated_size.x).min(fit.height() / rotated_size.y);
        if scale.is_finite() && scale > 0.0 {
            Self::paint_texture_rotated_at(painter, tex, fit.center(), scale, angle_deg);
        }
        fit
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

#[cfg(test)]
mod spread_rotation_tests {
    use super::*;

    /// 見開き回転バグの再発防止: spread_rotation_fit はローカル単位のscaleのみを返し、
    /// テクスチャの実ピクセルサイズを一切扱わない（呼び出し側でpixel_scale = scale/tex_hを
    /// 挟む前提の関数であることをシグネチャで固定する）。ここでは幾何計算そのものが
    /// bounds/角度に対して妥当な値になることを検証する。
    #[test]
    fn angle_0_places_pages_side_by_side() {
        let (local_l, local_r) = ViewerState::local_rects_from_sizes(
            egui::vec2(1.0, 1.0), egui::vec2(1.0, 1.0),
        );
        let bounds = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let (center_l, center_r, scale) =
            ViewerState::spread_rotation_fit(local_l, local_r, bounds, 0).unwrap();
        assert!(center_l.x < center_r.x, "左ページ中心は右ページ中心より左");
        assert!((center_l.y - center_r.y).abs() < 1e-3, "0度時は上下が揃う");
        assert!((scale - 100.0).abs() < 1e-3, "footprint(2x1)がbounds(200x100)にcontain-fit");
    }

    #[test]
    fn angle_90_swaps_layout_to_vertical() {
        let (local_l, local_r) = ViewerState::local_rects_from_sizes(
            egui::vec2(1.0, 1.0), egui::vec2(1.0, 1.0),
        );
        let bounds = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 200.0));
        let (center_l, center_r, scale) =
            ViewerState::spread_rotation_fit(local_l, local_r, bounds, 90).unwrap();
        assert!((center_l.x - center_r.x).abs() < 1e-3, "90度回転後は左右中心のx座標が揃う");
        assert!(center_l.y < center_r.y, "時計回り90度で元の左ページが上に来る");
        assert!((scale - 100.0).abs() < 1e-3, "footprint回転後(1x2)がbounds(100x200)にcontain-fit");
    }

    #[test]
    fn degenerate_bounds_returns_none_without_panicking() {
        let (local_l, local_r) = ViewerState::local_rects_from_sizes(
            egui::vec2(1.0, 1.0), egui::vec2(1.0, 1.0),
        );
        let bounds = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 0.0));
        assert!(ViewerState::spread_rotation_fit(local_l, local_r, bounds, 90).is_none());
    }
}
