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
