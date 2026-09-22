# Task13 Window and Session Viewer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add read-only `rustnt window list`, `rustnt window foreground`, and `rustnt session list` commands for the current user's Windows desktop session.

**Architecture:** Keep all Windows collection in a new `rustnt_core::window` module. Enumerate top-level windows with User32, filter them to the current process's Session and desktop, reuse `list_processes()` for process metadata, and enumerate Session metadata with WTS APIs. Keep the CLI responsible for strict command parsing, stable human-readable rendering, and exit-code mapping; do not add a service, protocol command, GUI, or window mutation path.

**Tech Stack:** Rust 2021, existing Cargo workspace, `windows-sys 0.59`, Win32 User32, Stations and Desktops, Remote Desktop/WTS, existing process enumeration, standard-library collections and tests, Cargo test/build/Clippy/rustfmt, Git worktree.

## Global Constraints

- Task13 is Windows-only, user-mode, read-only, and scoped to the current process's Session and current interactive desktop for window enumeration.
- `rustnt window list` enumerates all top-level windows in that scope, including hidden and empty-title windows; it never enumerates child windows.
- `rustnt window foreground` never crosses the current Session or desktop boundary and returns a safe `none` result when no matching foreground window exists.
- `rustnt session list` enumerates local Windows Sessions and marks the current process Session and active console Session.
- Do not start the LocalSystem service, request elevation, inject code, change window state, add a Named Pipe capability, or modify the protocol.
- Use existing `windows-sys 0.59`; add only the required existing feature flags and no third-party dependencies.
- Return structured values without HWND, HDESK, WTS, or process-handle ownership escaping the core API.
- Release every WTS allocation and temporary Win32 resource before the core API returns.
- Preserve `EnumWindows` Z order and sort Session results by ascending Session ID.
- Missing best-effort text or process metadata is represented as `None` where the public model permits it; CLI renders missing optional text as `N/A`.
- CLI argument errors return exit code `2`, operation and Win32 errors return exit code `1`, and successful commands return exit code `0`.
- Existing process, monitor, filesystem, service, and Task09 commands must retain their behavior.
- Normal verification must include workspace tests, workspace build, workspace Clippy with `-D warnings`, rustfmt check, diff check, and real runs of all three new commands.

---

## File Map

- Create `crates/rustnt-core/src/window.rs`: WindowInfo, WindowSnapshot, SessionState, SessionInfo, WindowError, pure mappings, User32 window enumeration, foreground lookup, WTS Session enumeration, and resource guards.
- Modify `crates/rustnt-core/src/lib.rs`: export `pub mod window;`.
- Modify `Cargo.toml`: add only `Win32_UI_WindowsAndMessaging`, `Win32_System_StationsAndDesktops`, and `Win32_System_RemoteDesktop` to the existing `windows-sys` feature list.
- Modify `crates/rustnt-cli/src/main.rs`: add window/session command types, routing, strict parsers, renderers, operation runners, and CLI tests.
- Modify `README.md`: document the three read-only commands and their current Session/current desktop boundary.
- Modify `docs/roadmap.md`: mark Task13 complete and set Task14 as the next capability after verification.
- Create `.superpowers/sdd/task-13-report.md`: record actual implementation commits, verification counts, smoke output, and environment boundaries.
- Modify `.superpowers/sdd/progress.md`: append the completed Task13 task/review ledger entry after all review gates pass.

## Interfaces

The core module must expose:

```rust
pub fn enumerate_windows() -> Result<WindowSnapshot, WindowError>;
pub fn foreground_window() -> Result<Option<WindowInfo>, WindowError>;
pub fn list_sessions() -> Result<Vec<SessionInfo>, WindowError>;
```

The public data structures must match the approved specification:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub hwnd: usize,
    pub title: String,
    pub class_name: String,
    pub pid: u32,
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub thread_id: u32,
    pub desktop_name: Option<String>,
    pub session_id: u32,
    pub visible: bool,
    pub minimized: bool,
    pub foreground: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub current_session_id: u32,
    pub current_desktop: String,
    pub windows: Vec<WindowInfo>,
    pub skipped_windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Connected,
    ConnectQuery,
    Shadow,
    Disconnected,
    Idle,
    Listen,
    Reset,
    Down,
    Init,
    Other(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u32,
    pub state: SessionState,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub client_name: Option<String>,
    pub is_current_session: bool,
    pub is_active_console: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    Win32 { operation: String, code: u32 },
    Enumeration(String),
    InvalidData(String),
}
```

## Task 1: Core Window Model and Top-Level Enumeration

**Files:**

- Create `crates/rustnt-core/src/window.rs`
- Modify `crates/rustnt-core/src/lib.rs`
- Modify `Cargo.toml`
- Test `crates/rustnt-core/src/window.rs` unit and Windows integration tests

**Consumes:**

- Existing `rustnt_core::list_processes() -> Result<Vec<ProcessInfo>, RustNtError>`.
- Existing `windows-sys` Foundation and Threading features.

**Produces:**

- Window public types and `WindowError`.
- Pure helpers `map_session_state`, `wide_string`, `format_hwnd`, and status helpers.
- `enumerate_windows()` and internal helpers for current Session/current desktop lookup and per-window collection.

- [ ] **Step 1: Add the failing pure tests and export stub**

Add `pub mod window;` to `crates/rustnt-core/src/lib.rs` and add tests in the new module for:

```rust
#[test]
fn session_state_mapping_preserves_known_and_unknown_values() {
    assert_eq!(map_session_state(windows_sys::Win32::System::RemoteDesktop::WTSActive), SessionState::Active);
    assert_eq!(map_session_state(windows_sys::Win32::System::RemoteDesktop::WTSConnected), SessionState::Connected);
    assert_eq!(map_session_state(77), SessionState::Other(77));
}

#[test]
fn wide_string_stops_at_the_first_nul() {
    assert_eq!(wide_string(&['R' as u16, 'N' as u16, 0, 'T' as u16]), "RN");
}

#[test]
fn format_hwnd_uses_fixed_pointer_style() {
    assert_eq!(format_hwnd(0x1234), "0x0000000000001234");
}

#[test]
fn missing_text_is_renderable_without_panicking() {
    assert_eq!(optional_text(None), "N/A");
    assert_eq!(optional_text(Some("Explorer".to_string())), "Explorer");
}
```

Run:

```text
cargo test -p rustnt-core window::tests::session_state_mapping_preserves_known_and_unknown_values -- --nocapture
```

Expected result: a compile failure naming the missing `window` module/helpers. Fix only test setup errors before adding production behavior.

- [ ] **Step 2: Implement the core data model and pure helpers**

Add the exact public structures from the Interfaces section. Implement:

```rust
fn map_session_state(value: i32) -> SessionState;
fn wide_string(value: &[u16]) -> String;
fn wide_string_from_ptr(value: *const u16) -> Option<String>;
fn format_hwnd(hwnd: usize) -> String;
fn optional_text(value: Option<String>) -> String;
```

Map the `WTS_CONNECTSTATE_CLASS` values `WTSActive`, `WTSConnected`,
`WTSConnectQuery`, `WTSShadow`, `WTSDisconnected`, `WTSIdle`, `WTSListen`,
`WTSReset`, `WTSDown`, and `WTSInit` to the matching enum variants. Preserve all
other integer values in `SessionState::Other`.

Keep pure helpers independent from Win32 calls so they run in the existing unit-test target.

- [ ] **Step 3: Implement current Session and desktop helpers**

Implement Windows-only helpers with narrow unsafe blocks:

```rust
fn current_session_id() -> Result<u32, WindowError>;
fn current_desktop_name() -> Result<String, WindowError>;
fn thread_desktop_name(thread_id: u32) -> Option<String>;
```

Use `GetCurrentProcessId` plus `ProcessIdToSessionId` for the current Session.
Use `GetCurrentThreadId`, `GetThreadDesktop`, and `GetUserObjectInformationW`
with `UOI_NAME` for the current desktop. For a desktop name query, first call
`GetUserObjectInformationW` with a null buffer and zero length to obtain the
required byte count, allocate a UTF-16 buffer, call again, and remove the
terminating NUL. Treat a null desktop handle or failed current-desktop query as
`WindowError::Win32`.

The thread desktop handle is borrowed from the thread desktop API; do not close
it and do not store it in a returned structure.

- [ ] **Step 4: Implement per-window collection and EnumWindows callback**

Build a PID lookup from `list_processes()`. On process enumeration failure, keep
the window command usable with an empty lookup because process metadata is
best-effort; do not fail the whole window command.

Use `EnumWindows` with an `unsafe extern "system"` callback and a pointer to a
short-lived Rust collection held only during the call. For each HWND:

1. Call `GetWindowThreadProcessId`; if it returns thread ID zero or PID zero,
   increment `skipped_windows` and continue.
2. Call `ProcessIdToSessionId`; if it fails, increment `skipped_windows`.
3. Read the window thread desktop name; if it is missing or differs from the
   current desktop, skip the window without crossing the desktop boundary.
4. Read title with `GetWindowTextLengthW` plus a UTF-16 buffer and class with
   `GetClassNameW`; preserve empty results as empty strings.
5. Set `visible` from `IsWindowVisible`, `minimized` from `IsIconic`, and
   `foreground` from a single `GetForegroundWindow` value captured before
   enumeration.
6. Add a `WindowInfo` with HWND converted to `usize`, PID, process lookup
   fields, thread ID, desktop name, current Session ID, and status flags.

Filter by current Session before appending. Preserve callback order. Set
`skipped_windows` only for windows whose identity cannot be established; a
window with unreadable optional metadata remains in the result.

After `EnumWindows` returns false, return `WindowError::Win32` only if the
callback did not request early termination and the API's last error is nonzero;
otherwise return the collected snapshot. Do not use callback payloads that borrow
stack references after `EnumWindows` returns.

- [ ] **Step 5: Run focused core tests and Windows smoke tests**

Run:

```text
cargo test -p rustnt-core window::tests -- --nocapture
cargo test -p rustnt-core -- --nocapture
```

Add Windows tests:

```rust
#[test]
#[cfg(windows)]
fn enumerate_windows_stays_in_current_session_and_has_valid_identity() {
    let snapshot = enumerate_windows().expect("window enumeration should succeed");
    assert!(!snapshot.current_desktop.is_empty());
    assert!(snapshot.windows.iter().all(|window| {
        window.session_id == snapshot.current_session_id
            && window.hwnd != 0
            && window.pid != 0
            && window.thread_id != 0
    }));
}

#[test]
#[cfg(windows)]
fn current_session_id_and_desktop_are_available() {
    assert!(current_session_id().expect("current session should resolve") > 0);
    assert!(!current_desktop_name()
        .expect("current desktop should resolve")
        .is_empty());
}
```

Expected: all pure tests, current-session tests, and existing core tests pass.

- [ ] **Step 6: Commit the core window slice**

```text
git add Cargo.toml crates/rustnt-core/src/lib.rs crates/rustnt-core/src/window.rs
git commit -m "feat: enumerate current desktop windows"
```

## Task 2: Foreground Lookup and WTS Session Enumeration

**Files:**

- Modify `crates/rustnt-core/src/window.rs`
- Test `crates/rustnt-core/src/window.rs`

**Consumes:**

- Task 1 data model, current Session/current desktop helpers, window collection.
- `windows-sys` Remote Desktop feature.

**Produces:**

- `foreground_window()`.
- `list_sessions()`.
- WTS allocation guards and session information decoding.

- [ ] **Step 1: Add failing pure tests for WTS mapping and session ordering**

Add tests:

```rust
#[test]
fn session_state_values_cover_the_wts_contract() {
    for (value, expected) in [
        (0, SessionState::Active),
        (1, SessionState::Connected),
        (2, SessionState::ConnectQuery),
        (3, SessionState::Shadow),
        (4, SessionState::Disconnected),
        (5, SessionState::Idle),
        (6, SessionState::Listen),
        (7, SessionState::Reset),
        (8, SessionState::Down),
        (9, SessionState::Init),
    ] {
        assert_eq!(map_session_state(value), expected);
    }
}

#[test]
fn session_info_sort_is_ascending() {
    let mut sessions = vec![
        SessionInfo { session_id: 9, state: SessionState::Idle, username: None, domain: None, client_name: None, is_current_session: false, is_active_console: false },
        SessionInfo { session_id: 1, state: SessionState::Active, username: None, domain: None, client_name: None, is_current_session: true, is_active_console: true },
    ];
    sessions.sort_by_key(|session| session.session_id);
    assert_eq!(sessions[0].session_id, 1);
    assert_eq!(sessions[1].session_id, 9);
}
```

Run:

```text
cargo test -p rustnt-core window::tests::session_state_values_cover_the_wts_contract -- --nocapture
```

Expected: compile/test failure until the session-facing implementation is present.

- [ ] **Step 2: Implement WTS RAII guards and query decoding**

Add a guard for WTS memory:

```rust
struct WtsMemory(*mut std::ffi::c_void);

impl Drop for WtsMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WTSFreeMemory(self.0) };
        }
    }
}
```

Use `WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, ...)`. When the
call succeeds, wrap the returned array in `WtsMemory`, iterate exactly
`count` `WTS_SESSION_INFOW` values, and query `WTSUserName`, `WTSDomainName`,
and `WTSClientName` individually with `WTSQuerySessionInformationW`. A failed
optional query yields `None` for that field and does not discard the Session.

For query strings, wrap each returned pointer in `WtsMemory` immediately,
interpret `bytes_returned / size_of::<u16>()` units, trim at the first NUL, and
drop the guard before moving to the next query. Reject an odd byte count with
`WindowError::InvalidData`.

Read `WTSGetActiveConsoleSessionId()` once, read the current process Session
once, map the state, mark both booleans, sort by Session ID, and return the
vector. If the main enumeration call fails, return `WindowError::Win32`.

- [ ] **Step 3: Implement foreground lookup without crossing the boundary**

Capture `GetForegroundWindow()`. If it is null, return `Ok(None)`. Otherwise
reuse the same per-window collection path from Task 1 for that HWND only:

- validate thread ID and PID;
- resolve PID Session and compare with current Session;
- resolve the window thread desktop and compare with current desktop;
- read optional metadata and status flags;
- return `Some(WindowInfo)` if all identity checks pass, otherwise `None`.

Do not call `SetForegroundWindow`, `ShowWindow`, `GetWindow`, or any other
mutation/navigation API. The returned structure must mark `foreground: true`.

- [ ] **Step 4: Run focused and integration tests**

Run:

```text
cargo test -p rustnt-core window::tests -- --nocapture
cargo test -p rustnt-core -- --nocapture
```

Add Windows tests:

```rust
#[test]
#[cfg(windows)]
fn session_list_is_sorted_and_marks_current_session() {
    let sessions = list_sessions().expect("session enumeration should succeed");
    assert!(sessions.windows(2).all(|pair| pair[0].session_id <= pair[1].session_id));
    let current_count = sessions.iter().filter(|session| session.is_current_session).count();
    assert_eq!(current_count, 1);
}

#[test]
#[cfg(windows)]
fn foreground_window_is_safe_when_unavailable() {
    let result = foreground_window().expect("foreground query should not fail");
    if let Some(window) = result {
        assert!(window.foreground);
        assert!(window.hwnd != 0);
        assert!(window.session_id > 0);
    }
}
```

Expected: all focused and existing core tests pass.

- [ ] **Step 5: Commit the Session/foreground slice**

```text
git add crates/rustnt-core/src/window.rs
git commit -m "feat: read windows sessions and foreground window"
```

## Task 3: CLI Commands, Documentation, and Verification

**Files:**

- Modify `crates/rustnt-cli/src/main.rs`
- Modify `README.md`
- Modify `docs/roadmap.md`
- Create `.superpowers/sdd/task-13-report.md`
- Test `crates/rustnt-cli/src/main.rs`

**Consumes:**

- `rustnt_core::window::{enumerate_windows, foreground_window, list_sessions}`.
- Task 1 and Task 2 public models and error type.
- Existing CLI routing, `format_bytes` style, and exit-code behavior.

**Produces:**

- Strict `window list`, `window foreground`, and `session list` parsing/routing.
- Fixed human-readable renderers and smoke-verification report.

- [ ] **Step 1: Add failing parser and renderer tests**

Add:

```rust
#[test]
fn parses_window_and_session_commands() {
    assert_eq!(
        parse_window_command(&strings(&["list"])).expect("window list should parse"),
        WindowCommand::List
    );
    assert_eq!(
        parse_window_command(&strings(&["foreground"])).expect("foreground should parse"),
        WindowCommand::Foreground
    );
    assert_eq!(
        parse_session_command(&strings(&["list"])).expect("session list should parse"),
        SessionCommand::List
    );
}

#[test]
fn rejects_window_and_session_extra_arguments() {
    for args in [
        vec![],
        vec!["list", "extra"],
        vec!["unknown"],
    ] {
        assert!(parse_window_command(&strings(&args)).is_err());
    }
    assert!(parse_session_command(&strings(&[])).is_err());
    assert!(parse_session_command(&strings(&["list", "extra"])).is_err());
}

#[test]
fn renders_window_fields_in_fixed_order() {
    let window = sample_window_info();
    let output = render_window_info(&window);
    for pair in [
        ("HWND", "TITLE"),
        ("TITLE", "CLASS"),
        ("CLASS", "PID"),
        ("PID", "PROCESS"),
        ("PROCESS", "PATH"),
        ("PATH", "THREAD"),
        ("THREAD", "DESKTOP"),
        ("DESKTOP", "SESSION"),
        ("SESSION", "VISIBLE"),
        ("VISIBLE", "MINIMIZED"),
        ("MINIMIZED", "FOREGROUND"),
    ] {
        assert!(output.find(pair.0).unwrap() < output.find(pair.1).unwrap());
    }
    assert!(output.contains("VISIBLE         yes"));
    assert!(output.contains("MINIMIZED       no"));
}

#[test]
fn renders_empty_foreground_and_session_optional_values() {
    assert!(render_foreground_window(None).contains("WINDOW          none"));
    let session = SessionInfo {
        session_id: 1,
        state: SessionState::Other(77),
        username: None,
        domain: None,
        client_name: None,
        is_current_session: false,
        is_active_console: false,
    };
    let output = render_session_info(&session);
    assert!(output.contains("STATE           OTHER(77)"));
    assert!(output.contains("USER            N/A"));
    assert!(output.contains("CURRENT         no"));
}
```

Run:

```text
cargo test -p rustnt-cli tests::parses_window_and_session_commands -- --nocapture
```

Expected: compile failure because the command types, parsers, fixtures, and renderers do not exist.

- [ ] **Step 2: Implement strict CLI parsing and routing**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCommand {
    List,
    Foreground,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCommand {
    List,
}
```

Route `window` and `session` before the existing `process` fallback. Each parser
must require exactly one subcommand and no extra argument. Unknown commands and
missing subcommands return `Err(String)`. Usage must include exactly:

```text
usage: rustnt window list
usage: rustnt window foreground
usage: rustnt session list
```

Map parser errors to `ExitCode::from(2)`, operation errors to `ExitCode::from(1)`,
and successful operations to `ExitCode::SUCCESS`.

- [ ] **Step 3: Implement deterministic renderers**

Implement:

```rust
fn render_window_snapshot(snapshot: &WindowSnapshot) -> String;
fn render_window_info(window: &WindowInfo) -> String;
fn render_foreground_window(window: Option<&WindowInfo>) -> String;
fn render_session_info(session: &SessionInfo) -> String;
fn render_sessions(sessions: &[SessionInfo]) -> String;
fn session_state_name(state: &SessionState) -> String;
```

Use the fixed field order from the specification. Format HWND as
`0x` plus 16 uppercase hexadecimal digits. Render booleans as lowercase
`yes`/`no`, optional strings as `N/A`, and separate windows/Sessions with one
blank line. `render_window_snapshot` must include `SESSION`, `DESKTOP`,
`WINDOWS`, and `SKIPPED` before the window blocks. `render_foreground_window(None)`
must produce `RustNT Foreground Window` and `WINDOW          none`.

- [ ] **Step 4: Implement command runners**

Call only the core APIs:

```rust
WindowCommand::List => rustnt_core::window::enumerate_windows()
WindowCommand::Foreground => rustnt_core::window::foreground_window()
SessionCommand::List => rustnt_core::window::list_sessions()
```

Map `WindowError` variants to readable strings without invoking the service.
Write successful structured output to stdout and errors to stderr.

- [ ] **Step 5: Update documentation and verification report**

Add a README section documenting:

```text
rustnt window list
rustnt window foreground
rustnt session list
```

State that window enumeration is current-user, current-Session, current-desktop,
top-level-only, includes hidden/empty-title windows, and is read-only. State that
Session metadata is best-effort and that no service/elevation/window mutation is
used.

Update `docs/roadmap.md` date to `2026-09-22`, mark Task13 complete, and set
Task14 as the next step.

Create `.superpowers/sdd/task-13-report.md` with actual command output, test
counts, build/Clippy/fmt/diff results, and any desktop-environment limitations.
Do not claim cross-Session coverage or a reparse-style guarantee that was not
tested.

- [ ] **Step 6: Run complete verification and smoke commands**

Run:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
cargo run -p rustnt-cli -- window list
cargo run -p rustnt-cli -- window foreground
cargo run -p rustnt-cli -- session list
```

Record actual outputs and counts in the Task13 report. Verify that the existing
process, monitor, fs, service, and Task09 tests remain green.

- [ ] **Step 7: Commit CLI and documentation integration**

```text
git add crates/rustnt-cli/src/main.rs README.md docs/roadmap.md .superpowers/sdd/task-13-report.md
git commit -m "feat: add window and session viewer CLI"
```

## Review and Handoff

After each task:

1. Record the task base commit before dispatching its implementer.
2. Generate a review package with `scripts/review-package BASE HEAD`.
3. Dispatch an independent task reviewer with the task brief, implementer
   report, and review package.
4. Fix all Critical and Important findings and re-review.
5. Append a clean task entry to `.superpowers/sdd/progress.md`.

After Task 3:

1. Generate a whole-branch review package from `git merge-base main HEAD`.
2. Dispatch the final reviewer against the approved specification and this plan.
3. Run fresh verification after any fixes.
4. Use the finishing-a-development-branch workflow before presenting merge,
   push, keep, or discard options.

## Plan Self-Review

- Specification coverage: window model, current Session/current desktop filter,
  all top-level windows, foreground lookup, Session state mapping, WTS metadata,
  strict CLI, output fields, errors, resource cleanup, tests, docs, and smoke
  commands are covered by Tasks 1-3.
- Placeholder scan: no unresolved TODO, TBD, or vague implementation step is
  used; every implementation step names exact APIs, files, functions, commands,
  and expected behavior.
- Type consistency: Task 1 defines the public models used by Task 2 and Task 3;
  Task 2 adds only the two APIs consumed by Task 3; renderer signatures use the
  same model field names as the approved specification.
- Scope check: no GUI, service, protocol, elevation, injection, or window
  mutation work is included.
