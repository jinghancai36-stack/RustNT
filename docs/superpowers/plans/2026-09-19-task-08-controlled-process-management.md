# Task 08 Controlled Process Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add bounded process inspection and explicitly authorized, caller-owned process termination to the existing RustNT LocalSystem service bridge.

**Architecture:** Keep the service as the only privileged boundary. Add a focused `rustnt_core::process_control` module for typed request/response data, process metadata, pure termination policy, and Windows process/token operations; keep protocol framing, Pipe dispatch, impersonation, and client transport in `service.rs`; keep CLI parsing, the two-step terminate workflow, rendering, and exit-code behavior in `rustnt-cli`.

**Tech Stack:** Rust 2021, `windows-sys 0.59`, existing synchronous Named Pipe protocol with overlapped server I/O, Windows Service Control Manager, Win32 process/token APIs, Cargo workspace tests, Clippy, rustfmt.

## Global Constraints

- Preserve protocol version `1` and existing command codes `0 PING`, `1 IDENTITY`, and `2 CAPABILITIES`.
- Add only command code `3 PROCESS_INSPECT` and command code `4 PROCESS_TERMINATE`.
- Keep the capability list exactly `ping,identity,capabilities,process_inspect,process_terminate`.
- Keep the existing local Pipe name, explicit DACL, `PIPE_REJECT_REMOTE_CLIENTS`, one-request-per-connection behavior, `ACK1` response confirmation, and five-second acknowledgement timeout.
- `PROCESS_INSPECT` accepts exactly four payload bytes containing one little-endian nonzero `u32` PID.
- `PROCESS_TERMINATE` accepts exactly twelve payload bytes containing one little-endian `u32` PID followed by one little-endian `u64` expected creation timestamp.
- Inspection output is bounded key-value UTF-8 containing only `PID`, `CREATION_TIME_100NS`, `IMAGE_NAME`, `IMAGE_PATH`, `THREADS`, `MEMORY_BYTES`, and `OWNER_SID`.
- Reject or safely replace newline, carriage-return, equals-sign, NUL, and control characters before putting process metadata into a key-value payload.
- Reject PID `0`, PID `4`, and the RustNT service process for both inspection and termination.
- Termination requires a local, elevated client that is a member of the local Administrators group.
- Termination requires target creation-time equality and target owner SID equality with the authenticated caller SID.
- Reject `SYSTEM` (`S-1-5-18`), `LocalService` (`S-1-5-19`), `NetworkService` (`S-1-5-20`), foreign-user targets, and the service's own process.
- Call `TerminateProcess` only with a fixed nonzero implementation-owned exit code; never accept a caller-provided exit code, access mask, timeout, executable path, name, wildcard, command line, or script.
- Wait for termination for at most two seconds and report `TERMINATED` or `TERMINATE_PENDING`.
- Malformed payloads must be rejected before a target process handle is opened.
- Failed authorization must not reveal target metadata.
- Process, token, Pipe, SCM, and event handles must be owned by RAII wrappers and must not outlive the request.
- New unsafe Win32 calls must have nearby safety comments.
- Failed requests must return connection-local error responses and must not terminate the service host.
- Usage errors return CLI exit code `2`; service, authorization, target, and Win32 failures return CLI exit code `1`.
- Do not add PowerShell, WMI, shell execution, arbitrary administrator commands, batch termination, remote control, kernel code, drivers, GUI code, undocumented NT APIs, suspension, resumption, priority changes, DLL injection, debugging, or token manipulation.
- Do not change the existing Task 02 process-list behavior except where the new module can safely reuse a helper without changing its public output.

## File Map

- Create `crates/rustnt-core/src/process_control.rs` for typed process-control contracts, payload codecs that do not depend on Pipe framing, sanitization, pure authorization policy, and Windows process/token/termination helpers.
- Modify `crates/rustnt-core/src/lib.rs` to expose `pub mod process_control;`.
- Modify `crates/rustnt-core/src/service.rs` to add command mappings, capabilities, framed request dispatch, client-token impersonation, service-side process-control dispatch, and payload-size/error integration.
- Modify `crates/rustnt-cli/src/main.rs` to parse `process inspect` and `process terminate`, call the service, decode typed responses, render bounded metadata, and preserve existing list/service commands.
- Modify `README.md` with the two commands and destructive-operation warning.
- Modify `docs/architecture.md` with the service process-control path, authorization sequence, and PID-reuse defense.
- Modify `docs/learning-notes.md` with process handle rights, creation time, token ownership, impersonation/revert, and Pipe ACL limitations.
- Modify `docs/benchmarks.md` with the Task 08 measurement procedure and result table.
- Create `.superpowers/sdd/task-8-report.md` during the final verification task with exact unit, build, manual, and cleanup results.
- Append Task08 completion information to `.superpowers/sdd/progress.md` only after implementation and review are complete.
- Do not modify `crates/rustnt-service/src/main.rs`; it remains a thin `run_service()` entry point.

---

### Task 1: Add Typed Process-Control Protocol Contracts

**Files:**
- Create: `crates/rustnt-core/src/process_control.rs`
- Modify: `crates/rustnt-core/src/lib.rs`
- Modify: `crates/rustnt-core/src/service.rs`
- Test: `crates/rustnt-core/src/process_control.rs` unit tests and `crates/rustnt-core/src/service.rs` unit tests

**Interfaces:**
- Consumes: existing `service::MAX_PAYLOAD`, `service::Command`, `service::Response`, and `service::ServiceError`.
- Produces:
  - `rustnt_core::process_control::ProcessInspectRequest`
  - `rustnt_core::process_control::ProcessTerminateRequest`
  - `rustnt_core::process_control::ProcessInspection`
  - `rustnt_core::process_control::ProcessStatus`
  - `rustnt_core::process_control::ProcessControlError`
  - `rustnt_core::process_control::encode_inspect_request(pid: u32) -> Result<Vec<u8>, ProcessControlError>`
  - `rustnt_core::process_control::decode_inspect_request(payload: &[u8]) -> Result<ProcessInspectRequest, ProcessControlError>`
  - `rustnt_core::process_control::encode_terminate_request(pid: u32, expected_creation_time_100ns: u64) -> Result<Vec<u8>, ProcessControlError>`
  - `rustnt_core::process_control::decode_terminate_request(payload: &[u8]) -> Result<ProcessTerminateRequest, ProcessControlError>`
  - `rustnt_core::process_control::encode_inspection_payload(inspection: &ProcessInspection, max_payload: usize) -> Result<Vec<u8>, ProcessControlError>`
  - `rustnt_core::process_control::decode_inspection_payload(payload: &[u8]) -> Result<ProcessInspection, ProcessControlError>`
  - `rustnt_core::process_control::encode_status_payload(status: ProcessStatus) -> Vec<u8>`
  - `rustnt_core::process_control::decode_status_payload(payload: &[u8]) -> Result<ProcessStatus, ProcessControlError>`
  - `rustnt_core::process_control::sanitize_field(value: &str) -> Result<String, ProcessControlError>`
  - `service::Command::ProcessInspect`
  - `service::Command::ProcessTerminate`
  - `service::command_code` mappings `3` and `4`

- [ ] **Step 1: Write failing codec tests**

Add focused tests in `process_control.rs` before adding the implementation:

```rust
#[test]
fn inspect_request_uses_exact_little_endian_four_byte_payload() {
    assert_eq!(
        encode_inspect_request(0x1122_3344).expect("request should encode"),
        vec![0x44, 0x33, 0x22, 0x11]
    );
}

#[test]
fn inspect_request_rejects_zero_truncated_and_trailing_payloads() {
    assert!(decode_inspect_request(&0u32.to_le_bytes()).is_err());
    assert!(decode_inspect_request(&[1, 0, 0]).is_err());
    assert!(decode_inspect_request(&[1, 0, 0, 0, 0]).is_err());
}

#[test]
fn terminate_request_uses_exact_little_endian_twelve_byte_payload() {
    assert_eq!(
        encode_terminate_request(0x1122_3344, 0x0102_0304_0506_0708)
            .expect("request should encode"),
        vec![
            0x44, 0x33, 0x22, 0x11, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01
        ]
    );
}

#[test]
fn terminate_request_rejects_zero_truncated_and_trailing_payloads() {
    assert!(decode_terminate_request(&[0; 12]).is_err());
    assert!(decode_terminate_request(&[1; 11]).is_err());
    assert!(decode_terminate_request(&[1; 13]).is_err());
}

#[test]
fn inspection_payload_rejects_field_breaking_values_and_oversize_output() {
    let inspection = sample_inspection("bad\nname");
    assert!(encode_inspection_payload(&inspection, 4096).is_err());

    let oversized = sample_inspection(&"x".repeat(4096));
    assert!(encode_inspection_payload(&oversized, 4096).is_err());
}

#[test]
fn status_payload_round_trips_fixed_status_names() {
    for status in [
        ProcessStatus::Terminated,
        ProcessStatus::TerminatePending,
        ProcessStatus::TargetNotFound,
        ProcessStatus::PidReused,
        ProcessStatus::TargetNotOwned,
        ProcessStatus::TargetProtected,
        ProcessStatus::CallerNotElevated,
        ProcessStatus::CallerNotAdmin,
        ProcessStatus::AccessDenied,
        ProcessStatus::TargetAccessDenied,
    ] {
        assert_eq!(
            decode_status_payload(&encode_status_payload(status)).expect("status should decode"),
            status
        );
    }
}
```

Add command mapping tests in `service.rs`:

```rust
#[test]
fn process_commands_have_stable_protocol_codes() {
    assert_eq!(command_code(Command::ProcessInspect), 3);
    assert_eq!(command_code(Command::ProcessTerminate), 4);
    assert_eq!(
        command_from_code(3).expect("inspect command should decode"),
        Command::ProcessInspect
    );
    assert_eq!(
        command_from_code(4).expect("terminate command should decode"),
        Command::ProcessTerminate
    );
}
```

The test module should include a `sample_inspection(image_name: &str) -> ProcessInspection` helper with:

```rust
ProcessInspection {
    pid: 42,
    creation_time_100ns: 1234,
    image_name: image_name.to_string(),
    image_path: Some(r"C:\Apps\demo.exe".to_string()),
    thread_count: 3,
    memory_bytes: Some(4096),
    owner_sid: "S-1-5-21-100-200-300-1001".to_string(),
}
```

Define the error type used by every codec and Windows helper:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessControlError {
    InvalidPayload(String),
    InvalidField(String),
    OversizedPayload,
    Status(ProcessStatus),
    Windows { operation: &'static str, code: u32 },
}
```

Implement `Display` and `std::error::Error` for this type. `InvalidPayload` and
`InvalidField` messages must identify only the malformed field or shape; they
must never contain command lines, environment text, token contents, or target
metadata.

- [ ] **Step 2: Run the focused tests and verify the expected RED state**

Run:

```powershell
cargo test -p rustnt-core process_control::tests -- --nocapture
cargo test -p rustnt-core service::tests::process_commands_have_stable_protocol_codes -- --nocapture
```

Expected: compilation fails because the new module, types, enum variants, and codec functions do not yet exist. If the command fails for an unrelated existing test or platform configuration issue, correct the test setup and rerun until the failure is specifically about the missing Task08 contracts.

- [ ] **Step 3: Implement the typed request, response, status, and sanitization contracts**

In `process_control.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInspectRequest {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTerminateRequest {
    pub pid: u32,
    pub expected_creation_time_100ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInspection {
    pub pid: u32,
    pub creation_time_100ns: u64,
    pub image_name: String,
    pub image_path: Option<String>,
    pub thread_count: u32,
    pub memory_bytes: Option<u64>,
    pub owner_sid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Terminated,
    TerminatePending,
    TargetNotFound,
    TargetAccessDenied,
    PidReused,
    TargetNotOwned,
    TargetProtected,
    CallerNotElevated,
    CallerNotAdmin,
    AccessDenied,
}
```

Implement exact payload lengths, little-endian decoding, nonzero PID validation, duplicate/missing field rejection, and `MAX_PAYLOAD` enforcement through the `max_payload` argument. Encode inspection fields in exactly this order:

```text
PID=<decimal>
CREATION_TIME_100NS=<decimal>
IMAGE_NAME=<sanitized UTF-8>
IMAGE_PATH=<sanitized UTF-8 or N/A>
THREADS=<decimal>
MEMORY_BYTES=<decimal or N/A>
OWNER_SID=<sanitized string>
```

Encode status failures as exactly `STATUS=<fixed-status-name>\n`. Use the fixed names from the design spec. `sanitize_field` must reject `\0`, `\r`, `\n`, `=`, and `char::is_control()` characters, and must reject an empty field for image name and owner SID when those fields are decoded.

Add `pub mod process_control;` to `crates/rustnt-core/src/lib.rs`.

Extend `Command` in `service.rs`, preserve the existing variants and derive set, and update `command_code` and `command_from_code`. Do not yet make the dispatcher call Win32 process operations; this task only makes the contracts compile.

- [ ] **Step 4: Add the fixed capability names and preserve existing frame behavior**

Update `collect_identity()` and the `CAPABILITIES` response to use:

```text
ping,identity,capabilities,process_inspect,process_terminate
```

Keep `IDENTITY` and `CAPABILITIES` empty-payload-only. Do not loosen generic frame validation. Add a test asserting the capability payload contains both new names and does not remove the original three.

- [ ] **Step 5: Run the focused tests and the existing core suite**

Run:

```powershell
cargo test -p rustnt-core process_control::tests -- --nocapture
cargo test -p rustnt-core service::tests -- --nocapture
```

Expected: all new codec/status/capability tests pass and all existing service protocol tests remain green.

- [ ] **Step 6: Refactor only after green and commit the contract slice**

Run:

```powershell
cargo fmt --all -- --check
git diff --check
git add crates/rustnt-core/src/lib.rs crates/rustnt-core/src/process_control.rs crates/rustnt-core/src/service.rs
git commit -m "feat: add process control protocol contracts"
```

Expected commit message: `feat: add process control protocol contracts`.

---

### Task 2: Implement Process Inspection and Pure Termination Policy

**Files:**
- Modify: `crates/rustnt-core/src/process_control.rs`
- Modify: `crates/rustnt-core/src/lib.rs` only if public re-exports are needed for tests or CLI
- Test: `crates/rustnt-core/src/process_control.rs` unit tests

**Interfaces:**
- Consumes: Task1 `ProcessInspection`, `ProcessStatus`, request types, sanitization helpers, and `PROCESS_QUERY_LIMITED_INFORMATION`-compatible Windows features already present in the workspace.
- Produces:
  - `pub struct CallerSecurity { pub user_sid: String, pub elevated: bool, pub administrator: bool }`
  - `pub struct TerminationPolicyInput<'a> { pub target_pid: u32, pub service_pid: u32, pub expected_creation_time_100ns: u64, pub target_creation_time_100ns: u64, pub caller_sid: &'a str, pub target_owner_sid: &'a str, pub caller_elevated: bool, pub caller_administrator: bool }`
  - `pub enum TerminationRejection { CallerNotElevated, CallerNotAdmin, TargetProtected, TargetNotFound, PidReused, TargetNotOwned, AccessDenied }`
  - `pub fn evaluate_termination_policy(input: &TerminationPolicyInput<'_>) -> Result<(), TerminationRejection>`
  - `pub fn is_system_sid(sid: &str) -> bool`
  - On Windows, `pub(crate) fn inspect_process(pid: u32, service_pid: u32) -> Result<ProcessInspection, ProcessControlError>`
  - On Windows, `pub(crate) fn terminate_process(request: &ProcessTerminateRequest, caller: &CallerSecurity, service_pid: u32) -> Result<ProcessStatus, ProcessControlError>`

- [ ] **Step 1: Write failing pure-policy tests**

Add one test per rejection class and one eligible case:

```rust
#[test]
fn termination_policy_rejects_non_elevated_callers_before_target_metadata() {
    let input = policy_input(|input| input.caller_elevated = false);
    assert_eq!(
        evaluate_termination_policy(&input),
        Err(TerminationRejection::CallerNotElevated)
    );
}

#[test]
fn termination_policy_rejects_non_admin_callers() {
    let input = policy_input(|input| input.caller_administrator = false);
    assert_eq!(
        evaluate_termination_policy(&input),
        Err(TerminationRejection::CallerNotAdmin)
    );
}

#[test]
fn termination_policy_rejects_pid_zero_pid_four_and_service_pid() {
    for pid in [0, 4, 9001] {
        let input = policy_input(|input| {
            input.target_pid = pid;
            input.service_pid = 9001;
        });
        assert_eq!(
            evaluate_termination_policy(&input),
            Err(TerminationRejection::TargetProtected)
        );
    }
}

#[test]
fn termination_policy_rejects_creation_time_mismatch() {
    let input = policy_input(|input| input.target_creation_time_100ns += 1);
    assert_eq!(
        evaluate_termination_policy(&input),
        Err(TerminationRejection::PidReused)
    );
}

#[test]
fn termination_policy_rejects_foreign_and_system_owned_targets() {
    let foreign = policy_input(|input| input.target_owner_sid = "S-1-5-21-foreign");
    assert_eq!(
        evaluate_termination_policy(&foreign),
        Err(TerminationRejection::TargetNotOwned)
    );

    let system = policy_input(|input| input.target_owner_sid = "S-1-5-18");
    assert_eq!(
        evaluate_termination_policy(&system),
        Err(TerminationRejection::TargetProtected)
    );
}

#[test]
fn termination_policy_allows_elevated_admin_for_caller_owned_target() {
    let input = policy_input(|_| {});
    assert_eq!(evaluate_termination_policy(&input), Ok(()));
}
```

The helper must construct an owned user process with PID `9002`, service PID `9001`, matching creation time `1234`, caller SID and target SID `S-1-5-21-user`, and both caller flags set to `true`.

Use this concrete helper so every policy test starts from the same eligible
case:

```rust
fn policy_input(
    mutate: impl FnOnce(&mut TerminationPolicyInput<'static>),
) -> TerminationPolicyInput<'static> {
    let mut input = TerminationPolicyInput {
        target_pid: 9002,
        service_pid: 9001,
        expected_creation_time_100ns: 1234,
        target_creation_time_100ns: 1234,
        caller_sid: "S-1-5-21-user",
        target_owner_sid: "S-1-5-21-user",
        caller_elevated: true,
        caller_administrator: true,
    };
    mutate(&mut input);
    input
}
```

- [ ] **Step 2: Run the policy tests and verify RED**

Run:

```powershell
cargo test -p rustnt-core process_control::tests::termination_policy -- --nocapture
```

Expected: compilation fails because `CallerSecurity`, `TerminationPolicyInput`, `TerminationRejection`, and `evaluate_termination_policy` are not implemented.

- [ ] **Step 3: Implement the pure policy in the documented check order**

Implement `evaluate_termination_policy` in this order:

1. `caller_elevated == false` -> `CallerNotElevated`.
2. `caller_administrator == false` -> `CallerNotAdmin`.
3. `target_pid == 0`, `target_pid == 4`, or `target_pid == service_pid` -> `TargetProtected`.
4. `target_creation_time_100ns != expected_creation_time_100ns` -> `PidReused`.
5. `is_system_sid(target_owner_sid)` -> `TargetProtected`.
6. `target_owner_sid != caller_sid` -> `TargetNotOwned`.
7. Otherwise return `Ok(())`.

Define `is_system_sid` with exact matches for `S-1-5-18`, `S-1-5-19`, and `S-1-5-20`. Do not infer ownership from a process name.

- [ ] **Step 4: Run policy tests and refactor only after green**

Run:

```powershell
cargo test -p rustnt-core process_control::tests::termination_policy -- --nocapture
```

Expected: all policy tests pass.

- [ ] **Step 5: Write failing Windows process-inspection tests**

Add Windows-gated tests that exercise only the current test process and validate stable invariants:

```rust
#[cfg(windows)]
#[test]
fn inspect_process_returns_creation_owner_and_image_metadata() {
    let inspection = inspect_process(std::process::id(), u32::MAX)
        .expect("current process should be inspectable");
    assert_eq!(inspection.pid, std::process::id());
    assert!(inspection.creation_time_100ns > 0);
    assert!(!inspection.image_name.is_empty());
    assert!(!inspection.owner_sid.is_empty());
    assert!(inspection.thread_count > 0);
}

#[cfg(windows)]
#[test]
fn inspect_process_rejects_reserved_pid_values() {
    assert_eq!(
        inspect_process(0, u32::MAX).expect_err("PID 0 must be rejected"),
        ProcessControlError::Status(ProcessStatus::TargetProtected)
    );
    assert_eq!(
        inspect_process(4, u32::MAX).expect_err("PID 4 must be rejected"),
        ProcessControlError::Status(ProcessStatus::TargetProtected)
    );
}
```

The test must use the actual current process ID, not a hard-coded user process. The reserved-PID test must not open a target handle.

- [ ] **Step 6: Run the Windows tests and verify RED for missing Win32 helpers**

Run on Windows:

```powershell
cargo test -p rustnt-core process_control::tests::inspect_process -- --nocapture
```

Expected: compilation fails only because `inspect_process` and its Win32 support types are not implemented.

- [ ] **Step 7: Implement RAII-backed process inspection**

Add Windows-only private helpers and wrappers in `process_control.rs`:

- `OwnedHandle(HANDLE)` with `new`, `get`, and `Drop` using `CloseHandle`.
- `filetime_to_u64(FILETIME) -> u64`.
- `wide_string(&[u16]) -> String` using `OsStringExt` and lossy UTF-8 conversion.
- `query_process_owner_sid(process: HANDLE) -> Result<String, ProcessControlError>` using `OpenProcessToken`, `GetTokenInformation(TokenUser, ...)`, and `ConvertSidToStringSidW`; free converted SID buffers with `LocalFree`.
- `query_process_creation_time(process: HANDLE) -> Result<u64, ProcessControlError>` using `GetProcessTimes`.
- `query_process_path(process: HANDLE) -> Option<String>` using `QueryFullProcessImageNameW`.
- `query_process_memory(process: HANDLE) -> Option<u64>` using `GetProcessMemoryInfo`.
- `find_process_entry(pid: u32) -> Result<(String, u32), ProcessControlError>` using `CreateToolhelp32Snapshot`, `Process32FirstW`, and `Process32NextW`.

`inspect_process(pid, service_pid)` must:

1. Reject PID `0`, PID `4`, and `service_pid` with `ProcessStatus::TargetProtected`.
2. Open the target with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`.
3. Map a missing process to `TargetNotFound` and a denied open to `TargetAccessDenied`.
4. Query creation time and owner SID as required fields.
5. Query image name and thread count from Toolhelp; map a vanished snapshot entry to `TargetNotFound`.
6. Query path and memory best-effort; use `None` when those optional calls fail.
7. Sanitize required and optional strings before returning `ProcessInspection`.
8. Drop the process handle before returning.

Every unsafe block must state which handle, pointer, buffer, or structure invariant makes the call valid.

- [ ] **Step 8: Implement bounded termination**

Implement `terminate_process` with this sequence:

1. Open the target with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE`.
2. Map a missing target to `TargetNotFound` and other open failures to `AccessDenied`.
3. Query creation time and owner SID.
4. Build `TerminationPolicyInput` from the request, caller, service PID, creation time, and owner SID.
5. Return the mapped rejection status without calling `TerminateProcess` when policy rejects.
6. Call `TerminateProcess(handle, 1)` with the fixed exit code `1`.
7. Call `WaitForSingleObject(handle, 2000)`.
8. Return `ProcessStatus::Terminated` for `WAIT_OBJECT_0`, `TerminatePending` for `WAIT_TIMEOUT`, and `AccessDenied` or a typed Win32 error for `WAIT_FAILED`.
9. Drop the handle before returning.

Do not expose a raw `HANDLE` in any public type. Do not store a target handle in a struct that survives the function call.

- [ ] **Step 9: Run all core tests and formatting**

Run:

```powershell
cargo test -p rustnt-core -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected: policy, codec, inspection, and all pre-existing core tests pass with no formatting or whitespace errors.

- [ ] **Step 10: Commit the process-control module**

```powershell
git add crates/rustnt-core/src/process_control.rs crates/rustnt-core/src/lib.rs
git commit -m "feat: add guarded process inspection and termination policy"
```

Expected commit message: `feat: add guarded process inspection and termination policy`.

---

### Task 3: Integrate Authorization and Process Operations into the Service Bridge

**Files:**
- Modify: `crates/rustnt-core/src/service.rs`
- Modify: `crates/rustnt-cli/src/main.rs` for the changed `ServiceClient` call signature
- Test: `crates/rustnt-core/src/service.rs` unit tests

**Interfaces:**
- Consumes: Task1 protocol commands/codecs and Task2 `CallerSecurity`, process inspection, and termination functions.
- Produces:
  - `ServiceClient::request(&self, command: Command, payload: &[u8]) -> Result<Response, ServiceError>`
  - `fn dispatch_request(pipe: HANDLE, request: Request) -> Response` on Windows
  - `fn query_pipe_client_security(pipe: HANDLE) -> Result<CallerSecurity, ServiceError>` on Windows
  - `fn process_error_response(error: ProcessControlError) -> Response`
  - service-side `PROCESS_INSPECT` and `PROCESS_TERMINATE` dispatch with connection-local failures

- [ ] **Step 1: Write failing dispatch and client-signature tests**

Add tests in `service.rs`:

```rust
#[test]
fn process_inspect_dispatch_rejects_wrong_payload_shape_without_opening_a_process() {
    let response = dispatch_request_for_test(Request {
        command: Command::ProcessInspect,
        payload: vec![1, 2, 3],
    });
    assert_eq!(response.status, STATUS_ERROR);
}

#[test]
fn process_terminate_dispatch_rejects_wrong_payload_shape_without_authorization() {
    let response = dispatch_request_for_test(Request {
        command: Command::ProcessTerminate,
        payload: vec![1; 11],
    });
    assert_eq!(response.status, STATUS_ERROR);
}

#[test]
fn capability_response_contains_only_the_fixed_five_command_names() {
    let payload = String::from_utf8(dispatch_command(Command::Capabilities).payload)
        .expect("capability payload should be UTF-8");
    assert_eq!(
        payload,
        "ping,identity,capabilities,process_inspect,process_terminate\n"
    );
}
```

Keep a test-only wrapper with `#[cfg(test)]`:

```rust
fn dispatch_request_for_test(request: Request) -> Response {
    dispatch_request(std::ptr::null_mut(), request)
}
```

The implementation must validate process payload shape before consulting the Pipe or opening a process; the null test handle must therefore be safe for malformed requests.

- [ ] **Step 2: Run the service tests and verify RED**

Run:

```powershell
cargo test -p rustnt-core service::tests::process_inspect_dispatch -- --nocapture
cargo test -p rustnt-core service::tests::process_terminate_dispatch -- --nocapture
cargo test -p rustnt-core service::tests::capability_response_contains_only_the_fixed_five_command_names -- --nocapture
```

Expected: compilation fails because `dispatch_request` still has the old signature, the new command variants are not dispatched, and the client request API has not yet accepted a payload.

- [ ] **Step 3: Change the request API without changing framing**

Change both Windows and non-Windows `ServiceClient` implementations to:

```rust
pub fn request(&self, command: Command, payload: &[u8]) -> Result<Response, ServiceError>
```

Encode the supplied payload in the existing `Request` frame. Keep `MAX_PAYLOAD` validation in `encode_request`. Update the existing identity call and every test call to pass `&[]`. Do not add a second transport protocol or bypass `ACK1`.

- [ ] **Step 4: Add service-side process dispatch**

Change the internal dispatcher to:

```rust
fn dispatch_request(pipe: HANDLE, request: Request) -> Response
```

Dispatch rules:

- `Ping`, `Identity`, and `Capabilities` accept only an empty payload and keep existing behavior.
- `ProcessInspect` decodes exactly four bytes, obtains the service process ID with `GetCurrentProcessId`, calls `inspect_process`, and returns status `0` with the inspection key-value payload on success.
- `ProcessTerminate` decodes exactly twelve bytes, validates the payload before impersonation, calls `query_pipe_client_security(pipe)`, obtains the service process ID, calls `terminate_process`, and returns status `0` with `STATUS=TERMINATED\n` or `STATUS=TERMINATE_PENDING\n` for accepted outcomes.
- `ProcessTerminate` maps policy and target rejections to status `1` and a fixed `STATUS=<name>\n` payload. It must not include target metadata in these responses.
- Win32 details not represented by a fixed public status are truncated through the existing `request_error_response` path and remain connection-local.

Do not return raw `ServiceError` strings for authorization failures when a fixed status exists.

- [ ] **Step 5: Implement named-Pipe client token authentication**

Add a Windows-only `query_pipe_client_security(pipe: HANDLE)` function with this exact behavior:

1. Call `ImpersonateNamedPipeClient(pipe)`.
2. Immediately create an `ImpersonationGuard` whose `Drop` calls `RevertToSelf`.
3. Call `OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token)`.
4. Query `TokenUser` and convert the caller SID to a string.
5. Query `TokenElevation` and set `elevated = TokenIsElevated != 0`.
6. Query `TokenGroups` and set `administrator = true` only when the token contains the exact local Administrators SID `S-1-5-32-544`.
7. Return `CallerSecurity { user_sid, elevated, administrator }`.
8. Let the guard revert impersonation before `query_pipe_client_security` returns.

Use a local RAII token wrapper and existing checked-pointer/token-buffer patterns. If impersonation, token open, token user, token elevation, or group lookup fails, return `ServiceError::windows` or a protocol error without exposing target data. Keep the existing `PIPE_REJECT_REMOTE_CLIENTS` flag; the transport itself rejects remote clients before this operation-level authorization.

- [ ] **Step 6: Update the server loop to pass the connected Pipe handle**

In `run_pipe_server`, call:

```rust
Ok(request_size) => match decode_request(&request_buffer[..request_size]) {
    Ok(request) => dispatch_request(pipe.get(), request),
    Err(error) => request_error_response(error),
},
```

Preserve the existing handling for stopped, disconnected, timed-out, oversized, and failed Pipe I/O. A malformed or unauthorized process request must produce a response and then follow the existing ACK/disconnect lifecycle; it must not return `Err` merely because the target operation was rejected.

- [ ] **Step 7: Run core tests, then build all crates**

Run:

```powershell
cargo test -p rustnt-core -- --nocapture
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: all tests, workspace build, Clippy, formatting, and diff checks pass. Existing service identity and lifecycle behavior must remain unchanged.

- [ ] **Step 8: Commit the service integration**

```powershell
git add crates/rustnt-core/src/service.rs crates/rustnt-cli/src/main.rs
git commit -m "feat: bridge controlled process operations through service"
```

Expected commit message: `feat: bridge controlled process operations through service`.

---

### Task 4: Add CLI Inspect and Terminate Workflows

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs`
- Test: `crates/rustnt-cli/src/main.rs` unit tests

**Interfaces:**
- Consumes: `Command::ProcessInspect`, `Command::ProcessTerminate`, `ServiceClient::request(command, payload)`, Task1 codecs, Task2 `ProcessInspection`, and existing service status APIs.
- Produces:
  - `enum ProcessCommand { List(Options), Inspect { pid: u32 }, Terminate { pid: u32 } }`
  - `fn parse_process_command(args: &[String]) -> Result<ProcessCommand, String>`
  - `fn parse_pid_option(args: &[String]) -> Result<u32, String>`
  - `fn run_process_command(command: ProcessCommand) -> Result<(), String>`
  - `fn render_process_inspection(inspection: &ProcessInspection) -> String`
  - `fn render_process_status(status: ProcessStatus) -> String`

- [ ] **Step 1: Write failing CLI parser tests**

Add tests:

```rust
#[test]
fn parses_inspect_and_terminate_with_positive_decimal_pid() {
    assert_eq!(
        parse_process_command(&strings(&["inspect", "--pid", "42"]))
            .expect("inspect should parse"),
        ProcessCommand::Inspect { pid: 42 }
    );
    assert_eq!(
        parse_process_command(&strings(&["terminate", "--pid", "42"]))
            .expect("terminate should parse"),
        ProcessCommand::Terminate { pid: 42 }
    );
}

#[test]
fn rejects_invalid_process_command_pid_and_arguments() {
    for args in [
        vec!["inspect"],
        vec!["inspect", "--pid"],
        vec!["inspect", "--pid", "0"],
        vec!["inspect", "--pid", "-1"],
        vec!["inspect", "--pid", "not-a-pid"],
        vec!["inspect", "--pid", "42", "--extra"],
        vec!["terminate", "--name", "demo.exe"],
        vec!["terminate", "--pid", "42", "--force"],
        vec!["unknown", "--pid", "42"],
    ] {
        assert!(parse_process_command(&strings(&args)).is_err(), "{args:?}");
    }
}
```

Add rendering tests using a fixed `ProcessInspection`:

```rust
#[test]
fn renders_bounded_inspection_fields_without_unlisted_metadata() {
    let output = render_process_inspection(&sample_inspection());
    assert!(output.contains("PID           42"));
    assert!(output.contains("CREATION      1234"));
    assert!(output.contains("OWNER SID     S-1-5-21-user"));
    assert!(!output.contains("COMMAND_LINE"));
    assert!(!output.contains("TOKEN"));
}
```

- [ ] **Step 2: Run the CLI tests and verify RED**

Run:

```powershell
cargo test -p rustnt-cli parses_inspect_and_terminate_with_positive_decimal_pid -- --nocapture
cargo test -p rustnt-cli rejects_invalid_process_command_pid_and_arguments -- --nocapture
```

Expected: compilation fails because the new process command enum/parser/renderer do not exist.

- [ ] **Step 3: Implement process command routing while preserving list behavior**

Change `main` so that:

- `service ...` continues to route to `run_service_cli`.
- `process list ...` continues to use the existing `Options`, watch loop, filter, sort, and output.
- `process inspect --pid <PID>` and `process terminate --pid <PID>` route through `parse_process_command`.
- Any other top-level command prints the full process usage and exits with `2`.

Use exact usage strings:

```text
usage: rustnt process list [--watch <seconds>] [--filter <text>] [--sort <pid|name|cpu|memory>]
       rustnt process inspect --pid <PID>
       rustnt process terminate --pid <PID>
```

`parse_pid_option` must require exactly `--pid <value>`, parse decimal `u32`, reject `0`, reject values beginning with `-`, reject unknown options, reject duplicate `--pid`, and reject trailing arguments. Do not accept names, paths, wildcards, `--force`, or an exit-code option.

- [ ] **Step 4: Implement inspect workflow and bounded rendering**

Implement `run_process_command(ProcessCommand::Inspect { pid })`:

1. Query `query_service_status()`.
2. Return `service is not running; run rustnt service start` when the service is `NotInstalled` or `Stopped`.
3. Encode the four-byte inspect payload.
4. Call `ServiceClient.request(Command::ProcessInspect, &payload)`.
5. If the response status is nonzero, decode the fixed status payload and return a concise error string containing the status name.
6. Decode the inspection payload and print `render_process_inspection`.

Render exactly the allowed fields in a readable block:

```text
RustNT Process Inspection

PID           <decimal>
CREATION      <decimal>
IMAGE         <sanitized name>
PATH          <sanitized path or N/A>
THREADS       <decimal>
MEMORY        <decimal or N/A>
OWNER SID     <string>
```

Use `N/A` for absent optional values. Do not render command lines, environments, handles, token contents, or arbitrary response fields.

- [ ] **Step 5: Implement the two-step terminate workflow**

Implement `run_process_command(ProcessCommand::Terminate { pid })`:

1. Check service running as in inspect.
2. Send `ProcessInspect` for the PID.
3. If inspection fails, return the fixed service status without retrying.
4. Extract `creation_time_100ns`.
5. Encode `ProcessTerminate` with the original PID and extracted timestamp.
6. Send exactly one `ProcessTerminate` request.
7. Decode and print the fixed result status.
8. For `TERMINATE_PENDING`, print `termination accepted but is still pending; inspect the process again`.
9. Never retry automatically after `PID_REUSED`, `TARGET_NOT_FOUND`, `ACCESS_DENIED`, or an authorization status.

Map all usage/parser failures to `ExitCode::from(2)` and all errors returned by `run_process_command` to `ExitCode::from(1)`.

- [ ] **Step 6: Run CLI tests and the full workspace suite**

Run:

```powershell
cargo test -p rustnt-cli -- --nocapture
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Expected: all existing list/service tests and all new process parser/render tests pass; workspace build and Clippy remain clean.

- [ ] **Step 7: Commit the CLI slice**

```powershell
git add crates/rustnt-cli/src/main.rs
git commit -m "feat: add process inspect and terminate CLI"
```

Expected commit message: `feat: add process inspect and terminate CLI`.

---

### Task 5: Document, Benchmark, Manually Validate, and Record Completion

**Files:**
- Modify: `README.md`
- Modify: `docs/architecture.md`
- Modify: `docs/learning-notes.md`
- Modify: `docs/benchmarks.md`
- Create: `.superpowers/sdd/task-8-report.md`
- Modify: `.superpowers/sdd/progress.md`

**Interfaces:**
- Consumes: the completed Task08 service protocol, CLI commands, unit-test suite, and existing service lifecycle commands.
- Produces: user-facing usage documentation, architecture/security notes, benchmark procedure, manual validation evidence, cleanup evidence, and a durable progress-ledger entry.

- [ ] **Step 1: Update README usage and destructive-operation warning**

Replace the statement that the service does not accept process-control requests with a Task08 description:

- `rustnt process inspect --pid <PID>` is read-only and returns bounded metadata.
- `rustnt process terminate --pid <PID>` is destructive and requires an elevated local administrator.
- Termination is limited to a caller-owned normal user process.
- PID `0`, PID `4`, the RustNT service, system-owned processes, foreign-user processes, and PID-reused targets are rejected.
- The service never accepts command lines, scripts, executable paths, arbitrary exit codes, or shell commands.

Keep the existing service install/start/status/identity/stop/uninstall commands and Windows user-mode/kernel boundary statement.

- [ ] **Step 2: Update architecture and learning notes**

In `docs/architecture.md`, document this exact flow:

```text
CLI PID validation
    -> service-running check
    -> PROCESS_INSPECT
    -> creation-time capture
    -> PROCESS_TERMINATE
    -> client-token impersonation
    -> elevated + local-admin check
    -> target handle query
    -> creation-time and owner-SID recheck
    -> protected-target policy
    -> TerminateProcess + 2-second wait
```

Explain that Pipe ACLs and remote-client rejection protect transport exposure but do not replace per-operation authorization.

In `docs/learning-notes.md`, add focused notes on:

- why `PROCESS_QUERY_LIMITED_INFORMATION` and `PROCESS_TERMINATE` are separate rights;
- why a PID is not a stable identity and creation time prevents PID-reuse races;
- why target owner SID must be compared with the impersonated caller SID;
- why `RevertToSelf` must happen before the LocalSystem service token performs target operations;
- why all process/token/Pipe handles need RAII;
- why no command line, environment, token contents, or arbitrary target metadata is returned.

- [ ] **Step 3: Add benchmark procedure and result table**

Add a Task08 section to `docs/benchmarks.md` with this table:

```text
measurement                              | result | environment
-----------------------------------------|--------|------------
process inspect round-trip latency       |        |
non-elevated/admin rejection latency     |        |
caller-owned terminate-to-stopped time   |        |
PID-reuse rejection round-trip latency   |        |
```

Document that each measurement records Windows version/build, Rust toolchain, build profile, service state, caller elevation, target PID class, and whether cleanup succeeded. Do not invent measurements; empty result cells remain until the manual run records actual values.

- [ ] **Step 4: Run non-privileged verification before manual checks**

Run:

```powershell
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Record the exact commands, exit codes, and test counts in `.superpowers/sdd/task-8-report.md`.

- [ ] **Step 5: Run the elevated Windows manual validation**

Use the already-built binaries and an elevated local Administrator console:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
rustnt process inspect --pid <known user-process>
rustnt process inspect --pid 4
rustnt process terminate --pid <RustNT service PID>
rustnt process terminate --pid <disposable user-owned helper PID>
rustnt process inspect --pid <disposable user-owned helper PID>
rustnt service stop
rustnt service uninstall
```

Record expected outcomes:

- known user process inspection prints PID, creation time, image, owner SID, thread count, and best-effort path/memory;
- PID `4` is rejected with `TARGET_PROTECTED` or an equivalent fixed protected-target status;
- the RustNT service PID is rejected and the service remains running;
- the disposable caller-owned helper is terminated or reports bounded `TERMINATE_PENDING`;
- a second inspection reports `TARGET_NOT_FOUND` or the documented stopped state;
- service stop and uninstall complete;
- no `RustNTControl` registration, service process, or disposable helper remains.

If UAC or Administrator interaction is unavailable, record the exact skipped commands and reason in `.superpowers/sdd/task-8-report.md`, then still verify that no service registration or helper process was left behind.

- [ ] **Step 6: Record the completion report and progress ledger entry**

Create `.superpowers/sdd/task-8-report.md` with:

```markdown
# Task 08 Verification Report

## Automated Verification

| Command | Result |
| --- | --- |
| `cargo test --workspace -- --nocapture` | |
| `cargo build --workspace` | |
| `cargo clippy --workspace --all-targets -- -D warnings` | |
| `cargo fmt --all -- --check` | |
| `git diff --check` | |

## Manual Windows Validation

Record the exact date, Windows build, caller context, commands run, observed
statuses, skipped UAC-gated steps, and cleanup result.

## Residual Risk

Record only environment-gated manual checks or known limitations. Do not claim
elevated validation passed without command evidence.
```

Append one durable line to `.superpowers/sdd/progress.md`:

```text
- Task 08 implementation, review, automated verification, and manual cleanup: complete; commit IDs are recorded in the Task08 verification report; elevated checks either passed or are explicitly documented as environment-gated.
```

Record the actual short commit IDs in the verification report before committing;
the progress ledger line itself must contain no sample commit text.

- [ ] **Step 7: Commit documentation and evidence**

Run:

```powershell
git add README.md docs/architecture.md docs/learning-notes.md docs/benchmarks.md .superpowers/sdd/task-8-report.md .superpowers/sdd/progress.md
git commit -m "docs: document controlled process management"
```

Expected commit message: `docs: document controlled process management`.

---

## Final Review and Completion Gate

- [ ] Read the Task08 design spec and this plan side by side.
- [ ] Confirm only command codes `3` and `4` were added.
- [ ] Confirm malformed payloads are rejected before target handles are opened.
- [ ] Confirm the service authenticates every terminate request through the connecting Pipe token.
- [ ] Confirm impersonation is reverted before LocalSystem target operations.
- [ ] Confirm PID reuse, caller ownership, system-owned targets, PID `0`, PID `4`, and service self-protection are all tested.
- [ ] Confirm the CLI performs inspect-then-terminate with no automatic retry.
- [ ] Confirm no arbitrary command, script, executable path, batch operation, remote path, or hidden force flag was added.
- [ ] Confirm all handles are RAII-owned and no target handle escapes its request.
- [ ] Confirm the full workspace test/build/Clippy/fmt/diff commands have fresh output.
- [ ] Confirm manual service/helper cleanup, or document the exact environment-gated limitation.
- [ ] Request a final code review against the full Task08 spec before treating the task as complete.
