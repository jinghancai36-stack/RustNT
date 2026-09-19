# Learning Notes

## Process

A Windows process is a running program together with its virtual address space,
handles, security context, and one or more threads.

## PID

A process identifier, or PID, is a numeric identifier assigned by Windows to a
process. It is an identifier, not an object that grants access by itself.

## HANDLE

A HANDLE is an opaque value used by Windows to refer to a kernel-managed object.
It is not a normal memory pointer. A PID identifies a process; a process HANDLE
represents an access reference to that process and is subject to permissions.

## Threads

A process contains threads that execute its code:

```text
Process
`-- Thread
```

The process snapshot provides a thread count, while the thread snapshot lets
RustNT count threads by their owning process ID. For Task 01, RustNT uses the
thread count already supplied by `PROCESSENTRY32W`, so it does not need to
create a separate thread snapshot.

## Process path

`QueryFullProcessImageNameW` asks Windows for the executable path associated
with an opened process HANDLE. Access can fail for protected processes, so the
path is represented as `Option<String>` and is not required for a successful
system-wide enumeration.

## CPU sampling

`GetProcessTimes` reports cumulative kernel and user time for a process.
`GetSystemTimes` reports cumulative system kernel and user time. CPU usage is
therefore a rate derived from two snapshots, not a property available from one
enumeration:

```text
(process time delta / system time delta) * 100
```

RustNT keeps the raw samples private to `rustnt-core`; the CLI receives only
the calculated percentage.

## Win32 API

When Rust calls a Win32 API, it crosses an FFI boundary into functions exposed
by Windows system libraries. Rust code must provide values and pointers in the
layout and lifetime expected by the Windows API.

## unsafe

FFI calls often require `unsafe` because the compiler cannot fully verify
foreign-function contracts, raw pointers, initialized structures, or opaque
handles. RustNT keeps each unsafe call small and documents why its arguments
are valid.

## Resource ownership

Many Windows resources must be released explicitly. RustNT wraps the handles
used by this task in a small RAII type whose `Drop` implementation calls
`CloseHandle`, preventing leaks on normal early returns and errors.

## Windows Service and SCM

A Windows service is a process whose lifecycle is coordinated by the Service
Control Manager (SCM). RustNT registers `RustNTControl` as its own service
process under `LocalSystem` with demand start. Demand start means installation
does not launch the process and the service is not configured to start at
boot; an operator or the SCM must request `start`.

The service reports state transitions instead of appearing instantaneously:
`START_PENDING` -> `RUNNING` -> `STOP_PENDING` -> `STOPPED`. The CLI asks the
SCM for these states and waits for the requested terminal state. A running
status can include the service process ID, while an absent registration is
reported as `NOT_INSTALLED`.

## Named Pipe framing

The bridge uses the local message-mode Pipe
`\\.\pipe\RustNT.Control.v1`. A message-mode Pipe preserves each request
and response as a message boundary, while the RustNT frame still carries
explicit lengths and a protocol version:

```text
request:  magic[4] | version[u16] | command[u16] | payload_length[u32] | payload
response: magic[4] | version[u16] | status[u32] | payload_length[u32] | payload
```

After reading the complete response frame, the client writes the four-byte
`ACK1` response acknowledgement on the same Pipe connection. The service waits
up to 5 seconds for this acknowledgement. It uses this transport-level
confirmation to know that the response was received before abandoning the
connection; a timeout, disconnect, or incorrect acknowledgement causes the
service to drop that connection and return to its one-instance accept loop.
`ACK1` is not a fourth protocol command.

The decoder rejects bad magic, unsupported versions, unknown commands,
oversized payloads, and trailing or truncated bytes. The fixed bridge commands
are `PING`, `IDENTITY`, `CAPABILITIES`, `PROCESS_INSPECT`, and
`PROCESS_TERMINATE`. The first three do not accept a request payload;
process-control commands use exact typed payloads.

## DACLs and LocalSystem risk

An access control list on a Named Pipe is part of the security boundary. RustNT
creates an explicit DACL granting full control to `SYSTEM` and Administrators
and read/write access to interactive users, and requests
`PIPE_REJECT_REMOTE_CLIENTS`. This makes the endpoint local and explicit
instead of inheriting an accidental default security descriptor.

`LocalSystem` has broad operating-system authority. Pipe access alone must not
be treated as authorization for a future mutating operation. Any future
privileged action needs a fixed whitelist, connecting-token checks,
operation-level authorization, input validation, and audit records. RustNT's
current bridge exposes only read-only identity and capability information plus
the health-check `PING` command.

## Token identity

A process token describes the security identity under which Windows evaluates
the process. The service queries its own token for the user SID, integrity
level, and elevation flag. SID `S-1-5-18` is rendered as `LocalSystem`; the
identity response also includes protocol version and the fixed capability
list. This is an observation of the service token, not a claim that every
client is trusted.

## Controlled process operations

`PROCESS_QUERY_LIMITED_INFORMATION` is a read-oriented process access right.
`PROCESS_TERMINATE` is a separate destructive right. The CLI never opens the
target with either right for the service operation; the LocalSystem service
does so only after validating the fixed request and the connecting client.

A PID is not a stable process identity. Windows can reuse a PID after the
original process exits, so the CLI first inspects the target and includes its
creation time in the terminate request. The service opens the current target,
reads its creation time again, and rejects a mismatch as `PID_REUSED`.

The service impersonates the Named Pipe client to query the caller's user SID,
elevation, and local Administrators membership. It compares the caller SID
with the target process token's owner SID and rejects system-owned or
cross-user targets. After collecting the caller facts, it calls
`RevertToSelf` before using the LocalSystem service context for target process
operations; this prevents the impersonated client context from changing the
meaning of later service-side calls.

Pipe ACLs and `PIPE_REJECT_REMOTE_CLIENTS` limit who can reach the endpoint,
but neither is sufficient authorization for a destructive operation. The
terminate path therefore performs token checks for every request and returns
fixed status names without exposing target metadata on authorization failure.

All process, token, Pipe, SCM, and event handles are RAII-owned. The service
does not retain a target HANDLE after the request. Inspection output excludes
command lines, environments, handles, token contents, and arbitrary target
data; field values are validated before entering the bounded key-value payload.
