# Architecture

Task 02 is a normal Windows user-mode application. RustNT does not replace
Windows components, the Windows kernel, or drivers.

```text
rustnt-cli
     |
     v
rustnt-core
     |
     v
Windows API
     |
     v
ntdll / Windows user-space infrastructure
     |
     v
NT Kernel
```

`rustnt-cli` owns command-line parsing and presentation. `rustnt-core` owns the
Windows interaction and exposes Rust data types. The boundary is deliberately
small so future commands can reuse the system-information layer without
placing Win32 calls in the CLI.

Process enumeration uses the documented Tool Help snapshot APIs:
`CreateToolhelp32Snapshot`, `Process32FirstW`, and `Process32NextW`.
The process record's `cntThreads` field supplies the snapshot's thread count.
Memory is best-effort through
`OpenProcess` and `GetProcessMemoryInfo`; protected or inaccessible processes
show an unavailable memory value rather than aborting the complete listing.

Each process also uses the same owned process HANDLE for
`QueryFullProcessImageNameW` and `GetProcessTimes` when access is available.
`GetSystemTimes` is sampled once per snapshot. `rustnt-core` exposes a
`ProcessSnapshot` so the CLI can compare two snapshots by PID without exposing
Win32 handles or unsafe code outside the core crate.

One-shot mode intentionally leaves CPU unavailable. Watch mode renders a
baseline, waits, samples again, and computes:

```text
process CPU = process kernel/user delta
               ------------------------- * 100
               system kernel/user delta
```

CPU values are clamped to `0..=100` and become unavailable when a process
disappears or any required counter delta is invalid.

## Task 03 service bridge

The privileged bridge is deliberately split between the CLI, the reusable
service module, and the thin service binary:

```text
rustnt-cli
    |
    v
rustnt-core::service
    |
    +--> SCM
    +--> Named Pipe
    +--> Process Token
    |
    v
rustnt-service
```

`rustnt-cli` parses the six `service` subcommands and renders status or
identity data. `rustnt-core::service` owns the Windows Service Control Manager
(SCM), Named Pipe, protocol, security, token, and HANDLE operations.
`rustnt-service` is the Windows service entry point and delegates its host
loop to the core crate.

### SCM registration and lifecycle

`rustnt service install` resolves the sibling `rustnt-service.exe`, opens the
local SCM through its Win32 API, and registers `RustNTControl` as a
`SERVICE_WIN32_OWN_PROCESS` service under the `LocalSystem` account with
`SERVICE_DEMAND_START`. It does not invoke `sc.exe`, PowerShell, WMI, or write
service registry keys directly. A matching existing registration is accepted;
a different configured binary path is reported as a conflict.

The service is started explicitly by `rustnt service start` or the SCM. The
host reports `START_PENDING`, then `RUNNING`, accepts stop and shutdown
controls, reports `STOP_PENDING`, and finishes at `STOPPED`. Lifecycle calls
poll the SCM until the requested terminal state or a bounded timeout. Status
also includes the process ID when the SCM reports one. Uninstall requires the
service to be stopped.

### Local Pipe and protocol boundary

The service uses the fixed local Pipe
`\\.\pipe\RustNT.Control.v1`. It is a message-mode, blocking Named Pipe
with one server instance, remote-client rejection, and an explicit DACL. The
DACL grants full control to `SYSTEM` and Administrators and read/write access
to interactive users. The service handles one request per connection and then
closes it.

Requests use the following little-endian frame:

```text
magic[4] | version[u16] | command[u16] | payload_length[u32] | payload
```

Responses use:

```text
magic[4] | version[u16] | status[u32] | payload_length[u32] | payload
```

The decoder validates magic, protocol version, known command, exact frame
length, and the 4096-byte payload limit. The only protocol commands are
`PING`, `IDENTITY`, and `CAPABILITIES`. Requests for these Task 03 commands
carry no payload. `PING` returns an empty success response,
`IDENTITY` returns service-token fields, and `CAPABILITIES` returns the fixed
allowlist `ping,identity,capabilities`.

### Token identity and privilege boundary

The service opens its own process token and reports the account SID, the
stable `LocalSystem` label for SID `S-1-5-18`, integrity level, elevation,
protocol version, and fixed capabilities. `LocalSystem` is intentionally a
high-privilege account: a vulnerability in a request handler could turn a
local client path into a system-level execution path. The explicit Pipe DACL
and remote rejection reduce exposure, but they are not operation-level
authorization for future dangerous actions.

For that reason, future privileged actions require a whitelist rather than
free-form command dispatch. Each operation must be explicitly named,
validated, authorized against the connecting token, and audited. Task 03 has
no process-control or arbitrary administrator-command implementation.

Process-control operations are reserved for Task 04. Administrator command
execution is reserved for a separately reviewed Task 05 design with explicit
authorization, auditing, and operation restrictions. These remain separate
future scopes.

All SCM, Pipe, event, and token HANDLE values are owned by local RAII wrappers
in the core service module. Unsafe FFI blocks keep nearby safety comments for
pointer, buffer, callback, and handle lifetime assumptions.
