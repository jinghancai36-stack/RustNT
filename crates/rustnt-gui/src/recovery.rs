use crate::config::ConfigPaths;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER_SIDECAR_SUFFIX: &str = ".marker";
const MAX_CRASH_RECORD_BYTES: usize = 4096;
static MARKER_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryState {
    pub previous_run_incomplete: bool,
}

pub struct RuntimeMarker {
    path: PathBuf,
}

#[derive(Debug)]
pub struct RecoveryError {
    pub operation: String,
    pub message: String,
}

impl RecoveryError {
    fn new(operation: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation: operation.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for RecoveryError {}

impl From<std::io::Error> for RecoveryError {
    fn from(error: std::io::Error) -> Self {
        Self::new("recovery I/O", error)
    }
}

pub fn inspect(paths: &ConfigPaths) -> Result<RecoveryState, RecoveryError> {
    let mut previous_run_incomplete = false;
    for path in marker_paths(paths)? {
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(RecoveryError::new("inspect runtime marker", error)),
        };
        previous_run_incomplete = true;
        match marker_liveness(marker_identity(&contents)) {
            MarkerLiveness::Stale => remove_stale_marker(&path)?,
            MarkerLiveness::Running | MarkerLiveness::Unknown => {}
        }
    }
    Ok(RecoveryState {
        previous_run_incomplete,
    })
}

pub fn create_marker(paths: &ConfigPaths) -> Result<RuntimeMarker, RecoveryError> {
    std::fs::create_dir_all(&paths.root)
        .map_err(|error| RecoveryError::new("create runtime marker directory", error))?;
    let creation_time = current_process_creation_time_100ns()
        .map_err(|error| RecoveryError::new("query current process creation time", error))?;
    let creation_time_line = creation_time
        .map(|value| format!("creation_time_100ns={value}\n"))
        .unwrap_or_default();
    let contents = format!(
        "pid={}\nstarted_at={}\n{creation_time_line}",
        std::process::id(),
        unix_timestamp_nanos()
    );
    for _ in 0..16 {
        let path = marker_sidecar_path(paths);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(RecoveryError::new("write runtime marker", error));
            }
        };
        if let Err(error) = file.write_all(contents.as_bytes()) {
            let _ = std::fs::remove_file(&path);
            return Err(RecoveryError::new("write runtime marker", error));
        }
        return Ok(RuntimeMarker { path });
    }
    Err(RecoveryError::new(
        "write runtime marker",
        "could not allocate a unique runtime marker path",
    ))
}

impl RuntimeMarker {
    pub fn remove(self) -> Result<(), RecoveryError> {
        match std::fs::remove_file(self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(RecoveryError::new("remove runtime marker", error)),
        }
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn marker_sidecar_path(paths: &ConfigPaths) -> PathBuf {
    let parent = paths.marker_file.parent().unwrap_or_else(|| Path::new("."));
    let name = paths
        .marker_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gui.running");
    let timestamp = unix_timestamp_nanos();
    let counter = MARKER_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        "{name}.{}.{}.{}{}",
        std::process::id(),
        timestamp,
        counter,
        MARKER_SIDECAR_SUFFIX
    ))
}

fn marker_paths(paths: &ConfigPaths) -> Result<Vec<PathBuf>, RecoveryError> {
    let mut paths_to_inspect = Vec::new();
    if paths.marker_file.exists() {
        paths_to_inspect.push(paths.marker_file.clone());
    }
    let entries = match std::fs::read_dir(&paths.root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths_to_inspect),
        Err(error) => return Err(RecoveryError::new("inspect runtime markers", error)),
    };
    let legacy_name = paths
        .marker_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gui.running");
    let prefix = format!("{legacy_name}.");
    paths_to_inspect.extend(entries.filter_map(Result::ok).filter_map(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        (name.starts_with(&prefix) && name.ends_with(MARKER_SIDECAR_SUFFIX)).then_some(entry.path())
    }));
    Ok(paths_to_inspect)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerIdentity {
    pid: u32,
    creation_time_100ns: Option<u64>,
}

fn marker_identity(contents: &str) -> Option<MarkerIdentity> {
    let pid = contents.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|value| value.trim().parse::<u32>().ok())
    })?;
    let creation_time_100ns = contents.lines().find_map(|line| {
        line.strip_prefix("creation_time_100ns=")
            .and_then(|value| value.trim().parse::<u64>().ok())
    });
    Some(MarkerIdentity {
        pid,
        creation_time_100ns,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerLiveness {
    Running,
    Stale,
    Unknown,
}

#[cfg(windows)]
fn marker_liveness(marker: Option<MarkerIdentity>) -> MarkerLiveness {
    let Some(marker) = marker else {
        return MarkerLiveness::Unknown;
    };
    windows_process::marker_liveness(marker)
}

#[cfg(not(windows))]
fn marker_liveness(_marker: Option<MarkerIdentity>) -> MarkerLiveness {
    MarkerLiveness::Unknown
}

#[cfg(windows)]
fn current_process_creation_time_100ns() -> std::io::Result<Option<u64>> {
    windows_process::current_process_creation_time_100ns().map(Some)
}

#[cfg(not(windows))]
fn current_process_creation_time_100ns() -> std::io::Result<Option<u64>> {
    Ok(None)
}

#[cfg(windows)]
mod windows_process {
    use super::{MarkerIdentity, MarkerLiveness};
    use std::io;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    pub fn current_process_creation_time_100ns() -> io::Result<u64> {
        // GetCurrentProcess returns a pseudo-handle that must not be closed.
        unsafe { process_creation_time_100ns(GetCurrentProcess()) }
    }

    pub fn marker_liveness(marker: MarkerIdentity) -> MarkerLiveness {
        if marker.pid == std::process::id() {
            return match marker.creation_time_100ns {
                None => MarkerLiveness::Running,
                Some(expected) => match current_process_creation_time_100ns() {
                    Ok(actual) if actual == expected => MarkerLiveness::Running,
                    Ok(_) => MarkerLiveness::Stale,
                    Err(_) => MarkerLiveness::Unknown,
                },
            };
        }

        unsafe {
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                marker.pid,
            );
            if process.is_null() {
                return if GetLastError() == ERROR_INVALID_PARAMETER {
                    MarkerLiveness::Stale
                } else {
                    MarkerLiveness::Unknown
                };
            }

            let liveness = process_liveness(process, marker.creation_time_100ns);
            let _ = CloseHandle(process);
            liveness
        }
    }

    unsafe fn process_liveness(
        process: HANDLE,
        expected_creation_time_100ns: Option<u64>,
    ) -> MarkerLiveness {
        match zero_time_wait(process) {
            ProcessWait::Running => {}
            ProcessWait::Exited => return MarkerLiveness::Stale,
            ProcessWait::Unknown => return MarkerLiveness::Unknown,
        }
        let Some(expected_creation_time_100ns) = expected_creation_time_100ns else {
            return MarkerLiveness::Running;
        };
        match process_creation_time_100ns(process) {
            Ok(actual) if actual == expected_creation_time_100ns => MarkerLiveness::Running,
            Ok(_) => MarkerLiveness::Stale,
            Err(_) => MarkerLiveness::Unknown,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ProcessWait {
        Running,
        Exited,
        Unknown,
    }

    fn map_wait_result(wait_result: u32) -> ProcessWait {
        match wait_result {
            WAIT_TIMEOUT => ProcessWait::Running,
            WAIT_OBJECT_0 => ProcessWait::Exited,
            _ => ProcessWait::Unknown,
        }
    }

    unsafe fn zero_time_wait(process: HANDLE) -> ProcessWait {
        map_wait_result(WaitForSingleObject(process, 0))
    }

    unsafe fn process_creation_time_100ns(process: HANDLE) -> io::Result<u64> {
        let mut creation_time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit_time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel_time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user_time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        if GetProcessTimes(
            process,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(
            (u64::from(creation_time.dwHighDateTime) << 32)
                | u64::from(creation_time.dwLowDateTime),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows_sys::Win32::Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};

        #[test]
        fn zero_time_wait_maps_running_exited_and_unknown_results() {
            assert_eq!(map_wait_result(WAIT_TIMEOUT), ProcessWait::Running);
            assert_eq!(map_wait_result(WAIT_OBJECT_0), ProcessWait::Exited);
            assert_eq!(map_wait_result(WAIT_FAILED), ProcessWait::Unknown);
        }

        #[test]
        fn zero_time_wait_reports_current_process_as_running() {
            let result = unsafe { zero_time_wait(GetCurrentProcess()) };

            assert_eq!(result, ProcessWait::Running);
        }
    }
}

fn remove_stale_marker(path: &Path) -> Result<(), RecoveryError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RecoveryError::new("remove stale runtime marker", error)),
    }
}

pub fn append_crash_record(path: &Path, summary: &str) -> std::io::Result<()> {
    let current_thread = std::thread::current();
    let thread_name = sanitized_summary(current_thread.name().unwrap_or("unnamed"), 256);
    let thread_id = format!("{:?}", current_thread.id());
    let prefix = format!(
        "{}: thread={thread_name} thread_id={thread_id}: ",
        unix_timestamp()
    );
    let available_summary_bytes = MAX_CRASH_RECORD_BYTES
        .saturating_sub(prefix.len())
        .saturating_sub(1);
    let summary = sanitized_summary(summary, available_summary_bytes);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(prefix.as_bytes())?;
    file.write_all(summary.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn sanitized_summary(summary: &str, max_bytes: usize) -> String {
    let mut sanitized = String::with_capacity(max_bytes.min(summary.len()));
    for character in summary.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if sanitized.len() + character.len_utf8() > max_bytes {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

type PanicHook = Box<dyn for<'a> Fn(&std::panic::PanicHookInfo<'a>) + Send + Sync + 'static>;

fn make_panic_hook(crash_log: PathBuf, previous: PanicHook) -> PanicHook {
    Box::new(move |panic_info| {
        let summary = panic_info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("panic");
        let _ = append_crash_record(&crash_log, summary);
        previous(panic_info);
    })
}

pub fn install_panic_hook(crash_log: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(make_panic_hook(crash_log, previous));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::config_paths_from_root;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    struct PanicHookGuard(Option<PanicHook>);

    impl PanicHookGuard {
        fn install(hook: PanicHook) -> Self {
            let previous = std::panic::take_hook();
            std::panic::set_hook(hook);
            Self(Some(previous))
        }
    }

    impl Drop for PanicHookGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                std::panic::set_hook(previous);
            }
        }
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("rustnt-gui-recovery-{name}-{unique}")))
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

    #[test]
    fn inspect_reports_normal_start_without_marker() {
        let root = TestRoot::new("normal");
        let paths = config_paths_from_root(root.path().to_owned());
        assert_eq!(
            inspect(&paths).unwrap(),
            RecoveryState {
                previous_run_incomplete: false
            }
        );
    }

    #[test]
    fn marker_is_detected_and_removed_only_explicitly() {
        let root = TestRoot::new("marker");
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
        assert!(marker.path.exists());
        let leaked_marker_path = marker.path.clone();
        drop(marker);
        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        std::fs::remove_file(leaked_marker_path).unwrap();

        let marker = create_marker(&paths).unwrap();
        let marker_path = marker.path.clone();
        marker.remove().unwrap();
        assert!(!marker_path.exists());
        assert!(!inspect(&paths).unwrap().previous_run_incomplete);
    }

    #[cfg(windows)]
    #[test]
    fn current_process_marker_is_preserved_during_inspection() {
        let root = TestRoot::new("current-process-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
        let marker_path = marker.path.clone();
        let contents = std::fs::read_to_string(&marker_path).unwrap();

        assert!(contents
            .lines()
            .any(|line| line.starts_with("creation_time_100ns=")));

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        assert!(marker_path.exists());

        marker.remove().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn current_pid_with_mismatched_creation_time_is_stale() {
        let root = TestRoot::new("mismatched-current-process-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(
            &paths.marker_file,
            format!(
                "pid={}\nstarted_at=old\ncreation_time_100ns={}\n",
                std::process::id(),
                u64::MAX
            ),
        )
        .unwrap();

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        assert!(!paths.marker_file.exists());
    }

    #[cfg(windows)]
    #[test]
    fn exited_sidecar_marker_reports_recovery_and_is_cleaned() {
        let root = TestRoot::new("stale-sidecar");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let stale_pid = u32::MAX;
        let stale_path = root
            .path()
            .join(format!("gui.running.{stale_pid}.test.marker"));
        std::fs::write(&stale_path, format!("pid={stale_pid}\nstarted_at=old\n")).unwrap();

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        assert!(!stale_path.exists());
        assert!(!inspect(&paths).unwrap().previous_run_incomplete);
    }

    #[test]
    fn concurrent_instances_use_owned_sidecars_and_preserve_each_other() {
        let root = TestRoot::new("multi-instance");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = create_marker(&paths).unwrap();
        let second = create_marker(&paths).unwrap();

        assert_ne!(first.path, second.path);
        assert!(first.path.exists());
        assert!(second.path.exists());
        assert!(inspect(&paths).unwrap().previous_run_incomplete);

        let first_path = first.path.clone();
        first.remove().unwrap();
        assert!(!first_path.exists());
        assert!(second.path.exists());
        assert!(inspect(&paths).unwrap().previous_run_incomplete);

        let second_path = second.path.clone();
        second.remove().unwrap();
        assert!(!second_path.exists());
        assert!(!inspect(&paths).unwrap().previous_run_incomplete);
    }

    #[cfg(windows)]
    #[test]
    fn legacy_marker_is_detected_without_being_overwritten_or_removed() {
        let root = TestRoot::new("legacy-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let legacy_contents = format!("pid={}\nstarted_at=old\n", std::process::id());
        std::fs::write(&paths.marker_file, &legacy_contents).unwrap();

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        let marker = create_marker(&paths).unwrap();
        assert_ne!(marker.path, paths.marker_file);
        assert_eq!(
            std::fs::read_to_string(&paths.marker_file).unwrap(),
            legacy_contents
        );
        marker.remove().unwrap();
        assert!(paths.marker_file.exists());
    }

    #[cfg(windows)]
    #[test]
    fn exited_legacy_marker_is_cleaned_but_legacy_format_remains_supported() {
        let root = TestRoot::new("stale-legacy-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(
            &paths.marker_file,
            format!("pid={}\nstarted_at=old\n", u32::MAX),
        )
        .unwrap();

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        assert!(!paths.marker_file.exists());
    }

    #[test]
    fn failed_crash_log_append_is_reported_without_panicking() {
        let root = TestRoot::new("crash-log");
        std::fs::create_dir_all(root.path()).unwrap();
        let result = append_crash_record(root.path(), "panic summary");
        assert!(result.is_err());
    }

    #[test]
    fn panic_hook_ignores_log_failure_and_calls_previous_hook() {
        let _lock = PANIC_HOOK_LOCK.lock().unwrap();
        let root = TestRoot::new("hook-log-failure");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
        let marker_path = marker.path.clone();
        let previous_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&previous_calls);
        let hook = make_panic_hook(
            root.path().to_owned(),
            Box::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let _guard = PanicHookGuard::install(hook);

        let result = std::panic::catch_unwind(|| panic!("panic while log path is a directory"));

        assert!(result.is_err());
        assert_eq!(previous_calls.load(Ordering::SeqCst), 1);
        assert!(marker_path.exists());
        drop(marker);
    }

    #[test]
    fn panic_hook_writes_one_line_and_keeps_marker_after_panic() {
        let _lock = PANIC_HOOK_LOCK.lock().unwrap();
        let root = TestRoot::new("hook-line");
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
        let marker_path = marker.path.clone();
        let previous_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&previous_calls);
        let hook = make_panic_hook(
            paths.crash_log.clone(),
            Box::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let _guard = PanicHookGuard::install(hook);

        let result = std::panic::catch_unwind(|| panic!("first\r\nsecond\rthird\n"));

        assert!(result.is_err());
        assert_eq!(previous_calls.load(Ordering::SeqCst), 1);
        let log = std::fs::read_to_string(&paths.crash_log).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(log.contains("first  second third "));
        assert!(marker_path.exists());
        drop(marker);
    }

    #[test]
    fn crash_record_sanitizes_controls_and_caps_record_length() {
        let root = TestRoot::new("bounded-crash-log");
        std::fs::create_dir_all(root.path()).unwrap();
        let crash_log = root.path().join("gui-crash.log");
        let summary = format!(
            "prefix\0\t\r\n{}",
            "\u{4e16}".repeat(MAX_CRASH_RECORD_BYTES * 2)
        );

        append_crash_record(&crash_log, &summary).unwrap();

        let log = std::fs::read_to_string(crash_log).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(log.len() <= MAX_CRASH_RECORD_BYTES);
        assert!(log.lines().all(|line| !line.chars().any(char::is_control)));
        assert!(log.contains("prefix    "));
    }

    #[test]
    fn crash_record_includes_current_thread_name_and_identifier() {
        let root = TestRoot::new("thread-crash-log");
        std::fs::create_dir_all(root.path()).unwrap();
        let crash_log = root.path().join("gui-crash.log");
        let handle = std::thread::Builder::new()
            .name("task14-crash-thread".to_owned())
            .spawn({
                let crash_log = crash_log.clone();
                move || append_crash_record(&crash_log, "thread summary")
            })
            .unwrap();
        handle.join().unwrap().unwrap();

        let log = std::fs::read_to_string(crash_log).unwrap();
        assert!(log.contains("thread=task14-crash-thread"));
        assert!(log.contains("thread_id=ThreadId("));
        assert!(log.contains("thread summary"));
    }
}
