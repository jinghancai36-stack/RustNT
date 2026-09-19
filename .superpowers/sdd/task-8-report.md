# Task 08 Verification Report

## Implementation Commits

- `491e9e5` - add process-control protocol contracts and codecs.
- `29c245e` - connect process inspection, termination policy, Pipe-client
  authorization, and service dispatch.
- `088018e` - add `process inspect` and `process terminate` CLI workflows.
- `53d37b0` - use the process synchronize access right for bounded waits.
- `1edb6c1` - harden process-control authorization and query-only inspection.
- The documentation and this verification report are committed together after
  this report is created.

## Environment

- Date: 2026-09-19
- OS: Windows 11 Pro, version `10.0.26100`, build `26100`, x64
- Rust: `rustc 1.98.1`, `cargo 1.98.1`, `x86_64-pc-windows-msvc`
- Account: `ceeses\ceeses`
- Token: Medium Mandatory Level; Administrators group is present as deny-only
- Service state before and after verification: `NOT_INSTALLED`

## Automated Verification

| Command | Result |
| --- | --- |
| `cargo test --workspace -- --nocapture` | PASS; 63 tests passed, 0 failed; doctests passed |
| `cargo build --workspace` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `git diff --check` | PASS |

The 63 workspace unit tests include 17 Task08 process-control tests, 17
service/core regression tests, and 6 CLI process-command tests. The remaining
tests cover the existing process-list, service lifecycle, framing, and token
identity behavior.

## Non-Privileged CLI Smoke

| Command | Observed result |
| --- | --- |
| `rustnt service status` | `NOT_INSTALLED`, exit code `0` |
| `rustnt process inspect --pid 4` | service-not-running error, exit code `1` |
| `rustnt process terminate --pid 1` | service-not-running error, exit code `1` |
| `rustnt process list` | process table rendered successfully, exit code `0` |

## Manual Windows Validation

The Task08 service-dependent checks were not run because the current shell is
not elevated: the account is a local Administrator, but the current token is
Medium Mandatory with Administrators marked deny-only. The following steps are
therefore environment-gated and were skipped:

- install/start `RustNTControl`;
- inspect a known user process through the service;
- inspect PID 4 through the service;
- reject termination of the RustNT service;
- terminate a disposable caller-owned helper;
- inspect the helper after termination;
- stop and uninstall the service.

The service status check after the smoke run reports `NOT_INSTALLED`, so no
service registration or service process was left behind. No helper process was
created by RustNT.

## Residual Risk

The automated suite validates payload exactness, sanitization, policy
decisions, process metadata collection, service dispatch ordering, client
token authorization code paths, CLI parsing, and bounded result rendering.
Elevated Pipe authorization and end-to-end termination remain pending an
interactive Administrator/UAC environment.
