use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use directories::BaseDirs;

use crate::{
    agenda::default_events_file,
    reminders::{default_log_file, default_state_file},
};

const SERVICE_LABEL: &str = "com.tenseleyflow.rcal.reminders";
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "rcal-reminders.service";
const WINDOWS_TASK_NAME: &str = "rcal-reminders";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub executable: PathBuf,
    pub events_file: PathBuf,
    pub state_file: PathBuf,
    pub log_file: PathBuf,
}

impl ServiceConfig {
    pub fn new(events_file: PathBuf) -> Result<Self, ServiceError> {
        let executable = std::env::current_exe().map_err(|err| ServiceError::CurrentExe {
            reason: err.to_string(),
        })?;
        Ok(Self {
            executable,
            events_file,
            state_file: default_state_file(),
            log_file: default_log_file(),
        })
    }
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("rcal"),
            events_file: default_events_file(),
            state_file: default_state_file(),
            log_file: default_log_file(),
        }
    }
}

pub trait CommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<(), ServiceError>;
    fn status(&mut self, program: &str, args: &[String]) -> Result<bool, ServiceError>;
}

#[derive(Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> Result<(), ServiceError> {
        let status =
            Command::new(program)
                .args(args)
                .status()
                .map_err(|err| ServiceError::Command {
                    program: program.to_string(),
                    reason: err.to_string(),
                })?;
        if status.success() {
            Ok(())
        } else {
            Err(ServiceError::Command {
                program: program.to_string(),
                reason: format!("exited with status {status}"),
            })
        }
    }

    fn status(&mut self, program: &str, args: &[String]) -> Result<bool, ServiceError> {
        let status =
            Command::new(program)
                .args(args)
                .status()
                .map_err(|err| ServiceError::Command {
                    program: program.to_string(),
                    reason: err.to_string(),
                })?;
        Ok(status.success())
    }
}

pub fn install_service(
    config: &ServiceConfig,
    runner: &mut dyn CommandRunner,
) -> Result<(), ServiceError> {
    platform_installer().install(config, runner)
}

pub fn uninstall_service(runner: &mut dyn CommandRunner) -> Result<(), ServiceError> {
    platform_installer().uninstall(runner)
}

pub fn service_status(runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError> {
    platform_installer().status(runner)
}

fn platform_installer() -> Box<dyn ServiceInstaller> {
    #[cfg(target_os = "macos")]
    {
        Box::new(MacLaunchAgent)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxSystemdUser)
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsScheduledTask)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Box::new(UnsupportedInstaller)
    }
}

trait ServiceInstaller {
    fn install(
        &self,
        config: &ServiceConfig,
        runner: &mut dyn CommandRunner,
    ) -> Result<(), ServiceError>;
    fn uninstall(&self, runner: &mut dyn CommandRunner) -> Result<(), ServiceError>;
    fn status(&self, runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Installed,
    NotInstalled,
}

impl fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Installed => write!(f, "installed"),
            Self::NotInstalled => write!(f, "not installed"),
        }
    }
}

#[derive(Debug)]
struct MacLaunchAgent;

impl MacLaunchAgent {
    fn plist_path() -> Result<PathBuf, ServiceError> {
        Ok(home_dir()?
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist")))
    }
}

impl ServiceInstaller for MacLaunchAgent {
    fn install(
        &self,
        config: &ServiceConfig,
        runner: &mut dyn CommandRunner,
    ) -> Result<(), ServiceError> {
        let path = Self::plist_path()?;
        if let Some(parent) = config.log_file.parent() {
            fs::create_dir_all(parent).map_err(|err| ServiceError::Write {
                path: parent.to_path_buf(),
                reason: err.to_string(),
            })?;
        }
        write_file(&path, &mac_launch_agent_plist(config))?;
        let _ = runner.run("launchctl", &["unload".to_string(), path_string(&path)]);
        runner.run(
            "launchctl",
            &["load".to_string(), "-w".to_string(), path_string(&path)],
        )
    }

    fn uninstall(&self, runner: &mut dyn CommandRunner) -> Result<(), ServiceError> {
        let path = Self::plist_path()?;
        if path.exists() {
            let _ = runner.run("launchctl", &["unload".to_string(), path_string(&path)]);
            fs::remove_file(&path).map_err(|err| ServiceError::Write {
                path: path.clone(),
                reason: err.to_string(),
            })?;
        }
        Ok(())
    }

    fn status(&self, runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError> {
        let path = Self::plist_path()?;
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        if runner.status(
            "launchctl",
            &["list".to_string(), SERVICE_LABEL.to_string()],
        )? {
            Ok(ServiceStatus::Installed)
        } else {
            Ok(ServiceStatus::NotInstalled)
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxSystemdUser;

#[cfg(target_os = "linux")]
impl LinuxSystemdUser {
    fn unit_path() -> Result<PathBuf, ServiceError> {
        let config_home = if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(config_home)
        } else {
            home_dir()?.join(".config")
        };
        Ok(config_home
            .join("systemd")
            .join("user")
            .join(SYSTEMD_SERVICE_NAME))
    }
}

#[cfg(target_os = "linux")]
impl ServiceInstaller for LinuxSystemdUser {
    fn install(
        &self,
        config: &ServiceConfig,
        runner: &mut dyn CommandRunner,
    ) -> Result<(), ServiceError> {
        let path = Self::unit_path()?;
        write_file(&path, &linux_systemd_unit(config))?;
        runner.run(
            "systemctl",
            &["--user".to_string(), "daemon-reload".to_string()],
        )?;
        runner.run(
            "systemctl",
            &[
                "--user".to_string(),
                "enable".to_string(),
                "--now".to_string(),
                SYSTEMD_SERVICE_NAME.to_string(),
            ],
        )
    }

    fn uninstall(&self, runner: &mut dyn CommandRunner) -> Result<(), ServiceError> {
        let path = Self::unit_path()?;
        let _ = runner.run(
            "systemctl",
            &[
                "--user".to_string(),
                "disable".to_string(),
                "--now".to_string(),
                SYSTEMD_SERVICE_NAME.to_string(),
            ],
        );
        if path.exists() {
            fs::remove_file(&path).map_err(|err| ServiceError::Write {
                path: path.clone(),
                reason: err.to_string(),
            })?;
        }
        runner.run(
            "systemctl",
            &["--user".to_string(), "daemon-reload".to_string()],
        )
    }

    fn status(&self, runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError> {
        let path = Self::unit_path()?;
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        if runner.status(
            "systemctl",
            &[
                "--user".to_string(),
                "is-active".to_string(),
                "--quiet".to_string(),
                SYSTEMD_SERVICE_NAME.to_string(),
            ],
        )? {
            Ok(ServiceStatus::Installed)
        } else {
            Ok(ServiceStatus::NotInstalled)
        }
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct WindowsScheduledTask;

#[cfg(target_os = "windows")]
impl ServiceInstaller for WindowsScheduledTask {
    fn install(
        &self,
        config: &ServiceConfig,
        runner: &mut dyn CommandRunner,
    ) -> Result<(), ServiceError> {
        runner.run("schtasks", &windows_schtasks_create_args(config))?;
        runner.run(
            "schtasks",
            &[
                "/Run".to_string(),
                "/TN".to_string(),
                WINDOWS_TASK_NAME.to_string(),
            ],
        )
    }

    fn uninstall(&self, runner: &mut dyn CommandRunner) -> Result<(), ServiceError> {
        let _ = runner.run(
            "schtasks",
            &[
                "/End".to_string(),
                "/TN".to_string(),
                WINDOWS_TASK_NAME.to_string(),
            ],
        );
        runner.run(
            "schtasks",
            &[
                "/Delete".to_string(),
                "/TN".to_string(),
                WINDOWS_TASK_NAME.to_string(),
                "/F".to_string(),
            ],
        )
    }

    fn status(&self, runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError> {
        if runner.status(
            "schtasks",
            &[
                "/Query".to_string(),
                "/TN".to_string(),
                WINDOWS_TASK_NAME.to_string(),
            ],
        )? {
            Ok(ServiceStatus::Installed)
        } else {
            Ok(ServiceStatus::NotInstalled)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
#[derive(Debug)]
struct UnsupportedInstaller;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
impl ServiceInstaller for UnsupportedInstaller {
    fn install(
        &self,
        _config: &ServiceConfig,
        _runner: &mut dyn CommandRunner,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::UnsupportedPlatform)
    }

    fn uninstall(&self, _runner: &mut dyn CommandRunner) -> Result<(), ServiceError> {
        Err(ServiceError::UnsupportedPlatform)
    }

    fn status(&self, _runner: &mut dyn CommandRunner) -> Result<ServiceStatus, ServiceError> {
        Err(ServiceError::UnsupportedPlatform)
    }
}

pub fn mac_launch_agent_plist(config: &ServiceConfig) -> String {
    let stdout = config.log_file.with_extension("out.log");
    let stderr = config.log_file.with_extension("err.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>reminders</string>
    <string>run</string>
    <string>--events-file</string>
    <string>{}</string>
    <string>--state-file</string>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        SERVICE_LABEL,
        xml_escape(&path_string(&config.executable)),
        xml_escape(&path_string(&config.events_file)),
        xml_escape(&path_string(&config.state_file)),
        xml_escape(&path_string(&stdout)),
        xml_escape(&path_string(&stderr)),
    )
}

pub fn linux_systemd_unit(config: &ServiceConfig) -> String {
    format!(
        "[Unit]\nDescription=rcal reminder notifications\n\n[Service]\nExecStart={} reminders run --events-file {} --state-file {}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        systemd_escape(&path_string(&config.executable)),
        systemd_escape(&path_string(&config.events_file)),
        systemd_escape(&path_string(&config.state_file)),
    )
}

pub fn windows_schtasks_create_args(config: &ServiceConfig) -> Vec<String> {
    let command = format!(
        "\"{}\" reminders run --events-file \"{}\" --state-file \"{}\"",
        path_string(&config.executable),
        path_string(&config.events_file),
        path_string(&config.state_file),
    );
    vec![
        "/Create".to_string(),
        "/TN".to_string(),
        WINDOWS_TASK_NAME.to_string(),
        "/SC".to_string(),
        "ONLOGON".to_string(),
        "/TR".to_string(),
        command,
        "/F".to_string(),
    ]
}

#[derive(Debug)]
pub enum ServiceError {
    CurrentExe { reason: String },
    MissingHome,
    UnsupportedPlatform,
    Write { path: PathBuf, reason: String },
    Command { program: String, reason: String },
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExe { reason } => {
                write!(f, "failed to locate current executable: {reason}")
            }
            Self::MissingHome => write!(f, "failed to locate a user home directory"),
            Self::UnsupportedPlatform => {
                write!(f, "reminder services are unsupported on this platform")
            }
            Self::Write { path, reason } => {
                write!(f, "failed to write {}: {reason}", path.display())
            }
            Self::Command { program, reason } => write!(f, "{program} failed: {reason}"),
        }
    }
}

impl Error for ServiceError {}

fn write_file(path: &Path, body: &str) -> Result<(), ServiceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| ServiceError::Write {
            path: parent.to_path_buf(),
            reason: err.to_string(),
        })?;
    }

    fs::write(path, body).map_err(|err| ServiceError::Write {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })
}

fn home_dir() -> Result<PathBuf, ServiceError> {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .ok_or(ServiceError::MissingHome)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_escape(value: &str) -> String {
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '\\' && ch != '"')
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServiceConfig {
        ServiceConfig {
            executable: PathBuf::from("/usr/local/bin/rcal"),
            events_file: PathBuf::from("/tmp/rcal/events.json"),
            state_file: PathBuf::from("/tmp/rcal/state.json"),
            log_file: PathBuf::from("/tmp/rcal/reminders.log"),
        }
    }

    #[test]
    fn mac_launch_agent_contains_reminder_run_command() {
        let plist = mac_launch_agent_plist(&config());

        assert!(plist.contains("com.tenseleyflow.rcal.reminders"));
        assert!(plist.contains("<string>/usr/local/bin/rcal</string>"));
        assert!(plist.contains("<string>reminders</string>"));
        assert!(plist.contains("<string>--events-file</string>"));
    }

    #[test]
    fn linux_unit_contains_reminder_run_command() {
        let unit = linux_systemd_unit(&config());

        assert!(unit.contains("Description=rcal reminder notifications"));
        assert!(unit.contains("ExecStart=/usr/local/bin/rcal reminders run"));
        assert!(unit.contains("Restart=always"));
    }

    #[test]
    fn windows_task_command_contains_reminder_run_command() {
        let args = windows_schtasks_create_args(&config());
        let joined = args.join(" ");

        assert!(joined.contains("/Create"));
        assert!(joined.contains("rcal-reminders"));
        assert!(joined.contains("\"/usr/local/bin/rcal\" reminders run"));
    }
}
