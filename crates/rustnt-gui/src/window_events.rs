//! Event-driven window monitoring (Task 01B.1).
//!
//! The desktop preview used to re-enumerate every top-level window on a fixed
//! two second timer, whether or not anything had changed. That costs a full
//! `EnumWindows` pass plus one cross-process `WM_GETTEXT` per window every two
//! seconds, and it leaves the list up to two seconds behind reality.
//!
//! This module replaces the timer with `SetWinEventHook`. The snapshot format
//! does not change: the UI still receives [`FeedEvent`] values over a channel,
//! only their cadence does. When the hooks cannot be installed the monitor
//! degrades to polling instead of failing, because a preview that updates
//! slowly is still correct.
//!
//! # Safety boundaries
//!
//! * The hook procedure is installed with `WINEVENT_OUTOFCONTEXT`, so Windows
//!   delivers events through this module's own message queue and never runs the
//!   callback inside another process. The callback only stores a flag; it does
//!   not enumerate windows, allocate, log, or take a lock.
//! * `enumerate_windows` runs on the pump thread, between message drains, so the
//!   callback can never re-enter it.
//! * Nothing here touches the registry, Winlogon, Explorer, the LocalSystem
//!   service, or the Task17A shell recovery state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use rustnt_core::window::{enumerate_windows, WindowSnapshot};
use windows_sys::Win32::Foundation::{
    GetLastError, HWND, POINT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, PostThreadMessageW,
    TranslateMessage, CHILDID_SELF, EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
    EVENT_SYSTEM_MINIMIZESTART, MSG, MWMO_INPUTAVAILABLE, OBJID_WINDOW, PM_NOREMOVE, PM_REMOVE,
    QS_ALLINPUT, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
};

/// Events arriving inside this window are merged into a single enumeration.
///
/// Without coalescing, dragging a window or a program starting several windows
/// at once would trigger one full `EnumWindows` pass per event.
pub const COALESCE_INTERVAL: Duration = Duration::from_millis(150);

/// Hard upper bound on snapshot age, so a missed event cannot freeze the list.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(20);

/// Upper bound on a single wait in the pump loop.
///
/// This is *not* a poll interval: it only bounds how long the thread can go
/// without draining its message queue. WinEvents are delivered through that
/// queue, so if a platform quirk ever stopped an event from waking the wait,
/// the worst case would still be one second of staleness instead of a whole
/// reconcile interval.
pub const PUMP_HEARTBEAT: Duration = Duration::from_secs(1);

/// Refresh interval used when the hooks are unavailable.
pub const FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How long the UI thread waits for the pump thread to report its mode.
///
/// The pump publishes its mode before its first enumeration, so this is only a
/// safety net against a thread that never starts.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// A silent feed is normal while idle; only a missed reconcile is a stall.
pub const EVENT_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Three missed polls in a row is a stall.
pub const POLLING_STALL_TIMEOUT: Duration = Duration::from_secs(6);

/// Thread message used to stop the pump.
///
/// Deliberately *not* `WM_QUIT`: if the pump thread id were ever stale and
/// reused, `WM_QUIT` would terminate an unrelated thread's message loop (in the
/// worst case the UI thread's). A private message is inert everywhere else.
const PUMP_STOP_MESSAGE: u32 = WM_APP + 1;

/// The top-level window events the preview reacts to.
///
/// Every change that can alter the list has a trigger: create, destroy, show,
/// hide, retitle, focus change, and minimize begin/end. High-frequency geometry
/// events (`EVENT_OBJECT_LOCATIONCHANGE`) are left out on purpose, because
/// moving or resizing a window does not change what the list shows.
pub const WATCHED_EVENTS: [u32; 8] = [
    EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZESTART,
    EVENT_SYSTEM_MINIMIZEEND,
    EVENT_OBJECT_CREATE,
    EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_SHOW,
    EVENT_OBJECT_HIDE,
    EVENT_OBJECT_NAMECHANGE,
];

/// Set by the hook procedure, consumed by the pump thread.
///
/// `WINEVENTPROC` has no user-data parameter, so this signal has to be
/// process-global. That is sound here because the preview runs exactly one
/// monitor per process and its hooks are removed before the pump exits; a
/// leftover flag only causes one redundant enumeration.
static EVENT_PENDING: AtomicBool = AtomicBool::new(false);

/// Serializes tests that start a live pump.
///
/// [`EVENT_PENDING`] is process-global (Windows gives the hook procedure no
/// user-data parameter), so two live pumps in the same test binary would steal
/// each other's wake-ups. Tests that only inspect the scheduling math stay
/// parallel; tests that spawn a pump take this lock.
#[cfg(test)]
pub(crate) static LIVE_PUMP_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_live_pump() -> std::sync::MutexGuard<'static, ()> {
    LIVE_PUMP_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How the feed learns that the window list may have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedMode {
    /// `SetWinEventHook` delivered change notifications.
    EventDriven,
    /// The hooks could not be installed, so the feed polls on a timer.
    Polling,
}

impl FeedMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::EventDriven => "event-driven",
            Self::Polling => "polling fallback",
        }
    }

    /// How long the feed may stay silent before that counts as a stall.
    ///
    /// In event-driven mode an idle desktop produces no events at all, so the
    /// threshold has to clear at least one reconcile interval; otherwise an
    /// idle machine would be reported as broken.
    pub fn stall_timeout(self) -> Duration {
        match self {
            Self::EventDriven => EVENT_STALL_TIMEOUT,
            Self::Polling => POLLING_STALL_TIMEOUT,
        }
    }

    /// `(coalescing window, reconcile interval)` for this mode.
    ///
    /// Polling has no events to coalesce, so the reconcile interval *is* the
    /// poll interval.
    fn schedule(self) -> (Duration, Duration) {
        match self {
            Self::EventDriven => (COALESCE_INTERVAL, RECONCILE_INTERVAL),
            Self::Polling => (FALLBACK_POLL_INTERVAL, FALLBACK_POLL_INTERVAL),
        }
    }
}

/// What the background monitor reported to the UI thread.
///
/// A failed enumeration is an event, not an absence of events: without it the
/// UI cannot tell "no snapshot yet" from "enumeration keeps failing".
#[derive(Debug)]
pub enum FeedEvent {
    Snapshot(WindowSnapshot),
    Failure(String),
}

/// Only top-level window-level notifications are worth a re-enumeration.
///
/// Object events are also delivered for child accessibility elements
/// (`OBJID_CLIENT` and friends) and for notifications that carry no window
/// handle; neither can change the top-level list.
pub fn is_relevant_event(hwnd: usize, id_object: i32, id_child: i32) -> bool {
    hwnd != 0 && id_object == OBJID_WINDOW && id_child == CHILDID_SELF as i32
}

/// How long the pump should wait before its next enumeration.
///
/// Returns [`Duration::ZERO`] when it should enumerate right now. The first
/// pending event is what anchors the coalescing window, so a continuous event
/// stream delays one enumeration by at most `quiet` but can never postpone it
/// indefinitely.
pub fn next_scan_delay(
    now: Instant,
    pending_since: Option<Instant>,
    last_scan: Option<Instant>,
    quiet: Duration,
    reconcile: Duration,
) -> Duration {
    let deadline = match (pending_since, last_scan) {
        (Some(pending), _) => pending + quiet,
        (None, Some(last)) => last + reconcile,
        // Nothing has been enumerated yet: do it immediately.
        (None, None) => return Duration::ZERO,
    };
    deadline.saturating_duration_since(now)
}

pub fn hook_failure_message(mode: FeedMode, code: u32) -> Option<String> {
    match mode {
        FeedMode::EventDriven => None,
        FeedMode::Polling => Some(format!(
            "window event hooks could not be installed (Windows error {code}); the list refreshes \
             every {}s instead",
            FALLBACK_POLL_INTERVAL.as_secs()
        )),
    }
}

/// Owns the event receiver plus the pump thread handle.
pub struct WindowMonitor {
    receiver: Receiver<FeedEvent>,
    mode: FeedMode,
    fallback_reason: Option<String>,
    thread_id: Option<u32>,
}

impl WindowMonitor {
    pub fn spawn() -> Self {
        let (event_sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::channel();
        // The worker gets a clone so that this thread still owns a sender while
        // `spawn` runs; it is dropped before returning, which is what lets the
        // UI observe a dead pump as a disconnected channel.
        let worker_sender = event_sender.clone();
        let spawned = thread::Builder::new()
            .name("rustnt-window-events".to_owned())
            .spawn(move || run_pump(worker_sender, ready_sender));

        if let Err(error) = spawned {
            let reason = format!("could not start the window event thread: {error}");
            let _ = event_sender.send(FeedEvent::Failure(reason.clone()));
            drop(event_sender);
            return Self {
                receiver,
                mode: FeedMode::Polling,
                fallback_reason: Some(reason),
                thread_id: None,
            };
        }
        drop(event_sender);

        match ready_receiver.recv_timeout(STARTUP_TIMEOUT) {
            Ok(ready) => Self {
                receiver,
                mode: ready.mode,
                fallback_reason: ready.fallback_reason,
                thread_id: Some(ready.thread_id),
            },
            Err(error) => {
                let reason = match error {
                    RecvTimeoutError::Timeout => {
                        "the window event thread did not report its state in time".to_owned()
                    }
                    RecvTimeoutError::Disconnected => {
                        "the window event thread stopped before reporting its state".to_owned()
                    }
                };
                Self {
                    receiver,
                    mode: FeedMode::Polling,
                    fallback_reason: Some(reason),
                    thread_id: None,
                }
            }
        }
    }

    /// Feed built from a channel the caller owns; used by unit tests.
    #[cfg(test)]
    pub(crate) fn from_receiver(receiver: Receiver<FeedEvent>) -> Self {
        Self {
            receiver,
            mode: FeedMode::EventDriven,
            fallback_reason: None,
            thread_id: None,
        }
    }

    pub fn mode(&self) -> FeedMode {
        self.mode
    }

    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    pub fn try_recv(&self) -> Result<FeedEvent, TryRecvError> {
        self.receiver.try_recv()
    }

    /// Ask the pump thread to finish. Safe to call more than once.
    pub fn stop(&self) {
        if let Some(thread_id) = self.thread_id {
            // SAFETY: the pump published this id only after creating its message
            // queue, so the thread id is valid for `PostThreadMessageW`.
            unsafe {
                PostThreadMessageW(thread_id, PUMP_STOP_MESSAGE, 0, 0);
            }
        }
    }
}

impl Drop for WindowMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MonitorReady {
    thread_id: u32,
    mode: FeedMode,
    fallback_reason: Option<String>,
}

/// Hooks owned by the pump thread; removed when they go out of scope.
struct EventHooks([HWINEVENTHOOK; WATCHED_EVENTS.len()]);

impl Drop for EventHooks {
    fn drop(&mut self) {
        for hook in self.0.iter().copied() {
            if !hook.is_null() {
                // SAFETY: every handle came from `SetWinEventHook` on this
                // thread and is removed exactly once.
                unsafe {
                    UnhookWinEvent(hook);
                }
            }
        }
    }
}

fn install_hooks() -> Result<EventHooks, u32> {
    let mut hooks: [HWINEVENTHOOK; WATCHED_EVENTS.len()] =
        [std::ptr::null_mut(); WATCHED_EVENTS.len()];
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

    for (index, event) in WATCHED_EVENTS.iter().copied().enumerate() {
        // SAFETY: the callback is a valid `WINEVENTPROC`, the module handle is
        // null (allowed), and `event`..=`event` is the single event to watch.
        let hook = unsafe {
            SetWinEventHook(
                event,
                event,
                std::ptr::null_mut(),
                Some(win_event_proc),
                0,
                0,
                flags,
            )
        };
        if hook.is_null() {
            // Read the code before anything else can overwrite it.
            // SAFETY: `GetLastError` has no preconditions.
            let code = unsafe { GetLastError() };
            // Dropping the partially filled guard removes the hooks installed so
            // far, so a partial installation can never leak a callback.
            drop(EventHooks(hooks));
            return Err(code);
        }
        hooks[index] = hook;
    }

    Ok(EventHooks(hooks))
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if is_relevant_event(hwnd as usize, id_object, id_child) {
        EVENT_PENDING.store(true, Ordering::Release);
    }
}

fn empty_message() -> MSG {
    MSG {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: POINT { x: 0, y: 0 },
    }
}

/// Create this thread's message queue.
///
/// Windows creates a queue on the first call that needs one, but the pump must
/// have one *before* it publishes its thread id, otherwise `PostThreadMessageW`
/// fails and the pump could never be stopped.
fn ensure_message_queue() {
    let mut message = empty_message();
    // SAFETY: `message` is a valid, writable `MSG`. PM_NOREMOVE leaves any
    // pending message in the queue.
    unsafe {
        PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
    }
}

/// Draining the queue is what makes Windows call the hook procedure.
///
/// Returns `false` once the stop message has been removed.
fn drain_messages(message: &mut MSG) -> bool {
    loop {
        // SAFETY: `message` is valid and writable; a null hwnd filter means
        // "messages for any window plus thread messages".
        let available = unsafe { PeekMessageW(message, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
        if available == 0 {
            return true;
        }
        if message.message == PUMP_STOP_MESSAGE {
            return false;
        }
        // SAFETY: the message was just filled in by `PeekMessageW` and is
        // dispatched unchanged to handlers owned by this thread.
        unsafe {
            TranslateMessage(message);
            DispatchMessageW(message);
        }
    }
}

/// Wait for input or for the next scan deadline.
///
/// `Err` carries the Windows error code when the wait itself failed, which is
/// the one condition that must be reported instead of silently stopping.
fn wait_for_input(delay: Duration) -> Result<(), u32> {
    let timeout = delay.as_millis().clamp(1, u128::from(u32::MAX)) as u32;
    // SAFETY: zero handles is a documented "wait for input or timeout" call.
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(
            0,
            std::ptr::null(),
            timeout,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        )
    };
    if result == WAIT_OBJECT_0 || result == WAIT_TIMEOUT {
        return Ok(());
    }
    if result == WAIT_FAILED {
        // SAFETY: `GetLastError` has no preconditions, read immediately after
        // the failing call.
        return Err(unsafe { GetLastError() });
    }
    Err(result)
}

fn scan(sender: &Sender<FeedEvent>) -> bool {
    let event = match enumerate_windows() {
        Ok(snapshot) => FeedEvent::Snapshot(snapshot),
        Err(error) => FeedEvent::Failure(error.to_string()),
    };
    sender.send(event).is_ok()
}

fn run_pump(sender: Sender<FeedEvent>, ready_sender: Sender<MonitorReady>) {
    ensure_message_queue();

    let hooks = install_hooks();
    let mode = if hooks.is_ok() {
        FeedMode::EventDriven
    } else {
        FeedMode::Polling
    };
    // SAFETY: `GetCurrentThreadId` has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let fallback_reason = hooks
        .as_ref()
        .err()
        .and_then(|code| hook_failure_message(mode, *code));

    if ready_sender
        .send(MonitorReady {
            thread_id,
            mode,
            fallback_reason: fallback_reason.clone(),
        })
        .is_err()
    {
        return;
    }
    if let Some(reason) = &fallback_reason {
        eprintln!("[rustnt-preview] {reason}");
    }

    let (quiet, reconcile) = mode.schedule();
    let mut message = empty_message();
    let mut pending_since: Option<Instant> = None;
    let mut last_scan: Option<Instant> = None;

    loop {
        if !drain_messages(&mut message) {
            break;
        }
        if EVENT_PENDING.swap(false, Ordering::AcqRel) && pending_since.is_none() {
            pending_since = Some(Instant::now());
        }

        let now = Instant::now();
        let delay = next_scan_delay(now, pending_since, last_scan, quiet, reconcile);
        if delay.is_zero() {
            pending_since = None;
            last_scan = Some(now);
            if !scan(&sender) {
                break;
            }
            continue;
        }

        if let Err(code) = wait_for_input(delay.min(PUMP_HEARTBEAT)) {
            let _ = sender.send(FeedEvent::Failure(format!(
                "the window event pump could not wait for input (Windows error {code})"
            )));
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_window_events_are_relevant() {
        assert!(is_relevant_event(0x1234, OBJID_WINDOW, CHILDID_SELF as i32));
    }

    #[test]
    fn events_without_a_window_or_for_child_objects_are_ignored() {
        // Notifications that carry no window handle.
        assert!(!is_relevant_event(0, OBJID_WINDOW, CHILDID_SELF as i32));
        // Accessibility events for child elements (OBJID_CLIENT and friends).
        assert!(!is_relevant_event(0x1234, -4, CHILDID_SELF as i32));
        // A specific child inside a window.
        assert!(!is_relevant_event(0x1234, OBJID_WINDOW, 7));
    }

    #[test]
    fn the_first_scan_happens_immediately() {
        let now = Instant::now();
        assert_eq!(
            next_scan_delay(now, None, None, COALESCE_INTERVAL, RECONCILE_INTERVAL),
            Duration::ZERO
        );
    }

    #[test]
    fn a_pending_event_waits_for_the_coalescing_window() {
        let now = Instant::now();
        let pending = now - Duration::from_millis(50);
        // 50ms into a 150ms window: 100ms left to wait.
        assert_eq!(
            next_scan_delay(
                now,
                Some(pending),
                Some(now),
                COALESCE_INTERVAL,
                RECONCILE_INTERVAL
            ),
            COALESCE_INTERVAL - Duration::from_millis(50)
        );
    }

    #[test]
    fn a_continuous_event_stream_cannot_starve_the_scan() {
        let now = Instant::now();
        // The first unserved event is far enough in the past that the window
        // has already elapsed, even though events kept arriving.
        let pending = now - COALESCE_INTERVAL - Duration::from_millis(10);
        assert_eq!(
            next_scan_delay(
                now,
                Some(pending),
                Some(now),
                COALESCE_INTERVAL,
                RECONCILE_INTERVAL
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn an_idle_feed_scans_on_the_reconcile_interval() {
        let now = Instant::now();
        let last = now - Duration::from_secs(5);
        assert_eq!(
            next_scan_delay(now, None, Some(last), COALESCE_INTERVAL, RECONCILE_INTERVAL),
            RECONCILE_INTERVAL - Duration::from_secs(5)
        );
    }

    #[test]
    fn a_passed_reconcile_deadline_scans_immediately() {
        let now = Instant::now();
        let last = now - RECONCILE_INTERVAL - Duration::from_secs(1);
        assert_eq!(
            next_scan_delay(now, None, Some(last), COALESCE_INTERVAL, RECONCILE_INTERVAL),
            Duration::ZERO
        );
    }

    #[test]
    fn polling_mode_uses_a_fixed_interval() {
        assert_eq!(
            FeedMode::Polling.schedule(),
            (FALLBACK_POLL_INTERVAL, FALLBACK_POLL_INTERVAL)
        );
        assert_eq!(
            FeedMode::EventDriven.schedule(),
            (COALESCE_INTERVAL, RECONCILE_INTERVAL)
        );
        // An idle event-driven feed waits for the reconcile interval, not for
        // the (much shorter) coalescing window.
        let now = Instant::now();
        assert_eq!(
            next_scan_delay(
                now,
                None,
                Some(now),
                FALLBACK_POLL_INTERVAL,
                FALLBACK_POLL_INTERVAL
            ),
            FALLBACK_POLL_INTERVAL
        );
    }

    #[test]
    fn the_stall_timeout_is_longer_than_the_reconcile_interval() {
        // Regression guard: an idle desktop produces no events at all, so a
        // stall threshold below the reconcile interval would report every idle
        // machine as broken.
        assert!(FeedMode::EventDriven.stall_timeout() > RECONCILE_INTERVAL);
        assert!(FeedMode::Polling.stall_timeout() > FALLBACK_POLL_INTERVAL);
        assert!(FeedMode::Polling.stall_timeout() < FeedMode::EventDriven.stall_timeout());
    }

    #[test]
    fn mode_labels_are_distinct_and_name_the_strategy() {
        assert_eq!(FeedMode::EventDriven.label(), "event-driven");
        assert_eq!(FeedMode::Polling.label(), "polling fallback");
    }

    #[test]
    fn only_the_polling_fallback_explains_itself() {
        assert!(hook_failure_message(FeedMode::EventDriven, 0).is_none());
        let message = hook_failure_message(FeedMode::Polling, 5).expect("fallback explains itself");
        assert!(message.contains("error 5"));
        assert!(message.contains("2s"));
    }

    #[test]
    fn the_coalescing_window_stays_in_the_documented_range() {
        assert!(COALESCE_INTERVAL >= Duration::from_millis(100));
        assert!(COALESCE_INTERVAL <= Duration::from_millis(200));
        assert!(RECONCILE_INTERVAL >= Duration::from_secs(15));
        assert!(RECONCILE_INTERVAL <= Duration::from_secs(30));
    }

    #[test]
    fn the_pump_heartbeat_bounds_a_missed_wake_up() {
        // A missed event must never cost more than one heartbeat, and the
        // heartbeat must be far below the reconcile interval it is protecting.
        assert!(PUMP_HEARTBEAT < RECONCILE_INTERVAL);
        assert!(PUMP_HEARTBEAT <= Duration::from_secs(1));
        assert!(PUMP_HEARTBEAT < FeedMode::Polling.stall_timeout());
    }

    /// Wait for the next event, ignoring the ones that do not matter yet.
    fn next_event(monitor: &WindowMonitor, deadline: Instant) -> Option<FeedEvent> {
        while Instant::now() < deadline {
            match monitor.try_recv() {
                Ok(event) => return Some(event),
                Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(20)),
                Err(TryRecvError::Disconnected) => return None,
            }
        }
        None
    }

    #[test]
    fn a_spawned_monitor_reports_a_mode_and_delivers_a_snapshot() {
        let _guard = lock_live_pump();
        let monitor = WindowMonitor::spawn();
        let event = next_event(&monitor, Instant::now() + Duration::from_secs(10));

        match monitor.mode() {
            FeedMode::EventDriven => assert_eq!(monitor.fallback_reason(), None),
            FeedMode::Polling => assert!(monitor.fallback_reason().is_some()),
        }

        let snapshot = match event {
            Some(FeedEvent::Snapshot(snapshot)) => snapshot,
            Some(FeedEvent::Failure(message)) => panic!("first feed event failed: {message}"),
            None => panic!("monitor delivered nothing within the deadline"),
        };

        if snapshot.windows.is_empty() && snapshot.skipped_windows == 0 {
            eprintln!(
                "skipping snapshot assertions: this session has no interactive desktop \
                 (windows=0, skipped=0)"
            );
            return;
        }
        assert!(
            !snapshot.windows.is_empty(),
            "enumeration returned no windows while reporting {} skipped windows",
            snapshot.skipped_windows
        );
    }

    #[test]
    fn stopping_the_monitor_disconnects_the_feed() {
        let _guard = lock_live_pump();
        let monitor = WindowMonitor::spawn();
        monitor.stop();

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match monitor.try_recv() {
                Ok(_) => continue,
                Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(20)),
                Err(TryRecvError::Disconnected) => return,
            }
        }
        panic!("the pump thread ignored the stop message");
    }

    /// Latency bound on the wake-up path.
    ///
    /// The event is synthesized by setting the same flag the hook procedure
    /// sets, because a test cannot change a real window without disturbing the
    /// desktop it runs on. This proves the pump drains its queue, honours the
    /// coalescing window, and produces a snapshot long before the reconcile
    /// interval; the OS delivery path itself is covered by the manual preview
    /// check.
    #[test]
    fn a_pending_event_produces_a_snapshot_before_the_reconcile_interval() {
        let _guard = lock_live_pump();
        let monitor = WindowMonitor::spawn();
        let first = next_event(&monitor, Instant::now() + Duration::from_secs(10));
        assert!(first.is_some(), "monitor delivered no first snapshot");

        // Let the pump settle, then ask for a refresh.
        EVENT_PENDING.store(true, Ordering::Release);
        let requested = Instant::now();
        let second = next_event(&monitor, requested + Duration::from_secs(5));
        let elapsed = requested.elapsed();

        assert!(second.is_some(), "signalled change produced no snapshot");
        assert!(
            elapsed < Duration::from_secs(2),
            "a signalled change took {elapsed:?}, which is the polling path, not the event path"
        );
    }
}
