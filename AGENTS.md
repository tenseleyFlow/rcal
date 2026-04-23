# rcal Agent Guide

## Project Vision

`rcal` is a better terminal calendar app written in Rust. It exists because the
default macOS `cal` experience is too small, too passive, and too limited for
daily calendar work.

The product direction is a full terminal calendar interface with:

- A responsive month grid that uses available terminal space well.
- One active date cell with a clear perimeter highlight, focus treatment, and
  full-brightness content while inactive cells are dimmed.
- Keyboard navigation by arrow keys, numeric date jumps, and weekday jumps.
- A focused day view with holidays, event lists, notes, and a 24-hour timeline.
- Mouse support for selecting and opening date cells.
- Future event integrations for local files, holidays, Outlook, Google
  Calendar, and Exchange-style sources.

The first implementation milestone is an interactive month TUI that opens on
the current month, focuses today, supports core keyboard navigation, and has a
responsive fallback path for constrained terminals.

## Technical Defaults

- Language: Rust.
- TUI stack: `ratatui` plus `crossterm`.
- Date/time stack: the `time` crate.
- CLI baseline: `rcal` opens the current month focused on today.
- Reserved deterministic test flag: `--date YYYY-MM-DD`.

Keep calendar math separate from rendering. Input handlers update app state;
rendering consumes immutable view models. Prefer explicit domain types such as
`CalendarDate`, `CalendarMonth`, `CalendarCell`, `Selection`, `Event`,
`Holiday`, and `DayAgenda`.

## Engineering Bar

- Keep the code efficient, clear, and idiomatic.
- Avoid overengineering unless the abstraction directly reduces real
  complexity or protects a future feature already planned.
- Favor small, testable modules over large mixed-responsibility files.
- Use memoization or precomputed view data when it materially simplifies or
  speeds repeated calendar rendering.
- Keep user-facing terminal behavior deterministic enough to snapshot and test.

## Workflow

- Work through `.docs/sprints/` in order.
- Each sprint must satisfy its targets and Definition of Done before moving on.
- Commit each meaningful chunk. Avoid monolithic commits.
- Keep `.docs/` private local planning material unless the project policy
  changes. Do not rely on `.docs/` being present for the application to build.
- Treat `AGENTS.md` as the durable project guidance available to future agents.

## Testing Expectations

Tests are first class.

- Add focused unit tests for calendar math, state transitions, and layout
  policy.
- Add integration tests for CLI behavior and navigation flows.
- Add snapshot or golden tests for stable TUI render output once rendering
  exists.
- Add terminal/e2e smoke tests once a real binary and TUI loop exist.
- Run the full available test suite before declaring a sprint complete.

## Sprint Closeout Audit

At the end of every sprint:

- Compare implementation against every sprint target.
- Mark unfinished work as either fixed immediately or explicitly deferred to a
  later sprint.
- Run all tests.
- Add edge case tests that the current harness missed.
- Organize findings by priority.
- Resolve findings and rerun the audit until clean.

Do not advance to the next sprint while known required targets remain
unresolved.
