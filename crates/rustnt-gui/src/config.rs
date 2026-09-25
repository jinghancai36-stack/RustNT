use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

pub const DEFAULT_WINDOW_WIDTH: f32 = 960.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 640.0;

const MIN_WINDOW_WIDTH: f32 = 320.0;
const MIN_WINDOW_HEIGHT: f32 = 240.0;
static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    std::fs::create_dir_all(&paths.root)
        .map_err(|error| ConfigError::new("create configuration directory", error))?;
    let encoded = match toml::to_string_pretty(&FileConfig::from_config(config)) {
        Ok(encoded) => encoded,
        Err(error) => return Err(ConfigError::new("serialize configuration", error)),
    };
    let temporary = write_temporary_config(&paths.config_file, &encoded)
        .map_err(|error| ConfigError::new("write temporary configuration", error))?;
    if let Err(error) = replace_config_file(&temporary, &paths.config_file) {
        let _ = std::fs::remove_file(&temporary);
        return Err(ConfigError::new("replace configuration", error));
    }
    Ok(())
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
}
