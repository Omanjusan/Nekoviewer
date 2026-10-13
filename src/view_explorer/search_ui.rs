use crate::i18n;

use super::help::help_tip;

use super::panels::draw_cursor_ring;
use super::calendar_gui::{apply_outcome_to_form, LocalDate};
use super::{FocusPane, NekoviewApp, SearchFormFocus, SearchFormState};

impl NekoviewApp {
    /// 左ペイン「検索」タブの中身。上＝検索条件フォーム、下＝検索結果履歴（ウィンドウ縦幅の20%で下寄せ固定）。
    /// [Phase A] レイアウトのモック確認用。ツリー/ドライブの基点ディレクトリ連携（Phase B）は未接続。
    pub(super) fn draw_search_left_pane(&mut self, ui: &mut egui::Ui) {
        // 検索結果はウィンドウ縦幅の一定比率で下寄せに固定し、残りを検索条件に回す。
        const RESULT_H_RATIO: f32 = 0.20;
        // 区切り線（separator）が取る分の余裕
        const SEPARATOR_H: f32 = 8.0;
        let total_h = ui.available_height();
        let result_h = (ui.ctx().content_rect().height() * RESULT_H_RATIO).min(total_h * 0.5);
        let condition_h = (total_h - result_h - SEPARATOR_H).max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), condition_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| self.draw_search_condition_pane(ui),
        );
        ui.separator();
        self.draw_search_pane(ui);
    }

    /// 検索結果の履歴リスト（draw_search_left_pane の下段）。
    fn draw_search_pane(&mut self, ui: &mut egui::Ui) {
        let help_on = self.help_enabled;
        let help_history = i18n::t().help_search_history();
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
                    let is_cursor = self.focused_pane == FocusPane::SearchHistory
                        && self.search_selected == Some(idx);
                    let resp = ui.selectable_label(self.search_selected == Some(idx), &label);
                    help_tip(&resp, help_on, &help_history);
                    if is_cursor {
                        draw_cursor_ring(ui, resp.rect);
                    }
                    if resp.clicked() {
                        self.focused_pane = FocusPane::SearchHistory;
                        self.select_search_history(idx);
                    }
                }
            });
    }

    /// アイテムペイン左側の「検索ペイン」。最上位に検索開始/条件クリアボタン、
    /// その直下にスプリッター線、それ以下に検索条件フォームを並べる。
    pub(super) fn draw_search_condition_pane(&mut self, ui: &mut egui::Ui) {
        let help_on = self.help_enabled;
        let help_base = i18n::t().help_search_base_dir();
        let help_actions = i18n::t().help_search_actions();
        let help_cond = i18n::t().help_search_conditions();
        ui.horizontal(|ui| {
            let start_enabled = !self.search_running;
            let start = ui.add_enabled(start_enabled, egui::Button::new(i18n::t().search_start_button()));
            self.sync_search_form_response(&start, SearchFormFocus::Start, start_enabled);
            help_tip(&start, help_on, &help_actions);
            if start.clicked() {
                let ctx = ui.ctx().clone();
                self.start_search(&ctx);
            }
            let clear = ui.button(i18n::t().search_clear_button());
            self.sync_search_form_response(&clear, SearchFormFocus::Clear, true);
            help_tip(&clear, help_on, &help_actions);
            if clear.clicked() {
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
                let r = ui.label(i18n::t().search_base_dir_label());
                help_tip(&r, help_on, &help_base);
                let mut base_dir_display = self.search_form.base_dir.as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                // 長いパスは折り返して縦に伸ばす（複数行 TextEdit は内容に合わせて高さが伸びる）
                let r = ui.add_enabled(
                    false,
                    egui::TextEdit::multiline(&mut base_dir_display)
                        .desired_rows(1)
                        .desired_width(f32::INFINITY),
                );
                help_tip(&r, help_on, &help_base);

                ui.add_space(4.0);
                let r = ui.label(i18n::t().search_name_pattern_label());
                help_tip(&r, help_on, &help_cond);
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.name_pattern).lock_focus(true));
                self.sync_search_form_response(&r, SearchFormFocus::NamePattern, true);
                help_tip(&r, help_on, &help_cond);
                let r = ui.checkbox(&mut self.search_form.include_subdirs, i18n::t().search_include_subdirs_label());
                self.sync_search_form_response(&r, SearchFormFocus::IncludeSubdirs, true);
                help_tip(&r, help_on, &help_cond);

                ui.add_space(4.0);
                let r = ui.label(i18n::t().search_size_min_label());
                help_tip(&r, help_on, &help_cond);
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.size_min_mb).lock_focus(true));
                self.sync_search_form_response(&r, SearchFormFocus::SizeMin, true);
                help_tip(&r, help_on, &help_cond);
                let r = ui.label(i18n::t().search_size_max_label());
                help_tip(&r, help_on, &help_cond);
                let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.size_max_mb).lock_focus(true));
                self.sync_search_form_response(&r, SearchFormFocus::SizeMax, true);
                help_tip(&r, help_on, &help_cond);

                ui.add_space(4.0);
                let r = ui.label(i18n::t().search_date_after_label());
                help_tip(&r, help_on, &help_cond);
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(false, egui::TextEdit::singleline(&mut self.search_form.date_after).desired_width(110.0));
                    help_tip(&r, help_on, &help_cond);
                    let selected = LocalDate::parse_yyyy_mm_dd(&self.search_form.date_after);
                    let (r, outcome) = self.search_date_start_calendar.show(
                        ui, egui::Id::new("search_date_start"), selected, self.search_calendar_today);
                    self.sync_search_form_response(&r, SearchFormFocus::DateAfter, true);
                    help_tip(&r, help_on, &help_cond);
                    apply_outcome_to_form(&mut self.search_form.date_after, outcome);
                });
                let r = ui.label(i18n::t().search_date_before_label());
                help_tip(&r, help_on, &help_cond);
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(false, egui::TextEdit::singleline(&mut self.search_form.date_before).desired_width(110.0));
                    help_tip(&r, help_on, &help_cond);
                    let selected = LocalDate::parse_yyyy_mm_dd(&self.search_form.date_before);
                    let (r, outcome) = self.search_date_end_calendar.show(
                        ui, egui::Id::new("search_date_end"), selected, self.search_calendar_today);
                    self.sync_search_form_response(&r, SearchFormFocus::DateBefore, true);
                    help_tip(&r, help_on, &help_cond);
                    apply_outcome_to_form(&mut self.search_form.date_before, outcome);
                });
            });
    }

    fn sync_search_form_response(&mut self, r: &egui::Response, field: SearchFormFocus, enabled: bool) {
        if self.focused_pane == FocusPane::SearchForm && self.search_form_focus == field
            && self.search_form_focus_request && enabled {
            r.request_focus();
            self.search_form_focus_request = false;
        }
        // キーボードの着地先は SearchFormFocus から request_focus で一方向に同期する。
        // gained_focus を逆方向の同期に使うと、チェックボックからのTabでegui標準巡回と
        // 独自巡回が同じフレームに二重で進み、SizeMinを飛ばしてしまう。
        // マウス操作の独自位置同期に必要な clicked だけを残す。
        if r.clicked() {
            self.focused_pane = FocusPane::SearchForm;
            self.search_form_focus = field;
        }
    }
}
