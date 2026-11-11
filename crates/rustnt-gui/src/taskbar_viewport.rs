//! B1 experimental taskbar viewport skeleton.
//!
//! Scope is a static placeholder only. This module deliberately owns no
//! window-tracking data source, registers no OS event hooks, starts no
//! background thread, and does not touch configuration, crash markers,
//! shell session state or the Windows shell. Every transition below is
//! driven by an explicit user action in the main window.

use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui;

/// Fixed seed for the single experimental viewport. A hash of a constant
/// string gives a stable identity across frames and across process runs.
pub const VIEWPORT_ID_SEED: &str = "rustnt.taskbar_preview.b1";

pub const WINDOW_TITLE: &str = "RustNT Taskbar Preview";
pub const PLACEHOLDER_HEADING: &str = "RustNT Taskbar Preview";
pub const OPEN_BUTTON_LABEL: &str = "Open Taskbar Preview";
pub const CLOSE_BUTTON_LABEL: &str = "Close Taskbar Preview";
pub const CLOSE_INSTRUCTIONS: &str = "Use the title-bar X or the button below to close this window. Closing it does not exit the main RustNT window.";

pub const NOTICE_CLOSED: &str = "Taskbar Preview is closed. Use the button to open it.";
pub const NOTICE_OPEN: &str = "Taskbar Preview is open. Close its window to release it.";
pub const NOTICE_ALREADY_OPEN: &str =
    "Taskbar Preview is already open; no second window was created.";
pub const NOTICE_REOPENED: &str = "Taskbar Preview was reopened.";
pub const NOTICE_SHUTDOWN: &str = "Taskbar Preview released on application exit.";

/// The one stable identity used by the deferred viewport. Reusing it is what
/// makes repeated open requests idempotent instead of creating extra windows.
pub fn taskbar_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of(VIEWPORT_ID_SEED)
}

/// Ordinary decorated window: no topmost level, no transparency, no hit-test
/// passthrough, no monitor targeting, no fullscreen, no position override.
/// Leaving the fields unset keeps the backend defaults (decorated, opaque,
/// normal z-order, normal focus behaviour).
pub fn viewport_builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_inner_size([420.0, 260.0])
        .with_min_inner_size([360.0, 220.0])
        .with_resizable(true)
        .with_close_button(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportStatus {
    Closed,
    Open,
}

impl ViewportStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Closed => "Closed",
            Self::Open => "Open",
        }
    }
}

/// Only an explicit application shutdown may set `AppShutdown`. Closing the
/// child viewport must never set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitIntent {
    None,
    AppShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    Opened,
    Reopened,
    AlreadyOpen,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed,
    AlreadyClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarViewportState {
    pub status: ViewportStatus,
    pub notice: &'static str,
    pub exit_intent: ExitIntent,
    pub open_count: u64,
    pub close_count: u64,
}

impl Default for TaskbarViewportState {
    fn default() -> Self {
        Self {
            status: ViewportStatus::Closed,
            notice: NOTICE_CLOSED,
            exit_intent: ExitIntent::None,
            open_count: 0,
            close_count: 0,
        }
    }
}

/// Shared, pure-data controller. It holds no OS handle, no channel and no
/// thread; dropping it releases nothing but a mutex-protected value.
pub struct TaskbarViewportController {
    state: Mutex<TaskbarViewportState>,
    run_mode: &'static str,
}

impl TaskbarViewportController {
    pub fn new(run_mode: &'static str) -> Self {
        Self {
            state: Mutex::new(TaskbarViewportState::default()),
            run_mode,
        }
    }

    fn lock(&self) -> MutexGuard<'_, TaskbarViewportState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn state(&self) -> TaskbarViewportState {
        self.lock().clone()
    }

    pub fn is_open(&self) -> bool {
        self.lock().status == ViewportStatus::Open
    }

    pub fn run_mode(&self) -> &'static str {
        self.run_mode
    }

    /// Explicit user action. Idempotent while already open.
    pub fn request_open(&self) -> OpenOutcome {
        let mut state = self.lock();
        if state.exit_intent == ExitIntent::AppShutdown {
            return OpenOutcome::ShuttingDown;
        }
        if state.status == ViewportStatus::Open {
            state.notice = NOTICE_ALREADY_OPEN;
            return OpenOutcome::AlreadyOpen;
        }
        let first_open = state.open_count == 0;
        state.status = ViewportStatus::Open;
        state.open_count += 1;
        state.notice = if first_open {
            NOTICE_OPEN
        } else {
            NOTICE_REOPENED
        };
        if first_open {
            OpenOutcome::Opened
        } else {
            OpenOutcome::Reopened
        }
    }

    /// Closing the child viewport only flips its own status. It never asks
    /// the application to exit and never touches recovery state.
    pub fn request_close(&self) -> CloseOutcome {
        let mut state = self.lock();
        if state.status == ViewportStatus::Closed {
            return CloseOutcome::AlreadyClosed;
        }
        state.status = ViewportStatus::Closed;
        state.close_count += 1;
        state.notice = NOTICE_CLOSED;
        CloseOutcome::Closed
    }

    /// Invoked once from the application shutdown hook. After this point no
    /// new viewport may be opened and no child viewport is kept alive.
    pub fn shutdown_for_app_exit(&self) {
        let mut state = self.lock();
        state.status = ViewportStatus::Closed;
        state.notice = NOTICE_SHUTDOWN;
        state.exit_intent = ExitIntent::AppShutdown;
    }

    /// Register the deferred viewport for this pass. Calling it every pass is
    /// required by the egui contract; not calling it removes the child window.
    pub fn show_if_open(self: &Arc<Self>, ctx: &egui::Context) {
        if !self.is_open() {
            return;
        }
        let controller = Arc::clone(self);
        ctx.show_viewport_deferred(
            taskbar_viewport_id(),
            viewport_builder(),
            move |ui, class| {
                if ui.input(|input| input.viewport().close_requested()) {
                    controller.request_close();
                    ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                    return;
                }
                render_placeholder(ui, &controller, class);
            },
        );
    }
}

fn viewport_class_label(class: egui::ViewportClass) -> &'static str {
    match class {
        egui::ViewportClass::Deferred => "deferred (independent native window)",
        egui::ViewportClass::EmbeddedWindow => "embedded fallback (no separate OS window)",
        egui::ViewportClass::Immediate => "immediate",
        egui::ViewportClass::Root => "root",
    }
}

fn render_placeholder(
    ui: &mut egui::Ui,
    controller: &Arc<TaskbarViewportController>,
    class: egui::ViewportClass,
) {
    let state = controller.state();
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading(PLACEHOLDER_HEADING);
        ui.separator();
        ui.label(format!("Run mode: {}", controller.run_mode()));
        ui.label(format!("Viewport state: {}", state.status.label()));
        ui.label(format!("Viewport class: {}", viewport_class_label(class)));
        ui.separator();
        ui.label(CLOSE_INSTRUCTIONS);
        ui.separator();
        ui.label("Static layout placeholder (no live window list in B1):");
        ui.horizontal(|ui| {
            ui.label("[ item ]");
            ui.label("[ item ]");
            ui.label("[ item ]");
        });
        ui.add_space(8.0);
        if ui.button(CLOSE_BUTTON_LABEL).clicked() {
            controller.request_close();
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_preview::{parse_startup_mode, StartupMode, StartupModeError, PREVIEW_FLAG};
    use eframe::egui;

    fn controller() -> TaskbarViewportController {
        TaskbarViewportController::new("standard GUI (B1 experimental)")
    }

    /// Strip CRLF and cut off the test module so a source guard only inspects
    /// production code (the guard's own token list must not be scanned).
    fn production_part(source: &str) -> String {
        let normalized = source.replace("\r\n", "\n");
        let marker = "\n#[cfg(test)]\nmod tests";
        match normalized.find(marker) {
            Some(index) => normalized[..index].to_owned(),
            None => normalized,
        }
    }

    const MODULE_FORBIDDEN: &[&str] = &[
        "WindowFeed",
        "WindowMonitor",
        "SetWinEventHook",
        "EnumWindows",
        "thread::spawn",
        "request_repaint_after",
        "SetForegroundWindow",
        "SetWindowPos",
        "SHAppBarMessage",
        "force_window_active",
        "ShellRecovery",
        "create_marker",
        "begin_startup",
        "complete_normal_exit",
        "shell.session",
        "StartupMode",
        "std::process::exit",
        "ViewportCommand::Focus",
        "with_monitor",
        "with_always_on_top",
        "with_mouse_passthrough",
        "with_transparent",
        "with_decorations(false)",
        "explorer.exe",
    ];

    const APP_FORBIDDEN: &[&str] = &[
        "WindowFeed",
        "WindowMonitor",
        "SetWinEventHook",
        "EnumWindows",
        "thread::spawn",
        "request_repaint_after",
        "SetForegroundWindow",
        "SHAppBarMessage",
        "SetWindowPos",
        "force_window_active",
        "create_marker",
        "begin_startup",
        "ViewportCommand::Focus",
        "with_monitor",
        "std::process::exit",
    ];

    // 1. Viewport 状态初始关闭
    #[test]
    fn viewport_starts_closed_with_no_pending_exit() {
        let state = controller().state();
        assert_eq!(state.status, ViewportStatus::Closed);
        assert_eq!(state.notice, NOTICE_CLOSED);
        assert_eq!(state.exit_intent, ExitIntent::None);
        assert_eq!(state.open_count, 0);
        assert_eq!(state.close_count, 0);
    }

    // 2. 用户请求打开
    #[test]
    fn explicit_user_request_opens_the_viewport() {
        let controller = controller();
        assert_eq!(controller.request_open(), OpenOutcome::Opened);
        assert!(controller.is_open());
        let state = controller.state();
        assert_eq!(state.status, ViewportStatus::Open);
        assert_eq!(state.notice, NOTICE_OPEN);
        assert_eq!(state.open_count, 1);
    }

    // 3. 重复打开幂等
    #[test]
    fn repeated_open_requests_are_idempotent() {
        let controller = controller();
        assert_eq!(controller.request_open(), OpenOutcome::Opened);
        assert_eq!(controller.request_open(), OpenOutcome::AlreadyOpen);
        let state = controller.state();
        assert_eq!(state.status, ViewportStatus::Open);
        assert_eq!(state.open_count, 1);
        assert_eq!(state.notice, NOTICE_ALREADY_OPEN);
        assert_eq!(taskbar_viewport_id(), taskbar_viewport_id());
    }

    // 4. 请求关闭
    #[test]
    fn explicit_close_request_closes_the_viewport() {
        let controller = controller();
        controller.request_open();
        assert_eq!(controller.request_close(), CloseOutcome::Closed);
        let state = controller.state();
        assert_eq!(state.status, ViewportStatus::Closed);
        assert_eq!(state.close_count, 1);
        assert_eq!(state.notice, NOTICE_CLOSED);
        assert_eq!(controller.request_close(), CloseOutcome::AlreadyClosed);
    }

    // 5. 关闭后再次打开
    #[test]
    fn viewport_can_be_reopened_after_close() {
        let controller = controller();
        controller.request_open();
        controller.request_close();
        assert_eq!(controller.request_open(), OpenOutcome::Reopened);
        let state = controller.state();
        assert_eq!(state.status, ViewportStatus::Open);
        assert_eq!(state.open_count, 2);
        assert_eq!(state.close_count, 1);
        assert_eq!(state.notice, NOTICE_REOPENED);
    }

    // 6. 子 viewport 关闭不触发主程序退出
    #[test]
    fn closing_child_viewport_does_not_request_application_exit() {
        let controller = controller();
        controller.request_open();
        let _ = controller.request_close();
        let state = controller.state();
        assert_eq!(state.exit_intent, ExitIntent::None);
        assert_eq!(state.status, ViewportStatus::Closed);
        assert_eq!(controller.request_open(), OpenOutcome::Reopened);
    }

    // 7. 主程序退出清理状态
    #[test]
    fn application_shutdown_releases_viewport_state() {
        let controller = controller();
        controller.request_open();
        controller.shutdown_for_app_exit();
        let state = controller.state();
        assert_eq!(state.status, ViewportStatus::Closed);
        assert_eq!(state.exit_intent, ExitIntent::AppShutdown);
        assert_eq!(state.notice, NOTICE_SHUTDOWN);
        assert!(!controller.is_open());
        assert_eq!(controller.request_open(), OpenOutcome::ShuttingDown);
    }

    // 8. 不创建第二个 WindowFeed / 不新增后台轮询
    #[test]
    fn b1_module_contains_no_window_tracking_or_background_polling() {
        let production = production_part(include_str!("taskbar_viewport.rs"));
        for &token in MODULE_FORBIDDEN {
            assert!(
                !production.contains(token),
                "B1 module must not contain {token}"
            );
        }
        assert_eq!(
            production.matches("ctx.show_viewport_deferred").count(),
            1,
            "exactly one deferred viewport registration site"
        );
        assert!(
            production.contains("show_if_open"),
            "show_if_open must be called from the app each pass"
        );
    }

    // 8b. app.rs 入口不引入第二套追踪或抢焦点路径
    #[test]
    fn app_entry_does_not_add_a_second_data_source_or_focus_hack() {
        let production = production_part(include_str!("app.rs"));
        for &token in APP_FORBIDDEN {
            assert!(
                !production.contains(token),
                "app.rs production code must not contain {token}"
            );
        }
        assert_eq!(
            production.matches("show_if_open(&ctx)").count(),
            1,
            "exactly one per-pass viewport registration in app.rs"
        );
    }

    // 9. 不触发 Shell Recovery
    #[test]
    fn viewport_lifecycle_never_enters_recovery_or_marker_paths() {
        let production = production_part(include_str!("taskbar_viewport.rs"));
        for token in [
            "ShellRecovery",
            "create_marker",
            "begin_startup",
            "shell.session",
            "complete_normal_exit",
        ] {
            assert!(
                !production.contains(token),
                "B1 must not touch recovery state: {token}"
            );
        }
        let controller = controller();
        controller.request_open();
        let _ = controller.request_close();
        assert_eq!(controller.state().exit_intent, ExitIntent::None);
    }

    // 10. 不改变 StartupMode
    #[test]
    fn startup_mode_semantics_are_unchanged() {
        assert_eq!(
            parse_startup_mode(Vec::<String>::new()).unwrap(),
            StartupMode::Standard
        );
        assert_eq!(
            parse_startup_mode(vec![PREVIEW_FLAG.to_owned()]).unwrap(),
            StartupMode::DesktopPreview
        );
        assert!(matches!(
            parse_startup_mode(vec!["--unknown".to_owned()]),
            Err(StartupModeError::UnknownArgument(_))
        ));
        assert!(matches!(
            parse_startup_mode(vec![PREVIEW_FLAG.to_owned(), "extra".to_owned()]),
            Err(StartupModeError::TooManyArguments)
        ));
        let main_source = production_part(include_str!("main.rs"));
        assert!(main_source.contains("StartupMode::Standard => run_gui(),"));
        assert!(main_source.contains("StartupMode::DesktopPreview => run_preview(),"));
        let preview_source = production_part(include_str!("desktop_preview.rs"));
        assert!(preview_source.contains("pub enum StartupMode {"));
        assert_eq!(preview_source.matches("DesktopPreview,").count(), 1);
    }

    // 固定 ViewportId
    #[test]
    fn viewport_id_is_stable_and_is_not_the_root() {
        let id = taskbar_viewport_id();
        assert_eq!(id, egui::ViewportId::from_hash_of(VIEWPORT_ID_SEED));
        assert_ne!(id, egui::ViewportId::ROOT);
        assert_eq!(VIEWPORT_ID_SEED, "rustnt.taskbar_preview.b1");
    }

    // 普通带装饰窗口，且无危险样式
    #[test]
    fn viewport_builder_uses_only_safe_defaults() {
        let builder = viewport_builder();
        assert_eq!(builder.title.as_deref(), Some(WINDOW_TITLE));
        assert_eq!(builder.decorations, None, "keep default decorations");
        assert_eq!(builder.transparent, None, "keep the window opaque");
        assert_eq!(builder.mouse_passthrough, None, "keep hit testing enabled");
        assert_eq!(builder.window_level, None, "no always-on-top");
        assert_eq!(builder.monitor, None, "no monitor targeting");
        assert_eq!(builder.position, None, "no auto-docking");
        assert_eq!(builder.fullscreen, None, "no fullscreen");
        assert_eq!(builder.active, None, "no focus policy override");
        assert_eq!(builder.close_button, Some(true));
        assert!(builder.inner_size.is_some());
    }

    // 关闭态下不注册视口（不自动弹出）
    #[test]
    fn closed_state_registers_no_viewport() {
        let controller = Arc::new(controller());
        let ctx = egui::Context::default();
        controller.show_if_open(&ctx);
        assert!(!controller.is_open());
        assert_eq!(controller.state().open_count, 0);
    }
}
