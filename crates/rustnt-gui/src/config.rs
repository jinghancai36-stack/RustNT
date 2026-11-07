use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

pub const DEFAULT_WINDOW_WIDTH: f32 = 960.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 640.0;

const MIN_WINDOW_WIDTH: f32 = 320.0;
const MIN_WINDOW_HEIGHT: f32 = 240.0;
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Serializes the *whole* `save_config` pipeline inside this process.
///
/// `ReplaceFileW` is an atomic single-file replacement primitive, not a
/// concurrency control: two callers replacing the same target concurrently can
/// both observe `ERROR_UNABLE_TO_REMOVE_REPLACED` (1175). A process-level mutex
/// removes that race without changing the public API, the temporary-file
/// strategy, or the on-disk format. It does not coordinate *other processes*;
/// the bounded retry below covers transient cross-process contention.
static CONFIG_SAVE_MUTEX: Mutex<()> = Mutex::new(());

/// 1 initial attempt + at most 2 retries. Never unbounded.
const MAX_REPLACE_ATTEMPTS: u32 = 3;
/// Short backoff: long sleeps would hold `CONFIG_SAVE_MUTEX` and stall every
/// other save in the process, and cannot repair a permanent error anyway.
const REPLACE_RETRY_BACKOFF: Duration = Duration::from_millis(1);

// Win32 status codes (winerror.h). `raw_os_error()` yields these only on
// Windows; elsewhere the errno namespace must never be interpreted as Win32.
#[cfg(windows)]
const ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;
#[cfg(windows)]
const ERROR_UNABLE_TO_REMOVE_REPLACED: i32 = 1175;
#[cfg(windows)]
const ERROR_UNABLE_TO_MOVE_REPLACEMENT: i32 = 1176;
#[cfg(windows)]
const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;

/// Why a replacement attempt failed, derived from Microsoft's documented
/// `ReplaceFileW` partial-failure states.
///
/// Reference: <https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew>
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplaceFailureKind {
    /// Retryable: the documented failure state leaves BOTH the target and the
    /// temporary file intact under their original names, so re-running the
    /// identical replacement is safe and cannot discard a valid config.
    Transient,
    /// Not retryable: `ReplaceFileW` changed on-disk state (the target may be
    /// gone, or the replacement may have inherited streams/attributes).
    /// Repeating `ReplaceFileW` blindly can lose the only valid configuration.
    StateChanged,
    /// Not retryable: no evidence of a transient condition. In particular
    /// `ERROR_ACCESS_DENIED` (5) usually denotes a permanent ACL/read-only/
    /// delete-permission problem; retrying cannot fix it and only delays
    /// reporting the real cause.
    Permanent,
}

#[cfg(windows)]
fn classify_replace_failure(error: &std::io::Error) -> ReplaceFailureKind {
    // ERROR_ACCESS_DENIED (5): at the ReplaceFileW stage this is a permanent
    // ACL / read-only / delete-permission condition (the API requires write
    // access to the replaced file). Retrying cannot fix it and only delays
    // reporting the real cause. The one genuinely transient access-denied case
    // is the create-path TOCTOU race, already handled stage-specifically in
    // `replace_config_file` by re-checking `target.exists()` and falling back
    // to the replacement path. It is deliberately NOT in the retry set.
    if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
        return ReplaceFailureKind::Permanent;
    }

    match error.raw_os_error() {
        // 1175: "The replaced file could not be deleted. The replaced and
        // replacement files retain their original file names." This is exactly
        // the status produced when concurrent writers collide on one target.
        // 32: a sharing violation means another process briefly holds a handle
        // to one of the files. Microsoft states that for "any other error" the
        // replaced and replacement files retain their original file names, so
        // a short bounded retry is safe and cannot risk the configuration.
        Some(ERROR_UNABLE_TO_REMOVE_REPLACED) | Some(ERROR_SHARING_VIOLATION) => {
            ReplaceFailureKind::Transient
        }
        // 1176 (no backup file, our call shape): "the replaced file no longer
        // exists and the replacement file exists under its original name" — the
        // target is GONE. 1177: the replacement keeps its name but inherited
        // the replaced file's streams/attributes while the replaced file now
        // exists under a different name. Neither state may be fed back into
        // ReplaceFileW: the target path is unreliable and the temporary file
        // may be the only surviving configuration.
        Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT) | Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2) => {
            ReplaceFailureKind::StateChanged
        }
        _ => ReplaceFailureKind::Permanent,
    }
}

#[cfg(not(windows))]
fn classify_replace_failure(_error: &std::io::Error) -> ReplaceFailureKind {
    // std::fs::rename has no documented partial-failure states to recover from.
    ReplaceFailureKind::Permanent
}

/// Retrying is only legitimate while both files still exist under their
/// original names. If the target disappeared (1176/1177 state) we must stop,
/// preserve the temporary file, and report the failure instead of repeating
/// `ReplaceFileW` against a missing target.
fn replacement_files_intact(temporary: &Path, target: &Path) -> bool {
    temporary.exists() && target.exists()
}

fn describe_replace_failure(
    error: &std::io::Error,
    kind: ReplaceFailureKind,
    attempts: u32,
    temporary: &Path,
    target: &Path,
) -> ConfigError {
    let state = match (target.exists(), temporary.exists()) {
        (true, true) => {
            "target retains the previous valid configuration; replacement file is intact"
        }
        (true, false) => {
            "target retains the previous valid configuration; replacement file is missing"
        }
        (false, true) => {
            "target is missing; replacement file is preserved as the only complete configuration (a subsequent save recreates the target from a fresh temporary file)"
        }
        (false, false) => "neither target nor replacement file is present",
    };
    ConfigError::new(
        "replace configuration",
        format!(
            "{} (classification: {:?}, {}/{} attempt(s); {}; preserved replacement: {})",
            error,
            kind,
            attempts,
            MAX_REPLACE_ATTEMPTS,
            state,
            temporary.display()
        ),
    )
}

/// Cleanup after a failed replacement.
///
/// The temporary file is only removed when the target still exists, i.e. the
/// previous valid configuration is intact. If the target is gone (the 1176
/// state), the temporary file is *deliberately kept*: deleting it would destroy
/// the only complete configuration left on disk. Keeping it is a recovery
/// artifact, not a silent success; the caller still receives the error.
fn cleanup_after_failed_replace(temporary: &Path, target: &Path) {
    if target.exists() {
        let _ = std::fs::remove_file(temporary);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    fn from_file_value(value: &str) -> (Self, Option<String>) {
        match value {
            "dark" => (Self::Dark, None),
            "light" => (Self::Light, None),
            _ => (
                Self::Dark,
                Some(format!("Invalid theme; using dark instead of {value}")),
            ),
        }
    }

    fn as_file_value(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuiConfig {
    pub theme: ThemeMode,
    pub window_width: f32,
    pub window_height: f32,
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            window_width: DEFAULT_WINDOW_WIDTH,
            window_height: DEFAULT_WINDOW_HEIGHT,
            window_x: None,
            window_y: None,
        }
    }
}

impl GuiConfig {
    fn validate(file: FileConfig) -> (Self, Vec<String>) {
        let (theme, theme_notice) = ThemeMode::from_file_value(&file.theme);
        let mut notices = theme_notice.into_iter().collect::<Vec<_>>();
        let window_width = if file.window_width.is_finite() && file.window_width >= MIN_WINDOW_WIDTH
        {
            file.window_width
        } else {
            notices.push("Invalid window size; using the default dimensions".to_owned());
            DEFAULT_WINDOW_WIDTH
        };
        let window_height =
            if file.window_height.is_finite() && file.window_height >= MIN_WINDOW_HEIGHT {
                file.window_height
            } else {
                if !notices.iter().any(|notice| notice.contains("window size")) {
                    notices.push("Invalid window size; using the default dimensions".to_owned());
                }
                DEFAULT_WINDOW_HEIGHT
            };
        let window_x = finite_position(file.window_x, &mut notices);
        let window_y = finite_position(file.window_y, &mut notices);
        (
            Self {
                theme,
                window_width,
                window_height,
                window_x,
                window_y,
            },
            notices,
        )
    }
}

fn finite_position(value: Option<f32>, notices: &mut Vec<String>) -> Option<f32> {
    match value {
        Some(position) if position.is_finite() => Some(position),
        Some(_) => {
            notices.push("Invalid window position; ignoring it".to_owned());
            None
        }
        None => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: PathBuf,
    pub config_file: PathBuf,
    pub marker_file: PathBuf,
    pub crash_log: PathBuf,
    pub shell_snapshot_file: PathBuf,
    pub shell_recovery_log: PathBuf,
    pub shell_session_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoad {
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub operation: String,
    pub message: String,
}

impl ConfigError {
    fn new(operation: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation: operation.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(error: std::io::Error) -> Self {
        Self::new("configuration I/O", error)
    }
}

impl From<toml::ser::Error> for ConfigError {
    fn from(error: toml::ser::Error) -> Self {
        Self::new("configuration serialization", error)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FileConfig {
    theme: String,
    window_width: f32,
    window_height: f32,
    #[serde(default)]
    window_x: Option<f32>,
    #[serde(default)]
    window_y: Option<f32>,
}

impl FileConfig {
    fn from_config(config: &GuiConfig) -> Self {
        Self {
            theme: config.theme.as_file_value().to_owned(),
            window_width: config.window_width,
            window_height: config.window_height,
            window_x: config.window_x,
            window_y: config.window_y,
        }
    }
}

pub fn config_paths_from_appdata() -> Result<ConfigPaths, ConfigError> {
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| ConfigError::new("resolve APPDATA", "APPDATA is missing or empty"))?;
    if appdata.is_empty() {
        return Err(ConfigError::new(
            "resolve APPDATA",
            "APPDATA is missing or empty",
        ));
    }
    Ok(config_paths_from_root(
        PathBuf::from(appdata).join("RustNT"),
    ))
}

pub fn config_paths_from_root(root: PathBuf) -> ConfigPaths {
    ConfigPaths {
        config_file: root.join("gui.toml"),
        marker_file: root.join("gui.running"),
        crash_log: root.join("gui-crash.log"),
        shell_snapshot_file: root.join("shell_snapshot.toml"),
        shell_recovery_log: root.join("shell_recovery.log"),
        shell_session_file: root.join("shell.session"),
        root,
    }
}

pub fn load_config(paths: &ConfigPaths) -> Result<(GuiConfig, ConfigLoad), ConfigError> {
    let encoded = match std::fs::read_to_string(&paths.config_file) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((GuiConfig::default(), ConfigLoad { notice: None }))
        }
        Err(error) => return Err(ConfigError::new("read configuration", error)),
    };
    let file_config: FileConfig = match toml::from_str(&encoded) {
        Ok(file_config) => file_config,
        Err(error) => {
            return Ok((
                GuiConfig::default(),
                ConfigLoad {
                    notice: Some(format!(
                        "Invalid configuration; using default settings: {error}"
                    )),
                },
            ))
        }
    };
    let (config, notices) = GuiConfig::validate(file_config);
    Ok((
        config,
        ConfigLoad {
            notice: (!notices.is_empty()).then(|| notices.join("; ")),
        },
    ))
}

pub fn save_config(paths: &ConfigPaths, config: &GuiConfig) -> Result<(), ConfigError> {
    save_config_with_replacer(paths, config, replace_config_file)
}

/// Full save pipeline, with the replacement primitive injectable so tests can
/// exercise transient/partial-failure states without touching production
/// behaviour. The process-level mutex covers the entire pipeline (directory,
/// serialization, unique temporary file, atomic replacement and failure
/// cleanup), not just the `ReplaceFileW` call.
fn save_config_with_replacer<F>(
    paths: &ConfigPaths,
    config: &GuiConfig,
    replacer: F,
) -> Result<(), ConfigError>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
{
    // A poisoned mutex means an earlier save panicked while holding it. That is
    // not a save failure we may silently convert into success, but keeping the
    // lock poisoned would disable saving for the rest of the process; recovering
    // the guard lets the next save start from a fresh, fully-written temporary.
    let _save_guard = CONFIG_SAVE_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    std::fs::create_dir_all(&paths.root)
        .map_err(|error| ConfigError::new("create configuration directory", error))?;
    let encoded = toml::to_string_pretty(&FileConfig::from_config(config))
        .map_err(|error| ConfigError::new("serialize configuration", error))?;
    let temporary = write_temporary_config(&paths.config_file, &encoded)
        .map_err(|error| ConfigError::new("write temporary configuration", error))?;

    let mut attempt = 0;
    loop {
        attempt += 1;
        match replacer(&temporary, &paths.config_file) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let kind = classify_replace_failure(&error);
                let can_retry = kind == ReplaceFailureKind::Transient
                    && attempt < MAX_REPLACE_ATTEMPTS
                    && replacement_files_intact(&temporary, &paths.config_file);
                if can_retry {
                    // Files are untouched between attempts: the same temporary
                    // file is retried against the same intact target.
                    std::thread::sleep(REPLACE_RETRY_BACKOFF);
                    continue;
                }
                let failure =
                    describe_replace_failure(&error, kind, attempt, &temporary, &paths.config_file);
                cleanup_after_failed_replace(&temporary, &paths.config_file);
                return Err(failure);
            }
        }
    }
}

fn temporary_config_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gui.toml");
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

fn write_temporary_config(target: &Path, encoded: &str) -> std::io::Result<PathBuf> {
    for _ in 0..16 {
        let temporary = temporary_config_path(target);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(encoded.as_bytes()) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        return Ok(temporary);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary configuration path",
    ))
}

#[cfg(not(windows))]
fn replace_config_file(
    temporary: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_config_file(
    temporary: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    if !target.exists() {
        return match std::fs::rename(temporary, target) {
            Ok(()) => Ok(()),
            Err(_error) if target.exists() => replace_existing_file(temporary, target),
            Err(error) => Err(error),
        };
    }
    replace_existing_file(temporary, target)
}

#[cfg(windows)]
fn replace_existing_file(
    temporary: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("rustnt-gui-config-{name}-{unique}")))
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
    fn defaults_use_safe_dark_window_settings() {
        let config = GuiConfig::default();
        assert_eq!(config.theme, ThemeMode::Dark);
        assert_eq!(config.window_width, DEFAULT_WINDOW_WIDTH);
        assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
        assert_eq!(config.window_x, None);
        assert_eq!(config.window_y, None);
    }

    #[test]
    fn missing_config_returns_defaults_without_notice() {
        let root = TestRoot::new("missing");
        let paths = config_paths_from_root(root.path().to_owned());
        let (config, load) = load_config(&paths).unwrap();
        assert_eq!(config, GuiConfig::default());
        assert_eq!(load.notice, None);
    }

    #[test]
    fn valid_toml_round_trips_theme_dimensions_and_position() {
        let root = TestRoot::new("round-trip");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        let expected = GuiConfig {
            theme: ThemeMode::Light,
            window_width: 1280.0,
            window_height: 720.0,
            window_x: Some(40.0),
            window_y: Some(60.0),
        };
        save_config(&paths, &expected).unwrap();
        let (actual, load) = load_config(&paths).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(load.notice, None);
        assert_no_temporary_configs(&paths);
    }

    #[test]
    fn saving_twice_replaces_existing_config_and_cleans_temporary_file() {
        let root = TestRoot::new("overwrite");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = GuiConfig::default();
        let second = GuiConfig {
            theme: ThemeMode::Light,
            window_width: 1440.0,
            window_height: 900.0,
            window_x: Some(12.0),
            window_y: Some(24.0),
        };

        save_config(&paths, &first).unwrap();
        save_config(&paths, &second).unwrap();

        let (actual, load) = load_config(&paths).unwrap();
        assert_eq!(actual, second);
        assert_eq!(load.notice, None);
        assert_no_temporary_configs(&paths);
    }

    #[test]
    fn concurrent_temporary_config_paths_are_unique_in_the_config_directory() {
        let root = TestRoot::new("temporary-paths");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        let handles = (0..32)
            .map(|_| {
                let paths = Arc::clone(&paths);
                std::thread::spawn(move || temporary_config_path(&paths.config_file))
            })
            .collect::<Vec<_>>();
        let temporary_paths = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        let unique_paths = temporary_paths
            .iter()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_paths.len(), temporary_paths.len());
        assert!(temporary_paths
            .iter()
            .all(|path| path.parent() == paths.config_file.parent()));
        assert!(temporary_paths.iter().all(|path| is_temporary_config(path)));
    }

    #[test]
    fn concurrent_save_config_calls_leave_valid_toml_without_temporary_files() {
        let root = TestRoot::new("concurrent-save");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles = (0..16)
            .map(|index| {
                let paths = Arc::clone(&paths);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    save_config(
                        &paths,
                        &GuiConfig {
                            theme: if index % 2 == 0 {
                                ThemeMode::Dark
                            } else {
                                ThemeMode::Light
                            },
                            window_width: 800.0 + index as f32,
                            window_height: 600.0 + index as f32,
                            window_x: Some(index as f32),
                            window_y: Some((index * 2) as f32),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let encoded = std::fs::read_to_string(&paths.config_file).unwrap();
        toml::from_str::<toml::Value>(&encoded).unwrap();
        assert_no_temporary_configs(&paths);
    }

    fn is_temporary_config(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("gui.toml.") && name.ends_with(".tmp"))
    }

    fn assert_no_temporary_configs(paths: &ConfigPaths) {
        let entries = std::fs::read_dir(&paths.root).unwrap();
        assert!(entries
            .filter_map(Result::ok)
            .all(|entry| { !is_temporary_config(&entry.path()) }));
    }

    #[test]
    fn invalid_theme_and_geometry_fall_back_without_panicking() {
        let root = TestRoot::new("invalid");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::write(
            &paths.config_file,
            "theme = \"neon\"\nwindow_width = -1.0\nwindow_height = inf\nwindow_x = nan\n",
        )
        .unwrap();
        let (config, load) = load_config(&paths).unwrap();
        assert_eq!(config.theme, ThemeMode::Dark);
        assert_eq!(config.window_width, DEFAULT_WINDOW_WIDTH);
        assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
        let notice = load.notice.unwrap();
        assert!(notice.contains("theme") && notice.contains("dark"));
        assert!(notice.contains("window size"));
        assert!(notice.contains("window position"));
    }

    #[test]
    fn invalid_toml_returns_defaults_with_configuration_notice() {
        let root = TestRoot::new("parse-error");
        std::fs::create_dir_all(root.path()).unwrap();
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::write(&paths.config_file, "this is not valid toml =").unwrap();
        let (config, load) = load_config(&paths).unwrap();
        assert_eq!(config, GuiConfig::default());
        let notice = load.notice.unwrap();
        assert!(notice.contains("configuration") && notice.contains("default"));
    }

    #[test]
    fn root_constructor_uses_expected_filenames_without_creating_directories() {
        let root = TestRoot::new("paths");
        let paths = config_paths_from_root(root.path().to_owned());
        assert_eq!(paths.root, root.path());
        assert_eq!(paths.config_file, paths.root.join("gui.toml"));
        assert_eq!(paths.marker_file, paths.root.join("gui.running"));
        assert_eq!(paths.crash_log, paths.root.join("gui-crash.log"));
        assert!(!paths.root.exists());
    }

    #[test]
    fn root_constructor_adds_task17a_paths_without_changing_existing_paths() {
        let root = TestRoot::new("task17a-paths");
        let paths = config_paths_from_root(root.path().to_owned());
        assert_eq!(paths.config_file, root.path().join("gui.toml"));
        assert_eq!(paths.marker_file, root.path().join("gui.running"));
        assert_eq!(paths.crash_log, root.path().join("gui-crash.log"));
        assert_eq!(
            paths.shell_snapshot_file,
            root.path().join("shell_snapshot.toml")
        );
        assert_eq!(
            paths.shell_recovery_log,
            root.path().join("shell_recovery.log")
        );
        assert_eq!(paths.shell_session_file, root.path().join("shell.session"));
        assert!(!root.path().exists());
    }

    // ---------------------------------------------------------------------
    // Task 01B.4B — config save reliability regression tests.
    // Every test below uses an isolated TestRoot; none touches real APPDATA.
    // ---------------------------------------------------------------------

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Barrier;
    use std::time::Duration;

    const CONCURRENT_THREADS: usize = 16;

    fn sample_config(seed: usize) -> GuiConfig {
        GuiConfig {
            theme: if seed.is_multiple_of(2) {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            window_width: 800.0 + seed as f32,
            window_height: 600.0 + seed as f32,
            window_x: Some(seed as f32),
            window_y: Some((seed * 2) as f32),
        }
    }

    fn injected_error(code: i32) -> std::io::Error {
        std::io::Error::from_raw_os_error(code)
    }

    fn temporary_config_files(paths: &ConfigPaths) -> Vec<PathBuf> {
        std::fs::read_dir(&paths.root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_temporary_config(path))
            .collect()
    }

    fn spawn_saves(paths: &Arc<ConfigPaths>, seeds: Vec<usize>) -> Vec<Result<(), ConfigError>> {
        let barrier = Arc::new(Barrier::new(seeds.len()));
        let handles = seeds
            .into_iter()
            .map(|seed| {
                let paths = Arc::clone(paths);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    save_config(&paths, &sample_config(seed))
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    }

    /// (1) 16 threads, multiple rounds: every save must succeed.
    #[test]
    fn sixteen_threads_multiple_rounds_all_succeed() {
        let root = TestRoot::new("stress-rounds");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        const ROUNDS: usize = 12;
        let committed = (0..CONCURRENT_THREADS * ROUNDS).collect::<Vec<_>>();
        let barrier = Arc::new(Barrier::new(CONCURRENT_THREADS));
        let handles = (0..CONCURRENT_THREADS)
            .map(|thread| {
                let paths = Arc::clone(&paths);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut results = Vec::new();
                    for round in 0..ROUNDS {
                        barrier.wait();
                        let seed = round * CONCURRENT_THREADS + thread;
                        // Keep looping even if one save fails, so no thread can
                        // leave the other threads stuck at the barrier.
                        results.push(save_config(&paths, &sample_config(seed)));
                    }
                    results
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.len(), committed.len());
        for result in &results {
            assert!(result.is_ok(), "concurrent save must succeed: {result:?}");
        }
        assert_no_temporary_configs(&paths);
    }

    /// (2) A successful save produces parseable TOML.
    #[test]
    fn saved_config_is_parseable_toml() {
        let root = TestRoot::new("parseable");
        let paths = config_paths_from_root(root.path().to_owned());
        let expected = sample_config(7);
        save_config(&paths, &expected).unwrap();

        let encoded = std::fs::read_to_string(&paths.config_file).unwrap();
        let value = toml::from_str::<toml::Value>(&encoded).unwrap();
        assert_eq!(value.get("theme").unwrap().as_str(), Some("light"));
        let file_config = toml::from_str::<FileConfig>(&encoded).unwrap();
        assert_eq!(file_config.window_width, expected.window_width);
        let (loaded, load) = load_config(&paths).unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(load.notice, None);
        assert_no_temporary_configs(&paths);
    }

    /// (3) The final configuration is one of the values actually submitted.
    #[test]
    fn final_config_is_one_of_the_committed_values() {
        let root = TestRoot::new("final-committed");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        let committed = (0..CONCURRENT_THREADS)
            .map(sample_config)
            .collect::<Vec<_>>();

        for result in spawn_saves(&paths, (0..CONCURRENT_THREADS).collect()) {
            assert!(result.is_ok(), "concurrent save must succeed: {result:?}");
        }

        let (final_config, load) = load_config(&paths).unwrap();
        assert_eq!(load.notice, None);
        assert!(
            committed.contains(&final_config),
            "final configuration must be a committed value, got {final_config:?}"
        );
        assert_no_temporary_configs(&paths);
    }

    /// (4) No temporary files survive a concurrent save batch.
    #[test]
    fn concurrent_saves_leave_no_temporary_files() {
        let root = TestRoot::new("no-residue");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        for result in spawn_saves(&paths, (0..CONCURRENT_THREADS).collect()) {
            assert!(result.is_ok(), "concurrent save must succeed: {result:?}");
        }
        assert!(temporary_config_files(&paths).is_empty());
        assert_no_temporary_configs(&paths);
    }

    /// (5) An injected replacement failure leaves the original config readable.
    #[test]
    fn injected_replace_failure_preserves_existing_config() {
        let root = TestRoot::new("injected-failure");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = sample_config(1);
        save_config(&paths, &original).unwrap();
        let original_bytes = std::fs::read(&paths.config_file).unwrap();

        let attempts = AtomicU32::new(0);
        let result = save_config_with_replacer(&paths, &sample_config(2), |_, _| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(injected_error(5)) // ERROR_ACCESS_DENIED: permanent, no retry
        });

        let error = result.expect_err("injected replacement failure must be returned");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(error.operation, "replace configuration");
        assert!(error.message.contains("os error 5"));
        assert!(error.message.contains("1/3"));
        assert_eq!(std::fs::read(&paths.config_file).unwrap(), original_bytes);
        let (loaded, load) = load_config(&paths).unwrap();
        assert_eq!(loaded, original);
        assert_eq!(load.notice, None);
        assert_no_temporary_configs(&paths);
    }

    /// (6) A retryable error exhausts the bounded budget and returns the
    /// original OS error with full context instead of a silent success.
    #[cfg(windows)]
    #[test]
    fn retryable_error_exhausts_bounded_retry_budget() {
        let root = TestRoot::new("retry-budget");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = sample_config(1);
        save_config(&paths, &original).unwrap();

        let attempts = AtomicU32::new(0);
        let result = save_config_with_replacer(&paths, &sample_config(2), |_, _| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(injected_error(ERROR_UNABLE_TO_REMOVE_REPLACED))
        });

        let error = result.expect_err("exhausted retries must return an error");
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_REPLACE_ATTEMPTS);
        assert_eq!(error.operation, "replace configuration");
        assert!(error.message.contains("os error 1175"));
        assert!(error.message.contains("3/3"));
        assert!(error.message.contains("Transient"));
        assert!(error.message.contains("previous valid configuration"));
        let (loaded, _) = load_config(&paths).unwrap();
        assert_eq!(loaded, original);
        assert_no_temporary_configs(&paths);
    }

    /// (7) Non-retryable errors return immediately, including ACCESS_DENIED
    /// and the partial-move states 1176 / 1177.
    #[cfg(windows)]
    #[test]
    fn non_retryable_errors_return_immediately() {
        for code in [
            ERROR_ACCESS_DENIED,
            ERROR_UNABLE_TO_MOVE_REPLACEMENT,
            ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
            2, // ERROR_FILE_NOT_FOUND — unknown status, must not be retried
        ] {
            let root = TestRoot::new("non-retryable");
            let paths = config_paths_from_root(root.path().to_owned());
            let original = sample_config(1);
            save_config(&paths, &original).unwrap();

            let attempts = AtomicU32::new(0);
            let result = save_config_with_replacer(&paths, &sample_config(2), |_, _| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(injected_error(code))
            });

            let error = result.expect_err("permanent failure must be returned");
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "error {code} must not be retried"
            );
            assert!(error.message.contains("1/3"), "{error}");
            let (loaded, _) = load_config(&paths).unwrap();
            assert_eq!(loaded, original);
            assert_no_temporary_configs(&paths);
        }
    }

    /// (8) Concurrent creation when the target does not exist initially.
    #[test]
    fn concurrent_creation_when_target_is_missing() {
        let root = TestRoot::new("create-race");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        assert!(!paths.config_file.exists());

        let committed = (0..CONCURRENT_THREADS)
            .map(sample_config)
            .collect::<Vec<_>>();
        for result in spawn_saves(&paths, (0..CONCURRENT_THREADS).collect()) {
            assert!(result.is_ok(), "concurrent create must succeed: {result:?}");
        }

        assert!(paths.config_file.exists());
        let encoded = std::fs::read_to_string(&paths.config_file).unwrap();
        toml::from_str::<toml::Value>(&encoded).unwrap();
        let (final_config, load) = load_config(&paths).unwrap();
        assert_eq!(load.notice, None);
        assert!(committed.contains(&final_config));
        assert_no_temporary_configs(&paths);
    }

    /// (9) Concurrent replacement when the target already exists.
    #[test]
    fn concurrent_replacement_when_target_exists() {
        let root = TestRoot::new("replace-race");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        let seed = sample_config(99);
        save_config(&paths, &seed).unwrap();
        assert!(paths.config_file.exists());
        let original_bytes = std::fs::read(&paths.config_file).unwrap();

        let committed = (0..CONCURRENT_THREADS)
            .map(sample_config)
            .collect::<Vec<_>>();
        for result in spawn_saves(&paths, (0..CONCURRENT_THREADS).collect()) {
            assert!(
                result.is_ok(),
                "concurrent replace must succeed: {result:?}"
            );
        }

        let encoded = std::fs::read_to_string(&paths.config_file).unwrap();
        toml::from_str::<toml::Value>(&encoded).unwrap();
        let (final_config, load) = load_config(&paths).unwrap();
        assert_eq!(load.notice, None);
        assert!(committed.contains(&final_config));
        assert_ne!(std::fs::read(&paths.config_file).unwrap(), original_bytes);
        assert_no_temporary_configs(&paths);
    }

    /// Bounded retry can actually recover: two transient failures, then success.
    #[cfg(windows)]
    #[test]
    fn retryable_error_recovers_on_a_later_attempt() {
        let root = TestRoot::new("retry-recovers");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = sample_config(1);
        save_config(&paths, &original).unwrap();
        let expected = sample_config(2);

        let attempts = AtomicU32::new(0);
        let result = save_config_with_replacer(&paths, &expected, |temporary, target| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt < MAX_REPLACE_ATTEMPTS {
                Err(injected_error(ERROR_UNABLE_TO_REMOVE_REPLACED))
            } else {
                replace_config_file(temporary, target)
            }
        });

        assert!(result.is_ok(), "third attempt must succeed: {result:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_REPLACE_ATTEMPTS);
        let (loaded, load) = load_config(&paths).unwrap();
        assert_eq!(loaded, expected);
        assert_eq!(load.notice, None);
        assert_no_temporary_configs(&paths);
    }

    /// 1176/1177 change on-disk state. Never repeat ReplaceFileW, never
    /// delete the only surviving complete configuration.
    #[cfg(windows)]
    #[test]
    fn partial_move_state_preserves_only_surviving_configuration() {
        let root = TestRoot::new("partial-move");
        let paths = config_paths_from_root(root.path().to_owned());
        save_config(&paths, &sample_config(1)).unwrap();
        let expected = sample_config(2);

        let attempts = AtomicU32::new(0);
        let result = save_config_with_replacer(&paths, &expected, |temporary, target| {
            attempts.fetch_add(1, Ordering::SeqCst);
            // Simulate the documented 1176 state (no backup): the target is
            // gone, the replacement file still exists under its own name.
            let _ = std::fs::remove_file(target);
            assert!(temporary.exists(), "replacement file must survive");
            Err(injected_error(ERROR_UNABLE_TO_MOVE_REPLACEMENT))
        });

        let error = result.expect_err("state-changed failure must be reported");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a state-changed error must never repeat ReplaceFileW"
        );
        assert!(!paths.config_file.exists());
        assert!(error.message.contains("StateChanged"));
        assert!(error.message.contains("preserved"));
        let preserved = temporary_config_files(&paths);
        assert_eq!(preserved.len(), 1, "the only valid config must be kept");
        let encoded = std::fs::read_to_string(&preserved[0]).unwrap();
        let file_config = toml::from_str::<FileConfig>(&encoded).unwrap();
        assert_eq!(file_config.theme, expected.theme.as_file_value());
        assert_eq!(file_config.window_width, expected.window_width);
    }

    /// Direct evidence of in-process mutual exclusion: while the replacement
    /// step is running, no second save may be inside its own replacement step.
    #[test]
    fn save_pipeline_is_serialized_within_the_process() {
        let root = TestRoot::new("serialized");
        let paths = Arc::new(config_paths_from_root(root.path().to_owned()));
        let in_flight = Arc::new(AtomicU32::new(0));
        let peak_in_flight = Arc::new(AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(CONCURRENT_THREADS));

        let handles = (0..CONCURRENT_THREADS)
            .map(|seed| {
                let paths = Arc::clone(&paths);
                let in_flight = Arc::clone(&in_flight);
                let peak_in_flight = Arc::clone(&peak_in_flight);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    save_config_with_replacer(&paths, &sample_config(seed), |temporary, target| {
                        let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        peak_in_flight.fetch_max(current, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(2));
                        let result = replace_config_file(temporary, target);
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        result
                    })
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(
            peak_in_flight.load(Ordering::SeqCst),
            1,
            "saves must be mutually excluded inside the process"
        );
        assert_no_temporary_configs(&paths);
    }

    /// Pure classification matrix (no filesystem writes).
    #[cfg(windows)]
    #[test]
    fn replace_failure_classification_follows_win32_documentation() {
        assert_eq!(
            classify_replace_failure(&injected_error(ERROR_UNABLE_TO_REMOVE_REPLACED)),
            ReplaceFailureKind::Transient
        );
        assert_eq!(
            classify_replace_failure(&injected_error(ERROR_SHARING_VIOLATION)),
            ReplaceFailureKind::Transient
        );
        assert_eq!(
            classify_replace_failure(&injected_error(ERROR_ACCESS_DENIED)),
            ReplaceFailureKind::Permanent
        );
        for code in [
            ERROR_UNABLE_TO_MOVE_REPLACEMENT,
            ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
        ] {
            assert_eq!(
                classify_replace_failure(&injected_error(code)),
                ReplaceFailureKind::StateChanged
            );
        }
        assert_eq!(
            classify_replace_failure(&injected_error(2)),
            ReplaceFailureKind::Permanent
        );
    }
}
