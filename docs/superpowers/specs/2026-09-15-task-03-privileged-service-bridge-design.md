# Task 03 Privileged Service Bridge Design

**Date:** 2026-09-15

## Goal

Add a LocalSystem Windows Service and a restricted Named Pipe bridge so the
RustNT CLI can query service identity and capabilities without executing
arbitrary commands.

## Scope

Task 03 adds:

- A new `rustnt-service` Windows Service binary.
- Native Win32 Service Control Manager lifecycle operations.
- A versioned local Named Pipe protocol.
- `PING`, `IDENTITY`, and `CAPABILITIES` read-only commands.
- Service identity reporting for account, integrity level, elevation, protocol,
  and supported capabilities.
- Explicit Pipe access restrictions and remote-client rejection.
- CLI commands for service installation, lifecycle, status, and identity.
- Protocol, CLI, and Windows lifecycle tests.

Task 03 does not add:

- Process termination, suspension, resumption, or priority changes.
- Arbitrary administrator command execution.
- Shell, PowerShell, WMI, `sc.exe`, or other external command integration.
- GUI, async runtimes, network listeners, ETW, drivers, or kernel-mode code.
- Automatic service startup.

Process-control operations are reserved for Task 04. Administrator command
execution is reserved for a separately reviewed Task 05 design with explicit
authorization, auditing, and operation restrictions.

## Architecture

The workspace will use this dependency direction:

```text
rustnt-cli
    |
    v
rustnt-core::service
    |
    +--> Service Control Manager
    +--> Named Pipe client/server
    +--> Process token APIs
    |
    v
rustnt-service
```

`rustnt-core::service` remains the only layer that calls the Windows APIs used
by this feature. The CLI will use Rust interfaces for SCM and IPC operations.
The service binary will be a thin entry point that calls the service host
implementation in `rustnt-core`.

The service will be registered as:

```text
Service Name: RustNTControl
Display Name: RustNT Control Service
Account: LocalSystem
Start Type: SERVICE_DEMAND_START
```

The service will be started explicitly by the CLI or Service Control Manager.
It will not run automatically at boot during Task 03.

## Components

### `rustnt-core::service`

The service module will provide:

- Service installation, removal, start, stop, and status operations.
- Service entry-point and control-handler support.
- Named Pipe client and server loops.
- Token identity collection.
- Protocol frame encoding and decoding.
- RAII wrappers for SCM, service, Pipe, and token HANDLE values.

No raw HANDLE or Windows-specific structure will cross into the CLI API.

### `rustnt-service`

The binary will:

1. Enter the Windows Service dispatcher.
2. Report service state transitions to the SCM.
3. Start the synchronous Pipe server.
4. Accept one request per client connection.
5. Exit cleanly after `STOP` or `SHUTDOWN`.

The service will support only `STOP` and `SHUTDOWN` controls. Pause, continue,
reload, and dynamic configuration are outside Task 03.

### `rustnt-cli`

The CLI will add:

```text
rustnt service install
rustnt service uninstall
rustnt service start
rustnt service stop
rustnt service status
rustnt service identity
```

`identity` will connect to the Pipe and will not start the service implicitly.
When the service is stopped, the CLI will report that `service start` is
required.

## IPC Protocol

The fixed Pipe name is:

```text
\\.\pipe\RustNT.Control.v1
```

Requests use this frame:

```text
magic[4] | version[u16] | command[u16] | payload_length[u32] | payload
```

Responses use this frame:

```text
magic[4] | version[u16] | status[u32] | payload_length[u32] | payload
```

The protocol uses little-endian integer fields and UTF-8 payload text where a
command returns human-readable values. No JSON serializer or asynchronous
runtime is required.

The first protocol version supports:

```text
Command 1: PING
Command 2: IDENTITY
Command 3: CAPABILITIES
```

`PING` returns a successful empty response. `IDENTITY` returns key-value lines
such as:

```text
SERVICE=RustNTControl
ACCOUNT=LocalSystem
INTEGRITY=System
ELEVATED=true
PROTOCOL=1
```

`CAPABILITIES` returns the supported command names:

```text
ping,identity,capabilities
```

Every request is checked for the expected magic, protocol version, known
command, and a payload length no larger than 4096 bytes. Unknown commands,
malformed frames, and oversized payloads receive an error response and do not
reach an operation handler.

The server handles one request per connection and closes the Pipe after the
response. This keeps the first implementation synchronous and makes the
authorization boundary easy to inspect.

## Security Boundary

The service runs as `LocalSystem`, which is intentionally high privilege and
must not become an unrestricted local command endpoint.

The Pipe server will use:

```text
PIPE_ACCESS_DUPLEX
PIPE_TYPE_MESSAGE
PIPE_READMODE_MESSAGE
PIPE_WAIT
PIPE_REJECT_REMOTE_CLIENTS
```

The Pipe will receive an explicit DACL:

```text
SYSTEM              full control
Administrators      read/write
Interactive Users   read/write
Remote clients      rejected
```

Task 03 operations are read-only. Before Task 04 or Task 05 adds mutating or
command-execution operations, the bridge must add client identity checks based
on the connecting token and operation-level authorization. Pipe ACLs alone
will not be treated as sufficient authorization for dangerous operations.

The service will not accept a command line, executable path, arbitrary
arguments, or opaque script payload. Only the fixed command enum is dispatched.

## Service Lifecycle

### Install

`rustnt service install` will:

1. Resolve the sibling `rustnt-service.exe` path.
2. Convert it to an absolute path.
3. Open the SCM with the required access.
4. Register `RustNTControl` for demand start under LocalSystem.
5. Treat an existing matching registration as success.

It will not call `sc.exe`, PowerShell, or write service registry keys directly.
SCM APIs are the only installation mechanism.

### Start and Stop

`start` and `stop` will be idempotent for already-running and already-stopped
states. The CLI will query service status until the requested terminal state
is reached or a bounded timeout expires.

The service reports:

```text
START_PENDING
RUNNING
STOP_PENDING
STOPPED
```

### Status

`status` will report `NOT_INSTALLED` when the service cannot be opened because
it is absent. Installed states will include the SCM state and, when available,
the process ID.

### Uninstall

`uninstall` will refuse to delete a running service. The operator must stop it
first so the service binary and Pipe resources are released predictably.

## Identity Collection

The service will inspect its own process token and return:

- Service name.
- Account name or a stable LocalSystem label.
- Integrity level.
- Elevated status.
- Protocol version.
- Fixed capability list.

Identity collection failure is an IPC operation error, not a process crash.
The service will return a structured error status and keep serving subsequent
valid requests.

## Error Model

CLI exit codes:

```text
0  success
1  SCM, IPC, service, or Windows runtime failure
2  invalid command-line usage
```

Errors will identify the operation and preserve the Win32 error code where
available. Normal per-request failures will not terminate the service loop.
Fatal service initialization failures will be reported to the SCM and stop the
service.

## Testing

Unit tests will cover:

- Request and response frame round trips.
- Little-endian field encoding.
- Invalid magic rejection.
- Unsupported protocol version rejection.
- Unknown command rejection.
- Oversized payload rejection.
- Identity response parsing.
- CLI command parsing and duplicate/unknown command errors.

Windows integration tests will cover:

- The service binary can register with demand start.
- The service can start and reach `RUNNING`.
- `status` reports the running state.
- `identity` reports `LocalSystem`, `System`, and protocol version `1`.
- `stop` reaches `STOPPED`.
- `uninstall` removes the service.

The lifecycle test will clean up the service in a finally-style path so a
failed assertion does not intentionally leave a privileged service installed.
The lifecycle test must run with administrator rights and will be documented
as an explicit manual or privileged test target if the normal test runner is
not elevated.

## Documentation

Update:

- `README.md` with service lifecycle and identity commands.
- `docs/architecture.md` with the CLI/core/service/SCM/Pipe layers.
- `docs/learning-notes.md` with Windows Services, SCM, Named Pipes, tokens,
  ACLs, and LocalSystem boundaries.
- `docs/benchmarks.md` with service startup latency, Pipe round-trip latency,
  and identity query cost.

## Completion Criteria

Task 03 is complete when:

- `rustnt-service` builds as a Windows binary.
- The service installs under LocalSystem with demand start.
- `rustnt service start` reaches `RUNNING`.
- `rustnt service identity` returns the expected identity and capabilities.
- Invalid and unauthorized protocol shapes are rejected.
- `rustnt service stop` and `uninstall` clean up correctly.
- All non-privileged tests pass.
- Privileged lifecycle tests are either passing or clearly documented as
  environment-gated.
- No process-control or arbitrary-command feature is included.
