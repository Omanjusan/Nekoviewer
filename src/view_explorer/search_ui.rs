use crate::i18n;

use super::panels::draw_cursor_ring;
use super::{FocusPane, NekoviewApp};

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
}
