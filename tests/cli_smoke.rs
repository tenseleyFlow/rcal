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

#[test]
fn events_file_renders_local_event() {
    let path = std::env::temp_dir().join(format!(
        "rcal-cli-events-{}-{}.json",
        std::process::id(),
        "render"
    ));
    std::fs::write(
        &path,
        r#"{
  "version": 1,
  "events": [
    {
      "id": "local-test",
      "title": "Planning",
      "start_date": "2026-04-23",
      "start_time": "09:00",
      "end_date": "2026-04-23",
      "end_time": "10:00",
      "location": "War room",
      "notes": "Bring notes",
      "reminders_minutes_before": [10, 60]
    }
  ]
}"#,
    )
    .expect("events file can be written");

    let output = rcal()
        .args([
            "--date",
            "2026-04-23",
            "--events-file",
            path.to_str().expect("temp path is utf-8"),
        ])
        .output()
        .expect("rcal binary runs");

    let _ = std::fs::remove_file(path);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    assert!(String::from_utf8_lossy(&output.stdout).contains("09:00 Plan"));
}

#[test]
fn malformed_events_file_fails_cleanly() {
    let path = std::env::temp_dir().join(format!(
        "rcal-cli-events-{}-{}.json",
        std::process::id(),
        "malformed"
    ));
    std::fs::write(&path, "{not json").expect("events file can be written");

    let output = rcal()
        .args([
            "--date",
            "2026-04-23",
            "--events-file",
            path.to_str().expect("temp path is utf-8"),
        ])
        .output()
        .expect("rcal binary runs");

    let _ = std::fs::remove_file(path);

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to load local events"));
    assert!(stderr.contains("failed to parse"));
}
