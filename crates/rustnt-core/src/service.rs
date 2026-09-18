use std::fmt;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
#[cfg(windows)]
use std::sync::{Condvar, Mutex};
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_CALL_NOT_IMPLEMENTED,
    ERROR_INVALID_HANDLE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_NOT_FOUND, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SERVICE_ALREADY_RUNNING,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_EXISTS, ERROR_SERVICE_NOT_ACTIVE,
    ERROR_SERVICE_SPECIFIC_ERROR, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    NO_ERROR, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenIntegrityLevel, TokenUser, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TOKEN_USER,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, DELETE, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, SetNamedPipeHandleState,
    WaitNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE,
    PIPE_WAIT,
};
#[cfg(windows)]
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx, RegisterServiceCtrlHandlerExW,
    SetServiceStatus, StartServiceCtrlDispatcherW, StartServiceW, QUERY_SERVICE_CONFIGW, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START,
    SERVICE_ERROR_NORMAL, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, SetEvent, WaitForMultipleObjects, INFINITE,
};
#[cfg(windows)]
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0};

pub const SERVICE_NAME: &str = "RustNTControl";
pub const PIPE_NAME: &str = r"\\.\pipe\RustNT.Control.v1";
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_PAYLOAD: usize = 4096;
/// The request header size: magic, version, command, and payload length.
pub const FRAME_HEADER_SIZE: usize = 12;
pub const MAX_REQUEST_FRAME_SIZE: usize = FRAME_HEADER_SIZE + MAX_PAYLOAD;
/// Backward-compatible alias for the maximum request frame size.
pub const MAX_FRAME_SIZE: usize = MAX_REQUEST_FRAME_SIZE;
/// The response header size: magic, version, status, and payload length.
pub const RESPONSE_HEADER_SIZE: usize = 14;
pub const MAX_RESPONSE_FRAME_SIZE: usize = RESPONSE_HEADER_SIZE + MAX_PAYLOAD;
pub const STATUS_SUCCESS: u32 = 0;

const REQUEST_MAGIC: &[u8; 4] = b"RNT1";
const RESPONSE_MAGIC: &[u8; 4] = b"RNS1";
#[cfg(windows)]
const RESPONSE_ACK_MAGIC: &[u8; 4] = b"ACK1";
const STATUS_ERROR: u32 = 1;
#[cfg(windows)]
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";
#[cfg(windows)]
const PIPE_BUFFER_SIZE: u32 = MAX_PAYLOAD as u32;
#[cfg(windows)]
const PIPE_TIMEOUT_MS: u32 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    pub operation: &'static str,
    pub code: Option<u32>,
    pub message: String,
}

impl ServiceError {
    fn protocol(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            code: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)?;
        if let Some(code) = self.code {
            write!(formatter, " (Windows error {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ServiceError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    Stopped,
    StartPending,
    Running,
    StopPending,
    Other(u32),
}

pub const SERVICE_TRANSITIONS: &[ServiceState] = &[
    ServiceState::StartPending,
    ServiceState::Running,
    ServiceState::StopPending,
    ServiceState::Stopped,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatus {
    pub state: ServiceState,
    pub process_id: Option<u32>,
}

#[cfg(windows)]
fn map_service_state(state: u32) -> ServiceState {
    match state {
        SERVICE_STOPPED => ServiceState::Stopped,
        SERVICE_START_PENDING => ServiceState::StartPending,
        SERVICE_RUNNING => ServiceState::Running,
        SERVICE_STOP_PENDING => ServiceState::StopPending,
        other => ServiceState::Other(other),
    }
}

#[cfg(not(windows))]
fn map_service_state(state: u32) -> ServiceState {
    ServiceState::Other(state)
}

#[cfg(windows)]
struct OwnedScHandle(SC_HANDLE);

#[cfg(windows)]
impl OwnedScHandle {
    fn new(handle: SC_HANDLE, operation: &'static str) -> Result<Self, ServiceError> {
        if handle.is_null() {
            Err(ServiceError::windows(operation))
        } else {
            Ok(Self(handle))
        }
    }

    fn get(&self) -> SC_HANDLE {
        self.0
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
impl ServiceError {
    fn windows(operation: &'static str) -> Self {
        let code = unsafe {
            // SAFETY: GetLastError reads the calling thread's Win32 error value.
            GetLastError()
        };
        Self {
            operation,
            code: Some(code),
            message: "Windows API call failed".to_string(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            operation: "install service",
            code: None,
            message: message.into(),
        }
    }
}

#[cfg(windows)]
fn wide_null(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn service_name() -> Vec<u16> {
    wide_null(SERVICE_NAME)
}

#[cfg(windows)]
fn open_scm(access: u32) -> Result<OwnedScHandle, ServiceError> {
    let handle = unsafe {
        // SAFETY: Null machine and database names select the local default SCM database.
        OpenSCManagerW(ptr::null(), ptr::null(), access)
    };
    OwnedScHandle::new(handle, "open service control manager")
}

#[cfg(windows)]
fn open_service(access: u32) -> Result<OwnedScHandle, ServiceError> {
    let scm = open_scm(SC_MANAGER_CONNECT)?;
    let name = service_name();
    let handle = unsafe {
        // SAFETY: scm is valid and name is a nul-terminated UTF-16 service name.
        OpenServiceW(scm.get(), name.as_ptr(), access)
    };
    OwnedScHandle::new(handle, "open service")
}

#[cfg(windows)]
fn query_status(handle: SC_HANDLE) -> Result<ServiceStatus, ServiceError> {
    let mut status = SERVICE_STATUS_PROCESS {
        dwServiceType: 0,
        dwCurrentState: 0,
        dwControlsAccepted: 0,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
        dwProcessId: 0,
        dwServiceFlags: 0,
    };
    let mut bytes_needed = 0;
    let result = unsafe {
        // SAFETY: status is writable storage of the documented size for this info level.
        QueryServiceStatusEx(
            handle,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut bytes_needed,
        )
    };
    if result == 0 {
        Err(ServiceError::windows("query service status"))
    } else {
        Ok(ServiceStatus {
            state: map_service_state(status.dwCurrentState),
            process_id: (status.dwProcessId != 0).then_some(status.dwProcessId),
        })
    }
}

#[cfg(windows)]
fn wait_for_state(handle: SC_HANDLE, expected: ServiceState) -> Result<(), ServiceError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = query_status(handle)?;
        if status.state == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ServiceError {
                operation: "wait for service state",
                code: None,
                message: format!("timed out waiting for {expected:?}"),
            });
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
pub fn install_service(binary_path: &Path) -> Result<(), ServiceError> {
    let scm = open_scm(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    let name = service_name();
    let display_name = wide_null("RustNT Control Service");
    let binary_path = wide_null(binary_path);
    let account = wide_null("LocalSystem");
    let handle = unsafe {
        // SAFETY: All strings are nul-terminated UTF-16 buffers valid for this call.
        CreateServiceW(
            scm.get(),
            name.as_ptr(),
            display_name.as_ptr(),
            SERVICE_QUERY_STATUS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_DEMAND_START,
            SERVICE_ERROR_NORMAL,
            binary_path.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            account.as_ptr(),
            ptr::null(),
        )
    };
    if !handle.is_null() {
        drop(OwnedScHandle(handle));
        return Ok(());
    }
    let error = ServiceError::windows("create service");
    if error.code != Some(ERROR_SERVICE_EXISTS) {
        return Err(error);
    }

    let existing = open_service(SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG)?;
    let mut required = 0;
    unsafe {
        // SAFETY: The null buffer query requests the required buffer size.
        QueryServiceConfigW(existing.get(), ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(ServiceError::windows(
            "query existing service configuration",
        ));
    }
    let word_size = std::mem::size_of::<usize>();
    let word_count = (required as usize)
        .checked_add(word_size - 1)
        .and_then(|size| size.checked_div(word_size))
        .ok_or_else(|| {
            ServiceError::protocol(
                "query existing service configuration",
                "service configuration size overflows the addressable size",
            )
        })?;
    // A usize-backed buffer has pointer alignment, which matches the alignment
    // required by QUERY_SERVICE_CONFIGW on the supported Windows targets.
    let mut buffer = vec![0usize; word_count];
    let config = unsafe {
        // SAFETY: buffer is writable, sufficiently sized, and pointer-aligned storage
        // of the size returned by the prior query.
        QueryServiceConfigW(
            existing.get(),
            buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
            (buffer.len() * word_size) as u32,
            &mut required,
        )
    };
    if config == 0 {
        return Err(ServiceError::windows(
            "query existing service configuration",
        ));
    }
    let config = unsafe { &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
    let configured_path = unsafe {
        // SAFETY: QueryServiceConfigW returned a live, nul-terminated path
        // pointer within the still-live configuration buffer.
        wide_string_from_ptr(config.lpBinaryPathName)
    };
    let expected_path = binary_path[..binary_path.len() - 1].to_vec();
    if configured_path != expected_path {
        return Err(ServiceError::conflict(
            "existing service has a different binary path",
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn uninstall_service() -> Result<(), ServiceError> {
    let service = open_service(SERVICE_QUERY_STATUS | DELETE)?;
    if query_status(service.get())?.state != ServiceState::Stopped {
        return Err(ServiceError::conflict(
            "service must be stopped before uninstall",
        ));
    }
    let result = unsafe {
        // SAFETY: service is a valid owned service handle with DELETE access.
        DeleteService(service.get())
    };
    if result == 0 {
        Err(ServiceError::windows("delete service"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub fn start_service() -> Result<(), ServiceError> {
    let service = open_service(SERVICE_START | SERVICE_QUERY_STATUS)?;
    let status = query_status(service.get())?;
    if status.state == ServiceState::Running {
        return Ok(());
    }
    let result = unsafe {
        // SAFETY: service is valid and no service arguments are supplied.
        StartServiceW(service.get(), 0, ptr::null())
    };
    if result == 0 {
        let error = ServiceError::windows("start service");
        if error.code != Some(ERROR_SERVICE_ALREADY_RUNNING) {
            return Err(error);
        }
    }
    wait_for_state(service.get(), ServiceState::Running)
}

#[cfg(windows)]
pub fn stop_service() -> Result<(), ServiceError> {
    let service = open_service(SERVICE_STOP | SERVICE_QUERY_STATUS)?;
    let status = query_status(service.get())?;
    if status.state == ServiceState::Stopped {
        return Ok(());
    }
    let mut native_status = SERVICE_STATUS {
        dwServiceType: 0,
        dwCurrentState: 0,
        dwControlsAccepted: 0,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    let result = unsafe {
        // SAFETY: native_status is writable storage for the service control response.
        ControlService(service.get(), SERVICE_CONTROL_STOP, &mut native_status)
    };
    if result == 0 {
        let error = ServiceError::windows("stop service");
        if error.code != Some(ERROR_SERVICE_NOT_ACTIVE) {
            return Err(error);
        }
    }
    wait_for_state(service.get(), ServiceState::Stopped)
}

#[cfg(windows)]
pub fn query_service_status() -> Result<ServiceStatus, ServiceError> {
    match open_service(SERVICE_QUERY_STATUS) {
        Ok(service) => query_status(service.get()),
        Err(error) if error.code == Some(ERROR_SERVICE_DOES_NOT_EXIST) => Ok(ServiceStatus {
            state: ServiceState::NotInstalled,
            process_id: None,
        }),
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
pub fn install_service(_binary_path: &std::path::Path) -> Result<(), ServiceError> {
    Err(ServiceError::protocol(
        "install service",
        "Windows is required",
    ))
}

#[cfg(not(windows))]
pub fn uninstall_service() -> Result<(), ServiceError> {
    Err(ServiceError::protocol(
        "uninstall service",
        "Windows is required",
    ))
}

#[cfg(not(windows))]
pub fn start_service() -> Result<(), ServiceError> {
    Err(ServiceError::protocol(
        "start service",
        "Windows is required",
    ))
}

#[cfg(not(windows))]
pub fn stop_service() -> Result<(), ServiceError> {
    Err(ServiceError::protocol(
        "stop service",
        "Windows is required",
    ))
}

#[cfg(not(windows))]
pub fn query_service_status() -> Result<ServiceStatus, ServiceError> {
    Err(ServiceError::protocol(
        "query service status",
        "Windows is required",
    ))
}

#[cfg(windows)]
/// Reads a nul-terminated UTF-16 string from a Win32-owned buffer.
///
/// # Safety
///
/// `value` must be null or point to readable memory containing a terminating
/// nul code unit. The caller must keep the buffer alive for the duration of
/// this call.
unsafe fn wide_string_from_ptr(value: *const u16) -> Vec<u16> {
    if value.is_null() {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut current = value;
    // SAFETY: The caller guarantees readable UTF-16 storage through the
    // terminating nul code unit for this loop condition and dereference.
    while *current != 0 {
        result.push(*current);
        current = current.add(1);
    }
    result
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

pub fn encode_request(request: &Request) -> Result<Vec<u8>, ServiceError> {
    validate_payload(&request.payload, "encode request")?;

    let mut bytes = Vec::with_capacity(FRAME_HEADER_SIZE + request.payload.len());
    bytes.extend_from_slice(REQUEST_MAGIC);
    bytes.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    bytes.extend_from_slice(&command_code(request.command).to_le_bytes());
    bytes.extend_from_slice(&(request.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&request.payload);
    Ok(bytes)
}

pub fn decode_request(bytes: &[u8]) -> Result<Request, ServiceError> {
    validate_frame_prefix(bytes, FRAME_HEADER_SIZE, REQUEST_MAGIC, "decode request")?;
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(ServiceError::protocol(
            "decode request",
            format!("unsupported protocol version {version}"),
        ));
    }

    let command = command_from_code(u16::from_le_bytes([bytes[6], bytes[7]]))?;
    let payload = payload_from_frame(bytes, FRAME_HEADER_SIZE, "decode request")?;
    Ok(Request { command, payload })
}

pub fn encode_response(response: &Response) -> Result<Vec<u8>, ServiceError> {
    validate_payload(&response.payload, "encode response")?;

    let mut bytes = Vec::with_capacity(RESPONSE_HEADER_SIZE + response.payload.len());
    bytes.extend_from_slice(RESPONSE_MAGIC);
    bytes.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    bytes.extend_from_slice(&response.status.to_le_bytes());
    bytes.extend_from_slice(&(response.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&response.payload);
    Ok(bytes)
}

pub fn decode_response(bytes: &[u8]) -> Result<Response, ServiceError> {
    validate_frame_prefix(
        bytes,
        RESPONSE_HEADER_SIZE,
        RESPONSE_MAGIC,
        "decode response",
    )?;
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != PROTOCOL_VERSION {
        return Err(ServiceError::protocol(
            "decode response",
            format!("unsupported protocol version {version}"),
        ));
    }

    let status = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
    let payload = payload_from_frame(bytes, RESPONSE_HEADER_SIZE, "decode response")?;
    Ok(Response { status, payload })
}

fn validate_payload(payload: &[u8], operation: &'static str) -> Result<(), ServiceError> {
    if payload.len() > MAX_PAYLOAD {
        Err(ServiceError::protocol(
            operation,
            format!("payload exceeds {MAX_PAYLOAD} bytes"),
        ))
    } else {
        Ok(())
    }
}

fn validate_frame_prefix(
    bytes: &[u8],
    header_size: usize,
    magic: &[u8; 4],
    operation: &'static str,
) -> Result<(), ServiceError> {
    if bytes.len() < header_size {
        return Err(ServiceError::protocol(
            operation,
            "frame is shorter than its header",
        ));
    }
    if &bytes[..4] != magic {
        return Err(ServiceError::protocol(operation, "invalid frame magic"));
    }
    Ok(())
}

fn payload_from_frame(
    bytes: &[u8],
    header_size: usize,
    operation: &'static str,
) -> Result<Vec<u8>, ServiceError> {
    let length_offset = header_size - 4;
    let length = u32::from_le_bytes([
        bytes[length_offset],
        bytes[length_offset + 1],
        bytes[length_offset + 2],
        bytes[length_offset + 3],
    ]) as usize;
    if length > MAX_PAYLOAD {
        return Err(ServiceError::protocol(
            operation,
            format!("payload exceeds {MAX_PAYLOAD} bytes"),
        ));
    }
    let expected_size = header_size.checked_add(length).ok_or_else(|| {
        ServiceError::protocol(operation, "frame length overflows the addressable size")
    })?;
    if bytes.len() != expected_size {
        return Err(ServiceError::protocol(
            operation,
            "frame length does not match its declared payload",
        ));
    }
    Ok(bytes[header_size..].to_vec())
}

fn command_code(command: Command) -> u16 {
    match command {
        Command::Ping => 0,
        Command::Identity => 1,
        Command::Capabilities => 2,
    }
}

fn command_from_code(code: u16) -> Result<Command, ServiceError> {
    match code {
        0 => Ok(Command::Ping),
        1 => Ok(Command::Identity),
        2 => Ok(Command::Capabilities),
        _ => Err(ServiceError::protocol(
            "decode request",
            format!("unknown command {code}"),
        )),
    }
}

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

pub fn identity_payload(identity: &ServiceIdentity) -> Vec<u8> {
    let fields = [
        identity.service.as_str(),
        identity.account.as_str(),
        identity.account_sid.as_str(),
        identity.integrity.as_str(),
    ];
    if fields.iter().any(|field| field.contains(['\n', '\r', '=']))
        || identity
            .capabilities
            .iter()
            .any(|capability| capability.contains(['\n', '\r', '=']))
    {
        return Vec::new();
    }

    format!(
        "SERVICE={}\nACCOUNT={}\nACCOUNT_SID={}\nINTEGRITY={}\nELEVATED={}\nPROTOCOL={}\nCAPABILITIES={}\n",
        identity.service,
        identity.account,
        identity.account_sid,
        identity.integrity,
        identity.elevated,
        identity.protocol_version,
        identity.capabilities.join(",")
    )
    .into_bytes()
}

#[cfg(windows)]
struct LocalAllocBuffer(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for LocalAllocBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: The pointer was returned by a LocalAlloc-backed Win32 API
                // and is released exactly once by this owner.
                LocalFree(self.0);
            }
        }
    }
}

#[cfg(windows)]
struct OwnedToken(HANDLE);

#[cfg(windows)]
impl OwnedToken {
    fn get(&self) -> HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedToken {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: The value is an owned token handle returned by OpenProcessToken
                // and is closed exactly once by this wrapper.
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn checked_pointer<T>(value: *const T, operation: &'static str) -> Result<*const T, ServiceError> {
    if value.is_null() {
        Err(ServiceError::protocol(
            operation,
            "Windows API returned a null pointer",
        ))
    } else {
        Ok(value)
    }
}

#[cfg(windows)]
fn query_token_information(
    token: HANDLE,
    class: windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS,
    operation: &'static str,
) -> Result<Vec<usize>, ServiceError> {
    let mut required = 0;
    let _ = unsafe {
        // SAFETY: A null buffer and zero length request the required byte count.
        GetTokenInformation(token, class, ptr::null_mut(), 0, &mut required)
    };
    if required == 0 {
        return Err(ServiceError::windows(operation));
    }
    let word_size = std::mem::size_of::<usize>();
    let word_count = (required as usize)
        .checked_add(word_size - 1)
        .and_then(|size| size.checked_div(word_size))
        .ok_or_else(|| ServiceError::protocol(operation, "token information size overflows"))?;
    let mut buffer = vec![0usize; word_count];
    let result = unsafe {
        // SAFETY: The buffer is writable for the size returned by the preceding query.
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    if result == 0 {
        Err(ServiceError::windows(operation))
    } else {
        Ok(buffer)
    }
}

#[cfg(windows)]
fn sid_string(sid: windows_sys::Win32::Security::PSID) -> Result<String, ServiceError> {
    let sid = checked_pointer(sid.cast_const(), "convert token SID")?;
    let mut string_sid = ptr::null_mut();
    let result = unsafe {
        // SAFETY: sid was checked non-null and points into a live token
        // information buffer owned by the caller.
        ConvertSidToStringSidW(sid.cast_mut().cast(), &mut string_sid)
    };
    if result == 0 {
        return Err(ServiceError::windows("convert token SID"));
    }
    let string_sid = checked_pointer(string_sid.cast_const(), "convert token SID")?;
    let buffer = LocalAllocBuffer(string_sid.cast_mut().cast());
    let value = unsafe {
        // SAFETY: ConvertSidToStringSidW returned a live, nul-terminated
        // LocalAlloc buffer owned by `buffer`.
        wide_string_from_ptr(buffer.0.cast())
    };
    Ok(String::from_utf16_lossy(&value))
}

#[cfg(windows)]
fn integrity_name(rid: u32) -> String {
    match rid {
        0 => "Untrusted".to_string(),
        4096 => "Low".to_string(),
        8192 => "Medium".to_string(),
        12288 => "High".to_string(),
        16384 => "System".to_string(),
        20480 => "Protected".to_string(),
        other => format!("Unknown({other})"),
    }
}

#[cfg(windows)]
pub fn collect_identity() -> Result<ServiceIdentity, ServiceError> {
    let mut token = ptr::null_mut();
    let result = unsafe {
        // SAFETY: GetCurrentProcess returns a pseudo-handle valid for this process;
        // token is writable storage for the returned owned handle.
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
    };
    if result == 0 {
        return Err(ServiceError::windows("open process token"));
    }
    let token = OwnedToken(token);

    let user = query_token_information(token.get(), TokenUser, "query token user")?;
    let user = unsafe {
        // SAFETY: The second query filled user with a TOKEN_USER structure at its start.
        &*user.as_ptr().cast::<TOKEN_USER>()
    };
    checked_pointer(user.User.Sid.cast_const(), "query token user SID")?;
    let account_sid = sid_string(user.User.Sid)?;
    let account = if account_sid == "S-1-5-18" {
        "LocalSystem".to_string()
    } else {
        account_sid.clone()
    };

    let label = query_token_information(
        token.get(),
        TokenIntegrityLevel,
        "query token integrity level",
    )?;
    let label = unsafe {
        // SAFETY: The second query filled label with a TOKEN_MANDATORY_LABEL structure.
        &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()
    };
    let integrity_sid = checked_pointer(label.Label.Sid.cast_const(), "query token integrity SID")?;
    let count_pointer = unsafe {
        // SAFETY: The SID pointer is supplied by the validated token information buffer.
        GetSidSubAuthorityCount(integrity_sid.cast_mut().cast())
    };
    let count = unsafe {
        // SAFETY: GetSidSubAuthorityCount returned the address of the SID's
        // sub-authority count, which is checked before dereferencing.
        *checked_pointer(
            count_pointer.cast_const(),
            "query token integrity SID count",
        )?
    };
    if count == 0 {
        return Err(ServiceError::protocol(
            "query token integrity level",
            "token integrity SID has no sub-authorities",
        ));
    }
    let integrity_pointer = unsafe {
        // SAFETY: The final sub-authority index is valid because count is nonzero.
        GetSidSubAuthority(integrity_sid.cast_mut().cast(), u32::from(count - 1))
    };
    let integrity_rid = unsafe {
        // SAFETY: GetSidSubAuthority returned the address of the final RID,
        // which is checked before dereferencing.
        *checked_pointer(integrity_pointer.cast_const(), "query token integrity RID")?
    };

    let elevation = query_token_information(token.get(), TokenElevation, "query token elevation")?;
    let elevation = unsafe {
        // SAFETY: The second query filled elevation with a TOKEN_ELEVATION structure.
        &*elevation.as_ptr().cast::<TOKEN_ELEVATION>()
    };

    Ok(ServiceIdentity {
        service: SERVICE_NAME.to_string(),
        account,
        account_sid,
        integrity: integrity_name(integrity_rid),
        elevated: elevation.TokenIsElevated != 0,
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![
            "ping".to_string(),
            "identity".to_string(),
            "capabilities".to_string(),
        ],
    })
}

#[cfg(not(windows))]
pub fn collect_identity() -> Result<ServiceIdentity, ServiceError> {
    Err(ServiceError::protocol(
        "collect identity",
        "Windows is required",
    ))
}

fn dispatch_command(command: Command) -> Response {
    match command {
        Command::Ping => Response {
            status: STATUS_SUCCESS,
            payload: Vec::new(),
        },
        Command::Identity => match collect_identity() {
            Ok(identity) => Response {
                status: STATUS_SUCCESS,
                payload: identity_payload(&identity),
            },
            Err(error) => request_error_response(error),
        },
        Command::Capabilities => Response {
            status: STATUS_SUCCESS,
            payload: b"ping,identity,capabilities\n".to_vec(),
        },
    }
}

fn dispatch_request(request: Request) -> Response {
    if request.payload.is_empty() {
        dispatch_command(request.command)
    } else {
        request_error_response(ServiceError::protocol(
            "dispatch request",
            "Task 03 commands do not accept request payloads",
        ))
    }
}

fn request_error_response(error: ServiceError) -> Response {
    let mut payload = error.to_string().into_bytes();
    payload.truncate(MAX_PAYLOAD);
    Response {
        status: STATUS_ERROR,
        payload,
    }
}

pub struct ServiceClient;

#[cfg(windows)]
impl ServiceClient {
    pub fn request(&self, command: Command) -> Result<Response, ServiceError> {
        let name = wide_null(PIPE_NAME);
        let deadline = Instant::now() + Duration::from_millis(PIPE_TIMEOUT_MS as u64);
        let pipe = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.as_millis() == 0 {
                return Err(ServiceError::protocol(
                    "wait for RustNT service pipe",
                    "timed out while opening the service pipe",
                ));
            }
            let wait_ms = u32::try_from(remaining.as_millis())
                .unwrap_or(u32::MAX)
                .max(1);
            let available = unsafe {
                // SAFETY: name is a nul-terminated UTF-16 Pipe name valid for the duration
                // of this bounded wait; the function retains no pointer after it returns.
                WaitNamedPipeW(name.as_ptr(), wait_ms)
            };
            if available == 0 {
                return Err(ServiceError::windows("wait for RustNT service pipe"));
            }

            let handle = unsafe {
                // SAFETY: name is a nul-terminated UTF-16 Pipe name and all remaining
                // pointer parameters are null where the API permits them.
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            };
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                break OwnedPipe::new(handle, "open RustNT service pipe")?;
            }

            let error = ServiceError::windows("open RustNT service pipe");
            if error.code != Some(ERROR_PIPE_BUSY) {
                return Err(error);
            }
        };

        let mode = PIPE_READMODE_MESSAGE;
        let mode_set = unsafe {
            // SAFETY: pipe is an owned, open Named Pipe handle and mode points to a
            // live message-read-mode value for the duration of this synchronous call.
            SetNamedPipeHandleState(pipe.get(), &mode, ptr::null(), ptr::null())
        };
        if mode_set == 0 {
            return Err(ServiceError::windows("set RustNT pipe read mode"));
        }

        let request = encode_request(&Request {
            command,
            payload: Vec::new(),
        })?;
        write_sync_pipe_message(pipe.get(), &request, "write RustNT pipe request")?;

        let flushed = unsafe {
            // SAFETY: pipe is an owned, open Pipe handle and no raw Rust pointer is
            // retained by FlushFileBuffers after it returns.
            FlushFileBuffers(pipe.get())
        };
        if flushed == 0 {
            return Err(ServiceError::windows("flush RustNT pipe request"));
        }

        let mut response = vec![0u8; MAX_RESPONSE_FRAME_SIZE];
        let response_size = read_sync_pipe_message(
            pipe.get(),
            &mut response,
            "read RustNT pipe response",
            MAX_RESPONSE_FRAME_SIZE,
        )?;
        let decoded = decode_response(&response[..response_size]);
        write_sync_pipe_message(
            pipe.get(),
            RESPONSE_ACK_MAGIC,
            "acknowledge RustNT response",
        )?;
        decoded
    }
}

#[cfg(not(windows))]
impl ServiceClient {
    pub fn request(&self, _command: Command) -> Result<Response, ServiceError> {
        Err(ServiceError::protocol(
            "request RustNT service pipe",
            "Windows is required",
        ))
    }
}

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, ServiceError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(ServiceError::windows(operation))
        } else {
            Ok(Self(handle))
        }
    }

    fn get(&self) -> HANDLE {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                // SAFETY: This wrapper uniquely owns a Win32 handle and closes it exactly
                // once after all operations using it have completed.
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
struct OwnedPipe {
    handle: HANDLE,
    connected: bool,
}

#[cfg(windows)]
impl OwnedPipe {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, ServiceError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(ServiceError::windows(operation))
        } else {
            Ok(Self {
                handle,
                connected: false,
            })
        }
    }

    fn get(&self) -> HANDLE {
        self.handle
    }

    fn mark_connected(&mut self) {
        self.connected = true;
    }
}

#[cfg(windows)]
impl Drop for OwnedPipe {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            if self.connected {
                unsafe {
                    // SAFETY: This server-side Pipe handle is still owned here; a single
                    // connection is detached before the handle is closed.
                    DisconnectNamedPipe(self.handle);
                }
            }
            unsafe {
                // SAFETY: This wrapper uniquely owns the Pipe handle and all pending I/O
                // is completed or cancelled before the wrapper is dropped.
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
fn is_pipe_disconnect_code(code: Option<u32>) -> bool {
    matches!(code, Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA))
}

#[cfg(windows)]
enum PipeIoError {
    Stopped,
    Disconnected,
    TimedOut,
    FrameTooLarge,
    Operation(ServiceError),
}

#[cfg(windows)]
fn create_pipe_instance() -> Result<OwnedPipe, ServiceError> {
    let sddl = wide_null(PIPE_SDDL);
    let mut descriptor = ptr::null_mut();
    let descriptor_created = unsafe {
        // SAFETY: sddl is a nul-terminated UTF-16 SDDL string and descriptor is writable
        // storage for the LocalAlloc-owned descriptor returned by the API.
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if descriptor_created == 0 {
        return Err(ServiceError::windows(
            "create RustNT pipe security descriptor",
        ));
    }
    if descriptor.is_null() {
        return Err(ServiceError::protocol(
            "create RustNT pipe security descriptor",
            "Windows API returned a null security descriptor",
        ));
    }
    let descriptor = LocalAllocBuffer(descriptor);
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide_null(PIPE_NAME);
    let handle = unsafe {
        // SAFETY: name is nul-terminated, security_attributes and its LocalAlloc-backed
        // descriptor remain live for this call, and CreateNamedPipeW retains neither pointer.
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER_SIZE,
            PIPE_BUFFER_SIZE,
            PIPE_TIMEOUT_MS,
            &security_attributes,
        )
    };
    OwnedPipe::new(handle, "create RustNT service pipe")
}

#[cfg(windows)]
fn create_io_event(operation: &'static str) -> Result<OwnedHandle, ServiceError> {
    let event = unsafe {
        // SAFETY: Null attributes and name request a private manual-reset event; no pointer
        // is retained after CreateEventW returns.
        CreateEventW(ptr::null(), 1, 0, ptr::null())
    };
    OwnedHandle::new(event, operation)
}

#[cfg(windows)]
fn new_overlapped(event: HANDLE) -> OVERLAPPED {
    OVERLAPPED {
        Internal: 0,
        InternalHigh: 0,
        Anonymous: OVERLAPPED_0 {
            Pointer: ptr::null_mut(),
        },
        hEvent: event,
    }
}

#[cfg(windows)]
fn complete_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    operation: &'static str,
) -> Result<u32, ServiceError> {
    let mut transferred = 0;
    let completed = unsafe {
        // SAFETY: handle is an owned Pipe handle, overlapped and its event remain live
        // until this wait completes, and transferred is writable result storage.
        GetOverlappedResult(handle, overlapped, &mut transferred, 1)
    };
    if completed == 0 {
        Err(ServiceError::windows(operation))
    } else {
        Ok(transferred)
    }
}

#[cfg(windows)]
enum OverlappedWait {
    Completed(u32),
    Stopped,
    TimedOut,
}

#[cfg(windows)]
fn cancel_and_complete_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    operation: &'static str,
) -> Result<OverlappedWait, ServiceError> {
    let cancel_error = unsafe {
        // SAFETY: handle owns the pending operation referenced by overlapped, and both
        // remain alive until GetOverlappedResult below has returned.
        if CancelIoEx(handle, overlapped) == 0 {
            let error = ServiceError::windows(operation);
            (error.code != Some(ERROR_NOT_FOUND)).then_some(error)
        } else {
            None
        }
    };

    let mut transferred = 0;
    let completed = unsafe {
        // SAFETY: cancellation has been requested, and handle/overlapped remain live until
        // this blocking completion query returns; transferred is writable result storage.
        GetOverlappedResult(handle, overlapped, &mut transferred, 1)
    };
    if completed != 0 {
        if let Some(error) = cancel_error {
            return Err(error);
        }
        return Ok(OverlappedWait::Completed(transferred));
    }

    let completion_error = ServiceError::windows(operation);
    if completion_error.code == Some(ERROR_OPERATION_ABORTED) && cancel_error.is_none() {
        return Ok(OverlappedWait::Stopped);
    }
    Err(cancel_error.unwrap_or(completion_error))
}

#[cfg(windows)]
fn wait_overlapped_with_stop(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    stop_event: HANDLE,
    operation: &'static str,
) -> Result<OverlappedWait, ServiceError> {
    wait_overlapped_with_stop_timeout(handle, overlapped, stop_event, operation, INFINITE)
}

#[cfg(windows)]
fn wait_overlapped_with_stop_timeout(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    stop_event: HANDLE,
    operation: &'static str,
    timeout_ms: u32,
) -> Result<OverlappedWait, ServiceError> {
    let handles = [overlapped.hEvent, stop_event];
    let wait_result = unsafe {
        // SAFETY: both handles are valid for the duration of this wait; overlapped and its
        // event remain live until the pending operation is completed or cancelled.
        WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout_ms)
    };
    if wait_result == WAIT_OBJECT_0 {
        return complete_overlapped(handle, overlapped, operation).map(OverlappedWait::Completed);
    }
    if wait_result == WAIT_OBJECT_0 + 1 {
        return match cancel_and_complete_overlapped(handle, overlapped, operation) {
            Ok(_) => Ok(OverlappedWait::Stopped),
            Err(error) => Err(error),
        };
    }
    if wait_result == WAIT_TIMEOUT {
        return match cancel_and_complete_overlapped(handle, overlapped, operation) {
            Ok(_) => Ok(OverlappedWait::TimedOut),
            Err(error) => Err(error),
        };
    }

    let wait_error = if wait_result == WAIT_FAILED {
        ServiceError::windows(operation)
    } else {
        ServiceError::protocol(operation, "unexpected wait result")
    };
    match cancel_and_complete_overlapped(handle, overlapped, operation) {
        Ok(_) => Err(wait_error),
        Err(cleanup_error) => Err(cleanup_error),
    }
}

#[cfg(windows)]
fn write_sync_pipe_message(
    handle: HANDLE,
    bytes: &[u8],
    operation: &'static str,
) -> Result<(), ServiceError> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| ServiceError::protocol(operation, "message length exceeds u32"))?;
    let mut written = 0;
    let result = unsafe {
        // SAFETY: handle is an open synchronous Pipe handle; bytes is readable for length
        // bytes and written is writable result storage for the duration of this call.
        WriteFile(
            handle,
            bytes.as_ptr(),
            length,
            &mut written,
            ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(ServiceError::windows(operation));
    }
    if written != length {
        return Err(ServiceError::protocol(
            operation,
            "Pipe write was incomplete",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn read_sync_pipe_message(
    handle: HANDLE,
    buffer: &mut [u8],
    operation: &'static str,
    frame_limit: usize,
) -> Result<usize, ServiceError> {
    let length = u32::try_from(buffer.len())
        .map_err(|_| ServiceError::protocol(operation, "buffer length exceeds u32"))?;
    let mut read = 0;
    let result = unsafe {
        // SAFETY: handle is an open synchronous Pipe handle; buffer is writable for length
        // bytes and read is writable result storage for the duration of this call.
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            length,
            &mut read,
            ptr::null_mut(),
        )
    };
    if result == 0 {
        let error = ServiceError::windows(operation);
        if error.code == Some(ERROR_MORE_DATA) {
            return Err(ServiceError::protocol(
                operation,
                format!("Pipe frame exceeds {frame_limit} bytes"),
            ));
        }
        return Err(error);
    }
    Ok(read as usize)
}

#[cfg(windows)]
fn write_overlapped_pipe_message(
    handle: HANDLE,
    bytes: &[u8],
    operation: &'static str,
    stop_event: HANDLE,
) -> Result<(), PipeIoError> {
    let length = u32::try_from(bytes.len()).map_err(|_| {
        PipeIoError::Operation(ServiceError::protocol(
            operation,
            "message length exceeds u32",
        ))
    })?;
    let event = create_io_event(operation).map_err(PipeIoError::Operation)?;
    let mut overlapped = new_overlapped(event.get());
    let mut written = 0;
    let result = unsafe {
        // SAFETY: handle and event are owned and live through completion; bytes is readable
        // for length bytes, and overlapped/written remain live until I/O has completed.
        WriteFile(
            handle,
            bytes.as_ptr(),
            length,
            &mut written,
            &mut overlapped,
        )
    };
    let written = if result != 0 {
        written
    } else {
        let error = ServiceError::windows(operation);
        if is_pipe_disconnect_code(error.code) {
            return Err(PipeIoError::Disconnected);
        }
        if error.code != Some(ERROR_IO_PENDING) {
            return Err(PipeIoError::Operation(error));
        }
        match wait_overlapped_with_stop(handle, &overlapped, stop_event, operation).map_err(
            |error| {
                if is_pipe_disconnect_code(error.code) {
                    PipeIoError::Disconnected
                } else {
                    PipeIoError::Operation(error)
                }
            },
        )? {
            OverlappedWait::Completed(written) => written,
            OverlappedWait::Stopped => return Err(PipeIoError::Stopped),
            OverlappedWait::TimedOut => return Err(PipeIoError::TimedOut),
        }
    };
    if written != length {
        return Err(PipeIoError::Operation(ServiceError::protocol(
            operation,
            "Pipe write was incomplete",
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn read_overlapped_pipe_message(
    handle: HANDLE,
    buffer: &mut [u8],
    operation: &'static str,
    stop_event: HANDLE,
    timeout_ms: u32,
) -> Result<usize, PipeIoError> {
    let length = u32::try_from(buffer.len()).map_err(|_| {
        PipeIoError::Operation(ServiceError::protocol(
            operation,
            "buffer length exceeds u32",
        ))
    })?;
    let event = create_io_event(operation).map_err(PipeIoError::Operation)?;
    let mut overlapped = new_overlapped(event.get());
    let mut read = 0;
    let result = unsafe {
        // SAFETY: handle and event are owned and live through completion; buffer is writable
        // for length bytes, and overlapped/read remain live until I/O has completed.
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            length,
            &mut read,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(read as usize);
    }

    let error = ServiceError::windows(operation);
    if error.code == Some(ERROR_MORE_DATA) {
        return Err(PipeIoError::FrameTooLarge);
    }
    if is_pipe_disconnect_code(error.code) {
        return Err(PipeIoError::Disconnected);
    }
    if error.code != Some(ERROR_IO_PENDING) {
        return Err(PipeIoError::Operation(error));
    }

    match wait_overlapped_with_stop_timeout(handle, &overlapped, stop_event, operation, timeout_ms)
        .map_err(|error| {
            if is_pipe_disconnect_code(error.code) {
                PipeIoError::Disconnected
            } else {
                PipeIoError::Operation(error)
            }
        })? {
        OverlappedWait::Stopped => Err(PipeIoError::Stopped),
        OverlappedWait::TimedOut => Err(PipeIoError::TimedOut),
        OverlappedWait::Completed(read) => Ok(read as usize),
    }
}

#[cfg(windows)]
fn wait_for_pipe_connection(
    pipe: &mut OwnedPipe,
    stop_event: HANDLE,
) -> Result<bool, ServiceError> {
    if stop_event.is_null() || stop_event == INVALID_HANDLE_VALUE {
        return Err(ServiceError::protocol(
            "wait for RustNT pipe connection",
            "service stop event is invalid",
        ));
    }

    let event = create_io_event("create RustNT pipe connect event")?;
    let mut overlapped = new_overlapped(event.get());
    let connected = unsafe {
        // SAFETY: pipe and event are owned and live through the wait; overlapped remains
        // live until its pending connection is completed or cancelled.
        ConnectNamedPipe(pipe.get(), &mut overlapped)
    };
    if connected != 0 {
        pipe.mark_connected();
        return Ok(true);
    }

    let error = ServiceError::windows("connect RustNT service pipe");
    if error.code == Some(ERROR_PIPE_CONNECTED) {
        pipe.mark_connected();
        return Ok(true);
    }
    if error.code != Some(ERROR_IO_PENDING) {
        return Err(error);
    }

    match wait_overlapped_with_stop(
        pipe.get(),
        &overlapped,
        stop_event,
        "wait for RustNT pipe connection",
    )? {
        OverlappedWait::Completed(_) => {
            pipe.mark_connected();
            Ok(true)
        }
        OverlappedWait::Stopped => Ok(false),
        OverlappedWait::TimedOut => Ok(false),
    }
}

#[cfg(windows)]
pub(crate) fn run_pipe_server(stop_event: HANDLE) -> Result<(), ServiceError> {
    loop {
        let mut pipe = create_pipe_instance()?;
        if !wait_for_pipe_connection(&mut pipe, stop_event)? {
            return Ok(());
        }

        let mut request_buffer = vec![0u8; MAX_REQUEST_FRAME_SIZE];
        let response = match read_overlapped_pipe_message(
            pipe.get(),
            &mut request_buffer,
            "read RustNT pipe request",
            stop_event,
            INFINITE,
        ) {
            Ok(request_size) => match decode_request(&request_buffer[..request_size]) {
                Ok(request) => dispatch_request(request),
                Err(error) => request_error_response(error),
            },
            Err(PipeIoError::Stopped) => return Ok(()),
            Err(PipeIoError::Disconnected) => continue,
            Err(PipeIoError::TimedOut) => continue,
            Err(PipeIoError::FrameTooLarge) => request_error_response(ServiceError::protocol(
                "read RustNT pipe request",
                format!("Pipe frame exceeds {MAX_REQUEST_FRAME_SIZE} bytes"),
            )),
            Err(PipeIoError::Operation(error)) => return Err(error),
        };
        let response = encode_response(&response)?;
        match write_overlapped_pipe_message(
            pipe.get(),
            &response,
            "write RustNT pipe response",
            stop_event,
        ) {
            Ok(()) => {}
            Err(PipeIoError::Stopped) => return Ok(()),
            Err(PipeIoError::Disconnected) => continue,
            Err(PipeIoError::TimedOut) => continue,
            Err(PipeIoError::FrameTooLarge) => {
                return Err(ServiceError::protocol(
                    "write RustNT pipe response",
                    "response unexpectedly exceeded the Pipe frame limit",
                ));
            }
            Err(PipeIoError::Operation(error)) => return Err(error),
        }

        let mut acknowledgement = [0u8; RESPONSE_ACK_MAGIC.len()];
        match read_overlapped_pipe_message(
            pipe.get(),
            &mut acknowledgement,
            "read RustNT response acknowledgement",
            stop_event,
            PIPE_TIMEOUT_MS,
        ) {
            Ok(size) if acknowledgement[..size] == *RESPONSE_ACK_MAGIC => {}
            Ok(_)
            | Err(PipeIoError::Disconnected)
            | Err(PipeIoError::TimedOut)
            | Err(PipeIoError::FrameTooLarge) => {
                continue;
            }
            Err(PipeIoError::Stopped) => return Ok(()),
            Err(PipeIoError::Operation(error)) => return Err(error),
        }
    }
}

#[cfg(windows)]
struct ServiceRuntime {
    status_handle: AtomicPtr<std::ffi::c_void>,
    running: AtomicBool,
    callback_gate: CallbackGate,
}

#[cfg(windows)]
impl ServiceRuntime {
    fn new() -> Self {
        Self {
            status_handle: AtomicPtr::new(ptr::null_mut()),
            running: AtomicBool::new(false),
            callback_gate: CallbackGate::new(),
        }
    }
}

#[cfg(windows)]
struct CallbackGate {
    state: Mutex<CallbackState>,
    drained: Condvar,
}

#[cfg(windows)]
struct CallbackState {
    accepting: bool,
    active_callbacks: usize,
    stop_event: Option<OwnedHandle>,
}

#[cfg(windows)]
struct CallbackGuard<'a> {
    gate: &'a CallbackGate,
}

#[cfg(windows)]
impl CallbackGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(CallbackState {
                accepting: true,
                active_callbacks: 0,
                stop_event: None,
            }),
            drained: Condvar::new(),
        }
    }

    fn try_enter(&self) -> Option<CallbackGuard<'_>> {
        let mut state = self.state.lock().expect("callback gate mutex poisoned");
        if !state.accepting {
            return None;
        }
        state.active_callbacks += 1;
        Some(CallbackGuard { gate: self })
    }

    fn install_event(&self, event: OwnedHandle) -> HANDLE {
        let handle = event.get();
        let mut state = self.state.lock().expect("callback gate mutex poisoned");
        state.stop_event = Some(event);
        handle
    }

    fn stop_accepting(&self) {
        let mut state = self.state.lock().expect("callback gate mutex poisoned");
        state.accepting = false;
    }

    fn wait_for_drain(&self) {
        let mut state = self.state.lock().expect("callback gate mutex poisoned");
        while state.active_callbacks != 0 {
            state = self
                .drained
                .wait(state)
                .expect("callback gate mutex poisoned");
        }
    }

    fn release_event(&self) {
        let mut state = self.state.lock().expect("callback gate mutex poisoned");
        drop(state.stop_event.take());
    }

    fn shutdown(&self) {
        self.stop_accepting();
        self.wait_for_drain();
        self.release_event();
    }

    #[cfg(test)]
    fn active_callbacks(&self) -> usize {
        self.state
            .lock()
            .expect("callback gate mutex poisoned")
            .active_callbacks
    }
}

#[cfg(windows)]
impl<'a> CallbackGuard<'a> {
    fn stop_event(&self) -> Option<HANDLE> {
        self.gate
            .state
            .lock()
            .expect("callback gate mutex poisoned")
            .stop_event
            .as_ref()
            .map(OwnedHandle::get)
    }
}

#[cfg(windows)]
impl Drop for CallbackGuard<'_> {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("callback gate mutex poisoned");
        state.active_callbacks -= 1;
        if state.active_callbacks == 0 {
            self.gate.drained.notify_all();
        }
    }
}

#[cfg(windows)]
fn report_service_status(
    context: &ServiceRuntime,
    state: u32,
    failure: Option<&ServiceError>,
) -> Result<(), ServiceError> {
    let status_handle = context.status_handle.load(Ordering::Acquire);
    if status_handle.is_null() {
        return Err(ServiceError::protocol(
            "report service status",
            "service status handle is not initialized",
        ));
    }

    let (win32_exit_code, service_specific_exit_code) = match failure {
        Some(error) => (ERROR_SERVICE_SPECIFIC_ERROR, error.code.unwrap_or(1).max(1)),
        None => (NO_ERROR, NO_ERROR),
    };
    let controls_accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls_accepted,
        dwWin32ExitCode: win32_exit_code,
        dwServiceSpecificExitCode: service_specific_exit_code,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    let result = unsafe {
        // SAFETY: status_handle was returned by RegisterServiceCtrlHandlerExW and remains
        // registered until the service entry returns. `status` is live for this call.
        SetServiceStatus(status_handle, &status)
    };
    if result == 0 {
        Err(ServiceError::windows("report service status"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
/// The SCM invokes this callback only while the boxed `ServiceRuntime` remains alive.
/// The context owns the stop event, and this callback performs only atomic reads and
/// event signaling; it never frees or mutates the context's ownership state.
unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    context: *mut std::ffi::c_void,
) -> u32 {
    if context.is_null() {
        return ERROR_INVALID_HANDLE;
    }
    let context = &*context.cast::<ServiceRuntime>();
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            let Some(callback) = context.callback_gate.try_enter() else {
                return NO_ERROR;
            };
            if !context.running.load(Ordering::Acquire) {
                return NO_ERROR;
            }
            let Some(stop_event) = callback.stop_event() else {
                return ERROR_INVALID_HANDLE;
            };
            if SetEvent(stop_event) == 0 {
                GetLastError()
            } else {
                NO_ERROR
            }
        }
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

#[cfg(windows)]
fn run_service_main(context: &mut ServiceRuntime) -> Result<(), ServiceError> {
    report_service_status(context, SERVICE_START_PENDING, None)?;

    let stop_event = match create_io_event("create RustNT service stop event") {
        Ok(event) => event,
        Err(error) => {
            let _ = report_service_status(context, SERVICE_STOPPED, Some(&error));
            return Err(error);
        }
    };
    let stop_event = context.callback_gate.install_event(stop_event);
    context.running.store(true, Ordering::Release);

    if let Err(error) = report_service_status(context, SERVICE_RUNNING, None) {
        context.running.store(false, Ordering::Release);
        context.callback_gate.shutdown();
        let _ = report_service_status(context, SERVICE_STOPPED, Some(&error));
        return Err(error);
    }

    let pipe_result = run_pipe_server(stop_event);
    context.running.store(false, Ordering::Release);
    let pending_result = report_service_status(context, SERVICE_STOP_PENDING, None);

    // Close callback admission, drain callbacks that may still be signaling the event, and
    // drop the event owner while the same gate protects the callback's HANDLE lookup.
    context.callback_gate.shutdown();
    let failure = pipe_result.as_ref().err().or(pending_result.as_ref().err());
    let stopped_result = report_service_status(context, SERVICE_STOPPED, failure);

    match pipe_result {
        Err(error) => Err(error),
        Ok(()) => pending_result.and(stopped_result),
    }
}

#[cfg(windows)]
unsafe extern "system" fn service_main(
    _argument_count: u32,
    _arguments: *mut windows_sys::core::PWSTR,
) {
    let mut context = Box::new(ServiceRuntime::new());
    let context_pointer = (&mut *context as *mut ServiceRuntime).cast();
    let name = service_name();
    let status_handle = RegisterServiceCtrlHandlerExW(
        name.as_ptr(),
        Some(service_control_handler),
        context_pointer,
    );
    if status_handle.is_null() {
        eprintln!("rustnt-service handler registration failed");
        return;
    }
    context
        .status_handle
        .store(status_handle, Ordering::Release);

    if let Err(error) = run_service_main(&mut context) {
        eprintln!("rustnt-service host error: {error}");
    }
}

#[cfg(windows)]
pub fn run_service() -> Result<(), ServiceError> {
    let mut name = service_name();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    let result = unsafe {
        // SAFETY: `table` and its UTF-16 service name remain alive until the dispatcher
        // returns, and the final entry is the required null terminator.
        StartServiceCtrlDispatcherW(table.as_ptr())
    };
    if result == 0 {
        Err(ServiceError::windows("start service control dispatcher"))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn run_service() -> Result<(), ServiceError> {
    Err(ServiceError::protocol("run service", "Windows is required"))
}

// Keep the Task 04 server call graph type-checked until Task 05's service host
// becomes its runtime caller, without exposing a raw HANDLE outside this crate.
#[cfg(windows)]
const _: fn(HANDLE) -> Result<(), ServiceError> = run_pipe_server;

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Services::{
        SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    };

    use super::{
        decode_request, decode_response, encode_request, encode_response, Command, Request,
        Response, ServiceIdentity, ServiceState, MAX_PAYLOAD, MAX_RESPONSE_FRAME_SIZE,
        PROTOCOL_VERSION, RESPONSE_HEADER_SIZE, SERVICE_NAME, SERVICE_TRANSITIONS, STATUS_SUCCESS,
    };

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
        let payload =
            String::from_utf8(super::identity_payload(&identity)).expect("payload should be UTF-8");
        assert!(payload.contains("SERVICE=RustNTControl\n"));
        assert!(payload.contains("ACCOUNT=LocalSystem\n"));
        assert!(payload.contains("ACCOUNT_SID=S-1-5-18\n"));
        assert!(payload.contains("INTEGRITY=System\n"));
        assert!(payload.contains("ELEVATED=true\n"));
        assert!(payload.contains("PROTOCOL=1\n"));
        assert!(payload.contains("CAPABILITIES=ping,identity,capabilities\n"));
    }

    #[test]
    fn identity_payload_rejects_field_injection_characters() {
        let mut identity = ServiceIdentity {
            service: SERVICE_NAME.to_string(),
            account: "LocalSystem".to_string(),
            account_sid: "S-1-5-18".to_string(),
            integrity: "System".to_string(),
            elevated: true,
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec!["ping".to_string()],
        };
        identity.account = "LocalSystem\nFORGED=true".to_string();
        assert!(super::identity_payload(&identity).is_empty());

        identity.account = "LocalSystem".to_string();
        identity.capabilities = vec!["ping=forged".to_string()];
        assert!(super::identity_payload(&identity).is_empty());
    }

    #[test]
    fn dispatch_allows_only_task_03_commands() {
        assert_eq!(
            super::dispatch_command(Command::Ping),
            Response {
                status: STATUS_SUCCESS,
                payload: Vec::new(),
            }
        );
        assert!(super::dispatch_command(Command::Identity)
            .payload
            .starts_with(b"SERVICE=RustNTControl\n"));
        assert!(
            String::from_utf8(super::dispatch_command(Command::Capabilities).payload)
                .expect("capabilities should be UTF-8")
                .contains("ping,identity,capabilities")
        );
    }

    #[cfg(windows)]
    #[test]
    fn disconnected_pipe_errors_are_connection_local() {
        assert!(super::is_pipe_disconnect_code(Some(
            windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE
        )));
        assert!(super::is_pipe_disconnect_code(Some(
            windows_sys::Win32::Foundation::ERROR_NO_DATA
        )));
        assert!(!super::is_pipe_disconnect_code(Some(
            windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
        )));
    }

    #[cfg(windows)]
    #[test]
    fn pipe_server_entrypoint_is_available_to_the_service_host() {
        let _ = super::run_pipe_server
            as fn(windows_sys::Win32::Foundation::HANDLE) -> Result<(), super::ServiceError>;
    }

    #[cfg(windows)]
    #[test]
    fn callback_gate_drains_before_event_release() {
        let gate = super::CallbackGate::new();
        let callback = gate
            .try_enter()
            .expect("callbacks should initially be accepted");

        gate.stop_accepting();
        assert!(
            gate.try_enter().is_none(),
            "new callbacks must be rejected during shutdown"
        );

        drop(callback);
        gate.wait_for_drain();
        assert_eq!(gate.active_callbacks(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn token_pointer_validation_rejects_null_pointers() {
        let error = super::checked_pointer::<u8>(std::ptr::null(), "test token pointer")
            .expect_err("null token pointer should fail");
        assert_eq!(error.operation, "test token pointer");
        assert_eq!(error.code, None);
        assert!(error.message.contains("null pointer"));
    }

    #[test]
    fn service_state_mapping_preserves_known_states() {
        assert_eq!(
            super::map_service_state(SERVICE_STOPPED),
            super::ServiceState::Stopped
        );
        assert_eq!(
            super::map_service_state(SERVICE_START_PENDING),
            super::ServiceState::StartPending
        );
        assert_eq!(
            super::map_service_state(SERVICE_RUNNING),
            super::ServiceState::Running
        );
        assert_eq!(
            super::map_service_state(SERVICE_STOP_PENDING),
            super::ServiceState::StopPending
        );
        assert_eq!(
            super::map_service_state(999),
            super::ServiceState::Other(999)
        );
    }

    #[test]
    fn request_round_trip_preserves_command_and_payload() {
        let request = Request {
            command: Command::Identity,
            payload: Vec::new(),
        };
        let bytes = encode_request(&request).expect("request should encode");
        assert_eq!(
            decode_request(&bytes).expect("request should decode"),
            request
        );
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
    fn response_rejects_bad_magic() {
        let response = Response {
            status: 0,
            payload: Vec::new(),
        };
        let mut bytes = encode_response(&response).expect("response should encode");
        bytes[0] ^= 0xff;

        assert!(decode_response(&bytes).is_err());
    }

    #[test]
    fn response_rejects_bad_version() {
        let response = Response {
            status: 0,
            payload: Vec::new(),
        };
        let mut bytes = encode_response(&response).expect("response should encode");
        bytes[4] = 2;

        assert!(decode_response(&bytes).is_err());
    }

    #[test]
    fn response_rejects_unknown_or_invalid_declared_length() {
        let response = Response {
            status: 0,
            payload: Vec::new(),
        };
        let mut bytes = encode_response(&response).expect("response should encode");
        bytes[10..14].copy_from_slice(&((MAX_PAYLOAD as u32) + 1).to_le_bytes());
        assert!(decode_response(&bytes).is_err());

        let mut bytes = encode_response(&response).expect("response should encode");
        bytes[10..14].copy_from_slice(&1u32.to_le_bytes());
        assert!(decode_response(&bytes).is_err());
    }

    #[test]
    fn max_response_payload_round_trip_uses_response_frame_contract() {
        let response = Response {
            status: 7,
            payload: vec![0xa5; MAX_PAYLOAD],
        };
        let bytes = encode_response(&response).expect("maximum response should encode");

        assert_eq!(bytes.len(), MAX_RESPONSE_FRAME_SIZE);
        assert_eq!(MAX_RESPONSE_FRAME_SIZE, RESPONSE_HEADER_SIZE + MAX_PAYLOAD);
        assert_eq!(
            decode_response(&bytes).expect("maximum response should decode"),
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

    #[test]
    fn codec_rejects_trailing_bytes() {
        let request = Request {
            command: Command::Ping,
            payload: Vec::new(),
        };
        let mut bytes = encode_request(&request).expect("request should encode");
        bytes.push(0);
        assert!(decode_request(&bytes).is_err());

        let response = Response {
            status: 0,
            payload: Vec::new(),
        };
        let mut bytes = encode_response(&response).expect("response should encode");
        bytes.push(0);
        assert!(decode_response(&bytes).is_err());
    }
}
