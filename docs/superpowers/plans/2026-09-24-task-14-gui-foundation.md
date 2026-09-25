# Task14 GUI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an independent `rustnt-gui.exe` that provides the RustNT GUI host window, persisted theme/window settings, and recovery after an incomplete previous run.

**Architecture:** Create a Windows-only `rustnt-gui` workspace crate. Keep filesystem configuration and runtime-marker state in focused modules, pass a shared configuration snapshot between `main.rs` and the `eframe::App`, and keep the first screen independent from the service, Named Pipe, CLI, and kernel boundaries.

**Tech Stack:** Rust 2021, `eframe 0.36.2` (`winit + egui` host), `serde 1.0.229`, `toml 1.1.6+spec-1.1.0`, standard library filesystem and synchronization primitives, Cargo workspace, Windows MSVC.

## Global Constraints

- The GUI is Windows-only, user-mode, and runs as a normal user process.
- Use `eframe` as the single native window/event-loop host; do not add a second GUI backend.
- Store settings in `%APPDATA%\\RustNT\\gui.toml`.
- Store the legacy-compatible runtime marker family under `%APPDATA%\\RustNT\\gui.running*` and
  the bounded crash log in `%APPDATA%\\RustNT\\gui-crash.log`.
- Missing or malformed configuration uses safe defaults and reports the fallback in the first screen.
- On Windows, inspect marker PID liveness without blocking. Remove markers proven stale; preserve
  markers when liveness is unknown. A marker creation timestamp must match the PID's current
  process creation timestamp, otherwise the marker is stale and is removed.
- Each instance creates and owns a unique sidecar marker and removes only that sidecar. Legacy
  `gui.running` remains readable but is never overwritten by a new instance. Multiple instances
  are allowed and inspection converges their marker state; no single-instance lock is used.
- A pre-existing runtime marker starts the GUI in safe recovery mode with default theme and window geometry.
- Normal event-loop return saves configuration and removes the runtime marker; panic leaves the marker in place.
- GUI code must not call the LocalSystem service, add a Named Pipe command, execute arbitrary commands, request elevation, or modify Windows Shell state.
- Use injected temporary directories in unit tests; tests must not write the real `%APPDATA%` directory.
- Every implementation task ends with focused tests and a separate Git commit.

---

## File Map

- Modify `C:\Users\ceeses\Desktop\RustNT\Cargo.toml`: add the three pinned GUI dependencies to workspace dependencies and add `crates/rustnt-gui` to workspace members.
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\Cargo.toml`: define the independent `rustnt-gui` package and `rustnt-gui` binary.
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\main.rs`: resolve paths, load state, install the panic hook, start `eframe`, save configuration, remove the marker, and map errors to exit code `1`.
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\config.rs`: path model, public GUI configuration, TOML conversion, validation, atomic save, and configuration tests.
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\recovery.rs`: runtime-marker lifecycle, crash-log append helper, panic-hook installation, and recovery tests.
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\app.rs`: `eframe::App`, first-screen controls, theme application, shared configuration updates, and user-visible status messages.
- Existing `C:\Users\ceeses\Desktop\RustNT\README.md` records the GUI launch and user-mode/configuration boundary; it is protected for this closeout.
- Existing `C:\Users\ceeses\Desktop\RustNT\docs\roadmap.md` records project status; it is protected for this closeout.
- Create `C:\Users\ceeses\Desktop\RustNT\.superpowers\sdd\task-14-report.md`: record actual test/build/smoke outputs, marker liveness evidence, and known platform boundaries.
- Existing `C:\Users\ceeses\Desktop\RustNT\.superpowers\sdd\progress.md` is protected for this closeout.

## Interfaces

The configuration module exposes these interfaces to `main.rs` and `app.rs`:

```rust
pub const DEFAULT_WINDOW_WIDTH: f32 = 960.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuiConfig {
    pub theme: ThemeMode,
    pub window_width: f32,
    pub window_height: f32,
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: std::path::PathBuf,
    pub config_file: std::path::PathBuf,
    pub marker_file: std::path::PathBuf,
    pub crash_log: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoad {
    pub notice: Option<String>,
}

#[derive(Debug)]
pub struct ConfigError {
    pub operation: String,
    pub message: String,
}

pub fn config_paths_from_appdata() -> Result<ConfigPaths, ConfigError>;
pub fn config_paths_from_root(root: std::path::PathBuf) -> ConfigPaths;
pub fn load_config(paths: &ConfigPaths) -> Result<(GuiConfig, ConfigLoad), ConfigError>;
pub fn save_config(paths: &ConfigPaths, config: &GuiConfig) -> Result<(), ConfigError>;
```

The recovery module exposes these interfaces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryState {
    pub previous_run_incomplete: bool,
}

pub struct RuntimeMarker {
    path: std::path::PathBuf,
}

#[derive(Debug)]
pub struct RecoveryError {
    pub operation: String,
    pub message: String,
}

pub fn inspect(paths: &ConfigPaths) -> Result<RecoveryState, RecoveryError>;
pub fn create_marker(paths: &ConfigPaths) -> Result<RuntimeMarker, RecoveryError>;
pub fn install_panic_hook(crash_log: std::path::PathBuf);
impl RuntimeMarker {
    pub fn remove(self) -> Result<(), RecoveryError>;
}
```

`RuntimeMarker` deliberately has no `Drop` cleanup. A panic must leave the marker for the next launch; `main.rs` removes it only after the event loop returns.

Implement `std::fmt::Display`, `std::error::Error`, and `From<std::io::Error>` for `RecoveryError`. The conversion must preserve whether the failed operation was inspection, marker creation, crash logging, or marker removal.

The app consumes a shared configuration value so `main.rs` can persist the final theme and viewport state after `eframe::run_native` returns:

```rust
pub type SharedConfig = std::sync::Arc<std::sync::Mutex<GuiConfig>>;

pub struct RustNtApp {
    pub(crate) config: SharedConfig,
    pub(crate) paths: ConfigPaths,
    pub(crate) recovery: RecoveryState,
    pub(crate) config_notice: Option<String>,
    pub(crate) save_notice: Option<String>,
}

#[derive(Debug)]
pub enum GuiError {
    Config(config::ConfigError),
    Recovery(recovery::RecoveryError),
    ConfigLock,
    Eframe(eframe::Error),
}

impl RustNtApp {
    pub fn new(
        config: SharedConfig,
        paths: ConfigPaths,
        recovery: RecoveryState,
        config_notice: Option<String>,
    ) -> Self;
}
```

### Task 1: Workspace and Configuration Module

**Files:**

- Modify `C:\Users\ceeses\Desktop\RustNT\Cargo.toml`
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\Cargo.toml`
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\main.rs`
- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\config.rs`

**Consumes:** The approved Task14 specification and the existing workspace package metadata.

**Produces:** A compilable GUI crate skeleton and a tested, path-injectable configuration module.

- [ ] **Step 1: Add the workspace member, dependencies, package metadata, and test stub**

Add the following workspace entries to `Cargo.toml`:

```toml
    "crates/rustnt-gui",
```

```toml
eframe = "0.36.2"
serde = { version = "1.0.229", features = ["derive"] }
toml = "1.1.6+spec-1.1.0"
```

Create `crates/rustnt-gui/Cargo.toml`:

```toml
[package]
name = "rustnt-gui"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "rustnt-gui"
path = "src/main.rs"

[dependencies]
eframe.workspace = true
serde.workspace = true
toml.workspace = true
```

Create the Windows-only crate skeleton and add a failing configuration test in `config.rs`:

```rust
#![cfg(windows)]

mod config;

fn main() {}
```

At this stage keep `main.rs` as `#![cfg(windows)]`, declare only `mod config;`, and leave
`app.rs` and `recovery.rs` out of the module tree until their tasks introduce them. This keeps
the first failing test focused on configuration types rather than missing later-task files.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_safe_dark_window_settings() {
        let config = GuiConfig::default();
        assert_eq!(config.theme, ThemeMode::Dark);
        assert_eq!(config.window_width, DEFAULT_WINDOW_WIDTH);
        assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
        assert_eq!(config.window_x, None);
        assert_eq!(config.window_y, None);
    }
}
```

Run:

```text
cargo test -p rustnt-gui defaults_use_safe_dark_window_settings
```

Expected result: compilation fails because `GuiConfig`, `ThemeMode`, and the default constants do not exist yet. Do not continue until the failure is caused by the missing configuration implementation rather than workspace metadata.

- [ ] **Step 2: Implement configuration types and TOML conversion**

Implement the interfaces above in `config.rs`. Keep the in-memory enum separate from the serialized representation so the file contains lowercase strings:

```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FileConfig {
    theme: String,
    window_width: f32,
    window_height: f32,
    #[serde(default)]
    window_x: Option<f32>,
    #[serde(default)]
    window_y: Option<f32>,
}
```

Use `ThemeMode::from_file_value`, `GuiConfig::validate`, and `FileConfig::from_config` helpers with these exact rules:

- `"dark"` maps to `ThemeMode::Dark` and `"light"` maps to `ThemeMode::Light`.
- Any other theme string maps to dark and produces a notice containing `theme` and `dark`.
- Width and height must be finite and at least `320.0` and `240.0`; invalid values return the default dimension and a notice containing `window size`.
- A position is retained only when it is finite; otherwise it becomes `None` and produces a notice containing `window position`.
- Missing files return `(GuiConfig::default(), ConfigLoad { notice: None })`.
- TOML parse errors return default configuration plus a notice containing `configuration` and `default`.

The path constructor must create no directories and must produce these filenames under the supplied root:

```rust
ConfigPaths {
    root: root.clone(),
    config_file: root.join("gui.toml"),
    marker_file: root.join("gui.running"),
    crash_log: root.join("gui-crash.log"),
}
```

`config_paths_from_appdata` must read `APPDATA` using `std::env::var_os`, reject a missing or empty value with `ConfigError`, and append `RustNT`.

- [ ] **Step 3: Add failing file round-trip and validation tests**

Add tests using a unique directory under `std::env::temp_dir()` and remove it with a test-local cleanup guard. Cover the actual behavior:

```rust
#[test]
fn missing_config_returns_defaults_without_notice() {
    let root = test_root("missing");
    let paths = config_paths_from_root(root.clone());
    let (config, load) = load_config(&paths).unwrap();
    assert_eq!(config, GuiConfig::default());
    assert_eq!(load.notice, None);
    remove_test_root(root);
}

#[test]
fn valid_toml_round_trips_theme_dimensions_and_position() {
    let root = test_root("round-trip");
    std::fs::create_dir_all(&root).unwrap();
    let paths = config_paths_from_root(root.clone());
    let expected = GuiConfig {
        theme: ThemeMode::Light,
        window_width: 1280.0,
        window_height: 720.0,
        window_x: Some(40.0),
        window_y: Some(60.0),
    };
    save_config(&paths, &expected).unwrap();
    let (actual, load) = load_config(&paths).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(load.notice, None);
    remove_test_root(root);
}

#[test]
fn invalid_theme_and_geometry_fall_back_without_panicking() {
    let root = test_root("invalid");
    std::fs::create_dir_all(&root).unwrap();
    let paths = config_paths_from_root(root.clone());
    std::fs::write(
        &paths.config_file,
        "theme = \"neon\"\nwindow_width = -1.0\nwindow_height = inf\nwindow_x = nan\n",
    )
    .unwrap();
    let (config, load) = load_config(&paths).unwrap();
    assert_eq!(config.theme, ThemeMode::Dark);
    assert_eq!(config.window_width, DEFAULT_WINDOW_WIDTH);
    assert_eq!(config.window_height, DEFAULT_WINDOW_HEIGHT);
    assert!(load.notice.is_some());
    remove_test_root(root);
}
```

Run:

```text
cargo test -p rustnt-gui config::tests -- --nocapture
```

Expected result before implementation: the new tests fail because file loading, saving, and validation are not implemented. This test also establishes that an invalid TOML number is treated as a safe fallback instead of being written back.

- [ ] **Step 4: Implement load, atomic save, and path errors**

Implement `load_config` with `std::fs::read_to_string`, `toml::from_str`, and the fallback rules. Implement `save_config` as:

```rust
let temporary = paths.config_file.with_extension("toml.tmp");
let encoded = toml::to_string_pretty(&FileConfig::from_config(config))?;
std::fs::write(&temporary, encoded)?;
std::fs::rename(&temporary, &paths.config_file)?;
```

Create the root directory in `save_config` with `std::fs::create_dir_all`. On every error, return a `ConfigError` containing the operation and source error. Remove the temporary file when serialization or rename fails if it exists. Do not expose raw TOML types outside `config.rs`.

Implement `std::fmt::Display`, `std::error::Error`, and the required `From<std::io::Error>` and `From<toml::ser::Error>` conversions for `ConfigError`; preserve the operation name when wrapping an error so `main.rs` can print a useful message.

Run:

```text
cargo test -p rustnt-gui config::tests -- --nocapture
cargo fmt --all -- --check
```

Expected result: all configuration tests pass and rustfmt reports no changes.

- [ ] **Step 5: Commit the configuration slice**

Run:

```text
git add Cargo.toml crates/rustnt-gui/Cargo.toml crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/config.rs
git commit -m "feat: add Task14 GUI configuration foundation"
```

Expected result: one commit containing only workspace setup and configuration code.

### Task 2: Runtime Marker and Crash Recovery

**Files:**

- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\recovery.rs`
- Modify `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\main.rs`

**Consumes:** `ConfigPaths` and `ConfigError` from Task 1.

**Produces:** A marker lifecycle that reports incomplete state, performs Windows PID
liveness and creation-time checks, cleans proven stale markers, preserves unknown states,
and removes only the owning sidecar on the normal return path.

- [ ] **Step 1: Write failing marker and crash-log tests**

Add tests with injected temporary paths:

```rust
#[test]
fn inspect_reports_normal_start_without_marker() {
    let root = test_root("normal");
    let paths = config_paths_from_root(root.clone());
    assert_eq!(inspect(&paths).unwrap(), RecoveryState { previous_run_incomplete: false });
    remove_test_root(root);
}

#[test]
fn marker_is_detected_and_removed_only_explicitly() {
    let root = test_root("marker");
    let paths = config_paths_from_root(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        &paths.marker_file,
        format!("pid={}\nstarted_at=old\n", std::process::id()),
    )
    .unwrap();
    assert_eq!(inspect(&paths).unwrap().previous_run_incomplete, true);
    let marker = create_marker(&paths).unwrap();
    assert!(marker.path.exists());
    marker.remove().unwrap();
    assert!(!marker.path.exists());
    assert!(paths.marker_file.exists());
    remove_test_root(root);
}

#[test]
fn failed_crash_log_append_is_reported_without_panicking() {
    let root = test_root("crash-log");
    std::fs::create_dir_all(&root).unwrap();
    let result = append_crash_record(&root, "panic summary");
    assert!(result.is_err());
    remove_test_root(root);
}
```

On Windows, add a real cross-process test that starts a short-lived
`ping.exe 127.0.0.1 -n 6` child, writes its PID to a marker, verifies the marker
is retained while the child is running, then waits for the child and verifies
the next inspection removes the stale marker. The test must own the child with
an RAII cleanup guard that terminates and waits on early return or assertion
unwind. If `ping.exe` cannot be started, print an explicit skip message and
return.

For the last test, pass a directory as the log target so `OpenOptions::open` fails predictably. The test asserts an error result; it must not trigger a panic.

Run:

```text
cargo test -p rustnt-gui recovery::tests -- --nocapture
```

Expected result: compilation fails because the recovery interfaces do not exist yet.

- [ ] **Step 2: Implement marker inspection and creation**

Implement `inspect` over the legacy `gui.running` file and unique sidecar markers. On Windows,
inspect the marker PID with a non-blocking process wait; when a creation timestamp is present,
compare it with the PID's current process creation timestamp to detect PID reuse. Remove only
markers proven stale, preserve markers with unknown liveness, and keep legacy marker contents
readable for compatibility. Implement `create_marker` by creating an owned sidecar containing
diagnostic text:

```text
pid=<std::process::id()>
started_at=<seconds since UNIX_EPOCH>
```

Use exclusive creation for the sidecar so concurrent instances cannot overwrite each other's
markers. A marker creation error must return `RecoveryError` and prevent GUI startup.
`RuntimeMarker::remove` uses `std::fs::remove_file` for its owned sidecar; a missing marker is
treated as success so cleanup remains idempotent.

- [ ] **Step 3: Implement crash logging and panic-hook installation**

Implement:

```rust
fn append_crash_record(path: &std::path::Path, summary: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}: {}", unix_timestamp(), summary.replace(['\r', '\n'], " "))?;
    Ok(())
}
```

`install_panic_hook` must capture the crash-log path, call `std::panic::take_hook()` once, and install a hook that writes a one-line summary while ignoring write errors before delegating to the previous hook. The hook must not call GUI code, lock the shared configuration, or remove the runtime marker.

- [ ] **Step 4: Run recovery tests and commit**

Run:

```text
cargo test -p rustnt-gui recovery::tests -- --nocapture
cargo test -p rustnt-gui config::tests -- --nocapture
cargo clippy -p rustnt-gui --all-targets -- -D warnings
```

Expected result: all focused tests pass and Clippy reports no warnings.

Commit:

```text
git add crates/rustnt-gui/src/recovery.rs crates/rustnt-gui/src/main.rs
git commit -m "feat: add GUI crash recovery marker"
```

### Task 3: Minimal `eframe` Application

**Files:**

- Create `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\app.rs`
- Modify `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\main.rs`

**Consumes:** `GuiConfig`, `ThemeMode`, `ConfigPaths`, `ConfigLoad`, and `RecoveryState`.

**Produces:** A first screen with stable title, status, recovery notice, theme toggle, and save action.

- [ ] **Step 1: Add pure theme/status tests before UI code**

Add small tests for the app-facing pure mappings:

```rust
#[test]
fn theme_label_is_stable() {
    assert_eq!(theme_label(ThemeMode::Dark), "dark");
    assert_eq!(theme_label(ThemeMode::Light), "light");
}

#[test]
fn recovery_label_distinguishes_previous_incomplete_run() {
    assert_eq!(recovery_label(RecoveryState { previous_run_incomplete: false }), "normal startup");
    assert_eq!(recovery_label(RecoveryState { previous_run_incomplete: true }), "safe recovery mode");
}
```

Run:

```text
cargo test -p rustnt-gui app::tests -- --nocapture
```

Expected result: failure naming the missing pure helper functions.

- [ ] **Step 2: Implement `RustNtApp` and shared configuration updates**

Implement `RustNtApp` with the interface from this plan. In `update`, lock the shared configuration only for short reads/writes and recover a poisoned mutex with `into_inner()` so a UI lock failure does not panic.

The first screen must render these exact user-facing values or equivalent stable labels:

```rust
ui.heading("RustNT");
ui.label("RustNT GUI foundation status");
ui.label(format!("Theme: {}", theme_label(config.theme)));
ui.label(format!("State: {}", recovery_label(self.recovery)));
```

Add a theme button that toggles `ThemeMode::Dark` and `ThemeMode::Light`, calls `ctx.set_visuals`, and updates the shared configuration. Add a `Save config` button that calls `save_config(&self.paths, &config)` and stores a short success or error message in `save_notice`.

When recovery is active, show a visible `Previous run did not exit normally. Safe defaults are active.` message. When configuration loading produced a notice, show it above the controls. Do not add process, service, shell, or elevation controls.

- [ ] **Step 3: Implement viewport initialization and snapshot updates**

Create a helper returning `egui::ViewportBuilder`:

```rust
pub(crate) fn viewport_for(config: &GuiConfig, recovery: RecoveryState) -> egui::ViewportBuilder {
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
```

Use the current eframe/egui viewport input to copy finite inner width, height, x, and y back into the shared configuration during `update`. When the API reports no position, preserve `None`. Recovery mode must use `GuiConfig::default()` for both theme and viewport input while keeping the recovery message visible.

- [ ] **Step 4: Run focused app tests and commit**

Run:

```text
cargo fmt --all -- --check
cargo test -p rustnt-gui app::tests -- --nocapture
cargo check -p rustnt-gui
cargo clippy -p rustnt-gui --all-targets -- -D warnings
```

Expected result: focused tests, check, and Clippy pass.

Commit:

```text
git add crates/rustnt-gui/src/app.rs crates/rustnt-gui/src/main.rs
git commit -m "feat: add RustNT GUI foundation screen"
```

### Task 4: Startup Wiring and User-Facing Documentation

**Files:**

- Modify `C:\Users\ceeses\Desktop\RustNT\crates\rustnt-gui\src\main.rs`
- Modify `C:\Users\ceeses\Desktop\RustNT\README.md`

**Consumes:** All configuration, recovery, and app interfaces from Tasks 1-3.

**Produces:** A runnable GUI binary with correct normal/abnormal exit behavior and documented usage.

- [ ] **Step 1: Write the startup integration test contract**

Before wiring the native window, add a testable `startup_config` helper with this contract:

```rust
fn startup_config(
    loaded: GuiConfig,
    recovery: RecoveryState,
) -> GuiConfig {
    if recovery.previous_run_incomplete {
        GuiConfig::default()
    } else {
        loaded
    }
}
```

Test both branches:

```rust
#[test]
fn recovery_uses_safe_defaults_and_normal_start_uses_loaded_settings() {
    let loaded = GuiConfig { theme: ThemeMode::Light, ..GuiConfig::default() };
    assert_eq!(startup_config(loaded.clone(), RecoveryState { previous_run_incomplete: false }), loaded);
    assert_eq!(startup_config(loaded, RecoveryState { previous_run_incomplete: true }), GuiConfig::default());
}
```

Run:

```text
cargo test -p rustnt-gui startup_config -- --nocapture
```

Expected result: the test fails until the helper exists.

- [ ] **Step 2: Implement `run_gui` and the native eframe startup**

Implement the following ordered flow in `main.rs`:

```rust
fn run_gui() -> Result<(), GuiError> {
    let paths = config_paths_from_appdata()?;
    std::fs::create_dir_all(&paths.root)?;
    let recovery = inspect(&paths)?;
    let (loaded, load) = load_config(&paths)?;
    let config = startup_config(loaded, recovery);
    let marker = create_marker(&paths)?;
    install_panic_hook(paths.crash_log.clone());
    let shared = std::sync::Arc::new(std::sync::Mutex::new(config.clone()));
    let options = eframe::NativeOptions {
        viewport: viewport_for(&config, recovery),
        ..Default::default()
    };
    let shared_for_app = shared.clone();
    let paths_for_app = paths.clone();
    let result = eframe::run_native(
        "RustNT",
        options,
        Box::new(move |_creation_context| {
            Ok(Box::new(RustNtApp::new(
                shared_for_app,
                paths_for_app,
                recovery,
                load.notice,
            )))
        }),
    );
    match result {
        Ok(()) => {
            let final_config = shared.lock().map_err(|_| GuiError::ConfigLock)?.clone();
            let save_result = save_config(&paths, &final_config);
            let remove_result = marker.remove();
            remove_result?;
            save_result?;
            Ok(())
        }
        Err(error) => Err(GuiError::Eframe(error)),
    }
}
```

The implementation must preserve the marker if `run_native` returns an error or panics. It must still attempt configuration save and marker removal after a normal event-loop return, and it must return exit code `1` for any startup, GUI, save, or cleanup error. `main` prints the error to stderr and exits with `std::process::exit(1)`; successful return exits with code `0`.

The exact `eframe 0.36.2` closure error/result type must be used after compiler confirmation. Do not downgrade the dependency to make the closure compile; adjust the closure to the installed API.

Implement `Display`, `Error`, and `From` conversions for `GuiError` so `main` can print the failing operation. In the normal-return branch, call `marker.remove()` before propagating a configuration-save error; this guarantees a save failure does not create a false recovery prompt. In the `Err(error)` branch returned by `run_native`, keep the marker and return the GUI error without attempting normal cleanup.

- [ ] **Step 3: Document GUI launch and boundaries**

Add a README section containing:

```text
cargo run -p rustnt-gui
```

Document that the GUI is a normal Windows user-mode process, uses `eframe`/`winit`/`egui`, stores settings under `%APPDATA%\\RustNT`, and enters safe recovery mode when `gui.running` remains after an incomplete run. State that Task14 does not replace Explorer, the taskbar, the Windows kernel, or the existing service authorization boundary.

- [ ] **Step 4: Run startup tests and compile the binary**

Run:

```text
cargo test -p rustnt-gui -- --nocapture
cargo check -p rustnt-gui
cargo build -p rustnt-gui
cargo clippy -p rustnt-gui --all-targets -- -D warnings
```

Expected result: all GUI tests pass, `target\\debug\\rustnt-gui.exe` is produced, and Clippy reports no warnings.

- [ ] **Step 5: Commit startup and documentation**

```text
git add crates/rustnt-gui/src/main.rs crates/rustnt-gui/src/app.rs README.md
git commit -m "feat: wire RustNT GUI startup and recovery"
```

### Task 5: Manual Smoke Verification, Reports, and Workspace Gate

**Files:**

- Create `C:\Users\ceeses\Desktop\RustNT\.superpowers\sdd\task-14-report.md`

**Consumes:** The completed GUI crate and all prior commits.

**Produces:** Actual verification evidence and a durable project status update.

- [ ] **Step 1: Run the complete automated verification gate**

Run these commands from the repository root and record actual output summaries:

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo check --workspace --all-targets
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
git status --short --branch
```

Expected result: every command exits with code `0`, `git diff --check` emits no output, and the final status lists only intended report/documentation changes before their commit.

- [ ] **Step 2: Run the GUI smoke matrix**

Use a dedicated test profile or a temporary `%APPDATA%` root only when the environment can safely redirect `APPDATA`; otherwise record the real path without deleting unrelated user files. Verify:

1. `cargo run -p rustnt-gui` opens a window titled `RustNT`.
2. The first screen displays GUI foundation status and normal startup state.
3. Toggling the theme changes the visible egui visuals.
4. Clicking `Save config` creates or updates `%APPDATA%\\RustNT\\gui.toml`.
5. Closing normally removes `%APPDATA%\\RustNT\\gui.running`.
6. Creating `gui.running` before launch causes safe recovery mode, default theme/size, and the recovery message.
7. Terminating a running GUI without its normal close path leaves `gui.running`; the next launch shows recovery mode.
8. A normal close after recovery removes the marker and saves a valid TOML file.

Record the exact limitation if forced termination cannot be automated in the current desktop session; do not claim that scenario was verified without evidence.

- [ ] **Step 3: Write the Task14 report and verify protected project status files**

Write `task-14-report.md` with:

- implementation commit IDs;
- dependency/toolchain versions;
- automated command results and test counts;
- manual smoke results for startup, theme, config, marker, and recovery;
- any skipped smoke item with the reason;
- explicit statement that the GUI remains a Windows user-mode client and does not alter kernel, driver, service, or Shell behavior.

The final review record updates only `.superpowers/sdd/task-14-report.md`.
README, roadmap, progress ledger, and the historical Task4 report are protected
files for this closeout and must have no diff.

- [ ] **Step 4: Run the final documentation and status checks**

Run:

```text
git diff --check
git status --short --branch
```

Expected result: no whitespace errors, and only the recovery test, Task14
spec/plan, and Task14 report are changed before the final commit. Review the
plan and report text manually for unresolved placeholders before committing.

- [ ] **Step 5: Commit the verified Task14 record**

```text
git add crates/rustnt-gui/src/recovery.rs docs/superpowers/specs/2026-09-24-task-14-gui-foundation-design.md docs/superpowers/plans/2026-09-24-task-14-gui-foundation.md .superpowers/sdd/task-14-report.md
git commit -m "test: close Task14 recovery liveness review"
```

## Self-Review Checklist

- Spec section 1 is covered by Tasks 1-4: independent crate, host window, theme, config, and recovery.
- Spec section 2 is enforced by the global constraints and the Task 4 README boundary statement.
- Spec section 3 is covered by the File Map and exact module ownership in Tasks 1-4.
- Spec section 4 is covered by pinned workspace dependencies and the Windows-only crate metadata.
- Spec section 5 is covered by configuration interfaces, validation tests, and atomic-save implementation in Task 1.
- Spec section 6 is covered by marker tests in Task 2 and ordered startup/cleanup in Task 4.
- Spec section 7 is covered by first-screen controls and status messages in Task 3.
- Spec section 8 is covered by focused tests, workspace commands, and the smoke matrix in Task 5.
- Spec section 9 is covered by the final report and final status checks.
- Spec section 10 is covered by the README boundary statement and the absence of service/core changes in the File Map.
- The plan contains no unresolved implementation placeholder.
- The types used by `main.rs`, `app.rs`, `config.rs`, and `recovery.rs` match the Interfaces section.
