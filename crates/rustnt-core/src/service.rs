use std::fmt;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_SERVICE_ALREADY_RUNNING,
    ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_EXISTS, ERROR_SERVICE_NOT_ACTIVE, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenElevation,
    TokenIntegrityLevel, TokenUser, TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    TOKEN_USER,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::DELETE;
#[cfg(windows)]
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceConfigW, QueryServiceStatusEx, StartServiceW, QUERY_SERVICE_CONFIGW,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SC_STATUS_PROCESS_INFO,
    SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, SERVICE_QUERY_CONFIG,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_WIN32_OWN_PROCESS,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Services::{
        SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    };

    use super::{
        decode_request, decode_response, encode_request, encode_response, Command, Request,
        Response, ServiceIdentity, MAX_PAYLOAD, MAX_RESPONSE_FRAME_SIZE, PROTOCOL_VERSION,
        RESPONSE_HEADER_SIZE, SERVICE_NAME,
    };

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
