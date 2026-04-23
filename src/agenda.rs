use std::{error::Error, fmt};

use time::{Month, Time};

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
            holidays: vec![Holiday::new("earth-day", "Earth Day", date, source)],
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month::April;

    fn date(day: u8) -> CalendarDate {
        CalendarDate::from_ymd(2026, April, day).expect("valid test date")
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

        assert_eq!(agenda.holidays[0].name, "Earth Day");
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
}
