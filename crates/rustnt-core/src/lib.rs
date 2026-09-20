#![cfg(windows)]

pub mod authorization;
pub mod monitor;
pub mod process_control;
pub mod service;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, GetSystemTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub thread_count: u32,
    pub memory_bytes: Option<u64>,
    pub path: Option<String>,
    pub cpu_percent: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustNtError {
    operation: &'static str,
    code: u32,
}

impl RustNtError {
    fn last(operation: &'static str) -> Self {
        let code = unsafe {
            // SAFETY: GetLastError reads the calling thread's Win32 error value and has no
            // pointer or handle preconditions.
            GetLastError()
        };
        Self { operation, code }
    }
}

impl fmt::Display for RustNtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: Windows error {}", self.operation, self.code)
    }
}

impl std::error::Error for RustNtError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    process_kernel: u64,
    process_user: u64,
    system_kernel: u64,
    system_user: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemTimes {
    idle: u64,
    kernel: u64,
    user: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    creation_time_100ns: Option<u64>,
}

pub struct ProcessSnapshot {
    processes: Vec<ProcessInfo>,
    cpu_times: HashMap<u32, CpuTimes>,
    system_times: SystemTimes,
    process_identities: HashMap<u32, ProcessIdentity>,
}

impl ProcessSnapshot {
    pub fn processes(&self) -> &[ProcessInfo] {
        &self.processes
    }

    pub fn into_processes(self) -> Vec<ProcessInfo> {
        self.processes
    }

    pub fn apply_cpu_from(&mut self, previous: &ProcessSnapshot) {
        for process in &mut self.processes {
            process.cpu_percent = previous
                .cpu_times
                .get(&process.pid)
                .and_then(|previous_times| {
                    self.cpu_times.get(&process.pid).and_then(|current_times| {
                        calculate_cpu_percent(*previous_times, *current_times)
                    })
                });
        }
    }

    pub fn system_cpu_percent_from(&self, previous: &ProcessSnapshot) -> Option<f32> {
        let idle_delta = self
            .system_times
            .idle
            .checked_sub(previous.system_times.idle)?;
        let kernel_delta = self
            .system_times
            .kernel
            .checked_sub(previous.system_times.kernel)?;
        let user_delta = self
            .system_times
            .user
            .checked_sub(previous.system_times.user)?;
        let total_delta = kernel_delta.checked_add(user_delta)?;
        if total_delta == 0 {
            return None;
        }
        let busy_delta = total_delta.checked_sub(idle_delta)?;
        let percentage = (busy_delta as f64 / total_delta as f64) * 100.0;
        if percentage.is_finite() {
            Some(percentage.clamp(0.0, 100.0) as f32)
        } else {
            None
        }
    }

    pub fn process_changes_from(
        &self,
        previous: &ProcessSnapshot,
    ) -> crate::monitor::ProcessChanges {
        let mut changes = crate::monitor::ProcessChanges::default();
        let current_by_pid: HashMap<_, _> = self
            .processes
            .iter()
            .map(|process| (process.pid, process))
            .collect();
        let previous_by_pid: HashMap<_, _> = previous
            .processes
            .iter()
            .map(|process| (process.pid, process))
            .collect();

        for (&pid, current_process) in &current_by_pid {
            match previous_by_pid.get(&pid) {
                None => changes.added.push(crate::monitor::ProcessChange {
                    pid,
                    name: current_process.name.clone(),
                }),
                Some(previous_process) if !same_process_identity(self, previous, pid) => {
                    changes.exited.push(crate::monitor::ProcessChange {
                        pid,
                        name: previous_process.name.clone(),
                    });
                    changes.added.push(crate::monitor::ProcessChange {
                        pid,
                        name: current_process.name.clone(),
                    });
                }
                Some(_) => {}
            }
        }

        for (&pid, previous_process) in &previous_by_pid {
            if !current_by_pid.contains_key(&pid) {
                changes.exited.push(crate::monitor::ProcessChange {
                    pid,
                    name: previous_process.name.clone(),
                });
            }
        }

        changes.added.sort_by_key(|change| change.pid);
        changes.exited.sort_by_key(|change| change.pid);
        changes
    }
}

fn same_process_identity(current: &ProcessSnapshot, previous: &ProcessSnapshot, pid: u32) -> bool {
    let current_creation = current
        .process_identities
        .get(&pid)
        .and_then(|identity| identity.creation_time_100ns);
    let previous_creation = previous
        .process_identities
        .get(&pid)
        .and_then(|identity| identity.creation_time_100ns);

    match (current_creation, previous_creation) {
        (Some(current_creation), Some(previous_creation)) => current_creation == previous_creation,
        _ => true,
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, RustNtError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(RustNtError::last(operation))
        } else {
            Ok(Self(handle))
        }
    }

    fn get(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                // SAFETY: The handle was returned by a Win32 API and is owned exclusively by
                // this wrapper, so closing it once is valid.
                CloseHandle(self.0);
            }
        }
    }
}

pub fn list_processes() -> Result<Vec<ProcessInfo>, RustNtError> {
    sample_processes().map(ProcessSnapshot::into_processes)
}

pub fn sample_processes() -> Result<ProcessSnapshot, RustNtError> {
    let system_times = system_times()?;
    let process_snapshot = unsafe {
        // SAFETY: The flags are valid, and process ID 0 requests a system-wide snapshot.
        CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    };
    let process_snapshot = OwnedHandle::new(process_snapshot, "failed to create process snapshot")?;

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        // SAFETY: PROCESSENTRY32W contains only integer and fixed-size array fields, for which
        // an all-zero value is valid before Tool Help fills the structure.
        ..unsafe { std::mem::zeroed() }
    };
    let first_ok = unsafe {
        // SAFETY: entry points to writable storage with dwSize initialized as required by Win32.
        Process32FirstW(process_snapshot.get(), &mut entry)
    };
    if first_ok == 0 {
        return Err(RustNtError::last("failed to read first process"));
    }

    let mut processes = Vec::new();
    let mut cpu_time_map = HashMap::new();
    let mut process_identities = HashMap::new();

    loop {
        let pid = entry.th32ProcessID;
        let name = wide_string(&entry.szExeFile);
        let (memory_bytes, path, process_cpu) = process_metrics(pid);
        let cpu_times = process_cpu.map(|(_, process_kernel, process_user)| CpuTimes {
            process_kernel,
            process_user,
            system_kernel: system_times.kernel,
            system_user: system_times.user,
        });
        process_identities.insert(
            pid,
            ProcessIdentity {
                creation_time_100ns: process_cpu.map(|(creation, _, _)| creation),
            },
        );
        processes.push(ProcessInfo {
            pid,
            name,
            thread_count: entry.cntThreads,
            memory_bytes,
            path,
            cpu_percent: None,
        });
        if let Some(cpu_times) = cpu_times {
            cpu_time_map.insert(pid, cpu_times);
        }

        let next_ok = unsafe {
            // SAFETY: The snapshot and entry remain valid for the duration of enumeration.
            Process32NextW(process_snapshot.get(), &mut entry)
        };
        if next_ok == 0 {
            break;
        }
    }

    Ok(ProcessSnapshot {
        processes,
        cpu_times: cpu_time_map,
        system_times,
        process_identities,
    })
}

fn system_times() -> Result<SystemTimes, RustNtError> {
    let mut idle = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let result = unsafe {
        // SAFETY: All pointers reference writable FILETIME values that live until the call
        // returns. GetSystemTimes does not retain them.
        GetSystemTimes(&mut idle, &mut kernel, &mut user)
    };
    if result == 0 {
        Err(RustNtError::last("failed to read system times"))
    } else {
        Ok(SystemTimes {
            idle: filetime_to_u64(idle),
            kernel: filetime_to_u64(kernel),
            user: filetime_to_u64(user),
        })
    }
}

type ProcessMetrics = (Option<u64>, Option<String>, Option<(u64, u64, u64)>);

fn process_metrics(pid: u32) -> ProcessMetrics {
    let handle = unsafe {
        // SAFETY: pid comes from the system snapshot; the requested rights are read-only query
        // rights. A null handle is handled as an unavailable metric.
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            0,
            pid,
        )
    };
    let Ok(handle) = OwnedHandle::new(handle, "failed to open process") else {
        return (None, None, None);
    };

    let memory_bytes = process_memory(handle.get());
    let path = process_path(handle.get());
    let process_cpu = process_times(handle.get());
    (memory_bytes, path, process_cpu)
}

fn process_memory(handle: HANDLE) -> Option<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        // SAFETY: PROCESS_MEMORY_COUNTERS is a C data structure of integer fields, and zero is
        // a valid initial value before GetProcessMemoryInfo writes the counters.
        ..unsafe { std::mem::zeroed() }
    };
    let result = unsafe {
        // SAFETY: The handle is valid and counters points to writable storage with cb set.
        GetProcessMemoryInfo(
            handle,
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if result == 0 {
        None
    } else {
        Some(counters.WorkingSetSize as u64)
    }
}

fn process_path(handle: HANDLE) -> Option<String> {
    const PATH_BUFFER_LENGTH: usize = 32_768;
    let mut buffer = vec![0u16; PATH_BUFFER_LENGTH];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        // SAFETY: The handle is valid for the duration of the call, and buffer/length point to
        // writable storage with capacity described by length.
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut length)
    };
    if result == 0 || length == 0 {
        None
    } else {
        Some(wide_string(&buffer[..length as usize]))
    }
}

fn process_times(handle: HANDLE) -> Option<(u64, u64, u64)> {
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let result = unsafe {
        // SAFETY: The handle is valid for the duration of the call, and all FILETIME pointers
        // reference writable local storage.
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
    };
    if result == 0 {
        None
    } else {
        Some((
            filetime_to_u64(creation),
            filetime_to_u64(kernel),
            filetime_to_u64(user),
        ))
    }
}

fn filetime_to_u64(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn calculate_cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<f32> {
    let process_kernel_delta = current
        .process_kernel
        .checked_sub(previous.process_kernel)?;
    let process_user_delta = current.process_user.checked_sub(previous.process_user)?;
    let system_kernel_delta = current.system_kernel.checked_sub(previous.system_kernel)?;
    let system_user_delta = current.system_user.checked_sub(previous.system_user)?;
    let process_delta = process_kernel_delta.checked_add(process_user_delta)?;
    let system_delta = system_kernel_delta.checked_add(system_user_delta)?;
    if process_delta == 0 || system_delta == 0 {
        return None;
    }

    let percentage = (process_delta as f64 / system_delta as f64) * 100.0;
    if percentage.is_finite() {
        Some(percentage.clamp(0.0, 100.0) as f32)
    } else {
        None
    }
}

fn wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..length])
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        calculate_cpu_percent, list_processes, sample_processes, CpuTimes, ProcessIdentity,
        ProcessInfo, ProcessSnapshot, SystemTimes,
    };
    use crate::monitor::{ProcessChange, ProcessChanges};

    #[test]
    fn system_cpu_percentage_uses_busy_delta_over_total_delta() {
        let previous = system_snapshot(100, 1_000, 2_000);
        let current = system_snapshot(200, 2_000, 3_000);

        assert_eq!(current.system_cpu_percent_from(&previous), Some(95.0));
    }

    #[test]
    fn system_cpu_percentage_is_unavailable_for_backwards_or_zero_deltas() {
        let previous = system_snapshot(100, 1_000, 2_000);

        assert_eq!(
            system_snapshot(100, 1_000, 2_000).system_cpu_percent_from(&previous),
            None
        );
        assert_eq!(
            system_snapshot(50, 1_000, 2_000).system_cpu_percent_from(&previous),
            None
        );
    }

    #[test]
    fn first_process_comparison_reports_added_and_exited_processes() {
        let previous = snapshot_with_processes(&[(7, "old.exe", Some(10))]);
        let current = snapshot_with_processes(&[(8, "new.exe", Some(20))]);

        assert_eq!(
            current.process_changes_from(&previous),
            ProcessChanges {
                added: vec![ProcessChange {
                    pid: 8,
                    name: "new.exe".to_string(),
                }],
                exited: vec![ProcessChange {
                    pid: 7,
                    name: "old.exe".to_string(),
                }],
            }
        );
    }

    #[test]
    fn process_comparison_reports_pid_reuse_as_exit_and_addition() {
        let previous = snapshot_with_processes(&[(7, "old.exe", Some(10))]);
        let current = snapshot_with_processes(&[(7, "new.exe", Some(20))]);

        let changes = current.process_changes_from(&previous);

        assert_eq!(
            changes,
            ProcessChanges {
                added: vec![ProcessChange {
                    pid: 7,
                    name: "new.exe".to_string(),
                }],
                exited: vec![ProcessChange {
                    pid: 7,
                    name: "old.exe".to_string(),
                }],
            }
        );
    }

    #[test]
    fn process_comparison_falls_back_to_pid_when_creation_time_is_missing() {
        let previous = snapshot_with_processes(&[(7, "old.exe", None)]);
        let current = snapshot_with_processes(&[(7, "new.exe", Some(20))]);

        assert_eq!(
            current.process_changes_from(&previous),
            ProcessChanges::default()
        );
    }

    #[test]
    fn process_comparison_sorts_changes_by_pid() {
        let previous = snapshot_with_processes(&[
            (9, "old-nine.exe", Some(90)),
            (3, "old-three.exe", Some(30)),
        ]);
        let current = snapshot_with_processes(&[
            (8, "new-eight.exe", Some(80)),
            (2, "new-two.exe", Some(20)),
        ]);

        assert_eq!(
            current.process_changes_from(&previous),
            ProcessChanges {
                added: vec![
                    ProcessChange {
                        pid: 2,
                        name: "new-two.exe".to_string(),
                    },
                    ProcessChange {
                        pid: 8,
                        name: "new-eight.exe".to_string(),
                    },
                ],
                exited: vec![
                    ProcessChange {
                        pid: 3,
                        name: "old-three.exe".to_string(),
                    },
                    ProcessChange {
                        pid: 9,
                        name: "old-nine.exe".to_string(),
                    },
                ],
            }
        );
    }

    #[test]
    fn cpu_percentage_uses_process_delta_over_system_delta() {
        let previous = CpuTimes {
            process_kernel: 100,
            process_user: 100,
            system_kernel: 1_000,
            system_user: 1_000,
        };
        let current = CpuTimes {
            process_kernel: 200,
            process_user: 200,
            system_kernel: 2_000,
            system_user: 2_000,
        };

        assert_eq!(calculate_cpu_percent(previous, current), Some(10.0));
    }

    #[test]
    fn cpu_percentage_is_unavailable_for_invalid_deltas() {
        let previous = CpuTimes {
            process_kernel: 100,
            process_user: 100,
            system_kernel: 1_000,
            system_user: 1_000,
        };
        let zero_system_delta = CpuTimes { ..previous };
        let backward = CpuTimes {
            process_kernel: 50,
            ..previous
        };

        assert_eq!(calculate_cpu_percent(previous, zero_system_delta), None);
        assert_eq!(calculate_cpu_percent(previous, backward), None);
    }

    #[test]
    #[cfg(windows)]
    fn process_enumeration_succeeds_and_returns_processes() {
        let processes = list_processes().expect("process enumeration should succeed");
        assert!(!processes.is_empty());
        assert!(processes.iter().all(|process| !process.name.is_empty()));
        assert!(processes.iter().any(|process| process.pid > 0));
        assert!(processes
            .iter()
            .all(|process| process.cpu_percent.is_none()));
    }

    #[test]
    #[cfg(windows)]
    fn current_process_is_reasonably_findable() {
        let current_pid = std::process::id();
        let processes = list_processes().expect("process enumeration should succeed");
        assert!(processes.iter().any(|process| process.pid == current_pid));
    }

    #[test]
    #[cfg(windows)]
    fn current_process_path_is_available_in_a_snapshot() {
        let snapshot = sample_processes().expect("process sampling should succeed");
        assert!(!snapshot.processes().is_empty());
        assert!(snapshot
            .processes()
            .iter()
            .all(|process| process.cpu_percent.is_none()));
        assert!(snapshot
            .processes()
            .iter()
            .any(|process| process.pid == std::process::id() && process.path.is_some()));
    }

    #[test]
    fn wide_string_stops_at_nul() {
        assert_eq!(
            super::wide_string(&[b'R' as u16, b'u' as u16, 0, b'x' as u16]),
            "Ru"
        );
    }

    fn system_snapshot(idle: u64, kernel: u64, user: u64) -> ProcessSnapshot {
        ProcessSnapshot {
            processes: Vec::new(),
            cpu_times: HashMap::new(),
            system_times: SystemTimes { idle, kernel, user },
            process_identities: HashMap::new(),
        }
    }

    fn snapshot_with_processes(processes: &[(u32, &str, Option<u64>)]) -> ProcessSnapshot {
        let mut process_rows = Vec::new();
        let mut cpu_times = HashMap::new();
        let mut process_identities = HashMap::new();

        for (pid, name, creation_time_100ns) in processes {
            process_rows.push(ProcessInfo {
                pid: *pid,
                name: (*name).to_string(),
                thread_count: 1,
                memory_bytes: None,
                path: None,
                cpu_percent: None,
            });
            cpu_times.insert(
                *pid,
                CpuTimes {
                    process_kernel: 10,
                    process_user: 20,
                    system_kernel: 1_000,
                    system_user: 2_000,
                },
            );
            process_identities.insert(
                *pid,
                ProcessIdentity {
                    creation_time_100ns: *creation_time_100ns,
                },
            );
        }

        ProcessSnapshot {
            processes: process_rows,
            cpu_times,
            system_times: SystemTimes {
                idle: 100,
                kernel: 1_000,
                user: 2_000,
            },
            process_identities,
        }
    }
}
