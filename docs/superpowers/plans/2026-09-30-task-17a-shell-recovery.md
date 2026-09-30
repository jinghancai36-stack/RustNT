# Task17A Shell Recovery Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (\`- [ ]\`) syntax for tracking.

**Goal:** Add a read-only Windows Shell snapshot and bounded RustNT startup-recovery record without modifying the registry, replacing Explorer, starting external processes, or changing privilege boundaries.

**Architecture:** Extend the existing GUI application-data paths with three Task17A files. Keep Windows registry access behind a narrow injected reader interface in \`shell_snapshot.rs\`; keep versioned snapshot persistence there; keep startup transaction and bounded event-log behavior in \`shell_recovery.rs\`; integrate both at the existing \`main.rs\` startup and normal-exit boundaries. Reuse the existing \`recovery.rs\` marker for GUI liveness and do not duplicate its PID/liveness implementation.

**Tech Stack:** Rust 2021 workspace, \`eframe\`/egui GUI, \`serde\` + TOML, \`windows-sys\` Win32 registry APIs, and Rust standard-library filesystem APIs.

## Global Constraints

- Read only \`HKCU\\Software\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon\\Shell\` and \`Userinit\`.
- Never write or delete registry values; never change Windows Shell configuration.
- Never launch, stop, restart, or control Explorer or any external process.
- Store all new files under \`%APPDATA%\\RustNT\`; do not change services, pipes, drivers, kernel, or privileges.
- Persist bounded metadata and a digest; never persist complete registry values.
- Snapshot failure is a degraded notice and must not block GUI startup; session/log failures remain startup errors.
- Every persistent format has \`format_version\`; corrupt or unknown data is preserved.
- Unit tests use injected readers and temporary directories, not the user's registry.

## File Map

- Modify \`crates/rustnt-gui/src/config.rs\`: add three Task17A paths.
- Create \`crates/rustnt-gui/src/shell_snapshot.rs\`: model, injected reader, Windows read-only reader, and atomic snapshot persistence.
- Create \`crates/rustnt-gui/src/shell_recovery.rs\`: transaction state, session marker, bounded recovery log, and coordinator.
- Modify \`crates/rustnt-gui/src/main.rs\`: module declarations and startup/normal-exit integration.
- Modify \`crates/rustnt-gui/src/app.rs\`: one optional bounded startup notice.
- Modify \`docs/roadmap.md\`, \`.superpowers/sdd/progress.md\`; create \`.superpowers/sdd/task-17a-report.md\` after verification.

---

### Task 1: Extend application-data paths

**Files:** Modify \`crates/rustnt-gui/src/config.rs\`; test its inline module.

**Interfaces:** Extend \`ConfigPaths\` with \`shell_snapshot_file\`, \`shell_recovery_log\`, and \`shell_session_file\`.

- [ ] **Step 1: Write the failing test**

Add beside the existing root-path test:

\`\`\`rust
#[test]
fn root_constructor_adds_task17a_paths_without_changing_existing_paths() {
    let root = TestRoot::new("task17a-paths");
    let paths = config_paths_from_root(root.path().to_owned());
    assert_eq!(paths.config_file, root.path().join("gui.toml"));
    assert_eq!(paths.marker_file, root.path().join("gui.running"));
    assert_eq!(paths.crash_log, root.path().join("gui-crash.log"));
    assert_eq!(paths.shell_snapshot_file, root.path().join("shell_snapshot.toml"));
    assert_eq!(paths.shell_recovery_log, root.path().join("shell_recovery.log"));
    assert_eq!(paths.shell_session_file, root.path().join("shell.session"));
    assert!(!root.path().exists());
}
\`\`\`

- [ ] **Step 2: Run to verify failure**

Run \`cargo test -p rustnt-gui root_constructor_adds_task17a_paths_without_changing_existing_paths\`. Expected: compile failure for missing fields.

- [ ] **Step 3: Implement**

Add the three \`PathBuf\` fields and construct them with the filenames shown in the test. Do not alter existing config serialization.

- [ ] **Step 4: Verify and commit**

Run \`cargo test -p rustnt-gui config -- --nocapture\`; expected: all config tests pass. Commit:

\`\`\`text
git add crates/rustnt-gui/src/config.rs
git commit -m "feat: add Task17A application data paths"
\`\`\`

---

### Task 2: Implement the injected snapshot model

**Files:** Create \`crates/rustnt-gui/src/shell_snapshot.rs\`; modify \`main.rs\` with \`pub mod shell_snapshot;\`.

**Interfaces:** Produce \`ShellValueTarget\`, \`RegistryValueType\`, \`RegistryRead\`, \`RegistryReadError\`, \`ShellValueReader\`, \`ShellValueSnapshot\`, \`ShellSnapshot\`, and \`collect_snapshot\`.

- [ ] **Step 1: Write failing tests**

\`\`\`rust
#[test]
fn collect_snapshot_records_metadata_without_full_value() {
    let reader = FixtureReader::with_values(
        Some((RegistryValueType::String, "explorer.exe\\0ignored")),
        Some((RegistryValueType::ExpandString, "%SystemRoot%\\\\system32\\\\userinit.exe")),
    );
    let snapshot = collect_snapshot(&reader, 1_700_000_000, 1234);
    assert_eq!(snapshot.format_version, FORMAT_VERSION);
    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(snapshot.entries[0].target, ShellValueTarget::Shell);
    assert!(snapshot.entries[0].digest.starts_with("fnv1a64:"));
    assert!(snapshot.entries[0].display_value.is_none());
}

#[test]
fn collect_snapshot_preserves_missing_and_reader_error() {
    let reader = FixtureReader::with_results(
        RegistryRead::Missing,
        Err(RegistryReadError::access_denied(5)),
    );
    let snapshot = collect_snapshot(&reader, 10, 20);
    assert_eq!(snapshot.entries[0].result, SnapshotResult::Missing);
    assert_eq!(snapshot.entries[1].result, SnapshotResult::Failed {
        category: "access_denied".to_owned(),
        win32_code: Some(5),
    });
}
\`\`\`

- [ ] **Step 2: Run to verify failure**

Run \`cargo test -p rustnt-gui shell_snapshot -- --nocapture\`. Expected: compile failure because the module and interfaces are absent.

- [ ] **Step 3: Implement the model**

Use \`FORMAT_VERSION: u32 = 1\`; targets \`Shell | Userinit\`; registry types \`String | ExpandString | Other(u32)\`; reads \`Present { value_type, value } | Missing\`; and a reader trait returning \`Result<RegistryRead, RegistryReadError>\`. The serialized entry must contain only target, type, result, character count, and digest, never the raw value.

- [ ] **Step 4: Implement deterministic bounded metadata**

Use FNV-1a over at most 4096 UTF-8 bytes:

\`\`\`rust
fn fnv1a64_digest(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes().iter().take(MAX_VALUE_BYTES) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}
\`\`\`

Always emit entries in \`[Shell, Userinit]\` order; reject embedded NUL and sanitize controls.

- [ ] **Step 5: Verify and commit**

Run \`cargo test -p rustnt-gui shell_snapshot -- --nocapture\`; expected: injected-reader tests pass. Commit:

\`\`\`text
git add crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/shell_snapshot.rs
git commit -m "feat: add injected shell snapshot model"
\`\`\`

---

### Task 3: Add the Windows read-only registry adapter

**Files:** Modify \`crates/rustnt-gui/src/shell_snapshot.rs\`; add Windows-only tests there.

**Interfaces:** Produce \`WindowsShellReader::new()\` implementing \`ShellValueReader\`.

- [ ] **Step 1: Add the adapter test**

\`\`\`rust
#[cfg(windows)]
#[test]
fn windows_reader_returns_typed_results_for_both_targets() {
    let reader = WindowsShellReader::new();
    let shell = reader.read(ShellValueTarget::Shell);
    let userinit = reader.read(ShellValueTarget::Userinit);
    assert!(shell.is_ok() || shell.as_ref().is_err_and(|e| e.win32_code.is_some()));
    assert!(userinit.is_ok() || userinit.as_ref().is_err_and(|e| e.win32_code.is_some()));
}
\`\`\`

- [ ] **Step 2: Implement the narrow wrapper**

Under \`cfg(windows)\`, use only \`RegOpenKeyExW\`, \`RegQueryValueExW\`, and \`RegCloseKey\`. Open \`HKEY_CURRENT_USER\` with \`KEY_QUERY_VALUE\`. Map missing values, bounded access errors, unsupported types, UTF-16 decoding errors, and embedded NULs to typed results. Do not call registry writes, process-launch APIs, \`Command\`, PowerShell, \`reg.exe\`, or Explorer APIs.

- [ ] **Step 3: Verify and commit**

Run \`cargo test -p rustnt-gui shell_snapshot -- --nocapture\` and \`cargo clippy -p rustnt-gui --all-targets -- -D warnings\`; expected: pass. Commit with \`git commit -am "feat: read current user shell values"\`.

---

### Task 4: Persist snapshots atomically

**Files:** Modify \`crates/rustnt-gui/src/shell_snapshot.rs\`; add temporary-directory tests there.

**Interfaces:** Produce \`save_snapshot\`, \`load_snapshot\`, and \`SnapshotError\`.

- [ ] **Step 1: Write failing persistence tests**

Cover first write, repeat replacement, old-file retention, missing file, malformed TOML, and unknown version:

\`\`\`rust
#[test]
fn save_and_load_snapshot_round_trip_without_raw_values() {
    let root = TestRoot::new("snapshot-round-trip");
    let paths = config_paths_from_root(root.path().to_owned());
    let snapshot = fixture_snapshot();
    save_snapshot(&paths, &snapshot).unwrap();
    assert_eq!(load_snapshot(&paths).unwrap().unwrap(), snapshot);
    let encoded = std::fs::read_to_string(&paths.shell_snapshot_file).unwrap();
    assert!(!encoded.contains("explorer.exe"));
}

#[test]
fn unknown_snapshot_version_is_reported_without_deleting_file() {
    let root = TestRoot::new("snapshot-version");
    let paths = config_paths_from_root(root.path().to_owned());
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(&paths.shell_snapshot_file, "format_version = 99\\n").unwrap();
    let error = load_snapshot(&paths).expect_err("unknown version must fail");
    assert!(error.message.contains("unsupported snapshot format"));
    assert!(paths.shell_snapshot_file.exists());
}
\`\`\`

- [ ] **Step 2: Run to verify failure**

Run \`cargo test -p rustnt-gui shell_snapshot -- --nocapture\`. Expected: compile failure for missing persistence functions.

- [ ] **Step 3: Implement**

Serialize only metadata. Reuse the existing unique PID/timestamp/counter temporary names and Windows \`ReplaceFileW\` behavior from \`config.rs\`. On failure delete only the new temporary file. \`load_snapshot\` returns \`Ok(None)\` for missing files and preserves malformed/unknown-version originals.

- [ ] **Step 4: Verify and commit**

Run \`cargo test -p rustnt-gui shell_snapshot -- --nocapture\` and \`cargo test -p rustnt-gui config -- --nocapture\`; expected: pass with no temporary files. Commit \`git commit -am "feat: persist shell snapshots atomically"\`.

---

### Task 5: Implement bounded recovery transactions

**Files:** Create \`crates/rustnt-gui/src/shell_recovery.rs\`; add \`pub mod shell_recovery;\` to \`main.rs\`.

**Interfaces:** Produce \`StartupState\`, \`RecoveryEventKind\`, \`ShellRecovery\`, \`ShellSession\`, \`begin_startup\`, \`record_snapshot_result\`, \`complete_normal_exit\`, and \`RecoveryLogError\`.

- [ ] **Step 1: Write failing state-machine tests**

\`\`\`rust
#[test]
fn clean_start_creates_session_and_running_state() {
    let root = TestRoot::new("clean-start");
    let paths = config_paths_from_root(root.path().to_owned());
    let (recovery, session) = ShellRecovery::begin_startup(&paths, 11, 22).unwrap();
    assert_eq!(recovery.startup_state(), StartupState::Starting);
    assert!(paths.shell_session_file.exists());
    session.mark_running().unwrap();
    assert_eq!(recovery.startup_state(), StartupState::Running);
}

#[test]
fn leftover_session_records_recovery_once_before_new_session() {
    let root = TestRoot::new("recovery-required");
    let paths = config_paths_from_root(root.path().to_owned());
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(&paths.shell_session_file, fixture_session_toml()).unwrap();
    let (_recovery, session) = ShellRecovery::begin_startup(&paths, 33, 44).unwrap();
    assert!(session.path().exists());
    assert!(std::fs::read_to_string(&paths.shell_recovery_log)
        .unwrap().contains("recovery_required"));
}
\`\`\`

- [ ] **Step 2: Run to verify failure**

Run \`cargo test -p rustnt-gui shell_recovery -- --nocapture\`. Expected: compile failure because the module and transitions are absent.

- [ ] **Step 3: Implement versioned state and event model**

Use states \`NotStarted\`, \`Starting\`, \`Running\`, \`RecoveryRequired\`, \`RecoveryRecorded\`, \`SnapshotUnavailable\`, and \`Completed\`. Use event kinds \`RecoveryRequired\`, \`Started\`, \`SnapshotSaved\`, \`SnapshotUnavailable\`, and \`Completed\`. Each newline-delimited event is sanitized and below 512 bytes.

- [ ] **Step 4: Implement marker lifecycle**

\`begin_startup\` creates the root, inspects existing session data, logs exactly one recovery event for a valid unfinished session, and atomically creates the new session. Malformed data is reported without deleting the old file until replacement succeeds. \`mark_running\` atomically updates state; normal exit writes \`completed\` before removing only the current session.

- [ ] **Step 5: Implement bounded log retention**

Read at most \`MAX_LOG_BYTES + 1\`; when over the limit retain newest complete newline-delimited events within \`MAX_LOG_BYTES\`. Rewrite using a unique temporary file and atomic replacement, preserving the old log on failure.

- [ ] **Step 6: Verify and commit**

Run \`cargo test -p rustnt-gui shell_recovery -- --nocapture\` and \`cargo clippy -p rustnt-gui --all-targets -- -D warnings\`; expected: pass. Commit \`git commit -am "feat: add bounded shell recovery transactions"\`.

---

### Task 6: Integrate into GUI startup and normal exit

**Files:** Modify \`crates/rustnt-gui/src/main.rs\` and \`app.rs\`; update inline tests.

**Interfaces:** Consume the existing GUI recovery functions plus Task17A coordinator; produce one optional bounded startup notice and no CLI/service API changes.

- [ ] **Step 1: Write failing integration tests**

\`\`\`rust
#[test]
fn snapshot_failure_becomes_one_startup_notice_without_blocking_gui_state() {
    let notice = crate::shell_recovery::startup_notice(
        crate::shell_recovery::StartupOutcome::SnapshotUnavailable(
            "registry access denied".to_owned(),
        ),
    );
    assert_eq!(
        notice.as_deref(),
        Some("Shell snapshot unavailable: registry access denied")
    );
}
\`\`\`

- [ ] **Step 2: Run to verify failure**

Run \`cargo test -p rustnt-gui main -- --nocapture\`. Expected: compile failure for the new notice/seam.

- [ ] **Step 3: Wire startup**

Preserve existing path creation, GUI inspection, config load, and GUI-marker ordering. Begin the Task17A session, read through \`WindowsShellReader\`, and save the snapshot. Snapshot read/save failures become one optional notice; session/log failures return startup errors before opening a window. Never pass raw values to egui.

- [ ] **Step 4: Wire normal exit**

On normal exit save GUI config, append Task17A \`completed\`, remove the current Task17A session, then remove the existing GUI marker. On eframe error skip normal cleanup so markers remain for next-launch recovery.

- [ ] **Step 5: Add GUI notice coverage and verify**

Pass the optional notice to \`RustNtApp::new\`, enqueue once through the existing notification center, update all constructor tests, then run:

\`\`\`text
cargo test -p rustnt-gui -- --nocapture
cargo clippy -p rustnt-gui --all-targets -- -D warnings
cargo fmt --all -- --check
\`\`\`

Expected: all GUI tests pass, Clippy is clean, and formatting passes. Commit \`git commit -am "feat: integrate Task17A shell recovery into GUI startup"\`.

---

### Task 7: Documentation, review, and final verification

**Files:** Modify \`docs/roadmap.md\` and \`.superpowers/sdd/progress.md\`; create \`.superpowers/sdd/task-17a-report.md\`.

- [ ] **Step 1: Update roadmap after implementation passes**

Change Task16 to \`只读文件浏览、导航、筛选、排序和错误恢复\`; add Task17A as complete and set the current next step to Task17B design review. State explicitly that actual Shell switching is not implemented.

- [ ] **Step 2: Write the measured verification report**

Include scope, no registry writes, no external launches, snapshot degradation, session/log failure behavior, exact command results, and only smoke checks actually executed. Do not leave placeholders.

- [ ] **Step 3: Run the complete matrix**

\`\`\`text
cargo fmt --all -- --check
cargo test --workspace --all-targets -- --test-threads=1
cargo check --workspace --all-targets
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
rg -n "RegSetValue|RegDeleteValue|CreateProcess|ShellExecute|Command::new|powershell|PowerShell|reg.exe|explorer.exe" crates/rustnt-gui/src/shell_snapshot.rs crates/rustnt-gui/src/shell_recovery.rs crates/rustnt-gui/src/main.rs
\`\`\`

Expected: Cargo and diff checks exit 0; the boundary scan finds no prohibited call. Identify the existing TOML semver warning as pre-existing if shown.

- [ ] **Step 4: Run a disposable Windows smoke check**

Use isolated \`APPDATA\`; launch only the built GUI as a normal user; verify snapshot/session/log creation; terminate and relaunch; verify \`recovery_required\`. Do not modify HKCU, Explorer, services, or startup settings. Remove only the disposable directory.

- [ ] **Step 5: Review, document, and commit**

Review for registry writes, external process calls, raw values, unbounded logs, destructive failed replacements, duplicate notices, and changed GUI-marker semantics. Commit:

\`\`\`text
git add docs/roadmap.md .superpowers/sdd/progress.md .superpowers/sdd/task-17a-report.md
git commit -m "docs: record Task17A verification"
\`\`\`

- [ ] **Step 6: Re-run the matrix on the committed tree**

Record final status and recent commits. The worktree must be clean before integration options are presented.

## Self-Review Checklist

- [ ] Every requirement in the Task17A specification maps to Tasks 1-7.
- [ ] Unknown snapshot/session/log versions are preserved and reported.
- [ ] Snapshot failures are degraded notices; session/log failures remain startup errors.
- [ ] Existing \`recovery.rs\` marker ownership and PID protections are reused.
- [ ] Tests never require user-specific registry values.
- [ ] No registry writes, Shell switching, Explorer control, external launch, elevation, service, driver, or kernel behavior is introduced.
- [ ] The final report contains measured results rather than placeholders.
- [ ] Task17B remains outside this plan.
