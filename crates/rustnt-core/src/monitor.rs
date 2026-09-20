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

#[cfg(test)]
mod tests {
    use super::{DiskInfo, MemoryInfo};

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
}
