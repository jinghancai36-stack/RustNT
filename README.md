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

Longer-term experiments may cover process management, filesystem performance,
Windows Shell components, system monitoring, IPC, Native NT APIs, Windows
services, and low-latency desktop components.

## Local service bridge

RustNT includes a restricted Windows service bridge for inspecting the service
identity and protocol capabilities. The service is registered with the Windows
Service Control Manager (SCM) as `RustNTControl`, runs as `LocalSystem`, and
uses demand start. It listens only on the local secured Named Pipe
`\\.\pipe\RustNT.Control.v1`; remote clients are rejected and the Pipe
has an explicit DACL.

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
Task 03 exposes only the fixed protocol commands `PING`, `IDENTITY`, and
`CAPABILITIES`; `IDENTITY` and `CAPABILITIES` are read-only queries. The
service does not accept process-control requests, executable paths, arbitrary
arguments, scripts, or administrator commands.

Task 04 and Task 05 are future product-scope labels in the roadmap. The current
Task 04/05 work only establishes this service-bridge foundation; it does not
implement process control or administrator command execution. Any future
process-control scope and separately reviewed administrator-command scope
remain subject to explicit authorization, auditing, and operation restrictions.

RustNT is a normal Windows user-mode project. It is not an operating system and
does not replace the Windows kernel, drivers, or compatibility infrastructure.
