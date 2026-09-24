# Task14 GUI Foundation Verification Report

**Date:** 2026-09-24
**Scope:** Task5 workspace verification and Windows GUI smoke verification

## Implementation commits

The Task14 implementation and review-fix commits verified by this report are:

- `81cf714e96d944069911321f139b14bbdc1f288d` - GUI configuration foundation
- `aa4a5520c1dd5e8d767a31771b4bc790ef09b969` - safe Windows config replacement
- `9b1bc53b79c99c80c4a0fd86d876d14987755803` - crash recovery marker
- `81b8e5198527a7bad6c889e1bb7cd548d26e7f2c` - recovery review fixes
- `948872f41cf569c121ea6d5394e0d08b43cb8e86` - GUI foundation screen
- `c86b39e192b5f04ea450c5e071616c188d7de7b3` - GUI screen review fixes
- `1117cf51546739d6db805231040e6ca1499d0309` - startup and recovery wiring
- `1cde00e6142bf1c4caad9d06ec587fca39eccee3` - normal-exit cleanup
- `f50de9a40d763dcf57956833f765b5467cd12743` - normal-exit cleanup tests

The Task5 changes are documentation only. No GUI source code, service code,
kernel code, driver code, or Shell integration was changed by this task.

## Toolchain and dependencies

```text
cargo 1.98.1 (797e8a9bc 2026-08-05)
rustc 1.98.1 (48a229cea 2026-09-01)
stable-x86_64-pc-windows-msvc (default)
```

Direct `rustnt-gui` dependency versions from `cargo tree -p rustnt-gui --depth 1`:

```text
eframe 0.36.2
serde 1.0.229
toml 1.1.6+spec-1.1.0
windows-sys 0.59.0
```

Cargo emitted the existing warning that semver metadata in the `toml`
requirement is ignored. This dependency declaration was not changed by Task5.

## Automated verification

Every required command exited with code `0`:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo test --workspace --all-targets` | 149 passed, 0 failed, 0 ignored |
| `cargo check --workspace --all-targets` | pass |
| `cargo build --workspace --all-targets` | pass; `rustnt-gui.exe` built |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass; no Clippy errors |
| `git diff --check` | pass; no whitespace errors |

The workspace test count is the sum reported by Cargo: 28 `rustnt-cli`, 97
`rustnt-core`, 23 `rustnt-gui`, 1 `rustnt-task09-target`, and zero-test
benchmark/service binaries.

The focused GUI command `cargo test -p rustnt-gui -- --nocapture` also passed
with 23 tests and 0 failures. Its expected poisoned-lock test output includes
the text `poison configuration lock`; the panic is caught by the test and does
not count as a test failure.

## Windows GUI smoke

The smoke run used this disposable application-data root:

```text
C:\Users\ceeses\AppData\Local\Temp\rustnt-task14-gui-smoke-20260924184839493
```

- `cargo run -p rustnt-gui` started the real `rustnt-gui.exe` with the isolated
  `APPDATA`. The process was responsive, its native window title was `RustNT`,
  and `RustNT\gui.running` was created.
- The dedicated process was terminated after verifying its executable path.
  The marker remained, as required for incomplete-run recovery.
- A second launch using the same isolated `APPDATA` succeeded with a
  responsive `RustNT` window and a new `gui.running` marker. The recovery
  default selection is covered by
  `recovery_uses_safe_defaults_and_normal_start_uses_loaded_settings`.
- Configuration serialization, reload, repeated replacement, theme labels,
  viewport defaults, normal cleanup ordering, and recovery cleanup behavior
  are covered by the 23 GUI unit tests listed above.

The current CUA Windows session reported no bindable native applications, so
these interactive checks are explicitly **skipped**: observing the first
screen contents, clicking `Toggle theme` and visually confirming egui visual
changes, clicking `Save config`, and closing through the window's normal UI
path. Consequently this smoke run does not claim a button click created
`gui.toml` or that a user-initiated normal close removed `gui.running`; those
paths have unit-test evidence only. The forced-termination marker-preservation
and relaunch checks were completed with the dedicated test process.

## Boundary

RustNT GUI remains a normal Windows user-mode client. Task14 does not alter
the Windows kernel, drivers, services, Shell behavior, or the existing service
authorization boundary.

## Final Review Fix: 2026-09-24

Addressed the three Important findings from the final review diff
`review-a219963..7f13321.diff`:

- `save_config` now creates each temporary TOML file in the configuration
  directory with a process ID, nanosecond timestamp, and atomic counter in its
  name. `create_new` prevents accidental reuse, and the existing Windows
  `ReplaceFileW` replacement path remains unchanged. A concurrent path
  uniqueness regression test covers the no-shared-`gui.toml.tmp` behavior.
- Runtime markers are now per-instance sidecars named from `gui.running` plus
  process ID, nanosecond timestamp, and atomic counter. `inspect` recognizes
  both the legacy `gui.running` sentinel and new sidecars. New instances never
  overwrite the legacy file, and `RuntimeMarker::remove` removes only its own
  sidecar, so concurrent instances do not delete each other's records and no
  single-instance lock is introduced. Tests cover two-instance lifecycle and
  legacy marker compatibility.
- Crash records are capped at `4096` bytes per record. All control characters
  in the panic summary are replaced with spaces before byte-safe truncation.
  The bounded payload and control-character behavior has a regression test.

The final focused command for this repair is:

```text
cargo test -p rustnt-gui -- --nocapture
```

It passed with 27 tests and 0 failures. The GUI focused Clippy check,
formatting check, and diff check also passed:

```text
cargo clippy -p rustnt-gui --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The workspace verification commands also passed with exit code `0`:
`cargo check --workspace --all-targets`,
`cargo build --workspace --all-targets`, and
`cargo clippy --workspace --all-targets -- -D warnings`.

The repair remains within the independent Windows user-mode GUI crate and
does not modify the README, roadmap, historical Task4 report, or Task14
design/plan.

## Final Review Gap Fix: 2026-09-24

This round closes the remaining marker, persistence-test, and crash-log gaps:

- `inspect` now reads the PID from legacy `gui.running` and sidecar markers
  on Windows. It preserves markers for the current or another running process,
  preserves markers when liveness cannot be verified, and removes only markers
  whose `OpenProcess`/`GetExitCodeProcess` result proves that the process has
  exited. A stale marker still makes the current launch enter recovery; after
  successful cleanup, the next launch can start normally. Legacy markers with
  an unrecognized PID remain compatible and visible to recovery. Tests cover
  current-process preservation, stale sidecar cleanup, stale legacy cleanup,
  and legacy compatibility.
- Configuration tests now inspect the real `gui.toml.*.tmp` pattern. A new
  barrier-synchronized test calls `save_config` concurrently from 16 threads,
  verifies every save succeeds, parses the final TOML, and confirms the
  configuration directory has no temporary files left.
- Crash records now include the current thread name and `ThreadId` alongside
  the panic summary. The complete record remains capped at 4096 bytes, with
  control characters replaced and truncation performed only at UTF-8 character
  boundaries. The thread metadata and multibyte truncation are covered by
  tests.

The explicit stale-PID test uses `u32::MAX`, which Windows rejects as an
invalid process identifier. Other `OpenProcess` failures are treated as
unknown and preserved, so an access-limited running instance is never removed
based on an uncertain result.

Final verification for this round:

```text
cargo test -p rustnt-gui -- --nocapture: 32 passed, 0 failed
cargo check --workspace --all-targets: pass
cargo build --workspace --all-targets: pass
cargo clippy --workspace --all-targets -- -D warnings: pass
cargo fmt --all -- --check: pass
git diff --check: pass
```
