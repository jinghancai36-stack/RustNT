# Task 1 Report

Status: DONE

Commit: `b937a48772a83b0d3a6da9f7dbf3b0e372b575b2`

Implemented the `rustnt-shell-switch` policy crate and added it to the workspace. The crate currently defines the Task 1 fixed domain types and transaction state transition policy. It contains no Windows Registry or ACL adapter.

Verification run in the isolated worktree:

- `cargo test -p rustnt-shell-switch`: 3 unit tests passed; 0 failed; doc tests passed. Cargo emitted the repository's existing TOML semver metadata warning.
- `cargo fmt --all -- --check`: passed after formatting.
- `git diff --check`: passed.

TDD evidence: the added `failed_state_requires_recovery_validation_before_restore_request` test was run against the permissive transition table and failed on the assertion as expected; after removing that transition, all 3 tests passed. The initial state-sequence tests were present in the first shared implementation snapshot, so their red run was not observed by the controller.

Execution note: the implementer subagent failed twice with backend HTTP 502. The controller completed the narrowly scoped Task 1 implementation in the isolated worktree; this did not change the implementation boundary.

## Independent Review

Review scope: Task 1 brief and diff `bebc53dc918262b8c6988d679ea88b20e93dfb82..3fba0a164ed0f83260285a9626bc5dd75c28c7e8`.

### Verdicts

- Spec compliance: **PASS** after the review fix.
- Quality: **PASS** after the review fix; Task 4 coordinator owns recovery precondition validation.

### Finding

- **Resolved [P1]:** The initial review found that validated failed transactions could not enter recovery. The user chose to allow the state edge in the context-free policy and require the Task 4 coordinator to validate the registry value and backup before taking it. The implementation now includes `Failed -> RestoreRequested`, and the focused test covers that edge.

### Constraint check

The diff adds only a policy crate and workspace membership. It introduces no registry I/O, Winlogon writes, GUI or service behavior, raw-value transaction logging, or test access to real HKLM. The fixed-domain transaction and audit structures contain metadata and fixed error categories, not raw registry bytes. No Task17A behavior is changed in this diff.

## Review Fix

Applied the requested policy correction: `Failed -> RestoreRequested` is now an allowed state edge after the Task 4 coordinator has validated recovery prerequisites. The focused test now asserts the permitted edge, and the full transition table includes it.

Fix commit: `3fba0a164ed0f83260285a9626bc5dd75c28c7e8`

Fresh verification: `cargo fmt --all`, `cargo test -p rustnt-shell-switch` (3 passed, 0 failed), `cargo fmt --all -- --check`, and `git diff --check` all passed.

Re-review: Spec PASS and Quality PASS. The reviewer confirmed the state edge at `crates/rustnt-shell-switch/src/lib.rs:49` and its test at line 237. Task 4 coordinator validation remains an explicit implementation requirement.
