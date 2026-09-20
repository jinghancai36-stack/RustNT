use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

const DRIVE_FIXED: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl MemoryInfo {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn used_percent(&self) -> Option<f32> {
        if self.total_bytes == 0 || self.available_bytes > self.total_bytes {
            return None;
        }
        Some((self.used_bytes() as f64 / self.total_bytes as f64 * 100.0) as f32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub root: String,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

impl DiskInfo {
    pub fn used_percent(&self) -> Option<f32> {
        let total = self.total_bytes?;
        let free = self.free_bytes?;
        if total == 0 || free > total {
            return None;
        }
        Some(((total - free) as f64 / total as f64 * 100.0) as f32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessChange {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessChanges {
    pub added: Vec<ProcessChange>,
    pub exited: Vec<ProcessChange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorSnapshot {
    pub cpu_percent: Option<f32>,
    pub memory: MemoryInfo,
    pub disks: Vec<DiskInfo>,
    pub process_changes: ProcessChanges,
}

pub struct MonitorSampler {
    previous: Option<crate::ProcessSnapshot>,
}

impl Default for MonitorSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorSampler {
    pub fn new() -> Self {
        Self { previous: None }
    }

    pub fn sample(&mut self) -> Result<MonitorSnapshot, crate::RustNtError> {
        let current = crate::sample_processes()?;
        let cpu_percent = self
            .previous
            .as_ref()
            .and_then(|previous| current.system_cpu_percent_from(previous));
        let process_changes = self
            .previous
            .as_ref()
            .map(|previous| current.process_changes_from(previous))
            .unwrap_or_default();
        let memory = sample_memory()?;
        let disks = sample_disks()?;
        self.previous = Some(current);
        Ok(MonitorSnapshot {
            cpu_percent,
            memory,
            disks,
            process_changes,
        })
    }
}

fn sample_memory() -> Result<MemoryInfo, crate::RustNtError> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        // SAFETY: MEMORYSTATUSEX contains integer fields and is initialized as required by
        // GlobalMemoryStatusEx before the call fills its output fields.
        ..unsafe { std::mem::zeroed() }
    };
    let result = unsafe {
        // SAFETY: status points to writable storage with dwLength initialized as required by
        // the Win32 API, and the API does not retain the pointer.
        GlobalMemoryStatusEx(&mut status)
    };
    if result == 0 {
        return Err(crate::RustNtError::last("failed to read memory status"));
    }
    Ok(MemoryInfo {
        total_bytes: status.ullTotalPhys,
        available_bytes: status.ullAvailPhys,
    })
}

fn is_fixed_drive_type(drive_type: u32) -> bool {
    drive_type == DRIVE_FIXED
}

fn sample_disks() -> Result<Vec<DiskInfo>, crate::RustNtError> {
    let drive_mask = unsafe {
        // SAFETY: GetLogicalDrives has no pointer or handle arguments.
        GetLogicalDrives()
    };
    if drive_mask == 0 {
        return Err(crate::RustNtError::last(
            "failed to enumerate logical drives",
        ));
    }

    let mut disks = Vec::new();
    for drive_index in 0..26u32 {
        if drive_mask & (1u32 << drive_index) == 0 {
            continue;
        }

        let root = vec![
            b'A' as u16 + drive_index as u16,
            b':' as u16,
            b'\\' as u16,
            0,
        ];
        let drive_type = unsafe {
            // SAFETY: root is a valid null-terminated UTF-16 drive root for the duration of the
            // call, and GetDriveTypeW does not retain it.
            GetDriveTypeW(root.as_ptr())
        };
        if is_fixed_drive_type(drive_type) {
            disks.push(sample_disk_capacity(&root));
        }
    }
    disks.sort_by(|left, right| left.root.cmp(&right.root));
    Ok(disks)
}

fn sample_disk_capacity(root: &[u16]) -> DiskInfo {
    let root_end = root
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(root.len());
    let root_name = String::from_utf16_lossy(&root[..root_end]);
    let mut free_bytes_available = 0;
    let mut total_bytes = 0;
    let mut _total_free_bytes = 0;
    let result = unsafe {
        // SAFETY: root is a valid null-terminated UTF-16 drive root and all output pointers
        // reference writable local storage for the duration of the call.
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &mut free_bytes_available,
            &mut total_bytes,
            &mut _total_free_bytes,
        )
    };
    if result == 0 {
        disk_info_from_capacity(root_name, None)
    } else {
        disk_info_from_capacity(root_name, Some((free_bytes_available, total_bytes)))
    }
}

fn disk_info_from_capacity(root: String, capacity: Option<(u64, u64)>) -> DiskInfo {
    match capacity {
        Some((free_bytes, total_bytes)) => DiskInfo {
            root,
            total_bytes: Some(total_bytes),
            free_bytes: Some(free_bytes),
        },
        None => DiskInfo {
            root,
            total_bytes: None,
            free_bytes: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        disk_info_from_capacity, is_fixed_drive_type, DiskInfo, MemoryInfo, MonitorSampler,
        DRIVE_FIXED,
    };

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;

    #[test]
    fn memory_used_percent_rejects_invalid_available_bytes() {
        let memory = MemoryInfo {
            total_bytes: 100,
            available_bytes: 101,
        };
        assert_eq!(memory.used_bytes(), 0);
        assert_eq!(memory.used_percent(), None);
    }

    #[test]
    fn disk_used_percent_returns_none_for_missing_or_invalid_capacity() {
        assert_eq!(
            DiskInfo {
                root: "C:\\".to_string(),
                total_bytes: None,
                free_bytes: Some(10),
            }
            .used_percent(),
            None
        );
        assert_eq!(
            DiskInfo {
                root: "C:\\".to_string(),
                total_bytes: Some(100),
                free_bytes: Some(101),
            }
            .used_percent(),
            None
        );
    }

    #[test]
    fn only_fixed_drive_types_are_selected() {
        assert!(is_fixed_drive_type(DRIVE_FIXED));
        assert!(!is_fixed_drive_type(DRIVE_REMOVABLE));
        assert!(!is_fixed_drive_type(DRIVE_REMOTE));
        assert!(!is_fixed_drive_type(DRIVE_CDROM));
    }

    #[test]
    fn failed_disk_capacity_retains_root_with_unavailable_fields() {
        assert_eq!(
            disk_info_from_capacity("Z:\\".to_string(), None),
            DiskInfo {
                root: "Z:\\".to_string(),
                total_bytes: None,
                free_bytes: None,
            }
        );
    }

    #[test]
    fn successful_disk_capacity_uses_caller_available_free_bytes() {
        assert_eq!(
            disk_info_from_capacity("C:\\".to_string(), Some((25, 100))),
            DiskInfo {
                root: "C:\\".to_string(),
                total_bytes: Some(100),
                free_bytes: Some(25),
            }
        );
    }

    #[test]
    fn first_sampler_snapshot_has_no_cpu_or_process_events() {
        let mut sampler = MonitorSampler::new();
        let snapshot = sampler
            .sample()
            .expect("first monitor sample should succeed");

        assert_eq!(snapshot.cpu_percent, None);
        assert!(snapshot.process_changes.added.is_empty());
        assert!(snapshot.process_changes.exited.is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn monitor_sampler_collects_two_snapshots() {
        let mut sampler = MonitorSampler::new();
        let first = sampler
            .sample()
            .expect("first monitor sample should succeed");
        let second = sampler
            .sample()
            .expect("second monitor sample should succeed");

        assert!(first.memory.total_bytes > 0);
        assert!(first.memory.available_bytes <= first.memory.total_bytes);
        assert!(second.memory.total_bytes > 0);
    }
}
