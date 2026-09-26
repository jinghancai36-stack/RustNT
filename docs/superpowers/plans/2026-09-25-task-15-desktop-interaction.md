# Task15 Desktop Interaction Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

Goal: Add a single-window RustNT Shell with internal page navigation, focused keyboard shortcuts, and a bounded in-memory notification center while preserving Task14 recovery and configuration behavior.

Architecture: Keep RustNtApp as the eframe host. Put route metadata and shortcut mapping in navigation.rs, notification lifecycle in notifications.rs, and integrate both through app.rs. The app renders a fixed three-page route table: Overview, System Monitor placeholder, and Windows & Sessions placeholder. All state remains process-local; no service, pipe, Windows API, global hotkey, external process, or other-window control is added.

Tech Stack: Rust 2021, eframe/egui 0.36.2, existing serde/toml/windows-sys dependencies, Rust unit tests, Cargo workspace checks.

## Global Constraints

- The GUI remains a Windows user-mode process.
- Task15 does not start external programs or control other Windows windows.
- Task15 does not register global hotkeys or modify Windows Shell, taskbar, desktop, startup entries, or registry.
- Task15 does not add LocalSystem service, Named Pipe, driver, kernel, or new system-protocol integration.
- System Monitor and Windows & Sessions render an explicit unconnected-data-source placeholder and never fabricate system data.
- Notifications are process-local, non-persistent, non-system, and bounded to 32 entries.
- Existing Task14 configuration, theme, viewport, recovery marker, panic, and cleanup behavior must remain intact.
- Every implementation task follows test-first order: write a failing focused test, run it, implement the minimum behavior, rerun the focused test, then commit.

## File Map

- Create: crates/rustnt-gui/src/navigation.rs for fixed page identifiers, route labels, shortcut mapping, and switcher state.
- Create: crates/rustnt-gui/src/notifications.rs for the bounded notification model and lifecycle operations.
- Modify: crates/rustnt-gui/src/main.rs to expose the two modules without changing startup or recovery sequencing.
- Modify: crates/rustnt-gui/src/app.rs to add Shell state, input handling, navigation, notification UI, and placeholder pages while retaining Overview behavior.
- Modify: .superpowers/sdd/progress.md after the feature is verified.
- Create: .superpowers/sdd/task-15-report.md for commands, results, scope boundaries, and GUI smoke limitations.

## Task 1: Deterministic page navigation

Files: create crates/rustnt-gui/src/navigation.rs; modify crates/rustnt-gui/src/main.rs.

Interfaces produced: PageId; NavigationState; pages() returning the fixed page slice; page_title(PageId); page_label(PageId); page_for_shortcut(ctrl: bool, key: egui::Key) -> Option<PageId>. PageId has exactly Overview, SystemMonitor, and WindowsSessions in that order. NavigationState::default selects Overview and sets switcher_open to false.

- [ ] Write failing tests for fixed page order, stable labels and titles, Ctrl+1/2/3 mapping, rejection of non-Ctrl and unknown keys, and default switcher state.
- [ ] Run cargo test -p rustnt-gui navigation -- --nocapture. Expected result: compilation failure because navigation.rs and its route functions do not exist.
- [ ] Implement the enum, state, static page slice, exhaustive title and label matches, and the Ctrl+1/2/3 mapping. Add pub mod navigation to main.rs without changing main startup order.
- [ ] Run cargo test -p rustnt-gui navigation -- --nocapture and cargo fmt --all -- --check. Expected result: focused tests pass and formatting exits 0.
- [ ] Commit with: git add crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/navigation.rs; git commit -m "feat: add Task15 page navigation model".

## Task 2: Bounded in-memory notifications

Files: create crates/rustnt-gui/src/notifications.rs; modify crates/rustnt-gui/src/main.rs.

Interfaces produced: NotificationKind with Info, Success, Warning, Error; Notification with kind, message, and read; NotificationCenter::new; push(kind, message); notifications(); unread_count(); mark_all_read(); mark_read(index); and clear(). NotificationCenter retains at most 32 entries, removes oldest entries first, removes carriage returns and newlines, and truncates messages to 512 Unicode scalar values.

- [ ] Write failing tests for default unread state, unread count, idempotent read and clear operations, FIFO limit behavior, newline cleanup, and 512-character truncation.
- [ ] Run cargo test -p rustnt-gui notifications -- --nocapture. Expected result: compilation failure because notifications.rs and NotificationCenter do not exist.
- [ ] Implement NotificationCenter with VecDeque, newest entries at the back, front removal while length exceeds 32, safe invalid-index handling, and idempotent read and clear operations. Add pub mod notifications to main.rs.
- [ ] Run cargo test -p rustnt-gui notifications -- --nocapture and cargo fmt --all -- --check. Expected result: focused tests pass and formatting exits 0.
- [ ] Commit with: git add crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/notifications.rs; git commit -m "feat: add bounded GUI notifications".

## Task 3: Single-window Shell integration

Files: modify crates/rustnt-gui/src/app.rs.

Interfaces consumed: PageId, NavigationState, pages, page_title, page_label, page_for_shortcut, NotificationKind, Notification, and NotificationCenter. Existing RustNtApp::new, viewport_for, SharedConfig, RecoveryState, configuration fields, and cleanup-facing behavior remain compatible.

- [ ] Add failing app tests for initial Overview selection, page changes that do not mutate GuiConfig or RecoveryState, explicit unconnected-data-source copy for both placeholder pages, and one notification per save result. Tests must remain independent of a native window.
- [ ] Run cargo test -p rustnt-gui app -- --nocapture. Expected result: the new tests fail to compile because Shell state, placeholder helpers, and notification integration are absent; pre-existing Task14 tests must still compile.
- [ ] Add navigation and notifications fields to RustNtApp and initialize them in RustNtApp::new. Handle Ctrl+1/2/3 before drawing, toggle the switcher with Ctrl+K, and close it with Esc. Do not register global hotkeys. Do not process a shortcut in a text editor if the egui input API exposes text focus; otherwise do not add text-edit controls in Task15.
- [ ] Render egui TopBottomPanel::top for title, current page, GUI status, and notification entry; SidePanel::left for the fixed page list; CentralPanel for the selected page. Keep Task14 theme and viewport snapshot calls. Render Overview with the existing widgets and render the other two pages with the exact unconnected-data-source placeholder.
- [ ] Add an in-window notification panel. Opening it marks notifications read, provides clear behavior, and displays bounded entries. Selecting a switcher item updates the route and closes the switcher. Actions create notifications once, never from per-frame code.
- [ ] Add each initial configuration notice once during construction and add one success or error notification per Save Config click. Keep existing visible config_notice and save_notice behavior, normal cleanup, marker removal, panic handling, and configuration file semantics.
- [ ] Run cargo test -p rustnt-gui -- --nocapture and cargo fmt --all -- --check. Expected result: all GUI tests, including Task14 recovery and configuration tests, pass.
- [ ] Commit with: git add crates/rustnt-gui/src/app.rs; git commit -m "feat: add Task15 single-window GUI shell".

## Task 4: Documentation and complete verification

Files: modify .superpowers/sdd/progress.md; create .superpowers/sdd/task-15-report.md.

- [ ] Record that navigation and notifications are process-local, the two capability pages are placeholders, no external program or other-window control was added, and no service, kernel, driver, Shell, or dependency boundary changed. Mark Task15 complete only after verification passes.
- [ ] Run cargo fmt --all -- --check; cargo test -p rustnt-gui -- --nocapture; cargo check --workspace --all-targets; cargo build --workspace --all-targets; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace --all-targets; and git diff --check. Expected result: every command exits 0; the workspace test run reports the 162 baseline tests plus new Task15 tests with zero failures. Record the existing ignored-semver-metadata warning for toml if it remains.
- [ ] Review git diff --stat HEAD~4..HEAD and git diff --check. Confirm changes are limited to the two GUI modules, app.rs/main.rs, progress/report documents, and no unrelated system code.
- [ ] Commit with: git add .superpowers/sdd/progress.md .superpowers/sdd/task-15-report.md; git commit -m "docs: record Task15 GUI verification".

## Completion Checklist

- [ ] Navigation tests pass and route order is stable.
- [ ] Notification tests pass and the 32-entry bound is enforced.
- [ ] Shell renders Overview plus two explicit placeholder pages.
- [ ] Ctrl+1/2/3, Ctrl+K, and Esc remain local to RustNT.
- [ ] Task14 configuration and recovery tests pass unchanged.
- [ ] Workspace checks, build, Clippy, formatting, and diff checks pass.
- [ ] Progress and verification report are committed.
