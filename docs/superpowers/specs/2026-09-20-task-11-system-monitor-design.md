# Task11 System Monitor v1 Design

**Date:** 2026-09-20

## Goal

Add a read-only system monitor to RustNT that can print one Windows system snapshot or refresh it continuously. The first version covers system CPU utilization, physical memory, local fixed disks, and process additions/exits without changing the service protocol or adding arbitrary command execution.

## Scope

Task11 includes:

* A new `rustnt monitor` CLI command.
* A one-shot snapshot mode when no watch option is supplied.
* A continuous watch mode with a one-second default interval and a positive integer override.
* A focused `rustnt-core::monitor` module for collection and structured snapshots.
* System CPU utilization calculated from consecutive `GetSystemTimes` samples.
* Physical memory collected through `GlobalMemoryStatusEx`.
* All local fixed disks discovered through logical-drive enumeration; network, removable, and optical drives are excluded.
* Process additions and exits derived from consecutive existing process snapshots.
* Human-readable table output only.
* Unit, parser, renderer, and Windows smoke tests.

Task11 does not include:

* JSON output, a TUI, charts, notifications, or persistence.
* A LocalSystem service, a new named-pipe command, or protocol version changes.
* Full process-table replacement; `rustnt process list` remains the process-table command.
* File-system browsing, window/session inspection, or arbitrary shell and PowerShell execution.

## CLI Contract

The command forms are:

```text
rustnt monitor
rustnt monitor --watch
rustnt monitor --watch <seconds>
```

`rustnt monitor` prints exactly one snapshot and exits. `--watch` refreshes continuously every second. A positive decimal value after `--watch` overrides the interval. Zero, negative, malformed, duplicate, or unknown options are rejected with the same nonzero CLI behavior used by the existing process commands.

The watch loop reuses the existing terminal refresh convention and exits through the normal console interruption path. A collector or rendering error is written to stderr and produces a nonzero exit result.

## Architecture

### Core monitor module

Create `crates/rustnt-core/src/monitor.rs` and expose it from the core crate. The module owns platform collection and exposes structured values so the CLI does not call Win32 APIs directly.

The primary interface is:

```text
MonitorSampler::new() -> MonitorSampler
MonitorSampler::sample(&mut self) -> Result<MonitorSnapshot, RustNtError>
```

`MonitorSampler` retains the previous process/system sample only for the lifetime of one CLI invocation. The first call establishes the process and CPU baseline. Every later call compares the new sample with the retained baseline, returns the derived values, and replaces the baseline.

The snapshot types are conceptually:

```text
MonitorSnapshot {
    cpu_percent: Option<f32>,
    memory: MemoryInfo,
    disks: Vec<DiskInfo>,
    process_changes: ProcessChanges,
}

MemoryInfo {
    total_bytes: u64,
    available_bytes: u64,
}

DiskInfo {
    root: String,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
}

ProcessChanges {
    added: Vec<ProcessChange>,
    exited: Vec<ProcessChange>,
}

ProcessChange {
    pid: u32,
    name: String,
}
```

The exact Rust visibility and derives should follow the existing core API style. Byte counts remain integer values in the core; unit conversion belongs to the CLI renderer.

### CPU sampling

Extend the existing system-time sampling data instead of creating a second process-enumeration path. The monitor stores the previous `GetSystemTimes` counters and calculates:

```text
total_delta = kernel_delta + user_delta
busy_delta = total_delta - idle_delta
cpu_percent = busy_delta / total_delta * 100
```

If counters move backwards, the total delta is zero, or the result cannot be calculated, `cpu_percent` is `None` for that snapshot. The first snapshot also returns `None`. The implementation must preserve the existing per-process CPU calculation behavior used by `rustnt process list --watch`.

### Memory sampling

Use `GlobalMemoryStatusEx` and return physical total and available bytes. Used memory is derived by the renderer as `total_bytes - available_bytes` after guarding against an invalid larger available value. A failure to obtain the system memory structure is a snapshot error because the memory section is a required system metric.

### Disk sampling

Use the existing workspace Windows API dependency set plus the minimum feature needed for `GlobalMemoryStatusEx`. Enumerate drive letters from `GetLogicalDrives`, keep only drives where `GetDriveTypeW` returns `DRIVE_FIXED`, and query each root with `GetDiskFreeSpaceExW`. The free value is the caller-available byte count returned by that API.

A disk that disappears or rejects the free-space query remains in the snapshot with `None` values for unavailable capacity fields, allowing the table to show `N/A` without discarding the rest of the monitor output. A failure to enumerate logical drives is a snapshot error.

### Process changes

Reuse `sample_processes()` for process enumeration and existing best-effort process metadata. Add a core comparison operation on the existing process snapshot rather than duplicating ToolHelp enumeration in the monitor module.

The comparison identity is the process ID plus the process creation timestamp when that timestamp is available from `GetProcessTimes`. If either side lacks a creation timestamp, comparison falls back to the process ID. This prevents ordinary PID reuse from being silently treated as the same process while keeping the existing best-effort collection policy.

The first sample returns empty `added` and `exited` lists and establishes the baseline. Later samples report only processes present in the current snapshot but not the previous one, and processes present in the previous snapshot but not the current one. A process table is never included in the monitor output.

## Data Flow

```text
rustnt monitor arguments
    -> CLI parser
    -> MonitorSampler::new()
    -> sample system times, memory, disks, and processes
    -> derive CPU and process changes from the previous sample
    -> MonitorSnapshot
    -> deterministic human-readable renderer
    -> stdout
```

One-shot mode calls `sample()` once. Watch mode renders the first baseline snapshot, flushes stdout, sleeps for the selected interval, and repeats. The sampler remains alive for the entire loop so CPU and process comparisons are meaningful.

## Output Contract

The renderer prints four stable sections: a title, CPU, memory, fixed-disk capacity, and process changes. It uses the existing byte-formatting conventions where possible and prints `N/A` for unavailable or not-yet-computable values.

A representative output shape is:

```text
RustNT System Monitor

CPU              23.4%
MEMORY           8.1 GiB used / 15.8 GiB total (51.3%)

DISKS
ROOT             FREE        TOTAL       USED
C:\\              120.4 GiB   476.9 GiB   74.8%

PROCESS CHANGES
NEW              123 rustnt.exe
EXITED           none
```

The exact column padding may follow existing CLI rendering helpers, but section names, metric meaning, and the no-event behavior are fixed. The renderer must not emit JSON, ANSI-only data, arbitrary command output, or a full process list.

## Error and Compatibility Policy

* The monitor is Windows-only, consistent with the current workspace crate configuration.
* Collection errors use the existing `RustNtError` and CLI error-reporting style.
* Per-process metadata failures remain best effort as they are for `rustnt process list`.
* Per-disk capacity failures are represented as `N/A` for that disk; they do not abort the snapshot.
* No service dispatch, named-pipe frame, capability registry, or protocol status changes are required.
* Existing process commands and their output remain behaviorally unchanged except for internal reuse of shared sampling data.
* PID reuse handling is best effort when Windows cannot provide a creation timestamp; this limitation is documented rather than hidden.

## Testing and Acceptance

Core tests must cover:

* CPU percentage calculation for known idle/kernel/user deltas.
* Invalid and backwards system counters returning `None`.
* First-sample baseline behavior with no process-change events.
* Added and exited process comparison.
* Creation-time-aware PID reuse handling and PID-only fallback.
* Memory used-byte derivation and unavailable values.
* Fixed-drive filtering and unavailable disk capacity representation through testable helper logic.

CLI tests must cover:

* Parsing `monitor` with no options.
* Parsing `--watch` with the one-second default and a positive override.
* Rejection of zero, malformed, duplicate, and unknown monitor options.
* Stable rendering for CPU `N/A`, memory, disks, and empty/non-empty process changes.

A Windows smoke test must instantiate `MonitorSampler`, obtain a snapshot, verify that memory values are internally valid, and confirm that the sampler can establish a second baseline without panicking. The final workspace gate remains:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Task11 is complete only when the command is implemented, the focused tests pass, the full verification gate passes, and the change is recorded in the Task11 progress/report documentation.
