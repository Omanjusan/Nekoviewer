//! 検索日付用のカレンダーGUI部品。
//!
//! `CalendarModel` は egui やOS時計に依存しない。本日は呼び出し側が
//! `open` に注入するため、ヘッドレステストを決定的に実行できる。

pub(crate) const CALENDAR_BUTTON_GLYPH: &str = "📅";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl LocalDate {
    pub(crate) fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        if month == 0 || month > 12 || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    fn valid(year: i32, month: u8, day: u8) -> Self {
        Self::new(year, month, day).expect("calendar creates only valid dates")
    }

    pub(crate) fn to_yyyy_mm_dd(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub(crate) fn parse_yyyy_mm_dd(value: &str) -> Option<Self> {
        let mut parts = value.split('-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Self::new(year, month, day)
    }

    pub(crate) fn today_local() -> Self {
        let now =
            time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
        Self::valid(now.year(), u8::from(now.month()), now.day())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CalendarOutcome {
    None,
    Confirmed(LocalDate),
    Cleared,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CalendarModel {
    cursor: LocalDate,
}

impl CalendarModel {
    /// 選択済み日があればその日、未指定なら注入されたローカル本日を初期値にする。
    pub(crate) fn open(today: LocalDate, selected: Option<LocalDate>) -> Self {
        Self {
            cursor: selected.unwrap_or(today),
        }
    }

    pub(crate) fn confirm(self) -> CalendarOutcome {
        CalendarOutcome::Confirmed(self.cursor)
    }

    pub(crate) fn clear(self) -> CalendarOutcome {
        CalendarOutcome::Cleared
    }

    pub(crate) fn cancel(self) -> CalendarOutcome {
        CalendarOutcome::Cancelled
    }

    pub(crate) fn select_day(&mut self, day: u8) -> bool {
        let Some(date) = LocalDate::new(self.cursor.year, self.cursor.month, day) else {
            return false;
        };
        self.cursor = date;
        true
    }

    pub(crate) fn previous_month(&mut self) {
        self.move_month(-1);
    }

    pub(crate) fn next_month(&mut self) {
        self.move_month(1);
    }

    fn move_month(&mut self, delta: i32) {
        let absolute = self.cursor.year * 12 + i32::from(self.cursor.month) - 1 + delta;
        let year = absolute.div_euclid(12);
        let month = (absolute.rem_euclid(12) + 1) as u8;
        let day = self.cursor.day.min(days_in_month(year, month));
        self.cursor = LocalDate::valid(year, month, day);
    }

    /// 日曜日=0 … 土曜日=6。
    pub(crate) fn first_weekday(self) -> u8 {
        weekday_sunday_zero(self.cursor.year, self.cursor.month, 1)
    }

    pub(crate) fn month_days(self) -> u8 {
        days_in_month(self.cursor.year, self.cursor.month)
    }
}

pub(crate) struct CalendarGui {
    open: bool,
    model: CalendarModel,
}

impl CalendarGui {
    pub(crate) fn new(today: LocalDate) -> Self {
        Self {
            open: false,
            model: CalendarModel::open(today, None),
        }
    }

    /// 日付ボタンとカレンダー窓を描画する。ローカル本日は呼び出し側が渡す。
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    fn open_for_selection(&mut self, today: LocalDate, selected: Option<LocalDate>) {
        self.model = CalendarModel::open(today, selected);
        self.open = true;
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        selected: Option<LocalDate>,
        today: LocalDate,
    ) -> (egui::Response, CalendarOutcome) {
        let button = ui.button(CALENDAR_BUTTON_GLYPH);
        if button.clicked() {
            self.open_for_selection(today, selected);
        }
        if !self.open {
            return (button, CalendarOutcome::None);
        }

        let mut window_open = self.open;
        let mut outcome = CalendarOutcome::None;
        egui::Window::new("日付を選択")
            .id(id.with("calendar_window"))
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui.button("◀").clicked() {
                        self.model.previous_month();
                    }
                    ui.label(format!(
                        "{}年{}月",
                        self.model.cursor.year, self.model.cursor.month
                    ));
                    if ui.button("▶").clicked() {
                        self.model.next_month();
                    }
                });

                egui::Grid::new(id.with("calendar_grid"))
                    .num_columns(7)
                    .show(ui, |ui| {
                        for label in ["日", "月", "火", "水", "木", "金", "土"] {
                            ui.label(label);
                        }
                        ui.end_row();

                        let leading = self.model.first_weekday();
                        let days = self.model.month_days();
                        for cell in 0..42_u8 {
                            if cell < leading || cell >= leading + days {
                                ui.label("");
                            } else {
                                let day = cell - leading + 1;
                                if ui
                                    .selectable_label(day == self.model.cursor.day, day.to_string())
                                    .clicked()
                                {
                                    self.model.select_day(day);
                                }
                            }
                            if cell % 7 == 6 {
                                ui.end_row();
                            }
                        }
                    });

                ui.horizontal(|ui| {
                    if ui.button("決定").clicked() {
                        outcome = self.model.confirm();
                    }
                    if ui.button("未指定に戻す").clicked() {
                        outcome = self.model.clear();
                    }
                    if ui.button("キャンセル").clicked() {
                        outcome = self.model.cancel();
                    }
                });
            });

        if outcome != CalendarOutcome::None {
            window_open = false;
        } else if !window_open {
            outcome = CalendarOutcome::Cancelled;
        }
        self.open = window_open;
        (button, outcome)
    }
}

pub(crate) fn apply_outcome_to_form(value: &mut String, outcome: CalendarOutcome) {
    match outcome {
        CalendarOutcome::Confirmed(date) => *value = date.to_yyyy_mm_dd(),
        CalendarOutcome::Cleared => value.clear(),
        CalendarOutcome::None | CalendarOutcome::Cancelled => {}
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn weekday_sunday_zero(year: i32, month: u8, day: u8) -> u8 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 {
        i32::from(month) - 3
    } else {
        i32::from(month) + 9
    };
    let doy = (153 * mp + 2) / 5 + i32::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;
    (days_since_epoch + 4).rem_euclid(7) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u8, day: u8) -> LocalDate {
        LocalDate::new(year, month, day).unwrap()
    }

    #[test]
    fn open_then_confirm_without_changes_returns_injected_local_today() {
        let today = date(2026, 8, 24);
        let calendar = CalendarModel::open(today, None);
        assert_eq!(calendar.confirm(), CalendarOutcome::Confirmed(today));
        assert_eq!(today.to_yyyy_mm_dd(), "2026-08-24");
    }

    #[test]
    fn selected_date_takes_priority_over_today_when_opened() {
        let selected = date(2025, 12, 31);
        let calendar = CalendarModel::open(date(2026, 8, 24), Some(selected));
        assert_eq!(calendar.cursor, selected);
    }

    #[test]
    fn rejects_nonexistent_dates_and_handles_leap_years() {
        assert_eq!(LocalDate::new(2025, 2, 29), None);
        assert_eq!(LocalDate::new(2024, 2, 29), Some(date(2024, 2, 29)));
        assert_eq!(LocalDate::new(2026, 4, 31), None);
    }

    #[test]
    fn month_navigation_crosses_year_and_clamps_day() {
        let mut calendar = CalendarModel::open(date(2024, 1, 31), None);
        calendar.next_month();
        assert_eq!(calendar.cursor, date(2024, 2, 29));
        calendar = CalendarModel::open(date(2026, 1, 15), None);
        calendar.previous_month();
        assert_eq!(calendar.cursor, date(2025, 12, 15));
    }

    #[test]
    fn weekday_layout_uses_sunday_as_first_column() {
        let calendar = CalendarModel::open(date(2026, 8, 24), None);
        assert_eq!(calendar.first_weekday(), 6); // 2026-08-01 is Saturday
        assert_eq!(calendar.month_days(), 31);
    }

    #[test]
    fn clear_and_cancel_have_distinct_outcomes() {
        let calendar = CalendarModel::open(date(2026, 8, 24), None);
        assert_eq!(calendar.clear(), CalendarOutcome::Cleared);
        assert_eq!(calendar.cancel(), CalendarOutcome::Cancelled);
    }

    #[test]
    fn parses_existing_form_string_and_rejects_invalid_text() {
        assert_eq!(
            LocalDate::parse_yyyy_mm_dd("2026-08-24"),
            Some(date(2026, 8, 24))
        );
        assert_eq!(LocalDate::parse_yyyy_mm_dd("2026-02-31"), None);
        assert_eq!(LocalDate::parse_yyyy_mm_dd("２０２６-08-24"), None);
    }

    #[test]
    fn start_and_end_outcomes_update_only_their_own_form_strings() {
        let mut start = String::new();
        let mut end = String::new();
        apply_outcome_to_form(&mut start, CalendarOutcome::Confirmed(date(2026, 8, 1)));
        assert_eq!(start, "2026-08-01");
        assert!(end.is_empty());
        apply_outcome_to_form(&mut end, CalendarOutcome::Confirmed(date(2026, 8, 31)));
        assert_eq!(start, "2026-08-01");
        assert_eq!(end, "2026-08-31");
    }

    #[test]
    fn independent_start_and_end_buttons_open_with_their_own_initial_dates() {
        let today = date(2026, 8, 24);
        let mut start = CalendarGui::new(today);
        let mut end = CalendarGui::new(today);
        start.open_for_selection(today, Some(date(2026, 8, 1)));
        end.open_for_selection(today, None);
        assert_eq!(
            start.model.confirm(),
            CalendarOutcome::Confirmed(date(2026, 8, 1))
        );
        assert_eq!(end.model.confirm(), CalendarOutcome::Confirmed(today));
    }
}
