# Task 02 Process Inspector v2 Design

**Date:** 2026-09-14

## Goal

Extend the Task 01 Windows process inspector with process paths, optional CPU
sampling, filtering, sorting, and watch-mode refresh while keeping Windows API
calls inside `rustnt-core`.

## Scope

Task 02 adds:

- `Option<String>` process image path.
- Optional CPU percentage calculated from two samples.
- Fast one-shot listing without a deliberate sampling delay.
- `--watch <seconds>` repeated refresh mode.
- `--filter <text>` case-insensitive matching against process name and path.
- `--sort <pid|name|cpu|memory>` sorting.
- Best-effort handling for inaccessible processes.

Task 02 does not add process termination, elevated services, GUI, ETW, async
runtimes, or PowerShell/WMI/tasklist integration.

## Architecture

`rustnt-core` remains the only crate that calls Windows APIs. It will expose
process records and a small sampling API. The CLI will parse arguments, request
data, filter and sort records through core-provided data, and render output.

The fast one-shot path will enumerate processes once and collect memory and
path information where permitted. CPU is `None` in this mode because a useful
percentage requires a time interval.

Watch mode will collect and render an initial snapshot with unavailable CPU
values, wait for the requested interval, collect a second snapshot, calculate
CPU percentages by PID, render the result, and repeat.

## Data Model

`ProcessInfo` will add:

```rust
pub path: Option<String>,
pub cpu_percent: Option<f32>,
```

The CPU sampling helper will use an internal snapshot type containing process
kernel time, process user time, system idle time, system kernel time, and system
user time. The public API will avoid exposing raw Win32 time structures.

## Windows APIs

The implementation will use documented APIs:

- `QueryFullProcessImageNameW` for the executable path.
- `GetProcessTimes` for process kernel and user time.
- `GetSystemTimes` for system idle, kernel, and user time.
- Existing Task 01 APIs for enumeration and memory.
- Existing `OpenProcess` and `CloseHandle` ownership pattern.

Path and CPU collection are best effort. A failed per-process query produces
`None` for that metric. Failure to create the process enumeration snapshot
remains a fatal operation error.

## CPU Calculation

For a process between samples:

```text
process_delta = kernel_delta + user_delta
system_delta = system_kernel_delta + system_user_delta
cpu_percent = process_delta / system_delta * 100
```

The value will be clamped to the range `0.0..=100.0` for a process-wide
measurement. PID reuse or a missing previous sample produces `None`. Zero or
backward time deltas also produce `None`.

## CLI

Supported syntax:

```text
rustnt process list
rustnt process list --filter <text>
rustnt process list --sort <pid|name|cpu|memory>
rustnt process list --watch <seconds>
rustnt process list --watch <seconds> --filter <text> --sort cpu
```

Invalid options or a non-positive watch interval produce a usage error and exit
code 2. The CLI will use `std::env` parsing and will not add a command-line
framework dependency for this task.

The table will include:

```text
PID PROCESS CPU THREADS MEMORY PATH
```

Long paths may be truncated for display while the underlying `ProcessInfo`
retains the complete path.

## Testing

Tests will cover:

- CPU delta calculation with deterministic synthetic samples.
- CPU calculation rejection for missing, zero, backward, and reused samples.
- Case-insensitive name/path filtering.
- PID, name, CPU, and memory sorting.
- Argument parsing and invalid watch intervals.
- Windows integration enumeration still succeeds and finds the current
  process.
- The CLI binary runs the one-shot command and emits the expected headers.

Tests will avoid fixed PIDs, fixed process names, and assumptions about which
processes are accessible.

## Limitations

CPU percentages are sampled values, not scheduler-perfect accounting. The
measurement can be affected by process exit, PID reuse, timer resolution, and
short intervals. Memory and executable paths can remain unavailable for system
or protected processes. Watch mode is synchronous and intentionally simple;
background services and IPC belong to later tasks.
