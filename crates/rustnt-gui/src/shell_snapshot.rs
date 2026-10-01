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

#[cfg(windows)]
pub struct WindowsShellReader;

#[cfg(windows)]
impl WindowsShellReader {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
impl Default for WindowsShellReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl ShellValueReader for WindowsShellReader {
    fn read(&self, target: ShellValueTarget) -> Result<RegistryRead, RegistryReadError> {
        use std::ptr::{null, null_mut};
        use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA};
        use windows_sys::Win32::System::Registry::{
            RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_EXPAND_SZ,
            REG_SZ,
        };

        const KEY_PATH: &str = r"Software\Microsoft\Windows NT\CurrentVersion\Winlogon";
        const SHELL_NAME: &str = "Shell";
        const USERINIT_NAME: &str = "Userinit";

        let value_name = match target {
            ShellValueTarget::Shell => SHELL_NAME,
            ShellValueTarget::Userinit => USERINIT_NAME,
        };
        let key_path = wide_null(KEY_PATH);
        let value_name = wide_null(value_name);
        let mut key = null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                key_path.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut key,
            )
        };
        if status != 0 {
            return if status == ERROR_FILE_NOT_FOUND {
                Ok(RegistryRead::Missing)
            } else {
                Err(registry_error(status))
            };
        }
        let _key_guard = RegistryKey(key);

        let mut raw_value_type = 0_u32;
        let mut byte_count = 0_u32;
        let status = unsafe {
            RegQueryValueExW(
                key,
                value_name.as_ptr(),
                null(),
                &mut raw_value_type,
                null_mut(),
                &mut byte_count,
            )
        };
        if status != 0 {
            return if status == ERROR_FILE_NOT_FOUND {
                Ok(RegistryRead::Missing)
            } else {
                Err(registry_error(status))
            };
        }

        let value_type = match raw_value_type {
            REG_SZ => RegistryValueType::String,
            REG_EXPAND_SZ => RegistryValueType::ExpandString,
            _ => {
                return Err(RegistryReadError {
                    category: "unsupported_type".to_owned(),
                    win32_code: None,
                });
            }
        };
        let byte_count = byte_count as usize;
        if byte_count > MAX_VALUE_BYTES {
            return Err(RegistryReadError {
                category: "value_too_large".to_owned(),
                win32_code: None,
            });
        }

        let mut data = vec![0_u8; byte_count];
        let mut actual_byte_count = byte_count as u32;
        let mut actual_value_type = raw_value_type;
        let status = unsafe {
            RegQueryValueExW(
                key,
                value_name.as_ptr(),
                null(),
                &mut actual_value_type,
                data.as_mut_ptr().cast(),
                &mut actual_byte_count,
            )
        };
        if status != 0 {
            if status == ERROR_MORE_DATA {
                return Err(RegistryReadError {
                    category: "value_too_large".to_owned(),
                    win32_code: Some(status),
                });
            }
            return Err(registry_error(status));
        }
        if actual_value_type != raw_value_type {
            return Err(unsupported_type(actual_value_type));
        }
        let actual_byte_count = actual_byte_count as usize;
        if actual_byte_count > data.len() {
            return Err(RegistryReadError {
                category: "value_too_large".to_owned(),
                win32_code: Some(ERROR_MORE_DATA),
            });
        }
        data.truncate(actual_byte_count);
        decode_registry_string(&data, value_type)
    }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn decode_registry_string(
    data: &[u8],
    value_type: RegistryValueType,
) -> Result<RegistryRead, RegistryReadError> {
    if !data.len().is_multiple_of(2) {
        return Err(RegistryReadError {
            category: "invalid_utf16".to_owned(),
            win32_code: None,
        });
    }
    let units: Vec<u16> = data
        .chunks(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    let content_end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    if units[content_end..].iter().any(|unit| *unit != 0) {
        return Err(RegistryReadError {
            category: "embedded_nul".to_owned(),
            win32_code: None,
        });
    }
    let value = String::from_utf16(&units[..content_end]).map_err(|_| RegistryReadError {
        category: "invalid_utf16".to_owned(),
        win32_code: None,
    })?;
    Ok(RegistryRead::Present { value_type, value })
}

#[cfg(windows)]
fn unsupported_type(value_type: u32) -> RegistryReadError {
    RegistryReadError {
        category: "unsupported_type".to_owned(),
        win32_code: Some(value_type),
    }
}

#[cfg(windows)]
fn registry_error(win32_code: u32) -> RegistryReadError {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_MORE_DATA};

    RegistryReadError {
        category: if win32_code == ERROR_ACCESS_DENIED {
            "access_denied"
        } else if win32_code == ERROR_MORE_DATA {
            "value_too_large"
        } else {
            "registry_error"
        }
        .to_owned(),
        win32_code: Some(win32_code),
    }
}

#[cfg(windows)]
struct RegistryKey(windows_sys::Win32::System::Registry::HKEY);

#[cfg(windows)]
impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::System::Registry::RegCloseKey;

            RegCloseKey(self.0);
        }
    }
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
        },
        Ok(RegistryRead::Present { value_type, value }) => {
            if value.len() > MAX_VALUE_BYTES {
                return ShellValueSnapshot {
                    target,
                    value_type: Some(value_type),
                    result: SnapshotResult::Failed {
                        category: "value_too_large".to_owned(),
                        win32_code: None,
                    },
                    character_count: 0,
                    digest: String::new(),
                };
            }

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
                };
            }

            ShellValueSnapshot {
                target,
                value_type: Some(value_type),
                result: SnapshotResult::Present,
                character_count: sanitized_character_count(&value),
                digest,
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

fn sanitized_character_count(value: &str) -> usize {
    let mut byte_count = 0;
    let mut character_count = 0;
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if byte_count + character.len_utf8() > MAX_VALUE_BYTES {
            break;
        }
        byte_count += character.len_utf8();
        character_count += 1;
    }
    character_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_reader_returns_typed_results_for_both_targets() {
        let reader = WindowsShellReader::new();
        let shell = reader.read(ShellValueTarget::Shell);
        let userinit = reader.read(ShellValueTarget::Userinit);
        assert!(shell.is_ok() || shell.as_ref().is_err_and(|e| e.win32_code.is_some()));
        assert!(userinit.is_ok() || userinit.as_ref().is_err_and(|e| e.win32_code.is_some()));
    }

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
        assert_eq!(
            snapshot.entries[0].result,
            SnapshotResult::Failed {
                category: "embedded_nul".to_owned(),
                win32_code: None,
            }
        );
        assert!(!format!("{snapshot:?}").contains("explorer.exe"));
        assert!(!format!("{snapshot:?}").contains("userinit.exe"));
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

    #[test]
    fn collect_snapshot_sanitizes_controls_and_bounds_metadata() {
        let prefix = "prefix\t\r\n";
        let value = format!("{prefix}{}", "x".repeat(MAX_VALUE_BYTES - prefix.len()));
        let reader =
            FixtureReader::with_values(Some((RegistryValueType::String, value.as_str())), None);

        let snapshot = collect_snapshot(&reader, 10, 20);
        let entry = &snapshot.entries[0];

        assert_eq!(entry.result, SnapshotResult::Present);
        assert!(entry.character_count <= MAX_VALUE_BYTES);
        assert!(!format!("{snapshot:?}").contains('\t'));
        assert!(!format!("{snapshot:?}").contains('\r'));
        assert!(!format!("{snapshot:?}").contains('\n'));
    }

    #[test]
    fn collect_snapshot_rejects_values_over_the_input_bound() {
        let value = format!("{}\0ignored", "x".repeat(MAX_VALUE_BYTES + 1));
        let reader =
            FixtureReader::with_values(Some((RegistryValueType::String, value.as_str())), None);

        let snapshot = collect_snapshot(&reader, 10, 20);

        assert_eq!(
            snapshot.entries[0].result,
            SnapshotResult::Failed {
                category: "value_too_large".to_owned(),
                win32_code: None,
            }
        );
        assert_eq!(snapshot.entries[0].character_count, 0);
        assert!(snapshot.entries[0].digest.is_empty());
    }

    #[test]
    fn collect_snapshot_toml_contains_metadata_without_registry_strings() {
        let shell_value = "explorer.exe --secret";
        let userinit_value = "%SystemRoot%\\system32\\userinit.exe --secret";
        let reader = FixtureReader::with_values(
            Some((RegistryValueType::String, shell_value)),
            Some((RegistryValueType::ExpandString, userinit_value)),
        );

        let snapshot = collect_snapshot(&reader, 10, 20);
        let serialized = toml::to_string(&snapshot).expect("snapshot should serialize as TOML");

        assert!(!serialized.contains(shell_value));
        assert!(!serialized.contains(userinit_value));
        assert!(serialized.contains("format_version"));
        assert!(serialized.contains("captured_at_unix_seconds"));
        assert!(serialized.contains("process_id"));
        assert!(serialized.contains("target"));
        assert!(serialized.contains("value_type"));
        assert!(serialized.contains("result"));
        assert!(serialized.contains("character_count"));
        assert!(serialized.contains("digest"));
    }
}
