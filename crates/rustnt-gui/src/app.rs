use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui;

use crate::config::{save_config, ConfigLoad, ConfigPaths, GuiConfig, ThemeMode};
use crate::recovery::RecoveryState;

const RECOVERY_NOTICE: &str = "Previous run did not exit normally. Safe defaults are active.";

pub struct RustNtApp {
    pub(crate) config: Arc<Mutex<GuiConfig>>,
    pub(crate) paths: ConfigPaths,
    pub(crate) recovery: RecoveryState,
    pub(crate) config_notice: Option<String>,
    pub(crate) save_notice: Option<String>,
}

impl RustNtApp {
    pub fn new(
        config: GuiConfig,
        paths: ConfigPaths,
        load: ConfigLoad,
        recovery: RecoveryState,
    ) -> Self {
        let config = if recovery.previous_run_incomplete {
            GuiConfig::default()
        } else {
            config
        };
        Self {
            config: Arc::new(Mutex::new(config)),
            paths,
            recovery,
            config_notice: load.notice,
            save_notice: None,
        }
    }

    fn config(&self) -> MutexGuard<'_, GuiConfig> {
        self.config
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn apply_theme(ctx: &egui::Context, theme: ThemeMode) {
        ctx.set_visuals(match theme {
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::Light => egui::Visuals::light(),
        });
    }

    fn snapshot_viewport(&self, ctx: &egui::Context) {
        let viewport = ctx.input(|input| input.viewport().clone());
        let mut config = self.config();
        if let Some(size) = viewport.inner_rect.map(|rect| rect.size()) {
            if size.x.is_finite() {
                config.window_width = size.x;
            }
            if size.y.is_finite() {
                config.window_height = size.y;
            }
        }
        if let Some(rect) = viewport.outer_rect {
            if rect.min.x.is_finite() && rect.min.y.is_finite() {
                config.window_x = Some(rect.min.x);
                config.window_y = Some(rect.min.y);
            }
        }
    }
}

impl eframe::App for RustNtApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.snapshot_viewport(&ctx);
        let theme = self.config().theme;
        Self::apply_theme(&ctx, theme);

        ui.heading("RustNT");
        ui.label("RustNT GUI foundation status");
        ui.label(format!("Theme: {}", theme_label(theme)));
        ui.label(format!("State: {}", recovery_label(self.recovery)));

        if let Some(notice) = &self.config_notice {
            ui.label(notice);
        }
        if self.recovery.previous_run_incomplete {
            ui.label(RECOVERY_NOTICE);
        }

        if ui.button("Toggle theme").clicked() {
            let mut config = self.config();
            config.theme = match config.theme {
                ThemeMode::Dark => ThemeMode::Light,
                ThemeMode::Light => ThemeMode::Dark,
            };
            Self::apply_theme(&ctx, config.theme);
        }
        if ui.button("Save config").clicked() {
            let config = self.config().clone();
            let result = save_config(&self.paths, &config);
            self.save_notice = Some(match result {
                Ok(()) => "Configuration saved".to_owned(),
                Err(error) => format!("Could not save configuration: {error}"),
            });
        }
        if let Some(notice) = &self.save_notice {
            ui.label(notice);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewportSettings {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) position: Option<[f32; 2]>,
}

pub(crate) fn viewport_settings_for(
    config: &GuiConfig,
    recovery: RecoveryState,
) -> ViewportSettings {
    if recovery.previous_run_incomplete {
        let defaults = GuiConfig::default();
        ViewportSettings {
            width: defaults.window_width,
            height: defaults.window_height,
            position: None,
        }
    } else {
        ViewportSettings {
            width: config.window_width,
            height: config.window_height,
            position: config.window_x.zip(config.window_y).map(|(x, y)| [x, y]),
        }
    }
}

pub fn viewport_for(config: &GuiConfig, recovery: RecoveryState) -> egui::ViewportBuilder {
    let settings = viewport_settings_for(config, recovery);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustNT")
        .with_inner_size([settings.width, settings.height]);
    if let Some(position) = settings.position {
        viewport = viewport.with_position(position);
    }
    viewport
}

pub fn theme_label(theme: ThemeMode) -> &'static str {
    match theme {
        ThemeMode::Dark => "dark",
        ThemeMode::Light => "light",
    }
}

pub fn recovery_label(recovery: RecoveryState) -> &'static str {
    if recovery.previous_run_incomplete {
        "safe recovery mode"
    } else {
        "normal startup"
    }
}

#[cfg(test)]
mod tests {
    use crate::config::ThemeMode;
    use crate::recovery::RecoveryState;

    use super::{recovery_label, theme_label, viewport_settings_for, ViewportSettings};

    #[test]
    fn theme_label_is_stable() {
        assert_eq!(theme_label(ThemeMode::Dark), "dark");
        assert_eq!(theme_label(ThemeMode::Light), "light");
    }

    #[test]
    fn recovery_label_distinguishes_previous_incomplete_run() {
        assert_eq!(
            recovery_label(RecoveryState {
                previous_run_incomplete: false,
            }),
            "normal startup"
        );
        assert_eq!(
            recovery_label(RecoveryState {
                previous_run_incomplete: true,
            }),
            "safe recovery mode"
        );
    }

    #[test]
    fn recovery_viewport_uses_default_dimensions_without_position() {
        let loaded = crate::config::GuiConfig {
            theme: ThemeMode::Light,
            window_width: 1440.0,
            window_height: 900.0,
            window_x: Some(320.0),
            window_y: Some(180.0),
        };

        assert_eq!(
            viewport_settings_for(
                &loaded,
                RecoveryState {
                    previous_run_incomplete: true,
                },
            ),
            ViewportSettings {
                width: crate::config::DEFAULT_WINDOW_WIDTH,
                height: crate::config::DEFAULT_WINDOW_HEIGHT,
                position: None,
            }
        );
    }

    #[test]
    fn normal_viewport_uses_loaded_dimensions_and_position() {
        let loaded = crate::config::GuiConfig {
            theme: ThemeMode::Light,
            window_width: 1440.0,
            window_height: 900.0,
            window_x: Some(320.0),
            window_y: Some(180.0),
        };

        assert_eq!(
            viewport_settings_for(
                &loaded,
                RecoveryState {
                    previous_run_incomplete: false,
                },
            ),
            ViewportSettings {
                width: 1440.0,
                height: 900.0,
                position: Some([320.0, 180.0]),
            }
        );
    }

    #[test]
    fn normal_viewport_does_not_force_missing_position() {
        let loaded = crate::config::GuiConfig {
            theme: ThemeMode::Dark,
            window_width: 1024.0,
            window_height: 768.0,
            window_x: None,
            window_y: None,
        };

        assert_eq!(
            viewport_settings_for(
                &loaded,
                RecoveryState {
                    previous_run_incomplete: false,
                },
            ),
            ViewportSettings {
                width: 1024.0,
                height: 768.0,
                position: None,
            }
        );
    }
}
