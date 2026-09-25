# Task 3 Report: Minimal eframe Application

## Status

Implemented the Task 3 GUI foundation in the assigned worktree. Task 4 startup and cleanup behavior remains unimplemented.

## TDD

### RED

Added the pure mapping tests for `theme_label` and `recovery_label` before the production helpers. The focused command failed as expected because both helpers were missing:

```text
cargo test -p rustnt-gui app::tests -- --nocapture
```

Failure: unresolved imports `super::recovery_label` and `super::theme_label`.

### GREEN

Implemented the minimum app behavior required by the brief, then reran the focused tests. Result: 2 tests passed.

## Implementation

- Added `RustNtApp` with shared `GuiConfig` state protected by `Arc<Mutex<_>>`.
- Added poisoned mutex recovery with `into_inner()`.
- Added the RustNT first screen, stable status labels, theme toggle, save action, configuration notice, recovery notice, and save notice.
- Added dark/light visuals and shared configuration updates.
- Added `viewport_for` with RustNT title, configured dimensions, position restoration, and recovery defaults.
- Added finite viewport size and position snapshot updates while preserving missing positions.
- Added `app` module declaration in `main.rs`; left `main` inert for Task 4.

## Verification

All required commands passed:

```text
cargo fmt --all -- --check       PASS
cargo test -p rustnt-gui app::tests -- --nocapture  PASS (2 tests)
cargo check -p rustnt-gui       PASS
cargo clippy -p rustnt-gui --all-targets -- -D warnings  PASS
```

Cargo prints the existing warning that the `toml` dependency version requirement contains ignored semver metadata. The manifest was not changed.

## Files

- `crates/rustnt-gui/src/app.rs`
- `crates/rustnt-gui/src/main.rs`

The pre-existing modification to `.superpowers/sdd/progress.md` was preserved and excluded from the commit.

## Concerns

- `RustNtApp` is ready for Task 4 to pass into `eframe::run_native`; this task intentionally does not add startup, recovery marker lifecycle, panic handling, or cleanup.
- Position snapshots update when eframe supplies `outer_rect`; absent position data preserves the existing `Option` values.

## Review Fix: 2026-09-24

Addressed the review findings from `review-81b8e51..948872f.diff`:

- The `Save config` action now clones `GuiConfig` while holding the mutex and calls `save_config` only after the guard has been released.
- Added the pure `viewport_settings_for` helper and focused tests for recovery defaults, normal startup position restoration, and missing positions. `viewport_for` now only applies `with_position` when the helper returns an explicit position.
- Task 4 `run_gui` startup wiring remains intentionally unimplemented.

### Review-Fix TDD Evidence

The new viewport tests were first run before the helper existed and failed with unresolved imports for `viewport_settings_for` and `ViewportSettings`. After the minimal implementation, they passed as part of the five-test app-focused suite.

### Review-Fix Verification

```text
cargo fmt --all -- --check                         PASS
cargo test -p rustnt-gui app::tests -- --nocapture PASS (5 tests)
cargo test -p rustnt-gui config::tests -- --nocapture PASS (7 tests)
cargo test -p rustnt-gui recovery::tests -- --nocapture PASS (5 tests)
cargo check -p rustnt-gui                         PASS
cargo clippy -p rustnt-gui --all-targets -- -D warnings PASS
```

Cargo continues to emit the pre-existing warning that semver metadata in the `toml` dependency requirement is ignored. No dependency metadata was changed for this review fix.
