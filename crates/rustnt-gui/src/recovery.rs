use crate::config::ConfigPaths;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    Ok(RecoveryState {
        previous_run_incomplete: paths.marker_file.exists(),
    })
}

pub fn create_marker(paths: &ConfigPaths) -> Result<RuntimeMarker, RecoveryError> {
    std::fs::create_dir_all(&paths.root)
        .map_err(|error| RecoveryError::new("create runtime marker directory", error))?;
    let contents = format!(
        "pid={}\nstarted_at={}\n",
        std::process::id(),
        unix_timestamp()
    );
    std::fs::write(&paths.marker_file, contents)
        .map_err(|error| RecoveryError::new("write runtime marker", error))?;
    Ok(RuntimeMarker {
        path: paths.marker_file.clone(),
    })
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

pub fn append_crash_record(path: &Path, summary: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(
        file,
        "{}: {}",
        unix_timestamp(),
        summary.replace(['\r', '\n'], " ")
    )?;
    Ok(())
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
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.marker_file, "old_pid=123\nstarted_at=old\n").unwrap();
        assert!(inspect(&paths).unwrap().previous_run_incomplete);
        let marker = create_marker(&paths).unwrap();
        assert!(paths.marker_file.exists());
        drop(marker);
        assert!(paths.marker_file.exists());

        let marker = create_marker(&paths).unwrap();
        marker.remove().unwrap();
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
        assert!(paths.marker_file.exists());
        drop(marker);
    }

    #[test]
    fn panic_hook_writes_one_line_and_keeps_marker_after_panic() {
        let _lock = PANIC_HOOK_LOCK.lock().unwrap();
        let root = TestRoot::new("hook-line");
        let paths = config_paths_from_root(root.path().to_owned());
        let marker = create_marker(&paths).unwrap();
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
        assert!(paths.marker_file.exists());
        drop(marker);
    }
}
