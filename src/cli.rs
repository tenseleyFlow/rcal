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
    app::{AppState, CreateEventInputResult, KeyboardInput, MouseInput},
    calendar::CalendarDate,
    tui::{
        AppView, DEFAULT_RENDER_HEIGHT, DEFAULT_RENDER_WIDTH, hit_test_app_date,
        render_app_to_string_with_agenda_source,
    },
};

const HELP: &str = concat!(
    "rcal ",
    env!("CARGO_PKG_VERSION"),
    "\n\n",
    "Usage:\n",
    "  rcal [--date YYYY-MM-DD] [--events-file PATH] [--holiday-source off|us-federal|nager] [--holiday-country CC]\n\n",
    "Options:\n",
    "  --date YYYY-MM-DD                   Open with the given date selected.\n",
    "  --events-file PATH                  Read and write local user events at PATH.\n",
    "  --holiday-source off|us-federal|nager\n",
    "                                      Choose holiday data. Default: us-federal.\n",
    "  --holiday-country CC                Country code for --holiday-source nager. Default: US.\n",
    "  -h, --help                          Show this help.\n",
    "  -V, --version                       Show version.\n\n",
    "Keys:\n",
    "  Arrow keys move selection; Enter opens day view; Esc returns to month; q exits.\n",
    "  + opens the Create event modal.\n",
    "  In day view, Left/Right move to the previous or next day.\n",
    "  Digits jump immediately; a quick second digit refines the selected day.\n",
    "  Weekday initials jump within the selected week.\n\n",
    "Mouse:\n",
    "  Left click selects a visible date; left click the selected date again to open day view.\n\n",
    "Notes:\n",
    "  Real calendar-account integration, editing, deletion, and reminder notifications are not in this milestone.\n",
);

const VERSION: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"), "\n");
const DIGIT_JUMP_TIMEOUT: Duration = Duration::from_millis(900);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub start_date: CalendarDate,
    pub events_file: PathBuf,
    pub holiday_source: HolidaySourceConfig,
    pub holiday_country: String,
}

impl AppConfig {
    pub fn new(start_date: CalendarDate) -> Self {
        Self {
            start_date,
            events_file: default_events_file(),
            holiday_source: HolidaySourceConfig::UsFederal,
            holiday_country: "US".to_string(),
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
    Help,
    Version,
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
            Self::UnknownArgument(arg) => write!(f, "unknown argument: {arg}"),
            Self::InvalidDate { input, reason } => {
                write!(f, "invalid --date value '{input}': {reason}")
            }
        }
    }
}

impl std::error::Error for CliError {}

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
    match parse_args(args, default_start_date()) {
        Ok(CliAction::Run(config)) => {
            let app = AppState::new(config.start_date);
            let agenda_source = match agenda_source(&config) {
                Ok(source) => source,
                Err(err) => return local_event_error_exit(&mut stderr, err),
            };
            let (width, height) = terminal_size();
            let rendered =
                render_app_to_string_with_agenda_source(&app, width, height, &agenda_source);
            match write!(stdout, "{rendered}") {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
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
    match parse_args(args, default_start_date()) {
        Ok(CliAction::Run(config)) => {
            let app = AppState::new(config.start_date);
            let agenda_source = match agenda_source(&config) {
                Ok(source) => source,
                Err(err) => return local_event_error_exit(&mut stderr, err),
            };
            match run_interactive_terminal(stdout, app, agenda_source) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
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

    let holiday_country_was_provided = holiday_country.is_some();
    let mut config = AppConfig::new(CalendarDate::from(start_date.unwrap_or(today)));
    if let Some(events_file) = events_file {
        config.events_file = events_file;
    }
    if let Some(holiday_source) = holiday_source {
        config.holiday_source = holiday_source;
    }
    if let Some(holiday_country) = holiday_country {
        config.holiday_country = holiday_country;
    }
    if holiday_country_was_provided && config.holiday_source != HolidaySourceConfig::Nager {
        return Err(CliError::HolidayCountryRequiresNager);
    }

    Ok(CliAction::Run(config))
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

    ConfiguredAgendaSource::from_events_file(config.events_file.clone(), holidays)
}

fn run_interactive_terminal<W>(
    stdout: W,
    app: AppState,
    agenda_source: ConfiguredAgendaSource,
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

    let result = run_event_loop(&mut terminal, app, agenda_source);
    let cleanup_result = restore_terminal(&mut terminal);

    result.and(cleanup_result)
}

fn run_event_loop<W>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    mut app: AppState,
    mut agenda_source: ConfiguredAgendaSource,
) -> io::Result<()>
where
    W: Write,
{
    let mut keyboard = KeyboardInput::default();
    let mut mouse = MouseInput::default();

    loop {
        terminal.draw(|frame| {
            frame.render_widget(
                AppView::with_agenda_source(&app, &agenda_source),
                frame.area(),
            );
        })?;

        if app.should_quit() {
            return Ok(());
        }

        let event = if !app.is_creating_event() && keyboard.is_waiting_for_digit() {
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
                if app.is_creating_event() {
                    match app.handle_create_key(key) {
                        CreateEventInputResult::Continue => {}
                        CreateEventInputResult::Cancel => app.close_create_form(),
                        CreateEventInputResult::Submit(draft) => {
                            match agenda_source.create_event(draft) {
                                Ok(_) => app.close_create_form(),
                                Err(err) => app.set_create_form_error(err.to_string()),
                            }
                        }
                    }
                } else {
                    let action = keyboard.translate(key);
                    app.apply(action);
                }
            }
            Event::Mouse(mouse_event) => {
                if app.is_creating_event() {
                    continue;
                }
                keyboard.clear();
                let size = terminal.size()?;
                let area = Rect::new(0, 0, size.width, size.height);
                let target_date =
                    hit_test_app_date(&app, area, mouse_event.column, mouse_event.row);
                let action = mouse.translate(mouse_event, target_date, app.selected_date());
                app.apply(action);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::CalendarMonth;
    use time::Month;

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn arg(value: &str) -> OsString {
        OsString::from(value)
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
