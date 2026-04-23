use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env,
    error::Error,
    fmt, fs,
    path::PathBuf,
    time::Duration,
};

use serde::Deserialize;
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
    pub notes: Option<String>,
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
            notes: None,
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
            notes: None,
            source,
            timing: EventTiming::Timed { start, end },
        })
    }

    pub fn with_notes(mut self, notes: impl Into<String>) -> Self {
        self.notes = Some(notes.into());
        self
    }

    pub const fn is_all_day(&self) -> bool {
        self.timing.is_all_day()
    }

    pub const fn is_timed(&self) -> bool {
        self.timing.is_timed()
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
}

impl ConfiguredAgendaSource {
    pub fn development(holidays: HolidayProvider) -> Self {
        Self {
            events: InMemoryAgendaSource::development_fixture(),
            holidays,
        }
    }

    pub fn new(events: InMemoryAgendaSource, holidays: HolidayProvider) -> Self {
        Self { events, holidays }
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
