use crate::gui_config::{SlideshowManualBehavior, ThumbbarPos, TransitionKind, ViewerConfig};
use crate::controller::{ViewerNav, ViewerOutput};
use crate::i18n;
use crate::log_key;
use crate::types::ReaderSortKey as ViewerSortKey;
pub use crate::types::{PageMode, ViewerEntry};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::cache::{AnimationFrameDiagnostic, AnimationInstanceId, PageCache, PageContent};
use crate::gui_config::WindowSlot;
use crate::fs::archive;
use crate::spread_offset::SpreadOffset;
use crate::rotation::{self, RotationState};
use crate::toolbar::{BarGroup, ViewerBarItem};
use crate::keymap::{Keymap, ReaderAction, MouseAction, MouseCombo};

const SCROLL_THRESHOLD: f32 = 50.0;
/// アニメ専用パイプラインへ一度に許可するデコード先行幅。
/// UIが可視アニメをtickしている間だけ、到達のたびに次の範囲を追加する。
const ANIM_DECODE_AHEAD_FRAMES: usize = 8;
/// content_px の初回フレーム前プレースホルダ。draw() 冒頭で毎フレーム実測値に
/// 上書きされるため、実際のデコードターゲットには事実上使われない。
const CONTENT_PX_PLACEHOLDER: (u32, u32) = (1920, 1080);
/// 実測前のGPUテクスチャ1辺の上限（egui-wgpu の既定デバイス上限）。
const MAX_TEXTURE_SIDE_FALLBACK: usize = 8192;

/// デコード目標の1辺を、GPUテクスチャの1辺上限へ収める。
/// 上限を超えたテクスチャはwgpuの検証エラーになるため、見開きの2倍やGUI設定の上限値
/// （最大7680の2倍=15360）が上限を超えないよう、デコード目標の段階で切る。
pub(crate) fn clamp_decode_edge(edge: u32, max_texture_side: usize) -> u32 {
    edge.min(max_texture_side.min(u32::MAX as usize) as u32).max(1)
}
/// サムネイルバー: 現在ページを中心にこの枚数分だけ先取り要求する（暫定固定値）。
/// フェーズ2で実際の可視範囲ベースに置き換え予定。
const THUMBBAR_ENQUEUE_WINDOW: i32 = 40;
const FULL_UV: egui::Rect =
    egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(1.0, 1.0) };

/// 実行中のオフセット方向を、ファイル先頭から復帰するための保存値へ正規化する。
/// ±1 はどちらも同じ1ページずれを表すため、先頭実ページを欠落させない -1 に揃える。
pub(crate) fn normalize_saved_spread_offset(offset: i32) -> i32 {
    if offset == 0 { 0 } else { -1 }
}

fn next_anim_decode_request(
    displayed_frame: usize,
    requested_through: usize,
    ring_capacity: usize,
) -> usize {
    // producerの到達位置を基準にすると、描画が止まっていても要求が自己増殖し、
    // 厳密に待っている次フレームをリングから追い出してしまう。
    let ahead = ANIM_DECODE_AHEAD_FRAMES.min(ring_capacity.max(1));
    requested_through.max(displayed_frame.saturating_add(ahead))
}

/// 1ページだけずれる見開き遷移を、退場・共通・入場ページへ分解する。
/// 共通ページを旧/新の両見開きで二重描画しないため、テクスチャではなく論理ページ番号で判定する。
fn offset_transition_pages(from_lo: i32, to_lo: i32) -> Option<(i32, i32, i32)> {
    if (to_lo - from_lo).abs() != 1 {
        return None;
    }
    let old = [from_lo, from_lo + 1];
    let new = [to_lo, to_lo + 1];
    let old_only = old.into_iter().find(|page| !new.contains(page))?;
    let shared = old.into_iter().find(|page| new.contains(page))?;
    let new_only = new.into_iter().find(|page| !old.contains(page))?;
    Some((old_only, shared, new_only))
}

fn visual_spread_pages(lo: i32, right_binding: bool) -> [i32; 2] {
    if right_binding { [lo + 1, lo] } else { [lo, lo + 1] }
}

fn lerp_rect(from: egui::Rect, to: egui::Rect, t: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        from.min + (to.min - from.min) * t,
        from.max + (to.max - from.max) * t,
    )
}

fn place_next_to(rect: egui::Rect, anchor: egui::Rect, on_left: bool) -> egui::Rect {
    let center_x = if on_left {
        anchor.left() - rect.width() / 2.0
    } else {
        anchor.right() + rect.width() / 2.0
    };
    egui::Rect::from_center_size(egui::pos2(center_x, anchor.center().y), rect.size())
}

fn animation_instance_changed(
    previous: Option<AnimationInstanceId>,
    current: AnimationInstanceId,
) -> bool {
    previous.is_some_and(|previous| previous != current)
}

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
    let (w, h, raw) = ring.try_with_frame(index, |f| {
        (f.image.width(), f.image.height(), f.image.as_raw().clone())
    })?;
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &raw);
    Some(ctx.load_texture(
        format!("page_{orig_i}"),
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

fn log_anim_texture_upload(
    archive_path: &std::path::Path,
    orig_i: usize,
    previous_generation: Option<u64>,
    generation: u64,
    frame_index: usize,
    visible: bool,
    generation_changed: bool,
    elapsed: Duration,
) {
    if generation_changed || elapsed >= Duration::from_millis(8) {
        crate::log_perf!(
            "[diag/anim-texture] archive={:?} page={} previous_generation={:?} generation={} frame={} visible={} reason={} upload={:.1}ms",
            archive_path,
            orig_i,
            previous_generation,
            generation,
            frame_index,
            visible,
            if generation_changed { "generation-change" } else { "frame-update" },
            elapsed.as_secs_f64() * 1000.0,
        );
    }
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
    instance_id: AnimationInstanceId,
    frame_index: usize,
    requested_through: usize,
    last_frame_at: Instant,
    /// 非可視ページとして凍結中か。フレーム送り(UIスレッド同期デコード)は可視ページ
    /// 限定のため、裏に回ったアニメはこのフラグを立てて位置を凍結し、再可視化時に
    /// 基準時刻を取り直して続きから再開する（凍結中の経過時間を追走させない）。
    paused: bool,
    /// 同じ欠落状態を毎tick出力しないための異常ログ抑止。
    missing_frame_logged: bool,
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
    /// 虫眼鏡モード用のホイール量（ノッチ単位・正=上回し）。ページ送りと同じ修飾キー割り当てを
    /// 使うが、スムージング前のイベントから拾う（100%スナップがノッチ単位で効くように）。
    wheel_notches: f32,
    // ポインタ
    hover_pos: Option<egui::Pos2>,
    middle_clicked: bool,
    primary_clicked: bool,
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
    /// キー・マウス判定はキーアサイン設定(TODO項目J、[keymap.rs](../keymap.rs))経由。
    /// ホイールは「PagePrev/PageNextのマウス割り当て」「FileNavPrevAlt/NextAltのマウス割り当て」
    /// それぞれの修飾キー条件が現在の入力状態と一致するかを見て、一致した方に生delta(sd.y、
    /// shift成分ありなら+sd.x)を渡す。PagePrev/PageNextは対で同じ条件を持つ想定のため、
    /// 片方から拾えれば十分（Prev側優先、無ければNext側）。
    fn collect(ctx: &egui::Context, keymap: &Keymap) -> Self {
        ctx.input(|i| {
            // 原寸表示中も含め、ホイールは常にページ送りへ渡す（原寸時の画像内スクロールは
            // ScrollAreaのドラッグ/スクロールバー操作に譲り、ホイールとは役割を分離する）。
            let sd = i.smooth_scroll_delta();
            let wheel_amount = |m: MouseCombo| -> f32 {
                if !m.modifiers_match(i) { return 0.0; }
                sd.y + if m.shift { sd.x } else { 0.0 }
            };
            let wheel_notches_of = |m: MouseCombo| -> f32 {
                if !m.modifiers_match(i) { return 0.0; }
                i.events.iter().map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => {
                        let d = delta.y + if m.shift { delta.x } else { 0.0 };
                        match unit {
                            egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => d,
                            egui::MouseWheelUnit::Point => d / SCROLL_THRESHOLD,
                        }
                    }
                    _ => 0.0,
                }).sum()
            };
            let act = |a: ReaderAction| keymap.reader_binding(a).key_pressed(i);
            let mouse_of = |a: ReaderAction| keymap.reader_binding(a).effective_mouse();
            let page_mouse = mouse_of(ReaderAction::PagePrev).or_else(|| mouse_of(ReaderAction::PageNext));
            let file_mouse = mouse_of(ReaderAction::FileNavPrevAlt).or_else(|| mouse_of(ReaderAction::FileNavNextAlt));
            let middle_clicked = mouse_of(ReaderAction::ToggleFullscreen)
                .is_some_and(|m| m.action == MouseAction::MiddleClick && m.modifiers_match(i))
                && i.pointer.button_clicked(egui::PointerButton::Middle);
            let slot_apply =
                if      act(ReaderAction::ApplySlot1) { Some(0) }
                else if act(ReaderAction::ApplySlot2) { Some(1) }
                else if act(ReaderAction::ApplySlot3) { Some(2) }
                else if act(ReaderAction::ApplySlot4) { Some(3) }
                else { None };
            let vp = i.viewport();
            Self {
                key_left:           act(ReaderAction::FileNavPrev),
                key_right:          act(ReaderAction::FileNavNext),
                key_up:             act(ReaderAction::PagePrev),
                key_down:           act(ReaderAction::PageNext),
                key_space:          act(ReaderAction::PageAdvanceSpace),
                esc:                act(ReaderAction::CloseOrExitFullscreen),
                zoom_key:           act(ReaderAction::ToggleZoomActual),
                fs_key:             act(ReaderAction::ToggleFullscreen),
                mode1:              act(ReaderAction::PageModeSingle),
                mode2:              act(ReaderAction::PageModeSpreadLeft),
                mode3:              act(ReaderAction::PageModeSpreadRight),
                shift4:             act(ReaderAction::SpreadOffsetPrev),
                shift5:             act(ReaderAction::SpreadOffsetNext),
                shift_nav_up:       act(ReaderAction::FileNavPrevAlt),
                shift_nav_down:     act(ReaderAction::FileNavNextAlt),
                key_home:           act(ReaderAction::JumpFirstPage),
                key_end:            act(ReaderAction::JumpLastPage),
                slot_apply,
                scroll_delta:       page_mouse.map(wheel_amount).unwrap_or(0.0),
                shift_scroll_delta: file_mouse.map(wheel_amount).unwrap_or(0.0),
                wheel_notches:      page_mouse.map(wheel_notches_of).unwrap_or(0.0),
                hover_pos:          i.pointer.hover_pos(),
                middle_clicked,
                primary_clicked:    i.pointer.button_clicked(egui::PointerButton::Primary),
                outer_rect:         vp.outer_rect,
                inner_rect:         vp.inner_rect,
                monitor_size:       vp.monitor_size,
                viewport_rect:      i.viewport_rect(),
                os_maximized:       vp.maximized.unwrap_or(false),
                close_requested:    vp.close_requested(),
                dt:                 i.stable_dt.min(0.1),
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
    anim_from_lo: i32,
    current_lo:  i32,
    page_mode:   PageMode,
    zoom_actual: bool,
    /// 虫眼鏡モードが有効（ON かつ 単ページ・回転なし・テクスチャ取得済み）。
    /// 有効な間、ホイールは拡縮に使われ、ページ送りには渡さない。
    magnifier:   bool,
    monitor:     Option<egui::Vec2>,
    /// TODO項目B: シングルページ表示に適用する手動回転角度(0/90/180/270)
    rotation_angle: i32,
    /// スライドショー設定のトランジション種類。アニメ中の描画分岐に使う。
    transition_kind: TransitionKind,
}

/// 右クリック「ファイル詳細」ダイアログの状態。開いた瞬間の情報をスナップショットして保持する
/// （以後のページ送りには追従しない）。
#[derive(Clone)]
struct FileDetailDialogState {
    /// 親アーカイブのファイル名。生ファイル表示中（アーカイブなし）はNone（空白表示）
    archive_name: Option<String>,
    /// 画面右側ページのファイル名（見開きでなければ「ファイル名」欄そのもの）
    right_name: Option<String>,
    /// 画面左側ページのファイル名。見開きでなければNone
    left_name: Option<String>,
}

pub struct ViewerState {
    archive_path: PathBuf,
    entries: Vec<ViewerEntry>,
    /// 見開き基点ページ（常に偶数。単ページ時はそのままページ番号）
    spread_base: i32,
    /// オフセット状態。spread_lo() = spread_base + offset.value()
    offset: SpreadOffset,
    textures: HashMap<usize, egui::TextureHandle>,
    /// 各GPUテクスチャがどのデコード世代から作られたか。
    texture_generations: HashMap<usize, u64>,
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
    /// 左右端ページ送りマーカーのホバー状態: (左端か, ホバー開始時刻)。
    /// フェードインのアルファ計算に使う。ゾーン外に出る/送り不可になると None に戻る。
    edge_turn_hover: Option<(bool, f64)>,
    /// 左エントリリストを最後に現在地へスクロールした spread_lo。
    /// 非表示中は更新せず、再表示時またはページ変更時だけ現在行を中央へ寄せる。
    entry_list_scrolled_lo: Option<i32>,
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
    /// 見開き原寸表示で最後にスクロール位置を初期化した spread_lo。
    /// 見開きが実際に切り替わった最初のフレームでだけ scroll_offset を
    /// 明示セットするための重複防止フラグ（毎フレームセットすると
    /// ユーザーのドラッグ/ホイール操作を毎回上書きしてしまう）。
    spread_actual_scrolled_lo: Option<i32>,
    /// フェーズ2: 直近フレームで実描画したサムネイルバーの可視インデックス範囲
    /// (原始インデックス、両端含む)。enqueue の優先範囲としても使う。
    /// None の間は仮想化描画がまだ一度も走っていない（起動直後の1フレーム分）。
    thumbbar_visible_range: Option<(i32, i32)>,
    /// 保存済み見開き状態のキャッシュ（app側がopen_viewer時にセット/操作後に更新）
    saved_spread: Option<(PageMode, i32)>,
    /// 保存済みソート条件のキャッシュ。None は保存OFFを表す。
    saved_sort: Option<(ViewerSortKey, bool)>,
    /// 保存メニューでのユーザー操作要求（1フレームで消費してViewerOutputへ渡す）
    pending_spread_action: Option<crate::controller::SpreadSaveAction>,
    /// ソート保存メニューでのユーザー操作要求（1フレームで消費）
    pending_sort_action: Option<crate::controller::SortSaveAction>,
    /// しおり保存の有効/無効状態のキャッシュ（app側がopen_viewer時にセット/操作後に更新）
    saved_bookmark_enabled: bool,
    /// しおり保存メニューでのユーザー操作要求（1フレームで消費）
    pending_bookmark_action: Option<crate::controller::BookmarkSaveAction>,
    /// 最後に右クリック座標から解決した実ページ(entry_name, display_name)。
    thumbnail_context_entry: Option<(String, String)>,
    /// DBから復元した登録サムネイル。Noneはデフォルト。
    saved_thumbnail_selection: Option<crate::spread_state::ThumbnailSelection>,
    pending_thumbnail_action: Option<crate::controller::ThumbnailSaveAction>,
    /// 右クリックメニュー「お気に入りに追加」が押されたか（1フレームで消費）
    pending_favorite_add: bool,
    /// 右クリックメニュー「ファイル詳細」が押されたか（1フレームで消費）
    pending_open_file_detail: bool,
    /// 右クリックメニュー「スライドショー」チェックボックスが操作されたか（1フレームで消費）
    pending_slideshow_toggle: bool,
    /// 右クリックメニュー「ブルーライトカット」チェックボックス表示用。show()冒頭でcfgから
    /// 同期する（真の状態はViewerConfig.image_filter.color_filter_modeが持つ）。
    blc_active: bool,
    /// 右クリックメニュー「ブルーライトカット」チェックボックスが操作されたか（1フレームで消費）
    pending_blc_toggle: bool,
    /// ファイル詳細ダイアログの状態。Some の間、draw_file_detail_dialogが表示する
    file_detail_dialog: Option<FileDetailDialogState>,
    /// OCR/翻訳子ウィンドウが現在開いているか。show()呼び出し時に外部(NekoviewApp)から
    /// 渡され、ツールバーのトグルボタン表示にのみ使う（真の状態はapp側が持つ）。
    translate_window_open: bool,
    /// ツールバーの翻訳トグルボタンを押せる状態か（疎通確認済み・翻訳モデル選択済み等）。
    translate_toggle_enabled: bool,
    /// ツールバーの翻訳トグルボタンが押されたか（1フレームで消費してViewerOutputへ渡す）
    pending_toggle_translate_window: bool,
    /// TODO項目B: 現在ページの手動回転状態。rotation_carry_over が true の間は
    /// この値ではなく ViewerConfig::rotation_session_angle を使う（呼び出し側判断）。
    rotation: RotationState,
    /// TODO項目D相当のダミーフラグ。D本体（設定UI・永続化）は未実装のため常時true固定
    /// （EXIF自動回転は既にデコード時にピクセルへ焼き込み済みのため、Bの範囲では
    /// このフラグを実際の分岐には使わない）。
    #[allow(dead_code)]
    exif_enabled: bool,
    /// スライドショー実行中か（非永続・実行時のみ）。
    slideshow_active: bool,
    /// 次に自動ページ送りするまでの基準時刻。start_slideshow/手動リセット/tick成功のたびに更新。
    slideshow_last_advance: Instant,
    /// tick_slideshow がページを送った直後だけ true。update_animation の変化検知で
    /// 「今回のページ変化はスライドショー自身によるものか」を判定するためのワンショットフラグ。
    slideshow_auto_advance_pending: bool,
    /// ビューアー内ツールパレット（オーバーレイ）の実行時状態。座標・LOCK・透過度・
    /// 可視性・マス内容。cfg.tool_paletteとの同期はpoll_tool_palette_debounce参照。
    tool_palette: crate::tool_palette::PaletteState,
    /// 展開中のDialog型マスのindex。Noneなら閉じている。同じマスを再クリックするか
    /// 展開領域外をクリックすると閉じる（1個の状態のみ保持＝同時に開けるのは1マス分）。
    tool_palette_open_dialog: Option<usize>,
    /// 起動後の初回フレームで cfg.tool_palette から self.tool_palette を読み込んだか。
    tool_palette_initialized: bool,
    /// self.tool_palette が最後に変化した時刻。PERSIST_DEBOUNCE_MS 経過したら
    /// cfg.tool_palette へ確定反映し、ViewerOutput経由でapp側にpersist_state()を促す。
    tool_palette_last_changed: Option<Instant>,
    /// 自動ハイドが確定する時刻（ポインタがパレット外へ出た時刻＋猶予）。
    /// Noneはポインタがパレット内／ロック中／ダイアログ展開中でタイマー無効。
    tool_palette_auto_hide_at: Option<f64>,
    /// true = 自動ハイドにより現在無描画状態。ポインタがパレット矩形に戻ると解除される。
    tool_palette_auto_hidden: bool,
    /// true = マス右クリックメニューのいずれかが展開中（前フレーム時点）。
    /// 展開中はポインタがパレット矩形の外に出ても自動ハイドタイマーを止め、
    /// かつパレット外でのクリック（メニューを閉じるためのクリック）を
    /// ページ送り／画像クリック／サムネ選択メニュー起動へ伝播させないためのガードに使う。
    tool_palette_menu_open: bool,
    /// 虫眼鏡モード（ホイール拡縮）の表示状態。モードOFF・非対応の表示（見開き・回転）・
    /// テクスチャ未取得の間は None。
    magnifier_view: Option<crate::magnifier::MagnifierView>,
    /// `magnifier_view` を作った対象ページ。ページが変わったらフィット表示から作り直す。
    magnifier_page: i32,
    /// フィット表示のまま（窓サイズ変更にフィット倍率で追従する）か。
    magnifier_at_fit: bool,
    /// このフレームで ScrollArea へ scroll_offset を押し込む必要があるか（拡縮・作り直し・追従した）。
    magnifier_offset_dirty: bool,
    /// `magnifier_view` に反映済みのテクスチャ寸法。原寸デコードへの差し替えで寸法が変わったとき、
    /// 画面上の見た目の大きさを保つよう倍率を換算するために使う。
    magnifier_img_size: egui::Vec2,
    /// スライダーバーを最後に操作・ホバーした時刻（自動ハイドの起点）。None = 作り直し直後で、
    /// 次の描画でその時刻を起点にする（モードON直後・ページ送り直後は必ず見える）。
    magnifier_bar_active_at: Option<f64>,
    /// ノッチ倍率／目盛りの詳細・簡易を切り替えた。次の `ViewerOutput` で保存を促して下ろす。
    magnifier_settings_dirty: bool,
    /// 虫眼鏡カーソルの画像キャッシュ（一辺のpxごと）。egui-winit は Arc のポインタで
    /// 同一画像を判定して再アップロードを避けるので、毎フレーム作り直さない。
    magnifier_cursor: Option<(u32, egui::CustomCursorImage)>,
    /// GPUテクスチャの1辺上限（毎フレーム ctx から取り込む）。デコード目標のクランプに使う。
    max_texture_side: usize,
}

impl ViewerState {
    // ── 読み取り専用アクセサ ─────────────────────────────────────────────────
    pub fn archive_path(&self) -> &PathBuf { &self.archive_path }
    pub fn entries(&self) -> &[ViewerEntry] { &self.entries }
    pub fn is_raw_file(&self) -> bool { self.is_raw_file }

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
    /// zoom_actual時は `max_decode_edge`（見開き中はその2倍）を長辺上限にする、
    /// それ以外は直近の描画領域サイズ(物理px)を上限にする。
    /// （旧実装は zoom_actual 時に None=無制限を返しており、"原寸時に許容する最大長辺幅"
    /// 設定が「ウィンドウ追従」ON時には効かないままになる抜け穴があったため統一した）
    pub fn current_decode_target(&self, zoom_actual: bool, max_decode_edge: u32, magnifier_on: bool) -> Option<(u32, u32)> {
        if zoom_actual {
            let edge = self.actual_decode_edge(max_decode_edge);
            Some((edge, edge))
        } else if magnifier_on && self.page_mode == PageMode::Single {
            // 虫眼鏡（単ページ）: 見開きの2倍は掛けず、1ページを max_decode_edge までデコードする。
            let edge = clamp_decode_edge(max_decode_edge, self.max_texture_side);
            Some((edge, edge))
        } else {
            let (w, h) = self.content_px;
            Some((clamp_decode_edge(w, self.max_texture_side), clamp_decode_edge(h, self.max_texture_side)))
        }
    }

    /// 「原寸」のデコード長辺上限。見開き中は2倍にし、GPUテクスチャの1辺上限で頭打ちにする。
    pub fn actual_decode_edge(&self, max_decode_edge: u32) -> u32 {
        let edge = if self.page_mode != PageMode::Single {
            max_decode_edge.saturating_mul(2)
        } else {
            max_decode_edge
        };
        clamp_decode_edge(edge, self.max_texture_side)
    }

    /// 世代非依存アニメのリサイズ切替で保持すべき、現在表示中のフレーム番号。
    pub(crate) fn animation_frame_index(&self, original_index: usize) -> usize {
        self.anim_states
            .get(&original_index)
            .map_or(0, |state| state.frame_index)
    }

    /// フェーズ6: 再デコード発火時に、指定ページのテクスチャ・アニメ再生状態を破棄する。
    /// 次の update_textures() で PageCache から作り直させる（アニメはフレーム0から再生し直す）。
    /// 項目(D): Exif Orientation ON/OFF切替時に、開いているアーカイブの全ページのテクスチャ・
    /// アニメ再生状態を破棄する。`invalidate_pages`(可視ページのみ)と違い、先読みウィンドウ内
    /// (update_textures の表示ウィンドウ)で既にテクスチャを持つ裏ページも古いOrientationの
    /// まま残ってしまうため、開いているアーカイブ全体を対象にする。
    pub fn invalidate_all_pages(&mut self) {
        self.textures.clear();
        self.texture_generations.clear();
        self.anim_states.clear();
    }

    /// 画像処理フィルター変更時の再デコード用。`anim_states`に登録済み（＝アニメーションとして
    /// 再生中）のページは textures/texture_generations も保持し、静止画ページのぶんだけを
    /// 無効化する。`anim_states`自体は一切クリアしない（クリアするとframe_indexが失われ、
    /// 再生位置が先頭へ戻ってしまうため）。見開きで片方が静止画・片方がアニメの場合でも、
    /// 静止画側だけが対象になる。
    pub fn invalidate_static_pages(&mut self) {
        let animated_pages = &self.anim_states;
        self.textures.retain(|orig_i, _| animated_pages.contains_key(orig_i));
        self.texture_generations.retain(|orig_i, _| animated_pages.contains_key(orig_i));
    }

    pub fn new(archive_path: PathBuf, slots: [Option<WindowSlot>; 4], default_slot: Option<usize>) -> Option<Self> {
        let image_entries = archive::list_images(&archive_path);
        if image_entries.is_empty() {
            return None;
        }
        Some(Self::from_image_entries(archive_path, image_entries, slots, default_slot))
    }

    /// 一覧取得済みの`ImageEntry`から構築する。非同期（進捗通知つき）で
    /// `archive::list_images_with_progress`を実行した結果を渡すための経路。
    /// 呼び出し側は事前に`image_entries`が空でないことを確認しておくこと。
    pub fn from_image_entries(
        archive_path: PathBuf,
        image_entries: Vec<archive::ImageEntry>,
        slots: [Option<WindowSlot>; 4],
        default_slot: Option<usize>,
    ) -> Self {
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
        Self {
            archive_path,
            entries,
            spread_base: 0,
            offset: SpreadOffset::new(),
            textures: HashMap::new(),
            texture_generations: HashMap::new(),
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
            edge_turn_hover: None,
            entry_list_scrolled_lo: None,
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
            spread_actual_scrolled_lo: None,
            thumbbar_visible_range: None,
            saved_spread: None,
            saved_sort: None,
            pending_spread_action: None,
            pending_sort_action: None,
            saved_bookmark_enabled: false,
            pending_bookmark_action: None,
            thumbnail_context_entry: None,
            saved_thumbnail_selection: None,
            pending_thumbnail_action: None,
            pending_favorite_add: false,
            pending_open_file_detail: false,
            pending_slideshow_toggle: false,
            blc_active: false,
            pending_blc_toggle: false,
            file_detail_dialog: None,
            translate_window_open: false,
            translate_toggle_enabled: false,
            pending_toggle_translate_window: false,
            rotation: RotationState::new(),
            exif_enabled: true,
            slideshow_active: false,
            slideshow_last_advance: Instant::now(),
            slideshow_auto_advance_pending: false,
            tool_palette: crate::tool_palette::PaletteState::default(),
            tool_palette_open_dialog: None,
            tool_palette_initialized: false,
            tool_palette_last_changed: None,
            tool_palette_auto_hide_at: None,
            tool_palette_auto_hidden: false,
            tool_palette_menu_open: false,
            magnifier_view: None,
            magnifier_page: 0,
            magnifier_at_fit: true,
            magnifier_offset_dirty: false,
            magnifier_img_size: egui::Vec2::ZERO,
            magnifier_bar_active_at: None,
            magnifier_settings_dirty: false,
            magnifier_cursor: None,
            max_texture_side: MAX_TEXTURE_SIDE_FALLBACK,
        }
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
            texture_generations: HashMap::new(),
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
            edge_turn_hover: None,
            entry_list_scrolled_lo: None,
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
            spread_actual_scrolled_lo: None,
            thumbbar_visible_range: None,
            saved_spread: None,
            saved_sort: None,
            pending_spread_action: None,
            pending_sort_action: None,
            saved_bookmark_enabled: false,
            pending_bookmark_action: None,
            thumbnail_context_entry: None,
            saved_thumbnail_selection: None,
            pending_thumbnail_action: None,
            pending_favorite_add: false,
            pending_open_file_detail: false,
            pending_slideshow_toggle: false,
            blc_active: false,
            pending_blc_toggle: false,
            file_detail_dialog: None,
            translate_window_open: false,
            translate_toggle_enabled: false,
            pending_toggle_translate_window: false,
            rotation: RotationState::new(),
            exif_enabled: true,
            slideshow_active: false,
            slideshow_last_advance: Instant::now(),
            slideshow_auto_advance_pending: false,
            tool_palette: crate::tool_palette::PaletteState::default(),
            tool_palette_open_dialog: None,
            tool_palette_initialized: false,
            tool_palette_last_changed: None,
            tool_palette_auto_hide_at: None,
            tool_palette_auto_hidden: false,
            tool_palette_menu_open: false,
            magnifier_view: None,
            magnifier_page: 0,
            magnifier_at_fit: true,
            magnifier_offset_dirty: false,
            magnifier_img_size: egui::Vec2::ZERO,
            magnifier_bar_active_at: None,
            magnifier_settings_dirty: false,
            magnifier_cursor: None,
            max_texture_side: MAX_TEXTURE_SIDE_FALLBACK,
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

    /// 現在表示中の先頭ページのentry_name（しおり保存用）。範囲外（先頭仮想ページ等）は None。
    pub fn current_bookmark_entry_name(&self) -> Option<&str> {
        let idx = self.spread_lo();
        if idx < 0 {
            return None;
        }
        self.entries.get(idx as usize).map(|e| e.entry_name.as_str())
    }

    /// しおり復帰：entry_nameが現在の一覧（ソート確定後）に見つかれば該当ページへ
    /// ジャンプしてtrueを返す。見つからなければ何もせずfalseを返す
    /// （呼び出し側でしおりデータ初期化＋失敗トーストへ）。
    pub fn restore_bookmark_position(&mut self, entry_name: &str) -> bool {
        let Some(idx) = self.entries.iter().position(|e| e.entry_name == entry_name) else {
            return false;
        };
        self.spread_base = idx as i32;
        self.offset.reset();
        self.anim_active = false;
        self.anim_progress = 1.0;
        self.prev_spread_lo = self.spread_base;
        self.rotation.reset();
        true
    }

    /// オフセットがずれているか（UI表示用）
    pub fn can_shift_forward(&self) -> bool {
        self.offset.can_advance()
            && self.spread_lo() + 1 <= self.entries.len() as i32 - 1
    }

    pub fn can_shift_backward(&self) -> bool {
        self.offset.can_retreat() && self.spread_lo() - 1 >= -1
    }

    pub fn shift_offset_forward(&mut self) {
        if self.can_shift_forward() {
            self.offset.advance();
        }
    }

    pub fn shift_offset_backward(&mut self) {
        if self.can_shift_backward() {
            self.offset.retreat();
        }
    }

    /// 次の見開き/ページへ進めるか（オフセットを保持したまま次のspread_baseが範囲内か）。
    /// 通常のキー/ホイール送りと左右端クリック送りの両方から共有される判定。
    fn can_advance_page(&self, step: i32, total: i32) -> bool {
        self.spread_base + step + self.offset.value() <= total - 1
    }

    /// 前の見開き/ページへ戻れるか
    fn can_retreat_page(&self, is_spread: bool, step: i32) -> bool {
        let min_lo = if is_spread { -1 } else { 0 };
        self.spread_base - step + self.offset.value() >= min_lo
    }

    /// 次の見開き/ページへ進む（不可能な場合は何もしない）
    fn advance_page(&mut self, step: i32, total: i32) {
        if self.can_advance_page(step, total) {
            self.spread_base += step;
        }
    }

    // ── スライドショー ───────────────────────────────────────────────────────
    // 右クリックメニュー・将来のビューアー直接操作ボタンなど複数経路から呼ばれる
    // ことを想定し、advance_spread_step と同様にコントローラ層を介さず ViewerState に
    // 直接 pub メソッドとして生やす（永続化を伴わないランタイム操作のため）。

    /// スライドショーが実行中か
    pub fn is_slideshow_active(&self) -> bool {
        self.slideshow_active
    }

    /// スライドショーを開始する（タイマーを今から起算）
    pub fn start_slideshow(&mut self) {
        self.slideshow_active = true;
        self.slideshow_last_advance = Instant::now();
    }

    /// スライドショーを停止する
    pub fn stop_slideshow(&mut self) {
        self.slideshow_active = false;
    }

    /// 実行中なら停止、停止中なら開始する
    pub fn toggle_slideshow(&mut self) {
        if self.slideshow_active {
            self.stop_slideshow();
        } else {
            self.start_slideshow();
        }
    }

    /// 毎フレーム呼ぶ。実行中かつ間隔が経過していたら1ページ分自動で送る。
    /// 送れない（終端到達）場合はスライドショーを自動停止する。
    /// 実際に送った場合は slideshow_auto_advance_pending を立て、次の
    /// update_animation でのページ変化検知が「手動操作」と誤認しないようにする。
    fn tick_slideshow(&mut self, ctx: &egui::Context, cfg: &ViewerConfig, total: usize) {
        if !self.slideshow_active {
            return;
        }
        let interval = Duration::from_millis(cfg.slideshow_interval_ms);
        let elapsed = self.slideshow_last_advance.elapsed();
        if elapsed < interval {
            ctx.request_repaint_after(interval - elapsed);
            return;
        }
        let is_spread = self.page_mode != PageMode::Single;
        let step = if is_spread { 2i32 } else { 1i32 };
        let total_i = total as i32;
        if self.can_advance_page(step, total_i) {
            self.advance_page(step, total_i);
            self.slideshow_auto_advance_pending = true;
            self.slideshow_last_advance = Instant::now();
            ctx.request_repaint();
        } else {
            // 終端到達: 自動停止
            self.slideshow_active = false;
        }
    }

    /// 手動でのページ変化を検知したときの処理（update_animation から呼ばれる）。
    /// 設定に応じてタイマーをリセットして継続するか、スライドショー自体を止める。
    fn on_manual_page_change(&mut self, cfg: &ViewerConfig) {
        if !self.slideshow_active {
            return;
        }
        match cfg.slideshow_manual_behavior {
            SlideshowManualBehavior::ResetTimer => {
                self.slideshow_last_advance = Instant::now();
            }
            SlideshowManualBehavior::Stop => {
                self.slideshow_active = false;
            }
        }
    }

    /// 現在有効なトランジション種類。スライドショー実行中は専用設定、それ以外は通常設定を使う。
    fn effective_transition_kind(&self, cfg: &ViewerConfig) -> TransitionKind {
        if self.slideshow_active { cfg.slideshow_transition_kind } else { cfg.transition_kind }
    }

    /// 現在有効なトランジション遷移時間(ms)。判定基準は effective_transition_kind と同じ。
    fn effective_transition_duration_ms(&self, cfg: &ViewerConfig) -> u64 {
        if self.slideshow_active { cfg.slideshow_transition_duration_ms } else { cfg.transition_duration_ms }
    }

    /// 前の見開き/ページへ戻る（不可能な場合は何もしない）
    fn retreat_page(&mut self, is_spread: bool, step: i32) {
        if self.can_retreat_page(is_spread, step) {
            self.spread_base -= step;
        }
    }

    /// OCR/翻訳子ウィンドウ用: 見開きを1組ぶん(step)実際に送る/戻す。
    /// オフセット(-1/0/+1)には一切触れない（4/5キーの見開き内シフトとは無関係）。
    /// 呼び出し側（子ウィンドウ）が「見開き内の2ページを見終わった」と判定した時にだけ
    /// 呼ぶことで、子2回操作＝親1回送りを実現する（シングルページモードはstep=1で毎回呼ぶ）。
    pub fn advance_spread_step(&mut self, total: usize, forward: bool) {
        let is_spread = self.page_mode != PageMode::Single;
        let step: i32 = if is_spread { 2 } else { 1 };
        let total_i = total as i32;
        let off = self.offset.value();

        if forward {
            let next_base = self.spread_base + step;
            if next_base + off <= total_i - 1 { self.spread_base = next_base; }
        } else {
            let prev_base = self.spread_base - step;
            let min_lo = if is_spread { -1 } else { 0 };
            if prev_base + off >= min_lo { self.spread_base = prev_base; }
        }
        self.offset.update_virtual_right(is_spread && self.spread_lo() + 1 >= total_i);
    }

    /// ページモードを切り替え、spread_base とオフセットを整合させる
    pub fn set_page_mode(&mut self, mode: PageMode, _cfg: &mut ViewerConfig) {
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
        match normalize_saved_spread_offset(offset_value) {
            -1 => self.offset.force_virtual_left(),
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
            Some((mode, offset)) => {
                mode != self.page_mode
                    || normalize_saved_spread_offset(offset)
                        != normalize_saved_spread_offset(self.offset.value())
            }
        }
    }

    /// 現在のソート条件を保存値と同じ形式で返す。
    pub fn current_sort_snapshot(&self) -> (ViewerSortKey, bool) {
        (self.sort_key, self.sort_ascending)
    }

    /// 保存済みソートをビューアー生成直後に適用する。
    /// レコードがない場合は呼ばれないため、従来の初期化経路には介入しない。
    pub fn restore_saved_sort(&mut self, key: ViewerSortKey, ascending: bool) {
        if self.is_raw_file || self.current_sort_snapshot() == (key, ascending) {
            return;
        }
        self.sort_key = key;
        self.sort_ascending = ascending;
        self.sort_entries();
    }

    /// app側がDBの読み込み・保存結果をViewerStateへ反映する。
    pub fn set_saved_sort(&mut self, value: Option<(ViewerSortKey, bool)>) {
        self.saved_sort = value;
    }

    pub fn sort_save_toggle_on(&self) -> bool {
        self.saved_sort.is_some()
    }

    pub fn sort_save_toggle_enabled(&self) -> bool {
        !self.is_raw_file
    }

    /// 保存ONで、現在値が保存済み値から変わっている場合だけtrue。
    pub fn sort_save_changed(&self) -> bool {
        self.saved_sort
            .is_some_and(|saved| saved != self.current_sort_snapshot())
    }

    /// 保存を解除する。現在のソート条件とページ位置は変更しない。
    pub fn clear_saved_sort(&mut self) {
        self.saved_sort = None;
    }

    pub fn take_sort_action(&mut self) -> Option<crate::controller::SortSaveAction> {
        self.pending_sort_action.take()
    }

    /// app側がDBの読み込み・保存結果をViewerStateへ反映する。
    pub fn set_saved_bookmark_enabled(&mut self, enabled: bool) {
        self.saved_bookmark_enabled = enabled;
    }

    pub fn bookmark_save_toggle_on(&self) -> bool {
        self.saved_bookmark_enabled
    }

    pub fn bookmark_save_toggle_enabled(&self) -> bool {
        !self.is_raw_file
    }

    pub fn take_bookmark_action(&mut self) -> Option<crate::controller::BookmarkSaveAction> {
        self.pending_bookmark_action.take()
    }

    pub fn set_saved_thumbnail_selection(
        &mut self,
        selection: Option<crate::spread_state::ThumbnailSelection>,
    ) {
        self.saved_thumbnail_selection = selection;
    }

    pub fn take_thumbnail_action(&mut self) -> Option<crate::controller::ThumbnailSaveAction> {
        self.pending_thumbnail_action.take()
    }

    /// メニュー操作で立てられた保存要求を取り出す（1フレームで消費）
    pub fn take_spread_action(&mut self) -> Option<crate::controller::SpreadSaveAction> {
        self.pending_spread_action.take()
    }

    /// 右クリックメニュー「お気に入りに追加」の要求を取り出す（1フレームで消費）
    pub fn take_favorite_add_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_favorite_add)
    }

    /// 右クリックメニュー「ブルーライトカット」チェックボックス表示用の現在状態。
    fn is_blc_active(&self) -> bool {
        self.blc_active
    }

    /// ツールバーの翻訳トグルボタンが押された要求を取り出す（1フレームで消費）
    fn take_translate_toggle_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_toggle_translate_window)
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

    /// 右クリック「ファイル詳細」が押された1フレーム後、開いた瞬間の情報をスナップショットして
    /// ダイアログ状態を確定する（以後のページ送りには追従しない）。
    fn maybe_open_file_detail_dialog(&mut self) {
        if !std::mem::take(&mut self.pending_open_file_detail) {
            return;
        }
        if self.is_raw_file {
            self.file_detail_dialog = Some(FileDetailDialogState {
                archive_name: None,
                right_name: Some(self.title()),
                left_name: None,
            });
            return;
        }
        let total = self.entries.len() as i32;
        let get = |i: i32| -> Option<String> {
            if i < 0 || i >= total { return None; }
            self.entries.get(i as usize).map(|e| e.entry_name.clone())
        };
        let lo = self.spread_lo();
        // 画面上の左右位置基準（SpreadRight=右綴じでは lo が画面右側に来る。render_spread の
        // 実際の引数順（tex_left, tex_right）と揃える）。
        let (right_name, left_name) = match self.page_mode {
            PageMode::Single => (get(lo.clamp(0, total - 1)), None),
            PageMode::SpreadLeft => (get(lo + 1), get(lo)),
            PageMode::SpreadRight => (get(lo), get(lo + 1)),
        };
        self.file_detail_dialog = Some(FileDetailDialogState {
            archive_name: Some(self.title()),
            right_name,
            left_name,
        });
    }

    /// ファイル詳細ダイアログを描画する（ビューアー窓自身のCtx内、表示のみ・編集不可）。
    fn draw_file_detail_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.file_detail_dialog.clone() else {
            return;
        };
        let t = i18n::t();
        let mut close = false;
        egui::Window::new(t.file_detail_dialog_title())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .max_width(420.0)
            .show(ctx, |ui| {
                ui.label(t.file_detail_archive_label());
                ui.add(egui::Label::new(dialog.archive_name.as_deref().unwrap_or("")).wrap());
                ui.add_space(8.0);
                if let Some(left_name) = &dialog.left_name {
                    ui.label(t.file_detail_entry_right_label());
                    ui.add(egui::Label::new(dialog.right_name.as_deref().unwrap_or("")).wrap());
                    ui.add_space(4.0);
                    ui.label(t.file_detail_entry_left_label());
                    ui.add(egui::Label::new(left_name.as_str()).wrap());
                } else {
                    ui.label(t.file_detail_entry_label());
                    ui.add(egui::Label::new(dialog.right_name.as_deref().unwrap_or("")).wrap());
                }
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    if ui.button(t.file_detail_close()).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.file_detail_dialog = None;
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        page_cache: &mut PageCache,
        active_generation: u64,
        preparing_generation: Option<u64>,
        cfg: &mut ViewerConfig,
        keymap: &Keymap,
        translate_window_open: bool,
        translate_toggle_enabled: bool,
    ) -> ViewerOutput {
        self.translate_window_open = translate_window_open;
        self.translate_toggle_enabled = translate_toggle_enabled;
        let ctx = ui.ctx().clone();
        let viewer_style = ui.style().clone();
        if !self.open || self.entries.is_empty() {
            return ViewerOutput { nav: ViewerNav::None, close_requested: !self.open, save_slots: None, spread_save_action: None, sort_save_action: None, thumbnail_save_action: None, bookmark_save_action: None, favorite_add_requested: false, toggle_translate_window: false, tool_palette_changed: false, magnifier_settings_changed: false };
        }

        // ── フレーム入力を一括収集（ctx.input はこの1回のみ）────────────────
        let mut input = FrameInput::collect(&ctx, keymap);

        // フェーズ6: リサイズ再デコードのターゲットサイズ算出用に、現在の描画領域サイズ（物理px）を記録する。
        let screen = ctx.content_rect().size() * ctx.pixels_per_point();
        self.content_px = (screen.x.max(1.0) as u32, screen.y.max(1.0) as u32);
        self.max_texture_side = ctx.input(|i| i.max_texture_side);

        // 既定スロットを初回フレームで一度だけ適用（クランプ付き）。
        self.apply_default_slot(&ctx, input.monitor_size);

        // ツールパレットの状態を初回フレームで一度だけ cfg から読み込む（起動時の復元）。
        if !self.tool_palette_initialized {
            self.tool_palette_initialized = true;
            self.tool_palette = cfg.tool_palette.clone();
        }

        // 右クリックメニューのスライドショーチェックボックス操作を反映する。
        if self.pending_slideshow_toggle {
            self.pending_slideshow_toggle = false;
            self.toggle_slideshow();
        }

        // 右クリックメニューのブルーライトカットチェックボックス操作を反映する。ON/OFFの
        // 切り替えのみ行い、色温度の数値(cfg.image_filter.blc_color_temperature_k)には触れない
        // （次にONへ戻したとき直前の値がそのまま復元される）。
        if self.pending_blc_toggle {
            self.pending_blc_toggle = false;
            cfg.image_filter.color_filter_mode = if cfg.image_filter.color_filter_mode
                == crate::image_filter::ColorFilterMode::BlueLightCut
            {
                crate::image_filter::ColorFilterMode::None
            } else {
                crate::image_filter::ColorFilterMode::BlueLightCut
            };
        }
        self.blc_active = cfg.image_filter.color_filter_mode == crate::image_filter::ColorFilterMode::BlueLightCut;

        // スライドショーのタイマー送りは update_animation より先に行い、同一フレームで
        // ページ変化検知（アニメ起動・手動/自動の判定）が反映されるようにする。
        self.tick_slideshow(&ctx, cfg, self.entries.len());

        let (animating, t) = self.update_animation(&ctx, input.dt, cfg);

        self.update_textures(
            &ctx,
            page_cache,
            active_generation,
            preparing_generation,
        );

        let total = self.entries.len();
        let current_lo = self.spread_lo();
        let (tex_lo, tex_hi) = self.page_textures_for(current_lo);
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

        // OSネイティブのタイトルバーは固定の英字文字列にする（Wayland/GNOME環境の
        // CSDフォールバック描画がCJKグリフを持たないシステムフォントに解決されると
        // 豆腐になるため。実ファイル名は右クリック「ファイル詳細」で確認できる）。
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("Nekoviewer - Imageview".to_owned()));

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
        // 虫眼鏡は現状、単ページ・回転なしのみ対応（見開き・回転はフェーズ7）。
        let magnifier_active = cfg.magnifier_on && !is_spread && rotation_angle == 0 && tex_lo.is_some();
        if magnifier_active {
            // ホイールはページ送りではなく拡縮へ回す。
            input.scroll_delta = 0.0;
        } else {
            input.wheel_notches = 0.0;
        }
        let frame = RenderFrame {
            tex_lo, tex_hi, prev_tex_lo, prev_tex_hi,
            animating,
            t,
            anim_dir_f:  self.anim_dir as f32,
            anim_from_lo: self.anim_from_lo,
            current_lo,
            page_mode:   self.page_mode,
            zoom_actual: cfg.zoom_actual,
            magnifier:   magnifier_active,
            monitor:     input.monitor_size,
            rotation_angle,
            transition_kind: self.effective_transition_kind(cfg),
        };
        let tool_palette_before = self.tool_palette.clone();
        let (double_clicked, single_clicked) = self.draw_central_panel(ui, &frame, &input, is_spread, step, total, cfg);
        if self.tool_palette != tool_palette_before {
            self.tool_palette_last_changed = Some(Instant::now());
        }
        let tool_palette_changed = self.poll_tool_palette_debounce(cfg, &ctx);

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

        let close_self = self.process_misc_input(&ctx, &input, double_clicked, cfg);

        self.tick_toast(&ctx, input.time);

        let spread_save_action = self.take_spread_action();
        let sort_save_action = self.take_sort_action();
        let thumbnail_save_action = self.take_thumbnail_action();
        let bookmark_save_action = self.take_bookmark_action();
        let favorite_add_requested = self.take_favorite_add_request();
        let toggle_translate_window = self.take_translate_toggle_request();
        self.maybe_open_file_detail_dialog();
        self.draw_file_detail_dialog(&ctx);
        ViewerOutput { nav, close_requested: close_self, save_slots, spread_save_action, sort_save_action, thumbnail_save_action, bookmark_save_action, favorite_add_requested, toggle_translate_window, tool_palette_changed, magnifier_settings_changed: std::mem::take(&mut self.magnifier_settings_dirty) }
    }

    /// ツールパレットの変更確定処理。image_filterのpoll_image_filter_debounceと同じ考え方で、
    /// cfg.tool_palette.visible の外部変更（エクスプローラーメニューのトグル等）をライブ反映する。
    /// self.tool_palette 全体ではなくvisibleのみ同期する（ドラッグ中の座標やマス内容など、
    /// ローカルで編集中かもしれない他フィールドを巻き戻さないため）。
    pub fn sync_tool_palette_visible(&mut self, visible: bool) {
        self.tool_palette.visible = visible;
    }

    /// self.tool_palette が最後に変化してからPERSIST_DEBOUNCE_MS経過したらcfgへ書き込む
    /// （ドラッグ中の連続した座標変化のたびにディスク書き込みが走るのを防ぐ）。
    /// 確定した瞬間だけtrueを返し、呼び出し元(app側)にpersist_state()を促す。
    fn poll_tool_palette_debounce(&mut self, cfg: &mut ViewerConfig, ctx: &egui::Context) -> bool {
        let Some(changed_at) = self.tool_palette_last_changed else { return false };
        let debounce = Duration::from_millis(crate::tool_palette::PERSIST_DEBOUNCE_MS);
        let elapsed = changed_at.elapsed();
        if elapsed >= debounce {
            cfg.tool_palette = self.tool_palette.clone();
            self.tool_palette_last_changed = None;
            true
        } else {
            ctx.request_repaint_after(debounce - elapsed);
            false
        }
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
            self.prev_spread_lo = current_lo;
            // 表示画像が差し替わったので手動回転をリセット（角度引き継ぎトグルONの間は
            // cfg側の共有角度をそのまま使い続けるため、ここではリセットしない）
            if !cfg.rotation_carry_over {
                self.rotation.reset();
            }
            if self.effective_transition_kind(cfg) == TransitionKind::None {
                // トランジション無し設定：アニメーションを起動せず即時切り替えにする
                self.anim_progress = 1.0;
                self.anim_active = false;
            } else {
                self.anim_progress = 0.0;
                self.anim_active = true;
            }

            // スライドショー: 今回のページ変化が tick_slideshow 自身による送りでなければ
            // 手動操作とみなす（ワンショットフラグを見て消費する）。
            if self.slideshow_auto_advance_pending {
                self.slideshow_auto_advance_pending = false;
            } else {
                self.on_manual_page_change(cfg);
            }
        }

        if self.anim_active {
            let transition_secs = (self.effective_transition_duration_ms(cfg) as f32 / 1000.0).max(0.001);
            self.anim_progress = (self.anim_progress + dt / transition_secs).min(1.0);
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
                        self.draw_bar_items(ui, cfg);
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
                            self.draw_bar_items(ui, cfg);
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
            self.advance_page(step, total_i);
        }
        if key_prev || scroll_prev {
            self.retreat_page(is_spread, step);
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

    /// 自動ハイド：ポインタがパレット外へ出てからハイドが確定するまでの猶予(秒)。
    const TOOL_PALETTE_AUTO_HIDE_DELAY_SEC: f64 = 0.5;

    /// ツールパレットの自動ハイドを判定する。auto_hide_locked中／ポインタがパレット内
    /// （ミニUI展開中含む）／非表示中(visible=false)はタイマーを常にリセットし、
    /// それ以外でポインタが外れたまま TOOL_PALETTE_AUTO_HIDE_DELAY_SEC 秒経過したら
    /// 無描画状態（auto_hidden=true）に切り替える。
    fn tick_tool_palette_auto_hide(&mut self, ctx: &egui::Context, time: f64, pointer_in_palette: bool) {
        if !self.tool_palette.visible
            || self.tool_palette.auto_hide_locked
            || pointer_in_palette
            || self.tool_palette_open_dialog.is_some()
        {
            self.tool_palette_auto_hide_at = None;
            self.tool_palette_auto_hidden = false;
            return;
        }
        match self.tool_palette_auto_hide_at {
            None => {
                self.tool_palette_auto_hide_at = Some(time + Self::TOOL_PALETTE_AUTO_HIDE_DELAY_SEC);
                ctx.request_repaint_after(Duration::from_secs_f64(Self::TOOL_PALETTE_AUTO_HIDE_DELAY_SEC));
            }
            Some(at) if time >= at => {
                self.tool_palette_auto_hidden = true;
            }
            Some(at) => {
                ctx.request_repaint_after(Duration::from_secs_f64((at - time).max(0.0)));
            }
        }
    }

    // ── ツールパレット：見た目のサイズ定数（Phase1: 5x2固定グリッド） ──────
    const TOOL_PALETTE_GAP: f32 = 4.0;
    const TOOL_PALETTE_HEADER_H: f32 = 22.0;
    const TOOL_PALETTE_PAD: f32 = 6.0;
    const TOOL_PALETTE_BTN_W: f32 = 28.0;
    /// 透過度／サイズボタンの幅。数値を表示する通常時は広め、記号1文字だけの
    /// compact時（最小マスサイズ選択時）は他ボタンと同じ幅まで縮める。
    const TOOL_PALETTE_WIDE_BTN_W: f32 = Self::TOOL_PALETTE_BTN_W + 12.0;
    const TOOL_PALETTE_DRAG_MIN_W: f32 = 16.0;

    /// true = 最小マスサイズ選択中。ヒントで詳細値を見られる前提で、ヘッダーの
    /// 透過度／サイズ表示を記号1文字だけに縮め、ヘッダー最小幅をさらに削る。
    fn tool_palette_header_compact(&self) -> bool {
        self.tool_palette.slot_size_idx == 0
    }

    fn tool_palette_header_min_w(&self) -> f32 {
        let mid_w = if self.tool_palette_header_compact() {
            Self::TOOL_PALETTE_BTN_W
        } else {
            Self::TOOL_PALETTE_WIDE_BTN_W
        };
        Self::TOOL_PALETTE_BTN_W * 3.0 + mid_w * 2.0 + Self::TOOL_PALETTE_DRAG_MIN_W
    }

    fn tool_palette_grid_size(&self) -> egui::Vec2 {
        let cols = crate::tool_palette::GRID_COLS as f32;
        let rows = crate::tool_palette::GRID_ROWS as f32;
        let slot = self.tool_palette.slot_size_px();
        let grid_w = Self::TOOL_PALETTE_PAD * 2.0 + cols * slot + (cols - 1.0) * Self::TOOL_PALETTE_GAP;
        let grid_h = Self::TOOL_PALETTE_HEADER_H + Self::TOOL_PALETTE_PAD * 2.0 + rows * slot + (rows - 1.0) * Self::TOOL_PALETTE_GAP;
        egui::vec2(grid_w.max(self.tool_palette_header_min_w()), grid_h)
    }

    /// ツールパレット全体のスクリーン矩形（非表示なら None）。viewport 内に収まるよう
    /// 左上座標をクランプする（ドラッグで画面外に出た場合の保険）。
    fn tool_palette_rect(&self, viewport: egui::Rect) -> Option<egui::Rect> {
        if !self.tool_palette.visible {
            return None;
        }
        let size = self.tool_palette_grid_size();
        let max_x = (viewport.max.x - size.x).max(viewport.min.x);
        let max_y = (viewport.max.y - size.y).max(viewport.min.y);
        let min_x = self.tool_palette.pos.0.clamp(viewport.min.x, max_x);
        let min_y = self.tool_palette.pos.1.clamp(viewport.min.y, max_y);
        Some(egui::Rect::from_min_size(egui::pos2(min_x, min_y), size))
    }

    /// ツールパレットのオーバーレイ本体を描画する。子Ui＋Painter直描き方式
    /// （thumbbar_overlayと同じ流儀）。マスの登録内容の描画・実行はPhase2/3で追加する。
    fn draw_tool_palette(&mut self, ui: &mut egui::Ui, rect: egui::Rect, viewport: egui::Rect, is_spread: bool, step: i32, total: usize, cfg: &mut ViewerConfig) {
        let lang = crate::i18n::t();
        let bg_alpha = (self.tool_palette.opacity_pct as f32 / 100.0 * 220.0).round() as u8;
        ui.painter().rect_filled(rect, 6.0, egui::Color32::from_black_alpha(bg_alpha));

        let mut child = ui.new_child(egui::UiBuilder::new().id_salt("tool_palette_child").max_rect(rect));

        // パレット全体を覆う土台のinteract。マス間・ヘッダー余白などどの個別ウィジェットにも
        // 拾われない隙間のクリック／右クリックが背面（画像側の全面クリック領域）へ抜けて
        // 旧来の右クリックメニューが開いてしまうのを防ぐ。マス等の個別interactは後から
        // 登録されるためそちらが優先され、この土台は隙間だけを握りつぶす形になる。
        child.interact(rect, child.id().with("tp_backstop"), egui::Sense::click().union(egui::Sense::drag()));

        // ── ヘッダー帯：LOCK／透過度／自動ハイドLOCK／サイズ／ドラッグハンドル／✕ ──
        let compact = self.tool_palette_header_compact();
        let mid_w = if compact { Self::TOOL_PALETTE_BTN_W } else { Self::TOOL_PALETTE_WIDE_BTN_W };
        let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), Self::TOOL_PALETTE_HEADER_H));
        let lock_rect = egui::Rect::from_min_size(header_rect.min, egui::vec2(Self::TOOL_PALETTE_BTN_W, Self::TOOL_PALETTE_HEADER_H));
        let opacity_rect = egui::Rect::from_min_size(lock_rect.right_top(), egui::vec2(mid_w, Self::TOOL_PALETTE_HEADER_H));
        let auto_hide_lock_rect = egui::Rect::from_min_size(opacity_rect.right_top(), egui::vec2(Self::TOOL_PALETTE_BTN_W, Self::TOOL_PALETTE_HEADER_H));
        let size_rect = egui::Rect::from_min_size(auto_hide_lock_rect.right_top(), egui::vec2(mid_w, Self::TOOL_PALETTE_HEADER_H));
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.max.x - Self::TOOL_PALETTE_BTN_W, header_rect.min.y),
            egui::vec2(Self::TOOL_PALETTE_BTN_W, Self::TOOL_PALETTE_HEADER_H),
        );
        let drag_min_x = size_rect.max.x;
        let drag_max_x = close_rect.min.x;
        if drag_max_x > drag_min_x && !self.tool_palette.locked {
            let drag_rect = egui::Rect::from_min_max(
                egui::pos2(drag_min_x, header_rect.min.y),
                egui::pos2(drag_max_x, header_rect.max.y),
            );
            let drag_resp = child
                .interact(drag_rect, child.id().with("tp_drag"), egui::Sense::drag())
                .on_hover_text(lang.tool_palette_drag_hint());
            if drag_resp.dragged() {
                self.tool_palette.pos.0 += drag_resp.drag_delta().x;
                self.tool_palette.pos.1 += drag_resp.drag_delta().y;
            }
        }

        // システムボタン群（LOCK/透過度/サイズ/✕）自体も背景と同じ透過度に合わせる。
        // ヘッダーにポインタが乗っている間だけ不透明に戻し、操作時に見失わないようにする。
        let header_opacity = if child.rect_contains_pointer(header_rect) {
            1.0
        } else {
            self.tool_palette.opacity_pct as f32 / 100.0
        };
        child.scope(|ui| {
            ui.set_opacity(header_opacity);

            let lock_resp = ui
                .put(lock_rect, egui::Button::new(if self.tool_palette.locked { "🔒" } else { "🔓" }))
                .on_hover_text(lang.tool_palette_lock_hint());
            if lock_resp.clicked() {
                self.tool_palette.locked = !self.tool_palette.locked;
            }
            let opacity_label = if compact { "%".to_string() } else { format!("{}%", self.tool_palette.opacity_pct) };
            let opacity_resp = ui
                .put(opacity_rect, egui::Button::new(opacity_label))
                .on_hover_text(lang.tool_palette_opacity_hint(self.tool_palette.opacity_pct));
            if opacity_resp.clicked() {
                let next = self.tool_palette.opacity_pct + 10;
                self.tool_palette.opacity_pct = if next > crate::tool_palette::OPACITY_CEILING_PCT {
                    crate::tool_palette::OPACITY_FLOOR_PCT
                } else {
                    next
                };
            }
            let auto_hide_lock_resp = ui
                .put(auto_hide_lock_rect, egui::Button::new(if self.tool_palette.auto_hide_locked { "🔒" } else { "🔓" }))
                .on_hover_text(if self.tool_palette.auto_hide_locked {
                    lang.tool_palette_auto_hide_on_hint()
                } else {
                    lang.tool_palette_auto_hide_off_hint()
                });
            if auto_hide_lock_resp.clicked() {
                self.tool_palette.auto_hide_locked = !self.tool_palette.auto_hide_locked;
            }
            let size_px = self.tool_palette.slot_size_px() as i32;
            let size_label = if compact { "S".to_string() } else { format!("{size_px}px") };
            let size_resp = ui
                .put(size_rect, egui::Button::new(size_label))
                .on_hover_text(lang.tool_palette_size_hint(size_px));
            if size_resp.clicked() {
                self.tool_palette.cycle_slot_size();
            }
            let close_resp = ui
                .put(close_rect, egui::Button::new("✕"))
                .on_hover_text(lang.tool_palette_close_hint());
            if close_resp.clicked() {
                self.tool_palette.visible = false;
            }
        });

        // ── グリッド：GRID_COLS×GRID_ROWS。空欄マスは右クリックでToggle型を登録する ──
        let slot = self.tool_palette.slot_size_px();
        let grid_origin = rect.min + egui::vec2(Self::TOOL_PALETTE_PAD, Self::TOOL_PALETTE_HEADER_H + Self::TOOL_PALETTE_PAD);
        let mut any_menu_open = false;
        for row in 0..crate::tool_palette::GRID_ROWS {
            for col in 0..crate::tool_palette::GRID_COLS {
                let idx = row * crate::tool_palette::GRID_COLS + col;
                let slot_min = grid_origin + egui::vec2(
                    col as f32 * (slot + Self::TOOL_PALETTE_GAP),
                    row as f32 * (slot + Self::TOOL_PALETTE_GAP),
                );
                let slot_rect = egui::Rect::from_min_size(slot_min, egui::vec2(slot, slot));
                let content = self.tool_palette.slots[idx];
                let slot_resp = child
                    .interact(slot_rect, child.id().with(("tp_slot", idx)), egui::Sense::click())
                    .on_hover_text(Self::tool_palette_slot_hover_text(content, lang));

                if slot_resp.context_menu_opened() {
                    any_menu_open = true;
                }
                egui::Popup::context_menu(&slot_resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::draw_tool_palette_slot_menu(ui, &mut self.tool_palette.slots[idx], &mut self.tool_palette.custom_labels[idx], lang));

                // 左クリック: Toggle型は即時実行してViewerConfigへ反映する
                // （既存のpoll_image_filter_changeが差分検知して再デコードをトリガーする）。
                // Dialog型はミニUIの展開/折りたたみをトグルする（同時に開けるのは1マス分）。
                if slot_resp.clicked() {
                    match content {
                        crate::tool_palette::PaletteSlotContent::Toggle(kind) => {
                            crate::tool_palette::execute_toggle(cfg, kind);
                        }
                        crate::tool_palette::PaletteSlotContent::Dialog(_) => {
                            self.tool_palette_open_dialog = if self.tool_palette_open_dialog == Some(idx) {
                                None
                            } else {
                                Some(idx)
                            };
                        }
                        crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::NextPage) => {
                            self.advance_page(step, total as i32);
                        }
                        crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::PrevPage) => {
                            self.retreat_page(is_spread, step);
                        }
                        crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::OpenFolder) => {
                            let target = if self.archive_path.is_dir() {
                                self.archive_path.clone()
                            } else {
                                self.archive_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| self.archive_path.clone())
                            };
                            crate::translate::open_in_file_manager(&target);
                        }
                        crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::ToggleFullscreen) => {
                            Self::toggle_fullscreen(child.ctx(), cfg);
                        }
                        crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::SlideshowToggle) => {
                            self.toggle_slideshow();
                        }
                        crate::tool_palette::PaletteSlotContent::Empty => {}
                    }
                }

                child.painter().rect_stroke(
                    slot_rect,
                    4.0,
                    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60)),
                    egui::StrokeKind::Inside,
                );
                let default_label = match content {
                    crate::tool_palette::PaletteSlotContent::Toggle(kind) => {
                        Some((crate::tool_palette::find_toggle_def(kind).label)(lang))
                    }
                    crate::tool_palette::PaletteSlotContent::Dialog(kind) => {
                        Some(crate::tool_palette::create_dialog(kind).title(lang))
                    }
                    crate::tool_palette::PaletteSlotContent::Action(kind) => Some(kind.label(lang)),
                    crate::tool_palette::PaletteSlotContent::Empty => None,
                };
                // カスタム名称: 未設定ならデフォルトラベル、空文字での確定は「何も表示しない」。
                let slot_label: Option<String> = default_label.and_then(|default| {
                    match &self.tool_palette.custom_labels[idx] {
                        Some(custom) if custom.is_empty() => None,
                        Some(custom) => Some(custom.clone()),
                        None => Some(default.to_string()),
                    }
                });
                const LABEL_PAD: f32 = 3.0;
                // ヘッダーボタンと同じ考え方：ホバー中だけ不透明に戻し、それ以外は設定透過率に従う。
                // 名称・ON/OFF表示の両方で共有する。
                let label_opacity_pct = if slot_resp.hovered() { 100 } else { self.tool_palette.opacity_pct };
                let label_alpha = (label_opacity_pct as f32 / 100.0 * 255.0).round() as u8;
                if let Some(label) = slot_label {
                    let font_size = (slot * 0.28).clamp(8.0, 14.0);
                    let label_color = egui::Color32::from_white_alpha(label_alpha);
                    let galley = child.painter().layout_no_wrap(
                        label,
                        egui::FontId::proportional(font_size),
                        label_color,
                    );
                    let text_pos = slot_rect.min + egui::vec2(LABEL_PAD, LABEL_PAD);
                    child.painter().with_clip_rect(slot_rect).galley(text_pos, galley, label_color);
                }
                // Toggle型のみ：名称の下側にON/OFFを色分けして明示する。
                if let crate::tool_palette::PaletteSlotContent::Toggle(kind) = content {
                    let on = (crate::tool_palette::find_toggle_def(kind).get)(cfg);
                    let (r, g, b) = if on { (80, 220, 120) } else { (170, 170, 170) };
                    let state_color = egui::Color32::from_rgba_unmultiplied(r, g, b, label_alpha);
                    let state_font_size = (slot * 0.24).clamp(7.0, 12.0);
                    let state_galley = child.painter().layout_no_wrap(
                        if on { "ON".to_string() } else { "OFF".to_string() },
                        egui::FontId::proportional(state_font_size),
                        state_color,
                    );
                    let state_pos = egui::pos2(
                        slot_rect.center().x - state_galley.size().x / 2.0,
                        slot_rect.max.y - state_galley.size().y - LABEL_PAD,
                    );
                    child.painter().with_clip_rect(slot_rect).galley(state_pos, state_galley, state_color);
                }
                // Action型のみ：マス中央に矢印グリフを大きく表示する。
                // SlideshowToggleのみ現在の再生状態に応じてグリフ自体を切り替える
                // （再生中は□＝停止操作、停止中は▷＝再生操作を示す）。
                if let crate::tool_palette::PaletteSlotContent::Action(kind) = content {
                    let glyph_str = if kind == crate::tool_palette::ActionKind::SlideshowToggle {
                        if self.is_slideshow_active() { "□" } else { "▷" }
                    } else {
                        kind.glyph()
                    };
                    let glyph_color = egui::Color32::from_white_alpha(label_alpha);
                    let glyph_font_size = (slot * 0.4).clamp(12.0, 28.0);
                    let glyph_galley = child.painter().layout_no_wrap(
                        glyph_str.to_string(),
                        egui::FontId::proportional(glyph_font_size),
                        glyph_color,
                    );
                    let glyph_pos = slot_rect.center() - glyph_galley.size() / 2.0;
                    child.painter().with_clip_rect(slot_rect).galley(glyph_pos, glyph_galley, glyph_color);
                }
                // 状態を持つAction（ToggleFullscreen/SlideshowToggle）のみ：Toggle型と同じ
                // 見た目で現在のON/OFFを色分け表示する。SlideshowToggleは右クリックメニュー等
                // 他導線からの起動/停止もここでViewerState側の実値を読むだけで追従する
                // （ポーリングではなく、既存の再描画タイミングに乗るだけ）。
                let stateful_action_on = match content {
                    crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::ToggleFullscreen) => {
                        Some(cfg.fullscreen)
                    }
                    crate::tool_palette::PaletteSlotContent::Action(crate::tool_palette::ActionKind::SlideshowToggle) => {
                        Some(self.is_slideshow_active())
                    }
                    _ => None,
                };
                if let Some(on) = stateful_action_on {
                    let (r, g, b) = if on { (80, 220, 120) } else { (170, 170, 170) };
                    let state_color = egui::Color32::from_rgba_unmultiplied(r, g, b, label_alpha);
                    let state_font_size = (slot * 0.24).clamp(7.0, 12.0);
                    let state_galley = child.painter().layout_no_wrap(
                        if on { "ON".to_string() } else { "OFF".to_string() },
                        egui::FontId::proportional(state_font_size),
                        state_color,
                    );
                    let state_pos = egui::pos2(
                        slot_rect.center().x - state_galley.size().x / 2.0,
                        slot_rect.max.y - state_galley.size().y - LABEL_PAD,
                    );
                    child.painter().with_clip_rect(slot_rect).galley(state_pos, state_galley, state_color);
                }
                if self.tool_palette_open_dialog == Some(idx) {
                    child.painter().rect_filled(slot_rect, 4.0, egui::Color32::from_white_alpha(30));
                }
            }
        }
        self.tool_palette_menu_open = any_menu_open;

        // ── 展開中のDialog型ミニUI：パレット本体の直下に、Dialog自身が申告したサイズで表示 ──
        if let Some(open_idx) = self.tool_palette_open_dialog {
            if let crate::tool_palette::PaletteSlotContent::Dialog(kind) = self.tool_palette.slots[open_idx] {
                let dialog_rect = self.tool_palette_dialog_rect(rect, viewport, kind);
                ui.painter().rect_filled(dialog_rect, 6.0, egui::Color32::from_black_alpha(230));
                let mut dialog_child = ui.new_child(
                    egui::UiBuilder::new().id_salt("tool_palette_dialog_child").max_rect(dialog_rect.shrink(Self::TOOL_PALETTE_PAD)),
                );
                let mut dialog = crate::tool_palette::create_dialog(kind);

                const CLOSE_BTN_W: f32 = 20.0;
                dialog_child.horizontal(|ui| {
                    ui.label(dialog.title(lang));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_sized([CLOSE_BTN_W, CLOSE_BTN_W], egui::Button::new("✕"))
                            .on_hover_text(lang.tool_palette_dialog_close())
                            .clicked()
                        {
                            self.tool_palette_open_dialog = None;
                        }
                    });
                });
                dialog_child.separator();
                // 閉じるボタンで tool_palette_open_dialog が None になった後もこのフレームの
                // 残りは描画し続けて問題ない（次フレームで dialog_rect ごと消える）。
                dialog.render(&mut dialog_child, cfg);
            } else {
                // 登録内容が右クリックメニューで変更された等でDialogでなくなった場合は自動で閉じる。
                self.tool_palette_open_dialog = None;
            }
        }
    }

    /// 展開中のDialog型ミニUIの矩形。表示位置はパレット本体の直下のまま、サイズは
    /// ツールボックス幅に引っ張られずDialog自身の preferred_size を使う。viewport内に
    /// 収まるようクランプする（画面外へのはみ出し対策。パレット本体との重なりが起きても
    /// 右上のXボタンでいつでも閉じられる）。
    fn tool_palette_dialog_rect(&self, palette_rect: egui::Rect, viewport: egui::Rect, kind: crate::tool_palette::DialogKind) -> egui::Rect {
        let size = crate::tool_palette::create_dialog(kind).preferred_size();
        let anchor = palette_rect.left_bottom() + egui::vec2(0.0, Self::TOOL_PALETTE_GAP);
        let max_x = (viewport.max.x - size.x).max(viewport.min.x);
        let max_y = (viewport.max.y - size.y).max(viewport.min.y);
        let clamped_min = egui::pos2(anchor.x.clamp(viewport.min.x, max_x), anchor.y.clamp(viewport.min.y, max_y));
        egui::Rect::from_min_size(clamped_min, size)
    }

    /// ツールパレットのマスにマウスを乗せたときのヒント文言。
    fn tool_palette_slot_hover_text(content: crate::tool_palette::PaletteSlotContent, lang: crate::i18n::Lang) -> String {
        use crate::tool_palette::PaletteSlotContent;
        match content {
            PaletteSlotContent::Empty => lang.tool_palette_slot_empty_hint().to_string(),
            PaletteSlotContent::Toggle(kind) => {
                format!("{}{}", (crate::tool_palette::find_toggle_def(kind).label)(lang), lang.tool_palette_slot_change_suffix())
            }
            PaletteSlotContent::Dialog(kind) => {
                format!("{}{}", crate::tool_palette::create_dialog(kind).title(lang), lang.tool_palette_slot_change_suffix())
            }
            PaletteSlotContent::Action(kind) => {
                format!("{}{}", kind.label(lang), lang.tool_palette_slot_change_suffix())
            }
        }
    }

    /// マス毎のカスタム名称入力欄の文字数ソフト上限（見た目のはみ出し抑制用の目安）。
    const TOOL_PALETTE_LABEL_CHAR_LIMIT: usize = 8;

    /// マス右クリックの登録メニュー。先頭に名称変更（サブメニュー内TextEdit）、続けて
    /// TOGGLE_DEFS / ALL_DIALOG_KINDS を走査して選択肢を並べる（データ駆動：新規Toggle/Dialog
    /// 追加時にメニュー側の変更は不要）。
    fn draw_tool_palette_slot_menu(ui: &mut egui::Ui, content: &mut crate::tool_palette::PaletteSlotContent, custom_label: &mut Option<String>, lang: crate::i18n::Lang) {
        use crate::tool_palette::PaletteSlotContent;
        ui.set_min_width(140.0);

        let has_content = !matches!(content, PaletteSlotContent::Empty);
        ui.add_enabled_ui(has_content, |ui| {
            ui.menu_button(lang.tool_palette_rename_menu_label(), |ui| {
                let draft_id = ui.id().with("tp_rename_draft");
                let mut draft = ui.data_mut(|d| d.get_temp::<String>(draft_id))
                    .unwrap_or_else(|| custom_label.clone().unwrap_or_default());
                ui.add(
                    egui::TextEdit::singleline(&mut draft)
                        .char_limit(Self::TOOL_PALETTE_LABEL_CHAR_LIMIT)
                        .hint_text(lang.tool_palette_rename_hint_text()),
                );
                ui.data_mut(|d| d.insert_temp(draft_id, draft.clone()));
                ui.horizontal(|ui| {
                    if ui.button(lang.tool_palette_rename_ok()).clicked() {
                        *custom_label = Some(draft);
                        ui.data_mut(|d| d.remove_temp::<String>(draft_id));
                        ui.close();
                    }
                    if ui.button(lang.tool_palette_rename_cancel()).clicked() {
                        ui.data_mut(|d| d.remove_temp::<String>(draft_id));
                        ui.close();
                    }
                });
            });
        });
        ui.separator();

        if !matches!(content, PaletteSlotContent::Empty) {
            if ui.button(lang.tool_palette_slot_clear_label()).clicked() {
                *content = PaletteSlotContent::Empty;
                *custom_label = None;
                ui.close();
            }
            ui.separator();
        }
        for def in crate::tool_palette::TOGGLE_DEFS {
            let checked = matches!(*content, PaletteSlotContent::Toggle(k) if k == def.key);
            if ui.selectable_label(checked, (def.label)(lang)).clicked() {
                if !checked { *custom_label = None; }
                *content = PaletteSlotContent::Toggle(def.key);
                ui.close();
            }
        }
        ui.separator();
        for kind in crate::tool_palette::ALL_DIALOG_KINDS {
            let checked = matches!(*content, PaletteSlotContent::Dialog(k) if k == kind);
            let title = crate::tool_palette::create_dialog(kind).title(lang);
            if ui.selectable_label(checked, title).clicked() {
                if !checked { *custom_label = None; }
                *content = PaletteSlotContent::Dialog(kind);
                ui.close();
            }
        }
        ui.separator();
        for kind in crate::tool_palette::ALL_ACTION_KINDS {
            let checked = matches!(*content, PaletteSlotContent::Action(k) if k == kind);
            if ui.selectable_label(checked, kind.label(lang)).clicked() {
                if !checked { *custom_label = None; }
                *content = PaletteSlotContent::Action(kind);
                ui.close();
            }
        }
    }

    fn draw_central_panel(
        &mut self,
        ui: &mut egui::Ui,
        frame: &RenderFrame,
        input: &FrameInput,
        is_spread: bool,
        step: i32,
        total: usize,
        cfg: &mut ViewerConfig,
    ) -> (bool, bool) {
        let mut double_clicked = false;
        let mut single_clicked = false;
        egui::CentralPanel::default().show(ui, |ui| {
            let clip   = ui.clip_rect();
            let avail  = ui.available_size();
            let origin = ui.cursor().left_top();

            // ── ツールパレット：矩形計算とクリックガード判定 ─────────────────────
            // パレット上のクリック／ドラッグを背面（画像・ページ送りゾーン）へ伝えない
            // ため、ポインタがパレット矩形内にある間は背面向けの入力をここで握りつぶす。
            let viewport_rect = egui::Rect::from_min_size(origin, avail);
            let palette_rect = self.tool_palette_rect(viewport_rect);
            let dialog_rect = self.tool_palette_open_dialog.and_then(|open_idx| {
                let pr = palette_rect?;
                match self.tool_palette.slots[open_idx] {
                    crate::tool_palette::PaletteSlotContent::Dialog(kind) => {
                        Some(self.tool_palette_dialog_rect(pr, viewport_rect, kind))
                    }
                    _ => None,
                }
            });
            // マス右クリックメニュー展開中は、ポインタがパレット矩形の外に出ていても
            // パレット内扱いにする（自動ハイドタイマー停止／ページ送り等へのクリック非伝播）。
            let pointer_in_palette = input.hover_pos.is_some_and(|p| {
                palette_rect.is_some_and(|r| r.contains(p)) || dialog_rect.is_some_and(|r| r.contains(p))
            }) || self.tool_palette_menu_open;
            self.tick_tool_palette_auto_hide(ui.ctx(), input.time, pointer_in_palette);

            // ── 虫眼鏡：ホイール拡縮（ポインタ基準）とスライダーバー ────────────────────
            // パレット上のホイールは握りつぶす。バー上のホイールは表示中心基準で拡縮する。
            // バーはモードON中は（非表示でも）常にイベントを吸収し、ホバーで再表示される。
            let bar_rects = frame.magnifier.then(|| cfg.magnifier.bar.resolve(viewport_rect, true));
            let pointer_in_bar = bar_rects.zip(input.hover_pos).is_some_and(|(b, p)| {
                b.total.expand(crate::magnifier::BAR_HOVER_SLOP).contains(p)
            });
            if frame.magnifier {
                let anchor = if pointer_in_palette || self.file_detail_dialog.is_some() {
                    None
                } else if pointer_in_bar {
                    Some(viewport_rect.center())
                } else {
                    input.hover_pos
                };
                self.update_magnifier(viewport_rect, frame.tex_lo.as_ref(), anchor, input.wheel_notches, &cfg.magnifier);
                if let Some(bar) = bar_rects {
                    self.draw_magnifier_bar(ui.ctx(), viewport_rect, frame.tex_lo.as_ref(), &bar, pointer_in_bar, input.wheel_notches != 0.0, input.time, &mut cfg.magnifier);
                }
            } else {
                self.magnifier_view = None;
                self.magnifier_bar_active_at = None;
            }

            // ── 虫眼鏡カーソル ─────────────────────────────────────────────────────
            // 画像領域の上だけ（パレット・バー・メニュー等の別レイヤー上では通常のカーソルのまま）。
            // ビットマップは標準アイコンより優先されるため、この条件が必須。
            // layer_id_at は Area/Window/ポップアップだけを返す（背景のパネルは None）。
            if frame.magnifier
                && input.hover_pos.is_some_and(|p| {
                    viewport_rect.contains(p)
                        && !pointer_in_palette
                        && !pointer_in_bar
                        && ui.ctx().layer_id_at(p).is_none_or(|layer| layer == ui.layer_id())
                })
            {
                let image = self.magnifier_cursor_image(ui.ctx().pixels_per_point());
                ui.ctx().output_mut(|o| o.cursor_image = Some(image));
            }

            // ── 左右端ページ送りゾーン ───────────────────────────────────────────
            let edge_ctx = ui.ctx().clone();
            let guarded_hover = if pointer_in_palette || pointer_in_bar { None } else { input.hover_pos };
            let guarded_primary_clicked = input.primary_clicked && !pointer_in_palette && !pointer_in_bar;
            self.handle_edge_turn(&edge_ctx, clip, guarded_hover, guarded_primary_clicked, is_spread, step, total, input.time);
            self.draw_edge_turn_marker(&edge_ctx, clip);

            if !frame.animating || frame.zoom_actual || frame.magnifier {
                // ── 通常レンダリング ──────────────────────────────────────────
                match frame.page_mode {
                    PageMode::Single => {
                        self.render_single(ui, &frame.tex_lo, frame.zoom_actual, frame.rotation_angle, &mut double_clicked, &mut single_clicked);
                    }
                    PageMode::SpreadLeft => {
                        self.render_spread(ui, &frame.tex_lo, &frame.tex_hi, self.spread_lo(), self.spread_lo() + 1, frame.monitor, frame.rotation_angle, frame.zoom_actual, &mut double_clicked, &mut single_clicked);
                    }
                    PageMode::SpreadRight => {
                        self.render_spread(ui, &frame.tex_hi, &frame.tex_lo, self.spread_lo() + 1, self.spread_lo(), frame.monitor, frame.rotation_angle, frame.zoom_actual, &mut double_clicked, &mut single_clicked);
                    }
                }
                // ツールパレットのマス右クリックメニュー展開中の外側クリックは、
                // メニューを閉じる操作として消費し画像クリックへ伝播させない。
                if self.tool_palette_menu_open {
                    double_clicked = false;
                    single_clicked = false;
                }
            } else {
                // ── スライドアニメーション ────────────────────────────────────
                let full_rect = egui::Rect::from_min_size(origin, avail);
                let resp = ui.allocate_rect(full_rect, egui::Sense::click());
                // コンテキストメニュー表示中の外側クリックはメニューを閉じる操作として
                // 消費し、ページ送り（single/double_clicked）へは伝播させない。
                let menu_open = resp.context_menu_opened() || self.tool_palette_menu_open;
                if !menu_open {
                    if resp.double_clicked() { double_clicked = true; }
                    if resp.clicked() && !resp.double_clicked() { single_clicked = true; }
                }
                if resp.secondary_clicked() && !self.tool_palette_menu_open {
                    let target = match frame.page_mode {
                        PageMode::Single => Some(self.spread_lo()),
                        PageMode::SpreadLeft => resp.interact_pointer_pos().and_then(|pos| {
                            self.thumbnail_target_for_spread(
                                pos,
                                full_rect,
                                &frame.tex_lo,
                                &frame.tex_hi,
                                self.spread_lo(),
                                self.spread_lo() + 1,
                                frame.monitor,
                                frame.rotation_angle,
                            )
                        }),
                        PageMode::SpreadRight => resp.interact_pointer_pos().and_then(|pos| {
                            self.thumbnail_target_for_spread(
                                pos,
                                full_rect,
                                &frame.tex_hi,
                                &frame.tex_lo,
                                self.spread_lo() + 1,
                                self.spread_lo(),
                                frame.monitor,
                                frame.rotation_angle,
                            )
                        }),
                    };
                    self.set_thumbnail_context(target);
                }
                let toggle_enabled = self.spread_save_toggle_enabled();
                let toggle_on = self.spread_save_toggle_on();
                let overwrite_enabled = self.spread_overwrite_enabled();
                let sort_toggle_enabled = self.sort_save_toggle_enabled();
                let sort_toggle_on = self.sort_save_toggle_on();
        let bookmark_toggle_enabled = self.bookmark_save_toggle_enabled();
        let bookmark_toggle_on = self.bookmark_save_toggle_on();
                let sort_changed = self.sort_save_changed();
                let current_sort = self.current_sort_snapshot();
                let thumbnail_target = self.thumbnail_context_entry.as_ref();
                let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
                let saved_thumbnail_display = self.saved_thumbnail_display_name();
                let slideshow_active = self.is_slideshow_active();
                let blc_active = self.is_blc_active();
                let action = &mut self.pending_spread_action;
                let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
                let thumbnail_action = &mut self.pending_thumbnail_action;
                let favorite_add = &mut self.pending_favorite_add;
                let open_file_detail = &mut self.pending_open_file_detail;
                let slideshow_toggle = &mut self.pending_slideshow_toggle;
                let blc_toggle = &mut self.pending_blc_toggle;
                egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));

                let painter = ui.painter().with_clip_rect(clip);

                if frame.transition_kind == TransitionKind::CrossFade {
                    // クロスフェード：旧・新ページをそれぞれ自然な位置に固定描画し、
                    // 新ページ側だけアルファをtで持ち上げる（位置移動は無し）。
                    // GPU側のアルファブレンドのみで済ませ、CPU側のピクセル合成は行わない。
                    let new_alpha = (frame.t.clamp(0.0, 1.0) * 255.0).round() as u8;
                    match frame.page_mode {
                        PageMode::Single => {
                            Self::paint_single_alpha(&painter, &frame.prev_tex_lo, avail, origin, 255);
                            Self::paint_single_alpha(&painter, &frame.tex_lo,      avail, origin, new_alpha);
                        }
                        PageMode::SpreadLeft => {
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_lo, &frame.prev_tex_hi, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_lo, rl, 255);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_hi, rr, 255);
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_lo, &frame.tex_hi, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.tex_lo, rl, new_alpha);
                            Self::paint_page_alpha(&painter, &frame.tex_hi, rr, new_alpha);
                        }
                        PageMode::SpreadRight => {
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_hi, &frame.prev_tex_lo, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_hi, rl, 255);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_lo, rr, 255);
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_hi, &frame.tex_lo, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.tex_hi, rl, new_alpha);
                            Self::paint_page_alpha(&painter, &frame.tex_lo, rr, new_alpha);
                        }
                    }
                } else if frame.transition_kind == TransitionKind::ClockwiseWipe {
                    // 時計回りワイプ：旧ページを不透明固定描画した上に、新ページを時計12時
                    // 起点・時計回りの扇形で重ね描きする（境界にフェザー付き）。見開きは
                    // 左右それぞれ独立した扇（同じt）で揃えて描く。
                    match frame.page_mode {
                        PageMode::Single => {
                            Self::paint_single_alpha(&painter, &frame.prev_tex_lo, avail, origin, 255);
                            let rect_new = Self::single_fit_rect(avail, origin, &frame.tex_lo);
                            Self::paint_clockwise_wipe_overlay(&painter, &frame.tex_lo, rect_new, frame.t);
                        }
                        PageMode::SpreadLeft => {
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_lo, &frame.prev_tex_hi, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_lo, rl, 255);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_hi, rr, 255);
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_lo, &frame.tex_hi, frame.monitor);
                            Self::paint_clockwise_wipe_overlay(&painter, &frame.tex_lo, rl, frame.t);
                            Self::paint_clockwise_wipe_overlay(&painter, &frame.tex_hi, rr, frame.t);
                        }
                        PageMode::SpreadRight => {
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_hi, &frame.prev_tex_lo, frame.monitor);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_hi, rl, 255);
                            Self::paint_page_alpha(&painter, &frame.prev_tex_lo, rr, 255);
                            let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_hi, &frame.tex_lo, frame.monitor);
                            Self::paint_clockwise_wipe_overlay(&painter, &frame.tex_hi, rl, frame.t);
                            Self::paint_clockwise_wipe_overlay(&painter, &frame.tex_lo, rr, frame.t);
                        }
                    }
                } else {
                    // 横スライド（HorizontalSlide、および将来追加分の暫定フォールバック）。
                    let off_old = avail.x * frame.t * (-frame.anim_dir_f);
                    let off_new = avail.x * (1.0 - frame.t) * frame.anim_dir_f;

                    match frame.page_mode {
                        PageMode::Single => {
                            Self::paint_single_at(&painter, &frame.prev_tex_lo, avail, origin, off_old);
                            Self::paint_single_at(&painter, &frame.tex_lo,      avail, origin, off_new);
                        }
                        PageMode::SpreadLeft => {
                            if !Self::paint_offset_spread(&painter, frame, avail, origin, false) {
                                let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_lo, &frame.prev_tex_hi, frame.monitor);
                                Self::paint_page(&painter, &frame.prev_tex_lo, rl.translate(egui::vec2(off_old, 0.0)));
                                Self::paint_page(&painter, &frame.prev_tex_hi, rr.translate(egui::vec2(off_old, 0.0)));
                                let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_lo, &frame.tex_hi, frame.monitor);
                                Self::paint_page(&painter, &frame.tex_lo, rl.translate(egui::vec2(off_new, 0.0)));
                                Self::paint_page(&painter, &frame.tex_hi, rr.translate(egui::vec2(off_new, 0.0)));
                            }
                        }
                        PageMode::SpreadRight => {
                            if !Self::paint_offset_spread(&painter, frame, avail, origin, true) {
                                let (rl, rr) = Self::spread_rects(avail, origin, &frame.prev_tex_hi, &frame.prev_tex_lo, frame.monitor);
                                Self::paint_page(&painter, &frame.prev_tex_hi, rl.translate(egui::vec2(off_old, 0.0)));
                                Self::paint_page(&painter, &frame.prev_tex_lo, rr.translate(egui::vec2(off_old, 0.0)));
                                let (rl, rr) = Self::spread_rects(avail, origin, &frame.tex_hi, &frame.tex_lo, frame.monitor);
                                Self::paint_page(&painter, &frame.tex_hi, rl.translate(egui::vec2(off_new, 0.0)));
                                Self::paint_page(&painter, &frame.tex_lo, rr.translate(egui::vec2(off_new, 0.0)));
                            }
                        }
                    }
                }
            }

            // パレット上でのクリックは画像側（ページめくり／原寸切替）へ伝えない。
            if pointer_in_palette {
                double_clicked = false;
                single_clicked = false;
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

            // ── ツールパレット：最前面オーバーレイ ────────────────────────────
            // auto_hidden中は完全無描画（pointer_in_paletteの当たり判定はrect自体が
            // 生きているため上のtick呼び出しで検知でき、描画をスキップするだけでよい）。
            if let Some(pr) = palette_rect {
                if !self.tool_palette_auto_hidden {
                    self.draw_tool_palette(ui, pr, viewport_rect, is_spread, step, total, cfg);
                }
            } else {
                // パレット非表示中はマスメニューも存在し得ないため、直前まで展開中だった
                // 状態が残っていればここで確実にクリアする（クリックガードの誤動作防止）。
                self.tool_palette_menu_open = false;
                // 非表示中：画面のどこでも右クリックすれば復活する。
                let revive_resp = ui.interact(viewport_rect, ui.id().with("tool_palette_revive"), egui::Sense::click());
                if revive_resp.secondary_clicked() {
                    self.tool_palette.visible = true;
                }
            }
        });
        // ページ送りゾーン内では原寸表示切替（ダブルクリック）を素通りさせない。
        // シングルクリック側の副作用（サムネバー自動非表示の早送り）は害が無いため残す。
        if self.edge_turn_hover.is_some() {
            double_clicked = false;
        }
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

    /// フルスクリーン⇔ウィンドウモードの切替本体。キー入力/中クリック(process_misc_input)と
    /// ツールパレットのActionKind::ToggleFullscreenの両方から呼ばれる。
    fn toggle_fullscreen(ctx: &egui::Context, cfg: &mut ViewerConfig) {
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

    fn process_misc_input(
        &mut self,
        ctx: &egui::Context,
        input: &FrameInput,
        double_clicked: bool,
        cfg: &mut ViewerConfig,
    ) -> bool {
        // 虫眼鏡の有効中は原寸トグルを止める（倍率は虫眼鏡側で持つ。原寸との統合はフェーズ3）。
        if (input.zoom_key || double_clicked) && self.magnifier_view.is_none() {
            cfg.zoom_actual = !cfg.zoom_actual;
            // フェーズ6: 表示ターゲットサイズが変わるイベントとして再デコードのデバウンス対象にする
            cfg.redecode_trigger_seq += 1;
        }

        // マス右クリックメニュー展開中の中クリックは、メニューを閉じる操作として
        // 消費し、フルスクリーン切替へは伝播させない。
        if input.fs_key || (input.middle_clicked && !self.tool_palette_menu_open) {
            Self::toggle_fullscreen(ctx, cfg);
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
    fn update_textures(
        &mut self,
        ctx: &egui::Context,
        page_cache: &mut PageCache,
        active_generation: u64,
        preparing_generation: Option<u64>,
    ) {
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

        let mut promoted_pages = Vec::new();
        for i in start..end {
            let orig_i = self.entries[i].original_index;
            let Some((generation, content)) = page_cache.get_best(
                &self.archive_path,
                orig_i,
                active_generation,
                preparing_generation,
            ) else {
                ctx.request_repaint_after(Duration::from_millis(100));
                continue;
            };
            let previous_generation = self.texture_generations.get(&orig_i).copied();
            let generation_changed = previous_generation != Some(generation);
            match content {
                PageContent::Static(img) => {
                    if generation_changed {
                        self.anim_states.remove(&orig_i);
                    }
                    if generation_changed || !self.textures.contains_key(&orig_i) {
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
                        self.texture_generations.insert(orig_i, generation);
                        promoted_pages.push((orig_i, generation));
                    }
                }
                PageContent::Animated(ring) => {
                    let instance_id = ring.instance_id();
                    let animation_instance_changed = animation_instance_changed(
                        self.anim_states.get(&orig_i).map(|state| state.instance_id),
                        instance_id,
                    );
                    if animation_instance_changed {
                        self.anim_states.remove(&orig_i);
                    }
                    // フェーズ3/3.5: GIF/APNG/AVIF/WebP。全フレーム常駐ではなく逐次デコード+リングバッファ。
                    // 可視ページの次フレーム生成はバックグラウンドへ要求し、UIスレッドでは
                    // 完成済みフレームだけを採用する。未完成中は現在のテクスチャを保持する。
                    if !visible_orig.contains(&orig_i) {
                        // 裏ページ: 位置を凍結（tickしない）。ページ送り時の白フラッシュ防止に、
                        // テクスチャ未保有時のみ凍結位置のフレームを1回だけアップロードする。
                        if let Some(state) = self.anim_states.get_mut(&orig_i) {
                            state.paused = true;
                        }
                        if generation_changed || !self.textures.contains_key(&orig_i) {
                            let frozen_index =
                                self.anim_states.get(&orig_i).map_or(0, |s| s.frame_index);
                            let upload_started = Instant::now();
                            if let Some(tex) = upload_ring_frame(ctx, orig_i, ring, frozen_index) {
                                let upload_elapsed = upload_started.elapsed();
                                self.textures.insert(orig_i, tex);
                                self.texture_generations.insert(orig_i, generation);
                                log_anim_texture_upload(
                                    &self.archive_path,
                                    orig_i,
                                    previous_generation,
                                    generation,
                                    frozen_index,
                                    false,
                                    generation_changed,
                                    upload_elapsed,
                                );
                                promoted_pages.push((orig_i, generation));
                            }
                        }
                        continue;
                    }
                    let state = self.anim_states.entry(orig_i).or_insert_with(|| AnimState {
                        instance_id,
                        frame_index: 0,
                        requested_through: 0,
                        last_frame_at: now,
                        paused: false,
                        missing_frame_logged: false,
                    });
                    // 凍結明け: 凍結中の経過時間を再生遅延として追走しないよう基準時刻を取り直し、
                    // 凍結位置から等速で再開する。
                    if state.paused {
                        state.paused = false;
                        state.last_frame_at = now;
                    }
                    let texture_missing = !self.textures.contains_key(&orig_i);
                    if texture_missing {
                        if let Some(reconnect_index) = ring.reconnect_frame_index(state.frame_index) {
                            if reconnect_index != state.frame_index {
                                let previous_index = state.frame_index;
                                state.frame_index = reconnect_index;
                                state.requested_through = state.requested_through.max(reconnect_index);
                                state.last_frame_at = now;
                                state.missing_frame_logged = false;
                                crate::log_perf!(
                                    "[diag/anim-reconnect] archive={:?} page={} from={} to={} instance={:?} texture=false visible=true",
                                    self.archive_path,
                                    orig_i,
                                    previous_index,
                                    reconnect_index,
                                    instance_id,
                                );
                            }
                        }
                    }
                    let mut needs_upload = generation_changed || texture_missing;
                    let current_delay = ring
                        .try_with_frame(state.frame_index, |f| f.delay)
                        .unwrap_or(Duration::from_millis(100));

                    let latest_ready = ring.playback_ready_after(state.frame_index);
                    if now.duration_since(state.last_frame_at) >= current_delay {
                        if let Some(ready_index) = latest_ready {
                            let previous_index = state.frame_index;
                            state.frame_index = ready_index;
                            if ready_index > previous_index + 1 {
                                crate::log_perf!(
                                    "[diag/anim-drop] archive={:?} page={} from={} to={} skipped={}",
                                    self.archive_path,
                                    orig_i,
                                    previous_index,
                                    ready_index,
                                    ready_index - previous_index - 1,
                                );
                            }
                            // 遅れを次フレームへ持ち越すと、重い素材で永久に複数枚追走するため、
                            // 実際に表示できた時点を新しい基準時刻にする。
                            state.last_frame_at = now;
                            needs_upload = true;
                        }
                    }

                    if needs_upload {
                        let frame_index = state.frame_index;
                        let upload_started = Instant::now();
                        if let Some(tex) = upload_ring_frame(ctx, orig_i, ring, frame_index) {
                            let upload_elapsed = upload_started.elapsed();
                            self.textures.insert(orig_i, tex);
                            self.texture_generations.insert(orig_i, generation);
                            log_anim_texture_upload(
                                &self.archive_path,
                                orig_i,
                                previous_generation,
                                generation,
                                frame_index,
                                true,
                                generation_changed,
                                upload_elapsed,
                            );
                            if generation_changed {
                                promoted_pages.push((orig_i, generation));
                            }
                            state.missing_frame_logged = false;
                        } else if !state.missing_frame_logged {
                            if let Some(AnimationFrameDiagnostic::Missing {
                                ring_range,
                                next_decode_index,
                                capacity,
                            }) = ring.diagnose_missing_frame(frame_index)
                            {
                                crate::log_perf!(
                                    "[diag/anim-missing-frame] archive={:?} page={} requested={} ring_range={:?} next_decode={} capacity={} instance={:?} texture=false visible=true",
                                    self.archive_path,
                                    orig_i,
                                    frame_index,
                                    ring_range,
                                    next_decode_index,
                                    capacity,
                                    instance_id,
                                );
                                state.missing_frame_logged = true;
                            }
                        }
                    }

                    // 初期テクスチャを確保してから、可視中だけ小さな範囲を先行デコードする。
                    // リサイズが追いつかない場合はpipeline側のraw queueが中間フレームを
                    // 最新1枚へ畳み込み、表示リサイズ前に破棄する。
                    state.requested_through = next_anim_decode_request(
                        state.frame_index,
                        state.requested_through,
                        ring.ring_capacity(),
                    );
                    ring.request_frame(state.requested_through);

                    // デコード/リサイズ/アップロードに要した実時間を差し引くため、ここで時刻を取り直す
                    // (loop開始時の `now` を使うと、上記処理のコストが remaining に反映されず
                    //  次のrepaintが実処理時間分だけ遅延し、アニメ全体が一様に遅く見える)
                    let now2 = Instant::now();
                    let elapsed_after_upload = now2.duration_since(state.last_frame_at);
                    let next_delay = ring
                        .try_with_frame(state.frame_index, |f| f.delay)
                        .unwrap_or(Duration::from_millis(100));
                    // 未完成フレームは短いポーリングだけ予約し、OS入力を妨げる同期waitはしない。
                    let remaining = if ring.playback_ready_after(state.frame_index).is_some() {
                        next_delay.saturating_sub(elapsed_after_upload)
                    } else {
                        Duration::from_millis(8)
                    };
                    min_repaint_after = min_repaint_after.min(remaining);
                }
            }
        }

        // 新GPUテクスチャの作成に成功したページだけ、旧CPU世代を後から解放する。
        for (orig_i, generation) in promoted_pages {
            page_cache.remove_older_versions(&self.archive_path, orig_i, generation);
        }

        let window_orig: HashSet<usize> = (start..end)
            .map(|i| self.entries[i].original_index)
            .collect();
        self.textures.retain(|orig_i, _| window_orig.contains(orig_i));
        self.texture_generations.retain(|orig_i, _| window_orig.contains(orig_i));
        self.anim_states.retain(|orig_i, _| window_orig.contains(orig_i));

        if min_repaint_after < Duration::MAX {
            ctx.request_repaint_after(min_repaint_after);
        }
    }

    /// 上部ホバートリガー高さ（対象領域の高さの15%、ただし最低40pxを保証）。
    /// 左エントリリストの発火域・左右端ページ送りゾーンの双方で共有する。
    fn hover_trigger_height(area_h: f32) -> f32 {
        const RATIO: f32 = 0.15;
        const MIN_PX: f32 = 40.0;
        (area_h * RATIO).max(MIN_PX)
    }

    /// 左右端クリックの進行方向。綴じ方向（page_mode）に応じて新ページが入ってくる側を
    /// 「進む」に割り当てる（[view_reader.rs:354]のオフセット符号コメント参照）。
    /// forward=true: 進む(spread_base増加) / false: 戻る(spread_base減少)
    fn edge_turn_is_forward(&self, left_edge: bool) -> bool {
        match self.page_mode {
            // 右綴じ: 新ページは左からIN → 左端＝進む
            PageMode::SpreadRight => left_edge,
            // 左綴じ・単ページ: 新ページは右からIN → 右端＝進む
            _ => !left_edge,
        }
    }

    /// 左右端ページ送りゾーンの半幅。
    const EDGE_TURN_ZONE_W: f32 = 100.0;

    /// マウス位置がどちらの端ゾーンにあるかを判定する（進む/戻る可否は見ない）。
    /// 左ゾーンは左エントリリストの発火域（左端上部の一部）と競合しないよう、
    /// その下端から画面下端までとする。右ゾーンは競合が無いため全高。
    fn edge_turn_zone_at(clip: egui::Rect, pos: egui::Pos2) -> Option<bool> {
        let top_avoid_h = Self::hover_trigger_height(clip.height());
        let in_left  = pos.x < clip.min.x + Self::EDGE_TURN_ZONE_W
            && pos.y >= clip.min.y + top_avoid_h && pos.y <= clip.max.y;
        let in_right = pos.x > clip.max.x - Self::EDGE_TURN_ZONE_W
            && pos.y >= clip.min.y && pos.y <= clip.max.y;
        if in_left { Some(true) } else if in_right { Some(false) } else { None }
    }

    /// そのゾーンへ進む/戻る操作を行った場合に、破壊的にならず実際に移動できるか。
    /// process_navigation の境界判定（can_advance_page/can_retreat_page）と同一の式を使うため、
    /// 「進行方向のページが無い」＝「不正なペア（仮想×仮想等）を生む移動」と一致する。
    fn edge_turn_can_move(&self, left_edge: bool, is_spread: bool, step: i32, total_i: i32) -> bool {
        if self.edge_turn_is_forward(left_edge) {
            self.can_advance_page(step, total_i)
        } else {
            self.can_retreat_page(is_spread, step)
        }
    }

    /// 中央パネル左右端のページ送りゾーン判定＋クリック実行＋ホバーのフェード状態更新。
    /// ダイアログ表示中は無効化する。
    fn handle_edge_turn(
        &mut self,
        ctx: &egui::Context,
        clip: egui::Rect,
        hover_pos: Option<egui::Pos2>,
        primary_clicked: bool,
        is_spread: bool,
        step: i32,
        total: usize,
        time: f64,
    ) {
        let total_i = total as i32;

        let active_side = if self.file_detail_dialog.is_some() {
            None
        } else {
            hover_pos
                .and_then(|pos| Self::edge_turn_zone_at(clip, pos))
                .filter(|&left_edge| self.edge_turn_can_move(left_edge, is_spread, step, total_i))
        };

        // ── ホバーのフェードイン状態（0.3秒）を更新 ──────────────────────────
        match (self.edge_turn_hover, active_side) {
            (Some((side, _)), Some(new_side)) if side == new_side => {}
            (_, Some(new_side)) => self.edge_turn_hover = Some((new_side, time)),
            (Some(_), None) => self.edge_turn_hover = None,
            (None, None) => {}
        }
        if self.edge_turn_hover.is_some() {
            ctx.request_repaint();
        }

        // ── クリック実行 ─────────────────────────────────────────────────────
        let Some(left_edge) = active_side else { return };
        if !primary_clicked {
            return;
        }
        if self.edge_turn_is_forward(left_edge) {
            self.advance_page(step, total_i);
        } else {
            self.retreat_page(is_spread, step);
        }
    }

    /// 左右端ページ送りマーカー（◀/▶）をフェードイン(0.3秒)しながら描画する。
    /// サムネイルバー等の上に確実に重ねるため最前面レイヤーに描画する。
    fn draw_edge_turn_marker(&self, ctx: &egui::Context, clip: egui::Rect) {
        let Some((left_edge, since)) = self.edge_turn_hover else { return };
        const FADE_SEC: f32 = 0.3;
        let elapsed = (ctx.input(|i| i.time) - since) as f32;
        let alpha = (elapsed / FADE_SEC).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }

        // クリック判定ゾーンの上端回避（左のみ）とは無関係に、マーカーは左右とも
        // パネル全高の中央に固定表示する。
        let center_y = (clip.min.y + clip.max.y) / 2.0;

        const MARKER_H: f32 = 36.0;
        const MARKER_W: f32 = 24.0;
        const EDGE_PAD: f32 = 16.0;
        let tip_x  = if left_edge { clip.min.x + EDGE_PAD } else { clip.max.x - EDGE_PAD };
        let base_x = if left_edge { tip_x + MARKER_W } else { tip_x - MARKER_W };
        let p_tip = egui::pos2(tip_x, center_y);
        let p_top = egui::pos2(base_x, center_y - MARKER_H / 2.0);
        let p_bot = egui::pos2(base_x, center_y + MARKER_H / 2.0);

        let a = (alpha * 255.0) as u8;
        let fill = egui::Color32::from_white_alpha(a);
        let shadow = egui::Color32::from_black_alpha((alpha * 150.0) as u8);
        let off = egui::vec2(1.0, 1.0);

        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("edge_turn_marker")));
        painter.add(egui::Shape::convex_polygon(vec![p_tip + off, p_top + off, p_bot + off], shadow, egui::Stroke::NONE));
        painter.add(egui::Shape::convex_polygon(vec![p_tip, p_top, p_bot], fill, egui::Stroke::NONE));
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

        let was_visible = self.entry_list_visible;
        let screen_left = viewport_rect.min.x;
        let trigger_h = Self::hover_trigger_height(viewport_rect.height());
        if let Some(pos) = hover_pos {
            if !self.entry_list_visible
                && pos.x < screen_left + TRIGGER_W
                && pos.y < viewport_rect.min.y + trigger_h
            {
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
            let scroll_anchor = Self::entry_list_scroll_anchor(lo, entries_snap.len());
            let should_scroll = !was_visible || self.entry_list_scrolled_lo != Some(lo);

            egui::Panel::left("entry_list_panel")
                .exact_size(ENTRY_PANEL_W)
                .frame(egui::Frame::side_top_panel(viewer_style))
                .show(ui, |ui| {
                    let output = egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut anchor_rect = None;
                            for (i, entry) in entries_snap.iter().enumerate() {
                                let is_cur = i as i32 == lo || i as i32 == hi;
                                let response = ui.selectable_label(is_cur, &entry.display_name);
                                if scroll_anchor == Some(i) {
                                    anchor_rect = Some(response.rect);
                                }
                            }
                            anchor_rect
                        });

                    if should_scroll && let Some(anchor_rect) = output.inner {
                        // 現在行のコンテンツ上の位置から絶対オフセットを求める。
                        // 0..max_offset にクランプすることで、先頭側ではマーカーが
                        // 中央まで移動し、中盤だけ中央固定、末尾側では再び下へ移動する。
                        let max_offset = (output.content_size.y - output.inner_rect.height()).max(0.0);
                        let desired = Self::entry_list_follow_offset(
                            output.state.offset.y,
                            anchor_rect.center().y,
                            output.inner_rect.center().y,
                            max_offset,
                        );
                        let mut state = output.state;
                        state.offset.y = desired;
                        state.store(ctx, output.id);
                        ctx.request_repaint();
                        self.entry_list_scrolled_lo = Some(lo);
                    }
                });
        }
    }

    /// 仮想ページを含む spread_lo から、左一覧でスクロール対象にする実在行を求める。
    fn entry_list_scroll_anchor(lo: i32, total: usize) -> Option<usize> {
        (total > 0).then(|| lo.clamp(0, total as i32 - 1) as usize)
    }

    /// 現在行が表示領域中央に来る絶対スクロール量を、先頭・末尾でクランプする。
    fn entry_list_follow_offset(
        current_offset: f32,
        marker_center: f32,
        viewport_center: f32,
        max_offset: f32,
    ) -> f32 {
        (current_offset + marker_center - viewport_center).clamp(0.0, max_offset.max(0.0))
    }

    /// ツールバー項目を cfg.bar_order の順に描画する（top bar / fs_sort_bar 共用）。
    /// 隣接項目のグループが変わる位置にセパレータを挟む（toolbar.rs::BarGroup 参照）。
    fn draw_bar_items(&mut self, ui: &mut egui::Ui, cfg: &mut ViewerConfig) {
        let order = cfg.bar_order;
        let mut prev_group: Option<BarGroup> = None;
        for item in order {
            if prev_group.is_some_and(|g| g != item.group()) {
                ui.separator();
            }
            prev_group = Some(item.group());
            self.draw_bar_item(ui, cfg, item);
        }
    }

    fn draw_bar_item(&mut self, ui: &mut egui::Ui, cfg: &mut ViewerConfig, item: ViewerBarItem) {
        let t = i18n::t();
        match item {
            ViewerBarItem::SortName | ViewerBarItem::SortNatural | ViewerBarItem::SortDate => {
                let (key, label) = match item {
                    ViewerBarItem::SortName    => (ViewerSortKey::Name,    t.sort_name()),
                    ViewerBarItem::SortNatural => (ViewerSortKey::Natural, t.sort_natural()),
                    _                          => (ViewerSortKey::Date,    t.sort_date()),
                };
                if ui.selectable_label(self.sort_key == key, label).clicked() {
                    self.sort_key = key;
                    self.sort_entries();
                }
            }
            ViewerBarItem::SortOrder => {
                ui.label(":");
                let order_label = if self.sort_ascending { t.sort_asc() } else { t.sort_desc() };
                if ui.button(order_label).clicked() {
                    self.sort_ascending = !self.sort_ascending;
                    self.sort_entries();
                }
            }
            // ページ表示モード3択（アイコントグル、選択状態=現在モード）
            ViewerBarItem::PageSingle | ViewerBarItem::SpreadLeft | ViewerBarItem::SpreadRight => {
                let (mode, tip) = match item {
                    ViewerBarItem::PageSingle => (PageMode::Single,      t.page_single()),
                    ViewerBarItem::SpreadLeft => (PageMode::SpreadLeft,  t.page_spread_left()),
                    _                         => (PageMode::SpreadRight, t.page_spread_right()),
                };
                // i18nラベルは "[単ページ]" 形式なのでツールチップでは囲みを外す
                let tip = tip.trim_matches(['[', ']']);
                // 単一画像ファイル直接表示（rawビューアー）では見開き系を選べない
                let enabled = mode == PageMode::Single || !self.is_raw_file();
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.selectable_label(self.page_mode == mode, item.icon().unwrap_or("?"))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.set_page_mode(mode, cfg);
                    }
                });
            }
            // 見開き1Pシフト（キー4/5と同じ操作のボタン経路）
            ViewerBarItem::SpreadBack | ViewerBarItem::SpreadFwd => {
                let back = item == ViewerBarItem::SpreadBack;
                let (label, tip, can) = if back {
                    ("-1P", t.spread_back(), self.can_shift_backward())
                } else {
                    ("+1P", t.spread_fwd(), self.can_shift_forward())
                };
                let tip = tip.trim_matches(['[', ']']);
                let enabled = self.page_mode != PageMode::Single && !self.is_raw_file() && can;
                if ui.add_enabled(enabled, egui::Button::new(label))
                    .on_hover_text(tip)
                    .clicked()
                {
                    if back { self.shift_offset_backward(); } else { self.shift_offset_forward(); }
                }
            }
            // ずれ状態の表示専用インジケータ。単ページ時は非表示（決定事項）。
            // 矢印は綴じ方向によるページ進行方向を反映（SpreadRight=左へ進行）
            ViewerBarItem::OffsetIndicator => {
                if self.page_mode == PageMode::Single { return; }
                let text = match (self.page_mode, self.offset.value()) {
                    (_, 0) => "0",
                    (PageMode::SpreadRight, 1) | (PageMode::SpreadLeft, -1) => "←1",
                    _ => "1→",
                };
                ui.label(text);
            }
            // 手動回転CW/CCWボタン（アイコンのみ、i18n対象外）
            ViewerBarItem::RotateCcw | ViewerBarItem::RotateCw => {
                let ccw = item == ViewerBarItem::RotateCcw;
                let tip = if ccw { t.rotate_ccw() } else { t.rotate_cw() };
                if ui.button(item.icon().unwrap_or("?")).on_hover_text(tip).clicked() {
                    if ccw { self.rotate_ccw(cfg); } else { self.rotate_cw(cfg); }
                }
            }
            // トグル2種は幅圧縮のためテキストラベル付きチェックボックスから
            // アイコントグル（選択状態=ON）へ変更。説明はホバーツールチップで補う
            ViewerBarItem::RotationCarry => {
                if ui.selectable_label(cfg.rotation_carry_over, item.icon().unwrap_or("?"))
                    .on_hover_text(t.rotation_carry_over_label())
                    .clicked()
                {
                    cfg.rotation_carry_over = !cfg.rotation_carry_over;
                }
            }
            ViewerBarItem::ExifRotation => {
                let tip = format!(
                    "{}\n{}",
                    t.exif_orientation_toolbar_label(),
                    t.settings_exif_orientation_explain()
                );
                if ui.selectable_label(cfg.exif_orientation_enabled, "EXIF")
                    .on_hover_text(tip)
                    .clicked()
                {
                    cfg.exif_orientation_enabled = !cfg.exif_orientation_enabled;
                }
            }
            // OCR/翻訳子ウィンドウの開閉トグル。実際の開閉状態はapp側が持ち、show()呼び出し時に
            // translate_window_openとして受け取って表示にのみ使う（真の状態はここでは変更しない）。
            ViewerBarItem::TranslateToggle => {
                let enabled = self.translate_toggle_enabled;
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.selectable_label(self.translate_window_open, t.toolbar_translate_toggle_label())
                        .on_hover_text(t.toolbar_translate_toggle_tip(enabled))
                        .clicked()
                    {
                        self.pending_toggle_translate_window = true;
                    }
                });
            }
        }
    }

    fn sort_setting_text(key: ViewerSortKey, ascending: bool, t: i18n::Lang) -> String {
        let key_label = match key {
            ViewerSortKey::Name => t.sort_name(),
            ViewerSortKey::Natural => t.sort_natural(),
            ViewerSortKey::Date => t.sort_date(),
        };
        let order_label = if ascending { t.sort_asc() } else { t.sort_desc() };
        format!(
            "{} {}",
            key_label.trim_matches(['[', ']']),
            order_label.trim_matches(['[', ']'])
        )
    }

    /// 画像本体の右クリックメニュー（見開き・ソート設定の保存）を描画する
    fn spread_save_context_menu(
        ui: &mut egui::Ui,
        toggle_enabled: bool,
        toggle_on_init: bool,
        overwrite_enabled: bool,
        action: &mut Option<crate::controller::SpreadSaveAction>,
        sort_toggle_enabled: bool,
        sort_toggle_on_init: bool,
        sort_changed: bool,
        current_sort: (ViewerSortKey, bool),
        sort_action: &mut Option<crate::controller::SortSaveAction>,
        bookmark_toggle_enabled: bool,
        bookmark_toggle_on_init: bool,
        bookmark_action: &mut Option<crate::controller::BookmarkSaveAction>,
        thumbnail_target: Option<&(String, String)>,
        saved_thumbnail_selection: Option<&crate::spread_state::ThumbnailSelection>,
        saved_thumbnail_display: Option<&str>,
        thumbnail_action: &mut Option<crate::controller::ThumbnailSaveAction>,
        favorite_add: &mut bool,
        open_file_detail: &mut bool,
        slideshow_active: bool,
        slideshow_toggle: &mut bool,
        blc_active: bool,
        blc_toggle: &mut bool,
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
            }
        });
        ui.add_enabled_ui(overwrite_enabled, |ui| {
            if ui.button(t.spread_save_overwrite_label()).clicked() {
                *action = Some(crate::controller::SpreadSaveAction::Overwrite);
            }
        });
        let mut sort_toggle_on = sort_toggle_on_init;
        ui.add_enabled_ui(sort_toggle_enabled, |ui| {
            if ui.checkbox(&mut sort_toggle_on, t.sort_save_toggle_label()).changed() {
                *sort_action = Some(if sort_toggle_on {
                    crate::controller::SortSaveAction::Enable
                } else {
                    crate::controller::SortSaveAction::Disable
                });
            }
        });
        let sort_text = Self::sort_setting_text(current_sort.0, current_sort.1, t);
        let changed_suffix = if sort_changed {
            format!("（{}）", t.sort_save_changed_label())
        } else {
            String::new()
        };
        ui.label(format!("{} : {}{}", t.sort_save_new_label(), sort_text, changed_suffix));
        let mut bookmark_toggle_on = bookmark_toggle_on_init;
        ui.add_enabled_ui(bookmark_toggle_enabled, |ui| {
            if ui.checkbox(&mut bookmark_toggle_on, t.bookmark_save_toggle_label()).changed() {
                *bookmark_action = Some(if bookmark_toggle_on {
                    crate::controller::BookmarkSaveAction::Enable
                } else {
                    crate::controller::BookmarkSaveAction::Disable
                });
            }
        });
        // 3項目は排他的なプリセット。チェック状態は右クリックしたページではなく、
        // アーカイブに保存済みの生成方法を表す。
        let saved_kind = saved_thumbnail_selection.map(|selection| selection.source_kind);
        let mut add_thumbnail_choice = |
            ui: &mut egui::Ui,
            kind: crate::spread_state::ThumbnailSourceKind,
            label: &str,
        | {
            let mut checked = saved_kind == Some(kind);
            ui.add_enabled_ui(thumbnail_target.is_some(), |ui| {
                if ui.checkbox(&mut checked, label).changed() {
                    *thumbnail_action = if checked {
                        thumbnail_target.map(|(entry_name, _)| {
                            crate::controller::ThumbnailSaveAction::Enable {
                                selection: crate::spread_state::ThumbnailSelection {
                                    entry_name: entry_name.clone(),
                                    source_kind: kind,
                                },
                            }
                        })
                    } else {
                        Some(crate::controller::ThumbnailSaveAction::Disable)
                    };
                }
            });
        };
        add_thumbnail_choice(
            ui,
            crate::spread_state::ThumbnailSourceKind::Full,
            t.thumbnail_register_page_label(),
        );
        ui.indent("thumbnail_half_presets", |ui| {
            add_thumbnail_choice(
                ui,
                crate::spread_state::ThumbnailSourceKind::LeftHalf,
                t.thumbnail_register_left_half_label(),
            );
            add_thumbnail_choice(
                ui,
                crate::spread_state::ThumbnailSourceKind::RightHalf,
                t.thumbnail_register_right_half_label(),
            );
        });
        let saved_display = saved_thumbnail_display
            .map(Self::thumbnail_status_name)
            .unwrap_or_else(|| t.thumbnail_default_label().to_string());
        let saved_display = match saved_kind {
            Some(crate::spread_state::ThumbnailSourceKind::LeftHalf) => {
                format!("{}［{}］", saved_display, t.thumbnail_left_generated_label())
            }
            Some(crate::spread_state::ThumbnailSourceKind::RightHalf) => {
                format!("{}［{}］", saved_display, t.thumbnail_right_generated_label())
            }
            _ => saved_display,
        };
        let status_response = ui.label(format!(
            "{}: {}",
            t.thumbnail_current_label(),
            saved_display,
        ));
        if let Some(full_name) = saved_thumbnail_display {
            let full_name = match saved_kind {
                Some(crate::spread_state::ThumbnailSourceKind::LeftHalf) => {
                    format!("{}［{}］", full_name, t.thumbnail_left_generated_label())
                }
                Some(crate::spread_state::ThumbnailSourceKind::RightHalf) => {
                    format!("{}［{}］", full_name, t.thumbnail_right_generated_label())
                }
                _ => full_name.to_string(),
            };
            status_response.on_hover_text(full_name);
        }
        ui.separator();
        let mut slideshow_checked = slideshow_active;
        if ui.checkbox(&mut slideshow_checked, t.slideshow_toggle_label()).changed() {
            *slideshow_toggle = true;
            ui.close();
        }
        ui.separator();
        if ui.button(t.favorite_quick_add_label()).clicked() {
            *favorite_add = true;
            ui.close();
        }
        if ui.button(t.file_detail_menu()).clicked() {
            *open_file_detail = true;
            ui.close();
        }
        ui.separator();
        let mut blc_checked = blc_active;
        if ui.checkbox(&mut blc_checked, t.blue_light_cut_toggle_label()).changed() {
            *blc_toggle = true;
            ui.close();
        }
    }

    fn thumbnail_status_name(display_name: &str) -> String {
        let stem = std::path::Path::new(display_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(display_name);
        let chars: Vec<char> = stem.chars().collect();
        const KEEP: usize = 16;
        if chars.len() <= KEEP * 2 + 3 {
            stem.to_string()
        } else {
            format!("{}...{}", chars[..KEEP].iter().collect::<String>(), chars[chars.len() - KEEP..].iter().collect::<String>())
        }
    }

    /// 虫眼鏡ビューの更新（毎フレーム、描画前）。作り直し・窓サイズ追従・ホイール拡縮を行い、
    /// scroll_offset の押し込みが必要なら `magnifier_offset_dirty` を立てる。
    /// `wheel_anchor`（拡縮の基準点・画面座標）が None（パレット上など）ならホイールを無視する。
    /// ビューポート外も無視。
    fn update_magnifier(
        &mut self,
        viewport: egui::Rect,
        tex: Option<&egui::TextureHandle>,
        wheel_anchor: Option<egui::Pos2>,
        wheel_notches: f32,
        cfg: &crate::magnifier::MagnifierConfig,
    ) {
        use crate::magnifier::{fit_scale, notch_scale, rescale_for_new_texture, scale_range, zoom_about, clamp_offset, MagnifierView};
        let Some(tex) = tex else {
            self.magnifier_view = None;
            return;
        };
        let img = tex.size_vec2();
        let vp = viewport.size();
        let fit = fit_scale(vp, img);
        let range = scale_range(fit, cfg.max_scale());
        let page = self.spread_lo();

        let mut view = match self.magnifier_view {
            Some(mut v) if self.magnifier_page == page => {
                // 原寸デコードへの差し替えなどでテクスチャ寸法が変わったら、画面上の大きさを
                // 保つよう倍率を換算する（スクロール位置は画面px基準なのでそのまま）。
                if self.magnifier_img_size != img {
                    v.scale = rescale_for_new_texture(v.scale, self.magnifier_img_size.x, img.x);
                }
                // フィット表示のままなら窓サイズ変更にフィット倍率で追従する。
                // それ以外は範囲内へ丸め、スクロール範囲も収め直す。
                let scale = if self.magnifier_at_fit { fit } else { v.scale.clamp(range.0, range.1) };
                if scale != v.scale {
                    self.magnifier_offset_dirty = true;
                }
                clamp_offset(MagnifierView { scale, offset: v.offset }, vp, img)
            }
            _ => {
                self.magnifier_page = page;
                self.magnifier_offset_dirty = true;
                self.magnifier_bar_active_at = None;
                MagnifierView::fit(vp, img)
            }
        };

        if wheel_notches != 0.0
            && let Some(p) = wheel_anchor.filter(|p| viewport.contains(*p))
        {
            let next = notch_scale(view.scale, wheel_notches, cfg.notch_ratio(), range);
            if next != view.scale {
                view = zoom_about(view, p - viewport.min, vp, img, next);
                self.magnifier_offset_dirty = true;
            }
        }

        self.magnifier_at_fit = view.scale <= fit + 1e-4;
        self.magnifier_img_size = img;
        self.magnifier_view = Some(view);
    }

    /// 虫眼鏡カーソルの画像（画面の拡大率に合わせた大きさ）。同じ大きさの間は使い回す。
    fn magnifier_cursor_image(&mut self, pixels_per_point: f32) -> egui::CustomCursorImage {
        let size = crate::magnifier_cursor::cursor_size_for_scale(pixels_per_point);
        if let Some((cached_size, image)) = &self.magnifier_cursor
            && *cached_size == size
        {
            return image.clone();
        }
        let bitmap = crate::magnifier_cursor::magnifier_cursor(size);
        let image = egui::CustomCursorImage {
            rgba: std::sync::Arc::from(bitmap.rgba),
            size: [size as u16, size as u16],
            hotspot: [bitmap.hotspot.0 as u16, bitmap.hotspot.1 as u16],
        };
        self.magnifier_cursor = Some((size, image.clone()));
        image
    }

    /// 虫眼鏡のスライダーバー（下端中央・前面レイヤー）。倍率はフィット〜上限を対数で割り当て、
    /// 目盛りは原寸(100%)基準。操作は表示中心基準の拡縮。全体を click+drag の土台で覆い、
    /// 背面（画像のドラッグ・クリック・端ゾーン）へイベントを通さない。
    /// 自動ハイド中は描画だけをやめ、吸収は続ける（ホバーで再表示される）。
    #[allow(clippy::too_many_arguments)]
    fn draw_magnifier_bar(
        &mut self,
        ctx: &egui::Context,
        viewport: egui::Rect,
        tex: Option<&egui::TextureHandle>,
        rects: &crate::magnifier::BarRects,
        pointer_in_bar: bool,
        wheel_active: bool,
        time: f64,
        cfg: &mut crate::magnifier::MagnifierConfig,
    ) {
        use crate::magnifier::*;
        let (Some(tex), Some(view)) = (tex, self.magnifier_view) else { return };
        let img = tex.size_vec2();
        let vp = viewport.size();
        let fit = fit_scale(vp, img);
        let range = scale_range(fit, cfg.max_scale());

        if wheel_active || pointer_in_bar {
            self.magnifier_bar_active_at = Some(time);
        }
        let last_active = *self.magnifier_bar_active_at.get_or_insert(time);
        let idle = (time - last_active).max(0.0) as f32;
        let alpha = bar_alpha(idle, cfg.autohide_secs(), pointer_in_bar);
        if alpha > 0.0 && !pointer_in_bar {
            let remaining = cfg.autohide_secs() - idle;
            if remaining > 0.0 {
                ctx.request_repaint_after(Duration::from_secs_f32(remaining + 0.02));
            } else {
                ctx.request_repaint(); // フェード中
            }
        }

        let track = track_rect(rects.body);
        let enabled = range.1 > range.0;
        let mut pressed_x: Option<f32> = None;
        let (mut step_clicked, mut detail_clicked) = (false, false);
        let lang = crate::i18n::t();
        let (step_label, detail_on, ratio) = (cfg.notch_step().label(), cfg.detail_ticks(), cfg.notch_ratio());
        egui::Area::new(egui::Id::new("magnifier_bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(rects.total.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.allocate_rect(rects.total, egui::Sense::hover());
                // 土台: バー全体（ホバー拡張ぶんを含む）のクリック・ドラッグを握りつぶす。
                ui.interact(
                    rects.total.expand(BAR_HOVER_SLOP),
                    egui::Id::new("magnifier_bar_backstop"),
                    egui::Sense::click_and_drag(),
                );
                let body_resp = ui.interact(rects.body, egui::Id::new("magnifier_bar_body"), egui::Sense::click_and_drag());
                if enabled && body_resp.is_pointer_button_down_on() {
                    pressed_x = ui.input(|i| i.pointer.interact_pos()).map(|p| p.x);
                }
                // ボタン: ホバーで再表示されるので、非表示中に押されることはない。
                let step_resp = rects.step_button.map(|r| {
                    ui.interact(r, egui::Id::new("magnifier_step_btn"), egui::Sense::click())
                        .on_hover_text(lang.magnifier_notch_step_hint())
                });
                let detail_resp = rects.detail_button.map(|r| {
                    ui.interact(r, egui::Id::new("magnifier_detail_btn"), egui::Sense::click())
                        .on_hover_text(lang.magnifier_detail_hint())
                });
                step_clicked = step_resp.as_ref().is_some_and(|r| r.clicked());
                detail_clicked = detail_resp.as_ref().is_some_and(|r| r.clicked());
                if alpha <= 0.0 {
                    return;
                }
                let a = |base: u8| (base as f32 * alpha).round() as u8;
                let painter = ui.painter();
                painter.rect_filled(rects.total, 6.0, egui::Color32::from_black_alpha(a(200)));
                let paint_button = |rect: Option<egui::Rect>, resp: &Option<egui::Response>, text: &str| {
                    let (Some(rect), Some(resp)) = (rect, resp) else { return };
                    let fill = egui::Color32::from_white_alpha(a(if resp.hovered() { 70 } else { 35 }));
                    painter.rect_filled(rect, 4.0, fill);
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_white_alpha(a(235)),
                    );
                };
                paint_button(rects.step_button, &step_resp, step_label);
                paint_button(rects.detail_button, &detail_resp, lang.magnifier_detail_button_label(detail_on));

                let cy = track.center().y;
                let x_of = |scale: f32| track.left() + scale_to_t(scale, range) * track.width();
                painter.line_segment(
                    [egui::pos2(track.left(), cy), egui::pos2(track.right(), cy)],
                    egui::Stroke::new(2.0, egui::Color32::from_white_alpha(a(110))),
                );
                let thumb_x = x_of(view.scale);
                let label_y = rects.body.top() + BAR_LABEL_H / 2.0;
                let label_font = egui::FontId::proportional(10.0);
                for tick in tick_scales(range, detail_on, ratio) {
                    let x = x_of(tick);
                    let is_actual = (tick - ACTUAL_SCALE).abs() < 1e-3;
                    let half = if is_actual { 6.0 } else { 3.5 };
                    let color = if is_actual {
                        egui::Color32::from_rgba_unmultiplied(230, 169, 79, a(230))
                    } else {
                        egui::Color32::from_white_alpha(a(140))
                    };
                    painter.line_segment([egui::pos2(x, cy - half), egui::pos2(x, cy + half)], egui::Stroke::new(1.5, color));
                    // 詳細は目盛りが密なので、数値は原寸(100%)だけに付ける。
                    if (!detail_on || is_actual) && (x - thumb_x).abs() > 26.0 {
                        painter.text(egui::pos2(x, label_y), egui::Align2::CENTER_CENTER, format_percent(tick), label_font.clone(), color);
                    }
                }
                let radius = (track.height() / 2.0 - 1.0).clamp(3.0, 7.0);
                painter.circle_filled(egui::pos2(thumb_x, cy), radius, egui::Color32::from_white_alpha(a(240)));
                let value_x = thumb_x.clamp(rects.body.left() + 20.0, rects.body.right() - 20.0);
                painter.text(
                    egui::pos2(value_x, label_y),
                    egui::Align2::CENTER_CENTER,
                    format_percent(view.scale),
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_white_alpha(a(255)),
                );
            });

        if step_clicked {
            cfg.cycle_notch_step();
        }
        if detail_clicked {
            cfg.toggle_detail_ticks();
        }
        if step_clicked || detail_clicked {
            self.magnifier_settings_dirty = true;
            self.magnifier_bar_active_at = Some(time);
        }

        if let Some(x) = pressed_x {
            let target = slider_scale_at(x, (track.left(), track.right()), range, BAR_ACTUAL_MAGNET_PX);
            if (target - view.scale).abs() > 1e-6 {
                self.magnifier_view = Some(zoom_about(view, vp / 2.0, vp, img, target));
                self.magnifier_offset_dirty = true;
                self.magnifier_at_fit = target <= fit + 1e-4;
            }
            self.magnifier_bar_active_at = Some(time);
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
        let sort_toggle_enabled = self.sort_save_toggle_enabled();
        let sort_toggle_on = self.sort_save_toggle_on();
        let bookmark_toggle_enabled = self.bookmark_save_toggle_enabled();
        let bookmark_toggle_on = self.bookmark_save_toggle_on();
        let sort_changed = self.sort_save_changed();
        let current_sort = self.current_sort_snapshot();
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            // 虫眼鏡の有効中は、原寸表示と同じスクロール描画に倍率だけ差し込む。
            let magnifier = self.magnifier_view;
            if zoom_actual || magnifier.is_some() {
                // ビューポートより画像が小さい場合は中央寄せ、大きい場合はスクロール領域いっぱいに
                // 敷いて従来どおりの原寸表示にする。90/270度時は回転後の外接サイズで
                // スクロール範囲を確保してから、その中心を軸に回転させる（等倍・拡縮なし）。
                let outer_available = ui.available_size();
                // スクロールバー操作に加え、画像を直接D&D（フリック）してビューポート内へ
                // 引き込む操作にも対応する（マウスでも常時有効化。標準はタッチ限定）。
                // ホイールはページ送り専用に譲る（見開き原寸と同様、ここで拾うと二重に効く）。
                let mut scroll_area = egui::ScrollArea::both()
                    .scroll_source(egui::containers::scroll_area::ScrollSource {
                        scroll_bar: true,
                        drag: egui::containers::scroll_area::DragScroll::Always,
                        mouse_wheel: false,
                    });
                // 拡縮・作り直しをしたフレームだけオフセットを押し込む（毎フレーム押すとドラッグを上書きする）。
                if let Some(m) = magnifier
                    && self.magnifier_offset_dirty
                {
                    scroll_area = scroll_area.scroll_offset(m.offset);
                }
                let scroll_out = scroll_area.show(ui, |ui| {
                    let scale = magnifier.map_or(1.0, |m| m.scale);
                    let img_size = egui::vec2(img_w as f32, img_h as f32) * scale;
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
                        Self::paint_texture_rotated_at(ui.painter(), tex, bbox.center(), scale, angle_deg);
                    }
                    let menu_open = resp.context_menu_opened() || self.tool_palette_menu_open;
                    if !menu_open {
                        if resp.double_clicked() { *double_clicked = true; }
                        if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
                    }
                    if resp.secondary_clicked() && !self.tool_palette_menu_open {
                        self.set_thumbnail_context(Some(self.spread_lo()));
                    }
                    let thumbnail_target = self.thumbnail_context_entry.as_ref();
                    let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
                    let saved_thumbnail_display = self.saved_thumbnail_display_name();
                    let slideshow_active = self.is_slideshow_active();
                    let blc_active = self.is_blc_active();
                    let action = &mut self.pending_spread_action;
                    let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
                    let thumbnail_action = &mut self.pending_thumbnail_action;
                    let favorite_add = &mut self.pending_favorite_add;
                    let open_file_detail = &mut self.pending_open_file_detail;
                    let slideshow_toggle = &mut self.pending_slideshow_toggle;
                    let blc_toggle = &mut self.pending_blc_toggle;
                    egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));
                });
                // ドラッグ・スクロールバーでの移動を虫眼鏡ビューへ取り込む。
                if let Some(m) = self.magnifier_view.as_mut() {
                    m.offset = scroll_out.state.offset;
                }
                self.magnifier_offset_dirty = false;
            } else {
                let available = ui.available_size();
                let bounds = egui::Rect::from_min_size(ui.cursor().left_top(), available);
                let _fit = Self::paint_page_rotated(ui.painter(), tex, bounds, angle_deg);
                // 見開き表示と同様、左クリック／ダブルクリックも表示領域全体（画像周囲の
                // 余白含む）で受け付ける。画像本体への限定は原寸切替が阻害される
                // 原因になっていたため撤去（右クリックメニューは元々全域対応済み）。
                let resp  = ui.allocate_rect(bounds, egui::Sense::click());
                let menu_open = resp.context_menu_opened() || self.tool_palette_menu_open;
                if !menu_open {
                    if resp.double_clicked() { *double_clicked = true; }
                    if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
                }
                if resp.secondary_clicked() && !self.tool_palette_menu_open {
                    self.set_thumbnail_context(Some(self.spread_lo()));
                }
                let thumbnail_target = self.thumbnail_context_entry.as_ref();
                let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
                let saved_thumbnail_display = self.saved_thumbnail_display_name();
                let slideshow_active = self.is_slideshow_active();
                let blc_active = self.is_blc_active();
                let action = &mut self.pending_spread_action;
                let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
                let thumbnail_action = &mut self.pending_thumbnail_action;
                let favorite_add = &mut self.pending_favorite_add;
                let open_file_detail = &mut self.pending_open_file_detail;
                let slideshow_toggle = &mut self.pending_slideshow_toggle;
                let blc_toggle = &mut self.pending_blc_toggle;
                egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));
            }
        } else {
            let rect = egui::Rect::from_min_size(ui.cursor().left_top(), ui.available_size());
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_gray(40));
            if resp.secondary_clicked() && !self.tool_palette_menu_open {
                self.set_thumbnail_context(Some(self.spread_lo()));
            }
            let thumbnail_target = self.thumbnail_context_entry.as_ref();
            let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
            let saved_thumbnail_display = self.saved_thumbnail_display_name();
            let slideshow_active = self.is_slideshow_active();
            let blc_active = self.is_blc_active();
            let action = &mut self.pending_spread_action;
            let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
            let thumbnail_action = &mut self.pending_thumbnail_action;
            let favorite_add = &mut self.pending_favorite_add;
            let open_file_detail = &mut self.pending_open_file_detail;
            let slideshow_toggle = &mut self.pending_slideshow_toggle;
            let blc_toggle = &mut self.pending_blc_toggle;
            egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));
        }
    }

    fn render_spread(
        &mut self,
        ui: &mut egui::Ui,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        left_index: i32,
        right_index: i32,
        monitor: Option<egui::Vec2>,
        angle_deg: i32,
        zoom_actual: bool,
        double_clicked: &mut bool,
        single_clicked: &mut bool,
    ) {
        let toggle_enabled = self.spread_save_toggle_enabled();
        let toggle_on = self.spread_save_toggle_on();
        let overwrite_enabled = self.spread_overwrite_enabled();
        let sort_toggle_enabled = self.sort_save_toggle_enabled();
        let sort_toggle_on = self.sort_save_toggle_on();
        let bookmark_toggle_enabled = self.bookmark_save_toggle_enabled();
        let bookmark_toggle_on = self.bookmark_save_toggle_on();
        let sort_changed = self.sort_save_changed();
        let current_sort = self.current_sort_snapshot();

        // 原寸表示は回転(90/270度)には未対応。回転中は従来通りフィット表示にフォールバックする。
        if zoom_actual && angle_deg == 0 {
            self.render_spread_actual(
                ui, tex_left, tex_right, left_index, right_index, double_clicked, single_clicked,
                toggle_enabled, toggle_on, overwrite_enabled,
                sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort,
                bookmark_toggle_enabled, bookmark_toggle_on,
            );
            return;
        }

        let available = ui.available_size();
        let origin = ui.cursor().left_top();

        let full_rect = egui::Rect::from_min_size(origin, available);
        let resp = ui.allocate_rect(full_rect, egui::Sense::click());
        let menu_open = resp.context_menu_opened() || self.tool_palette_menu_open;
        if !menu_open {
            if resp.double_clicked() { *double_clicked = true; }
            if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
        }
        if resp.secondary_clicked() && !self.tool_palette_menu_open {
            if let Some(pos) = resp.interact_pointer_pos() {
                let index = self.thumbnail_target_for_spread(
                    pos, full_rect, tex_left, tex_right, left_index, right_index, monitor, angle_deg,
                );
                self.set_thumbnail_context(index);
            }
        }
        let thumbnail_target = self.thumbnail_context_entry.as_ref();
        let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
        let saved_thumbnail_display = self.saved_thumbnail_display_name();
        let slideshow_active = self.is_slideshow_active();
        let blc_active = self.is_blc_active();
        let action = &mut self.pending_spread_action;
        let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
        let thumbnail_action = &mut self.pending_thumbnail_action;
        let favorite_add = &mut self.pending_favorite_add;
        let open_file_detail = &mut self.pending_open_file_detail;
        let slideshow_toggle = &mut self.pending_slideshow_toggle;
        let blc_toggle = &mut self.pending_blc_toggle;
        egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));

        if angle_deg == 0 {
            let (rect_l, rect_r) = Self::spread_rects(available, origin, tex_left, tex_right, monitor);
            let painter = ui.painter();
            Self::paint_page(painter, tex_left,  rect_l);
            Self::paint_page(painter, tex_right, rect_r);
        } else {
            Self::paint_spread_rotated(ui.painter(), full_rect, tex_left, tex_right, angle_deg);
        }
    }

    /// 見開き原寸表示（zoom_actual、角度0限定）。2ページをそれぞれ原寸のまま
    /// ノド（境界線）で突き合わせ、天（上端）を揃えて描画する。高さが異なる方は
    /// 天からその高さ分だけ描画し、残りは余白のまま。ビューポートより大きければ
    /// ScrollArea（スクロールバー＋D&Dパン）で全域を閲覧できるようにする。
    #[allow(clippy::too_many_arguments)]
    fn render_spread_actual(
        &mut self,
        ui: &mut egui::Ui,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        left_index: i32,
        right_index: i32,
        double_clicked: &mut bool,
        single_clicked: &mut bool,
        toggle_enabled: bool,
        toggle_on: bool,
        overwrite_enabled: bool,
        sort_toggle_enabled: bool,
        sort_toggle_on: bool,
        sort_changed: bool,
        current_sort: (ViewerSortKey, bool),
        bookmark_toggle_enabled: bool,
        bookmark_toggle_on: bool,
    ) {
        let outer_available = ui.available_size();
        let sl = Self::spread_page_size(tex_left);
        let sr = Self::spread_page_size(tex_right);
        let image_size = egui::vec2(sl.x + sr.x, sl.y.max(sr.y));
        let content_size = image_size.max(outer_available);

        // 見開きが実際に切り替わった最初のフレームでだけ、進行方向に応じた
        // 初期スクロール位置（左端上端 or 右端上端）をセットする。毎フレームセット
        // するとユーザーのドラッグ操作を毎回上書きしてしまうため。
        let current_lo = self.spread_lo();
        // マウスホイールはページ送り専用に譲る（ここで拾うと二重に効いてしまう）。
        // スクロールバー操作とコンテンツのD&Dパンのみ有効にする。
        let mut scroll_area = egui::ScrollArea::both()
            .scroll_source(egui::containers::scroll_area::ScrollSource {
                scroll_bar: true,
                drag: egui::containers::scroll_area::DragScroll::Always,
                mouse_wheel: false,
            });
        if self.spread_actual_scrolled_lo != Some(current_lo) {
            self.spread_actual_scrolled_lo = Some(current_lo);
            let max_scroll_x = (content_size.x - outer_available.x).max(0.0);
            // anim_dir: +1=新ページが右からIN(右へ進行) → 左端から見せる、
            //           -1=左からIN(左へ進行) → 右端から見せる。
            let target_x = if self.anim_dir < 0 { max_scroll_x } else { 0.0 };
            scroll_area = scroll_area.scroll_offset(egui::vec2(target_x, 0.0));
        }

        scroll_area.show(ui, |ui| {
            let (content_rect, resp) = ui.allocate_exact_size(content_size, egui::Sense::click());
            let bbox = egui::Rect::from_min_size(
                content_rect.min + (content_size - image_size) / 2.0,
                image_size,
            );
            let rect_l = egui::Rect::from_min_size(bbox.min, sl);
            let rect_r = egui::Rect::from_min_size(egui::pos2(bbox.min.x + sl.x, bbox.min.y), sr);
            let painter = ui.painter();
            Self::paint_page(painter, tex_left,  rect_l);
            Self::paint_page(painter, tex_right, rect_r);

            let menu_open = resp.context_menu_opened() || self.tool_palette_menu_open;
            if !menu_open {
                if resp.double_clicked() { *double_clicked = true; }
                if resp.clicked() && !resp.double_clicked() { *single_clicked = true; }
            }
            if resp.secondary_clicked() && !self.tool_palette_menu_open {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let index = self.thumbnail_target_from_rects(pos, rect_l, rect_r, left_index, right_index);
                    self.set_thumbnail_context(index);
                }
            }
            let thumbnail_target = self.thumbnail_context_entry.as_ref();
            let saved_thumbnail_selection = self.saved_thumbnail_selection.as_ref();
            let saved_thumbnail_display = self.saved_thumbnail_display_name();
            let slideshow_active = self.is_slideshow_active();
            let blc_active = self.is_blc_active();
            let action = &mut self.pending_spread_action;
            let sort_action = &mut self.pending_sort_action;
                let bookmark_action = &mut self.pending_bookmark_action;
            let thumbnail_action = &mut self.pending_thumbnail_action;
            let favorite_add = &mut self.pending_favorite_add;
            let open_file_detail = &mut self.pending_open_file_detail;
            let slideshow_toggle = &mut self.pending_slideshow_toggle;
            let blc_toggle = &mut self.pending_blc_toggle;
            egui::Popup::context_menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| Self::spread_save_context_menu(ui, toggle_enabled, toggle_on, overwrite_enabled, action, sort_toggle_enabled, sort_toggle_on, sort_changed, current_sort, sort_action, bookmark_toggle_enabled, bookmark_toggle_on, bookmark_action, thumbnail_target, saved_thumbnail_selection, saved_thumbnail_display.as_deref(), thumbnail_action, favorite_add, open_file_detail, slideshow_active, slideshow_toggle, blc_active, blc_toggle));
        });
    }

    /// サムネイル登録対象のヒットテスト。片側が仮想ページなら、クリック位置に
    /// 関係なく実ページ側を返す。両側が実ページのときだけ描画されたページ矩形を判定する。
    fn thumbnail_target_for_spread(
        &self,
        pos: egui::Pos2,
        bounds: egui::Rect,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        left_index: i32,
        right_index: i32,
        monitor: Option<egui::Vec2>,
        angle_deg: i32,
    ) -> Option<i32> {
        if angle_deg == 0 {
            let (rect_l, rect_r) = Self::spread_rects(
                bounds.size(), bounds.min, tex_left, tex_right, monitor,
            );
            return self.thumbnail_target_from_rects(pos, rect_l, rect_r, left_index, right_index);
        }

        let total = self.entries.len() as i32;
        let left_real = (0..total).contains(&left_index);
        let right_real = (0..total).contains(&right_index);
        match (left_real, right_real) {
            (true, false) => return Some(left_index),
            (false, true) => return Some(right_index),
            (false, false) => return None,
            (true, true) => {}
        }

        let (local_left, local_right) = Self::spread_local_rects(tex_left, tex_right);
        let (hit_left, hit_right) = match Self::spread_rotation_fit(local_left, local_right, bounds, angle_deg) {
            Some((center_left, center_right, scale)) => (
                Self::rotated_rect_contains(pos, center_left, local_left.size() * scale / 2.0, angle_deg),
                Self::rotated_rect_contains(pos, center_right, local_right.size() * scale / 2.0, angle_deg),
            ),
            None => (false, false),
        };

        if hit_left {
            Some(left_index)
        } else if hit_right {
            Some(right_index)
        } else {
            None
        }
    }

    /// 見開きヒットテストの共通部分：片側が仮想ページなら無条件に実ページ側を返し、
    /// 両側が実ページのときだけ与えられた矩形で判定する（フィット表示・原寸表示共通）。
    fn thumbnail_target_from_rects(
        &self,
        pos: egui::Pos2,
        rect_l: egui::Rect,
        rect_r: egui::Rect,
        left_index: i32,
        right_index: i32,
    ) -> Option<i32> {
        let total = self.entries.len() as i32;
        let left_real = (0..total).contains(&left_index);
        let right_real = (0..total).contains(&right_index);
        match (left_real, right_real) {
            (true, false) => return Some(left_index),
            (false, true) => return Some(right_index),
            (false, false) => return None,
            (true, true) => {}
        }

        if rect_l.contains(pos) {
            Some(left_index)
        } else if rect_r.contains(pos) {
            Some(right_index)
        } else {
            None
        }
    }

    fn set_thumbnail_context(&mut self, index: Option<i32>) {
        self.thumbnail_context_entry = index
            .filter(|i| *i >= 0)
            .and_then(|i| self.entries.get(i as usize))
            .map(|entry| (entry.entry_name.clone(), entry.display_name.clone()));
    }

    fn saved_thumbnail_display_name(&self) -> Option<String> {
        let saved = &self.saved_thumbnail_selection.as_ref()?.entry_name;
        self.entries.iter()
            .find(|entry| &entry.entry_name == saved)
            .map(|entry| entry.display_name.clone())
    }

    fn rotated_rect_contains(
        point: egui::Pos2,
        center: egui::Pos2,
        half: egui::Vec2,
        angle_deg: i32,
    ) -> bool {
        let inverse = egui::emath::Rot2::from_angle(-(angle_deg as f32).to_radians());
        let local = inverse * (point - center);
        local.x.abs() <= half.x && local.y.abs() <= half.y
    }

    /// オフセット操作（見開き基点が±1だけ変化）の3ページ連続 tween。
    /// 旧・新見開きの共通ページを1回だけ描き、その移動量を退場/入場ページにも適用する。
    fn paint_offset_spread(
        painter: &egui::Painter,
        frame: &RenderFrame,
        available: egui::Vec2,
        origin: egui::Pos2,
        right_binding: bool,
    ) -> bool {
        let Some((old_only, shared, new_only)) =
            offset_transition_pages(frame.anim_from_lo, frame.current_lo)
        else {
            return false;
        };

        let old_textures = if right_binding {
            [&frame.prev_tex_hi, &frame.prev_tex_lo]
        } else {
            [&frame.prev_tex_lo, &frame.prev_tex_hi]
        };
        let new_textures = if right_binding {
            [&frame.tex_hi, &frame.tex_lo]
        } else {
            [&frame.tex_lo, &frame.tex_hi]
        };
        let old_pages = visual_spread_pages(frame.anim_from_lo, right_binding);
        let new_pages = visual_spread_pages(frame.current_lo, right_binding);
        let (old_l, old_r) = Self::spread_rects(
            available, origin, old_textures[0], old_textures[1], frame.monitor,
        );
        let (new_l, new_r) = Self::spread_rects(
            available, origin, new_textures[0], new_textures[1], frame.monitor,
        );
        let old_rects = [old_l, old_r];
        let new_rects = [new_l, new_r];

        let old_slot = |page| old_pages.iter().position(|candidate| *candidate == page);
        let new_slot = |page| new_pages.iter().position(|candidate| *candidate == page);
        let (Some(old_only_slot), Some(shared_old_slot), Some(shared_new_slot), Some(new_only_slot)) = (
            old_slot(old_only), old_slot(shared), new_slot(shared), new_slot(new_only),
        ) else {
            return false;
        };

        let shared_old_rect = old_rects[shared_old_slot];
        let shared_new_rect = new_rects[shared_new_slot];
        let shared_rect = lerp_rect(shared_old_rect, shared_new_rect, frame.t);
        let transition_bounds = lerp_rect(old_l.union(old_r), new_l.union(new_r), frame.t);
        let painter = painter.with_clip_rect(painter.clip_rect().intersect(transition_bounds));
        let old_only_rect = place_next_to(
            old_rects[old_only_slot], shared_rect, old_only_slot < shared_old_slot,
        );
        let new_only_rect = place_next_to(
            new_rects[new_only_slot], shared_rect, new_only_slot < shared_new_slot,
        );

        Self::paint_page(
            &painter,
            old_textures[old_only_slot],
            old_only_rect,
        );
        Self::paint_page(
            &painter,
            old_textures[shared_old_slot],
            shared_rect,
        );
        Self::paint_page(
            &painter,
            new_textures[new_only_slot],
            new_only_rect,
        );
        true
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

    /// `paint_page`のalpha指定版（クロスフェード用）。GPU側のアルファブレンドのみで
    /// 済ませるため、CPU側のピクセル合成は行わない。
    fn paint_page_alpha(painter: &egui::Painter, tex: &Option<egui::TextureHandle>, rect: egui::Rect, alpha: u8) {
        match tex {
            Some(t) => { painter.image(t.id(), rect, FULL_UV, egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha)); }
            None    => { painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(40, 40, 40, alpha)); }
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

    /// 時計回りワイプの分割数（フル1周あたり）。数十頂点程度なのでキャッシュ不要、
    /// 毎フレームその場で組み立てる。
    const WIPE_SEGMENTS_PER_CIRCLE: usize = 48;
    /// ワイプ境界のフェザー幅（度数を周率に変換した値）。境界のすぐ外側だけ
    /// アルファを255→0へ滑らかに落とし、境界のギザつきを緩和する。
    const WIPE_FEATHER_FRAC: f32 = 8.0 / 360.0;

    /// 時計12時位置を起点に時計回りで進む扇の周上の点を返す（frac: 0.0=12時、0.25=3時...）。
    fn wipe_point(center: egui::Pos2, radius: f32, frac: f32) -> egui::Pos2 {
        let angle = frac * std::f32::consts::TAU;
        center + egui::vec2(angle.sin(), -angle.cos()) * radius
    }

    /// 時計回りワイプの新ページ側オーバーレイを描く（旧ページは呼び出し側が先に
    /// 不透明で描画しておくこと）。境界に数度分のアルファグラデーション(フェザー)を
    /// 付けて滑らかに見せる。扇形メッシュ(頂点数は数十程度)を毎フレーム組み立てるだけで、
    /// CPU側のピクセル合成やシェーダー追加は行わない。
    fn paint_clockwise_wipe_overlay(
        painter: &egui::Painter,
        tex: &Option<egui::TextureHandle>,
        rect: egui::Rect,
        t: f32,
    ) {
        let Some(tex) = tex else { return };
        if !rect.is_finite() || rect.width() < 1.0 || rect.height() < 1.0 { return; }
        let t = t.clamp(0.0, 1.0);
        let swept = (t + Self::WIPE_FEATHER_FRAC).min(1.0);
        if swept <= 0.0 { return; }

        let center = rect.center();
        // 矩形の対角線半分より少し大きい半径にして、扇の外周が矩形を確実に覆うようにする
        // （実際の表示範囲はクリップで矩形内に絞るので、はみ出し分のコストは無視できる）。
        let radius = (rect.width().powi(2) + rect.height().powi(2)).sqrt() / 2.0 + 1.0;
        let steps = ((Self::WIPE_SEGMENTS_PER_CIRCLE as f32 * swept).ceil() as usize).max(1);

        let alpha_at = |frac: f32| -> u8 {
            if frac <= t {
                255
            } else {
                let fade = (1.0 - (frac - t) / Self::WIPE_FEATHER_FRAC).clamp(0.0, 1.0);
                (fade * 255.0).round() as u8
            }
        };
        let uv_at = |p: egui::Pos2| -> egui::Pos2 {
            egui::pos2(
                (p.x - rect.min.x) / rect.width(),
                (p.y - rect.min.y) / rect.height(),
            )
        };
        let vertex_at = |frac: f32| -> egui::epaint::Vertex {
            let p = Self::wipe_point(center, radius, frac);
            egui::epaint::Vertex {
                pos: p,
                uv: uv_at(p),
                color: egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha_at(frac)),
            }
        };
        let center_vertex = |frac: f32| -> egui::epaint::Vertex {
            egui::epaint::Vertex {
                pos: center,
                uv: uv_at(center),
                color: egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha_at(frac)),
            }
        };

        let mut mesh = egui::Mesh::with_texture(tex.id());
        for i in 0..steps {
            let frac_a = swept * (i as f32) / (steps as f32);
            let frac_b = swept * ((i + 1) as f32) / (steps as f32);
            let base = mesh.vertices.len() as u32;
            mesh.vertices.push(center_vertex(frac_a));
            mesh.vertices.push(vertex_at(frac_a));
            mesh.vertices.push(vertex_at(frac_b));
            mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        painter.with_clip_rect(painter.clip_rect().intersect(rect)).add(mesh);
    }

    /// 単ページの「フィット表示」矩形（avail内にアスペクト比を保って収める）を返す。
    /// クロスフェード/ワイプで旧・新それぞれ自分のテクスチャの自然な矩形を使うために使う。
    fn single_fit_rect(avail: egui::Vec2, origin: egui::Pos2, tex: &Option<egui::TextureHandle>) -> egui::Rect {
        let Some(tex) = tex else { return egui::Rect::NOTHING };
        let [img_w, img_h] = tex.size();
        if img_w == 0 || img_h == 0 { return egui::Rect::NOTHING; }
        let scale = (avail.x / img_w as f32).min(avail.y / img_h as f32);
        let size = egui::vec2(img_w as f32 * scale, img_h as f32 * scale);
        let tl = origin + (avail - size) / 2.0;
        egui::Rect::from_min_size(tl, size)
    }

    /// 単ページをoffset無し・alpha指定で描画（クロスフェード用）
    fn paint_single_alpha(
        painter: &egui::Painter,
        tex: &Option<egui::TextureHandle>,
        avail: egui::Vec2,
        origin: egui::Pos2,
        alpha: u8,
    ) {
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            let scale = (avail.x / img_w as f32).min(avail.y / img_h as f32);
            let size  = egui::vec2(img_w as f32 * scale, img_h as f32 * scale);
            let tl    = origin + (avail - size) / 2.0;
            painter.image(tex.id(), egui::Rect::from_min_size(tl, size), FULL_UV, egui::Color32::from_rgba_unmultiplied(255, 255, 255, alpha));
        }
    }
}

#[cfg(test)]
mod offset_tween_tests {
    use super::{lerp_rect, offset_transition_pages, place_next_to, visual_spread_pages};

    #[test]
    fn virtual_first_shift_has_one_shared_real_page() {
        assert_eq!(offset_transition_pages(-1, 0), Some((-1, 0, 1)));
        assert_eq!(offset_transition_pages(0, -1), Some((1, 0, -1)));
    }

    #[test]
    fn middle_offset_shifts_have_one_shared_page() {
        assert_eq!(offset_transition_pages(0, 1), Some((0, 1, 2)));
        assert_eq!(offset_transition_pages(1, 0), Some((2, 1, 0)));
    }

    #[test]
    fn ordinary_spread_navigation_does_not_use_offset_tween() {
        assert_eq!(offset_transition_pages(0, 2), None);
        assert_eq!(offset_transition_pages(2, 0), None);
        assert_eq!(offset_transition_pages(0, 0), None);
    }

    #[test]
    fn binding_direction_only_reverses_visual_page_order() {
        assert_eq!(visual_spread_pages(-1, false), [-1, 0]);
        assert_eq!(visual_spread_pages(0, false), [0, 1]);
        assert_eq!(visual_spread_pages(-1, true), [0, -1]);
        assert_eq!(visual_spread_pages(0, true), [1, 0]);
    }

    #[test]
    fn shared_page_rect_matches_old_and_new_layout_at_endpoints() {
        let old = egui::Rect::from_min_size(egui::pos2(50.0, 20.0), egui::vec2(100.0, 200.0));
        let new = egui::Rect::from_min_size(egui::pos2(10.0, 30.0), egui::vec2(120.0, 180.0));
        assert_eq!(lerp_rect(old, new, 0.0), old);
        assert_eq!(lerp_rect(old, new, 1.0), new);
    }

    #[test]
    fn unique_pages_stay_connected_to_the_shared_page() {
        let shared = egui::Rect::from_min_size(egui::pos2(100.0, 20.0), egui::vec2(80.0, 160.0));
        let unique = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(60.0, 120.0));
        let left = place_next_to(unique, shared, true);
        let right = place_next_to(unique, shared, false);
        assert_eq!(left.right(), shared.left());
        assert_eq!(right.left(), shared.right());
        assert_eq!(left.center().y, shared.center().y);
        assert_eq!(right.center().y, shared.center().y);
    }
}

#[cfg(test)]
mod animation_schedule_tests {
    use super::{animation_instance_changed, next_anim_decode_request, ANIM_DECODE_AHEAD_FRAMES};
    use crate::cache::AnimationInstanceId;

    #[test]
    fn request_window_is_anchored_to_displayed_frame() {
        assert_eq!(
            next_anim_decode_request(9, 9, 32),
            9 + ANIM_DECODE_AHEAD_FRAMES,
        );
    }

    #[test]
    fn producer_progress_cannot_extend_a_stationary_display_window() {
        assert_eq!(next_anim_decode_request(0, 8, 32), 8);
        assert_eq!(next_anim_decode_request(0, 8, 32), 8);
    }

    #[test]
    fn request_window_never_exceeds_ring_capacity() {
        assert_eq!(next_anim_decode_request(10, 10, 4), 14);
    }

    #[test]
    fn decode_request_saturates_at_usize_max() {
        assert_eq!(next_anim_decode_request(usize::MAX, usize::MAX, 32), usize::MAX);
    }

    #[test]
    fn animation_state_identity_ignores_same_instance_and_rejects_replacement() {
        let first = AnimationInstanceId::for_test(1);
        let replacement = AnimationInstanceId::for_test(2);

        assert!(!animation_instance_changed(None, first));
        assert!(!animation_instance_changed(Some(first), first));
        assert!(animation_instance_changed(Some(first), replacement));
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

    #[test]
    fn rotated_hit_test_tracks_the_drawn_page_at_all_supported_angles() {
        let center = egui::pos2(100.0, 100.0);
        let half = egui::vec2(40.0, 20.0);
        for angle in [0, 90, 180, 270] {
            let rot = egui::emath::Rot2::from_angle((angle as f32).to_radians());
            let inside = center + rot * egui::vec2(30.0, 10.0);
            let outside = center + rot * egui::vec2(50.0, 10.0);
            assert!(ViewerState::rotated_rect_contains(inside, center, half, angle));
            assert!(!ViewerState::rotated_rect_contains(outside, center, half, angle));
        }
    }

    #[test]
    fn thumbnail_status_name_removes_extension_and_elides_the_middle() {
        assert_eq!(ViewerState::thumbnail_status_name("cover.page.jpg"), "cover.page");
        let long = "1234567890abcdefghijklmnopqrstuvwx9876543210.jpg";
        assert_eq!(
            ViewerState::thumbnail_status_name(long),
            "1234567890abcdef...stuvwx9876543210"
        );
    }
}

#[cfg(test)]
mod entry_list_scroll_tests {
    use super::ViewerState;

    #[test]
    fn anchor_uses_current_page_in_normal_range() {
        assert_eq!(ViewerState::entry_list_scroll_anchor(7, 20), Some(7));
    }

    #[test]
    fn anchor_clamps_virtual_spread_pages_to_real_entries() {
        assert_eq!(ViewerState::entry_list_scroll_anchor(-1, 20), Some(0));
        assert_eq!(ViewerState::entry_list_scroll_anchor(20, 20), Some(19));
    }

    #[test]
    fn anchor_is_absent_for_an_empty_archive() {
        assert_eq!(ViewerState::entry_list_scroll_anchor(0, 0), None);
    }

    #[test]
    fn marker_moves_from_top_until_it_reaches_the_center_stopper() {
        assert_eq!(
            ViewerState::entry_list_follow_offset(0.0, 80.0, 150.0, 1_000.0),
            0.0,
        );
    }

    #[test]
    fn list_scrolls_once_marker_reaches_the_center_stopper() {
        assert_eq!(
            ViewerState::entry_list_follow_offset(120.0, 180.0, 150.0, 1_000.0),
            150.0,
        );
    }

    #[test]
    fn list_stops_at_the_end_and_marker_can_move_below_center() {
        assert_eq!(
            ViewerState::entry_list_follow_offset(980.0, 180.0, 150.0, 1_000.0),
            1_000.0,
        );
    }
}

#[cfg(test)]
mod sort_save_state_tests {
    use super::*;

    fn viewer() -> ViewerState {
        ViewerState::new_raw(PathBuf::from("test.png"), [None; 4], None)
    }

    fn archive_viewer() -> ViewerState {
        let mut viewer = viewer();
        viewer.is_raw_file = false;
        viewer.entries.push(ViewerEntry {
            entry_name: "second".to_string(),
            display_name: "second".to_string(),
            date_key: 1,
            original_index: 1,
        });
        viewer
    }

    #[test]
    fn saved_spread_offset_normalizes_both_shift_directions_to_virtual_first() {
        assert_eq!(normalize_saved_spread_offset(0), 0);
        assert_eq!(normalize_saved_spread_offset(-1), -1);
        assert_eq!(normalize_saved_spread_offset(1), -1);
        assert_eq!(normalize_saved_spread_offset(-2), -1);
        assert_eq!(normalize_saved_spread_offset(2), -1);
    }

    #[test]
    fn restoring_shifted_saved_spread_always_keeps_the_first_real_page() {
        for mode in [PageMode::SpreadLeft, PageMode::SpreadRight] {
            for offset in [-1, 1] {
                let mut viewer = archive_viewer();
                let mut cfg = ViewerConfig::default();

                viewer.restore_saved_spread(mode, offset, &mut cfg);

                assert_eq!(viewer.spread_lo(), -1, "saved offset {offset}");
                assert_eq!(viewer.offset.value(), -1, "saved offset {offset}");
                assert!(viewer.page_mode == mode);
            }
        }
    }

    #[test]
    fn aligned_saved_spread_starts_from_the_first_real_page() {
        let mut viewer = archive_viewer();
        let mut cfg = ViewerConfig::default();

        viewer.restore_saved_spread(PageMode::SpreadRight, 0, &mut cfg);

        assert_eq!(viewer.spread_lo(), 0);
        assert_eq!(viewer.offset.value(), 0);
    }

    #[test]
    fn opposite_runtime_shift_directions_are_the_same_saved_setting() {
        let mut viewer = archive_viewer();
        viewer.page_mode = PageMode::SpreadLeft;
        viewer.offset.advance();
        viewer.set_saved_spread(Some((PageMode::SpreadLeft, -1)));

        assert!(!viewer.spread_overwrite_enabled());

        viewer.page_mode = PageMode::SpreadRight;
        assert!(viewer.spread_overwrite_enabled());
    }

    #[test]
    fn backward_shift_is_blocked_when_it_would_show_two_virtual_pages() {
        let mut viewer = archive_viewer();
        viewer.page_mode = PageMode::SpreadLeft;
        viewer.offset.advance();
        viewer.spread_base = -2;
        assert_eq!(viewer.spread_lo(), -1);

        assert!(!viewer.can_shift_backward());
        viewer.shift_offset_backward();

        assert_eq!(viewer.spread_base, -2);
        assert_eq!(viewer.offset.value(), 1);
        assert_eq!(viewer.spread_lo(), -1);
    }

    #[test]
    fn forward_shift_is_blocked_when_it_would_show_two_virtual_pages() {
        let mut viewer = archive_viewer();
        viewer.page_mode = PageMode::SpreadRight;
        viewer.offset.retreat();
        viewer.spread_base = 2;
        assert_eq!(viewer.spread_lo(), 1);

        assert!(!viewer.can_shift_forward());
        viewer.shift_offset_forward();

        assert_eq!(viewer.spread_base, 2);
        assert_eq!(viewer.offset.value(), -1);
        assert_eq!(viewer.spread_lo(), 1);
    }

    #[test]
    fn offset_shift_remains_available_when_the_result_contains_a_real_page() {
        let mut viewer = archive_viewer();
        viewer.page_mode = PageMode::SpreadLeft;

        assert!(viewer.can_shift_backward());
        viewer.shift_offset_backward();
        assert_eq!(viewer.spread_lo(), -1);

        assert!(viewer.can_shift_forward());
        viewer.shift_offset_forward();
        assert_eq!(viewer.spread_lo(), 0);

        assert!(viewer.can_shift_forward());
        viewer.shift_offset_forward();
        assert_eq!(viewer.spread_lo(), 1);
    }

    #[test]
    fn single_page_archive_never_allows_a_shift_past_its_only_real_page() {
        let mut viewer = archive_viewer();
        viewer.entries.truncate(1);
        viewer.page_mode = PageMode::SpreadRight;

        assert!(!viewer.can_shift_forward());
        viewer.shift_offset_forward();
        assert_eq!(viewer.spread_lo(), 0);

        assert!(viewer.can_shift_backward());
        viewer.shift_offset_backward();
        assert_eq!(viewer.spread_lo(), -1);
        assert!(!viewer.can_shift_backward());
    }

    #[test]
    fn unsaved_viewer_keeps_existing_name_ascending_default() {
        let viewer = viewer();
        let current = viewer.current_sort_snapshot();
        assert!(matches!(current.0, ViewerSortKey::Name));
        assert!(current.1);
        assert!(!viewer.sort_save_toggle_on());
        assert!(!viewer.sort_save_changed());
    }

    #[test]
    fn saved_sort_reports_changes_to_key_or_direction() {
        let mut viewer = viewer();
        viewer.set_saved_sort(Some((ViewerSortKey::Name, true)));
        assert!(viewer.sort_save_toggle_on());
        assert!(!viewer.sort_save_changed());

        viewer.sort_key = ViewerSortKey::Natural;
        assert!(viewer.sort_save_changed());

        viewer.sort_key = ViewerSortKey::Name;
        viewer.sort_ascending = false;
        assert!(viewer.sort_save_changed());

        viewer.set_saved_sort(Some((ViewerSortKey::Name, false)));
        assert!(!viewer.sort_save_changed());
    }

    #[test]
    fn clearing_saved_sort_keeps_current_sort_and_page_position() {
        let mut viewer = viewer();
        viewer.entries = vec![
            ViewerEntry {
                entry_name: "b".to_string(),
                display_name: "b".to_string(),
                date_key: 2,
                original_index: 0,
            },
            ViewerEntry {
                entry_name: "a".to_string(),
                display_name: "a".to_string(),
                date_key: 1,
                original_index: 1,
            },
        ];
        viewer.sort_key = ViewerSortKey::Date;
        viewer.sort_ascending = false;
        viewer.saved_sort = Some((ViewerSortKey::Date, false));
        viewer.spread_base = 4;

        viewer.clear_saved_sort();

        assert!(!viewer.sort_save_toggle_on());
        let current = viewer.current_sort_snapshot();
        assert!(matches!(current.0, ViewerSortKey::Date));
        assert!(!current.1);
        assert_eq!(viewer.entries[0].display_name, "b");
        assert_eq!(viewer.entries[1].display_name, "a");
        assert_eq!(viewer.spread_base, 4);
    }

    #[test]
    fn clearing_already_default_sort_does_not_reset_page_position() {
        let mut viewer = viewer();
        viewer.saved_sort = Some((ViewerSortKey::Name, true));
        viewer.spread_base = 4;

        viewer.clear_saved_sort();

        assert!(!viewer.sort_save_toggle_on());
        assert_eq!(viewer.spread_base, 4);
    }

    #[test]
    fn sort_setting_text_uses_unbracketed_localized_labels() {
        assert_eq!(
            ViewerState::sort_setting_text(ViewerSortKey::Name, true, i18n::Lang::Japanese),
            "名前 昇順"
        );
        assert_eq!(
            ViewerState::sort_setting_text(ViewerSortKey::Natural, false, i18n::Lang::English),
            "Natural Desc"
        );
        assert_eq!(
            ViewerState::sort_setting_text(ViewerSortKey::Date, true, i18n::Lang::Chinese),
            "日期 升序"
        );
    }

    #[test]
    fn saved_sort_is_applied_before_saved_spread_without_losing_offset() {
        let mut viewer = viewer();
        viewer.is_raw_file = false;
        viewer.entries = vec![
            ViewerEntry {
                entry_name: "old".to_string(),
                display_name: "old".to_string(),
                date_key: 1,
                original_index: 0,
            },
            ViewerEntry {
                entry_name: "new".to_string(),
                display_name: "new".to_string(),
                date_key: 2,
                original_index: 1,
            },
        ];

        viewer.restore_saved_sort(ViewerSortKey::Date, false);
        let mut cfg = ViewerConfig::default();
        viewer.restore_saved_spread(PageMode::SpreadLeft, 1, &mut cfg);

        assert_eq!(viewer.entries[0].display_name, "new");
        assert!(matches!(viewer.current_sort_snapshot().0, ViewerSortKey::Date));
        assert!(!viewer.current_sort_snapshot().1);
        assert!(matches!(viewer.page_mode, PageMode::SpreadLeft));
        assert_eq!(viewer.offset.value(), -1);
        assert_eq!(viewer.spread_lo(), -1);
    }

    #[test]
    fn restoring_existing_default_sort_does_not_reset_page_position() {
        let mut viewer = viewer();
        viewer.is_raw_file = false;
        viewer.spread_base = 4;

        viewer.restore_saved_sort(ViewerSortKey::Name, true);

        assert_eq!(viewer.spread_base, 4);
    }
}

#[cfg(test)]
mod bookmark_restore_tests {
    use super::*;

    fn viewer() -> ViewerState {
        ViewerState::new_raw(PathBuf::from("test.png"), [None; 4], None)
    }

    fn archive_viewer() -> ViewerState {
        let mut viewer = viewer();
        viewer.is_raw_file = false;
        viewer.entries = vec![
            ViewerEntry {
                entry_name: "first".to_string(),
                display_name: "first".to_string(),
                date_key: 0,
                original_index: 0,
            },
            ViewerEntry {
                entry_name: "second".to_string(),
                display_name: "second".to_string(),
                date_key: 1,
                original_index: 1,
            },
        ];
        viewer
    }

    #[test]
    fn restore_bookmark_position_jumps_to_matching_entry() {
        let mut viewer = archive_viewer();
        viewer.spread_base = 0;

        assert!(viewer.restore_bookmark_position("second"));

        assert_eq!(viewer.spread_base, 1);
        assert_eq!(viewer.offset.value(), 0);
    }

    #[test]
    fn restore_bookmark_position_fails_without_changing_state_when_entry_missing() {
        let mut viewer = archive_viewer();
        viewer.spread_base = 0;

        assert!(!viewer.restore_bookmark_position("missing"));

        assert_eq!(viewer.spread_base, 0, "見つからない場合は現在位置を変更しない");
    }

    #[test]
    fn current_bookmark_entry_name_reflects_spread_lo() {
        let mut viewer = archive_viewer();
        viewer.spread_base = 1;

        assert_eq!(viewer.current_bookmark_entry_name(), Some("second"));
    }

    #[test]
    fn current_bookmark_entry_name_is_none_for_virtual_leading_page() {
        let mut viewer = archive_viewer();
        viewer.spread_base = -1;

        assert_eq!(viewer.current_bookmark_entry_name(), None);
    }

    /// 回帰テスト：restore_saved_spread は spread_base を問答無用で0にリセットするため、
    /// しおり復帰は必ずその「後」に呼ぶ実装契約になっている（open_viewer側の呼び出し順）。
    /// 順序が入れ替わって再発しないよう、ここでその契約を固定する。
    #[test]
    fn restore_bookmark_position_after_spread_restore_wins() {
        let mut viewer = archive_viewer();
        let mut cfg = ViewerConfig::default();

        viewer.restore_saved_spread(PageMode::SpreadLeft, 0, &mut cfg);
        assert_eq!(viewer.spread_base, 0, "見開き復元は先頭へリセットする");

        assert!(viewer.restore_bookmark_position("second"));

        assert_eq!(viewer.spread_base, 1, "しおり復帰が見開き復元を上書きして残る");
    }
}

#[cfg(test)]
mod decode_edge_tests {
    use super::clamp_decode_edge;

    #[test]
    fn edge_within_limit_is_unchanged() {
        assert_eq!(clamp_decode_edge(4000, 8192), 4000);
        assert_eq!(clamp_decode_edge(8192, 8192), 8192);
    }

    #[test]
    fn edge_over_limit_is_clamped() {
        // 見開きの2倍・GUI設定の最大値の2倍でも、テクスチャ1辺の上限を超えない。
        assert_eq!(clamp_decode_edge(8000 * 2, 8192), 8192);
        assert_eq!(clamp_decode_edge(7680u32.saturating_mul(2), 8192), 8192);
        assert_eq!(clamp_decode_edge(u32::MAX, 8192), 8192);
    }

    #[test]
    fn edge_is_never_zero() {
        assert_eq!(clamp_decode_edge(0, 8192), 1);
        assert_eq!(clamp_decode_edge(100, 0), 1);
    }
}
