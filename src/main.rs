use std::process::ExitCode;

fn main() -> ExitCode {
    rcal::cli::run_terminal(std::env::args_os().skip(1))
}
