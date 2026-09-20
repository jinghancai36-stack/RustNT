# Task11 System Monitor v1 Report

## Delivered

Task11 adds a read-only Windows system monitor to the existing RustNT CLI and
core workspace. The implementation reports system CPU utilization, physical
memory, local fixed-disk capacity, and process additions/exits. It does not add
a service command, named-pipe message, protocol version change, or arbitrary
command execution.

The command is available as `rustnt monitor`. A one-shot invocation prints one
human-readable snapshot and exits. Watch mode clears and refreshes the display
at a one-second default interval or at a caller-supplied positive interval.

## CLI Usage

```text
rustnt monitor
rustnt monitor --watch
rustnt monitor --watch <positive seconds>
```

Invalid, zero, duplicate, and unknown monitor options are rejected with the
existing nonzero CLI usage-error behavior. Output is text with CPU, MEMORY,
DISKS, and PROCESS CHANGES sections.

## Core Collection

The `rustnt_core::monitor` module owns structured monitor snapshots and the
Windows collection path. It samples `GetSystemTimes` through the existing
process snapshot path for CPU utilization, `GlobalMemoryStatusEx` for physical
memory, and logical drives plus `GetDiskFreeSpaceExW` for fixed-drive capacity.
Process changes compare successive process snapshots and report added and
exited processes by PID and name.

## Tests and Verification

The complete gate was run in the required order on 2026-09-20 from the
workspace root:

| Command | Result |
| --- | --- |
| `cargo test --workspace -- --nocapture` | PASS; 94 tests passed, 0 failed; 19 CLI, 74 core, 1 Task09 target; doctests ran with 0 tests |
| `cargo build --workspace` | PASS; workspace build finished successfully |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS; finished with no warnings or errors |
| `cargo fmt --all -- --check` | PASS; no formatting changes required |
| `git diff --check` | PASS; no whitespace errors (Git emitted only the existing LF-to-CRLF warning for `progress.md`) |

The monitor-specific tests cover option parsing, deterministic rendering,
first-sample baselining, CPU and memory percentage edge cases, fixed-drive
selection, Win32 disk-capacity conversion for both failure (retained root with
unavailable fields) and success (caller-available free bytes), process
comparison ordering, PID reuse, and PID fallback behavior.

## Known Boundaries

- The first sample establishes the CPU and process baseline, so it reports no
  CPU percentage and no process additions or exits.
- Disk collection includes local fixed drives only; removable, remote, and
  optical drives are excluded.
- A disk that cannot provide capacity remains in the snapshot, with its
  unavailable capacity rendered as `N/A`.
- When process creation time is unavailable, process comparison uses PID as a
  best-effort identity fallback; PID reuse can therefore appear as an exit and
  an addition when stronger identity data is absent.
- The monitor is Windows-only and currently provides human-readable output
  without JSON, charts, persistence, notifications, or a service transport.

## Commits

- `1a869a4` - add monitor snapshot derivations.
- `a40b138` - fix process snapshot comparison indexing.
- `f56f77e` - collect Windows system monitor metrics.
- `0b31ee2` - add the `rustnt monitor` CLI command.
- `a751ef2` - fix caller-available disk free-space mapping and add conversion tests.

These changes are integrated on mainline at `6803b5d`; this report and the
roadmap/progress updates are the final Task11 documentation change.
