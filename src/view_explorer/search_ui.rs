use crate::i18n;

use super::panels::draw_cursor_ring;
use super::{FocusPane, NekoviewApp, SearchFormState};

impl NekoviewApp {
    /// 左ペイン「検索」タブの中身。Phase1時点では結果リストの表示のみ
    /// （検索フォーム・検索実行はPhase2/3で追加）。
    pub(super) fn draw_search_pane(&mut self, ui: &mut egui::Ui) {
        if self.search_history.is_empty() {
            ui.add_space(8.0);
            ui.weak(i18n::t().search_no_results_hint());
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("search_result_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
                // search_history は新しい実行が先頭 = 上から下へそのまま並べれば
                // 「検索された順で最上位に追加され、下に送られていく」表示になる。
                for idx in 0..self.search_history.len() {
                    let label = self.search_history[idx].label.clone();
                    let is_cursor = self.focused_pane == FocusPane::SearchTab
                        && !self.search_at_tab
                        && self.search_selected == Some(idx);
                    let resp = ui.selectable_label(self.search_selected == Some(idx), &label);
                    if is_cursor {
                        draw_cursor_ring(ui, resp.rect);
                    }
                    if resp.clicked() {
                        self.focused_pane = FocusPane::SearchTab;
                        self.search_at_tab = false;
                        self.search_selected = Some(idx);
                    }
                }
            });
    }

    /// アイテムペイン左側の「検索ペイン」。最上位に検索開始/条件クリアボタン、
    /// その直下にスプリッター線、それ以下に検索条件フォームを並べる。
    /// 検索開始の実処理（再帰スキャン＋DB照会）はPhase3で接続する。
    pub(super) fn draw_search_condition_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let start_enabled = !self.search_running;
            if ui.add_enabled(start_enabled, egui::Button::new(i18n::t().search_start_button())).clicked() {
                // Phase3で再帰スキャン＋DB照会を接続する。
            }
            if ui.button(i18n::t().search_clear_button()).clicked() {
                self.search_form = SearchFormState::default();
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("search_condition_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(i18n::t().search_name_pattern_label());
                let r = ui.text_edit_singleline(&mut self.search_form.name_pattern);
                if r.has_focus() { self.focused_pane = FocusPane::SearchTab; }
                ui.checkbox(&mut self.search_form.include_subdirs, i18n::t().search_include_subdirs_label());

                ui.add_space(4.0);
                ui.label(i18n::t().search_size_min_label());
                let r = ui.text_edit_singleline(&mut self.search_form.size_min_mb);
                if r.has_focus() { self.focused_pane = FocusPane::SearchTab; }
                ui.label(i18n::t().search_size_max_label());
                let r = ui.text_edit_singleline(&mut self.search_form.size_max_mb);
                if r.has_focus() { self.focused_pane = FocusPane::SearchTab; }

                ui.add_space(4.0);
                ui.label(i18n::t().search_date_after_label());
                let r = ui.text_edit_singleline(&mut self.search_form.date_after);
                if r.has_focus() { self.focused_pane = FocusPane::SearchTab; }
                ui.label(i18n::t().search_date_before_label());
                let r = ui.text_edit_singleline(&mut self.search_form.date_before);
                if r.has_focus() { self.focused_pane = FocusPane::SearchTab; }
            });
    }
}
