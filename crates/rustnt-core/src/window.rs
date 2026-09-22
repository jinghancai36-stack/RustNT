use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::slice;

use windows_sys::Win32::Foundation::{GetLastError, SetLastError, BOOL, HANDLE, HWND, LPARAM};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSActive, WTSClientName, WTSConnectQuery, WTSConnected, WTSDisconnected,
    WTSDomainName, WTSDown, WTSEnumerateSessionsW, WTSFreeMemory, WTSGetActiveConsoleSessionId,
    WTSIdle, WTSInit, WTSListen, WTSQuerySessionInformationW, WTSReset, WTSShadow, WTSUserName,
    WTS_CURRENT_SERVER_HANDLE,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

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

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Win32 { operation, code } => {
                write!(formatter, "{operation}: Windows error {code}")
            }
            Self::Enumeration(message) => write!(formatter, "window enumeration: {message}"),
            Self::InvalidData(message) => write!(formatter, "invalid window data: {message}"),
        }
    }
}

impl std::error::Error for WindowError {}

fn map_session_state(value: i32) -> SessionState {
    #[allow(non_upper_case_globals)]
    match value {
        WTSActive => SessionState::Active,
        WTSConnected => SessionState::Connected,
        WTSConnectQuery => SessionState::ConnectQuery,
        WTSShadow => SessionState::Shadow,
        WTSDisconnected => SessionState::Disconnected,
        WTSIdle => SessionState::Idle,
        WTSListen => SessionState::Listen,
        WTSReset => SessionState::Reset,
        WTSDown => SessionState::Down,
        WTSInit => SessionState::Init,
        other => SessionState::Other(other as u32),
    }
}

fn wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

#[cfg(test)]
fn wide_string_from_ptr(value: *const u16) -> Option<String> {
    if value.is_null() {
        return None;
    }

    let mut length = 0usize;
    // SAFETY: callers pass a pointer returned by a Win32 API that points to a
    // nul-terminated UTF-16 string valid for the duration of this call.
    unsafe {
        while *value.add(length) != 0 {
            length = length.checked_add(1)?;
        }
        Some(String::from_utf16_lossy(slice::from_raw_parts(
            value, length,
        )))
    }
}

pub fn format_hwnd(hwnd: usize) -> String {
    format!("0x{hwnd:016X}")
}

pub fn optional_text(value: Option<String>) -> String {
    value.unwrap_or_else(|| "N/A".to_string())
}

fn windows_error(operation: &str) -> WindowError {
    let code = unsafe {
        // SAFETY: GetLastError reads the calling thread's Win32 error value.
        GetLastError()
    };
    WindowError::Win32 {
        operation: operation.to_string(),
        code,
    }
}

fn current_session_id() -> Result<u32, WindowError> {
    let process_id = unsafe {
        // SAFETY: GetCurrentProcessId has no pointer or handle preconditions.
        GetCurrentProcessId()
    };
    let mut session_id = 0u32;
    let result = unsafe {
        // SAFETY: session_id points to writable storage for the duration of the call.
        ProcessIdToSessionId(process_id, &mut session_id)
    };
    if result == 0 {
        Err(windows_error("resolve current Session"))
    } else {
        Ok(session_id)
    }
}

fn desktop_name(desktop: HANDLE) -> Result<String, WindowError> {
    let mut bytes_needed = 0u32;
    let first_result = unsafe {
        // SAFETY: a null buffer and zero length request the required byte count.
        GetUserObjectInformationW(desktop, UOI_NAME, ptr::null_mut(), 0, &mut bytes_needed)
    };
    if first_result != 0 || bytes_needed == 0 {
        return Err(windows_error("query desktop name size"));
    }
    if !bytes_needed.is_multiple_of(std::mem::size_of::<u16>() as u32) {
        return Err(WindowError::InvalidData(
            "desktop name size is not UTF-16 aligned".to_string(),
        ));
    }

    let mut buffer = vec![0u16; (bytes_needed / 2) as usize];
    let mut bytes_written = bytes_needed;
    let result = unsafe {
        // SAFETY: buffer is writable and its byte length is described by bytes_written.
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            buffer.as_mut_ptr().cast::<c_void>(),
            bytes_needed,
            &mut bytes_written,
        )
    };
    if result == 0 {
        return Err(windows_error("query desktop name"));
    }
    if !bytes_written.is_multiple_of(std::mem::size_of::<u16>() as u32) {
        return Err(WindowError::InvalidData(
            "desktop name result is not UTF-16 aligned".to_string(),
        ));
    }
    let units = (bytes_written / 2) as usize;
    Ok(wide_string(&buffer[..units.min(buffer.len())]))
}

fn current_desktop_name() -> Result<String, WindowError> {
    let thread_id = unsafe {
        // SAFETY: GetCurrentThreadId has no pointer or handle preconditions.
        GetCurrentThreadId()
    };
    let desktop = unsafe {
        // SAFETY: the current thread ID is valid for this process; the returned desktop handle
        // is borrowed from the system and is not closed by RustNT.
        GetThreadDesktop(thread_id)
    };
    if desktop.is_null() {
        return Err(windows_error("resolve current desktop"));
    }
    desktop_name(desktop.cast())
}

fn thread_desktop_name(thread_id: u32) -> Option<String> {
    let desktop = unsafe {
        // SAFETY: the thread ID comes from GetWindowThreadProcessId and is used only for this
        // synchronous query; the returned desktop handle is borrowed and never closed here.
        GetThreadDesktop(thread_id)
    };
    if desktop.is_null() {
        return None;
    }
    desktop_name(desktop.cast()).ok()
}

fn read_window_text(hwnd: HWND) -> String {
    let length = unsafe {
        // SAFETY: hwnd comes from EnumWindows or a foreground-window query.
        GetWindowTextLengthW(hwnd)
    };
    if length <= 0 {
        return String::new();
    }

    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe {
        // SAFETY: buffer is writable and large enough for the requested text plus NUL.
        GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32)
    };
    if written <= 0 {
        String::new()
    } else {
        wide_string(&buffer[..written as usize])
    }
}

fn read_class_name(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; 256];
    let written = unsafe {
        // SAFETY: buffer is writable and its capacity is passed to the API.
        GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32)
    };
    if written <= 0 {
        String::new()
    } else {
        wide_string(&buffer[..written as usize])
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcessMetadata<'a> {
    name: Option<&'a str>,
    path: Option<&'a str>,
}

fn process_metadata<'a>(
    processes: &'a HashMap<u32, crate::ProcessInfo>,
    pid: u32,
) -> ProcessMetadata<'a> {
    processes
        .get(&pid)
        .map(|process| ProcessMetadata {
            name: Some(process.name.as_str()),
            path: process.path.as_deref(),
        })
        .unwrap_or(ProcessMetadata {
            name: None,
            path: None,
        })
}

enum WindowCollection {
    IdentityUnavailable,
    Filtered,
}

fn collect_window(
    hwnd: HWND,
    current_session_id: u32,
    current_desktop: &str,
    foreground_hwnd: HWND,
    processes: &HashMap<u32, crate::ProcessInfo>,
) -> Result<Option<WindowInfo>, WindowCollection> {
    let mut pid = 0u32;
    let thread_id = unsafe {
        // SAFETY: pid points to writable storage and hwnd is supplied by User32.
        GetWindowThreadProcessId(hwnd, &mut pid)
    };
    if thread_id == 0 || pid == 0 {
        return Err(WindowCollection::IdentityUnavailable);
    }

    let mut session_id = 0u32;
    let session_ok = unsafe {
        // SAFETY: session_id points to writable storage for the duration of the call.
        ProcessIdToSessionId(pid, &mut session_id)
    };
    if session_ok == 0 {
        return Err(WindowCollection::IdentityUnavailable);
    }
    if session_id != current_session_id {
        return Err(WindowCollection::Filtered);
    }

    let desktop_name =
        thread_desktop_name(thread_id).ok_or(WindowCollection::IdentityUnavailable)?;
    if desktop_name != current_desktop {
        return Err(WindowCollection::Filtered);
    }

    let metadata = process_metadata(processes, pid);
    Ok(Some(WindowInfo {
        hwnd: hwnd as usize,
        title: read_window_text(hwnd),
        class_name: read_class_name(hwnd),
        pid,
        process_name: metadata.name.map(str::to_string),
        process_path: metadata.path.map(str::to_string),
        thread_id,
        desktop_name: Some(desktop_name),
        session_id,
        visible: unsafe {
            // SAFETY: hwnd is a valid window handle supplied by User32.
            IsWindowVisible(hwnd) != 0
        },
        minimized: unsafe {
            // SAFETY: hwnd is a valid window handle supplied by User32.
            IsIconic(hwnd) != 0
        },
        foreground: hwnd == foreground_hwnd,
    }))
}

struct WindowEnumContext {
    current_session_id: u32,
    current_desktop: String,
    foreground_hwnd: HWND,
    processes: HashMap<u32, crate::ProcessInfo>,
    windows: Vec<WindowInfo>,
    skipped_windows: u32,
    callback_failed: bool,
}

unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let context = &mut *(lparam as *mut WindowEnumContext);
    match collect_window(
        hwnd,
        context.current_session_id,
        &context.current_desktop,
        context.foreground_hwnd,
        &context.processes,
    ) {
        Ok(Some(window)) => context.windows.push(window),
        Ok(None) => {}
        Err(WindowCollection::IdentityUnavailable) => {
            context.skipped_windows = context.skipped_windows.saturating_add(1)
        }
        Err(WindowCollection::Filtered) => {}
    }
    1
}

fn process_lookup() -> HashMap<u32, crate::ProcessInfo> {
    crate::list_processes()
        .unwrap_or_default()
        .into_iter()
        .map(|process| (process.pid, process))
        .collect()
}

pub fn enumerate_windows() -> Result<WindowSnapshot, WindowError> {
    let current_session_id = current_session_id()?;
    let current_desktop = current_desktop_name()?;
    let foreground_hwnd = unsafe {
        // SAFETY: GetForegroundWindow has no pointer or handle preconditions.
        GetForegroundWindow()
    };
    let mut context = WindowEnumContext {
        current_session_id,
        current_desktop: current_desktop.clone(),
        foreground_hwnd,
        processes: process_lookup(),
        windows: Vec::new(),
        skipped_windows: 0,
        callback_failed: false,
    };

    unsafe {
        // SAFETY: context remains alive and exclusively borrowed for the duration of EnumWindows.
        SetLastError(0);
        let result = EnumWindows(
            Some(enum_windows_callback),
            (&mut context as *mut _) as LPARAM,
        );
        if result == 0 && GetLastError() != 0 {
            context.callback_failed = true;
        }
    }
    if context.callback_failed {
        return Err(windows_error("enumerate top-level windows"));
    }

    Ok(WindowSnapshot {
        current_session_id,
        current_desktop,
        windows: context.windows,
        skipped_windows: context.skipped_windows,
    })
}

pub fn foreground_window() -> Result<Option<WindowInfo>, WindowError> {
    let hwnd = unsafe {
        // SAFETY: GetForegroundWindow has no pointer or handle preconditions.
        GetForegroundWindow()
    };
    if hwnd.is_null() {
        return Ok(None);
    }

    let current_session_id = current_session_id()?;
    let current_desktop = current_desktop_name()?;
    let processes = process_lookup();
    Ok(
        collect_window(hwnd, current_session_id, &current_desktop, hwnd, &processes)
            .ok()
            .flatten(),
    )
}

struct WtsMemory(*mut c_void);

impl Drop for WtsMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: the pointer is returned by a WTS allocation API.
                WTSFreeMemory(self.0);
            }
        }
    }
}

fn bounded_wide_string_from_ptr(
    value: *const u16,
    bytes_returned: u32,
) -> Result<Option<String>, WindowError> {
    if value.is_null() {
        return Ok(None);
    }
    if !bytes_returned.is_multiple_of(std::mem::size_of::<u16>() as u32) {
        return Err(WindowError::InvalidData(
            "WTS text size is not UTF-16 aligned".to_string(),
        ));
    }
    let units = (bytes_returned / 2) as usize;
    // SAFETY: WTS guarantees the returned buffer contains at least bytes_returned bytes.
    Ok(Some(unsafe {
        wide_string(slice::from_raw_parts(value, units))
    }))
}

fn query_session_text(session_id: u32, info_class: i32) -> Result<Option<String>, WindowError> {
    let mut buffer = ptr::null_mut();
    let mut bytes_returned = 0u32;
    let result = unsafe {
        // SAFETY: output pointers are writable and the session ID comes from WTS enumeration.
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session_id,
            info_class,
            &mut buffer,
            &mut bytes_returned,
        )
    };
    if result == 0 || buffer.is_null() {
        return Ok(None);
    }
    let memory = WtsMemory(buffer.cast());
    let value = bounded_wide_string_from_ptr(buffer, bytes_returned)?;
    drop(memory);
    Ok(value.filter(|text| !text.is_empty()))
}

pub fn list_sessions() -> Result<Vec<SessionInfo>, WindowError> {
    let current_session_id = current_session_id()?;
    let active_console_session = unsafe {
        // SAFETY: WTSGetActiveConsoleSessionId has no pointer or handle preconditions.
        WTSGetActiveConsoleSessionId()
    };
    let mut sessions_ptr = ptr::null_mut();
    let mut count = 0u32;
    let result = unsafe {
        // SAFETY: output pointers are writable for the duration of the call.
        WTSEnumerateSessionsW(
            WTS_CURRENT_SERVER_HANDLE,
            0,
            1,
            &mut sessions_ptr,
            &mut count,
        )
    };
    if result == 0 {
        return Err(windows_error("enumerate Windows sessions"));
    }
    if sessions_ptr.is_null() && count != 0 {
        return Err(WindowError::InvalidData(
            "session enumeration returned a null buffer".to_string(),
        ));
    }

    let memory = WtsMemory(sessions_ptr.cast());
    let sessions = if count == 0 {
        Vec::new()
    } else {
        let session_rows = unsafe {
            // SAFETY: WTS returned count contiguous WTS_SESSION_INFOW values.
            slice::from_raw_parts(sessions_ptr, count as usize)
        };
        let mut sessions = Vec::with_capacity(count as usize);
        for session in session_rows {
            sessions.push(SessionInfo {
                session_id: session.SessionId,
                state: map_session_state(session.State),
                username: query_session_text(session.SessionId, WTSUserName)?,
                domain: query_session_text(session.SessionId, WTSDomainName)?,
                client_name: query_session_text(session.SessionId, WTSClientName)?,
                is_current_session: session.SessionId == current_session_id,
                is_active_console: session.SessionId == active_console_session,
            });
        }
        sessions
    };
    drop(memory);

    let mut sessions = sessions;
    sessions.sort_by_key(|session| session.session_id);
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::{
        enumerate_windows, foreground_window, format_hwnd, list_sessions, map_session_state,
        optional_text, wide_string, wide_string_from_ptr, SessionState,
    };

    #[test]
    fn session_state_mapping_preserves_known_and_unknown_values() {
        assert_eq!(
            map_session_state(windows_sys::Win32::System::RemoteDesktop::WTSActive),
            SessionState::Active
        );
        assert_eq!(
            map_session_state(windows_sys::Win32::System::RemoteDesktop::WTSConnected),
            SessionState::Connected
        );
        assert_eq!(map_session_state(77), SessionState::Other(77));
    }

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
    fn wide_string_stops_at_the_first_nul() {
        assert_eq!(wide_string(&['R' as u16, 'N' as u16, 0, 'T' as u16]), "RN");
    }

    #[test]
    fn wide_string_from_ptr_reads_until_nul() {
        let value = ['R' as u16, 'N' as u16, 0];
        assert_eq!(wide_string_from_ptr(value.as_ptr()).as_deref(), Some("RN"));
        assert_eq!(wide_string_from_ptr(std::ptr::null()), None);
    }

    #[test]
    fn wts_text_rejects_odd_byte_counts() {
        let value = ['R' as u16, 0];
        assert!(matches!(
            super::bounded_wide_string_from_ptr(value.as_ptr(), 1),
            Err(super::WindowError::InvalidData(_))
        ));
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

    #[test]
    fn session_info_sort_is_ascending() {
        let mut sessions = [
            super::SessionInfo {
                session_id: 9,
                state: SessionState::Idle,
                username: None,
                domain: None,
                client_name: None,
                is_current_session: false,
                is_active_console: false,
            },
            super::SessionInfo {
                session_id: 1,
                state: SessionState::Active,
                username: None,
                domain: None,
                client_name: None,
                is_current_session: true,
                is_active_console: true,
            },
        ];
        sessions.sort_by_key(|session| session.session_id);
        assert_eq!(sessions[0].session_id, 1);
        assert_eq!(sessions[1].session_id, 9);
    }

    #[test]
    #[cfg(windows)]
    fn current_session_id_and_desktop_are_available() {
        assert!(super::current_session_id().expect("current session should resolve") > 0);
        assert!(!super::current_desktop_name()
            .expect("current desktop should resolve")
            .is_empty());
    }

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
    fn session_list_is_sorted_and_marks_current_session() {
        let sessions = list_sessions().expect("session enumeration should succeed");
        assert!(sessions
            .windows(2)
            .all(|pair| pair[0].session_id <= pair[1].session_id));
        assert_eq!(
            sessions
                .iter()
                .filter(|session| session.is_current_session)
                .count(),
            1
        );
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
}
