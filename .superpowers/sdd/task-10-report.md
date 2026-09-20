# Task 10 Verification Report

## Scope

Task10 adds a typed capability registry, service-side authorization, bounded
in-memory audit events, internal request IDs, and per-caller termination rate
limiting without changing protocol version 1 or command codes.

## Implementation Commits

- `5ad9a03` - specify the Task10 capability and audit design.
- `08ba604` - plan the Task10 implementation.
- `eff9a6d` - add typed capability, authorization, audit, and limiter primitives.
- `9bde8bf` - integrate capability audit into service dispatch.

## Implemented Boundary

- `Command::capability()` is the single mapping from the five protocol commands to typed capabilities.
- `PING`, `IDENTITY`, `CAPABILITIES`, and `PROCESS_INSPECT` remain read-only capabilities.
- `PROCESS_TERMINATE` requires a caller SID, an elevated token, and local Administrators membership before the existing Task08 process policy runs.
- Termination is limited to 4 requests per caller SID in a 10-second window.
- Known malformed payloads, authorization rejections, process-policy statuses, rate limits, and Windows failures emit typed audit events.
- The default audit sink retains the newest 256 events in memory; the limiter retains at most 256 caller-SID subjects.
- Rate-limited audit events omit the requested PID because it has not passed target-process validation.
- No protocol command, request field, persistent Event Log source, or audit-query endpoint was added.

## Environment

- Date: 2026-09-20
- OS: Windows 11, build `10.0.26100.0`
- Rust: `rustc 1.98.1`, `cargo 1.98.1`
- Account: `ceeses\\ceeses`
- Token: Medium Mandatory Level
- Local Administrators group: present but deny-only

## Automated Verification

| Command | Result |
| --- | --- |
| `cargo test --workspace -- --nocapture` | PASS; 77 tests passed, 0 failed; doctests passed |
| `cargo build --workspace` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `git diff --check` | PASS |

The workspace test total is 77: 15 CLI tests, 61 core tests, and 1 disposable
Task09 target test. The core tests include capability mapping, authorization
ordering, bounded audit retention, bounded limiter subjects, request IDs,
malformed known payload auditing, rejection auditing, rate-limit auditing, and
unchanged protocol command codes and frame behavior.

## Elevated Manual Validation

The following checks remain environment-gated because this shell is not an
elevated Administrator token:

- install and start `RustNTControl`;
- query service identity over the secured Named Pipe;
- inspect the disposable target through the service;
- terminate the caller-owned disposable target;
- exercise the live UAC/elevation rejection matrix and verify service-side audit behavior;
- stop and uninstall the service after the run.

The current token reports Medium Mandatory Level and the Administrators group
as deny-only. These cases are not counted as automated passes. No production
authorization bypass was added to make them pass in a non-elevated shell.

## Residual Limitations

Audit events are process-local memory only. A service restart discards them, and
there is no remote audit-query operation. The current service loop is serial and
the request ID and limiter state live only for the service process lifetime.
Persistent Windows Event Log integration and richer operational telemetry remain
future work.
