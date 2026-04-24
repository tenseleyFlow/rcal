use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env,
    error::Error,
    fmt, fs, io,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use time::{Month, Time, Weekday};

use crate::{
    agenda::{
        AgendaError, AgendaSource, CreateEventDraft, CreateEventTiming, DateRange, Event,
        EventDateTime, EventWriteTarget, EventWriteTargetId, Holiday, InMemoryAgendaSource,
        OccurrenceAnchor, OccurrenceMetadata, RecurrenceEnd, RecurrenceFrequency,
        RecurrenceMonthlyRule, RecurrenceOrdinal, RecurrenceRule, RecurrenceYearlyRule, Reminder,
        SourceMetadata,
    },
    calendar::CalendarDate,
};

const MICROSOFT_CACHE_VERSION: u8 = 1;
const GOOGLE_CACHE_VERSION: u8 = 1;
const GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
const LOGIN_BASE_URL: &str = "https://login.microsoftonline.com";
const MICROSOFT_SCOPES: &str = "offline_access User.Read Calendars.ReadWrite";
const KEYRING_SERVICE: &str = "rcal.microsoft";
const GOOGLE_CALENDAR_BASE_URL: &str = "https://www.googleapis.com/calendar/v3";
const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str = concat!(
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    " ",
    "https://www.googleapis.com/auth/calendar.events"
);
const GOOGLE_KEYRING_SERVICE: &str = "rcal.google";
pub const MICROSOFT_OFFICIAL_CLIENT_ID: &str = "9a49eaac-422b-4192-a65d-82dc8f43c11d";
pub const MICROSOFT_DEFAULT_TENANT: &str = "common";
pub const GOOGLE_OFFICIAL_CLIENT_ID: &str = "";
pub const GOOGLE_OFFICIAL_CLIENT_SECRET: &str = "";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub create_target: ProviderCreateTarget,
    pub microsoft: MicrosoftProviderConfig,
    pub google: GoogleProviderConfig,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            create_target: ProviderCreateTarget::Local,
            microsoft: MicrosoftProviderConfig::default(),
            google: GoogleProviderConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCreateTarget {
    Local,
    Microsoft,
    Google,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftProviderConfig {
    pub enabled: bool,
    pub default_account: Option<String>,
    pub default_calendar: Option<String>,
    pub sync_past_days: i32,
    pub sync_future_days: i32,
    pub cache_file: PathBuf,
    pub accounts: Vec<MicrosoftAccountConfig>,
}

impl Default for MicrosoftProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_account: None,
            default_calendar: None,
            sync_past_days: 30,
            sync_future_days: 365,
            cache_file: default_microsoft_cache_file(),
            accounts: Vec::new(),
        }
    }
}

impl MicrosoftProviderConfig {
    pub fn account(&self, id: &str) -> Option<&MicrosoftAccountConfig> {
        self.accounts.iter().find(|account| account.id == id)
    }

    pub fn default_account(&self) -> Option<&MicrosoftAccountConfig> {
        self.default_account
            .as_deref()
            .and_then(|id| self.account(id))
            .or_else(|| self.accounts.first())
    }

    pub fn default_calendar(&self) -> Option<(&MicrosoftAccountConfig, &str)> {
        let account = self.default_account()?;
        let calendar = self
            .default_calendar
            .as_deref()
            .or_else(|| account.calendars.first().map(String::as_str))?;
        Some((account, calendar))
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        if !self.enabled {
            return Ok(());
        }
        if self.accounts.is_empty() {
            return Err(ProviderError::Config(
                "providers.microsoft.enabled requires at least one account".to_string(),
            ));
        }
        let mut seen = HashMap::new();
        for account in &self.accounts {
            if account.id.trim().is_empty() {
                return Err(ProviderError::Config(
                    "Microsoft account id may not be empty".to_string(),
                ));
            }
            if seen.insert(account.id.clone(), ()).is_some() {
                return Err(ProviderError::Config(format!(
                    "duplicate Microsoft account id '{}'",
                    account.id
                )));
            }
            if account.client_id.trim().is_empty() {
                return Err(ProviderError::Config(format!(
                    "Microsoft account '{}' requires client_id",
                    account.id
                )));
            }
            if account.tenant.trim().is_empty() {
                return Err(ProviderError::Config(format!(
                    "Microsoft account '{}' requires tenant",
                    account.id
                )));
            }
            if account
                .calendars
                .iter()
                .any(|calendar| calendar.trim().is_empty())
            {
                return Err(ProviderError::Config(format!(
                    "Microsoft account '{}' has an empty calendar id",
                    account.id
                )));
            }
        }
        if let Some(default_account) = &self.default_account
            && self.account(default_account).is_none()
        {
            return Err(ProviderError::Config(format!(
                "providers.microsoft.default_account '{}' is not configured",
                default_account
            )));
        }
        if let Some(default_calendar) = &self.default_calendar {
            let Some(account) = self.default_account() else {
                return Err(ProviderError::Config(
                    "providers.microsoft.default_calendar requires a default account".to_string(),
                ));
            };
            if !account
                .calendars
                .iter()
                .any(|calendar| calendar == default_calendar)
            {
                return Err(ProviderError::Config(format!(
                    "default calendar '{}' is not listed for account '{}'",
                    default_calendar, account.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftAccountConfig {
    pub id: String,
    pub client_id: String,
    pub tenant: String,
    pub redirect_port: u16,
    pub calendars: Vec<String>,
}

impl MicrosoftAccountConfig {
    pub fn new_official(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            client_id: MICROSOFT_OFFICIAL_CLIENT_ID.to_string(),
            tenant: MICROSOFT_DEFAULT_TENANT.to_string(),
            redirect_port: 8765,
            calendars: Vec::new(),
        }
    }

    pub fn token_url(&self) -> String {
        format!("{LOGIN_BASE_URL}/{}/oauth2/v2.0/token", self.tenant)
    }

    pub fn device_code_url(&self) -> String {
        format!("{LOGIN_BASE_URL}/{}/oauth2/v2.0/devicecode", self.tenant)
    }

    pub fn authorize_url(&self) -> String {
        format!("{LOGIN_BASE_URL}/{}/oauth2/v2.0/authorize", self.tenant)
    }

    fn redirect_uri(&self) -> String {
        format!("http://localhost:{}/callback", self.redirect_port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleProviderConfig {
    pub enabled: bool,
    pub default_account: Option<String>,
    pub default_calendar: Option<String>,
    pub sync_past_days: i32,
    pub sync_future_days: i32,
    pub cache_file: PathBuf,
    pub accounts: Vec<GoogleAccountConfig>,
}

impl Default for GoogleProviderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_account: None,
            default_calendar: None,
            sync_past_days: 30,
            sync_future_days: 365,
            cache_file: default_google_cache_file(),
            accounts: Vec::new(),
        }
    }
}

impl GoogleProviderConfig {
    pub fn account(&self, id: &str) -> Option<&GoogleAccountConfig> {
        self.accounts.iter().find(|account| account.id == id)
    }

    pub fn default_account(&self) -> Option<&GoogleAccountConfig> {
        self.default_account
            .as_deref()
            .and_then(|id| self.account(id))
            .or_else(|| self.accounts.first())
    }

    pub fn default_calendar(&self) -> Option<(&GoogleAccountConfig, &str)> {
        let account = self.default_account()?;
        let calendar = self
            .default_calendar
            .as_deref()
            .or_else(|| account.calendars.first().map(String::as_str))?;
        Some((account, calendar))
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        if !self.enabled {
            return Ok(());
        }
        if self.accounts.is_empty() {
            return Err(ProviderError::Config(
                "providers.google.enabled requires at least one account".to_string(),
            ));
        }
        let mut seen = HashMap::new();
        for account in &self.accounts {
            if account.id.trim().is_empty() {
                return Err(ProviderError::Config(
                    "Google account id may not be empty".to_string(),
                ));
            }
            if seen.insert(account.id.clone(), ()).is_some() {
                return Err(ProviderError::Config(format!(
                    "duplicate Google account id '{}'",
                    account.id
                )));
            }
            if account.client_id.trim().is_empty() {
                return Err(ProviderError::Config(format!(
                    "Google account '{}' requires client_id",
                    account.id
                )));
            }
            if account
                .calendars
                .iter()
                .any(|calendar| calendar.trim().is_empty())
            {
                return Err(ProviderError::Config(format!(
                    "Google account '{}' has an empty calendar id",
                    account.id
                )));
            }
        }
        if let Some(default_account) = &self.default_account
            && self.account(default_account).is_none()
        {
            return Err(ProviderError::Config(format!(
                "providers.google.default_account '{}' is not configured",
                default_account
            )));
        }
        if let Some(default_calendar) = &self.default_calendar {
            let Some(account) = self.default_account() else {
                return Err(ProviderError::Config(
                    "providers.google.default_calendar requires a default account".to_string(),
                ));
            };
            if !account
                .calendars
                .iter()
                .any(|calendar| calendar == default_calendar)
            {
                return Err(ProviderError::Config(format!(
                    "default Google calendar '{}' is not listed for account '{}'",
                    default_calendar, account.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAccountConfig {
    pub id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_port: u16,
    pub calendars: Vec<String>,
}

impl GoogleAccountConfig {
    pub fn new(
        id: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            client_id: client_id.into(),
            client_secret,
            redirect_port: 8766,
            calendars: Vec::new(),
        }
    }

    pub fn new_official(id: impl Into<String>) -> Result<Self, ProviderError> {
        let Some((client_id, client_secret)) = google_official_client_config() else {
            return Err(ProviderError::Config(
                "this rcal build does not include an official Google OAuth client yet; pass --client-id and --client-secret or finish the official Google client registration".to_string(),
            ));
        };
        Ok(Self::new(id, client_id, client_secret))
    }

    fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.redirect_port)
    }
}

pub fn google_official_client_config() -> Option<(String, Option<String>)> {
    let client_id = GOOGLE_OFFICIAL_CLIENT_ID.trim();
    if client_id.is_empty() {
        return None;
    }
    let client_secret = (!GOOGLE_OFFICIAL_CLIENT_SECRET.trim().is_empty())
        .then(|| GOOGLE_OFFICIAL_CLIENT_SECRET.to_string());
    Some((GOOGLE_OFFICIAL_CLIENT_ID.to_string(), client_secret))
}

pub fn default_microsoft_cache_file() -> PathBuf {
    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home)
            .join("rcal")
            .join("microsoft-cache.json");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("rcal")
            .join("microsoft-cache.json");
    }
    env::temp_dir().join("rcal").join("microsoft-cache.json")
}

pub fn default_google_cache_file() -> PathBuf {
    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home)
            .join("rcal")
            .join("google-cache.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("rcal")
            .join("google-cache.json");
    }
    env::temp_dir().join("rcal").join("google-cache.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftCalendarInfo {
    pub id: String,
    pub name: String,
    pub can_edit: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleCalendarInfo {
    pub id: String,
    pub name: String,
    pub can_edit: bool,
    pub is_default: bool,
}

#[derive(Debug, Default, Clone)]
pub struct GoogleAgendaSource {
    cache: GoogleCacheFile,
}

impl GoogleAgendaSource {
    pub fn load(path: &Path) -> Result<Self, ProviderError> {
        Ok(Self {
            cache: GoogleCacheFile::load(path)?,
        })
    }

    pub fn empty() -> Self {
        Self {
            cache: GoogleCacheFile::empty(),
        }
    }

    pub fn event_by_id(&self, id: &str) -> Option<Event> {
        self.cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .and_then(GoogleCachedEvent::to_event)
    }

    pub fn event_count(&self) -> usize {
        self.cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .map(|calendar| calendar.events.len())
            .sum()
    }
}

impl AgendaSource for GoogleAgendaSource {
    fn events_intersecting(&self, range: DateRange) -> Vec<Event> {
        let cached_events = self
            .cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .collect::<Vec<_>>();
        let concrete_occurrence_series_ids = cached_events
            .iter()
            .filter_map(|event| event.series_master_app_id.clone())
            .collect::<HashSet<_>>();
        let events = cached_events
            .into_iter()
            .filter(|event| {
                event.event_type.as_deref() != Some("recurringMaster")
                    || !concrete_occurrence_series_ids.contains(&event.id)
            })
            .filter_map(GoogleCachedEvent::to_event)
            .collect::<Vec<_>>();
        let mut events = InMemoryAgendaSource::with_events_and_holidays(events, Vec::new())
            .events_intersecting(range);
        events.sort_by(|left, right| left.id.cmp(&right.id));
        events
    }

    fn holidays_in(&self, _range: DateRange) -> Vec<Holiday> {
        Vec::new()
    }

    fn editable_event_by_id(&self, id: &str) -> Option<Event> {
        self.event_by_id(id)
    }
}

#[derive(Debug, Default, Clone)]
pub struct MicrosoftAgendaSource {
    cache: MicrosoftCacheFile,
}

impl MicrosoftAgendaSource {
    pub fn load(path: &Path) -> Result<Self, ProviderError> {
        Ok(Self {
            cache: MicrosoftCacheFile::load(path)?,
        })
    }

    pub fn empty() -> Self {
        Self {
            cache: MicrosoftCacheFile::empty(),
        }
    }

    pub fn event_by_id(&self, id: &str) -> Option<Event> {
        self.cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .and_then(MicrosoftCachedEvent::to_event)
    }

    pub fn metadata_for_event(&self, id: &str) -> Option<MicrosoftEventMetadata> {
        self.cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .map(MicrosoftCachedEvent::metadata)
    }

    pub fn event_count(&self) -> usize {
        self.cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .map(|calendar| calendar.events.len())
            .sum()
    }
}

impl AgendaSource for MicrosoftAgendaSource {
    fn events_intersecting(&self, range: DateRange) -> Vec<Event> {
        let cached_events = self
            .cache
            .accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .collect::<Vec<_>>();
        let concrete_occurrence_series_ids = cached_events
            .iter()
            .filter_map(|event| event.series_master_app_id.clone())
            .collect::<HashSet<_>>();
        let events = cached_events
            .into_iter()
            .filter(|event| {
                event.event_type.as_deref() != Some("seriesMaster")
                    || !concrete_occurrence_series_ids.contains(&event.id)
            })
            .filter_map(MicrosoftCachedEvent::to_event)
            .collect::<Vec<_>>();
        let mut events = InMemoryAgendaSource::with_events_and_holidays(events, Vec::new())
            .events_intersecting(range);
        events.sort_by(|left, right| left.id.cmp(&right.id));
        events
    }

    fn holidays_in(&self, _range: DateRange) -> Vec<Holiday> {
        Vec::new()
    }

    fn editable_event_by_id(&self, id: &str) -> Option<Event> {
        self.event_by_id(id)
    }
}

#[derive(Debug)]
pub struct MicrosoftProviderRuntime {
    config: MicrosoftProviderConfig,
    cache: MicrosoftCacheFile,
}

impl MicrosoftProviderRuntime {
    pub fn load(config: MicrosoftProviderConfig) -> Result<Self, ProviderError> {
        config.validate()?;
        let cache = MicrosoftCacheFile::load(&config.cache_file)?;
        Ok(Self { config, cache })
    }

    pub fn agenda_source(&self) -> MicrosoftAgendaSource {
        MicrosoftAgendaSource {
            cache: self.cache.clone(),
        }
    }

    pub fn write_targets(&self) -> Vec<EventWriteTarget> {
        self.config
            .accounts
            .iter()
            .flat_map(|account| {
                account.calendars.iter().filter_map(|calendar_id| {
                    let record = self.cache.calendar_record(&account.id, calendar_id);
                    if record.as_ref().is_some_and(|calendar| !calendar.can_edit) {
                        return None;
                    }
                    let label = record
                        .as_ref()
                        .map(|calendar| calendar.name.as_str())
                        .filter(|name| !name.trim().is_empty())
                        .map(|name| format!("Microsoft {}: {name}", account.id))
                        .unwrap_or_else(|| {
                            format!(
                                "Microsoft {}: {}",
                                account.id,
                                short_calendar_label(calendar_id)
                            )
                        });
                    Some(EventWriteTarget::microsoft(
                        account.id.clone(),
                        calendar_id.clone(),
                        label,
                    ))
                })
            })
            .collect()
    }

    pub fn default_write_target(&self) -> Option<EventWriteTargetId> {
        let (account, calendar_id) = self.config.default_calendar()?;
        Some(EventWriteTargetId::microsoft(
            account.id.clone(),
            calendar_id.to_string(),
        ))
    }

    pub fn status(&self, token_store: &dyn MicrosoftTokenStore) -> MicrosoftProviderStatus {
        let accounts = self
            .config
            .accounts
            .iter()
            .map(|account| MicrosoftAccountStatus {
                id: account.id.clone(),
                authenticated: token_store.load(&account.id).ok().flatten().is_some(),
                calendars: account.calendars.clone(),
            })
            .collect::<Vec<_>>();
        MicrosoftProviderStatus {
            enabled: self.config.enabled,
            cache_file: self.config.cache_file.clone(),
            event_count: self.agenda_source().event_count(),
            accounts,
        }
    }

    pub fn sync(
        &mut self,
        account_filter: Option<&str>,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
        now: CalendarDate,
    ) -> Result<MicrosoftSyncSummary, ProviderError> {
        if !self.config.enabled {
            return Err(ProviderError::Config(
                "Microsoft provider is disabled".to_string(),
            ));
        }

        let mut summary = MicrosoftSyncSummary::default();
        let accounts = self
            .config
            .accounts
            .iter()
            .filter(|account| account_filter.map(|id| id == account.id).unwrap_or(true))
            .cloned()
            .collect::<Vec<_>>();

        if accounts.is_empty() {
            return Err(ProviderError::Config(format!(
                "Microsoft account '{}' is not configured",
                account_filter.unwrap_or("<none>")
            )));
        }

        for account in accounts {
            let token = access_token(&account, http, token_store)?;
            let calendar_ids = account.calendars.clone();
            for calendar_id in calendar_ids {
                let calendar = fetch_calendar(http, &token, &calendar_id)?;
                let start = now.add_days(-self.config.sync_past_days);
                let end = now.add_days(self.config.sync_future_days);
                let events = fetch_calendar_view(http, &token, &account.id, &calendar, start, end)?;
                summary.events += events.len();
                summary.calendars += 1;
                self.cache
                    .replace_calendar(&account.id, calendar, events, current_epoch_seconds());
            }
            summary.accounts += 1;
        }

        self.cache.save(&self.config.cache_file)?;
        Ok(summary)
    }

    pub fn create_event(
        &mut self,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let target = self.default_write_target().ok_or_else(|| {
            ProviderError::Config("no Microsoft default calendar configured".to_string())
        })?;
        self.create_event_in_target(draft, &target, http, token_store)
    }

    pub fn create_event_in_target(
        &mut self,
        draft: CreateEventDraft,
        target: &EventWriteTargetId,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let Some((account_id, calendar_id)) = target.microsoft_parts() else {
            return Err(ProviderError::Config(
                "Microsoft provider requires a Microsoft calendar target".to_string(),
            ));
        };
        let account = self
            .config
            .account(account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!("account '{account_id}' is not configured"))
            })?
            .clone();
        if !account
            .calendars
            .iter()
            .any(|calendar| calendar == calendar_id)
        {
            return Err(ProviderError::Config(format!(
                "calendar '{calendar_id}' is not configured for account '{account_id}'"
            )));
        }
        let token = access_token(&account, http, token_store)?;
        let body = graph_event_payload(&draft, false)?;
        let response = graph_request(
            http,
            "POST",
            &format!(
                "{GRAPH_BASE_URL}/me/calendars/{}/events",
                percent_encode(calendar_id)
            ),
            &token,
            Some(body.to_string()),
        )?;
        let value = parse_graph_success_json(response)?;
        let calendar =
            fetch_calendar(http, &token, calendar_id).unwrap_or(MicrosoftCalendarRecord {
                id: calendar_id.to_string(),
                name: calendar_id.to_string(),
                can_edit: true,
                is_default: false,
            });
        let cached = MicrosoftCachedEvent::from_graph(&account.id, &calendar, value)?;
        self.cache
            .upsert_event(&account.id, calendar, cached.clone());
        self.cache.save(&self.config.cache_file)?;
        cached.to_event().ok_or_else(|| {
            ProviderError::Mapping("created Microsoft event could not be converted".to_string())
        })
    }

    pub fn update_event(
        &mut self,
        id: &str,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let metadata = self
            .cache
            .metadata_for_event(id)
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let account = self
            .config
            .account(&metadata.account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "account '{}' is not configured",
                    metadata.account_id
                ))
            })?
            .clone();
        let token = access_token(&account, http, token_store)?;
        let body = graph_event_payload(&draft, true)?;
        let response = graph_request(
            http,
            "PATCH",
            &format!(
                "{GRAPH_BASE_URL}/me/events/{}",
                percent_encode(&metadata.graph_id)
            ),
            &token,
            Some(body.to_string()),
        )?;
        let value = parse_graph_success_json(response)?;
        let calendar = self
            .cache
            .calendar_record(&metadata.account_id, &metadata.calendar_id)
            .unwrap_or(MicrosoftCalendarRecord {
                id: metadata.calendar_id.clone(),
                name: metadata.calendar_id.clone(),
                can_edit: true,
                is_default: false,
            });
        let cached = MicrosoftCachedEvent::from_graph(&metadata.account_id, &calendar, value)?;
        self.cache.remove_occurrences_for_series(&cached.id);
        self.cache
            .upsert_event(&metadata.account_id, calendar, cached.clone());
        self.cache.save(&self.config.cache_file)?;
        cached.to_event().ok_or_else(|| {
            ProviderError::Mapping("updated Microsoft event could not be converted".to_string())
        })
    }

    pub fn update_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.update_event(
            &id,
            draft.without_recurrence_for_provider(),
            http,
            token_store,
        )
    }

    pub fn delete_event(
        &mut self,
        id: &str,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let metadata = self
            .cache
            .metadata_for_event(id)
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let event = self
            .cache
            .event_by_id(id)
            .and_then(|cached| cached.to_event())
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let account = self
            .config
            .account(&metadata.account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "account '{}' is not configured",
                    metadata.account_id
                ))
            })?
            .clone();
        let token = access_token(&account, http, token_store)?;
        let response = graph_request(
            http,
            "DELETE",
            &format!(
                "{GRAPH_BASE_URL}/me/events/{}",
                percent_encode(&metadata.graph_id)
            ),
            &token,
            None,
        )?;
        parse_graph_empty_success(response)?;
        self.cache.remove_event(id);
        self.cache.save(&self.config.cache_file)?;
        Ok(event)
    }

    pub fn delete_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<(), ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.delete_event(&id, http, token_store).map(|_| ())
    }

    pub fn duplicate_event(
        &mut self,
        id: &str,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let event = self
            .cache
            .event_by_id(id)
            .and_then(|cached| cached.to_event())
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        self.create_event(
            CreateEventDraft::from_event(&event).without_recurrence_for_provider(),
            http,
            token_store,
        )
    }

    pub fn duplicate_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn MicrosoftTokenStore,
    ) -> Result<Event, ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.duplicate_event(&id, http, token_store)
    }
}

#[derive(Debug)]
pub struct GoogleProviderRuntime {
    config: GoogleProviderConfig,
    cache: GoogleCacheFile,
}

impl GoogleProviderRuntime {
    pub fn load(config: GoogleProviderConfig) -> Result<Self, ProviderError> {
        config.validate()?;
        let cache = GoogleCacheFile::load(&config.cache_file)?;
        Ok(Self { config, cache })
    }

    pub fn agenda_source(&self) -> GoogleAgendaSource {
        GoogleAgendaSource {
            cache: self.cache.clone(),
        }
    }

    pub fn write_targets(&self) -> Vec<EventWriteTarget> {
        if !self.config.enabled {
            return Vec::new();
        }
        self.config
            .accounts
            .iter()
            .flat_map(|account| {
                account.calendars.iter().map(|calendar_id| {
                    let label = self
                        .cache
                        .calendar_record(&account.id, calendar_id)
                        .map(|calendar| format!("Google {}: {}", account.id, calendar.name))
                        .unwrap_or_else(|| {
                            format!(
                                "Google {}: {}",
                                account.id,
                                short_calendar_label(calendar_id)
                            )
                        });
                    EventWriteTarget::provider("google", &account.id, calendar_id, label)
                })
            })
            .collect()
    }

    pub fn default_write_target(&self) -> Option<EventWriteTargetId> {
        let (account, calendar_id) = self.config.default_calendar()?;
        Some(EventWriteTargetId::provider(
            "google",
            account.id.clone(),
            calendar_id.to_string(),
        ))
    }

    pub fn status(&self, token_store: &dyn GoogleTokenStore) -> GoogleProviderStatus {
        let accounts = self
            .config
            .accounts
            .iter()
            .map(|account| GoogleAccountStatus {
                id: account.id.clone(),
                authenticated: token_store.load(&account.id).ok().flatten().is_some(),
                calendars: account.calendars.clone(),
            })
            .collect();
        GoogleProviderStatus {
            enabled: self.config.enabled,
            cache_file: self.config.cache_file.clone(),
            event_count: self.agenda_source().event_count(),
            accounts,
        }
    }

    pub fn sync(
        &mut self,
        account_id: Option<&str>,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
        now: CalendarDate,
    ) -> Result<GoogleSyncSummary, ProviderError> {
        if !self.config.enabled {
            return Err(ProviderError::Config(
                "Google provider is disabled".to_string(),
            ));
        }
        let mut summary = GoogleSyncSummary::default();
        let accounts = match account_id {
            Some(account_id) => {
                vec![self.config.account(account_id).cloned().ok_or_else(|| {
                    ProviderError::Config(format!(
                        "Google account '{account_id}' is not configured"
                    ))
                })?]
            }
            None => self.config.accounts.clone(),
        };
        for account in accounts {
            let token = google_access_token(&account, http, token_store)?;
            let calendar_ids = account.calendars.clone();
            for calendar_id in calendar_ids {
                let calendar = fetch_google_calendar(http, &token, &calendar_id)?;
                let start = now.add_days(-self.config.sync_past_days);
                let end = now.add_days(self.config.sync_future_days);
                let events = fetch_google_events(http, &token, &account.id, &calendar, start, end)?;
                summary.events += events.len();
                summary.calendars += 1;
                self.cache
                    .replace_calendar(&account.id, calendar, events, current_epoch_seconds());
            }
            summary.accounts += 1;
        }

        self.cache.save(&self.config.cache_file)?;
        Ok(summary)
    }

    pub fn create_event(
        &mut self,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let target = self.default_write_target().ok_or_else(|| {
            ProviderError::Config("no Google default calendar configured".to_string())
        })?;
        self.create_event_in_target(draft, &target, http, token_store)
    }

    pub fn create_event_in_target(
        &mut self,
        draft: CreateEventDraft,
        target: &EventWriteTargetId,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let Some((_, account_id, calendar_id)) = target.provider_parts() else {
            return Err(ProviderError::Config(
                "Google provider requires a Google calendar target".to_string(),
            ));
        };
        if !target.is_provider("google") {
            return Err(ProviderError::Config(
                "Google provider requires a Google calendar target".to_string(),
            ));
        }
        let account = self
            .config
            .account(account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!("Google account '{account_id}' is not configured"))
            })?
            .clone();
        if !account
            .calendars
            .iter()
            .any(|calendar| calendar == calendar_id)
        {
            return Err(ProviderError::Config(format!(
                "Google calendar '{calendar_id}' is not configured for account '{account_id}'"
            )));
        }
        let token = google_access_token(&account, http, token_store)?;
        let body = google_event_payload(&draft, false)?;
        let response = google_request(
            http,
            "POST",
            &format!(
                "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events",
                percent_encode(calendar_id)
            ),
            &token,
            Some(body.to_string()),
        )?;
        let value = parse_google_success_json(response)?;
        let calendar =
            fetch_google_calendar(http, &token, calendar_id).unwrap_or(GoogleCalendarRecord {
                id: calendar_id.to_string(),
                name: calendar_id.to_string(),
                can_edit: true,
                is_default: false,
            });
        let cached = GoogleCachedEvent::from_google(&account.id, &calendar, value)?;
        self.cache
            .upsert_event(&account.id, calendar, cached.clone());
        self.cache.save(&self.config.cache_file)?;
        cached.to_event().ok_or_else(|| {
            ProviderError::Mapping("created Google event could not be converted".to_string())
        })
    }

    pub fn update_event(
        &mut self,
        id: &str,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let metadata = self
            .cache
            .metadata_for_event(id)
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let account = self
            .config
            .account(&metadata.account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "Google account '{}' is not configured",
                    metadata.account_id
                ))
            })?
            .clone();
        let token = google_access_token(&account, http, token_store)?;
        let body = google_event_payload(&draft, true)?;
        let response = google_request(
            http,
            "PATCH",
            &format!(
                "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events/{}",
                percent_encode(&metadata.calendar_id),
                percent_encode(&metadata.google_id)
            ),
            &token,
            Some(body.to_string()),
        )?;
        let value = parse_google_success_json(response)?;
        let calendar = self
            .cache
            .calendar_record(&metadata.account_id, &metadata.calendar_id)
            .unwrap_or(GoogleCalendarRecord {
                id: metadata.calendar_id.clone(),
                name: metadata.calendar_id.clone(),
                can_edit: true,
                is_default: false,
            });
        let cached = GoogleCachedEvent::from_google(&metadata.account_id, &calendar, value)?;
        self.cache.remove_occurrences_for_series(&cached.id);
        self.cache
            .upsert_event(&metadata.account_id, calendar, cached.clone());
        self.cache.save(&self.config.cache_file)?;
        cached.to_event().ok_or_else(|| {
            ProviderError::Mapping("updated Google event could not be converted".to_string())
        })
    }

    pub fn update_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        draft: CreateEventDraft,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.update_event(
            &id,
            draft.without_recurrence_for_provider(),
            http,
            token_store,
        )
    }

    pub fn delete_event(
        &mut self,
        id: &str,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let metadata = self
            .cache
            .metadata_for_event(id)
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let event = self
            .cache
            .event_by_id(id)
            .and_then(|cached| cached.to_event())
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        let account = self
            .config
            .account(&metadata.account_id)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "Google account '{}' is not configured",
                    metadata.account_id
                ))
            })?
            .clone();
        let token = google_access_token(&account, http, token_store)?;
        let response = google_request(
            http,
            "DELETE",
            &format!(
                "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events/{}",
                percent_encode(&metadata.calendar_id),
                percent_encode(&metadata.google_id)
            ),
            &token,
            None,
        )?;
        parse_google_empty_success(response)?;
        self.cache.remove_event(id);
        self.cache.save(&self.config.cache_file)?;
        Ok(event)
    }

    pub fn delete_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<(), ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.delete_event(&id, http, token_store).map(|_| ())
    }

    pub fn duplicate_event(
        &mut self,
        id: &str,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let event = self
            .cache
            .event_by_id(id)
            .and_then(|cached| cached.to_event())
            .ok_or_else(|| ProviderError::NotFound(id.to_string()))?;
        self.create_event(
            CreateEventDraft::from_event(&event).without_recurrence_for_provider(),
            http,
            token_store,
        )
    }

    pub fn duplicate_occurrence(
        &mut self,
        series_id: &str,
        anchor: OccurrenceAnchor,
        http: &dyn MicrosoftHttpClient,
        token_store: &dyn GoogleTokenStore,
    ) -> Result<Event, ProviderError> {
        let id = self
            .cache
            .event_id_for_anchor(series_id, anchor)
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{series_id}:{}", anchor_label(anchor)))
            })?;
        self.duplicate_event(&id, http, token_store)
    }
}

trait ProviderDraftExt {
    fn without_recurrence_for_provider(self) -> Self;
}

impl ProviderDraftExt for CreateEventDraft {
    fn without_recurrence_for_provider(mut self) -> Self {
        self.recurrence = None;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftProviderStatus {
    pub enabled: bool,
    pub cache_file: PathBuf,
    pub event_count: usize,
    pub accounts: Vec<MicrosoftAccountStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftAccountStatus {
    pub id: String,
    pub authenticated: bool,
    pub calendars: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MicrosoftSyncSummary {
    pub accounts: usize,
    pub calendars: usize,
    pub events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftEventMetadata {
    pub account_id: String,
    pub calendar_id: String,
    pub graph_id: String,
    pub series_master_id: Option<String>,
    pub occurrence_anchor: Option<OccurrenceAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleProviderStatus {
    pub enabled: bool,
    pub cache_file: PathBuf,
    pub event_count: usize,
    pub accounts: Vec<GoogleAccountStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAccountStatus {
    pub id: String,
    pub authenticated: bool,
    pub calendars: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GoogleSyncSummary {
    pub accounts: usize,
    pub calendars: usize,
    pub events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleEventMetadata {
    pub account_id: String,
    pub calendar_id: String,
    pub google_id: String,
    pub recurring_event_id: Option<String>,
    pub occurrence_anchor: Option<OccurrenceAnchor>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct GoogleCacheFile {
    version: u8,
    #[serde(default)]
    accounts: Vec<GoogleCacheAccount>,
}

impl GoogleCacheFile {
    fn empty() -> Self {
        Self {
            version: GOOGLE_CACHE_VERSION,
            accounts: Vec::new(),
        }
    }

    fn load(path: &Path) -> Result<Self, ProviderError> {
        let body = match fs::read_to_string(path) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(err) => {
                return Err(ProviderError::CacheRead {
                    path: path.to_path_buf(),
                    reason: err.to_string(),
                });
            }
        };
        let file =
            serde_json::from_str::<Self>(&body).map_err(|err| ProviderError::CacheParse {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        if file.version != GOOGLE_CACHE_VERSION {
            return Err(ProviderError::CacheParse {
                path: path.to_path_buf(),
                reason: format!("unsupported Google cache version {}", file.version),
            });
        }
        Ok(file)
    }

    fn save(&self, path: &Path) -> Result<(), ProviderError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| ProviderError::CacheWrite {
                path: parent.to_path_buf(),
                reason: err.to_string(),
            })?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|err| ProviderError::CacheWrite {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
        let temp_path = path.with_extension("json.tmp");
        fs::write(&temp_path, body).map_err(|err| ProviderError::CacheWrite {
            path: temp_path.clone(),
            reason: err.to_string(),
        })?;
        fs::rename(&temp_path, path).map_err(|err| ProviderError::CacheWrite {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })
    }

    fn replace_calendar(
        &mut self,
        account_id: &str,
        calendar: GoogleCalendarRecord,
        events: Vec<GoogleCachedEvent>,
        synced_at_epoch_seconds: u64,
    ) {
        let account = self.account_mut(account_id);
        if let Some(existing) = account
            .calendars
            .iter_mut()
            .find(|existing| existing.id == calendar.id)
        {
            existing.name = calendar.name;
            existing.can_edit = calendar.can_edit;
            existing.is_default = calendar.is_default;
            existing.last_synced_at_epoch_seconds = Some(synced_at_epoch_seconds);
            existing.events = events;
        } else {
            account.calendars.push(GoogleCacheCalendar {
                id: calendar.id,
                name: calendar.name,
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
                sync_token: None,
                last_synced_at_epoch_seconds: Some(synced_at_epoch_seconds),
                events,
            });
        }
        account
            .calendars
            .sort_by(|left, right| left.id.cmp(&right.id));
    }

    fn upsert_event(
        &mut self,
        account_id: &str,
        calendar: GoogleCalendarRecord,
        event: GoogleCachedEvent,
    ) {
        let account = self.account_mut(account_id);
        let calendar_record = if let Some(existing) = account
            .calendars
            .iter_mut()
            .find(|existing| existing.id == calendar.id)
        {
            existing
        } else {
            account.calendars.push(GoogleCacheCalendar {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
                sync_token: None,
                last_synced_at_epoch_seconds: None,
                events: Vec::new(),
            });
            account.calendars.last_mut().expect("calendar was pushed")
        };
        if let Some(existing) = calendar_record
            .events
            .iter_mut()
            .find(|existing| existing.id == event.id)
        {
            *existing = event;
        } else {
            calendar_record.events.push(event);
        }
        calendar_record
            .events
            .sort_by(|left, right| left.id.cmp(&right.id));
    }

    fn remove_event(&mut self, id: &str) {
        for calendar in self
            .accounts
            .iter_mut()
            .flat_map(|account| &mut account.calendars)
        {
            calendar.events.retain(|event| {
                event.id != id && event.series_master_app_id.as_deref() != Some(id)
            });
        }
    }

    fn remove_occurrences_for_series(&mut self, series_id: &str) {
        for calendar in self
            .accounts
            .iter_mut()
            .flat_map(|account| &mut account.calendars)
        {
            calendar
                .events
                .retain(|event| event.series_master_app_id.as_deref() != Some(series_id));
        }
    }

    fn metadata_for_event(&self, id: &str) -> Option<GoogleEventMetadata> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .map(GoogleCachedEvent::metadata)
    }

    fn event_by_id(&self, id: &str) -> Option<GoogleCachedEvent> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .cloned()
    }

    fn event_id_for_anchor(&self, series_id: &str, anchor: OccurrenceAnchor) -> Option<String> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| {
                event
                    .occurrence_anchor()
                    .map(|event_anchor| event_anchor == anchor)
                    .unwrap_or(false)
                    && event.series_master_app_id.as_deref() == Some(series_id)
            })
            .map(|event| event.id.clone())
    }

    fn calendar_record(&self, account_id: &str, calendar_id: &str) -> Option<GoogleCalendarRecord> {
        self.accounts
            .iter()
            .find(|account| account.id == account_id)?
            .calendars
            .iter()
            .find(|calendar| calendar.id == calendar_id)
            .map(|calendar| GoogleCalendarRecord {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
            })
    }

    fn account_mut(&mut self, account_id: &str) -> &mut GoogleCacheAccount {
        if let Some(index) = self
            .accounts
            .iter()
            .position(|account| account.id == account_id)
        {
            &mut self.accounts[index]
        } else {
            self.accounts.push(GoogleCacheAccount {
                id: account_id.to_string(),
                calendars: Vec::new(),
            });
            self.accounts.last_mut().expect("account was pushed")
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoogleCacheAccount {
    id: String,
    #[serde(default)]
    calendars: Vec<GoogleCacheCalendar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoogleCacheCalendar {
    id: String,
    name: String,
    #[serde(default)]
    can_edit: bool,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    sync_token: Option<String>,
    #[serde(default)]
    last_synced_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    events: Vec<GoogleCachedEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoogleCachedEvent {
    id: String,
    account_id: String,
    calendar_id: String,
    calendar_name: String,
    google_id: String,
    #[serde(default)]
    event_type: Option<String>,
    #[serde(default)]
    recurring_event_id: Option<String>,
    #[serde(default)]
    series_master_app_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    title: String,
    timing: GoogleCachedTiming,
    #[serde(default)]
    original_start: Option<GoogleCachedTiming>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    reminders_minutes_before: Vec<u16>,
    #[serde(default)]
    recurrence: Option<MicrosoftCachedRecurrence>,
    #[serde(default)]
    raw: Value,
}

impl GoogleCachedEvent {
    fn from_google(
        account_id: &str,
        calendar: &GoogleCalendarRecord,
        raw: Value,
    ) -> Result<Self, ProviderError> {
        let google_id = graph_string(&raw, "id")
            .ok_or_else(|| ProviderError::Mapping("Google event is missing id".to_string()))?;
        let title = graph_string(&raw, "summary").unwrap_or_else(|| "(Untitled)".to_string());
        let timing = google_timing(&raw, "start", "end")?;
        let original_start = raw
            .get("originalStartTime")
            .map(google_single_time)
            .transpose()?;
        let recurring_event_id = graph_string(&raw, "recurringEventId");
        let series_master_app_id = recurring_event_id
            .as_ref()
            .map(|id| google_event_app_id(account_id, &calendar.id, id));
        let recurrence = raw
            .get("recurrence")
            .and_then(Value::as_array)
            .and_then(|rules| {
                rules
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|rule| rule.starts_with("RRULE:"))
            })
            .and_then(google_rrule_to_cache);
        let reminders_minutes_before = raw
            .get("reminders")
            .and_then(|reminders| reminders.get("overrides"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|reminder| graph_i64(reminder, "minutes"))
            .filter_map(|value| u16::try_from(value).ok())
            .collect();

        Ok(Self {
            id: google_event_app_id(account_id, &calendar.id, &google_id),
            account_id: account_id.to_string(),
            calendar_id: calendar.id.clone(),
            calendar_name: calendar.name.clone(),
            google_id,
            event_type: if recurrence.is_some() && recurring_event_id.is_none() {
                Some("recurringMaster".to_string())
            } else {
                graph_string(&raw, "eventType")
            },
            recurring_event_id,
            series_master_app_id,
            status: graph_string(&raw, "status"),
            title,
            timing,
            original_start,
            location: graph_string(&raw, "location").filter(|value| !value.trim().is_empty()),
            notes: graph_string(&raw, "description").filter(|value| !value.trim().is_empty()),
            reminders_minutes_before,
            recurrence,
            raw,
        })
    }

    fn to_event(&self) -> Option<Event> {
        if self.status.as_deref() == Some("cancelled") {
            return None;
        }
        let source = SourceMetadata::new(
            format!("google:{}:{}", self.account_id, self.calendar_id),
            format!("Google {}/{}", self.account_id, self.calendar_name),
        )
        .with_external_id(self.google_id.clone());
        let mut event = match self.timing {
            GoogleCachedTiming::AllDay { date } => {
                Event::all_day(self.id.clone(), self.title.clone(), date, source)
            }
            GoogleCachedTiming::Timed { start, end } => {
                Event::timed(self.id.clone(), self.title.clone(), start, end, source).ok()?
            }
        };
        event.location = self.location.clone();
        event.notes = self.notes.clone();
        event.reminders = self
            .reminders_minutes_before
            .iter()
            .copied()
            .map(Reminder::minutes_before)
            .collect();
        event.recurrence = self
            .recurrence
            .as_ref()
            .and_then(MicrosoftCachedRecurrence::to_rule);
        if let Some(series_master_app_id) = &self.series_master_app_id {
            event.occurrence = Some(OccurrenceMetadata {
                series_id: series_master_app_id.clone(),
                anchor: self.occurrence_anchor()?,
            });
        }
        Some(event)
    }

    fn metadata(&self) -> GoogleEventMetadata {
        GoogleEventMetadata {
            account_id: self.account_id.clone(),
            calendar_id: self.calendar_id.clone(),
            google_id: self.google_id.clone(),
            recurring_event_id: self.recurring_event_id.clone(),
            occurrence_anchor: self.occurrence_anchor(),
        }
    }

    fn occurrence_anchor(&self) -> Option<OccurrenceAnchor> {
        let timing = self.original_start.unwrap_or(self.timing);
        match timing {
            GoogleCachedTiming::AllDay { date } => Some(OccurrenceAnchor::AllDay { date }),
            GoogleCachedTiming::Timed { start, .. } => Some(OccurrenceAnchor::Timed { start }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum GoogleCachedTiming {
    AllDay {
        date: CalendarDate,
    },
    Timed {
        start: EventDateTime,
        end: EventDateTime,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct MicrosoftCacheFile {
    version: u8,
    #[serde(default)]
    accounts: Vec<MicrosoftCacheAccount>,
}

impl MicrosoftCacheFile {
    fn empty() -> Self {
        Self {
            version: MICROSOFT_CACHE_VERSION,
            accounts: Vec::new(),
        }
    }

    fn load(path: &Path) -> Result<Self, ProviderError> {
        let body = match fs::read_to_string(path) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(err) => {
                return Err(ProviderError::CacheRead {
                    path: path.to_path_buf(),
                    reason: err.to_string(),
                });
            }
        };
        let file =
            serde_json::from_str::<Self>(&body).map_err(|err| ProviderError::CacheParse {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        if file.version != MICROSOFT_CACHE_VERSION {
            return Err(ProviderError::CacheParse {
                path: path.to_path_buf(),
                reason: format!("unsupported Microsoft cache version {}", file.version),
            });
        }
        Ok(file)
    }

    fn save(&self, path: &Path) -> Result<(), ProviderError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| ProviderError::CacheWrite {
                path: parent.to_path_buf(),
                reason: err.to_string(),
            })?;
        }
        let body = serde_json::to_string_pretty(self).map_err(|err| ProviderError::CacheWrite {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
        let temp_path = path.with_extension("json.tmp");
        fs::write(&temp_path, body).map_err(|err| ProviderError::CacheWrite {
            path: temp_path.clone(),
            reason: err.to_string(),
        })?;
        fs::rename(&temp_path, path).map_err(|err| ProviderError::CacheWrite {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })
    }

    fn replace_calendar(
        &mut self,
        account_id: &str,
        calendar: MicrosoftCalendarRecord,
        events: Vec<MicrosoftCachedEvent>,
        synced_at_epoch_seconds: u64,
    ) {
        let account = self.account_mut(account_id);
        if let Some(existing) = account
            .calendars
            .iter_mut()
            .find(|existing| existing.id == calendar.id)
        {
            existing.name = calendar.name;
            existing.can_edit = calendar.can_edit;
            existing.is_default = calendar.is_default;
            existing.last_synced_at_epoch_seconds = Some(synced_at_epoch_seconds);
            existing.events = events;
        } else {
            account.calendars.push(MicrosoftCacheCalendar {
                id: calendar.id,
                name: calendar.name,
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
                delta_link: None,
                last_synced_at_epoch_seconds: Some(synced_at_epoch_seconds),
                events,
            });
        }
        account
            .calendars
            .sort_by(|left, right| left.id.cmp(&right.id));
    }

    fn upsert_event(
        &mut self,
        account_id: &str,
        calendar: MicrosoftCalendarRecord,
        event: MicrosoftCachedEvent,
    ) {
        let account = self.account_mut(account_id);
        let calendar_record = if let Some(existing) = account
            .calendars
            .iter_mut()
            .find(|existing| existing.id == calendar.id)
        {
            existing
        } else {
            account.calendars.push(MicrosoftCacheCalendar {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
                delta_link: None,
                last_synced_at_epoch_seconds: None,
                events: Vec::new(),
            });
            account.calendars.last_mut().expect("calendar was pushed")
        };
        if let Some(existing) = calendar_record
            .events
            .iter_mut()
            .find(|existing| existing.id == event.id)
        {
            *existing = event;
        } else {
            calendar_record.events.push(event);
        }
        calendar_record
            .events
            .sort_by(|left, right| left.id.cmp(&right.id));
    }

    fn remove_event(&mut self, id: &str) {
        for calendar in self
            .accounts
            .iter_mut()
            .flat_map(|account| &mut account.calendars)
        {
            calendar.events.retain(|event| {
                event.id != id && event.series_master_app_id.as_deref() != Some(id)
            });
        }
    }

    fn remove_occurrences_for_series(&mut self, series_id: &str) {
        for calendar in self
            .accounts
            .iter_mut()
            .flat_map(|account| &mut account.calendars)
        {
            calendar
                .events
                .retain(|event| event.series_master_app_id.as_deref() != Some(series_id));
        }
    }

    fn metadata_for_event(&self, id: &str) -> Option<MicrosoftEventMetadata> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .map(MicrosoftCachedEvent::metadata)
    }

    fn event_by_id(&self, id: &str) -> Option<MicrosoftCachedEvent> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| event.id == id)
            .cloned()
    }

    fn event_id_for_anchor(&self, series_id: &str, anchor: OccurrenceAnchor) -> Option<String> {
        self.accounts
            .iter()
            .flat_map(|account| &account.calendars)
            .flat_map(|calendar| &calendar.events)
            .find(|event| {
                event
                    .occurrence_anchor()
                    .map(|event_anchor| event_anchor == anchor)
                    .unwrap_or(false)
                    && event.series_master_app_id.as_deref() == Some(series_id)
            })
            .map(|event| event.id.clone())
    }

    fn calendar_record(
        &self,
        account_id: &str,
        calendar_id: &str,
    ) -> Option<MicrosoftCalendarRecord> {
        self.accounts
            .iter()
            .find(|account| account.id == account_id)?
            .calendars
            .iter()
            .find(|calendar| calendar.id == calendar_id)
            .map(|calendar| MicrosoftCalendarRecord {
                id: calendar.id.clone(),
                name: calendar.name.clone(),
                can_edit: calendar.can_edit,
                is_default: calendar.is_default,
            })
    }

    fn account_mut(&mut self, account_id: &str) -> &mut MicrosoftCacheAccount {
        if let Some(index) = self
            .accounts
            .iter()
            .position(|account| account.id == account_id)
        {
            &mut self.accounts[index]
        } else {
            self.accounts.push(MicrosoftCacheAccount {
                id: account_id.to_string(),
                calendars: Vec::new(),
            });
            self.accounts.last_mut().expect("account was pushed")
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MicrosoftCacheAccount {
    id: String,
    #[serde(default)]
    calendars: Vec<MicrosoftCacheCalendar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MicrosoftCacheCalendar {
    id: String,
    name: String,
    #[serde(default)]
    can_edit: bool,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    delta_link: Option<String>,
    #[serde(default)]
    last_synced_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    events: Vec<MicrosoftCachedEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MicrosoftCachedEvent {
    id: String,
    account_id: String,
    calendar_id: String,
    calendar_name: String,
    graph_id: String,
    #[serde(default)]
    event_type: Option<String>,
    #[serde(default)]
    series_master_id: Option<String>,
    #[serde(default)]
    series_master_app_id: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
    #[serde(default)]
    change_key: Option<String>,
    title: String,
    timing: MicrosoftCachedTiming,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    reminders_minutes_before: Vec<u16>,
    #[serde(default)]
    recurrence: Option<MicrosoftCachedRecurrence>,
    #[serde(default)]
    raw: Value,
}

impl MicrosoftCachedEvent {
    fn from_graph(
        account_id: &str,
        calendar: &MicrosoftCalendarRecord,
        raw: Value,
    ) -> Result<Self, ProviderError> {
        let graph_id = graph_string(&raw, "id")
            .ok_or_else(|| ProviderError::Mapping("Graph event is missing id".to_string()))?;
        let event_type = graph_string(&raw, "type");
        let title = graph_string(&raw, "subject").unwrap_or_else(|| "(Untitled)".to_string());
        let is_all_day = graph_bool(&raw, "isAllDay").unwrap_or(false);
        let start = graph_datetime(&raw, "start")?;
        let end = graph_datetime(&raw, "end")?;
        let timing = if is_all_day {
            MicrosoftCachedTiming::AllDay { date: start.date }
        } else {
            MicrosoftCachedTiming::Timed { start, end }
        };
        let series_master_id = graph_string(&raw, "seriesMasterId");
        let series_master_app_id = series_master_id
            .as_ref()
            .map(|id| microsoft_event_app_id(account_id, &calendar.id, id));
        let recurrence = raw
            .get("recurrence")
            .filter(|value| !value.is_null())
            .map(graph_recurrence_to_cache)
            .transpose()?;

        let reminders_minutes_before = if graph_bool(&raw, "isReminderOn").unwrap_or(false) {
            graph_i64(&raw, "reminderMinutesBeforeStart")
                .and_then(|value| u16::try_from(value).ok())
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        Ok(Self {
            id: microsoft_event_app_id(account_id, &calendar.id, &graph_id),
            account_id: account_id.to_string(),
            calendar_id: calendar.id.clone(),
            calendar_name: calendar.name.clone(),
            graph_id,
            event_type,
            series_master_id,
            series_master_app_id,
            occurrence_id: graph_string(&raw, "occurrenceId"),
            change_key: graph_string(&raw, "changeKey"),
            title,
            timing,
            location: raw
                .get("location")
                .and_then(|location| graph_string(location, "displayName"))
                .filter(|value| !value.trim().is_empty()),
            notes: graph_string(&raw, "bodyPreview")
                .or_else(|| {
                    raw.get("body")
                        .and_then(|body| graph_string(body, "content"))
                })
                .filter(|value| !value.trim().is_empty()),
            reminders_minutes_before,
            recurrence,
            raw,
        })
    }

    fn to_event(&self) -> Option<Event> {
        let source = SourceMetadata::new(
            format!("microsoft:{}:{}", self.account_id, self.calendar_id),
            format!("Microsoft {}/{}", self.account_id, self.calendar_name),
        )
        .with_external_id(self.graph_id.clone());
        let mut event = match self.timing {
            MicrosoftCachedTiming::AllDay { date } => {
                Event::all_day(self.id.clone(), self.title.clone(), date, source)
            }
            MicrosoftCachedTiming::Timed { start, end } => {
                Event::timed(self.id.clone(), self.title.clone(), start, end, source).ok()?
            }
        };
        event.location = self.location.clone();
        event.notes = self.notes.clone();
        event.reminders = self
            .reminders_minutes_before
            .iter()
            .copied()
            .map(Reminder::minutes_before)
            .collect();
        event.recurrence = self
            .recurrence
            .as_ref()
            .and_then(MicrosoftCachedRecurrence::to_rule);
        if let Some(series_master_app_id) = &self.series_master_app_id {
            event.occurrence = Some(OccurrenceMetadata {
                series_id: series_master_app_id.clone(),
                anchor: self.occurrence_anchor()?,
            });
        }
        Some(event)
    }

    fn metadata(&self) -> MicrosoftEventMetadata {
        MicrosoftEventMetadata {
            account_id: self.account_id.clone(),
            calendar_id: self.calendar_id.clone(),
            graph_id: self.graph_id.clone(),
            series_master_id: self.series_master_id.clone(),
            occurrence_anchor: self.occurrence_anchor(),
        }
    }

    fn occurrence_anchor(&self) -> Option<OccurrenceAnchor> {
        match self.timing {
            MicrosoftCachedTiming::AllDay { date } => Some(OccurrenceAnchor::AllDay { date }),
            MicrosoftCachedTiming::Timed { start, .. } => Some(OccurrenceAnchor::Timed { start }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum MicrosoftCachedTiming {
    AllDay {
        date: CalendarDate,
    },
    Timed {
        start: EventDateTime,
        end: EventDateTime,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MicrosoftCachedRecurrence {
    frequency: RecurrenceFrequencyRecord,
    interval: u16,
    end: RecurrenceEndRecord,
    #[serde(default)]
    weekdays: Vec<WeekdayRecord>,
    #[serde(default)]
    monthly: Option<MonthlyRuleRecord>,
    #[serde(default)]
    yearly: Option<YearlyRuleRecord>,
}

impl MicrosoftCachedRecurrence {
    fn to_rule(&self) -> Option<RecurrenceRule> {
        let frequency = match self.frequency {
            RecurrenceFrequencyRecord::Daily => RecurrenceFrequency::Daily,
            RecurrenceFrequencyRecord::Weekly => RecurrenceFrequency::Weekly,
            RecurrenceFrequencyRecord::Monthly => RecurrenceFrequency::Monthly,
            RecurrenceFrequencyRecord::Yearly => RecurrenceFrequency::Yearly,
        };
        Some(RecurrenceRule {
            frequency,
            interval: self.interval.max(1),
            end: self.end.to_rule()?,
            weekdays: self
                .weekdays
                .iter()
                .copied()
                .map(WeekdayRecord::to_weekday)
                .collect::<Option<Vec<_>>>()?,
            monthly: self.monthly.and_then(MonthlyRuleRecord::to_rule),
            yearly: self.yearly.and_then(YearlyRuleRecord::to_rule),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecurrenceFrequencyRecord {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum RecurrenceEndRecord {
    Never,
    Until { date: CalendarDate },
    Count { count: u32 },
}

impl RecurrenceEndRecord {
    fn to_rule(self) -> Option<RecurrenceEnd> {
        match self {
            Self::Never => Some(RecurrenceEnd::Never),
            Self::Until { date } => Some(RecurrenceEnd::Until(date)),
            Self::Count { count } => Some(RecurrenceEnd::Count(count)),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WeekdayRecord {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl WeekdayRecord {
    fn from_weekday(weekday: Weekday) -> Self {
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

    fn to_weekday(self) -> Option<Weekday> {
        Some(match self {
            Self::Sunday => Weekday::Sunday,
            Self::Monday => Weekday::Monday,
            Self::Tuesday => Weekday::Tuesday,
            Self::Wednesday => Weekday::Wednesday,
            Self::Thursday => Weekday::Thursday,
            Self::Friday => Weekday::Friday,
            Self::Saturday => Weekday::Saturday,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum MonthlyRuleRecord {
    DayOfMonth {
        day: u8,
    },
    WeekdayOrdinal {
        ordinal: OrdinalRecord,
        weekday: WeekdayRecord,
    },
}

impl MonthlyRuleRecord {
    fn to_rule(self) -> Option<RecurrenceMonthlyRule> {
        match self {
            Self::DayOfMonth { day } => Some(RecurrenceMonthlyRule::DayOfMonth(day)),
            Self::WeekdayOrdinal { ordinal, weekday } => {
                Some(RecurrenceMonthlyRule::WeekdayOrdinal {
                    ordinal: ordinal.to_rule(),
                    weekday: weekday.to_weekday()?,
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum YearlyRuleRecord {
    Date {
        month: u8,
        day: u8,
    },
    WeekdayOrdinal {
        month: u8,
        ordinal: OrdinalRecord,
        weekday: WeekdayRecord,
    },
}

impl YearlyRuleRecord {
    fn to_rule(self) -> Option<RecurrenceYearlyRule> {
        match self {
            Self::Date { month, day } => Some(RecurrenceYearlyRule::Date {
                month: Month::try_from(month).ok()?,
                day,
            }),
            Self::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            } => Some(RecurrenceYearlyRule::WeekdayOrdinal {
                month: Month::try_from(month).ok()?,
                ordinal: ordinal.to_rule(),
                weekday: weekday.to_weekday()?,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OrdinalRecord {
    First,
    Second,
    Third,
    Fourth,
    Last,
}

impl OrdinalRecord {
    fn from_rule(ordinal: RecurrenceOrdinal) -> Self {
        match ordinal {
            RecurrenceOrdinal::Number(1) => Self::First,
            RecurrenceOrdinal::Number(2) => Self::Second,
            RecurrenceOrdinal::Number(3) => Self::Third,
            RecurrenceOrdinal::Number(_) => Self::Fourth,
            RecurrenceOrdinal::Last => Self::Last,
        }
    }

    fn to_rule(self) -> RecurrenceOrdinal {
        match self {
            Self::First => RecurrenceOrdinal::Number(1),
            Self::Second => RecurrenceOrdinal::Number(2),
            Self::Third => RecurrenceOrdinal::Number(3),
            Self::Fourth => RecurrenceOrdinal::Number(4),
            Self::Last => RecurrenceOrdinal::Last,
        }
    }
}

#[derive(Debug, Clone)]
struct MicrosoftCalendarRecord {
    id: String,
    name: String,
    can_edit: bool,
    is_default: bool,
}

#[derive(Debug, Clone)]
struct GoogleCalendarRecord {
    id: String,
    name: String,
    can_edit: bool,
    is_default: bool,
}

pub trait MicrosoftTokenStore {
    fn load(&self, account_id: &str) -> Result<Option<MicrosoftToken>, ProviderError>;
    fn save(&self, account_id: &str, token: &MicrosoftToken) -> Result<(), ProviderError>;
    fn delete(&self, account_id: &str) -> Result<(), ProviderError>;
}

#[derive(Debug, Default)]
pub struct KeyringMicrosoftTokenStore;

impl MicrosoftTokenStore for KeyringMicrosoftTokenStore {
    fn load(&self, account_id: &str) -> Result<Option<MicrosoftToken>, ProviderError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(err.to_string()))?;
        match entry.get_password() {
            Ok(body) => serde_json::from_str(&body)
                .map(Some)
                .map_err(|err| ProviderError::Keyring(err.to_string())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(ProviderError::Keyring(err.to_string())),
        }
    }

    fn save(&self, account_id: &str, token: &MicrosoftToken) -> Result<(), ProviderError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(err.to_string()))?;
        let body =
            serde_json::to_string(token).map_err(|err| ProviderError::Keyring(err.to_string()))?;
        entry
            .set_password(&body)
            .map_err(|err| ProviderError::Keyring(err.to_string()))
    }

    fn delete(&self, account_id: &str) -> Result<(), ProviderError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(ProviderError::Keyring(err.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicrosoftToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_epoch_seconds: u64,
}

pub trait GoogleTokenStore {
    fn load(&self, account_id: &str) -> Result<Option<GoogleToken>, ProviderError>;
    fn save(&self, account_id: &str, token: &GoogleToken) -> Result<(), ProviderError>;
    fn delete(&self, account_id: &str) -> Result<(), ProviderError>;
}

#[derive(Debug, Default)]
pub struct KeyringGoogleTokenStore;

impl GoogleTokenStore for KeyringGoogleTokenStore {
    fn load(&self, account_id: &str) -> Result<Option<GoogleToken>, ProviderError> {
        let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(format!("Google: {err}")))?;
        match entry.get_password() {
            Ok(body) => serde_json::from_str(&body)
                .map(Some)
                .map_err(|err| ProviderError::Keyring(format!("Google: {err}"))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(ProviderError::Keyring(format!("Google: {err}"))),
        }
    }

    fn save(&self, account_id: &str, token: &GoogleToken) -> Result<(), ProviderError> {
        let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(format!("Google: {err}")))?;
        let body = serde_json::to_string(token)
            .map_err(|err| ProviderError::Keyring(format!("Google: {err}")))?;
        entry
            .set_password(&body)
            .map_err(|err| ProviderError::Keyring(format!("Google: {err}")))
    }

    fn delete(&self, account_id: &str) -> Result<(), ProviderError> {
        let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, account_id)
            .map_err(|err| ProviderError::Keyring(format!("Google: {err}")))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(ProviderError::Keyring(format!("Google: {err}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoogleToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_epoch_seconds: u64,
}

pub trait MicrosoftHttpClient {
    fn request(
        &self,
        request: MicrosoftHttpRequest,
    ) -> Result<MicrosoftHttpResponse, ProviderError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct ReqwestMicrosoftHttpClient;

impl MicrosoftHttpClient for ReqwestMicrosoftHttpClient {
    fn request(
        &self,
        request: MicrosoftHttpRequest,
    ) -> Result<MicrosoftHttpResponse, ProviderError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(StdDuration::from_secs(20))
            .user_agent("rcal/0.1")
            .build()
            .map_err(|err| ProviderError::Http(err.to_string()))?;
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|err| ProviderError::Http(err.to_string()))?;
        let mut builder = client.request(method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let response = builder
            .send()
            .map_err(|err| ProviderError::Http(err.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = response
            .text()
            .map_err(|err| ProviderError::Http(err.to_string()))?;
        Ok(MicrosoftHttpResponse {
            status,
            headers,
            body,
        })
    }
}

pub fn login_device_code_or_browser(
    account: &MicrosoftAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn MicrosoftTokenStore,
    stdout: &mut dyn Write,
    prefer_browser: bool,
) -> Result<(), ProviderError> {
    let result = if prefer_browser {
        login_browser(account, http, token_store, stdout)
    } else {
        login_device_code(account, http, token_store, stdout)
    };
    match result {
        Ok(()) => Ok(()),
        Err(err) if !prefer_browser => {
            let _ = writeln!(stdout, "device-code login failed: {err}");
            let _ = writeln!(stdout, "falling back to browser login");
            login_browser(account, http, token_store, stdout)
        }
        Err(err) => Err(err),
    }
}

pub fn logout(
    account_id: &str,
    token_store: &dyn MicrosoftTokenStore,
) -> Result<(), ProviderError> {
    token_store.delete(account_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrosoftTokenInspection {
    pub account_id: String,
    pub token_format: String,
    pub stored_expires_at_epoch_seconds: u64,
    pub jwt_expires_at_epoch_seconds: Option<i64>,
    pub audience: Option<String>,
    pub scopes: Option<String>,
    pub roles: Vec<String>,
    pub tenant_id: Option<String>,
    pub issuer: Option<String>,
    pub app_id: Option<String>,
    pub authorized_party: Option<String>,
    pub has_refresh_token: bool,
}

pub fn inspect_token(
    account_id: &str,
    token_store: &dyn MicrosoftTokenStore,
) -> Result<MicrosoftTokenInspection, ProviderError> {
    let token = token_store.load(account_id)?.ok_or_else(|| {
        ProviderError::Auth(format!(
            "Microsoft account '{account_id}' is not authenticated"
        ))
    })?;
    let claims = access_token_claims(&token.access_token)?;
    let claims_ref = claims.as_ref();
    Ok(MicrosoftTokenInspection {
        account_id: account_id.to_string(),
        token_format: if claims_ref.is_some() {
            "jwt"
        } else {
            "opaque"
        }
        .to_string(),
        stored_expires_at_epoch_seconds: token.expires_at_epoch_seconds,
        jwt_expires_at_epoch_seconds: claims_ref.and_then(|claims| graph_i64(claims, "exp")),
        audience: claims_ref.and_then(|claims| graph_string(claims, "aud")),
        scopes: claims_ref.and_then(|claims| graph_string(claims, "scp")),
        roles: claims_ref
            .and_then(|claims| claims.get("roles"))
            .and_then(Value::as_array)
            .map(|roles| {
                roles
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        tenant_id: claims_ref.and_then(|claims| graph_string(claims, "tid")),
        issuer: claims_ref.and_then(|claims| graph_string(claims, "iss")),
        app_id: claims_ref.and_then(|claims| graph_string(claims, "appid")),
        authorized_party: claims_ref.and_then(|claims| graph_string(claims, "azp")),
        has_refresh_token: !token.refresh_token.is_empty(),
    })
}

fn login_device_code(
    account: &MicrosoftAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn MicrosoftTokenStore,
    stdout: &mut dyn Write,
) -> Result<(), ProviderError> {
    let body = form_body(&[
        ("client_id", account.client_id.as_str()),
        ("scope", MICROSOFT_SCOPES),
    ]);
    let response = http.request(MicrosoftHttpRequest {
        method: "POST".to_string(),
        url: account.device_code_url(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: Some(body),
    })?;
    let value = parse_oauth_json(response)?;
    let device_code = required_json_string(&value, "device_code")?;
    let user_code = required_json_string(&value, "user_code")?;
    let verification_uri = graph_string(&value, "verification_uri")
        .or_else(|| graph_string(&value, "verification_url"))
        .ok_or_else(|| {
            ProviderError::Auth("device-code response is missing verification URL".to_string())
        })?;
    let message = graph_string(&value, "message")
        .unwrap_or_else(|| format!("Visit {verification_uri} and enter code {user_code}"));
    let expires_in = graph_i64(&value, "expires_in").unwrap_or(900).max(1) as u64;
    let mut interval = graph_i64(&value, "interval").unwrap_or(5).max(1) as u64;
    writeln!(stdout, "{message}").map_err(|err| ProviderError::Auth(err.to_string()))?;

    let started = current_epoch_seconds();
    loop {
        if current_epoch_seconds().saturating_sub(started) > expires_in {
            return Err(ProviderError::Auth(
                "device-code login timed out".to_string(),
            ));
        }
        thread::sleep(StdDuration::from_secs(interval));
        let body = form_body(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", account.client_id.as_str()),
            ("device_code", &device_code),
        ]);
        let response = http.request(MicrosoftHttpRequest {
            method: "POST".to_string(),
            url: account.token_url(),
            headers: vec![(
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: Some(body),
        })?;
        if response.status == 200 {
            let token = token_from_response(response)?;
            token_store.save(&account.id, &token)?;
            writeln!(stdout, "authenticated Microsoft account '{}'", account.id)
                .map_err(|err| ProviderError::Auth(err.to_string()))?;
            return Ok(());
        }
        let value = serde_json::from_str::<Value>(&response.body)
            .map_err(|err| ProviderError::Auth(err.to_string()))?;
        match graph_string(&value, "error").as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = interval.saturating_add(5),
            Some("authorization_declined") => {
                return Err(ProviderError::Auth(
                    "authorization was declined".to_string(),
                ));
            }
            Some("expired_token") => {
                return Err(ProviderError::Auth("device code expired".to_string()));
            }
            Some(error) => {
                return Err(ProviderError::Auth(format!(
                    "{error}: {}",
                    graph_string(&value, "error_description").unwrap_or_default()
                )));
            }
            None => return Err(ProviderError::Auth(response.body)),
        }
    }
}

fn login_browser(
    account: &MicrosoftAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn MicrosoftTokenStore,
    stdout: &mut dyn Write,
) -> Result<(), ProviderError> {
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let state = pkce_verifier();
    let redirect_uri = account.redirect_uri();
    let listener = TcpListener::bind(("127.0.0.1", account.redirect_port)).map_err(|err| {
        ProviderError::Auth(format!("failed to listen for OAuth callback: {err}"))
    })?;
    let auth_url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        account.authorize_url(),
        percent_encode(&account.client_id),
        percent_encode(&redirect_uri),
        percent_encode(MICROSOFT_SCOPES),
        percent_encode(&state),
        percent_encode(&challenge),
    );
    writeln!(stdout, "opening browser for Microsoft login")
        .map_err(|err| ProviderError::Auth(err.to_string()))?;
    open_browser(&auth_url)?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|err| ProviderError::Auth(err.to_string()))?;
    let mut request = String::new();
    BufReader::new(
        stream
            .try_clone()
            .map_err(|err| ProviderError::Auth(err.to_string()))?,
    )
    .read_line(&mut request)
    .map_err(|err| ProviderError::Auth(err.to_string()))?;
    let query = request
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split_once('?').map(|(_, query)| query))
        .ok_or_else(|| ProviderError::Auth("OAuth callback did not include a query".to_string()))?;
    let params = parse_query(query);
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("Microsoft did not provide an error description");
        let message = format!("{error}: {description}");
        let _ = write_oauth_callback_response(&mut stream, "Microsoft", false, &message);
        return Err(ProviderError::Auth(message));
    }
    let code = params.get("code").ok_or_else(|| {
        let message = "OAuth callback did not include code".to_string();
        let _ = write_oauth_callback_response(&mut stream, "Microsoft", false, &message);
        ProviderError::Auth(message)
    })?;
    if params.get("state") != Some(&state) {
        let message = "OAuth callback state mismatch".to_string();
        let _ = write_oauth_callback_response(&mut stream, "Microsoft", false, &message);
        return Err(ProviderError::Auth(message));
    }
    let _ = write_oauth_callback_response(
        &mut stream,
        "Microsoft",
        true,
        "rcal Microsoft login complete. You can close this tab.",
    );
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("client_id", account.client_id.as_str()),
        ("scope", MICROSOFT_SCOPES),
        ("code", code),
        ("redirect_uri", &redirect_uri),
        ("code_verifier", &verifier),
    ]);
    let response = http.request(MicrosoftHttpRequest {
        method: "POST".to_string(),
        url: account.token_url(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: Some(body),
    })?;
    let token = token_from_response(response)?;
    token_store.save(&account.id, &token)?;
    writeln!(stdout, "authenticated Microsoft account '{}'", account.id)
        .map_err(|err| ProviderError::Auth(err.to_string()))
}

fn access_token(
    account: &MicrosoftAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn MicrosoftTokenStore,
) -> Result<String, ProviderError> {
    let token = token_store.load(&account.id)?.ok_or_else(|| {
        ProviderError::Auth(format!(
            "Microsoft account '{}' is not authenticated",
            account.id
        ))
    })?;
    if token.expires_at_epoch_seconds > current_epoch_seconds().saturating_add(120) {
        return Ok(token.access_token);
    }
    let body = form_body(&[
        ("grant_type", "refresh_token"),
        ("client_id", account.client_id.as_str()),
        ("refresh_token", token.refresh_token.as_str()),
        ("scope", MICROSOFT_SCOPES),
    ]);
    let response = http.request(MicrosoftHttpRequest {
        method: "POST".to_string(),
        url: account.token_url(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: Some(body),
    })?;
    let refreshed = token_from_response(response)?;
    token_store.save(&account.id, &refreshed)?;
    Ok(refreshed.access_token)
}

pub fn list_calendars(
    account: &MicrosoftAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn MicrosoftTokenStore,
) -> Result<Vec<MicrosoftCalendarInfo>, ProviderError> {
    let token = access_token(account, http, token_store)?;
    let response = graph_request(
        http,
        "GET",
        &format!("{GRAPH_BASE_URL}/me/calendars?$select=id,name,canEdit,isDefaultCalendar"),
        &token,
        None,
    )?;
    let value = parse_graph_success_json(response)?;
    let calendars = value
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::Mapping("calendar list response is missing value".to_string())
        })?
        .iter()
        .map(|calendar| MicrosoftCalendarInfo {
            id: graph_string(calendar, "id").unwrap_or_default(),
            name: graph_string(calendar, "name").unwrap_or_default(),
            can_edit: graph_bool(calendar, "canEdit").unwrap_or(false),
            is_default: graph_bool(calendar, "isDefaultCalendar").unwrap_or(false),
        })
        .filter(|calendar| !calendar.id.is_empty())
        .collect::<Vec<_>>();
    Ok(calendars)
}

pub fn login_google_browser(
    account: &GoogleAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn GoogleTokenStore,
    stdout: &mut dyn Write,
) -> Result<(), ProviderError> {
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let state = pkce_verifier();
    let redirect_uri = account.redirect_uri();
    let listener = TcpListener::bind(("127.0.0.1", account.redirect_port)).map_err(|err| {
        ProviderError::Auth(format!("failed to listen for Google OAuth callback: {err}"))
    })?;
    let auth_url = format!(
        "{GOOGLE_AUTH_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        percent_encode(&account.client_id),
        percent_encode(&redirect_uri),
        percent_encode(GOOGLE_SCOPES),
        percent_encode(&state),
        percent_encode(&challenge),
    );
    writeln!(stdout, "opening browser for Google login")
        .map_err(|err| ProviderError::Auth(err.to_string()))?;
    open_browser(&auth_url)?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|err| ProviderError::Auth(err.to_string()))?;
    let mut request = String::new();
    BufReader::new(
        stream
            .try_clone()
            .map_err(|err| ProviderError::Auth(err.to_string()))?,
    )
    .read_line(&mut request)
    .map_err(|err| ProviderError::Auth(err.to_string()))?;
    let query = request
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split_once('?').map(|(_, query)| query))
        .ok_or_else(|| {
            ProviderError::Auth("Google OAuth callback did not include a query".to_string())
        })?;
    let params = parse_query(query);
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or("Google did not provide an error description");
        let message = format!("{error}: {description}");
        let _ = write_oauth_callback_response(&mut stream, "Google", false, &message);
        return Err(ProviderError::Auth(message));
    }
    let code = params.get("code").ok_or_else(|| {
        let message = "Google OAuth callback did not include code".to_string();
        let _ = write_oauth_callback_response(&mut stream, "Google", false, &message);
        ProviderError::Auth(message)
    })?;
    if params.get("state") != Some(&state) {
        let message = "Google OAuth callback state mismatch".to_string();
        let _ = write_oauth_callback_response(&mut stream, "Google", false, &message);
        return Err(ProviderError::Auth(message));
    }
    let _ = write_oauth_callback_response(
        &mut stream,
        "Google",
        true,
        "rcal Google login complete. You can close this tab.",
    );
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("client_id", account.client_id.as_str()),
        ("code", code),
        ("redirect_uri", &redirect_uri),
        ("code_verifier", &verifier),
    ];
    if let Some(client_secret) = &account.client_secret {
        fields.push(("client_secret", client_secret.as_str()));
    }
    let response = http.request(MicrosoftHttpRequest {
        method: "POST".to_string(),
        url: GOOGLE_TOKEN_URL.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: Some(form_body(&fields)),
    })?;
    let token = google_token_from_response(response, None)?;
    token_store.save(&account.id, &token)?;
    writeln!(stdout, "authenticated Google account '{}'", account.id)
        .map_err(|err| ProviderError::Auth(err.to_string()))
}

pub fn logout_google(
    account_id: &str,
    token_store: &dyn GoogleTokenStore,
) -> Result<(), ProviderError> {
    token_store.delete(account_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleTokenInspection {
    pub account_id: String,
    pub stored_expires_at_epoch_seconds: u64,
    pub has_refresh_token: bool,
}

pub fn inspect_google_token(
    account_id: &str,
    token_store: &dyn GoogleTokenStore,
) -> Result<GoogleTokenInspection, ProviderError> {
    let token = token_store.load(account_id)?.ok_or_else(|| {
        ProviderError::Auth(format!(
            "Google account '{account_id}' is not authenticated"
        ))
    })?;
    Ok(GoogleTokenInspection {
        account_id: account_id.to_string(),
        stored_expires_at_epoch_seconds: token.expires_at_epoch_seconds,
        has_refresh_token: !token.refresh_token.is_empty(),
    })
}

pub fn list_google_calendars(
    account: &GoogleAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn GoogleTokenStore,
) -> Result<Vec<GoogleCalendarInfo>, ProviderError> {
    let token = google_access_token(account, http, token_store)?;
    let response = google_request(
        http,
        "GET",
        &format!("{GOOGLE_CALENDAR_BASE_URL}/users/me/calendarList?maxResults=250"),
        &token,
        None,
    )?;
    let value = parse_google_success_json(response)?;
    let calendars = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::Mapping("Google calendar list response is missing items".to_string())
        })?
        .iter()
        .map(|calendar| {
            let access_role = graph_string(calendar, "accessRole").unwrap_or_default();
            GoogleCalendarInfo {
                id: graph_string(calendar, "id").unwrap_or_default(),
                name: graph_string(calendar, "summaryOverride")
                    .or_else(|| graph_string(calendar, "summary"))
                    .unwrap_or_default(),
                can_edit: matches!(access_role.as_str(), "owner" | "writer"),
                is_default: graph_bool(calendar, "primary").unwrap_or(false),
            }
        })
        .filter(|calendar| !calendar.id.is_empty())
        .collect::<Vec<_>>();
    Ok(calendars)
}

fn fetch_calendar(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    calendar_id: &str,
) -> Result<MicrosoftCalendarRecord, ProviderError> {
    let response = graph_request(
        http,
        "GET",
        &format!(
            "{GRAPH_BASE_URL}/me/calendars/{}?$select=id,name,canEdit,isDefaultCalendar",
            percent_encode(calendar_id)
        ),
        token,
        None,
    )?;
    let value = parse_graph_success_json(response)?;
    Ok(MicrosoftCalendarRecord {
        id: graph_string(&value, "id").unwrap_or_else(|| calendar_id.to_string()),
        name: graph_string(&value, "name").unwrap_or_else(|| calendar_id.to_string()),
        can_edit: graph_bool(&value, "canEdit").unwrap_or(true),
        is_default: graph_bool(&value, "isDefaultCalendar").unwrap_or(false),
    })
}

fn fetch_calendar_view(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    account_id: &str,
    calendar: &MicrosoftCalendarRecord,
    start: CalendarDate,
    end: CalendarDate,
) -> Result<Vec<MicrosoftCachedEvent>, ProviderError> {
    let mut url = format!(
        "{GRAPH_BASE_URL}/me/calendars/{}/calendarView?startDateTime={}T00:00:00&endDateTime={}T00:00:00&$top=200",
        percent_encode(&calendar.id),
        start,
        end
    );
    let mut events = Vec::new();
    let mut series_master_ids = HashSet::new();
    loop {
        let response = graph_request(http, "GET", &url, token, None)?;
        let value = parse_graph_success_json(response)?;
        if let Some(values) = value.get("value").and_then(Value::as_array) {
            for event in values {
                if event.get("@removed").is_some() {
                    continue;
                }
                if let Some(series_master_id) = graph_string(event, "seriesMasterId") {
                    series_master_ids.insert(series_master_id);
                }
                events.push(MicrosoftCachedEvent::from_graph(
                    account_id,
                    calendar,
                    event.clone(),
                )?);
            }
        }
        if let Some(next_link) = graph_string(&value, "@odata.nextLink") {
            url = next_link;
        } else {
            break;
        }
    }
    for series_master_id in series_master_ids {
        if events
            .iter()
            .any(|event| event.graph_id == series_master_id)
        {
            continue;
        }
        let raw = fetch_event(http, token, &series_master_id)?;
        events.push(MicrosoftCachedEvent::from_graph(account_id, calendar, raw)?);
    }
    Ok(events)
}

fn fetch_event(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    graph_id: &str,
) -> Result<Value, ProviderError> {
    let response = graph_request(
        http,
        "GET",
        &format!("{GRAPH_BASE_URL}/me/events/{}", percent_encode(graph_id)),
        token,
        None,
    )?;
    parse_graph_success_json(response)
}

fn google_access_token(
    account: &GoogleAccountConfig,
    http: &dyn MicrosoftHttpClient,
    token_store: &dyn GoogleTokenStore,
) -> Result<String, ProviderError> {
    let token = token_store.load(&account.id)?.ok_or_else(|| {
        ProviderError::Auth(format!(
            "Google account '{}' is not authenticated",
            account.id
        ))
    })?;
    if token.expires_at_epoch_seconds > current_epoch_seconds().saturating_add(120) {
        return Ok(token.access_token);
    }
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("client_id", account.client_id.as_str()),
        ("refresh_token", token.refresh_token.as_str()),
    ];
    if let Some(client_secret) = &account.client_secret {
        fields.push(("client_secret", client_secret.as_str()));
    }
    let response = http.request(MicrosoftHttpRequest {
        method: "POST".to_string(),
        url: GOOGLE_TOKEN_URL.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )],
        body: Some(form_body(&fields)),
    })?;
    let refreshed = google_token_from_response(response, Some(token.refresh_token))?;
    token_store.save(&account.id, &refreshed)?;
    Ok(refreshed.access_token)
}

fn fetch_google_calendar(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    calendar_id: &str,
) -> Result<GoogleCalendarRecord, ProviderError> {
    let response = google_request(
        http,
        "GET",
        &format!(
            "{GOOGLE_CALENDAR_BASE_URL}/users/me/calendarList/{}",
            percent_encode(calendar_id)
        ),
        token,
        None,
    )?;
    let value = parse_google_success_json(response)?;
    let access_role = graph_string(&value, "accessRole").unwrap_or_default();
    Ok(GoogleCalendarRecord {
        id: graph_string(&value, "id").unwrap_or_else(|| calendar_id.to_string()),
        name: graph_string(&value, "summaryOverride")
            .or_else(|| graph_string(&value, "summary"))
            .unwrap_or_else(|| calendar_id.to_string()),
        can_edit: matches!(access_role.as_str(), "owner" | "writer"),
        is_default: graph_bool(&value, "primary").unwrap_or(false),
    })
}

fn fetch_google_events(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    account_id: &str,
    calendar: &GoogleCalendarRecord,
    start: CalendarDate,
    end: CalendarDate,
) -> Result<Vec<GoogleCachedEvent>, ProviderError> {
    let mut url = format!(
        "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events?singleEvents=true&showDeleted=false&maxResults=2500&orderBy=startTime&timeMin={}T00:00:00Z&timeMax={}T00:00:00Z",
        percent_encode(&calendar.id),
        start,
        end
    );
    let mut events = Vec::new();
    let mut series_master_ids = HashSet::new();
    loop {
        let response = google_request(http, "GET", &url, token, None)?;
        let value = parse_google_success_json(response)?;
        if let Some(values) = value.get("items").and_then(Value::as_array) {
            for event in values {
                if graph_string(event, "status").as_deref() == Some("cancelled") {
                    continue;
                }
                if let Some(series_master_id) = graph_string(event, "recurringEventId") {
                    series_master_ids.insert(series_master_id);
                }
                events.push(GoogleCachedEvent::from_google(
                    account_id,
                    calendar,
                    event.clone(),
                )?);
            }
        }
        if let Some(next_page_token) = graph_string(&value, "nextPageToken") {
            url = format!(
                "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events?singleEvents=true&showDeleted=false&maxResults=2500&orderBy=startTime&timeMin={}T00:00:00Z&timeMax={}T00:00:00Z&pageToken={}",
                percent_encode(&calendar.id),
                start,
                end,
                percent_encode(&next_page_token)
            );
        } else {
            break;
        }
    }
    for series_master_id in series_master_ids {
        if events
            .iter()
            .any(|event| event.google_id == series_master_id)
        {
            continue;
        }
        let raw = fetch_google_event(http, token, &calendar.id, &series_master_id)?;
        events.push(GoogleCachedEvent::from_google(account_id, calendar, raw)?);
    }
    Ok(events)
}

fn fetch_google_event(
    http: &dyn MicrosoftHttpClient,
    token: &str,
    calendar_id: &str,
    google_id: &str,
) -> Result<Value, ProviderError> {
    let response = google_request(
        http,
        "GET",
        &format!(
            "{GOOGLE_CALENDAR_BASE_URL}/calendars/{}/events/{}",
            percent_encode(calendar_id),
            percent_encode(google_id)
        ),
        token,
        None,
    )?;
    parse_google_success_json(response)
}

fn graph_request(
    http: &dyn MicrosoftHttpClient,
    method: &str,
    url: &str,
    token: &str,
    body: Option<String>,
) -> Result<MicrosoftHttpResponse, ProviderError> {
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Accept".to_string(), "application/json".to_string()),
        ("Prefer".to_string(), "outlook.timezone=\"UTC\"".to_string()),
    ];
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    http.request(MicrosoftHttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers,
        body,
    })
}

fn parse_graph_success_json(response: MicrosoftHttpResponse) -> Result<Value, ProviderError> {
    if (200..300).contains(&response.status) {
        return serde_json::from_str(&response.body)
            .map_err(|err| ProviderError::Mapping(err.to_string()));
    }
    Err(ProviderError::Graph(graph_error_message(response)))
}

fn parse_graph_empty_success(response: MicrosoftHttpResponse) -> Result<(), ProviderError> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(ProviderError::Graph(graph_error_message(response)))
    }
}

fn google_request(
    http: &dyn MicrosoftHttpClient,
    method: &str,
    url: &str,
    token: &str,
    body: Option<String>,
) -> Result<MicrosoftHttpResponse, ProviderError> {
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    http.request(MicrosoftHttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers,
        body,
    })
}

fn parse_google_success_json(response: MicrosoftHttpResponse) -> Result<Value, ProviderError> {
    if (200..300).contains(&response.status) {
        return serde_json::from_str(&response.body)
            .map_err(|err| ProviderError::Mapping(format!("Google: {err}")));
    }
    Err(ProviderError::Api(google_error_message(response)))
}

fn parse_google_empty_success(response: MicrosoftHttpResponse) -> Result<(), ProviderError> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(ProviderError::Api(google_error_message(response)))
    }
}

fn graph_error_message(response: MicrosoftHttpResponse) -> String {
    let www_authenticate = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
        .map(|(_, value)| value.as_str());
    if let Ok(value) = serde_json::from_str::<Value>(&response.body)
        && let Some(error) = value.get("error")
    {
        let code = graph_string(error, "code").unwrap_or_else(|| response.status.to_string());
        let message = graph_string(error, "message").unwrap_or_else(|| response.body.clone());
        return match www_authenticate {
            Some(header) if !header.is_empty() => format!("{code}: {message} ({header})"),
            _ => format!("{code}: {message}"),
        };
    }
    match (response.body.trim(), www_authenticate) {
        ("", Some(header)) if !header.is_empty() => format!("HTTP {}: {header}", response.status),
        (body, Some(header)) if !header.is_empty() => {
            format!("HTTP {}: {body} ({header})", response.status)
        }
        (body, _) => format!("HTTP {}: {body}", response.status),
    }
}

fn google_error_message(response: MicrosoftHttpResponse) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(&response.body)
        && let Some(error) = value.get("error")
    {
        let code = graph_string(error, "status")
            .or_else(|| graph_string(error, "code"))
            .unwrap_or_else(|| response.status.to_string());
        let message = graph_string(error, "message").unwrap_or_else(|| response.body.clone());
        return format!("Google Calendar {code}: {message}");
    }
    let body = response.body.trim();
    if body.is_empty() {
        format!("Google Calendar HTTP {}", response.status)
    } else {
        format!("Google Calendar HTTP {}: {body}", response.status)
    }
}

fn google_oauth_error_message(response: MicrosoftHttpResponse) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(&response.body)
        && let Some(error) = graph_string(&value, "error")
    {
        let description = graph_string(&value, "error_description");
        return match description {
            Some(description) if !description.is_empty() => {
                format!("Google OAuth {}: {error}: {description}", response.status)
            }
            _ => format!("Google OAuth {}: {error}", response.status),
        };
    }
    let body = response.body.trim();
    if body.is_empty() {
        format!("Google OAuth HTTP {}", response.status)
    } else {
        format!("Google OAuth HTTP {}: {body}", response.status)
    }
}

fn parse_oauth_json(response: MicrosoftHttpResponse) -> Result<Value, ProviderError> {
    if response.status != 200 {
        return Err(ProviderError::Auth(graph_error_message(response)));
    }
    serde_json::from_str::<Value>(&response.body)
        .map_err(|err| ProviderError::Auth(err.to_string()))
}

fn token_from_response(response: MicrosoftHttpResponse) -> Result<MicrosoftToken, ProviderError> {
    let value = parse_oauth_json(response)?;
    let access_token = required_json_string(&value, "access_token")?;
    let refresh_token = graph_string(&value, "refresh_token").unwrap_or_default();
    let expires_in = graph_i64(&value, "expires_in").unwrap_or(3600).max(1) as u64;
    Ok(MicrosoftToken {
        access_token,
        refresh_token,
        expires_at_epoch_seconds: current_epoch_seconds().saturating_add(expires_in),
    })
}

fn google_token_from_response(
    response: MicrosoftHttpResponse,
    existing_refresh_token: Option<String>,
) -> Result<GoogleToken, ProviderError> {
    if response.status != 200 {
        return Err(ProviderError::Auth(google_oauth_error_message(response)));
    }
    let value = serde_json::from_str::<Value>(&response.body)
        .map_err(|err| ProviderError::Auth(format!("Google token response is invalid: {err}")))?;
    let access_token = required_json_string(&value, "access_token")?;
    let refresh_token = graph_string(&value, "refresh_token")
        .or(existing_refresh_token)
        .unwrap_or_default();
    let expires_in = graph_i64(&value, "expires_in").unwrap_or(3600).max(1) as u64;
    Ok(GoogleToken {
        access_token,
        refresh_token,
        expires_at_epoch_seconds: current_epoch_seconds().saturating_add(expires_in),
    })
}

fn access_token_claims(access_token: &str) -> Result<Option<Value>, ProviderError> {
    let Some(payload) = access_token.split('.').nth(1) else {
        return Ok(None);
    };
    let bytes = base64_url_decode_no_pad(payload).ok_or_else(|| {
        ProviderError::Auth("stored Microsoft access token has invalid JWT encoding".to_string())
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|err| {
            ProviderError::Auth(format!("stored Microsoft access token is invalid: {err}"))
        })
        .map(Some)
}

fn required_json_string(value: &Value, key: &str) -> Result<String, ProviderError> {
    graph_string(value, key)
        .ok_or_else(|| ProviderError::Auth(format!("OAuth response is missing {key}")))
}

pub fn graph_event_payload(
    draft: &CreateEventDraft,
    is_update: bool,
) -> Result<Value, ProviderError> {
    if draft.reminders.len() > 1 {
        return Err(ProviderError::Validation(
            "Microsoft events support only one reminder".to_string(),
        ));
    }
    let mut payload = Map::new();
    payload.insert("subject".to_string(), Value::String(draft.title.clone()));
    if let Some(notes) = &draft.notes {
        payload.insert(
            "body".to_string(),
            json!({
                "contentType": "text",
                "content": notes
            }),
        );
    }
    if let Some(location) = &draft.location {
        payload.insert("location".to_string(), json!({ "displayName": location }));
    }
    if let Some(reminder) = draft.reminders.first() {
        payload.insert("isReminderOn".to_string(), Value::Bool(true));
        payload.insert(
            "reminderMinutesBeforeStart".to_string(),
            Value::Number(serde_json::Number::from(reminder.minutes_before)),
        );
    } else if !is_update {
        payload.insert("isReminderOn".to_string(), Value::Bool(false));
    }
    match draft.timing {
        CreateEventTiming::AllDay { date } => {
            payload.insert("isAllDay".to_string(), Value::Bool(true));
            payload.insert(
                "start".to_string(),
                graph_datetime_payload(date, Time::MIDNIGHT),
            );
            payload.insert(
                "end".to_string(),
                graph_datetime_payload(date.add_days(1), Time::MIDNIGHT),
            );
        }
        CreateEventTiming::Timed { start, end } => {
            payload.insert("isAllDay".to_string(), Value::Bool(false));
            payload.insert(
                "start".to_string(),
                graph_datetime_payload(start.date, start.time),
            );
            payload.insert(
                "end".to_string(),
                graph_datetime_payload(end.date, end.time),
            );
        }
    }
    if let Some(recurrence) = &draft.recurrence {
        payload.insert(
            "recurrence".to_string(),
            graph_recurrence_payload(recurrence, draft)?,
        );
    }
    Ok(Value::Object(payload))
}

pub fn google_event_payload(
    draft: &CreateEventDraft,
    is_update: bool,
) -> Result<Value, ProviderError> {
    if draft.reminders.len() > 5 {
        return Err(ProviderError::Validation(
            "Google Calendar events support at most five reminders".to_string(),
        ));
    }
    let mut payload = Map::new();
    payload.insert("summary".to_string(), Value::String(draft.title.clone()));
    if let Some(notes) = &draft.notes {
        payload.insert("description".to_string(), Value::String(notes.clone()));
    } else if is_update {
        payload.insert("description".to_string(), Value::String(String::new()));
    }
    if let Some(location) = &draft.location {
        payload.insert("location".to_string(), Value::String(location.clone()));
    } else if is_update {
        payload.insert("location".to_string(), Value::String(String::new()));
    }
    if draft.reminders.is_empty() {
        payload.insert("reminders".to_string(), json!({ "useDefault": false }));
    } else {
        payload.insert(
            "reminders".to_string(),
            json!({
                "useDefault": false,
                "overrides": draft.reminders.iter().map(|reminder| {
                    json!({
                        "method": "popup",
                        "minutes": reminder.minutes_before
                    })
                }).collect::<Vec<_>>()
            }),
        );
    }
    match draft.timing {
        CreateEventTiming::AllDay { date } => {
            payload.insert("start".to_string(), json!({ "date": date.to_string() }));
            payload.insert(
                "end".to_string(),
                json!({ "date": date.add_days(1).to_string() }),
            );
        }
        CreateEventTiming::Timed { start, end } => {
            payload.insert("start".to_string(), google_datetime_payload(start));
            payload.insert("end".to_string(), google_datetime_payload(end));
        }
    }
    if let Some(recurrence) = &draft.recurrence {
        payload.insert(
            "recurrence".to_string(),
            Value::Array(vec![Value::String(google_rrule_payload(recurrence, draft))]),
        );
    }
    Ok(Value::Object(payload))
}

fn graph_datetime_payload(date: CalendarDate, time: Time) -> Value {
    json!({
        "dateTime": format!("{}T{:02}:{:02}:00", date, time.hour(), time.minute()),
        "timeZone": "UTC"
    })
}

fn google_datetime_payload(value: EventDateTime) -> Value {
    json!({
        "dateTime": format!(
            "{}T{:02}:{:02}:00Z",
            value.date,
            value.time.hour(),
            value.time.minute()
        )
    })
}

fn graph_recurrence_payload(
    rule: &RecurrenceRule,
    draft: &CreateEventDraft,
) -> Result<Value, ProviderError> {
    let start_date = match draft.timing {
        CreateEventTiming::AllDay { date } => date,
        CreateEventTiming::Timed { start, .. } => start.date,
    };
    let mut pattern = Map::new();
    pattern.insert(
        "interval".to_string(),
        Value::Number(serde_json::Number::from(rule.interval().max(1))),
    );
    match rule.frequency {
        RecurrenceFrequency::Daily => {
            pattern.insert("type".to_string(), Value::String("daily".to_string()));
        }
        RecurrenceFrequency::Weekly => {
            pattern.insert("type".to_string(), Value::String("weekly".to_string()));
            pattern.insert(
                "daysOfWeek".to_string(),
                Value::Array(
                    rule.weekdays
                        .iter()
                        .copied()
                        .map(graph_weekday)
                        .map(Value::String)
                        .collect(),
                ),
            );
            pattern.insert(
                "firstDayOfWeek".to_string(),
                Value::String("sunday".to_string()),
            );
        }
        RecurrenceFrequency::Monthly => match rule.monthly {
            Some(RecurrenceMonthlyRule::DayOfMonth(day)) => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("absoluteMonthly".to_string()),
                );
                pattern.insert(
                    "dayOfMonth".to_string(),
                    Value::Number(serde_json::Number::from(day)),
                );
            }
            Some(RecurrenceMonthlyRule::WeekdayOrdinal { ordinal, weekday }) => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("relativeMonthly".to_string()),
                );
                pattern.insert("index".to_string(), Value::String(graph_ordinal(ordinal)));
                pattern.insert(
                    "daysOfWeek".to_string(),
                    Value::Array(vec![Value::String(graph_weekday(weekday))]),
                );
            }
            None => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("absoluteMonthly".to_string()),
                );
                pattern.insert(
                    "dayOfMonth".to_string(),
                    Value::Number(serde_json::Number::from(start_date.day())),
                );
            }
        },
        RecurrenceFrequency::Yearly => match rule.yearly {
            Some(RecurrenceYearlyRule::Date { month, day }) => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("absoluteYearly".to_string()),
                );
                pattern.insert(
                    "month".to_string(),
                    Value::Number(serde_json::Number::from(u8::from(month))),
                );
                pattern.insert(
                    "dayOfMonth".to_string(),
                    Value::Number(serde_json::Number::from(day)),
                );
            }
            Some(RecurrenceYearlyRule::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            }) => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("relativeYearly".to_string()),
                );
                pattern.insert(
                    "month".to_string(),
                    Value::Number(serde_json::Number::from(u8::from(month))),
                );
                pattern.insert("index".to_string(), Value::String(graph_ordinal(ordinal)));
                pattern.insert(
                    "daysOfWeek".to_string(),
                    Value::Array(vec![Value::String(graph_weekday(weekday))]),
                );
            }
            None => {
                pattern.insert(
                    "type".to_string(),
                    Value::String("absoluteYearly".to_string()),
                );
                pattern.insert(
                    "month".to_string(),
                    Value::Number(serde_json::Number::from(u8::from(start_date.month()))),
                );
                pattern.insert(
                    "dayOfMonth".to_string(),
                    Value::Number(serde_json::Number::from(start_date.day())),
                );
            }
        },
    }
    let range = match rule.end {
        RecurrenceEnd::Never => json!({
            "type": "noEnd",
            "startDate": start_date.to_string(),
            "recurrenceTimeZone": "UTC"
        }),
        RecurrenceEnd::Until(date) => json!({
            "type": "endDate",
            "startDate": start_date.to_string(),
            "endDate": date.to_string(),
            "recurrenceTimeZone": "UTC"
        }),
        RecurrenceEnd::Count(count) => json!({
            "type": "numbered",
            "startDate": start_date.to_string(),
            "numberOfOccurrences": count,
            "recurrenceTimeZone": "UTC"
        }),
    };
    Ok(json!({
        "pattern": Value::Object(pattern),
        "range": range
    }))
}

fn google_rrule_payload(rule: &RecurrenceRule, draft: &CreateEventDraft) -> String {
    let start_date = match draft.timing {
        CreateEventTiming::AllDay { date } => date,
        CreateEventTiming::Timed { start, .. } => start.date,
    };
    let mut parts = vec![format!(
        "FREQ={}",
        match rule.frequency {
            RecurrenceFrequency::Daily => "DAILY",
            RecurrenceFrequency::Weekly => "WEEKLY",
            RecurrenceFrequency::Monthly => "MONTHLY",
            RecurrenceFrequency::Yearly => "YEARLY",
        }
    )];
    if rule.interval() > 1 {
        parts.push(format!("INTERVAL={}", rule.interval()));
    }
    match rule.frequency {
        RecurrenceFrequency::Weekly => {
            if !rule.weekdays.is_empty() {
                parts.push(format!(
                    "BYDAY={}",
                    rule.weekdays
                        .iter()
                        .copied()
                        .map(google_weekday)
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
        }
        RecurrenceFrequency::Monthly => match rule.monthly {
            Some(RecurrenceMonthlyRule::DayOfMonth(day)) => {
                parts.push(format!("BYMONTHDAY={day}"));
            }
            Some(RecurrenceMonthlyRule::WeekdayOrdinal { ordinal, weekday }) => {
                parts.push(format!(
                    "BYDAY={}{}",
                    google_ordinal_prefix(ordinal),
                    google_weekday(weekday)
                ));
            }
            None => parts.push(format!("BYMONTHDAY={}", start_date.day())),
        },
        RecurrenceFrequency::Yearly => match rule.yearly {
            Some(RecurrenceYearlyRule::Date { month, day }) => {
                parts.push(format!("BYMONTH={}", u8::from(month)));
                parts.push(format!("BYMONTHDAY={day}"));
            }
            Some(RecurrenceYearlyRule::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            }) => {
                parts.push(format!("BYMONTH={}", u8::from(month)));
                parts.push(format!(
                    "BYDAY={}{}",
                    google_ordinal_prefix(ordinal),
                    google_weekday(weekday)
                ));
            }
            None => {
                parts.push(format!("BYMONTH={}", u8::from(start_date.month())));
                parts.push(format!("BYMONTHDAY={}", start_date.day()));
            }
        },
        RecurrenceFrequency::Daily => {}
    }
    match rule.end {
        RecurrenceEnd::Never => {}
        RecurrenceEnd::Until(date) => {
            parts.push(format!(
                "UNTIL={}T000000Z",
                date.to_string().replace('-', "")
            ));
        }
        RecurrenceEnd::Count(count) => {
            parts.push(format!("COUNT={count}"));
        }
    }
    format!("RRULE:{}", parts.join(";"))
}

fn graph_recurrence_to_cache(value: &Value) -> Result<MicrosoftCachedRecurrence, ProviderError> {
    let pattern = value
        .get("pattern")
        .ok_or_else(|| ProviderError::Mapping("recurrence is missing pattern".to_string()))?;
    let range = value
        .get("range")
        .ok_or_else(|| ProviderError::Mapping("recurrence is missing range".to_string()))?;
    let interval = graph_i64(pattern, "interval")
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(1)
        .max(1);
    let end = match graph_string(range, "type").as_deref() {
        Some("noEnd") => RecurrenceEndRecord::Never,
        Some("endDate") => RecurrenceEndRecord::Until {
            date: graph_string(range, "endDate")
                .and_then(|value| parse_date(&value))
                .ok_or_else(|| {
                    ProviderError::Mapping("recurrence endDate is invalid".to_string())
                })?,
        },
        Some("numbered") => RecurrenceEndRecord::Count {
            count: graph_i64(range, "numberOfOccurrences")
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(1),
        },
        _ => RecurrenceEndRecord::Never,
    };
    let weekdays = pattern
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(parse_graph_weekday)
        .map(WeekdayRecord::from_weekday)
        .collect::<Vec<_>>();
    match graph_string(pattern, "type").as_deref() {
        Some("daily") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Daily,
            interval,
            end,
            weekdays: Vec::new(),
            monthly: None,
            yearly: None,
        }),
        Some("weekly") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Weekly,
            interval,
            end,
            weekdays,
            monthly: None,
            yearly: None,
        }),
        Some("absoluteMonthly") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Monthly,
            interval,
            end,
            weekdays: Vec::new(),
            monthly: Some(MonthlyRuleRecord::DayOfMonth {
                day: graph_i64(pattern, "dayOfMonth")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
            }),
            yearly: None,
        }),
        Some("relativeMonthly") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Monthly,
            interval,
            end,
            weekdays: Vec::new(),
            monthly: Some(MonthlyRuleRecord::WeekdayOrdinal {
                ordinal: parse_graph_ordinal(&graph_string(pattern, "index").unwrap_or_default()),
                weekday: weekdays.first().copied().unwrap_or(WeekdayRecord::Monday),
            }),
            yearly: None,
        }),
        Some("absoluteYearly") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Yearly,
            interval,
            end,
            weekdays: Vec::new(),
            monthly: None,
            yearly: Some(YearlyRuleRecord::Date {
                month: graph_i64(pattern, "month")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
                day: graph_i64(pattern, "dayOfMonth")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
            }),
        }),
        Some("relativeYearly") => Ok(MicrosoftCachedRecurrence {
            frequency: RecurrenceFrequencyRecord::Yearly,
            interval,
            end,
            weekdays: Vec::new(),
            monthly: None,
            yearly: Some(YearlyRuleRecord::WeekdayOrdinal {
                month: graph_i64(pattern, "month")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
                ordinal: parse_graph_ordinal(&graph_string(pattern, "index").unwrap_or_default()),
                weekday: weekdays.first().copied().unwrap_or(WeekdayRecord::Monday),
            }),
        }),
        other => Err(ProviderError::Mapping(format!(
            "unsupported Microsoft recurrence type '{}'",
            other.unwrap_or("<missing>")
        ))),
    }
}

fn google_rrule_to_cache(rule: &str) -> Option<MicrosoftCachedRecurrence> {
    let body = rule.strip_prefix("RRULE:")?;
    let mut parts = HashMap::new();
    for part in body.split(';') {
        let (key, value) = part.split_once('=')?;
        parts.insert(key, value);
    }
    let frequency = match *parts.get("FREQ")? {
        "DAILY" => RecurrenceFrequencyRecord::Daily,
        "WEEKLY" => RecurrenceFrequencyRecord::Weekly,
        "MONTHLY" => RecurrenceFrequencyRecord::Monthly,
        "YEARLY" => RecurrenceFrequencyRecord::Yearly,
        _ => return None,
    };
    let interval = parts
        .get("INTERVAL")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(1)
        .max(1);
    let end = if let Some(count) = parts
        .get("COUNT")
        .and_then(|value| value.parse::<u32>().ok())
    {
        RecurrenceEndRecord::Count { count }
    } else if let Some(until) = parts.get("UNTIL") {
        let date = until
            .get(0..8)
            .and_then(|value| {
                let year = value.get(0..4)?;
                let month = value.get(4..6)?;
                let day = value.get(6..8)?;
                parse_date(&format!("{year}-{month}-{day}"))
            })
            .unwrap_or_else(|| CalendarDate::from_ymd(9999, Month::December, 31).expect("date"));
        RecurrenceEndRecord::Until { date }
    } else {
        RecurrenceEndRecord::Never
    };
    let weekdays = parts
        .get("BYDAY")
        .map(|value| {
            value
                .split(',')
                .filter_map(|day| {
                    parse_google_weekday(day.trim_start_matches([
                        '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
                    ]))
                })
                .map(WeekdayRecord::from_weekday)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let monthly = if matches!(frequency, RecurrenceFrequencyRecord::Monthly) {
        if let Some(day) = parts
            .get("BYMONTHDAY")
            .and_then(|value| value.parse::<u8>().ok())
        {
            Some(MonthlyRuleRecord::DayOfMonth { day })
        } else if let Some(day) = parts.get("BYDAY").and_then(|value| value.split(',').next()) {
            google_ordinal_weekday(day)
                .map(|(ordinal, weekday)| MonthlyRuleRecord::WeekdayOrdinal { ordinal, weekday })
        } else {
            None
        }
    } else {
        None
    };
    let yearly = if matches!(frequency, RecurrenceFrequencyRecord::Yearly) {
        let month = parts
            .get("BYMONTH")
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(1);
        if let Some(day) = parts
            .get("BYMONTHDAY")
            .and_then(|value| value.parse::<u8>().ok())
        {
            Some(YearlyRuleRecord::Date { month, day })
        } else if let Some(day) = parts.get("BYDAY").and_then(|value| value.split(',').next()) {
            google_ordinal_weekday(day).map(|(ordinal, weekday)| YearlyRuleRecord::WeekdayOrdinal {
                month,
                ordinal,
                weekday,
            })
        } else {
            None
        }
    } else {
        None
    };
    Some(MicrosoftCachedRecurrence {
        frequency,
        interval,
        end,
        weekdays: if matches!(frequency, RecurrenceFrequencyRecord::Weekly) {
            weekdays
        } else {
            Vec::new()
        },
        monthly,
        yearly,
    })
}

fn google_ordinal_weekday(value: &str) -> Option<(OrdinalRecord, WeekdayRecord)> {
    let weekday = value.get(value.len().saturating_sub(2)..)?;
    let ordinal = value.get(..value.len().saturating_sub(2)).unwrap_or("1");
    let ordinal = match ordinal {
        "1" | "" => OrdinalRecord::First,
        "2" => OrdinalRecord::Second,
        "3" => OrdinalRecord::Third,
        "4" => OrdinalRecord::Fourth,
        "-1" => OrdinalRecord::Last,
        _ => OrdinalRecord::First,
    };
    Some((
        ordinal,
        WeekdayRecord::from_weekday(parse_google_weekday(weekday)?),
    ))
}

fn graph_weekday(weekday: Weekday) -> String {
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

fn parse_graph_weekday(value: &str) -> Option<Weekday> {
    match value {
        "sunday" => Some(Weekday::Sunday),
        "monday" => Some(Weekday::Monday),
        "tuesday" => Some(Weekday::Tuesday),
        "wednesday" => Some(Weekday::Wednesday),
        "thursday" => Some(Weekday::Thursday),
        "friday" => Some(Weekday::Friday),
        "saturday" => Some(Weekday::Saturday),
        _ => None,
    }
}

fn google_weekday(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Sunday => "SU",
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
    }
}

fn parse_google_weekday(value: &str) -> Option<Weekday> {
    match value {
        "SU" => Some(Weekday::Sunday),
        "MO" => Some(Weekday::Monday),
        "TU" => Some(Weekday::Tuesday),
        "WE" => Some(Weekday::Wednesday),
        "TH" => Some(Weekday::Thursday),
        "FR" => Some(Weekday::Friday),
        "SA" => Some(Weekday::Saturday),
        _ => None,
    }
}

fn graph_ordinal(ordinal: RecurrenceOrdinal) -> String {
    match OrdinalRecord::from_rule(ordinal) {
        OrdinalRecord::First => "first",
        OrdinalRecord::Second => "second",
        OrdinalRecord::Third => "third",
        OrdinalRecord::Fourth => "fourth",
        OrdinalRecord::Last => "last",
    }
    .to_string()
}

fn google_ordinal_prefix(ordinal: RecurrenceOrdinal) -> &'static str {
    match ordinal {
        RecurrenceOrdinal::Number(1) => "1",
        RecurrenceOrdinal::Number(2) => "2",
        RecurrenceOrdinal::Number(3) => "3",
        RecurrenceOrdinal::Number(_) => "4",
        RecurrenceOrdinal::Last => "-1",
    }
}

fn parse_graph_ordinal(value: &str) -> OrdinalRecord {
    match value {
        "first" => OrdinalRecord::First,
        "second" => OrdinalRecord::Second,
        "third" => OrdinalRecord::Third,
        "fourth" => OrdinalRecord::Fourth,
        "last" => OrdinalRecord::Last,
        _ => OrdinalRecord::First,
    }
}

fn graph_datetime(value: &Value, key: &str) -> Result<EventDateTime, ProviderError> {
    let date_time = value
        .get(key)
        .and_then(|date_time| graph_string(date_time, "dateTime"))
        .ok_or_else(|| ProviderError::Mapping(format!("Graph event is missing {key}.dateTime")))?;
    parse_event_datetime(&date_time)
        .ok_or_else(|| ProviderError::Mapping(format!("invalid Graph dateTime '{date_time}'")))
}

fn google_timing(
    value: &Value,
    start_key: &str,
    end_key: &str,
) -> Result<GoogleCachedTiming, ProviderError> {
    let start = value
        .get(start_key)
        .ok_or_else(|| ProviderError::Mapping(format!("Google event is missing {start_key}")))?;
    let end = value
        .get(end_key)
        .ok_or_else(|| ProviderError::Mapping(format!("Google event is missing {end_key}")))?;
    match (graph_string(start, "date"), graph_string(end, "date")) {
        (Some(date), _) => Ok(GoogleCachedTiming::AllDay {
            date: parse_date(&date).ok_or_else(|| {
                ProviderError::Mapping(format!("invalid Google all-day date '{date}'"))
            })?,
        }),
        _ => {
            let start = graph_string(start, "dateTime").ok_or_else(|| {
                ProviderError::Mapping(format!("Google event is missing {start_key}.dateTime"))
            })?;
            let end = graph_string(end, "dateTime").ok_or_else(|| {
                ProviderError::Mapping(format!("Google event is missing {end_key}.dateTime"))
            })?;
            Ok(GoogleCachedTiming::Timed {
                start: parse_event_datetime(&start).ok_or_else(|| {
                    ProviderError::Mapping(format!("invalid Google dateTime '{start}'"))
                })?,
                end: parse_event_datetime(&end).ok_or_else(|| {
                    ProviderError::Mapping(format!("invalid Google dateTime '{end}'"))
                })?,
            })
        }
    }
}

fn google_single_time(value: &Value) -> Result<GoogleCachedTiming, ProviderError> {
    if let Some(date) = graph_string(value, "date") {
        return Ok(GoogleCachedTiming::AllDay {
            date: parse_date(&date).ok_or_else(|| {
                ProviderError::Mapping(format!("invalid Google originalStartTime date '{date}'"))
            })?,
        });
    }
    let start = graph_string(value, "dateTime").ok_or_else(|| {
        ProviderError::Mapping("Google originalStartTime is missing dateTime".to_string())
    })?;
    let start = parse_event_datetime(&start).ok_or_else(|| {
        ProviderError::Mapping(format!(
            "invalid Google originalStartTime dateTime '{start}'"
        ))
    })?;
    Ok(GoogleCachedTiming::Timed { start, end: start })
}

fn parse_event_datetime(value: &str) -> Option<EventDateTime> {
    let (date, rest) = value.split_once('T')?;
    let date = parse_date(date)?;
    let time_part = rest.split(['.', 'Z', '+', '-']).next()?;
    let time = parse_time(time_part)?;
    Some(EventDateTime::new(date, time))
}

fn parse_date(value: &str) -> Option<CalendarDate> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = Month::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    CalendarDate::from_ymd(year, month, day).ok()
}

fn parse_time(value: &str) -> Option<Time> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    let second = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Time::from_hms(hour, minute, second).ok()
}

fn graph_string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(ToOwned::to_owned)
}

fn graph_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key)?.as_bool()
}

fn graph_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64()
}

fn microsoft_event_app_id(account_id: &str, calendar_id: &str, graph_id: &str) -> String {
    format!("microsoft:{account_id}:{calendar_id}:{graph_id}")
}

fn google_event_app_id(account_id: &str, calendar_id: &str, google_id: &str) -> String {
    format!("google:{account_id}:{calendar_id}:{google_id}")
}

fn short_calendar_label(calendar_id: &str) -> String {
    const MAX: usize = 18;
    let label = calendar_id.chars().take(MAX).collect::<String>();
    if calendar_id.chars().count() > MAX {
        format!("{label}...")
    } else {
        label
    }
}

fn anchor_label(anchor: OccurrenceAnchor) -> String {
    match anchor {
        OccurrenceAnchor::AllDay { date } => date.to_string(),
        OccurrenceAnchor::Timed { start } => {
            format!(
                "{}T{:02}:{:02}",
                start.date,
                start.time.hour(),
                start.time.minute()
            )
        }
    }
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn form_body(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> String {
    let mut output = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            output.push(hex);
            index += 3;
            continue;
        }
        output.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn write_oauth_callback_response(
    stream: &mut impl Write,
    provider_name: &str,
    success: bool,
    message: &str,
) -> io::Result<()> {
    let status = if success { "200 OK" } else { "400 Bad Request" };
    let heading = if success {
        format!("rcal {provider_name} login complete")
    } else {
        format!("rcal {provider_name} login failed")
    };
    let body = format!("{heading}\n\n{message}\n");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn pkce_verifier() -> String {
    let mut bytes = [0_u8; 32];
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    } else {
        bytes[..8].copy_from_slice(&current_epoch_seconds().to_le_bytes());
    }
    base64_url_no_pad(&bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64_url_no_pad(&digest)
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let chunk = &bytes[index..index + 3];
        output.push(TABLE[(chunk[0] >> 2) as usize] as char);
        output.push(TABLE[(((chunk[0] & 0b11) << 4) | (chunk[1] >> 4)) as usize] as char);
        output.push(TABLE[(((chunk[1] & 0b1111) << 2) | (chunk[2] >> 6)) as usize] as char);
        output.push(TABLE[(chunk[2] & 0b111111) as usize] as char);
        index += 3;
    }
    match bytes.len() - index {
        1 => {
            let byte = bytes[index];
            output.push(TABLE[(byte >> 2) as usize] as char);
            output.push(TABLE[((byte & 0b11) << 4) as usize] as char);
        }
        2 => {
            let first = bytes[index];
            let second = bytes[index + 1];
            output.push(TABLE[(first >> 2) as usize] as char);
            output.push(TABLE[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
            output.push(TABLE[((second & 0b1111) << 2) as usize] as char);
        }
        _ => {}
    }
    output
}

fn base64_url_decode_no_pad(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

fn open_browser(url: &str) -> Result<(), ProviderError> {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).status();
    #[cfg(target_os = "linux")]
    let result = Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = Command::new("cmd").args(["/C", "start", url]).status();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: Result<std::process::ExitStatus, io::Error> =
        Err(io::Error::new(io::ErrorKind::Other, "unsupported platform"));

    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(ProviderError::Auth(format!(
            "failed to open browser: exited with {status}"
        ))),
        Err(err) => Err(ProviderError::Auth(format!(
            "failed to open browser: {err}"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    Config(String),
    Auth(String),
    Keyring(String),
    Http(String),
    Graph(String),
    Api(String),
    Mapping(String),
    Validation(String),
    NotFound(String),
    CacheRead { path: PathBuf, reason: String },
    CacheParse { path: PathBuf, reason: String },
    CacheWrite { path: PathBuf, reason: String },
    Agenda(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(reason) => write!(f, "provider config error: {reason}"),
            Self::Auth(reason) => write!(f, "provider auth error: {reason}"),
            Self::Keyring(reason) => write!(f, "provider token keyring error: {reason}"),
            Self::Http(reason) => write!(f, "provider HTTP error: {reason}"),
            Self::Graph(reason) => write!(f, "Microsoft Graph error: {reason}"),
            Self::Api(reason) => write!(f, "provider API error: {reason}"),
            Self::Mapping(reason) => write!(f, "provider event mapping error: {reason}"),
            Self::Validation(reason) => write!(f, "{reason}"),
            Self::NotFound(id) => write!(f, "provider event '{id}' was not found"),
            Self::CacheRead { path, reason } => {
                write!(
                    f,
                    "failed to read provider cache {}: {reason}",
                    path.display()
                )
            }
            Self::CacheParse { path, reason } => {
                write!(
                    f,
                    "failed to parse provider cache {}: {reason}",
                    path.display()
                )
            }
            Self::CacheWrite { path, reason } => {
                write!(
                    f,
                    "failed to write provider cache {}: {reason}",
                    path.display()
                )
            }
            Self::Agenda(reason) => write!(f, "{reason}"),
        }
    }
}

impl Error for ProviderError {}

impl From<AgendaError> for ProviderError {
    fn from(err: AgendaError) -> Self {
        Self::Agenda(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agenda::{CreateEventTiming, RecurrenceEnd, RecurrenceFrequency};
    use std::{
        cell::RefCell,
        collections::{HashMap, VecDeque},
        sync::atomic::{AtomicUsize, Ordering},
    };

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid date")
    }

    fn at(date: CalendarDate, hour: u8, minute: u8) -> EventDateTime {
        EventDateTime::new(date, Time::from_hms(hour, minute, 0).expect("valid time"))
    }

    fn temp_path(name: &str) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir()
            .join(format!(
                "rcal-provider-test-{}-{counter}",
                std::process::id()
            ))
            .join(name)
    }

    fn account() -> MicrosoftAccountConfig {
        MicrosoftAccountConfig {
            id: "work".to_string(),
            client_id: "client-id".to_string(),
            tenant: "organizations".to_string(),
            redirect_port: 8765,
            calendars: vec!["cal".to_string()],
        }
    }

    fn provider_config(cache_file: PathBuf) -> MicrosoftProviderConfig {
        MicrosoftProviderConfig {
            enabled: true,
            default_account: Some("work".to_string()),
            default_calendar: Some("cal".to_string()),
            sync_past_days: 30,
            sync_future_days: 365,
            cache_file,
            accounts: vec![account()],
        }
    }

    fn google_account() -> GoogleAccountConfig {
        GoogleAccountConfig {
            id: "personal".to_string(),
            client_id: "google-client".to_string(),
            client_secret: Some("google-secret".to_string()),
            redirect_port: 8766,
            calendars: vec!["primary".to_string()],
        }
    }

    fn google_provider_config(cache_file: PathBuf) -> GoogleProviderConfig {
        GoogleProviderConfig {
            enabled: true,
            default_account: Some("personal".to_string()),
            default_calendar: Some("primary".to_string()),
            sync_past_days: 30,
            sync_future_days: 365,
            cache_file,
            accounts: vec![google_account()],
        }
    }

    fn google_calendar_record(id: &str, name: &str, can_edit: bool) -> GoogleCalendarRecord {
        GoogleCalendarRecord {
            id: id.to_string(),
            name: name.to_string(),
            can_edit,
            is_default: false,
        }
    }

    fn calendar_record(id: &str, name: &str, can_edit: bool) -> MicrosoftCalendarRecord {
        MicrosoftCalendarRecord {
            id: id.to_string(),
            name: name.to_string(),
            can_edit,
            is_default: false,
        }
    }

    #[derive(Default)]
    struct MemoryTokenStore {
        tokens: RefCell<HashMap<String, MicrosoftToken>>,
    }

    impl MemoryTokenStore {
        fn with_token(account_id: &str) -> Self {
            let store = Self::default();
            store.tokens.borrow_mut().insert(
                account_id.to_string(),
                MicrosoftToken {
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    expires_at_epoch_seconds: current_epoch_seconds().saturating_add(3600),
                },
            );
            store
        }
    }

    impl MicrosoftTokenStore for MemoryTokenStore {
        fn load(&self, account_id: &str) -> Result<Option<MicrosoftToken>, ProviderError> {
            Ok(self.tokens.borrow().get(account_id).cloned())
        }

        fn save(&self, account_id: &str, token: &MicrosoftToken) -> Result<(), ProviderError> {
            self.tokens
                .borrow_mut()
                .insert(account_id.to_string(), token.clone());
            Ok(())
        }

        fn delete(&self, account_id: &str) -> Result<(), ProviderError> {
            self.tokens.borrow_mut().remove(account_id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct GoogleMemoryTokenStore {
        tokens: RefCell<HashMap<String, GoogleToken>>,
    }

    impl GoogleMemoryTokenStore {
        fn with_token(account_id: &str) -> Self {
            let store = Self::default();
            store.tokens.borrow_mut().insert(
                account_id.to_string(),
                GoogleToken {
                    access_token: "access-token".to_string(),
                    refresh_token: "refresh-token".to_string(),
                    expires_at_epoch_seconds: current_epoch_seconds().saturating_add(3600),
                },
            );
            store
        }
    }

    impl GoogleTokenStore for GoogleMemoryTokenStore {
        fn load(&self, account_id: &str) -> Result<Option<GoogleToken>, ProviderError> {
            Ok(self.tokens.borrow().get(account_id).cloned())
        }

        fn save(&self, account_id: &str, token: &GoogleToken) -> Result<(), ProviderError> {
            self.tokens
                .borrow_mut()
                .insert(account_id.to_string(), token.clone());
            Ok(())
        }

        fn delete(&self, account_id: &str) -> Result<(), ProviderError> {
            self.tokens.borrow_mut().remove(account_id);
            Ok(())
        }
    }

    struct RecordingHttpClient {
        responses: RefCell<VecDeque<MicrosoftHttpResponse>>,
        requests: RefCell<Vec<MicrosoftHttpRequest>>,
    }

    impl RecordingHttpClient {
        fn new(responses: Vec<MicrosoftHttpResponse>) -> Self {
            Self {
                responses: RefCell::new(VecDeque::from(responses)),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn json(status: u16, value: Value) -> MicrosoftHttpResponse {
            MicrosoftHttpResponse {
                status,
                headers: Vec::new(),
                body: value.to_string(),
            }
        }

        fn text_with_header(
            status: u16,
            body: &str,
            name: &str,
            value: &str,
        ) -> MicrosoftHttpResponse {
            MicrosoftHttpResponse {
                status,
                headers: vec![(name.to_string(), value.to_string())],
                body: body.to_string(),
            }
        }
    }

    impl MicrosoftHttpClient for RecordingHttpClient {
        fn request(
            &self,
            request: MicrosoftHttpRequest,
        ) -> Result<MicrosoftHttpResponse, ProviderError> {
            self.requests.borrow_mut().push(request);
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| ProviderError::Http("unexpected request".to_string()))
        }
    }

    #[test]
    fn graph_timed_event_maps_to_rcal_event() {
        let calendar = MicrosoftCalendarRecord {
            id: "cal".to_string(),
            name: "Work".to_string(),
            can_edit: true,
            is_default: true,
        };
        let raw = json!({
            "id": "abc",
            "subject": "Standup",
            "type": "singleInstance",
            "isAllDay": false,
            "start": {"dateTime": "2026-04-23T09:00:00", "timeZone": "UTC"},
            "end": {"dateTime": "2026-04-23T09:30:00", "timeZone": "UTC"},
            "location": {"displayName": "Room"},
            "bodyPreview": "Notes",
            "isReminderOn": true,
            "reminderMinutesBeforeStart": 15
        });

        let cached = MicrosoftCachedEvent::from_graph("work", &calendar, raw).expect("maps");
        let event = cached.to_event().expect("event converts");

        assert_eq!(event.id, "microsoft:work:cal:abc");
        assert_eq!(event.title, "Standup");
        assert_eq!(event.location.as_deref(), Some("Room"));
        assert_eq!(event.reminders, vec![Reminder::minutes_before(15)]);
        assert!(event.source.source_id.starts_with("microsoft:work:cal"));
    }

    #[test]
    fn google_timed_event_maps_to_rcal_event() {
        let calendar = google_calendar_record("primary", "Calendar", true);
        let raw = json!({
            "id": "abc",
            "summary": "Standup",
            "eventType": "default",
            "status": "confirmed",
            "start": {"dateTime": "2026-04-23T09:00:00Z"},
            "end": {"dateTime": "2026-04-23T09:30:00Z"},
            "location": "Room",
            "description": "Notes",
            "reminders": {
                "useDefault": false,
                "overrides": [
                    {"method": "popup", "minutes": 10},
                    {"method": "email", "minutes": 60}
                ]
            }
        });

        let cached = GoogleCachedEvent::from_google("personal", &calendar, raw).expect("maps");
        let event = cached.to_event().expect("event converts");

        assert_eq!(event.id, "google:personal:primary:abc");
        assert_eq!(event.title, "Standup");
        assert_eq!(event.location.as_deref(), Some("Room"));
        assert_eq!(event.notes.as_deref(), Some("Notes"));
        assert_eq!(event.reminders.len(), 2);
        assert_eq!(event.source.source_id, "google:personal:primary");
    }

    #[test]
    fn google_rrule_payload_and_parse_cover_weekly_multi_day() {
        let draft = CreateEventDraft {
            title: "Class".to_string(),
            timing: CreateEventTiming::Timed {
                start: EventDateTime::new(
                    date(2026, Month::April, 23),
                    Time::from_hms(13, 50, 0).unwrap(),
                ),
                end: EventDateTime::new(
                    date(2026, Month::April, 23),
                    Time::from_hms(14, 40, 0).unwrap(),
                ),
            },
            location: None,
            notes: None,
            reminders: Vec::new(),
            recurrence: Some(RecurrenceRule {
                frequency: RecurrenceFrequency::Weekly,
                interval: 1,
                end: RecurrenceEnd::Count(9),
                weekdays: vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday],
                monthly: None,
                yearly: None,
            }),
        };

        let rule = google_rrule_payload(draft.recurrence.as_ref().unwrap(), &draft);
        let parsed = google_rrule_to_cache(&rule)
            .and_then(|cache| cache.to_rule())
            .expect("rrule parses");

        assert_eq!(rule, "RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=9");
        assert_eq!(parsed.frequency, RecurrenceFrequency::Weekly);
        assert_eq!(parsed.weekdays, draft.recurrence.as_ref().unwrap().weekdays);
        assert_eq!(parsed.end, RecurrenceEnd::Count(9));
    }

    #[test]
    fn google_sync_writes_selected_calendar_cache_and_renders_event() {
        let cache_file = temp_path("google-sync/google-cache.json");
        let _ = fs::remove_file(&cache_file);
        let store = GoogleMemoryTokenStore::with_token("personal");
        let http = RecordingHttpClient::new(vec![
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "primary",
                    "summary": "Calendar",
                    "accessRole": "owner",
                    "primary": true
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "items": [
                        {
                            "id": "evt",
                            "summary": "Planning",
                            "eventType": "default",
                            "status": "confirmed",
                            "start": {"dateTime": "2026-04-23T09:00:00Z"},
                            "end": {"dateTime": "2026-04-23T10:00:00Z"}
                        }
                    ]
                }),
            ),
        ]);
        let mut runtime =
            GoogleProviderRuntime::load(google_provider_config(cache_file.clone())).expect("load");

        let summary = runtime
            .sync(
                Some("personal"),
                &http,
                &store,
                date(2026, Month::April, 23),
            )
            .expect("sync succeeds");
        let source = GoogleAgendaSource::load(&cache_file).expect("cache reloads");
        let events = source.events_intersecting(DateRange::day(date(2026, Month::April, 23)));
        let _ = fs::remove_file(&cache_file);

        assert_eq!(summary.events, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "google:personal:primary:evt");
        assert_eq!(events[0].source.source_id, "google:personal:primary");
    }

    #[test]
    fn google_sync_fetches_recurring_master_without_render_duplicate() {
        let cache_file = temp_path("google-series/google-cache.json");
        let _ = fs::remove_file(&cache_file);
        let store = GoogleMemoryTokenStore::with_token("personal");
        let http = RecordingHttpClient::new(vec![
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "primary",
                    "summary": "Calendar",
                    "accessRole": "owner",
                    "primary": true
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "items": [
                        {
                            "id": "occ-1",
                            "summary": "Class",
                            "eventType": "default",
                            "status": "confirmed",
                            "recurringEventId": "master",
                            "originalStartTime": {"dateTime": "2026-04-23T13:50:00Z"},
                            "start": {"dateTime": "2026-04-23T13:50:00Z"},
                            "end": {"dateTime": "2026-04-23T14:40:00Z"}
                        }
                    ]
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "master",
                    "summary": "Class",
                    "eventType": "default",
                    "status": "confirmed",
                    "start": {"dateTime": "2026-04-20T13:50:00Z"},
                    "end": {"dateTime": "2026-04-20T14:40:00Z"},
                    "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR"]
                }),
            ),
        ]);
        let mut runtime =
            GoogleProviderRuntime::load(google_provider_config(cache_file.clone())).expect("load");

        runtime
            .sync(
                Some("personal"),
                &http,
                &store,
                date(2026, Month::April, 23),
            )
            .expect("sync succeeds");
        let source = GoogleAgendaSource::load(&cache_file).expect("cache reloads");
        let events = source.events_intersecting(DateRange::day(date(2026, Month::April, 23)));
        let master = source
            .editable_event_by_id("google:personal:primary:master")
            .expect("series master cached for edit");
        let _ = fs::remove_file(&cache_file);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "google:personal:primary:occ-1");
        assert_eq!(
            events[0]
                .occurrence
                .as_ref()
                .map(|occurrence| occurrence.series_id.as_str()),
            Some("google:personal:primary:master")
        );
        assert!(master.recurrence.is_some());
    }

    #[test]
    fn google_oauth_errors_parse_top_level_error_response() {
        let response = RecordingHttpClient::json(
            400,
            json!({
                "error": "invalid_request",
                "error_description": "client_secret is missing."
            }),
        );

        let err = google_token_from_response(response, None).expect_err("400 fails");

        assert_eq!(
            err.to_string(),
            "provider auth error: Google OAuth 400: invalid_request: client_secret is missing."
        );
    }

    #[test]
    fn provider_write_targets_use_configured_editable_calendars() {
        let cache_file = temp_path("targets/microsoft-cache.json");
        let mut config = provider_config(cache_file);
        config.default_calendar = Some("team".to_string());
        config.accounts[0].calendars = vec![
            "cal".to_string(),
            "team".to_string(),
            "holidays".to_string(),
        ];
        let mut runtime = MicrosoftProviderRuntime::load(config).expect("load");
        runtime
            .cache
            .replace_calendar("work", calendar_record("cal", "Work", true), Vec::new(), 0);
        runtime.cache.replace_calendar(
            "work",
            calendar_record("holidays", "Holidays", false),
            Vec::new(),
            0,
        );

        let targets = runtime.write_targets();

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].label, "Microsoft work: Work");
        assert_eq!(targets[1].label, "Microsoft work: team");
        assert_eq!(
            runtime.default_write_target(),
            Some(EventWriteTargetId::microsoft("work", "team"))
        );
    }

    #[test]
    fn create_event_uses_explicit_calendar_target() {
        let cache_file = temp_path("create-target/microsoft-cache.json");
        let mut config = provider_config(cache_file.clone());
        config.accounts[0].calendars = vec!["cal".to_string(), "personal".to_string()];
        let store = MemoryTokenStore::with_token("work");
        let http = RecordingHttpClient::new(vec![
            RecordingHttpClient::json(
                201,
                json!({
                    "id": "evt",
                    "subject": "Planning",
                    "type": "singleInstance",
                    "isAllDay": false,
                    "start": {"dateTime": "2026-04-23T09:00:00", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-04-23T10:00:00", "timeZone": "UTC"}
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "personal",
                    "name": "Personal",
                    "canEdit": true,
                    "isDefaultCalendar": false
                }),
            ),
        ]);
        let mut runtime = MicrosoftProviderRuntime::load(config).expect("load");
        let day = date(2026, Month::April, 23);
        let draft = CreateEventDraft {
            title: "Planning".to_string(),
            timing: CreateEventTiming::Timed {
                start: at(day, 9, 0),
                end: at(day, 10, 0),
            },
            location: None,
            notes: None,
            reminders: Vec::new(),
            recurrence: None,
        };
        let target = EventWriteTargetId::microsoft("work", "personal");

        let event = runtime
            .create_event_in_target(draft, &target, &http, &store)
            .expect("create succeeds");

        let _ = fs::remove_dir_all(
            cache_file
                .parent()
                .and_then(Path::parent)
                .expect("test root"),
        );
        assert_eq!(event.id, "microsoft:work:personal:evt");
        assert_eq!(
            http.requests.borrow()[0].url,
            format!("{GRAPH_BASE_URL}/me/calendars/personal/events")
        );
    }

    #[test]
    fn graph_all_day_recurring_occurrence_maps_anchor() {
        let calendar = MicrosoftCalendarRecord {
            id: "cal".to_string(),
            name: "Work".to_string(),
            can_edit: true,
            is_default: true,
        };
        let raw = json!({
            "id": "occ",
            "subject": "OOO",
            "type": "occurrence",
            "seriesMasterId": "master",
            "isAllDay": true,
            "start": {"dateTime": "2026-04-23T00:00:00", "timeZone": "UTC"},
            "end": {"dateTime": "2026-04-24T00:00:00", "timeZone": "UTC"}
        });

        let event = MicrosoftCachedEvent::from_graph("work", &calendar, raw)
            .expect("maps")
            .to_event()
            .expect("event converts");

        assert_eq!(
            event.occurrence(),
            Some(&OccurrenceMetadata {
                series_id: "microsoft:work:cal:master".to_string(),
                anchor: OccurrenceAnchor::AllDay {
                    date: date(2026, Month::April, 23)
                },
            })
        );
    }

    #[test]
    fn draft_payload_rejects_multiple_microsoft_reminders() {
        let draft = CreateEventDraft {
            title: "Focus".to_string(),
            timing: CreateEventTiming::Timed {
                start: at(date(2026, Month::April, 23), 9, 0),
                end: at(date(2026, Month::April, 23), 10, 0),
            },
            location: None,
            notes: None,
            reminders: vec![Reminder::minutes_before(5), Reminder::minutes_before(10)],
            recurrence: None,
        };

        let err = graph_event_payload(&draft, false).expect_err("multiple reminders fail");
        assert!(err.to_string().contains("only one reminder"));
    }

    #[test]
    fn weekly_recurrence_payload_uses_graph_pattern() {
        let draft = CreateEventDraft {
            title: "Class".to_string(),
            timing: CreateEventTiming::Timed {
                start: at(date(2026, Month::April, 20), 13, 50),
                end: at(date(2026, Month::April, 20), 14, 40),
            },
            location: None,
            notes: None,
            reminders: Vec::new(),
            recurrence: Some(RecurrenceRule {
                frequency: RecurrenceFrequency::Weekly,
                interval: 1,
                end: RecurrenceEnd::Count(10),
                weekdays: vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday],
                monthly: None,
                yearly: None,
            }),
        };

        let payload = graph_event_payload(&draft, false).expect("payload builds");
        let recurrence = payload.get("recurrence").expect("recurrence included");

        assert_eq!(recurrence["pattern"]["type"], "weekly");
        assert_eq!(
            recurrence["pattern"]["daysOfWeek"],
            json!(["monday", "wednesday", "friday"])
        );
        assert_eq!(recurrence["range"]["type"], "numbered");
        assert_eq!(recurrence["range"]["numberOfOccurrences"], 10);
    }

    #[test]
    fn list_calendars_uses_stored_token_and_maps_response() {
        let store = MemoryTokenStore::with_token("work");
        let http = RecordingHttpClient::new(vec![RecordingHttpClient::json(
            200,
            json!({
                "value": [
                    {
                        "id": "cal",
                        "name": "Calendar",
                        "canEdit": true,
                        "isDefaultCalendar": true
                    }
                ]
            }),
        )]);

        let calendars = list_calendars(&account(), &http, &store).expect("calendars list");

        assert_eq!(
            calendars,
            vec![MicrosoftCalendarInfo {
                id: "cal".to_string(),
                name: "Calendar".to_string(),
                can_edit: true,
                is_default: true,
            }]
        );
        let requests = http.requests.borrow();
        assert_eq!(requests[0].method, "GET");
        assert!(
            requests[0]
                .url
                .ends_with("/me/calendars?$select=id,name,canEdit,isDefaultCalendar")
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "Authorization" && value == "Bearer access-token")
        );
    }

    #[test]
    fn graph_errors_include_www_authenticate_header_when_body_is_empty() {
        let response = RecordingHttpClient::text_with_header(
            401,
            "",
            "WWW-Authenticate",
            "Bearer error=\"invalid_token\", error_description=\"Invalid audience\"",
        );

        let err = parse_graph_success_json(response).expect_err("401 fails");

        assert_eq!(
            err.to_string(),
            "Microsoft Graph error: HTTP 401: Bearer error=\"invalid_token\", error_description=\"Invalid audience\""
        );
    }

    #[test]
    fn inspect_token_reports_safe_jwt_claims_without_token_body() {
        let store = MemoryTokenStore::default();
        let claims = json!({
            "aud": "https://graph.microsoft.com",
            "scp": "User.Read Calendars.ReadWrite",
            "tid": "tenant-id",
            "iss": "https://sts.windows.net/tenant-id/",
            "appid": "app-id",
            "azp": "authorized-party",
            "exp": 1_777_000_000
        });
        let access_token = format!(
            "{}.{}.signature",
            base64_url_no_pad(br#"{"alg":"none"}"#),
            base64_url_no_pad(claims.to_string().as_bytes())
        );
        store.tokens.borrow_mut().insert(
            "work".to_string(),
            MicrosoftToken {
                access_token,
                refresh_token: "refresh".to_string(),
                expires_at_epoch_seconds: 1_777_000_100,
            },
        );

        let inspection = inspect_token("work", &store).expect("token inspects");

        assert_eq!(inspection.account_id, "work");
        assert_eq!(inspection.token_format, "jwt");
        assert_eq!(
            inspection.audience.as_deref(),
            Some("https://graph.microsoft.com")
        );
        assert_eq!(
            inspection.scopes.as_deref(),
            Some("User.Read Calendars.ReadWrite")
        );
        assert_eq!(inspection.tenant_id.as_deref(), Some("tenant-id"));
        assert_eq!(inspection.jwt_expires_at_epoch_seconds, Some(1_777_000_000));
        assert!(inspection.has_refresh_token);
    }

    #[test]
    fn inspect_token_tolerates_opaque_access_tokens() {
        let store = MemoryTokenStore::default();
        store.tokens.borrow_mut().insert(
            "work".to_string(),
            MicrosoftToken {
                access_token: "opaque-consumer-token".to_string(),
                refresh_token: "refresh".to_string(),
                expires_at_epoch_seconds: 1_777_000_100,
            },
        );

        let inspection = inspect_token("work", &store).expect("opaque token inspects");

        assert_eq!(inspection.token_format, "opaque");
        assert_eq!(inspection.audience, None);
        assert_eq!(inspection.scopes, None);
        assert_eq!(inspection.jwt_expires_at_epoch_seconds, None);
        assert!(inspection.has_refresh_token);
    }

    #[test]
    fn oauth_callback_response_surfaces_browser_errors() {
        let mut response = Vec::new();

        write_oauth_callback_response(
            &mut response,
            "Microsoft",
            false,
            "invalid_request: the application must use consumers",
        )
        .expect("response writes");

        let response = String::from_utf8(response).expect("utf8 response");
        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
        assert!(response.contains("rcal Microsoft login failed"));
        assert!(response.contains("invalid_request"));
    }

    #[test]
    fn sync_writes_selected_calendar_cache_and_renders_provider_event() {
        let cache_file = temp_path("sync/microsoft-cache.json");
        let _ = fs::remove_dir_all(
            cache_file
                .parent()
                .and_then(Path::parent)
                .expect("test root"),
        );
        let store = MemoryTokenStore::with_token("work");
        let http = RecordingHttpClient::new(vec![
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "cal",
                    "name": "Work",
                    "canEdit": true,
                    "isDefaultCalendar": true
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "value": [
                        {
                            "id": "evt",
                            "subject": "Focus",
                            "type": "singleInstance",
                            "isAllDay": false,
                            "start": {"dateTime": "2026-04-23T09:00:00", "timeZone": "UTC"},
                            "end": {"dateTime": "2026-04-23T10:00:00", "timeZone": "UTC"}
                        }
                    ]
                }),
            ),
        ]);
        let mut runtime =
            MicrosoftProviderRuntime::load(provider_config(cache_file.clone())).expect("load");

        let summary = runtime
            .sync(None, &http, &store, date(2026, Month::April, 23))
            .expect("sync succeeds");
        let source = MicrosoftAgendaSource::load(&cache_file).expect("cache reloads");
        let events = source.events_intersecting(DateRange::day(date(2026, Month::April, 23)));

        let _ = fs::remove_dir_all(
            cache_file
                .parent()
                .and_then(Path::parent)
                .expect("test root"),
        );
        assert_eq!(summary.accounts, 1);
        assert_eq!(summary.calendars, 1);
        assert_eq!(summary.events, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "microsoft:work:cal:evt");
        assert_eq!(events[0].title, "Focus");
        assert!(events[0].is_microsoft());
    }

    #[test]
    fn sync_fetches_series_master_for_provider_series_edit_without_render_duplicate() {
        let cache_file = temp_path("series/microsoft-cache.json");
        let _ = fs::remove_dir_all(
            cache_file
                .parent()
                .and_then(Path::parent)
                .expect("test root"),
        );
        let store = MemoryTokenStore::with_token("work");
        let http = RecordingHttpClient::new(vec![
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "cal",
                    "name": "Work",
                    "canEdit": true,
                    "isDefaultCalendar": true
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "value": [
                        {
                            "id": "occ-1",
                            "subject": "Class",
                            "type": "occurrence",
                            "seriesMasterId": "master",
                            "isAllDay": false,
                            "start": {"dateTime": "2026-04-24T13:50:00", "timeZone": "UTC"},
                            "end": {"dateTime": "2026-04-24T14:40:00", "timeZone": "UTC"}
                        }
                    ]
                }),
            ),
            RecordingHttpClient::json(
                200,
                json!({
                    "id": "master",
                    "subject": "Class",
                    "type": "seriesMaster",
                    "isAllDay": false,
                    "start": {"dateTime": "2026-04-20T13:50:00", "timeZone": "UTC"},
                    "end": {"dateTime": "2026-04-20T14:40:00", "timeZone": "UTC"},
                    "recurrence": {
                        "pattern": {
                            "type": "weekly",
                            "interval": 1,
                            "daysOfWeek": ["monday", "wednesday", "friday"],
                            "firstDayOfWeek": "sunday"
                        },
                        "range": {
                            "type": "noEnd",
                            "startDate": "2026-04-20",
                            "recurrenceTimeZone": "UTC"
                        }
                    }
                }),
            ),
        ]);
        let mut runtime =
            MicrosoftProviderRuntime::load(provider_config(cache_file.clone())).expect("load");

        runtime
            .sync(None, &http, &store, date(2026, Month::April, 23))
            .expect("sync succeeds");
        let source = MicrosoftAgendaSource::load(&cache_file).expect("cache reloads");
        let events = source.events_intersecting(DateRange::day(date(2026, Month::April, 24)));
        let series = source
            .editable_event_by_id("microsoft:work:cal:master")
            .expect("series master is cached for editing");

        let _ = fs::remove_dir_all(
            cache_file
                .parent()
                .and_then(Path::parent)
                .expect("test root"),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "microsoft:work:cal:occ-1");
        assert!(events[0].occurrence().is_some());
        assert!(series.recurrence.is_some());
    }
}
