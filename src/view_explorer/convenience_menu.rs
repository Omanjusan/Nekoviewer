//! 便利機能の展開メニュー（フォルダ本アクセス・スコアリング・ツールボックスのON/OFF）。
//!
//! 旧3ボタンを1個にまとめたもの。開閉・キー操作の作法はソート展開メニュー（panels.rs の draw_sort_menu ほか）を踏襲する。
//! 文言は crate::convenience_text。仕様は docs/features/convenience-menu.md。

use super::panels::draw_cursor_ring;
use super::*;
use crate::convenience_text as text;
use crate::view_reader::{sort_choice_button, SORT_CHOICE_SELECTED};

/// OFF側を選択中に見せる塗り色（グレー）。ONの青と並べて、現在の状態が分かるようにする。
const OFF_SELECTED: egui::Color32 = egui::Color32::from_rgb(110, 116, 128);
const MENU_WIDTH: f32 = 380.0;

impl NekoviewApp {
    /// その機能が今ONか。
    pub(super) fn convenience_is_on(&self, item: ConvenienceItem) -> bool {
        match item {
            ConvenienceItem::FolderBook => self.folder_book_access_enabled,
            ConvenienceItem::Scoring => self.viewer_cfg.lock().unwrap().rating_overlay_enabled,
            ConvenienceItem::ToolBox => self.viewer_cfg.lock().unwrap().tool_palette.visible,
        }
    }

    /// その機能のON/OFFを設定する（クリック・キーボード両経路で共通）。
    pub(super) fn set_convenience(&mut self, item: ConvenienceItem, on: bool) {
        if self.convenience_is_on(item) == on {
            return;
        }
        match item {
            // 永続化しない（アプリ再起動のたびに毎回OFFへ戻る）
            ConvenienceItem::FolderBook => self.folder_book_access_enabled = on,
            ConvenienceItem::Scoring => {
                self.viewer_cfg.lock().unwrap().rating_overlay_enabled = on;
                self.persist_state();
            }
            // ファイルを渡り歩いても同じ状態を保つ（viewer_cfg経由でPaletteStateへ直結）
            ConvenienceItem::ToolBox => self.viewer_cfg.lock().unwrap().tool_palette.visible = on,
        }
    }

    /// キーボードでメニューを開き、カーソルを先頭の項目へ置く。
    pub(super) fn open_convenience_menu_by_keyboard(&mut self) {
        self.convenience_menu_open = true;
        self.convenience_menu_cursor = Some(0);
    }

    pub(super) fn close_convenience_menu(&mut self) {
        self.convenience_menu_open = false;
        self.convenience_menu_cursor = None;
    }

    /// メニューバーのボタン（開閉・展開メニュー）。draw_menu_bar から呼ぶ。
    pub(super) fn draw_convenience_button(&mut self, ui: &mut egui::Ui, is_cursor: bool, help_on: bool) {
        let r = ui.button(text::button_label());
        if is_cursor {
            draw_cursor_ring(ui, r.rect);
        }
        super::help::help_tip(&r, help_on, &i18n::t().help_view_toggles());
        if r.clicked() {
            if self.convenience_menu_open {
                self.close_convenience_menu();
            } else {
                // マウスで開いた場合はメニュー内カーソルを出さない（キー操作はバー側のまま）
                self.convenience_menu_open = true;
                self.convenience_menu_cursor = None;
            }
        }
        // バー側カーソルがボタンから外れたら、キーボードで開いたメニューは縮小する
        if self.convenience_menu_cursor.is_some() && !is_cursor {
            self.close_convenience_menu();
        }
        let mut open = self.convenience_menu_open;
        let mut close_clicked = false;
        egui::Popup::menu(&r)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| close_clicked = self.draw_convenience_menu(ui));
        self.convenience_menu_open = open && !close_clicked;
        if !open {
            self.convenience_menu_cursor = None;
        }
    }

    /// 展開メニューの中身。タイトル（右寄せ✕）＋3項目（名前・ON/OFF・説明）を区切り線で並べる。
    /// 戻り値は、✕で閉じる操作があったか。
    fn draw_convenience_menu(&mut self, ui: &mut egui::Ui) -> bool {
        let mut close_clicked = false;
        ui.set_max_width(MENU_WIDTH);
        ui.set_min_width(MENU_WIDTH);
        let cur = self.convenience_menu_cursor.and_then(|i| CONVENIENCE_ITEMS.get(i).copied());

        ui.horizontal(|ui| {
            ui.strong(text::menu_title());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_cross_button(ui).on_hover_text(text::close_hover()).clicked() {
                    close_clicked = true;
                }
            });
        });

        for (n, item) in CONVENIENCE_ITEMS.into_iter().enumerate() {
            ui.separator();
            let (name, desc) = match item {
                ConvenienceItem::FolderBook => (text::name_folder_book(), text::desc_folder_book()),
                ConvenienceItem::Scoring => (text::name_scoring(), text::desc_scoring()),
                ConvenienceItem::ToolBox => (text::name_toolbox(), text::desc_toolbox()),
            };
            let on = self.convenience_is_on(item);
            let mut clicked: Option<bool> = None;
            let row = ui.horizontal(|ui| {
                ui.strong(name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // right_to_left のため、後に置いた方が左になる（左から ON、OFF）
                    let off_r = sort_choice_button(ui, !on, text::off_label(), OFF_SELECTED);
                    let on_r = sort_choice_button(ui, on, text::on_label(), SORT_CHOICE_SELECTED);
                    if on_r.clicked() { clicked = Some(true); }
                    if off_r.clicked() { clicked = Some(false); }
                });
            });
            if cur == Some(item) {
                draw_cursor_ring(ui, row.response.rect.expand(2.0));
            }
            ui.add(egui::Label::new(egui::RichText::new(desc).weak()).wrap());
            if let Some(v) = clicked {
                self.set_convenience(item, v);
                self.convenience_menu_cursor = self.convenience_menu_cursor.map(|_| n);
            }
        }
        close_clicked
    }

    /// 便利機能メニュー内のキー操作（メニュー内カーソルがあるときだけ呼ぶ）。
    /// ↑↓=3項目を1列として移動（先頭で↑ならメニューを閉じる。末尾で↓は何も起きない）、
    /// ←→=メニューを閉じてバー側カーソルをその方向へ動かす、Enter=その項目のON/OFFを切り替える。
    pub(super) fn handle_convenience_menu_keys(&mut self, ctx: &egui::Context) {
        let km = &self.config.keymap;
        let (up, down, left, right, enter) = ctx.input(|i| (
            km.explorer_binding(crate::keymap::ExplorerAction::NavUp).key_pressed(i),
            km.explorer_binding(crate::keymap::ExplorerAction::NavDown).key_pressed(i),
            km.explorer_binding(crate::keymap::ExplorerAction::NavLeft).key_pressed(i),
            km.explorer_binding(crate::keymap::ExplorerAction::NavRight).key_pressed(i),
            km.explorer_binding(crate::keymap::ExplorerAction::Confirm).key_pressed(i),
        ));
        if !(up || down || left || right || enter) {
            return;
        }
        if left || right {
            self.close_convenience_menu();
            self.step_menu_cursor(right);
            return;
        }
        let mut idx = self.convenience_menu_cursor.unwrap_or(0).min(CONVENIENCE_ITEMS.len() - 1);
        if up {
            if idx == 0 {
                self.close_convenience_menu();
                return;
            }
            idx -= 1;
        }
        if down && idx + 1 < CONVENIENCE_ITEMS.len() {
            idx += 1;
        }
        if enter {
            let item = CONVENIENCE_ITEMS[idx];
            let on = self.convenience_is_on(item);
            self.set_convenience(item, !on);
        }
        self.convenience_menu_cursor = Some(idx);
    }
}

/// 閉じるボタン。「✕」の字形がフォントに無く豆腐になるため、2本の斜線で描く（28px四方）。
fn close_cross_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        if hovered {
            ui.painter().rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill);
        }
        let color = if hovered { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
        let c = rect.center();
        let d = 6.0;
        let stroke = egui::Stroke::new(1.6, color);
        ui.painter().line_segment([c + egui::vec2(-d, -d), c + egui::vec2(d, d)], stroke);
        ui.painter().line_segment([c + egui::vec2(-d, d), c + egui::vec2(d, -d)], stroke);
    }
    resp
}
