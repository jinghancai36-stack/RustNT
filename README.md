# RustNT

RustNT is an educational and experimental Windows systems project. It explores
whether selected parts of the Windows user-space experience can be rebuilt in
Rust with a clear architecture and measurable performance.

Task 02 provides a direct Win32 process inspector with best-effort executable
paths, memory, optional CPU sampling, filtering, and sorting:

```text
rustnt process list
rustnt process list --sort memory
rustnt process list --filter edge
rustnt process list --watch 1 --sort cpu
```

The one-shot command reports CPU as `N/A` because CPU usage requires two
samples. Watch mode refreshes synchronously at the requested interval and
calculates process CPU from process-time delta divided by system-time delta.
Process paths, memory, and CPU are best-effort values; inaccessible or
short-lived processes can show `N/A`.

## Read-only filesystem commands

Task12A adds five user-mode, read-only filesystem commands:

```text
rustnt fs stat --path <path>
rustnt fs list --path <directory>
rustnt fs space --path <path>
rustnt fs permissions --path <path>
rustnt fs search --path <directory> --name <text>
```

`search` is bounded to depth 16 and 1000 results. Reparse points are reported
but never followed. `permissions` reports the raw owner, DACL, and supported
allow/deny ACEs; it does not expand groups or calculate effective access.
These commands use the current user token, do not elevate, and do not call the
LocalSystem service. Protected paths can still return access denied.

## Window and session viewer

Task13 adds three read-only user-mode commands:

```text
rustnt window list
rustnt window foreground
rustnt session list
```

`window list` enumerates top-level windows on the current desktop and in the
current Windows Session, including hidden windows and windows with empty titles.
`window foreground` reports the foreground window only when it belongs to that
same Session and desktop. Both commands include best-effort process metadata
when it is available. `session list` reports local Windows Session state,
username, domain, client name, and current/active-console markers.

Window and Session inspection is read-only. It does not start the LocalSystem
service, request elevation, mutate window state, enumerate child windows, or
cross the current Session/current desktop boundary.

Longer-term experiments may cover filesystem performance, Windows Shell
components, system monitoring, IPC, Native NT APIs, Windows services, and
low-latency desktop components.

## Local service bridge

RustNT includes a restricted Windows service bridge for inspecting service
identity/capabilities and for narrowly controlled process operations. The
service is registered with the Windows Service Control Manager (SCM) as
`RustNTControl`, runs as `LocalSystem`, and uses demand start. It listens only
on the local secured Named Pipe `\\.\pipe\RustNT.Control.v1`; remote clients
are rejected and the Pipe has an explicit DACL.

Run the lifecycle explicitly:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
rustnt service stop
rustnt service uninstall
```

`status` reports SCM state and the service process ID when Windows provides
one. `identity` does not start the service implicitly, so run `start` first.
The fixed protocol commands are `PING`, `IDENTITY`, `CAPABILITIES`,
`PROCESS_INSPECT`, and `PROCESS_TERMINATE`.

Inspect one process:

```text
rustnt process inspect --pid <PID>
```

Terminate one process:

```text
rustnt process terminate --pid <PID>
```

Inspection is read-only and returns bounded metadata. Termination is
destructive and requires a local elevated Administrator client. It is limited
to a caller-owned normal user process, checks the process creation timestamp
again to reject PID reuse, and rejects PID 0, PID 4, the RustNT service,
system-owned processes, and foreign-user processes.

Task10 adds a typed authorization and audit boundary without changing protocol
version 1 or command codes. The fixed capabilities are `ping`, `identity`,
`capabilities`, `process_inspect`, and `process_terminate`; the service maps
each command to exactly one capability before dispatch. Termination requests
must pass the connecting client's SID, elevated-token, and local-Administrator
checks before the existing ownership, PID-reuse, and protected-target policy
runs. Only the destructive operation is rate-limited: at most 4 requests per
caller SID in a 10-second window.

The service keeps the newest 256 typed audit events in memory and bounds the
rate-limiter subject table to 256 caller SIDs. Events contain only a request
ID, fixed capability/outcome/reason values, and optional validated identity
fields. They are not persisted to Windows Event Log and are not exposed through
the Pipe protocol; restarting the service discards them.

The service never accepts executable paths, process names, wildcards, command
lines, scripts, arbitrary exit codes, or shell commands. The CLI does not
perform elevation and does not open target processes with termination rights;
the LocalSystem service remains the privileged boundary.

## GUI foundation

Launch the Windows GUI with:

```text
cargo run -p rustnt-gui
```

The GUI is a normal Windows user-mode process hosted by `eframe`, `winit`, and
`egui`. It stores settings, the incomplete-run marker, and the crash log under
`%APPDATA%\\RustNT`. If `gui.running` remains after an incomplete run, the next
launch enters safe recovery mode with default settings and window geometry.

Task14 does not replace Explorer, the taskbar, the Windows kernel, or the
existing service authorization boundary.

RustNT is a normal Windows user-mode project. It is not an operating system and
does not replace the Windows kernel, drivers, or compatibility infrastructure.
