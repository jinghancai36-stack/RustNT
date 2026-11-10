use crate::config::ConfigPaths;
use crate::notifications::NotificationKind;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(not(windows))]
use std::sync::{MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

/// One initial replacement attempt plus at most two bounded retries. Retrying
/// is confined to the replacement step: the read-modify-write of `append_event`
/// is never re-entered, so an event can never be appended twice.
const MAX_REPLACE_ATTEMPTS: u32 = 3;
/// Short backoff. The cross-process session coordinator is held for the whole
/// pipeline, so long sleeps would stall every other writer in the system.
const REPLACE_RETRY_BACKOFF: Duration = Duration::from_millis(1);
/// Bounded number of fresh unique names when `create_new` loses a race.
const MAX_TEMPORARY_NAME_ATTEMPTS: usize = 16;

// Win32 status codes (winerror.h). They are only interpreted for errors raised
// by the replacement step; real non-Windows rename errors use the errno
// namespace and therefore fall through to `Permanent`, which preserves both
// files and reports the original error. Deterministic injection tests may
// supply these codes on any platform.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_ALREADY_EXISTS: i32 = 183;
const ERROR_UNABLE_TO_REMOVE_REPLACED: i32 = 1175;
const ERROR_UNABLE_TO_MOVE_REPLACEMENT: i32 = 1176;
const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupNotice {
    pub kind: NotificationKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupOutcome {
    SnapshotUnavailable(String),
}

pub fn startup_notice(outcome: StartupOutcome) -> Option<StartupNotice> {
    match outcome {
        StartupOutcome::SnapshotUnavailable(summary) => Some(StartupNotice {
            kind: NotificationKind::Warning,
            message: sanitize(&format!("Shell snapshot unavailable: {summary}"), 160),
        }),
    }
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
                let summary = if summary.is_empty() {
                    "unspecified snapshot error"
                } else {
                    summary.as_str()
                };
                append_event(
                    &self.recovery.paths,
                    self.pid,
                    RecoveryEventKind::SnapshotUnavailable,
                    summary,
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
    let encoded = String::from_utf8(bytes).map_err(|_| {
        RecoveryLogError::new(
            "read shell session",
            "malformed shell session: invalid UTF-8",
        )
    })?;
    let value = toml::from_str::<toml::Value>(&encoded).map_err(|error| {
        RecoveryLogError::new(
            "read shell session",
            format!("malformed shell session: invalid TOML: {error}"),
        )
    })?;
    let version_value = value.get("format_version").ok_or_else(|| {
        RecoveryLogError::new(
            "read shell session",
            "malformed shell session: missing format_version",
        )
    })?;
    let version = version_value.as_integer().ok_or_else(|| {
        RecoveryLogError::new(
            "read shell session",
            "malformed shell session: invalid format_version",
        )
    })?;
    if version != i64::from(SESSION_FORMAT_VERSION) {
        return Err(RecoveryLogError::new(
            "read shell session",
            format!("unsupported shell session format_version {version}"),
        ));
    }

    let session = toml::from_str::<SessionFile>(&encoded).map_err(|error| {
        RecoveryLogError::new(
            "read shell session",
            format!("malformed shell session: invalid fields: {error}"),
        )
    })?;
    if !session.is_valid() {
        return Err(RecoveryLogError::new(
            "read shell session",
            "malformed shell session: invalid fields",
        ));
    }
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
    append_event_with_replacer(paths, pid, kind, result, replace_file)
}

/// Injection seam for `append_event`. Only the replacement step is injectable,
/// so the read-modify-write still happens exactly once per call.
fn append_event_with_replacer<F>(
    paths: &ConfigPaths,
    pid: u32,
    kind: RecoveryEventKind,
    result: &str,
    replacer: F,
) -> Result<(), RecoveryLogError>
where
    F: Fn(&Path, &Path) -> Result<(), ReplaceStepError>,
{
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
    write_atomic_with_replacer(&paths.shell_recovery_log, encoded.as_bytes(), replacer)
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

// ---------------------------------------------------------------------------
// Atomic replacement policy (Task 01B.5B Gate B)
//
// Windows replacement now uses ONE explicit Win32 operation that is safe
// whether the target already exists or not:
//
//     MoveFileExW(temporary, target, MOVEFILE_REPLACE_EXISTING)
//
// * Target absent  -> the temporary file is atomically renamed into place. On
//   failure the target stays absent and the temporary file is the only complete
//   copy, so it is preserved as a recovery artifact instead of being deleted.
// * Target present -> the OS atomically replaces it as part of the move. We
//   never delete the target first, and no backup file is created (equivalent
//   to a NULL backup in ReplaceFileW terms), so there is no third state to
//   reconcile.
// * Failure        -> both files keep their original names and contents;
//   MoveFileExW has no documented partial-failure states. Retrying is limited
//   to this step, bounded, and only allowed while both files are intact.
//
// The previous implementation chose between `std::fs::rename` and
// `ReplaceFileW` using a `target.exists()` metadata check. That check and the
// following operation were not atomic (TOCTOU), and `ReplaceFileW` has
// documented partial-failure states (1175/1176/1177) in which the replaced
// file can disappear while the replacement file holds the only complete
// content. Moving to a single explicit operation removes both the check/action
// window and that class of flaky failures at the API boundary. The 1175/1176/
// 1177 classification below is retained as defensive policy and is verified
// deterministically through the injection seam; it is never exercised
// speculatively and never turns a failure into success.
//
// Only the replacement step is injectable. `append_event` still reads the old
// log once, appends exactly one event, and encodes exactly once per call.
// ---------------------------------------------------------------------------

/// Which step of the write pipeline reported a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStage {
    CreateTemporary,
    Install,
    /// Injection-only: models the legacy `ReplaceFileW` call shape so the
    /// 1175/1176/1177 policy stays covered even though production no longer
    /// calls that API.
    #[cfg_attr(not(test), allow(dead_code))]
    ReplaceExisting,
}

impl WriteStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::CreateTemporary => "CreateTemporary",
            Self::Install => "Install(MoveFileExW)",
            Self::ReplaceExisting => "ReplaceExisting(ReplaceFileW-shaped)",
        }
    }
}

#[derive(Debug)]
struct ReplaceStepError {
    stage: WriteStage,
    source: std::io::Error,
}

impl ReplaceStepError {
    fn new(stage: WriteStage, source: std::io::Error) -> Self {
        Self { stage, source }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    /// Both files are intact; a bounded retry of the SAME file pair is safe.
    Transient,
    /// On-disk state may have changed. Never retry, never auto-recover.
    StateChanged,
    /// No evidence of a transient condition; preserve evidence and report.
    Permanent,
    /// `create_new` lost a name race; pick a different unique temporary name.
    RetryTemporaryName,
}

impl FailureClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "Transient",
            Self::StateChanged => "StateChanged",
            Self::Permanent => "Permanent",
            Self::RetryTemporaryName => "RetryTemporaryName",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileState {
    target_exists: bool,
    temporary_exists: bool,
    target_readonly: bool,
    /// Target is readable, non-empty, newline-terminated and free of CR: the
    /// last verifiably complete log/session still exists on disk.
    target_verifiable: bool,
}

fn capture_file_state(temporary: &Path, target: &Path) -> FileState {
    let target_exists = target.exists();
    FileState {
        target_exists,
        temporary_exists: temporary.exists(),
        target_readonly: std::fs::metadata(target)
            .map(|metadata| metadata.permissions().readonly())
            .unwrap_or(false),
        target_verifiable: target_is_verifiable_complete(target),
    }
}

fn target_is_verifiable_complete(target: &Path) -> bool {
    let Ok(bytes) = std::fs::read(target) else {
        return false;
    };
    !bytes.is_empty()
        && bytes.len() <= MAX_LOG_BYTES
        && bytes.ends_with(b"\n")
        && !bytes.contains(&b'\r')
}

/// Classify by BOTH the operation stage and the observed on-disk state.
/// 32 and 5 are deliberately not blanket rules: a sharing violation is only
/// retryable while both files survive, and an access-denied at the install
/// stage can be a momentary lock (bounded retry) while the same code at the
/// legacy ReplaceFileW stage is a permanent ACL / read-only condition.
fn classify_replace_failure(
    stage: WriteStage,
    error: &std::io::Error,
    state: &FileState,
) -> FailureClass {
    if stage == WriteStage::CreateTemporary {
        return if error.kind() == std::io::ErrorKind::AlreadyExists
            || error.raw_os_error() == Some(ERROR_ALREADY_EXISTS)
        {
            FailureClass::RetryTemporaryName
        } else {
            FailureClass::Permanent
        };
    }
    match error.raw_os_error() {
        // 1176 / 1177: the replaced file may be gone or renamed and the
        // replacement may have inherited streams/attributes. Never retry.
        Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT) | Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2) => {
            FailureClass::StateChanged
        }
        // 1175: "the replaced and replacement files retain their original file
        // names". 32: a third party briefly holds a handle. Both are only safe
        // to retry while both files are intact.
        Some(ERROR_UNABLE_TO_REMOVE_REPLACED) | Some(ERROR_SHARING_VIOLATION) => {
            if state.target_exists && state.temporary_exists {
                FailureClass::Transient
            } else {
                FailureClass::StateChanged
            }
        }
        Some(ERROR_ACCESS_DENIED) => {
            if stage == WriteStage::Install
                && state.target_exists
                && state.temporary_exists
                && !state.target_readonly
            {
                FailureClass::Transient
            } else {
                FailureClass::Permanent
            }
        }
        _ => FailureClass::Permanent,
    }
}

fn describe_file_state(state: &FileState) -> &'static str {
    match (state.target_exists, state.temporary_exists) {
        (true, true) => "target retains the previous complete file; temporary file holds the new complete content",
        (false, true) => "target is missing; temporary file is the only complete copy and is preserved",
        (true, false) => "target retains the previous complete file; temporary file is gone",
        (false, false) => "neither target nor temporary file is present",
    }
}

fn partial_failure_note(class: FailureClass) -> &'static str {
    match class {
        FailureClass::StateChanged => "possible partial-failure states: target may be gone (1176 with NULL backup) or renamed (1177); temporary file may hold the only complete content; no backup file is created; no automatic recovery is attempted",
        FailureClass::Transient => "both files retained their original names; no backup file is created",
        _ => "no backup file is created; no automatic recovery is attempted",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupAction {
    NothingToRemove,
    RemovedTemporaryAfterVerifiedTarget,
    PreservedTemporaryAsOnlyCompleteArtifact,
    PreservedBothAsEvidence,
}

impl CleanupAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::NothingToRemove => "nothing to remove (temporary file is gone)",
            Self::RemovedTemporaryAfterVerifiedTarget => {
                "removed temporary file only after verifying the old target is complete"
            }
            Self::PreservedTemporaryAsOnlyCompleteArtifact => {
                "kept temporary file as the only complete recovery artifact"
            }
            Self::PreservedBothAsEvidence => "kept both files as evidence; no automatic recovery",
        }
    }
}

/// State-aware cleanup. The unconditional `remove_file(temporary)` on every
/// failure is gone: the temporary file is deleted only when the old target is
/// verifiably complete AND the failure was a bounded-retry exhaustion. In every
/// ambiguous or partial-failure state the artifacts are preserved so that a
/// human (not this process) can decide how to recover.
fn cleanup_after_failed_replace(
    temporary: &Path,
    state: &FileState,
    class: FailureClass,
) -> CleanupAction {
    if !state.temporary_exists {
        return CleanupAction::NothingToRemove;
    }
    if !state.target_exists {
        return CleanupAction::PreservedTemporaryAsOnlyCompleteArtifact;
    }
    if class == FailureClass::Transient && state.target_verifiable {
        let _ = std::fs::remove_file(temporary);
        return CleanupAction::RemovedTemporaryAfterVerifiedTarget;
    }
    CleanupAction::PreservedBothAsEvidence
}

/// Rich failure returned by the atomic write pipeline. `RecoveryLogError`
/// (the public error type) is unchanged; its `message` now carries the stage,
/// classification, attempt budget, exact file paths and observable state.
#[derive(Debug)]
struct AtomicWriteError {
    stage: WriteStage,
    class: FailureClass,
    source: std::io::Error,
    attempts: u32,
    target: PathBuf,
    temporary: PathBuf,
    state: FileState,
    cleanup: CleanupAction,
}

impl AtomicWriteError {
    fn for_create_stage(
        source: std::io::Error,
        target: &Path,
        temporary: &Path,
    ) -> AtomicWriteError {
        let state = capture_file_state(temporary, target);
        AtomicWriteError {
            stage: WriteStage::CreateTemporary,
            class: FailureClass::Permanent,
            source,
            attempts: 0,
            target: target.to_owned(),
            temporary: temporary.to_owned(),
            state,
            cleanup: CleanupAction::NothingToRemove,
        }
    }
}

impl fmt::Display for AtomicWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "stage={} win32={:?} classification={} attempts={}/{}; target={} (exists={}); temporary={} (exists={}); backup=none; {}; {}; cleanup={}; os error: {}",
            self.stage.as_str(),
            self.source.raw_os_error(),
            self.class.as_str(),
            self.attempts,
            MAX_REPLACE_ATTEMPTS,
            self.target.display(),
            self.state.target_exists,
            self.temporary.display(),
            self.state.temporary_exists,
            describe_file_state(&self.state),
            partial_failure_note(self.class),
            self.cleanup.as_str(),
            self.source,
        )
    }
}

impl std::error::Error for AtomicWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn temporary_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shell");
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
}

/// Write the complete encoded buffer into a uniquely named temporary file.
/// `create_new` is authoritative: on `AlreadyExists` we simply choose another
/// name (bounded), instead of relying on a racy `!path.exists()` check.
fn create_temporary<G>(
    target: &Path,
    bytes: &[u8],
    before_create: G,
) -> Result<PathBuf, (PathBuf, std::io::Error)>
where
    G: Fn(&Path),
{
    let mut last_candidate = temporary_path(target);
    for _ in 0..MAX_TEMPORARY_NAME_ATTEMPTS {
        let candidate = temporary_path(target);
        last_candidate = candidate.clone();
        before_create(&candidate);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    || error.raw_os_error() == Some(ERROR_ALREADY_EXISTS) =>
            {
                continue
            }
            Err(error) => return Err((candidate, error)),
        };
        if let Err(error) = file.write_all(bytes) {
            drop(file);
            // A partially written temporary file is never a recovery artifact.
            let _ = std::fs::remove_file(&candidate);
            return Err((candidate, error));
        }
        return Ok(candidate);
    }
    Err((
        last_candidate,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary recovery file path",
        ),
    ))
}

fn write_atomic(target: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    write_atomic_with_replacer(target, bytes, replace_file)
}

/// Full pipeline with the replacement primitive injectable. Tests use this to
/// force specific Win32 codes and disk states at a precisely controlled step;
/// production always passes `replace_file`.
fn write_atomic_with_replacer<F>(
    target: &Path,
    bytes: &[u8],
    replacer: F,
) -> Result<(), AtomicWriteError>
where
    F: Fn(&Path, &Path) -> Result<(), ReplaceStepError>,
{
    write_atomic_with_hooks(target, bytes, replacer, |_| {})
}

fn write_atomic_with_hooks<F, G>(
    target: &Path,
    bytes: &[u8],
    replacer: F,
    before_create: G,
) -> Result<(), AtomicWriteError>
where
    F: Fn(&Path, &Path) -> Result<(), ReplaceStepError>,
    G: Fn(&Path),
{
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    if let Err(error) = std::fs::create_dir_all(parent) {
        return Err(AtomicWriteError::for_create_stage(error, target, parent));
    }
    let temporary = match create_temporary(target, bytes, before_create) {
        Ok(temporary) => temporary,
        Err((candidate, error)) => {
            return Err(AtomicWriteError::for_create_stage(
                error, target, &candidate,
            ));
        }
    };

    let mut attempt = 0;
    loop {
        attempt += 1;
        match replacer(&temporary, target) {
            Ok(()) => return Ok(()),
            Err(step) => {
                let state = capture_file_state(&temporary, target);
                let class = classify_replace_failure(step.stage, &step.source, &state);
                let can_retry = class == FailureClass::Transient && attempt < MAX_REPLACE_ATTEMPTS;
                if can_retry {
                    // Same temporary file, same target, same encoded buffer.
                    // No re-read of the log and no second appended event.
                    std::thread::sleep(REPLACE_RETRY_BACKOFF);
                    continue;
                }
                let cleanup = cleanup_after_failed_replace(&temporary, &state, class);
                return Err(AtomicWriteError {
                    stage: step.stage,
                    class,
                    source: step.source,
                    attempts: attempt,
                    target: target.to_owned(),
                    temporary,
                    state,
                    cleanup,
                });
            }
        }
    }
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), ReplaceStepError> {
    std::fs::rename(temporary, target)
        .map_err(|source| ReplaceStepError::new(WriteStage::Install, source))
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), ReplaceStepError> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};
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
    // MOVEFILE_REPLACE_EXISTING handles BOTH target-present (atomic replace)
    // and target-absent (atomic rename) in one call. No `exists()` decision
    // gate, no pre-delete of the target, no backup file.
    let moved = unsafe {
        MoveFileExW(
            temporary_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    if moved == 0 {
        Err(ReplaceStepError::new(
            WriteStage::Install,
            std::io::Error::last_os_error(),
        ))
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
    fn snapshot_startup_notice_is_bounded_and_contains_only_the_summary() {
        let notice = startup_notice(StartupOutcome::SnapshotUnavailable(format!(
            "registry access denied {}",
            "x".repeat(200)
        )))
        .expect("snapshot failure should produce a notice");

        assert!(notice
            .message
            .starts_with("Shell snapshot unavailable: registry access denied"));
        assert!(notice.message.chars().count() <= 160);
        assert_eq!(notice.kind, crate::notifications::NotificationKind::Warning);
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
    fn empty_snapshot_error_writes_parseable_summary_and_allows_completion() {
        let root = TestRoot::new("empty-snapshot-error");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();

        session.record_snapshot_result(Err(String::new())).unwrap();
        session
            .record_snapshot_result(Err("later failure".to_owned()))
            .unwrap();
        session.complete_normal_exit().unwrap();

        let log = std::fs::read_to_string(&paths.shell_recovery_log).unwrap();
        assert!(log.contains("result=unspecified snapshot error"));
        assert!(log.contains("event=completed"));
        assert!(log.lines().skip(1).all(parse_event_line));
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
    fn malformed_old_session_blocks_startup_without_rewriting_bytes() {
        let root = TestRoot::new("malformed-session");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = b"not toml\0";
        std::fs::write(&paths.shell_session_file, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("malformed session must block startup");
        assert!(error.message.contains("malformed shell session"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
    }

    #[test]
    fn invalid_utf8_old_session_blocks_startup_without_rewriting_bytes() {
        let root = TestRoot::new("invalid-utf8-session");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = [0xff, 0xfe, 0x00];
        std::fs::write(&paths.shell_session_file, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("invalid UTF-8 session must block startup");
        assert!(error.message.contains("invalid UTF-8"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
    }

    #[test]
    fn unknown_session_state_blocks_startup_without_rewriting_bytes() {
        let root = TestRoot::new("unknown-session-state");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = b"format_version = 1\npid = 7\nstarted_at = 8\nnonce = 'fixture-nonce'\nstate = 'future_state'\n";
        std::fs::write(&paths.shell_session_file, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("unknown session state must block startup");
        assert!(error.message.contains("malformed shell session"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
    }

    #[test]
    fn session_missing_required_field_blocks_startup_without_rewriting_bytes() {
        let root = TestRoot::new("missing-session-field");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = b"format_version = 1\npid = 7\nstarted_at = 8\nstate = 'running'\n";
        std::fs::write(&paths.shell_session_file, original).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 3, 4)
            .expect_err("session missing required field must block startup");
        assert!(error.message.contains("malformed shell session"));
        assert_eq!(std::fs::read(&paths.shell_session_file).unwrap(), original);
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

    // =====================================================================
    // Task 01B.5B Gate B — deterministic fault-injection regression suite.
    // Every failure below is produced by an injected replacement step; none
    // relies on randomly triggering Win32 error 1175.
    // =====================================================================

    use std::sync::atomic::AtomicUsize;

    const OLD_EVENT: &str = "timestamp=1 pid=1 event=started result=ok";

    #[derive(Clone, Copy)]
    enum InjectedState {
        LeaveIntact,
        RemoveTarget,
        RemoveTemporary,
        RenameTarget,
    }

    #[derive(Clone, Copy)]
    struct InjectedFailure {
        stage: WriteStage,
        code: i32,
        state: InjectedState,
    }

    impl InjectedFailure {
        const fn new(stage: WriteStage, code: i32, state: InjectedState) -> Self {
            Self { stage, code, state }
        }
    }

    /// Replacement-step injector: on each attempt it can place the disk into a
    /// documented Win32 partial-failure state before returning a chosen error
    /// code. Once the plan is exhausted it delegates to the real primitive.
    fn injecting_replacer(
        failures: Vec<InjectedFailure>,
        calls: Arc<AtomicUsize>,
    ) -> impl Fn(&Path, &Path) -> Result<(), ReplaceStepError> {
        move |temporary, target| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            if let Some(failure) = failures.get(index) {
                match failure.state {
                    InjectedState::LeaveIntact => {}
                    InjectedState::RemoveTarget => {
                        let _ = std::fs::remove_file(target);
                    }
                    InjectedState::RemoveTemporary => {
                        let _ = std::fs::remove_file(temporary);
                    }
                    InjectedState::RenameTarget => {
                        let renamed = target.with_extension("renamed");
                        let _ = std::fs::remove_file(&renamed);
                        let _ = std::fs::rename(target, &renamed);
                    }
                }
                return Err(ReplaceStepError::new(
                    failure.stage,
                    std::io::Error::from_raw_os_error(failure.code),
                ));
            }
            replace_file(temporary, target)
        }
    }

    fn fixture_log(event: &str) -> Vec<u8> {
        format!("{LOG_HEADER}\n{event}\n").into_bytes()
    }

    fn assert_valid_log(bytes: &[u8]) {
        assert!(bytes.len() <= MAX_LOG_BYTES, "log exceeds MAX_LOG_BYTES");
        assert!(bytes.starts_with(format!("{LOG_HEADER}\n").as_bytes()));
        assert!(bytes.ends_with(b"\n"), "log must end with a complete line");
        assert!(!bytes.contains(&b'\r'), "log must not contain CR");
        let text = std::str::from_utf8(bytes).expect("log must be UTF-8");
        for line in text.lines().skip(1) {
            assert!(parse_event_line(line), "invalid event line: {line:?}");
        }
    }

    fn temporary_files(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().ends_with(".tmp"))
            .collect()
    }

    fn seeded_log_root(name: &str) -> (TestRoot, ConfigPaths, Vec<u8>) {
        let root = TestRoot::new(name);
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        let original = fixture_log(OLD_EVENT);
        std::fs::write(&paths.shell_recovery_log, &original).unwrap();
        (root, paths, original)
    }

    fn transient_plan(code: i32, count: usize) -> Vec<InjectedFailure> {
        (0..count)
            .map(|_| InjectedFailure::new(WriteStage::Install, code, InjectedState::LeaveIntact))
            .collect()
    }

    // --- §4.1 / §4.2: target absent / present -----------------------------

    #[test]
    fn gate_b_first_write_when_target_is_absent_creates_a_complete_file() {
        let root = TestRoot::new("gb-first-write");
        let target = root.path().join("shell_recovery.log");
        let bytes = fixture_log(OLD_EVENT);

        write_atomic(&target, &bytes).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_valid_log(&bytes);
        assert!(
            temporary_files(root.path()).is_empty(),
            "no temporary residue"
        );
    }

    #[test]
    fn gate_b_existing_target_is_replaced_atomically() {
        let root = TestRoot::new("gb-existing-target");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_recovery.log");
        std::fs::write(&target, fixture_log(OLD_EVENT)).unwrap();
        let replacement = fixture_log("timestamp=2 pid=2 event=completed result=ok");

        write_atomic(&target, &replacement).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), replacement);
        assert_valid_log(&replacement);
        assert!(
            temporary_files(root.path()).is_empty(),
            "no temporary residue"
        );
    }

    // --- §4.3 / §4.4 / §4.10: 1175 bounded retry ---------------------------

    #[test]
    fn gate_b_injected_1175_is_retried_with_bounded_attempts_and_succeeds() {
        let (_root, paths, _original) = seeded_log_root("gb-1175-retry-ok");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(transient_plan(1175, 2), calls.clone());

        append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), MAX_REPLACE_ATTEMPTS as usize);
        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_valid_log(&log);
        let text = String::from_utf8(log).unwrap();
        assert_eq!(
            text.matches("event=completed").count(),
            1,
            "event appended once"
        );
        assert!(text.contains(OLD_EVENT), "old event is retained");
        assert!(temporary_files(paths.shell_recovery_log.parent().unwrap()).is_empty());
    }

    #[test]
    fn gate_b_injected_1175_exhausts_budget_and_returns_contextual_error() {
        let (_root, paths, original) = seeded_log_root("gb-1175-exhaust");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(transient_plan(1175, 4), calls.clone());

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("exhausted retries must never silently succeed");

        assert_eq!(calls.load(Ordering::SeqCst), MAX_REPLACE_ATTEMPTS as usize);
        assert!(error.message.contains("classification=Transient"));
        assert!(error.message.contains("attempts=3/3"));
        assert!(error
            .message
            .contains(paths.shell_recovery_log.to_string_lossy().as_ref()));
        assert!(error.message.contains("cleanup="));
        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_eq!(log, original, "old log must remain byte-identical");
        assert_valid_log(&log);
        assert_eq!(
            String::from_utf8_lossy(&log)
                .matches("event=completed")
                .count(),
            0,
            "no duplicate or lost event"
        );
        assert!(
            temporary_files(paths.shell_recovery_log.parent().unwrap()).is_empty(),
            "verified old target allows removal of the temporary file"
        );
    }

    // --- §4.5: 32 sharing violation ----------------------------------------

    #[test]
    fn gate_b_injected_32_sharing_violation_is_retried_and_recovers() {
        let (_root, paths, _original) = seeded_log_root("gb-32-retry");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(transient_plan(32, 2), calls.clone());

        append_event_with_replacer(&paths, 9, RecoveryEventKind::SnapshotSaved, "ok", replacer)
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), MAX_REPLACE_ATTEMPTS as usize);
        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_valid_log(&log);
        assert_eq!(
            String::from_utf8_lossy(&log)
                .matches("event=snapshot_saved")
                .count(),
            1
        );
        assert!(temporary_files(paths.shell_recovery_log.parent().unwrap()).is_empty());
    }

    #[test]
    fn gate_b_32_with_missing_temporary_is_never_retried() {
        let (_root, paths, original) = seeded_log_root("gb-32-tmp-missing");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(
            vec![InjectedFailure::new(
                WriteStage::Install,
                32,
                InjectedState::RemoveTemporary,
            )],
            calls.clone(),
        );

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("state change must not be retried");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(error.message.contains("classification=StateChanged"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
        assert!(temporary_files(paths.shell_recovery_log.parent().unwrap()).is_empty());
    }

    // --- §4.6: 5 permission / read-only ------------------------------------

    #[test]
    fn gate_b_injected_5_at_replace_existing_stage_is_not_blanket_retried() {
        let (_root, paths, original) = seeded_log_root("gb-5-permission");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(
            vec![InjectedFailure::new(
                WriteStage::ReplaceExisting,
                5,
                InjectedState::LeaveIntact,
            )],
            calls.clone(),
        );

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("access denied must be reported");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry at this stage");
        assert!(error.message.contains("classification=Permanent"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
        assert_eq!(
            temporary_files(paths.shell_recovery_log.parent().unwrap()).len(),
            1,
            "both files are preserved as evidence"
        );
    }

    #[cfg(windows)]
    #[test]
    // Windows-only test: it restores the read-only attribute it just set on an
    // isolated temporary file so `TestRoot::drop` can remove the directory.
    // The Unix world-writable concern behind this lint does not apply.
    #[allow(clippy::permissions_set_readonly_false)]
    fn gate_b_readonly_target_produces_access_denied_and_preserves_both_files() {
        let root = TestRoot::new("gb-readonly");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_recovery.log");
        let original = fixture_log(OLD_EVENT);
        std::fs::write(&target, &original).unwrap();
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions).unwrap();

        let replacement = fixture_log("timestamp=2 pid=2 event=completed result=ok");
        let error = write_atomic(&target, &replacement)
            .expect_err("a read-only target must not be silently replaced");

        assert_eq!(error.source.raw_os_error(), Some(ERROR_ACCESS_DENIED));
        assert_eq!(error.class, FailureClass::Permanent);
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert_eq!(
            temporary_files(root.path()).len(),
            1,
            "temporary file is preserved as evidence"
        );

        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&target, permissions).unwrap();
    }

    // --- §4.7: 1176 / 1177 partial failure ---------------------------------

    #[test]
    fn gate_b_injected_1176_target_gone_preserves_the_only_complete_artifact() {
        let (_root, paths, original) = seeded_log_root("gb-1176-target-gone");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(
            vec![InjectedFailure::new(
                WriteStage::Install,
                1176,
                InjectedState::RemoveTarget,
            )],
            calls.clone(),
        );

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("partial failure must not be auto-recovered");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "1176 must never be retried"
        );
        assert!(error.message.contains("classification=StateChanged"));
        assert!(error.message.contains("target is missing"));
        assert!(error.message.contains("backup=none"));
        assert!(error
            .message
            .contains(paths.shell_recovery_log.to_string_lossy().as_ref()));
        assert!(!paths.shell_recovery_log.exists(), "target is gone");
        let artifacts = temporary_files(paths.shell_recovery_log.parent().unwrap());
        assert_eq!(artifacts.len(), 1, "exactly one recovery artifact");
        let preserved = std::fs::read(&artifacts[0]).unwrap();
        assert_valid_log(&preserved);
        assert!(String::from_utf8_lossy(&preserved).contains("event=completed"));
        assert_ne!(preserved, original);
        assert!(
            error
                .message
                .contains(artifacts[0].to_string_lossy().as_ref()),
            "error names the actual recovery path"
        );
    }

    #[test]
    fn gate_b_injected_1177_preserves_both_old_and_new_content() {
        let (_root, paths, original) = seeded_log_root("gb-1177-renamed");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(
            vec![InjectedFailure::new(
                WriteStage::Install,
                1177,
                InjectedState::RenameTarget,
            )],
            calls.clone(),
        );

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("1177 must not be silently recovered");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "1177 must never be retried"
        );
        assert!(error.message.contains("classification=StateChanged"));
        let renamed = paths.shell_recovery_log.with_extension("renamed");
        assert!(
            renamed.exists(),
            "old content survives under a different name"
        );
        assert_eq!(std::fs::read(&renamed).unwrap(), original);
        let artifacts = temporary_files(paths.shell_recovery_log.parent().unwrap());
        assert_eq!(
            artifacts.len(),
            1,
            "new content survives in the temporary file"
        );
        assert_valid_log(&std::fs::read(&artifacts[0]).unwrap());
    }

    // --- §4.8: temporary retention policy ----------------------------------

    #[test]
    fn gate_b_temporary_retention_policy_is_state_driven() {
        let root = TestRoot::new("gb-cleanup-policy");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_recovery.log");
        let temporary = root.path().join("shell_recovery.log.1.tmp");
        std::fs::write(&target, fixture_log(OLD_EVENT)).unwrap();
        std::fs::write(&temporary, fixture_log(OLD_EVENT)).unwrap();

        let state = capture_file_state(&temporary, &target);
        assert!(state.target_verifiable);
        assert_eq!(
            cleanup_after_failed_replace(&temporary, &state, FailureClass::Transient),
            CleanupAction::RemovedTemporaryAfterVerifiedTarget
        );
        assert!(!temporary.exists());

        std::fs::write(&temporary, fixture_log(OLD_EVENT)).unwrap();
        let state = capture_file_state(&temporary, &target);
        assert_eq!(
            cleanup_after_failed_replace(&temporary, &state, FailureClass::StateChanged),
            CleanupAction::PreservedBothAsEvidence
        );
        assert_eq!(
            cleanup_after_failed_replace(&temporary, &state, FailureClass::Permanent),
            CleanupAction::PreservedBothAsEvidence
        );
        assert!(temporary.exists() && target.exists());

        std::fs::remove_file(&target).unwrap();
        let state = capture_file_state(&temporary, &target);
        assert_eq!(
            cleanup_after_failed_replace(&temporary, &state, FailureClass::Transient),
            CleanupAction::PreservedTemporaryAsOnlyCompleteArtifact
        );
        assert!(temporary.exists());
    }

    // --- §4.9: old log integrity / no silent success -----------------------

    #[test]
    fn gate_b_unknown_error_preserves_old_log_and_never_silently_succeeds() {
        let (_root, paths, original) = seeded_log_root("gb-unknown-error");
        let calls = Arc::new(AtomicUsize::new(0));
        let replacer = injecting_replacer(
            vec![InjectedFailure::new(
                WriteStage::Install,
                3,
                InjectedState::LeaveIntact,
            )],
            calls.clone(),
        );

        let error =
            append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                .expect_err("unknown error must be returned");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(error.message.contains("classification=Permanent"));
        assert_eq!(std::fs::read(&paths.shell_recovery_log).unwrap(), original);
        assert_valid_log(&original);
        assert_eq!(
            temporary_files(paths.shell_recovery_log.parent().unwrap()).len(),
            1
        );
    }

    #[test]
    fn gate_b_all_injected_failures_are_returned_and_never_silently_succeed() {
        let scenarios = [
            (WriteStage::Install, 1175, InjectedState::LeaveIntact),
            (WriteStage::Install, 32, InjectedState::LeaveIntact),
            (WriteStage::Install, 5, InjectedState::LeaveIntact),
            (WriteStage::Install, 1176, InjectedState::RemoveTarget),
            (WriteStage::Install, 1177, InjectedState::RenameTarget),
            (WriteStage::Install, 3, InjectedState::LeaveIntact),
            (
                WriteStage::ReplaceExisting,
                1175,
                InjectedState::LeaveIntact,
            ),
        ];
        for (index, (stage, code, state)) in scenarios.iter().enumerate() {
            let root = TestRoot::new(&format!("gb-no-silent-{index}"));
            std::fs::create_dir_all(root.path()).unwrap();
            let paths = config_paths_from_root(root.path().to_owned());
            let original = fixture_log(OLD_EVENT);
            std::fs::write(&paths.shell_recovery_log, &original).unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let replacer = injecting_replacer(
                vec![InjectedFailure::new(*stage, *code, *state); 4],
                calls.clone(),
            );

            let error =
                append_event_with_replacer(&paths, 9, RecoveryEventKind::Completed, "ok", replacer)
                    .expect_err(&format!("injected win32={code} must fail loudly"));
            assert!(calls.load(Ordering::SeqCst) <= MAX_REPLACE_ATTEMPTS as usize);
            assert!(error
                .message
                .contains(paths.shell_recovery_log.to_string_lossy().as_ref()));
            if paths.shell_recovery_log.exists() {
                let log = std::fs::read(&paths.shell_recovery_log).unwrap();
                assert_eq!(log, original, "old log must not be overwritten");
                assert_valid_log(&log);
            }
        }
    }

    // --- §4.11 / §4.12: max length, complete lines -------------------------

    #[test]
    fn gate_b_log_stays_bounded_and_line_complete_under_many_appends() {
        let root = TestRoot::new("gb-bounded-log");
        let paths = config_paths_from_root(root.path().to_owned());
        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        for index in 0..200 {
            session
                .record_snapshot_result(Err(format!("failure {index} {}", "x".repeat(500))))
                .unwrap();
        }

        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_valid_log(&log);
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn gate_b_incomplete_log_is_still_rejected_after_failed_replacement() {
        let root = TestRoot::new("gb-incomplete-after-failure");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        let incomplete = b"format_version=1\ntimestamp=1 pid=1 event=started result=ok\npartial";
        std::fs::write(&paths.shell_recovery_log, incomplete).unwrap();

        let error = ShellRecovery::begin_startup(&paths, 1, 2)
            .expect_err("an incomplete final line must block startup");
        assert!(error.message.contains("incomplete final line"));
        assert_eq!(
            std::fs::read(&paths.shell_recovery_log).unwrap(),
            incomplete,
            "no replacement is attempted on malformed input"
        );
        assert!(temporary_files(root.path()).is_empty());
    }

    // --- §4.13: named coordinator still effective --------------------------

    #[cfg(windows)]
    #[test]
    fn gate_b_named_coordinator_still_serializes_concurrent_session_writes() {
        let root = TestRoot::new("gb-concurrent-coordinator");
        let paths = config_paths_from_root(root.path().to_owned());
        let handles = (0..8)
            .map(|index| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    let (_recovery, session) =
                        ShellRecovery::begin_startup(&paths, 100 + index, 1).unwrap();
                    session.mark_running().unwrap();
                    session.record_snapshot_result(Ok(())).unwrap();
                    session.complete_normal_exit().unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_valid_log(&log);
        assert!(temporary_files(root.path()).is_empty(), "no residue");
    }

    // --- §4.14: recovery state machine on write failure --------------------

    #[test]
    fn gate_b_write_failure_is_returned_and_recovery_state_is_not_skipped() {
        let root = TestRoot::new("gb-state-machine-failure");
        let paths = config_paths_from_root(root.path().to_owned());
        let (recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        session.mark_running().unwrap();

        // Deterministically make the log write fail. No real Shell state,
        // registry value or user configuration is touched.
        std::fs::remove_file(&paths.shell_recovery_log).unwrap();
        std::fs::create_dir(&paths.shell_recovery_log).unwrap();
        let error = session
            .complete_normal_exit()
            .expect_err("log write failure must not be swallowed");
        assert!(!error.message.is_empty());
        assert_ne!(recovery.startup_state(), StartupState::Completed);
        assert!(
            session.path().exists(),
            "session must not be removed on failure"
        );
        let session_toml = std::fs::read_to_string(session.path()).unwrap();
        assert!(!session_toml.contains("state = \"completed\""));

        std::fs::remove_dir(&paths.shell_recovery_log).unwrap();
        std::fs::write(&paths.shell_recovery_log, fixture_log(OLD_EVENT)).unwrap();
        let (next_recovery, _next_session) = ShellRecovery::begin_startup(&paths, 3, 4).unwrap();
        assert_eq!(
            next_recovery.startup_state(),
            StartupState::RecoveryRecorded
        );
        let log = std::fs::read(&paths.shell_recovery_log).unwrap();
        assert_valid_log(&log);
        assert_eq!(
            String::from_utf8_lossy(&log)
                .matches("event=recovery_required")
                .count(),
            1
        );
    }

    // --- Gate A §8.3 #7 / #8 / #13: create_new collision, TOCTOU, matrix ---

    #[test]
    fn gate_b_create_new_collision_selects_a_fresh_unique_name() {
        let root = TestRoot::new("gb-create-collision");
        let target = root.path().join("shell_recovery.log");
        let bytes = fixture_log(OLD_EVENT);
        let seen = Arc::new(AtomicUsize::new(0));
        let collisions = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let seen_for_hook = seen.clone();
        let collisions_for_hook = collisions.clone();

        write_atomic_with_hooks(&target, &bytes, replace_file, move |candidate| {
            if seen_for_hook.fetch_add(1, Ordering::SeqCst) == 0 {
                std::fs::write(candidate, b"collision placeholder").unwrap();
                collisions_for_hook
                    .lock()
                    .unwrap()
                    .push(candidate.to_owned());
            }
        })
        .unwrap();

        assert_eq!(
            seen.load(Ordering::SeqCst),
            2,
            "one collision, then a fresh name"
        );
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_valid_log(&bytes);
        let placeholder = collisions.lock().unwrap()[0].clone();
        std::fs::remove_file(&placeholder).unwrap();
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn gate_b_interleaved_target_creation_does_not_produce_a_partial_or_mixed_file() {
        let root = TestRoot::new("gb-toctou");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_recovery.log");
        let bytes = fixture_log("timestamp=2 pid=2 event=completed result=ok");
        let interleaved = Arc::new(AtomicUsize::new(0));
        let replacer = move |temporary: &Path, target: &Path| {
            if interleaved.fetch_add(1, Ordering::SeqCst) == 0 {
                // Simulates a writer creating the target in the window where
                // the old `target.exists()` check would already be stale.
                std::fs::write(target, fixture_log(OLD_EVENT)).unwrap();
            }
            replace_file(temporary, target)
        };

        write_atomic_with_replacer(&target, &bytes, replacer).unwrap();

        let final_bytes = std::fs::read(&target).unwrap();
        assert_eq!(final_bytes, bytes, "one atomic, complete replacement");
        assert_valid_log(&final_bytes);
        assert!(temporary_files(root.path()).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn gate_b_static_external_handle_is_reported_not_silently_swallowed() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        let root = TestRoot::new("gb-external-handle");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_recovery.log");
        let original = fixture_log(OLD_EVENT);
        std::fs::write(&target, &original).unwrap();

        let wide = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // Share READ|WRITE but NOT DELETE: a replacement cannot proceed.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE, "CreateFileW must open target");

        let replacement = fixture_log("timestamp=2 pid=2 event=completed result=ok");
        let error = write_atomic(&target, &replacement)
            .expect_err("a static external handle must not be silently ignored");
        assert!(
            matches!(error.source.raw_os_error(), Some(32) | Some(5) | Some(1175)),
            "unexpected win32 code: {:?}",
            error.source.raw_os_error()
        );
        assert_eq!(error.attempts, MAX_REPLACE_ATTEMPTS, "bounded retry only");
        assert_eq!(std::fs::read(&target).unwrap(), original, "old log intact");
        assert!(
            temporary_files(root.path()).is_empty(),
            "verified old target allows temporary-file cleanup"
        );

        unsafe {
            CloseHandle(handle);
        }
        write_atomic(&target, &replacement).expect("replacement succeeds once handle is closed");
        assert_eq!(std::fs::read(&target).unwrap(), replacement);
        assert_valid_log(&replacement);
    }

    #[test]
    fn gate_b_sixteen_threads_and_high_frequency_replacements_stay_consistent() {
        let roots = (0..16)
            .map(|index| TestRoot::new(&format!("gb-stress-{index}")))
            .collect::<Vec<_>>();
        let handles = roots
            .iter()
            .enumerate()
            .map(|(index, root)| {
                let dir = root.path().to_owned();
                std::thread::spawn(move || {
                    let target = dir.join("shell_recovery.log");
                    for round in 0..80u32 {
                        let event =
                            format!("timestamp={round} pid={index} event=started result=ok");
                        write_atomic(&target, &fixture_log(&event)).unwrap();
                    }
                    let final_bytes = std::fs::read(&target).unwrap();
                    assert_valid_log(&final_bytes);
                    assert!(temporary_files(&dir).is_empty(), "no temporary residue");
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        for root in &roots {
            assert!(temporary_files(root.path()).is_empty());
        }
    }

    #[test]
    fn gate_b_error_classification_matrix_is_stage_and_state_dependent() {
        let io = |code: i32| std::io::Error::from_raw_os_error(code);
        let intact = FileState {
            target_exists: true,
            temporary_exists: true,
            target_readonly: false,
            target_verifiable: true,
        };
        let broken = FileState {
            target_exists: false,
            temporary_exists: true,
            target_readonly: false,
            target_verifiable: false,
        };
        let readonly = FileState {
            target_readonly: true,
            ..intact
        };

        assert_eq!(
            classify_replace_failure(
                WriteStage::CreateTemporary,
                &io(ERROR_ALREADY_EXISTS),
                &intact
            ),
            FailureClass::RetryTemporaryName
        );
        assert_eq!(
            classify_replace_failure(WriteStage::CreateTemporary, &io(5), &intact),
            FailureClass::Permanent
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(1175), &intact),
            FailureClass::Transient
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(1175), &broken),
            FailureClass::StateChanged
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(32), &intact),
            FailureClass::Transient
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(32), &broken),
            FailureClass::StateChanged
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(5), &intact),
            FailureClass::Transient
        );
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(5), &readonly),
            FailureClass::Permanent
        );
        assert_eq!(
            classify_replace_failure(WriteStage::ReplaceExisting, &io(5), &intact),
            FailureClass::Permanent
        );
        for code in [1176, 1177] {
            assert_eq!(
                classify_replace_failure(WriteStage::Install, &io(code), &intact),
                FailureClass::StateChanged
            );
            assert_eq!(
                classify_replace_failure(WriteStage::ReplaceExisting, &io(code), &broken),
                FailureClass::StateChanged
            );
        }
        assert_eq!(
            classify_replace_failure(WriteStage::Install, &io(3), &intact),
            FailureClass::Permanent
        );
    }
}
