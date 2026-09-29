# Task16 Task1 Report: Browser State

## Status

Implemented the Task16 read-only browser state and deterministic view
transformations. The implementation reuses `rustnt-core::filesystem` types and
`list_directory` without changing the core filesystem or privilege boundary.

## Files

- `crates/rustnt-gui/Cargo.toml`: added the existing workspace `rustnt-core`
  path dependency.
- `Cargo.lock`: recorded the workspace dependency edge.
- `crates/rustnt-gui/src/main.rs`: exposed `file_browser` without changing GUI
  startup order.
- `crates/rustnt-gui/src/file_browser.rs`: added browser state, injectable
  loading, navigation history, filtering, sorting, error retention, and tests.

## TDD and Verification

The focused test command was run before implementation and failed because the
new module and browser interfaces were absent. After implementation:

- `cargo test -p rustnt-gui file_browser -- --nocapture`: 14 passed, 0 failed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The focused tests cover initial state, successful and failed loads, preserving
the last valid listing after failure, child/parent navigation, drive and UNC
root boundaries, back/forward/refresh behavior, filtering, all sort fields and
directions, stable tie-breakers, empty directories, and reparse metadata.

Cargo emits the existing ignored semver metadata warning for the `toml`
dependency; it is unrelated to Task16.

## Scope Review

No file writes, external launches, Shell integration, service, Named Pipe,
driver, kernel, or privilege changes were added. `app.rs`, navigation, and
historical reports were not modified.

## Concerns

The GUI integration and native-window behavior remain for Task2. This task only
provides the state model and deterministic transformations.

## Review Fix

The UNC share-root check now accepts both forward-slash and backslash path
forms, including mixed separator prefixes. Focused tests cover both
`//server/share/` as a share root and `//server/share/folder` as its child.

Verification:

- `cargo test -p rustnt-gui file_browser -- --nocapture`: 14 passed, 0 failed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

The new forward-slash assertions first failed with the previous implementation
because `//server/share/` incorrectly returned `Some("//server/")`; they pass
after the prefix check was corrected.

## Clippy Warning Fix

The Task1 test assertions were updated to satisfy workspace Clippy without
changing production behavior: `std::slice::from_ref(&child)` replaces the
cloned single-element slice, and `!is_empty()` replaces the boolean comparison.
