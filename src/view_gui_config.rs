//! 設定ダイアログの egui 描画部分。データの永続化(state ファイル)は gui_config.rs、
//! 起動時設定(config.ini)は config.rs が担当し、ここは NekoviewApp に生えた
//! [設定]ボタン以降のUI（タブ切り替え・各タブの中身・下書き→反映のフロー）のみを扱う。

use crate::config::{AppConfig, ResizeFilter, filter_to_str};
use crate::gui_config::{ThumbbarPos, ViewerConfig};
use crate::i18n;
use crate::view_explorer::NekoviewApp;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Common,
    Anim,
    Static,
    Viewer,
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
}

impl SettingsDraft {
    pub(crate) fn from_current(config: &AppConfig, viewer_cfg: &ViewerConfig, show_hidden: bool) -> Self {
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
        }
    }

    /// キャッシュ合計がシステムRAMの50%を超えているか（超過時は[反映]を拒否する）。
    fn cache_over_budget(&self) -> bool {
        self.cache_total_user_set && self.system_ram_mb > 0 && self.cache_total_mb > self.system_ram_mb / 2
    }

    /// [反映]クリック時に実際の設定へ書き戻す。呼び出し側は事前に `cache_over_budget()` を
    /// 確認し、超過時はそもそも呼ばない（保存を拒否する）こと。
    fn apply_to(&self, config: &mut AppConfig, viewer_cfg: &mut ViewerConfig) {
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

impl NekoviewApp {
    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    /// 設定ダイアログを開く。編集用の下書き(draft)を現在値から作り直す
    /// （[反映]を押すまで実際の設定には反映されない）。
    pub fn open_settings(&mut self) {
        self.settings_draft = SettingsDraft::from_current(&self.config, &self.viewer_cfg.lock().unwrap(), self.show_hidden);
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
        if apply {
            // キャッシュ合計がシステムRAMの50%を超えている間は保存を拒否する
            // （警告は同フレーム内で既に赤文字表示済み）。
            if !self.settings_draft.cache_over_budget() {
                let exif_enabled_before = self.viewer_cfg.lock().unwrap().exif_orientation_enabled;
                self.settings_draft.apply_to(&mut self.config, &mut self.viewer_cfg.lock().unwrap());
                if self.settings_draft.exif_orientation_enabled != exif_enabled_before {
                    self.redecode_after_exif_toggle();
                }
                self.show_hidden = self.settings_draft.show_hidden;
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

    fn draw_settings_tab_other(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(i18n::t().settings_version_label());
            ui.label(env!("CARGO_PKG_VERSION"));
        });
    }
}
