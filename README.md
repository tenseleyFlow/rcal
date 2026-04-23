# rcal

`rcal` is a terminal calendar for quick month, week, and day navigation. It is
built in Rust with `ratatui`, `crossterm`, and the `time` crate.

This first milestone is meant for local daily trial use. It opens on the
current month, keeps keyboard navigation fast, falls back to week or day views
when terminal space is tight, and shows agenda previews from the current local
fixture plus holiday sources.

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
rcal [--date YYYY-MM-DD] [--holiday-source off|us-federal|nager] [--holiday-country CC]
```

Options:

- `--date YYYY-MM-DD`: open with a deterministic selected date.
- `--holiday-source us-federal`: use offline U.S. federal holidays. This is the
  default.
- `--holiday-source off`: disable holiday rendering.
- `--holiday-source nager`: fetch public holidays from Nager.Date on demand.
- `--holiday-country CC`: two-letter country code for Nager.Date. This option
  requires `--holiday-source nager`; default is `US`.
- `--help`: show CLI help.
- `--version`: show the installed version.

Nager.Date is cache-first and opt-in. Default startup does not need network
access.

## Controls

- Arrow keys move the selected date.
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
- Left click selects a visible date; left click the selected date again to open
  day view.

## Layout

`rcal` tries to render the full month first. If the terminal is too constrained,
it falls back to the selected week. If even that cannot fit cleanly, it falls
back to a focused day summary.

## Current Limits

- Real account integrations for Outlook, Google Calendar, Exchange, and similar
  providers are not implemented yet.
- Event editing and persistent user event storage are deferred.
- The current event data is an in-memory development fixture.
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
