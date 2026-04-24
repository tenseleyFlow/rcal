use std::{
    env,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    app::{KeyBindingError, KeyBindingOverrides, KeyBindings},
    providers::{
        MicrosoftAccountConfig, MicrosoftProviderConfig, ProviderConfig, ProviderCreateTarget,
    },
};

pub const DEFAULT_CONFIG_TOML: &str = r#"# rcal configuration
# Generate this file with `rcal config init`.

[paths]
# Local user-created events.
events_file = "~/.local/share/rcal/events.json"

[holidays]
# One of: "off", "us-federal", "nager".
source = "us-federal"
# Two-letter country code. Only used by the Nager.Date holiday source.
country = "US"

[reminders]
# Delivered/skipped reminder state. Services snapshot this path at install time.
state_file = "~/.local/state/rcal/reminders-state.json"

[providers]
# Where newly created events go when provider support is enabled: "local" or "microsoft".
create_target = "local"

[providers.microsoft]
# Microsoft Graph provider for Outlook / Microsoft 365 calendars.
# Current development builds require your own Microsoft Entra app client_id.
# Use tenant = "consumers" for personal Outlook/Hotmail accounts and
# tenant = "organizations" for work or school Microsoft 365 accounts.
enabled = false
default_account = "work"
default_calendar = "CALENDAR_ID"
sync_past_days = 30
sync_future_days = 365
# Provider cache is separate from the local events file.
# cache_file = "~/.cache/rcal/microsoft-cache.json"

[[providers.microsoft.accounts]]
id = "work"
client_id = "AZURE_APP_CLIENT_ID"
tenant = "organizations"
redirect_port = 8765
calendars = ["CALENDAR_ID"]

[keybindings]
# Normal month/day app commands. Modal/form editing keys are fixed for now.
move_left = ["left"]
move_right = ["right"]
move_up = ["up"]
move_down = ["down"]
open_day_or_edit = ["enter"]
close_day = ["esc"]
create_event = ["+"]
delete_event = ["d"]
copy_event = ["c"]
help = ["?"]
quit = ["q", "ctrl-c"]

jump_monday = ["m"]
jump_tuesday = ["tu"]
jump_wednesday = ["w"]
jump_thursday = ["th"]
jump_friday = ["f"]
jump_saturday = ["sa"]
jump_sunday = ["su"]
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub path: Option<PathBuf>,
    pub events_file: Option<PathBuf>,
    pub holiday_source: Option<ConfigHolidaySource>,
    pub holiday_country: Option<String>,
    pub reminder_state_file: Option<PathBuf>,
    pub keybindings: KeyBindings,
    pub providers: ProviderConfig,
}

impl UserConfig {
    pub fn empty() -> Self {
        Self {
            path: None,
            events_file: None,
            holiday_source: None,
            holiday_country: None,
            reminder_state_file: None,
            keybindings: KeyBindings::default(),
            providers: ProviderConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigHolidaySource {
    Off,
    UsFederal,
    Nager,
}

pub fn default_config_file() -> PathBuf {
    default_config_file_for(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn default_config_file_for(
    xdg_config_home: Option<impl Into<PathBuf>>,
    home: Option<impl Into<PathBuf>>,
) -> PathBuf {
    if let Some(xdg_config_home) = xdg_config_home {
        return xdg_config_home.into().join("rcal").join("config.toml");
    }

    if let Some(home) = home {
        return home.into().join(".config").join("rcal").join("config.toml");
    }

    env::temp_dir().join("rcal").join("config.toml")
}

pub fn load_discovered_config() -> Result<UserConfig, ConfigError> {
    let path = default_config_file();
    if !path.exists() {
        return Ok(UserConfig::empty());
    }

    load_config_file(&path)
}

pub fn load_explicit_config(path: impl Into<PathBuf>) -> Result<UserConfig, ConfigError> {
    let path = expand_user_path(path.into())?;
    if !path.exists() {
        return Err(ConfigError::Missing { path });
    }

    load_config_file(&path)
}

pub fn load_config_file(path: &Path) -> Result<UserConfig, ConfigError> {
    let body = fs::read_to_string(path).map_err(|err| ConfigError::Read {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    let parsed = toml::from_str::<RawConfig>(&body).map_err(|err| ConfigError::Parse {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    raw_config_to_user_config(parsed, path)
}

pub fn init_config_file(path: Option<PathBuf>, force: bool) -> Result<PathBuf, ConfigError> {
    let path = match path {
        Some(path) => expand_user_path(path)?,
        None => default_config_file(),
    };

    if path.exists() && !force {
        return Err(ConfigError::AlreadyExists { path });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| ConfigError::Write {
            path: parent.to_path_buf(),
            reason: err.to_string(),
        })?;
    }

    fs::write(&path, DEFAULT_CONFIG_TOML).map_err(|err| ConfigError::Write {
        path: path.clone(),
        reason: err.to_string(),
    })?;

    Ok(path)
}

fn raw_config_to_user_config(raw: RawConfig, path: &Path) -> Result<UserConfig, ConfigError> {
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let events_file = raw
        .paths
        .and_then(|paths| paths.events_file)
        .map(|path| resolve_config_path(&path, base_dir))
        .transpose()?;
    let reminder_state_file = raw
        .reminders
        .and_then(|reminders| reminders.state_file)
        .map(|path| resolve_config_path(&path, base_dir))
        .transpose()?;

    let (holiday_source, holiday_country) = if let Some(holidays) = raw.holidays {
        (
            holidays
                .source
                .map(|value| parse_holiday_source(&value, path))
                .transpose()?,
            holidays
                .country
                .map(|value| parse_holiday_country(&value, path))
                .transpose()?,
        )
    } else {
        (None, None)
    };

    let keybindings = if let Some(keybindings) = raw.keybindings {
        KeyBindings::with_overrides(keybindings.into_overrides()).map_err(|err| {
            ConfigError::Invalid {
                path: path.to_path_buf(),
                reason: format!("invalid keybindings: {err}"),
            }
        })?
    } else {
        KeyBindings::default()
    };
    let providers = raw
        .providers
        .map(|providers| providers.into_config(path, base_dir))
        .transpose()?
        .unwrap_or_default();

    Ok(UserConfig {
        path: Some(path.to_path_buf()),
        events_file,
        holiday_source,
        holiday_country,
        reminder_state_file,
        keybindings,
        providers,
    })
}

fn parse_holiday_source(value: &str, path: &Path) -> Result<ConfigHolidaySource, ConfigError> {
    match value {
        "off" => Ok(ConfigHolidaySource::Off),
        "us-federal" => Ok(ConfigHolidaySource::UsFederal),
        "nager" => Ok(ConfigHolidaySource::Nager),
        _ => Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: format!(
                "invalid holidays.source '{value}'; expected off, us-federal, or nager"
            ),
        }),
    }
}

fn parse_holiday_country(value: &str, path: &Path) -> Result<String, ConfigError> {
    if value.len() == 2 && value.bytes().all(|value| value.is_ascii_alphabetic()) {
        Ok(value.to_ascii_uppercase())
    } else {
        Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: format!("invalid holidays.country '{value}'; expected two ASCII letters"),
        })
    }
}

fn resolve_config_path(value: &str, base_dir: &Path) -> Result<PathBuf, ConfigError> {
    let expanded = expand_user_path(PathBuf::from(value))?;
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(base_dir.join(expanded))
    }
}

pub fn expand_user_path(path: PathBuf) -> Result<PathBuf, ConfigError> {
    let Some(value) = path.to_str() else {
        return Ok(path);
    };

    if value == "~" {
        return Ok(home_dir()?.to_path_buf());
    }

    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }

    Ok(path)
}

fn home_dir() -> Result<PathBuf, ConfigError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(ConfigError::MissingHome)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    paths: Option<RawPathsConfig>,
    holidays: Option<RawHolidaysConfig>,
    reminders: Option<RawRemindersConfig>,
    providers: Option<RawProvidersConfig>,
    keybindings: Option<RawKeyBindingsConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPathsConfig {
    events_file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHolidaysConfig {
    source: Option<String>,
    country: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRemindersConfig {
    state_file: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvidersConfig {
    create_target: Option<String>,
    microsoft: Option<RawMicrosoftProviderConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMicrosoftProviderConfig {
    enabled: Option<bool>,
    default_account: Option<String>,
    default_calendar: Option<String>,
    sync_past_days: Option<i32>,
    sync_future_days: Option<i32>,
    cache_file: Option<String>,
    accounts: Option<Vec<RawMicrosoftAccountConfig>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMicrosoftAccountConfig {
    id: String,
    client_id: String,
    tenant: String,
    redirect_port: Option<u16>,
    calendars: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeyBindingsConfig {
    move_left: Option<Vec<String>>,
    move_right: Option<Vec<String>>,
    move_up: Option<Vec<String>>,
    move_down: Option<Vec<String>>,
    open_day_or_edit: Option<Vec<String>>,
    close_day: Option<Vec<String>>,
    create_event: Option<Vec<String>>,
    delete_event: Option<Vec<String>>,
    copy_event: Option<Vec<String>>,
    help: Option<Vec<String>>,
    quit: Option<Vec<String>>,
    jump_monday: Option<Vec<String>>,
    jump_tuesday: Option<Vec<String>>,
    jump_wednesday: Option<Vec<String>>,
    jump_thursday: Option<Vec<String>>,
    jump_friday: Option<Vec<String>>,
    jump_saturday: Option<Vec<String>>,
    jump_sunday: Option<Vec<String>>,
}

impl RawKeyBindingsConfig {
    fn into_overrides(self) -> KeyBindingOverrides {
        KeyBindingOverrides {
            move_left: self.move_left,
            move_right: self.move_right,
            move_up: self.move_up,
            move_down: self.move_down,
            open_day_or_edit: self.open_day_or_edit,
            close_day: self.close_day,
            create_event: self.create_event,
            delete_event: self.delete_event,
            copy_event: self.copy_event,
            help: self.help,
            quit: self.quit,
            jump_monday: self.jump_monday,
            jump_tuesday: self.jump_tuesday,
            jump_wednesday: self.jump_wednesday,
            jump_thursday: self.jump_thursday,
            jump_friday: self.jump_friday,
            jump_saturday: self.jump_saturday,
            jump_sunday: self.jump_sunday,
        }
    }
}

impl RawProvidersConfig {
    fn into_config(self, path: &Path, base_dir: &Path) -> Result<ProviderConfig, ConfigError> {
        let mut config = ProviderConfig::default();
        let create_target_was_set = self.create_target.is_some();
        if let Some(create_target) = self.create_target {
            config.create_target = parse_create_target(&create_target, path)?;
        }
        if let Some(microsoft) = self.microsoft {
            config.microsoft = microsoft.into_config(path, base_dir)?;
            if config.microsoft.enabled && !create_target_was_set {
                config.create_target = ProviderCreateTarget::Microsoft;
            }
        }
        config
            .microsoft
            .validate()
            .map_err(|err| ConfigError::Invalid {
                path: path.to_path_buf(),
                reason: err.to_string(),
            })?;
        Ok(config)
    }
}

impl RawMicrosoftProviderConfig {
    fn into_config(
        self,
        path: &Path,
        base_dir: &Path,
    ) -> Result<MicrosoftProviderConfig, ConfigError> {
        let mut config = MicrosoftProviderConfig::default();
        if let Some(enabled) = self.enabled {
            config.enabled = enabled;
        }
        config.default_account = self.default_account;
        config.default_calendar = self.default_calendar;
        if let Some(sync_past_days) = self.sync_past_days {
            config.sync_past_days = sync_past_days.max(0);
        }
        if let Some(sync_future_days) = self.sync_future_days {
            config.sync_future_days = sync_future_days.max(1);
        }
        if let Some(cache_file) = self.cache_file {
            config.cache_file = resolve_config_path(&cache_file, base_dir)?;
        }
        config.accounts = self
            .accounts
            .unwrap_or_default()
            .into_iter()
            .map(RawMicrosoftAccountConfig::into_config)
            .collect();
        config.validate().map_err(|err| ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
        Ok(config)
    }
}

impl RawMicrosoftAccountConfig {
    fn into_config(self) -> MicrosoftAccountConfig {
        MicrosoftAccountConfig {
            id: self.id,
            client_id: self.client_id,
            tenant: self.tenant,
            redirect_port: self.redirect_port.unwrap_or(8765),
            calendars: self.calendars.unwrap_or_default(),
        }
    }
}

fn parse_create_target(value: &str, path: &Path) -> Result<ProviderCreateTarget, ConfigError> {
    match value {
        "local" => Ok(ProviderCreateTarget::Local),
        "microsoft" => Ok(ProviderCreateTarget::Microsoft),
        _ => Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            reason: format!(
                "invalid providers.create_target '{value}'; expected local or microsoft"
            ),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Missing { path: PathBuf },
    MissingHome,
    AlreadyExists { path: PathBuf },
    Read { path: PathBuf, reason: String },
    Write { path: PathBuf, reason: String },
    Parse { path: PathBuf, reason: String },
    Invalid { path: PathBuf, reason: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "config file does not exist: {}", path.display()),
            Self::MissingHome => write!(f, "failed to locate a user home directory"),
            Self::AlreadyExists { path } => {
                write!(
                    f,
                    "config file already exists at {}; pass --force to overwrite",
                    path.display()
                )
            }
            Self::Read { path, reason } => {
                write!(f, "failed to read config {}: {reason}", path.display())
            }
            Self::Write { path, reason } => {
                write!(f, "failed to write config {}: {reason}", path.display())
            }
            Self::Parse { path, reason } => {
                write!(f, "failed to parse config {}: {reason}", path.display())
            }
            Self::Invalid { path, reason } => {
                write!(f, "invalid config {}: {reason}", path.display())
            }
        }
    }
}

impl Error for ConfigError {}

impl From<KeyBindingError> for ConfigError {
    fn from(err: KeyBindingError) -> Self {
        Self::Invalid {
            path: PathBuf::from("<config>"),
            reason: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_config_path(name: &str) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir()
            .join(format!("rcal-config-test-{}-{counter}", std::process::id()))
            .join(name)
    }

    #[test]
    fn default_config_path_prefers_xdg_then_home() {
        assert_eq!(
            default_config_file_for(Some("/tmp/xdg"), Some("/tmp/home")),
            PathBuf::from("/tmp/xdg/rcal/config.toml")
        );
        assert_eq!(
            default_config_file_for(None::<&str>, Some("/tmp/home")),
            PathBuf::from("/tmp/home/.config/rcal/config.toml")
        );
    }

    #[test]
    fn missing_discovered_config_is_empty() {
        let config = UserConfig::empty();
        assert!(config.events_file.is_none());
        assert!(config.reminder_state_file.is_none());
    }

    #[test]
    fn config_loads_paths_relative_to_file_and_expands_home() {
        let path = temp_config_path("relative/config.toml");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        fs::create_dir_all(path.parent().expect("config dir")).expect("dir creates");
        fs::write(
            &path,
            r#"
[paths]
events_file = "events.json"

[reminders]
state_file = "~/state.json"
"#,
        )
        .expect("config writes");

        let config = load_config_file(&path).expect("config loads");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));

        let expected_events_file = path.parent().expect("config dir").join("events.json");
        assert_eq!(
            config.events_file.as_deref(),
            Some(expected_events_file.as_path())
        );
        assert!(
            config
                .reminder_state_file
                .expect("state path")
                .ends_with("state.json")
        );
    }

    #[test]
    fn generated_config_parses_cleanly() {
        let parsed = toml::from_str::<RawConfig>(DEFAULT_CONFIG_TOML).expect("template parses");
        assert!(parsed.paths.expect("paths").events_file.is_some());
        assert!(parsed.keybindings.expect("keybindings").quit.is_some());
    }

    #[test]
    fn microsoft_provider_config_parses_and_resolves_paths() {
        let path = temp_config_path("providers/config.toml");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        fs::create_dir_all(path.parent().expect("config dir")).expect("dir creates");
        fs::write(
            &path,
            r#"
[providers]
create_target = "microsoft"

[providers.microsoft]
enabled = true
default_account = "work"
default_calendar = "cal-1"
sync_past_days = 7
sync_future_days = 90
cache_file = "microsoft-cache.json"

[[providers.microsoft.accounts]]
id = "work"
client_id = "client-id"
tenant = "organizations"
redirect_port = 9001
calendars = ["cal-1"]
"#,
        )
        .expect("config writes");

        let config = load_config_file(&path).expect("config loads");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));

        assert_eq!(
            config.providers.create_target,
            ProviderCreateTarget::Microsoft
        );
        assert!(config.providers.microsoft.enabled);
        assert_eq!(
            config.providers.microsoft.cache_file,
            path.parent()
                .expect("config dir")
                .join("microsoft-cache.json")
        );
        assert_eq!(config.providers.microsoft.sync_past_days, 7);
        assert_eq!(config.providers.microsoft.sync_future_days, 90);
        assert_eq!(config.providers.microsoft.accounts[0].redirect_port, 9001);
    }

    #[test]
    fn invalid_microsoft_provider_config_fails_clearly() {
        let path = temp_config_path("providers-invalid/config.toml");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        fs::create_dir_all(path.parent().expect("config dir")).expect("dir creates");
        fs::write(
            &path,
            r#"
[providers.microsoft]
enabled = true
default_account = "work"
default_calendar = "missing"

[[providers.microsoft.accounts]]
id = "work"
client_id = "client-id"
tenant = "organizations"
calendars = ["cal-1"]
"#,
        )
        .expect("config writes");

        let err = load_config_file(&path).expect_err("invalid provider config fails");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));

        assert!(err.to_string().contains("default calendar"));
    }

    #[test]
    fn malformed_and_unknown_config_fail_clearly() {
        assert!(toml::from_str::<RawConfig>("not = [").is_err());
        assert!(toml::from_str::<RawConfig>("[unknown]\nvalue = true\n").is_err());
    }

    #[test]
    fn config_init_refuses_to_overwrite_without_force() {
        let path = temp_config_path("init/config.toml");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        let created = init_config_file(Some(path.clone()), false).expect("config initializes");
        assert_eq!(created, path);
        let err = init_config_file(Some(path.clone()), false).expect_err("overwrite fails");
        init_config_file(Some(path.clone()), true).expect("force overwrites");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        assert!(matches!(err, ConfigError::AlreadyExists { .. }));
    }

    #[test]
    fn invalid_keybindings_fail() {
        let path = temp_config_path("keys/config.toml");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        fs::create_dir_all(path.parent().expect("config dir")).expect("dir creates");
        fs::write(
            &path,
            r#"
[keybindings]
create_event = ["1"]
"#,
        )
        .expect("config writes");

        let err = load_config_file(&path).expect_err("digit key fails");
        let _ = fs::remove_dir_all(path.parent().and_then(Path::parent).expect("test root"));
        assert!(err.to_string().contains("reserved for day jumps"));
    }
}
