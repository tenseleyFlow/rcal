use std::process::Command;

fn rcal() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rcal"))
}

#[test]
fn deterministic_date_flag_succeeds() {
    let output = rcal()
        .args(["--date", "2026-04-23"])
        .output()
        .expect("rcal binary runs");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("April 2026"));
    assert!(stdout.contains("Sun"));
    assert!(stdout.contains("[23*]"));
}

#[test]
fn invalid_date_flag_fails() {
    let output = rcal()
        .args(["--date", "2026-02-30"])
        .output()
        .expect("rcal binary runs");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid --date value '2026-02-30'"));
    assert!(stderr.contains("Usage:"));
    assert!(stderr.contains("rcal [--date YYYY-MM-DD]"));
}

#[test]
fn help_flag_succeeds() {
    let output = rcal().arg("--help").output().expect("rcal binary runs");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rcal 0.1.0"));
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("--holiday-source off|us-federal|nager"));
    assert!(stdout.contains("Left click selects a visible date"));
}

#[test]
fn version_flag_succeeds() {
    let output = rcal().arg("--version").output().expect("rcal binary runs");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "rcal 0.1.0\n");
}

#[test]
fn holiday_country_without_nager_fails() {
    let output = rcal()
        .args(["--holiday-country", "GB"])
        .output()
        .expect("rcal binary runs");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--holiday-country may only be used with --holiday-source nager"));
}
