use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use time::{Month, Time, Weekday};

use crate::{
    calendar::CalendarDate,
    providers::{
        KeyringMicrosoftTokenStore, MicrosoftProviderConfig, MicrosoftProviderRuntime,
        ProviderCreateTarget, ProviderError, ReqwestMicrosoftHttpClient,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub start: CalendarDate,
    pub end: CalendarDate,
}

impl DateRange {
    pub fn day(date: CalendarDate) -> Self {
        Self {
            start: date,
            end: date.add_days(1),
        }
    }

    pub fn new(start: CalendarDate, end: CalendarDate) -> Result<Self, AgendaError> {
        if start < end {
            Ok(Self { start, end })
        } else {
            Err(AgendaError::InvalidDateRange { start, end })
        }
    }

    pub fn contains_date(self, date: CalendarDate) -> bool {
        self.start <= date && date < self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventDateTime {
    pub date: CalendarDate,
    pub time: Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DayMinute(u16);

impl DayMinute {
    pub const START: Self = Self(0);
    pub const END: Self = Self(24 * 60);

    pub const fn from_minutes(minutes: u16) -> Self {
        Self(minutes)
    }

    pub fn from_time(time: Time) -> Self {
        Self(u16::from(time.hour()) * 60 + u16::from(time.minute()))
    }

    pub const fn as_minutes(self) -> u16 {
        self.0
    }
}

impl EventDateTime {
    pub const fn new(date: CalendarDate, time: Time) -> Self {
        Self { date, time }
    }
}

impl Serialize for EventDateTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!(
            "{}T{:02}:{:02}",
            self.date,
            self.time.hour(),
            self.time.minute()
        ))
    }
}

impl<'de> Deserialize<'de> for EventDateTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_event_datetime_record(&value)
            .ok_or_else(|| de::Error::custom(format!("invalid event datetime '{value}'")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMetadata {
    pub source_id: String,
    pub source_name: String,
    pub external_id: Option<String>,
}

impl SourceMetadata {
    pub fn new(source_id: impl Into<String>, source_name: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            source_name: source_name.into(),
            external_id: None,
        }
    }

    pub fn with_external_id(mut self, external_id: impl Into<String>) -> Self {
        self.external_id = Some(external_id.into());
        self
    }

    pub fn fixture() -> Self {
        Self::new("fixture", "In-memory fixture")
    }

    pub fn local() -> Self {
        Self::new("local", "Local events")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventWriteTarget {
    pub id: EventWriteTargetId,
    pub label: String,
}

impl EventWriteTarget {
    pub fn local() -> Self {
        Self {
            id: EventWriteTargetId::Local,
            label: "Local".to_string(),
        }
    }

    pub fn microsoft(
        account_id: impl Into<String>,
        calendar_id: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            id: EventWriteTargetId::Microsoft {
                account_id: account_id.into(),
                calendar_id: calendar_id.into(),
            },
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventWriteTargetId {
    Local,
    Microsoft {
        account_id: String,
        calendar_id: String,
    },
}

impl EventWriteTargetId {
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn from_event(event: &Event) -> Option<Self> {
        if event.is_local() {
            return Some(Self::Local);
        }
        let mut parts = event.source.source_id.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("microsoft"), Some(account_id), Some(calendar_id)) => Some(Self::Microsoft {
                account_id: account_id.to_string(),
                calendar_id: calendar_id.to_string(),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reminder {
    pub minutes_before: u16,
}

impl Reminder {
    pub const fn minutes_before(minutes_before: u16) -> Self {
        Self { minutes_before }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTiming {
    AllDay {
        date: CalendarDate,
    },
    Timed {
        start: EventDateTime,
        end: EventDateTime,
    },
}

impl EventTiming {
    pub const fn date(self) -> Option<CalendarDate> {
        match self {
            Self::AllDay { date } => Some(date),
            Self::Timed { .. } => None,
        }
    }

    pub const fn is_all_day(self) -> bool {
        matches!(self, Self::AllDay { .. })
    }

    pub const fn is_timed(self) -> bool {
        matches!(self, Self::Timed { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceFrequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceEnd {
    Never,
    Until(CalendarDate),
    Count(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceOrdinal {
    Number(u8),
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceMonthlyRule {
    DayOfMonth(u8),
    WeekdayOrdinal {
        ordinal: RecurrenceOrdinal,
        weekday: Weekday,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceYearlyRule {
    Date {
        month: Month,
        day: u8,
    },
    WeekdayOrdinal {
        month: Month,
        ordinal: RecurrenceOrdinal,
        weekday: Weekday,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRule {
    pub frequency: RecurrenceFrequency,
    pub interval: u16,
    pub end: RecurrenceEnd,
    pub weekdays: Vec<Weekday>,
    pub monthly: Option<RecurrenceMonthlyRule>,
    pub yearly: Option<RecurrenceYearlyRule>,
}

impl RecurrenceRule {
    pub fn new(frequency: RecurrenceFrequency) -> Self {
        Self {
            frequency,
            interval: 1,
            end: RecurrenceEnd::Never,
            weekdays: Vec::new(),
            monthly: None,
            yearly: None,
        }
    }

    pub fn with_interval(mut self, interval: u16) -> Self {
        self.interval = interval.max(1);
        self
    }

    pub fn interval(&self) -> u16 {
        self.interval.max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OccurrenceAnchor {
    AllDay { date: CalendarDate },
    Timed { start: EventDateTime },
}

impl OccurrenceAnchor {
    pub const fn date(self) -> CalendarDate {
        match self {
            Self::AllDay { date } => date,
            Self::Timed { start } => start.date,
        }
    }

    fn storage_key(self) -> String {
        match self {
            Self::AllDay { date } => format!("{date}"),
            Self::Timed { start } => {
                format!("{}T{}", start.date, format_time(start.time))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceMetadata {
    pub series_id: String,
    pub anchor: OccurrenceAnchor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceOverride {
    pub anchor: OccurrenceAnchor,
    pub draft: CreateEventDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub reminders: Vec<Reminder>,
    pub source: SourceMetadata,
    pub timing: EventTiming,
    pub recurrence: Option<RecurrenceRule>,
    pub occurrence: Option<OccurrenceMetadata>,
    pub occurrence_overrides: Vec<OccurrenceOverride>,
    pub deleted_occurrences: Vec<OccurrenceAnchor>,
}

impl Event {
    pub fn all_day(
        id: impl Into<String>,
        title: impl Into<String>,
        date: CalendarDate,
        source: SourceMetadata,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            location: None,
            notes: None,
            reminders: Vec::new(),
            source,
            timing: EventTiming::AllDay { date },
            recurrence: None,
            occurrence: None,
            occurrence_overrides: Vec::new(),
            deleted_occurrences: Vec::new(),
        }
    }

    pub fn timed(
        id: impl Into<String>,
        title: impl Into<String>,
        start: EventDateTime,
        end: EventDateTime,
        source: SourceMetadata,
    ) -> Result<Self, AgendaError> {
        if start >= end {
            return Err(AgendaError::InvalidEventRange { start, end });
        }

        Ok(Self {
            id: id.into(),
            title: title.into(),
            location: None,
            notes: None,
            reminders: Vec::new(),
            source,
            timing: EventTiming::Timed { start, end },
            recurrence: None,
            occurrence: None,
            occurrence_overrides: Vec::new(),
            deleted_occurrences: Vec::new(),
        })
    }

    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }

    pub fn with_reminders(mut self, reminders: Vec<Reminder>) -> Self {
        self.reminders = reminders;
        self
    }

    pub fn with_recurrence(mut self, recurrence: RecurrenceRule) -> Self {
        self.recurrence = Some(recurrence);
        self
    }

    pub const fn is_all_day(&self) -> bool {
        self.timing.is_all_day()
    }

    pub const fn is_timed(&self) -> bool {
        self.timing.is_timed()
    }

    pub fn is_local(&self) -> bool {
        self.source.source_id == "local"
    }

    pub fn is_microsoft(&self) -> bool {
        self.source.source_id.starts_with("microsoft:")
    }

    pub fn is_editable(&self) -> bool {
        self.is_local() || self.is_microsoft()
    }

    pub const fn is_recurring_series(&self) -> bool {
        self.recurrence.is_some()
    }

    pub const fn occurrence(&self) -> Option<&OccurrenceMetadata> {
        self.occurrence.as_ref()
    }

    pub fn intersects_range(&self, range: DateRange) -> bool {
        match self.timing {
            EventTiming::AllDay { date } => range.contains_date(date),
            EventTiming::Timed { start, end } => {
                let range_start = EventDateTime::new(range.start, Time::MIDNIGHT);
                let range_end = EventDateTime::new(range.end, Time::MIDNIGHT);

                start < range_end && end > range_start
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateEventDraft {
    pub title: String,
    pub timing: CreateEventTiming,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub reminders: Vec<Reminder>,
    pub recurrence: Option<RecurrenceRule>,
}

impl CreateEventDraft {
    pub fn from_event(event: &Event) -> Self {
        Self {
            title: event.title.clone(),
            timing: match event.timing {
                EventTiming::AllDay { date } => CreateEventTiming::AllDay { date },
                EventTiming::Timed { start, end } => CreateEventTiming::Timed { start, end },
            },
            location: event.location.clone(),
            notes: event.notes.clone(),
            reminders: event.reminders.clone(),
            recurrence: event.recurrence.clone(),
        }
    }

    pub fn into_event(self, id: String) -> Result<Event, AgendaError> {
        let source = SourceMetadata::local().with_external_id(id.clone());
        let mut event = match self.timing {
            CreateEventTiming::AllDay { date } => Event::all_day(id, self.title, date, source),
            CreateEventTiming::Timed { start, end } => {
                Event::timed(id, self.title, start, end, source)?
            }
        };

        event.location = self.location;
        event.notes = self.notes;
        event.reminders = self.reminders;
        event.recurrence = self.recurrence;
        Ok(event)
    }

    fn without_recurrence(mut self) -> Self {
        self.recurrence = None;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateEventTiming {
    AllDay {
        date: CalendarDate,
    },
    Timed {
        start: EventDateTime,
        end: EventDateTime,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holiday {
    pub id: String,
    pub name: String,
    pub date: CalendarDate,
    pub source: SourceMetadata,
}

impl Holiday {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        date: CalendarDate,
        source: SourceMetadata,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            date,
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedAgendaEvent {
    pub event: Event,
    pub visible_start: DayMinute,
    pub visible_end: DayMinute,
    pub starts_before_day: bool,
    pub ends_after_day: bool,
    pub overlap_group: usize,
}

impl TimedAgendaEvent {
    pub fn overlaps(&self, other: &Self) -> bool {
        self.visible_start < other.visible_end && other.visible_start < self.visible_end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayAgenda {
    pub date: CalendarDate,
    pub holidays: Vec<Holiday>,
    pub all_day_events: Vec<Event>,
    pub timed_events: Vec<TimedAgendaEvent>,
}

impl DayAgenda {
    pub fn empty(date: CalendarDate) -> Self {
        Self {
            date,
            holidays: Vec::new(),
            all_day_events: Vec::new(),
            timed_events: Vec::new(),
        }
    }

    pub fn build(
        date: CalendarDate,
        events: impl IntoIterator<Item = Event>,
        holidays: impl IntoIterator<Item = Holiday>,
    ) -> Self {
        let range = DateRange::day(date);
        let events = events
            .into_iter()
            .filter(|event| event.intersects_range(range))
            .collect::<Vec<_>>();

        let mut agenda = Self {
            date,
            holidays: holidays
                .into_iter()
                .filter(|holiday| holiday.date == date)
                .collect(),
            all_day_events: events
                .iter()
                .cloned()
                .filter_map(|event| match event.timing {
                    EventTiming::AllDay { date: event_date } if event_date == date => Some(event),
                    _ => None,
                })
                .collect(),
            timed_events: events_to_timed_agenda_events(date, events),
        };

        agenda.sort();
        agenda
    }

    pub fn from_source<S>(date: CalendarDate, source: &S) -> Self
    where
        S: AgendaSource + ?Sized,
    {
        let range = DateRange::day(date);
        Self::build(
            date,
            source.events_intersecting(range),
            source.holidays_in(range),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.holidays.is_empty() && self.all_day_events.is_empty() && self.timed_events.is_empty()
    }

    fn sort(&mut self) {
        self.holidays
            .sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        self.all_day_events
            .sort_by(|left, right| left.title.cmp(&right.title).then(left.id.cmp(&right.id)));
        self.timed_events.sort_by(|left, right| {
            left.visible_start
                .cmp(&right.visible_start)
                .then(left.visible_end.cmp(&right.visible_end))
                .then(left.event.title.cmp(&right.event.title))
                .then(left.event.id.cmp(&right.event.id))
        });
        assign_overlap_groups(&mut self.timed_events);
    }
}

pub trait AgendaSource {
    fn events_intersecting(&self, range: DateRange) -> Vec<Event>;

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday>;

    fn event_write_targets(&self) -> Vec<EventWriteTarget> {
        vec![EventWriteTarget::local()]
    }

    fn default_event_write_target(&self) -> EventWriteTargetId {
        EventWriteTargetId::Local
    }

    fn local_event_by_id(&self, _id: &str) -> Option<Event> {
        None
    }

    fn editable_event_by_id(&self, id: &str) -> Option<Event> {
        self.local_event_by_id(id)
    }
}

#[derive(Debug)]
pub struct ConfiguredAgendaSource {
    events: InMemoryAgendaSource,
    holidays: HolidayProvider,
    events_file: Option<PathBuf>,
    create_target: ProviderCreateTarget,
    microsoft: Option<MicrosoftProviderRuntime>,
}

impl ConfiguredAgendaSource {
    pub fn development(holidays: HolidayProvider) -> Self {
        Self::new(InMemoryAgendaSource::new(), holidays)
    }

    pub fn new(events: InMemoryAgendaSource, holidays: HolidayProvider) -> Self {
        Self {
            events,
            holidays,
            events_file: None,
            create_target: ProviderCreateTarget::Local,
            microsoft: None,
        }
    }

    pub fn from_events_file(
        events_file: impl Into<PathBuf>,
        holidays: HolidayProvider,
    ) -> Result<Self, LocalEventStoreError> {
        let events_file = events_file.into();
        let events = load_events_file(&events_file)?;
        Ok(Self {
            events,
            holidays,
            events_file: Some(events_file),
            create_target: ProviderCreateTarget::Local,
            microsoft: None,
        })
    }

    pub fn with_microsoft_provider(
        mut self,
        config: MicrosoftProviderConfig,
        create_target: ProviderCreateTarget,
    ) -> Result<Self, LocalEventStoreError> {
        if config.enabled {
            self.microsoft = Some(MicrosoftProviderRuntime::load(config).map_err(provider_error)?);
            self.create_target = create_target;
        }
        Ok(self)
    }

    pub fn create_event(&mut self, draft: CreateEventDraft) -> Result<Event, LocalEventStoreError> {
        let target = self.default_event_write_target();
        self.create_event_with_target(draft, &target)
    }

    pub fn create_event_with_target(
        &mut self,
        draft: CreateEventDraft,
        target: &EventWriteTargetId,
    ) -> Result<Event, LocalEventStoreError> {
        if let EventWriteTargetId::Microsoft { .. } = target
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .create_event_in_target(draft, target, &http, &token_store)
                .map_err(provider_error);
        }
        if matches!(target, EventWriteTargetId::Microsoft { .. }) {
            return Err(LocalEventStoreError::Provider {
                reason: "Microsoft provider is not configured".to_string(),
            });
        }

        self.create_local_event(draft)
    }

    fn create_local_event(
        &mut self,
        draft: CreateEventDraft,
    ) -> Result<Event, LocalEventStoreError> {
        let id = self.next_local_event_id(&draft.title);
        let event = draft
            .into_event(id)
            .map_err(|err| LocalEventStoreError::Encode {
                path: self.events_file.clone(),
                reason: err.to_string(),
            })?;
        if let Some(path) = &self.events_file {
            let mut events = self.events.events().to_vec();
            events.push(event.clone());
            write_events_file(path, &events)?;
        }
        self.events.push_event(event.clone());
        Ok(event)
    }

    pub fn update_event(
        &mut self,
        id: &str,
        draft: CreateEventDraft,
    ) -> Result<Event, LocalEventStoreError> {
        let target = self
            .event_target_for_id(id)
            .unwrap_or(EventWriteTargetId::Local);
        self.update_event_with_target(id, draft, &target)
    }

    pub fn update_event_with_target(
        &mut self,
        id: &str,
        draft: CreateEventDraft,
        target: &EventWriteTargetId,
    ) -> Result<Event, LocalEventStoreError> {
        let current_target = self.event_target_for_id(id);
        if current_target
            .as_ref()
            .is_some_and(|current| current != target)
        {
            let created = self.create_event_with_target(draft, target)?;
            self.delete_event(id)?;
            return Ok(created);
        }

        if self.events.local_event_by_id(id).is_none()
            && id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .update_event(id, draft, &http, &token_store)
                .map_err(provider_error);
        }

        let mut events = self.events.events().to_vec();
        let Some(index) = events.iter().position(|event| event.id == id) else {
            return Err(LocalEventStoreError::EventNotFound { id: id.to_string() });
        };
        if !events[index].is_local() {
            return Err(LocalEventStoreError::EventNotEditable { id: id.to_string() });
        }

        let mut event =
            draft
                .into_event(id.to_string())
                .map_err(|err| LocalEventStoreError::Encode {
                    path: self.events_file.clone(),
                    reason: err.to_string(),
                })?;
        let existing_overrides = std::mem::take(&mut events[index].occurrence_overrides);
        event.occurrence_overrides = existing_overrides
            .into_iter()
            .filter(|override_record| event_generates_anchor(&event, override_record.anchor))
            .collect();
        let existing_deleted_occurrences = std::mem::take(&mut events[index].deleted_occurrences);
        event.deleted_occurrences = existing_deleted_occurrences
            .into_iter()
            .filter(|anchor| event_generates_anchor(&event, *anchor))
            .collect();
        events[index] = event.clone();

        if let Some(path) = &self.events_file {
            write_events_file(path, &events)?;
        }
        self.events.events = events;
        Ok(event)
    }

    pub fn update_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        draft: CreateEventDraft,
    ) -> Result<Event, LocalEventStoreError> {
        if self.events.local_event_by_id(series_id).is_none()
            && series_id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .update_occurrence(series_id, anchor, draft, &http, &token_store)
                .map_err(provider_error);
        }

        let mut events = self.events.events().to_vec();
        let Some(index) = events.iter().position(|event| event.id == series_id) else {
            return Err(LocalEventStoreError::EventNotFound {
                id: series_id.to_string(),
            });
        };
        if !events[index].is_local() || !events[index].is_recurring_series() {
            return Err(LocalEventStoreError::EventNotEditable {
                id: series_id.to_string(),
            });
        }
        if !event_generates_anchor(&events[index], anchor) {
            return Err(LocalEventStoreError::OccurrenceNotFound {
                id: series_id.to_string(),
                anchor: anchor.storage_key(),
            });
        }

        let override_record = OccurrenceOverride {
            anchor,
            draft: draft.without_recurrence(),
        };
        if let Some(existing) = events[index]
            .occurrence_overrides
            .iter_mut()
            .find(|existing| existing.anchor == anchor)
        {
            *existing = override_record;
        } else {
            events[index].occurrence_overrides.push(override_record);
        }
        events[index]
            .deleted_occurrences
            .retain(|deleted_anchor| *deleted_anchor != anchor);

        let event = occurrence_override_event(&events[index], anchor).ok_or_else(|| {
            LocalEventStoreError::OccurrenceNotFound {
                id: series_id.to_string(),
                anchor: anchor.storage_key(),
            }
        })?;

        if let Some(path) = &self.events_file {
            write_events_file(path, &events)?;
        }
        self.events.events = events;
        Ok(event)
    }

    pub fn delete_event(&mut self, id: &str) -> Result<Event, LocalEventStoreError> {
        if self.events.local_event_by_id(id).is_none()
            && id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .delete_event(id, &http, &token_store)
                .map_err(provider_error);
        }

        let mut events = self.events.events().to_vec();
        let Some(index) = events.iter().position(|event| event.id == id) else {
            return Err(LocalEventStoreError::EventNotFound { id: id.to_string() });
        };
        if !events[index].is_local() {
            return Err(LocalEventStoreError::EventNotEditable { id: id.to_string() });
        }

        let deleted = events.remove(index);
        if let Some(path) = &self.events_file {
            write_events_file(path, &events)?;
        }
        self.events.events = events;
        Ok(deleted)
    }

    pub fn duplicate_event(&mut self, id: &str) -> Result<Event, LocalEventStoreError> {
        if self.events.local_event_by_id(id).is_none()
            && id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .duplicate_event(id, &http, &token_store)
                .map_err(provider_error);
        }

        let event = self
            .events
            .local_event_by_id(id)
            .ok_or_else(|| LocalEventStoreError::EventNotFound { id: id.to_string() })?;
        self.insert_event_copy(event)
    }

    pub fn duplicate_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
    ) -> Result<Event, LocalEventStoreError> {
        if self.events.local_event_by_id(series_id).is_none()
            && series_id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .duplicate_occurrence(series_id, anchor, &http, &token_store)
                .map_err(provider_error);
        }

        let series = self.events.local_event_by_id(series_id).ok_or_else(|| {
            LocalEventStoreError::EventNotFound {
                id: series_id.to_string(),
            }
        })?;
        if !series.is_recurring_series() {
            return Err(LocalEventStoreError::EventNotEditable {
                id: series_id.to_string(),
            });
        }
        if !event_generates_anchor(&series, anchor) {
            return Err(LocalEventStoreError::OccurrenceNotFound {
                id: series_id.to_string(),
                anchor: anchor.storage_key(),
            });
        }

        let occurrence = occurrence_override_event(&series, anchor)
            .unwrap_or_else(|| generated_occurrence_event(&series, anchor));
        let draft = CreateEventDraft::from_event(&occurrence).without_recurrence();
        self.create_event(draft)
    }

    pub fn delete_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
    ) -> Result<(), LocalEventStoreError> {
        if self.events.local_event_by_id(series_id).is_none()
            && series_id.starts_with("microsoft:")
            && let Some(microsoft) = &mut self.microsoft
        {
            let http = ReqwestMicrosoftHttpClient;
            let token_store = KeyringMicrosoftTokenStore;
            return microsoft
                .delete_occurrence(series_id, anchor, &http, &token_store)
                .map_err(provider_error);
        }

        let mut events = self.events.events().to_vec();
        let Some(index) = events.iter().position(|event| event.id == series_id) else {
            return Err(LocalEventStoreError::EventNotFound {
                id: series_id.to_string(),
            });
        };
        if !events[index].is_local() || !events[index].is_recurring_series() {
            return Err(LocalEventStoreError::EventNotEditable {
                id: series_id.to_string(),
            });
        }
        if !event_generates_anchor(&events[index], anchor) {
            return Err(LocalEventStoreError::OccurrenceNotFound {
                id: series_id.to_string(),
                anchor: anchor.storage_key(),
            });
        }

        events[index]
            .occurrence_overrides
            .retain(|override_record| override_record.anchor != anchor);
        if !events[index].deleted_occurrences.contains(&anchor) {
            events[index].deleted_occurrences.push(anchor);
        }

        if let Some(path) = &self.events_file {
            write_events_file(path, &events)?;
        }
        self.events.events = events;
        Ok(())
    }

    fn insert_event_copy(&mut self, mut event: Event) -> Result<Event, LocalEventStoreError> {
        let id = self.next_local_event_id(&event.title);
        event.id = id.clone();
        event.source = SourceMetadata::local().with_external_id(id);
        event.occurrence = None;

        if let Some(path) = &self.events_file {
            let mut events = self.events.events().to_vec();
            events.push(event.clone());
            write_events_file(path, &events)?;
        }
        self.events.push_event(event.clone());
        Ok(event)
    }

    fn event_target_for_id(&self, id: &str) -> Option<EventWriteTargetId> {
        self.events
            .local_event_by_id(id)
            .as_ref()
            .and_then(EventWriteTargetId::from_event)
            .or_else(|| {
                let microsoft = self.microsoft.as_ref()?;
                let event = microsoft.agenda_source().editable_event_by_id(id)?;
                EventWriteTargetId::from_event(&event)
            })
    }

    fn next_local_event_id(&self, title: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let counter = self.events.events().len() + 1;
        let slug = slugify(title);
        if slug.is_empty() {
            format!("local-{now}-{counter}")
        } else {
            format!("local-{now}-{counter}-{slug}")
        }
    }
}

impl AgendaSource for ConfiguredAgendaSource {
    fn events_intersecting(&self, range: DateRange) -> Vec<Event> {
        let mut events = self.events.events_intersecting(range);
        if let Some(microsoft) = &self.microsoft {
            events.extend(microsoft.agenda_source().events_intersecting(range));
        }
        events.sort_by(|left, right| {
            event_sort_key(left)
                .cmp(&event_sort_key(right))
                .then(left.id.cmp(&right.id))
        });
        events
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        self.holidays.holidays_in(range)
    }

    fn event_write_targets(&self) -> Vec<EventWriteTarget> {
        let mut targets = vec![EventWriteTarget::local()];
        if let Some(microsoft) = &self.microsoft {
            targets.extend(microsoft.write_targets());
        }
        targets
    }

    fn default_event_write_target(&self) -> EventWriteTargetId {
        if self.create_target == ProviderCreateTarget::Microsoft
            && let Some(target) = self
                .microsoft
                .as_ref()
                .and_then(MicrosoftProviderRuntime::default_write_target)
        {
            return target;
        }
        EventWriteTargetId::Local
    }

    fn local_event_by_id(&self, id: &str) -> Option<Event> {
        self.events.local_event_by_id(id)
    }

    fn editable_event_by_id(&self, id: &str) -> Option<Event> {
        self.events.local_event_by_id(id).or_else(|| {
            self.microsoft
                .as_ref()
                .and_then(|microsoft| microsoft.agenda_source().event_by_id(id))
        })
    }
}

fn provider_error(err: ProviderError) -> LocalEventStoreError {
    LocalEventStoreError::Provider {
        reason: err.to_string(),
    }
}

#[derive(Debug)]
pub enum HolidayProvider {
    Off(EmptyAgendaSource),
    UsFederal(UsFederalHolidaySource),
    Nager(NagerHolidaySource),
}

impl HolidayProvider {
    pub const fn off() -> Self {
        Self::Off(EmptyAgendaSource)
    }

    pub const fn us_federal() -> Self {
        Self::UsFederal(UsFederalHolidaySource)
    }

    pub fn nager(country_code: impl Into<String>) -> Self {
        Self::Nager(NagerHolidaySource::new(country_code))
    }
}

impl AgendaSource for HolidayProvider {
    fn events_intersecting(&self, _range: DateRange) -> Vec<Event> {
        Vec::new()
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        match self {
            Self::Off(source) => source.holidays_in(range),
            Self::UsFederal(source) => source.holidays_in(range),
            Self::Nager(source) => source.holidays_in(range),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EmptyAgendaSource;

impl AgendaSource for EmptyAgendaSource {
    fn events_intersecting(&self, _range: DateRange) -> Vec<Event> {
        Vec::new()
    }

    fn holidays_in(&self, _range: DateRange) -> Vec<Holiday> {
        Vec::new()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UsFederalHolidaySource;

impl AgendaSource for UsFederalHolidaySource {
    fn events_intersecting(&self, _range: DateRange) -> Vec<Event> {
        Vec::new()
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        let mut holidays = Vec::new();

        for year in range.start.year() - 1..=range.end.year() + 1 {
            holidays.extend(us_federal_holidays_for_year(year));
        }

        holidays.retain(|holiday| range.contains_date(holiday.date));
        holidays.sort_by(|left, right| left.date.cmp(&right.date).then(left.name.cmp(&right.name)));
        holidays.dedup_by(|left, right| left.id == right.id);
        holidays
    }
}

#[derive(Debug)]
pub struct NagerHolidaySource {
    country_code: String,
    cache_dir: PathBuf,
    timeout: Duration,
    state: RefCell<NagerHolidayState>,
}

impl NagerHolidaySource {
    pub fn new(country_code: impl Into<String>) -> Self {
        Self::with_cache_dir(
            country_code,
            default_nager_cache_dir(),
            Duration::from_millis(1500),
        )
    }

    pub fn with_cache_dir(
        country_code: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            country_code: country_code.into().to_ascii_uppercase(),
            cache_dir: cache_dir.into(),
            timeout,
            state: RefCell::new(NagerHolidayState::default()),
        }
    }

    fn ensure_year_loaded(&self, year: i32) {
        if self.state.borrow().attempted_years.contains(&year) {
            return;
        }

        let holidays = self.load_year(year).unwrap_or_default();
        let mut state = self.state.borrow_mut();
        state.attempted_years.insert(year);
        state.holidays_by_year.insert(year, holidays);
    }

    fn load_year(&self, year: i32) -> Option<Vec<Holiday>> {
        if let Some(cached) = self.load_year_from_cache(year) {
            return Some(cached);
        }

        let body = self.fetch_year(year)?;
        let holidays = parse_nager_holidays(&self.country_code, &body)?;
        self.write_year_cache(year, &body);
        Some(holidays)
    }

    fn load_year_from_cache(&self, year: i32) -> Option<Vec<Holiday>> {
        let body = fs::read_to_string(self.cache_file(year)).ok()?;
        parse_nager_holidays(&self.country_code, &body)
    }

    fn fetch_year(&self, year: i32) -> Option<String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .user_agent("rcal/0.1")
            .build()
            .ok()?;
        let url = format!(
            "https://date.nager.at/api/v3/PublicHolidays/{year}/{}",
            self.country_code
        );
        let response = client.get(url).send().ok()?;

        if !response.status().is_success() {
            return None;
        }

        response.text().ok()
    }

    fn write_year_cache(&self, year: i32, body: &str) {
        let file = self.cache_file(year);
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(file, body);
    }

    fn cache_file(&self, year: i32) -> PathBuf {
        self.cache_dir
            .join(&self.country_code)
            .join(format!("{year}.json"))
    }
}

impl AgendaSource for NagerHolidaySource {
    fn events_intersecting(&self, _range: DateRange) -> Vec<Event> {
        Vec::new()
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        let final_date = range.end.add_days(-1);
        for year in range.start.year()..=final_date.year() {
            self.ensure_year_loaded(year);
        }

        let state = self.state.borrow();
        let mut holidays = state
            .holidays_by_year
            .values()
            .flatten()
            .filter(|holiday| range.contains_date(holiday.date))
            .cloned()
            .collect::<Vec<_>>();
        holidays.sort_by(|left, right| left.date.cmp(&right.date).then(left.name.cmp(&right.name)));
        holidays
    }
}

#[derive(Debug, Default)]
struct NagerHolidayState {
    holidays_by_year: HashMap<i32, Vec<Holiday>>,
    attempted_years: HashSet<i32>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InMemoryAgendaSource {
    events: Vec<Event>,
    holidays: Vec<Holiday>,
}

impl InMemoryAgendaSource {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            holidays: Vec::new(),
        }
    }

    pub fn with_events_and_holidays(events: Vec<Event>, holidays: Vec<Holiday>) -> Self {
        Self { events, holidays }
    }

    pub fn push_event(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn push_holiday(&mut self, holiday: Holiday) {
        self.holidays.push(holiday);
    }

    pub fn development_fixture() -> Self {
        let date = CalendarDate::from_ymd(2026, Month::April, 23).expect("fixture date is valid");
        let source = SourceMetadata::fixture();

        Self {
            events: vec![
                Event::all_day("release-day", "Release day", date, source.clone()),
                Event::timed(
                    "standup",
                    "Standup",
                    EventDateTime::new(date, time(9, 0)),
                    EventDateTime::new(date, time(9, 30)),
                    source.clone(),
                )
                .expect("fixture event range is valid"),
                Event::timed(
                    "review",
                    "Review",
                    EventDateTime::new(date, time(9, 15)),
                    EventDateTime::new(date, time(10, 0)),
                    source.clone(),
                )
                .expect("fixture event range is valid"),
                Event::timed(
                    "deploy",
                    "Late deploy",
                    EventDateTime::new(date, time(23, 0)),
                    EventDateTime::new(date.add_days(1), time(1, 0)),
                    source.clone(),
                )
                .expect("fixture event range is valid"),
            ],
            holidays: Vec::new(),
        }
    }
}

impl AgendaSource for InMemoryAgendaSource {
    fn events_intersecting(&self, range: DateRange) -> Vec<Event> {
        let mut events = self
            .events
            .iter()
            .flat_map(|event| events_intersecting_range(event, range))
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            event_sort_key(left)
                .cmp(&event_sort_key(right))
                .then(left.id.cmp(&right.id))
        });
        events
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        self.holidays
            .iter()
            .filter(|holiday| range.contains_date(holiday.date))
            .cloned()
            .collect()
    }

    fn local_event_by_id(&self, id: &str) -> Option<Event> {
        self.events
            .iter()
            .find(|event| event.id == id && event.is_local())
            .cloned()
    }
}

fn events_intersecting_range(event: &Event, range: DateRange) -> Vec<Event> {
    if event.recurrence.is_none() {
        return event
            .intersects_range(range)
            .then(|| event.clone())
            .into_iter()
            .collect();
    }

    expand_recurring_event(event, range)
        .into_iter()
        .filter(|event| event.intersects_range(range))
        .collect()
}

fn expand_recurring_event(event: &Event, range: DateRange) -> Vec<Event> {
    let Some(recurrence) = &event.recurrence else {
        return Vec::new();
    };
    let Some(start_date) = event_start_date(event) else {
        return Vec::new();
    };

    let mut events = Vec::new();
    let final_date = range.end.add_days(-1);
    let mut date = start_date;
    let mut generated_count = 0_u32;

    while date <= final_date {
        if let RecurrenceEnd::Until(until) = recurrence.end
            && date > until
        {
            break;
        }

        if recurs_on_date(date, start_date, recurrence) {
            generated_count = generated_count.saturating_add(1);
            if let RecurrenceEnd::Count(max_count) = recurrence.end
                && generated_count > max_count
            {
                break;
            }

            let anchor = occurrence_anchor_for_date(event, date);
            if event.deleted_occurrences.contains(&anchor) {
                date = date.add_days(1);
                continue;
            }
            let instance = occurrence_override_event(event, anchor)
                .unwrap_or_else(|| generated_occurrence_event(event, anchor));
            events.push(instance);
        }

        date = date.add_days(1);
    }

    events
}

fn event_generates_anchor(event: &Event, anchor: OccurrenceAnchor) -> bool {
    let Some(recurrence) = &event.recurrence else {
        return false;
    };
    let Some(start_date) = event_start_date(event) else {
        return false;
    };
    if anchor.date() < start_date || !recurs_on_date(anchor.date(), start_date, recurrence) {
        return false;
    }
    if !anchor_is_within_recurrence_end(anchor.date(), start_date, recurrence) {
        return false;
    }
    occurrence_anchor_for_date(event, anchor.date()) == anchor
}

fn anchor_is_within_recurrence_end(
    anchor_date: CalendarDate,
    start_date: CalendarDate,
    recurrence: &RecurrenceRule,
) -> bool {
    if let RecurrenceEnd::Until(until) = recurrence.end
        && anchor_date > until
    {
        return false;
    }

    if let RecurrenceEnd::Count(max_count) = recurrence.end {
        let mut count = 0_u32;
        let mut date = start_date;
        while date <= anchor_date {
            if recurs_on_date(date, start_date, recurrence) {
                count = count.saturating_add(1);
            }
            date = date.add_days(1);
        }
        return count <= max_count;
    }

    true
}

fn occurrence_override_event(series: &Event, anchor: OccurrenceAnchor) -> Option<Event> {
    let override_record = series
        .occurrence_overrides
        .iter()
        .find(|override_record| override_record.anchor == anchor)?;
    occurrence_event_from_draft(series, anchor, override_record.draft.clone()).ok()
}

fn generated_occurrence_event(series: &Event, anchor: OccurrenceAnchor) -> Event {
    let mut event = series.clone();
    event.id = occurrence_event_id(series, anchor);
    event.timing = occurrence_timing(series, anchor);
    event.occurrence = Some(OccurrenceMetadata {
        series_id: series.id.clone(),
        anchor,
    });
    event.recurrence = None;
    event.occurrence_overrides = Vec::new();
    event.deleted_occurrences = Vec::new();
    event
}

fn occurrence_event_from_draft(
    series: &Event,
    anchor: OccurrenceAnchor,
    draft: CreateEventDraft,
) -> Result<Event, AgendaError> {
    let mut event = draft
        .without_recurrence()
        .into_event(occurrence_event_id(series, anchor))?;
    event.source = series.source.clone();
    event.occurrence = Some(OccurrenceMetadata {
        series_id: series.id.clone(),
        anchor,
    });
    Ok(event)
}

fn occurrence_event_id(series: &Event, anchor: OccurrenceAnchor) -> String {
    format!("{}#{}", series.id, anchor.storage_key())
}

fn occurrence_anchor_for_date(event: &Event, date: CalendarDate) -> OccurrenceAnchor {
    match event.timing {
        EventTiming::AllDay { .. } => OccurrenceAnchor::AllDay { date },
        EventTiming::Timed { start, .. } => OccurrenceAnchor::Timed {
            start: EventDateTime::new(date, start.time),
        },
    }
}

fn occurrence_timing(event: &Event, anchor: OccurrenceAnchor) -> EventTiming {
    match (event.timing, anchor) {
        (EventTiming::AllDay { .. }, OccurrenceAnchor::AllDay { date }) => {
            EventTiming::AllDay { date }
        }
        (
            EventTiming::Timed { start, end },
            OccurrenceAnchor::Timed {
                start: anchor_start,
            },
        ) => {
            let duration_minutes = datetime_distance_minutes(start, end);
            EventTiming::Timed {
                start: anchor_start,
                end: add_minutes(anchor_start, duration_minutes),
            }
        }
        _ => event.timing,
    }
}

fn event_start_date(event: &Event) -> Option<CalendarDate> {
    match event.timing {
        EventTiming::AllDay { date } => Some(date),
        EventTiming::Timed { start, .. } => Some(start.date),
    }
}

fn recurs_on_date(date: CalendarDate, start_date: CalendarDate, rule: &RecurrenceRule) -> bool {
    if date < start_date {
        return false;
    }

    match rule.frequency {
        RecurrenceFrequency::Daily => {
            days_between(start_date, date) % i32::from(rule.interval()) == 0
        }
        RecurrenceFrequency::Weekly => {
            let week_index = calendar_weeks_between(start_date, date);
            let weekdays = recurrence_weekdays(rule, start_date);
            week_index % i32::from(rule.interval()) == 0 && weekdays.contains(&date.weekday())
        }
        RecurrenceFrequency::Monthly => {
            let months = months_between(start_date, date);
            if months < 0 || months % i32::from(rule.interval()) != 0 {
                return false;
            }
            let monthly = rule
                .monthly
                .unwrap_or(RecurrenceMonthlyRule::DayOfMonth(start_date.day()));
            match monthly {
                RecurrenceMonthlyRule::DayOfMonth(day) => {
                    CalendarDate::from_ymd(date.year(), date.month(), day).ok() == Some(date)
                }
                RecurrenceMonthlyRule::WeekdayOrdinal { ordinal, weekday } => {
                    weekday_ordinal_date(date.year(), date.month(), ordinal, weekday) == Some(date)
                }
            }
        }
        RecurrenceFrequency::Yearly => {
            let years = date.year() - start_date.year();
            if years < 0 || years % i32::from(rule.interval()) != 0 {
                return false;
            }
            let yearly = rule.yearly.unwrap_or(RecurrenceYearlyRule::Date {
                month: start_date.month(),
                day: start_date.day(),
            });
            match yearly {
                RecurrenceYearlyRule::Date { month, day } => {
                    CalendarDate::from_ymd(date.year(), month, day).ok() == Some(date)
                }
                RecurrenceYearlyRule::WeekdayOrdinal {
                    month,
                    ordinal,
                    weekday,
                } => {
                    date.month() == month
                        && weekday_ordinal_date(date.year(), month, ordinal, weekday) == Some(date)
                }
            }
        }
    }
}

fn recurrence_weekdays(rule: &RecurrenceRule, start_date: CalendarDate) -> Vec<Weekday> {
    if rule.weekdays.is_empty() {
        vec![start_date.weekday()]
    } else {
        rule.weekdays.clone()
    }
}

fn calendar_weeks_between(start: CalendarDate, end: CalendarDate) -> i32 {
    let start_week = sunday_of_week(start);
    let end_week = sunday_of_week(end);
    days_between(start_week, end_week) / 7
}

fn sunday_of_week(date: CalendarDate) -> CalendarDate {
    date.add_days(-i32::from(date.weekday().number_days_from_sunday()))
}

fn weekday_ordinal_date(
    year: i32,
    month: Month,
    ordinal: RecurrenceOrdinal,
    weekday: Weekday,
) -> Option<CalendarDate> {
    match ordinal {
        RecurrenceOrdinal::Number(number) if (1..=4).contains(&number) => {
            let first = CalendarDate::from_ymd(year, month, 1).ok()?;
            let first_weekday = first.weekday().number_days_from_sunday();
            let target_weekday = weekday.number_days_from_sunday();
            let offset = (target_weekday + 7 - first_weekday) % 7;
            let day = 1 + offset + (number - 1) * 7;
            CalendarDate::from_ymd(year, month, day).ok()
        }
        RecurrenceOrdinal::Last => {
            let mut date = CalendarDate::from_ymd(year, month, month.length(year)).ok()?;
            while date.weekday() != weekday {
                date = date.add_days(-1);
            }
            Some(date)
        }
        _ => None,
    }
}

pub fn recurrence_ordinal_for_date(date: CalendarDate) -> RecurrenceOrdinal {
    if date.day().saturating_add(7) > date.month().length(date.year()) {
        RecurrenceOrdinal::Last
    } else {
        RecurrenceOrdinal::Number(((date.day() - 1) / 7) + 1)
    }
}

fn days_between(start: CalendarDate, end: CalendarDate) -> i32 {
    end.inner().to_julian_day() - start.inner().to_julian_day()
}

fn months_between(start: CalendarDate, end: CalendarDate) -> i32 {
    (end.year() - start.year()) * 12 + i32::from(u8::from(end.month()))
        - i32::from(u8::from(start.month()))
}

fn datetime_distance_minutes(start: EventDateTime, end: EventDateTime) -> i32 {
    days_between(start.date, end.date) * 24 * 60 + time_minutes(end.time) - time_minutes(start.time)
}

fn add_minutes(start: EventDateTime, duration_minutes: i32) -> EventDateTime {
    let absolute_minutes = time_minutes(start.time) + duration_minutes;
    let day_offset = absolute_minutes.div_euclid(24 * 60);
    let minute_of_day = absolute_minutes.rem_euclid(24 * 60);
    EventDateTime::new(
        start.date.add_days(day_offset),
        Time::from_hms(
            u8::try_from(minute_of_day / 60).expect("hour stays in range"),
            u8::try_from(minute_of_day % 60).expect("minute stays in range"),
            0,
        )
        .expect("computed time is valid"),
    )
}

fn time_minutes(time: Time) -> i32 {
    i32::from(time.hour()) * 60 + i32::from(time.minute())
}

fn event_sort_key(event: &Event) -> (CalendarDate, DayMinute, String) {
    match event.timing {
        EventTiming::AllDay { date } => (date, DayMinute::START, event.title.clone()),
        EventTiming::Timed { start, .. } => (
            start.date,
            DayMinute::from_time(start.time),
            event.title.clone(),
        ),
    }
}

pub fn default_events_file() -> PathBuf {
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data_home).join("rcal").join("events.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("rcal")
            .join("events.json");
    }

    env::temp_dir().join("rcal").join("events.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEventStoreError {
    Read {
        path: PathBuf,
        reason: String,
    },
    Parse {
        path: PathBuf,
        reason: String,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u8,
    },
    EventNotFound {
        id: String,
    },
    OccurrenceNotFound {
        id: String,
        anchor: String,
    },
    EventNotEditable {
        id: String,
    },
    Encode {
        path: Option<PathBuf>,
        reason: String,
    },
    Provider {
        reason: String,
    },
    Write {
        path: PathBuf,
        reason: String,
    },
}

impl fmt::Display for LocalEventStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, reason } => {
                write!(f, "failed to read {}: {reason}", path.display())
            }
            Self::Parse { path, reason } => {
                write!(f, "failed to parse {}: {reason}", path.display())
            }
            Self::UnsupportedVersion { path, version } => write!(
                f,
                "unsupported local events file version {version} in {}",
                path.display()
            ),
            Self::EventNotFound { id } => write!(f, "local event '{id}' was not found"),
            Self::OccurrenceNotFound { id, anchor } => {
                write!(
                    f,
                    "recurring occurrence '{anchor}' was not found for local event '{id}'"
                )
            }
            Self::EventNotEditable { id } => write!(f, "event '{id}' is not editable locally"),
            Self::Encode { path, reason } => {
                if let Some(path) = path {
                    write!(f, "failed to encode {}: {reason}", path.display())
                } else {
                    write!(f, "failed to encode local event: {reason}")
                }
            }
            Self::Provider { reason } => write!(f, "{reason}"),
            Self::Write { path, reason } => {
                write!(f, "failed to write {}: {reason}", path.display())
            }
        }
    }
}

impl Error for LocalEventStoreError {}

fn load_events_file(path: &Path) -> Result<InMemoryAgendaSource, LocalEventStoreError> {
    let body = match fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(InMemoryAgendaSource::new());
        }
        Err(err) => {
            return Err(LocalEventStoreError::Read {
                path: path.to_path_buf(),
                reason: err.to_string(),
            });
        }
    };

    let file = serde_json::from_str::<LocalEventsFile>(&body).map_err(|err| {
        LocalEventStoreError::Parse {
            path: path.to_path_buf(),
            reason: err.to_string(),
        }
    })?;

    if !matches!(file.version, 1 | LOCAL_EVENTS_VERSION) {
        return Err(LocalEventStoreError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: file.version,
        });
    }

    let events = file
        .events
        .into_iter()
        .map(|record| record.into_event(path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(InMemoryAgendaSource::with_events_and_holidays(
        events,
        Vec::new(),
    ))
}

fn write_events_file(path: &Path, events: &[Event]) -> Result<(), LocalEventStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| LocalEventStoreError::Write {
            path: parent.to_path_buf(),
            reason: err.to_string(),
        })?;
    }

    let file = LocalEventsFile {
        version: LOCAL_EVENTS_VERSION,
        events: events.iter().map(LocalEventRecord::from_event).collect(),
    };
    let body = serde_json::to_string_pretty(&file).map_err(|err| LocalEventStoreError::Encode {
        path: Some(path.to_path_buf()),
        reason: err.to_string(),
    })?;
    let temp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("json")
    ));

    fs::write(&temp_path, body).map_err(|err| LocalEventStoreError::Write {
        path: temp_path.clone(),
        reason: err.to_string(),
    })?;
    fs::rename(&temp_path, path).map_err(|err| LocalEventStoreError::Write {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })
}

const LOCAL_EVENTS_VERSION: u8 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct LocalEventsFile {
    version: u8,
    #[serde(default)]
    events: Vec<LocalEventRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum LocalEventRecord {
    Timed {
        id: String,
        title: String,
        start_date: String,
        start_time: String,
        end_date: String,
        end_time: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reminders_minutes_before: Vec<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recurrence: Option<LocalRecurrenceRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overrides: Vec<LocalOccurrenceOverrideRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deleted_occurrences: Vec<LocalOccurrenceAnchorRecord>,
    },
    AllDay {
        id: String,
        title: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reminders_minutes_before: Vec<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recurrence: Option<LocalRecurrenceRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overrides: Vec<LocalOccurrenceOverrideRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deleted_occurrences: Vec<LocalOccurrenceAnchorRecord>,
    },
}

impl LocalEventRecord {
    fn from_event(event: &Event) -> Self {
        let reminders_minutes_before = event
            .reminders
            .iter()
            .map(|reminder| reminder.minutes_before)
            .collect::<Vec<_>>();
        let recurrence = event
            .recurrence
            .as_ref()
            .map(LocalRecurrenceRecord::from_rule);
        let overrides = event
            .occurrence_overrides
            .iter()
            .map(LocalOccurrenceOverrideRecord::from_override)
            .collect::<Vec<_>>();
        let deleted_occurrences = event
            .deleted_occurrences
            .iter()
            .copied()
            .map(LocalOccurrenceAnchorRecord::from_anchor)
            .collect::<Vec<_>>();

        match event.timing {
            EventTiming::AllDay { date } => Self::AllDay {
                id: event.id.clone(),
                title: event.title.clone(),
                date: date.to_string(),
                location: event.location.clone(),
                notes: event.notes.clone(),
                reminders_minutes_before,
                recurrence,
                overrides,
                deleted_occurrences,
            },
            EventTiming::Timed { start, end } => Self::Timed {
                id: event.id.clone(),
                title: event.title.clone(),
                start_date: start.date.to_string(),
                start_time: format_time(start.time),
                end_date: end.date.to_string(),
                end_time: format_time(end.time),
                location: event.location.clone(),
                notes: event.notes.clone(),
                reminders_minutes_before,
                recurrence,
                overrides,
                deleted_occurrences,
            },
        }
    }

    fn into_event(self, path: &Path) -> Result<Event, LocalEventStoreError> {
        match self {
            Self::Timed {
                id,
                title,
                start_date,
                start_time,
                end_date,
                end_time,
                location,
                notes,
                reminders_minutes_before,
                recurrence,
                overrides,
                deleted_occurrences,
            } => {
                let start = EventDateTime::new(
                    parse_local_date(&start_date, path)?,
                    parse_local_time(&start_time, path)?,
                );
                let end = EventDateTime::new(
                    parse_local_date(&end_date, path)?,
                    parse_local_time(&end_time, path)?,
                );
                let mut event = Event::timed(
                    id.clone(),
                    title,
                    start,
                    end,
                    SourceMetadata::local().with_external_id(id),
                )
                .map_err(|err| LocalEventStoreError::Parse {
                    path: path.to_path_buf(),
                    reason: err.to_string(),
                })?;
                event.location = empty_to_none(location);
                event.notes = empty_to_none(notes);
                event.reminders = reminders_from_minutes(reminders_minutes_before);
                event.recurrence = recurrence
                    .map(|recurrence| recurrence.into_rule(path))
                    .transpose()?;
                event.occurrence_overrides = overrides
                    .into_iter()
                    .map(|override_record| override_record.into_override(path))
                    .collect::<Result<Vec<_>, _>>()?;
                event.deleted_occurrences = deleted_occurrences
                    .into_iter()
                    .map(|anchor| anchor.into_anchor(path))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(event)
            }
            Self::AllDay {
                id,
                title,
                date,
                location,
                notes,
                reminders_minutes_before,
                recurrence,
                overrides,
                deleted_occurrences,
            } => {
                let mut event = Event::all_day(
                    id.clone(),
                    title,
                    parse_local_date(&date, path)?,
                    SourceMetadata::local().with_external_id(id),
                );
                event.location = empty_to_none(location);
                event.notes = empty_to_none(notes);
                event.reminders = reminders_from_minutes(reminders_minutes_before);
                event.recurrence = recurrence
                    .map(|recurrence| recurrence.into_rule(path))
                    .transpose()?;
                event.occurrence_overrides = overrides
                    .into_iter()
                    .map(|override_record| override_record.into_override(path))
                    .collect::<Result<Vec<_>, _>>()?;
                event.deleted_occurrences = deleted_occurrences
                    .into_iter()
                    .map(|anchor| anchor.into_anchor(path))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(event)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalRecurrenceRecord {
    frequency: String,
    interval: u16,
    #[serde(default)]
    weekdays: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monthly: Option<LocalRecurrenceMonthlyRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    yearly: Option<LocalRecurrenceYearlyRecord>,
    end: LocalRecurrenceEndRecord,
}

impl LocalRecurrenceRecord {
    fn from_rule(rule: &RecurrenceRule) -> Self {
        Self {
            frequency: match rule.frequency {
                RecurrenceFrequency::Daily => "daily",
                RecurrenceFrequency::Weekly => "weekly",
                RecurrenceFrequency::Monthly => "monthly",
                RecurrenceFrequency::Yearly => "yearly",
            }
            .to_string(),
            interval: rule.interval(),
            weekdays: rule
                .weekdays
                .iter()
                .map(|weekday| weekday_name(*weekday))
                .collect(),
            monthly: rule.monthly.map(LocalRecurrenceMonthlyRecord::from_rule),
            yearly: rule.yearly.map(LocalRecurrenceYearlyRecord::from_rule),
            end: LocalRecurrenceEndRecord::from_rule(rule.end),
        }
    }

    fn into_rule(self, path: &Path) -> Result<RecurrenceRule, LocalEventStoreError> {
        let frequency = match self.frequency.as_str() {
            "daily" => RecurrenceFrequency::Daily,
            "weekly" => RecurrenceFrequency::Weekly,
            "monthly" => RecurrenceFrequency::Monthly,
            "yearly" => RecurrenceFrequency::Yearly,
            value => {
                return Err(LocalEventStoreError::Parse {
                    path: path.to_path_buf(),
                    reason: format!("invalid recurrence frequency '{value}'"),
                });
            }
        };
        let weekdays = self
            .weekdays
            .into_iter()
            .map(|weekday| parse_weekday_record(&weekday, path))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RecurrenceRule {
            frequency,
            interval: self.interval.max(1),
            end: self.end.into_rule(path)?,
            weekdays,
            monthly: self
                .monthly
                .map(|monthly| monthly.into_rule(path))
                .transpose()?,
            yearly: self
                .yearly
                .map(|yearly| yearly.into_rule(path))
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum LocalRecurrenceEndRecord {
    Never,
    Until { date: String },
    Count { count: u32 },
}

impl LocalRecurrenceEndRecord {
    fn from_rule(end: RecurrenceEnd) -> Self {
        match end {
            RecurrenceEnd::Never => Self::Never,
            RecurrenceEnd::Until(date) => Self::Until {
                date: date.to_string(),
            },
            RecurrenceEnd::Count(count) => Self::Count { count },
        }
    }

    fn into_rule(self, path: &Path) -> Result<RecurrenceEnd, LocalEventStoreError> {
        match self {
            Self::Never => Ok(RecurrenceEnd::Never),
            Self::Until { date } => Ok(RecurrenceEnd::Until(parse_local_date(&date, path)?)),
            Self::Count { count } => Ok(RecurrenceEnd::Count(count.max(1))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum LocalRecurrenceMonthlyRecord {
    DayOfMonth {
        day: u8,
    },
    WeekdayOrdinal {
        ordinal: LocalRecurrenceOrdinalRecord,
        weekday: LocalWeekdayRecord,
    },
}

impl LocalRecurrenceMonthlyRecord {
    fn from_rule(rule: RecurrenceMonthlyRule) -> Self {
        match rule {
            RecurrenceMonthlyRule::DayOfMonth(day) => Self::DayOfMonth { day },
            RecurrenceMonthlyRule::WeekdayOrdinal { ordinal, weekday } => Self::WeekdayOrdinal {
                ordinal: LocalRecurrenceOrdinalRecord::from_rule(ordinal),
                weekday: LocalWeekdayRecord::from_weekday(weekday),
            },
        }
    }

    fn into_rule(self, _path: &Path) -> Result<RecurrenceMonthlyRule, LocalEventStoreError> {
        Ok(match self {
            Self::DayOfMonth { day } => RecurrenceMonthlyRule::DayOfMonth(day),
            Self::WeekdayOrdinal { ordinal, weekday } => RecurrenceMonthlyRule::WeekdayOrdinal {
                ordinal: ordinal.into_rule(),
                weekday: weekday.into_weekday(),
            },
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum LocalRecurrenceYearlyRecord {
    Date {
        month: u8,
        day: u8,
    },
    WeekdayOrdinal {
        month: u8,
        ordinal: LocalRecurrenceOrdinalRecord,
        weekday: LocalWeekdayRecord,
    },
}

impl LocalRecurrenceYearlyRecord {
    fn from_rule(rule: RecurrenceYearlyRule) -> Self {
        match rule {
            RecurrenceYearlyRule::Date { month, day } => Self::Date {
                month: u8::from(month),
                day,
            },
            RecurrenceYearlyRule::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            } => Self::WeekdayOrdinal {
                month: u8::from(month),
                ordinal: LocalRecurrenceOrdinalRecord::from_rule(ordinal),
                weekday: LocalWeekdayRecord::from_weekday(weekday),
            },
        }
    }

    fn into_rule(self, path: &Path) -> Result<RecurrenceYearlyRule, LocalEventStoreError> {
        Ok(match self {
            Self::Date { month, day } => RecurrenceYearlyRule::Date {
                month: parse_month_record(month, path)?,
                day,
            },
            Self::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            } => RecurrenceYearlyRule::WeekdayOrdinal {
                month: parse_month_record(month, path)?,
                ordinal: ordinal.into_rule(),
                weekday: weekday.into_weekday(),
            },
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocalRecurrenceOrdinalRecord {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl LocalRecurrenceOrdinalRecord {
    fn from_rule(ordinal: RecurrenceOrdinal) -> Self {
        match ordinal {
            RecurrenceOrdinal::Number(1) => Self::First,
            RecurrenceOrdinal::Number(2) => Self::Second,
            RecurrenceOrdinal::Number(3) => Self::Third,
            RecurrenceOrdinal::Number(4) => Self::Fourth,
            RecurrenceOrdinal::Last | RecurrenceOrdinal::Number(_) => Self::Last,
        }
    }

    const fn into_rule(self) -> RecurrenceOrdinal {
        match self {
            Self::First => RecurrenceOrdinal::Number(1),
            Self::Second => RecurrenceOrdinal::Number(2),
            Self::Third => RecurrenceOrdinal::Number(3),
            Self::Fourth => RecurrenceOrdinal::Number(4),
            Self::Last => RecurrenceOrdinal::Last,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocalWeekdayRecord {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl LocalWeekdayRecord {
    const fn from_weekday(weekday: Weekday) -> Self {
        match weekday {
            Weekday::Sunday => Self::Sunday,
            Weekday::Monday => Self::Monday,
            Weekday::Tuesday => Self::Tuesday,
            Weekday::Wednesday => Self::Wednesday,
            Weekday::Thursday => Self::Thursday,
            Weekday::Friday => Self::Friday,
            Weekday::Saturday => Self::Saturday,
        }
    }

    const fn into_weekday(self) -> Weekday {
        match self {
            Self::Sunday => Weekday::Sunday,
            Self::Monday => Weekday::Monday,
            Self::Tuesday => Weekday::Tuesday,
            Self::Wednesday => Weekday::Wednesday,
            Self::Thursday => Weekday::Thursday,
            Self::Friday => Weekday::Friday,
            Self::Saturday => Weekday::Saturday,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalOccurrenceOverrideRecord {
    anchor: LocalOccurrenceAnchorRecord,
    event: LocalEventDraftRecord,
}

impl LocalOccurrenceOverrideRecord {
    fn from_override(override_record: &OccurrenceOverride) -> Self {
        Self {
            anchor: LocalOccurrenceAnchorRecord::from_anchor(override_record.anchor),
            event: LocalEventDraftRecord::from_draft(&override_record.draft),
        }
    }

    fn into_override(self, path: &Path) -> Result<OccurrenceOverride, LocalEventStoreError> {
        Ok(OccurrenceOverride {
            anchor: self.anchor.into_anchor(path)?,
            draft: self.event.into_draft(path)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LocalOccurrenceAnchorRecord {
    AllDay { date: String },
    Timed { date: String, time: String },
}

impl LocalOccurrenceAnchorRecord {
    fn from_anchor(anchor: OccurrenceAnchor) -> Self {
        match anchor {
            OccurrenceAnchor::AllDay { date } => Self::AllDay {
                date: date.to_string(),
            },
            OccurrenceAnchor::Timed { start } => Self::Timed {
                date: start.date.to_string(),
                time: format_time(start.time),
            },
        }
    }

    fn into_anchor(self, path: &Path) -> Result<OccurrenceAnchor, LocalEventStoreError> {
        Ok(match self {
            Self::AllDay { date } => OccurrenceAnchor::AllDay {
                date: parse_local_date(&date, path)?,
            },
            Self::Timed { date, time } => OccurrenceAnchor::Timed {
                start: EventDateTime::new(
                    parse_local_date(&date, path)?,
                    parse_local_time(&time, path)?,
                ),
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "timing", rename_all = "snake_case")]
enum LocalEventDraftRecord {
    Timed {
        title: String,
        start_date: String,
        start_time: String,
        end_date: String,
        end_time: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reminders_minutes_before: Vec<u16>,
    },
    AllDay {
        title: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reminders_minutes_before: Vec<u16>,
    },
}

impl LocalEventDraftRecord {
    fn from_draft(draft: &CreateEventDraft) -> Self {
        let reminders_minutes_before = draft
            .reminders
            .iter()
            .map(|reminder| reminder.minutes_before)
            .collect::<Vec<_>>();
        match draft.timing {
            CreateEventTiming::Timed { start, end } => Self::Timed {
                title: draft.title.clone(),
                start_date: start.date.to_string(),
                start_time: format_time(start.time),
                end_date: end.date.to_string(),
                end_time: format_time(end.time),
                location: draft.location.clone(),
                notes: draft.notes.clone(),
                reminders_minutes_before,
            },
            CreateEventTiming::AllDay { date } => Self::AllDay {
                title: draft.title.clone(),
                date: date.to_string(),
                location: draft.location.clone(),
                notes: draft.notes.clone(),
                reminders_minutes_before,
            },
        }
    }

    fn into_draft(self, path: &Path) -> Result<CreateEventDraft, LocalEventStoreError> {
        Ok(match self {
            Self::Timed {
                title,
                start_date,
                start_time,
                end_date,
                end_time,
                location,
                notes,
                reminders_minutes_before,
            } => {
                let start = EventDateTime::new(
                    parse_local_date(&start_date, path)?,
                    parse_local_time(&start_time, path)?,
                );
                let end = EventDateTime::new(
                    parse_local_date(&end_date, path)?,
                    parse_local_time(&end_time, path)?,
                );
                if start >= end {
                    return Err(LocalEventStoreError::Parse {
                        path: path.to_path_buf(),
                        reason: format!(
                            "invalid override range: start {start:?} must be before end {end:?}"
                        ),
                    });
                }

                CreateEventDraft {
                    title,
                    timing: CreateEventTiming::Timed { start, end },
                    location: empty_to_none(location),
                    notes: empty_to_none(notes),
                    reminders: reminders_from_minutes(reminders_minutes_before),
                    recurrence: None,
                }
            }
            Self::AllDay {
                title,
                date,
                location,
                notes,
                reminders_minutes_before,
            } => CreateEventDraft {
                title,
                timing: CreateEventTiming::AllDay {
                    date: parse_local_date(&date, path)?,
                },
                location: empty_to_none(location),
                notes: empty_to_none(notes),
                reminders: reminders_from_minutes(reminders_minutes_before),
                recurrence: None,
            },
        })
    }
}

fn reminders_from_minutes(minutes: Vec<u16>) -> Vec<Reminder> {
    let mut reminders = minutes
        .into_iter()
        .map(Reminder::minutes_before)
        .collect::<Vec<_>>();
    reminders.sort();
    reminders.dedup();
    reminders
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn weekday_name(weekday: Weekday) -> String {
    match weekday {
        Weekday::Sunday => "sunday",
        Weekday::Monday => "monday",
        Weekday::Tuesday => "tuesday",
        Weekday::Wednesday => "wednesday",
        Weekday::Thursday => "thursday",
        Weekday::Friday => "friday",
        Weekday::Saturday => "saturday",
    }
    .to_string()
}

fn parse_weekday_record(value: &str, path: &Path) -> Result<Weekday, LocalEventStoreError> {
    match value {
        "sunday" => Ok(Weekday::Sunday),
        "monday" => Ok(Weekday::Monday),
        "tuesday" => Ok(Weekday::Tuesday),
        "wednesday" => Ok(Weekday::Wednesday),
        "thursday" => Ok(Weekday::Thursday),
        "friday" => Ok(Weekday::Friday),
        "saturday" => Ok(Weekday::Saturday),
        _ => Err(LocalEventStoreError::Parse {
            path: path.to_path_buf(),
            reason: format!("invalid weekday '{value}'"),
        }),
    }
}

fn parse_month_record(value: u8, path: &Path) -> Result<Month, LocalEventStoreError> {
    Month::try_from(value).map_err(|_| LocalEventStoreError::Parse {
        path: path.to_path_buf(),
        reason: format!("invalid month '{value}'"),
    })
}

fn parse_event_datetime_record(value: &str) -> Option<EventDateTime> {
    let (date, time) = value.split_once('T')?;
    Some(EventDateTime::new(
        parse_iso_date(date)?,
        parse_hhmm_time(time)?,
    ))
}

fn parse_local_date(value: &str, path: &Path) -> Result<CalendarDate, LocalEventStoreError> {
    parse_iso_date(value).ok_or_else(|| LocalEventStoreError::Parse {
        path: path.to_path_buf(),
        reason: format!("invalid date '{value}'"),
    })
}

fn parse_local_time(value: &str, path: &Path) -> Result<Time, LocalEventStoreError> {
    parse_hhmm_time(value).ok_or_else(|| LocalEventStoreError::Parse {
        path: path.to_path_buf(),
        reason: format!("invalid time '{value}'"),
    })
}

fn parse_hhmm_time(value: &str) -> Option<Time> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u8>().ok()?;
    let minute = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }

    Time::from_hms(hour, minute, 0).ok()
}

fn format_time(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgendaError {
    InvalidDateRange {
        start: CalendarDate,
        end: CalendarDate,
    },
    InvalidEventRange {
        start: EventDateTime,
        end: EventDateTime,
    },
}

impl fmt::Display for AgendaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDateRange { start, end } => {
                write!(
                    f,
                    "invalid date range: start {start} must be before end {end}"
                )
            }
            Self::InvalidEventRange { start, end } => write!(
                f,
                "invalid event range: start {start:?} must be before end {end:?}"
            ),
        }
    }
}

impl Error for AgendaError {}

fn events_to_timed_agenda_events(
    date: CalendarDate,
    events: impl IntoIterator<Item = Event>,
) -> Vec<TimedAgendaEvent> {
    let day_start = EventDateTime::new(date, Time::MIDNIGHT);
    let day_end = EventDateTime::new(date.add_days(1), Time::MIDNIGHT);

    events
        .into_iter()
        .filter_map(|event| match event.timing {
            EventTiming::Timed { start, end } if start < day_end && end > day_start => {
                let starts_before_day = start < day_start;
                let ends_after_day = end > day_end;
                let visible_start = if starts_before_day {
                    DayMinute::START
                } else {
                    DayMinute::from_time(start.time)
                };
                let visible_end = if end >= day_end {
                    DayMinute::END
                } else {
                    DayMinute::from_time(end.time)
                };

                Some(TimedAgendaEvent {
                    event,
                    visible_start,
                    visible_end,
                    starts_before_day,
                    ends_after_day,
                    overlap_group: 0,
                })
            }
            _ => None,
        })
        .collect()
}

fn assign_overlap_groups(events: &mut [TimedAgendaEvent]) {
    let mut group_end: Option<DayMinute> = None;
    let mut group_index = 0;

    for event in events {
        if let Some(end) = group_end {
            if event.visible_start >= end {
                group_index += 1;
                group_end = Some(event.visible_end);
            } else if event.visible_end > end {
                group_end = Some(event.visible_end);
            }
        } else {
            group_end = Some(event.visible_end);
        }

        event.overlap_group = group_index;
    }
}

fn time(hour: u8, minute: u8) -> Time {
    Time::from_hms(hour, minute, 0).expect("fixture time is valid")
}

fn us_federal_holidays_for_year(year: i32) -> Vec<Holiday> {
    [
        fixed_us_federal_holiday(year, "new-years-day", "New Year's Day", Month::January, 1),
        weekday_us_federal_holiday(
            year,
            "martin-luther-king-jr-day",
            "Birthday of Martin Luther King, Jr.",
            Month::January,
            Weekday::Monday,
            3,
        ),
        weekday_us_federal_holiday(
            year,
            "washingtons-birthday",
            "Washington's Birthday",
            Month::February,
            Weekday::Monday,
            3,
        ),
        last_weekday_us_federal_holiday(
            year,
            "memorial-day",
            "Memorial Day",
            Month::May,
            Weekday::Monday,
        ),
        fixed_us_federal_holiday(
            year,
            "juneteenth",
            "Juneteenth National Independence Day",
            Month::June,
            19,
        ),
        fixed_us_federal_holiday(year, "independence-day", "Independence Day", Month::July, 4),
        weekday_us_federal_holiday(
            year,
            "labor-day",
            "Labor Day",
            Month::September,
            Weekday::Monday,
            1,
        ),
        weekday_us_federal_holiday(
            year,
            "columbus-day",
            "Columbus Day",
            Month::October,
            Weekday::Monday,
            2,
        ),
        fixed_us_federal_holiday(year, "veterans-day", "Veterans Day", Month::November, 11),
        weekday_us_federal_holiday(
            year,
            "thanksgiving-day",
            "Thanksgiving Day",
            Month::November,
            Weekday::Thursday,
            4,
        ),
        fixed_us_federal_holiday(year, "christmas-day", "Christmas Day", Month::December, 25),
    ]
    .into()
}

fn fixed_us_federal_holiday(year: i32, slug: &str, name: &str, month: Month, day: u8) -> Holiday {
    let actual = CalendarDate::from_ymd(year, month, day).expect("fixed holiday date is valid");
    holiday_with_source(
        format!("us-federal-{year}-{slug}"),
        name,
        observed_date(actual),
        "us-federal",
        "U.S. federal holidays",
    )
}

fn weekday_us_federal_holiday(
    year: i32,
    slug: &str,
    name: &str,
    month: Month,
    weekday: Weekday,
    nth: u8,
) -> Holiday {
    let date = nth_weekday_of_month(year, month, weekday, nth);
    holiday_with_source(
        format!("us-federal-{year}-{slug}"),
        name,
        date,
        "us-federal",
        "U.S. federal holidays",
    )
}

fn last_weekday_us_federal_holiday(
    year: i32,
    slug: &str,
    name: &str,
    month: Month,
    weekday: Weekday,
) -> Holiday {
    let date = last_weekday_of_month(year, month, weekday);
    holiday_with_source(
        format!("us-federal-{year}-{slug}"),
        name,
        date,
        "us-federal",
        "U.S. federal holidays",
    )
}

fn observed_date(actual: CalendarDate) -> CalendarDate {
    match actual.weekday() {
        Weekday::Saturday => actual.add_days(-1),
        Weekday::Sunday => actual.add_days(1),
        _ => actual,
    }
}

fn nth_weekday_of_month(year: i32, month: Month, weekday: Weekday, nth: u8) -> CalendarDate {
    let first = CalendarDate::from_ymd(year, month, 1).expect("month start date is valid");
    let first_weekday = first.weekday().number_days_from_sunday();
    let target_weekday = weekday.number_days_from_sunday();
    let offset = (target_weekday + 7 - first_weekday) % 7;
    let day = 1 + offset + (nth - 1) * 7;

    CalendarDate::from_ymd(year, month, day).expect("nth weekday date is valid")
}

fn last_weekday_of_month(year: i32, month: Month, weekday: Weekday) -> CalendarDate {
    let mut date =
        CalendarDate::from_ymd(year, month, month.length(year)).expect("month end date is valid");

    while date.weekday() != weekday {
        date = date.add_days(-1);
    }

    date
}

fn holiday_with_source(
    id: impl Into<String>,
    name: impl Into<String>,
    date: CalendarDate,
    source_id: impl Into<String>,
    source_name: impl Into<String>,
) -> Holiday {
    Holiday::new(id, name, date, SourceMetadata::new(source_id, source_name))
}

fn parse_nager_holidays(country_code: &str, body: &str) -> Option<Vec<Holiday>> {
    let records = serde_json::from_str::<Vec<NagerHolidayRecord>>(body).ok()?;
    let mut holidays = Vec::with_capacity(records.len());

    for record in records {
        let date = parse_iso_date(&record.date)?;
        holidays.push(Holiday::new(
            format!(
                "nager-{country_code}-{}-{}",
                record.date,
                slugify(&record.name)
            ),
            record.name,
            date,
            SourceMetadata::new(format!("nager-{country_code}"), "Nager.Date")
                .with_external_id(format!("{country_code}:{}", record.date)),
        ));
    }

    Some(holidays)
}

fn parse_iso_date(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u8>().ok()?;
    let day = parts.next()?.parse::<u8>().ok()?;

    if parts.next().is_some() {
        return None;
    }

    CalendarDate::from_ymd(year, Month::try_from(month).ok()?, day).ok()
}

fn default_nager_cache_dir() -> PathBuf {
    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home)
            .join("rcal")
            .join("holidays")
            .join("nager");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("rcal")
            .join("holidays")
            .join("nager");
    }

    env::temp_dir().join("rcal").join("holidays").join("nager")
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for value in value.chars().flat_map(char::to_lowercase) {
        if value.is_ascii_alphanumeric() {
            slug.push(value);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }

    if slug.ends_with('-') {
        slug.pop();
    }

    slug
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NagerHolidayRecord {
    date: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    fn date(day: u8) -> CalendarDate {
        date_ymd(2026, Month::April, day)
    }

    fn date_ymd(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn at(date: CalendarDate, hour: u8, minute: u8) -> EventDateTime {
        EventDateTime::new(date, time(hour, minute))
    }

    fn source() -> SourceMetadata {
        SourceMetadata::fixture()
    }

    fn temp_events_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("rcal-local-events-test-{}", std::process::id()))
            .join(name)
            .join("events.json")
    }

    fn timed(id: &str, title: &str, start: EventDateTime, end: EventDateTime) -> Event {
        Event::timed(id, title, start, end, source()).expect("valid timed event")
    }

    #[test]
    fn agenda_construction_handles_empty_days() {
        let day = date(23);
        let source = InMemoryAgendaSource::new();

        let agenda = DayAgenda::from_source(day, &source);

        assert_eq!(agenda, DayAgenda::empty(day));
        assert!(agenda.is_empty());
    }

    #[test]
    fn agenda_construction_handles_holiday_only_days() {
        let day = date(23);
        let mut agenda_source = InMemoryAgendaSource::new();
        agenda_source.push_holiday(Holiday::new("earth-day", "Earth Day", day, source()));
        agenda_source.push_holiday(Holiday::new("tomorrow", "Tomorrow", date(24), source()));

        let agenda = DayAgenda::from_source(day, &agenda_source);

        assert!(!agenda.is_empty());
        assert_eq!(agenda.holidays.len(), 1);
        assert_eq!(agenda.holidays[0].name, "Earth Day");
        assert!(agenda.all_day_events.is_empty());
        assert!(agenda.timed_events.is_empty());
    }

    #[test]
    fn agenda_keeps_all_day_events_separate_and_sorted() {
        let day = date(23);
        let events = vec![
            Event::all_day("b", "Release", day, source()),
            Event::all_day("a", "Birthday", day, source()),
            Event::all_day("other", "Other Day", date(24), source()),
        ];

        let agenda = DayAgenda::build(day, events, []);

        let titles = agenda
            .all_day_events
            .iter()
            .map(|event| event.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["Birthday", "Release"]);
        assert!(agenda.timed_events.is_empty());
    }

    #[test]
    fn timed_events_sort_by_visible_time_then_title() {
        let day = date(23);
        let events = vec![
            timed("late", "Late", at(day, 13, 0), at(day, 14, 0)),
            timed("alpha", "Alpha", at(day, 9, 0), at(day, 10, 0)),
            timed("beta", "Beta", at(day, 9, 0), at(day, 9, 30)),
        ];

        let agenda = DayAgenda::build(day, events, []);

        let ids = agenda
            .timed_events
            .iter()
            .map(|event| event.event.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["beta", "alpha", "late"]);
    }

    #[test]
    fn overlapping_timed_events_share_group_until_the_cluster_ends() {
        let day = date(23);
        let events = vec![
            timed("first", "First", at(day, 9, 0), at(day, 10, 0)),
            timed("overlap", "Overlap", at(day, 9, 30), at(day, 10, 30)),
            timed("touching", "Touching", at(day, 10, 30), at(day, 11, 0)),
        ];

        let agenda = DayAgenda::build(day, events, []);
        let groups = agenda
            .timed_events
            .iter()
            .map(|event| event.overlap_group)
            .collect::<Vec<_>>();

        assert_eq!(groups, [0, 0, 1]);
        assert!(agenda.timed_events[0].overlaps(&agenda.timed_events[1]));
        assert!(!agenda.timed_events[1].overlaps(&agenda.timed_events[2]));
    }

    #[test]
    fn cross_midnight_events_clip_to_selected_day() {
        let day = date(23);
        let previous_day = day.add_days(-1);
        let next_day = day.add_days(1);
        let events = vec![
            timed(
                "from-yesterday",
                "From yesterday",
                at(previous_day, 23, 0),
                at(day, 1, 30),
            ),
            timed(
                "into-tomorrow",
                "Into tomorrow",
                at(day, 23, 0),
                at(next_day, 1, 0),
            ),
            timed(
                "ends-at-start",
                "Ends at start",
                at(previous_day, 22, 0),
                at(day, 0, 0),
            ),
        ];

        let agenda = DayAgenda::build(day, events, []);
        let first = &agenda.timed_events[0];
        let second = &agenda.timed_events[1];

        assert_eq!(agenda.timed_events.len(), 2);
        assert_eq!(first.event.id, "from-yesterday");
        assert_eq!(first.visible_start, DayMinute::START);
        assert_eq!(first.visible_end.as_minutes(), 90);
        assert!(first.starts_before_day);
        assert!(!first.ends_after_day);

        assert_eq!(second.event.id, "into-tomorrow");
        assert_eq!(second.visible_start.as_minutes(), 23 * 60);
        assert_eq!(second.visible_end, DayMinute::END);
        assert!(!second.starts_before_day);
        assert!(second.ends_after_day);
    }

    #[test]
    fn in_memory_fixture_provides_development_agenda_data() {
        let day = date(23);
        let source = InMemoryAgendaSource::development_fixture();

        let agenda = DayAgenda::from_source(day, &source);

        assert!(agenda.holidays.is_empty());
        assert_eq!(agenda.all_day_events[0].title, "Release day");
        assert_eq!(agenda.timed_events.len(), 3);
        assert_eq!(agenda.timed_events[0].overlap_group, 0);
        assert_eq!(agenda.timed_events[1].overlap_group, 0);
        assert_eq!(agenda.timed_events[2].visible_end, DayMinute::END);
    }

    #[test]
    fn date_ranges_are_half_open_for_sources() {
        let day = date(23);
        let range = DateRange::new(day, day.add_days(1)).expect("valid range");
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![
                timed("inside", "Inside", at(day, 12, 0), at(day, 13, 0)),
                timed(
                    "outside",
                    "Outside",
                    at(day.add_days(1), 12, 0),
                    at(day.add_days(1), 13, 0),
                ),
            ],
            vec![
                Holiday::new("inside-holiday", "Inside Holiday", day, source()),
                Holiday::new(
                    "outside-holiday",
                    "Outside Holiday",
                    day.add_days(1),
                    source(),
                ),
            ],
        );

        assert_eq!(source.events_intersecting(range).len(), 1);
        assert_eq!(source.holidays_in(range).len(), 1);
        assert!(!range.contains_date(day.add_days(1)));
    }

    #[test]
    fn daily_recurrence_expands_with_interval_and_count() {
        let start = date_ymd(2026, Month::April, 1);
        let event = Event::all_day("daily", "Every other day", start, source()).with_recurrence(
            RecurrenceRule {
                frequency: RecurrenceFrequency::Daily,
                interval: 2,
                end: RecurrenceEnd::Count(3),
                weekdays: Vec::new(),
                monthly: None,
                yearly: None,
            },
        );
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let range = DateRange::new(start, date_ymd(2026, Month::April, 10)).expect("valid range");

        let dates = source
            .events_intersecting(range)
            .into_iter()
            .filter_map(|event| event.timing.date())
            .collect::<Vec<_>>();

        assert_eq!(dates, [date_ymd(2026, Month::April, 1), date(3), date(5)]);
    }

    #[test]
    fn weekly_recurrence_supports_multiple_days_and_interval() {
        let start = date_ymd(2026, Month::April, 5);
        let event =
            Event::all_day("weekly", "Workout", start, source()).with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Weekly,
                interval: 2,
                end: RecurrenceEnd::Never,
                weekdays: vec![Weekday::Sunday, Weekday::Tuesday],
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let range = DateRange::new(start, date_ymd(2026, Month::April, 23)).expect("valid range");

        let dates = source
            .events_intersecting(range)
            .into_iter()
            .filter_map(|event| event.timing.date())
            .collect::<Vec<_>>();

        assert_eq!(dates, [date(5), date(7), date(19), date(21)]);
    }

    #[test]
    fn weekly_interval_uses_calendar_weeks_not_rolling_start_windows() {
        let start = date_ymd(2026, Month::April, 15);
        let event =
            Event::all_day("class", "CS412", start, source()).with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Weekly,
                interval: 2,
                end: RecurrenceEnd::Until(date_ymd(2026, Month::May, 15)),
                weekdays: vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday],
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let range = DateRange::new(
            date_ymd(2026, Month::April, 1),
            date_ymd(2026, Month::May, 1),
        )
        .expect("valid range");

        let dates = source
            .events_intersecting(range)
            .into_iter()
            .filter_map(|event| event.timing.date())
            .collect::<Vec<_>>();

        assert_eq!(
            dates,
            [
                date_ymd(2026, Month::April, 15),
                date_ymd(2026, Month::April, 17),
                date_ymd(2026, Month::April, 27),
                date_ymd(2026, Month::April, 29),
            ]
        );
        assert!(!dates.contains(&date_ymd(2026, Month::April, 20)));
    }

    #[test]
    fn monthly_recurrence_skips_invalid_day_of_month_dates() {
        let start = date_ymd(2026, Month::January, 31);
        let event = Event::all_day("month-day", "Month end", start, source()).with_recurrence(
            RecurrenceRule {
                frequency: RecurrenceFrequency::Monthly,
                interval: 1,
                end: RecurrenceEnd::Never,
                weekdays: Vec::new(),
                monthly: Some(RecurrenceMonthlyRule::DayOfMonth(31)),
                yearly: None,
            },
        );
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let range = DateRange::new(start, date_ymd(2026, Month::April, 1)).expect("valid range");

        let dates = source
            .events_intersecting(range)
            .into_iter()
            .filter_map(|event| event.timing.date())
            .collect::<Vec<_>>();

        assert_eq!(dates, [start, date_ymd(2026, Month::March, 31)]);
    }

    #[test]
    fn monthly_recurrence_supports_last_weekday_rules() {
        let start = date_ymd(2026, Month::April, 30);
        let event = Event::all_day("last-thursday", "Review", start, source()).with_recurrence(
            RecurrenceRule {
                frequency: RecurrenceFrequency::Monthly,
                interval: 1,
                end: RecurrenceEnd::Count(3),
                weekdays: Vec::new(),
                monthly: Some(RecurrenceMonthlyRule::WeekdayOrdinal {
                    ordinal: RecurrenceOrdinal::Last,
                    weekday: Weekday::Thursday,
                }),
                yearly: None,
            },
        );
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());
        let range = DateRange::new(start, date_ymd(2026, Month::July, 1)).expect("valid range");

        let dates = source
            .events_intersecting(range)
            .into_iter()
            .filter_map(|event| event.timing.date())
            .collect::<Vec<_>>();

        assert_eq!(
            dates,
            [
                date_ymd(2026, Month::April, 30),
                date_ymd(2026, Month::May, 28),
                date_ymd(2026, Month::June, 25)
            ]
        );
    }

    #[test]
    fn yearly_recurrence_skips_invalid_dates_and_supports_weekday_ordinal() {
        let leap_day = date_ymd(2024, Month::February, 29);
        let leap =
            Event::all_day("leap", "Leap", leap_day, source()).with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Yearly,
                interval: 1,
                end: RecurrenceEnd::Never,
                weekdays: Vec::new(),
                monthly: None,
                yearly: Some(RecurrenceYearlyRule::Date {
                    month: Month::February,
                    day: 29,
                }),
            });
        let thanksgiving = Event::all_day(
            "thanksgiving",
            "Thanksgiving",
            date_ymd(2026, Month::November, 26),
            source(),
        )
        .with_recurrence(RecurrenceRule {
            frequency: RecurrenceFrequency::Yearly,
            interval: 1,
            end: RecurrenceEnd::Count(2),
            weekdays: Vec::new(),
            monthly: None,
            yearly: Some(RecurrenceYearlyRule::WeekdayOrdinal {
                month: Month::November,
                ordinal: RecurrenceOrdinal::Number(4),
                weekday: Weekday::Thursday,
            }),
        });
        let source =
            InMemoryAgendaSource::with_events_and_holidays(vec![leap, thanksgiving], Vec::new());
        let range = DateRange::new(
            date_ymd(2024, Month::January, 1),
            date_ymd(2029, Month::January, 1),
        )
        .expect("valid range");

        let ids_and_dates = source
            .events_intersecting(range)
            .into_iter()
            .map(|event| (event.id, event.timing.date().expect("all-day date")))
            .collect::<Vec<_>>();

        assert!(ids_and_dates.contains(&("leap#2024-02-29".to_string(), leap_day)));
        assert!(ids_and_dates.contains(&(
            "leap#2028-02-29".to_string(),
            date_ymd(2028, Month::February, 29)
        )));
        assert!(
            !ids_and_dates
                .iter()
                .any(|(_, date)| *date == date_ymd(2025, Month::February, 28))
        );
        assert!(ids_and_dates.contains(&(
            "thanksgiving#2026-11-26".to_string(),
            date_ymd(2026, Month::November, 26)
        )));
        assert!(ids_and_dates.contains(&(
            "thanksgiving#2027-11-25".to_string(),
            date_ymd(2027, Month::November, 25)
        )));
    }

    #[test]
    fn recurring_cross_midnight_events_intersect_each_visible_day() {
        let start = date(23);
        let event = Event::timed(
            "late",
            "Late shift",
            at(start, 23, 0),
            at(start.add_days(1), 1, 0),
            source(),
        )
        .expect("valid recurring event")
        .with_recurrence(RecurrenceRule {
            frequency: RecurrenceFrequency::Daily,
            interval: 1,
            end: RecurrenceEnd::Count(2),
            weekdays: Vec::new(),
            monthly: None,
            yearly: None,
        });
        let source = InMemoryAgendaSource::with_events_and_holidays(vec![event], Vec::new());

        let agenda = DayAgenda::from_source(start.add_days(1), &source);

        assert_eq!(agenda.timed_events.len(), 2);
        assert!(agenda.timed_events[0].starts_before_day);
        assert_eq!(agenda.timed_events[0].visible_end.as_minutes(), 60);
        assert_eq!(agenda.timed_events[1].visible_start.as_minutes(), 23 * 60);
        assert!(agenda.timed_events[1].ends_after_day);
    }

    #[test]
    fn invalid_ranges_are_rejected() {
        let day = date(23);

        assert!(DateRange::new(day, day).is_err());
        assert!(Event::timed("bad", "Bad", at(day, 9, 0), at(day, 9, 0), source()).is_err());
    }

    #[test]
    fn local_event_store_loads_missing_file_as_empty() {
        let path = temp_events_path("missing");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));

        let source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");

        assert!(
            source
                .events_intersecting(DateRange::day(date(23)))
                .is_empty()
        );
    }

    #[test]
    fn local_event_store_saves_and_loads_timed_and_all_day_events() {
        let path = temp_events_path("save-load");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");

        source
            .create_event(CreateEventDraft {
                title: "Planning".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 10, 0),
                },
                location: Some("War room".to_string()),
                notes: Some("Bring notes".to_string()),
                reminders: vec![Reminder::minutes_before(10), Reminder::minutes_before(60)],
                recurrence: None,
            })
            .expect("timed event saves");
        source
            .create_event(CreateEventDraft {
                title: "Release day".to_string(),
                timing: CreateEventTiming::AllDay { date: day },
                location: None,
                notes: None,
                reminders: vec![Reminder::minutes_before(24 * 60)],
                recurrence: None,
            })
            .expect("all-day event saves");

        let body = std::fs::read_to_string(&path).expect("event file exists");
        assert!(body.contains(r#""version": 2"#));
        assert!(body.contains(r#""reminders_minutes_before""#));

        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day, &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_eq!(agenda.all_day_events.len(), 1);
        assert_eq!(agenda.all_day_events[0].title, "Release day");
        assert_eq!(agenda.timed_events.len(), 1);
        assert_eq!(agenda.timed_events[0].event.title, "Planning");
        assert_eq!(
            agenda.timed_events[0].event.location.as_deref(),
            Some("War room")
        );
        assert_eq!(
            agenda.timed_events[0]
                .event
                .reminders
                .iter()
                .map(|reminder| reminder.minutes_before)
                .collect::<Vec<_>>(),
            [10, 60]
        );
    }

    #[test]
    fn local_event_store_updates_existing_event_id_and_persists() {
        let path = temp_events_path("update");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Planning".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 10, 0),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: None,
            })
            .expect("event saves");

        let updated = source
            .update_event(
                &event.id,
                CreateEventDraft {
                    title: "Updated planning".to_string(),
                    timing: CreateEventTiming::AllDay { date: day },
                    location: Some("Room 2".to_string()),
                    notes: Some("Moved".to_string()),
                    reminders: vec![Reminder::minutes_before(5)],
                    recurrence: None,
                },
            )
            .expect("event updates");

        assert_eq!(updated.id, event.id);
        assert!(updated.is_local());

        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day, &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert!(agenda.timed_events.is_empty());
        assert_eq!(agenda.all_day_events.len(), 1);
        assert_eq!(agenda.all_day_events[0].id, event.id);
        assert_eq!(agenda.all_day_events[0].title, "Updated planning");
        assert_eq!(agenda.all_day_events[0].location.as_deref(), Some("Room 2"));
    }

    #[test]
    fn local_event_store_duplicates_single_event_and_persists() {
        let path = temp_events_path("duplicate-event");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Planning".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 10, 0),
                },
                location: Some("Room 1".to_string()),
                notes: Some("Bring notes".to_string()),
                reminders: vec![Reminder::minutes_before(15)],
                recurrence: None,
            })
            .expect("event saves");

        let copied = source.duplicate_event(&event.id).expect("event copies");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day, &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_ne!(copied.id, event.id);
        assert!(copied.is_local());
        assert_eq!(copied.title, "Planning");
        assert_eq!(copied.location.as_deref(), Some("Room 1"));
        assert_eq!(agenda.timed_events.len(), 2);
    }

    #[test]
    fn local_event_store_duplicates_occurrence_as_standalone_event() {
        let path = temp_events_path("duplicate-occurrence");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(2),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");
        let anchor = OccurrenceAnchor::Timed {
            start: at(day.add_days(1), 9, 0),
        };

        let copied = source
            .duplicate_occurrence(&event.id, anchor)
            .expect("occurrence copies");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day.add_days(1), &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_ne!(copied.id, event.id);
        assert!(copied.occurrence().is_none());
        assert!(copied.recurrence.is_none());
        assert_eq!(agenda.timed_events.len(), 2);
        assert!(
            agenda
                .timed_events
                .iter()
                .any(|agenda_event| agenda_event.event.id == copied.id)
        );
    }

    #[test]
    fn local_event_store_duplicates_recurring_series() {
        let path = temp_events_path("duplicate-series");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(2),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");

        let copied = source
            .duplicate_event(&event.id)
            .expect("recurring series copies");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day.add_days(1), &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_ne!(copied.id, event.id);
        assert!(copied.recurrence.is_some());
        assert_eq!(agenda.timed_events.len(), 2);
        assert!(
            agenda
                .timed_events
                .iter()
                .any(|agenda_event| agenda_event.event.id.starts_with(&copied.id))
        );
    }

    #[test]
    fn local_event_store_loads_version_one_and_rewrites_version_two_on_save() {
        let path = temp_events_path("version-one");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        std::fs::create_dir_all(path.parent().expect("path has parent"))
            .expect("parent can be created");
        std::fs::write(
            &path,
            r#"{
  "version": 1,
  "events": [
    {
      "id": "old",
      "title": "Old file",
      "date": "2026-04-23"
    }
  ]
}"#,
        )
        .expect("file can be written");
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("version one file loads");

        source
            .update_event(
                "old",
                CreateEventDraft {
                    title: "Rewritten".to_string(),
                    timing: CreateEventTiming::AllDay { date: date(23) },
                    location: None,
                    notes: None,
                    reminders: Vec::new(),
                    recurrence: None,
                },
            )
            .expect("event update saves");

        let body = std::fs::read_to_string(&path).expect("event file exists");
        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert!(body.contains(r#""version": 2"#));
        assert!(body.contains("Rewritten"));
    }

    #[test]
    fn local_event_store_saves_recurring_series_and_occurrence_overrides() {
        let path = temp_events_path("recurring-overrides");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(3),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");
        let anchor = OccurrenceAnchor::Timed {
            start: at(day.add_days(1), 9, 0),
        };

        source
            .update_occurrence(
                &event.id,
                anchor,
                CreateEventDraft {
                    title: "Moved standup".to_string(),
                    timing: CreateEventTiming::Timed {
                        start: at(day.add_days(1), 10, 0),
                        end: at(day.add_days(1), 10, 30),
                    },
                    location: Some("Room 2".to_string()),
                    notes: None,
                    reminders: vec![Reminder::minutes_before(5)],
                    recurrence: None,
                },
            )
            .expect("occurrence override saves");

        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day.add_days(1), &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_eq!(agenda.timed_events.len(), 1);
        let overridden = &agenda.timed_events[0].event;
        assert_eq!(overridden.title, "Moved standup");
        assert_eq!(overridden.location.as_deref(), Some("Room 2"));
        assert_eq!(
            overridden.occurrence().map(|occurrence| occurrence.anchor),
            Some(anchor)
        );
    }

    #[test]
    fn local_event_store_deletes_single_event_and_persists() {
        let path = temp_events_path("delete-event");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Planning".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 10, 0),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: None,
            })
            .expect("event saves");

        let deleted = source.delete_event(&event.id).expect("event deletes");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let agenda = DayAgenda::from_source(day, &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_eq!(deleted.id, event.id);
        assert!(agenda.is_empty());
    }

    #[test]
    fn local_event_store_deletes_one_recurring_occurrence_and_persists() {
        let path = temp_events_path("delete-occurrence");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(3),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");
        let deleted_anchor = OccurrenceAnchor::Timed {
            start: at(day.add_days(1), 9, 0),
        };

        source
            .update_occurrence(
                &event.id,
                deleted_anchor,
                CreateEventDraft {
                    title: "Moved".to_string(),
                    timing: CreateEventTiming::Timed {
                        start: at(day.add_days(1), 10, 0),
                        end: at(day.add_days(1), 10, 30),
                    },
                    location: None,
                    notes: None,
                    reminders: Vec::new(),
                    recurrence: None,
                },
            )
            .expect("override saves before delete");
        source
            .delete_occurrence(&event.id, deleted_anchor)
            .expect("occurrence deletes");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");

        let first = DayAgenda::from_source(day, &reloaded);
        let second = DayAgenda::from_source(day.add_days(1), &reloaded);
        let third = DayAgenda::from_source(day.add_days(2), &reloaded);

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert_eq!(first.timed_events.len(), 1);
        assert!(second.timed_events.is_empty());
        assert_eq!(third.timed_events.len(), 1);
        let stored = reloaded
            .local_event_by_id(&event.id)
            .expect("series exists");
        assert_eq!(stored.deleted_occurrences, vec![deleted_anchor]);
        assert!(stored.occurrence_overrides.is_empty());
    }

    #[test]
    fn local_event_store_delete_series_removes_all_occurrences() {
        let path = temp_events_path("delete-series");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(3),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");

        source.delete_event(&event.id).expect("series deletes");
        let reloaded = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("saved file reloads");
        let range = DateRange::new(day, day.add_days(4)).expect("valid range");

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert!(reloaded.events_intersecting(range).is_empty());
    }

    #[test]
    fn series_edits_drop_overrides_whose_anchor_no_longer_generates() {
        let path = temp_events_path("series-edit-overrides");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        let day = date(23);
        let mut source = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect("missing event file is empty");
        let event = source
            .create_event(CreateEventDraft {
                title: "Standup".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(day, 9, 0),
                    end: at(day, 9, 30),
                },
                location: None,
                notes: None,
                reminders: Vec::new(),
                recurrence: Some(RecurrenceRule {
                    frequency: RecurrenceFrequency::Daily,
                    interval: 1,
                    end: RecurrenceEnd::Count(3),
                    weekdays: Vec::new(),
                    monthly: None,
                    yearly: None,
                }),
            })
            .expect("recurring event saves");
        let anchor = OccurrenceAnchor::Timed {
            start: at(day.add_days(1), 9, 0),
        };
        source
            .update_occurrence(
                &event.id,
                anchor,
                CreateEventDraft {
                    title: "Override".to_string(),
                    timing: CreateEventTiming::Timed {
                        start: at(day.add_days(1), 10, 0),
                        end: at(day.add_days(1), 10, 30),
                    },
                    location: None,
                    notes: None,
                    reminders: Vec::new(),
                    recurrence: None,
                },
            )
            .expect("override saves");

        let updated = source
            .update_event(
                &event.id,
                CreateEventDraft {
                    title: "Standup".to_string(),
                    timing: CreateEventTiming::Timed {
                        start: at(day, 9, 0),
                        end: at(day, 9, 30),
                    },
                    location: None,
                    notes: None,
                    reminders: Vec::new(),
                    recurrence: Some(RecurrenceRule {
                        frequency: RecurrenceFrequency::Weekly,
                        interval: 1,
                        end: RecurrenceEnd::Never,
                        weekdays: vec![day.weekday()],
                        monthly: None,
                        yearly: None,
                    }),
                },
            )
            .expect("series update saves");

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert!(updated.occurrence_overrides.is_empty());
    }

    #[test]
    fn local_event_store_rejects_missing_and_non_local_updates() {
        let day = date(23);
        let mut source = ConfiguredAgendaSource::new(
            InMemoryAgendaSource::with_events_and_holidays(
                vec![timed("fixture", "Fixture", at(day, 8, 0), at(day, 9, 0))],
                Vec::new(),
            ),
            HolidayProvider::off(),
        );
        let draft = CreateEventDraft {
            title: "Updated".to_string(),
            timing: CreateEventTiming::Timed {
                start: at(day, 10, 0),
                end: at(day, 11, 0),
            },
            location: None,
            notes: None,
            reminders: Vec::new(),
            recurrence: None,
        };

        assert!(matches!(
            source
                .update_event("missing", draft.clone())
                .expect_err("missing event fails"),
            LocalEventStoreError::EventNotFound { .. }
        ));
        assert!(matches!(
            source
                .update_event("fixture", draft)
                .expect_err("fixture event fails"),
            LocalEventStoreError::EventNotEditable { .. }
        ));
    }

    #[test]
    fn local_event_store_rejects_malformed_json() {
        let path = temp_events_path("malformed");
        let _ = std::fs::remove_dir_all(path.parent().expect("path has parent"));
        std::fs::create_dir_all(path.parent().expect("path has parent"))
            .expect("parent can be created");
        std::fs::write(&path, "{not json").expect("file can be written");

        let err = ConfiguredAgendaSource::from_events_file(&path, HolidayProvider::off())
            .expect_err("malformed file fails");

        let _ = std::fs::remove_dir_all(path.parent().expect("test dir exists"));

        assert!(matches!(err, LocalEventStoreError::Parse { .. }));
        assert!(err.to_string().contains("failed to parse"));
    }

    #[test]
    fn us_federal_source_returns_2026_observed_holidays() {
        let source = UsFederalHolidaySource;
        let range = DateRange::new(
            date_ymd(2026, Month::January, 1),
            date_ymd(2027, Month::January, 1),
        )
        .expect("valid range");

        let holidays = source.holidays_in(range);
        let observed = holidays
            .iter()
            .map(|holiday| (holiday.name.as_str(), holiday.date))
            .collect::<Vec<_>>();

        assert_eq!(holidays.len(), 11);
        assert!(observed.contains(&("New Year's Day", date_ymd(2026, Month::January, 1))));
        assert!(observed.contains(&(
            "Birthday of Martin Luther King, Jr.",
            date_ymd(2026, Month::January, 19),
        )));
        assert!(
            observed.contains(&("Washington's Birthday", date_ymd(2026, Month::February, 16),))
        );
        assert!(observed.contains(&("Memorial Day", date_ymd(2026, Month::May, 25))));
        assert!(observed.contains(&(
            "Juneteenth National Independence Day",
            date_ymd(2026, Month::June, 19),
        )));
        assert!(observed.contains(&("Independence Day", date_ymd(2026, Month::July, 3))));
        assert!(observed.contains(&("Labor Day", date_ymd(2026, Month::September, 7))));
        assert!(observed.contains(&("Columbus Day", date_ymd(2026, Month::October, 12))));
        assert!(observed.contains(&("Veterans Day", date_ymd(2026, Month::November, 11))));
        assert!(observed.contains(&("Thanksgiving Day", date_ymd(2026, Month::November, 26))));
        assert!(observed.contains(&("Christmas Day", date_ymd(2026, Month::December, 25))));
    }

    #[test]
    fn us_federal_source_includes_previous_year_observed_new_year() {
        let source = UsFederalHolidaySource;
        let day = date_ymd(2021, Month::December, 31);

        let holidays = source.holidays_in(DateRange::day(day));

        assert_eq!(holidays.len(), 1);
        assert_eq!(holidays[0].name, "New Year's Day");
        assert_eq!(holidays[0].date, day);
    }

    #[test]
    fn nager_source_reads_cached_holidays_without_network() {
        let cache_dir = std::env::temp_dir()
            .join(format!("rcal-nager-test-{}", std::process::id()))
            .join("cached-holidays-basic");
        let _ = std::fs::remove_dir_all(&cache_dir);
        let country_dir = cache_dir.join("GB");
        std::fs::create_dir_all(&country_dir).expect("cache dir can be created");
        std::fs::write(
            country_dir.join("2026.json"),
            r#"[{"date":"2026-12-25","localName":"Christmas Day","name":"Christmas Day","countryCode":"GB","fixed":false,"global":true,"counties":null,"launchYear":null,"types":["Public"]}]"#,
        )
        .expect("cache file can be written");

        let source = NagerHolidaySource::with_cache_dir("gb", &cache_dir, Duration::from_millis(1));
        let holidays = source.holidays_in(DateRange::day(date_ymd(2026, Month::December, 25)));

        let _ = std::fs::remove_dir_all(cache_dir);

        assert_eq!(holidays.len(), 1);
        assert_eq!(holidays[0].id, "nager-GB-2026-12-25-christmas-day");
        assert_eq!(holidays[0].name, "Christmas Day");
        assert_eq!(holidays[0].source.source_id, "nager-GB");
    }

    #[test]
    fn nager_source_respects_half_open_year_boundaries() {
        let cache_dir = std::env::temp_dir()
            .join(format!("rcal-nager-test-{}", std::process::id()))
            .join("half-open-range");
        let _ = std::fs::remove_dir_all(&cache_dir);
        let country_dir = cache_dir.join("GB");
        std::fs::create_dir_all(&country_dir).expect("cache dir can be created");
        std::fs::write(country_dir.join("2026.json"), "[]").expect("cache file can be written");
        std::fs::write(country_dir.join("2027.json"), "[]").expect("cache file can be written");

        let source = NagerHolidaySource::with_cache_dir("gb", &cache_dir, Duration::from_millis(1));
        let range = DateRange::new(
            date_ymd(2026, Month::December, 31),
            date_ymd(2027, Month::January, 1),
        )
        .expect("valid range");

        let _ = source.holidays_in(range);
        let attempted_years = source.state.borrow().attempted_years.clone();
        let _ = std::fs::remove_dir_all(cache_dir);

        assert!(attempted_years.contains(&2026));
        assert!(!attempted_years.contains(&2027));
    }
}
