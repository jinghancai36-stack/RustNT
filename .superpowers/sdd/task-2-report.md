# Task 2 Report: Fixed shell switch targets and registry validation

## Status

Implemented the pure Rust policy portion of Task 2 in commit
`feat: validate fixed shell switch targets`. The assigned checkout does not
contain the planned `rustnt-shell-helper` crate, so no Windows registry adapter
or helper manifest was added. The policy crate remains free of Registry API
dependencies and accepts an injected `WinlogonStore`.

## Implementation

- Added fixed `WinlogonValue::{Shell, Userinit}` names and `WinlogonStore`.
- Added strict `REG_SZ` UTF-16 validation with NUL termination, embedded-NUL,
  malformed, empty, and 4096-byte bound checks.
- Added fixed-version `InstallManifest` parsing with unknown-field rejection.
- Added installation path verification for `Program Files\\RustNT` and the
  three fixed executable names, including missing-file, symlink, and escape
  rejection.

## TDD evidence

The required rejection test was written first and failed to compile because
`validate_registry_string` was missing. After the minimal implementation, the
focused test passed; the remaining boundary tests were then added.

## Verification

- `cargo test -p rustnt-shell-switch`: 12 passed.
- `cargo clippy -p rustnt-shell-switch --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

Cargo emits the existing workspace warning about semver metadata in the `toml`
version requirement; it is unrelated to this change.

## Concerns

The helper crate, Windows known-folder lookup, ACL verification, and real HKLM
access are intentionally deferred to the later helper-crate task because that
crate is absent from this checkout.
