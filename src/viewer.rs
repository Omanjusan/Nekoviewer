use crate::i18n;
use crate::log_key;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::cache::{PageCache, PageContent};
use crate::config::WindowSlot;
use crate::fs::archive;
use crate::spread_offset::SpreadOffset;

const SCROLL_THRESHOLD: f32 = 50.0;
const ANIM_SECS: f32 = 0.4;
const FULL_UV: egui::Rect =
    egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(1.0, 1.0) };

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// viewer.show() の戻り値。ファイル間ナビゲーションの要求を app 側に伝える。
#[derive(Clone, Copy, PartialEq)]
pub enum ViewerNav {
    None,
    PrevFile,
    NextFile,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PageMode {
    /// 1: 単独ページ
    Single,
    /// 2: 見開き左綴じ（左→右）
    SpreadLeft,
    /// 3: 見開き右綴じ（右→左）
    SpreadRight,
}

#[derive(Clone, Copy, PartialEq)]
enum ViewerSortKey {
    Name,
    Natural,
    Date,
}

/// GIF等アニメーション再生状態（ページごとに保持）
struct AnimState {
    frame_index: usize,
    last_frame_at: Instant,
}

/// ソート済みエントリ。original_index はキャッシュキーに使い、ソートで変化しない。
#[derive(Clone)]
pub struct ViewerEntry {
    pub entry_name: String,    // ZIP 内部パス（ロード用）
    pub display_name: String,  // 表示用ファイル名
    pub date_key: u64,         // 日付ソートキー
    pub original_index: usize, // list_images 返却時の元インデックス（キャッシュキー）
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

pub struct ViewerState {
    pub archive_path: PathBuf,
    pub entries: Vec<ViewerEntry>,
    /// 見開き基点ページ（常に偶数。単ページ時はそのままページ番号）
    spread_base: i32,
    /// オフセット状態。spread_lo() = spread_base + offset.value()
    offset: SpreadOffset,
    pub textures: HashMap<usize, egui::TextureHandle>,
    pub open: bool,
    pub fullscreen: bool,
    pub page_mode: PageMode,
    scroll_acc: f32,
    pub zoom_actual: bool,
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
    pub slots: [Option<WindowSlot>; 4],
    /// スロット保存後に app 側へ永続化を要求するフラグ
    pub save_requested: bool,
    /// 前フレームの outer_rect 左上座標（保存用、1フレーム遅れ許容）
    outer_pos: Option<egui::Pos2>,
    /// 初回フレームかどうか（with_inner_size を一度だけ渡すため）
    pub first_frame: bool,
    /// 左エントリリストパネルの表示状態（マウスホバーで on/off）
    entry_list_visible: bool,
    /// フルスクリーン時ソートバーの表示状態（上端ホバーで on/off）
    fs_sort_bar_visible: bool,
    sort_key: ViewerSortKey,
    sort_ascending: bool,
    /// アニメーションページの再生状態（original_index → AnimState）
    anim_states: HashMap<usize, AnimState>,
    /// true のとき生画像ファイルを直接表示中（見開きモード封印）
    pub is_raw_file: bool,
    /// Shift+スクロールの蓄積値（ファイル間ナビゲーション用）
    shift_scroll_acc: f32,
    /// トーストメッセージ: (テキスト, 消去予定のegui時刻) None=非表示
    toast: Option<(String, Option<f64>)>,
}

impl ViewerState {
    pub fn new(archive_path: PathBuf, slots: [Option<WindowSlot>; 4]) -> Option<Self> {
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
            fullscreen: false,
            page_mode: PageMode::Single,
            scroll_acc: 0.0,
            zoom_actual: false,
            prev_spread_lo: 0,
            anim_from_lo: 0,
            anim_dir: 1,
            anim_progress: 1.0,
            anim_active: false,
            slots,
            save_requested: false,
            outer_pos: None,
            first_frame: true,
            entry_list_visible: false,
            fs_sort_bar_visible: false,
            sort_key: ViewerSortKey::Name,
            sort_ascending: true,
            anim_states: HashMap::new(),
            is_raw_file: false,
            shift_scroll_acc: 0.0,
            toast: None,
        })
    }

    /// 生画像ファイル（ZIP非対応・1ファイル固定）用コンストラクタ
    pub fn new_raw(path: PathBuf, slots: [Option<WindowSlot>; 4]) -> Self {
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
            fullscreen: false,
            page_mode: PageMode::Single,
            scroll_acc: 0.0,
            zoom_actual: false,
            prev_spread_lo: 0,
            anim_from_lo: 0,
            anim_dir: 1,
            anim_progress: 1.0,
            anim_active: false,
            slots,
            save_requested: false,
            outer_pos: None,
            first_frame: true,
            entry_list_visible: false,
            fs_sort_bar_visible: false,
            sort_key: ViewerSortKey::Name,
            sort_ascending: true,
            anim_states: HashMap::new(),
            is_raw_file: true,
            shift_scroll_acc: 0.0,
            toast: None,
        }
    }

    /// 最終ページから開くコンストラクタ（前ファイルへの移動用）
    pub fn new_at_last_page(archive_path: PathBuf, slots: [Option<WindowSlot>; 4]) -> Option<Self> {
        let mut s = Self::new(archive_path, slots)?;
        s.spread_base = (s.entries.len() as i32 - 1).max(0);
        Some(s)
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
    pub fn set_page_mode(&mut self, mode: PageMode) {
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
                    self.zoom_actual = false;
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

    pub fn show(&mut self, ctx: &egui::Context, page_cache: &PageCache) -> ViewerNav {
        if !self.open || self.entries.is_empty() {
            return ViewerNav::None;
        }

        // ── spread_lo の変化を検出してアニメーション起動 ──────────────────────
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

        // アニメーション進捗を dt で更新
        if self.anim_active {
            let dt = ctx.input(|i| i.unstable_dt);
            self.anim_progress = (self.anim_progress + dt / ANIM_SECS).min(1.0);
            if self.anim_progress >= 1.0 { self.anim_active = false; }
            ctx.request_repaint();
        }

        let t = ease_out(self.anim_progress);
        let animating = self.anim_active;

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
                Some(PageContent::Animated(anim)) => {
                    let state = self.anim_states.entry(orig_i).or_insert_with(|| AnimState {
                        frame_index: 0,
                        last_frame_at: now,
                    });
                    let elapsed = now.duration_since(state.last_frame_at);
                    let current_delay = anim.frames[state.frame_index].delay;

                    let needs_upload = if elapsed >= current_delay {
                        // フレームを進める（ループ回数は将来対応、現状は無限ループ）
                        state.frame_index = (state.frame_index + 1) % anim.frames.len();
                        state.last_frame_at = now;
                        true
                    } else {
                        !self.textures.contains_key(&orig_i)
                    };

                    if needs_upload {
                        let img = &anim.frames[state.frame_index].image;
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

                    // 次フレームまでの残余時間を収集（最小値で repaint 予約）
                    let elapsed_after_upload = now.duration_since(state.last_frame_at);
                    let next_delay = anim.frames[state.frame_index].delay;
                    let remaining = next_delay.saturating_sub(elapsed_after_upload);
                    min_repaint_after = min_repaint_after.min(remaining);
                }
                None => {}
            }
        }

        let window_orig: HashSet<usize> = (start..end)
            .map(|i| self.entries[i].original_index)
            .collect();
        self.textures.retain(|orig_i, _| window_orig.contains(orig_i));
        self.anim_states.retain(|orig_i, _| window_orig.contains(orig_i));

        // アニメーションページが存在する場合は次フレームまでの時間で再描画を予約
        if min_repaint_after < Duration::MAX {
            ctx.request_repaint_after(min_repaint_after);
        }

        let (tex_lo, tex_hi) = self.page_textures_for(self.spread_lo());
        let (prev_tex_lo, prev_tex_hi) = if animating {
            self.page_textures_for(self.anim_from_lo)
        } else {
            (None, None)
        };

        // outer_rect の左上座標を毎フレーム記録（保存ボタン用、1フレーム遅れ許容）
        if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
            self.outer_pos = Some(outer.min);
        }

        // ── 入力読み取り ────────────────────────────────────────────────────────
        let zoom_actual = self.zoom_actual;
        let (key_left, key_right, key_up, key_down, key_space, esc, zoom_key, fs_key,
             mode1, mode2, mode3, shift4, shift5,
             shift_nav_up, shift_nav_down, scroll_delta_raw, shift_scroll_delta) =
            ctx.input(|i| {
                let sh = i.modifiers.shift;
                let raw = if zoom_actual { 0.0 } else {
                    i.raw_scroll_delta.y + if sh { i.raw_scroll_delta.x } else { 0.0 }
                };
                (
                    i.key_pressed(egui::Key::ArrowLeft)  && !sh,
                    i.key_pressed(egui::Key::ArrowRight) && !sh,
                    i.key_pressed(egui::Key::ArrowUp)    && !sh,
                    i.key_pressed(egui::Key::ArrowDown)  && !sh,
                    i.key_pressed(egui::Key::Space),
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::Enter) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::Enter) &&  i.modifiers.alt,
                    i.key_pressed(egui::Key::Num1),
                    i.key_pressed(egui::Key::Num2),
                    i.key_pressed(egui::Key::Num3),
                    i.key_pressed(egui::Key::Num4),
                    i.key_pressed(egui::Key::Num5),
                    i.key_pressed(egui::Key::ArrowUp)   && sh,
                    i.key_pressed(egui::Key::ArrowDown) && sh,
                    raw,
                    if sh { raw } else { 0.0 },
                )
            });

        // F5〜F8: スロット適用
        let slot_apply: Option<usize> = ctx.input(|i| {
            if      i.key_pressed(egui::Key::F5) { Some(0) }
            else if i.key_pressed(egui::Key::F6) { Some(1) }
            else if i.key_pressed(egui::Key::F7) { Some(2) }
            else if i.key_pressed(egui::Key::F8) { Some(3) }
            else { None }
        });

        // 左右キーはファイル間移動に使用。上下/スペースキーでページ送り戻り
        // shift_nav_up/down は Shift 押下中でも同方向のページ送り戻りを継続させる
        let key_next = key_space || key_down || shift_nav_down;
        let key_prev = key_up || shift_nav_up;

        // 4/5 のシフト方向も綴じ方向に合わせる（4=←方向, 5=→方向）
        let (shift_dec, shift_inc) = match self.page_mode {
            PageMode::SpreadRight => (shift5, shift4),
            _                     => (shift4, shift5),
        };

        let scroll_delta = scroll_delta_raw;

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
        if mode1 { self.set_page_mode(PageMode::Single); }
        // 生ファイル表示中は見開きキーを無効化（set_page_mode 内でも封印済みだが念のため）
        if !self.is_raw_file {
            if mode2 { self.set_page_mode(PageMode::SpreadLeft); }
            if mode3 { self.set_page_mode(PageMode::SpreadRight); }
        }

        let is_spread = self.page_mode != PageMode::Single;
        let step = if is_spread { 2i32 } else { 1i32 };

        // タイトルを独立ウィンドウのタイトルバーに反映
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));

        // ── スロット適用（F5〜F8）────────────────────────────────────────────
        if let Some(idx) = slot_apply {
            if let Some(slot) = self.slots[idx] {
                let monitor = ctx.input(|i| i.viewport().monitor_size);
                let (cx, cy) = if let Some(m) = monitor {
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
        if !self.fullscreen {
            egui::TopBottomPanel::top("slot_bar")
                .frame(egui::Frame::side_top_panel(ctx.style().as_ref()))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // ── 左: ZIPエントリのソートボタン ──
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
                        if sort_changed {
                            self.sort_entries();
                        }

                        // ── 右: ウィンドウ位置スロットボタン ──
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for i in (0..4usize).rev() {
                                let label = i18n::t().slot_label(i + 5);
                                let has_slot = self.slots[i].is_some();
                                if ui.selectable_label(has_slot, &label).clicked() {
                                    let inner = ctx.input(|inp| inp.viewport().inner_rect);
                                    if let (Some(pos), Some(inner)) = (self.outer_pos, inner) {
                                        self.slots[i] = Some(WindowSlot {
                                            x: pos.x as i32,
                                            y: pos.y as i32,
                                            w: inner.width() as u32,
                                            h: inner.height() as u32,
                                        });
                                        self.save_requested = true;
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
        if self.fullscreen {
            const FS_TRIGGER_H: f32 = 40.0;
            const FS_BAR_H: f32 = 32.0;
            const FS_HIDE_MARGIN: f32 = 10.0;

            let screen_top = ctx.screen_rect().min.y;
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                if !self.fs_sort_bar_visible && pos.y < screen_top + FS_TRIGGER_H {
                    self.fs_sort_bar_visible = true;
                    ctx.request_repaint();
                } else if self.fs_sort_bar_visible && pos.y > screen_top + FS_BAR_H + FS_HIDE_MARGIN {
                    self.fs_sort_bar_visible = false;
                    ctx.request_repaint();
                }
            }

            if self.fs_sort_bar_visible {
                egui::TopBottomPanel::top("fs_sort_bar")
                    .frame(egui::Frame::side_top_panel(ctx.style().as_ref()))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
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
                            if sort_changed {
                                self.sort_entries();
                            }
                        });
                    });
            }
        }

        // ── 左エントリリストのホバー制御 ──────────────────────────────────────
        const ENTRY_PANEL_W: f32 = 180.0;
        const TRIGGER_W: f32 = 40.0;
        const HIDE_MARGIN: f32 = 20.0;

        let screen_left = ctx.screen_rect().min.x;
        if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
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

            egui::SidePanel::left("entry_list_panel")
                .exact_width(ENTRY_PANEL_W)
                .frame(egui::Frame::side_top_panel(ctx.style().as_ref()))
                .show(ctx, |ui| {
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

        let mut double_clicked = false;
        let mut middle_clicked = false;

        let anim_dir_f = self.anim_dir as f32;
        let page_mode = self.page_mode;
        let zoom_actual = self.zoom_actual;

        egui::CentralPanel::default().show(ctx, |ui| {
            let clip   = ui.clip_rect();
            let avail  = ui.available_size();
            let origin = ui.cursor().left_top();

            if !animating || zoom_actual {
                // ── 通常レンダリング ──────────────────────────────────────────
                match page_mode {
                    PageMode::Single => {
                        self.render_single(ui, &tex_lo, &mut double_clicked, &mut middle_clicked);
                    }
                    PageMode::SpreadLeft => {
                        Self::render_spread(ui, &tex_lo, &tex_hi, &mut middle_clicked);
                    }
                    PageMode::SpreadRight => {
                        Self::render_spread(ui, &tex_hi, &tex_lo, &mut middle_clicked);
                    }
                }
            } else {
                // ── スライドアニメーション ────────────────────────────────────
                let full_rect = egui::Rect::from_min_size(origin, avail);
                let resp = ui.allocate_rect(full_rect, egui::Sense::click());
                if ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) { middle_clicked = true; }
                if resp.double_clicked() { double_clicked = true; }

                let painter = ui.painter().with_clip_rect(clip);
                let off_old = avail.x * t * (-anim_dir_f);
                let off_new = avail.x * (1.0 - t) * anim_dir_f;
                let monitor = ui.ctx().input(|i| i.viewport().monitor_size);

                match page_mode {
                    PageMode::Single => {
                        Self::paint_single_at(&painter, &prev_tex_lo, avail, origin, off_old);
                        Self::paint_single_at(&painter, &tex_lo,      avail, origin, off_new);
                    }
                    PageMode::SpreadLeft => {
                        let (rl, rr) = Self::spread_rects(avail, origin, &prev_tex_lo, &prev_tex_hi, monitor);
                        Self::paint_page(&painter, &prev_tex_lo, rl.translate(egui::vec2(off_old, 0.0)));
                        Self::paint_page(&painter, &prev_tex_hi, rr.translate(egui::vec2(off_old, 0.0)));
                        let (rl, rr) = Self::spread_rects(avail, origin, &tex_lo, &tex_hi, monitor);
                        Self::paint_page(&painter, &tex_lo, rl.translate(egui::vec2(off_new, 0.0)));
                        Self::paint_page(&painter, &tex_hi, rr.translate(egui::vec2(off_new, 0.0)));
                    }
                    PageMode::SpreadRight => {
                        let (rl, rr) = Self::spread_rects(avail, origin, &prev_tex_hi, &prev_tex_lo, monitor);
                        Self::paint_page(&painter, &prev_tex_hi, rl.translate(egui::vec2(off_old, 0.0)));
                        Self::paint_page(&painter, &prev_tex_lo, rr.translate(egui::vec2(off_old, 0.0)));
                        let (rl, rr) = Self::spread_rects(avail, origin, &tex_hi, &tex_lo, monitor);
                        Self::paint_page(&painter, &tex_hi, rl.translate(egui::vec2(off_new, 0.0)));
                        Self::paint_page(&painter, &tex_lo, rr.translate(egui::vec2(off_new, 0.0)));
                    }
                }
            }

            // ── 右下ページ数オーバーレイ ──────────────────────────────────────
            let page_text = format!("{}/{}", self.spread_lo().max(0) + 1, self.entries.len());
            let font_id = egui::FontId::proportional(14.0);
            let text_color = egui::Color32::WHITE;
            let shadow_color = egui::Color32::from_black_alpha(180);
            let panel_rect = ui.clip_rect();
            let galley = ui.fonts(|f| f.layout_no_wrap(page_text, font_id, text_color));
            let text_size = galley.size();
            let margin = egui::vec2(8.0, 6.0);
            let text_pos = panel_rect.right_bottom() - text_size - margin;
            let painter = ui.painter();
            painter.text(text_pos + egui::vec2(1.0, 1.0), egui::Align2::LEFT_TOP, &galley.text().to_string(), egui::FontId::proportional(14.0), shadow_color);
            painter.galley(text_pos, galley, text_color);

            // ── トーストオーバーレイ（下部中央）──────────────────────────────
            if let Some((msg, Some(_))) = &self.toast {
                let toast_font = egui::FontId::proportional(16.0);
                let tg = ui.fonts(|f| f.layout_no_wrap(msg.clone(), toast_font, egui::Color32::WHITE));
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

        // ── ファイル間ナビゲーション（Shift+↑↓ or Shift+スクロール）────────────
        self.shift_scroll_acc += shift_scroll_delta;
        let shift_scroll_prev = self.shift_scroll_acc >  SCROLL_THRESHOLD;
        let shift_scroll_next = self.shift_scroll_acc < -SCROLL_THRESHOLD;
        if shift_scroll_prev { self.shift_scroll_acc -= SCROLL_THRESHOLD; }
        if shift_scroll_next { self.shift_scroll_acc += SCROLL_THRESHOLD; }

        let total_i = total as i32;
        let off = self.offset.value();
        let at_first = self.spread_lo() <= if is_spread { -1 } else { 0 };
        let at_last  = self.spread_base + step + off > total_i - 1;

        let mut nav = ViewerNav::None;
        if (shift_nav_up   || shift_scroll_prev) && at_first { nav = ViewerNav::PrevFile; }
        if (shift_nav_down || shift_scroll_next) && at_last  { nav = ViewerNav::NextFile; }
        if key_left  { nav = ViewerNav::PrevFile; }
        if key_right { nav = ViewerNav::NextFile; }

        // ── ページ送り ───────────────────────────────────────────────────────
        self.scroll_acc += scroll_delta;
        let scroll_next = self.scroll_acc < -SCROLL_THRESHOLD;
        let scroll_prev = self.scroll_acc > SCROLL_THRESHOLD;
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
            // オフセットシフト後に再評価（シフトで末尾仮想に入った場合に対応）
            self.offset.update_virtual_right(self.spread_lo() + 1 >= total_i);
        }

        // ── その他の入力 ─────────────────────────────────────────────────────
        if (zoom_key || double_clicked) && !is_spread {
            self.zoom_actual = !self.zoom_actual;
        }

        if fs_key || middle_clicked {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            log_key!("[key] fullscreen → {}", self.fullscreen);
        }

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested || esc {
            if self.fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            self.open = false;
            self.fullscreen = false;
        }

        // ── トースト期限チェック ──────────────────────────────────────────────
        {
            let now = ctx.input(|i| i.time);
            match &mut self.toast {
                Some((_, expires @ None)) => {
                    *expires = Some(now + 3.0);
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Some((_, Some(exp))) if now >= *exp => {
                    self.toast = None;
                }
                Some(_) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                None => {}
            }
        }

        nav
    }

    fn render_single(
        &mut self,
        ui: &mut egui::Ui,
        tex: &Option<egui::TextureHandle>,
        double_clicked: &mut bool,
        middle_clicked: &mut bool,
    ) {
        if let Some(tex) = tex {
            let [img_w, img_h] = tex.size();
            if self.zoom_actual {
                egui::ScrollArea::both().show(ui, |ui| {
                    let size = egui::vec2(img_w as f32, img_h as f32);
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                    ui.painter().image(tex.id(), rect, FULL_UV, egui::Color32::WHITE);
                    if resp.double_clicked() { *double_clicked = true; }
                    if ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) { *middle_clicked = true; }
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
                if ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) { *middle_clicked = true; }
            }
        } else {
            let rect = egui::Rect::from_min_size(ui.cursor().left_top(), ui.available_size());
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::from_gray(40));
            if ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) { *middle_clicked = true; }
        }
    }

    fn render_spread(
        ui: &mut egui::Ui,
        tex_left: &Option<egui::TextureHandle>,
        tex_right: &Option<egui::TextureHandle>,
        middle_clicked: &mut bool,
    ) {
        let available = ui.available_size();
        let origin = ui.cursor().left_top();
        let monitor = ui.ctx().input(|i| i.viewport().monitor_size);

        let full_rect = egui::Rect::from_min_size(origin, available);
        let resp = ui.allocate_rect(full_rect, egui::Sense::click());
        if ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Middle)) { *middle_clicked = true; }

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
        monitor: Option<egui::Vec2>,
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
