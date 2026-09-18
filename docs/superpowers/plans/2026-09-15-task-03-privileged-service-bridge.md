# Task 03 Privileged Service Bridge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a LocalSystem Windows Service and a restricted Named Pipe bridge so the RustNT CLI can query service identity and capabilities without executing arbitrary commands.

**Architecture:** `rustnt-service` will be a thin Windows Service binary. `rustnt-core::service` will own all SCM, Named Pipe, security, token, protocol, and HANDLE operations. `rustnt-cli` will parse service commands and call typed core APIs. The service will use demand start, one request per local Pipe connection, and a fixed allowlist of `PING`, `IDENTITY`, and `CAPABILITIES`.

**Tech Stack:** Rust 2021, Cargo workspace, `windows-sys 0.59`, documented Win32 Service Control Manager, Named Pipe, Security, Token, File, and I/O APIs, standard-library synchronization and byte encoding.

## Global Constraints

- The service runs as `LocalSystem`.
- The service name is `RustNTControl`.
- The fixed Pipe name is `\\.\pipe\RustNT.Control.v1`.
- The service uses `SERVICE_DEMAND_START` and does not start automatically at boot.
- `rustnt-core::service` is the only layer that calls the Windows APIs used by this feature.
- The CLI does not call Win32 APIs directly.
- Only `PING`, `IDENTITY`, and `CAPABILITIES` are accepted in Task 03.
- Request payloads are limited to 4096 bytes.
- Invalid magic, protocol version, command, or payload length is rejected before dispatch.
- Named Pipe access uses an explicit DACL and rejects remote clients.
- Every owned SCM, service, Pipe, token, and LocalAlloc HANDLE is wrapped by a focused RAII type.
- Every unsafe block is small and includes a comment explaining its pointer, structure, callback, or handle assumptions.
- Do not call `sc.exe`, PowerShell, WMI, `tasklist`, or other external process/service commands.
- Do not add JSON, network listeners, GUI code, async runtimes, drivers, ETW, process control, or arbitrary command execution.
- CLI usage errors return exit code 2; runtime, SCM, service, and IPC failures return exit code 1; success returns exit code 0.
- The workspace is now a Git repository on branch `main`; commit commands remain blocked until Git author identity is configured.

For every Cargo command in this Windows environment, initialize the MSVC and SDK variables first:

```powershell
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$msvc = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207'
$sdkRoot = 'C:\Program Files (x86)\Windows Kits\10'
$sdkVersion = '10.0.26100.0'
$env:Path = "$cargoBin;$msvc\bin\Hostx64\x64;$sdkRoot\bin\$sdkVersion\x64;$env:Path"
$env:LIB = "$msvc\lib\x64;$sdkRoot\Lib\$sdkVersion\um\x64;$sdkRoot\Lib\$sdkVersion\ucrt\x64"
$env:INCLUDE = "$msvc\include;$sdkRoot\Include\$sdkVersion\ucrt;$sdkRoot\Include\$sdkVersion\shared;$sdkRoot\Include\$sdkVersion\um"
```

---

### Task 1: Wire the service module and implement the pure protocol codec

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/rustnt-core/src/lib.rs`
- Create: `crates/rustnt-core/src/service.rs`
- Test: `crates/rustnt-core/src/service.rs` unit-test module

**Interfaces:**

```rust
pub const SERVICE_NAME: &str = "RustNTControl";
pub const PIPE_NAME: &str = r"\\.\pipe\RustNT.Control.v1";
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_PAYLOAD: usize = 4096;
pub const FRAME_HEADER_SIZE: usize = 12;
pub const MAX_REQUEST_FRAME_SIZE: usize = FRAME_HEADER_SIZE + MAX_PAYLOAD;
pub const RESPONSE_HEADER_SIZE: usize = 14;
pub const MAX_RESPONSE_FRAME_SIZE: usize = RESPONSE_HEADER_SIZE + MAX_PAYLOAD;
/// Backward-compatible alias for the maximum request frame size.
pub const MAX_FRAME_SIZE: usize = MAX_REQUEST_FRAME_SIZE;
pub const STATUS_SUCCESS: u32 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    pub operation: &'static str,
    pub code: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Ping,
    Identity,
    Capabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub command: Command,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u32,
    pub payload: Vec<u8>,
}

pub fn encode_request(request: &Request) -> Result<Vec<u8>, ServiceError>;
pub fn decode_request(bytes: &[u8]) -> Result<Request, ServiceError>;
pub fn encode_response(response: &Response) -> Result<Vec<u8>, ServiceError>;
pub fn decode_response(bytes: &[u8]) -> Result<Response, ServiceError>;
```

- [ ] **Step 1: Add the service module and Win32 features**

Do not add the `rustnt-service` binary to the workspace until Task 5 creates
its complete service host. Extend the `windows-sys` feature list with:

```toml
"Win32_Security",
"Win32_Security_Authorization",
"Win32_Storage_FileSystem",
"Win32_System_IO",
"Win32_System_Pipes",
"Win32_System_Services",
```

Add `pub mod service;` to `rustnt-core/src/lib.rs`.

- [ ] **Step 2: Write failing codec tests**

Add tests for a request round trip, response round trip, and malformed input:

```rust
#[test]
fn request_round_trip_preserves_command_and_payload() {
    let request = Request {
        command: Command::Identity,
        payload: Vec::new(),
    };
    let bytes = encode_request(&request).expect("request should encode");
    assert_eq!(decode_request(&bytes).expect("request should decode"), request);
}

#[test]
fn response_round_trip_preserves_status_and_payload() {
    let response = Response {
        status: 0,
        payload: b"ACCOUNT=LocalSystem\n".to_vec(),
    };
    let bytes = encode_response(&response).expect("response should encode");
    assert_eq!(
        decode_response(&bytes).expect("response should decode"),
        response
    );
}

#[test]
fn codec_rejects_bad_magic_version_command_and_length() {
    let request = Request {
        command: Command::Ping,
        payload: Vec::new(),
    };
    let mut bytes = encode_request(&request).expect("request should encode");

    bytes[0] ^= 0xff;
    assert!(decode_request(&bytes).is_err());

    let mut bytes = encode_request(&request).expect("request should encode");
    bytes[4] = 2;
    assert!(decode_request(&bytes).is_err());

    let mut bytes = encode_request(&request).expect("request should encode");
    bytes[6] = 0xff;
    assert!(decode_request(&bytes).is_err());

    let mut bytes = encode_request(&request).expect("request should encode");
    bytes[8] = 0xff;
    assert!(decode_request(&bytes).is_err());
}

#[test]
fn codec_rejects_payloads_over_4096_bytes() {
    let request = Request {
        command: Command::Ping,
        payload: vec![0; MAX_PAYLOAD + 1],
    };
    assert!(encode_request(&request).is_err());
}
```

- [ ] **Step 3: Run the focused tests and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core service::tests
```

Expected: compilation failure because the service module, codec types, and
functions do not exist.

- [ ] **Step 4: Implement the versioned fixed frame**

Use these exact wire layouts:

```text
Request:  magic[4] = RNT1 | version[u16] | command[u16] | payload_length[u32] | payload
Response: magic[4] = RNS1 | version[u16] | status[u32]  | payload_length[u32] | payload
```

Encode integer fields with `to_le_bytes`. Decode only after checking the
minimum header length for the frame kind (`FRAME_HEADER_SIZE` for requests and
`RESPONSE_HEADER_SIZE` for responses), expected magic, version `1`, known
command for requests, and `payload_length <= MAX_PAYLOAD`. Reject trailing
bytes after the declared payload so a frame cannot hide a second command.
Implement `Display` and `std::error::Error` for `ServiceError` so both service
binaries can print operation, message, and Win32 error details.

- [ ] **Step 5: Run the codec tests and formatting**

Run:

```powershell
cargo test -p rustnt-core service::tests
cargo fmt --all -- --check
```

Expected: all codec tests pass and formatting exits with code 0.

- [ ] **Step 6: Commit the protocol foundation**

```powershell
git add Cargo.toml crates/rustnt-core/src/lib.rs crates/rustnt-core/src/service.rs
git commit -m "feat: add RustNT service protocol foundation"
```

If Git still reports missing author identity, keep the changes staged and
record the exact error instead of inventing a repository identity.

---

### Task 2: Implement the SCM client and service lifecycle API

**Files:**
- Modify: `crates/rustnt-core/src/service.rs`
- Test: `crates/rustnt-core/src/service.rs` unit-test module

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    Stopped,
    StartPending,
    Running,
    StopPending,
    Other(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatus {
    pub state: ServiceState,
    pub process_id: Option<u32>,
}

pub fn install_service(binary_path: &std::path::Path) -> Result<(), ServiceError>;
pub fn uninstall_service() -> Result<(), ServiceError>;
pub fn start_service() -> Result<(), ServiceError>;
pub fn stop_service() -> Result<(), ServiceError>;
pub fn query_service_status() -> Result<ServiceStatus, ServiceError>;
```

- [ ] **Step 1: Write pure status mapping tests**

Test that the Win32 numeric states map to the public enum:

```rust
#[test]
fn service_state_mapping_preserves_known_states() {
    assert_eq!(map_service_state(SERVICE_STOPPED), ServiceState::Stopped);
    assert_eq!(
        map_service_state(SERVICE_START_PENDING),
        ServiceState::StartPending
    );
    assert_eq!(map_service_state(SERVICE_RUNNING), ServiceState::Running);
    assert_eq!(
        map_service_state(SERVICE_STOP_PENDING),
        ServiceState::StopPending
    );
    assert_eq!(map_service_state(999), ServiceState::Other(999));
}
```

- [ ] **Step 2: Run the focused tests and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core service_state_mapping
```

Expected: compilation failure because the status type and mapping helper do
not exist.

- [ ] **Step 3: Add the SCM HANDLE wrapper**

Create `OwnedScHandle(SC_HANDLE)` with:

```rust
impl Drop for OwnedScHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: The value is an owned SCM or service handle returned by an
                // advapi32 API and is closed exactly once by this wrapper.
                CloseServiceHandle(self.0);
            }
        }
    }
}
```

Add `open_scm(access)` and `open_service(access)` helpers. Use only the
smallest required access masks:

```text
install: SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE
status:  SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS
start:   SC_MANAGER_CONNECT, SERVICE_START | SERVICE_QUERY_STATUS
stop:    SC_MANAGER_CONNECT, SERVICE_STOP | SERVICE_QUERY_STATUS
remove:  SC_MANAGER_CONNECT, DELETE | SERVICE_QUERY_STATUS
```

- [ ] **Step 4: Implement service installation**

Convert the supplied `Path` to a nul-terminated UTF-16 buffer and call
`CreateServiceW` with:

```text
service name:       RustNTControl
display name:       RustNT Control Service
service type:       SERVICE_WIN32_OWN_PROCESS
start type:         SERVICE_DEMAND_START
error control:      SERVICE_ERROR_NORMAL
account:            LocalSystem
password:           null
```

If `OpenServiceW` finds an existing service, query its configuration. Return
success only when the existing service uses the expected service name and
binary path; otherwise return an explicit configuration-conflict error.

- [ ] **Step 5: Implement start, stop, status, and uninstall**

Use `StartServiceW`, `ControlService`, `QueryServiceStatusEx`, and
`DeleteService`. Poll status every 50 milliseconds for at most 5 seconds while
waiting for `RUNNING` or `STOPPED`. Treat already-running start and
already-stopped stop as success. Map `ERROR_SERVICE_DOES_NOT_EXIST` from
`OpenServiceW` to `Ok(ServiceStatus { state: NotInstalled, process_id: None })`
for `query_service_status`; return it as an operation error for start, stop,
and uninstall. Refuse uninstall while the state is not `STOPPED`.

- [ ] **Step 6: Run lifecycle API unit tests and compile the workspace**

Run:

```powershell
cargo test -p rustnt-core service
cargo check --workspace
cargo fmt --all -- --check
```

Expected: unit tests pass, all crates compile, and formatting exits with code
0. Do not install a service in the normal unit-test process yet.

- [ ] **Step 7: Commit the SCM client**

```powershell
git add crates/rustnt-core/src/service.rs
git commit -m "feat: add RustNT service lifecycle client"
```

---

### Task 3: Implement LocalSystem token identity collection

**Files:**
- Modify: `crates/rustnt-core/src/service.rs`
- Test: `crates/rustnt-core/src/service.rs` unit-test module

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceIdentity {
    pub service: String,
    pub account: String,
    pub account_sid: String,
    pub integrity: String,
    pub elevated: bool,
    pub protocol_version: u16,
    pub capabilities: Vec<String>,
}

pub fn collect_identity() -> Result<ServiceIdentity, ServiceError>;
pub fn identity_payload(identity: &ServiceIdentity) -> Vec<u8>;
```

- [ ] **Step 1: Write deterministic identity payload tests**

```rust
#[test]
fn identity_payload_contains_stable_fields() {
    let identity = ServiceIdentity {
        service: SERVICE_NAME.to_string(),
        account: "LocalSystem".to_string(),
        account_sid: "S-1-5-18".to_string(),
        integrity: "System".to_string(),
        elevated: true,
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![
            "ping".to_string(),
            "identity".to_string(),
            "capabilities".to_string(),
        ],
    };
    let payload = String::from_utf8(identity_payload(&identity)).expect("payload should be UTF-8");
    assert!(payload.contains("SERVICE=RustNTControl\n"));
    assert!(payload.contains("ACCOUNT=LocalSystem\n"));
    assert!(payload.contains("ACCOUNT_SID=S-1-5-18\n"));
    assert!(payload.contains("INTEGRITY=System\n"));
    assert!(payload.contains("ELEVATED=true\n"));
    assert!(payload.contains("PROTOCOL=1\n"));
    assert!(payload.contains("CAPABILITIES=ping,identity,capabilities\n"));
}
```

- [ ] **Step 2: Run the focused test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core identity_payload
```

Expected: compilation failure because `ServiceIdentity`, `collect_identity`,
and `identity_payload` do not exist.

- [ ] **Step 3: Add the owned token and LocalAlloc wrappers**

Open the current process token with `OpenProcessToken(GetCurrentProcess(),
TOKEN_QUERY, ...)`. Wrap the returned token in the existing HANDLE ownership
pattern. Wrap buffers returned by `ConvertSidToStringSidW` and
`ConvertStringSecurityDescriptorToSecurityDescriptorW` with a focused
`LocalAllocBuffer` whose `Drop` calls `LocalFree`.

- [ ] **Step 4: Query the account SID**

Call `GetTokenInformation(TokenUser)` using the two-call size-query pattern:

```text
call with null buffer -> required byte count
allocate Vec<u8>
call with Vec pointer -> TOKEN_USER
```

Convert `TOKEN_USER.User.Sid` with `ConvertSidToStringSidW`. Return
`account = "LocalSystem"` only when the SID is exactly `S-1-5-18`; otherwise
return the SID string as the account label.

- [ ] **Step 5: Query integrity and elevation**

Query `TokenIntegrityLevel` and read the final SID sub-authority. Map the
standard RID values to `Untrusted`, `Low`, `Medium`, `High`, `System`, or
`Protected`; preserve an unknown value as `Unknown(<rid>)`. Query
`TokenElevation` and set `elevated` from `TOKEN_ELEVATION.TokenIsElevated != 0`.

- [ ] **Step 6: Build the fixed capability payload**

Set:

```text
service:          RustNTControl
protocol_version: 1
capabilities:     ping, identity, capabilities
```

Encode key-value lines with `\n` separators. Reject embedded newline or `=`
characters in dynamic fields so a token-derived value cannot forge an
additional field.

- [ ] **Step 7: Run identity tests and compile**

Run:

```powershell
cargo test -p rustnt-core service
cargo check --workspace
cargo fmt --all -- --check
```

Expected: deterministic tests pass and the Windows workspace compiles.

- [ ] **Step 8: Commit identity collection**

```powershell
git add crates/rustnt-core/src/service.rs
git commit -m "feat: report RustNT service token identity"
```

---

### Task 4: Implement the secured Named Pipe client and server

**Files:**
- Modify: `crates/rustnt-core/src/service.rs`
- Test: `crates/rustnt-core/src/service.rs` unit-test module

**Interfaces:**

```rust
pub struct ServiceClient;

impl ServiceClient {
    pub fn request(&self, command: Command) -> Result<Response, ServiceError>;
}

fn run_pipe_server(stop_event: HANDLE) -> Result<(), ServiceError>;
```

- [ ] **Step 1: Write pure request-dispatch tests**

```rust
#[test]
fn dispatch_allows_only_task_03_commands() {
    assert_eq!(dispatch_command(Command::Ping), Response {
        status: STATUS_SUCCESS,
        payload: Vec::new(),
    });
    assert!(dispatch_command(Command::Identity)
        .payload
        .starts_with(b"SERVICE=RustNTControl\n"));
    assert!(String::from_utf8(dispatch_command(Command::Capabilities).payload)
        .expect("capabilities should be UTF-8")
        .contains("ping,identity,capabilities"));
}
```

- [ ] **Step 2: Run the focused test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core dispatch_allows_only_task_03_commands
```

Expected: compilation failure because the dispatch function and Pipe client
do not exist.

- [ ] **Step 3: Create the explicit Pipe security descriptor**

Use this SDDL string:

```text
D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)
```

Convert it with `ConvertStringSecurityDescriptorToSecurityDescriptorW`, put
the returned descriptor in `SECURITY_ATTRIBUTES`, and pass it to
`CreateNamedPipeW`. Use:

```text
open mode:   PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED
pipe mode:   PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT
instances:   1
buffers:     4096
timeout:     5000
```

Include `PIPE_REJECT_REMOTE_CLIENTS` in the open mode. Keep the security
descriptor alive until `CreateNamedPipeW` returns.

- [ ] **Step 4: Implement the synchronous client request**

Use `CreateFileW` for `PIPE_NAME` with `GENERIC_READ | GENERIC_WRITE`. Write
one encoded request with `WriteFile`, call `FlushFileBuffers`, read the
response into a `MAX_RESPONSE_FRAME_SIZE` buffer with `ReadFile`, decode it,
and close the Pipe through the RAII wrapper. Use a 5-second `WaitNamedPipeW`
timeout before opening. The client sends zero payload bytes for all Task 03
commands.

- [ ] **Step 5: Implement the bounded server loop**

Create an overlapped Pipe instance and use the `stop_event` supplied by the
service host. Start `ConnectNamedPipe` with an `OVERLAPPED` event. Wait on the
connect event and the service stop event. If the stop event is signaled, cancel
or close the pending Pipe and exit. After a connection, read exactly one frame
into a `MAX_REQUEST_FRAME_SIZE` buffer, reject frames larger than
`MAX_REQUEST_FRAME_SIZE`, dispatch only known commands, write one response,
flush, disconnect, and create the next instance.

Do not retain client handles or process any second request on one connection.
Return operational errors to the service host while treating malformed client
frames as per-request error responses.

- [ ] **Step 6: Add safety comments around overlapped I/O**

Each unsafe call must explain:

- the Pipe and event handles are valid and owned for the call;
- the `OVERLAPPED` structure and event remain alive until the wait completes;
- the byte buffers are writable or readable for the exact declared length;
- the service loop does not retain raw pointers after the call.

- [ ] **Step 7: Run Pipe unit tests and compile**

Run:

```powershell
cargo test -p rustnt-core service
cargo check --workspace
cargo fmt --all -- --check
```

Expected: dispatch and protocol tests pass, the workspace compiles, and
formatting exits with code 0.

- [ ] **Step 8: Commit the IPC layer**

```powershell
git add crates/rustnt-core/src/service.rs
git commit -m "feat: add secured RustNT named pipe bridge"
```

---

### Task 5: Implement the Windows Service host and service binary

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/rustnt-core/src/service.rs`
- Create: `crates/rustnt-service/Cargo.toml`
- Create: `crates/rustnt-service/src/main.rs`
- Test: `crates/rustnt-core/src/service.rs` unit-test module

**Interfaces:**

```rust
pub fn run_service() -> Result<(), ServiceError>;
```

- [ ] **Step 1: Write service-status reporting tests**

Add a pure test for the status transition sequence:

```rust
#[test]
fn service_status_sequence_is_start_run_stop() {
    assert_eq!(
        SERVICE_TRANSITIONS,
        &[
            ServiceState::StartPending,
            ServiceState::Running,
            ServiceState::StopPending,
            ServiceState::Stopped,
        ]
    );
}
```

- [ ] **Step 2: Run the focused test and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-core service_status_sequence
```

Expected: compilation failure until the service host status definitions exist.

- [ ] **Step 3: Implement the Service dispatcher entry**

Add `crates/rustnt-service` to the workspace and create its manifest with only
the local `rustnt-core` dependency. Create `src/main.rs` with:

```rust
#![cfg(windows)]

fn main() {
    if let Err(error) = rustnt_core::service::run_service() {
        eprintln!("rustnt-service error: {error}");
        std::process::exit(1);
    }
}
```

Build a nul-terminated UTF-16 service name and a
`SERVICE_TABLE_ENTRYW` array ending with a null entry. Call
`StartServiceCtrlDispatcherW`. Register the service control callback with
`RegisterServiceCtrlHandlerExW`.

The service entry must:

1. Register the control handler.
2. Report `START_PENDING`.
3. Create the stop event.
4. Report `RUNNING`.
5. Run `run_pipe_server(stop_event)`.
6. Report `STOP_PENDING`.
7. Close the Pipe/event resources.
8. Report `STOPPED`.

- [ ] **Step 4: Implement the control callback**

Accept `SERVICE_CONTROL_STOP` and `SERVICE_CONTROL_SHUTDOWN`. Set the shared
stop event and return `NO_ERROR`. Return `ERROR_CALL_NOT_IMPLEMENTED` for
pause, continue, interrogate, and unknown controls. Store the status handle,
stop event, and running state in a boxed runtime context whose address remains
stable until the service exits.

Document the callback safety assumptions: SCM invokes it only while the
registered context remains alive; the context owns the event handle; the
callback performs only atomic/event/status operations and does not free the
context.

- [ ] **Step 5: Connect dispatch to the Pipe server**

The `run_pipe_server` loop returns normally on stop. A malformed client request
must not terminate the service. A fatal Pipe creation, security descriptor, or
I/O error must cause the host to report `STOPPED` with a nonzero Win32
service-specific exit code.

- [ ] **Step 6: Run build and unit tests**

Run:

```powershell
cargo test --workspace
cargo build --workspace
cargo fmt --all -- --check
```

Expected: all tests pass, `target\debug\rustnt-service.exe` exists, and
formatting exits with code 0.

- [ ] **Step 7: Commit the service host**

```powershell
git add crates/rustnt-core/src/service.rs crates/rustnt-service/src/main.rs
git commit -m "feat: host RustNT as a Windows service"
```

---

### Task 6: Add CLI service commands and output

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs`
- Test: `crates/rustnt-cli/src/main.rs` unit-test module

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceCommand {
    Install,
    Uninstall,
    Start,
    Stop,
    Status,
    Identity,
}

fn parse_service_command(args: &[String]) -> Result<ServiceCommand, String>;
fn service_binary_path() -> Result<std::path::PathBuf, String>;
fn render_service_status(status: rustnt_core::service::ServiceStatus) -> String;
fn render_identity(identity: rustnt_core::service::ServiceIdentity) -> String;
```

- [ ] **Step 1: Write failing CLI parser and renderer tests**

```rust
#[test]
fn parses_all_service_commands() {
    assert_eq!(
        parse_service_command(&strings(&["install"])).expect("install should parse"),
        ServiceCommand::Install
    );
    assert_eq!(
        parse_service_command(&strings(&["identity"])).expect("identity should parse"),
        ServiceCommand::Identity
    );
}

#[test]
fn rejects_unknown_or_extra_service_arguments() {
    assert!(parse_service_command(&strings(&["unknown"])).is_err());
    assert!(parse_service_command(&strings(&["start", "extra"])).is_err());
}

#[test]
fn renders_identity_fields() {
    let identity = sample_identity();
    let output = render_identity(identity);
    assert!(output.contains("ACCOUNT       LocalSystem"));
    assert!(output.contains("INTEGRITY     System"));
    assert!(output.contains("ELEVATED      true"));
    assert!(output.contains("CAPABILITIES  ping,identity,capabilities"));
}
```

- [ ] **Step 2: Run focused CLI tests and verify the expected failure**

Run:

```powershell
cargo test -p rustnt-cli service
```

Expected: compilation failure because service command parsing and renderers do
not exist.

- [ ] **Step 3: Implement command routing**

Preserve the existing process route:

```text
rustnt process list [Task 02 options]
```

Add:

```text
rustnt service install
rustnt service uninstall
rustnt service start
rustnt service stop
rustnt service status
rustnt service identity
```

Reject missing, unknown, and extra arguments with the service usage text and
exit code 2. Resolve `rustnt-service.exe` as the sibling of
`env::current_exe()`. Do not search `PATH` or launch another command.

- [ ] **Step 4: Implement lifecycle output**

Use typed core calls. Print:

```text
RustNT Service

SERVICE       RustNTControl
STATE         RUNNING
PROCESS       1234
```

For `NOT_INSTALLED`, print `STATE         NOT_INSTALLED` and return success from
`status`. Return runtime errors from install/start/stop/uninstall/identity as
exit code 1.

- [ ] **Step 5: Implement identity output**

Call `ServiceClient::request(Command::Identity)`, decode the response payload,
and print:

```text
RustNT Service Identity

SERVICE       RustNTControl
ACCOUNT       LocalSystem
ACCOUNT SID   S-1-5-18
INTEGRITY     System
ELEVATED      true
PROTOCOL      1
CAPABILITIES  ping,identity,capabilities
```

If the service is stopped, report a concise error telling the user to run
`rustnt service start`; do not auto-start the service.

- [ ] **Step 6: Run CLI tests and formatting**

Run:

```powershell
cargo test -p rustnt-cli
cargo test --workspace
cargo fmt --all -- --check
```

Expected: all tests pass and formatting exits with code 0.

- [ ] **Step 7: Commit the CLI surface**

```powershell
git add crates/rustnt-cli/src/main.rs
git commit -m "feat: add RustNT service CLI commands"
```

---

### Task 7: Update documentation and perform privileged lifecycle verification

**Files:**
- Modify: `README.md`
- Modify: `docs/architecture.md`
- Modify: `docs/learning-notes.md`
- Modify: `docs/benchmarks.md`
- Test: workspace and manual Windows service lifecycle

- [ ] **Step 1: Add service usage documentation**

Document:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
rustnt service stop
rustnt service uninstall
```

State that the service runs as LocalSystem, starts on demand, listens only on
the local secured Pipe, and exposes only read-only identity/capability
commands in Task 03. State clearly that process control belongs to Task 04 and
arbitrary administrator commands belong to a separately reviewed Task 05.

- [ ] **Step 2: Document the architecture and learning concepts**

Update the architecture diagram to include:

```text
rustnt-cli
    |
    v
rustnt-core::service
    |
    +--> SCM
    +--> Named Pipe
    +--> Process Token
    |
    v
rustnt-service
```

Explain SCM registration, demand start, service status transitions, Named Pipe
framing, explicit DACLs, LocalSystem risk, Token identity, and why a whitelist
is required for future privileged actions.

- [ ] **Step 3: Extend benchmark guidance**

Add measurements for:

```text
service install latency
service start-to-running latency
Pipe request/response round trip
identity query duration
service stop latency
```

Record Windows version, account context, whether the service was cold or
already installed, and whether the test ran elevated.

- [ ] **Step 4: Run non-privileged verification**

Run:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo build --workspace
```

Expected: formatting, tests, and build all exit with code 0.

- [ ] **Step 5: Run the administrator lifecycle smoke test**

From an elevated terminal, execute:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
rustnt service stop
rustnt service uninstall
```

Verify:

```text
install       exits 0
start         exits 0 and reaches RUNNING
status        prints RUNNING and a service process ID
identity      prints LocalSystem, System, true, protocol 1
stop          exits 0 and reaches STOPPED
uninstall     exits 0
```

If a step fails, capture the operation name and Win32 error code. Always rerun
`rustnt service stop` and `rustnt service uninstall` when a service was
successfully installed, so the test machine does not retain an unintended
privileged service.

- [ ] **Step 6: Perform the final scope scan**

Run:

```powershell
rg -n "PowerShell|tasklist|wmic|WMI|tokio|CreateProcess|WinExec|ShellExecute|unsafe|CloseHandle|CloseServiceHandle|LocalSystem|RustNT.Control.v1" crates docs README.md
```

Confirm:

- external command names occur only in constraints or explanatory
  documentation, never in implementation code;
- all new unsafe blocks have nearby safety comments;
- every owned HANDLE has a matching RAII wrapper;
- the only protocol commands are `PING`, `IDENTITY`, and `CAPABILITIES`;
- no process-control or arbitrary-command code exists.

- [ ] **Step 7: Commit the completed Task 03**

```powershell
git add README.md docs crates
git commit -m "feat: add restricted LocalSystem service bridge"
```

If author identity is still unavailable, leave the complete changes staged and
report that commit is the only remaining repository operation.
