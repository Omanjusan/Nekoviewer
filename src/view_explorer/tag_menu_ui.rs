//! タグパネルの入口「🏷 タグ管理」と、その子のポップアップメニュー。
//!
//! 子の項目: 現フォルダに対するタグ抽出用学習／現フォルダファイルのタグ自動抽出＆付与／
//! 大ルール／タグ・カテゴリ編集（従来のタグ管理画面）。件数は項目の右にグレーで出す。

use crate::i18n;

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

    /// タグパネル最上段の入口ボタン（左右中央）。押すと子のポップアップメニューが開く。
    pub(super) fn draw_tag_entry_menu(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            // 別窓・処理を起動する項目は、押したら自分で閉じる。
            let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
            egui::containers::menu::MenuButton::new(i18n::t().tag_panel_manager_button()).config(config).ui(ui, |ui| self.draw_tag_entry_menu_items(ui));
        });
    }

    fn draw_tag_entry_menu_items(&mut self, ui: &mut egui::Ui) {
        let t = i18n::t();
        ui.set_min_width(280.0);
        let learned = self.tag_learned.len();

        // 1: 現フォルダに対するタグ抽出用学習
        let resp = ui
            .add(egui::Button::new(t.tag_menu_learn_item()).right_text(egui::RichText::new(t.tag_learn_status(learned)).weak()))
            .on_hover_text(t.tag_learn_status_hover());
        if resp.clicked() {
            self.open_tag_learn_window();
            ui.close();
        }

        // 2: 現フォルダファイルのタグ自動抽出＆付与
        let enabled = self.can_start_tag_scan();
        let resp = ui.add_enabled(enabled, egui::Button::new(t.tag_menu_scan_item()));
        let hint = if self.tag_trial_active { t.tag_scan_trial_hint() } else { t.tag_scan_unavailable_hint() };
        let resp = if enabled { resp } else { resp.on_disabled_hover_text(hint) };
        if resp.clicked() {
            self.request_tag_scan();
            ui.close();
        }

        // 3: 大ルール（仮の一覧窓）
        let resp = ui
            .add(egui::Button::new(t.tag_rules_open_button()).right_text(egui::RichText::new(t.tag_rules_count(self.tag_rules.rules.iter().filter(|r| r.enabled).count(), self.tag_rules.rules.len())).weak()))
            .on_hover_text(t.tag_rules_open_hover());
        if resp.clicked() {
            self.tag_rules_open = !self.tag_rules_open;
            ui.close();
        }

        ui.separator();

        // 4: タグ・カテゴリ編集（従来のタグ管理画面）
        if ui.button(t.tag_menu_edit_item()).clicked() {
            self.open_tag_manager_overlay();
            ui.close();
        }
    }
}
