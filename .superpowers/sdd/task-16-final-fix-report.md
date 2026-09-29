# Task16 Final Fix Report

## Scope

The final review identified that `RustNtApp::new` could fall back from an
unreadable `USERPROFILE` directory without retaining the original error in the
File Manager view. The fix is limited to `crates/rustnt-gui/src/app.rs`.

Initialization now loads the preferred directory first, records its
`FileSystemError`, loads the process directory as the fallback, and retains the
preferred error on the fallback browser state. The File Manager therefore keeps
showing the usable fallback contents while exposing the original preferred
directory failure through its existing error view.

The regression test injects deterministic directory loaders and does not create
or use a native window. It verifies the fallback path, loaded entries, state
error, and rendered `BrowserView` error.

## Verification

- `cargo test -p rustnt-gui app -- --nocapture`: 24 passed, 0 failed.
- `cargo test -p rustnt-gui -- --nocapture`: 79 passed, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.

Cargo emits the existing warning that the `toml` dependency version contains
ignored semver metadata. No files outside `app.rs` and this report were changed
for the fix; historical reports and Task14 files were left untouched.
