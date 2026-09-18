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

RustNT is a normal Windows user-mode project. It is not an operating system and
does not replace the Windows kernel, drivers, or compatibility infrastructure.
