# Task16 File Manager v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only File Manager page to the RustNT single-window GUI for browsing, filtering, sorting, and refreshing one Windows directory at a time.

**Architecture:** Reuse the existing `rustnt-core::filesystem::list_directory` API through a new workspace dependency from `rustnt-gui`. Keep path history, filtering, sorting, selection, and error retention in a focused `file_browser.rs` state model with an injectable directory loader for deterministic tests. Keep `app.rs` responsible for egui layout and route the existing third page to File Manager without changing service, authorization, recovery, or filesystem Win32 implementation boundaries.

**Tech Stack:** Rust 2021, eframe/egui 0.36.2, existing `rustnt-core` filesystem types, Windows user-mode APIs, Rust unit tests, Cargo workspace checks.

## Global Constraints

- Task16 is read-only: no file or directory creation, deletion, copy, move, rename, or content write.
- Task16 does not open files, launch external programs, register global hotkeys, modify Windows Shell, add services, Named Pipes, drivers, kernel interfaces, or new privilege capabilities.
- The GUI reads only direct children of the current directory and does not recurse or compute directory totals.
- Failed directory loads preserve the last valid entries and expose an in-window error state.
- The existing Task14 configuration, viewport, recovery marker, panic, and cleanup behavior remains unchanged.
- The existing Task15 local shortcut and notification behavior remains local to RustNT.
- Every implementation task follows test-first order: write a failing focused test, run it, implement the minimum behavior, rerun the focused test, then commit.

## File Map

- Modify: `crates/rustnt-gui/Cargo.toml` to add the existing `rustnt-core` workspace path dependency.
- Create: `crates/rustnt-gui/src/file_browser.rs` for browser state, sorting/filtering, path history, loader injection, and tests.
- Modify: `crates/rustnt-gui/src/main.rs` to expose `file_browser` without changing startup order.
- Modify: `crates/rustnt-gui/src/navigation.rs` to rename the third GUI route to File Manager while keeping Ctrl+3 local.
- Modify: `crates/rustnt-gui/src/app.rs` to own browser state, load directories, render the page, route controls, and emit one notification per failed user load action.
- Modify: `.superpowers/sdd/progress.md` after implementation and verification.
- Create: `.superpowers/sdd/task-16-report.md` with commands, results, scope boundaries, and GUI smoke limitations.

## Task 1: Browser state and deterministic transformations

Files: modify `crates/rustnt-gui/Cargo.toml` and `crates/rustnt-gui/src/main.rs`; create `crates/rustnt-gui/src/file_browser.rs`.

Interfaces produced: `FileBrowserState`, `SortField`, `SortDirection`, `BrowserView`, path-history transitions, injectable directory loading, and stable entry filtering/sorting helpers.

- [ ] Write failing tests for default state, successful load, failed load preserving the previous directory, entering a child directory, parent-root boundary, back/forward stack behavior, and refresh behavior.
- [ ] Write failing tests for case-insensitive name filtering, directory-first default ordering, each supported sort field in both directions, stable tie-breaking, empty-directory state, and reparse-point metadata retention.
- [ ] Run `cargo test -p rustnt-gui file_browser -- --nocapture`; expect compilation failure because the module, dependency, and browser interfaces do not exist.
- [ ] Add the `rustnt-core` path dependency and expose `pub mod file_browser` without changing GUI startup or recovery sequencing.
- [ ] Implement the state model with a current valid path, entries, back/forward stacks, filter, sort state, selected path, and last error. Use an injected loader for tests and the core `list_directory` function in production.
- [ ] Implement Windows-safe parent navigation that stops at a drive root and UNC share root, and ensure failed transitions do not mutate the current valid entries or history.
- [ ] Implement deterministic filtering and sorting over direct child metadata only. Use case-insensitive name matching, directories before files for the default name sort, and path/name tie-breakers for stable results.
- [ ] Run `cargo test -p rustnt-gui file_browser -- --nocapture` and `cargo fmt --all -- --check`; all focused tests must pass.
- [ ] Commit with: `git add crates/rustnt-gui/Cargo.toml crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/file_browser.rs && git commit -m "feat: add Task16 file browser state"`.

## Task 2: Route and GUI File Manager page

Files: modify `crates/rustnt-gui/src/navigation.rs` and `crates/rustnt-gui/src/app.rs`.

Interfaces consumed: `FileBrowserState`, `BrowserView`, `SortField`, `SortDirection`, and existing `rustnt-core::filesystem` metadata types.

- [ ] Add failing navigation tests for the third route label/title becoming File Manager while preserving fixed route order and Ctrl+3 mapping.
- [ ] Add failing app tests for initial File Manager state, successful injected directory presentation, path navigation controls, filter/sort state changes, empty-directory copy, and error state behavior.
- [ ] Run `cargo test -p rustnt-gui navigation app -- --nocapture`; expect the new assertions to fail before integration.
- [ ] Replace the GUI-only Windows Sessions route metadata with `PageId::FileManager`; keep Task13 window/session core and CLI code untouched.
- [ ] Add browser state to `RustNtApp`, initialize it once in `new`, and load the chosen initial user directory without panicking. Prefer `USERPROFILE`; fall back to the current process directory when unavailable.
- [ ] Render a File Manager central panel using existing egui visuals: back, forward, parent, refresh, path input, filter input, sort field, sort direction, status/error text, and a stable direct-child table.
- [ ] Render directory/file/reparse-point labels from actual metadata. Single click selects directories; double click enters directories. Files have no open action.
- [ ] Preserve the last valid directory contents on failed path submissions or refreshes. Add at most one error notification per failed explicit user action, never from per-frame rendering.
- [ ] Ensure existing Overview controls, notification panel, switcher, Ctrl+1/2/3, Ctrl+K, Esc, theme, save behavior, and viewport snapshots remain unchanged.
- [ ] Run `cargo test -p rustnt-gui -- --nocapture` and `cargo fmt --all -- --check`; GUI tests must pass.
- [ ] Commit with: `git add crates/rustnt-gui/src/navigation.rs crates/rustnt-gui/src/app.rs && git commit -m "feat: add Task16 GUI file manager"`.

## Task 3: Integration regression and documentation

Files: modify `.superpowers/sdd/progress.md`; create `.superpowers/sdd/task-16-report.md`.

- [ ] Write the Task16 progress entry only after the full verification matrix passes. Record that the page is read-only, process-local, reuses existing core filesystem APIs, and adds no privilege or Windows Shell boundary.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo test -p rustnt-gui -- --nocapture`.
- [ ] Run `cargo check --workspace --all-targets`.
- [ ] Run `cargo build --workspace --all-targets`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace --all-targets -- --test-threads=1` to avoid known Windows parallel test-process instability.
- [ ] Run `git diff --check` and review the branch diff for changes limited to the planned GUI dependency/module, navigation/app integration, and Task16 documents.
- [ ] Record all command results, test counts, existing `toml` semver warning, and any native-window smoke limitation in `.superpowers/sdd/task-16-report.md`.
- [ ] Commit with: `git add .superpowers/sdd/progress.md .superpowers/sdd/task-16-report.md && git commit -m "docs: record Task16 file manager verification"`.

## Completion Checklist

- [ ] File Manager is the third GUI route and Ctrl+3 remains local.
- [ ] Directories can be entered, revisited, refreshed, and traversed with back/forward/parent controls.
- [ ] Name filtering and all four sort modes are deterministic and tested.
- [ ] Empty directories and access/path errors are visible without discarding the last valid listing.
- [ ] No file write, external launch, Shell integration, service, driver, kernel, or privilege behavior was added.
- [ ] Task14 and Task15 tests remain green.
- [ ] Workspace format, check, build, Clippy, serial tests, and diff checks pass.
- [ ] Progress and verification report are committed.
