//! Read-only desktop preview (Task 01A).
//!
//! The preview renders a normal, closable `eframe` window that lists the
//! current top-level application windows and can raise one of them. It never
//! touches the registry, Winlogon, Explorer, the LocalSystem service, or the
//! Task17A shell recovery state.

use std::fmt;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use eframe::egui;
use rustnt_core::window::{
    activate_window, enumerate_windows, ActivationOutcome, WindowInfo, WindowSnapshot,
};

pub const PREVIEW_FLAG: &str = "--desktop-preview";
pub const PREVIEW_WINDOW_TITLE: &str = "RustNT Desktop - Preview";
pub const PREVIEW_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub const MAX_TASKBAR_ITEMS: usize = 8;

/// Top-level windows that belong to the Windows shell itself rather than to an
/// application. They are hidden from the preview list.
const SHELL_WINDOW_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "ApplicationManager_DesktopShellWindow",
    "MultitaskingViewFrame",
    "ForegroundStaging",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Standard,
    DesktopPreview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupModeError {
    UnknownArgument(String),
    TooManyArguments,
}

impl fmt::Display for StartupModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownArgument(argument) => {
                write!(formatter, "unknown argument: {argument}")
            }
            Self::TooManyArguments => formatter.write_str("expected at most one argument"),
        }
    }
}

impl std::error::Error for StartupModeError {}

pub fn parse_startup_mode(
    arguments: impl IntoIterator<Item = String>,
) -> Result<StartupMode, StartupModeError> {
    let mut arguments = arguments.into_iter();
    match (arguments.next(), arguments.next()) {
        (None, _) => Ok(StartupMode::Standard),
        (Some(flag), None) if flag == PREVIEW_FLAG => Ok(StartupMode::DesktopPreview),
        (Some(flag), None) => Err(StartupModeError::UnknownArgument(flag)),
        (Some(_), Some(_)) => Err(StartupModeError::TooManyArguments),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowListItem {
    pub hwnd: usize,
    pub title: String,
    pub pid: u32,
    pub foreground: bool,
    pub minimized: bool,
}

pub fn is_preview_window(window: &WindowInfo, self_pid: u32) -> bool {
    if window.pid == self_pid {
        return false;
    }
    if !window.visible || window.title.trim().is_empty() {
        return false;
    }
    !SHELL_WINDOW_CLASSES
        .iter()
        .any(|class_name| class_name.eq_ignore_ascii_case(&window.class_name))
}

pub fn collect_window_items(snapshot: &WindowSnapshot, self_pid: u32) -> Vec<WindowListItem> {
    snapshot
        .windows
        .iter()
        .filter(|window| is_preview_window(window, self_pid))
        .map(|window| WindowListItem {
            hwnd: window.hwnd,
            title: window.title.clone(),
            pid: window.pid,
            foreground: window.foreground,
            minimized: window.minimized,
        })
        .collect()
}

pub fn sort_window_items(items: &mut [WindowListItem]) {
    items.sort_by(|left, right| {
        right
            .foreground
            .cmp(&left.foreground)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });
}

pub fn taskbar_items(items: &[WindowListItem]) -> &[WindowListItem] {
    let limit = items.len().min(MAX_TASKBAR_ITEMS);
    &items[..limit]
}

pub fn activation_notice(outcome: ActivationOutcome) -> String {
    match outcome {
        ActivationOutcome::Activated => "Window activated.".to_owned(),
        ActivationOutcome::AcceptedButNotForeground => {
            "Activation was accepted but Windows kept the foreground window (foreground policy limitation)."
                .to_owned()
        }
        ActivationOutcome::Rejected { code } => format!(
            "Windows rejected the foreground request (error {code}); this is an environment limitation."
        ),
    }
}

pub struct WindowFeed {
    receiver: Receiver<WindowSnapshot>,
    latest: Option<WindowSnapshot>,
}

impl WindowFeed {
    pub fn spawn() -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("rustnt-window-feed".to_owned())
            .spawn(move || loop {
                if let Ok(snapshot) = enumerate_windows() {
                    if sender.send(snapshot).is_err() {
                        break;
                    }
                }
                thread::sleep(PREVIEW_REFRESH_INTERVAL);
            })
            .expect("spawn window feed thread");
        Self {
            receiver,
            latest: None,
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut updated = false;
        while let Ok(snapshot) = self.receiver.try_recv() {
            self.latest = Some(snapshot);
            updated = true;
        }
        updated
    }

    pub fn latest(&self) -> Option<&WindowSnapshot> {
        self.latest.as_ref()
    }
}

pub struct DesktopPreviewApp {
    feed: WindowFeed,
    notice: Option<String>,
}

impl DesktopPreviewApp {
    pub fn new() -> Self {
        Self {
            feed: WindowFeed::spawn(),
            notice: None,
        }
    }

    fn items(&self) -> Vec<WindowListItem> {
        let Some(snapshot) = self.feed.latest() else {
            return Vec::new();
        };
        let mut items = collect_window_items(snapshot, std::process::id());
        sort_window_items(&mut items);
        items
    }

    fn request_close(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl Default for DesktopPreviewApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for DesktopPreviewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.feed.poll();
        ctx.request_repaint_after(Duration::from_millis(500));
        let items = self.items();

        egui::Panel::top("preview-header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustNT Desktop");
                ui.label(
                    egui::RichText::new("Preview Mode")
                        .strong()
                        .color(egui::Color32::from_rgb(0x2f, 0x9e, 0x6f)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Exit Preview").clicked() {
                        Self::request_close(&ctx);
                    }
                });
            });
        });

        egui::Panel::bottom("preview-taskbar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Taskbar (preview)").weak());
                for item in taskbar_items(&items) {
                    let label = if item.minimized {
                        format!("{} [min]", item.title)
                    } else {
                        item.title.clone()
                    };
                    if ui.button(label).clicked() {
                        self.notice = Some(match activate_window(item.hwnd) {
                            Ok(outcome) => activation_notice(outcome),
                            Err(error) => format!("Activation rejected: {error}"),
                        });
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let rect = ui.available_rect_before_wrap();
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(0x12, 0x16, 0x1c));

            if let Some(notice) = &self.notice {
                ui.colored_label(egui::Color32::from_rgb(0xf0, 0xc0, 0x60), notice);
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                if items.is_empty() {
                    ui.label("Waiting for the first window snapshot...");
                }
                for item in &items {
                    let prefix = if item.foreground { "> " } else { "  " };
                    let suffix = if item.minimized { " [minimized]" } else { "" };
                    let label = format!("{prefix}{}{suffix}", item.title);
                    if ui
                        .selectable_label(item.foreground, format!("{label}    pid {}", item.pid))
                        .clicked()
                    {
                        self.notice = Some(match activate_window(item.hwnd) {
                            Ok(outcome) => activation_notice(outcome),
                            Err(error) => format!("Activation rejected: {error}"),
                        });
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(class_name: &str, title: &str, pid: u32, visible: bool) -> WindowInfo {
        WindowInfo {
            hwnd: 0x1234,
            title: title.to_owned(),
            class_name: class_name.to_owned(),
            pid,
            process_name: Some("app.exe".to_owned()),
            process_path: None,
            thread_id: 1,
            desktop_name: Some("Default".to_owned()),
            session_id: 1,
            visible,
            minimized: false,
            foreground: false,
        }
    }

    fn item(title: &str, hwnd: usize, foreground: bool) -> WindowListItem {
        WindowListItem {
            hwnd,
            title: title.to_owned(),
            pid: 100,
            foreground,
            minimized: false,
        }
    }

    #[test]
    fn startup_mode_defaults_to_standard() {
        assert_eq!(
            parse_startup_mode(std::iter::empty()).unwrap(),
            StartupMode::Standard
        );
    }

    #[test]
    fn startup_mode_accepts_the_preview_flag() {
        assert_eq!(
            parse_startup_mode([PREVIEW_FLAG.to_owned()]).unwrap(),
            StartupMode::DesktopPreview
        );
    }

    #[test]
    fn startup_mode_rejects_unknown_or_extra_arguments() {
        assert_eq!(
            parse_startup_mode(["--shell".to_owned()]),
            Err(StartupModeError::UnknownArgument("--shell".to_owned()))
        );
        assert_eq!(
            parse_startup_mode([PREVIEW_FLAG.to_owned(), "extra".to_owned()]),
            Err(StartupModeError::TooManyArguments)
        );
    }

    #[test]
    fn preview_filter_excludes_own_process_windows() {
        let own = window("RustNT", "RustNT Desktop - Preview", 42, true);
        assert!(!is_preview_window(&own, 42));
    }

    #[test]
    fn preview_filter_excludes_hidden_and_untitled_windows() {
        assert!(!is_preview_window(
            &window("Notepad", "Notes", 7, false),
            42
        ));
        assert!(!is_preview_window(&window("Notepad", "   ", 7, true), 42));
    }

    #[test]
    fn preview_filter_excludes_shell_host_windows() {
        for class_name in ["Progman", "WorkerW", "Shell_TrayWnd", "shell_traywnd"] {
            assert!(
                !is_preview_window(&window(class_name, "Desktop", 7, true), 42),
                "{class_name} must be filtered"
            );
        }
    }

    #[test]
    fn preview_filter_keeps_normal_application_windows() {
        assert!(is_preview_window(
            &window("Notepad", "Untitled - Notepad", 7, true),
            42
        ));
    }

    #[test]
    fn window_items_are_filtered_and_sorted_with_foreground_first() {
        let snapshot = WindowSnapshot {
            current_session_id: 1,
            current_desktop: "Default".to_owned(),
            windows: vec![
                window("Notepad", "zeta", 7, true),
                window("Chrome_WidgetWin_1", "alpha", 8, true),
                window("RustNT", "RustNT Desktop - Preview", 42, true),
            ],
            skipped_windows: 0,
        };

        let mut items = collect_window_items(&snapshot, 42);
        items[1].foreground = true;
        sort_window_items(&mut items);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "alpha");
        assert!(items[0].foreground);
        assert_eq!(items[1].title, "zeta");
    }

    #[test]
    fn sorting_is_case_insensitive_and_stable_for_equal_titles() {
        let mut items = vec![
            item("Beta", 3, false),
            item("alpha", 2, false),
            item("Alpha", 1, false),
        ];
        sort_window_items(&mut items);
        assert_eq!(items[0].hwnd, 2);
        assert_eq!(items[1].hwnd, 1);
        assert_eq!(items[2].hwnd, 3);
    }

    #[test]
    fn taskbar_is_bounded() {
        let items = (0..12)
            .map(|index| item(&format!("window {index}"), index, false))
            .collect::<Vec<_>>();
        assert_eq!(taskbar_items(&items).len(), MAX_TASKBAR_ITEMS);
        assert_eq!(taskbar_items(&items[..3]).len(), 3);
    }

    #[test]
    fn activation_notice_reports_environment_limitations() {
        assert!(activation_notice(ActivationOutcome::Activated).contains("activated"));
        assert!(
            activation_notice(ActivationOutcome::AcceptedButNotForeground).contains("foreground")
        );
        assert!(activation_notice(ActivationOutcome::Rejected { code: 5 }).contains("5"));
    }
}
