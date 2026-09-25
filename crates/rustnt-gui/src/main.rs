#![cfg(windows)]

pub mod app;
pub mod config;
pub mod navigation;
pub mod recovery;

use std::fmt;
use std::sync::{Arc, Mutex};

use app::{viewport_for, RustNtApp, SharedConfig};
use config::{config_paths_from_appdata, load_config, save_config, ConfigError, GuiConfig};
use recovery::{create_marker, inspect, install_panic_hook, RecoveryError};

#[derive(Debug)]
enum GuiError {
    Config(ConfigError),
    Recovery(RecoveryError),
    ConfigLock,
    Eframe(eframe::Error),
}

impl fmt::Display for GuiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "configuration error: {error}"),
            Self::Recovery(error) => write!(formatter, "recovery error: {error}"),
            Self::ConfigLock => write!(formatter, "configuration lock is poisoned"),
            Self::Eframe(error) => write!(formatter, "GUI error: {error}"),
        }
    }
}

impl std::error::Error for GuiError {}

impl From<ConfigError> for GuiError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<RecoveryError> for GuiError {
    fn from(error: RecoveryError) -> Self {
        Self::Recovery(error)
    }
}

impl From<eframe::Error> for GuiError {
    fn from(error: eframe::Error) -> Self {
        Self::Eframe(error)
    }
}

fn startup_config(loaded: GuiConfig, recovery: recovery::RecoveryState) -> GuiConfig {
    if recovery.previous_run_incomplete {
        GuiConfig::default()
    } else {
        loaded
    }
}

fn aggregate_cleanup_errors(errors: impl IntoIterator<Item = GuiError>) -> Result<(), GuiError> {
    errors
        .into_iter()
        .min_by_key(cleanup_error_priority)
        .map_or(Ok(()), Err)
}

fn cleanup_error_priority(error: &GuiError) -> u8 {
    match error {
        GuiError::Config(_) | GuiError::ConfigLock => 0,
        GuiError::Recovery(_) => 1,
        GuiError::Eframe(_) => 2,
    }
}

fn cleanup_after_normal_exit(
    shared: &SharedConfig,
    save: impl FnOnce(&GuiConfig) -> Result<(), ConfigError>,
    remove: impl FnOnce() -> Result<(), RecoveryError>,
) -> Result<(), GuiError> {
    let final_config = match shared.lock() {
        Ok(config) => Ok(config.clone()),
        Err(_) => Err(GuiError::ConfigLock),
    };
    let mut cleanup_errors = Vec::new();
    match final_config {
        Ok(config) => {
            if let Err(error) = save(&config) {
                cleanup_errors.push(error.into());
            }
        }
        Err(error) => cleanup_errors.push(error),
    }
    if let Err(error) = remove() {
        cleanup_errors.push(error.into());
    }
    aggregate_cleanup_errors(cleanup_errors)
}

fn finish_run_native(
    result: Result<(), eframe::Error>,
    cleanup: impl FnOnce() -> Result<(), GuiError>,
) -> Result<(), GuiError> {
    match result {
        Ok(()) => cleanup(),
        Err(error) => Err(GuiError::Eframe(error)),
    }
}

fn run_gui() -> Result<(), GuiError> {
    let paths = config_paths_from_appdata()?;
    std::fs::create_dir_all(&paths.root).map_err(ConfigError::from)?;
    let recovery = inspect(&paths)?;
    let (loaded, load) = load_config(&paths)?;
    let config = startup_config(loaded, recovery);
    let marker = create_marker(&paths)?;
    install_panic_hook(paths.crash_log.clone());
    let shared: SharedConfig = Arc::new(Mutex::new(config.clone()));
    let options = eframe::NativeOptions {
        viewport: viewport_for(&config, recovery),
        ..Default::default()
    };
    let shared_for_app = Arc::clone(&shared);
    let paths_for_app = paths.clone();
    let result = eframe::run_native(
        "RustNT",
        options,
        Box::new(move |_creation_context| {
            Ok(Box::new(RustNtApp::new(
                shared_for_app,
                paths_for_app,
                recovery,
                load.notice,
            )))
        }),
    );
    finish_run_native(result, || {
        cleanup_after_normal_exit(
            &shared,
            |final_config| save_config(&paths, final_config),
            || marker.remove(),
        )
    })
}

fn main() {
    if let Err(error) = run_gui() {
        eprintln!("RustNT GUI failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_cleanup_errors, cleanup_after_normal_exit, finish_run_native, startup_config,
        GuiError,
    };
    use crate::config::{ConfigError, GuiConfig, ThemeMode};
    use crate::recovery::{RecoveryError, RecoveryState};
    use std::sync::{Arc, Mutex};

    fn recovery_error() -> RecoveryError {
        RecoveryError {
            operation: "remove runtime marker".to_owned(),
            message: "access denied".to_owned(),
        }
    }

    fn config_error() -> ConfigError {
        ConfigError {
            operation: "save configuration".to_owned(),
            message: "disk full".to_owned(),
        }
    }

    #[test]
    fn normal_cleanup_clones_before_save_and_removes_after_save_success() {
        let shared = Arc::new(Mutex::new(GuiConfig::default()));
        let shared_for_save = Arc::clone(&shared);
        let mut remove_called = false;

        let result = cleanup_after_normal_exit(
            &shared,
            |config| {
                assert_eq!(config, &GuiConfig::default());
                assert!(shared_for_save.try_lock().is_ok());
                Ok(())
            },
            || {
                remove_called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(remove_called);
    }

    #[test]
    fn normal_cleanup_removes_after_save_failure_and_prioritizes_config_error() {
        let shared = Arc::new(Mutex::new(GuiConfig::default()));
        let mut remove_called = false;

        let result = cleanup_after_normal_exit(
            &shared,
            |_| Err(config_error()),
            || {
                remove_called = true;
                Err(recovery_error())
            },
        )
        .expect_err("save failure should be returned");

        assert!(remove_called);
        assert!(matches!(result, GuiError::Config(_)));
    }

    #[test]
    fn normal_cleanup_prioritizes_lock_error_and_still_removes_marker() {
        let shared = Arc::new(Mutex::new(GuiConfig::default()));
        let shared_for_poison = Arc::clone(&shared);
        let _ = std::panic::catch_unwind(move || {
            let _guard = shared_for_poison.lock().unwrap();
            panic!("poison configuration lock");
        });
        let mut save_called = false;
        let mut remove_called = false;

        let result = cleanup_after_normal_exit(
            &shared,
            |_| {
                save_called = true;
                Ok(())
            },
            || {
                remove_called = true;
                Err(recovery_error())
            },
        )
        .expect_err("lock failure should be returned");

        assert!(!save_called);
        assert!(remove_called);
        assert!(matches!(result, GuiError::ConfigLock));
    }

    #[test]
    fn run_native_error_skips_normal_cleanup_and_preserves_marker() {
        let mut cleanup_called = false;
        let result = finish_run_native(
            Err(eframe::Error::AppCreation(Box::new(std::io::Error::other(
                "window creation failed",
            )))),
            || {
                cleanup_called = true;
                Ok(())
            },
        )
        .expect_err("run_native errors should be returned");

        assert!(!cleanup_called);
        assert!(matches!(result, GuiError::Eframe(_)));
    }

    #[test]
    fn cleanup_returns_configuration_error_before_marker_error() {
        let marker_error = GuiError::Recovery(RecoveryError {
            operation: "remove runtime marker".to_owned(),
            message: "access denied".to_owned(),
        });

        let error = aggregate_cleanup_errors(vec![marker_error, GuiError::ConfigLock])
            .expect_err("cleanup should report its highest-priority error");

        assert!(matches!(error, GuiError::ConfigLock));
    }

    #[test]
    fn recovery_uses_safe_defaults_and_normal_start_uses_loaded_settings() {
        let loaded = GuiConfig {
            theme: ThemeMode::Light,
            ..GuiConfig::default()
        };
        assert_eq!(
            startup_config(
                loaded.clone(),
                RecoveryState {
                    previous_run_incomplete: false,
                },
            ),
            loaded
        );
        assert_eq!(
            startup_config(
                loaded,
                RecoveryState {
                    previous_run_incomplete: true,
                },
            ),
            GuiConfig::default()
        );
    }
}
