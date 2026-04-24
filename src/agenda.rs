use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use time::{Month, Time, Weekday};

use crate::calendar::CalendarDate;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub title: String,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub reminders: Vec<Reminder>,
    pub source: SourceMetadata,
    pub timing: EventTiming,
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

    pub const fn is_all_day(&self) -> bool {
        self.timing.is_all_day()
    }

    pub const fn is_timed(&self) -> bool {
        self.timing.is_timed()
    }

    pub fn is_local(&self) -> bool {
        self.source.source_id == "local"
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
}

impl CreateEventDraft {
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
        Ok(event)
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
}

#[derive(Debug)]
pub struct ConfiguredAgendaSource {
    events: InMemoryAgendaSource,
    holidays: HolidayProvider,
    events_file: Option<PathBuf>,
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
        })
    }

    pub fn create_event(&mut self, draft: CreateEventDraft) -> Result<Event, LocalEventStoreError> {
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
        let mut events = self.events.events().to_vec();
        let Some(index) = events.iter().position(|event| event.id == id) else {
            return Err(LocalEventStoreError::EventNotFound { id: id.to_string() });
        };
        if !events[index].is_local() {
            return Err(LocalEventStoreError::EventNotEditable { id: id.to_string() });
        }

        let event =
            draft
                .into_event(id.to_string())
                .map_err(|err| LocalEventStoreError::Encode {
                    path: self.events_file.clone(),
                    reason: err.to_string(),
                })?;
        events[index] = event.clone();

        if let Some(path) = &self.events_file {
            write_events_file(path, &events)?;
        }
        self.events.events = events;
        Ok(event)
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
        self.events.events_intersecting(range)
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        self.holidays.holidays_in(range)
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
        self.events
            .iter()
            .filter(|event| event.intersects_range(range))
            .cloned()
            .collect()
    }

    fn holidays_in(&self, range: DateRange) -> Vec<Holiday> {
        self.holidays
            .iter()
            .filter(|holiday| range.contains_date(holiday.date))
            .cloned()
            .collect()
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
    EventNotEditable {
        id: String,
    },
    Encode {
        path: Option<PathBuf>,
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
            Self::EventNotEditable { id } => write!(f, "event '{id}' is not editable locally"),
            Self::Encode { path, reason } => {
                if let Some(path) = path {
                    write!(f, "failed to encode {}: {reason}", path.display())
                } else {
                    write!(f, "failed to encode local event: {reason}")
                }
            }
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

    if file.version != LOCAL_EVENTS_VERSION {
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

const LOCAL_EVENTS_VERSION: u8 = 1;

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
    },
}

impl LocalEventRecord {
    fn from_event(event: &Event) -> Self {
        let reminders_minutes_before = event
            .reminders
            .iter()
            .map(|reminder| reminder.minutes_before)
            .collect::<Vec<_>>();

        match event.timing {
            EventTiming::AllDay { date } => Self::AllDay {
                id: event.id.clone(),
                title: event.title.clone(),
                date: date.to_string(),
                location: event.location.clone(),
                notes: event.notes.clone(),
                reminders_minutes_before,
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
                Ok(event)
            }
            Self::AllDay {
                id,
                title,
                date,
                location,
                notes,
                reminders_minutes_before,
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
                Ok(event)
            }
        }
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
            })
            .expect("timed event saves");
        source
            .create_event(CreateEventDraft {
                title: "Release day".to_string(),
                timing: CreateEventTiming::AllDay { date: day },
                location: None,
                notes: None,
                reminders: vec![Reminder::minutes_before(24 * 60)],
            })
            .expect("all-day event saves");

        let body = std::fs::read_to_string(&path).expect("event file exists");
        assert!(body.contains(r#""version": 1"#));
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
