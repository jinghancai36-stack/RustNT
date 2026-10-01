use crate::config::ConfigPaths;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(not(windows))]
use std::sync::{MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(windows)]
use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, WAIT_ABANDONED_0, WAIT_FAILED, WAIT_OBJECT_0,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

pub const MAX_LOG_BYTES: usize = 8 * 1024;
pub const MAX_SESSION_BYTES: usize = 4 * 1024;
const SESSION_FORMAT_VERSION: u32 = 1;
const LOG_HEADER: &str = "format_version=1";
const MAX_EVENT_BYTES: usize = 511;
const MAX_SESSION_NONCE_BYTES: usize = 96;
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(not(windows))]
static SESSION_COORDINATOR: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(windows)]
const SESSION_COORDINATOR_NAME: &str = "Local\\RustNT.ShellRecovery.Session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupState {
    NotStarted,
    Starting,
    Running,
    RecoveryRequired,
    RecoveryRecorded,
    SnapshotUnavailable,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryEventKind {
    RecoveryRequired,
    Started,
    SnapshotSaved,
    SnapshotUnavailable,
    Completed,
}

impl RecoveryEventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RecoveryRequired => "recovery_required",
            Self::Started => "started",
            Self::SnapshotSaved => "snapshot_saved",
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLogError {
    pub operation: String,
    pub message: String,
}

impl RecoveryLogError {
    fn new(operation: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation: operation.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for RecoveryLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for RecoveryLogError {}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SessionFile {
    format_version: u32,
    pid: u32,
    started_at: u64,
    nonce: String,
    state: String,
}

impl SessionFile {
    fn is_valid(&self) -> bool {
        self.format_version == SESSION_FORMAT_VERSION
            && !self.nonce.is_empty()
            && self.nonce.len() <= MAX_SESSION_NONCE_BYTES
            && matches!(
                self.state.as_str(),
                "starting"
                    | "running"
                    | "recovery_required"
                    | "recovery_recorded"
                    | "snapshot_unavailable"
                    | "completed"
            )
    }
}

#[derive(Debug, Clone)]
pub struct ShellRecovery {
    paths: ConfigPaths,
    state: Arc<Mutex<StartupState>>,
}

#[derive(Debug, Clone)]
pub struct ShellSession {
    recovery: ShellRecovery,
    pid: u32,
    started_at: u64,
    nonce: String,
    path: PathBuf,
}

impl ShellRecovery {
    pub fn begin_startup(
        paths: &ConfigPaths,
        pid: u32,
        started_at: u64,
    ) -> Result<(Self, ShellSession), RecoveryLogError> {
        begin_startup(paths, pid, started_at)
    }

    pub fn startup_state(&self) -> StartupState {
        self.state
            .lock()
            .map(|state| *state)
            .unwrap_or(StartupState::NotStarted)
    }

    fn set_state(&self, state: StartupState) -> Result<(), RecoveryLogError> {
        self.state
            .lock()
            .map(|mut current| *current = state)
            .map_err(|_| {
                RecoveryLogError::new("update shell recovery state", "state lock poisoned")
            })
    }
}

impl ShellSession {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn mark_running(&self) -> Result<(), RecoveryLogError> {
        let _coordinator = lock_session_coordinator()?;
        if !self.owns_current_session_locked()? {
            return Ok(());
        }
        self.write_state_locked("running")?;
        self.set_state(StartupState::Running)
    }

    pub fn record_snapshot_result(
        &self,
        result: Result<(), String>,
    ) -> Result<(), RecoveryLogError> {
        let _coordinator = lock_session_coordinator()?;
        if !self.owns_current_session_locked()? {
            return Ok(());
        }
        match result {
            Ok(()) => append_event(
                &self.recovery.paths,
                self.pid,
                RecoveryEventKind::SnapshotSaved,
                "ok",
            )?,
            Err(error) => {
                let summary = sanitize(&error, MAX_EVENT_BYTES);
                append_event(
                    &self.recovery.paths,
                    self.pid,
                    RecoveryEventKind::SnapshotUnavailable,
                    &summary,
                )?;
                self.write_state_locked("snapshot_unavailable")?;
                self.set_state(StartupState::SnapshotUnavailable)?;
            }
        }
        Ok(())
    }

    pub fn complete_normal_exit(&self) -> Result<(), RecoveryLogError> {
        let _coordinator = lock_session_coordinator()?;
        if !self.owns_current_session_locked()? {
            return Ok(());
        }
        append_event(
            &self.recovery.paths,
            self.pid,
            RecoveryEventKind::Completed,
            "ok",
        )?;
        self.write_state_locked("completed")?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RecoveryLogError::new("remove shell session", error)),
        }
        self.set_state(StartupState::Completed)
    }

    fn owns_current_session_locked(&self) -> Result<bool, RecoveryLogError> {
        let Some(current) = read_session_file(&self.path)? else {
            return Ok(false);
        };
        Ok(current.format_version == SESSION_FORMAT_VERSION
            && current.pid == self.pid
            && current.started_at == self.started_at
            && current.nonce == self.nonce)
    }

    fn write_state_locked(&self, state: &str) -> Result<(), RecoveryLogError> {
        write_session(
            &self.path,
            &SessionFile {
                format_version: SESSION_FORMAT_VERSION,
                pid: self.pid,
                started_at: self.started_at,
                nonce: self.nonce.clone(),
                state: state.to_owned(),
            },
        )
    }

    fn set_state(&self, state: StartupState) -> Result<(), RecoveryLogError> {
        self.recovery.set_state(state)
    }
}

pub fn begin_startup(
    paths: &ConfigPaths,
    pid: u32,
    started_at: u64,
) -> Result<(ShellRecovery, ShellSession), RecoveryLogError> {
    std::fs::create_dir_all(&paths.root)
        .map_err(|error| RecoveryLogError::new("create shell recovery directory", error))?;

    let _coordinator = lock_session_coordinator()?;
    let previous = read_session_file(&paths.shell_session_file)?;
    let recovered = previous
        .as_ref()
        .is_some_and(|session| session.state != "completed");

    let recovery = ShellRecovery {
        paths: paths.clone(),
        state: Arc::new(Mutex::new(StartupState::Starting)),
    };
    if recovered {
        recovery.set_state(StartupState::RecoveryRequired)?;
        append_event(
            paths,
            pid,
            RecoveryEventKind::RecoveryRequired,
            "unfinished session",
        )?;
        recovery.set_state(StartupState::RecoveryRecorded)?;
    }

    let session = ShellSession {
        recovery: recovery.clone(),
        pid,
        started_at,
        nonce: new_session_nonce(pid),
        path: paths.shell_session_file.clone(),
    };
    session.write_state_locked("starting")?;
    append_event(paths, pid, RecoveryEventKind::Started, "ok")?;
    Ok((recovery, session))
}

pub fn record_snapshot_result(
    session: &ShellSession,
    result: Result<(), String>,
) -> Result<(), RecoveryLogError> {
    session.record_snapshot_result(result)
}

pub fn complete_normal_exit(session: &ShellSession) -> Result<(), RecoveryLogError> {
    session.complete_normal_exit()
}

enum SessionCoordinatorGuard {
    #[cfg(not(windows))]
    Process(MutexGuard<'static, ()>),
    #[cfg(windows)]
    Windows(HANDLE),
}

#[cfg(windows)]
impl Drop for SessionCoordinatorGuard {
    fn drop(&mut self) {
        let Self::Windows(handle) = self;
        // The guard owns the mutex after a successful wait. CloseHandle also releases the
        // process handle; ReleaseMutex makes the ownership transition explicit first.
        unsafe {
            let _ = ReleaseMutex(*handle);
            let _ = CloseHandle(*handle);
        }
    }
}

fn lock_session_coordinator() -> Result<SessionCoordinatorGuard, RecoveryLogError> {
    #[cfg(not(windows))]
    {
        SESSION_COORDINATOR
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map(SessionCoordinatorGuard::Process)
            .map_err(|_| RecoveryLogError::new("coordinate shell session", "session lock poisoned"))
    }

    #[cfg(windows)]
    {
        let name = OsStr::new(SESSION_COORDINATOR_NAME)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(RecoveryLogError::new(
                "coordinate shell session",
                format!("CreateMutexW failed with Win32 error {}", unsafe {
                    GetLastError()
                }),
            ));
        }
        let wait_result = unsafe { WaitForSingleObject(handle, u32::MAX) };
        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED_0 {
            let error = if wait_result == WAIT_FAILED {
                format!("WaitForSingleObject failed with Win32 error {}", unsafe {
                    GetLastError()
                })
            } else {
                format!("WaitForSingleObject returned {wait_result}")
            };
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(RecoveryLogError::new("coordinate shell session", error));
        }
        Ok(SessionCoordinatorGuard::Windows(handle))
    }
}

fn read_session_file(path: &Path) -> Result<Option<SessionFile>, RecoveryLogError> {
    let Some(bytes) = read_bounded_file(path, MAX_SESSION_BYTES, "read shell session")? else {
        return Ok(None);
    };
    let encoded = match String::from_utf8(bytes) {
        Ok(encoded) => encoded,
        Err(_) => return Ok(None),
    };
    let value = match toml::from_str::<toml::Value>(&encoded) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if let Some(version) = value
        .get("format_version")
        .and_then(toml::Value::as_integer)
    {
        if version != i64::from(SESSION_FORMAT_VERSION) {
            return Err(RecoveryLogError::new(
                "read shell session",
                format!("unsupported shell session format_version {version}"),
            ));
        }
    }
    let session = match toml::from_str::<SessionFile>(&encoded) {
        Ok(session) if session.is_valid() => session,
        Ok(_) | Err(_) => return Ok(None),
    };
    Ok(Some(session))
}

fn read_bounded_file(
    path: &Path,
    max_bytes: usize,
    operation: &str,
) -> Result<Option<Vec<u8>>, RecoveryLogError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RecoveryLogError::new(operation, error)),
    };
    let mut bytes = Vec::with_capacity(max_bytes + 1);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| RecoveryLogError::new(operation, error))?;
    if bytes.len() > max_bytes {
        return Err(RecoveryLogError::new(
            operation,
            format!("persistent file exceeds {max_bytes} bytes"),
        ));
    }
    Ok(Some(bytes))
}

fn append_event(
    paths: &ConfigPaths,
    pid: u32,
    kind: RecoveryEventKind,
    result: &str,
) -> Result<(), RecoveryLogError> {
    let event = format!(
        "timestamp={} pid={} event={} result={}\n",
        unix_timestamp(),
        pid,
        kind.as_str(),
        sanitize(result, MAX_EVENT_BYTES),
    );
    let event = sanitize(&event, MAX_EVENT_BYTES)
        .trim_end_matches('\n')
        .to_owned()
        + "\n";
    let mut lines = read_recovery_log(&paths.shell_recovery_log)?;
    lines.push(event.trim_end_matches('\n').to_owned());
    let mut retained_events = Vec::with_capacity(lines.len());
    let mut size = 0;
    size += LOG_HEADER.len() + 1;
    for line in lines.into_iter().skip(1).rev() {
        let line_size = line.len() + 1;
        if size + line_size > MAX_LOG_BYTES {
            break;
        }
        size += line_size;
        retained_events.push(line);
    }
    retained_events.reverse();
    let mut retained = Vec::with_capacity(retained_events.len() + 1);
    retained.push(LOG_HEADER.to_owned());
    retained.extend(retained_events);
    let encoded = retained.join("\n") + "\n";
    write_atomic(&paths.shell_recovery_log, encoded.as_bytes())
        .map_err(|error| RecoveryLogError::new("write shell recovery log", error))
}

fn read_recovery_log(path: &Path) -> Result<Vec<String>, RecoveryLogError> {
    let Some(bytes) = read_bounded_file(path, MAX_LOG_BYTES, "read shell recovery log")? else {
        return Ok(vec![LOG_HEADER.to_owned()]);
    };
    let encoded = String::from_utf8(bytes).map_err(|_| {
        RecoveryLogError::new(
            "read shell recovery log",
            "malformed recovery log: invalid UTF-8",
        )
    })?;
    if !encoded.ends_with('\n') {
        return Err(RecoveryLogError::new(
            "read shell recovery log",
            "malformed recovery log: incomplete final line",
        ));
    }
    let mut lines = encoded
        .strip_suffix('\n')
        .unwrap_or_default()
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if lines.is_empty() || lines[0] != LOG_HEADER {
        if let Some(version) = lines.first().and_then(|line| {
            line.strip_prefix("format_version=")
                .and_then(|value| value.parse::<u32>().ok())
        }) {
            return Err(RecoveryLogError::new(
                "read shell recovery log",
                format!("unsupported recovery log format_version {version}"),
            ));
        }
        return Err(RecoveryLogError::new(
            "read shell recovery log",
            "malformed recovery log: missing format_version header",
        ));
    }
    if lines.iter().skip(1).any(|line| !parse_event_line(line)) {
        return Err(RecoveryLogError::new(
            "read shell recovery log",
            "malformed recovery log: invalid event line",
        ));
    }
    if lines.len() == 1 && encoded != format!("{LOG_HEADER}\n") {
        return Err(RecoveryLogError::new(
            "read shell recovery log",
            "malformed recovery log: empty line",
        ));
    }
    Ok(std::mem::take(&mut lines))
}

fn parse_event_line(line: &str) -> bool {
    let mut fields = line.splitn(4, ' ');
    let Some(timestamp) = fields
        .next()
        .and_then(|field| field.strip_prefix("timestamp="))
    else {
        return false;
    };
    let Some(pid) = fields.next().and_then(|field| field.strip_prefix("pid=")) else {
        return false;
    };
    let Some(event) = fields.next().and_then(|field| field.strip_prefix("event=")) else {
        return false;
    };
    let Some(result) = fields
        .next()
        .and_then(|field| field.strip_prefix("result="))
    else {
        return false;
    };
    timestamp.parse::<u64>().is_ok()
        && pid.parse::<u32>().is_ok()
        && matches!(
            event,
            "recovery_required"
                | "started"
                | "snapshot_saved"
                | "snapshot_unavailable"
                | "completed"
        )
        && !result.is_empty()
        && !result.chars().any(char::is_control)
}

fn write_session(path: &Path, session: &SessionFile) -> Result<(), RecoveryLogError> {
    let encoded = toml::to_string(session)
        .map_err(|error| RecoveryLogError::new("serialize shell session", error))?;
    if encoded.len() > MAX_SESSION_BYTES {
        return Err(RecoveryLogError::new(
            "serialize shell session",
            format!("persistent file exceeds {MAX_SESSION_BYTES} bytes"),
        ));
    }
    write_atomic(path, encoded.as_bytes())
        .map_err(|error| RecoveryLogError::new("write shell session", error))
}

fn new_session_nonce(pid: u32) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = format!("{pid:x}-{timestamp:x}-{counter:x}");
    debug_assert!(nonce.len() <= MAX_SESSION_NONCE_BYTES);
    nonce
}

fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shell");
    let temporary = (0..16)
        .map(|_| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let counter = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            parent.join(format!(
                "{name}.{}.{}.{}.tmp",
                std::process::id(),
                timestamp,
                counter
            ))
        })
        .find(|path| !path.exists())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "temporary path collision",
            )
        })?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    let result = replace_file(&temporary, target);
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    if !target.exists() {
        return match std::fs::rename(temporary, target) {
            Ok(()) => Ok(()),
            Err(_) if target.exists() => replace_existing_file(temporary, target),
            Err(error) => Err(error),
        };
    }
    replace_existing_file(temporary, target)
}

#[cfg(windows)]
fn replace_existing_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            target_wide.as_ptr(),
            temporary_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn sanitize(value: &str, max_bytes: usize) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > max_bytes {
            break;
        }
        output.push(character);
    }
    output
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::config_paths_from_root;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("rustnt-shell-recovery-{name}-{unique}")))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_session_toml() -> &'static str {
        "format_version = 1\npid = 7\nstarted_at = 8\nnonce = 'fixture-nonce'\nstate = 'running'\n"
    }

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
        let log = std::fs::read_to_string(&paths.shell_recovery_log).unwrap();
        assert_eq!(log.matches("event=recovery_required").count(), 1);
    }

    #[test]
    fn snapshot_result_transitions_to_saved_or_unavailable() {
        let root = TestRoot::new("snapshot-result");
        let paths = config_paths_from_root(root.path().to_owned());
        let (recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        session.record_snapshot_result(Ok(())).unwrap();
        assert_eq!(recovery.startup_state(), StartupState::Starting);
        session
            .record_snapshot_result(Err("registry denied".to_owned()))
            .unwrap();
        assert_eq!(recovery.startup_state(), StartupState::SnapshotUnavailable);
    }

    #[test]
    fn normal_exit_logs_completed_then_removes_current_session() {
        let root = TestRoot::new("normal-exit");
        let paths = config_paths_from_root(root.path().to_owned());
        let (recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        session.mark_running().unwrap();
        session.complete_normal_exit().unwrap();
        assert_eq!(recovery.startup_state(), StartupState::Completed);
        assert!(!session.path().exists());
        assert!(std::fs::read_to_string(&paths.shell_recovery_log)
            .unwrap()
            .contains("event=completed"));
    }

    #[test]
    fn log_events_are_sanitized_and_bounded_to_complete_lines() {
        let root = TestRoot::new("bounded-log");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        for _ in 0..100 {
            session
                .record_snapshot_result(Err("bad\r\nvalue".to_owned()))
                .unwrap();
        }
        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert!(log.len() <= MAX_LOG_BYTES);
        assert!(log.starts_with(b"format_version=1\n"));
        assert!(log.ends_with(b"\n"));
        assert!(!log.contains(&b'\r'));
    }

    #[test]
    fn malformed_log_is_rejected_without_rewriting_existing_bytes() {
        let root = TestRoot::new("incomplete-log");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(
            &paths.shell_recovery_log,
            b"format_version=1\ntimestamp=1 pid=1 event=started result=ok\npartial-record",
        )
        .unwrap();

        let original = std::fs::read(&paths.shell_recovery_log).unwrap();
        let error = ShellRecovery::begin_startup(&paths, 1, 2)
            .expect_err("malformed log must block startup");
        assert!(error.message.contains("malformed"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
    }

    #[test]
    fn malformed_old_session_is_replaced_atomically() {
        let root = TestRoot::new("malformed-session");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_session_file, b"not toml\0").unwrap();
        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();
        assert!(session.path().exists());
        assert!(std::fs::read_to_string(&paths.shell_session_file)
            .unwrap()
            .contains("format_version"));
    }

    #[test]
    fn invalid_utf8_old_session_is_treated_as_malformed() {
        let root = TestRoot::new("invalid-utf8-session");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_session_file, [0xff, 0xfe, 0x00]).unwrap();

        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();
        assert!(session.path().exists());
        assert_ne!(
            std::fs::read(&paths.shell_session_file).unwrap(),
            [0xff, 0xfe, 0x00]
        );
        assert!(std::fs::read_to_string(&paths.shell_session_file)
            .unwrap()
            .contains("format_version"));
        let log = std::fs::read_to_string(&paths.shell_recovery_log).unwrap();
        assert!(!log.contains("event=recovery_required"));
    }

    #[test]
    fn unknown_session_state_is_treated_as_malformed() {
        let root = TestRoot::new("unknown-session-state");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(
            &paths.shell_session_file,
            "format_version = 1\npid = 7\nstarted_at = 8\nnonce = 'fixture-nonce'\nstate = 'future_state'\n",
        )
        .unwrap();

        let (recovery, _session) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();
        assert_eq!(recovery.startup_state(), StartupState::Starting);
        let log = std::fs::read_to_string(&paths.shell_recovery_log).unwrap();
        assert!(!log.contains("event=recovery_required"));
    }

    #[test]
    fn session_write_failure_is_returned() {
        let root = TestRoot::new("session-write-failure");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().join("child"));
        std::fs::write(root.path().join("child"), b"file").unwrap();
        assert!(ShellRecovery::begin_startup(&paths, 1, 2).is_err());
    }

    #[test]
    fn recovery_log_write_failure_is_returned() {
        let root = TestRoot::new("log-write-failure");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::create_dir(&paths.shell_recovery_log).unwrap();
        assert!(ShellRecovery::begin_startup(&paths, 1, 2).is_err());
    }

    #[test]
    fn stale_session_handle_does_not_remove_newer_session() {
        let root = TestRoot::new("stale-session");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_first_recovery, first) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        let (_second_recovery, second) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();
        first.complete_normal_exit().unwrap();
        assert!(second.path().exists());
        assert!(std::fs::read_to_string(second.path())
            .unwrap()
            .contains("pid = 3"));
    }

    #[test]
    fn unknown_session_format_version_is_rejected_without_rewriting_bytes() {
        let root = TestRoot::new("unknown-session-version");
        let paths = config_paths_from_root(root.path().to_owned());
        let original =
            b"format_version = 99\npid = 7\nstarted_at = 8\nnonce = 'future'\nstate = 'running'\n";
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_session_file, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("unknown session format must block startup");
        assert!(error.message.contains("unsupported shell session format"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
    }

    #[test]
    fn oversized_session_is_rejected_without_rewriting_bytes() {
        let root = TestRoot::new("oversized-session");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = vec![b'x'; MAX_SESSION_BYTES + 1];
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_session_file, &original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("oversized session must block startup");
        assert!(error.message.contains("exceeds"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
    }

    #[test]
    fn unknown_log_format_version_is_rejected_without_rewriting_bytes() {
        let root = TestRoot::new("unknown-log-version");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = b"format_version=99\ntimestamp=1 pid=1 event=started result=ok\n";
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_recovery_log, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("unknown log format must block startup");
        assert!(error.message.contains("unsupported recovery log format"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
    }

    #[test]
    fn invalid_utf8_log_is_rejected_without_rewriting_bytes() {
        let root = TestRoot::new("invalid-utf8-log");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = b"format_version=1\n\xff\n";
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_recovery_log, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("invalid UTF-8 log must block startup");
        assert!(error.message.contains("invalid UTF-8"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
    }

    #[test]
    fn same_pid_and_start_time_stale_handle_is_rejected_by_nonce() {
        let root = TestRoot::new("same-identity-stale-session");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_first_recovery, first) = ShellRecovery::begin_startup(&paths, 7, 8).unwrap();
        let (_second_recovery, second) = ShellRecovery::begin_startup(&paths, 7, 8).unwrap();

        assert_ne!(first.nonce, second.nonce);
        first.mark_running().unwrap();
        first.complete_normal_exit().unwrap();

        let current = read_session_file(&paths.shell_session_file)
            .unwrap()
            .unwrap();
        assert_eq!(current.nonce, second.nonce);
        assert_eq!(current.state, "starting");
    }

    #[cfg(windows)]
    #[test]
    fn named_session_coordinator_is_shared_across_processes() {
        use std::process::Command;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let root = TestRoot::new("named-coordinator");
        let ready = root.path().join("ready");
        let release = root.path().join("release");
        std::fs::create_dir_all(root.path()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "shell_recovery::tests::hold_named_session_coordinator",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RUSTNT_COORDINATOR_READY", &ready)
            .env("RUSTNT_COORDINATOR_RELEASE", &release)
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not acquire named coordinator");

        let (acquired, wait_for_acquire) = mpsc::channel();
        let release_for_waiter = release.clone();
        let waiter = thread::spawn(move || {
            let guard = lock_session_coordinator().unwrap();
            acquired.send(()).unwrap();
            while !release_for_waiter.exists() {
                thread::sleep(Duration::from_millis(10));
            }
            drop(guard);
        });
        thread::sleep(Duration::from_millis(100));
        assert!(wait_for_acquire.try_recv().is_err());
        std::fs::write(&release, b"release").unwrap();
        waiter.join().unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[cfg(windows)]
    #[test]
    fn hold_named_session_coordinator() {
        let Some(ready) = std::env::var_os("RUSTNT_COORDINATOR_READY") else {
            return;
        };
        let release = PathBuf::from(std::env::var_os("RUSTNT_COORDINATOR_RELEASE").unwrap());
        let _guard = lock_session_coordinator().unwrap();
        std::fs::write(ready, b"ready").unwrap();
        while !release.exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn stale_session_handle_does_not_change_newer_session_to_running() {
        let root = TestRoot::new("stale-running-session");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_first_recovery, first) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        let (_second_recovery, second) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();

        first.mark_running().unwrap();

        let current = std::fs::read_to_string(second.path()).unwrap();
        assert!(current.contains("pid = 3"));
        assert!(current.contains("started_at = 4"));
        assert!(current.contains("state = \"starting\""));
    }
}
