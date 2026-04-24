# rcal

`rcal` is a terminal calendar for quick month, week, and day navigation. It is
built in Rust with `ratatui`, `crossterm`, and the `time` crate.


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
rcal providers microsoft auth inspect --account ID
rcal providers microsoft calendars list --account ID
rcal providers microsoft setup --account ID [--browser] [--calendar ID]
rcal providers microsoft sync [--account ID]
rcal providers microsoft status
rcal providers google auth login --account ID
rcal providers google auth logout --account ID
rcal providers google auth inspect --account ID
rcal providers google calendars list --account ID
rcal providers google setup --account ID --client-id ID [--client-secret SECRET] [--calendar ID]
rcal providers google sync [--account ID]
rcal providers google status
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
keybindings. It can also configure the Microsoft and Google Calendar providers.
Modal/form keys stay fixed for now.

Nager.Date is cache-first and opt-in. Default startup does not need network
access.

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

The create/edit modal supports timed events, single-day all-day events,
recurrence, location, notes, and multiple reminder offsets. Its `Calendar`
field controls where the event is saved; use Left/Right on that field to cycle
between local storage and configured editable provider calendars. Local events
are stored as JSON, while provider events are written through their remote API
and then shown immediately from the provider cache.
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


## Microsoft Provider

Microsoft Graph is the first remote provider. It is cache-first: the TUI reads
the local Microsoft cache instantly, and you refresh remote data explicitly:

```sh
rcal providers microsoft setup --account work --browser
rcal
```

The setup command uses rcal's official public/native Microsoft client ID, opens
the Microsoft login flow, selects your default editable calendar, writes
`~/.config/rcal/config.toml`, performs the first sync, and sets new event
creation to Microsoft by default. No client secret is used or stored; rcal
stores user tokens in the OS keychain.

Use `--calendar CALENDAR_ID` if you already know which editable calendar to use.
You can list calendars later with:

```sh
rcal providers microsoft calendars list --account work
```

### Microsoft Account Setup

The normal setup flow is:

```sh
rcal providers microsoft setup --account work --browser
rcal providers microsoft status
rcal
```

For advanced testing, you may still override the Microsoft app registration in
`config.toml`. Omit `client_id` and `tenant` to use rcal's official public app:

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
# client_id = "AZURE_APP_CLIENT_ID"
# tenant = "common"
redirect_port = 8765
calendars = ["CALENDAR_ID"]
```

Manual sync remains available:

```sh
rcal providers microsoft sync --account work
```

Deprecated manual onboarding flow, pending deletion:

```sh
rcal config init
# edit ~/.config/rcal/config.toml with providers.microsoft account/calendar data
rcal providers microsoft auth login --account work --browser
rcal providers microsoft calendars list --account work
# copy an editable calendar ID into default_calendar and calendars
rcal providers microsoft sync --account work
```

Use `rcal providers microsoft setup --account work --browser` instead. The
manual flow is kept temporarily for advanced custom-app testing and for users
upgrading from early provider builds.

Use `rcal providers microsoft auth inspect --account work` to inspect safe
token claims such as audience, scopes, tenant, and expiry. The command does not
print the token body.

The provider syncs configured calendars through Graph `calendarView`, caches
selected events separately from the local events JSON, and routes
create/edit/delete/copy operations for Microsoft events back through Graph.
Provider reminders fire from cached provider events after a sync; the reminder
daemon does not sync remote calendars itself.

## Google Calendar Provider

Google Calendar support is also cache-first. You refresh remote data explicitly:

```sh
rcal providers google setup --account personal --client-id GOOGLE_CLIENT_ID
rcal
```

Google setup currently needs your own Google OAuth Desktop client ID. If Google
also gives you a client secret, pass `--client-secret GOOGLE_CLIENT_SECRET`.
The setup flow opens browser auth, selects your default editable calendar,
writes `~/.config/rcal/config.toml`, performs the first sync, and sets new
event creation to Google by default. User tokens are stored in the OS keychain.

Use `--calendar CALENDAR_ID` to choose a known editable calendar. You can list
calendars later with:

```sh
rcal providers google calendars list --account personal
```

Manual sync:

```sh
rcal providers google sync --account personal
```

Generated Google config looks like:

```toml
[providers]
create_target = "google" # or "local" to keep new events local-only

[providers.google]
enabled = true
default_account = "personal"
default_calendar = "primary"
sync_past_days = 30
sync_future_days = 365

[[providers.google.accounts]]
id = "personal"
client_id = "GOOGLE_CLIENT_ID"
# client_secret = "GOOGLE_CLIENT_SECRET"
redirect_port = 8766
calendars = ["primary"]
```

The Google provider syncs configured calendars through the Calendar API,
caches selected events separately from the local events JSON, and routes
create/edit/delete/copy operations for Google events back through Google
Calendar. Provider reminders fire from cached Google events after a sync.


## Current Limits

- CalDAV and other non-Microsoft/non-Google providers are not implemented yet.
- Provider sync is manual and cache-first; there is no background provider
  sync daemon yet.
- Google currently requires a user-supplied OAuth Desktop client ID; rcal does
  not yet ship an official Google OAuth client.

## Development

CI runs the same core commands used locally:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The project intentionally keeps private planning notes under `.docs/`; they are
not part of the tracked release surface.
