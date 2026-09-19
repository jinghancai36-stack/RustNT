# Task 09 Privileged End-to-End Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a disposable Task09 target process and complete the repeatable validation record for Task08's privileged service boundary.

**Architecture:** Add one dependency-free workspace binary that prints its PID and sleeps without changing system state. Keep production service and protocol behavior unchanged unless a validation defect is reproduced; record automated preflight and elevated manual results in a Task09 report.

**Tech Stack:** Rust 2021, Cargo workspace, existing `rustnt-core`, `rustnt-cli`, and `rustnt-service` crates, Windows SCM and Named Pipe behavior validated through the existing CLI.

## Global Constraints

- Do not add new service protocol commands or broaden process-control authorization.
- Do not use PowerShell, WMI, shell commands, scripts, arbitrary executable paths, or remote control as product behavior.
- The positive termination target must be the disposable helper process only.
- Never terminate PID 0, PID 4, the RustNT service, or a system-owned process as a positive-path action.
- Keep all Windows API calls in `rustnt-core`; the helper uses only the Rust standard library.
- Any production fix found during validation requires a regression test and its own commit.
- The service must be stopped and uninstalled after an elevated run.

---

### Task 1: Add the Disposable Validation Target

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/rustnt-task09-target/Cargo.toml`
- Create: `crates/rustnt-task09-target/src/main.rs`

**Interfaces:**
- Produces a workspace binary named `rustnt-task09-target`.
- Prints exactly one startup line in the form `RUSTNT_TASK09_TARGET_PID=<positive decimal PID>`.
- Remains alive until externally terminated or interrupted.

- [ ] **Step 1: Write the failing unit test**

Add a test in the new binary source for the startup-line formatter:

```rust
#[test]
fn startup_line_contains_the_target_pid() {
    assert_eq!(
        startup_line(1234),
        "RUSTNT_TASK09_TARGET_PID=1234\n"
    );
}
```

Run:

```text
cargo test -p rustnt-task09-target startup_line_contains_the_target_pid
```

Expected: FAIL because the package, function, and test target do not exist yet.

- [ ] **Step 2: Add the workspace package and minimal implementation**

Add `"crates/rustnt-task09-target"` to the workspace members and create:

```toml
[package]
name = "rustnt-task09-target"
version.workspace = true
edition.workspace = true
license.workspace = true
```

Implement the target with:

```rust
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

fn startup_line(pid: u32) -> String {
    format!("RUSTNT_TASK09_TARGET_PID={pid}\n")
}

fn main() {
    print!("{}", startup_line(std::process::id()));
    io::stdout().flush().expect("startup line must be flushed");
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    use super::startup_line;

    #[test]
    fn startup_line_contains_the_target_pid() {
        assert_eq!(
            startup_line(1234),
            "RUSTNT_TASK09_TARGET_PID=1234\n"
        );
    }
}
```

- [ ] **Step 3: Run the focused test and build**

Run:

```text
cargo test -p rustnt-task09-target startup_line_contains_the_target_pid
cargo build -p rustnt-task09-target
```

Expected: the focused test passes and the helper executable is produced.

- [ ] **Step 4: Verify the helper is disposable**

Run the built executable in a separate terminal:

```text
target\debug\rustnt-task09-target.exe
```

Expected: it prints one positive PID and remains idle. Stop it with the terminal interrupt; no service or system state is changed.

- [ ] **Step 5: Commit**

```text
git add Cargo.toml crates/rustnt-task09-target
git commit -m "test: add disposable Task09 target"
```

### Task 2: Record Automated Preflight and Manual Matrix

**Files:**
- Create: `.superpowers/sdd/task-9-report.md`
- Modify: `.superpowers/sdd/progress.md`

**Interfaces:**
- The report records exact commands, exit results, environment details, skipped elevated cases, and cleanup state.
- The progress file records Task09 status without claiming privileged success when the token is not elevated.

- [ ] **Step 1: Run automated preflight**

Run:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
rustnt service status
```

Record the observed test count and the service state. If the service is not installed, record `NOT_INSTALLED` as the clean preflight state.

- [ ] **Step 2: Write the report from observed output**

Include:

```text
Date/time:
Commit:
Windows version/build:
Rust toolchain:
Account:
Elevated:
Administrators membership:
Service state before:
Service state after:
Helper PID:
Automated results:
Elevated results:
Skipped cases and exact reason:
Residual process/service cleanup:
```

Do not invent measurements or mark skipped Administrator/UAC cases as passed.

- [ ] **Step 3: Update progress**

Append a Task09 entry that distinguishes:

- helper and automated preflight complete;
- elevated end-to-end validation passed or is environment-gated;
- any production fixes and their commit IDs.

- [ ] **Step 4: Commit the report**

Because `.superpowers/sdd/.gitignore` ignores reports by default, force-add the report:

```text
git add .superpowers/sdd/progress.md
git add -f .superpowers/sdd/task-9-report.md
git commit -m "docs: record Task09 validation"
```

### Task 3: Run the Elevated Validation When Available

**Files:**
- Modify: `.superpowers/sdd/task-9-report.md`
- Modify: `.superpowers/sdd/progress.md`

**Interfaces:**
- Uses the existing `rustnt` CLI and `rustnt-task09-target` executable.
- Does not modify production authorization to make a case pass.

- [ ] **Step 1: Prepare an elevated terminal**

Open a terminal with an interactive Administrator token. Confirm the account and elevation with the existing service identity/status commands before installing anything.

- [ ] **Step 2: Run the positive path**

Run:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
target\debug\rustnt-task09-target.exe
rustnt process inspect --pid <HELPER_PID>
rustnt process terminate --pid <HELPER_PID>
rustnt service stop
rustnt service uninstall
rustnt service status
```

Expected: service reaches `RUNNING`, identity reports the fixed capabilities, inspection returns bounded metadata, termination reports `TERMINATED` or an explicitly handled `TERMINATE_PENDING`, and final status is `NOT_INSTALLED`.

- [ ] **Step 3: Run the rejection path**

Record the results of:

```text
rustnt process inspect --pid 4
rustnt process terminate --pid 4
rustnt process terminate --pid <RUSTNT_SERVICE_PID>
```

Expected: protected-target or access-denied status; no system or service process is terminated.

- [ ] **Step 4: Verify cleanup**

Confirm the helper is gone, the service is `NOT_INSTALLED`, and no RustNT service process remains. Record cleanup output in the report.

- [ ] **Step 5: Run the final verification**

Run:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
git status --short --branch
```

Expected: all commands exit successfully and the working tree is clean after the report commit.
