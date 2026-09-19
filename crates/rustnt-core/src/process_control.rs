use std::fmt;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessControlError {
    InvalidPayload(String),
    InvalidField(String),
    OversizedPayload,
    Status(ProcessStatus),
    Windows { operation: &'static str, code: u32 },
}

impl fmt::Display for ProcessControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayload(message) => write!(formatter, "invalid payload: {message}"),
            Self::InvalidField(field) => write!(formatter, "invalid field: {field}"),
            Self::OversizedPayload => write!(formatter, "payload exceeds configured limit"),
            Self::Status(status) => write!(formatter, "process operation status: {status:?}"),
            Self::Windows { operation, code } => {
                write!(formatter, "{operation}: Windows error {code}")
            }
        }
    }
}

impl std::error::Error for ProcessControlError {}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerSecurity {
    pub user_sid: String,
    pub elevated: bool,
    pub administrator: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationPolicyInput<'a> {
    pub target_pid: u32,
    pub service_pid: u32,
    pub expected_creation_time_100ns: u64,
    pub target_creation_time_100ns: u64,
    pub caller_sid: &'a str,
    pub target_owner_sid: &'a str,
    pub caller_elevated: bool,
    pub caller_administrator: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationRejection {
    CallerNotElevated,
    CallerNotAdmin,
    TargetProtected,
    TargetNotFound,
    PidReused,
    TargetNotOwned,
    AccessDenied,
}

impl TerminationRejection {
    pub(crate) fn status(self) -> ProcessStatus {
        match self {
            Self::CallerNotElevated => ProcessStatus::CallerNotElevated,
            Self::CallerNotAdmin => ProcessStatus::CallerNotAdmin,
            Self::TargetProtected => ProcessStatus::TargetProtected,
            Self::TargetNotFound => ProcessStatus::TargetNotFound,
            Self::PidReused => ProcessStatus::PidReused,
            Self::TargetNotOwned => ProcessStatus::TargetNotOwned,
            Self::AccessDenied => ProcessStatus::AccessDenied,
        }
    }
}

pub fn evaluate_termination_policy(
    input: &TerminationPolicyInput<'_>,
) -> Result<(), TerminationRejection> {
    if !input.caller_elevated {
        return Err(TerminationRejection::CallerNotElevated);
    }
    if !input.caller_administrator {
        return Err(TerminationRejection::CallerNotAdmin);
    }
    if input.target_pid == 0 || input.target_pid == 4 || input.target_pid == input.service_pid {
        return Err(TerminationRejection::TargetProtected);
    }
    if input.target_creation_time_100ns != input.expected_creation_time_100ns {
        return Err(TerminationRejection::PidReused);
    }
    if is_system_sid(input.target_owner_sid) {
        return Err(TerminationRejection::TargetProtected);
    }
    if input.target_owner_sid != input.caller_sid {
        return Err(TerminationRejection::TargetNotOwned);
    }
    Ok(())
}

pub fn is_system_sid(sid: &str) -> bool {
    matches!(sid, "S-1-5-18" | "S-1-5-19" | "S-1-5-20")
}

#[cfg(windows)]
use std::ffi::{c_void, OsString};
#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, FILETIME, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, TerminateProcess,
    WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

#[cfg(windows)]
const INSPECT_PROCESS_ACCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION;

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, ProcessControlError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(windows_error(operation))
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
                // SAFETY: this wrapper exclusively owns the Win32 handle and closes it once.
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
struct OwnedLocalBuffer(*mut c_void);

#[cfg(windows)]
impl Drop for OwnedLocalBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: the pointer was returned by a LocalAlloc-backed API and is freed once.
                LocalFree(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn windows_error(operation: &'static str) -> ProcessControlError {
    let code = unsafe {
        // SAFETY: GetLastError reads the current thread's Win32 error value.
        GetLastError()
    };
    ProcessControlError::Windows { operation, code }
}

#[cfg(windows)]
fn target_open_error(operation: &'static str) -> ProcessControlError {
    match windows_error(operation) {
        ProcessControlError::Windows {
            code: ERROR_ACCESS_DENIED,
            ..
        } => ProcessControlError::Status(ProcessStatus::TargetAccessDenied),
        _ => ProcessControlError::Status(ProcessStatus::TargetNotFound),
    }
}

#[cfg(windows)]
fn filetime_to_u64(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(windows)]
fn wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..length])
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn process_creation_time(handle: HANDLE) -> Result<u64, ProcessControlError> {
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let result = unsafe {
        // SAFETY: handle is valid for this call and all FILETIME pointers target writable locals.
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
    };
    if result == 0 {
        Err(windows_error("query process creation time"))
    } else {
        Ok(filetime_to_u64(creation))
    }
}

#[cfg(windows)]
fn process_path(handle: HANDLE) -> Option<String> {
    const PATH_BUFFER_LENGTH: usize = 32_768;
    let mut buffer = vec![0u16; PATH_BUFFER_LENGTH];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        // SAFETY: handle is valid and buffer/length describe writable storage for the call.
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
    };
    if result == 0 || length == 0 {
        None
    } else {
        Some(wide_string(&buffer[..length as usize]))
    }
}

#[cfg(windows)]
fn process_memory(handle: HANDLE) -> Option<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        // SAFETY: PROCESS_MEMORY_COUNTERS is an integer-only C structure and zero is valid.
        ..unsafe { std::mem::zeroed() }
    };
    let result = unsafe {
        // SAFETY: handle is valid and counters points to writable storage with cb initialized.
        GetProcessMemoryInfo(
            handle,
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    (result != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(windows)]
fn process_owner_sid(handle: HANDLE) -> Result<String, ProcessControlError> {
    let mut token = ptr::null_mut();
    let opened = unsafe {
        // SAFETY: handle is a valid process handle and token points to writable handle storage.
        OpenProcessToken(handle, TOKEN_QUERY, &mut token)
    };
    if opened == 0 {
        return Err(windows_error("open target process token"));
    }
    let token = OwnedHandle::new(token, "open target process token")?;

    let mut required = 0u32;
    let _ = unsafe {
        // SAFETY: null buffer and zero length request the required TokenUser size.
        GetTokenInformation(token.get(), TokenUser, ptr::null_mut(), 0, &mut required)
    };
    if required == 0 {
        return Err(windows_error("query target token user size"));
    }
    let mut buffer = vec![0u8; required as usize];
    let queried = unsafe {
        // SAFETY: buffer is sized by the preceding query and writable for required bytes.
        GetTokenInformation(
            token.get(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    if queried == 0 {
        return Err(windows_error("query target token user"));
    }
    let user = unsafe {
        // SAFETY: TokenUser data begins with TOKEN_USER in the returned buffer.
        &*buffer.as_ptr().cast::<TOKEN_USER>()
    };
    if user.User.Sid.is_null() {
        return Err(ProcessControlError::InvalidField(
            "target owner SID".to_string(),
        ));
    }
    let mut string_sid = ptr::null_mut();
    let converted = unsafe {
        // SAFETY: SID points into the live token buffer and string_sid is writable output storage.
        ConvertSidToStringSidW(user.User.Sid, &mut string_sid)
    };
    if converted == 0 || string_sid.is_null() {
        return Err(windows_error("convert target owner SID"));
    }
    let string_sid = OwnedLocalBuffer(string_sid.cast());
    let sid = unsafe {
        // SAFETY: ConvertSidToStringSidW returned a live nul-terminated LocalAlloc buffer.
        wide_string_from_ptr(string_sid.0.cast())
    };
    Ok(sid)
}

#[cfg(windows)]
unsafe fn wide_string_from_ptr(value: *const u16) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut result = Vec::new();
    let mut current = value;
    // SAFETY: caller guarantees readable UTF-16 storage through its terminating nul.
    while *current != 0 {
        result.push(*current);
        current = current.add(1);
    }
    String::from_utf16_lossy(&result)
}

#[cfg(windows)]
fn process_entry(pid: u32) -> Result<(String, u32), ProcessControlError> {
    let snapshot = unsafe {
        // SAFETY: the flags request a system process snapshot and no pointer is retained.
        CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    };
    let snapshot = OwnedHandle::new(snapshot, "create process snapshot")?;
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        // SAFETY: PROCESSENTRY32W is a fixed-layout integer/array structure.
        ..unsafe { std::mem::zeroed() }
    };
    let first = unsafe {
        // SAFETY: snapshot is valid and entry has the required dwSize.
        Process32FirstW(snapshot.get(), &mut entry)
    };
    if first == 0 {
        return Err(ProcessControlError::Status(ProcessStatus::TargetNotFound));
    }
    loop {
        if entry.th32ProcessID == pid {
            return Ok((wide_string(&entry.szExeFile), entry.cntThreads));
        }
        let next = unsafe {
            // SAFETY: snapshot and entry remain valid during enumeration.
            Process32NextW(snapshot.get(), &mut entry)
        };
        if next == 0 {
            return Err(ProcessControlError::Status(ProcessStatus::TargetNotFound));
        }
    }
}

#[cfg(windows)]
pub(crate) fn inspect_process(
    pid: u32,
    service_pid: u32,
) -> Result<ProcessInspection, ProcessControlError> {
    if pid == 0 || pid == 4 || pid == service_pid {
        return Err(ProcessControlError::Status(ProcessStatus::TargetProtected));
    }
    let handle = unsafe {
        // SAFETY: pid is used only as a process identifier; the returned handle is checked.
        OpenProcess(INSPECT_PROCESS_ACCESS, 0, pid)
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(target_open_error("open target process"));
    }
    let handle = OwnedHandle::new(handle, "open target process")?;
    let (image_name, thread_count) = process_entry(pid)?;
    let creation_time_100ns = process_creation_time(handle.get())?;
    let owner_sid = process_owner_sid(handle.get())?;
    let image_path = process_path(handle.get())
        .map(|path| sanitize_field(&path))
        .transpose()?;
    Ok(ProcessInspection {
        pid,
        creation_time_100ns,
        image_name: sanitize_field(&image_name)?,
        image_path,
        thread_count,
        memory_bytes: process_memory(handle.get()),
        owner_sid: sanitize_field(&owner_sid)?,
    })
}

#[cfg(windows)]
pub(crate) fn terminate_process(
    request: &ProcessTerminateRequest,
    caller: &CallerSecurity,
    service_pid: u32,
) -> Result<ProcessStatus, ProcessControlError> {
    if !caller.elevated {
        return Err(ProcessControlError::Status(
            ProcessStatus::CallerNotElevated,
        ));
    }
    if !caller.administrator {
        return Err(ProcessControlError::Status(ProcessStatus::CallerNotAdmin));
    }
    if request.pid == 0 || request.pid == 4 || request.pid == service_pid {
        return Err(ProcessControlError::Status(ProcessStatus::TargetProtected));
    }
    let handle = unsafe {
        // SAFETY: pid is used only as an identifier; the returned handle is checked and owned.
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            request.pid,
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        let error = windows_error("open target process");
        return Err(match error {
            ProcessControlError::Windows {
                code: ERROR_ACCESS_DENIED,
                ..
            } => ProcessControlError::Status(ProcessStatus::AccessDenied),
            _ => ProcessControlError::Status(ProcessStatus::TargetNotFound),
        });
    }
    let handle = OwnedHandle::new(handle, "open target process")?;
    let target_creation_time_100ns = process_creation_time(handle.get())?;
    let target_owner_sid = process_owner_sid(handle.get())?;
    let policy = TerminationPolicyInput {
        target_pid: request.pid,
        service_pid,
        expected_creation_time_100ns: request.expected_creation_time_100ns,
        target_creation_time_100ns,
        caller_sid: &caller.user_sid,
        target_owner_sid: &target_owner_sid,
        caller_elevated: caller.elevated,
        caller_administrator: caller.administrator,
    };
    if let Err(rejection) = evaluate_termination_policy(&policy) {
        return Err(ProcessControlError::Status(rejection.status()));
    }
    let terminated = unsafe {
        // SAFETY: handle is valid and owned; exit code 1 is fixed by the implementation.
        TerminateProcess(handle.get(), 1)
    };
    if terminated == 0 {
        return Err(ProcessControlError::Status(ProcessStatus::AccessDenied));
    }
    let wait = unsafe {
        // SAFETY: handle remains valid until the bounded wait returns.
        WaitForSingleObject(handle.get(), 2_000)
    };
    match wait {
        WAIT_OBJECT_0 => Ok(ProcessStatus::Terminated),
        WAIT_TIMEOUT => Ok(ProcessStatus::TerminatePending),
        WAIT_FAILED => Err(windows_error("wait for terminated process")),
        _ => Err(ProcessControlError::Windows {
            operation: "wait for terminated process",
            code: wait,
        }),
    }
}

pub fn encode_inspect_request(pid: u32) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(pid)?;
    Ok(pid.to_le_bytes().to_vec())
}

pub fn decode_inspect_request(
    payload: &[u8],
) -> Result<ProcessInspectRequest, ProcessControlError> {
    if payload.len() != 4 {
        return Err(ProcessControlError::InvalidPayload(
            "inspect request must contain exactly 4 bytes".to_string(),
        ));
    }
    let pid = u32::from_le_bytes(
        payload
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    validate_pid(pid)?;
    Ok(ProcessInspectRequest { pid })
}

pub fn encode_terminate_request(
    pid: u32,
    expected_creation_time_100ns: u64,
) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(pid)?;
    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&pid.to_le_bytes());
    payload.extend_from_slice(&expected_creation_time_100ns.to_le_bytes());
    Ok(payload)
}

pub fn decode_terminate_request(
    payload: &[u8],
) -> Result<ProcessTerminateRequest, ProcessControlError> {
    if payload.len() != 12 {
        return Err(ProcessControlError::InvalidPayload(
            "terminate request must contain exactly 12 bytes".to_string(),
        ));
    }
    let pid = u32::from_le_bytes(
        payload[..4]
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    let expected_creation_time_100ns = u64::from_le_bytes(
        payload[4..]
            .try_into()
            .expect("payload length was checked before conversion"),
    );
    validate_pid(pid)?;
    Ok(ProcessTerminateRequest {
        pid,
        expected_creation_time_100ns,
    })
}

pub fn encode_inspection_payload(
    inspection: &ProcessInspection,
    max_payload: usize,
) -> Result<Vec<u8>, ProcessControlError> {
    validate_pid(inspection.pid)?;
    let image_name = sanitize_field(&inspection.image_name)?;
    let image_path = inspection
        .image_path
        .as_deref()
        .map(sanitize_field)
        .transpose()?
        .unwrap_or_else(|| "N/A".to_string());
    let owner_sid = sanitize_field(&inspection.owner_sid)?;
    let payload = format!(
        "PID={}\nCREATION_TIME_100NS={}\nIMAGE_NAME={}\nIMAGE_PATH={}\nTHREADS={}\nMEMORY_BYTES={}\nOWNER_SID={}\n",
        inspection.pid,
        inspection.creation_time_100ns,
        image_name,
        image_path,
        inspection.thread_count,
        inspection
            .memory_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "N/A".to_string()),
        owner_sid,
    )
    .into_bytes();
    if payload.len() > max_payload {
        return Err(ProcessControlError::OversizedPayload);
    }
    Ok(payload)
}

pub fn decode_inspection_payload(payload: &[u8]) -> Result<ProcessInspection, ProcessControlError> {
    let text = std::str::from_utf8(payload).map_err(|_| {
        ProcessControlError::InvalidPayload("inspection payload must be UTF-8".to_string())
    })?;
    let text = text.strip_suffix('\n').ok_or_else(|| {
        ProcessControlError::InvalidPayload(
            "inspection payload must end with a newline".to_string(),
        )
    })?;
    let expected_keys = [
        "PID",
        "CREATION_TIME_100NS",
        "IMAGE_NAME",
        "IMAGE_PATH",
        "THREADS",
        "MEMORY_BYTES",
        "OWNER_SID",
    ];
    let mut values = Vec::with_capacity(expected_keys.len());
    for (line, expected_key) in text.split('\n').zip(expected_keys) {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            ProcessControlError::InvalidPayload("inspection field is malformed".to_string())
        })?;
        if key != expected_key {
            return Err(ProcessControlError::InvalidPayload(format!(
                "expected field {expected_key}"
            )));
        }
        values.push(value);
    }
    if text.split('\n').count() != expected_keys.len() {
        return Err(ProcessControlError::InvalidPayload(
            "inspection payload has an unexpected field count".to_string(),
        ));
    }

    let pid = parse_decimal(values[0], "PID")?;
    validate_pid(pid)?;
    let creation_time_100ns = parse_decimal(values[1], "CREATION_TIME_100NS")?;
    let image_name = sanitize_required(values[2], "IMAGE_NAME")?;
    let image_path = if values[3] == "N/A" {
        None
    } else {
        Some(sanitize_required(values[3], "IMAGE_PATH")?)
    };
    let thread_count = parse_decimal(values[4], "THREADS")?;
    let memory_bytes = if values[5] == "N/A" {
        None
    } else {
        Some(parse_decimal(values[5], "MEMORY_BYTES")?)
    };
    let owner_sid = sanitize_required(values[6], "OWNER_SID")?;

    Ok(ProcessInspection {
        pid,
        creation_time_100ns,
        image_name,
        image_path,
        thread_count,
        memory_bytes,
        owner_sid,
    })
}

pub fn encode_status_payload(status: ProcessStatus) -> Vec<u8> {
    format!("STATUS={}\n", status_name(status)).into_bytes()
}

pub fn decode_status_payload(payload: &[u8]) -> Result<ProcessStatus, ProcessControlError> {
    let text = std::str::from_utf8(payload).map_err(|_| {
        ProcessControlError::InvalidPayload("status payload must be UTF-8".to_string())
    })?;
    let value = text
        .strip_prefix("STATUS=")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| {
            ProcessControlError::InvalidPayload("status payload is malformed".to_string())
        })?;
    status_from_name(value).ok_or_else(|| {
        ProcessControlError::InvalidPayload("status payload contains an unknown status".to_string())
    })
}

pub fn sanitize_field(value: &str) -> Result<String, ProcessControlError> {
    if value.is_empty()
        || value
            .chars()
            .any(|character| character == '=' || character.is_control())
    {
        return Err(ProcessControlError::InvalidField(
            "field contains a delimiter or control character".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn validate_pid(pid: u32) -> Result<(), ProcessControlError> {
    if pid == 0 {
        Err(ProcessControlError::InvalidPayload(
            "PID must be greater than zero".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn sanitize_required(value: &str, field: &str) -> Result<String, ProcessControlError> {
    sanitize_field(value).map_err(|_| ProcessControlError::InvalidField(field.to_string()))
}

fn parse_decimal<T>(value: &str, field: &str) -> Result<T, ProcessControlError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| ProcessControlError::InvalidField(field.to_string()))
}

fn status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Terminated => "TERMINATED",
        ProcessStatus::TerminatePending => "TERMINATE_PENDING",
        ProcessStatus::TargetNotFound => "TARGET_NOT_FOUND",
        ProcessStatus::TargetAccessDenied => "TARGET_ACCESS_DENIED",
        ProcessStatus::PidReused => "PID_REUSED",
        ProcessStatus::TargetNotOwned => "TARGET_NOT_OWNED",
        ProcessStatus::TargetProtected => "TARGET_PROTECTED",
        ProcessStatus::CallerNotElevated => "CALLER_NOT_ELEVATED",
        ProcessStatus::CallerNotAdmin => "CALLER_NOT_ADMIN",
        ProcessStatus::AccessDenied => "ACCESS_DENIED",
    }
}

fn status_from_name(name: &str) -> Option<ProcessStatus> {
    Some(match name {
        "TERMINATED" => ProcessStatus::Terminated,
        "TERMINATE_PENDING" => ProcessStatus::TerminatePending,
        "TARGET_NOT_FOUND" => ProcessStatus::TargetNotFound,
        "TARGET_ACCESS_DENIED" => ProcessStatus::TargetAccessDenied,
        "PID_REUSED" => ProcessStatus::PidReused,
        "TARGET_NOT_OWNED" => ProcessStatus::TargetNotOwned,
        "TARGET_PROTECTED" => ProcessStatus::TargetProtected,
        "CALLER_NOT_ELEVATED" => ProcessStatus::CallerNotElevated,
        "CALLER_NOT_ADMIN" => ProcessStatus::CallerNotAdmin,
        "ACCESS_DENIED" => ProcessStatus::AccessDenied,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode_inspect_request, decode_inspection_payload, decode_status_payload,
        decode_terminate_request, encode_inspect_request, encode_inspection_payload,
        encode_status_payload, encode_terminate_request, evaluate_termination_policy,
        ProcessInspection, ProcessStatus, TerminationPolicyInput, TerminationRejection,
    };

    fn sample_inspection(image_name: &str) -> ProcessInspection {
        ProcessInspection {
            pid: 42,
            creation_time_100ns: 1234,
            image_name: image_name.to_string(),
            image_path: Some(r"C:\Apps\demo.exe".to_string()),
            thread_count: 3,
            memory_bytes: Some(4096),
            owner_sid: "S-1-5-21-100-200-300-1001".to_string(),
        }
    }

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
            vec![0x44, 0x33, 0x22, 0x11, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
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
    fn inspection_payload_round_trips_allowed_fields() {
        let inspection = sample_inspection("demo.exe");
        let payload =
            encode_inspection_payload(&inspection, 4096).expect("inspection should encode");
        assert_eq!(
            decode_inspection_payload(&payload).expect("inspection should decode"),
            inspection
        );
    }

    #[test]
    fn status_payload_round_trips_fixed_status_names() {
        for status in [
            ProcessStatus::Terminated,
            ProcessStatus::TerminatePending,
            ProcessStatus::TargetNotFound,
            ProcessStatus::TargetAccessDenied,
            ProcessStatus::PidReused,
            ProcessStatus::TargetNotOwned,
            ProcessStatus::TargetProtected,
            ProcessStatus::CallerNotElevated,
            ProcessStatus::CallerNotAdmin,
            ProcessStatus::AccessDenied,
        ] {
            assert_eq!(
                decode_status_payload(&encode_status_payload(status))
                    .expect("status should decode"),
                status
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn inspect_uses_query_only_process_access() {
        assert_eq!(
            super::INSPECT_PROCESS_ACCESS,
            windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION
        );
    }

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

    #[test]
    fn termination_policy_is_system_sid_exact() {
        assert!(super::is_system_sid("S-1-5-18"));
        assert!(super::is_system_sid("S-1-5-19"));
        assert!(super::is_system_sid("S-1-5-20"));
        assert!(!super::is_system_sid("S-1-5-21-user"));
    }

    #[cfg(windows)]
    #[test]
    fn inspect_process_returns_creation_owner_and_image_metadata() {
        let inspection = super::inspect_process(std::process::id(), u32::MAX)
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
            super::inspect_process(0, u32::MAX).expect_err("PID 0 must be rejected"),
            super::ProcessControlError::Status(ProcessStatus::TargetProtected)
        );
        assert_eq!(
            super::inspect_process(4, u32::MAX).expect_err("PID 4 must be rejected"),
            super::ProcessControlError::Status(ProcessStatus::TargetProtected)
        );
    }

    #[cfg(windows)]
    #[test]
    fn terminate_process_rejects_unauthorized_callers_before_opening_target() {
        let request = super::ProcessTerminateRequest {
            pid: u32::MAX,
            expected_creation_time_100ns: 1,
        };
        let caller = super::CallerSecurity {
            user_sid: "S-1-5-21-user".to_string(),
            elevated: false,
            administrator: false,
        };
        assert_eq!(
            super::terminate_process(&request, &caller, u32::MAX - 1)
                .expect_err("unauthorized termination must be rejected"),
            super::ProcessControlError::Status(ProcessStatus::CallerNotElevated)
        );
    }
}
