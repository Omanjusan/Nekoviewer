//! タグパネルの入口「🏷 タグ管理」と、エクスプローラー中央に出す選択メニュー。
//!
//! 主導線は「自動タグ抽出」（大きな長方形）。副導線は、カード3枚（抽出用学習／抽出ルール定義・確認／タグ・カテゴリ編集）。
//! 状態（学習の件数・ルールの有効件数）は、カードのチップに出す。学習データが無いときは、色つきの一言で、学習をすすめる。

use crate::i18n;
use crate::tag_confirm_text as ctext;

use super::NekoviewApp;

impl NekoviewApp {
    /// タグ系の窓・モーダルを出す位置: エクスプローラー部（中央パネル）の中心。まだ測れていなければ画面中央。
    pub(super) fn explorer_center(&self, ctx: &egui::Context) -> egui::Pos2 {
        let r = self.tag_manager_area_rect;
        if r.is_positive() { r.center() } else { ctx.content_rect().center() }
    }

    /// `egui::Modal` の、エクスプローラー部中央に出す版。背後の暗幕は従来どおり画面全体。
    pub(super) fn explorer_modal(&self, ctx: &egui::Context, id: &'static str) -> egui::Modal {
        let area = egui::Area::new(egui::Id::new(id))
            .kind(egui::UiKind::Modal)
            .sense(egui::Sense::hover())
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(self.explorer_center(ctx))
            .order(egui::Order::Foreground)
            .interactable(true);
        egui::Modal::new(egui::Id::new(id)).area(area)
    }

    /// タグパネル最上段の入口ボタン（左右中央）。押すと、エクスプローラー中央に選択メニュー（`draw_tag_entry_modal`）が開く。
    pub(super) fn draw_tag_entry_menu(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            if ui.button(i18n::render(|t| t.tag_panel_manager_button().to_string())).clicked() {
                self.tag_entry_menu_open = true;
            }
        });
    }

    /// エクスプローラー中央の選択メニュー。主導線「自動タグ抽出」を大きな長方形にし、学習・ルール・編集を、カードにして並べる。
    pub(super) fn draw_tag_entry_modal(&mut self, ctx: &egui::Context) {
        if !self.tag_entry_menu_open {
            return;
        }
        let learned = self.tag_learned.len();
        let enabled = self.can_start_tag_scan();
        let trial = self.tag_trial_active;
        let targets = self.tag_scan_targets().len();
        let rules_on = self.tag_rules.rules.iter().filter(|r| r.enabled).count();
        let rules_all = self.tag_rules.rules.len();
        let mut pick: Option<EntryPick> = None;
        let mut close = false;

        let modal = self.explorer_modal(ctx, "tag_entry_menu").show(ctx, |ui| {
            ui.set_width(MENU_W);
            // 見出し: 左にタイトル、中央に対象の件数、右端に閉じるボタン。
            let (head, _) = ui.allocate_exact_size(egui::vec2(MENU_W, 30.0), egui::Sense::hover());
            let mut left = ui.new_child(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::left_to_right(egui::Align::Center)));
            left.heading(ctext::menu_title());
            ui.painter().text(head.center(), egui::Align2::CENTER_CENTER, ctext::menu_scope(targets), egui::FontId::proportional(15.0), ui.visuals().weak_text_color());
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::right_to_left(egui::Align::Center)));
            if close_button(&mut right).on_hover_text(i18n::as_ja(|| ctext::menu_close_hint())).clicked() {
                close = true;
            }
            ui.add_space(10.0);

            // 主導線。
            let reason = if trial {
                i18n::as_ja(|| i18n::render(|t| t.tag_scan_trial_hint().to_string()))
            } else {
                i18n::as_ja(|| i18n::render(|t| t.tag_scan_unavailable_hint().to_string()))
            };
            if main_button(ui, enabled, &reason) {
                pick = Some(EntryPick::Scan);
            }
            ui.add_space(14.0);

            // 副導線（カード3枚）。
            ui.label(egui::RichText::new(ctext::menu_group_label()).weak().size(13.0));
            ui.add_space(4.0);
            let gap = 10.0;
            let w = (MENU_W - gap * 2.0) / 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                let learn_chip = i18n::render(|t| t.tag_learn_status(learned).to_string());
                let learn_hover = i18n::as_ja(|| i18n::render(|t| t.tag_learn_status_hover().to_string()));
                let advice = if learned == 0 { Some(ctext::menu_learn_advice()) } else { None };
                if card(ui, w, ctext::menu_learn_title(), ctext::menu_learn_desc(), advice, Some((&learn_chip, learned == 0)), &learn_hover) {
                    pick = Some(EntryPick::Learn);
                }
                let rules_chip = i18n::render(|t| t.tag_rules_count(rules_on, rules_all).to_string());
                let rules_hover = i18n::as_ja(|| i18n::render(|t| t.tag_rules_open_hover().to_string()));
                if card(ui, w, ctext::menu_rules_title(), ctext::menu_rules_desc(), None, Some((&rules_chip, false)), &rules_hover) {
                    pick = Some(EntryPick::Rules);
                }
                let edit_title = i18n::render(|t| t.tag_menu_edit_item().to_string());
                if card(ui, w, &edit_title, ctext::menu_edit_desc(), None, None, "") {
                    pick = Some(EntryPick::Edit);
                }
            });
        });

        if let Some(p) = pick {
            self.tag_entry_menu_open = false;
            match p {
                EntryPick::Scan => self.request_tag_scan(),
                EntryPick::Learn => self.open_tag_learn_window(),
                EntryPick::Rules => self.tag_rules_open = true,
                EntryPick::Edit => self.open_tag_manager_overlay(),
            }
        } else if close || modal.should_close() {
            self.tag_entry_menu_open = false;
        }
    }
}

/// メニューの幅。
const MENU_W: f32 = 640.0;
/// 主導線の高さ。
const MAIN_H: f32 = 96.0;
/// カードの高さ。
const CARD_H: f32 = 188.0;
const AMBER: egui::Color32 = egui::Color32::from_rgb(230, 160, 40);

enum EntryPick {
    Scan,
    Learn,
    Rules,
    Edit,
}

/// 閉じるボタン（「✕」）。フォントの字形に頼らず、線で描く（字形が無いと、四角になるため）。
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let vis = ui.visuals();
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, vis.widgets.hovered.weak_bg_fill);
    }
    let color = if resp.hovered() { vis.strong_text_color() } else { vis.text_color() };
    let stroke = egui::Stroke::new(2.0, color);
    let c = rect.center();
    let d = 6.0;
    ui.painter().line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
    ui.painter().line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
    resp
}

/// 主導線の大きな長方形。押せるときは塗りつぶし、押せないときは薄い塗りと枠だけにして、理由をホバーで出す。押されたら true。
fn main_button(ui: &mut egui::Ui, enabled: bool, reason: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(MENU_W, MAIN_H), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let vis = ui.visuals();
    let (fill, fg, sub_fg, stroke) = if enabled {
        let base = vis.selection.bg_fill;
        let fill = if resp.hovered() { base.gamma_multiply(1.18) } else { base };
        (fill, vis.selection.stroke.color, vis.selection.stroke.color.gamma_multiply(0.85), egui::Stroke::NONE)
    } else {
        (vis.faint_bg_color, vis.weak_text_color(), vis.weak_text_color(), vis.widgets.noninteractive.bg_stroke)
    };
    let painter = ui.painter();
    painter.rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);
    let left = rect.left() + 24.0;
    painter.text(egui::pos2(left, rect.center().y - 14.0), egui::Align2::LEFT_CENTER, ctext::menu_main_title(), egui::FontId::proportional(30.0), fg);
    let sub = if enabled { ctext::menu_main_sub() } else { ctext::menu_main_sub_disabled() };
    painter.text(egui::pos2(left, rect.center().y + 20.0), egui::Align2::LEFT_CENTER, sub, egui::FontId::proportional(15.0), sub_fg);
    painter.text(egui::pos2(rect.right() - 22.0, rect.center().y), egui::Align2::RIGHT_CENTER, "▶", egui::FontId::proportional(28.0), sub_fg);
    if !enabled {
        resp.on_hover_text(reason);
        return false;
    }
    resp.clicked()
}

/// 副導線のカード。タイトル・説明・（あれば）色つきの一言と、下端の状態のチップ。`chip` は（文字, 注意の色にするか）。押されたら true。
fn card(ui: &mut egui::Ui, w: f32, title: &str, desc: &str, advice: Option<&str>, chip: Option<(&str, bool)>, hover: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, CARD_H), egui::Sense::click());
    let vis = ui.visuals();
    let fill = if resp.hovered() { vis.widgets.hovered.weak_bg_fill } else { vis.faint_bg_color };
    let stroke = if resp.hovered() { vis.selection.stroke } else { vis.widgets.noninteractive.bg_stroke };
    ui.painter().rect(rect, 6.0, fill, egui::Stroke::new(1.0, stroke.color), egui::StrokeKind::Inside);
    let inner = rect.shrink(10.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.add(egui::Label::new(egui::RichText::new(title).strong().size(17.0)).wrap().selectable(false));
    child.add(egui::Label::new(egui::RichText::new(desc).weak().size(13.0)).wrap().selectable(false));
    if let Some(a) = advice {
        child.add(egui::Label::new(egui::RichText::new(a).size(13.0).color(AMBER)).wrap().selectable(false));
    }
    if let Some((text, warn)) = chip {
        let font = egui::FontId::proportional(13.0);
        let color = if warn { AMBER } else { ui.visuals().text_color() };
        let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
        let pad = egui::vec2(8.0, 2.0);
        let chip_rect = egui::Rect::from_min_size(egui::pos2(inner.left(), inner.bottom() - galley.size().y - pad.y * 2.0), galley.size() + pad * 2.0);
        let (bg, line) = if warn { (egui::Color32::TRANSPARENT, egui::Stroke::new(1.0, AMBER)) } else { (ui.visuals().widgets.inactive.weak_bg_fill, egui::Stroke::NONE) };
        ui.painter().rect(chip_rect, 99.0, bg, line, egui::StrokeKind::Inside);
        ui.painter().galley(chip_rect.min + pad, galley, color);
    }
    if !hover.is_empty() {
        resp.clone().on_hover_text(hover);
    }
    resp.clicked()
}
