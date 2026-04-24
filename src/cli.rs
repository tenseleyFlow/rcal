use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use time::{Date, OffsetDateTime, format_description};

use crate::{
    agenda::{ConfiguredAgendaSource, HolidayProvider, LocalEventStoreError, default_events_file},
    app::{
        AppState, CreateEventInputResult, EventCopyInputResult, EventCopySubmission,
        EventDeleteInputResult, EventDeleteSubmission, EventFormMode, HelpInputResult, KeyBindings,
        KeyboardInput, MouseInput, RecurrenceChoiceInputResult,
    },
    calendar::CalendarDate,
    config::{
        ConfigError, ConfigHolidaySource, MicrosoftSetupConfig, UserConfig, default_config_file,
        init_config_file, load_discovered_config, load_explicit_config,
        write_microsoft_setup_config,
    },
    providers::{
        KeyringMicrosoftTokenStore, MicrosoftAccountConfig, MicrosoftCalendarInfo,
        MicrosoftProviderConfig, MicrosoftProviderRuntime, ProviderConfig, ProviderError,
        ReqwestMicrosoftHttpClient, inspect_token, list_calendars, login_device_code_or_browser,
        logout,
    },
    reminders::{
        ReminderDaemonConfig, ReminderError, SystemNotifier, default_state_file,
        notification_backend_name, run_daemon, run_once, test_notification,
    },
    services::{
        ServiceConfig, ServiceError, SystemCommandRunner, install_service, service_status,
        uninstall_service,
    },
    tui::{
        AppView, DEFAULT_RENDER_HEIGHT, DEFAULT_RENDER_WIDTH, hit_test_app_date,
        render_app_to_string_with_agenda_source_and_keybindings,
    },
};

const HELP: &str = concat!(
    "rcal ",
    env!("CARGO_PKG_VERSION"),
    "\n\n",
    "Usage:\n",
    "  rcal [--config PATH|--no-config] [--date YYYY-MM-DD] [--events-file PATH] [--holiday-source off|us-federal|nager] [--holiday-country CC]\n",
    "  rcal config init [--path PATH] [--force]\n\n",
    "  rcal providers microsoft auth login --account ID [--browser]\n",
    "  rcal providers microsoft auth logout --account ID\n",
    "  rcal providers microsoft auth inspect --account ID\n",
    "  rcal providers microsoft calendars list --account ID\n",
    "  rcal providers microsoft setup --account ID [--browser] [--calendar ID]\n",
    "  rcal providers microsoft sync [--account ID]\n",
    "  rcal providers microsoft status\n\n",
    "  rcal reminders run [--events-file PATH] [--state-file PATH] [--once]\n",
    "  rcal reminders install [--events-file PATH] [--state-file PATH]\n",
    "  rcal reminders uninstall\n",
    "  rcal reminders status\n",
    "  rcal reminders test [--verbose]\n\n",
    "Options:\n",
    "  --config PATH                       Load a specific config file.\n",
    "  --no-config                         Ignore any discovered config file.\n",
    "  --date YYYY-MM-DD                   Open with the given date selected.\n",
    "  --events-file PATH                  Read and write local user events at PATH.\n",
    "  --holiday-source off|us-federal|nager\n",
    "                                      Choose holiday data. Default: us-federal.\n",
    "  --holiday-country CC                Country code for --holiday-source nager. Default: US.\n",
    "  -h, --help                          Show this help.\n",
    "  -V, --version                       Show version.\n\n",
    "Config:\n",
    "  rcal config init                     Write a commented starter TOML config.\n",
    "  Config is discovered at $XDG_CONFIG_HOME/rcal/config.toml, else ~/.config/rcal/config.toml.\n",
    "  CLI flags override config values. Reminder services snapshot resolved paths at install time.\n\n",
    "Keys:\n",
    "  Arrow keys move selection; Enter opens day view; Esc returns to month; q exits.\n",
    "  ? opens contextual help.\n",
    "  + opens the Create event modal.\n",
    "  In day view, c opens the Copy confirmation for the selected editable event.\n",
    "  In day view, d opens the Delete confirmation for the selected editable event.\n",
    "  In day view, Left/Right move to the previous or next day.\n",
    "  Digits jump immediately; a quick second digit refines the selected day.\n",
    "  Weekday initials jump within the selected week.\n\n",
    "Mouse:\n",
    "  Left click selects a visible date; double-click a visible date to open day view.\n\n",
    "Notes:\n",
    "  Microsoft provider data is cache-first. Run `rcal providers microsoft sync` to refresh it.\n",
);

const VERSION: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"), "\n");
const DIGIT_JUMP_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub start_date: CalendarDate,
    pub events_file: PathBuf,
    pub holiday_source: HolidaySourceConfig,
    pub holiday_country: String,
    pub keybindings: KeyBindings,
    pub providers: ProviderConfig,
}

impl AppConfig {
    pub fn new(start_date: CalendarDate) -> Self {
        Self {
            start_date,
            events_file: default_events_file(),
            holiday_source: HolidaySourceConfig::UsFederal,
            holiday_country: "US".to_string(),
            keybindings: KeyBindings::default(),
            providers: ProviderConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolidaySourceConfig {
    Off,
    UsFederal,
    Nager,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Run(AppConfig),
    Reminders(ReminderCliAction),
    Config(ConfigCliAction),
    Providers(ProviderCliAction),
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigCliAction {
    Init { path: Option<PathBuf>, force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCliAction {
    Microsoft(MicrosoftCliAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicrosoftCliAction {
    Setup {
        account: String,
        browser: bool,
        calendar: Option<String>,
        config_path: PathBuf,
        config: MicrosoftProviderConfig,
    },
    AuthLogin {
        account: String,
        browser: bool,
        config: MicrosoftProviderConfig,
    },
    AuthLogout {
        account: String,
    },
    AuthInspect {
        account: String,
    },
    CalendarsList {
        account: String,
        config: MicrosoftProviderConfig,
    },
    Sync {
        account: Option<String>,
        config: MicrosoftProviderConfig,
    },
    Status {
        config: MicrosoftProviderConfig,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderCliAction {
    Run(ReminderRunConfig),
    Install {
        events_file: PathBuf,
        state_file: PathBuf,
    },
    Uninstall,
    Status,
    Test {
        verbose: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderRunConfig {
    pub events_file: PathBuf,
    pub state_file: PathBuf,
    pub providers: ProviderConfig,
    pub once: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    DuplicateDate,
    MissingDateValue,
    DuplicateEventsFile,
    MissingEventsFileValue,
    DuplicateHolidaySource,
    MissingHolidaySourceValue,
    InvalidHolidaySource(String),
    DuplicateHolidayCountry,
    MissingHolidayCountryValue,
    InvalidHolidayCountry(String),
    HolidayCountryRequiresNager,
    DuplicateConfig,
    MissingConfigValue,
    ConfigAndNoConfig,
    MissingConfigCommand,
    UnknownConfigCommand(String),
    DuplicateConfigInitPath,
    MissingConfigInitPathValue,
    Config(ConfigError),
    Provider(ProviderError),
    MissingProviderCommand,
    UnknownProviderCommand(String),
    MissingProviderAccount,
    DuplicateProviderAccount,
    MissingProviderCalendar,
    DuplicateProviderCalendar,
    MissingReminderCommand,
    UnknownReminderCommand(String),
    DuplicateStateFile,
    MissingStateFileValue,
    UnknownArgument(String),
    InvalidDate { input: String, reason: String },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDate => write!(f, "--date may only be provided once"),
            Self::MissingDateValue => write!(f, "--date requires a value in YYYY-MM-DD format"),
            Self::DuplicateEventsFile => write!(f, "--events-file may only be provided once"),
            Self::MissingEventsFileValue => write!(f, "--events-file requires a path"),
            Self::DuplicateHolidaySource => write!(f, "--holiday-source may only be provided once"),
            Self::MissingHolidaySourceValue => write!(
                f,
                "--holiday-source requires one of: off, us-federal, nager"
            ),
            Self::InvalidHolidaySource(value) => write!(
                f,
                "invalid --holiday-source value '{value}'; expected off, us-federal, or nager"
            ),
            Self::DuplicateHolidayCountry => {
                write!(f, "--holiday-country may only be provided once")
            }
            Self::MissingHolidayCountryValue => {
                write!(f, "--holiday-country requires a two-letter country code")
            }
            Self::InvalidHolidayCountry(value) => {
                write!(
                    f,
                    "invalid --holiday-country value '{value}'; expected two ASCII letters"
                )
            }
            Self::HolidayCountryRequiresNager => {
                write!(
                    f,
                    "--holiday-country may only be used with --holiday-source nager"
                )
            }
            Self::DuplicateConfig => write!(f, "--config may only be provided once"),
            Self::MissingConfigValue => write!(f, "--config requires a path"),
            Self::ConfigAndNoConfig => write!(f, "--config and --no-config cannot be combined"),
            Self::MissingConfigCommand => write!(f, "config requires a command: init"),
            Self::UnknownConfigCommand(command) => write!(f, "unknown config command: {command}"),
            Self::DuplicateConfigInitPath => {
                write!(f, "config init --path may only be provided once")
            }
            Self::MissingConfigInitPathValue => write!(f, "config init --path requires a path"),
            Self::Config(err) => write!(f, "{err}"),
            Self::Provider(err) => write!(f, "{err}"),
            Self::MissingProviderCommand => write!(f, "providers requires a command: microsoft"),
            Self::UnknownProviderCommand(command) => {
                write!(f, "unknown providers command: {command}")
            }
            Self::MissingProviderAccount => write!(f, "--account requires a Microsoft account id"),
            Self::DuplicateProviderAccount => write!(f, "--account may only be provided once"),
            Self::MissingProviderCalendar => {
                write!(f, "--calendar requires a Microsoft calendar id")
            }
            Self::DuplicateProviderCalendar => write!(f, "--calendar may only be provided once"),
            Self::MissingReminderCommand => write!(
                f,
                "reminders requires one of: run, install, uninstall, status, test"
            ),
            Self::UnknownReminderCommand(command) => {
                write!(f, "unknown reminders command: {command}")
            }
            Self::DuplicateStateFile => write!(f, "--state-file may only be provided once"),
            Self::MissingStateFileValue => write!(f, "--state-file requires a path"),
            Self::UnknownArgument(arg) => write!(f, "unknown argument: {arg}"),
            Self::InvalidDate { input, reason } => {
                write!(f, "invalid --date value '{input}': {reason}")
            }
        }
    }
}

impl std::error::Error for CliError {}

impl From<ConfigError> for CliError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
    }
}

impl From<ProviderError> for CliError {
    fn from(err: ProviderError) -> Self {
        Self::Provider(err)
    }
}

pub fn run_terminal<I>(args: I) -> std::process::ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let stdout = io::stdout();
    let stderr = io::stderr();

    if stdout.is_terminal() {
        run_styled_terminal(args, stdout, stderr)
    } else {
        run(args, stdout, stderr)
    }
}

pub fn run<I, W, E>(args: I, mut stdout: W, mut stderr: E) -> std::process::ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    match parse_runtime_args(args, default_start_date()) {
        Ok(CliAction::Run(config)) => {
            let app = AppState::new(config.start_date);
            let agenda_source = match agenda_source(&config) {
                Ok(source) => source,
                Err(err) => return local_event_error_exit(&mut stderr, err),
            };
            let (width, height) = terminal_size();
            let rendered = render_app_to_string_with_agenda_source_and_keybindings(
                &app,
                width,
                height,
                &agenda_source,
                &config.keybindings,
            );
            match write!(stdout, "{rendered}") {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
        Ok(CliAction::Reminders(action)) => run_reminder_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Config(action)) => run_config_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Providers(action)) => run_provider_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Help) => match write!(stdout, "{HELP}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Ok(CliAction::Version) => match write!(stdout, "{VERSION}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}\n\n{HELP}");
            std::process::ExitCode::from(2)
        }
    }
}

fn run_styled_terminal<I, W, E>(args: I, mut stdout: W, mut stderr: E) -> std::process::ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    match parse_runtime_args(args, default_start_date()) {
        Ok(CliAction::Run(config)) => {
            let app = AppState::new(config.start_date);
            let agenda_source = match agenda_source(&config) {
                Ok(source) => source,
                Err(err) => return local_event_error_exit(&mut stderr, err),
            };
            match run_interactive_terminal(stdout, app, agenda_source, config.keybindings) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
        Ok(CliAction::Reminders(action)) => run_reminder_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Config(action)) => run_config_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Providers(action)) => run_provider_action(action, &mut stdout, &mut stderr),
        Ok(CliAction::Help) => match write!(stdout, "{HELP}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Ok(CliAction::Version) => match write!(stdout, "{VERSION}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}\n\n{HELP}");
            std::process::ExitCode::from(2)
        }
    }
}

pub fn parse_args<I>(args: I, today: Date) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    parse_args_with_config(args, today, UserConfig::empty(), None)
}

fn parse_runtime_args<I>(args: I, today: Date) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let (args, config_selection) = strip_config_flags(args)?;

    if let Some(action) = early_static_action(&args) {
        return Ok(action);
    }

    if let Some(first) = args.first()
        && first == "config"
    {
        return parse_config_args(args.into_iter().skip(1), config_selection.path);
    }

    let is_setup = is_microsoft_setup_command(&args);
    let config = match &config_selection {
        ConfigSelection {
            no_config: true, ..
        } => UserConfig::empty(),
        ConfigSelection {
            path: Some(path), ..
        } if is_setup => match load_explicit_config(path.clone()) {
            Ok(config) => config,
            Err(ConfigError::Missing { .. }) => UserConfig::empty(),
            Err(err) => return Err(err.into()),
        },
        ConfigSelection {
            path: Some(path), ..
        } => load_explicit_config(path.clone())?,
        ConfigSelection { .. } => load_discovered_config()?,
    };

    parse_args_with_config(args, today, config, config_selection.path)
}

fn is_microsoft_setup_command(args: &[OsString]) -> bool {
    matches!(
        (args.first(), args.get(1), args.get(2)),
        (Some(first), Some(second), Some(third))
            if first == "providers" && second == "microsoft" && third == "setup"
    )
}

fn parse_args_with_config<I>(
    args: I,
    today: Date,
    user_config: UserConfig,
    explicit_config_path: Option<PathBuf>,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    if let Some(first) = args.first()
        && first == "config"
    {
        return parse_config_args(args.into_iter().skip(1), explicit_config_path);
    }

    if let Some(first) = args.first()
        && first == "reminders"
    {
        return parse_reminder_args(args.into_iter().skip(1), &user_config);
    }

    if let Some(first) = args.first()
        && first == "providers"
    {
        let config_path = explicit_config_path
            .or_else(|| user_config.path.clone())
            .unwrap_or_else(default_config_file);
        return parse_provider_args(args.into_iter().skip(1), &user_config, config_path);
    }

    parse_calendar_args(args, today, user_config)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigSelection {
    path: Option<PathBuf>,
    no_config: bool,
}

fn strip_config_flags(args: Vec<OsString>) -> Result<(Vec<OsString>, ConfigSelection), CliError> {
    let mut stripped = Vec::new();
    let mut config_path = None;
    let mut no_config = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--config" {
            if config_path.is_some() {
                return Err(CliError::DuplicateConfig);
            }
            config_path = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingConfigValue)?,
            ));
            continue;
        }

        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--config="))
        {
            if config_path.is_some() {
                return Err(CliError::DuplicateConfig);
            }
            config_path = Some(PathBuf::from(value));
            continue;
        }

        if arg == "--no-config" {
            no_config = true;
            continue;
        }

        stripped.push(arg);
    }

    if config_path.is_some() && no_config {
        return Err(CliError::ConfigAndNoConfig);
    }

    Ok((
        stripped,
        ConfigSelection {
            path: config_path,
            no_config,
        },
    ))
}

fn early_static_action(args: &[OsString]) -> Option<CliAction> {
    let first = args.first()?;
    if first == "--help" || first == "-h" {
        return Some(CliAction::Help);
    }
    if first == "--version" || first == "-V" {
        return Some(CliAction::Version);
    }
    if first == "reminders"
        && let Some(second) = args.get(1)
        && (second == "--help" || second == "-h")
    {
        return Some(CliAction::Help);
    }
    if first == "config"
        && let Some(second) = args.get(1)
        && (second == "--help" || second == "-h")
    {
        return Some(CliAction::Help);
    }
    if first == "providers"
        && let Some(second) = args.get(1)
        && (second == "--help" || second == "-h")
    {
        return Some(CliAction::Help);
    }

    None
}

fn parse_calendar_args<I>(
    args: I,
    today: Date,
    user_config: UserConfig,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut start_date = None;
    let mut events_file = None;
    let mut holiday_source = None;
    let mut holiday_country = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(CliAction::Help);
        }

        if arg == "--version" || arg == "-V" {
            return Ok(CliAction::Version);
        }

        if arg == "--date" {
            if start_date.is_some() {
                return Err(CliError::DuplicateDate);
            }

            let value = args.next().ok_or(CliError::MissingDateValue)?;
            start_date = Some(parse_date_arg(&value)?);
            continue;
        }

        if let Some(value) = arg.to_str().and_then(|value| value.strip_prefix("--date=")) {
            if start_date.is_some() {
                return Err(CliError::DuplicateDate);
            }

            start_date = Some(parse_date_str(value)?);
            continue;
        }

        if arg == "--events-file" {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }

            let value = args.next().ok_or(CliError::MissingEventsFileValue)?;
            events_file = Some(PathBuf::from(value));
            continue;
        }

        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--events-file="))
        {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }

            events_file = Some(PathBuf::from(value));
            continue;
        }

        if arg == "--holiday-source" {
            if holiday_source.is_some() {
                return Err(CliError::DuplicateHolidaySource);
            }

            let value = args.next().ok_or(CliError::MissingHolidaySourceValue)?;
            holiday_source = Some(parse_holiday_source_arg(&value)?);
            continue;
        }

        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--holiday-source="))
        {
            if holiday_source.is_some() {
                return Err(CliError::DuplicateHolidaySource);
            }

            holiday_source = Some(parse_holiday_source_str(value)?);
            continue;
        }

        if arg == "--holiday-country" {
            if holiday_country.is_some() {
                return Err(CliError::DuplicateHolidayCountry);
            }

            let value = args.next().ok_or(CliError::MissingHolidayCountryValue)?;
            holiday_country = Some(parse_holiday_country_arg(&value)?);
            continue;
        }

        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--holiday-country="))
        {
            if holiday_country.is_some() {
                return Err(CliError::DuplicateHolidayCountry);
            }

            holiday_country = Some(parse_holiday_country_str(value)?);
            continue;
        }

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    let cli_holiday_country_was_provided = holiday_country.is_some();
    let mut config = AppConfig::new(CalendarDate::from(start_date.unwrap_or(today)));
    if let Some(events_file) = user_config.events_file {
        config.events_file = events_file;
    }
    if let Some(holiday_source) = user_config.holiday_source {
        config.holiday_source = holiday_source.into();
    }
    if let Some(holiday_country) = user_config.holiday_country {
        config.holiday_country = holiday_country;
    }
    config.keybindings = user_config.keybindings;
    config.providers = user_config.providers;

    if let Some(events_file) = events_file {
        config.events_file = events_file;
    }
    if let Some(holiday_source) = holiday_source {
        config.holiday_source = holiday_source;
    }
    if let Some(holiday_country) = holiday_country {
        config.holiday_country = holiday_country;
    }
    if cli_holiday_country_was_provided && config.holiday_source != HolidaySourceConfig::Nager {
        return Err(CliError::HolidayCountryRequiresNager);
    }

    Ok(CliAction::Run(config))
}

impl From<ConfigHolidaySource> for HolidaySourceConfig {
    fn from(value: ConfigHolidaySource) -> Self {
        match value {
            ConfigHolidaySource::Off => Self::Off,
            ConfigHolidaySource::UsFederal => Self::UsFederal,
            ConfigHolidaySource::Nager => Self::Nager,
        }
    }
}

fn parse_config_args<I>(
    args: I,
    explicit_config_path: Option<PathBuf>,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingConfigCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownConfigCommand(display_arg(&command)));
    };

    match command {
        "init" => parse_config_init_args(args, explicit_config_path),
        "--help" | "-h" => Ok(CliAction::Help),
        _ => Err(CliError::UnknownConfigCommand(command.to_string())),
    }
}

fn parse_config_init_args<I>(
    args: I,
    explicit_config_path: Option<PathBuf>,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut path = explicit_config_path;
    let mut force = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--path" {
            if path.is_some() {
                return Err(CliError::DuplicateConfigInitPath);
            }
            path = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingConfigInitPathValue)?,
            ));
            continue;
        }

        if let Some(value) = arg.to_str().and_then(|value| value.strip_prefix("--path=")) {
            if path.is_some() {
                return Err(CliError::DuplicateConfigInitPath);
            }
            path = Some(PathBuf::from(value));
            continue;
        }

        if arg == "--force" {
            force = true;
            continue;
        }

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Config(ConfigCliAction::Init { path, force }))
}

fn parse_provider_args<I>(
    args: I,
    user_config: &UserConfig,
    config_path: PathBuf,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingProviderCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownProviderCommand(display_arg(&command)));
    };

    match command {
        "microsoft" => {
            parse_microsoft_provider_args(args, &user_config.providers.microsoft, config_path)
        }
        "--help" | "-h" => Ok(CliAction::Help),
        _ => Err(CliError::UnknownProviderCommand(command.to_string())),
    }
}

fn parse_microsoft_provider_args<I>(
    args: I,
    config: &MicrosoftProviderConfig,
    config_path: PathBuf,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingProviderCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownProviderCommand(display_arg(&command)));
    };

    match command {
        "auth" => parse_microsoft_auth_args(args, config),
        "calendars" => parse_microsoft_calendars_args(args, config),
        "setup" => parse_microsoft_setup_args(args, config, config_path),
        "sync" => parse_microsoft_sync_args(args, config),
        "status" => no_extra_provider_args(
            args,
            MicrosoftCliAction::Status {
                config: config.clone(),
            },
        ),
        "--help" | "-h" => Ok(CliAction::Help),
        _ => Err(CliError::UnknownProviderCommand(format!(
            "microsoft {command}"
        ))),
    }
}

fn parse_microsoft_auth_args<I>(
    args: I,
    config: &MicrosoftProviderConfig,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingProviderCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownProviderCommand(display_arg(&command)));
    };

    match command {
        "login" => {
            let (account, browser) = parse_account_and_browser(args)?;
            Ok(CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::AuthLogin {
                    account,
                    browser,
                    config: config.clone(),
                },
            )))
        }
        "logout" => {
            let account = parse_required_account(args)?;
            Ok(CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::AuthLogout { account },
            )))
        }
        "inspect" => {
            let account = parse_required_account(args)?;
            Ok(CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::AuthInspect { account },
            )))
        }
        _ => Err(CliError::UnknownProviderCommand(format!(
            "microsoft auth {command}"
        ))),
    }
}

fn parse_microsoft_setup_args<I>(
    args: I,
    config: &MicrosoftProviderConfig,
    config_path: PathBuf,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut browser = false;
    let mut account = None;
    let mut calendar = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--browser" {
            browser = true;
            continue;
        }
        if arg == "--calendar" {
            if calendar.is_some() {
                return Err(CliError::DuplicateProviderCalendar);
            }
            calendar = Some(display_arg(
                &args.next().ok_or(CliError::MissingProviderCalendar)?,
            ));
            continue;
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--calendar="))
        {
            if calendar.is_some() {
                return Err(CliError::DuplicateProviderCalendar);
            }
            calendar = Some(value.to_string());
            continue;
        }
        parse_account_arg(arg, &mut args, &mut account)?;
    }

    Ok(CliAction::Providers(ProviderCliAction::Microsoft(
        MicrosoftCliAction::Setup {
            account: account.ok_or(CliError::MissingProviderAccount)?,
            browser,
            calendar,
            config_path,
            config: config.clone(),
        },
    )))
}

fn parse_microsoft_calendars_args<I>(
    args: I,
    config: &MicrosoftProviderConfig,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingProviderCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownProviderCommand(display_arg(&command)));
    };
    match command {
        "list" => {
            let account = parse_required_account(args)?;
            Ok(CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::CalendarsList {
                    account,
                    config: config.clone(),
                },
            )))
        }
        _ => Err(CliError::UnknownProviderCommand(format!(
            "microsoft calendars {command}"
        ))),
    }
}

fn parse_microsoft_sync_args<I>(
    args: I,
    config: &MicrosoftProviderConfig,
) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let account = parse_optional_account(args)?;
    Ok(CliAction::Providers(ProviderCliAction::Microsoft(
        MicrosoftCliAction::Sync {
            account,
            config: config.clone(),
        },
    )))
}

fn no_extra_provider_args<I>(args: I, action: MicrosoftCliAction) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    if let Some(arg) = args.next() {
        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }
    Ok(CliAction::Providers(ProviderCliAction::Microsoft(action)))
}

fn parse_required_account<I>(args: I) -> Result<String, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    parse_optional_account(args)?.ok_or(CliError::MissingProviderAccount)
}

fn parse_account_and_browser<I>(args: I) -> Result<(String, bool), CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut browser = false;
    let mut account = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--browser" {
            browser = true;
            continue;
        }
        parse_account_arg(arg, &mut args, &mut account)?;
    }
    Ok((account.ok_or(CliError::MissingProviderAccount)?, browser))
}

fn parse_optional_account<I>(args: I) -> Result<Option<String>, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut account = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        parse_account_arg(arg, &mut args, &mut account)?;
    }
    Ok(account)
}

fn parse_account_arg<I>(
    arg: OsString,
    args: &mut I,
    account: &mut Option<String>,
) -> Result<(), CliError>
where
    I: Iterator<Item = OsString>,
{
    if arg == "--account" {
        if account.is_some() {
            return Err(CliError::DuplicateProviderAccount);
        }
        *account = Some(display_arg(
            &args.next().ok_or(CliError::MissingProviderAccount)?,
        ));
        return Ok(());
    }
    if let Some(value) = arg
        .to_str()
        .and_then(|value| value.strip_prefix("--account="))
    {
        if account.is_some() {
            return Err(CliError::DuplicateProviderAccount);
        }
        *account = Some(value.to_string());
        return Ok(());
    }
    Err(CliError::UnknownArgument(display_arg(&arg)))
}

fn parse_reminder_args<I>(args: I, user_config: &UserConfig) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let command = args.next().ok_or(CliError::MissingReminderCommand)?;
    let Some(command) = command.to_str() else {
        return Err(CliError::UnknownReminderCommand(display_arg(&command)));
    };

    match command {
        "run" => parse_reminder_run_args(args, user_config),
        "install" => parse_reminder_install_args(args, user_config),
        "uninstall" => no_extra_reminder_args(args, ReminderCliAction::Uninstall),
        "status" => no_extra_reminder_args(args, ReminderCliAction::Status),
        "test" => parse_reminder_test_args(args),
        "--help" | "-h" => Ok(CliAction::Help),
        _ => Err(CliError::UnknownReminderCommand(command.to_string())),
    }
}

fn parse_reminder_run_args<I>(args: I, user_config: &UserConfig) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut events_file = None;
    let mut state_file = None;
    let mut once = false;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--events-file" {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }
            events_file = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingEventsFileValue)?,
            ));
            continue;
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--events-file="))
        {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }
            events_file = Some(PathBuf::from(value));
            continue;
        }
        if arg == "--state-file" {
            if state_file.is_some() {
                return Err(CliError::DuplicateStateFile);
            }
            state_file = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingStateFileValue)?,
            ));
            continue;
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--state-file="))
        {
            if state_file.is_some() {
                return Err(CliError::DuplicateStateFile);
            }
            state_file = Some(PathBuf::from(value));
            continue;
        }
        if arg == "--once" {
            once = true;
            continue;
        }

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Reminders(ReminderCliAction::Run(
        ReminderRunConfig {
            events_file: events_file.unwrap_or_else(|| {
                user_config
                    .events_file
                    .clone()
                    .unwrap_or_else(default_events_file)
            }),
            state_file: state_file.unwrap_or_else(|| {
                user_config
                    .reminder_state_file
                    .clone()
                    .unwrap_or_else(default_state_file)
            }),
            providers: user_config.providers.clone(),
            once,
        },
    )))
}

fn parse_reminder_install_args<I>(args: I, user_config: &UserConfig) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut events_file = None;
    let mut state_file = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--events-file" {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }
            events_file = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingEventsFileValue)?,
            ));
            continue;
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--events-file="))
        {
            if events_file.is_some() {
                return Err(CliError::DuplicateEventsFile);
            }
            events_file = Some(PathBuf::from(value));
            continue;
        }
        if arg == "--state-file" {
            if state_file.is_some() {
                return Err(CliError::DuplicateStateFile);
            }
            state_file = Some(PathBuf::from(
                args.next().ok_or(CliError::MissingStateFileValue)?,
            ));
            continue;
        }
        if let Some(value) = arg
            .to_str()
            .and_then(|value| value.strip_prefix("--state-file="))
        {
            if state_file.is_some() {
                return Err(CliError::DuplicateStateFile);
            }
            state_file = Some(PathBuf::from(value));
            continue;
        }

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Reminders(ReminderCliAction::Install {
        events_file: events_file.unwrap_or_else(|| {
            user_config
                .events_file
                .clone()
                .unwrap_or_else(default_events_file)
        }),
        state_file: state_file.unwrap_or_else(|| {
            user_config
                .reminder_state_file
                .clone()
                .unwrap_or_else(default_state_file)
        }),
    }))
}

fn parse_reminder_test_args<I>(args: I) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut verbose = false;

    for arg in args {
        if arg == "--verbose" {
            verbose = true;
            continue;
        }

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Reminders(ReminderCliAction::Test { verbose }))
}

fn no_extra_reminder_args<I>(args: I, action: ReminderCliAction) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    if let Some(arg) = args.next() {
        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Reminders(action))
}

fn default_start_date() -> Date {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .date()
}

fn terminal_size() -> (u16, u16) {
    terminal::size()
        .ok()
        .filter(|(width, height)| *width > 0 && *height > 0)
        .unwrap_or((DEFAULT_RENDER_WIDTH, DEFAULT_RENDER_HEIGHT))
}

fn agenda_source(config: &AppConfig) -> Result<ConfiguredAgendaSource, LocalEventStoreError> {
    let holidays = match config.holiday_source {
        HolidaySourceConfig::Off => HolidayProvider::off(),
        HolidaySourceConfig::UsFederal => HolidayProvider::us_federal(),
        HolidaySourceConfig::Nager => HolidayProvider::nager(config.holiday_country.clone()),
    };

    ConfiguredAgendaSource::from_events_file(config.events_file.clone(), holidays).and_then(
        |source| {
            source.with_microsoft_provider(
                config.providers.microsoft.clone(),
                config.providers.create_target,
            )
        },
    )
}

fn run_config_action(
    action: ConfigCliAction,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::process::ExitCode {
    match action {
        ConfigCliAction::Init { path, force } => match init_config_file(path, force) {
            Ok(path) => {
                let _ = writeln!(stdout, "wrote config {}", path.display());
                std::process::ExitCode::SUCCESS
            }
            Err(err) => config_error_exit(stderr, err),
        },
    }
}

fn run_provider_action(
    action: ProviderCliAction,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::process::ExitCode {
    match action {
        ProviderCliAction::Microsoft(action) => run_microsoft_action(action, stdout, stderr),
    }
}

fn run_microsoft_action(
    action: MicrosoftCliAction,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::process::ExitCode {
    let http = ReqwestMicrosoftHttpClient;
    let token_store = KeyringMicrosoftTokenStore;
    let result = match action {
        MicrosoftCliAction::Setup {
            account,
            browser,
            calendar,
            config_path,
            config,
        } => (|| {
            let account_config = MicrosoftAccountConfig::new_official(account.clone());
            login_device_code_or_browser(&account_config, &http, &token_store, stdout, browser)?;
            let calendars = list_calendars(&account_config, &http, &token_store)?;
            let calendar = choose_setup_calendar(&calendars, calendar.as_deref())?;
            let setup = MicrosoftSetupConfig {
                account_id: account.clone(),
                calendar_id: calendar.id.clone(),
                calendar_name: calendar.name.clone(),
                sync_past_days: config.sync_past_days,
                sync_future_days: config.sync_future_days,
                redirect_port: account_config.redirect_port,
            };
            let written = write_microsoft_setup_config(Some(config_path), &setup)
                .map_err(|err| ProviderError::Config(err.to_string()))?;
            let _ = writeln!(
                stdout,
                "selected Microsoft calendar '{}' ({})",
                calendar.name, calendar.id
            );
            let _ = writeln!(stdout, "wrote config {}", written.display());
            let setup_config = microsoft_setup_provider_config(config, account_config, calendar);
            let mut runtime = MicrosoftProviderRuntime::load(setup_config)?;
            let summary = runtime.sync(
                Some(&account),
                &http,
                &token_store,
                CalendarDate::from(default_start_date()),
            )?;
            let _ = writeln!(
                stdout,
                "synced accounts={} calendars={} events={}",
                summary.accounts, summary.calendars, summary.events
            );
            Ok(())
        })(),
        MicrosoftCliAction::AuthLogin {
            account,
            browser,
            config,
        } => {
            let Some(account_config) = config.account(&account) else {
                return provider_error_exit(
                    stderr,
                    ProviderError::Config(format!(
                        "Microsoft account '{account}' is not configured"
                    )),
                );
            };
            login_device_code_or_browser(account_config, &http, &token_store, stdout, browser)
        }
        MicrosoftCliAction::AuthLogout { account } => logout(&account, &token_store).map(|()| {
            let _ = writeln!(stdout, "removed Microsoft credentials for '{account}'");
        }),
        MicrosoftCliAction::AuthInspect { account } => {
            inspect_token(&account, &token_store).map(|inspection| {
                let _ = writeln!(
                    stdout,
                    "account={} authenticated=true",
                    inspection.account_id
                );
                let _ = writeln!(stdout, "token_format={}", inspection.token_format);
                let _ = writeln!(
                    stdout,
                    "aud={}",
                    inspection.audience.as_deref().unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "scp={}",
                    inspection.scopes.as_deref().unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "roles={}",
                    if inspection.roles.is_empty() {
                        "<missing>".to_string()
                    } else {
                        inspection.roles.join(",")
                    }
                );
                let _ = writeln!(
                    stdout,
                    "tid={}",
                    inspection.tenant_id.as_deref().unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "iss={}",
                    inspection.issuer.as_deref().unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "appid={}",
                    inspection.app_id.as_deref().unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "azp={}",
                    inspection
                        .authorized_party
                        .as_deref()
                        .unwrap_or("<missing>")
                );
                let _ = writeln!(
                    stdout,
                    "jwt_exp={}",
                    inspection
                        .jwt_expires_at_epoch_seconds
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "<missing>".to_string())
                );
                let _ = writeln!(
                    stdout,
                    "stored_exp={}",
                    inspection.stored_expires_at_epoch_seconds
                );
                let _ = writeln!(stdout, "has_refresh_token={}", inspection.has_refresh_token);
            })
        }
        MicrosoftCliAction::CalendarsList { account, config } => {
            let Some(account_config) = config.account(&account) else {
                return provider_error_exit(
                    stderr,
                    ProviderError::Config(format!(
                        "Microsoft account '{account}' is not configured"
                    )),
                );
            };
            list_calendars(account_config, &http, &token_store).map(|calendars| {
                for calendar in calendars {
                    let _ = writeln!(
                        stdout,
                        "{}\t{}\tcan_edit={}\tdefault={}",
                        calendar.id, calendar.name, calendar.can_edit, calendar.is_default
                    );
                }
            })
        }
        MicrosoftCliAction::Sync { account, config } => {
            let mut runtime = match MicrosoftProviderRuntime::load(config) {
                Ok(runtime) => runtime,
                Err(err) => return provider_error_exit(stderr, err),
            };
            runtime
                .sync(
                    account.as_deref(),
                    &http,
                    &token_store,
                    CalendarDate::from(default_start_date()),
                )
                .map(|summary| {
                    let _ = writeln!(
                        stdout,
                        "synced accounts={} calendars={} events={}",
                        summary.accounts, summary.calendars, summary.events
                    );
                })
        }
        MicrosoftCliAction::Status { config } => {
            let runtime = match MicrosoftProviderRuntime::load(config.clone()) {
                Ok(runtime) => runtime,
                Err(err) => return provider_error_exit(stderr, err),
            };
            let status = runtime.status(&token_store);
            let _ = writeln!(
                stdout,
                "enabled={} cache={} cached_events={}",
                status.enabled,
                status.cache_file.display(),
                status.event_count
            );
            for account in status.accounts {
                let _ = writeln!(
                    stdout,
                    "account={} authenticated={} calendars={}",
                    account.id,
                    account.authenticated,
                    account.calendars.join(",")
                );
            }
            Ok(())
        }
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => provider_error_exit(stderr, err),
    }
}

fn choose_setup_calendar<'a>(
    calendars: &'a [MicrosoftCalendarInfo],
    requested: Option<&str>,
) -> Result<&'a MicrosoftCalendarInfo, ProviderError> {
    if let Some(requested) = requested {
        let calendar = calendars
            .iter()
            .find(|calendar| calendar.id == requested)
            .ok_or_else(|| {
                ProviderError::Config(format!(
                    "Microsoft calendar '{requested}' was not found for this account"
                ))
            })?;
        if !calendar.can_edit {
            return Err(ProviderError::Config(format!(
                "Microsoft calendar '{}' is read-only; choose an editable calendar",
                calendar.name
            )));
        }
        return Ok(calendar);
    }

    calendars
        .iter()
        .find(|calendar| calendar.can_edit && calendar.is_default)
        .or_else(|| calendars.iter().find(|calendar| calendar.can_edit))
        .ok_or_else(|| {
            ProviderError::Config(
                "no editable Microsoft calendars were found for this account".to_string(),
            )
        })
}

fn microsoft_setup_provider_config(
    mut config: MicrosoftProviderConfig,
    mut account: MicrosoftAccountConfig,
    calendar: &MicrosoftCalendarInfo,
) -> MicrosoftProviderConfig {
    config.enabled = true;
    config.default_account = Some(account.id.clone());
    config.default_calendar = Some(calendar.id.clone());
    account.calendars = vec![calendar.id.clone()];

    if let Some(existing) = config
        .accounts
        .iter_mut()
        .find(|existing| existing.id == account.id)
    {
        *existing = account;
    } else {
        config.accounts.push(account);
    }

    config
}

fn run_reminder_action(
    action: ReminderCliAction,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::process::ExitCode {
    match action {
        ReminderCliAction::Run(config) => {
            let daemon_config = ReminderDaemonConfig::new(config.events_file, config.state_file)
                .with_providers(config.providers);
            let mut notifier = SystemNotifier;
            if config.once {
                match run_once(
                    &daemon_config,
                    crate::reminders::current_local_datetime(),
                    &mut notifier,
                ) {
                    Ok(summary) => {
                        let _ = writeln!(
                            stdout,
                            "delivered={} skipped={} failed={}",
                            summary.delivered, summary.skipped, summary.failed
                        );
                        std::process::ExitCode::SUCCESS
                    }
                    Err(err) => reminder_error_exit(stderr, err),
                }
            } else {
                match run_daemon(daemon_config, &mut notifier) {
                    Ok(()) => std::process::ExitCode::SUCCESS,
                    Err(err) => reminder_error_exit(stderr, err),
                }
            }
        }
        ReminderCliAction::Install {
            events_file,
            state_file,
        } => {
            let config = match ServiceConfig::new(events_file, state_file) {
                Ok(config) => config,
                Err(err) => return service_error_exit(stderr, err),
            };
            let mut runner = SystemCommandRunner;
            match install_service(&config, &mut runner) {
                Ok(()) => {
                    let _ = writeln!(stdout, "installed reminder service");
                    std::process::ExitCode::SUCCESS
                }
                Err(err) => service_error_exit(stderr, err),
            }
        }
        ReminderCliAction::Uninstall => {
            let mut runner = SystemCommandRunner;
            match uninstall_service(&mut runner) {
                Ok(()) => {
                    let _ = writeln!(stdout, "uninstalled reminder service");
                    std::process::ExitCode::SUCCESS
                }
                Err(err) => service_error_exit(stderr, err),
            }
        }
        ReminderCliAction::Status => {
            let mut runner = SystemCommandRunner;
            match service_status(&mut runner) {
                Ok(status) => {
                    let _ = writeln!(stdout, "{status}");
                    std::process::ExitCode::SUCCESS
                }
                Err(err) => service_error_exit(stderr, err),
            }
        }
        ReminderCliAction::Test { verbose } => {
            let mut notifier = SystemNotifier;
            if verbose {
                let _ = writeln!(
                    stdout,
                    "notification_backend={}",
                    notification_backend_name()
                );
            }
            match test_notification(&mut notifier) {
                Ok(()) => {
                    let _ = writeln!(stdout, "sent test reminder notification");
                    std::process::ExitCode::SUCCESS
                }
                Err(err) => reminder_error_exit(stderr, err),
            }
        }
    }
}

fn run_interactive_terminal<W>(
    stdout: W,
    app: AppState,
    agenda_source: ConfiguredAgendaSource,
    keybindings: KeyBindings,
) -> io::Result<()>
where
    W: Write,
{
    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
    };

    if let Err(err) = execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    ) {
        let _ = execute!(
            terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
        return Err(err);
    }

    let result = run_event_loop(&mut terminal, app, agenda_source, keybindings);
    let cleanup_result = restore_terminal(&mut terminal);

    result.and(cleanup_result)
}

fn run_event_loop<W>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    mut app: AppState,
    mut agenda_source: ConfiguredAgendaSource,
    keybindings: KeyBindings,
) -> io::Result<()>
where
    W: Write,
{
    let mut keyboard = KeyboardInput::new(keybindings.clone());
    let mut mouse = MouseInput::default();

    loop {
        terminal.draw(|frame| {
            frame.render_widget(
                AppView::with_agenda_source_and_keybindings(&app, &agenda_source, &keybindings),
                frame.area(),
            );
        })?;

        if app.should_quit() {
            return Ok(());
        }

        let event = if !app.is_creating_event()
            && !app.is_choosing_recurring_edit()
            && !app.is_confirming_delete()
            && !app.is_copying_event()
            && !app.is_showing_help()
            && keyboard.is_waiting_for_digit()
        {
            if event::poll(DIGIT_JUMP_TIMEOUT)? {
                event::read()?
            } else {
                keyboard.clear_digit();
                continue;
            }
        } else {
            event::read()?
        };

        match event {
            Event::Key(key) => {
                mouse.clear();
                if app.is_showing_help() {
                    match app.handle_help_key(key) {
                        HelpInputResult::Continue | HelpInputResult::Close => {}
                    }
                } else if app.is_confirming_delete() {
                    match app.handle_delete_choice_key(key) {
                        EventDeleteInputResult::Continue => {}
                        EventDeleteInputResult::Cancel => app.close_delete_choice(),
                        EventDeleteInputResult::Submit(submission) => match submission {
                            EventDeleteSubmission::Event { event_id }
                            | EventDeleteSubmission::Series {
                                series_id: event_id,
                            } => match agenda_source.delete_event(&event_id) {
                                Ok(_) => {
                                    app.close_delete_choice();
                                    app.reconcile_day_event_selection(&agenda_source);
                                }
                                Err(err) => app.set_delete_error(err.to_string()),
                            },
                            EventDeleteSubmission::Occurrence { series_id, anchor } => {
                                match agenda_source.delete_occurrence(&series_id, anchor) {
                                    Ok(()) => {
                                        app.close_delete_choice();
                                        app.reconcile_day_event_selection(&agenda_source);
                                    }
                                    Err(err) => app.set_delete_error(err.to_string()),
                                }
                            }
                        },
                    }
                } else if app.is_copying_event() {
                    match app.handle_copy_choice_key(key) {
                        EventCopyInputResult::Continue => {}
                        EventCopyInputResult::Cancel => app.close_copy_choice(),
                        EventCopyInputResult::Submit(submission) => {
                            let result = match submission {
                                EventCopySubmission::Event { event_id }
                                | EventCopySubmission::Series {
                                    series_id: event_id,
                                } => agenda_source.duplicate_event(&event_id),
                                EventCopySubmission::Occurrence { series_id, anchor } => {
                                    agenda_source.duplicate_occurrence(&series_id, anchor)
                                }
                            };
                            match result {
                                Ok(event) => {
                                    app.close_copy_choice();
                                    app.select_day_event_id(event.id);
                                    app.reconcile_day_event_selection(&agenda_source);
                                }
                                Err(err) => app.set_copy_error(err.to_string()),
                            }
                        }
                    }
                } else if app.is_choosing_recurring_edit() {
                    match app.handle_recurrence_choice_key(key, &agenda_source) {
                        RecurrenceChoiceInputResult::Continue => {}
                        RecurrenceChoiceInputResult::Cancel => app.close_recurrence_choice(),
                    }
                } else if app.is_creating_event() {
                    match app.handle_create_key(key) {
                        CreateEventInputResult::Continue => {}
                        CreateEventInputResult::Cancel => app.close_create_form(),
                        CreateEventInputResult::Submit(submission) => {
                            let submission = *submission;
                            match submission.mode {
                                EventFormMode::Create => {
                                    match agenda_source.create_event_with_target(
                                        submission.draft,
                                        &submission.target,
                                    ) {
                                        Ok(_) => {
                                            app.close_create_form();
                                            app.reconcile_day_event_selection(&agenda_source);
                                        }
                                        Err(err) => app.set_create_form_error(err.to_string()),
                                    }
                                }
                                EventFormMode::Edit { event_id } => {
                                    match agenda_source.update_event_with_target(
                                        &event_id,
                                        submission.draft,
                                        &submission.target,
                                    ) {
                                        Ok(_) => {
                                            app.close_create_form();
                                            app.reconcile_day_event_selection(&agenda_source);
                                        }
                                        Err(err) => app.set_create_form_error(err.to_string()),
                                    }
                                }
                                EventFormMode::EditOccurrence { series_id, anchor } => {
                                    match agenda_source.update_occurrence(
                                        &series_id,
                                        anchor,
                                        submission.draft,
                                    ) {
                                        Ok(_) => {
                                            app.close_create_form();
                                            app.reconcile_day_event_selection(&agenda_source);
                                        }
                                        Err(err) => app.set_create_form_error(err.to_string()),
                                    }
                                }
                            }
                        }
                    }
                } else {
                    let action = keyboard.translate(key);
                    app.apply_with_agenda_source(action, &agenda_source);
                }
            }
            Event::Mouse(mouse_event) => {
                if app.is_creating_event()
                    || app.is_choosing_recurring_edit()
                    || app.is_confirming_delete()
                    || app.is_copying_event()
                    || app.is_showing_help()
                {
                    continue;
                }
                keyboard.clear();
                let size = terminal.size()?;
                let area = Rect::new(0, 0, size.width, size.height);
                let target_date =
                    hit_test_app_date(&app, area, mouse_event.column, mouse_event.row);
                let action = mouse.translate(mouse_event, target_date, app.selected_date());
                app.apply_with_agenda_source(action, &agenda_source);
            }
            Event::Resize(_, _) => {
                keyboard.clear();
                mouse.clear();
            }
            _ => {}
        }
    }
}

fn restore_terminal<W>(terminal: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()>
where
    W: Write,
{
    let raw_result = terminal::disable_raw_mode();
    let screen_result = execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let cursor_result = terminal.show_cursor();

    raw_result?;
    screen_result?;
    cursor_result?;
    Ok(())
}

fn parse_date_arg(value: &OsStr) -> Result<Date, CliError> {
    let value = value.to_str().ok_or_else(|| CliError::InvalidDate {
        input: display_arg(value),
        reason: "date must be valid UTF-8".to_string(),
    })?;

    parse_date_str(value)
}

fn parse_holiday_source_arg(value: &OsStr) -> Result<HolidaySourceConfig, CliError> {
    let value = value
        .to_str()
        .ok_or_else(|| CliError::InvalidHolidaySource("value must be valid UTF-8".to_string()))?;

    parse_holiday_source_str(value)
}

fn parse_holiday_source_str(value: &str) -> Result<HolidaySourceConfig, CliError> {
    match value {
        "off" => Ok(HolidaySourceConfig::Off),
        "us-federal" => Ok(HolidaySourceConfig::UsFederal),
        "nager" => Ok(HolidaySourceConfig::Nager),
        _ => Err(CliError::InvalidHolidaySource(value.to_string())),
    }
}

fn parse_holiday_country_arg(value: &OsStr) -> Result<String, CliError> {
    let value = value
        .to_str()
        .ok_or_else(|| CliError::InvalidHolidayCountry("value must be valid UTF-8".to_string()))?;

    parse_holiday_country_str(value)
}

fn parse_holiday_country_str(value: &str) -> Result<String, CliError> {
    if value.len() == 2 && value.bytes().all(|value| value.is_ascii_alphabetic()) {
        Ok(value.to_ascii_uppercase())
    } else {
        Err(CliError::InvalidHolidayCountry(value.to_string()))
    }
}

fn parse_date_str(value: &str) -> Result<Date, CliError> {
    let format =
        format_description::parse("[year]-[month]-[day]").map_err(|err| CliError::InvalidDate {
            input: value.to_string(),
            reason: err.to_string(),
        })?;

    Date::parse(value, &format).map_err(|err| CliError::InvalidDate {
        input: value.to_string(),
        reason: err.to_string(),
    })
}

fn display_arg(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn io_error_exit(stderr: &mut impl Write, err: io::Error) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: failed to write output: {err}");
    std::process::ExitCode::FAILURE
}

fn local_event_error_exit(
    stderr: &mut impl Write,
    err: LocalEventStoreError,
) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: failed to load local events: {err}");
    std::process::ExitCode::from(2)
}

fn reminder_error_exit(stderr: &mut impl Write, err: ReminderError) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: {err}");
    std::process::ExitCode::FAILURE
}

fn config_error_exit(stderr: &mut impl Write, err: ConfigError) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: {err}");
    std::process::ExitCode::from(2)
}

fn service_error_exit(stderr: &mut impl Write, err: ServiceError) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: {err}");
    std::process::ExitCode::FAILURE
}

fn provider_error_exit(stderr: &mut impl Write, err: ProviderError) -> std::process::ExitCode {
    let _ = writeln!(stderr, "error: {err}");
    std::process::ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::app::{KeyBindingOverrides, KeyCommand};
    use crate::calendar::CalendarMonth;
    use crate::providers::{MicrosoftAccountConfig, MicrosoftCalendarInfo, ProviderCreateTarget};
    use time::Month;

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn arg(value: &str) -> OsString {
        OsString::from(value)
    }

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir()
            .join(format!("rcal-cli-test-{}-{counter}", std::process::id()))
            .join(name)
    }

    fn config_with_paths_and_keys() -> UserConfig {
        UserConfig {
            path: Some(PathBuf::from("/tmp/rcal/config.toml")),
            events_file: Some(PathBuf::from("/tmp/config-events.json")),
            holiday_source: Some(ConfigHolidaySource::Nager),
            holiday_country: Some("GB".to_string()),
            reminder_state_file: Some(PathBuf::from("/tmp/config-state.json")),
            keybindings: KeyBindings::with_overrides(KeyBindingOverrides {
                create_event: Some(vec!["n".to_string()]),
                help: Some(vec!["h".to_string()]),
                ..KeyBindingOverrides::default()
            })
            .expect("test bindings are valid"),
            providers: ProviderConfig::default(),
        }
    }

    fn microsoft_provider_config() -> MicrosoftProviderConfig {
        MicrosoftProviderConfig {
            enabled: true,
            default_account: Some("work".to_string()),
            default_calendar: Some("cal-1".to_string()),
            sync_past_days: 30,
            sync_future_days: 365,
            cache_file: PathBuf::from("/tmp/microsoft-cache.json"),
            accounts: vec![MicrosoftAccountConfig {
                id: "work".to_string(),
                client_id: "client-id".to_string(),
                tenant: "organizations".to_string(),
                redirect_port: 8765,
                calendars: vec!["cal-1".to_string()],
            }],
        }
    }

    fn config_with_microsoft_provider() -> UserConfig {
        UserConfig {
            providers: ProviderConfig {
                create_target: ProviderCreateTarget::Microsoft,
                microsoft: microsoft_provider_config(),
            },
            ..UserConfig::empty()
        }
    }

    #[test]
    fn no_args_uses_provided_today() {
        let today = date(2026, Month::April, 23);

        let action = parse_args([], today.into()).expect("parse succeeds");

        assert_eq!(action, CliAction::Run(AppConfig::new(today)));
    }

    #[test]
    fn date_flag_sets_start_date() {
        let today = date(2026, Month::April, 23);

        let action =
            parse_args([arg("--date"), arg("2027-01-02")], today.into()).expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig::new(date(2027, Month::January, 2)))
        );
    }

    #[test]
    fn date_equals_form_sets_start_date() {
        let today = date(2026, Month::April, 23);

        let action = parse_args([arg("--date=2027-01-02")], today.into()).expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig::new(date(2027, Month::January, 2)))
        );
    }

    #[test]
    fn holiday_source_flag_sets_provider() {
        let today = date(2026, Month::April, 23);

        let action = parse_args([arg("--holiday-source"), arg("off")], today.into())
            .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: today,
                holiday_source: HolidaySourceConfig::Off,
                holiday_country: "US".to_string(),
                ..AppConfig::new(today)
            })
        );
    }

    #[test]
    fn nager_holiday_source_accepts_country_code() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [
                arg("--holiday-source=nager"),
                arg("--holiday-country"),
                arg("gb"),
            ],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: today,
                holiday_source: HolidaySourceConfig::Nager,
                holiday_country: "GB".to_string(),
                ..AppConfig::new(today)
            })
        );
    }

    #[test]
    fn nager_holiday_country_can_precede_source() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [
                arg("--holiday-country=ca"),
                arg("--holiday-source"),
                arg("nager"),
            ],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: today,
                holiday_source: HolidaySourceConfig::Nager,
                holiday_country: "CA".to_string(),
                ..AppConfig::new(today)
            })
        );
    }

    #[test]
    fn events_file_flag_sets_path() {
        let today = date(2026, Month::April, 23);
        let path = PathBuf::from("/tmp/rcal-test-events.json");

        let action = parse_args(
            [arg("--events-file"), arg("/tmp/rcal-test-events.json")],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: today,
                events_file: path,
                ..AppConfig::new(today)
            })
        );
    }

    #[test]
    fn events_file_options_are_rejected_when_invalid() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_args([arg("--events-file")], today.into()).expect_err("missing path fails"),
            CliError::MissingEventsFileValue
        );
        assert_eq!(
            parse_args(
                [
                    arg("--events-file"),
                    arg("/tmp/one.json"),
                    arg("--events-file=/tmp/two.json"),
                ],
                today.into(),
            )
            .expect_err("duplicate path fails"),
            CliError::DuplicateEventsFile
        );
    }

    #[test]
    fn config_values_merge_under_cli_overrides() {
        let today = date(2026, Month::April, 23);

        let action = parse_args_with_config(
            [
                arg("--events-file"),
                arg("/tmp/cli-events.json"),
                arg("--holiday-country"),
                arg("ca"),
            ],
            today.into(),
            config_with_paths_and_keys(),
            None,
        )
        .expect("parse succeeds");

        let CliAction::Run(config) = action else {
            panic!("calendar args should run the app");
        };

        assert_eq!(config.events_file, PathBuf::from("/tmp/cli-events.json"));
        assert_eq!(config.holiday_source, HolidaySourceConfig::Nager);
        assert_eq!(config.holiday_country, "CA");
        assert_eq!(config.keybindings.display_for(KeyCommand::CreateEvent), "n");
    }

    #[test]
    fn explicit_runtime_config_file_is_loaded() {
        let today = date(2026, Month::April, 23);
        let path = temp_path("explicit/config.toml");
        let root = path
            .parent()
            .expect("config dir")
            .parent()
            .expect("test root")
            .to_path_buf();
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(path.parent().expect("config dir")).expect("dir creates");
        fs::write(
            &path,
            r#"
[paths]
events_file = "events.json"

[keybindings]
create_event = ["n"]
"#,
        )
        .expect("config writes");

        let action = parse_runtime_args(
            [
                arg("--config"),
                path.as_os_str().to_os_string(),
                arg("--date"),
                arg("2026-04-23"),
            ],
            today.into(),
        )
        .expect("runtime parse succeeds");

        let _ = fs::remove_dir_all(&root);
        let CliAction::Run(config) = action else {
            panic!("calendar args should run the app");
        };
        assert_eq!(
            config.events_file,
            path.parent().expect("config dir").join("events.json")
        );
        assert_eq!(config.keybindings.display_for(KeyCommand::CreateEvent), "n");
    }

    #[test]
    fn global_config_flags_reject_conflicts() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_runtime_args(
                [arg("--config"), arg("/tmp/config.toml"), arg("--no-config")],
                today.into(),
            )
            .expect_err("conflicting config flags fail"),
            CliError::ConfigAndNoConfig
        );
    }

    #[test]
    fn config_init_args_parse_path_and_force() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [
                arg("config"),
                arg("init"),
                arg("--path=/tmp/rcal/config.toml"),
                arg("--force"),
            ],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Config(ConfigCliAction::Init {
                path: Some(PathBuf::from("/tmp/rcal/config.toml")),
                force: true,
            })
        );
    }

    #[test]
    fn config_init_can_use_global_config_path_without_loading_it() {
        let today = date(2026, Month::April, 23);

        let action = parse_runtime_args(
            [
                arg("--config"),
                arg("/tmp/nonexistent-rcal-config.toml"),
                arg("config"),
                arg("init"),
                arg("--force"),
            ],
            today.into(),
        )
        .expect("config init parses without loading missing config");

        assert_eq!(
            action,
            CliAction::Config(ConfigCliAction::Init {
                path: Some(PathBuf::from("/tmp/nonexistent-rcal-config.toml")),
                force: true,
            })
        );
    }

    #[test]
    fn reminder_run_args_set_events_and_state_paths() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [
                arg("reminders"),
                arg("run"),
                arg("--events-file"),
                arg("/tmp/events.json"),
                arg("--state-file=/tmp/state.json"),
                arg("--once"),
            ],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Reminders(ReminderCliAction::Run(ReminderRunConfig {
                events_file: PathBuf::from("/tmp/events.json"),
                state_file: PathBuf::from("/tmp/state.json"),
                providers: ProviderConfig::default(),
                once: true,
            }))
        );
    }

    #[test]
    fn reminder_commands_use_config_default_paths() {
        let today = date(2026, Month::April, 23);

        let run_action = parse_args_with_config(
            [arg("reminders"), arg("run"), arg("--once")],
            today.into(),
            config_with_paths_and_keys(),
            None,
        )
        .expect("run parses");
        assert_eq!(
            run_action,
            CliAction::Reminders(ReminderCliAction::Run(ReminderRunConfig {
                events_file: PathBuf::from("/tmp/config-events.json"),
                state_file: PathBuf::from("/tmp/config-state.json"),
                providers: ProviderConfig::default(),
                once: true,
            }))
        );

        let install_action = parse_args_with_config(
            [
                arg("reminders"),
                arg("install"),
                arg("--state-file=/tmp/override-state.json"),
            ],
            today.into(),
            config_with_paths_and_keys(),
            None,
        )
        .expect("install parses");
        assert_eq!(
            install_action,
            CliAction::Reminders(ReminderCliAction::Install {
                events_file: PathBuf::from("/tmp/config-events.json"),
                state_file: PathBuf::from("/tmp/override-state.json"),
            })
        );
    }

    #[test]
    fn reminder_install_args_set_events_path() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [
                arg("reminders"),
                arg("install"),
                arg("--events-file=/tmp/events.json"),
            ],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Reminders(ReminderCliAction::Install {
                events_file: PathBuf::from("/tmp/events.json"),
                state_file: default_state_file(),
            })
        );
    }

    #[test]
    fn reminder_test_accepts_verbose_diagnostic_flag() {
        let today = date(2026, Month::April, 23);

        let action = parse_args(
            [arg("reminders"), arg("test"), arg("--verbose")],
            today.into(),
        )
        .expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Reminders(ReminderCliAction::Test { verbose: true })
        );
    }

    #[test]
    fn reminder_args_are_rejected_when_invalid() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_args([arg("reminders")], today.into()).expect_err("missing command fails"),
            CliError::MissingReminderCommand
        );
        assert_eq!(
            parse_args([arg("reminders"), arg("bogus")], today.into())
                .expect_err("unknown command fails"),
            CliError::UnknownReminderCommand("bogus".to_string())
        );
        assert_eq!(
            parse_args(
                [
                    arg("reminders"),
                    arg("run"),
                    arg("--state-file"),
                    arg("/tmp/one.json"),
                    arg("--state-file=/tmp/two.json"),
                ],
                today.into(),
            )
            .expect_err("duplicate state path fails"),
            CliError::DuplicateStateFile
        );
    }

    #[test]
    fn microsoft_provider_commands_parse_configured_account() {
        let today = date(2026, Month::April, 23);
        let user_config = config_with_microsoft_provider();
        let microsoft = user_config.providers.microsoft.clone();

        let sync_action = parse_args_with_config(
            [
                arg("providers"),
                arg("microsoft"),
                arg("sync"),
                arg("--account"),
                arg("work"),
            ],
            today.into(),
            user_config.clone(),
            None,
        )
        .expect("sync parses");
        assert_eq!(
            sync_action,
            CliAction::Providers(ProviderCliAction::Microsoft(MicrosoftCliAction::Sync {
                account: Some("work".to_string()),
                config: microsoft.clone(),
            }))
        );

        let inspect_action = parse_args_with_config(
            [
                arg("providers"),
                arg("microsoft"),
                arg("auth"),
                arg("inspect"),
                arg("--account"),
                arg("work"),
            ],
            today.into(),
            user_config.clone(),
            None,
        )
        .expect("inspect parses");
        assert_eq!(
            inspect_action,
            CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::AuthInspect {
                    account: "work".to_string(),
                },
            ))
        );

        let login_action = parse_args_with_config(
            [
                arg("providers"),
                arg("microsoft"),
                arg("auth"),
                arg("login"),
                arg("--account=work"),
                arg("--browser"),
            ],
            today.into(),
            user_config,
            None,
        )
        .expect("login parses");
        assert_eq!(
            login_action,
            CliAction::Providers(ProviderCliAction::Microsoft(
                MicrosoftCliAction::AuthLogin {
                    account: "work".to_string(),
                    browser: true,
                    config: microsoft,
                },
            ))
        );
    }

    #[test]
    fn microsoft_setup_command_parses_with_calendar_and_config_path() {
        let today = date(2026, Month::April, 23);
        let user_config = config_with_microsoft_provider();
        let microsoft = user_config.providers.microsoft.clone();
        let config_path = PathBuf::from("/tmp/rcal/setup-config.toml");

        let action = parse_args_with_config(
            [
                arg("providers"),
                arg("microsoft"),
                arg("setup"),
                arg("--account"),
                arg("work"),
                arg("--browser"),
                arg("--calendar=cal-2"),
            ],
            today.into(),
            user_config,
            Some(config_path.clone()),
        )
        .expect("setup parses");

        assert_eq!(
            action,
            CliAction::Providers(ProviderCliAction::Microsoft(MicrosoftCliAction::Setup {
                account: "work".to_string(),
                browser: true,
                calendar: Some("cal-2".to_string()),
                config_path,
                config: microsoft,
            }))
        );
    }

    #[test]
    fn microsoft_setup_allows_missing_explicit_config_file() {
        let today = date(2026, Month::April, 23);
        let path = temp_path("missing-setup/config.toml");
        let root = path
            .parent()
            .expect("config dir")
            .parent()
            .expect("test root")
            .to_path_buf();
        let _ = fs::remove_dir_all(&root);

        let action = parse_runtime_args(
            [
                arg("--config"),
                path.as_os_str().to_os_string(),
                arg("providers"),
                arg("microsoft"),
                arg("setup"),
                arg("--account"),
                arg("work"),
            ],
            today.into(),
        )
        .expect("setup parses without a preexisting config");

        assert_eq!(
            action,
            CliAction::Providers(ProviderCliAction::Microsoft(MicrosoftCliAction::Setup {
                account: "work".to_string(),
                browser: false,
                calendar: None,
                config_path: path,
                config: MicrosoftProviderConfig::default(),
            }))
        );
    }

    #[test]
    fn microsoft_setup_calendar_selection_prefers_editable_default() {
        let calendars = vec![
            MicrosoftCalendarInfo {
                id: "readonly".to_string(),
                name: "Holidays".to_string(),
                can_edit: false,
                is_default: true,
            },
            MicrosoftCalendarInfo {
                id: "cal".to_string(),
                name: "Calendar".to_string(),
                can_edit: true,
                is_default: true,
            },
            MicrosoftCalendarInfo {
                id: "other".to_string(),
                name: "Other".to_string(),
                can_edit: true,
                is_default: false,
            },
        ];

        let calendar =
            choose_setup_calendar(&calendars, None).expect("default editable calendar selected");

        assert_eq!(calendar.id, "cal");
    }

    #[test]
    fn microsoft_setup_calendar_selection_uses_first_editable_fallback() {
        let calendars = vec![
            MicrosoftCalendarInfo {
                id: "readonly".to_string(),
                name: "Holidays".to_string(),
                can_edit: false,
                is_default: true,
            },
            MicrosoftCalendarInfo {
                id: "work".to_string(),
                name: "Work".to_string(),
                can_edit: true,
                is_default: false,
            },
        ];

        let calendar =
            choose_setup_calendar(&calendars, None).expect("first editable calendar selected");

        assert_eq!(calendar.id, "work");
    }

    #[test]
    fn microsoft_setup_calendar_selection_rejects_missing_or_readonly_requested_calendar() {
        let calendars = vec![MicrosoftCalendarInfo {
            id: "readonly".to_string(),
            name: "Holidays".to_string(),
            can_edit: false,
            is_default: false,
        }];

        let missing =
            choose_setup_calendar(&calendars, Some("missing")).expect_err("missing calendar fails");
        let readonly = choose_setup_calendar(&calendars, Some("readonly"))
            .expect_err("readonly calendar fails");

        assert!(missing.to_string().contains("was not found"));
        assert!(readonly.to_string().contains("read-only"));
    }

    #[test]
    fn microsoft_provider_args_reject_missing_and_duplicate_accounts() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_args(
                [
                    arg("providers"),
                    arg("microsoft"),
                    arg("auth"),
                    arg("login")
                ],
                today.into(),
            )
            .expect_err("missing account fails"),
            CliError::MissingProviderAccount
        );
        assert_eq!(
            parse_args(
                [
                    arg("providers"),
                    arg("microsoft"),
                    arg("sync"),
                    arg("--account=one"),
                    arg("--account=two"),
                ],
                today.into(),
            )
            .expect_err("duplicate account fails"),
            CliError::DuplicateProviderAccount
        );
    }

    #[test]
    fn invalid_holiday_options_are_rejected() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_args([arg("--holiday-source"), arg("network")], today.into())
                .expect_err("invalid source fails"),
            CliError::InvalidHolidaySource("network".to_string())
        );
        assert_eq!(
            parse_args([arg("--holiday-country"), arg("USA")], today.into())
                .expect_err("invalid country fails"),
            CliError::InvalidHolidayCountry("USA".to_string())
        );
    }

    #[test]
    fn holiday_country_requires_nager_source() {
        let today = date(2026, Month::April, 23);

        assert_eq!(
            parse_args([arg("--holiday-country"), arg("GB")], today.into())
                .expect_err("country without Nager fails"),
            CliError::HolidayCountryRequiresNager
        );
        assert_eq!(
            parse_args(
                [
                    arg("--holiday-source"),
                    arg("off"),
                    arg("--holiday-country"),
                    arg("GB"),
                ],
                today.into(),
            )
            .expect_err("country with non-Nager source fails"),
            CliError::HolidayCountryRequiresNager
        );
    }

    #[test]
    fn invalid_date_is_rejected() {
        let today = date(2026, Month::April, 23);

        let err = parse_args([arg("--date"), arg("2026-02-30")], today.into())
            .expect_err("invalid dates fail");

        assert!(matches!(err, CliError::InvalidDate { .. }));
    }

    #[test]
    fn missing_date_value_is_rejected() {
        let today = date(2026, Month::April, 23);

        let err = parse_args([arg("--date")], today.into()).expect_err("missing values fail");

        assert_eq!(err, CliError::MissingDateValue);
    }

    #[test]
    fn duplicate_date_is_rejected() {
        let today = date(2026, Month::April, 23);

        let err = parse_args(
            [
                arg("--date"),
                arg("2026-04-23"),
                arg("--date"),
                arg("2026-04-24"),
            ],
            today.into(),
        )
        .expect_err("duplicate dates fail");

        assert_eq!(err, CliError::DuplicateDate);
    }

    #[test]
    fn date_flag_seeds_calendar_month_selection() {
        let today = date(2026, Month::April, 23);

        let action =
            parse_args([arg("--date"), arg("2027-01-02")], today.into()).expect("parse succeeds");
        let CliAction::Run(config) = action else {
            panic!("date flag should produce run config");
        };

        let month = CalendarMonth::for_launch_date(config.start_date);

        assert_eq!(month.current.year, 2027);
        assert_eq!(month.current.month, Month::January);
        assert_eq!(
            month.selected_cell().map(|cell| cell.date),
            Some(date(2027, Month::January, 2))
        );
    }
}
