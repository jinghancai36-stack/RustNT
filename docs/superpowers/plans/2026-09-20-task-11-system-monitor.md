# Task11 System Monitor v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `rustnt monitor` command that reports CPU, physical memory, local fixed disks, and added/exited processes in one-shot or watch mode.

**Architecture:** Keep Windows collection in a new `rustnt_core::monitor` module. Extend the existing process snapshot with system-time and process-identity data so monitoring reuses the current Tool Help enumeration and CPU sampling path. Keep the CLI responsible for argument parsing, rendering, and the watch loop; do not add a service or named-pipe command.

**Tech Stack:** Rust 2021 workspace, `windows-sys 0.59`, Win32 `GetSystemTimes`, `GlobalMemoryStatusEx`, `GetLogicalDrives`, `GetDriveTypeW`, `GetDiskFreeSpaceExW`, existing `cargo test`, `cargo build`, Clippy, rustfmt, and Git.

## Global Constraints

* The feature is Windows-only and follows the existing `#![cfg(windows)]` crate configuration.
* The CLI command is `rustnt monitor`; without options it prints one snapshot.
* `rustnt monitor --watch` refreshes every 1 second; `rustnt monitor --watch <seconds>` uses a positive integer interval.
* The monitor output is human-readable text only; no JSON, TUI, charts, persistence, or notifications.
* Disk output includes only `DRIVE_FIXED` logical drives and renders unavailable capacity fields as `N/A`.
* The first monitor sample establishes a CPU/process baseline and reports no process changes.
* Process changes contain only additions and exits, not a replacement process table.
* No service dispatch, named-pipe frame, protocol command code, capability registry, or authorization behavior changes are allowed.
* Existing `rustnt process list`, `inspect`, and `terminate` behavior must remain intact.
* Each implementation task ends with focused tests and a dedicated commit.

---

### Task 1: Add Monitor Domain Types and Snapshot Derivations

**Files:**
- Create: `crates/rustnt-core/src/monitor.rs`
- Modify: `crates/rustnt-core/src/lib.rs`
- Test: `crates/rustnt-core/src/lib.rs` and `crates/rustnt-core/src/monitor.rs` unit-test modules

**Interfaces:**
- Produces `rustnt_core::monitor::{DiskInfo, MemoryInfo, MonitorSnapshot, ProcessChange, ProcessChanges}`.
- Produces `ProcessSnapshot::system_cpu_percent_from(&self, previous: &ProcessSnapshot) -> Option<f32>`.
- Produces `ProcessSnapshot::process_changes_from(&self, previous: &ProcessSnapshot) -> rustnt_core::monitor::ProcessChanges`.
- Keeps `ProcessSnapshot::apply_cpu_from` and all existing public process APIs compatible.

- [ ] **Step 1: Write failing tests for system CPU and process-change derivation**

Add internal test helpers that construct `ProcessSnapshot` values with synthetic system counters, process CPU counters, process names, and process identities. The tests must cover:

```rust
#[test]
fn system_cpu_percentage_uses_busy_delta_over_total_delta() {
    let previous = system_snapshot(100, 1_000, 2_000);
    let current = system_snapshot(200, 2_000, 3_000);

    assert_eq!(
        current.system_cpu_percent_from(&previous),
        Some(95.0)
    );
}

#[test]
fn system_cpu_percentage_is_unavailable_for_backwards_or_zero_deltas() {
    let previous = system_snapshot(100, 1_000, 2_000);

    assert_eq!(
        system_snapshot(100, 1_000, 2_000).system_cpu_percent_from(&previous),
        None
    );
    assert_eq!(
        system_snapshot(50, 1_000, 2_000).system_cpu_percent_from(&previous),
        None
    );
}

#[test]
fn first_process_comparison_reports_added_and_exited_processes() {
    let previous = snapshot_with_processes(&[(7, "old.exe", Some(10))]);
    let current = snapshot_with_processes(&[(8, "new.exe", Some(20))]);

    assert_eq!(
        current.process_changes_from(&previous),
        ProcessChanges {
            added: vec![ProcessChange {
                pid: 8,
                name: "new.exe".to_string(),
            }],
            exited: vec![ProcessChange {
                pid: 7,
                name: "old.exe".to_string(),
            }],
        }
    );
}
```

The helper values must make the CPU calculation explicit: the first argument is idle time, the second is kernel time, and the third is user time. Add tests for a same-PID creation-time change being reported as an exit plus an addition, and for a missing creation time falling back to PID identity.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```text
cargo test -p rustnt-core system_cpu_percentage -- --nocapture
cargo test -p rustnt-core process_comparison -- --nocapture
```

Expected: compilation or test failures because the monitor types and new `ProcessSnapshot` methods do not exist yet.

- [ ] **Step 3: Add the monitor value types**

Create `crates/rustnt-core/src/monitor.rs` with these public data contracts:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl MemoryInfo {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn used_percent(&self) -> Option<f32> {
        if self.total_bytes == 0 || self.available_bytes > self.total_bytes {
            return None;
        }
        Some((self.used_bytes() as f64 / self.total_bytes as f64 * 100.0) as f32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub root: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

impl DiskInfo {
    pub fn used_percent(&self) -> Option<f32> {
        let total = self.total_bytes?;
        let free = self.free_bytes?;
        if total == 0 || free > total {
            return None;
        }
        Some(((total - free) as f64 / total as f64 * 100.0) as f32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessChange {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessChanges {
    pub added: Vec<ProcessChange>,
    pub exited: Vec<ProcessChange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorSnapshot {
    pub cpu_percent: Option<f32>,
    pub memory: MemoryInfo,
    pub disks: Vec<DiskInfo>,
    pub process_changes: ProcessChanges,
}
```

Add `pub mod monitor;` to `crates/rustnt-core/src/lib.rs`.

- [ ] **Step 4: Extend process snapshots without duplicating enumeration**

Modify `crates/rustnt-core/src/lib.rs` as follows:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemTimes {
    idle: u64,
    kernel: u64,
    user: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    creation_time_100ns: Option<u64>,
}

pub struct ProcessSnapshot {
    processes: Vec<ProcessInfo>,
    cpu_times: HashMap<u32, CpuTimes>,
    system_times: SystemTimes,
    process_identities: HashMap<u32, ProcessIdentity>,
}
```

Change `system_times()` to return `SystemTimes`, retaining the idle counter rather than discarding it. Change `process_times()` to return creation, kernel, and user times:

```rust
fn process_times(handle: HANDLE) -> Option<(u64, u64, u64)>;
```

Store the creation timestamp in `process_identities`, while keeping `ProcessInfo` unchanged so the existing CLI fixture and public process row shape remain stable. Add these methods to `impl ProcessSnapshot`:

```rust
pub fn system_cpu_percent_from(&self, previous: &ProcessSnapshot) -> Option<f32>;

pub fn process_changes_from(
    &self,
    previous: &ProcessSnapshot,
) -> crate::monitor::ProcessChanges;
```

`system_cpu_percent_from` must use checked deltas and calculate busy time as `kernel_delta + user_delta - idle_delta`. Return `None` when any counter moves backwards, total time is zero, or busy time would underflow. Clamp finite results to `0.0..=100.0`.

`process_changes_from` must compare PID and creation timestamp when both timestamps are present. If either timestamp is absent, compare by PID. Sort both result vectors by PID before returning them so render output is deterministic.

- [ ] **Step 5: Add value-type edge-case tests**

Add tests for:

```rust
#[test]
fn memory_used_percent_rejects_invalid_available_bytes() {
    let memory = MemoryInfo {
        total_bytes: 100,
        available_bytes: 101,
    };
    assert_eq!(memory.used_bytes(), 0);
    assert_eq!(memory.used_percent(), None);
}

#[test]
fn disk_used_percent_returns_none_for_missing_or_invalid_capacity() {
    assert_eq!(
        DiskInfo {
            root: "C:\\".to_string(),
            total_bytes: None,
            free_bytes: Some(10),
        }
        .used_percent(),
        None
    );
    assert_eq!(
        DiskInfo {
            root: "C:\\".to_string(),
            total_bytes: Some(100),
            free_bytes: Some(101),
        }
        .used_percent(),
        None
    );
}
```

- [ ] **Step 6: Run the focused core tests and commit**

Run:

```text
cargo test -p rustnt-core -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected: all core tests pass, including the existing process enumeration, CPU, path, and string tests. Commit:

```text
git add crates/rustnt-core/src/lib.rs crates/rustnt-core/src/monitor.rs
git commit -m "feat: add monitor snapshot derivations"
```

### Task 2: Implement Windows Monitor Collection and Sampler

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/rustnt-core/src/monitor.rs`
- Modify: `crates/rustnt-core/src/lib.rs` only if collector access requires a `pub(crate)` helper
- Test: `crates/rustnt-core/src/monitor.rs` unit and Windows smoke tests

**Interfaces:**
- Produces `MonitorSampler::new() -> MonitorSampler`.
- Produces `MonitorSampler::sample(&mut self) -> Result<MonitorSnapshot, RustNtError>`.
- Uses `sample_processes()` and the derivation methods from Task 1.
- Does not expose Win32 handles or raw API structures outside the core module.

- [ ] **Step 1: Write failing collector and sampler tests**

Add pure helper tests for drive filtering and sampler behavior:

```rust
#[test]
fn only_fixed_drive_types_are_selected() {
    assert!(is_fixed_drive_type(DRIVE_FIXED));
    assert!(!is_fixed_drive_type(DRIVE_REMOVABLE));
    assert!(!is_fixed_drive_type(DRIVE_REMOTE));
    assert!(!is_fixed_drive_type(DRIVE_CDROM));
}

#[test]
fn first_sampler_snapshot_has_no_cpu_or_process_events() {
    let mut sampler = MonitorSampler::new();
    let snapshot = sampler.sample().expect("first monitor sample should succeed");

    assert_eq!(snapshot.cpu_percent, None);
    assert!(snapshot.process_changes.added.is_empty());
    assert!(snapshot.process_changes.exited.is_empty());
}

#[test]
#[cfg(windows)]
fn monitor_sampler_collects_two_snapshots() {
    let mut sampler = MonitorSampler::new();
    let first = sampler.sample().expect("first monitor sample should succeed");
    let second = sampler.sample().expect("second monitor sample should succeed");

    assert!(first.memory.total_bytes > 0);
    assert!(first.memory.available_bytes <= first.memory.total_bytes);
    assert!(second.memory.total_bytes > 0);
}
```

The smoke test may need a short sleep between samples only if the Windows counter resolution produces zero deltas; it must remain bounded and use `Duration::from_millis(20)` at most once.

- [ ] **Step 2: Run the focused tests and verify the collector is missing**

Run:

```text
cargo test -p rustnt-core monitor_sampler -- --nocapture
cargo test -p rustnt-core only_fixed_drive_types -- --nocapture
```

Expected: failures because `MonitorSampler`, the Win32 collectors, and the drive helper are not implemented.

- [ ] **Step 3: Enable the minimum Windows API feature**

Add this feature to the existing `windows-sys` dependency in the root `Cargo.toml`:

```toml
"Win32_System_SystemInformation",
```

Do not add a new crate or enable unrelated API families.

- [ ] **Step 4: Implement physical memory collection**

In `crates/rustnt-core/src/monitor.rs`, import `GlobalMemoryStatusEx` and `MEMORYSTATUSEX`. Implement:

```rust
fn sample_memory() -> Result<MemoryInfo, RustNtError> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let result = unsafe { GlobalMemoryStatusEx(&mut status) };
    if result == 0 {
        return Err(RustNtError::last("failed to read memory status"));
    }
    Ok(MemoryInfo {
        total_bytes: status.ullTotalPhys as u64,
        available_bytes: status.ullAvailPhys as u64,
    })
}
```

Use the existing `RustNtError` policy and add a `pub(crate)` visibility adjustment to `RustNtError::last` only if the child module cannot call it directly.

- [ ] **Step 5: Implement fixed-drive discovery and capacity collection**

Use `GetLogicalDrives` to obtain the bitmask for drive letters `A` through `Z`. For each set bit, build a null-terminated root such as `C:\\`, call `GetDriveTypeW`, and retain only `DRIVE_FIXED`. Query retained roots with `GetDiskFreeSpaceExW`.

Implement these helpers:

```rust
fn is_fixed_drive_type(drive_type: u32) -> bool;
fn sample_disks() -> Result<Vec<DiskInfo>, RustNtError>;
fn sample_disk_capacity(root: &[u16]) -> DiskInfo;
```

`sample_disk_capacity` must keep the drive root even when `GetDiskFreeSpaceExW` fails and set both capacity fields to `None`. `sample_disks` must return an error only when logical-drive enumeration itself fails. Sort disks by root before returning.

- [ ] **Step 6: Implement `MonitorSampler::sample`**

Use this flow:

```rust
pub struct MonitorSampler {
    previous: Option<ProcessSnapshot>,
}

impl MonitorSampler {
    pub fn new() -> Self {
        Self { previous: None }
    }

    pub fn sample(&mut self) -> Result<MonitorSnapshot, RustNtError> {
        let current = crate::sample_processes()?;
        let cpu_percent = self
            .previous
            .as_ref()
            .and_then(|previous| current.system_cpu_percent_from(previous));
        let process_changes = self
            .previous
            .as_ref()
            .map(|previous| current.process_changes_from(previous))
            .unwrap_or_default();
        let memory = sample_memory()?;
        let disks = sample_disks()?;
        self.previous = Some(current);
        Ok(MonitorSnapshot {
            cpu_percent,
            memory,
            disks,
            process_changes,
        })
    }
}
```

The previous process snapshot must be replaced only after required memory and disk collection succeeds, so a failed call does not corrupt the next comparison baseline.

- [ ] **Step 7: Run core collection tests and commit**

Run:

```text
cargo test -p rustnt-core -- --nocapture
cargo build -p rustnt-core
cargo clippy -p rustnt-core --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: all core tests pass and the monitor smoke test reports valid physical memory and a usable second sample. Commit:

```text
git add Cargo.toml crates/rustnt-core/src/lib.rs crates/rustnt-core/src/monitor.rs
git commit -m "feat: collect Windows system monitor metrics"
```

### Task 3: Add the `rustnt monitor` CLI Command

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs`
- Test: `crates/rustnt-cli/src/main.rs` unit-test module

**Interfaces:**
- Produces `parse_monitor_options(args: &[String]) -> Result<MonitorOptions, String>`.
- Produces `render_monitor(snapshot: &rustnt_core::monitor::MonitorSnapshot) -> String`.
- Produces a one-shot `run_monitor_command` and a continuous `run_monitor_watch`.
- Leaves process and service parsers unchanged.

- [ ] **Step 1: Write failing parser and renderer tests**

Add:

```rust
#[test]
fn parses_monitor_with_default_one_second_watch() {
    assert_eq!(
        parse_monitor_options(&strings(&["--watch"]))
            .expect("monitor watch should parse")
            .watch_seconds,
        Some(1)
    );
}

#[test]
fn parses_monitor_with_explicit_watch_interval() {
    assert_eq!(
        parse_monitor_options(&strings(&["--watch", "3"]))
            .expect("monitor interval should parse")
            .watch_seconds,
        Some(3)
    );
}

#[test]
fn rejects_invalid_monitor_options() {
    for args in [
        vec!["--watch", "0"],
        vec!["--watch", "-1"],
        vec!["--watch", "not-a-number"],
        vec!["--watch", "2", "--watch", "3"],
        vec!["--unknown"],
    ] {
        assert!(parse_monitor_options(&strings(&args)).is_err());
    }
}
```

Build a `MonitorSnapshot` fixture with CPU `None`, valid memory, one fixed disk with missing capacity, one added process, and no exited process. Assert that `render_monitor` contains `CPU`, `MEMORY`, `DISKS`, `PROCESS CHANGES`, `N/A`, the added PID/name, and `EXITED           none`.

- [ ] **Step 2: Run CLI tests and verify they fail**

Run:

```text
cargo test -p rustnt-cli parses_monitor -- --nocapture
cargo test -p rustnt-cli render_monitor -- --nocapture
```

Expected: compilation failures because the monitor parser and renderer do not exist.

- [ ] **Step 3: Add monitor routing and usage text**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorOptions {
    watch_seconds: Option<u64>,
}
```

Update `main` so `monitor` is routed before the existing `process` branch:

```rust
if args.first().map(String::as_str) == Some("monitor") {
    let options = match parse_monitor_options(&args[1..]) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("usage error: {error}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    return match run_monitor_command(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    };
}
```

Extend `print_usage` with:

```text
usage: rustnt monitor [--watch [seconds]]
```

- [ ] **Step 4: Implement monitor option parsing**

Implement:

```rust
fn parse_monitor_options(args: &[String]) -> Result<MonitorOptions, String> {
    let mut watch_seconds = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] != "--watch" {
            return Err(format!("unknown monitor option: {}", args[index]));
        }
        if watch_seconds.is_some() {
            return Err("duplicate --watch".to_string());
        }
        let seconds = match args.get(index + 1) {
            None => 1,
            Some(value) if value.starts_with("--") => 1,
            Some(value) => {
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--watch requires a positive integer".to_string())?;
                if seconds == 0 {
                    return Err("--watch must be greater than zero".to_string());
                }
                index += 1;
                seconds
            }
        };
        watch_seconds = Some(seconds);
        index += 1;
    }
    Ok(MonitorOptions { watch_seconds })
}
```

This makes `rustnt monitor` a one-shot command, `rustnt monitor --watch` a one-second watch, and `rustnt monitor --watch 5` a five-second watch. A second flag is rejected even if the first flag used the default interval.

- [ ] **Step 5: Implement deterministic monitor rendering**

Import the core monitor types and add:

```rust
fn render_monitor(snapshot: &rustnt_core::monitor::MonitorSnapshot) -> String;
```

Render these fixed sections:

```text
RustNT System Monitor

CPU              <percentage or N/A>
MEMORY           <used> / <total> (<percentage or N/A>)

DISKS
ROOT             FREE        TOTAL       USED
<root>           <free/N/A>   <total/N/A> <percentage/N/A>

PROCESS CHANGES
NEW              <pid name rows or none>
EXITED           <pid name rows or none>
```

Use the existing `format_bytes` helper. Use one decimal place for percentages. Use `none` exactly for an empty added or exited vector. Do not include process command lines, paths, memory, or a full process list in this output.

- [ ] **Step 6: Implement one-shot and watch execution**

Add:

```rust
fn run_monitor_command(options: MonitorOptions) -> Result<(), String> {
    match options.watch_seconds {
        Some(interval) => run_monitor_watch(interval).map_err(|error| error.to_string()),
        None => {
            let mut sampler = rustnt_core::monitor::MonitorSampler::new();
            let snapshot = sampler.sample().map_err(|error| error.to_string())?;
            println!("{}", render_monitor(&snapshot));
            Ok(())
        }
    }
}

fn run_monitor_watch(interval: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut sampler = rustnt_core::monitor::MonitorSampler::new();
    loop {
        let snapshot = sampler.sample()?;
        print!("\x1B[2J\x1B[H{}", render_monitor(&snapshot));
        io::stdout().flush()?;
        std::thread::sleep(Duration::from_secs(interval));
    }
}
```

The sampler must remain alive inside the loop. Do not call the service client or require the service to be installed/running.

- [ ] **Step 7: Run focused CLI tests and commit**

Run:

```text
cargo test -p rustnt-cli -- --nocapture
cargo build -p rustnt-cli
cargo clippy -p rustnt-cli --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: all existing process/service tests and the new monitor parser/renderer tests pass. Commit:

```text
git add crates/rustnt-cli/src/main.rs
git commit -m "feat: add system monitor CLI"
```

### Task 4: Full Verification and Task Documentation

**Files:**
- Modify: `docs/roadmap.md`
- Modify: `.superpowers/sdd/progress.md`
- Create: `.superpowers/sdd/task-11-report.md`
- Test: full workspace verification commands

**Interfaces:**
- Records Task11 implementation status and verification evidence.
- Does not rewrite unrelated historical Task10 entries.
- Produces a clean Git worktree after the documentation commit.

- [ ] **Step 1: Run the complete verification gate**

Run in this order:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Record the actual passing test count and command results in the report. If any command fails, fix the failing behavior first and rerun the complete gate.

- [ ] **Step 2: Update the roadmap and progress record**

In `docs/roadmap.md`, mark Task11 as completed and update the current-next-step wording to identify Task12 as the next planned capability after the verified Task11 work.

In `.superpowers/sdd/progress.md`, append a Task11 entry containing:

```text
Task11 system monitor v1: complete
- command: rustnt monitor
- watch: --watch and --watch <positive seconds>
- metrics: CPU, physical memory, local fixed disks, process additions/exits
- service/protocol changes: none
- verification: full workspace test, build, clippy, format, and diff checks
```

Preserve existing Task10 history and do not remove unrelated user changes.

- [ ] **Step 3: Write the implementation report**

Create `.superpowers/sdd/task-11-report.md` with these headings:

```markdown
# Task11 System Monitor v1 Report

## Delivered

## CLI Usage

## Core Collection

## Tests and Verification

## Known Boundaries

## Commits
```

Under `Known Boundaries`, record first-sample CPU/process baselining, fixed-drive-only behavior, per-disk `N/A` capacity, and best-effort PID fallback when creation time is unavailable. Under `Commits`, list the four Task11 commit IDs after they exist.

- [ ] **Step 4: Review the diff and commit documentation**

Run:

```text
git diff --check
git status --short
git log -4 --oneline
```

Confirm only the intended Task11 files changed, then commit:

```text
git add docs/roadmap.md .superpowers/sdd/progress.md .superpowers/sdd/task-11-report.md
git commit -m "docs: record Task11 system monitor"
```

- [ ] **Step 5: Verify the final repository state**

Run:

```text
git status --short
cargo test --workspace -- --nocapture
```

Expected: `git status --short` is empty and the workspace test suite passes after the documentation commit.
