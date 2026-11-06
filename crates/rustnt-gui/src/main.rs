#![cfg(windows)]

pub mod app;
pub mod config;
pub mod desktop_preview;
pub mod file_browser;
pub mod navigation;
pub mod notifications;
pub mod recovery;
pub mod shell_recovery;
pub mod shell_snapshot;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use app::{viewport_for, RustNtApp, SharedConfig};
use config::{config_paths_from_appdata, load_config, save_config, ConfigError, GuiConfig};
use desktop_preview::{
    parse_startup_mode, DesktopPreviewApp, StartupMode, PREVIEW_FLAG, PREVIEW_WINDOW_TITLE,
};
use recovery::{create_marker, inspect, install_panic_hook, RecoveryError};
use shell_recovery::{
    startup_notice, RecoveryLogError, ShellRecovery, StartupNotice, StartupOutcome,
};
use shell_snapshot::{
    collect_snapshot, save_snapshot, ShellSnapshot, SnapshotResult, WindowsShellReader,
};

#[derive(Debug)]
enum GuiError {
    Config(ConfigError),
    Recovery(RecoveryError),
    ShellRecovery(RecoveryLogError),
    ConfigLock,
    Eframe(eframe::Error),
}

impl fmt::Display for GuiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "configuration error: {error}"),
            Self::Recovery(error) => write!(formatter, "recovery error: {error}"),
            Self::ShellRecovery(error) => write!(formatter, "shell recovery error: {error}"),
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

impl From<RecoveryLogError> for GuiError {
    fn from(error: RecoveryLogError) -> Self {
        Self::ShellRecovery(error)
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
        GuiError::Recovery(_) | GuiError::ShellRecovery(_) => 1,
        GuiError::Eframe(_) => 2,
    }
}

fn cleanup_after_normal_exit(
    shared: &SharedConfig,
    save: impl FnOnce(&GuiConfig) -> Result<(), ConfigError>,
    complete_shell_session: impl FnOnce() -> Result<(), RecoveryLogError>,
    remove_gui_marker: impl FnOnce() -> Result<(), RecoveryError>,
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
    if let Err(error) = complete_shell_session() {
        cleanup_errors.push(error.into());
    }
    if let Err(error) = remove_gui_marker() {
        cleanup_errors.push(error.into());
    }
    aggregate_cleanup_errors(cleanup_errors)
}

fn run_after_startup(
    prepare: impl FnOnce() -> Result<(), GuiError>,
    open_window: impl FnOnce() -> Result<(), eframe::Error>,
    cleanup: impl FnOnce() -> Result<(), GuiError>,
) -> Result<(), GuiError> {
    prepare()?;
    finish_run_native(open_window(), cleanup)
}

fn snapshot_failure_summary(snapshot: &ShellSnapshot) -> Option<String> {
    snapshot.entries.iter().find_map(|entry| {
        let SnapshotResult::Failed {
            category,
            win32_code,
        } = &entry.result
        else {
            return None;
        };
        let target = match entry.target {
            shell_snapshot::ShellValueTarget::Shell => "Shell",
            shell_snapshot::ShellValueTarget::Userinit => "Userinit",
        };
        Some(match win32_code {
            Some(code) => format!("{target} read failed: {category} (Windows error {code})"),
            None => format!("{target} read failed: {category}"),
        })
    })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    let (_shell_recovery, shell_session) =
        ShellRecovery::begin_startup(&paths, std::process::id(), unix_timestamp())?;
    let snapshot = collect_snapshot(
        &WindowsShellReader::new(),
        unix_timestamp(),
        std::process::id(),
    );
    let mut snapshot_failure = snapshot_failure_summary(&snapshot);
    if let Err(error) = save_snapshot(&paths, &snapshot) {
        snapshot_failure = Some(match snapshot_failure {
            Some(previous) => format!("{previous}; snapshot save failed: {error}"),
            None => format!("snapshot save failed: {error}"),
        });
    }
    let startup_notice: Option<StartupNotice> = match snapshot_failure {
        Some(summary) => {
            shell_session.record_snapshot_result(Err(summary.clone()))?;
            startup_notice(StartupOutcome::SnapshotUnavailable(summary))
        }
        None => {
            shell_session.record_snapshot_result(Ok(()))?;
            None
        }
    };
    shell_session.mark_running()?;
    let options = eframe::NativeOptions {
        viewport: viewport_for(&config, recovery),
        ..Default::default()
    };
    let shared_for_app = Arc::clone(&shared);
    let paths_for_app = paths.clone();
    run_after_startup(
        || Ok(()),
        || {
            eframe::run_native(
                "RustNT",
                options,
                Box::new(move |_creation_context| {
                    Ok(Box::new(RustNtApp::new(
                        shared_for_app,
                        paths_for_app,
                        recovery,
                        load.notice,
                        startup_notice,
                    )))
                }),
            )
        },
        || {
            cleanup_after_normal_exit(
                &shared,
                |final_config| save_config(&paths, final_config),
                || shell_session.complete_normal_exit(),
                || marker.remove(),
            )
        },
    )
}

fn run_preview() -> Result<(), GuiError> {
    // The preview is intentionally isolated from configuration, the GUI crash
    // marker, and the Task17A shell recovery session: it must never be able to
    // influence a real shell recovery decision.
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(PREVIEW_WINDOW_TITLE)
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        PREVIEW_WINDOW_TITLE,
        options,
        Box::new(move |_creation_context| Ok(Box::new(DesktopPreviewApp::new()))),
    )
    .map_err(GuiError::from)
}

fn main() {
    let mode = match parse_startup_mode(std::env::args().skip(1)) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("RustNT GUI failed: {error}");
            eprintln!("usage: rustnt-gui [{PREVIEW_FLAG}]");
            std::process::exit(2);
        }
    };

    let result = match mode {
        StartupMode::Standard => run_gui(),
        StartupMode::DesktopPreview => run_preview(),
    };

    if let Err(error) = result {
        eprintln!("RustNT GUI failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        aggregate_cleanup_errors, cleanup_after_normal_exit, finish_run_native, run_after_startup,
        startup_config, GuiError,
    };
    use crate::config::{ConfigError, GuiConfig, ThemeMode};
    use crate::recovery::{RecoveryError, RecoveryState};
    use crate::shell_recovery::RecoveryLogError;
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

    fn shell_recovery_error() -> RecoveryLogError {
        RecoveryLogError {
            operation: "complete shell session".to_owned(),
            message: "access denied".to_owned(),
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
            || Ok(()),
        );

        assert!(result.is_ok());
        assert!(remove_called);
    }

    #[test]
    fn normal_cleanup_completes_task17a_before_removing_legacy_marker() {
        let shared = Arc::new(Mutex::new(GuiConfig::default()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let save_calls = Arc::clone(&calls);
        let complete_calls = Arc::clone(&calls);
        let remove_calls = Arc::clone(&calls);

        cleanup_after_normal_exit(
            &shared,
            |_| {
                save_calls.lock().unwrap().push("save config");
                Ok(())
            },
            || {
                complete_calls.lock().unwrap().push("complete Task17A");
                Ok(())
            },
            || {
                remove_calls.lock().unwrap().push("remove GUI marker");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *calls.lock().unwrap(),
            ["save config", "complete Task17A", "remove GUI marker"]
        );
    }

    #[test]
    fn shell_session_startup_failure_prevents_opening_window() {
        let mut window_opened = false;
        let result = run_after_startup(
            || {
                Err(GuiError::ShellRecovery(
                    crate::shell_recovery::RecoveryLogError {
                        operation: "create shell session".to_owned(),
                        message: "access denied".to_owned(),
                    },
                ))
            },
            || {
                window_opened = true;
                Ok(())
            },
            || Ok(()),
        );

        assert!(matches!(result, Err(GuiError::ShellRecovery(_))));
        assert!(!window_opened);
    }

    #[test]
    fn eframe_error_skips_both_task17a_and_legacy_marker_cleanup() {
        let mut task17a_cleanup_called = false;
        let mut gui_marker_cleanup_called = false;
        let result = run_after_startup(
            || Ok(()),
            || {
                Err(eframe::Error::AppCreation(Box::new(std::io::Error::other(
                    "window failed",
                ))))
            },
            || {
                task17a_cleanup_called = true;
                gui_marker_cleanup_called = true;
                Ok(())
            },
        );

        assert!(matches!(result, Err(GuiError::Eframe(_))));
        assert!(!task17a_cleanup_called);
        assert!(!gui_marker_cleanup_called);
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
                Err(shell_recovery_error())
            },
            || Err(recovery_error()),
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
                Err(shell_recovery_error())
            },
            || Err(recovery_error()),
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
