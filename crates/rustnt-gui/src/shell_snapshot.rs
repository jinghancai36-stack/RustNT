use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::ConfigPaths;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_VALUE_BYTES: usize = 4096;
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024;

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ShellValueTarget {
    Shell,
    Userinit,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
fn unsupported_type(_value_type: u32) -> RegistryReadError {
    RegistryReadError {
        category: "unsupported_type".to_owned(),
        win32_code: None,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SnapshotResult {
    Present,
    Missing,
    Failed {
        category: String,
        win32_code: Option<u32>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellValueSnapshot {
    pub target: ShellValueTarget,
    pub value_type: Option<RegistryValueType>,
    pub result: SnapshotResult,
    pub character_count: usize,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShellSnapshot {
    pub format_version: u32,
    pub captured_at_unix_seconds: u64,
    pub process_id: u32,
    pub entries: Vec<ShellValueSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotError {
    pub operation: String,
    pub message: String,
}

impl SnapshotError {
    fn new(operation: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation: operation.into(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for SnapshotError {}

pub fn save_snapshot(paths: &ConfigPaths, snapshot: &ShellSnapshot) -> Result<(), SnapshotError> {
    std::fs::create_dir_all(&paths.root)
        .map_err(|error| SnapshotError::new("create snapshot directory", error))?;
    let encoded = toml::to_string_pretty(snapshot)
        .map_err(|error| SnapshotError::new("serialize snapshot", error))?;
    if encoded.len() > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::new(
            "serialize snapshot",
            format!("persistent file exceeds {MAX_SNAPSHOT_BYTES} bytes"),
        ));
    }
    let temporary = write_temporary_snapshot(&paths.shell_snapshot_file, &encoded)
        .map_err(|error| SnapshotError::new("write temporary snapshot", error))?;
    if let Err(error) = replace_snapshot_file(&temporary, &paths.shell_snapshot_file) {
        let _ = std::fs::remove_file(&temporary);
        return Err(SnapshotError::new("replace snapshot", error));
    }
    Ok(())
}

pub fn load_snapshot(paths: &ConfigPaths) -> Result<Option<ShellSnapshot>, SnapshotError> {
    let Some(bytes) = read_bounded_snapshot(&paths.shell_snapshot_file)? else {
        return Ok(None);
    };
    let encoded = String::from_utf8(bytes)
        .map_err(|error| SnapshotError::new("read snapshot", format!("invalid UTF-8: {error}")))?;
    let value = toml::from_str::<toml::Value>(&encoded).map_err(|error| {
        SnapshotError::new("load snapshot", format!("invalid snapshot: {error}"))
    })?;
    let version = value
        .get("format_version")
        .and_then(toml::Value::as_integer)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            SnapshotError::new("load snapshot", "invalid snapshot: missing format_version")
        })?;
    if version != FORMAT_VERSION {
        return Err(SnapshotError::new(
            "load snapshot",
            format!("unsupported snapshot format: version {version}"),
        ));
    }
    toml::from_str(&encoded)
        .map(Some)
        .map_err(|error| SnapshotError::new("load snapshot", format!("invalid snapshot: {error}")))
}

fn read_bounded_snapshot(path: &Path) -> Result<Option<Vec<u8>>, SnapshotError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SnapshotError::new("read snapshot", error)),
    };
    let mut bytes = Vec::with_capacity(MAX_SNAPSHOT_BYTES + 1);
    file.take((MAX_SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| SnapshotError::new("read snapshot", error))?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(SnapshotError::new(
            "read snapshot",
            format!("persistent file exceeds {MAX_SNAPSHOT_BYTES} bytes"),
        ));
    }
    Ok(Some(bytes))
}

fn temporary_snapshot_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shell_snapshot.toml");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        "{name}.{}.{}.{}.tmp",
        std::process::id(),
        timestamp,
        counter
    ))
}

fn write_temporary_snapshot(target: &Path, encoded: &str) -> std::io::Result<PathBuf> {
    for _ in 0..16 {
        let temporary = temporary_snapshot_path(target);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(encoded.as_bytes()) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        return Ok(temporary);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary snapshot path",
    ))
}

#[cfg(not(windows))]
fn replace_snapshot_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_snapshot_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    if !target.exists() {
        return match std::fs::rename(temporary, target) {
            Ok(()) => Ok(()),
            Err(_error) if target.exists() => replace_existing_snapshot(temporary, target),
            Err(error) => Err(error),
        };
    }
    replace_existing_snapshot(temporary, target)
}

#[cfg(windows)]
fn replace_existing_snapshot(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            target_wide.as_ptr(),
            temporary_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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
    use crate::config::config_paths_from_root;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    #[test]
    fn windows_reader_returns_typed_results_for_both_targets() {
        let reader = WindowsShellReader::new();
        let shell = reader.read(ShellValueTarget::Shell);
        let userinit = reader.read(ShellValueTarget::Userinit);
        assert!(shell.is_ok() || shell.as_ref().is_err_and(|e| e.win32_code.is_some()));
        assert!(userinit.is_ok() || userinit.as_ref().is_err_and(|e| e.win32_code.is_some()));
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_type_does_not_store_registry_type_as_win32_code() {
        assert_eq!(unsupported_type(1234).win32_code, None);
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

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("rustnt-gui-shell-snapshot-{name}-{unique}")))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_snapshot() -> ShellSnapshot {
        ShellSnapshot {
            format_version: FORMAT_VERSION,
            captured_at_unix_seconds: 1_700_000_000,
            process_id: 1234,
            entries: vec![
                ShellValueSnapshot {
                    target: ShellValueTarget::Shell,
                    value_type: Some(RegistryValueType::String),
                    result: SnapshotResult::Present,
                    character_count: 12,
                    digest: "fnv1a64:0123456789abcdef".to_owned(),
                },
                ShellValueSnapshot {
                    target: ShellValueTarget::Userinit,
                    value_type: Some(RegistryValueType::ExpandString),
                    result: SnapshotResult::Missing,
                    character_count: 0,
                    digest: String::new(),
                },
            ],
        }
    }

    fn assert_no_temporary_snapshots(paths: &crate::config::ConfigPaths) {
        let entries = std::fs::read_dir(&paths.root).unwrap();
        assert!(entries.filter_map(Result::ok).all(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with("shell_snapshot.toml.")
                || !entry.file_name().to_string_lossy().ends_with(".tmp")
        }));
    }

    #[test]
    fn save_and_load_snapshot_round_trip_without_raw_values() {
        let root = TestRoot::new("round-trip");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        save_snapshot(&paths, &snapshot).unwrap();
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), snapshot);
        let encoded = std::fs::read_to_string(&paths.shell_snapshot_file).unwrap();
        assert!(!encoded.contains("explorer.exe"));
        assert_no_temporary_snapshots(&paths);
    }

    #[test]
    fn saving_snapshot_twice_replaces_existing_file_and_cleans_temporary_file() {
        let root = TestRoot::new("overwrite");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = fixture_snapshot();
        let mut second = fixture_snapshot();
        second.process_id = 5678;

        save_snapshot(&paths, &first).unwrap();
        save_snapshot(&paths, &second).unwrap();

        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), second);
        assert_no_temporary_snapshots(&paths);
    }

    #[test]
    fn missing_snapshot_returns_none_without_creating_directory() {
        let root = TestRoot::new("missing");
        let paths = config_paths_from_root(root.path().to_owned());

        assert_eq!(load_snapshot(&paths).unwrap(), None);
        assert!(!root.path().exists());
    }

    #[test]
    fn malformed_snapshot_is_reported_without_deleting_file() {
        let root = TestRoot::new("malformed");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = "this is not valid toml =";
        std::fs::write(&paths.shell_snapshot_file, original).unwrap();

        let error = load_snapshot(&paths).expect_err("malformed snapshot must fail");
        assert!(error.message.contains("invalid snapshot"));
        assert_eq!(
            std::fs::read_to_string(&paths.shell_snapshot_file).unwrap(),
            original
        );
    }

    #[test]
    fn unknown_snapshot_version_is_reported_without_deleting_file() {
        let root = TestRoot::new("version");
        let paths = config_paths_from_root(root.path().to_owned());
        std::fs::create_dir_all(root.path()).unwrap();
        let original = b"format_version = 99\n";
        std::fs::write(&paths.shell_snapshot_file, original).unwrap();

        let error = load_snapshot(&paths).expect_err("unknown version must fail");
        assert!(error.message.contains("unsupported snapshot format"));
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
    }

    #[test]
    fn oversized_snapshot_is_rejected_without_deleting_file() {
        let root = TestRoot::new("oversized-load");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = vec![b'x'; MAX_SNAPSHOT_BYTES + 1];
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_snapshot_file, &original).unwrap();

        let error = load_snapshot(&paths).expect_err("oversized snapshot must fail");
        assert!(error.message.contains("exceeds"));
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
    }

    #[test]
    fn oversized_serialized_snapshot_is_rejected_before_replacement() {
        let root = TestRoot::new("oversized-save");
        let paths = config_paths_from_root(root.path().to_owned());
        let mut snapshot = fixture_snapshot();
        snapshot.entries = (0..512)
            .map(|index| ShellValueSnapshot {
                target: if index % 2 == 0 {
                    ShellValueTarget::Shell
                } else {
                    ShellValueTarget::Userinit
                },
                value_type: Some(RegistryValueType::Other(index)),
                result: SnapshotResult::Failed {
                    category: "bounded-error".to_owned(),
                    win32_code: Some(index),
                },
                character_count: 0,
                digest: "x".repeat(128),
            })
            .collect();

        let error = save_snapshot(&paths, &snapshot).expect_err("oversized snapshot must fail");
        assert!(error.message.contains("exceeds"));
        assert!(!paths.shell_snapshot_file.exists());
    }

    #[test]
    fn save_snapshot_failure_preserves_existing_file_and_cleans_temporary() {
        let root = TestRoot::new("save-failure");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        save_snapshot(&paths, &snapshot).unwrap();
        let original = std::fs::read(&paths.shell_snapshot_file).unwrap();

        let invalid_root = root.path().join("invalid-root");
        std::fs::write(&invalid_root, b"not a directory").unwrap();
        let mut failing_paths = paths.clone();
        failing_paths.root = invalid_root;

        let error = save_snapshot(&failing_paths, &snapshot).expect_err("save must fail");
        assert_eq!(error.operation, "create snapshot directory");
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
        assert_no_temporary_snapshots(&paths);
    }

    #[test]
    fn replace_snapshot_failure_preserves_existing_target_bytes() {
        let root = TestRoot::new("replace-failure");
        let paths = config_paths_from_root(root.path().to_owned());
        let original = b"existing snapshot bytes\n";
        let temporary = root.path().join("missing.tmp");
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(&paths.shell_snapshot_file, original).unwrap();

        let error = replace_snapshot_file(&temporary, &paths.shell_snapshot_file)
            .expect_err("missing temporary snapshot must fail");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
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
