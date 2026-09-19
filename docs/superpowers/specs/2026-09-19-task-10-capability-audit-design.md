# Task 10 Capability Authorization and Audit Design

**Date:** 2026-09-19

## Goal

Create a typed, testable authorization boundary for the existing service
commands and record bounded operation outcomes without changing protocol
version 1 or adding arbitrary command execution.

## Scope

Task10 adds:

- A fixed `Capability` enum for the five existing commands.
- A typed `RequestContext` carrying an internal request ID and caller facts
  when the operation is destructive.
- A single capability authorization function used by service dispatch.
- A bounded in-memory `AuditSink` abstraction with a test implementation.
- Structured audit events for completed, rejected, failed, malformed, and
  rate-limited requests.
- A per-caller-SID rate limiter for destructive process termination requests.
- Tests for capability mapping, authorization decisions, audit retention, rate
  limiting, request IDs, and service dispatch integration.

Task10 does not add:

- New protocol command codes or request fields.
- A remote audit-query endpoint.
- Persistent audit storage or Windows Event Log registration.
- New process-control operations.
- Arbitrary commands, scripts, PowerShell, WMI, or shell execution.
- A bypass for Task08's process ownership, PID reuse, elevation, or
  protected-target checks.

## Fixed Capability Registry

The registry is internal and deterministic:

| Command | Capability name | Destructive |
| --- | --- | --- |
| `PING` | `ping` | No |
| `IDENTITY` | `identity` | No |
| `CAPABILITIES` | `capabilities` | No |
| `PROCESS_INSPECT` | `process_inspect` | No |
| `PROCESS_TERMINATE` | `process_terminate` | Yes |

The capability response continues to use the existing fixed string:

```text
ping,identity,capabilities,process_inspect,process_terminate
```

The `Command` to `Capability` mapping is the only source used by dispatch,
capability rendering, and audit records. Protocol command codes remain 0
through 4.

## Authorization

The service creates a `RequestContext` after a request frame has been decoded:

```text
request_id
capability
client_sid (when queried)
caller_elevated
caller_administrator
```

Read-only capabilities are allowed after transport and frame validation.
`PROCESS_TERMINATE` additionally requires a client SID, an elevated token, and
local Administrators membership before the existing Task08 target policy runs.
The Task08 checks remain in `terminate_process` as defense in depth.

Authorization failures use the existing fixed process status names where they
already exist. Rate-limit rejection is a bounded service error and does not
introduce a new protocol status.

## Audit Events

The audit model is typed and contains no arbitrary payload text:

```text
request_id
capability
outcome: SUCCEEDED | REJECTED | FAILED
reason: fixed optional reason
target_pid: optional PID
client_sid: optional caller SID
```

Reasons are fixed enum values such as `CALLER_NOT_ELEVATED`,
`CALLER_NOT_ADMIN`, `TARGET_PROTECTED`, `PID_REUSED`, `TARGET_NOT_OWNED`,
`ACCESS_DENIED`, `MALFORMED_PAYLOAD`, `RATE_LIMITED`, and `WINDOWS_FAILURE`.
Audit records never include command lines, environments, token contents,
arbitrary request bytes, or executable arguments.

The default service runtime owns a bounded in-memory sink containing the most
recent 256 events. A sink interface allows tests and a future persistent
Windows Event Log adapter to receive the same typed events. Task10 does not
add an audit-query protocol command.

## Rate Limiting

Only `PROCESS_TERMINATE` is rate-limited. The initial fixed policy is:

```text
maximum: 4 requests
window: 10 seconds
key: authenticated caller SID
```

The limiter is checked after caller identity and capability authorization but
before opening the target process. Expired entries are removed lazily. A
rejected request is audited as `RATE_LIMITED` and returns a bounded service
error without target metadata.

## Request Flow

```text
decode frame
    -> assign request ID
    -> map command to fixed capability
    -> query caller only for destructive capability
    -> authorize capability
    -> apply destructive rate limit
    -> execute existing command policy
    -> emit one bounded audit event
    -> return existing response format
```

Malformed frames that cannot identify a command remain connection-local
protocol errors and are not exposed through a new endpoint. Malformed payloads
for a known command are recorded as `MALFORMED_PAYLOAD`.

## Testing

Unit tests must cover:

- exact command-to-capability mapping and fixed capability names;
- read-only authorization and destructive authorization rejection order;
- bounded audit retention and fixed event fields;
- rate-limit burst rejection and window expiry;
- monotonically increasing request IDs;
- service dispatch audit events for success, malformed payload, rejection, and
  rate-limited termination requests;
- unchanged protocol command codes and payload frames.

The full workspace test, build, Clippy, formatting, and diff checks remain the
final verification gate.
