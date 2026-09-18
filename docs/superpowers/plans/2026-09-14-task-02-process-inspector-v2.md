# Task 02 Process Inspector v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the Windows process inspector with executable paths, optional CPU sampling, filtering, sorting, and synchronous watch-mode refresh.

**Architecture:** `rustnt-core` will collect one process snapshot using documented Win32 APIs and keep CPU time samples behind a Rust API. `rustnt-cli` will parse options, request snapshots, apply filtering and sorting, and render a table. One-shot mode remains immediate and leaves CPU unavailable; watch mode renders a baseline, waits, calculates CPU by PID, and repeats.

**Tech Stack:** Rust 2021, Cargo workspace, `windows-sys 0.59`, Win32 Tool Help, Process Status, and Threading APIs, Rust standard library argument parsing and timing.

## Global Constraints

- Windows-only implementation; do not add GUI libraries.
- Keep Windows API calls inside `rustnt-core`.
- Use documented Windows APIs only.
- Do not invoke PowerShell, WMI, `tasklist`, or other external process-inspection commands.
- Keep `unsafe` blocks small and document their safety assumptions.
- Use RAII for every owned Windows HANDLE.
- Keep per-process path, memory, and CPU failures best effort; snapshot creation failure remains fatal.
- Do not add async runtimes, services, IPC, process termination, ETW, or elevated privileges in Task 02.
- Preserve the command `rustnt process list`.
- Use test-first changes for new behavior and run tests on Windows.
- This workspace is not a Git repository, so commit steps are replaced by local verification checkpoints.

---

### Task 1: Add the snapshot data model and deterministic CPU math

**Files:**
- Modify: `crates/rustnt-core/src/lib.rs`
- Test: `crates/rustnt-core/src/lib.rs` unit-test module

**Interfaces:**
- Preserve `pub fn list_processes() -> Result<Vec<ProcessInfo>, RustNtError>`.
- Extend `ProcessInfo` with:

```rust
pub path: Option<String>,
pub cpu_percent: Option<f32>,
```

- Add an internal `CpuTimes` value containing process kernel/user time and system kernel/user time as `u64` 100-nanosecond units.
- Add a public `ProcessSnapshot` with methods:

```rust
pub fn processes(&self) -> &[ProcessInfo];
pub fn into_processes(self) -> Vec<ProcessInfo>;
pub fn apply_cpu_from(&mut self, previous: &ProcessSnapshot);
```

- Add:

```rust
pub fn sample_processes() -> Result<ProcessSnapshot, RustNtError>;
```

- `list_processes()` will call `sample_processes()` and return `into_processes()`.

- [ ] **Step 1: Write deterministic failing CPU tests**

Add tests for a pure helper named `calculate_cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<f32>`:

```rust
#[test]
fn cpu_percentage_uses_process_delta_over_system_delta() {
    let previous = CpuTimes {
        process_kernel: 100,
        process_user: 100,
        system_kernel: 1_000,
        system_user: 1_000,
    };
    let current = CpuTimes {
        process_kernel: 200,
        process_user: 200,
        system_kernel: 2_000,
        system_user: 2_000,
    };

    assert_eq!(calculate_cpu_percent(previous, current), Some(10.0));
}

#[test]
fn cpu_percentage_is_unavailable_for_invalid_deltas() {
    let previous = CpuTimes {
        process_kernel: 100,
        process_user: 100,
        system_kernel: 1_000,
        system_user: 1_000,
    };
    let zero_system_delta = CpuTimes { ..previous };
    let backward = CpuTimes {
        process_kernel: 50,
        ..previous
    };

    assert_eq!(calculate_cpu_percent(previous, zero_system_delta), None);
    assert_eq!(calculate_cpu_percent(previous, backward), None);
}
```

- [ ] **Step 2: Run the focused test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core cpu_percentage
```

Expected: compilation/test failure because `CpuTimes`, `calculate_cpu_percent`, and the new snapshot API do not exist yet.

- [ ] **Step 3: Implement the minimal model and CPU helper**

Use checked subtraction for every time delta and return `None` if any required delta is zero, backward, or if the computed value is not finite. Calculate:

```rust
let process_delta = process_kernel_delta + process_user_delta;
let system_delta = system_kernel_delta + system_user_delta;
let percentage = (process_delta as f64 / system_delta as f64) * 100.0;
Some(percentage.clamp(0.0, 100.0) as f32)
```

Store per-process CPU times inside `ProcessSnapshot` in a private `HashMap<u32, CpuTimes>` keyed by PID. `apply_cpu_from` will update only PIDs present in both snapshots and will leave all other `cpu_percent` values as `None`.

- [ ] **Step 4: Run the focused tests and workspace format check**

Run:

```powershell
cargo test -p rustnt-core cpu_percentage
cargo fmt --all -- --check
```

Expected: the deterministic CPU tests pass and formatting exits with code 0.

- [ ] **Step 5: Verify the existing integration behavior**

Run:

```powershell
cargo test -p rustnt-core
```

Expected: existing process-enumeration tests and the new CPU tests pass.

---

### Task 2: Collect executable paths, process times, and system times through Win32

**Files:**
- Modify: `crates/rustnt-core/src/lib.rs`
- Modify: `Cargo.toml` only if an additional `windows-sys` feature is required by the compiler
- Test: `crates/rustnt-core/src/lib.rs` Windows integration tests

**Interfaces:**
- `sample_processes()` returns a `ProcessSnapshot` whose `ProcessInfo` records contain:
  - `path: Some(full_path)` when `QueryFullProcessImageNameW` succeeds.
  - `memory_bytes: Some(working_set)` when `GetProcessMemoryInfo` succeeds.
  - `cpu_percent: None` before a prior snapshot is applied.
- `list_processes()` keeps its existing one-shot behavior and returns the records from one snapshot.

- [ ] **Step 1: Add failing Windows assertions for path and baseline CPU**

Extend the Windows process test:

```rust
let snapshot = sample_processes().expect("process sampling should succeed");
assert!(!snapshot.processes().is_empty());
assert!(snapshot.processes().iter().all(|process| process.cpu_percent.is_none()));
assert!(snapshot.processes().iter().any(|process| {
    process.pid == std::process::id() && process.path.is_some()
}));
```

The current implementation does not populate `path`, so the path assertion must fail before implementation.

- [ ] **Step 2: Run the Windows-focused test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core process_enumeration
```

Expected: failure on the new assertion that the current process has a path.

- [ ] **Step 3: Refactor per-process querying around one owned HANDLE**

For each PID, request `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ` with `OpenProcess`. Use one `OwnedHandle` for:

```rust
GetProcessMemoryInfo
GetProcessTimes
QueryFullProcessImageNameW
```

A null or invalid handle returns a record with `memory_bytes`, `path`, and process CPU times unavailable. It must not abort enumeration.

- [ ] **Step 4: Implement FILETIME conversion and system-time collection**

Add a small helper:

```rust
fn filetime_to_u64(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}
```

Call `GetSystemTimes` once per snapshot using three initialized `FILETIME` values. Save system kernel and user totals in the internal `CpuTimes` base.

- [ ] **Step 5: Implement executable path collection**

Allocate a UTF-16 buffer sized for the documented maximum path used by this task, initialize the character count to the buffer length, call `QueryFullProcessImageNameW`, truncate to the returned length, and convert with the existing wide-string helper. Return `None` on any API failure or empty result.

- [ ] **Step 6: Implement process-time collection**

Call `GetProcessTimes` with initialized creation, exit, kernel, and user `FILETIME` structures. Store only kernel and user values in the private per-PID map. A process that exits during sampling receives no CPU baseline entry.

- [ ] **Step 7: Run the Windows tests and inspect real output**

Run:

```powershell
cargo test -p rustnt-core
cargo run -p rustnt-cli -- process list
```

Expected: tests pass; output contains valid paths for accessible processes, `N/A` for inaccessible processes, and no CPU column regression before CLI changes.

---

### Task 3: Add CLI argument parsing, filtering, sorting, and rendering

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs`
- Test: `crates/rustnt-cli/src/main.rs` unit-test module

**Interfaces:**
- Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Pid,
    Name,
    Cpu,
    Memory,
}

struct Options {
    watch_seconds: Option<u64>,
    filter: Option<String>,
    sort: SortKey,
}
```

- Add:

```rust
fn parse_options(args: &[String]) -> Result<Options, String>;
fn matches_filter(process: &ProcessInfo, filter: Option<&str>) -> bool;
fn sort_processes(processes: &mut [ProcessInfo], sort: SortKey);
```

- [ ] **Step 1: Write failing parser, filter, and sort tests**

Add tests with synthetic `ProcessInfo` records:

```rust
#[test]
fn parses_watch_filter_and_sort_options() {
    let args = strings(&["--watch", "2", "--filter", "edge", "--sort", "cpu"]);
    let options = parse_options(&args).expect("options should parse");
    assert_eq!(options.watch_seconds, Some(2));
    assert_eq!(options.filter.as_deref(), Some("edge"));
    assert_eq!(options.sort, SortKey::Cpu);
}

#[test]
fn rejects_missing_or_zero_watch_interval() {
    assert!(parse_options(&strings(&["--watch"])).is_err());
    assert!(parse_options(&strings(&["--watch", "0"])).is_err());
}

#[test]
fn filter_matches_name_and_path_case_insensitively() {
    let process = process(42, "Edge.exe", Some("C:\\Apps\\Microsoft\\Edge.exe"));
    assert!(matches_filter(&process, Some("MICROSOFT")));
    assert!(!matches_filter(&process, Some("not-found")));
}

#[test]
fn sort_by_memory_places_unavailable_values_last() {
    let mut processes = vec![
        process_with_memory(1, Some(100)),
        process_with_memory(2, None),
        process_with_memory(3, Some(10)),
    ];
    sort_processes(&mut processes, SortKey::Memory);
    assert_eq!(processes.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![3, 1, 2]);
}
```

- [ ] **Step 2: Run focused CLI tests and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-cli parse_watch_filter_sort
```

Expected: compilation/test failure because the option types and helpers do not exist.

- [ ] **Step 3: Implement strict standard-library option parsing**

Accept only:

```text
--watch <positive integer>
--filter <text>
--sort pid
--sort name
--sort cpu
--sort memory
```

Reject unknown flags, duplicate flags, missing values, invalid sort keys, and zero watch intervals with an error string. Keep the existing `process list` command validation and return exit code 2 for usage errors.

- [ ] **Step 4: Implement filtering and deterministic sorting**

Match the filter against `ProcessInfo.name` and `ProcessInfo.path.unwrap_or_default()` using lowercase strings. Sort ascending by the selected key. For CPU and memory, compare `Some` values numerically and place `None` after all available values. Use PID as the final tie-breaker for deterministic output.

- [ ] **Step 5: Update table rendering**

Render these columns:

```text
PID PROCESS CPU THREADS MEMORY PATH
```

Use `N/A` for missing CPU, memory, and path. Format CPU as one decimal percentage and preserve the existing binary memory formatting. Add an ASCII path truncation helper that keeps the final visible width stable.

- [ ] **Step 6: Run the focused and full CLI tests**

Run:

```powershell
cargo test -p rustnt-cli
cargo fmt --all -- --check
```

Expected: parser, filtering, sorting, formatting, and existing tests pass with no formatting differences.

---

### Task 4: Implement synchronous watch mode and update documentation

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs`
- Modify: `docs/architecture.md`
- Modify: `docs/learning-notes.md`
- Modify: `docs/benchmarks.md`
- Modify: `README.md`

**Interfaces:**
- `main()` will accept `rustnt process list` plus the Task 02 options.
- One-shot mode will call `rustnt_core::list_processes()`.
- Watch mode will call `rustnt_core::sample_processes()`, render the baseline, sleep `Duration::from_secs(interval)`, apply CPU from the previous snapshot, render, and repeat.

- [ ] **Step 1: Add a failing CLI smoke assertion for new headers**

Add a testable `render_processes` function and test:

```rust
#[test]
fn render_includes_task_02_columns() {
    let output = render_processes(&[process(7, "demo.exe", None)], SortKey::Pid);
    assert!(output.contains("CPU"));
    assert!(output.contains("MEMORY"));
    assert!(output.contains("PATH"));
}
```

- [ ] **Step 2: Run the smoke test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-cli render_includes_task_02_columns
```

Expected: failure until the new renderer exists.

- [ ] **Step 3: Implement one-shot rendering**

Build a `Vec<ProcessInfo>`, apply `matches_filter`, call `sort_processes`, and return a formatted `String` from `render_processes`. `main()` prints the string once and exits successfully.

- [ ] **Step 4: Implement watch mode**

Use:

```rust
let mut previous = rustnt_core::sample_processes()?;
loop {
    let mut current = rustnt_core::sample_processes()?;
    current.apply_cpu_from(&previous);
    print!("\x1B[2J\x1B[H{}", render_processes(current.processes(), options.sort));
    std::io::stdout().flush()?;
    previous = current;
    std::thread::sleep(Duration::from_secs(interval));
}
```

Apply filtering before rendering each snapshot. Ctrl+C may terminate the process using the normal console behavior; no signal-handling dependency is added.

- [ ] **Step 5: Update documentation**

Document:

```powershell
rustnt process list
rustnt process list --sort memory
rustnt process list --filter edge
rustnt process list --watch 1 --sort cpu
```

Explain that one-shot CPU is `N/A`, watch mode requires two samples, path and memory access are best effort, and CPU is a process-time delta divided by system-time delta.

- [ ] **Step 6: Run all verification commands**

Run:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo run -p rustnt-cli -- process list
cargo run -p rustnt-cli -- process list --sort memory
cargo run -p rustnt-cli -- process list --filter edge
```

Expected:

- formatting exits 0;
- all workspace tests pass;
- the default command prints PID, process, CPU, threads, memory, and path columns;
- sorting and filtering commands exit 0;
- no PowerShell, WMI, tasklist, or GUI dependency is introduced.

- [ ] **Step 7: Perform final scope review**

Check the source tree for:

```powershell
rg -n "PowerShell|tasklist|wmic|WMI|tokio|unsafe|CloseHandle" .
```

Confirm every new `unsafe` block has a safety comment, every owned HANDLE is wrapped by `OwnedHandle`, and no Task 03 service or privilege code has been added.
