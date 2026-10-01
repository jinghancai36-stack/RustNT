# Task17A Task 5 Review Fix Report

## Scope

This pass addresses the second review findings for Task17A persistence and
session ownership. The unrelated existing modification in
`.superpowers/sdd/task-14-task4-report.md` was preserved.

## RED

Added regression tests before the implementation changes for:

- `format_version` session markers and unknown-version byte preservation;
- bounded session reads and oversized-file rejection;
- versioned, strict recovery-log parsing with malformed and invalid-UTF-8
  byte preservation;
- nonce-based stale handles when `pid` and `started_at` are identical;
- a Windows child-process test proving the named coordinator is shared across
  processes;
- bounded snapshot load and save paths.

The first focused test run failed at compile time because the new
`MAX_SESSION_BYTES` and `nonce` behavior did not yet exist. This was the
expected RED state.

## GREEN

- Session markers now use `format_version`, a bounded generated nonce, and
  ownership checks over `format_version`, `pid`, `started_at`, and nonce.
- Windows uses the stable `Local\\RustNT.ShellRecovery.Session` named mutex
  with `CreateMutexW`, `WaitForSingleObject`, `ReleaseMutex`, and
  `CloseHandle`. Non-Windows tests use a process-local `Mutex`.
- All session marker reads, writes, and deletion paths run under the same
  coordinator guard.
- Session reads consume at most `MAX_SESSION_BYTES + 1`; unknown versions and
  oversized files return readable errors without rewriting the original.
  Malformed TOML and invalid UTF-8 retain the existing safe replacement path.
- Recovery logs begin with `format_version=1`. Existing logs must be valid
  UTF-8, have the supported header, end on a complete line, and contain valid
  event fields. Unknown versions and malformed content return errors without
  lossy rewrite or deletion.
- Snapshot save and load paths enforce `MAX_SNAPSHOT_BYTES`.

## Verification

The focused recovery test suite passed with 20 tests, including the Windows
cross-process named-mutex test. The focused snapshot suite passed with 16
tests, including oversized save/load coverage.

Final command results are recorded below after the full verification pass:

```text
cargo fmt --all -- --check: PASS
cargo test -p rustnt-gui shell_recovery -- --nocapture: PASS
cargo clippy -p rustnt-gui --all-targets -- -D warnings: PASS
git diff --check: PASS
```

The existing Cargo warning about ignored semver metadata in the `toml`
dependency remains unchanged and is outside Task17A Task 5.

## Third Review Fixes

This pass keeps malformed `shell.session` bytes intact and blocks startup with
`RecoveryLogError` for invalid UTF-8, invalid TOML, missing required fields,
invalid states, and other invalid fields. The existing unknown
`format_version` error remains unchanged. The malformed-session, invalid-UTF-8,
unknown-state, and missing-field tests now assert errors and exact byte
equality.

`record_snapshot_result(Err(String::new()))` now records the bounded summary
`unspecified snapshot error`, so later append and normal completion continue to
parse successfully. `save_snapshot` rejects unsupported versions before
creating a temporary file and preserves the existing target bytes.

Verification:

- `cargo fmt --all -- --check`: PASS
- `cargo test -p rustnt-gui shell_recovery -- --nocapture`: 22 passed.
- `cargo test -p rustnt-gui shell_snapshot -- --nocapture`: 17 passed.
- `cargo clippy -p rustnt-gui --all-targets -- -D warnings`: PASS
- `git diff --check`: PASS

The two focused test filters were run separately because Cargo accepts one test
filter argument. The existing Cargo warning about ignored semver metadata in
the `toml` dependency remains unchanged.
