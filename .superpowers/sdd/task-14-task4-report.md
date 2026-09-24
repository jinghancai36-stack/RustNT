# Task 14 Task 4 Report: Startup Wiring and User-Facing Documentation

## Status

Completed the Task 4 GUI startup wiring in the assigned worktree. The GUI now
loads configuration and recovery state, creates the runtime marker before
starting eframe, runs the `RustNtApp` through `eframe::run_native`, persists
the final configuration after a normal event-loop return, and reports startup
errors through the process exit code.

## Implementation

- Added `startup_config` to select loaded settings for a normal start and safe
  default settings after an incomplete previous run.
- Added `GuiError` with `Display`, `Error`, and conversions for configuration,
  recovery, mutex, and eframe failures.
- Wired `config_paths_from_appdata`, directory creation, recovery inspection,
  configuration loading, marker creation, panic-hook installation, viewport
  construction, shared app state, and `eframe::run_native` in the required
  startup order.
- Updated `RustNtApp` construction to receive the shared configuration and the
  already loaded configuration notice from startup.
- After a normal `run_native` return, the final configuration is saved, the
  marker is removed, and the save error is propagated only after marker
  cleanup. A `run_native` error returns immediately and leaves the marker in
  place. A panic likewise unwinds without normal cleanup, leaving the marker
  for recovery detection.
- `main` prints startup, GUI, save, or cleanup errors to stderr and exits with
  status `1`.
- Documented GUI launch, storage location, recovery behavior, and Task14
  product boundaries in `README.md`.

## Tests and Verification

The startup helper test covers both normal settings restoration and recovery
defaults. The complete GUI test suite passed with 18 tests.

```text
cargo test -p rustnt-gui recovery_uses_safe_defaults_and_normal_start_uses_loaded_settings -- --nocapture
PASS: 1 passed; 0 failed

cargo test -p rustnt-gui -- --nocapture
PASS: 18 passed; 0 failed

cargo check -p rustnt-gui
PASS

cargo build -p rustnt-gui
PASS: target\\debug\\rustnt-gui.exe produced

cargo clippy -p rustnt-gui --all-targets -- -D warnings
PASS

cargo fmt --all -- --check
PASS
```

`cargo` emits the existing manifest warning that the `toml` requirement
contains semver metadata that Cargo ignores. The dependency declaration was
not changed by this task.

`git diff --check` passed before staging. The protected historical
`.superpowers/sdd/task-4-report.md` and `.superpowers/sdd/progress.md` were not
included in the Task4 commit.

## Files

- `crates/rustnt-gui/src/main.rs`
- `crates/rustnt-gui/src/app.rs`
- `README.md`
- `.superpowers/sdd/task-14-task4-report.md`

## Concerns

- The native event loop was compile- and test-verified but was not launched
  interactively during this headless verification run.
- The pre-existing Cargo warning about semver metadata remains until the
  dependency requirement is cleaned up in a separate change.
