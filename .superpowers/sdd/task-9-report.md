# Task 09 Verification Report

## Scope

Task09 adds a dependency-free disposable target process and validates the
Task08 service boundary without changing the process-control protocol or
authorization policy.

## Implementation Commits

- `0c414f3` - specify Task09 privileged end-to-end validation.
- `676901a` - plan Task09 validation.
- `e19c805` - add the disposable `rustnt-task09-target` binary.

## Environment

- Date: 2026-09-19
- OS version: `10.0.26100.0`
- Rust: `rustc 1.98.1`, `cargo 1.98.1`
- Account: `ceeses\ceeses`
- Token: Medium Mandatory Level
- Local Administrators group: present but deny-only
- Service state before and after preflight: `NOT_INSTALLED`

## Automated Preflight

| Command | Result |
| --- | --- |
| `cargo test --workspace -- --nocapture` | PASS; 64 tests passed, 0 failed; doctests passed |
| `cargo build --workspace` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `git diff --check` | PASS |
| `cargo run -q -p rustnt-cli -- service status` | PASS; `NOT_INSTALLED` |

The 64 workspace tests include 15 CLI tests, 48 core/service tests, and one
disposable-target test.

## Disposable Target Smoke

The target was run in a foreground PTY with:

```text
cargo run -q -p rustnt-task09-target
```

Observed startup output:

```text
RUSTNT_TASK09_TARGET_PID=41772
```

The process was stopped with Ctrl+C. A follow-up process query found no
`rustnt-task09-target` process. The RustNT service remained `NOT_INSTALLED`.

## Elevated Manual Validation

The following checks were skipped because the current shell is not elevated:

- install and start `RustNTControl`;
- query service identity over the Named Pipe;
- inspect the disposable target through the service;
- terminate the caller-owned disposable target;
- reject PID 4, the service PID, system-owned targets, and stale creation time;
- stop and uninstall the service after the run.

The current token reports Medium Mandatory Level and the Administrators group
as deny-only. These cases are environment-gated and are not counted as passes.

## Cleanup and Residual Risk

- Service registration: none left by Task09.
- Service process: none left by Task09.
- Disposable target process: none left by Task09.
- Residual risk: elevated Administrator/UAC end-to-end behavior still requires
  an interactive elevated terminal. No production authorization bypass was
  added to make the non-elevated environment pass.
