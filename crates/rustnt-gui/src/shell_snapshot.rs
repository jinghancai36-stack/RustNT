use serde::Serialize;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_VALUE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ShellValueTarget {
    Shell,
    Userinit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum RegistryValueType {
    String,
    ExpandString,
    Other(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryRead {
    Present {
        value_type: RegistryValueType,
        value: String,
    },
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryReadError {
    pub category: String,
    pub win32_code: Option<u32>,
}

impl RegistryReadError {
    pub fn access_denied(win32_code: u32) -> Self {
        Self {
            category: "access_denied".to_owned(),
            win32_code: Some(win32_code),
        }
    }
}

pub trait ShellValueReader {
    fn read(&self, target: ShellValueTarget) -> Result<RegistryRead, RegistryReadError>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum SnapshotResult {
    Present,
    Missing,
    Failed {
        category: String,
        win32_code: Option<u32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShellValueSnapshot {
    pub target: ShellValueTarget,
    pub value_type: Option<RegistryValueType>,
    pub result: SnapshotResult,
    pub character_count: usize,
    pub digest: String,
    #[serde(skip)]
    pub display_value: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShellSnapshot {
    pub format_version: u32,
    pub captured_at_unix_seconds: u64,
    pub process_id: u32,
    pub entries: Vec<ShellValueSnapshot>,
}

pub fn collect_snapshot(
    reader: &impl ShellValueReader,
    captured_at_unix_seconds: u64,
    process_id: u32,
) -> ShellSnapshot {
    let entries = [ShellValueTarget::Shell, ShellValueTarget::Userinit]
        .into_iter()
        .map(|target| snapshot_value(reader, target))
        .collect();

    ShellSnapshot {
        format_version: FORMAT_VERSION,
        captured_at_unix_seconds,
        process_id,
        entries,
    }
}

fn snapshot_value(reader: &impl ShellValueReader, target: ShellValueTarget) -> ShellValueSnapshot {
    match reader.read(target) {
        Ok(RegistryRead::Missing) => ShellValueSnapshot {
            target,
            value_type: None,
            result: SnapshotResult::Missing,
            character_count: 0,
            digest: String::new(),
            display_value: None,
        },
        Ok(RegistryRead::Present { value_type, value }) => {
            let digest = fnv1a64_digest(&value);
            if value.contains('\0') {
                return ShellValueSnapshot {
                    target,
                    value_type: Some(value_type),
                    result: SnapshotResult::Failed {
                        category: "embedded_nul".to_owned(),
                        win32_code: None,
                    },
                    character_count: 0,
                    digest,
                    display_value: None,
                };
            }

            let display_value = sanitize_and_bound(&value);
            ShellValueSnapshot {
                target,
                value_type: Some(value_type),
                result: SnapshotResult::Present,
                character_count: display_value.chars().count(),
                digest,
                display_value: Some(display_value),
            }
        }
        Err(error) => ShellValueSnapshot {
            target,
            value_type: None,
            result: SnapshotResult::Failed {
                category: error.category,
                win32_code: error.win32_code,
            },
            character_count: 0,
            digest: String::new(),
            display_value: None,
        },
    }
}

fn fnv1a64_digest(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes().iter().take(MAX_VALUE_BYTES) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn sanitize_and_bound(value: &str) -> String {
    let mut sanitized = String::with_capacity(MAX_VALUE_BYTES.min(value.len()));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if sanitized.len() + character.len_utf8() > MAX_VALUE_BYTES {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureReader {
        shell: RegistryReadResult,
        userinit: RegistryReadResult,
    }

    type RegistryReadResult = Result<RegistryRead, RegistryReadError>;

    impl FixtureReader {
        fn with_values(
            shell: Option<(RegistryValueType, &str)>,
            userinit: Option<(RegistryValueType, &str)>,
        ) -> Self {
            Self::with_results(
                shell.map_or(Ok(RegistryRead::Missing), |(value_type, value)| {
                    Ok(RegistryRead::Present {
                        value_type,
                        value: value.to_owned(),
                    })
                }),
                userinit.map_or(Ok(RegistryRead::Missing), |(value_type, value)| {
                    Ok(RegistryRead::Present {
                        value_type,
                        value: value.to_owned(),
                    })
                }),
            )
        }

        fn with_results(shell: RegistryReadResult, userinit: RegistryReadResult) -> Self {
            Self { shell, userinit }
        }
    }

    impl ShellValueReader for FixtureReader {
        fn read(&self, target: ShellValueTarget) -> RegistryReadResult {
            match target {
                ShellValueTarget::Shell => self.shell.clone(),
                ShellValueTarget::Userinit => self.userinit.clone(),
            }
        }
    }

    #[test]
    fn collect_snapshot_records_metadata_without_full_value() {
        let reader = FixtureReader::with_values(
            Some((RegistryValueType::String, "explorer.exe\0ignored")),
            Some((
                RegistryValueType::ExpandString,
                "%SystemRoot%\\system32\\userinit.exe",
            )),
        );
        let snapshot = collect_snapshot(&reader, 1_700_000_000, 1234);
        assert_eq!(snapshot.format_version, FORMAT_VERSION);
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0].target, ShellValueTarget::Shell);
        assert!(snapshot.entries[0].digest.starts_with("fnv1a64:"));
        assert!(snapshot.entries[0].display_value.is_none());
    }

    #[test]
    fn collect_snapshot_preserves_missing_and_reader_error() {
        let reader = FixtureReader::with_results(
            Ok(RegistryRead::Missing),
            Err(RegistryReadError::access_denied(5)),
        );
        let snapshot = collect_snapshot(&reader, 10, 20);
        assert_eq!(snapshot.entries[0].result, SnapshotResult::Missing);
        assert_eq!(
            snapshot.entries[1].result,
            SnapshotResult::Failed {
                category: "access_denied".to_owned(),
                win32_code: Some(5),
            }
        );
    }
}
