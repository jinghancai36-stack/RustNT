# Benchmarking Philosophy

RustNT performance claims must be measurable. "Feels faster" is not a useful
benchmark result.

Future comparisons should record the relevant parts of:

- cold start
- warm start
- latency
- CPU usage
- memory usage
- allocations
- I/O
- test dataset
- hardware
- Windows version

Task 01 creates the benchmark crate as a placeholder. It does not introduce
benchmark infrastructure before there is a concrete behavior to measure.

Task 02 adds a synchronous watch mode. Future measurements should record the
refresh interval, number of enumerated processes, number of accessible process
handles, number of paths returned, and the time spent in:

- Tool Help snapshot creation and enumeration
- per-process HANDLE opening
- memory/path/time queries
- table filtering, sorting, and rendering

CPU percentages are interval-dependent, so results from different
`--watch <seconds>` values should not be compared as if they were the same
measurement.

## Service bridge measurements

Task 03 measurements must be reproducible and must not be reported without the
environment and lifecycle state that produced them. Measure each operation in
milliseconds with a monotonic timer, repeat enough times to show a stable
distribution, and retain the raw samples. Do not combine install, start, Pipe,
identity, stop, or uninstall timings into one unlabeled number.

### Checklist

- [ ] Record the Windows edition and exact version/build.
- [ ] Record the account context running the measurement.
- [ ] Record whether the terminal is elevated.
- [ ] Record whether the service was not installed, freshly installed, or
      already installed before each run.
- [ ] Record whether the service was cold or already running before each
      measurement.
- [ ] Build the workspace from the documented commit and record the binary
      location.
- [ ] Capture command exit codes and any Windows error codes.
- [ ] Stop and uninstall the service after a run that installed it.

### Result template

```text
Date/time:
Commit:
Windows edition/version/build:
Machine:
Account:
Elevated: yes/no
Service state before run: not installed / stopped / running
Install state: fresh install / already installed
Cold/warm state: cold / warm
Repetitions:
Timer and units:

Measurement                              | Samples / summary | Exit codes
service install latency                  |                    |
service start-to-running latency         |                    |
Pipe request/response round trip         |                    |
identity query duration                  |                    |
service stop latency                     |                    |

Notes, Win32 errors, and cleanup result:
```

The lifecycle command sequence used for a full measurement pass is:

```text
rustnt service install
rustnt service start
rustnt service status
rustnt service identity
rustnt service stop
rustnt service uninstall
```

The Pipe round-trip measurement should identify which fixed command was sent
(`PING`, `IDENTITY`, or `CAPABILITIES`) and include request-to-complete
response time. Identity duration should include the service token query and
response decoding, not service startup. No measured values are asserted in
this document.
