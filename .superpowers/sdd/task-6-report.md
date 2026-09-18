# RustNT Task 06 Report

## Status

Implemented the RustNT service CLI surface in `crates/rustnt-cli/src/main.rs`.
The existing `rustnt process list` route and its option parsing were preserved.

## Implementation Notes

- Added strict parsing for `install`, `uninstall`, `start`, `stop`, `status`, and `identity`.
- Missing, unknown, or extra service arguments print service usage and return exit code 2.
- Lifecycle commands call the typed `rustnt_core::service` APIs directly.
- Install resolves `rustnt-service.exe` as `current_exe().parent().join(...)`.
- Status renders the service name, state, and process ID. `NOT_INSTALLED` is a successful status result.
- Identity checks status before sending a request, does not auto-start, and reports `rustnt service start` when the service is stopped or not installed.
- Identity uses `ServiceClient::request(Command::Identity)`, decodes the typed key/value payload, and renders the required fields.
- Runtime service errors return exit code 1.
- Added the parser and identity renderer tests specified by the brief.

## TDD Evidence

Initial command, run after adding the tests and before adding the implementation:

```text
cargo test -p rustnt-cli service
```

Result: expected RED compile failure. The compiler reported unresolved imports for
`parse_service_command`, `render_identity`, and `ServiceCommand`.

After implementation, the same focused command passed 2 tests. The renderer test
name from the brief does not include `service`, so it is selected by the full CLI
test command instead.

## Verification

Commands run after implementation:

```text
cargo test -p rustnt-cli service
```

Result: 2 passed, 0 failed.

```text
cargo test -p rustnt-cli
```

Result: 10 passed, 0 failed.

```text
cargo test --workspace
```

Result: rustnt-cli 10 passed, rustnt-core 24 passed, all other test and doc-test
targets completed with 0 failures.

```text
cargo fmt --all -- --check
```

Result: exit code 0.

```text
cargo clippy -p rustnt-cli --all-targets -- -D warnings
```

Result: exit code 0 with no warnings or errors.

```text
git diff --check
```

Result: exit code 0.

## Concerns

- The verification environment did not perform live Windows Service Control
  Manager operations or a running named-pipe identity request. Those paths use
  the existing typed core APIs and require an installed service with appropriate
  Windows permissions to exercise end to end.
- The focused `service` filter selects two parser tests; the identity renderer
  is covered by the full CLI test run because its required test name is
  `renders_identity_fields`.

## Review / Fix Record

Review finding: `install_service` passed an unquoted executable path directly
to `CreateServiceW`. A path such as `C:\Program Files\RustNT\rustnt-service.exe`
could therefore be parsed incorrectly by the service process launcher and
created an unquoted service path risk.

Fix: added the Windows-only `format_service_binary_path` helper in
`crates/rustnt-core/src/service.rs`. It preserves paths without spaces and
wraps paths containing spaces in one pair of double quotes before
`CreateServiceW` receives the UTF-16 command line. Added a unit test covering
both cases; the CLI Task06 routing and output were not changed.

TDD evidence for the review fix:

```text
cargo test -p rustnt-core service_binary_path_is_quoted_only_when_it_contains_spaces
```

Initial result: expected RED compile failure because the helper did not exist.
Final result: 1 passed, 0 failed.

Post-fix verification:

```text
cargo test --workspace
```

Result: rustnt-cli 10 passed, rustnt-core 25 passed, all other test and
doc-test targets completed with 0 failures.

```text
cargo fmt --all -- --check
cargo clippy -p rustnt-cli --all-targets -- -D warnings
git diff --check
```

Result: all three commands exited with code 0.
