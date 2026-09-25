use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui;

use crate::config::{save_config, ConfigError, ConfigPaths, GuiConfig, ThemeMode};
use crate::navigation::{
    page_for_shortcut, page_label, page_title, pages, NavigationState, PageId,
};
use crate::notifications::{NotificationCenter, NotificationKind};
use crate::recovery::RecoveryState;

const RECOVERY_NOTICE: &str = "Previous run did not exit normally. Safe defaults are active.";
const UNCONNECTED_DATA_SOURCE: &str = "Data source is not connected yet.";

pub type SharedConfig = Arc<Mutex<GuiConfig>>;

pub struct RustNtApp {
    pub(crate) config: SharedConfig,
    pub(crate) paths: ConfigPaths,
    pub(crate) recovery: RecoveryState,
    pub(crate) config_notice: Option<String>,
    pub(crate) save_notice: Option<String>,
    pub(crate) navigation: NavigationState,
    pub(crate) notifications: NotificationCenter,
    notifications_open: bool,
}

impl RustNtApp {
    pub fn new(
        config: SharedConfig,
        paths: ConfigPaths,
        recovery: RecoveryState,
        config_notice: Option<String>,
    ) -> Self {
        let mut notifications = NotificationCenter::new();
        if let Some(notice) = &config_notice {
            notifications.push(NotificationKind::Warning, notice.clone());
        }
        if recovery.previous_run_incomplete {
            notifications.push(NotificationKind::Warning, RECOVERY_NOTICE);
        }

        Self {
            config,
            paths,
            recovery,
            config_notice,
            save_notice: None,
            navigation: NavigationState::default(),
            notifications,
            notifications_open: false,
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

    fn select_page(&mut self, page: PageId) {
        self.navigation.current_page = page;
    }

    fn record_save_result(&mut self, result: Result<(), ConfigError>) {
        match result {
            Ok(()) => {
                self.save_notice = Some("Configuration saved".to_owned());
                self.notifications
                    .push(NotificationKind::Success, "Configuration saved");
            }
            Err(error) => {
                let notice = format!("Could not save configuration: {error}");
                self.save_notice = Some(notice.clone());
                self.notifications.push(NotificationKind::Error, notice);
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }

        let (ctrl, page, ctrl_k, escape) = ctx.input(|input| {
            let page = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3]
                .into_iter()
                .find_map(|key| {
                    input
                        .key_pressed(key)
                        .then(|| page_for_shortcut(input.modifiers.ctrl, key))
                });
            (
                input.modifiers.ctrl,
                page.flatten(),
                input.modifiers.ctrl && input.key_pressed(egui::Key::K),
                input.key_pressed(egui::Key::Escape),
            )
        });

        if ctrl {
            if let Some(page) = page {
                self.select_page(page);
            }
        }
        if ctrl_k {
            self.navigation.switcher_open = !self.navigation.switcher_open;
        }
        if escape {
            if self.navigation.switcher_open {
                self.navigation.switcher_open = false;
            } else {
                self.notifications.clear();
            }
        }
    }

    fn show_top_panel(&mut self, ui: &mut egui::Ui, theme: ThemeMode) {
        egui::Panel::top("status-panel").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustNT");
                ui.separator();
                ui.label(page_title(self.navigation.current_page));
                ui.separator();
                ui.label(format!("GUI: {}", recovery_label(self.recovery)));
                ui.label(format!("Theme: {}", theme_label(theme)));
                if ui
                    .button(format!(
                        "Notifications ({})",
                        self.notifications.unread_count()
                    ))
                    .clicked()
                {
                    self.notifications_open = true;
                    self.notifications.mark_all_read();
                }
            });
        });
    }

    fn show_navigation_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation-panel")
            .default_size(150.0)
            .resizable(false)
            .show(ui, |ui| {
                ui.heading("Pages");
                ui.separator();
                for page in pages() {
                    let selected = self.navigation.current_page == *page;
                    if ui.selectable_label(selected, page_label(*page)).clicked() {
                        self.select_page(*page);
                    }
                }
            });
    }

    fn show_central_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: ThemeMode) {
        egui::CentralPanel::default().show(ui, |ui| match self.navigation.current_page {
            PageId::Overview => self.show_overview(ui, ctx, theme),
            PageId::SystemMonitor | PageId::WindowsSessions => {
                ui.heading(page_title(self.navigation.current_page));
                ui.label(placeholder_text(self.navigation.current_page));
            }
        });
    }

    fn show_overview(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: ThemeMode) {
        ui.heading("Overview");
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
            Self::apply_theme(ctx, config.theme);
        }
        if ui.button("Save config").clicked() {
            let config = self.config().clone();
            self.record_save_result(save_config(&self.paths, &config));
        }
        if let Some(notice) = &self.save_notice {
            ui.label(notice);
        }
    }

    fn show_switcher(&mut self, ctx: &egui::Context) {
        if !self.navigation.switcher_open {
            return;
        }

        let mut open = true;
        egui::Window::new("Page switcher")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Select a page");
                for page in pages() {
                    let shortcut = match *page {
                        PageId::Overview => "1",
                        PageId::SystemMonitor => "2",
                        PageId::WindowsSessions => "3",
                    };
                    if ui
                        .selectable_label(
                            self.navigation.current_page == *page,
                            format!("{}  (Ctrl+{})", page_title(*page), shortcut),
                        )
                        .clicked()
                    {
                        self.select_page(*page);
                        self.navigation.switcher_open = false;
                    }
                }
            });
        if !open {
            self.navigation.switcher_open = false;
        }
    }

    fn show_notifications(&mut self, ctx: &egui::Context) {
        if !self.notifications_open {
            return;
        }

        let mut open = true;
        egui::Window::new("Notifications")
            .open(&mut open)
            .default_width(360.0)
            .resizable(true)
            .show(ctx, |ui| {
                if ui.button("Clear notifications").clicked() {
                    self.notifications.clear();
                }
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| {
                        if self.notifications.notifications().is_empty() {
                            ui.label("No notifications");
                        } else {
                            for notification in self.notifications.notifications() {
                                ui.label(format!(
                                    "{}: {}",
                                    notification_kind_label(notification.kind),
                                    notification.message
                                ));
                            }
                        }
                    });
            });
        if !open {
            self.notifications_open = false;
        }
    }
}

impl eframe::App for RustNtApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.snapshot_viewport(&ctx);
        let theme = self.config().theme;
        Self::apply_theme(&ctx, theme);
        self.show_top_panel(ui, theme);
        self.show_navigation_panel(ui);
        self.show_central_panel(ui, &ctx, theme);
        self.show_switcher(&ctx);
        self.show_notifications(&ctx);
    }
}

fn notification_kind_label(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::Info => "Info",
        NotificationKind::Success => "Success",
        NotificationKind::Warning => "Warning",
        NotificationKind::Error => "Error",
    }
}

pub(crate) fn placeholder_text(page: PageId) -> &'static str {
    match page {
        PageId::Overview => "",
        PageId::SystemMonitor | PageId::WindowsSessions => UNCONNECTED_DATA_SOURCE,
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
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::config::{config_paths_from_root, GuiConfig, ThemeMode};
    use crate::navigation::PageId;
    use crate::recovery::RecoveryState;

    use super::{
        placeholder_text, recovery_label, theme_label, viewport_settings_for, RustNtApp,
        ViewportSettings,
    };

    fn test_app(config_notice: Option<String>) -> RustNtApp {
        RustNtApp::new(
            Arc::new(Mutex::new(GuiConfig::default())),
            config_paths_from_root(PathBuf::from("task15-app-tests")),
            RecoveryState {
                previous_run_incomplete: false,
            },
            config_notice,
        )
    }

    #[test]
    fn new_app_starts_on_overview_without_notifications() {
        let app = test_app(None);

        assert_eq!(app.navigation.current_page, PageId::Overview);
        assert!(!app.navigation.switcher_open);
        assert!(app.notifications.notifications().is_empty());
    }

    #[test]
    fn initial_configuration_notice_is_enqueued_once() {
        let app = test_app(Some("Invalid configuration".to_owned()));

        assert_eq!(app.notifications.notifications().len(), 1);
        assert_eq!(app.notifications.unread_count(), 1);
        assert_eq!(
            app.notifications.notifications()[0].message,
            "Invalid configuration"
        );
    }

    #[test]
    fn changing_page_does_not_mutate_config_or_recovery() {
        let mut app = test_app(None);
        let before_config = app.config.lock().unwrap().clone();
        let before_recovery = app.recovery;

        app.select_page(PageId::SystemMonitor);

        assert_eq!(app.navigation.current_page, PageId::SystemMonitor);
        assert_eq!(*app.config.lock().unwrap(), before_config);
        assert_eq!(app.recovery, before_recovery);
    }

    #[test]
    fn capability_pages_explain_that_their_data_source_is_unconnected() {
        let expected = "Data source is not connected yet.";

        assert_eq!(placeholder_text(PageId::SystemMonitor), expected);
        assert_eq!(placeholder_text(PageId::WindowsSessions), expected);
    }

    #[test]
    fn save_result_creates_one_notification() {
        let mut app = test_app(None);

        app.record_save_result(Ok(()));
        assert_eq!(app.notifications.notifications().len(), 1);
        assert_eq!(app.save_notice.as_deref(), Some("Configuration saved"));

        app.record_save_result(Ok(()));
        assert_eq!(app.notifications.notifications().len(), 2);

        app.record_save_result(Err(crate::config::ConfigError {
            operation: "save configuration".to_owned(),
            message: "disk full".to_owned(),
        }));
        assert_eq!(app.notifications.notifications().len(), 3);
        assert_eq!(
            app.save_notice.as_deref(),
            Some("Could not save configuration: save configuration: disk full")
        );
    }

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
