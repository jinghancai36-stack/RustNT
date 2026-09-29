use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui;
use rustnt_core::filesystem::{list_directory, DirectoryEntry, FileKind, FileSystemError};

use crate::config::{save_config, ConfigError, ConfigPaths, GuiConfig, ThemeMode};
use crate::file_browser::{FileBrowserState, SortDirection, SortField};
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
    pub(crate) file_browser: FileBrowserState,
    notifications_open: bool,
    file_browser_path_input: String,
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

        let file_browser =
            initialize_file_browser(initial_file_browser_path(), process_directory_path());
        let file_browser_path_input = file_browser.current_path().to_owned();

        Self {
            config,
            paths,
            recovery,
            config_notice,
            save_notice: None,
            navigation: NavigationState::default(),
            notifications,
            file_browser,
            notifications_open: false,
            file_browser_path_input,
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

    fn select_switcher_page(&mut self, page: PageId) {
        self.select_page(page);
        self.navigation.switcher_open = false;
    }

    fn open_notification_panel(&mut self) {
        self.notifications_open = true;
        self.notifications.mark_all_read();
    }

    fn clear_notifications(&mut self) {
        self.notifications.clear();
    }

    fn close_notification_panel(&mut self) {
        self.notifications_open = false;
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

    fn finish_file_browser_action<T>(
        &mut self,
        result: Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        match result {
            Ok(value) => {
                self.file_browser_path_input = self.file_browser.current_path().to_owned();
                Ok(value)
            }
            Err(error) => {
                self.file_browser_path_input = self.file_browser.current_path().to_owned();
                self.notifications.push(
                    NotificationKind::Error,
                    format!("File Manager: {}", file_system_error_label(&error)),
                );
                Err(error)
            }
        }
    }

    fn load_file_browser(&mut self) -> Result<(), FileSystemError> {
        let result = self.file_browser.load();
        self.finish_file_browser_action(result)
    }

    #[cfg(test)]
    fn load_file_browser_with<F>(&mut self, loader: F) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let result = self.file_browser.load_with(loader);
        self.finish_file_browser_action(result)
    }

    fn navigate_file_browser(&mut self, path: String) -> Result<(), FileSystemError> {
        self.navigate_file_browser_with(path, list_directory)
    }

    fn navigate_file_browser_with<F>(
        &mut self,
        path: impl Into<String>,
        loader: F,
    ) -> Result<(), FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let result = self.file_browser.navigate_to(path, loader);
        self.finish_file_browser_action(result)
    }

    fn go_back_file_browser(&mut self) -> Result<bool, FileSystemError> {
        self.go_back_file_browser_with(list_directory)
    }

    fn go_back_file_browser_with<F>(&mut self, loader: F) -> Result<bool, FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let result = self.file_browser.back(loader);
        self.finish_file_browser_action(result)
    }

    fn go_forward_file_browser(&mut self) -> Result<bool, FileSystemError> {
        self.go_forward_file_browser_with(list_directory)
    }

    fn go_forward_file_browser_with<F>(&mut self, loader: F) -> Result<bool, FileSystemError>
    where
        F: FnOnce(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
    {
        let result = self.file_browser.forward(loader);
        self.finish_file_browser_action(result)
    }

    fn go_parent_file_browser(&mut self) -> Result<bool, FileSystemError> {
        let result = self.file_browser.parent(list_directory);
        self.finish_file_browser_action(result)
    }

    fn set_file_browser_filter(&mut self, filter: impl Into<String>) {
        self.file_browser.set_filter(filter);
    }

    #[cfg(test)]
    fn set_file_browser_sort(&mut self, field: SortField, direction: SortDirection) {
        self.file_browser.set_sort_field(field);
        self.file_browser.set_sort_direction(direction);
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
            } else if !self.notifications_open {
                self.clear_notifications();
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
                    self.open_notification_panel();
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
            PageId::SystemMonitor => {
                ui.heading(page_title(self.navigation.current_page));
                ui.label(placeholder_text(self.navigation.current_page));
            }
            PageId::FileManager => self.show_file_manager(ui),
        });
    }

    fn show_file_manager(&mut self, ui: &mut egui::Ui) {
        ui.heading("File Manager");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.file_browser.back_stack().is_empty(),
                    egui::Button::new("Back"),
                )
                .clicked()
            {
                let _ = self.go_back_file_browser();
            }
            if ui
                .add_enabled(
                    !self.file_browser.forward_stack().is_empty(),
                    egui::Button::new("Forward"),
                )
                .clicked()
            {
                let _ = self.go_forward_file_browser();
            }
            if ui
                .add_enabled(
                    FileBrowserState::parent_path(self.file_browser.current_path()).is_some(),
                    egui::Button::new("Parent"),
                )
                .clicked()
            {
                let _ = self.go_parent_file_browser();
            }
            if ui.button("Refresh").clicked() {
                let _ = self.load_file_browser();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Path");
            let response = ui.text_edit_singleline(&mut self.file_browser_path_input);
            if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                let path = self.file_browser_path_input.clone();
                let _ = self.navigate_file_browser(path);
            }
        });

        ui.horizontal(|ui| {
            ui.label("Filter");
            let mut filter = self.file_browser.filter().to_owned();
            if ui.text_edit_singleline(&mut filter).changed() {
                self.set_file_browser_filter(filter);
            }
            let mut sort_field = self.file_browser.sort_field();
            egui::ComboBox::from_id_salt("file-manager-sort-field")
                .selected_text(sort_field_label(sort_field))
                .show_ui(ui, |ui| {
                    for field in [
                        SortField::Name,
                        SortField::Kind,
                        SortField::Size,
                        SortField::Modified,
                    ] {
                        if ui
                            .selectable_value(&mut sort_field, field, sort_field_label(field))
                            .clicked()
                        {
                            self.file_browser.set_sort_field(sort_field);
                        }
                    }
                });
            if ui
                .button(sort_direction_label(self.file_browser.sort_direction()))
                .clicked()
            {
                let direction = match self.file_browser.sort_direction() {
                    SortDirection::Ascending => SortDirection::Descending,
                    SortDirection::Descending => SortDirection::Ascending,
                };
                self.file_browser.set_sort_direction(direction);
            }
        });

        let view = self.file_browser.view();
        ui.label(format!("Path: {}", view.path));
        if let Some(error) = &view.last_error {
            ui.colored_label(
                egui::Color32::RED,
                format!("Error: {}", file_system_error_label(error)),
            );
        } else if view.is_empty && view.loaded {
            ui.label("Directory is empty");
        } else {
            ui.label(format!("{} direct item(s)", view.entries.len()));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("file-manager-table")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("Name");
                    ui.strong("Type");
                    ui.strong("Size");
                    ui.strong("Modified");
                    ui.end_row();
                    for entry in view.entries {
                        let name = crate::file_browser::entry_name(&entry.metadata.path);
                        let response = ui.selectable_label(
                            view.selected_path.as_deref() == Some(entry.metadata.path.as_str()),
                            name,
                        );
                        if response.clicked() && entry.metadata.kind == FileKind::Directory {
                            self.file_browser
                                .set_selected_path(Some(entry.metadata.path.clone()));
                        }
                        if response.double_clicked() && entry.metadata.kind == FileKind::Directory {
                            let _ = self.navigate_file_browser(entry.metadata.path.clone());
                        }
                        ui.label(file_kind_label(&entry));
                        ui.label(file_size_label(&entry));
                        ui.label(file_modified_label(&entry));
                        ui.end_row();
                    }
                });
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
                        PageId::FileManager => "3",
                    };
                    if ui
                        .selectable_label(
                            self.navigation.current_page == *page,
                            format!("{}  (Ctrl+{})", page_title(*page), shortcut),
                        )
                        .clicked()
                    {
                        self.select_switcher_page(*page);
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
                    self.clear_notifications();
                }
                if ui.button("Close notifications").clicked() {
                    self.close_notification_panel();
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
            self.close_notification_panel();
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
        PageId::SystemMonitor => UNCONNECTED_DATA_SOURCE,
        PageId::FileManager => "",
    }
}

fn initial_file_browser_path() -> String {
    std::env::var("USERPROFILE")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .or_else(process_directory_path)
        .unwrap_or_else(|| FileBrowserState::default().current_path().to_owned())
}

fn initialize_file_browser(
    preferred_path: String,
    fallback_path: Option<String>,
) -> FileBrowserState {
    initialize_file_browser_with(preferred_path, fallback_path, list_directory)
}

fn initialize_file_browser_with<F>(
    preferred_path: String,
    fallback_path: Option<String>,
    mut loader: F,
) -> FileBrowserState
where
    F: FnMut(&str) -> Result<Vec<DirectoryEntry>, FileSystemError>,
{
    let mut file_browser = FileBrowserState::new(preferred_path);
    let preferred_error = match file_browser.load_with(&mut loader) {
        Ok(()) => return file_browser,
        Err(error) => error,
    };

    let Some(fallback_path) = fallback_path.filter(|path| path != file_browser.current_path())
    else {
        return file_browser;
    };

    let mut fallback_browser = FileBrowserState::new(fallback_path);
    if fallback_browser.load_with(&mut loader).is_ok() {
        let _ = fallback_browser.load_with(|_| Err(preferred_error.clone()));
    }
    fallback_browser
}

fn process_directory_path() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn file_system_error_label(error: &FileSystemError) -> String {
    match error {
        FileSystemError::InvalidPath(path) => format!("Invalid path: {path}"),
        FileSystemError::NotFound(path) => format!("Path not found: {path}"),
        FileSystemError::AccessDenied(path) => format!("Access denied: {path}"),
        FileSystemError::Io(message) => format!("I/O error: {message}"),
        FileSystemError::Win32 { operation, code } => {
            format!("{operation} failed (Windows error {code})")
        }
    }
}

fn sort_field_label(field: SortField) -> &'static str {
    match field {
        SortField::Name => "Name",
        SortField::Kind => "Type",
        SortField::Size => "Size",
        SortField::Modified => "Modified",
    }
}

fn sort_direction_label(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::Ascending => "Ascending",
        SortDirection::Descending => "Descending",
    }
}

fn file_kind_label(entry: &DirectoryEntry) -> &'static str {
    if entry.metadata.is_reparse_point {
        return "Reparse point";
    }
    match entry.metadata.kind {
        FileKind::Directory => "Directory",
        FileKind::File => "File",
        FileKind::ReparsePoint => "Reparse point",
        FileKind::Other => "Other",
    }
}

fn file_size_label(entry: &DirectoryEntry) -> String {
    entry
        .metadata
        .size_bytes
        .map(|size| size.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn file_modified_label(entry: &DirectoryEntry) -> String {
    entry
        .metadata
        .modified
        .map(|modified| format!("{modified:?}"))
        .unwrap_or_else(|| "-".to_owned())
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
    use std::time::{Duration, UNIX_EPOCH};

    use crate::file_browser::{SortDirection, SortField};
    use eframe::egui;
    use rustnt_core::filesystem::{DirectoryEntry, FileKind, FileMetadata, FileSystemError};

    use crate::config::{config_paths_from_root, GuiConfig, ThemeMode};
    use crate::navigation::PageId;
    use crate::recovery::RecoveryState;

    use super::{
        initialize_file_browser_with, placeholder_text, recovery_label, theme_label,
        viewport_settings_for, RustNtApp, ViewportSettings,
    };

    fn fixture_entries(path: &str) -> Vec<DirectoryEntry> {
        vec![
            DirectoryEntry {
                metadata: FileMetadata {
                    path: format!(r"{path}\alpha"),
                    kind: FileKind::Directory,
                    size_bytes: None,
                    created: None,
                    modified: Some(UNIX_EPOCH + Duration::from_secs(2)),
                    accessed: None,
                    attributes: 0,
                    is_reparse_point: false,
                },
            },
            DirectoryEntry {
                metadata: FileMetadata {
                    path: format!(r"{path}\beta.txt"),
                    kind: FileKind::File,
                    size_bytes: Some(20),
                    created: None,
                    modified: Some(UNIX_EPOCH + Duration::from_secs(1)),
                    accessed: None,
                    attributes: 0,
                    is_reparse_point: false,
                },
            },
        ]
    }

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

    fn run_input(app: &mut RustNtApp, context: &egui::Context, input: egui::RawInput) {
        let output = context.run_ui(input, |ui| app.handle_shortcuts(ui.ctx()));
        output.drop_without_applying_deltas();
    }

    fn run_top_frame(app: &mut RustNtApp, context: &egui::Context, input: egui::RawInput) {
        let output = context.run_ui(input, |ui| {
            app.show_top_panel(ui, ThemeMode::Dark);
            app.show_notifications(ui.ctx());
        });
        output.drop_without_applying_deltas();
    }

    fn pointer_input(position: egui::Pos2, pressed: bool) -> egui::RawInput {
        egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        }
    }

    fn click_top_rect(app: &mut RustNtApp, context: &egui::Context, rect: egui::Rect) {
        run_top_frame(app, context, pointer_input(rect.center(), true));
        run_top_frame(app, context, pointer_input(rect.center(), false));
    }

    fn key_input(key: egui::Key, modifiers: egui::Modifiers) -> egui::RawInput {
        egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn new_app_starts_on_overview_without_notifications() {
        let app = test_app(None);

        assert_eq!(app.navigation.current_page, PageId::Overview);
        assert!(!app.navigation.switcher_open);
        assert!(app.notifications.notifications().is_empty());
    }

    #[test]
    fn new_app_initializes_a_loaded_file_manager_directory() {
        let app = test_app(None);

        assert!(app.file_browser.view().loaded);
        assert!(!app.file_browser.current_path().is_empty());
    }

    #[test]
    fn initial_file_browser_fallback_retains_preferred_directory_error() {
        let preferred_path = r"C:\preferred".to_owned();
        let fallback_path = r"C:\fallback".to_owned();
        let preferred_error = FileSystemError::NotFound(preferred_path.clone());

        let state = initialize_file_browser_with(
            preferred_path.clone(),
            Some(fallback_path.clone()),
            |path| {
                if path == preferred_path {
                    Err(preferred_error.clone())
                } else {
                    Ok(fixture_entries(path))
                }
            },
        );

        assert_eq!(state.current_path(), fallback_path);
        assert_eq!(state.entries(), fixture_entries(r"C:\fallback").as_slice());
        assert_eq!(state.last_error(), Some(&preferred_error));
        assert_eq!(state.view().last_error, Some(preferred_error));
    }

    #[test]
    fn injected_directory_entries_are_presented_by_the_file_manager() {
        let mut app = test_app(None);

        app.load_file_browser_with(|path| Ok(fixture_entries(path)))
            .expect("injected directory should load");

        let view = app.file_browser.view();
        assert_eq!(view.entries.len(), 2);
        assert_eq!(view.entries[0].metadata.kind, FileKind::Directory);
        assert_eq!(view.entries[1].metadata.kind, FileKind::File);
    }

    #[test]
    fn file_manager_path_controls_preserve_history() {
        let mut app = test_app(None);
        app.load_file_browser_with(|path| Ok(fixture_entries(path)))
            .unwrap();
        let root = app.file_browser.current_path().to_owned();

        app.navigate_file_browser_with("child", |path| Ok(fixture_entries(path)))
            .unwrap();
        assert_eq!(app.file_browser.current_path(), "child");
        assert_eq!(app.file_browser.back_stack(), std::slice::from_ref(&root));

        app.go_back_file_browser_with(|path| Ok(fixture_entries(path)))
            .unwrap();
        assert_eq!(app.file_browser.current_path(), root);
        app.go_forward_file_browser_with(|path| Ok(fixture_entries(path)))
            .unwrap();
        assert_eq!(app.file_browser.current_path(), "child");
    }

    #[test]
    fn file_manager_filter_and_sort_controls_update_the_view() {
        let mut app = test_app(None);
        app.load_file_browser_with(|path| Ok(fixture_entries(path)))
            .unwrap();

        app.set_file_browser_filter("beta");
        app.set_file_browser_sort(SortField::Size, SortDirection::Descending);

        let view = app.file_browser.view();
        assert_eq!(view.entries.len(), 1);
        assert!(view.entries[0].metadata.path.ends_with(r"\beta.txt"));
    }

    #[test]
    fn empty_file_manager_directory_has_stable_empty_copy() {
        let mut app = test_app(None);

        app.load_file_browser_with(|_| Ok(Vec::new()))
            .expect("empty directory should load");

        let view = app.file_browser.view();
        assert!(view.loaded);
        assert!(view.is_empty);
        assert!(view.entries.is_empty());
    }

    #[test]
    fn failed_file_manager_action_keeps_contents_and_adds_one_error() {
        let mut app = test_app(None);
        app.load_file_browser_with(|path| Ok(fixture_entries(path)))
            .unwrap();
        let path = app.file_browser.current_path().to_owned();
        let entries = app.file_browser.entries().to_vec();

        let error = app.navigate_file_browser_with("missing", |_| {
            Err(FileSystemError::NotFound("missing".to_owned()))
        });

        assert!(error.is_err());
        assert_eq!(app.file_browser.current_path(), path);
        assert_eq!(app.file_browser.entries(), entries.as_slice());
        assert!(app.file_browser.view().last_error.is_some());
        assert_eq!(app.notifications.notifications().len(), 1);
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
    fn ctrl_k_from_egui_input_opens_the_switcher() {
        let mut app = test_app(None);
        let context = egui::Context::default();

        run_input(
            &mut app,
            &context,
            key_input(egui::Key::K, egui::Modifiers::CTRL),
        );

        assert!(app.navigation.switcher_open);
    }

    #[test]
    fn ctrl_2_from_egui_input_routes_to_system_monitor() {
        let mut app = test_app(None);
        let context = egui::Context::default();

        run_input(
            &mut app,
            &context,
            key_input(egui::Key::Num2, egui::Modifiers::CTRL),
        );

        assert_eq!(app.navigation.current_page, PageId::SystemMonitor);
    }

    #[test]
    fn esc_does_not_clear_notifications_while_notification_panel_is_open() {
        let mut app = test_app(None);
        app.notifications
            .push(crate::notifications::NotificationKind::Info, "keep me");
        app.notifications_open = true;
        let context = egui::Context::default();

        run_input(
            &mut app,
            &context,
            key_input(egui::Key::Escape, egui::Modifiers::NONE),
        );

        assert!(app.notifications_open);
        assert_eq!(app.notifications.notifications().len(), 1);
    }

    #[test]
    fn switcher_selection_changes_page_and_closes_switcher() {
        let mut app = test_app(None);
        app.navigation.switcher_open = true;

        app.select_switcher_page(PageId::SystemMonitor);

        assert_eq!(app.navigation.current_page, PageId::SystemMonitor);
        assert!(!app.navigation.switcher_open);
    }

    #[test]
    fn notification_panel_actions_have_explicit_open_clear_and_close_transitions() {
        let mut app = test_app(None);
        app.notifications
            .push(crate::notifications::NotificationKind::Info, "read me");

        app.open_notification_panel();
        assert!(app.notifications_open);
        assert_eq!(app.notifications.unread_count(), 0);

        app.clear_notifications();
        assert!(app.notifications.notifications().is_empty());

        app.close_notification_panel();
        assert!(!app.notifications_open);
    }

    #[test]
    fn notification_entry_opens_panel_and_marks_notifications_read_through_egui() {
        let mut app = test_app(None);
        app.notifications
            .push(crate::notifications::NotificationKind::Info, "read me");
        let context = egui::Context::default();

        run_top_frame(&mut app, &context, egui::RawInput::default());
        let notification_button = context
            .interactive_rects_last_pass()
            .into_iter()
            .last()
            .expect("notification entry should be interactive");
        click_top_rect(&mut app, &context, notification_button);

        assert!(app.notifications_open);
        assert_eq!(app.notifications.unread_count(), 0);
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
        assert_eq!(placeholder_text(PageId::FileManager), "");
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
