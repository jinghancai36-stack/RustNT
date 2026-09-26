# Task 1: Deterministic Page Navigation

## Implementation

Implemented the Task15 deterministic page navigation model in `crates/rustnt-gui/src/navigation.rs` and declared the module in `crates/rustnt-gui/src/main.rs` without changing startup order.

The model provides:

- `PageId` with exactly `Overview`, `SystemMonitor`, and `WindowsSessions`, in that order.
- `NavigationState`, whose default is `Overview` with `switcher_open` set to `false`.
- A fixed `pages()` slice.
- Exhaustive stable `page_title` and `page_label` mappings.
- `page_for_shortcut`, mapping Ctrl+1, Ctrl+2, and Ctrl+3 to the three pages and rejecting non-Ctrl and unknown keys.

## Files

- `crates/rustnt-gui/src/main.rs`: added `pub mod navigation;`.
- `crates/rustnt-gui/src/navigation.rs`: added the route model and five focused unit tests.
- `.superpowers/sdd/task-1-report.md`: this implementation report.

The generated `.superpowers/sdd/task-1-brief.md` was left uncommitted.

## RED

Command:

```text
cargo test -p rustnt-gui navigation -- --nocapture
```

Result: expected compilation failure before implementation. The compiler reported unresolved imports for `page_for_shortcut`, `page_label`, `page_title`, `pages`, `NavigationState`, and `PageId` in `navigation.rs`. The initial test-only `egui::Key` import was also corrected to `eframe::egui::Key`; the subsequent RED run failed only on the absent navigation API.

## GREEN

Command:

```text
cargo test -p rustnt-gui navigation -- --nocapture
```

Result:

```text
running 5 tests
...
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 36 filtered out
```

## Tests

Fresh verification after formatting:

- `cargo fmt --all -- --check`: passed, exit code 0.
- `cargo test -p rustnt-gui navigation -- --nocapture`: passed, 5 passed and 0 failed.
- `cargo test -p rustnt-gui -- --nocapture`: passed, 41 passed and 0 failed.

The full suite prints the panic message from the existing lock-poisoning test while catching that panic as part of the test; the suite still exits successfully. Cargo also reports the pre-existing warning that the `toml` dependency version requirement contains ignored semver metadata.

## Self-review

- Scope is limited to the requested module declaration, new navigation module, and this report.
- `app.rs` and unrelated files are unchanged.
- `PageId` order is explicit and covered by a test.
- Labels, titles, default state, shortcut mappings, non-Ctrl rejection, and unknown-key rejection are covered by focused tests.
- Shortcut handling is exhaustive for the supported keys and returns `None` for all other keys.
- Formatting and the complete existing GUI test suite pass.

## Concerns

- The full test suite retains the existing expected panic output from the lock-poisoning test, which can look alarming in logs despite the successful result.
- Cargo emits the existing `toml` semver metadata warning; it is outside Task 1 scope and was not changed.
