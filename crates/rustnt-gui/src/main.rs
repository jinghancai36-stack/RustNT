#![cfg(windows)]

pub mod app;
pub mod config;
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
    match result {
        Ok(()) => {
            let mut cleanup_errors = Vec::new();
            match shared.lock() {
                Ok(final_config) => {
                    if let Err(error) = save_config(&paths, &final_config) {
                        cleanup_errors.push(error.into());
                    }
                }
                Err(_) => cleanup_errors.push(GuiError::ConfigLock),
            }
            if let Err(error) = marker.remove() {
                cleanup_errors.push(error.into());
            }
            aggregate_cleanup_errors(cleanup_errors)
        }
        Err(error) => Err(GuiError::Eframe(error)),
    }
}

fn main() {
    if let Err(error) = run_gui() {
        eprintln!("RustNT GUI failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{aggregate_cleanup_errors, startup_config, GuiError};
    use crate::config::{GuiConfig, ThemeMode};
    use crate::recovery::{RecoveryError, RecoveryState};

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
