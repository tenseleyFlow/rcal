# rcal

`rcal` is a terminal calendar for quick month, week, and day navigation. It is
built in Rust with `ratatui`, `crossterm`, and the `time` crate.

This first milestone is meant for local daily trial use. It opens on the
current month, keeps keyboard navigation fast, falls back to week or day views
when terminal space is tight, and shows agenda previews from the current local
events file plus holiday sources.

## Install

From a checkout of this repository:

```sh
cargo install --path .
```

For development:

```sh
cargo run -- --date 2026-04-23
```

## Usage

```sh
rcal [--config PATH|--no-config] [--date YYYY-MM-DD] [--events-file PATH] [--holiday-source off|us-federal|nager] [--holiday-country CC]
rcal config init [--path PATH] [--force]
rcal providers microsoft auth login --account ID [--browser]
rcal providers microsoft auth logout --account ID
rcal providers microsoft calendars list --account ID
rcal providers microsoft sync [--account ID]
rcal providers microsoft status
rcal reminders run [--events-file PATH] [--state-file PATH] [--once]
rcal reminders install [--events-file PATH] [--state-file PATH]
rcal reminders uninstall
rcal reminders status
rcal reminders test [--verbose]
```

Options:

- `--config PATH`: load a specific TOML config file.
- `--no-config`: ignore any discovered config file.
- `--date YYYY-MM-DD`: open with a deterministic selected date.
- `--events-file PATH`: read and write local user-created events at `PATH`.
  By default, rcal uses `$XDG_DATA_HOME/rcal/events.json`,
  `$HOME/.local/share/rcal/events.json`, or a temp fallback.
- `--holiday-source us-federal`: use offline U.S. federal holidays. This is the
  default.
- `--holiday-source off`: disable holiday rendering.
- `--holiday-source nager`: fetch public holidays from Nager.Date on demand.
- `--holiday-country CC`: two-letter country code for Nager.Date. This option
  requires `--holiday-source nager`; default is `US`.
- `--help`: show CLI help.
- `--version`: show the installed version.

Config is discovered at `$XDG_CONFIG_HOME/rcal/config.toml`, else
`~/.config/rcal/config.toml`. It is never created automatically; run
`rcal config init` to write a commented starter file. Omitted settings keep
built-in defaults, and CLI flags override config values. Config can set the
events file, holiday source and country, reminder state file, and normal-mode
keybindings. It can also configure the Microsoft provider. Modal/form keys stay
fixed for now.

Nager.Date is cache-first and opt-in. Default startup does not need network
access.

## Microsoft Provider

Microsoft Graph is the first remote provider. It is cache-first: the TUI reads
the local Microsoft cache instantly, and you refresh remote data explicitly:

```sh
rcal providers microsoft auth login --account work
rcal providers microsoft calendars list --account work
rcal providers microsoft sync --account work
```

For the current development release, users provide their own Microsoft Entra
app registration `client_id` in `config.toml`. A future production release can
ship an rcal-owned public/native client ID so end users do not need to create an
Azure app. No client secret is used or stored; rcal stores user tokens in the OS
keychain.

### Microsoft Account Setup

Create a config file:

```sh
rcal config init
```

Register a temporary local test app in the Microsoft Entra admin center:

- Name: `rcal local test` or similar.
- Supported account type:
  - Personal Outlook/Hotmail/Live accounts: **Personal Microsoft accounts
    only**.
  - Work or school Microsoft 365 accounts: **Accounts in any organizational
    directory**.
- Redirect URI: platform **Mobile and desktop applications**, value
  `http://localhost:8765/callback`.
- API permissions: Microsoft Graph delegated `User.Read` and
  `Calendars.ReadWrite`.
- If Azure refuses the account type change with
  `api.requestedAccessTokenVersion`, set the app manifest's
  `requestedAccessTokenVersion` or `accessTokenAcceptedVersion` to `2`.

Then edit `~/.config/rcal/config.toml`:

```toml
[providers]
create_target = "microsoft" # or "local" to keep new events local-only

[providers.microsoft]
enabled = true
default_account = "work"
default_calendar = "CALENDAR_ID"
sync_past_days = 30
sync_future_days = 365

[[providers.microsoft.accounts]]
id = "work"
client_id = "AZURE_APP_CLIENT_ID"
tenant = "consumers"      # personal accounts
# tenant = "organizations" # work/school accounts
redirect_port = 8765
calendars = ["CALENDAR_ID"]
```

Authenticate, list calendars, then copy the editable calendar ID into both
`default_calendar` and `calendars`:

```sh
rcal providers microsoft auth login --account work --browser
rcal providers microsoft calendars list --account work
rcal providers microsoft sync --account work
rcal
```

Use `rcal providers microsoft auth inspect --account work` to inspect safe
token claims such as audience, scopes, tenant, and expiry. The command does not
print the token body.

The provider syncs configured calendars through Graph `calendarView`, caches
selected events separately from the local events JSON, and routes
create/edit/delete/copy operations for Microsoft events back through Graph.
Provider reminders fire from cached provider events after a sync; the reminder
daemon does not sync remote calendars itself.

## Controls

- Arrow keys move the selected date.
- `?` opens contextual help.
- `+` opens the Create event modal.
- In day view, `c` opens the Copy confirmation for the selected editable event.
- In day view, `d` opens the Delete confirmation for the selected editable event.
- `Enter` opens the focused day view.
- `Esc` returns from day view to month view.
- `q` exits.
- In day view, Left/Right move to the previous or next day while staying in day
  view.
- Digits jump immediately to a day in the visible month. A quick second digit
  refines the selected day, so `1` selects day 1 and `1` then `6` selects day
  16.
- Weekday initials jump within the selected week. Use `tu` for Tuesday, `th`
  for Thursday, `su` for Sunday, and `sa` for Saturday.
- Left click selects a visible date; double-click a visible date to open day
  view.

Created events are stored locally as JSON and are shown immediately in month,
week, and day views. The create/edit modal supports timed events, single-day
all-day events, recurrence, location, notes, and multiple reminder offsets.
Reminder notifications are delivered by a user-level background service. Use
`rcal reminders install` to install it, `rcal reminders status` to inspect it,
and `rcal reminders test` to send a test notification. On macOS, notification
delivery uses `osascript` because it is more reliable for CLI-launched
notifications than the generic notification backend. Reminder install snapshots
the resolved events and state file paths, so reinstall the service after config
changes that affect reminders.

## Layout

`rcal` tries to render the full month first. If the terminal is too constrained,
it falls back to the selected week. If even that cannot fit cleanly, it falls
back to a focused day summary.

## Current Limits

- Google Calendar, CalDAV, and other providers are not implemented yet.
- Microsoft sync is manual and cache-first; there is no background provider
  sync daemon yet.
- Packaging is currently source-based through Cargo.

## Development

CI runs the same core commands used locally:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The project intentionally keeps private planning notes under `.docs/`; they are
not part of the tracked release surface.
