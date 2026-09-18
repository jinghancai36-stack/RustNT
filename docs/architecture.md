# Architecture

Task 02 is a normal Windows user-mode application. RustNT does not replace
Windows components, the Windows kernel, or drivers.

```text
rustnt-cli
     |
     v
rustnt-core
     |
     v
Windows API
     |
     v
ntdll / Windows user-space infrastructure
     |
     v
NT Kernel
```

`rustnt-cli` owns command-line parsing and presentation. `rustnt-core` owns the
Windows interaction and exposes Rust data types. The boundary is deliberately
small so future commands can reuse the system-information layer without
placing Win32 calls in the CLI.

Process enumeration uses the documented Tool Help snapshot APIs:
`CreateToolhelp32Snapshot`, `Process32FirstW`, and `Process32NextW`.
The process record's `cntThreads` field supplies the snapshot's thread count.
Memory is best-effort through
`OpenProcess` and `GetProcessMemoryInfo`; protected or inaccessible processes
show an unavailable memory value rather than aborting the complete listing.

Each process also uses the same owned process HANDLE for
`QueryFullProcessImageNameW` and `GetProcessTimes` when access is available.
`GetSystemTimes` is sampled once per snapshot. `rustnt-core` exposes a
`ProcessSnapshot` so the CLI can compare two snapshots by PID without exposing
Win32 handles or unsafe code outside the core crate.

One-shot mode intentionally leaves CPU unavailable. Watch mode renders a
baseline, waits, samples again, and computes:

```text
process CPU = process kernel/user delta
               ------------------------- * 100
               system kernel/user delta
```

CPU values are clamped to `0..=100` and become unavailable when a process
disappears or any required counter delta is invalid.
