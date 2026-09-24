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
            let result = {
                let config = self.config();
                save_config(&self.paths, &config)
            };
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

pub fn viewport_for(config: &GuiConfig, recovery: RecoveryState) -> egui::ViewportBuilder {
    let config = if recovery.previous_run_incomplete {
        GuiConfig::default()
    } else {
        config.clone()
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustNT")
        .with_inner_size([config.window_width, config.window_height]);
    if !recovery.previous_run_incomplete {
        if let (Some(x), Some(y)) = (config.window_x, config.window_y) {
            viewport = viewport.with_position([x, y]);
        }
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

    use super::{recovery_label, theme_label};

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
}
