# Task 2 Report: Runtime Marker and Crash Recovery

## Status

Implemented Task14 Task2 in the assigned worktree. Scope is limited to runtime
marker lifecycle, recovery state inspection, crash-log appending, panic-hook
installation, and the `recovery` module declaration. GUI startup and `app.rs`
were not implemented.

## TDD

### RED

Added the three recovery tests first and ran:

```text
cargo test -p rustnt-gui recovery::tests -- --nocapture
```

The command failed to compile because `RecoveryState`, `inspect`,
`create_marker`, and `append_crash_record` did not exist. This was the expected
missing-interface failure.

### GREEN

Implemented the smallest interfaces required by the tests and reran the same
command. All three recovery tests passed.

## Implementation

- `RecoveryState` reports whether `ConfigPaths::marker_file` already exists.
- `create_marker` creates the config root and writes PID plus Unix start time.
- `RuntimeMarker::remove` explicitly removes the marker and treats a missing
  file as successful; it has no `Drop` cleanup.
- `append_crash_record` appends a timestamped, single-line panic summary and
  returns I/O failures.
- `install_panic_hook` captures the previous hook, ignores crash-log write
  failures, and delegates to the previous hook.
- `main.rs` declares `pub mod recovery;` without adding GUI startup.

## Tests and Checks

- `cargo test -p rustnt-gui recovery::tests -- --nocapture`: 3 passed.
- `cargo test -p rustnt-gui config::tests -- --nocapture`: 7 passed.
- `cargo clippy -p rustnt-gui --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.

Cargo emits an existing manifest warning that the `toml` version requirement
contains semver metadata; it is unrelated to this task and does not fail the
checks.

## Files

- `crates/rustnt-gui/src/recovery.rs`
- `crates/rustnt-gui/src/main.rs`

## Concerns

- The panic hook is intentionally process-global and delegates to the hook
  captured at installation time. Repeated installation should remain limited
  to application startup.
- The existing `toml` manifest metadata warning remains unchanged.

## Review Follow-up

Addressed review findings without connecting GUI startup:

- Extended the marker test to assert that dropping `RuntimeMarker` leaves the
  marker file in place, and that only explicit `remove()` deletes it.
- Added a `make_panic_hook` constructor that accepts the previous hook. The
  production installer uses this same closure, while tests can exercise it
  directly without depending on the process-global hook for behavior.
- Added serialized tests using a temporary global-hook guard. They restore the
  previous hook after each test and verify that a log-path write failure does
  not panic, the previous hook is called, CR/LF input becomes one log line, and
  the runtime marker remains after panic.

## Review Follow-up Verification

### RED

After adding the new tests, the recovery test command failed to compile because
`make_panic_hook` was not yet defined. This was the expected missing-test-hook
constructor failure.

### GREEN and Final Checks

```text
cargo test -p rustnt-gui recovery::tests -- --nocapture
warning: crates\rustnt-gui\Cargo.toml: version requirement `1.1.6+spec-1.1.0` for dependency `toml` includes semver metadata which will be ignored, removing the metadata is recommended to avoid confusion
warning: `rustnt-gui` (manifest) generated 1 warning
running 5 tests
test recovery::tests::inspect_reports_normal_start_without_marker ... ok
test recovery::tests::failed_crash_log_append_is_reported_without_panicking ... ok
test recovery::tests::marker_is_detected_and_removed_only_explicitly ... ok
test recovery::tests::panic_hook_writes_one_line_and_keeps_marker_after_panic ... ok
test recovery::tests::panic_hook_ignores_log_failure_and_calls_previous_hook ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out
```

```text
cargo test -p rustnt-gui config::tests -- --nocapture
warning: crates\rustnt-gui\Cargo.toml: version requirement `1.1.6+spec-1.1.0` for dependency `toml` includes semver metadata which will be ignored, removing the metadata is recommended to avoid confusion
warning: `rustnt-gui` (manifest) generated 1 warning
running 7 tests
test config::tests::defaults_use_safe_dark_window_settings ... ok
test config::tests::root_constructor_uses_expected_filenames_without_creating_directories ... ok
test config::tests::missing_config_returns_defaults_without_notice ... ok
test config::tests::invalid_toml_returns_defaults_with_configuration_notice ... ok
test config::tests::invalid_theme_and_geometry_fall_back_without_panicking ... ok
test config::tests::valid_toml_round_trips_theme_dimensions_and_position ... ok
test config::tests::saving_twice_replaces_existing_config_and_cleans_temporary_file ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out
```

```text
cargo fmt --all -- --check
exit code: 0
```

```text
cargo clippy -p rustnt-gui --all-targets -- -D warnings
warning: crates\rustnt-gui\Cargo.toml: version requirement `1.1.6+spec-1.1.0` for dependency `toml` includes semver metadata which will be ignored, removing the metadata is recommended to avoid confusion
warning: `rustnt-gui` (manifest) generated 1 warning
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit code: 0
```

The manifest warning is pre-existing and unrelated to this change.
