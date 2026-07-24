//! 設定ダイアログの egui 描画部分。データの永続化(state ファイル)は gui_config.rs、
//! 起動時設定(config.ini)は config.rs が担当し、ここは NekoviewApp に生えた
//! [設定]ボタン以降のUI（タブ切り替え・各タブの中身・下書き→反映のフロー）のみを扱う。

use crate::config::{AppConfig, ResizeFilter, filter_to_str};
use crate::gui_config::{ThumbbarPos, ViewerConfig};
use crate::i18n;
use crate::keymap::{Keymap, ReaderAction, ExplorerAction, KeyCombo, MouseCombo, MouseAction, mouse_action_name};
use crate::translate::{OVERLAY_WIDTH_CEILING, OVERLAY_WIDTH_FLOOR, TranslateConfig};
use crate::view_explorer::NekoviewApp;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Common,
    Anim,
    Static,
    Viewer,
    Translate,
    Keymap,
    Other,
}

/// 8K UHD(7680x4320)の長辺を「取り扱い上限解像度」スライダーの上限に使う。
const MAX_DECODE_EDGE_CEILING: u32 = 7680;
const MAX_DECODE_EDGE_FLOOR: u32 = 200;
/// キャッシュ合計スライダーの下限MB。
const CACHE_TOTAL_FLOOR_MB: u64 = 64;
/// サムネイルサイズスライダーの下限・上限px（config.rs のパース時clampと合わせる）。
const THUMB_SIZE_FLOOR: u32 = 64;
const THUMB_SIZE_CEILING: u32 = 512;

/// 設定ダイアログの編集用下書き。[反映]を押すまでは AppConfig/ViewerConfig 本体には
/// 一切書き戻さない（自由にタイプ・切り替えさせるための一時バッファ）。
/// 数値項目はすべて Slider で編集する（テキスト入力は範囲外の値を打ち込めてしまい
/// フールプルーフでないため使わない方針）。
pub(crate) struct SettingsDraft {
    redecode_on_resize: bool,
    debounce_ms: u64,
    /// キャッシュ合計（ページ+ファイル）をユーザーが手動指定するか。false = 自動(システムRAMの30%)。
    cache_total_user_set: bool,
    cache_total_mb: u64,
    /// ダイアログを開いた時点のシステム総RAM(MB)。毎フレームのsysinfo呼び出しを避けるためキャッシュする。
    system_ram_mb: u64,
    max_decode_edge: u32,
    thumb_size: u32,
    viewer_filter: ResizeFilter,
    thumb_filter: ResizeFilter,
    lang: i18n::Lang,
    show_hidden: bool,
    ring_min: usize,
    ring_max: usize,
    thumbbar_pos: ThumbbarPos,
    thumbbar_thumb_size: u32,
    thumbbar_idle_hide_ms: u64,
    thumbbar_overlap: bool,
    thumbbar_marker_r: u8,
    thumbbar_marker_g: u8,
    thumbbar_marker_b: u8,
    thumbbar_marker_a: u8,
    exif_orientation_enabled: bool,
    translate_base_url: String,
    translate_ocr_model: String,
    translate_translation_model: String,
    /// OCRモデルをユーザーが自分のドロップダウンから明示的に選び直したか。falseの間は
    /// 翻訳モデルの変更にOCRモデルが追従し続ける（翻訳モデルが主、OCRは従の関係）。
    translate_ocr_model_overridden: bool,
    /// 直近の「モデル取得」で得たモデル一覧（未取得なら空、ダイアログ内のみの一時状態）。
    translate_available_models: Vec<String>,
    translate_overlay_width: u32,
    keymap: Keymap,
    /// キーボードの「変更」ボタンで開く専用ダイアログの状態。Some の間だけ表示する。
    key_capture_dialog: Option<KeyCaptureDialogState>,
    /// マウスの「変更」ボタンで開く専用ダイアログの状態。Some の間だけ表示する。
    /// キーボード版と同じModal方式に統一済み（マウスはReaderActionのみ対象）。
    mouse_capture_dialog: Option<MouseCaptureDialogState>,
    /// 直近の登録が他アクションと重複していた場合の警告文。登録自体はブロックしない
    /// （入れ替えを行うには一時的な重複を経由する必要があるため）。次の登録操作まで表示し続ける。
    keymap_last_warning: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeymapCaptureTarget {
    Reader(ReaderAction),
    Explorer(ExplorerAction),
}

/// キーボード用キャプチャダイアログの編集状態。SHIFT/CTRL/ALTはトグルで独立管理し、
/// 「設定開始」中に拾うキーイベントは本体キー1つだけに使う。これにより、コンボキー
/// （例: SHIFT+A）を狙った際に修飾キー単体を本体キーとして誤登録する問題を構造的に防ぐ。
struct KeyCaptureDialogState {
    target: KeymapCaptureTarget,
    shift: bool,
    ctrl: bool,
    alt: bool,
    key: Option<egui::Key>,
    /// 「設定開始」押下後、次のキー入力を待っている間 true。
    listening: bool,
    /// [設定]で確定しようとして衝突拒否された場合の案内文。
    error: Option<String>,
}

impl KeyCaptureDialogState {
    fn new(target: KeymapCaptureTarget, current: Option<KeyCombo>) -> Self {
        Self {
            target,
            shift: current.is_some_and(|k| k.shift),
            ctrl: current.is_some_and(|k| k.ctrl),
            alt: current.is_some_and(|k| k.alt),
            key: current.map(|k| k.key),
            listening: false,
            error: None,
        }
    }
}

/// マウス用キャプチャダイアログの編集状態。キーボード版(KeyCaptureDialogState)と同じ構造で、
/// 「本体キー」を「マウスボタン」に置き換えただけ。マウスはReaderActionのみが対象。
struct MouseCaptureDialogState {
    action: ReaderAction,
    shift: bool,
    ctrl: bool,
    alt: bool,
    mouse_action: Option<MouseAction>,
    /// 「設定開始」押下後、次のマウス入力を待っている間 true。
    listening: bool,
    /// [設定]で確定しようとしてマウスボタン未設定だった場合の案内文。
    error: Option<String>,
}

impl MouseCaptureDialogState {
    fn new(action: ReaderAction, current: Option<MouseCombo>) -> Self {
        Self {
            action,
            shift: current.is_some_and(|m| m.shift),
            ctrl: current.is_some_and(|m| m.ctrl),
            alt: current.is_some_and(|m| m.alt),
            mouse_action: current.map(|m| m.action),
            listening: false,
            error: None,
        }
    }
}

impl KeymapCaptureTarget {
    fn display_name(self) -> &'static str {
        match self {
            Self::Reader(a) => a.display_name(),
            Self::Explorer(a) => a.display_name(),
        }
    }
}

impl SettingsDraft {
    pub(crate) fn from_current(config: &AppConfig, viewer_cfg: &ViewerConfig, show_hidden: bool, translate_cfg: &TranslateConfig) -> Self {
        let system_ram_mb = crate::cache::system_total_ram_mb();
        Self {
            redecode_on_resize: viewer_cfg.redecode_on_resize,
            debounce_ms: viewer_cfg.resize_debounce_ms,
            cache_total_user_set: config.cache_total_mb.is_some(),
            cache_total_mb: config.cache_total_mb.unwrap_or_else(crate::cache::default_cache_total_mb),
            system_ram_mb,
            max_decode_edge: config.max_decode_edge,
            thumb_size: config.thumb_size,
            viewer_filter: config.viewer_filter,
            thumb_filter: config.thumb_filter,
            lang: i18n::t(),
            show_hidden,
            ring_min: config.anim_ring_min_frames,
            ring_max: config.anim_ring_max_frames,
            thumbbar_pos: viewer_cfg.thumbbar_pos,
            thumbbar_thumb_size: viewer_cfg.thumbbar_thumb_size,
            thumbbar_idle_hide_ms: viewer_cfg.thumbbar_idle_hide_ms,
            thumbbar_overlap: viewer_cfg.thumbbar_overlap,
            thumbbar_marker_r: viewer_cfg.thumbbar_marker_r,
            thumbbar_marker_g: viewer_cfg.thumbbar_marker_g,
            thumbbar_marker_b: viewer_cfg.thumbbar_marker_b,
            thumbbar_marker_a: viewer_cfg.thumbbar_marker_a,
            exif_orientation_enabled: viewer_cfg.exif_orientation_enabled,
            translate_base_url: translate_cfg.base_url.clone(),
            translate_ocr_model: translate_cfg.ocr_model.clone(),
            translate_translation_model: translate_cfg.translation_model.clone(),
            translate_ocr_model_overridden: translate_cfg.ocr_model != translate_cfg.translation_model,
            translate_available_models: Vec::new(),
            translate_overlay_width: translate_cfg.overlay_width,
            keymap: config.keymap.clone(),
            key_capture_dialog: None,
            mouse_capture_dialog: None,
            keymap_last_warning: None,
        }
    }

    /// キャッシュ合計がシステムRAMの50%を超えているか（超過時は[反映]を拒否する）。
    fn cache_over_budget(&self) -> bool {
        self.cache_total_user_set && self.system_ram_mb > 0 && self.cache_total_mb > self.system_ram_mb / 2
    }

    /// [反映]クリック時に実際の設定へ書き戻す。呼び出し側は事前に `cache_over_budget()` を
    /// 確認し、超過時はそもそも呼ばない（保存を拒否する）こと。
    fn apply_to(&self, config: &mut AppConfig, viewer_cfg: &mut ViewerConfig, translate_cfg: &mut TranslateConfig) {
        viewer_cfg.redecode_on_resize = self.redecode_on_resize;
        viewer_cfg.resize_debounce_ms = self.debounce_ms;
        config.cache_total_mb = if self.cache_total_user_set { Some(self.cache_total_mb) } else { None };
        config.max_decode_edge = self.max_decode_edge;
        config.thumb_size = self.thumb_size;
        config.viewer_filter = self.viewer_filter;
        config.thumb_filter = self.thumb_filter;
        i18n::set(self.lang);

        config.anim_ring_min_frames = self.ring_min;
        config.anim_ring_max_frames = self.ring_max;

        viewer_cfg.thumbbar_pos = self.thumbbar_pos;
        viewer_cfg.thumbbar_thumb_size = self.thumbbar_thumb_size;
        viewer_cfg.thumbbar_idle_hide_ms = self.thumbbar_idle_hide_ms;
        viewer_cfg.thumbbar_overlap = self.thumbbar_overlap;
        viewer_cfg.thumbbar_marker_r = self.thumbbar_marker_r;
        viewer_cfg.thumbbar_marker_g = self.thumbbar_marker_g;
        viewer_cfg.thumbbar_marker_b = self.thumbbar_marker_b;
        viewer_cfg.thumbbar_marker_a = self.thumbbar_marker_a;
        viewer_cfg.exif_orientation_enabled = self.exif_orientation_enabled;

        translate_cfg.base_url = self.translate_base_url.trim().to_string();
        translate_cfg.ocr_model = self.translate_ocr_model.trim().to_string();
        translate_cfg.translation_model = self.translate_translation_model.trim().to_string();
        translate_cfg.overlay_width = self.translate_overlay_width;
        config.keymap = self.keymap.clone();
    }
}

/// リサイズフィルタ選択コンボボックス（共通タブで静止画・アニメ共通の1個を編集する）。
fn draw_resize_filter_combo(ui: &mut egui::Ui, id: &str, value: &mut ResizeFilter) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(filter_to_str(*value))
        .show_ui(ui, |ui| {
            for f in [ResizeFilter::Nearest, ResizeFilter::Triangle, ResizeFilter::CatmullRom, ResizeFilter::Lanczos3] {
                ui.selectable_value(value, f, filter_to_str(f));
            }
        });
}

fn draw_settings_tab_common(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    ui.label(i18n::t().settings_base_resolution_label());
    ui.horizontal(|ui| {
        ui.radio_value(&mut draft.redecode_on_resize, false, i18n::t().settings_base_resolution_actual());
        ui.radio_value(&mut draft.redecode_on_resize, true, i18n::t().settings_base_resolution_follow_window());
    });
    ui.label(i18n::t().settings_base_resolution_explain());

    ui.label(i18n::t().settings_max_decode_label());
    // 少し長めの Slider にする（既定の spacing.slider_width だと数千pxの範囲では細かく合わせにくい）。
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 260.0;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut draft.max_decode_edge, MAX_DECODE_EDGE_FLOOR..=MAX_DECODE_EDGE_CEILING).show_value(false));
            ui.label(format!("{} px", draft.max_decode_edge));
        });
    });
    ui.label(i18n::t().settings_max_decode_explain());

    ui.separator();

    ui.label(i18n::t().settings_debounce_label());
    if ui.button(i18n::t().redecode_debounce_label(draft.debounce_ms)).clicked() {
        draft.debounce_ms = crate::gui_config::next_debounce_ms(draft.debounce_ms);
    }
    ui.label(i18n::t().settings_debounce_explain());
    ui.separator();

    ui.label(i18n::t().settings_cache_system_ram(draft.system_ram_mb));
    ui.checkbox(&mut draft.cache_total_user_set, i18n::t().settings_cache_manual_toggle());
    ui.label(i18n::t().settings_cache_manual_explain());
    ui.add_enabled_ui(draft.cache_total_user_set, |ui| {
        // 50%を超えると保存拒否になるため、Sliderの上限もシステムRAMの50%に合わせる。
        let ceiling = (draft.system_ram_mb / 2).max(CACHE_TOTAL_FLOOR_MB);
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = 260.0;
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut draft.cache_total_mb, CACHE_TOTAL_FLOOR_MB..=ceiling)
                    .show_value(false)
                    .step_by(200.0));
                ui.label(format!("{} MB", draft.cache_total_mb));
            });
        });
    });
    if draft.cache_over_budget() {
        ui.colored_label(egui::Color32::from_rgb(220, 60, 60), i18n::t().settings_cache_over_budget());
    }
    ui.separator();

    ui.label(i18n::t().settings_thumb_size_label());
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 260.0;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut draft.thumb_size, THUMB_SIZE_FLOOR..=THUMB_SIZE_CEILING).show_value(false));
            ui.label(format!("{} px", draft.thumb_size));
        });
    });
    ui.label(i18n::t().settings_thumb_size_explain());
    ui.separator();

    ui.label(i18n::t().settings_resize_filter_viewer_label());
    draw_resize_filter_combo(ui, "common_viewer_filter", &mut draft.viewer_filter);
    ui.add_space(4.0);
    ui.label(i18n::t().settings_resize_filter_thumb_label());
    draw_resize_filter_combo(ui, "common_thumb_filter", &mut draft.thumb_filter);
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(i18n::t().settings_show_hidden_label());
        ui.checkbox(&mut draft.show_hidden, "");
    });
    ui.separator();

    ui.label(i18n::t().settings_lang_label());
    egui::ComboBox::from_id_salt("settings_lang")
        .selected_text(draft.lang.native_name())
        .show_ui(ui, |ui| {
            for lang in [i18n::Lang::Japanese, i18n::Lang::English, i18n::Lang::Chinese] {
                ui.selectable_value(&mut draft.lang, lang, lang.native_name());
            }
        });
}

fn draw_settings_tab_anim(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    ui.label(i18n::t().settings_ring_bounds_label());
    // Slider は値域(1..=60)外を選べないため、テキスト入力よりフールプルーフ。
    // 下限スライダーの上端を現在の上限に、上限スライダーの下端を現在の下限に連動させ、
    // min<=max もUI操作だけで常に保証する。show_value(false)でSlider内蔵の編集可能な
    // 数値ボックスを消し、代わりに読み取り専用のラベルで現在値だけ表示する。
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut draft.ring_min, 1..=draft.ring_max).show_value(false).text(i18n::t().settings_ring_min_label()));
        ui.label(draft.ring_min.to_string());
    });
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut draft.ring_max, draft.ring_min..=60).show_value(false).text(i18n::t().settings_ring_max_label()));
        ui.label(draft.ring_max.to_string());
    });
    ui.label(i18n::t().settings_ring_bounds_explain());
}

fn draw_settings_tab_viewer(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    ui.horizontal(|ui| {
        ui.label(i18n::t().settings_exif_orientation_label());
        ui.checkbox(&mut draft.exif_orientation_enabled, "");
    });
    ui.label(i18n::t().settings_exif_orientation_explain());
    ui.separator();

    // 大項目見出し。下の各項目ラベル(■付き)と混同しないよう太字・大きめで区別する。
    ui.label(egui::RichText::new(i18n::t().settings_thumbbar_section_label()).strong().size(15.0));

    ui.label(i18n::t().settings_thumbbar_pos_label());
    egui::ComboBox::from_id_salt("thumbbar_pos")
        .selected_text(match draft.thumbbar_pos {
            ThumbbarPos::Left => i18n::t().settings_thumbbar_pos_left(),
            ThumbbarPos::Right => i18n::t().settings_thumbbar_pos_right(),
            ThumbbarPos::Top => i18n::t().settings_thumbbar_pos_top(),
            ThumbbarPos::Bottom => i18n::t().settings_thumbbar_pos_bottom(),
            ThumbbarPos::None => i18n::t().settings_thumbbar_pos_none(),
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut draft.thumbbar_pos, ThumbbarPos::Left, i18n::t().settings_thumbbar_pos_left());
            ui.selectable_value(&mut draft.thumbbar_pos, ThumbbarPos::Right, i18n::t().settings_thumbbar_pos_right());
            ui.selectable_value(&mut draft.thumbbar_pos, ThumbbarPos::Top, i18n::t().settings_thumbbar_pos_top());
            ui.selectable_value(&mut draft.thumbbar_pos, ThumbbarPos::Bottom, i18n::t().settings_thumbbar_pos_bottom());
            ui.selectable_value(&mut draft.thumbbar_pos, ThumbbarPos::None, i18n::t().settings_thumbbar_pos_none());
        });
    ui.label(i18n::t().settings_thumbbar_pos_explain());
    ui.separator();

    ui.label(i18n::t().settings_thumbbar_size_label());
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 260.0;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut draft.thumbbar_thumb_size, 48..=200).show_value(false));
            ui.label(format!("{} px", draft.thumbbar_thumb_size));
        });
    });
    ui.label(i18n::t().settings_thumbbar_size_explain());
    ui.separator();

    ui.label(i18n::t().settings_thumbbar_idle_label());
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 260.0;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut draft.thumbbar_idle_hide_ms, 0..=10000).show_value(false).step_by(500.0));
            let text = if draft.thumbbar_idle_hide_ms == 0 {
                i18n::t().settings_thumbbar_idle_always().to_string()
            } else {
                format!("{:.1} s", draft.thumbbar_idle_hide_ms as f64 / 1000.0)
            };
            ui.label(text);
        });
    });
    ui.label(i18n::t().settings_thumbbar_idle_explain());
    ui.separator();

    ui.horizontal(|ui| {
        ui.label(i18n::t().settings_thumbbar_overlap_label());
        ui.checkbox(&mut draft.thumbbar_overlap, "");
    });
    ui.label(i18n::t().settings_thumbbar_overlap_explain());
    ui.separator();

    ui.label(i18n::t().settings_thumbbar_marker_label());
    ui.label(i18n::t().settings_thumbbar_marker_explain());

    // スライダーの値をその場で確認できるよう、現在地マーカーのサンプル矩形をリアルタイム描画する。
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(90));
    let alpha = (draft.thumbbar_marker_a as f32 / 100.0 * 255.0).round() as u8;
    painter.rect_filled(rect, 3.0, egui::Color32::from_rgba_unmultiplied(
        draft.thumbbar_marker_r, draft.thumbbar_marker_g, draft.thumbbar_marker_b, alpha,
    ));

    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 220.0;
        ui.horizontal(|ui| {
            ui.label("R");
            ui.add(egui::Slider::new(&mut draft.thumbbar_marker_r, 0..=255));
        });
        ui.horizontal(|ui| {
            ui.label("G");
            ui.add(egui::Slider::new(&mut draft.thumbbar_marker_g, 0..=255));
        });
        ui.horizontal(|ui| {
            ui.label("B");
            ui.add(egui::Slider::new(&mut draft.thumbbar_marker_b, 0..=255));
        });
        ui.horizontal(|ui| {
            ui.label("A");
            ui.add(egui::Slider::new(&mut draft.thumbbar_marker_a, 0..=100));
        });
    });
}

/// キーアサインタブ: ReaderAction/ExplorerActionの現在の割り当てをセクション分けして
/// 一覧表示する。「変更」ボタン押下で次のキー/マウス入力を捕捉して確定する。
///
/// 表示は egui::Grid ではなく手動レイアウトで組む。Gridの自動列幅・自動間隔では
/// 「列間の隙間ゼロ」「行をまたいで機能〜マウスまで一本につながる罫線」「セル内容の
/// 上下中央揃え」「機能名の長さに引きずられない完全固定の列幅」を同時に満たせなかった
/// ため、各セルの矩形を allocate_exact_size で自前に確保し、背景・罫線を painter で直接描く。
const KEYMAP_ROW_H: f32 = 44.0;
const KEYMAP_COL_FEATURE_W: f32 = 150.0;
const KEYMAP_COL_KB_W: f32 = 290.0;
const KEYMAP_COL_MOUSE_W: f32 = 230.0;
const KEYMAP_BUTTON_W: f32 = 46.0;
const KEYMAP_RESET_W: f32 = 22.0;
const KEYMAP_CELL_PAD: f32 = 8.0;

/// SHIFT/CTRL/ALTを1文字バッジで表示し、本体キー/マウス動作と同じ1行に収める
/// （コンボキーでも縦に伸びないよう、詳細は横一列にまとめる）。
fn draw_mod_badge(ui: &mut egui::Ui, label: &str, on: bool) {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let visuals = ui.visuals();
    let (bg, fg, stroke) = if on {
        (visuals.selection.bg_fill, egui::Color32::WHITE, visuals.selection.bg_fill)
    } else {
        (visuals.extreme_bg_color, visuals.weak_text_color(), visuals.widgets.noninteractive.bg_stroke.color)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::new(10.0, egui::FontFamily::Monospace), fg);
}

fn draw_key_combo_line(ui: &mut egui::Ui, kb: Option<KeyCombo>) {
    let (shift, ctrl, alt, key) = match kb {
        Some(k) => (k.shift, k.ctrl, k.alt, format!("{:?}", k.key)),
        None => (false, false, false, "-".to_string()),
    };
    ui.spacing_mut().item_spacing.x = 3.0;
    draw_mod_badge(ui, "S", shift);
    draw_mod_badge(ui, "C", ctrl);
    draw_mod_badge(ui, "A", alt);
    ui.add_space(4.0);
    ui.label(egui::RichText::new(key).monospace());
}

fn draw_mouse_combo_line(ui: &mut egui::Ui, mc: Option<MouseCombo>) {
    let (shift, ctrl, alt, action) = match mc {
        Some(m) => (m.shift, m.ctrl, m.alt, mouse_action_name(m.action).to_string()),
        None => (false, false, false, "-".to_string()),
    };
    ui.spacing_mut().item_spacing.x = 3.0;
    draw_mod_badge(ui, "S", shift);
    draw_mod_badge(ui, "C", ctrl);
    draw_mod_badge(ui, "A", alt);
    ui.add_space(4.0);
    ui.label(egui::RichText::new(action).monospace());
}

/// 「既定値」= 巻き戻し矢印1文字のシンボルボタン。カスタム値があるときだけ押せる。
fn draw_reset_button(ui: &mut egui::Ui, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new("↺").size(13.0)).min_size(egui::vec2(KEYMAP_RESET_W, 18.0)),
    ).on_hover_text("既定値に戻す").clicked()
}

/// 1セル分の矩形を確保し、背景を塗って中身を左詰め・上下中央揃えで描画する。
/// 矩形を返すので、呼び出し元で行をまたぐ罫線をまとめて引ける。
fn keymap_cell(ui: &mut egui::Ui, bg: egui::Color32, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, KEYMAP_ROW_H), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, bg);
    let content_rect = egui::Rect::from_min_max(rect.min + egui::vec2(KEYMAP_CELL_PAD, 0.0), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 4.0;
    add_contents(&mut child);
    rect
}

/// 「機能」列専用セル。長いラベルは折り返す（行高さ内に収まるよう小さめのフォントで）。
fn keymap_feature_cell(ui: &mut egui::Ui, bg: egui::Color32, width: f32, label: &str) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, KEYMAP_ROW_H), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, bg);
    let content_rect = egui::Rect::from_min_max(rect.min + egui::vec2(KEYMAP_CELL_PAD, 0.0), rect.max - egui::vec2(KEYMAP_CELL_PAD, 0.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.add(egui::Label::new(egui::RichText::new(label).size(12.5)).wrap());
    rect
}

/// 行の罫線をまとめて描く: 機能セル左端〜末尾セル右端まで一本の横線、各列境界に縦線。
fn draw_keymap_row_borders(ui: &mut egui::Ui, cells: &[egui::Rect]) {
    let color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let stroke = egui::Stroke::new(1.0, color);
    let Some(first) = cells.first() else { return };
    let Some(last) = cells.last() else { return };
    let bottom = cells.iter().map(|r| r.bottom()).fold(f32::MIN, f32::max);
    ui.painter().line_segment([egui::pos2(first.left(), bottom), egui::pos2(last.right(), bottom)], stroke);
    // 縦線は列の境界だけに引く（最後のセルの右端には引かない）。
    for r in &cells[..cells.len().saturating_sub(1)] {
        ui.painter().line_segment([egui::pos2(r.right(), r.top()), egui::pos2(r.right(), bottom)], stroke);
    }
}

fn draw_settings_tab_keymap(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    if ui.button("すべて既定に戻す").clicked() {
        draft.keymap = Keymap::default();
        draft.keymap_last_warning = None;
    }
    if let Some(warning) = &draft.keymap_last_warning {
        ui.colored_label(egui::Color32::from_rgb(220, 160, 40), warning);
    }
    ui.separator();

    // 列ごとの背景色分けはやめて全列同色にする（境界は縦罫線のみで示す）。
    let cell_bg = ui.visuals().faint_bg_color;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let r1 = keymap_feature_cell(ui, cell_bg, KEYMAP_COL_FEATURE_W, "機能");
        let r2 = keymap_cell(ui, cell_bg, KEYMAP_COL_KB_W, |ui| { ui.strong("キーボード"); });
        let r3 = keymap_cell(ui, cell_bg, KEYMAP_COL_MOUSE_W, |ui| { ui.strong("マウス"); });
        draw_keymap_row_borders(ui, &[r1, r2, r3]);
    });
    for action in ReaderAction::ALL.iter().copied() {
        let binding = draft.keymap.reader_binding(action);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r1 = keymap_feature_cell(ui, cell_bg, KEYMAP_COL_FEATURE_W, action.display_name());
            let r2 = keymap_cell(ui, cell_bg, KEYMAP_COL_KB_W, |ui| {
                if ui.add_sized([KEYMAP_BUTTON_W, 18.0], egui::Button::new("変更").small()).clicked() {
                    draft.key_capture_dialog = Some(KeyCaptureDialogState::new(KeymapCaptureTarget::Reader(action), binding.keyboard.or(binding.default_keyboard)));
                }
                if draw_reset_button(ui, binding.keyboard.is_some()) {
                    draft.keymap.set_reader_keyboard(action, None);
                }
                draw_key_combo_line(ui, binding.keyboard.or(binding.default_keyboard));
            });
            let r3 = keymap_cell(ui, cell_bg, KEYMAP_COL_MOUSE_W, |ui| {
                if ui.add_sized([KEYMAP_BUTTON_W, 18.0], egui::Button::new("変更").small()).clicked() {
                    draft.mouse_capture_dialog = Some(MouseCaptureDialogState::new(action, binding.mouse.or(binding.default_mouse)));
                }
                if draw_reset_button(ui, binding.mouse.is_some()) {
                    draft.keymap.set_reader_mouse(action, None);
                }
                draw_mouse_combo_line(ui, binding.mouse.or(binding.default_mouse));
            });
            draw_keymap_row_borders(ui, &[r1, r2, r3]);
        });
    }

    ui.add_space(8.0);
    ui.heading("エクスプローラー");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let r1 = keymap_feature_cell(ui, cell_bg, KEYMAP_COL_FEATURE_W, "機能");
        let r2 = keymap_cell(ui, cell_bg, KEYMAP_COL_KB_W, |ui| { ui.strong("キーボード"); });
        draw_keymap_row_borders(ui, &[r1, r2]);
    });
    for action in ExplorerAction::ALL.iter().copied() {
        let binding = draft.keymap.explorer_binding(action);
        let editable = action.is_editable();
        let name = if editable { action.display_name().to_string() } else { format!("{}（固定）", action.display_name()) };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r1 = keymap_feature_cell(ui, cell_bg, KEYMAP_COL_FEATURE_W, &name);
            let r2 = keymap_cell(ui, cell_bg, KEYMAP_COL_KB_W, |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    if ui.add_sized([KEYMAP_BUTTON_W, 18.0], egui::Button::new("変更").small()).clicked() {
                        draft.key_capture_dialog = Some(KeyCaptureDialogState::new(KeymapCaptureTarget::Explorer(action), binding.keyboard.or(binding.default_keyboard)));
                    }
                    if draw_reset_button(ui, binding.keyboard.is_some()) {
                        draft.keymap.set_explorer_keyboard(action, None);
                    }
                    draw_key_combo_line(ui, binding.keyboard.or(binding.default_keyboard));
                });
            });
            draw_keymap_row_borders(ui, &[r1, r2]);
        });
    }
}

/// SHIFT/CTRL/ALTトグルボタン。押し込み状態はegui標準のselected色で表現する。
fn draw_mod_toggle(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    let text = format!("{label}\n{}", if *value { "ON" } else { "OFF" });
    if ui.add_sized([86.0, 34.0], egui::Button::new(text).selected(*value)).clicked() {
        *value = !*value;
    }
}

/// キーボード用キャプチャダイアログ。設定ダイアログのModal内から毎フレーム呼ばれ、
/// draft.key_capture_dialogがSomeの間だけ表示する。SHIFT/CTRL/ALTはトグルで独立管理し、
/// 「設定開始」中に拾うキーイベントは本体キー1つだけに使う。これによりコンボキー
/// （例: SHIFT+A）を狙った際に、修飾キー単体を本体キーとして誤登録する問題を防ぐ。
fn draw_key_capture_dialog(ctx: &egui::Context, draft: &mut SettingsDraft) {
    if draft.key_capture_dialog.is_none() {
        return;
    }
    let mut close_cancel = false;
    let mut confirm: Option<KeyCombo> = None;

    egui::Modal::new(egui::Id::new("key_capture_dialog")).show(ctx, |ui| {
        let state = draft.key_capture_dialog.as_mut().unwrap();
        ui.set_min_width(380.0);
        ui.heading("キーアサイン変更");
        ui.label(format!("対象: {} / キーボード", state.target.display_name()));
        ui.separator();

        ui.label("修飾キー");
        ui.horizontal(|ui| {
            draw_mod_toggle(ui, "SHIFT", &mut state.shift);
            draw_mod_toggle(ui, "CTRL", &mut state.ctrl);
            draw_mod_toggle(ui, "ALT", &mut state.alt);
        });
        ui.add_space(8.0);

        ui.label("本体キー");
        ui.vertical_centered(|ui| {
            let mut key_label = state.key.map(|k| format!("{k:?}")).unwrap_or_else(|| "-".to_string());
            ui.add_sized([300.0, 0.0], egui::TextEdit::singleline(&mut key_label).interactive(false));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let btn_label = if state.listening { "待機中…" } else { "設定開始" };
                if ui.add_sized([90.0, 0.0], egui::Button::new(btn_label).selected(state.listening)).clicked() {
                    state.listening = !state.listening;
                }
                if ui.add_sized([70.0, 0.0], egui::Button::new("消去")).clicked() {
                    state.key = None;
                    state.listening = false;
                }
            });
        });

        if state.listening {
            ui.colored_label(egui::Color32::from_rgb(220, 160, 40), "キーを押してください（修飾キー単体は無視します）");
            let captured = ctx.input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, .. } => Some(*key),
                _ => None,
            }));
            if let Some(k) = captured {
                state.key = Some(k);
                state.listening = false;
            }
            ctx.request_repaint();
        }

        if let Some(err) = &state.error {
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
        }

        ui.separator();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("設定").clicked() {
                if let Some(key) = state.key {
                    confirm = Some(KeyCombo { key, ctrl: state.ctrl, shift: state.shift, alt: state.alt });
                } else {
                    state.error = Some("本体キーが未設定です。「設定開始」でキーを入力してください。".to_string());
                }
            }
            if ui.button("キャンセル").clicked() {
                close_cancel = true;
            }
        });
    });

    if close_cancel {
        draft.key_capture_dialog = None;
        return;
    }
    if let Some(combo) = confirm {
        let target = draft.key_capture_dialog.as_ref().unwrap().target;
        let conflict = match target {
            KeymapCaptureTarget::Reader(a) => draft.keymap.find_reader_keyboard_conflict(combo, a).map(|c| c.display_name()),
            KeymapCaptureTarget::Explorer(a) => draft.keymap.find_explorer_keyboard_conflict(combo, a).map(|c| c.display_name()),
        };
        match target {
            KeymapCaptureTarget::Reader(a) => draft.keymap.set_reader_keyboard(a, Some(combo)),
            KeymapCaptureTarget::Explorer(a) => draft.keymap.set_explorer_keyboard(a, Some(combo)),
        }
        draft.keymap_last_warning = conflict.map(|name| format!("「{name}」と重複する入力を登録しました。"));
        draft.key_capture_dialog = None;
    }
}

/// マウス用キャプチャダイアログ。draw_key_capture_dialog()と同じ構造・レイアウトで、
/// 「本体キー」を「マウスボタン」に置き換えたもの。修飾キーはトグルが真実の情報源で、
/// 「設定開始」中に拾うのはマウスの物理入力（クリック種別／ホイール）1つだけ。
fn draw_mouse_capture_dialog(ctx: &egui::Context, draft: &mut SettingsDraft) {
    if draft.mouse_capture_dialog.is_none() {
        return;
    }
    let mut close_cancel = false;
    let mut confirm: Option<MouseCombo> = None;
    // 「設定開始」ボタン自体のクリックを即座にLeftClickとして誤検出しないよう、
    // このフレームの冒頭時点(=前フレームまで)で listening だったかどうかで判定する。
    let was_listening = draft.mouse_capture_dialog.as_ref().unwrap().listening;
    let mut dialog_button_clicked = false;

    egui::Modal::new(egui::Id::new("mouse_capture_dialog")).show(ctx, |ui| {
        let state = draft.mouse_capture_dialog.as_mut().unwrap();
        ui.set_min_width(380.0);
        ui.heading("キーアサイン変更");
        ui.label(format!("対象: {} / マウス", state.action.display_name()));
        ui.separator();

        ui.label("修飾キー");
        ui.horizontal(|ui| {
            draw_mod_toggle(ui, "SHIFT", &mut state.shift);
            draw_mod_toggle(ui, "CTRL", &mut state.ctrl);
            draw_mod_toggle(ui, "ALT", &mut state.alt);
        });
        ui.add_space(8.0);

        ui.label("マウスボタン");
        ui.vertical_centered(|ui| {
            let mut action_label = state.mouse_action.map(mouse_action_name).unwrap_or("-").to_string();
            ui.add_sized([300.0, 0.0], egui::TextEdit::singleline(&mut action_label).interactive(false));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let btn_label = if state.listening { "待機中…" } else { "設定開始" };
                if ui.add_sized([90.0, 0.0], egui::Button::new(btn_label).selected(state.listening)).clicked() {
                    state.listening = !state.listening;
                    dialog_button_clicked = true;
                }
                if ui.add_sized([70.0, 0.0], egui::Button::new("消去")).clicked() {
                    state.mouse_action = None;
                    state.listening = false;
                    dialog_button_clicked = true;
                }
            });
        });

        if state.listening {
            ui.colored_label(egui::Color32::from_rgb(220, 160, 40), "マウスを操作してください");
        }

        if let Some(err) = &state.error {
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
        }

        ui.separator();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("設定").clicked() {
                dialog_button_clicked = true;
                if let Some(action) = state.mouse_action {
                    confirm = Some(MouseCombo { action, ctrl: state.ctrl, shift: state.shift, alt: state.alt });
                } else {
                    state.error = Some("マウスボタンが未設定です。「設定開始」でマウスを操作してください。".to_string());
                }
            }
            if ui.button("キャンセル").clicked() {
                dialog_button_clicked = true;
                close_cancel = true;
            }
        });
    });

    if close_cancel {
        draft.mouse_capture_dialog = None;
        return;
    }

    // ダイアログ自身のボタンクリックを物理マウス入力として誤検出しないよう、
    // was_listening（前フレームまでの状態）かつ今フレームでダイアログのボタンを
    // 押していない場合のみ、次の物理入力を1つ拾う。
    if was_listening && !dialog_button_clicked {
        let captured = ctx.input(|i| {
            use egui::PointerButton::*;
            if i.pointer.button_double_clicked(Primary) {
                Some(MouseAction::LeftDoubleClick)
            } else if i.pointer.button_double_clicked(Secondary) {
                Some(MouseAction::RightDoubleClick)
            } else if i.pointer.button_double_clicked(Middle) {
                Some(MouseAction::MiddleDoubleClick)
            } else if i.pointer.button_clicked(Primary) {
                Some(MouseAction::LeftClick)
            } else if i.pointer.button_clicked(Secondary) {
                Some(MouseAction::RightClick)
            } else if i.pointer.button_clicked(Middle) {
                Some(MouseAction::MiddleClick)
            } else if i.pointer.button_clicked(Extra1) {
                Some(MouseAction::Extra1)
            } else if i.pointer.button_clicked(Extra2) {
                Some(MouseAction::Extra2)
            } else {
                let sd = i.smooth_scroll_delta();
                if sd.y > 5.0 {
                    Some(MouseAction::WheelUp)
                } else if sd.y < -5.0 {
                    Some(MouseAction::WheelDown)
                } else {
                    None
                }
            }
        });
        if let Some(action) = captured {
            let state = draft.mouse_capture_dialog.as_mut().unwrap();
            state.mouse_action = Some(action);
            state.listening = false;
        }
        ctx.request_repaint();
    }

    if let Some(combo) = confirm {
        let action = draft.mouse_capture_dialog.as_ref().unwrap().action;
        let conflict = draft.keymap.find_reader_mouse_conflict(combo, action).map(|c| c.display_name());
        draft.keymap.set_reader_mouse(action, Some(combo));
        draft.keymap_last_warning = conflict.map(|name| format!("「{name}」と重複する入力を登録しました。"));
        draft.mouse_capture_dialog = None;
    }
}

impl NekoviewApp {
    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    /// 設定ダイアログを開く。編集用の下書き(draft)を現在値から作り直す
    /// （[反映]を押すまで実際の設定には反映されない）。
    pub fn open_settings(&mut self) {
        self.settings_draft = SettingsDraft::from_current(&self.config, &self.viewer_cfg.lock().unwrap(), self.show_hidden, &self.translate_cfg);
        self.translate_conn_rx = None;
        self.translate_conn_status = None;
        self.settings_open = true;
    }

    /// 設定ダイアログ本体。`egui::Modal` はこの `ctx`（エクスプローラー窓）内の入力を
    /// 自動的にブロックする。ビューアー窓側は別 Context のため、`render_viewer` 側で
    /// 同様の Modal を出して操作を止める（`settings_is_open()` 参照）。
    /// 各タブの入力は draft（`self.settings_draft`）に対して自由に編集させ、
    /// タブ共通の[反映]/[閉じる]ボタンでのみ実際の設定へ書き戻す。
    pub(crate) fn draw_settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut close = false;
        let mut apply = false;
        egui::Modal::new(egui::Id::new("settings_dialog")).show(ctx, |ui| {
            // エクスプローラー窓がダイアログより小さいと見切れて操作不能になるため、
            // 窓サイズ(からModalの枠ぶんの余白を引いた値)を上限に、タブ行・ボタン行を
            // 含む全体を縦横スクロール可能にする。窓が十分大きければバーは出ない。
            //
            // min_scrolled も max と同値で毎フレーム指定するのが肝。ScrollArea の外形は
            // 親(Modal の Area)が記憶した前フレームサイズにもクランプされるため、max だけ
            // だと一度縮んだ記憶に引きずられて窓を広げても戻らない(縮小の一方通行)。
            // min_scrolled はその記憶を無視して張れる下限なので、窓サイズへ双方向に追従する。
            let target = (ctx.content_rect().size() - egui::vec2(80.0, 80.0))
                .max(egui::vec2(100.0, 100.0));
            egui::ScrollArea::both()
                .max_width(target.x)
                .max_height(target.y)
                .min_scrolled_width(target.x)
                .min_scrolled_height(target.y)
                .show(ui, |ui| {
            ui.set_min_width(460.0);
            ui.heading(i18n::t().settings_title());
            ui.separator();

            ui.horizontal(|ui| {
                for (tab, label) in [
                    (SettingsTab::Common, i18n::t().settings_tab_common()),
                    (SettingsTab::Anim, i18n::t().settings_tab_anim()),
                    (SettingsTab::Static, i18n::t().settings_tab_static()),
                    (SettingsTab::Viewer, i18n::t().settings_tab_viewer()),
                    (SettingsTab::Translate, i18n::t().settings_tab_translate()),
                    (SettingsTab::Keymap, "キーアサイン"),
                    (SettingsTab::Other, i18n::t().settings_tab_other()),
                ] {
                    ui.selectable_value(&mut self.settings_tab, tab, label);
                }
            });
            ui.separator();

            match self.settings_tab {
                SettingsTab::Common => draw_settings_tab_common(ui, &mut self.settings_draft),
                SettingsTab::Anim => draw_settings_tab_anim(ui, &mut self.settings_draft),
                SettingsTab::Static => self.draw_settings_tab_static(ui),
                SettingsTab::Viewer => draw_settings_tab_viewer(ui, &mut self.settings_draft),
                SettingsTab::Translate => self.draw_settings_tab_translate(ui, ctx),
                SettingsTab::Keymap => draw_settings_tab_keymap(ui, &mut self.settings_draft),
                SettingsTab::Other => self.draw_settings_tab_other(ui),
            }

            ui.separator();
            ui.label(i18n::t().settings_legend());
            ui.separator();
            // GTK/GNOME 慣習（キャンセル系=左、既定/主アクション=右）に合わせて
            // [閉じる]を左、主アクションの[反映]を右に置く。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(i18n::t().settings_apply()).clicked() {
                    apply = true;
                }
                if ui.button(i18n::t().settings_close()).clicked() {
                    close = true;
                }
            });
            });
        });
        // キーボード/マウス用キャプチャダイアログ: 設定ダイアログ本体の後に描画し、
        // レイヤー順で最前面（=入力を受け付ける対象）にする。
        draw_key_capture_dialog(ctx, &mut self.settings_draft);
        draw_mouse_capture_dialog(ctx, &mut self.settings_draft);
        if apply {
            // キャッシュ合計がシステムRAMの50%を超えている間は保存を拒否する
            // （警告は同フレーム内で既に赤文字表示済み）。
            if !self.settings_draft.cache_over_budget() {
                // 疎通確認済みフラグは「実際に反映されるURLが、直前に疎通チェックへ成功した
                // URLと一致する場合」だけ引き継ぐ。URLだけ変えて疎通チェックし直さずに反映
                // した場合は不一致になりfalseへ落ちる（セッション単位の安全側判定）。
                let verified_url_matches = self.translate_conn_verified_url.as_deref()
                    == Some(self.settings_draft.translate_base_url.trim());
                self.settings_draft.apply_to(&mut self.config, &mut self.viewer_cfg.lock().unwrap(), &mut self.translate_cfg);
                self.translate_conn_verified = verified_url_matches;
                self.show_hidden = self.settings_draft.show_hidden;
                // thumb_size/thumb_filterはstateファイルに乗っていないためconfig.iniへ直接保存する。
                self.config.save();
                // keymapは行数可変のため専用ファイル(keymap.ini)へ別途保存する。
                self.config.keymap.save();
                self.persist_state();
                close = true;
            }
        }
        if close {
            self.settings_open = false;
        }
    }

    fn draw_settings_tab_static(&mut self, ui: &mut egui::Ui) {
        ui.label(i18n::t().settings_static_placeholder());
    }

    /// 翻訳機能(実験的)タブ。ローカルAI(OpenAI互換API)のURL・モデル取得・翻訳/OCRモデル選択と、
    /// OCR結果オーバーレイの横幅・配置(四隅)を設定する。クラウドAPIキー管理は対象外。
    /// モデル名はフリーテキスト入力を廃止し、「モデル取得」で得た一覧からの選択のみ受け付ける。
    fn draw_settings_tab_translate(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // モデル取得の結果をポーリング（成功時は ModelsOk → 一覧をドロップダウンへ反映。
        // OCRモデルが選択済みならそのモデルで続けてVisionOk/Failedを待つ。失敗はどの段階でも即終了）。
        if let Some(rx) = &self.translate_conn_rx {
            let mut finished = false;
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    crate::translate::ConnCheckMsg::ModelsOk(models) => {
                        self.translate_conn_status = Some(format!("疎通OK（{}件取得）", models.len()));
                        self.settings_draft.translate_available_models = models;
                        // どのURLに対する疎通成功かを記録しておく（[反映]時にURL一致を検証するため）。
                        self.translate_conn_verified_url = Some(self.settings_draft.translate_base_url.trim().to_string());
                        if self.settings_draft.translate_ocr_model.trim().is_empty() {
                            finished = true;
                        }
                    }
                    crate::translate::ConnCheckMsg::VisionOk(preview) => {
                        self.translate_conn_status = Some(format!("画像入力OK（応答例: {preview}）"));
                        finished = true;
                    }
                    crate::translate::ConnCheckMsg::Failed(e) => {
                        // eにはHTTPステータスや接続エラーの詳細が含まれる(translate.rs参照)。
                        self.translate_conn_status = Some(format!("失敗: {e}"));
                        finished = true;
                    }
                }
            }
            if finished {
                self.translate_conn_rx = None;
            }
        }

        ui.label(i18n::t().settings_translate_experimental_note());
        ui.separator();

        ui.label(i18n::t().settings_translate_url_label());
        ui.text_edit_singleline(&mut self.settings_draft.translate_base_url);

        ui.horizontal(|ui| {
            let testing = self.translate_conn_rx.is_some();
            let url_empty = self.settings_draft.translate_base_url.trim().is_empty();
            ui.add_enabled_ui(!testing && !url_empty, |ui| {
                if ui.button(i18n::t().settings_translate_test_button()).clicked() {
                    let base_url = self.settings_draft.translate_base_url.trim().to_string();
                    let model = self.settings_draft.translate_ocr_model.trim().to_string();
                    self.translate_conn_status = None;
                    self.translate_conn_rx = Some(crate::translate::spawn_conn_check(ctx.clone(), base_url, model));
                }
            });
            if self.translate_conn_rx.is_some() {
                ui.label(i18n::t().settings_translate_testing());
            }
        });
        if let Some(status) = &self.translate_conn_status {
            ui.label(status);
        }
        ui.separator();

        let unselected = i18n::t().settings_translate_model_unselected();
        let models = self.settings_draft.translate_available_models.clone();

        ui.label(i18n::t().settings_translate_translation_model_label());
        if models.is_empty() {
            ui.weak(i18n::t().settings_translate_no_models_hint());
        } else {
            let selected = if self.settings_draft.translate_translation_model.is_empty() {
                unselected
            } else {
                self.settings_draft.translate_translation_model.as_str()
            };
            egui::ComboBox::from_id_salt("settings_translate_translation_model").selected_text(selected).show_ui(ui, |ui| {
                for m in &models {
                    if ui.selectable_label(self.settings_draft.translate_translation_model == *m, m).clicked() {
                        self.settings_draft.translate_translation_model = m.clone();
                        // 翻訳モデルが主。OCRモデルをユーザーがまだ明示的に選び直していなければ追従させる。
                        if !self.settings_draft.translate_ocr_model_overridden {
                            self.settings_draft.translate_ocr_model = m.clone();
                        }
                    }
                }
            });
        }

        ui.label(i18n::t().settings_translate_ocr_model_label());
        if models.is_empty() {
            ui.weak(i18n::t().settings_translate_no_models_hint());
        } else {
            let selected = if self.settings_draft.translate_ocr_model.is_empty() {
                unselected
            } else {
                self.settings_draft.translate_ocr_model.as_str()
            };
            egui::ComboBox::from_id_salt("settings_translate_ocr_model").selected_text(selected).show_ui(ui, |ui| {
                for m in &models {
                    if ui.selectable_label(self.settings_draft.translate_ocr_model == *m, m).clicked() {
                        self.settings_draft.translate_ocr_model = m.clone();
                        // 明示的に選び直した時点で、以後は翻訳モデルの変更に追従しない。
                        self.settings_draft.translate_ocr_model_overridden = true;
                    }
                }
            });
        }
        ui.separator();

        // 注意: このスライダーはどの描画コードからも参照されていない(未使用設定)。
        // 旧・画面隅フローティングオーバーレイ用に用意されたが、翻訳ボタンをツールバーへ
        // 昇格した際にオーバーレイ自体を撤去したため宙に浮いている。値は保持のみ残す。
        ui.label(i18n::t().settings_translate_overlay_width_label());
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = 260.0;
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.settings_draft.translate_overlay_width, OVERLAY_WIDTH_FLOOR..=OVERLAY_WIDTH_CEILING).show_value(false));
                ui.label(format!("{} px", self.settings_draft.translate_overlay_width));
            });
        });
    }

    fn draw_settings_tab_other(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(i18n::t().settings_version_label());
            ui.label(env!("CARGO_PKG_VERSION"));
        });
    }
}
