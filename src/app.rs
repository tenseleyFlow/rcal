use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use time::{Month, Time, Weekday};

use crate::{
    agenda::{
        AgendaSource, CreateEventDraft, CreateEventTiming, DayAgenda, Event, EventDateTime,
        EventTiming, OccurrenceAnchor, RecurrenceEnd, RecurrenceFrequency, RecurrenceMonthlyRule,
        RecurrenceRule, RecurrenceYearlyRule, Reminder, recurrence_ordinal_for_date,
    },
    calendar::{CalendarDate, CalendarMonth, DAYS_PER_WEEK},
};

const MOUSE_DOUBLE_CLICK_TIMEOUT: Duration = Duration::from_millis(500);

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
    recurrence_choice: Option<RecurrenceEditChoice>,
    delete_choice: Option<EventDeleteChoice>,
    help_open: bool,
    selected_day_event_id: Option<String>,
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
            recurrence_choice: None,
            delete_choice: None,
            help_open: false,
            selected_day_event_id: None,
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

    pub const fn recurrence_choice(&self) -> Option<&RecurrenceEditChoice> {
        self.recurrence_choice.as_ref()
    }

    pub const fn delete_choice(&self) -> Option<&EventDeleteChoice> {
        self.delete_choice.as_ref()
    }

    pub fn selected_day_event_id(&self) -> Option<&str> {
        self.selected_day_event_id.as_deref()
    }

    pub const fn is_creating_event(&self) -> bool {
        self.create_form.is_some()
    }

    pub const fn is_choosing_recurring_edit(&self) -> bool {
        self.recurrence_choice.is_some()
    }

    pub const fn is_confirming_delete(&self) -> bool {
        self.delete_choice.is_some()
    }

    pub const fn is_showing_help(&self) -> bool {
        self.help_open
    }

    pub fn close_create_form(&mut self) {
        self.create_form = None;
    }

    pub fn close_recurrence_choice(&mut self) {
        self.recurrence_choice = None;
    }

    pub fn close_delete_choice(&mut self) {
        self.delete_choice = None;
    }

    pub fn close_help(&mut self) {
        self.help_open = false;
    }

    pub fn set_delete_error(&mut self, message: impl Into<String>) {
        if let Some(choice) = &mut self.delete_choice {
            choice.error = Some(message.into());
        }
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

    pub fn handle_recurrence_choice_key(
        &mut self,
        key: KeyEvent,
        source: &dyn AgendaSource,
    ) -> RecurrenceChoiceInputResult {
        if key.kind == KeyEventKind::Release {
            return RecurrenceChoiceInputResult::Continue;
        }

        let Some(choice) = &mut self.recurrence_choice else {
            return RecurrenceChoiceInputResult::Continue;
        };

        match key.code {
            KeyCode::Esc => RecurrenceChoiceInputResult::Cancel,
            KeyCode::Up => {
                choice.select_previous();
                RecurrenceChoiceInputResult::Continue
            }
            KeyCode::Down => {
                choice.select_next();
                RecurrenceChoiceInputResult::Continue
            }
            KeyCode::Enter => match choice.selected_action() {
                RecurrenceEditChoiceAction::ThisOccurrence => {
                    let series_id = choice.series_id.clone();
                    let anchor = choice.anchor;
                    self.recurrence_choice = None;
                    if let Some(event) = selectable_day_events(self.selected_date, source)
                        .into_iter()
                        .find(|event| {
                            event
                                .occurrence()
                                .map(|occurrence| {
                                    occurrence.series_id == series_id && occurrence.anchor == anchor
                                })
                                .unwrap_or(false)
                        })
                    {
                        self.create_form = Some(CreateEventForm::edit_occurrence(&event));
                    }
                    RecurrenceChoiceInputResult::Continue
                }
                RecurrenceEditChoiceAction::Series => {
                    let series_id = choice.series_id.clone();
                    self.recurrence_choice = None;
                    if let Some(event) = source.local_event_by_id(&series_id) {
                        self.create_form = Some(CreateEventForm::edit(&event));
                    }
                    RecurrenceChoiceInputResult::Continue
                }
                RecurrenceEditChoiceAction::Cancel => RecurrenceChoiceInputResult::Cancel,
            },
            _ => RecurrenceChoiceInputResult::Continue,
        }
    }

    pub fn handle_delete_choice_key(&mut self, key: KeyEvent) -> EventDeleteInputResult {
        if key.kind == KeyEventKind::Release {
            return EventDeleteInputResult::Continue;
        }

        let Some(choice) = &mut self.delete_choice else {
            return EventDeleteInputResult::Continue;
        };

        match key.code {
            KeyCode::Esc => EventDeleteInputResult::Cancel,
            KeyCode::Up => {
                choice.select_previous();
                EventDeleteInputResult::Continue
            }
            KeyCode::Down => {
                choice.select_next();
                EventDeleteInputResult::Continue
            }
            KeyCode::Enter => match choice.selected_action() {
                EventDeleteChoiceAction::Cancel => EventDeleteInputResult::Cancel,
                EventDeleteChoiceAction::DeleteEvent => {
                    EventDeleteInputResult::Submit(EventDeleteSubmission::Event {
                        event_id: choice.event_id().to_string(),
                    })
                }
                EventDeleteChoiceAction::DeleteThisOccurrence => {
                    let EventDeleteTarget::Occurrence { series_id, anchor } = &choice.target else {
                        return EventDeleteInputResult::Continue;
                    };
                    EventDeleteInputResult::Submit(EventDeleteSubmission::Occurrence {
                        series_id: series_id.clone(),
                        anchor: *anchor,
                    })
                }
                EventDeleteChoiceAction::DeleteSeries => {
                    let EventDeleteTarget::Occurrence { series_id, .. } = &choice.target else {
                        return EventDeleteInputResult::Continue;
                    };
                    EventDeleteInputResult::Submit(EventDeleteSubmission::Series {
                        series_id: series_id.clone(),
                    })
                }
            },
            _ => EventDeleteInputResult::Continue,
        }
    }

    pub fn handle_help_key(&mut self, key: KeyEvent) -> HelpInputResult {
        if key.kind == KeyEventKind::Release {
            return HelpInputResult::Continue;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => {
                self.close_help();
                HelpInputResult::Close
            }
            KeyCode::Char(value) if value.eq_ignore_ascii_case(&'q') => {
                self.should_quit = true;
                HelpInputResult::Continue
            }
            KeyCode::Char(value) if ctrl_c(value, key.modifiers) => {
                self.should_quit = true;
                HelpInputResult::Continue
            }
            _ => HelpInputResult::Continue,
        }
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

    pub fn reconcile_day_event_selection(&mut self, source: &dyn AgendaSource) {
        if self.view_mode != ViewMode::Day {
            self.selected_day_event_id = None;
            return;
        }

        let events = selectable_day_events(self.selected_date, source);
        if let Some(selected_id) = &self.selected_day_event_id
            && events.iter().any(|event| &event.id == selected_id)
        {
            return;
        }

        self.selected_day_event_id = events.first().map(|event| event.id.clone());
    }

    pub fn apply(&mut self, action: AppAction) {
        self.apply_resolved(action, None);
    }

    pub fn apply_with_agenda_source(&mut self, action: AppAction, source: &dyn AgendaSource) {
        self.apply_resolved(action, Some(source));
    }

    fn apply_resolved(&mut self, action: AppAction, source: Option<&dyn AgendaSource>) {
        match action {
            AppAction::Noop => {}
            AppAction::Quit => self.should_quit = true,
            AppAction::OpenHelp
                if self.create_form.is_none()
                    && self.recurrence_choice.is_none()
                    && self.delete_choice.is_none() =>
            {
                self.help_open = true;
            }
            _ if self.help_open => {}
            AppAction::OpenDay if self.view_mode == ViewMode::Day => {
                if let Some(source) = source {
                    self.open_selected_event_for_edit(source);
                }
            }
            AppAction::OpenDay => {
                self.view_mode = ViewMode::Day;
                if let Some(source) = source {
                    self.reconcile_day_event_selection(source);
                }
            }
            AppAction::CloseDay => {
                self.view_mode = ViewMode::Month;
                self.selected_day_event_id = None;
                self.recurrence_choice = None;
                self.delete_choice = None;
                self.help_open = false;
            }
            AppAction::OpenCreate => {
                if self.create_form.is_none()
                    && self.recurrence_choice.is_none()
                    && self.delete_choice.is_none()
                    && !self.help_open
                {
                    let context = match self.view_mode {
                        ViewMode::Month => CreateEventContext::EditableDate,
                        ViewMode::Day => CreateEventContext::FixedDate,
                    };
                    self.create_form = Some(CreateEventForm::new(self.selected_date, context));
                }
            }
            AppAction::OpenDelete if self.view_mode == ViewMode::Day => {
                if self.create_form.is_none()
                    && self.recurrence_choice.is_none()
                    && self.delete_choice.is_none()
                    && !self.help_open
                    && let Some(source) = source
                {
                    self.open_selected_event_for_delete(source);
                }
            }
            AppAction::MoveDays(days) if self.view_mode == ViewMode::Month => {
                self.selected_date = self.selected_date.add_days(days);
            }
            AppAction::MoveDays(days)
                if self.view_mode == ViewMode::Day && matches!(days, -1 | 1) =>
            {
                self.selected_date = self.selected_date.add_days(days);
                if let Some(source) = source {
                    self.reconcile_day_event_selection(source);
                } else {
                    self.selected_day_event_id = None;
                }
            }
            AppAction::MoveDays(-7) if self.view_mode == ViewMode::Day => {
                if let Some(source) = source {
                    self.move_day_event_selection(source, -1);
                }
            }
            AppAction::MoveDays(7) if self.view_mode == ViewMode::Day => {
                if let Some(source) = source {
                    self.move_day_event_selection(source, 1);
                }
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
            | AppAction::JumpToWeekday(_)
            | AppAction::OpenDelete
            | AppAction::OpenHelp => {}
        }
    }

    fn move_day_event_selection(&mut self, source: &dyn AgendaSource, delta: i32) {
        let events = selectable_day_events(self.selected_date, source);
        if events.is_empty() {
            self.selected_day_event_id = None;
            return;
        }

        let current_index = self
            .selected_day_event_id
            .as_ref()
            .and_then(|id| events.iter().position(|event| &event.id == id))
            .unwrap_or(0);
        let len = events.len();
        let next_index = if delta < 0 {
            (current_index + len - 1) % len
        } else {
            (current_index + 1) % len
        };
        self.selected_day_event_id = Some(events[next_index].id.clone());
    }

    fn open_selected_event_for_edit(&mut self, source: &dyn AgendaSource) {
        self.reconcile_day_event_selection(source);
        let Some(selected_id) = self.selected_day_event_id.as_deref() else {
            return;
        };
        if let Some(event) = selectable_day_events(self.selected_date, source)
            .into_iter()
            .find(|event| event.id == selected_id)
        {
            if let Some(occurrence) = event.occurrence() {
                self.recurrence_choice = Some(RecurrenceEditChoice::new(
                    occurrence.series_id.clone(),
                    occurrence.anchor,
                ));
            } else {
                self.create_form = Some(CreateEventForm::edit(&event));
            }
        }
    }

    fn open_selected_event_for_delete(&mut self, source: &dyn AgendaSource) {
        self.reconcile_day_event_selection(source);
        let Some(selected_id) = self.selected_day_event_id.as_deref() else {
            return;
        };
        if let Some(event) = selectable_day_events(self.selected_date, source)
            .into_iter()
            .find(|event| event.id == selected_id)
        {
            self.delete_choice = Some(EventDeleteChoice::for_event(&event));
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
    OpenDelete,
    OpenHelp,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEventContext {
    EditableDate,
    FixedDate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventFormMode {
    Create,
    Edit {
        event_id: String,
    },
    EditOccurrence {
        series_id: String,
        anchor: OccurrenceAnchor,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceEditChoice {
    series_id: String,
    anchor: OccurrenceAnchor,
    selected: usize,
}

impl RecurrenceEditChoice {
    const OPTIONS: [RecurrenceEditChoiceAction; 3] = [
        RecurrenceEditChoiceAction::ThisOccurrence,
        RecurrenceEditChoiceAction::Series,
        RecurrenceEditChoiceAction::Cancel,
    ];

    fn new(series_id: String, anchor: OccurrenceAnchor) -> Self {
        Self {
            series_id,
            anchor,
            selected: 0,
        }
    }

    pub fn rows(&self) -> Vec<RecurrenceEditChoiceRow> {
        Self::OPTIONS
            .iter()
            .enumerate()
            .map(|(index, action)| RecurrenceEditChoiceRow {
                label: action.label(),
                selected: index == self.selected,
            })
            .collect()
    }

    fn selected_action(&self) -> RecurrenceEditChoiceAction {
        Self::OPTIONS[self.selected]
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1) % Self::OPTIONS.len();
    }

    fn select_previous(&mut self) {
        self.selected = if self.selected == 0 {
            Self::OPTIONS.len() - 1
        } else {
            self.selected - 1
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecurrenceEditChoiceRow {
    pub label: &'static str,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurrenceEditChoiceAction {
    ThisOccurrence,
    Series,
    Cancel,
}

impl RecurrenceEditChoiceAction {
    const fn label(self) -> &'static str {
        match self {
            Self::ThisOccurrence => "Edit this occurrence",
            Self::Series => "Edit series",
            Self::Cancel => "Cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurrenceChoiceInputResult {
    Continue,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDeleteChoice {
    target: EventDeleteTarget,
    selected: usize,
    error: Option<String>,
}

impl EventDeleteChoice {
    fn for_event(event: &Event) -> Self {
        let target = if let Some(occurrence) = event.occurrence() {
            EventDeleteTarget::Occurrence {
                series_id: occurrence.series_id.clone(),
                anchor: occurrence.anchor,
            }
        } else {
            EventDeleteTarget::Event {
                event_id: event.id.clone(),
            }
        };

        Self {
            target,
            selected: 0,
            error: None,
        }
    }

    pub fn heading(&self) -> &'static str {
        "Delete"
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn rows(&self) -> Vec<EventDeleteChoiceRow> {
        self.actions()
            .into_iter()
            .enumerate()
            .map(|(index, action)| EventDeleteChoiceRow {
                label: action.label(),
                selected: index == self.selected,
                dangerous: action.is_dangerous(),
            })
            .collect()
    }

    fn actions(&self) -> Vec<EventDeleteChoiceAction> {
        match self.target {
            EventDeleteTarget::Event { .. } => vec![
                EventDeleteChoiceAction::DeleteEvent,
                EventDeleteChoiceAction::Cancel,
            ],
            EventDeleteTarget::Occurrence { .. } => vec![
                EventDeleteChoiceAction::DeleteThisOccurrence,
                EventDeleteChoiceAction::DeleteSeries,
                EventDeleteChoiceAction::Cancel,
            ],
        }
    }

    fn selected_action(&self) -> EventDeleteChoiceAction {
        self.actions()[self.selected]
    }

    fn select_next(&mut self) {
        let len = self.actions().len();
        self.selected = (self.selected + 1) % len;
        self.error = None;
    }

    fn select_previous(&mut self) {
        let len = self.actions().len();
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
        self.error = None;
    }

    fn event_id(&self) -> &str {
        match &self.target {
            EventDeleteTarget::Event { event_id } => event_id,
            EventDeleteTarget::Occurrence { series_id, .. } => series_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EventDeleteTarget {
    Event {
        event_id: String,
    },
    Occurrence {
        series_id: String,
        anchor: OccurrenceAnchor,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDeleteChoiceRow {
    pub label: &'static str,
    pub selected: bool,
    pub dangerous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventDeleteChoiceAction {
    DeleteEvent,
    DeleteThisOccurrence,
    DeleteSeries,
    Cancel,
}

impl EventDeleteChoiceAction {
    const fn label(self) -> &'static str {
        match self {
            Self::DeleteEvent => "Delete event",
            Self::DeleteThisOccurrence => "Delete this occurrence",
            Self::DeleteSeries => "Delete series",
            Self::Cancel => "Cancel",
        }
    }

    const fn is_dangerous(self) -> bool {
        !matches!(self, Self::Cancel)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDeleteSubmission {
    Event {
        event_id: String,
    },
    Occurrence {
        series_id: String,
        anchor: OccurrenceAnchor,
    },
    Series {
        series_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDeleteInputResult {
    Continue,
    Cancel,
    Submit(EventDeleteSubmission),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpInputResult {
    Continue,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventForm {
    mode: EventFormMode,
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
    repeat: RepeatFrequency,
    recurrence_interval: String,
    weekly_days: [bool; DAYS_PER_WEEK],
    monthly_mode: RecurrenceMonthlyFormMode,
    yearly_mode: RecurrenceYearlyFormMode,
    recurrence_end: RecurrenceEndFormMode,
    recurrence_until_date: String,
    recurrence_count: String,
    focused: usize,
    error: Option<String>,
}

impl CreateEventForm {
    pub fn new(selected_date: CalendarDate, context: CreateEventContext) -> Self {
        Self {
            mode: EventFormMode::Create,
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
            repeat: RepeatFrequency::None,
            recurrence_interval: "1".to_string(),
            weekly_days: weekly_days_for(selected_date.weekday()),
            monthly_mode: RecurrenceMonthlyFormMode::DayOfMonth,
            yearly_mode: RecurrenceYearlyFormMode::Date,
            recurrence_end: RecurrenceEndFormMode::Never,
            recurrence_until_date: selected_date.to_string(),
            recurrence_count: "10".to_string(),
            focused: 0,
            error: None,
        }
    }

    pub fn edit(event: &Event) -> Self {
        let (all_day, start_date, start_time, end_date, end_time, selected_date) =
            match event.timing {
                EventTiming::AllDay { date } => (
                    true,
                    date.to_string(),
                    "09:00".to_string(),
                    date.to_string(),
                    "10:00".to_string(),
                    date,
                ),
                EventTiming::Timed { start, end } => (
                    false,
                    start.date.to_string(),
                    format_time_field(start.time),
                    end.date.to_string(),
                    format_time_field(end.time),
                    start.date,
                ),
            };
        let mut reminders = [false; REMINDER_PRESETS.len()];
        for reminder in &event.reminders {
            if let Some(index) = REMINDER_PRESETS
                .iter()
                .position(|preset| preset.minutes == reminder.minutes_before)
            {
                reminders[index] = true;
            }
        }

        let mut form = Self {
            mode: EventFormMode::Edit {
                event_id: event.id.clone(),
            },
            context: CreateEventContext::EditableDate,
            selected_date,
            title: event.title.clone(),
            all_day,
            start_date,
            start_time,
            end_date,
            end_time,
            location: event.location.clone().unwrap_or_default(),
            notes: event.notes.clone().unwrap_or_default(),
            reminders,
            repeat: RepeatFrequency::None,
            recurrence_interval: "1".to_string(),
            weekly_days: weekly_days_for(selected_date.weekday()),
            monthly_mode: RecurrenceMonthlyFormMode::DayOfMonth,
            yearly_mode: RecurrenceYearlyFormMode::Date,
            recurrence_end: RecurrenceEndFormMode::Never,
            recurrence_until_date: selected_date.to_string(),
            recurrence_count: "10".to_string(),
            focused: 0,
            error: None,
        };
        form.load_recurrence(event.recurrence.as_ref());
        form
    }

    pub fn edit_occurrence(event: &Event) -> Self {
        let Some(occurrence) = event.occurrence() else {
            return Self::edit(event);
        };
        let mut form = Self::edit(event);
        form.mode = EventFormMode::EditOccurrence {
            series_id: occurrence.series_id.clone(),
            anchor: occurrence.anchor,
        };
        form.repeat = RepeatFrequency::None;
        form
    }

    pub fn mode(&self) -> &EventFormMode {
        &self.mode
    }

    pub fn heading(&self) -> &'static str {
        match &self.mode {
            EventFormMode::Create => "Create",
            EventFormMode::Edit { .. } | EventFormMode::EditOccurrence { .. } => "Edit",
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
                Ok(draft) => CreateEventInputResult::Submit(Box::new(EventFormSubmission {
                    mode: self.mode.clone(),
                    draft,
                })),
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
            KeyCode::Up => {
                self.focus_previous();
                CreateEventInputResult::Continue
            }
            KeyCode::Down => {
                self.focus_next();
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
            let recurrence = self.recurrence_rule(date)?;
            return Ok(CreateEventDraft {
                title,
                timing: CreateEventTiming::AllDay { date },
                location,
                notes,
                reminders,
                recurrence,
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
        let recurrence = self.recurrence_rule(start_date)?;

        Ok(CreateEventDraft {
            title,
            timing: CreateEventTiming::Timed { start, end },
            location,
            notes,
            reminders,
            recurrence,
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

    fn load_recurrence(&mut self, recurrence: Option<&RecurrenceRule>) {
        let Some(recurrence) = recurrence else {
            return;
        };
        self.repeat = RepeatFrequency::from_rule(recurrence.frequency);
        self.recurrence_interval = recurrence.interval().to_string();
        if !recurrence.weekdays.is_empty() {
            self.weekly_days = [false; DAYS_PER_WEEK];
            for weekday in &recurrence.weekdays {
                self.weekly_days[usize::from(weekday.number_days_from_sunday())] = true;
            }
        }
        self.recurrence_end = match recurrence.end {
            RecurrenceEnd::Never => RecurrenceEndFormMode::Never,
            RecurrenceEnd::Until(date) => {
                self.recurrence_until_date = date.to_string();
                RecurrenceEndFormMode::Until
            }
            RecurrenceEnd::Count(count) => {
                self.recurrence_count = count.to_string();
                RecurrenceEndFormMode::Count
            }
        };
        if let Some(monthly) = recurrence.monthly {
            self.monthly_mode = match monthly {
                RecurrenceMonthlyRule::DayOfMonth(_) => RecurrenceMonthlyFormMode::DayOfMonth,
                RecurrenceMonthlyRule::WeekdayOrdinal { .. } => {
                    RecurrenceMonthlyFormMode::WeekdayOrdinal
                }
            };
        }
        if let Some(yearly) = recurrence.yearly {
            self.yearly_mode = match yearly {
                RecurrenceYearlyRule::Date { .. } => RecurrenceYearlyFormMode::Date,
                RecurrenceYearlyRule::WeekdayOrdinal { .. } => {
                    RecurrenceYearlyFormMode::WeekdayOrdinal
                }
            };
        }
    }

    fn recurrence_rule(
        &self,
        start_date: CalendarDate,
    ) -> Result<Option<RecurrenceRule>, CreateEventFormError> {
        if matches!(self.mode, EventFormMode::EditOccurrence { .. })
            || self.repeat == RepeatFrequency::None
        {
            return Ok(None);
        }

        let interval = parse_positive_u16(&self.recurrence_interval, "interval")?;
        let end = match self.recurrence_end {
            RecurrenceEndFormMode::Never => RecurrenceEnd::Never,
            RecurrenceEndFormMode::Until => {
                RecurrenceEnd::Until(parse_date_field(&self.recurrence_until_date, "until date")?)
            }
            RecurrenceEndFormMode::Count => {
                RecurrenceEnd::Count(parse_positive_u32(&self.recurrence_count, "count")?)
            }
        };
        let frequency = self.repeat.frequency().expect("repeat is not none");
        let mut rule = RecurrenceRule::new(frequency).with_interval(interval);
        rule.end = end;

        match self.repeat {
            RepeatFrequency::Weekly => {
                rule.weekdays = self
                    .weekly_days
                    .iter()
                    .enumerate()
                    .filter_map(|(index, enabled)| enabled.then_some(weekday_at(index)))
                    .collect();
                if rule.weekdays.is_empty() {
                    rule.weekdays.push(start_date.weekday());
                }
            }
            RepeatFrequency::Monthly => {
                rule.monthly = Some(match self.monthly_mode {
                    RecurrenceMonthlyFormMode::DayOfMonth => {
                        RecurrenceMonthlyRule::DayOfMonth(start_date.day())
                    }
                    RecurrenceMonthlyFormMode::WeekdayOrdinal => {
                        RecurrenceMonthlyRule::WeekdayOrdinal {
                            ordinal: recurrence_ordinal_for_date(start_date),
                            weekday: start_date.weekday(),
                        }
                    }
                });
            }
            RepeatFrequency::Yearly => {
                rule.yearly = Some(match self.yearly_mode {
                    RecurrenceYearlyFormMode::Date => RecurrenceYearlyRule::Date {
                        month: start_date.month(),
                        day: start_date.day(),
                    },
                    RecurrenceYearlyFormMode::WeekdayOrdinal => {
                        RecurrenceYearlyRule::WeekdayOrdinal {
                            month: start_date.month(),
                            ordinal: recurrence_ordinal_for_date(start_date),
                            weekday: start_date.weekday(),
                        }
                    }
                });
            }
            RepeatFrequency::Daily | RepeatFrequency::None => {}
        }

        Ok(Some(rule))
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
        if !matches!(self.mode, EventFormMode::EditOccurrence { .. }) {
            fields.push(CreateEventField::Repeat);
            if self.repeat != RepeatFrequency::None {
                fields.push(CreateEventField::RecurrenceInterval);
                match self.repeat {
                    RepeatFrequency::Weekly => {
                        fields.extend((0..DAYS_PER_WEEK).map(CreateEventField::WeeklyDay));
                    }
                    RepeatFrequency::Monthly => fields.push(CreateEventField::MonthlyMode),
                    RepeatFrequency::Yearly => fields.push(CreateEventField::YearlyMode),
                    RepeatFrequency::Daily | RepeatFrequency::None => {}
                }
                fields.push(CreateEventField::RecurrenceEnd);
                match self.recurrence_end {
                    RecurrenceEndFormMode::Until => fields.push(CreateEventField::UntilDate),
                    RecurrenceEndFormMode::Count => fields.push(CreateEventField::OccurrenceCount),
                    RecurrenceEndFormMode::Never => {}
                }
            }
        }
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
            CreateEventField::Notes => self.notes.clone(),
            CreateEventField::Reminder(index) => {
                let preset = REMINDER_PRESETS[index];
                format!("{} {}", checkbox(self.reminders[index]), preset.label)
            }
            CreateEventField::Repeat => self.repeat.label().to_string(),
            CreateEventField::RecurrenceInterval => self.recurrence_interval.clone(),
            CreateEventField::WeeklyDay(index) => {
                format!(
                    "{} {}",
                    checkbox(self.weekly_days[index]),
                    weekday_short_label(weekday_at(index))
                )
            }
            CreateEventField::MonthlyMode => self.monthly_mode.label().to_string(),
            CreateEventField::YearlyMode => self.yearly_mode.label().to_string(),
            CreateEventField::RecurrenceEnd => self.recurrence_end.label().to_string(),
            CreateEventField::UntilDate => self.recurrence_until_date.clone(),
            CreateEventField::OccurrenceCount => self.recurrence_count.clone(),
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
            CreateEventField::Repeat => self.repeat = self.repeat.next(),
            CreateEventField::WeeklyDay(index) => {
                self.weekly_days[index] = !self.weekly_days[index]
            }
            CreateEventField::MonthlyMode => self.monthly_mode = self.monthly_mode.next(),
            CreateEventField::YearlyMode => self.yearly_mode = self.yearly_mode.next(),
            CreateEventField::RecurrenceEnd => self.recurrence_end = self.recurrence_end.next(),
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
            CreateEventField::RecurrenceInterval => Some(&mut self.recurrence_interval),
            CreateEventField::UntilDate => Some(&mut self.recurrence_until_date),
            CreateEventField::OccurrenceCount => Some(&mut self.recurrence_count),
            CreateEventField::AllDay
            | CreateEventField::Reminder(_)
            | CreateEventField::Repeat
            | CreateEventField::WeeklyDay(_)
            | CreateEventField::MonthlyMode
            | CreateEventField::YearlyMode
            | CreateEventField::RecurrenceEnd => None,
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
    Multiline,
    Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFormSubmission {
    pub mode: EventFormMode,
    pub draft: CreateEventDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEventInputResult {
    Continue,
    Cancel,
    Submit(Box<EventFormSubmission>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEventFormError {
    RequiredField(&'static str),
    InvalidDate { field: &'static str, value: String },
    InvalidTime { field: &'static str, value: String },
    InvalidNumber { field: &'static str, value: String },
    InvalidRange,
}

impl std::fmt::Display for CreateEventFormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiredField(field) => write!(f, "{field} is required"),
            Self::InvalidDate { field, value } => write!(f, "{field} '{value}' must be YYYY-MM-DD"),
            Self::InvalidTime { field, value } => write!(f, "{field} '{value}' must be HH:MM"),
            Self::InvalidNumber { field, value } => {
                write!(f, "{field} '{value}' must be a positive number")
            }
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
enum RepeatFrequency {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl RepeatFrequency {
    const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Yearly => "Yearly",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::None => Self::Daily,
            Self::Daily => Self::Weekly,
            Self::Weekly => Self::Monthly,
            Self::Monthly => Self::Yearly,
            Self::Yearly => Self::None,
        }
    }

    const fn frequency(self) -> Option<RecurrenceFrequency> {
        match self {
            Self::None => None,
            Self::Daily => Some(RecurrenceFrequency::Daily),
            Self::Weekly => Some(RecurrenceFrequency::Weekly),
            Self::Monthly => Some(RecurrenceFrequency::Monthly),
            Self::Yearly => Some(RecurrenceFrequency::Yearly),
        }
    }

    const fn from_rule(frequency: RecurrenceFrequency) -> Self {
        match frequency {
            RecurrenceFrequency::Daily => Self::Daily,
            RecurrenceFrequency::Weekly => Self::Weekly,
            RecurrenceFrequency::Monthly => Self::Monthly,
            RecurrenceFrequency::Yearly => Self::Yearly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurrenceMonthlyFormMode {
    DayOfMonth,
    WeekdayOrdinal,
}

impl RecurrenceMonthlyFormMode {
    const fn label(self) -> &'static str {
        match self {
            Self::DayOfMonth => "Day of month",
            Self::WeekdayOrdinal => "Nth weekday",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::DayOfMonth => Self::WeekdayOrdinal,
            Self::WeekdayOrdinal => Self::DayOfMonth,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurrenceYearlyFormMode {
    Date,
    WeekdayOrdinal,
}

impl RecurrenceYearlyFormMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Date => "Date",
            Self::WeekdayOrdinal => "Nth weekday",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Date => Self::WeekdayOrdinal,
            Self::WeekdayOrdinal => Self::Date,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurrenceEndFormMode {
    Never,
    Until,
    Count,
}

impl RecurrenceEndFormMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::Until => "Until date",
            Self::Count => "Count",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Never => Self::Until,
            Self::Until => Self::Count,
            Self::Count => Self::Never,
        }
    }
}

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
    Repeat,
    RecurrenceInterval,
    WeeklyDay(usize),
    MonthlyMode,
    YearlyMode,
    RecurrenceEnd,
    UntilDate,
    OccurrenceCount,
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
            Self::Repeat => "Repeat",
            Self::RecurrenceInterval => "Interval",
            Self::WeeklyDay(_) => "Weekly",
            Self::MonthlyMode => "Monthly",
            Self::YearlyMode => "Yearly",
            Self::RecurrenceEnd => "Ends",
            Self::UntilDate => "Until",
            Self::OccurrenceCount => "Count",
        }
    }

    const fn kind(self) -> CreateEventFormRowKind {
        match self {
            Self::Notes => CreateEventFormRowKind::Multiline,
            Self::AllDay | Self::Reminder(_) | Self::WeeklyDay(_) => CreateEventFormRowKind::Toggle,
            _ => CreateEventFormRowKind::Text,
        }
    }
}

fn checkbox(enabled: bool) -> &'static str {
    if enabled { "[x]" } else { "[ ]" }
}

fn weekly_days_for(weekday: Weekday) -> [bool; DAYS_PER_WEEK] {
    let mut days = [false; DAYS_PER_WEEK];
    days[usize::from(weekday.number_days_from_sunday())] = true;
    days
}

fn weekday_at(index: usize) -> Weekday {
    match index {
        0 => Weekday::Sunday,
        1 => Weekday::Monday,
        2 => Weekday::Tuesday,
        3 => Weekday::Wednesday,
        4 => Weekday::Thursday,
        5 => Weekday::Friday,
        6 => Weekday::Saturday,
        _ => unreachable!("weekday index stays in range"),
    }
}

fn weekday_short_label(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Sunday => "Sun",
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
    }
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

fn parse_positive_u16(value: &str, field: &'static str) -> Result<u16, CreateEventFormError> {
    let parsed = value.trim().parse::<u16>().ok().filter(|value| *value > 0);
    parsed.ok_or_else(|| CreateEventFormError::InvalidNumber {
        field,
        value: value.to_string(),
    })
}

fn parse_positive_u32(value: &str, field: &'static str) -> Result<u32, CreateEventFormError> {
    let parsed = value.trim().parse::<u32>().ok().filter(|value| *value > 0);
    parsed.ok_or_else(|| CreateEventFormError::InvalidNumber {
        field,
        value: value.to_string(),
    })
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

fn format_time_field(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

fn selectable_day_events(date: CalendarDate, source: &dyn AgendaSource) -> Vec<Event> {
    let agenda = DayAgenda::from_source(date, source);
    agenda
        .all_day_events
        .into_iter()
        .chain(
            agenda
                .timed_events
                .into_iter()
                .map(|agenda_event| agenda_event.event),
        )
        .filter(Event::is_local)
        .collect()
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

        if value == '?' {
            self.clear();
            return AppAction::OpenHelp;
        }

        if value.eq_ignore_ascii_case(&'d') {
            self.clear();
            return AppAction::OpenDelete;
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
    last_left_click: Option<MouseClick>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseClick {
    date: CalendarDate,
    at: Instant,
}

impl MouseInput {
    pub fn translate(
        &mut self,
        mouse: MouseEvent,
        target_date: Option<CalendarDate>,
        selected_date: CalendarDate,
    ) -> AppAction {
        self.translate_at(mouse, target_date, selected_date, Instant::now())
    }

    fn translate_at(
        &mut self,
        mouse: MouseEvent,
        target_date: Option<CalendarDate>,
        selected_date: CalendarDate,
        now: Instant,
    ) -> AppAction {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(target_date) = target_date else {
                    self.clear();
                    return AppAction::Noop;
                };

                let is_double_click = self
                    .last_left_click
                    .map(|click| {
                        click.date == target_date
                            && now.saturating_duration_since(click.at) <= MOUSE_DOUBLE_CLICK_TIMEOUT
                    })
                    .unwrap_or(false);

                self.last_left_click = Some(MouseClick {
                    date: target_date,
                    at: now,
                });

                if is_double_click && selected_date == target_date {
                    self.clear();
                    AppAction::OpenDay
                } else {
                    AppAction::SelectDate(target_date)
                }
            }
            MouseEventKind::Up(_) => AppAction::Noop,
            _ => {
                self.clear();
                AppAction::Noop
            }
        }
    }

    pub fn clear(&mut self) {
        self.last_left_click = None;
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
    use time::{Month, Time};

    use crate::agenda::{Holiday, InMemoryAgendaSource, RecurrenceOrdinal, SourceMetadata};

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn at(date: CalendarDate, hour: u8, minute: u8) -> EventDateTime {
        EventDateTime::new(
            date,
            Time::from_hms(hour, minute, 0).expect("valid test time"),
        )
    }

    fn local_timed_event(id: &str, title: &str, start: EventDateTime, end: EventDateTime) -> Event {
        Event::timed(id, title, start, end, SourceMetadata::local())
            .expect("valid local timed event")
    }

    fn local_all_day_event(id: &str, title: &str, date: CalendarDate) -> Event {
        Event::all_day(id, title, date, SourceMetadata::local())
    }

    fn fixture_timed_event(
        id: &str,
        title: &str,
        start: EventDateTime,
        end: EventDateTime,
    ) -> Event {
        Event::timed(id, title, start, end, SourceMetadata::fixture())
            .expect("valid fixture timed event")
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

    fn apply_keys_with_source(
        app: &mut AppState,
        input: &mut KeyboardInput,
        source: &dyn AgendaSource,
        keys: impl IntoIterator<Item = KeyEvent>,
    ) {
        for key in keys {
            let action = input.translate(key);
            app.apply_with_agenda_source(action, source);
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
    fn day_view_selects_first_local_event_and_skips_non_local_items() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![
                Event::all_day("fixture-all", "A Fixture", day, SourceMetadata::fixture()),
                local_all_day_event("local-all", "Release", day),
                fixture_timed_event("fixture-time", "Fixture", at(day, 8, 0), at(day, 9, 0)),
                local_timed_event("local-time", "Standup", at(day, 9, 0), at(day, 9, 30)),
            ],
            vec![Holiday::new(
                "holiday",
                "Holiday",
                day,
                SourceMetadata::fixture(),
            )],
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(&mut app, &mut input, &source, [key(KeyCode::Enter)]);

        assert_eq!(app.view_mode(), ViewMode::Day);
        assert_eq!(app.selected_day_event_id(), Some("local-all"));
    }

    #[test]
    fn day_view_up_and_down_cycle_local_event_selection() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![
                local_all_day_event("local-all", "Release", day),
                local_timed_event("local-time", "Standup", at(day, 9, 0), at(day, 9, 30)),
            ],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), key(KeyCode::Down)],
        );

        assert_eq!(app.selected_date(), day);
        assert_eq!(app.selected_day_event_id(), Some("local-time"));

        apply_keys_with_source(&mut app, &mut input, &source, [key(KeyCode::Down)]);
        assert_eq!(app.selected_day_event_id(), Some("local-all"));

        apply_keys_with_source(&mut app, &mut input, &source, [key(KeyCode::Up)]);
        assert_eq!(app.selected_day_event_id(), Some("local-time"));
    }

    #[test]
    fn day_view_enter_opens_edit_for_selected_local_event() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![local_timed_event(
                "local-time",
                "Standup",
                at(day, 9, 0),
                at(day, 9, 30),
            )],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), key(KeyCode::Enter)],
        );

        let form = app.create_form().expect("edit form opens");
        assert_eq!(
            form.mode(),
            &EventFormMode::Edit {
                event_id: "local-time".to_string()
            }
        );
        assert_eq!(form.rows()[0].value, "Standup");
    }

    #[test]
    fn day_view_enter_on_recurring_event_opens_edit_choice() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("series", "Standup", at(day, 9, 0), at(day, 9, 30))
            .with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Daily,
                interval: 1,
                end: RecurrenceEnd::Count(2),
                weekdays: Vec::new(),
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), key(KeyCode::Enter)],
        );

        let choice = app.recurrence_choice().expect("choice modal opens");
        assert_eq!(choice.rows()[0].label, "Edit this occurrence");
        assert_eq!(app.selected_day_event_id(), Some("series#2026-04-23T09:00"));
    }

    #[test]
    fn recurring_edit_choice_can_open_occurrence_or_series_edit() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("series", "Standup", at(day, 9, 0), at(day, 9, 30))
            .with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Daily,
                interval: 1,
                end: RecurrenceEnd::Count(2),
                weekdays: Vec::new(),
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());

        let mut occurrence_app = AppState::new(day);
        occurrence_app.apply_with_agenda_source(AppAction::OpenDay, &source);
        occurrence_app.apply_with_agenda_source(AppAction::OpenDay, &source);
        assert_eq!(
            occurrence_app.handle_recurrence_choice_key(key(KeyCode::Enter), &source),
            RecurrenceChoiceInputResult::Continue
        );
        assert_eq!(
            occurrence_app.create_form().expect("form opens").mode(),
            &EventFormMode::EditOccurrence {
                series_id: "series".to_string(),
                anchor: OccurrenceAnchor::Timed {
                    start: at(day, 9, 0)
                },
            }
        );

        let mut series_app = AppState::new(day);
        series_app.apply_with_agenda_source(AppAction::OpenDay, &source);
        series_app.apply_with_agenda_source(AppAction::OpenDay, &source);
        assert_eq!(
            series_app.handle_recurrence_choice_key(key(KeyCode::Down), &source),
            RecurrenceChoiceInputResult::Continue
        );
        assert_eq!(
            series_app.handle_recurrence_choice_key(key(KeyCode::Enter), &source),
            RecurrenceChoiceInputResult::Continue
        );
        assert_eq!(
            series_app.create_form().expect("series form opens").mode(),
            &EventFormMode::Edit {
                event_id: "series".to_string()
            }
        );
    }

    #[test]
    fn day_view_d_opens_delete_choice_for_selected_local_event() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![local_timed_event(
                "local-time",
                "Standup",
                at(day, 9, 0),
                at(day, 9, 30),
            )],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), char_key('d')],
        );

        let choice = app.delete_choice().expect("delete modal opens");
        assert_eq!(choice.rows()[0].label, "Delete event");
        assert_eq!(
            app.handle_delete_choice_key(key(KeyCode::Enter)),
            EventDeleteInputResult::Submit(EventDeleteSubmission::Event {
                event_id: "local-time".to_string()
            })
        );
    }

    #[test]
    fn day_view_d_opens_recurring_delete_choices() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("series", "Standup", at(day, 9, 0), at(day, 9, 30))
            .with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Daily,
                interval: 1,
                end: RecurrenceEnd::Count(2),
                weekdays: Vec::new(),
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), char_key('d')],
        );

        let choice = app.delete_choice().expect("delete modal opens");
        let rows = choice.rows();
        assert_eq!(rows[0].label, "Delete this occurrence");
        assert_eq!(rows[1].label, "Delete series");
        assert_eq!(
            app.handle_delete_choice_key(key(KeyCode::Enter)),
            EventDeleteInputResult::Submit(EventDeleteSubmission::Occurrence {
                series_id: "series".to_string(),
                anchor: OccurrenceAnchor::Timed {
                    start: at(day, 9, 0)
                },
            })
        );
    }

    #[test]
    fn delete_choice_up_and_down_do_not_change_day_event_selection() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![local_timed_event(
                "local-time",
                "Standup",
                at(day, 9, 0),
                at(day, 9, 30),
            )],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();
        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), char_key('d')],
        );

        assert_eq!(
            app.handle_delete_choice_key(key(KeyCode::Down)),
            EventDeleteInputResult::Continue
        );
        assert_eq!(
            app.handle_delete_choice_key(key(KeyCode::Up)),
            EventDeleteInputResult::Continue
        );

        assert_eq!(app.selected_day_event_id(), Some("local-time"));
        assert!(app.delete_choice().expect("choice stays open").rows()[0].selected);
    }

    #[test]
    fn day_view_reconciles_selection_when_event_leaves_day() {
        let day = date(2026, Month::April, 23);
        let next_day = date(2026, Month::April, 24);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![
                local_timed_event("move", "Move", at(day, 8, 0), at(day, 9, 0)),
                local_timed_event("stay", "Stay", at(day, 10, 0), at(day, 11, 0)),
            ],
            Vec::new(),
        );
        let moved_source = InMemoryAgendaSource::with_events_and_holidays(
            vec![
                local_timed_event("move", "Move", at(next_day, 8, 0), at(next_day, 9, 0)),
                local_timed_event("stay", "Stay", at(day, 10, 0), at(day, 11, 0)),
            ],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        app.apply_with_agenda_source(AppAction::OpenDay, &source);

        assert_eq!(app.selected_day_event_id(), Some("move"));

        app.reconcile_day_event_selection(&moved_source);

        assert_eq!(app.selected_date(), day);
        assert_eq!(app.selected_day_event_id(), Some("stay"));
    }

    #[test]
    fn edit_form_up_and_down_keep_day_event_selection() {
        let day = date(2026, Month::April, 23);
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![local_timed_event(
                "local-time",
                "Standup",
                at(day, 9, 0),
                at(day, 9, 30),
            )],
            Vec::new(),
        );
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();
        apply_keys_with_source(
            &mut app,
            &mut input,
            &source,
            [key(KeyCode::Enter), key(KeyCode::Enter)],
        );

        assert_eq!(
            app.handle_create_key(key(KeyCode::Down)),
            CreateEventInputResult::Continue
        );
        assert_eq!(app.selected_day_event_id(), Some("local-time"));
        assert!(app.create_form().expect("form stays open").rows()[1].focused);
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
    fn question_mark_opens_help_and_esc_closes_it() {
        let day = date(2026, Month::April, 23);
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        app.apply(input.translate(char_key('?')));

        assert!(app.is_showing_help());
        assert_eq!(
            app.handle_help_key(key(KeyCode::Esc)),
            HelpInputResult::Close
        );
        assert!(!app.is_showing_help());
        assert_eq!(app.selected_date(), day);
    }

    #[test]
    fn help_modal_blocks_calendar_navigation_until_closed() {
        let day = date(2026, Month::April, 23);
        let mut app = AppState::new(day);
        let mut input = KeyboardInput::default();

        app.apply(input.translate(char_key('?')));
        app.apply(input.translate(key(KeyCode::Right)));

        assert!(app.is_showing_help());
        assert_eq!(app.selected_date(), day);

        assert_eq!(app.handle_help_key(char_key('?')), HelpInputResult::Close);
        app.apply(input.translate(key(KeyCode::Right)));
        assert_eq!(app.selected_date(), date(2026, Month::April, 24));
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
    fn create_form_up_and_down_move_between_fields() {
        let day = date(2026, Month::April, 23);
        let mut app = AppState::new(day);
        app.apply(AppAction::OpenCreate);

        assert!(app.create_form().expect("form opens").rows()[0].focused);

        assert_eq!(
            app.handle_create_key(key(KeyCode::Down)),
            CreateEventInputResult::Continue
        );
        let rows = app.create_form().expect("form stays open").rows();
        assert!(rows[1].focused);
        assert_eq!(app.selected_date(), day);

        assert_eq!(
            app.handle_create_key(key(KeyCode::Up)),
            CreateEventInputResult::Continue
        );
        assert!(app.create_form().expect("form stays open").rows()[0].focused);
        assert_eq!(app.selected_date(), day);
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
    fn edit_form_preloads_timed_event_fields() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("local-time", "Standup", at(day, 9, 0), at(day, 9, 30))
            .with_location("Room 1")
            .with_notes("Bring notes")
            .with_reminders(vec![
                Reminder::minutes_before(10),
                Reminder::minutes_before(60),
            ]);

        let form = CreateEventForm::edit(&event);

        assert_eq!(
            form.mode(),
            &EventFormMode::Edit {
                event_id: "local-time".to_string()
            }
        );
        assert_eq!(form.title, "Standup");
        assert!(!form.all_day);
        assert_eq!(form.start_date, "2026-04-23");
        assert_eq!(form.start_time, "09:00");
        assert_eq!(form.end_date, "2026-04-23");
        assert_eq!(form.end_time, "09:30");
        assert_eq!(form.location, "Room 1");
        assert_eq!(form.notes, "Bring notes");
        assert!(form.reminders[1]);
        assert!(form.reminders[4]);
    }

    #[test]
    fn edit_form_preloads_all_day_event_and_can_switch_to_timed() {
        let day = date(2026, Month::April, 23);
        let event = local_all_day_event("local-all", "Release", day);
        let mut form = CreateEventForm::edit(&event);

        assert!(form.all_day);
        assert_eq!(form.start_date, "2026-04-23");
        assert_eq!(form.start_time, "09:00");
        assert_eq!(form.end_time, "10:00");

        form.all_day = false;
        let draft = form.submit().expect("all-day edit can become timed");

        assert_eq!(
            draft.timing,
            CreateEventTiming::Timed {
                start: EventDateTime::new(day, Time::from_hms(9, 0, 0).expect("valid time")),
                end: EventDateTime::new(day, Time::from_hms(10, 0, 0).expect("valid time")),
            }
        );
    }

    #[test]
    fn edit_form_can_switch_timed_event_to_all_day() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("local-time", "Standup", at(day, 9, 0), at(day, 9, 30));
        let mut form = CreateEventForm::edit(&event);

        form.all_day = true;
        let draft = form.submit().expect("timed edit can become all day");

        assert_eq!(draft.timing, CreateEventTiming::AllDay { date: day });
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
    fn create_form_submits_weekly_recurrence() {
        let day = date(2026, Month::April, 23);
        let mut form = CreateEventForm::new(day, CreateEventContext::EditableDate);
        form.title = "Practice".to_string();
        form.repeat = RepeatFrequency::Weekly;
        form.recurrence_interval = "2".to_string();
        form.weekly_days = [false; DAYS_PER_WEEK];
        form.weekly_days[usize::from(Weekday::Tuesday.number_days_from_sunday())] = true;
        form.weekly_days[usize::from(Weekday::Thursday.number_days_from_sunday())] = true;
        form.recurrence_end = RecurrenceEndFormMode::Count;
        form.recurrence_count = "4".to_string();

        let draft = form.submit().expect("form submits");
        let recurrence = draft.recurrence.expect("recurrence submitted");

        assert_eq!(recurrence.frequency, RecurrenceFrequency::Weekly);
        assert_eq!(recurrence.interval, 2);
        assert_eq!(recurrence.weekdays, [Weekday::Tuesday, Weekday::Thursday]);
        assert_eq!(recurrence.end, RecurrenceEnd::Count(4));
    }

    #[test]
    fn edit_series_form_preloads_recurrence_and_occurrence_form_hides_it() {
        let day = date(2026, Month::April, 23);
        let event = local_timed_event("series", "Standup", at(day, 9, 0), at(day, 9, 30))
            .with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Monthly,
                interval: 1,
                end: RecurrenceEnd::Until(day.add_days(60)),
                weekdays: Vec::new(),
                monthly: Some(RecurrenceMonthlyRule::WeekdayOrdinal {
                    ordinal: RecurrenceOrdinal::Last,
                    weekday: day.weekday(),
                }),
                yearly: None,
            });

        let form = CreateEventForm::edit(&event);

        assert_eq!(form.repeat, RepeatFrequency::Monthly);
        assert_eq!(form.monthly_mode, RecurrenceMonthlyFormMode::WeekdayOrdinal);
        assert_eq!(form.recurrence_end, RecurrenceEndFormMode::Until);

        let mut occurrence = event.clone();
        occurrence.id = "series#2026-04-23T09:00".to_string();
        occurrence.occurrence = Some(crate::agenda::OccurrenceMetadata {
            series_id: "series".to_string(),
            anchor: OccurrenceAnchor::Timed {
                start: at(day, 9, 0),
            },
        });
        let occurrence_form = CreateEventForm::edit_occurrence(&occurrence);

        assert!(
            !occurrence_form
                .rows()
                .iter()
                .any(|row| row.label == "Repeat")
        );
    }

    #[test]
    fn select_date_action_can_pick_adjacent_month_cells() {
        let mut app = AppState::new(date(2026, Month::April, 23));

        app.apply(AppAction::SelectDate(date(2026, Month::May, 1)));

        assert_eq!(app.selected_date(), date(2026, Month::May, 1));
    }

    #[test]
    fn mouse_double_click_selects_then_opens_day() {
        let target = date(2026, Month::April, 18);
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = MouseInput::default();
        let start = Instant::now();

        let action =
            input.translate_at(mouse_down(10, 10), Some(target), app.selected_date(), start);
        app.apply(action);

        assert_eq!(app.selected_date(), target);
        assert_eq!(app.view_mode(), ViewMode::Month);

        let action = input.translate_at(
            mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 10),
            Some(target),
            app.selected_date(),
            start + Duration::from_millis(40),
        );
        app.apply(action);
        assert_eq!(app.view_mode(), ViewMode::Month);

        let action = input.translate_at(
            mouse_down(10, 10),
            Some(target),
            app.selected_date(),
            start + Duration::from_millis(120),
        );
        app.apply(action);

        assert_eq!(app.view_mode(), ViewMode::Day);
    }

    #[test]
    fn slow_second_mouse_click_only_reselects_date() {
        let target = date(2026, Month::April, 18);
        let mut app = AppState::new(date(2026, Month::April, 23));
        let mut input = MouseInput::default();
        let start = Instant::now();

        let action =
            input.translate_at(mouse_down(10, 10), Some(target), app.selected_date(), start);
        app.apply(action);
        let action = input.translate_at(
            mouse_down(10, 10),
            Some(target),
            app.selected_date(),
            start + MOUSE_DOUBLE_CLICK_TIMEOUT + Duration::from_millis(1),
        );
        app.apply(action);

        assert_eq!(app.selected_date(), target);
        assert_eq!(app.view_mode(), ViewMode::Month);
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
