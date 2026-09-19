# Task 10 Capability Authorization and Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add typed capability authorization, bounded in-memory audit events, request IDs, and per-caller termination rate limiting without changing the existing wire protocol.

**Architecture:** Put capability, authorization, audit, and rate-limit types in a focused `rustnt-core::authorization` module. Add a private service runtime containing a request sequencer, bounded audit sink, and limiter; pass it through the existing one-request-per-connection server loop. Keep Task08 process policy checks in place as defense in depth.

**Tech Stack:** Rust 2021, standard-library `HashMap`, `VecDeque`, `Mutex`, `Instant`, existing `rustnt-core::service`, Windows Named Pipe server.

## Global Constraints

- Protocol version remains `1`; command codes remain `0..=4`.
- The fixed capability names remain `ping,identity,capabilities,process_inspect,process_terminate`.
- No new protocol command, request field, remote audit endpoint, or persistent log dependency.
- `PROCESS_TERMINATE` remains limited by Task08 elevation, Administrator, owner SID, creation time, and protected-target checks.
- Audit events contain fixed typed values only; never command lines, environments, token contents, arbitrary payload bytes, or executable arguments.
- The termination limiter allows at most 4 requests per caller SID in a 10-second window.
- All new behavior must have focused tests before implementation.

---

### Task 1: Add Typed Capability and Audit Primitives

**Files:**
- Create: `crates/rustnt-core/src/authorization.rs`
- Modify: `crates/rustnt-core/src/lib.rs`

**Interfaces:**
- `Capability::{Ping, Identity, Capabilities, ProcessInspect, ProcessTerminate}`.
- `Capability::name() -> &'static str`.
- `Capability::all() -> &'static [Capability; 5]`.
- `Capability::is_destructive() -> bool`.
- `RequestContext { request_id, capability, client_sid, caller_elevated, caller_administrator }`.
- `authorize(&RequestContext) -> Result<(), AuthorizationRejection>`.
- `AuditEvent`, `AuditOutcome`, `AuditReason`, and `AuditSink`.
- `MemoryAuditSink::new(capacity)`, `record(event)`, and `snapshot()`.
- `RequestRateLimiter::new(max_requests, window)`, `allow(subject)`, and testable `allow_at(subject, now)`.

- [ ] **Step 1: Write failing capability and authorization tests**

Add tests that reference the planned types and assert:

```rust
#[test]
fn fixed_capabilities_have_stable_names() {
    assert_eq!(
        Capability::all()
            .iter()
            .map(|capability| capability.name())
            .collect::<Vec<_>>(),
        vec![
            "ping",
            "identity",
            "capabilities",
            "process_inspect",
            "process_terminate",
        ]
    );
}

#[test]
fn termination_authorization_requires_elevated_local_admin() {
    let context = RequestContext {
        request_id: 1,
        capability: Capability::ProcessTerminate,
        client_sid: Some("S-1-5-21-user".to_string()),
        caller_elevated: false,
        caller_administrator: true,
    };
    assert_eq!(
        authorize(&context),
        Err(AuthorizationRejection::CallerNotElevated)
    );
}
```

Run:

```text
cargo test -p rustnt-core fixed_capabilities_have_stable_names
cargo test -p rustnt-core termination_authorization_requires_elevated_local_admin
```

Expected: FAIL because the authorization module and functions do not exist.

- [ ] **Step 2: Implement fixed capabilities and authorization**

Implement the enum, fixed name table, `RequestContext`, and authorization order:

```text
missing client SID
    -> caller not elevated
    -> caller not local Administrator
    -> allowed
```

Read-only capabilities return `Ok(())` without a client SID. Destructive
authorization returns fixed rejection values and does not inspect target
metadata.

- [ ] **Step 3: Add failing audit retention and limiter tests**

Add tests for bounded retention and deterministic rate limiting:

```rust
#[test]
fn memory_audit_sink_keeps_only_the_newest_events() {
    let sink = MemoryAuditSink::new(2);
    sink.record(AuditEvent::success(1, Capability::Ping, None, None));
    sink.record(AuditEvent::success(2, Capability::Ping, None, None));
    sink.record(AuditEvent::success(3, Capability::Ping, None, None));
    assert_eq!(
        sink.snapshot()
            .iter()
            .map(|event| event.request_id)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn rate_limiter_rejects_burst_and_allows_after_window() {
    let start = Instant::now();
    let mut limiter = RequestRateLimiter::new(2, Duration::from_secs(10));
    assert!(limiter.allow_at("S-1-5-21-user", start));
    assert!(limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(1)));
    assert!(!limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(2)));
    assert!(limiter.allow_at("S-1-5-21-user", start + Duration::from_secs(11)));
}
```

Run:

```text
cargo test -p rustnt-core memory_audit_sink_keeps_only_the_newest_events
cargo test -p rustnt-core rate_limiter_rejects_burst_and_allows_after_window
```

Expected: FAIL because the sink, event constructors, and limiter do not exist.

- [ ] **Step 4: Implement bounded audit sink and rate limiter**

Use a `Mutex<VecDeque<AuditEvent>>` for the bounded sink and a
`HashMap<String, VecDeque<Instant>>` for the limiter. Drop the oldest audit
event when capacity is reached. Purge timestamps older than the window before
checking a new request.

- [ ] **Step 5: Run focused primitive tests**

```text
cargo test -p rustnt-core authorization -- --nocapture
cargo fmt --all -- --check
cargo clippy -p rustnt-core --all-targets -- -D warnings
```

Expected: all authorization-module tests pass with no Clippy warnings.

- [ ] **Step 6: Commit**

```text
git add crates/rustnt-core/src/authorization.rs crates/rustnt-core/src/lib.rs
git commit -m "feat: add typed capability and audit primitives"
```

### Task 2: Integrate Capabilities and Audit Into Service Dispatch

**Files:**
- Modify: `crates/rustnt-core/src/service.rs`

**Interfaces:**
- `Command::capability() -> Capability`.
- Private `ServiceRuntime` with request sequencing, audit sink, and limiter.
- `dispatch_request(pipe, runtime, request) -> Response`.
- `run_pipe_server` owns one runtime for the service process.

- [ ] **Step 1: Write failing service integration tests**

Add Windows tests for known-command dispatch:

```rust
#[test]
fn dispatch_assigns_request_ids_and_audits_successes() {
    let mut runtime = ServiceRuntime::new();
    let first = dispatch_request_for_test_with_runtime(
        &mut runtime,
        Request {
            command: Command::Ping,
            payload: Vec::new(),
        },
    );
    let second = dispatch_request_for_test_with_runtime(
        &mut runtime,
        Request {
            command: Command::Ping,
            payload: Vec::new(),
        },
    );
    assert_eq!(first.status, STATUS_SUCCESS);
    assert_eq!(second.status, STATUS_SUCCESS);
    let events = runtime.audit.snapshot();
    assert_eq!(events[0].request_id, 1);
    assert_eq!(events[1].request_id, 2);
    assert!(events.iter().all(|event| {
        event.capability == Capability::Ping
            && event.outcome == AuditOutcome::Succeeded
    }));
}

#[test]
fn malformed_known_payload_is_audited_without_opening_a_target() {
    let mut runtime = ServiceRuntime::new();
    let response = dispatch_request_for_test_with_runtime(
        &mut runtime,
        Request {
            command: Command::ProcessInspect,
            payload: vec![1, 2, 3],
        },
    );
    assert_eq!(response.status, STATUS_ERROR);
    let events = runtime.audit.snapshot();
    assert_eq!(events[0].reason, Some(AuditReason::MalformedPayload));
}
```

Run:

```text
cargo test -p rustnt-core dispatch_assigns_request_ids_and_audits_successes
cargo test -p rustnt-core malformed_known_payload_is_audited_without_opening_a_target
```

Expected: FAIL because dispatch has no runtime, capability mapping, or audit
emission.

- [ ] **Step 2: Add the command-to-capability mapping**

Implement `Command::capability()` using the five fixed mappings and replace
the duplicated capability response literals with `Capability::all()` names.
Keep all existing command codes and response bytes unchanged.

- [ ] **Step 3: Add the service runtime**

Implement:

```rust
struct ServiceRuntime {
    next_request_id: u64,
    audit: MemoryAuditSink,
    limiter: RequestRateLimiter,
}
```

Initialize the sink with capacity `256` and the limiter with `4` requests per
`10` seconds. Assign request IDs starting at `1` and never emit `0`.

- [ ] **Step 4: Thread the runtime through the server**

Create one runtime before the `run_pipe_server` accept loop and pass it to
`dispatch_request`. Keep the runtime alive for the entire service process so
request IDs and rate-limit history are not reset per connection.

- [ ] **Step 5: Emit one typed audit event per known request**

Record:

- successful read-only responses as `SUCCEEDED`;
- malformed known payloads as `FAILED/MALFORMED_PAYLOAD`;
- existing fixed process rejection statuses as `REJECTED`;
- authorization failures as `REJECTED`;
- rate-limit failures as `REJECTED/RATE_LIMITED`;
- unclassified Win32/service failures as `FAILED/WINDOWS_FAILURE`.

Use the validated target PID and caller SID only when already available.
Never copy arbitrary response text or request bytes into an audit event.

- [ ] **Step 6: Apply authorization and rate limiting before termination**

For `PROCESS_TERMINATE`:

1. Decode the exact 12-byte payload.
2. Query the Pipe client security context.
3. Build `RequestContext`.
4. Call `authorize`.
5. Apply the limiter keyed by caller SID.
6. Call the existing `terminate_process`.

Keep the existing checks inside `terminate_process` and preserve all existing
fixed status responses.

- [ ] **Step 7: Run service tests and commit**

```text
cargo test -p rustnt-core service -- --nocapture
cargo fmt --all -- --check
cargo clippy -p rustnt-core --all-targets -- -D warnings
git add crates/rustnt-core/src/service.rs
git commit -m "feat: integrate capability audit into service dispatch"
```

### Task 3: Documentation and Final Verification

**Files:**
- Modify: `README.md`
- Modify: `docs/architecture.md`
- Modify: `docs/learning-notes.md`
- Modify: `.superpowers/sdd/progress.md`
- Create: `.superpowers/sdd/task-10-report.md`

- [ ] **Step 1: Document the internal boundary**

Document that capability names are fixed protocol metadata, authorization is
performed inside the service, audit records are bounded and in-memory, and
termination requests are rate-limited by authenticated caller SID.

- [ ] **Step 2: Run the full verification**

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
git status --short --branch
```

Expected: all commands pass, with no uncommitted files after the report
commit.

- [ ] **Step 3: Write the verification report**

Record the exact test count, build/lint/format results, unchanged protocol
codes, audit and limiter tests, and any remaining limitation. Do not claim
that in-memory events are persistent audit storage.

- [ ] **Step 4: Commit documentation and report**

```text
git add README.md docs/architecture.md docs/learning-notes.md .superpowers/sdd/progress.md
git add -f .superpowers/sdd/task-10-report.md
git commit -m "docs: record Task10 capability audit"
```
