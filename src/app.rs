use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use time::{Month, Time, Weekday};

use crate::{
    agenda::{
        AgendaSource, CreateEventDraft, CreateEventTiming, DayAgenda, EventDateTime, Reminder,
    },
    calendar::{CalendarDate, CalendarMonth, DAYS_PER_WEEK},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Month,
    Day,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    selected_date: CalendarDate,
    today: CalendarDate,
    view_mode: ViewMode,
    create_form: Option<CreateEventForm>,
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
            create_form: None,
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

    pub const fn create_form(&self) -> Option<&CreateEventForm> {
        self.create_form.as_ref()
    }

    pub const fn is_creating_event(&self) -> bool {
        self.create_form.is_some()
    }

    pub fn close_create_form(&mut self) {
        self.create_form = None;
    }

    pub fn set_create_form_error(&mut self, message: impl Into<String>) {
        if let Some(form) = &mut self.create_form {
            form.error = Some(message.into());
        }
    }

    pub fn handle_create_key(&mut self, key: KeyEvent) -> CreateEventInputResult {
        let Some(form) = &mut self.create_form else {
            return CreateEventInputResult::Continue;
        };

        form.handle_key(key)
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
            AppAction::OpenCreate => {
                if self.create_form.is_none() {
                    let context = match self.view_mode {
                        ViewMode::Month => CreateEventContext::EditableDate,
                        ViewMode::Day => CreateEventContext::FixedDate,
                    };
                    self.create_form = Some(CreateEventForm::new(self.selected_date, context));
                }
            }
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
    OpenCreate,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEventContext {
    EditableDate,
    FixedDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventForm {
    context: CreateEventContext,
    selected_date: CalendarDate,
    title: String,
    all_day: bool,
    start_date: String,
    start_time: String,
    end_date: String,
    end_time: String,
    location: String,
    notes: String,
    reminders: [bool; REMINDER_PRESETS.len()],
    focused: usize,
    error: Option<String>,
}

impl CreateEventForm {
    pub fn new(selected_date: CalendarDate, context: CreateEventContext) -> Self {
        Self {
            context,
            selected_date,
            title: String::new(),
            all_day: false,
            start_date: selected_date.to_string(),
            start_time: "09:00".to_string(),
            end_date: selected_date.to_string(),
            end_time: "10:00".to_string(),
            location: String::new(),
            notes: String::new(),
            reminders: [false; REMINDER_PRESETS.len()],
            focused: 0,
            error: None,
        }
    }

    pub const fn context(&self) -> CreateEventContext {
        self.context
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn rows(&self) -> Vec<CreateEventFormRow> {
        self.visible_fields()
            .into_iter()
            .enumerate()
            .map(|(index, field)| CreateEventFormRow {
                label: field.label(),
                value: self.field_value(field),
                focused: index == self.focused,
                kind: field.kind(),
            })
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CreateEventInputResult {
        if key.kind == KeyEventKind::Release {
            return CreateEventInputResult::Continue;
        }

        if ctrl_s(key) {
            return match self.submit() {
                Ok(draft) => CreateEventInputResult::Submit(draft),
                Err(err) => {
                    self.error = Some(err.to_string());
                    CreateEventInputResult::Continue
                }
            };
        }

        match key.code {
            KeyCode::Esc => CreateEventInputResult::Cancel,
            KeyCode::Tab => {
                self.focus_next();
                CreateEventInputResult::Continue
            }
            KeyCode::BackTab => {
                self.focus_previous();
                CreateEventInputResult::Continue
            }
            KeyCode::Backspace => {
                self.edit_text_field(|value| {
                    value.pop();
                });
                CreateEventInputResult::Continue
            }
            KeyCode::Enter => {
                self.activate_focused_field();
                CreateEventInputResult::Continue
            }
            KeyCode::Char(value)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.edit_text_field(|field| field.push(value));
                CreateEventInputResult::Continue
            }
            _ => CreateEventInputResult::Continue,
        }
    }

    pub fn submit(&self) -> Result<CreateEventDraft, CreateEventFormError> {
        let title = normalize_required(&self.title, "title")?;
        let location = normalize_optional(&self.location);
        let notes = normalize_optional(&self.notes);
        let reminders = self
            .reminders
            .iter()
            .zip(REMINDER_PRESETS)
            .filter_map(|(enabled, preset)| {
                enabled.then_some(Reminder::minutes_before(preset.minutes))
            })
            .collect::<Vec<_>>();

        if self.all_day {
            let date = self.start_date()?;
            return Ok(CreateEventDraft {
                title,
                timing: CreateEventTiming::AllDay { date },
                location,
                notes,
                reminders,
            });
        }

        let start_date = self.start_date()?;
        let start_time = parse_time_field(&self.start_time, "start time")?;
        let end_time = parse_time_field(&self.end_time, "end time")?;
        let end_date = match self.context {
            CreateEventContext::EditableDate => self.end_date()?,
            CreateEventContext::FixedDate if end_time <= start_time => start_date.add_days(1),
            CreateEventContext::FixedDate => start_date,
        };
        let start = EventDateTime::new(start_date, start_time);
        let end = EventDateTime::new(end_date, end_time);
        if start >= end {
            return Err(CreateEventFormError::InvalidRange);
        }

        Ok(CreateEventDraft {
            title,
            timing: CreateEventTiming::Timed { start, end },
            location,
            notes,
            reminders,
        })
    }

    fn start_date(&self) -> Result<CalendarDate, CreateEventFormError> {
        match self.context {
            CreateEventContext::EditableDate => parse_date_field(&self.start_date, "start date"),
            CreateEventContext::FixedDate => Ok(self.selected_date),
        }
    }

    fn end_date(&self) -> Result<CalendarDate, CreateEventFormError> {
        parse_date_field(&self.end_date, "end date")
    }

    fn visible_fields(&self) -> Vec<CreateEventField> {
        let mut fields = vec![CreateEventField::Title, CreateEventField::AllDay];
        if self.context == CreateEventContext::EditableDate {
            fields.push(CreateEventField::StartDate);
        }
        fields.push(CreateEventField::StartTime);
        if self.context == CreateEventContext::EditableDate {
            fields.push(CreateEventField::EndDate);
        }
        fields.extend([
            CreateEventField::EndTime,
            CreateEventField::Location,
            CreateEventField::Notes,
        ]);
        fields.extend((0..REMINDER_PRESETS.len()).map(CreateEventField::Reminder));
        fields
    }

    fn field_value(&self, field: CreateEventField) -> String {
        match field {
            CreateEventField::Title => self.title.clone(),
            CreateEventField::AllDay => checkbox(self.all_day).to_string(),
            CreateEventField::StartDate => self.start_date.clone(),
            CreateEventField::StartTime => self.start_time.clone(),
            CreateEventField::EndDate => self.end_date.clone(),
            CreateEventField::EndTime => self.end_time.clone(),
            CreateEventField::Location => self.location.clone(),
            CreateEventField::Notes => self.notes.replace('\n', " / "),
            CreateEventField::Reminder(index) => {
                let preset = REMINDER_PRESETS[index];
                format!("{} {}", checkbox(self.reminders[index]), preset.label)
            }
        }
    }

    fn focus_next(&mut self) {
        let field_count = self.visible_fields().len();
        self.focused = (self.focused + 1) % field_count;
        self.error = None;
    }

    fn focus_previous(&mut self) {
        let field_count = self.visible_fields().len();
        self.focused = if self.focused == 0 {
            field_count - 1
        } else {
            self.focused - 1
        };
        self.error = None;
    }

    fn focused_field(&self) -> CreateEventField {
        self.visible_fields()[self.focused]
    }

    fn activate_focused_field(&mut self) {
        match self.focused_field() {
            CreateEventField::AllDay => self.all_day = !self.all_day,
            CreateEventField::Reminder(index) => self.reminders[index] = !self.reminders[index],
            CreateEventField::Notes => self.notes.push('\n'),
            _ => {}
        }
        self.error = None;
    }

    fn edit_text_field(&mut self, edit: impl FnOnce(&mut String)) {
        let field = self.focused_field();
        let target = match field {
            CreateEventField::Title => Some(&mut self.title),
            CreateEventField::StartDate => Some(&mut self.start_date),
            CreateEventField::StartTime => Some(&mut self.start_time),
            CreateEventField::EndDate => Some(&mut self.end_date),
            CreateEventField::EndTime => Some(&mut self.end_time),
            CreateEventField::Location => Some(&mut self.location),
            CreateEventField::Notes => Some(&mut self.notes),
            CreateEventField::AllDay | CreateEventField::Reminder(_) => None,
        };

        if let Some(target) = target {
            edit(target);
            self.error = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventFormRow {
    pub label: &'static str,
    pub value: String,
    pub focused: bool,
    pub kind: CreateEventFormRowKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEventFormRowKind {
    Text,
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEventInputResult {
    Continue,
    Cancel,
    Submit(CreateEventDraft),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEventFormError {
    RequiredField(&'static str),
    InvalidDate { field: &'static str, value: String },
    InvalidTime { field: &'static str, value: String },
    InvalidRange,
}

impl std::fmt::Display for CreateEventFormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiredField(field) => write!(f, "{field} is required"),
            Self::InvalidDate { field, value } => write!(f, "{field} '{value}' must be YYYY-MM-DD"),
            Self::InvalidTime { field, value } => write!(f, "{field} '{value}' must be HH:MM"),
            Self::InvalidRange => write!(f, "end must be after start"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReminderPreset {
    label: &'static str,
    minutes: u16,
}

const REMINDER_PRESETS: [ReminderPreset; 6] = [
    ReminderPreset {
        label: "5m",
        minutes: 5,
    },
    ReminderPreset {
        label: "10m",
        minutes: 10,
    },
    ReminderPreset {
        label: "15m",
        minutes: 15,
    },
    ReminderPreset {
        label: "30m",
        minutes: 30,
    },
    ReminderPreset {
        label: "1h",
        minutes: 60,
    },
    ReminderPreset {
        label: "1d",
        minutes: 24 * 60,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateEventField {
    Title,
    AllDay,
    StartDate,
    StartTime,
    EndDate,
    EndTime,
    Location,
    Notes,
    Reminder(usize),
}

impl CreateEventField {
    const fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::AllDay => "All day",
            Self::StartDate => "Start date",
            Self::StartTime => "Start time",
            Self::EndDate => "End date",
            Self::EndTime => "End time",
            Self::Location => "Location",
            Self::Notes => "Notes",
            Self::Reminder(_) => "Reminder",
        }
    }

    const fn kind(self) -> CreateEventFormRowKind {
        match self {
            Self::AllDay | Self::Reminder(_) => CreateEventFormRowKind::Toggle,
            _ => CreateEventFormRowKind::Text,
        }
    }
}

fn checkbox(enabled: bool) -> &'static str {
    if enabled { "[x]" } else { "[ ]" }
}

fn normalize_required(value: &str, field: &'static str) -> Result<String, CreateEventFormError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(CreateEventFormError::RequiredField(field))
    } else {
        Ok(trimmed.to_string())
    }
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_date_field(
    value: &str,
    field: &'static str,
) -> Result<CalendarDate, CreateEventFormError> {
    let mut parts = value.trim().split('-');
    let year = parts.next().and_then(|value| value.parse::<i32>().ok());
    let month = parts.next().and_then(|value| value.parse::<u8>().ok());
    let day = parts.next().and_then(|value| value.parse::<u8>().ok());
    if parts.next().is_some() {
        return Err(CreateEventFormError::InvalidDate {
            field,
            value: value.to_string(),
        });
    }

    let Some((year, month, day)) = year
        .zip(month)
        .zip(day)
        .map(|((year, month), day)| (year, month, day))
    else {
        return Err(CreateEventFormError::InvalidDate {
            field,
            value: value.to_string(),
        });
    };

    CalendarDate::from_ymd(
        year,
        Month::try_from(month).map_err(|_| CreateEventFormError::InvalidDate {
            field,
            value: value.to_string(),
        })?,
        day,
    )
    .map_err(|_| CreateEventFormError::InvalidDate {
        field,
        value: value.to_string(),
    })
}

fn parse_time_field(value: &str, field: &'static str) -> Result<Time, CreateEventFormError> {
    let mut parts = value.trim().split(':');
    let hour = parts.next().and_then(|value| value.parse::<u8>().ok());
    let minute = parts.next().and_then(|value| value.parse::<u8>().ok());
    if parts.next().is_some() {
        return Err(CreateEventFormError::InvalidTime {
            field,
            value: value.to_string(),
        });
    }

    let Some((hour, minute)) = hour.zip(minute) else {
        return Err(CreateEventFormError::InvalidTime {
            field,
            value: value.to_string(),
        });
    };

    Time::from_hms(hour, minute, 0).map_err(|_| CreateEventFormError::InvalidTime {
        field,
        value: value.to_string(),
    })
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
        if value == '+' {
            self.clear();
            return AppAction::OpenCreate;
        }

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

fn ctrl_s(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(value) if value.eq_ignore_ascii_case(&'s'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
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

    fn ctrl_char_key(value: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(value), KeyModifiers::CONTROL)
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
    fn plus_opens_create_form_with_contextual_dates() {
        let day = date(2026, Month::April, 23);
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        app.apply(input.translate(char_key('+')));

        assert_eq!(
            app.create_form().expect("form opens").context(),
            CreateEventContext::EditableDate
        );

        app.close_create_form();
        app.apply(AppAction::OpenDay);
        app.apply(input.translate(char_key('+')));

        assert_eq!(
            app.create_form().expect("form opens").context(),
            CreateEventContext::FixedDate
        );
    }

    #[test]
    fn create_form_text_input_does_not_move_selection() {
        let day = date(2026, Month::April, 23);
        let mut app = AppState::new(day);
        app.apply(AppAction::OpenCreate);

        assert_eq!(
            app.handle_create_key(char_key('1')),
            CreateEventInputResult::Continue
        );

        assert_eq!(app.selected_date(), day);
        assert_eq!(
            app.create_form().expect("form stays open").rows()[0].value,
            "1"
        );
    }

    #[test]
    fn create_form_validates_required_title() {
        let mut form = CreateEventForm::new(
            date(2026, Month::April, 23),
            CreateEventContext::EditableDate,
        );

        let result = form.handle_key(ctrl_char_key('s'));

        assert_eq!(result, CreateEventInputResult::Continue);
        assert_eq!(form.error(), Some("title is required"));
    }

    #[test]
    fn create_form_submits_day_view_cross_midnight_event() {
        let day = date(2026, Month::April, 23);
        let mut form = CreateEventForm::new(day, CreateEventContext::FixedDate);
        form.title = "Late work".to_string();
        form.start_time = "23:00".to_string();
        form.end_time = "01:00".to_string();
        form.location = "Terminal".to_string();
        form.notes = "Keep an eye on deploy".to_string();
        form.reminders[1] = true;
        form.reminders[4] = true;

        let draft = form.submit().expect("form submits");

        assert_eq!(draft.title, "Late work");
        assert_eq!(draft.location.as_deref(), Some("Terminal"));
        assert_eq!(draft.notes.as_deref(), Some("Keep an eye on deploy"));
        assert_eq!(
            draft
                .reminders
                .iter()
                .map(|reminder| reminder.minutes_before)
                .collect::<Vec<_>>(),
            [10, 60]
        );
        assert_eq!(
            draft.timing,
            CreateEventTiming::Timed {
                start: EventDateTime::new(day, Time::from_hms(23, 0, 0).expect("valid time")),
                end: EventDateTime::new(
                    day.add_days(1),
                    Time::from_hms(1, 0, 0).expect("valid time")
                ),
            }
        );
    }

    #[test]
    fn create_form_submits_all_day_event() {
        let day = date(2026, Month::April, 23);
        let mut form = CreateEventForm::new(day, CreateEventContext::EditableDate);
        form.title = "Conference".to_string();
        form.all_day = true;
        form.reminders[5] = true;

        let draft = form.submit().expect("form submits");

        assert_eq!(draft.timing, CreateEventTiming::AllDay { date: day });
        assert_eq!(draft.reminders[0].minutes_before, 24 * 60);
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
