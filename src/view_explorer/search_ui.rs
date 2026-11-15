use crate::i18n;

use super::help::help_tip;

use super::panels::draw_cursor_ring;
use super::calendar_gui::{apply_outcome_to_form, LocalDate};
use super::{FocusPane, FolderPaneTab, NekoviewApp, SearchFormFocus, SearchMode};

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

    /// 新モードでは基点フォルダを使わないので、検索タブのツリー/ドライブは隠す。
    pub(super) fn search_tree_hidden(&self) -> bool {
        self.folder_pane_tab == FolderPaneTab::Search && self.search_form.mode == SearchMode::New
    }

    /// 検索モードを切り替える。入力欄は新旧で独立なので、互いの入力は消さない。
    /// キーボードの着地先は切替後のモードの先頭欄へ戻し、隠れたツリー/ドライブにフォーカスが残らないようにする。
    pub(super) fn set_search_mode(&mut self, mode: SearchMode) {
        if self.search_form.mode == mode {
            return;
        }
        self.search_form.mode = mode;
        self.search_form_focus = mode.first_field();
        if mode == SearchMode::New && matches!(self.focused_pane, FocusPane::TreeTab | FocusPane::Drives) {
            self.focused_pane = FocusPane::SearchForm;
        }
        if self.focused_pane == FocusPane::SearchForm {
            self.search_form_focus_request = true;
        }
    }

    /// 検索タブ左ペインの条件部。最上段にモード切替（新／旧）、その下に条件フォーム、
    /// フォームの直下にスプリッターと検索開始/条件クリアのボタンを置く。ボタンは新旧共通。
    pub(super) fn draw_search_condition_pane(&mut self, ui: &mut egui::Ui) {
        let help_on = self.help_enabled;
        let help_mode = i18n::t().help_search_mode();
        let help_actions = i18n::t().help_search_actions();

        ui.horizontal(|ui| {
            for (mode, label) in [
                (SearchMode::New, i18n::t().search_mode_new()),
                (SearchMode::Legacy, i18n::t().search_mode_legacy()),
            ] {
                let r = ui.selectable_label(self.search_form.mode == mode, label);
                help_tip(&r, help_on, &help_mode);
                if r.clicked() {
                    self.set_search_mode(mode);
                }
            }
        });
        ui.separator();

        // ボタンは条件フォームの直下に置く（フォームの高さに合わせて上下に動く）。フォームが高すぎて
        // 収まらないときだけ、ボタン行の分を残してフォームをスクロールにする。
        let bottom_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 3.0 + 8.0;
        let form_h = (ui.available_height() - bottom_h).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("search_condition_scroll")
            .max_height(form_h)
            .auto_shrink([false, true])
            .show(ui, |ui| match self.search_form.mode {
                SearchMode::New => self.draw_search_new_fields(ui),
                SearchMode::Legacy => self.draw_search_legacy_fields(ui),
            });

        ui.separator();
        ui.horizontal(|ui| {
            // 新モードの検索中は「中断」に変わる（押すと打ち切る。それまでの結果は残る）。
            // 検索は旧・新を通して同時に1つだけなので、どちらかの実行中は開始できない。
            let mode = self.search_form.mode;
            let cancelling = mode == SearchMode::New && self.search_job.is_some();
            let (label, enabled) = if cancelling {
                (i18n::t().search_cancel_button(), true)
            } else {
                let can_start = !self.any_search_running()
                    && (mode == SearchMode::Legacy || self.spread_db.is_some())
                    && super::search_history_ui::search_condition_is_valid(&self.search_form);
                (i18n::t().search_start_button(), can_start)
            };
            let start = ui.add_enabled(enabled, egui::Button::new(label));
            // 条件が空で押せないときは、その理由を出す。
            let start = if !enabled && !self.any_search_running() && !super::search_history_ui::search_condition_is_valid(&self.search_form) {
                start.on_disabled_hover_text(i18n::t().search_condition_required())
            } else {
                start
            };
            self.sync_search_form_response(&start, SearchFormFocus::Start, enabled);
            help_tip(&start, help_on, &help_actions);
            if start.clicked() {
                if cancelling {
                    self.cancel_new_search();
                } else if mode == SearchMode::New {
                    self.start_new_search(None);
                } else {
                    let ctx = ui.ctx().clone();
                    self.start_search(&ctx, None);
                }
            }
            let clear = ui.button(i18n::t().search_clear_button());
            self.sync_search_form_response(&clear, SearchFormFocus::Clear, true);
            help_tip(&clear, help_on, &help_actions);
            if clear.clicked() {
                self.clear_search_conditions();
            }
        });
    }

    /// 今のモードの入力欄だけを空に戻す。基点ディレクトリはツリー/ドライブで選ぶものなので対象外。
    /// もう一方のモードの入力は残す（新旧は独立）。
    fn clear_search_conditions(&mut self) {
        let f = &mut self.search_form;
        match f.mode {
            SearchMode::New => {
                f.new_text.clear();
                f.new_size_min_mb.clear();
                f.new_size_max_mb.clear();
            }
            SearchMode::Legacy => {
                f.name_pattern.clear();
                f.include_subdirs = false;
                f.size_min_mb.clear();
                f.size_max_mb.clear();
                f.date_after.clear();
                f.date_before.clear();
            }
        }
    }

    /// 新モードの条件欄: 検索文字列とサイズ。日付は今は無い（後から足せるよう、欄ごとに独立させてある）。
    fn draw_search_new_fields(&mut self, ui: &mut egui::Ui) {
        let help_on = self.help_enabled;
        let help_cond = i18n::t().help_search_new_conditions();
        let r = ui.label(i18n::t().search_new_text_label());
        help_tip(&r, help_on, &help_cond);
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.search_form.new_text)
                .hint_text(i18n::t().search_new_text_hint())
                .desired_width(f32::INFINITY)
                .lock_focus(true),
        )
        .on_hover_text(i18n::t().search_new_text_tooltip());
        self.sync_search_form_response(&r, SearchFormFocus::NewText, true);
        help_tip(&r, help_on, &help_cond);

        ui.add_space(4.0);
        let r = ui.label(i18n::t().search_size_min_label());
        help_tip(&r, help_on, &help_cond);
        let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.new_size_min_mb).lock_focus(true));
        self.sync_search_form_response(&r, SearchFormFocus::NewSizeMin, true);
        help_tip(&r, help_on, &help_cond);
        let r = ui.label(i18n::t().search_size_max_label());
        help_tip(&r, help_on, &help_cond);
        let r = ui.add(egui::TextEdit::singleline(&mut self.search_form.new_size_max_mb).lock_focus(true));
        self.sync_search_form_response(&r, SearchFormFocus::NewSizeMax, true);
        help_tip(&r, help_on, &help_cond);

        // フォームの最後: 注意点（マーカーの扱いなど）を出すボタン。
        ui.add_space(8.0);
        if ui.button(i18n::t().search_notice_button()).clicked() {
            self.search_notice_open = true;
        }
    }

    /// 新モードの「注意点」ダイアログ。右上の×だけで閉じる。検索タブの新モードを離れたら閉じる。
    pub(super) fn draw_search_notice_dialog(&mut self, ctx: &egui::Context) {
        if !self.search_notice_open {
            return;
        }
        if !(self.folder_pane_tab == FolderPaneTab::Search && self.search_form.mode == SearchMode::New) {
            self.search_notice_open = false;
            return;
        }
        let t = i18n::t();
        let mut open = true;
        egui::Window::new(t.search_notice_title())
            .id(egui::Id::new("search_notice_dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.add(egui::Label::new(t.search_notice_body()).wrap());
            });
        self.search_notice_open = open;
    }

    /// 旧モードの条件欄（従来どおり）。基底フォルダ・ファイル名・サブディレクトリ・サイズ・日付。
    fn draw_search_legacy_fields(&mut self, ui: &mut egui::Ui) {
        let help_on = self.help_enabled;
        let help_base = i18n::t().help_search_base_dir();
        let help_cond = i18n::t().help_search_conditions();
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
