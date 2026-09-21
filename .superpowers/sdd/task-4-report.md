# Task 4 Report: CLI Integration, Documentation, and Verification

## Result

Implemented Task4 on branch `codex/task12a-filesystem-readonly` from baseline
commit `eeb5884`.

## Changes

- Added the `rustnt fs` route and fixed usage lines for `stat`, `list`, `space`,
  `permissions`, and `search`.
- Added strict parsing for exactly one `--path`, plus exactly one non-empty
  `--name` for search; duplicate, unknown, missing, and extra arguments are
  rejected.
- Added command execution through the existing `rustnt_core::filesystem`
  read-only APIs with exit-code mapping.
- Added fixed human-readable renderers for metadata, directory entries, disk
  space, raw permissions, and search summaries.
- Added local timestamp formatting through the existing `windows-sys`
  dependency and `Win32_System_Time`.
- Added CLI tests for all five commands, invalid arguments, missing values,
  space field order, permission ACE rows, and search summaries.
- Updated README and roadmap with Task12A behavior and security boundaries.
- Added `.superpowers/sdd/task-12a-report.md` with actual verification results.

## TDD Evidence

The new CLI tests were added before the production parser and renderers.

Focused RED command:

```text
cargo test -p rustnt-cli tests::parses_all_filesystem_commands -- --nocapture
```

Before implementation, compilation failed because
`parse_filesystem_command`, `FileSystemCommand`, and the filesystem renderers
did not exist.

## Verification

- `cargo test -p rustnt-cli -- --nocapture`: 24 passed.
- `cargo test --workspace -- --nocapture`: 24 CLI tests, 85 core tests, and
  the remaining workspace tests passed.
- `cargo build --workspace`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- Normal-path smoke commands for all five `fs` subcommands passed on a
  temporary fixture. Missing `--name` returned 2 and a missing file returned 1.

## Scope

No service call, capability, protocol command, elevation path, file mutation,
content read, or reparse-point traversal was added.
