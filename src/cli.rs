use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, IsTerminal, Write},
};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use time::{Date, OffsetDateTime, format_description};

use crate::{
    agenda::{AgendaSource, ConfiguredAgendaSource, HolidayProvider},
    app::{AppState, KeyboardInput},
    calendar::CalendarDate,
    tui::{
        AppView, DEFAULT_RENDER_HEIGHT, DEFAULT_RENDER_WIDTH,
        render_app_to_string_with_agenda_source,
    },
};

const USAGE: &str = "Usage: rcal [--date YYYY-MM-DD] [--holiday-source off|us-federal|nager] [--holiday-country CC]\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub start_date: CalendarDate,
    pub holiday_source: HolidaySourceConfig,
    pub holiday_country: String,
}

impl AppConfig {
    pub fn new(start_date: CalendarDate) -> Self {
        Self {
            start_date,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    DuplicateDate,
    MissingDateValue,
    DuplicateHolidaySource,
    MissingHolidaySourceValue,
    InvalidHolidaySource(String),
    DuplicateHolidayCountry,
    MissingHolidayCountryValue,
    InvalidHolidayCountry(String),
    UnknownArgument(String),
    InvalidDate { input: String, reason: String },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDate => write!(f, "--date may only be provided once"),
            Self::MissingDateValue => write!(f, "--date requires a value in YYYY-MM-DD format"),
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
            let agenda_source = agenda_source(&config);
            let (width, height) = terminal_size();
            let rendered =
                render_app_to_string_with_agenda_source(&app, width, height, &agenda_source);
            match write!(stdout, "{rendered}") {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
        Ok(CliAction::Help) => match write!(stdout, "{USAGE}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}\n\n{USAGE}");
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
            let agenda_source = agenda_source(&config);
            match run_interactive_terminal(stdout, app, &agenda_source) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => io_error_exit(&mut stderr, err),
            }
        }
        Ok(CliAction::Help) => match write!(stdout, "{USAGE}") {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => io_error_exit(&mut stderr, err),
        },
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}\n\n{USAGE}");
            std::process::ExitCode::from(2)
        }
    }
}

pub fn parse_args<I>(args: I, today: Date) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut start_date = None;
    let mut holiday_source = None;
    let mut holiday_country = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(CliAction::Help);
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

    let mut config = AppConfig::new(CalendarDate::from(start_date.unwrap_or(today)));
    if let Some(holiday_source) = holiday_source {
        config.holiday_source = holiday_source;
    }
    if let Some(holiday_country) = holiday_country {
        config.holiday_country = holiday_country;
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

fn agenda_source(config: &AppConfig) -> ConfiguredAgendaSource {
    let holidays = match config.holiday_source {
        HolidaySourceConfig::Off => HolidayProvider::off(),
        HolidaySourceConfig::UsFederal => HolidayProvider::us_federal(),
        HolidaySourceConfig::Nager => HolidayProvider::nager(config.holiday_country.clone()),
    };

    ConfiguredAgendaSource::development(holidays)
}

fn run_interactive_terminal<W, S>(mut stdout: W, app: AppState, agenda_source: &S) -> io::Result<()>
where
    W: Write,
    S: AgendaSource,
{
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_event_loop(&mut terminal, app, agenda_source);
    let cleanup_result = restore_terminal(&mut terminal);

    result.and(cleanup_result)
}

fn run_event_loop<W, S>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    mut app: AppState,
    agenda_source: &S,
) -> io::Result<()>
where
    W: Write,
    S: AgendaSource,
{
    let mut keyboard = KeyboardInput::default();

    loop {
        terminal.draw(|frame| {
            frame.render_widget(
                AppView::with_agenda_source(&app, agenda_source),
                frame.area(),
            );
        })?;

        if app.should_quit() {
            return Ok(());
        }

        match event::read()? {
            Event::Key(key) => {
                let action = keyboard.translate(key);
                app.apply(action);
            }
            Event::Resize(_, _) => keyboard.clear(),
            _ => {}
        }
    }
}

fn restore_terminal<W>(terminal: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()>
where
    W: Write,
{
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
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
            })
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
