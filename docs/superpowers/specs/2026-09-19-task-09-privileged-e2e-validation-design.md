# Task 09 Privileged End-to-End Validation Design

**Date:** 2026-09-19

## Goal

Close the Task08 validation gap by providing a disposable process target,
repeatable automated preflight checks, and a documented Administrator/UAC test
matrix for the LocalSystem service bridge.

Task09 validates the existing security boundary. It does not add new
privileged capabilities or broaden the process-control policy.

## Scope

Task09 adds:

- A dependency-free `rustnt-task09-target` executable that prints its PID and
  remains alive until the test operator terminates it.
- Automated build and unit-test coverage for the disposable target.
- A repeatable elevated validation procedure covering service lifecycle,
  inspection, authorization rejection, caller-owned termination, PID reuse
  behavior, and cleanup.
- A verification report containing environment details, observed outputs,
  pass/fail status, and residual risk.
- Targeted fixes only when the validation or review identifies a real defect.

Task09 does not add:

- New service protocol commands.
- Batch, name-based, wildcard, or remote process control.
- Arbitrary command, script, PowerShell, WMI, or shell execution.
- A general audit framework; that remains Task10 scope.
- Driver, kernel, GUI, Shell, or filesystem functionality.

## Architecture

The disposable target is a separate workspace binary:

```text
rustnt-task09-target.exe
    |
    +-- prints RUSTNT_TASK09_TARGET_PID=<pid>
    +-- waits without changing system state
    +-- exits when terminated by the operator or service
```

The target has no Windows API dependency and no production-service dependency.
This keeps the test process ordinary, caller-owned, and easy to remove.

The validation path remains:

```text
elevated terminal
    |
    +--> rustnt service install/start/status/identity
    +--> rustnt process inspect --pid <target>
    +--> rustnt process terminate --pid <target>
    |
    +--> rustnt service stop/uninstall
```

The CLI and service implementation remain the system under test. The helper
only supplies a known disposable target.

## Validation Matrix

### Preflight

- Record Windows version/build, Rust toolchain, account, elevation, and
  Administrators membership.
- Confirm the service is not installed or is stopped before starting.
- Build the workspace and the disposable target.
- Confirm the target prints a positive PID.

### Positive path

- Install the service.
- Start the service and wait for `RUNNING`.
- Query service identity over the Pipe.
- Inspect the disposable target and record PID, creation time, owner SID,
  image name, and bounded metadata.
- Terminate the same caller-owned target.
- Confirm the target stops and no automatic retry is issued.

### Rejection path

- Inspect PID 4 and confirm the protected-target response.
- Attempt to terminate PID 4 and confirm rejection.
- Attempt to terminate the RustNT service PID and confirm rejection.
- Attempt to terminate a system-owned target and confirm rejection.
- Attempt a stale creation-time request and confirm `PID_REUSED`.
- Run a non-elevated client, when available, and confirm
  `CALLER_NOT_ELEVATED` or the documented service-level rejection.

### Cleanup

- Stop the service.
- Uninstall the service.
- Confirm the service reports `NOT_INSTALLED`.
- Confirm the disposable target is gone.
- Confirm no RustNT service process or test target remains.

## Acceptance Criteria

Task09 is complete when:

1. `rustnt-task09-target` builds on the workspace target and prints a PID.
2. Workspace tests, build, Clippy, formatting, and diff checks pass.
3. The automated preflight has no residual service or helper process.
4. The elevated matrix is either passed in full or explicitly recorded as
   environment-gated with the exact skipped cases and reason.
5. Any code fix discovered during validation has a regression test and a
   separate commit.

An environment-gated report is not presented as proof of successful
administrator end-to-end termination. It records the limitation for the next
interactive elevated run.

## Safety Rules

- The target process must be the disposable helper, never a production process.
- No test may terminate PID 0, PID 4, the service, or a system-owned process as
  a positive-path action.
- Do not leave the service installed after validation.
- Do not add bypass flags or skip authorization to make a test pass.
- Do not record command lines, tokens, secrets, or unrelated process metadata.
