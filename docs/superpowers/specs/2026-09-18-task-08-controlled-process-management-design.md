# Task 08 Controlled Process Management Design

**Date:** 2026-09-18

## Goal

Add a narrowly scoped process-management capability to the existing RustNT
LocalSystem service bridge without turning it into an arbitrary command or
script execution endpoint.

Task 08 introduces read-only process inspection and an explicitly authorized
process termination operation. The operation is limited to user-owned
processes, requires an elevated local administrator client, and verifies the
target process creation time before acting.

## Scope

Task 08 adds:

- A service protocol command for inspecting one process by PID.
- A service protocol command for terminating one validated process.
- CLI commands:

  ```text
  rustnt process inspect --pid <PID>
  rustnt process terminate --pid <PID>
  ```

- PID and process-creation-time validation to detect PID reuse.
- Client-token authorization for destructive operations.
- Protection for the RustNT service, core system PIDs, and system-owned
  processes.
- Bounded termination wait and deterministic error reporting.
- Unit tests for protocol payloads, target policy, CLI parsing, and rejection
  paths.
- Windows integration/manual coverage for inspect, authorization, termination,
  and cleanup.

Task 08 does not add:

- Arbitrary executable paths, command lines, scripts, PowerShell, WMI, or
  shell integration.
- Batch termination, wildcard targets, name-based termination, or remote
  process control.
- Suspension, resumption, priority changes, DLL injection, debugging, or
  token manipulation.
- Termination of `System`, PID 0, PID 4, the RustNT service, or processes
  owned by another account.
- A general administrator-command endpoint.
- Kernel-mode code, drivers, GUI code, network listeners, or undocumented NT
  APIs.

The operation is intentionally forceful: the implementation uses the
documented Windows process termination API. It must be presented as a
destructive action and must not accept a caller-provided exit code.

## Design Principles

1. **The service remains the privileged boundary.** The CLI does not open a
   target process with termination rights and does not perform elevation.
2. **Every destructive request is explicit.** A request contains one PID and
   one expected creation timestamp; there is no free-form operation string.
3. **PID reuse is rejected.** The service opens the target, reads its creation
   time, and compares it with the expected value before termination.
4. **Authorization is operation-level.** Named Pipe ACLs are not sufficient.
   The service validates the connecting client token for every terminate
   request.
5. **The initial policy is conservative.** Task 08 supports terminating a
   caller-owned user process only. System-owned and cross-user targets are
   rejected even when the service has LocalSystem privileges.
6. **Failures are connection-local.** A malformed or unauthorized request
   produces an error response and does not stop the service.

## Architecture

The existing dependency direction remains:

```text
rustnt-cli
    |
    v
rustnt-core::service
    |
    +--> Named Pipe client/server
    +--> Client token authorization
    +--> Target process token and creation time
    +--> Process termination
    |
    v
rustnt-service
```

`rustnt-core` owns all Windows API calls, protocol framing, target-policy
decisions, and HANDLE lifetime. `rustnt-cli` parses arguments, performs the
two-step terminate workflow, and renders typed results. `rustnt-service`
continues to be a thin entry point.

The existing service lifecycle and `ACK1` response confirmation remain
unchanged. New commands use the same local Pipe, message framing, explicit
DACL, remote-client rejection, one-request-per-connection behavior, and
five-second response acknowledgement timeout.

## Protocol

The existing protocol version remains `1`. Existing command codes are
unchanged:

```text
0  PING
1  IDENTITY
2  CAPABILITIES
```

Task 08 adds:

```text
3  PROCESS_INSPECT
4  PROCESS_TERMINATE
```

The command enum, decoder, dispatcher, and capability response must all be
updated together. The capability list becomes:

```text
ping,identity,capabilities,process_inspect,process_terminate
```

### `PROCESS_INSPECT`

Request payload:

```text
pid[u32]
```

The payload must be exactly four bytes and the PID must be nonzero. The
service opens the target with query-only rights and returns a bounded
key-value payload:

```text
PID=<decimal>
CREATION_TIME_100NS=<decimal>
IMAGE_NAME=<sanitized UTF-8>
IMAGE_PATH=<sanitized UTF-8 or N/A>
THREADS=<decimal>
MEMORY_BYTES=<decimal or N/A>
OWNER_SID=<string>
```

The response contains no command line, environment, handles, token contents,
or arbitrary target data. Newline, carriage-return, equals-sign, and other
field-breaking characters must be rejected or rendered as a safe replacement.
The response remains within `MAX_PAYLOAD`.

Inspection is read-only, but it still refuses PID 0, PID 4, and the RustNT
service process. It may report a structured `TARGET_NOT_FOUND` or
`TARGET_ACCESS_DENIED` response instead of terminating the connection.

### `PROCESS_TERMINATE`

Request payload:

```text
pid[u32] | expected_creation_time_100ns[u64]
```

The payload must be exactly twelve bytes. The service performs these checks in
order:

1. The request shape and PID are valid.
2. The connecting client is local, elevated, and a member of the local
   Administrators group.
3. The target PID is not 0, 4, or the service's own PID.
4. The target process can be opened with
   `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE`.
5. The target creation time equals the expected creation time.
6. The target process owner SID equals the authenticated caller's user SID.
7. The target is not classified as a protected/system-owned process.

Only after all checks pass may the service call `TerminateProcess` with a
fixed nonzero exit code owned by the implementation. The request cannot
select an exit code, timeout, access mask, or alternate API.

The service waits for the target handle for at most two seconds. The response
reports one of:

```text
TERMINATED
TERMINATE_PENDING
TARGET_NOT_FOUND
PID_REUSED
TARGET_NOT_OWNED
TARGET_PROTECTED
CALLER_NOT_ELEVATED
CALLER_NOT_ADMIN
ACCESS_DENIED
```

`TERMINATE_PENDING` means the termination request was accepted but the target
did not signal within the bounded wait. It is not permission to retry blindly;
the CLI must instruct the caller to inspect the target again.

## Authorization and Target Policy

The service must authenticate the connecting Pipe client for every destructive
request. The planned implementation uses the named-pipe client token APIs to
inspect the caller, requires local administrator membership and an elevated
token, and records the caller's user SID for target ownership comparison.
The service must revert impersonation before using the LocalSystem service
token to open or terminate the target.

The policy is:

| Target or caller | Result |
| --- | --- |
| Caller is not elevated | Reject |
| Caller is not a local administrator | Reject |
| PID 0 or PID 4 | Reject |
| RustNT service process | Reject |
| Target owner is SYSTEM, LocalService, or NetworkService | Reject |
| Target owner differs from caller SID | Reject |
| Target creation time differs from request | Reject |
| Caller-owned normal user process | Eligible for termination |

The policy must be represented by pure functions over typed metadata wherever
possible, so rejection behavior can be tested without creating or terminating
real processes. It must not depend on process names alone; names are
ambiguous and can be changed.

## CLI Workflow

### Inspect

```text
rustnt process inspect --pid <PID>
```

The CLI validates that `<PID>` is a positive decimal `u32`, requires the
service to be running, sends `PROCESS_INSPECT`, checks the response status,
decodes the bounded fields, and renders them without implying that all fields
are available.

### Terminate

```text
rustnt process terminate --pid <PID>
```

The CLI validates the PID, requests an inspection from the service, extracts
the returned creation time, and submits a terminate request containing the
PID and that expected timestamp. The service rechecks the timestamp under its
own process handle immediately before termination. A mismatch is reported as
`PID_REUSED` or `TARGET_NOT_FOUND`; the CLI must not retry automatically.

The CLI must never accept a process name, wildcard, executable path, command
line, arbitrary exit code, or hidden `--force` bypass. Usage errors return
exit code `2`; service, authorization, target, and Win32 failures return
exit code `1`.

## Error and Resource Handling

- All new Win32 calls have small `unsafe` blocks with nearby safety comments.
- Process, token, and Pipe handles use existing or equivalent RAII wrappers.
- The service never retains a target HANDLE after the request completes.
- Target lookup races are reported as typed errors, not panics.
- Malformed payloads are rejected before any process HANDLE is opened.
- A failed authorization check must not reveal target metadata beyond the
  documented error class.
- A failed termination must not cause the service host to exit.
- Audit diagnostics should include operation, PID, caller SID when available,
  and outcome, but must not include command lines or token secrets.

## Testing

### Unit tests

Add focused tests for:

- Encoding and decoding the exact four-byte inspect payload.
- Encoding and decoding the exact twelve-byte terminate payload.
- Rejection of zero PID, truncated payloads, oversized payloads, and trailing
  bytes.
- Command-code mapping for `PROCESS_INSPECT` and `PROCESS_TERMINATE`.
- Capability output containing the two new fixed command names.
- Sanitization/rejection of field-breaking process metadata.
- Target-policy decisions for self, PID 0, PID 4, system-owned, foreign-user,
  creation-time mismatch, non-admin caller, non-elevated caller, and eligible
  caller-owned targets.
- CLI parsing for both commands, invalid PID text, zero PID, missing values,
  unknown options, and extra arguments.

### Windows integration and manual checks

With an elevated local administrator:

1. Install and start `RustNTControl`.
2. Run `rustnt process inspect --pid <known user-process>`.
3. Confirm PID, creation timestamp, owner SID, image name, and best-effort
   metrics are rendered.
4. Confirm `rustnt process inspect --pid 4` is rejected.
5. Confirm `rustnt process terminate --pid <RustNT service PID>` is rejected.
6. Start a disposable user-owned helper process and terminate it through the
   CLI.
7. Confirm a second inspection reports `TARGET_NOT_FOUND` or an equivalent
   stopped state.
8. Stop and uninstall the service in cleanup.

The helper process must be disposable, must not be a system service, and must
not be started through a shell command as part of RustNT's implementation.
If UAC or administrator interaction is unavailable, record the exact skipped
steps and leave the service uninstalled.

## Documentation

Update:

- `README.md` with inspect and terminate usage plus the destructive-operation
  warning.
- `docs/architecture.md` with the process-management path, authorization
  checks, PID reuse defense, and service boundary.
- `docs/learning-notes.md` with process HANDLE rights, PID reuse, token
  ownership, impersonation/revert, and why Pipe ACLs are not authorization.
- `docs/benchmarks.md` with inspect latency, authorization rejection latency,
  terminate-to-stopped latency, and the target/test environment.

## Completion Criteria

Task 08 is complete when:

- The protocol exposes only the two fixed process-management commands in
  addition to the existing three.
- `inspect` is read-only and returns bounded, sanitized target metadata.
- `terminate` requires an elevated local administrator and caller-owned target.
- PID reuse is rejected through creation-time comparison.
- PID 0, PID 4, the RustNT service, and system-owned targets are rejected.
- No arbitrary command, script, executable path, batch operation, or remote
  process-control path exists.
- Unit tests and non-privileged workspace verification pass.
- Elevated manual validation is passing or explicitly documented as
  environment-gated, with no residual service or helper process.
