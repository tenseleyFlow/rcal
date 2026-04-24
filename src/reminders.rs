use std::{
    collections::HashMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    thread,
    time::Duration as StdDuration,
};

use directories::ProjectDirs;
use fs2::FileExt;
#[cfg(not(target_os = "macos"))]
use notify_rust::Notification;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::process::Command;
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::{
    agenda::{
        AgendaSource, ConfiguredAgendaSource, DateRange, Event, EventDateTime, EventTiming,
        HolidayProvider,
    },
    calendar::CalendarDate,
};

const STATE_VERSION: u8 = 1;
const GRACE_MINUTES: i64 = 10;
const PRUNE_AFTER_DAYS: i32 = 30;
const LOOKAHEAD_DAYS: i32 = 47;
const POLL_INTERVAL: StdDuration = StdDuration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderDaemonConfig {
    pub events_file: PathBuf,
    pub state_file: PathBuf,
    pub poll_interval: StdDuration,
    pub grace: Duration,
}

impl ReminderDaemonConfig {
    pub fn new(events_file: PathBuf, state_file: PathBuf) -> Self {
        Self {
            events_file,
            state_file,
            poll_interval: POLL_INTERVAL,
            grace: Duration::minutes(GRACE_MINUTES),
        }
    }

    pub fn lock_file(&self) -> PathBuf {
        self.state_file.with_extension("lock")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderInstance {
    pub key: String,
    pub event_id: String,
    pub title: String,
    pub location: Option<String>,
    pub event_start: EventDateTime,
    pub fire_at: PrimitiveDateTime,
    pub minutes_before: u16,
    pub all_day: bool,
}

impl ReminderInstance {
    pub fn notification_title(&self) -> String {
        format!("Reminder: {}", self.title)
    }

    pub fn notification_body(&self) -> String {
        let when = if self.all_day {
            format!("All day on {}", self.event_start.date)
        } else {
            format!(
                "Starts at {:02}:{:02} on {}",
                self.event_start.time.hour(),
                self.event_start.time.minute(),
                self.event_start.date
            )
        };

        if let Some(location) = &self.location {
            format!("{when}\nLocation: {location}")
        } else {
            when
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderRunSummary {
    pub delivered: usize,
    pub skipped: usize,
    pub failed: usize,
}

pub trait Notifier {
    fn notify(&mut self, reminder: &ReminderInstance) -> Result<(), ReminderError>;
}

#[derive(Debug, Default)]
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&mut self, reminder: &ReminderInstance) -> Result<(), ReminderError> {
        show_system_notification(
            &reminder.notification_title(),
            &reminder.notification_body(),
        )
    }
}

pub const fn notification_backend_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos-osascript"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "notify-rust"
    }
}

#[cfg(target_os = "macos")]
fn show_system_notification(summary: &str, body: &str) -> Result<(), ReminderError> {
    let output = Command::new("osascript")
        .args(macos_display_notification_args(summary, body))
        .output()
        .map_err(|err| ReminderError::Notification(format!("osascript failed: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ReminderError::Notification(format!(
        "osascript exited with {}: {}",
        output.status,
        stderr.trim()
    )))
}

#[cfg(target_os = "macos")]
fn macos_display_notification_args(summary: &str, body: &str) -> Vec<String> {
    [
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
        summary,
        body,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[cfg(not(target_os = "macos"))]
fn show_system_notification(summary: &str, body: &str) -> Result<(), ReminderError> {
    Notification::new()
        .summary(summary)
        .body(body)
        .show()
        .map(|_| ())
        .map_err(|err| ReminderError::Notification(err.to_string()))
}

pub fn default_state_file() -> PathBuf {
    if let Some(project_dirs) = ProjectDirs::from("com", "tenseleyFlow", "rcal") {
        if let Some(state_dir) = project_dirs.state_dir() {
            return state_dir.join("reminders-state.json");
        }

        return project_dirs.data_local_dir().join("reminders-state.json");
    }

    std::env::temp_dir()
        .join("rcal")
        .join("reminders-state.json")
}

pub fn default_log_file() -> PathBuf {
    if let Some(project_dirs) = ProjectDirs::from("com", "tenseleyFlow", "rcal") {
        return project_dirs.cache_dir().join("reminders.log");
    }

    std::env::temp_dir().join("rcal").join("reminders.log")
}

pub fn current_local_datetime() -> PrimitiveDateTime {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    PrimitiveDateTime::new(now.date(), now.time())
}

pub fn run_daemon(
    config: ReminderDaemonConfig,
    notifier: &mut dyn Notifier,
) -> Result<(), ReminderError> {
    let _lock = DaemonLock::acquire(&config.lock_file())?;
    loop {
        let now = current_local_datetime();
        let _ = run_once(&config, now, notifier)?;
        thread::sleep(config.poll_interval);
    }
}

pub fn run_once(
    config: &ReminderDaemonConfig,
    now: PrimitiveDateTime,
    notifier: &mut dyn Notifier,
) -> Result<ReminderRunSummary, ReminderError> {
    let source =
        ConfiguredAgendaSource::from_events_file(&config.events_file, HolidayProvider::off())
            .map_err(|err| ReminderError::Events(err.to_string()))?;
    let mut state = ReminderState::load(&config.state_file)?;
    let instances = reminder_instances(&source, now);
    let mut delivered = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let expires_before = now - config.grace;

    for instance in instances {
        if state.contains(&instance.key) {
            continue;
        }

        if instance.fire_at > now {
            continue;
        }

        if instance.fire_at < expires_before {
            state.record(instance, ReminderStatus::Skipped);
            skipped += 1;
            continue;
        }

        match notifier.notify(&instance) {
            Ok(()) => {
                state.record(instance, ReminderStatus::Delivered);
                delivered += 1;
            }
            Err(_) => {
                failed += 1;
            }
        }
    }

    state.prune(now.date());
    state.save(&config.state_file)?;
    Ok(ReminderRunSummary {
        delivered,
        skipped,
        failed,
    })
}

pub fn test_notification(notifier: &mut dyn Notifier) -> Result<(), ReminderError> {
    let date = CalendarDate::from(current_local_datetime().date());
    let reminder = ReminderInstance {
        key: "test".to_string(),
        event_id: "test".to_string(),
        title: "rcal reminder test".to_string(),
        location: None,
        event_start: EventDateTime::new(date, current_local_datetime().time()),
        fire_at: current_local_datetime(),
        minutes_before: 0,
        all_day: false,
    };
    notifier.notify(&reminder)
}

pub fn reminder_instances(
    source: &dyn AgendaSource,
    now: PrimitiveDateTime,
) -> Vec<ReminderInstance> {
    let range = DateRange::new(
        CalendarDate::from(now.date()).add_days(-PRUNE_AFTER_DAYS),
        CalendarDate::from(now.date()).add_days(LOOKAHEAD_DAYS),
    )
    .expect("reminder scan range is valid");
    let mut instances = source
        .events_intersecting(range)
        .into_iter()
        .filter(Event::is_local)
        .flat_map(reminders_for_event)
        .collect::<Vec<_>>();
    instances.sort_by(|left, right| {
        left.fire_at
            .cmp(&right.fire_at)
            .then(left.title.cmp(&right.title))
            .then(left.key.cmp(&right.key))
    });
    instances
}

fn reminders_for_event(event: Event) -> Vec<ReminderInstance> {
    let Some(event_start) = reminder_event_start(&event) else {
        return Vec::new();
    };
    let all_day = matches!(event.timing, EventTiming::AllDay { .. });
    let start_at = event_datetime_to_primitive(event_start);

    event
        .reminders
        .iter()
        .map(|reminder| {
            let fire_at = start_at - Duration::minutes(i64::from(reminder.minutes_before));
            ReminderInstance {
                key: reminder_key(&event, event_start, reminder.minutes_before),
                event_id: event.id.clone(),
                title: event.title.clone(),
                location: event.location.clone(),
                event_start,
                fire_at,
                minutes_before: reminder.minutes_before,
                all_day,
            }
        })
        .collect()
}

fn reminder_event_start(event: &Event) -> Option<EventDateTime> {
    match event.timing {
        EventTiming::AllDay { date } => Some(EventDateTime::new(date, Time::MIDNIGHT)),
        EventTiming::Timed { start, .. } => Some(start),
    }
}

fn event_datetime_to_primitive(datetime: EventDateTime) -> PrimitiveDateTime {
    PrimitiveDateTime::new(datetime.date.into(), datetime.time)
}

fn reminder_key(event: &Event, event_start: EventDateTime, minutes_before: u16) -> String {
    format!(
        "{}|{}T{:02}:{:02}|{}m",
        event.id,
        event_start.date,
        event_start.time.hour(),
        event_start.time.minute(),
        minutes_before
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReminderState {
    records: HashMap<String, ReminderStateRecord>,
}

impl ReminderState {
    fn empty() -> Self {
        Self {
            records: HashMap::new(),
        }
    }

    fn load(path: &Path) -> Result<Self, ReminderError> {
        let body = match fs::read_to_string(path) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(err) => {
                return Err(ReminderError::StateRead {
                    path: path.to_path_buf(),
                    reason: err.to_string(),
                });
            }
        };
        let file = serde_json::from_str::<ReminderStateFile>(&body).map_err(|err| {
            ReminderError::StateParse {
                path: path.to_path_buf(),
                reason: err.to_string(),
            }
        })?;
        if file.version != STATE_VERSION {
            return Err(ReminderError::StateParse {
                path: path.to_path_buf(),
                reason: format!("unsupported reminder state version {}", file.version),
            });
        }
        for record in &file.reminders {
            if parse_date(&record.fire_date).is_none() || parse_time(&record.fire_time).is_none() {
                return Err(ReminderError::StateParse {
                    path: path.to_path_buf(),
                    reason: format!("invalid reminder fire time for key '{}'", record.key),
                });
            }
        }

        Ok(Self {
            records: file
                .reminders
                .into_iter()
                .map(|record| (record.key.clone(), record))
                .collect(),
        })
    }

    fn save(&self, path: &Path) -> Result<(), ReminderError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| ReminderError::StateWrite {
                path: parent.to_path_buf(),
                reason: err.to_string(),
            })?;
        }

        let mut reminders = self.records.values().cloned().collect::<Vec<_>>();
        reminders.sort_by(|left, right| left.key.cmp(&right.key));
        let file = ReminderStateFile {
            version: STATE_VERSION,
            reminders,
        };
        let body =
            serde_json::to_string_pretty(&file).map_err(|err| ReminderError::StateWrite {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        let temp_path = path.with_extension(format!(
            "{}.tmp",
            path.extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("json")
        ));
        fs::write(&temp_path, body).map_err(|err| ReminderError::StateWrite {
            path: temp_path.clone(),
            reason: err.to_string(),
        })?;
        fs::rename(&temp_path, path).map_err(|err| ReminderError::StateWrite {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })
    }

    fn contains(&self, key: &str) -> bool {
        self.records.contains_key(key)
    }

    fn record(&mut self, instance: ReminderInstance, status: ReminderStatus) {
        self.records.insert(
            instance.key.clone(),
            ReminderStateRecord {
                key: instance.key,
                status,
                fire_date: instance.fire_at.date().to_string(),
                fire_time: format_time(instance.fire_at.time()),
            },
        );
    }

    fn prune(&mut self, today: Date) {
        let cutoff = CalendarDate::from(today).add_days(-PRUNE_AFTER_DAYS);
        self.records.retain(|_, record| {
            record
                .fire_date_value()
                .map(|date| date >= cutoff)
                .unwrap_or(true)
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ReminderStateFile {
    version: u8,
    #[serde(default)]
    reminders: Vec<ReminderStateRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ReminderStateRecord {
    key: String,
    status: ReminderStatus,
    fire_date: String,
    fire_time: String,
}

impl ReminderStateRecord {
    fn fire_date_value(&self) -> Option<CalendarDate> {
        parse_date(&self.fire_date)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReminderStatus {
    Delivered,
    Skipped,
}

struct DaemonLock {
    _file: fs::File,
}

impl DaemonLock {
    fn acquire(path: &Path) -> Result<Self, ReminderError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| ReminderError::Lock {
                path: parent.to_path_buf(),
                reason: err.to_string(),
            })?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|err| ReminderError::Lock {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        file.try_lock_exclusive()
            .map_err(|err| ReminderError::Lock {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;

        Ok(Self { _file: file })
    }
}

#[derive(Debug)]
pub enum ReminderError {
    Events(String),
    Notification(String),
    StateRead { path: PathBuf, reason: String },
    StateParse { path: PathBuf, reason: String },
    StateWrite { path: PathBuf, reason: String },
    Lock { path: PathBuf, reason: String },
}

impl fmt::Display for ReminderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Events(reason) => write!(f, "failed to load reminder events: {reason}"),
            Self::Notification(reason) => write!(f, "failed to send notification: {reason}"),
            Self::StateRead { path, reason } => {
                write!(
                    f,
                    "failed to read reminder state {}: {reason}",
                    path.display()
                )
            }
            Self::StateParse { path, reason } => {
                write!(
                    f,
                    "failed to parse reminder state {}: {reason}",
                    path.display()
                )
            }
            Self::StateWrite { path, reason } => {
                write!(
                    f,
                    "failed to write reminder state {}: {reason}",
                    path.display()
                )
            }
            Self::Lock { path, reason } => {
                write!(
                    f,
                    "failed to lock reminder daemon {}: {reason}",
                    path.display()
                )
            }
        }
    }
}

impl Error for ReminderError {}

fn format_time(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}

fn parse_date(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u8>().ok()?;
    let day = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    CalendarDate::from_ymd(year, Month::try_from(month).ok()?, day).ok()
}

fn parse_time(value: &str) -> Option<Time> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u8>().ok()?;
    let minute = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Time::from_hms(hour, minute, 0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    use crate::agenda::{
        CreateEventDraft, CreateEventTiming, Event, InMemoryAgendaSource, RecurrenceEnd,
        RecurrenceFrequency, RecurrenceRule, Reminder, SourceMetadata,
    };

    #[derive(Default)]
    struct FakeNotifier {
        sent: Vec<String>,
        fail: bool,
    }

    impl Notifier for FakeNotifier {
        fn notify(&mut self, reminder: &ReminderInstance) -> Result<(), ReminderError> {
            if self.fail {
                Err(ReminderError::Notification("boom".to_string()))
            } else {
                self.sent.push(reminder.key.clone());
                Ok(())
            }
        }
    }

    fn date(day: u8) -> CalendarDate {
        CalendarDate::from_ymd(2026, Month::April, day).expect("valid test date")
    }

    fn at(date: CalendarDate, hour: u8, minute: u8) -> EventDateTime {
        EventDateTime::new(date, Time::from_hms(hour, minute, 0).expect("valid time"))
    }

    fn now(day: u8, hour: u8, minute: u8) -> PrimitiveDateTime {
        PrimitiveDateTime::new(
            date(day).into(),
            Time::from_hms(hour, minute, 0).expect("valid time"),
        )
    }

    fn timed_event(id: &str, title: &str, start: EventDateTime, end: EventDateTime) -> Event {
        Event::timed(id, title, start, end, SourceMetadata::local()).expect("valid timed event")
    }

    fn temp_path(name: &str, file: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("rcal-reminders-test-{}", std::process::id()))
            .join(name)
            .join(file)
    }

    #[test]
    fn reminder_fire_times_cover_timed_all_day_cross_midnight_and_recurring_events() {
        let day = date(23);
        let timed = timed_event("timed", "Timed", at(day, 9, 0), at(day, 10, 0))
            .with_reminders(vec![Reminder::minutes_before(15)]);
        let all_day = Event::all_day("all-day", "All day", day, SourceMetadata::local())
            .with_reminders(vec![Reminder::minutes_before(60)]);
        let cross_midnight =
            timed_event("late", "Late", at(day, 23, 30), at(day.add_days(1), 1, 0))
                .with_reminders(vec![Reminder::minutes_before(30)]);
        let recurring = timed_event("daily", "Daily", at(day, 8, 0), at(day, 8, 30))
            .with_reminders(vec![Reminder::minutes_before(10)])
            .with_recurrence(RecurrenceRule {
                frequency: RecurrenceFrequency::Daily,
                interval: 1,
                end: RecurrenceEnd::Count(2),
                weekdays: Vec::new(),
                monthly: None,
                yearly: None,
            });
        let source = InMemoryAgendaSource::with_events_and_holidays(
            vec![timed, all_day, cross_midnight, recurring],
            Vec::new(),
        );

        let instances = reminder_instances(&source, now(23, 9, 0));
        let keys = instances
            .iter()
            .map(|instance| (instance.event_id.as_str(), instance.fire_at))
            .collect::<Vec<_>>();

        assert!(keys.contains(&("timed", now(23, 8, 45))));
        assert!(keys.contains(&("all-day", now(22, 23, 0))));
        assert!(keys.contains(&("late", now(23, 23, 0))));
        assert!(
            keys.iter()
                .any(|(id, fire_at)| id.starts_with("daily#") && *fire_at == now(24, 7, 50))
        );
    }

    #[test]
    fn run_once_delivers_within_grace_and_skips_older_reminders() {
        let dir = temp_path("grace", "events.json");
        let _ = std::fs::remove_dir_all(dir.parent().expect("path has parent"));
        let state_file = temp_path("grace", "state.json");
        let mut source = ConfiguredAgendaSource::from_events_file(&dir, HolidayProvider::off())
            .expect("events load");
        source
            .create_event(CreateEventDraft {
                title: "Recent".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(date(23), 9, 5),
                    end: at(date(23), 10, 0),
                },
                location: None,
                notes: None,
                reminders: vec![Reminder::minutes_before(10)],
                recurrence: None,
            })
            .expect("event saves");
        source
            .create_event(CreateEventDraft {
                title: "Old".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(date(23), 8, 30),
                    end: at(date(23), 9, 0),
                },
                location: None,
                notes: None,
                reminders: vec![Reminder::minutes_before(30)],
                recurrence: None,
            })
            .expect("event saves");
        let config = ReminderDaemonConfig::new(dir.clone(), state_file);
        let mut notifier = FakeNotifier::default();

        let summary = run_once(&config, now(23, 9, 0), &mut notifier).expect("run succeeds");

        let _ = std::fs::remove_dir_all(dir.parent().expect("test dir exists"));
        assert_eq!(summary.delivered, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(notifier.sent.len(), 1);
    }

    #[test]
    fn delivered_state_dedupes_across_runs() {
        let events_file = temp_path("dedupe", "events.json");
        let _ = std::fs::remove_dir_all(events_file.parent().expect("path has parent"));
        let state_file = temp_path("dedupe", "state.json");
        let mut source =
            ConfiguredAgendaSource::from_events_file(&events_file, HolidayProvider::off())
                .expect("events load");
        source
            .create_event(CreateEventDraft {
                title: "Planning".to_string(),
                timing: CreateEventTiming::Timed {
                    start: at(date(23), 9, 5),
                    end: at(date(23), 10, 0),
                },
                location: None,
                notes: None,
                reminders: vec![Reminder::minutes_before(10)],
                recurrence: None,
            })
            .expect("event saves");
        let config = ReminderDaemonConfig::new(events_file.clone(), state_file);
        let mut notifier = FakeNotifier::default();
        let first = run_once(&config, now(23, 9, 0), &mut notifier).expect("first run");
        let second = run_once(&config, now(23, 9, 1), &mut notifier).expect("second run");

        let _ = std::fs::remove_dir_all(events_file.parent().expect("test dir exists"));
        assert_eq!(first.delivered, 1);
        assert_eq!(second.delivered, 0);
        assert_eq!(notifier.sent.len(), 1);
    }

    #[test]
    fn malformed_state_is_a_clear_error_and_prune_drops_old_records() {
        let state_file = temp_path("state", "state.json");
        let _ = std::fs::remove_dir_all(state_file.parent().expect("path has parent"));
        std::fs::create_dir_all(state_file.parent().expect("path has parent"))
            .expect("dir creates");
        std::fs::write(&state_file, "not-json").expect("state writes");
        let err = ReminderState::load(&state_file).expect_err("malformed state fails");
        assert!(err.to_string().contains("failed to parse reminder state"));

        let mut state = ReminderState::empty();
        state.records.insert(
            "old".to_string(),
            ReminderStateRecord {
                key: "old".to_string(),
                status: ReminderStatus::Delivered,
                fire_date: "2026-03-01".to_string(),
                fire_time: "09:00".to_string(),
            },
        );
        state.records.insert(
            "new".to_string(),
            ReminderStateRecord {
                key: "new".to_string(),
                status: ReminderStatus::Delivered,
                fire_date: "2026-04-20".to_string(),
                fire_time: "09:00".to_string(),
            },
        );
        state.prune(date(23).into());

        let _ = std::fs::remove_dir_all(state_file.parent().expect("test dir exists"));
        assert!(!state.records.contains_key("old"));
        assert!(state.records.contains_key("new"));
    }

    #[test]
    fn notification_body_includes_time_and_location() {
        let reminder = ReminderInstance {
            key: "key".to_string(),
            event_id: "event".to_string(),
            title: "Planning".to_string(),
            location: Some("Room 1".to_string()),
            event_start: at(date(23), 9, 0),
            fire_at: now(23, 8, 50),
            minutes_before: 10,
            all_day: false,
        };

        assert_eq!(reminder.notification_title(), "Reminder: Planning");
        assert!(reminder.notification_body().contains("Starts at 09:00"));
        assert!(reminder.notification_body().contains("Location: Room 1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_notification_uses_osascript_with_separate_user_text_args() {
        let args = macos_display_notification_args(
            "Reminder: Planning \"review\"",
            "Starts now\nLocation: Room 1",
        );

        assert_eq!(args[0], "-e");
        assert_eq!(args[1], "on run argv");
        assert_eq!(
            args[3],
            "display notification (item 2 of argv) with title (item 1 of argv)"
        );
        assert_eq!(args[5], "end run");
        assert_eq!(args[6], "Reminder: Planning \"review\"");
        assert_eq!(args[7], "Starts now\nLocation: Room 1");
    }
}
