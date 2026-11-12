//! タグ管理画面の「危険な設定」窓。取り返しのつきにくい操作（タグのクリア・リカバリポイントの設定）をまとめる。
//!
//! - ボタンはタイトルのロールバックボタンの右隣。窓の外をクリックしたとき・もう一度ボタンを押したときに閉じる
//! - 試用中（一括適用の仮マスターを編集中）は押せない
//! - 中身（クリアとリカバリポイント）は D2・D3 で実装する。いまは枠と、押せない状態のボタンだけ

use super::*;
use crate::i18n;

impl NekoviewApp {
    /// 窓を開く。開いていれば閉じる。
    pub(super) fn toggle_tag_danger_window(&mut self) {
        self.tag_danger_open = !self.tag_danger_open;
    }

    /// 「危険な設定」窓を描く。窓の外を押したら閉じる（ボタン自体の押下はボタン側で切り替える）。
    pub(super) fn draw_tag_danger_window(&mut self, ctx: &egui::Context) {
        if !self.tag_danger_open {
            return;
        }
        let t = i18n::t();
        let mut open = true;
        let shown = egui::Window::new(egui::RichText::new(t.tag_danger_title()).color(egui::Color32::from_rgb(230, 90, 80)))
            .id(egui::Id::new("tag_danger_window"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                // タグのクリア（D2で実装）。
                ui.label(egui::RichText::new(t.tag_danger_clear_heading()).strong());
                ui.label(t.tag_danger_clear_explain());
                ui.add_enabled(false, egui::Button::new(t.tag_danger_clear_button()))
                    .on_disabled_hover_text(t.tag_danger_coming_soon());
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                // リカバリポイント（D3で実装）。
                ui.label(egui::RichText::new(t.tag_danger_recovery_heading()).strong());
                ui.label(t.tag_danger_recovery_explain());
                ui.add_enabled(false, egui::Button::new(t.tag_danger_recovery_set_button()))
                    .on_disabled_hover_text(t.tag_danger_coming_soon());
            });
        if !open {
            self.tag_danger_open = false;
            return;
        }
        if ctx.input(|i| i.pointer.primary_pressed()) {
            let win_rect = shown.map(|r| r.response.rect);
            let pos = ctx.input(|i| i.pointer.interact_pos());
            let inside = pos.is_some_and(|p| win_rect.is_some_and(|r| r.contains(p)) || self.tag_danger_btn_rect.contains(p));
            if !inside {
                self.tag_danger_open = false;
            }
        }
    }
}
