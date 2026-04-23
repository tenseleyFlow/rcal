use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, IsTerminal, Write},
};

use crossterm::terminal;
use ratatui::{Terminal, backend::CrosstermBackend};
use time::{Date, OffsetDateTime, format_description};

use crate::{
    calendar::{CalendarDate, CalendarMonth},
    tui::{DEFAULT_RENDER_HEIGHT, DEFAULT_RENDER_WIDTH, MonthGrid, render_month_to_string},
};

const USAGE: &str = "Usage: rcal [--date YYYY-MM-DD]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppConfig {
    pub start_date: CalendarDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAction {
    Run(AppConfig),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    DuplicateDate,
    MissingDateValue,
    UnknownArgument(String),
    InvalidDate { input: String, reason: String },
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDate => write!(f, "--date may only be provided once"),
            Self::MissingDateValue => write!(f, "--date requires a value in YYYY-MM-DD format"),
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
            let month = CalendarMonth::for_launch_date(config.start_date);
            let (width, height) = terminal_size();
            let rendered = render_month_to_string(&month, width, height);
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
            let month = CalendarMonth::for_launch_date(config.start_date);
            match draw_month_to_terminal(stdout, &month) {
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

        return Err(CliError::UnknownArgument(display_arg(&arg)));
    }

    Ok(CliAction::Run(AppConfig {
        start_date: CalendarDate::from(start_date.unwrap_or(today)),
    }))
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

fn draw_month_to_terminal<W>(stdout: W, month: &CalendarMonth) -> io::Result<()>
where
    W: Write,
{
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| {
        frame.render_widget(MonthGrid::new(month), frame.area());
    })?;
    Ok(())
}

fn parse_date_arg(value: &OsStr) -> Result<Date, CliError> {
    let value = value.to_str().ok_or_else(|| CliError::InvalidDate {
        input: display_arg(value),
        reason: "date must be valid UTF-8".to_string(),
    })?;

    parse_date_str(value)
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

        assert_eq!(action, CliAction::Run(AppConfig { start_date: today }));
    }

    #[test]
    fn date_flag_sets_start_date() {
        let today = date(2026, Month::April, 23);

        let action =
            parse_args([arg("--date"), arg("2027-01-02")], today.into()).expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: date(2027, Month::January, 2)
            })
        );
    }

    #[test]
    fn date_equals_form_sets_start_date() {
        let today = date(2026, Month::April, 23);

        let action = parse_args([arg("--date=2027-01-02")], today.into()).expect("parse succeeds");

        assert_eq!(
            action,
            CliAction::Run(AppConfig {
                start_date: date(2027, Month::January, 2)
            })
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
