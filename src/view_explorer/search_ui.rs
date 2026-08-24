use crate::i18n;

use super::panels::draw_cursor_ring;
use super::{FocusPane, NekoviewApp, SearchFormState};

impl NekoviewApp {
    /// 左ペイン「検索」タブの中身。上＝検索条件フォーム（固定高さ）、下＝検索結果履歴。
    /// [Phase A] レイアウトのモック確認用。ツリー/ドライブの基点ディレクトリ連携（Phase B）は未接続。
    pub(super) fn draw_search_left_pane(&mut self, ui: &mut egui::Ui) {
        const CONDITION_H: f32 = 285.0;
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), CONDITION_H),
            egui::Layout::top_down(egui::Align::Min),
            |ui| self.draw_search_condition_pane(ui),
        );
        ui.separator();
        self.draw_search_pane(ui);
    }

    /// 検索結果の履歴リスト（draw_search_left_pane の下段）。
    fn draw_search_pane(&mut self, ui: &mut egui::Ui) {
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
                        && self.search_selected == Some(idx);
                    let resp = ui.selectable_label(self.search_selected == Some(idx), &label);
                    if is_cursor {
                        draw_cursor_ring(ui, resp.rect);
                    }
                    if resp.clicked() {
                        self.focused_pane = FocusPane::SearchTab;
                        self.enter_search_view(idx);
                    }
                }
            });
    }

    /// アイテムペイン左側の「検索ペイン」。最上位に検索開始/条件クリアボタン、
    /// その直下にスプリッター線、それ以下に検索条件フォームを並べる。
    pub(super) fn draw_search_condition_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let start_enabled = !self.search_running;
            if ui.add_enabled(start_enabled, egui::Button::new(i18n::t().search_start_button())).clicked() {
                let ctx = ui.ctx().clone();
                self.start_search(&ctx);
            }
            if ui.button(i18n::t().search_clear_button()).clicked() {
                // 基点ディレクトリはツリー/ドライブで選ぶものなので、条件クリアの対象外にする。
                let base_dir = self.search_form.base_dir.clone();
                self.search_form = SearchFormState { base_dir, ..SearchFormState::default() };
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("search_condition_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(i18n::t().search_base_dir_label());
                let mut base_dir_display = self.search_form.base_dir.as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                ui.add_enabled(false, egui::TextEdit::singleline(&mut base_dir_display));

                ui.add_space(4.0);
                ui.label(i18n::t().search_name_pattern_label());
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.name_pattern).lock_focus(true));
                if r.gained_focus() { self.focused_pane = FocusPane::SearchTab; }
                // Tab巡回でSearchTabに着地した直後は、まだどのテキスト欄にもegui側の
                // ネイティブフォーカスが無く画面上の変化が一切見えない（Filter欄と違いここまで
                // 何もしていなかった）。最初の項目（ファイル名）へ自動的にフォーカスを送ることで、
                // Grid同様に「着地したのに何も動いていないように見える」ちらつきを解消する。
                if self.focused_pane == FocusPane::SearchTab
                    && !r.has_focus()
                    && ui.ctx().memory(|m| m.focused()).is_none()
                {
                    r.request_focus();
                }
                ui.checkbox(&mut self.search_form.include_subdirs, i18n::t().search_include_subdirs_label());

                ui.add_space(4.0);
                ui.label(i18n::t().search_size_min_label());
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.size_min_mb).lock_focus(true));
                if r.gained_focus() { self.focused_pane = FocusPane::SearchTab; }
                ui.label(i18n::t().search_size_max_label());
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.size_max_mb).lock_focus(true));
                if r.gained_focus() { self.focused_pane = FocusPane::SearchTab; }

                ui.add_space(4.0);
                ui.label(i18n::t().search_date_after_label());
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.date_after).lock_focus(true));
                if r.gained_focus() { self.focused_pane = FocusPane::SearchTab; }
                ui.label(i18n::t().search_date_before_label());
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.date_before).lock_focus(true));
                if r.gained_focus() { self.focused_pane = FocusPane::SearchTab; }
            });
    }
}
