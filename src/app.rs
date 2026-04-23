use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use time::Weekday;

use crate::{
    agenda::{AgendaSource, DayAgenda},
    calendar::{CalendarDate, CalendarMonth, DAYS_PER_WEEK},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Month,
    Day,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppState {
    selected_date: CalendarDate,
    today: CalendarDate,
    view_mode: ViewMode,
    should_quit: bool,
}

impl AppState {
    pub const fn new(start_date: CalendarDate) -> Self {
        Self::from_dates(start_date, start_date)
    }

    pub const fn from_dates(selected_date: CalendarDate, today: CalendarDate) -> Self {
        Self {
            selected_date,
            today,
            view_mode: ViewMode::Month,
            should_quit: false,
        }
    }

    pub const fn selected_date(&self) -> CalendarDate {
        self.selected_date
    }

    pub const fn today(&self) -> CalendarDate {
        self.today
    }

    pub const fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn calendar_month(&self) -> CalendarMonth {
        CalendarMonth::from_dates(self.selected_date, self.today)
    }

    pub fn day_agenda<S>(&self, source: &S) -> DayAgenda
    where
        S: AgendaSource + ?Sized,
    {
        DayAgenda::from_source(self.selected_date, source)
    }

    pub fn apply(&mut self, action: AppAction) {
        match action {
            AppAction::Noop => {}
            AppAction::Quit => self.should_quit = true,
            AppAction::OpenDay => self.view_mode = ViewMode::Day,
            AppAction::CloseDay => self.view_mode = ViewMode::Month,
            AppAction::MoveDays(days) if self.view_mode == ViewMode::Month => {
                self.selected_date = self.selected_date.add_days(days);
            }
            AppAction::MoveDays(days)
                if self.view_mode == ViewMode::Day && matches!(days, -1 | 1) =>
            {
                self.selected_date = self.selected_date.add_days(days);
            }
            AppAction::SelectDate(date) if self.view_mode == ViewMode::Month => {
                self.selected_date = date;
            }
            AppAction::JumpToDay(day) if self.view_mode == ViewMode::Month => {
                if let Some(date) = self.calendar_month().current.date(day) {
                    self.selected_date = date;
                }
            }
            AppAction::JumpToWeekday(weekday) if self.view_mode == ViewMode::Month => {
                if let Some(date) = self.weekday_in_selected_week(weekday) {
                    self.selected_date = date;
                }
            }
            AppAction::MoveDays(_)
            | AppAction::SelectDate(_)
            | AppAction::JumpToDay(_)
            | AppAction::JumpToWeekday(_) => {}
        }
    }

    fn weekday_in_selected_week(&self, weekday: Weekday) -> Option<CalendarDate> {
        let month = self.calendar_month();
        let selected = month.selected_cell()?;
        let weekday_index = usize::from(weekday.number_days_from_sunday());

        if weekday_index >= DAYS_PER_WEEK {
            return None;
        }

        Some(month.weeks[selected.week_index].cells[weekday_index].date)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    Noop,
    MoveDays(i32),
    SelectDate(CalendarDate),
    JumpToDay(u8),
    JumpToWeekday(Weekday),
    OpenDay,
    CloseDay,
    Quit,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardInput {
    pending: PendingKey,
}

impl KeyboardInput {
    pub fn translate(&mut self, key: KeyEvent) -> AppAction {
        if key.kind == KeyEventKind::Release {
            return AppAction::Noop;
        }

        match key.code {
            KeyCode::Left => {
                self.clear();
                AppAction::MoveDays(-1)
            }
            KeyCode::Right => {
                self.clear();
                AppAction::MoveDays(1)
            }
            KeyCode::Up => {
                self.clear();
                AppAction::MoveDays(-7)
            }
            KeyCode::Down => {
                self.clear();
                AppAction::MoveDays(7)
            }
            KeyCode::Enter => {
                self.clear();
                AppAction::OpenDay
            }
            KeyCode::Esc => {
                self.clear();
                AppAction::CloseDay
            }
            KeyCode::Char(value) if ctrl_c(value, key.modifiers) => {
                self.clear();
                AppAction::Quit
            }
            KeyCode::Char(_) if key.modifiers.intersects(KeyModifiers::ALT) => {
                self.clear();
                AppAction::Noop
            }
            KeyCode::Char(value) => self.translate_char(value),
            _ => {
                self.clear();
                AppAction::Noop
            }
        }
    }

    pub fn clear(&mut self) {
        self.pending = PendingKey::None;
    }

    pub const fn is_waiting_for_digit(&self) -> bool {
        matches!(self.pending, PendingKey::Digit(_))
    }

    pub fn clear_digit(&mut self) {
        if self.is_waiting_for_digit() {
            self.clear();
        }
    }

    fn translate_char(&mut self, value: char) -> AppAction {
        if value.is_ascii_digit() {
            return self.translate_digit(value);
        }

        let value = value.to_ascii_lowercase();

        match self.pending {
            PendingKey::Digit(_) => {
                self.clear();
                self.translate_weekday_start(value)
            }
            PendingKey::T => {
                self.clear();
                match value {
                    'u' => AppAction::JumpToWeekday(Weekday::Tuesday),
                    'h' => AppAction::JumpToWeekday(Weekday::Thursday),
                    _ => self.translate_weekday_start(value),
                }
            }
            PendingKey::S => {
                self.clear();
                match value {
                    'a' => AppAction::JumpToWeekday(Weekday::Saturday),
                    'u' => AppAction::JumpToWeekday(Weekday::Sunday),
                    _ => self.translate_weekday_start(value),
                }
            }
            PendingKey::None => self.translate_weekday_start(value),
        }
    }

    fn translate_digit(&mut self, value: char) -> AppAction {
        let digit = value
            .to_digit(10)
            .expect("ASCII digit converts to a base-10 digit") as u8;

        match self.pending {
            PendingKey::Digit(first) => {
                self.clear();
                AppAction::JumpToDay(first * 10 + digit)
            }
            _ if digit == 0 => {
                self.pending = PendingKey::Digit(digit);
                AppAction::Noop
            }
            _ if digit <= 3 => {
                self.pending = PendingKey::Digit(digit);
                AppAction::JumpToDay(digit)
            }
            _ => {
                self.clear();
                AppAction::JumpToDay(digit)
            }
        }
    }

    fn translate_weekday_start(&mut self, value: char) -> AppAction {
        match value {
            'm' => AppAction::JumpToWeekday(Weekday::Monday),
            'w' => AppAction::JumpToWeekday(Weekday::Wednesday),
            'f' => AppAction::JumpToWeekday(Weekday::Friday),
            't' => {
                self.pending = PendingKey::T;
                AppAction::Noop
            }
            's' => {
                self.pending = PendingKey::S;
                AppAction::Noop
            }
            'q' => AppAction::Quit,
            _ => AppAction::Noop,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pending_open_date: Option<CalendarDate>,
}

impl MouseInput {
    pub fn translate(
        &mut self,
        mouse: MouseEvent,
        target_date: Option<CalendarDate>,
        selected_date: CalendarDate,
    ) -> AppAction {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(target_date) = target_date else {
                    self.clear();
                    return AppAction::Noop;
                };

                if self.pending_open_date == Some(target_date) && selected_date == target_date {
                    self.clear();
                    AppAction::OpenDay
                } else {
                    self.pending_open_date = Some(target_date);
                    AppAction::SelectDate(target_date)
                }
            }
            _ => {
                self.clear();
                AppAction::Noop
            }
        }
    }

    pub fn clear(&mut self) {
        self.pending_open_date = None;
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum PendingKey {
    #[default]
    None,
    Digit(u8),
    T,
    S,
}

fn ctrl_c(value: char, modifiers: KeyModifiers) -> bool {
    value.eq_ignore_ascii_case(&'c') && modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    use time::Month;

    use crate::agenda::{Holiday, InMemoryAgendaSource, SourceMetadata};

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn char_key(value: char) -> KeyEvent {
        key(KeyCode::Char(value))
    }

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn mouse_down(column: u16, row: u16) -> MouseEvent {
        mouse_event(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn apply_keys(
        app: &mut AppState,
        input: &mut KeyboardInput,
        keys: impl IntoIterator<Item = KeyEvent>,
    ) {
        for key in keys {
            let action = input.translate(key);
            app.apply(action);
        }
    }

    #[test]
    fn arrow_keys_move_within_month() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        apply_keys(
            &mut app,
            &mut input,
            [
                key(KeyCode::Left),
                key(KeyCode::Right),
                key(KeyCode::Up),
                key(KeyCode::Down),
            ],
        );

        assert_eq!(app.selected_date(), date(2026, Month::April, 23));
    }

    #[test]
    fn arrow_keys_cross_month_and_year_boundaries() {
        let mut app = AppState::new(date(2026, Month::December, 31));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [key(KeyCode::Right)]);
        assert_eq!(app.selected_date(), date(2027, Month::January, 1));

        apply_keys(&mut app, &mut input, [key(KeyCode::Left)]);
        assert_eq!(app.selected_date(), date(2026, Month::December, 31));

        app = AppState::new(date(2026, Month::January, 1));
        apply_keys(&mut app, &mut input, [key(KeyCode::Left)]);
        assert_eq!(app.selected_date(), date(2025, Month::December, 31));
    }

    #[test]
    fn numeric_jump_selects_valid_days() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        let action = input.translate(char_key('1'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 1));
        assert!(input.is_waiting_for_digit());

        let action = input.translate(char_key('8'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 18));
        assert!(!input.is_waiting_for_digit());

        apply_keys(&mut app, &mut input, [char_key('0'), char_key('4')]);
        assert_eq!(app.selected_date(), date(2026, Month::April, 4));

        apply_keys(&mut app, &mut input, [char_key('9')]);
        assert_eq!(app.selected_date(), date(2026, Month::April, 9));
    }

    #[test]
    fn single_digit_jumps_cover_visible_day_digits() {
        for day in 1..=9 {
            let mut app = AppState::new(date(2026, Month::April, 23));
            let mut input = KeyboardInput::default();
            let digit = char::from_digit(day.into(), 10).expect("single digit");

            let action = input.translate(char_key(digit));
            app.apply(action);

            assert_eq!(app.selected_date(), date(2026, Month::April, day));
        }
    }

    #[test]
    fn zero_prefix_can_still_jump_to_single_digit_days() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        let action = input.translate(char_key('0'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 23));
        assert!(input.is_waiting_for_digit());

        let action = input.translate(char_key('7'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 7));
    }

    #[test]
    fn numeric_jump_timeout_keeps_single_digit_selection() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        let action = input.translate(char_key('1'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 1));

        input.clear_digit();

        let action = input.translate(char_key('6'));
        app.apply(action);
        assert_eq!(app.selected_date(), date(2026, Month::April, 6));
    }

    #[test]
    fn invalid_numeric_jump_leaves_selection_unchanged_and_clears_buffer() {
        let mut app = AppState::new(date(2026, Month::February, 10));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [char_key('3'), char_key('0')]);
        assert_eq!(app.selected_date(), date(2026, Month::February, 3));

        apply_keys(&mut app, &mut input, [char_key('1'), char_key('5')]);
        assert_eq!(app.selected_date(), date(2026, Month::February, 15));
    }

    #[test]
    fn weekday_jumps_target_selected_week_from_each_position() {
        let jump_cases = [
            ([char_key('S'), char_key('u')], 19),
            ([char_key('M'), char_key(' ')], 20),
            ([char_key('T'), char_key('u')], 21),
            ([char_key('W'), char_key(' ')], 22),
            ([char_key('T'), char_key('h')], 23),
            ([char_key('F'), char_key(' ')], 24),
            ([char_key('S'), char_key('a')], 25),
        ];

        for start_day in 19..=25 {
            for (keys, expected_day) in jump_cases {
                let mut app = AppState::new(date(2026, Month::April, start_day));
                let mut input = KeyboardInput::default();

                apply_keys(&mut app, &mut input, keys);

                assert_eq!(
                    app.selected_date(),
                    date(2026, Month::April, expected_day),
                    "start day {start_day} should jump to weekday date {expected_day}"
                );
            }
        }
    }

    #[test]
    fn enter_opens_day_view_and_escape_returns_to_month() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [key(KeyCode::Enter)]);
        assert_eq!(app.view_mode(), ViewMode::Day);
        assert_eq!(app.selected_date(), date(2026, Month::April, 23));

        apply_keys(&mut app, &mut input, [key(KeyCode::Esc)]);
        assert_eq!(app.view_mode(), ViewMode::Month);
        assert_eq!(app.selected_date(), date(2026, Month::April, 23));
    }

    #[test]
    fn day_view_left_and_right_move_between_days() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [key(KeyCode::Enter)]);
        apply_keys(&mut app, &mut input, [key(KeyCode::Left)]);

        assert_eq!(app.view_mode(), ViewMode::Day);
        assert_eq!(app.selected_date(), date(2026, Month::April, 22));

        apply_keys(
            &mut app,
            &mut input,
            [key(KeyCode::Right), key(KeyCode::Right)],
        );

        assert_eq!(app.view_mode(), ViewMode::Day);
        assert_eq!(app.selected_date(), date(2026, Month::April, 24));
    }

    #[test]
    fn day_view_up_and_down_do_not_change_days() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [key(KeyCode::Enter)]);
        apply_keys(&mut app, &mut input, [key(KeyCode::Up), key(KeyCode::Down)]);

        assert_eq!(app.view_mode(), ViewMode::Day);
        assert_eq!(app.selected_date(), date(2026, Month::April, 23));
    }

    #[test]
    fn quit_action_marks_app_done() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = KeyboardInput::default();

        apply_keys(&mut app, &mut input, [char_key('q')]);

        assert!(app.should_quit());
    }

    #[test]
    fn select_date_action_can_pick_adjacent_month_cells() {
        let mut app = AppState::new(date(2026, Month::April, 23));

        app.apply(AppAction::SelectDate(date(2026, Month::May, 1)));

        assert_eq!(app.selected_date(), date(2026, Month::May, 1));
    }

    #[test]
    fn mouse_click_selects_then_second_click_opens_day() {
        let target = date(2026, Month::April, 18);
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = MouseInput::default();

        let action = input.translate(mouse_down(10, 10), Some(target), app.selected_date());
        app.apply(action);

        assert_eq!(app.selected_date(), target);
        assert_eq!(app.view_mode(), ViewMode::Month);

        let action = input.translate(mouse_down(10, 10), Some(target), app.selected_date());
        app.apply(action);

        assert_eq!(app.view_mode(), ViewMode::Day);
    }

    #[test]
    fn mouse_clicks_without_a_date_target_are_ignored() {
        let mut input = MouseInput::default();
        let selected = date(2026, Month::April, 23);

        let action = input.translate(mouse_down(0, 0), None, selected);

        assert_eq!(action, AppAction::Noop);
    }

    #[test]
    fn non_left_mouse_actions_clear_pending_open() {
        let target = date(2026, Month::April, 18);
        let mut input = MouseInput::default();

        assert_eq!(
            input.translate(
                mouse_down(10, 10),
                Some(target),
                date(2026, Month::April, 23),
            ),
            AppAction::SelectDate(target)
        );
        assert_eq!(
            input.translate(
                mouse_event(MouseEventKind::Down(MouseButton::Right), 10, 10),
                Some(target),
                target,
            ),
            AppAction::Noop
        );
        assert_eq!(
            input.translate(mouse_down(10, 10), Some(target), target),
            AppAction::SelectDate(target)
        );
    }

    #[test]
    fn selected_day_agenda_uses_selected_date() {
        let app = AppState::from_dates(date(2026, Month::April, 23), date(2026, Month::April, 18));
        let source = InMemoryAgendaSource::with_events_and_holidays(
            Vec::new(),
            vec![
                Holiday::new(
                    "selected",
                    "Selected Day",
                    date(2026, Month::April, 23),
                    SourceMetadata::fixture(),
                ),
                Holiday::new(
                    "today",
                    "Today",
                    date(2026, Month::April, 18),
                    SourceMetadata::fixture(),
                ),
            ],
        );

        let agenda = app.day_agenda(&source);

        assert_eq!(agenda.date, date(2026, Month::April, 23));
        assert_eq!(agenda.holidays.len(), 1);
        assert_eq!(agenda.holidays[0].name, "Selected Day");
    }
}
