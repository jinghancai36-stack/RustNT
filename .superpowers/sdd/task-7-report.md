# Task07 Documentation Report

## Scope

Baseline: `650d9d7` (`fix: quote Windows service binary path`).

The Task07 update covered the requested documentation and progress/report artifacts:

- `README.md`
- `docs/architecture.md`
- `docs/learning-notes.md`
- `docs/benchmarks.md`
- `.superpowers/sdd/progress.md`
- `.superpowers/sdd/task-7-report.md`

No Rust source files were changed.

## Requirement checklist

- [x] Document the Windows service as running under `LocalSystem`.
- [x] Document demand-start registration and the `RustNTControl` service name.
- [x] Document the secured local named pipe `\\.\pipe\RustNT.Control.v1`.
- [x] Document the protocol command set as `PING`, `IDENTITY`, and `CAPABILITIES` only.
- [x] State that the bridge does not expose process control, arbitrary admin commands, or arbitrary script execution.
- [x] Document the service lifecycle commands and status/identity behavior.
- [x] Include the requested architecture flow and implementation boundaries.
- [x] Identify Task04 and Task05 as future product-scope labels and state that
      the current work only establishes the service-bridge foundation.
- [x] State that process control and administrator command execution are not
      implemented and remain separately reviewed future scopes.
- [x] Document the four-byte `ACK1` response acknowledgement on the same Pipe
      connection, the 5-second wait, and its transport-level purpose.
- [x] Add a reproducible benchmark checklist/template without invented measurements.
- [x] Include Windows version, account, elevation, cold/warm state, and install state in the benchmark checklist.
- [x] Remove trailing whitespace reported by `git diff --check` in `docs/benchmarks.md`.
- [x] Append Task07 progress.

## Verification

Commands run from the repository root:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS; no output |
| `cargo test --workspace` | PASS; 35 tests passed, 0 failed; doctests passed |
| `cargo build --workspace` | PASS; all workspace crates built |
| `git diff --check` | PASS; no whitespace errors |

The non-elevated service status check also passed:

```text
SERVICE       RustNTControl
STATE         NOT_INSTALLED
PROCESS       N/A
EXIT_CODE=0
SERVICE_STATE=NOT_INSTALLED
```

The protocol documentation now matches the implementation's response
handshake: the client reads the response frame and sends `ACK1` on the same
connection; the service waits up to 5 seconds for that confirmation, then
abandons an unacknowledged connection and returns to its one-instance accept
loop. `ACK1` is transport confirmation and is not an additional command.

## Concerns

A fresh elevated install/start/status/identity/stop/uninstall smoke test was attempted with `Start-Process -Verb RunAs`, but the UAC prompt did not return in this session, so the command was interrupted before producing a result. A follow-up non-elevated status check confirmed that no service was installed or left running.

The benchmark section intentionally contains no measured numbers. It provides the required reproducible measurement procedure and result template for a Windows environment with elevation available.
