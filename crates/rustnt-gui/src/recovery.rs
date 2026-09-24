use crate::config::ConfigPaths;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

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
        match marker_liveness(marker_pid(&contents)) {
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
    let contents = format!(
        "pid={}\nstarted_at={}\n",
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

fn marker_pid(contents: &str) -> Option<u32> {
    contents.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|value| value.trim().parse::<u32>().ok())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerLiveness {
    Running,
    Stale,
    Unknown,
}

#[cfg(windows)]
fn marker_liveness(pid: Option<u32>) -> MarkerLiveness {
    let Some(pid) = pid else {
        return MarkerLiveness::Unknown;
    };
    if pid == std::process::id() {
        return MarkerLiveness::Running;
    }

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return if GetLastError() == ERROR_INVALID_PARAMETER {
                MarkerLiveness::Stale
            } else {
                MarkerLiveness::Unknown
            };
        }

        let mut exit_code = 0;
        let liveness = if GetExitCodeProcess(process, &mut exit_code) == 0 {
            MarkerLiveness::Unknown
        } else if exit_code == STILL_ACTIVE as u32 {
            MarkerLiveness::Running
        } else {
            MarkerLiveness::Stale
        };
        let _ = CloseHandle(process);
        liveness
    }
}

#[cfg(not(windows))]
fn marker_liveness(_pid: Option<u32>) -> MarkerLiveness {
    MarkerLiveness::Unknown
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

    #[test]
    fn current_process_marker_is_preserved_during_inspection() {
        let root = TestRoot::new("current-process-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
        let marker_path = marker.path.clone();

        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        assert!(marker_path.exists());

        marker.remove().unwrap();
    }

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

    #[test]
    fn legacy_marker_is_detected_without_being_overwritten_or_removed() {
        let root = TestRoot::new("legacy-marker");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let legacy_contents = "pid=old\nstarted_at=old\n";
        std::fs::write(&paths.marker_file, legacy_contents).unwrap();

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
