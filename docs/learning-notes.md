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
