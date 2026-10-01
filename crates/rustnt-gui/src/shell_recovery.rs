use crate::config::ConfigPaths;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_LOG_BYTES: usize = 8 * 1024;
const SESSION_FORMAT_VERSION: u32 = 1;
const MAX_EVENT_BYTES: usize = 511;
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    version: u32,
    pid: u32,
    started_at: u64,
    state: String,
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
        self.write_state("running")?;
        self.set_state(StartupState::Running)
    }

    pub fn record_snapshot_result(
        &self,
        result: Result<(), String>,
    ) -> Result<(), RecoveryLogError> {
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
                self.write_state("snapshot_unavailable")?;
                self.set_state(StartupState::SnapshotUnavailable)?;
            }
        }
        Ok(())
    }

    pub fn complete_normal_exit(&self) -> Result<(), RecoveryLogError> {
        if !self.owns_current_session()? {
            return Ok(());
        }
        append_event(
            &self.recovery.paths,
            self.pid,
            RecoveryEventKind::Completed,
            "ok",
        )?;
        self.write_state("completed")?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RecoveryLogError::new("remove shell session", error)),
        }
        self.set_state(StartupState::Completed)
    }

    fn owns_current_session(&self) -> Result<bool, RecoveryLogError> {
        let encoded = match std::fs::read_to_string(&self.path) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(RecoveryLogError::new("read shell session", error)),
        };
        let current: SessionFile = match toml::from_str(&encoded) {
            Ok(current) => current,
            Err(_) => return Ok(false),
        };
        Ok(current.version == SESSION_FORMAT_VERSION
            && current.pid == self.pid
            && current.started_at == self.started_at)
    }

    fn write_state(&self, state: &str) -> Result<(), RecoveryLogError> {
        write_session(
            &self.path,
            &SessionFile {
                version: SESSION_FORMAT_VERSION,
                pid: self.pid,
                started_at: self.started_at,
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

    let previous = match std::fs::read_to_string(&paths.shell_session_file) {
        Ok(encoded) => toml::from_str::<SessionFile>(&encoded).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(RecoveryLogError::new("read shell session", error)),
    };
    let recovered = previous.as_ref().is_some_and(|session| {
        session.version == SESSION_FORMAT_VERSION && session.state != "completed"
    });

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
        path: paths.shell_session_file.clone(),
    };
    session.write_state("starting")?;
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
    let existing = match std::fs::File::open(&paths.shell_recovery_log) {
        Ok(mut file) => {
            let mut bytes = Vec::with_capacity(MAX_LOG_BYTES + 1);
            std::io::Read::take(std::io::Read::by_ref(&mut file), (MAX_LOG_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| RecoveryLogError::new("read shell recovery log", error))?;
            bytes
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(RecoveryLogError::new("read shell recovery log", error)),
    };
    let complete_length = existing
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut lines = existing
        .get(..complete_length)
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect::<Vec<_>>();
    lines.push(event.trim_end_matches('\n').to_owned());
    let mut retained = Vec::new();
    let mut size = 0;
    for line in lines.into_iter().rev() {
        let line_size = line.len() + 1;
        if size + line_size > MAX_LOG_BYTES {
            break;
        }
        size += line_size;
        retained.push(line);
    }
    retained.reverse();
    let encoded = retained.join("\n") + "\n";
    write_atomic(&paths.shell_recovery_log, encoded.as_bytes())
        .map_err(|error| RecoveryLogError::new("write shell recovery log", error))
}

fn write_session(path: &Path, session: &SessionFile) -> Result<(), RecoveryLogError> {
    let encoded = toml::to_string(session)
        .map_err(|error| RecoveryLogError::new("serialize shell session", error))?;
    write_atomic(path, encoded.as_bytes())
        .map_err(|error| RecoveryLogError::new("write shell session", error))
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
        "version = 1\npid = 7\nstarted_at = 8\nstate = 'running'\n"
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
        assert!(log.ends_with(b"\n"));
        assert!(!log.contains(&b'\r'));
    }

    #[test]
    fn bounded_log_discards_an_incomplete_existing_record() {
        let root = TestRoot::new("incomplete-log");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(
            &paths.shell_recovery_log,
            b"timestamp=1 pid=1 event=started result=ok\npartial-record",
        )
        .unwrap();

        let (_recovery, session) = ShellRecovery::begin_startup(&paths, 1, 2).unwrap();
        let log = std::fs::read_to_string(&paths.shell_recovery_log).unwrap();
        assert!(!log.contains("partial-record"));
        assert!(log.lines().all(|line| !line.is_empty()));
        session.complete_normal_exit().unwrap();
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
            .contains("version"));
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
}
