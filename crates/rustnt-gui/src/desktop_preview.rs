//! Read-only desktop preview (Task 01A / 01B).
//!
//! The preview renders a normal, closable `eframe` window that lists the
//! current top-level application windows and can raise one of them. It never
//! touches the registry, Winlogon, Explorer, the LocalSystem service, or the
//! Task17A shell recovery state.
//!
//! Window discovery lives in [`crate::window_events`]: snapshots arrive when
//! something changed, with a polling fallback when the event hooks are
//! unavailable. This module only turns snapshots into list state.

use std::fmt;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use eframe::egui;
use rustnt_core::window::{activate_window, ActivationOutcome, WindowInfo, WindowSnapshot};

use crate::window_events::{FeedEvent, FeedMode, WindowMonitor};

pub const PREVIEW_FLAG: &str = "--desktop-preview";
pub const PREVIEW_WINDOW_TITLE: &str = "RustNT Desktop - Preview";
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
        // A zero code is not a success signal: Windows simply reported no code.
        ActivationOutcome::Rejected { code: 0 } => {
            "Windows rejected the foreground request without reporting an error code; this is an environment limitation."
                .to_owned()
        }
        ActivationOutcome::Rejected { code } => format!(
            "Windows rejected the foreground request (error {code}); this is an environment limitation."
        ),
    }
}

/// UI-visible state of the window feed.
///
/// The events themselves are [`FeedEvent`]; a failed enumeration arrives as one,
/// so the UI can tell "no snapshot yet" from "enumeration keeps failing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewState {
    /// No result has arrived yet.
    Waiting,
    /// A snapshot has been received (it may still contain zero usable windows).
    Ready,
    /// The feed failed, stopped, or stopped making progress; the message is
    /// user-facing.
    Failed(String),
}

pub fn empty_list_notice(state: &PreviewState) -> String {
    match state {
        PreviewState::Waiting => "Waiting for the first window snapshot...".to_owned(),
        PreviewState::Ready => "No application windows matched the preview filter.".to_owned(),
        PreviewState::Failed(message) => format!("Window feed unavailable: {message}"),
    }
}

/// Pure state decision, so the UI rules stay testable without threads or time.
///
/// `since_last_event` also covers a feed thread that is still alive but blocked
/// inside a Win32 call: it never sends an error, so only the absence of events
/// can reveal the stall. `stall_timeout` comes from the feed mode, because a
/// silent event-driven feed is normal on an idle desktop while a silent polling
/// feed is not.
pub fn preview_state(
    has_snapshot: bool,
    last_error: Option<&str>,
    disconnected: bool,
    since_last_event: Duration,
    stall_timeout: Duration,
) -> PreviewState {
    if disconnected {
        return PreviewState::Failed(
            "the background window feed stopped, so the list no longer updates".to_owned(),
        );
    }
    if since_last_event >= stall_timeout {
        return PreviewState::Failed(format!(
            "no window snapshot arrived for {}s; window enumeration appears to be blocked",
            since_last_event.as_secs()
        ));
    }
    if has_snapshot {
        return PreviewState::Ready;
    }
    match last_error {
        Some(error) => PreviewState::Failed(error.to_owned()),
        None => PreviewState::Waiting,
    }
}

pub struct WindowFeed {
    monitor: WindowMonitor,
    latest: Option<WindowSnapshot>,
    last_error: Option<String>,
    disconnected: bool,
    started: Instant,
    last_event_at: Option<Instant>,
}

impl WindowFeed {
    pub fn spawn() -> Self {
        Self::from_monitor(WindowMonitor::spawn())
    }

    fn from_monitor(monitor: WindowMonitor) -> Self {
        Self {
            monitor,
            latest: None,
            last_error: None,
            disconnected: false,
            started: Instant::now(),
            last_event_at: None,
        }
    }

    /// Feed over a caller-owned channel; used by unit tests to inject events.
    #[cfg(test)]
    fn from_receiver(receiver: std::sync::mpsc::Receiver<FeedEvent>) -> Self {
        Self::from_monitor(WindowMonitor::from_receiver(receiver))
    }

    pub fn poll(&mut self) -> bool {
        let mut updated = false;
        loop {
            match self.monitor.try_recv() {
                Ok(FeedEvent::Snapshot(snapshot)) => {
                    if self.latest.is_none() {
                        let usable = collect_window_items(&snapshot, std::process::id()).len();
                        eprintln!(
                            "[rustnt-preview] first window snapshot ({}): {} windows, {usable} listed",
                            self.mode().label(),
                            snapshot.windows.len()
                        );
                    }
                    self.latest = Some(snapshot);
                    self.last_error = None;
                    self.last_event_at = Some(Instant::now());
                    updated = true;
                }
                Ok(FeedEvent::Failure(message)) => {
                    if self.last_error.as_deref() != Some(message.as_str()) {
                        eprintln!("[rustnt-preview] window enumeration failed: {message}");
                    }
                    self.last_error = Some(message);
                    self.last_event_at = Some(Instant::now());
                    updated = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.disconnected {
                        eprintln!("[rustnt-preview] window feed stopped: sender disconnected");
                    }
                    self.disconnected = true;
                    updated = true;
                    break;
                }
            }
        }
        updated
    }

    pub fn latest(&self) -> Option<&WindowSnapshot> {
        self.latest.as_ref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn mode(&self) -> FeedMode {
        self.monitor.mode()
    }

    /// Why the feed is not using window events, if it is not.
    pub fn mode_notice(&self) -> Option<&str> {
        self.monitor.fallback_reason()
    }

    fn since_last_event(&self) -> Duration {
        self.last_event_at.unwrap_or(self.started).elapsed()
    }

    pub fn state(&self) -> PreviewState {
        preview_state(
            self.latest.is_some(),
            self.last_error.as_deref(),
            self.disconnected,
            self.since_last_event(),
            self.mode().stall_timeout(),
        )
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
                ui.label(
                    egui::RichText::new(format!("updates: {}", self.feed.mode().label()))
                        .weak()
                        .size(11.0),
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

            let state = self.feed.state();
            if let PreviewState::Failed(message) = &state {
                ui.colored_label(
                    egui::Color32::from_rgb(0xe0, 0x6c, 0x6c),
                    format!("Window feed problem: {message}"),
                );
            } else if let Some(error) = self.feed.last_error() {
                ui.colored_label(
                    egui::Color32::from_rgb(0xf0, 0xc0, 0x60),
                    format!("Last refresh failed: {error}"),
                );
            } else if let Some(reason) = self.feed.mode_notice() {
                // Degraded, not broken: the list still updates, just on a timer.
                ui.colored_label(
                    egui::Color32::from_rgb(0xf0, 0xc0, 0x60),
                    format!("Window events unavailable: {reason}"),
                );
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                if items.is_empty() {
                    ui.label(empty_list_notice(&state));
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
    use crate::window_events::{EVENT_STALL_TIMEOUT, POLLING_STALL_TIMEOUT};
    use std::sync::mpsc;
    use std::thread;

    /// Shorthand for the state decision in event-driven mode.
    fn event_state(
        has_snapshot: bool,
        last_error: Option<&str>,
        disconnected: bool,
        since_last_event: Duration,
    ) -> PreviewState {
        preview_state(
            has_snapshot,
            last_error,
            disconnected,
            since_last_event,
            EVENT_STALL_TIMEOUT,
        )
    }

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

    fn snapshot(titles: &[&str]) -> WindowSnapshot {
        WindowSnapshot {
            current_session_id: 1,
            current_desktop: "Default".to_owned(),
            windows: titles
                .iter()
                .map(|title| window("Notepad", title, 7, true))
                .collect(),
            skipped_windows: 0,
        }
    }

    #[test]
    fn waiting_is_only_reported_before_the_first_event() {
        assert_eq!(
            event_state(false, None, false, Duration::from_millis(200)),
            PreviewState::Waiting
        );
        assert_eq!(
            empty_list_notice(&PreviewState::Waiting),
            "Waiting for the first window snapshot..."
        );
    }

    #[test]
    fn first_snapshot_clears_the_waiting_state() {
        let (sender, receiver) = mpsc::channel();
        let mut feed = WindowFeed::from_receiver(receiver);
        assert_eq!(feed.state(), PreviewState::Waiting);

        sender
            .send(FeedEvent::Snapshot(snapshot(&["alpha"])))
            .unwrap();
        assert!(feed.poll());

        assert_eq!(feed.state(), PreviewState::Ready);
        assert_eq!(feed.latest().map(|snap| snap.windows.len()), Some(1));
    }

    #[test]
    fn an_empty_snapshot_is_ready_not_waiting() {
        let (sender, receiver) = mpsc::channel();
        let mut feed = WindowFeed::from_receiver(receiver);
        sender.send(FeedEvent::Snapshot(snapshot(&[]))).unwrap();
        feed.poll();

        let state = feed.state();
        assert_eq!(state, PreviewState::Ready);
        assert_ne!(
            empty_list_notice(&state),
            empty_list_notice(&PreviewState::Waiting)
        );
        assert!(empty_list_notice(&state).contains("No application windows"));
    }

    #[test]
    fn enumeration_failure_is_reported_instead_of_waiting() {
        let (sender, receiver) = mpsc::channel();
        let mut feed = WindowFeed::from_receiver(receiver);
        sender
            .send(FeedEvent::Failure("window enumeration: denied".to_owned()))
            .unwrap();
        feed.poll();

        let state = feed.state();
        assert!(matches!(state, PreviewState::Failed(_)));
        assert!(empty_list_notice(&state).contains("denied"));
        assert_eq!(feed.last_error(), Some("window enumeration: denied"));
    }

    #[test]
    fn a_dropped_sender_is_reported_as_a_stopped_feed() {
        let (sender, receiver) = mpsc::channel::<FeedEvent>();
        let mut feed = WindowFeed::from_receiver(receiver);
        sender
            .send(FeedEvent::Snapshot(snapshot(&["alpha"])))
            .unwrap();
        feed.poll();
        drop(sender);
        feed.poll();

        let state = feed.state();
        assert!(matches!(state, PreviewState::Failed(_)));
        assert!(empty_list_notice(&state).contains("no longer updates"));
    }

    #[test]
    fn a_silent_feed_is_reported_as_blocked_after_the_stall_timeout() {
        let state = preview_state(false, None, false, EVENT_STALL_TIMEOUT, EVENT_STALL_TIMEOUT);
        assert!(matches!(state, PreviewState::Failed(_)));
        assert!(empty_list_notice(&state).contains("blocked"));
    }

    #[test]
    fn an_idle_event_driven_feed_is_not_a_stall() {
        // Nothing arrives between reconciliations; that is normal, not broken.
        let idle = event_state(
            true,
            None,
            false,
            EVENT_STALL_TIMEOUT - Duration::from_secs(1),
        );
        assert_eq!(idle, PreviewState::Ready);

        // The same silence in polling mode means three missed refreshes.
        let polling = preview_state(
            true,
            None,
            false,
            POLLING_STALL_TIMEOUT,
            POLLING_STALL_TIMEOUT,
        );
        assert!(matches!(polling, PreviewState::Failed(_)));
        assert_eq!(
            event_state(true, None, false, POLLING_STALL_TIMEOUT),
            PreviewState::Ready
        );
    }

    #[test]
    fn the_feed_reports_the_mode_it_is_using() {
        let (sender, receiver) = mpsc::channel();
        let feed = WindowFeed::from_receiver(receiver);
        assert_eq!(feed.mode(), FeedMode::EventDriven);
        assert!(feed.mode_notice().is_none());
        assert_eq!(feed.mode().label(), "event-driven");
        drop(sender);
    }

    #[test]
    fn refresh_replaces_the_previous_snapshot() {
        let (sender, receiver) = mpsc::channel();
        let mut feed = WindowFeed::from_receiver(receiver);
        sender
            .send(FeedEvent::Snapshot(snapshot(&["alpha", "beta"])))
            .unwrap();
        feed.poll();
        assert_eq!(feed.latest().unwrap().windows.len(), 2);

        sender
            .send(FeedEvent::Snapshot(snapshot(&["gamma"])))
            .unwrap();
        feed.poll();

        let latest = feed.latest().unwrap();
        assert_eq!(latest.windows.len(), 1);
        assert_eq!(latest.windows[0].title, "gamma");
        assert_eq!(feed.state(), PreviewState::Ready);
    }

    #[test]
    fn a_failure_after_a_successful_snapshot_keeps_the_last_snapshot() {
        let (sender, receiver) = mpsc::channel();
        let mut feed = WindowFeed::from_receiver(receiver);
        sender
            .send(FeedEvent::Snapshot(snapshot(&["alpha"])))
            .unwrap();
        feed.poll();
        sender
            .send(FeedEvent::Failure("temporary".to_owned()))
            .unwrap();
        feed.poll();

        assert_eq!(feed.state(), PreviewState::Ready);
        assert_eq!(feed.last_error(), Some("temporary"));
        assert_eq!(feed.latest().unwrap().windows.len(), 1);
    }

    /// Does this snapshot describe a session with no top-level window at all?
    ///
    /// That is what a non-interactive window station (session 0, headless CI)
    /// looks like: `EnumWindows` succeeds and yields nothing. A filter
    /// regression is excluded here because windows dropped by the session,
    /// desktop or identity checks are counted in `skipped_windows`, so a broken
    /// filter still reports non-zero skips instead of looking like this case.
    fn no_interactive_desktop(snapshot: &WindowSnapshot) -> bool {
        snapshot.windows.is_empty() && snapshot.skipped_windows == 0
    }

    #[test]
    fn spawned_feed_delivers_a_snapshot() {
        let _guard = crate::window_events::lock_live_pump();
        let mut feed = WindowFeed::spawn();
        // No snapshot at all within the deadline is a feed failure, not an
        // environment limitation: it must fail loudly.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline && feed.latest().is_none() {
            feed.poll();
            thread::sleep(Duration::from_millis(50));
        }
        let snapshot = feed.latest().expect("feed must deliver a snapshot");

        if no_interactive_desktop(snapshot) {
            eprintln!(
                "skipping spawned_feed_delivers_a_snapshot: this session has no interactive \
                 desktop (windows=0, skipped=0)"
            );
            return;
        }

        let items = collect_window_items(snapshot, std::process::id());
        println!(
            "snapshot windows={} skipped={} items={}",
            snapshot.windows.len(),
            snapshot.skipped_windows,
            items.len()
        );
        assert!(
            !snapshot.windows.is_empty(),
            "enumeration returned no windows while reporting {} skipped windows: \
             the session/desktop filter is dropping every window",
            snapshot.skipped_windows
        );
        assert!(
            items.iter().all(|item| item.pid != std::process::id()),
            "the preview list must never contain this process's own windows"
        );
    }

    #[test]
    fn activation_notice_reports_environment_limitations() {
        assert!(activation_notice(ActivationOutcome::Activated).contains("activated"));
        assert!(
            activation_notice(ActivationOutcome::AcceptedButNotForeground).contains("foreground")
        );
        assert!(activation_notice(ActivationOutcome::Rejected { code: 5 }).contains("5"));
        // A zero code is a rejection without a code, never a success message.
        let zero = activation_notice(ActivationOutcome::Rejected { code: 0 });
        assert!(zero.contains("rejected"));
        assert!(zero.contains("without reporting an error code"));
        assert!(!zero.contains("error 0"));
    }
}
