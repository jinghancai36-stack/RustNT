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
    save_snapshot_with_replacer(paths, snapshot, replace_snapshot_file)
}

/// Private write chain with an injectable replacement step.
///
/// Production always passes `replace_snapshot_file`. The seam exists solely so
/// that deterministic fault-injection tests in this file can simulate Win32
/// failure states without touching the real API or relying on random 1175.
fn save_snapshot_with_replacer(
    paths: &ConfigPaths,
    snapshot: &ShellSnapshot,
    replacer: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), SnapshotError> {
    if snapshot.format_version != FORMAT_VERSION {
        return Err(SnapshotError::new(
            "validate snapshot",
            format!(
                "unsupported snapshot format: version {}",
                snapshot.format_version
            ),
        ));
    }
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
    let target = &paths.shell_snapshot_file;
    if let Err(error) = replacer(&temporary, target) {
        // State-aware cleanup: keep the temporary file unless removing it is
        // proven to lose no data. Never turn a failed replace into success.
        let assessment = observe_failure_state(target, &temporary, &encoded);
        let decision = decide_cleanup(&assessment);
        if decision == CleanupDecision::Remove {
            let _ = std::fs::remove_file(&temporary);
        }
        return Err(replace_snapshot_error(
            target,
            &temporary,
            &error,
            &assessment,
            decision,
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactState {
    Missing,
    Complete,
    Incomplete,
    Unverifiable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupDecision {
    Keep(&'static str),
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FailureAssessment {
    target: ArtifactState,
    temporary: ArtifactState,
    identical_bytes: bool,
}

fn decide_cleanup(assessment: &FailureAssessment) -> CleanupDecision {
    if assessment.target == ArtifactState::Missing {
        return CleanupDecision::Keep("target is missing; temporary may be the only complete copy");
    }
    if assessment.target == ArtifactState::Unverifiable
        || assessment.temporary == ArtifactState::Unverifiable
    {
        return CleanupDecision::Keep("file state cannot be verified reliably");
    }
    if assessment.target == ArtifactState::Incomplete
        || assessment.temporary == ArtifactState::Incomplete
    {
        return CleanupDecision::Keep("an artifact is not verifiably complete");
    }
    if assessment.identical_bytes {
        // The only proven-safe removal: both files hold identical complete
        // bytes, so deleting the temporary copy loses no data at all.
        return CleanupDecision::Remove;
    }
    // Default is conservative: preserve complete artifacts for diagnostics.
    CleanupDecision::Keep("complete artifacts are preserved for diagnostics")
}

fn observe_failure_state(target: &Path, temporary: &Path, encoded: &str) -> FailureAssessment {
    let (target_state, target_bytes) = observe_artifact(target, None);
    let (temporary_state, temporary_bytes) = observe_artifact(temporary, Some(encoded.as_bytes()));
    let identical_bytes = match (target_bytes, temporary_bytes) {
        (Some(target_bytes), Some(temporary_bytes)) => target_bytes == temporary_bytes,
        _ => false,
    };
    FailureAssessment {
        target: target_state,
        temporary: temporary_state,
        identical_bytes,
    }
}

/// Presence is decided from metadata plus a directory enumeration, NEVER from a
/// single failed `fs::read`: an exclusive external handle can make a read fail
/// while the file still exists (Task 01B.5C-B Gate A observation-bias finding).
fn observe_artifact(path: &Path, expected: Option<&[u8]>) -> (ArtifactState, Option<Vec<u8>>) {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match directory_contains(path) {
                Some(false) => (ArtifactState::Missing, None),
                // Directory listing disagrees with metadata, or cannot be read:
                // treat the state as unverifiable rather than "missing".
                _ => (ArtifactState::Unverifiable, None),
            };
        }
        Err(_) => return (ArtifactState::Unverifiable, None),
    }
    match read_artifact_bytes(path) {
        Ok(bytes) => {
            let complete = match expected {
                Some(expected_bytes) => bytes == expected_bytes,
                None => is_valid_snapshot_bytes(&bytes),
            };
            let state = if complete {
                ArtifactState::Complete
            } else {
                ArtifactState::Incomplete
            };
            (state, Some(bytes))
        }
        // A failed read means the state is unverifiable, NOT that the file is gone.
        Err(_) => (ArtifactState::Unverifiable, None),
    }
}

fn directory_contains(path: &Path) -> Option<bool> {
    let parent = path.parent()?;
    let name = path.file_name()?;
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        if entry.file_name().as_os_str() == name {
            return Some(true);
        }
    }
    Some(false)
}

fn read_artifact_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(MAX_SNAPSHOT_BYTES + 1);
    file.take((MAX_SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn is_valid_snapshot_bytes(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return false;
    };
    value
        .get("format_version")
        .and_then(toml::Value::as_integer)
        == Some(i64::from(FORMAT_VERSION))
}

fn replace_snapshot_error(
    target: &Path,
    temporary: &Path,
    error: &std::io::Error,
    assessment: &FailureAssessment,
    decision: CleanupDecision,
) -> SnapshotError {
    let win32 = error
        .raw_os_error()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let cleanup = match decision {
        CleanupDecision::Remove => "removed (proven byte-identical, no data loss)".to_owned(),
        CleanupDecision::Keep(reason) => format!("preserved ({reason})"),
    };
    SnapshotError::new(
        "replace snapshot",
        format!(
            "MoveFileExW replacement failed (Windows error {win32}: {error}); \
             target={}; temporary={}; target_state={:?}; temporary_state={:?}; cleanup={cleanup}",
            target.display(),
            temporary.display(),
            assessment.target,
            assessment.temporary
        ),
    )
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
            // Close the handle before removing this provably incomplete artifact.
            drop(file);
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        // Explicitly close the temporary file handle BEFORE the caller replaces
        // the target. No flush(), sync_all() or MOVEFILE_WRITE_THROUGH is
        // introduced here: the pre-fix durability semantics are preserved.
        drop(file);
        return Ok(temporary);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary snapshot path",
    ))
}

#[cfg(not(windows))]
fn replace_snapshot_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    // Non-Windows fallback: a single rename, with no `exists()` decision gate.
    std::fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_snapshot_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

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
    // One call handles BOTH target-present (atomic replace) and target-absent
    // (atomic rename). No `exists()` gate, no pre-delete of the target, no
    // backup file, no MOVEFILE_COPY_ALLOWED (temporary and target share a
    // directory/volume). 1175/1176/1177 are legacy ReplaceFileW partial-failure
    // codes; MoveFileExW has no documented equivalents.
    let moved = unsafe {
        MoveFileExW(
            temporary_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    };
    if moved == 0 {
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
    fn save_rejects_unknown_snapshot_version_without_replacing_existing_file() {
        let root = TestRoot::new("save-version");
        let paths = config_paths_from_root(root.path().to_owned());
        let valid_snapshot = fixture_snapshot();
        save_snapshot(&paths, &valid_snapshot).unwrap();
        let original = std::fs::read(&paths.shell_snapshot_file).unwrap();
        let mut unsupported_snapshot = valid_snapshot;
        unsupported_snapshot.format_version = FORMAT_VERSION + 1;

        let error = save_snapshot(&paths, &unsupported_snapshot)
            .expect_err("unknown snapshot version must be rejected");
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

    // =====================================================================
    // Task 01B.5C-B Gate B — deterministic fault-injection regression suite.
    //
    // Three evidence classes are kept strictly separate:
    //   * real MoveFileExW failure   -> `gate_b_real_*` / released-handle tests
    //                                  (no injection at all)
    //   * simulated error state      -> `failing_replacer(...)` closures
    //   * legacy ReplaceFileW codes  -> 1175/1176/1177 are SIMULATED legacy
    //     partial-failure states, never normal MoveFileExW production errors.
    // No Mutex, no retry, no second protocol, no new on-disk format.
    // =====================================================================

    #[derive(Clone, Copy)]
    enum InjectedLayout {
        LeaveIntact,
        RemoveTarget,
        RenameTarget,
    }

    /// Private, deterministic replacement-step injector: it puts the disk into
    /// a chosen documented failure state, then returns a chosen Win32 code.
    fn failing_replacer(
        code: i32,
        layout: InjectedLayout,
    ) -> impl Fn(&Path, &Path) -> std::io::Result<()> {
        move |_temporary, target| {
            match layout {
                InjectedLayout::LeaveIntact => {}
                InjectedLayout::RemoveTarget => {
                    let _ = std::fs::remove_file(target);
                }
                InjectedLayout::RenameTarget => {
                    let renamed = target.with_extension("renamed");
                    let _ = std::fs::remove_file(&renamed);
                    let _ = std::fs::rename(target, &renamed);
                }
            }
            Err(std::io::Error::from_raw_os_error(code))
        }
    }

    fn encoded_snapshot(snapshot: &ShellSnapshot) -> String {
        toml::to_string_pretty(snapshot).expect("fixture must serialize")
    }

    fn temporary_files(root: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("shell_snapshot.toml.") && name.ends_with(".tmp")
            })
            .map(|entry| entry.path())
            .collect()
    }

    fn assert_error_context(error: &SnapshotError, paths: &ConfigPaths, code: i32) {
        assert_eq!(error.operation, "replace snapshot");
        assert!(error.message.contains("MoveFileExW replacement failed"));
        assert!(error.message.contains(&format!("Windows error {code}")));
        assert!(error
            .message
            .contains(&paths.shell_snapshot_file.to_string_lossy().to_string()));
        assert!(error.message.contains("temporary="));
        assert!(error.message.contains("cleanup="));
    }

    fn second_snapshot() -> ShellSnapshot {
        let mut snapshot = fixture_snapshot();
        snapshot.process_id = 5678;
        snapshot.captured_at_unix_seconds = 1_700_000_999;
        snapshot
    }

    #[cfg(windows)]
    fn open_exclusive_handle(path: &Path) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };

        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // share mode 0 = exclusive; a replacement cannot proceed and reads
        // through std::fs::File::open will fail while the handle is held.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE, "CreateFileW must open target");
        handle
    }

    #[cfg(windows)]
    fn close_exclusive_handle(handle: windows_sys::Win32::Foundation::HANDLE) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(handle);
        }
    }

    #[test]
    fn gate_b_first_creation_creates_target_with_single_move() {
        let root = TestRoot::new("gb-first-create");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        assert!(!paths.shell_snapshot_file.exists());
        save_snapshot(&paths, &snapshot).unwrap();
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), snapshot);
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn gate_b_replace_existing_is_atomic() {
        let root = TestRoot::new("gb-replace");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = fixture_snapshot();
        let second = second_snapshot();
        save_snapshot(&paths, &first).unwrap();
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), first);
        let first_bytes = std::fs::read(&paths.shell_snapshot_file).unwrap();
        save_snapshot(&paths, &second).unwrap();
        let second_bytes = std::fs::read(&paths.shell_snapshot_file).unwrap();
        assert_ne!(first_bytes, second_bytes);
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), second);
        assert_eq!(
            second_bytes.as_slice(),
            encoded_snapshot(&second).as_bytes()
        );
        assert!(temporary_files(root.path()).is_empty());
    }

    #[test]
    fn gate_b_temporary_handle_is_released_before_replacement() {
        let root = TestRoot::new("gb-handle");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_snapshot.toml");
        let encoded = encoded_snapshot(&fixture_snapshot());
        let temporary = write_temporary_snapshot(&target, &encoded).unwrap();
        // A real replacement succeeds only if write_temporary_snapshot has
        // explicitly closed the temporary file handle before returning.
        replace_snapshot_file(&temporary, &target).unwrap();
        assert!(!temporary.exists());
        assert_eq!(
            std::fs::read(&target).unwrap().as_slice(),
            encoded.as_bytes()
        );
    }

    #[test]
    fn gate_b_old_target_bytes_remain_intact_after_failed_replace() {
        let root = TestRoot::new("gb-old-intact");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = fixture_snapshot();
        let second = second_snapshot();
        save_snapshot(&paths, &first).unwrap();
        let original = std::fs::read(&paths.shell_snapshot_file).unwrap();
        let error = save_snapshot_with_replacer(
            &paths,
            &second,
            failing_replacer(32, InjectedLayout::LeaveIntact),
        )
        .expect_err("injected failure must be reported");
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), first);
        assert_error_context(&error, &paths, 32);
    }

    #[test]
    fn gate_b_new_temporary_bytes_remain_complete_after_failed_replace() {
        let root = TestRoot::new("gb-new-complete");
        let paths = config_paths_from_root(root.path().to_owned());
        let second = second_snapshot();
        save_snapshot(&paths, &fixture_snapshot()).unwrap();
        save_snapshot_with_replacer(
            &paths,
            &second,
            failing_replacer(5, InjectedLayout::LeaveIntact),
        )
        .expect_err("injected failure must be reported");
        let temps = temporary_files(root.path());
        assert_eq!(
            temps.len(),
            1,
            "exactly one complete temporary artifact must be preserved"
        );
        assert_eq!(
            std::fs::read(&temps[0]).unwrap().as_slice(),
            encoded_snapshot(&second).as_bytes()
        );
    }

    #[test]
    fn gate_b_replace_failure_propagates_error_to_caller() {
        let root = TestRoot::new("gb-propagate");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        let error = save_snapshot_with_replacer(
            &paths,
            &snapshot,
            failing_replacer(32, InjectedLayout::LeaveIntact),
        )
        .expect_err("replacement failure must propagate as Err");
        assert_eq!(error.operation, "replace snapshot");
        assert_error_context(&error, &paths, 32);
    }

    #[test]
    fn gate_b_replace_failure_never_returns_ok() {
        for code in [32, 5, 1175, 1176, 1177] {
            let root = TestRoot::new(&format!("gb-never-ok-{code}"));
            let paths = config_paths_from_root(root.path().to_owned());
            let snapshot = fixture_snapshot();
            save_snapshot(&paths, &snapshot).unwrap();
            let result = save_snapshot_with_replacer(
                &paths,
                &snapshot,
                failing_replacer(code, InjectedLayout::LeaveIntact),
            );
            assert!(
                result.is_err(),
                "simulated Win32 code {code} must not be silently swallowed"
            );
        }
    }

    #[test]
    fn gate_b_target_missing_keeps_only_complete_temporary() {
        let root = TestRoot::new("gb-target-missing");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        save_snapshot(&paths, &snapshot).unwrap();
        // Simulated 1176 terminal state: the replaced file no longer exists.
        let error = save_snapshot_with_replacer(
            &paths,
            &snapshot,
            failing_replacer(1176, InjectedLayout::RemoveTarget),
        )
        .expect_err("injected 1176 must fail");
        assert!(!paths.shell_snapshot_file.exists());
        let temps = temporary_files(root.path());
        assert_eq!(temps.len(), 1, "the only complete copy must survive");
        assert_eq!(
            std::fs::read(&temps[0]).unwrap().as_slice(),
            encoded_snapshot(&snapshot).as_bytes()
        );
        assert!(error.message.contains("target is missing"));
        assert!(error.message.contains("preserved"));
        assert_error_context(&error, &paths, 1176);
    }

    #[cfg(windows)]
    #[test]
    fn gate_b_unverifiable_state_keeps_temporary_and_is_not_missing() {
        let root = TestRoot::new("gb-unverifiable");
        std::fs::create_dir_all(root.path()).unwrap();
        let target = root.path().join("shell_snapshot.toml");
        std::fs::write(&target, "format_version = 1\n").unwrap();
        let handle = open_exclusive_handle(&target);
        // A single failed read under an exclusive handle must NOT be
        // interpreted as "file missing" (Gate A observation-bias finding).
        let (state, bytes) = observe_artifact(&target, None);
        assert_eq!(state, ArtifactState::Unverifiable);
        assert!(bytes.is_none());
        assert_eq!(directory_contains(&target), Some(true));
        close_exclusive_handle(handle);
        let assessment = FailureAssessment {
            target: ArtifactState::Unverifiable,
            temporary: ArtifactState::Complete,
            identical_bytes: false,
        };
        assert!(matches!(
            decide_cleanup(&assessment),
            CleanupDecision::Keep(_)
        ));
    }

    #[test]
    fn gate_b_injected_error_32_has_paths_and_win32_code() {
        let root = TestRoot::new("gb-injected-32");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        let error = save_snapshot_with_replacer(
            &paths,
            &snapshot,
            failing_replacer(32, InjectedLayout::LeaveIntact),
        )
        .expect_err("injected ERROR_SHARING_VIOLATION must fail");
        assert_error_context(&error, &paths, 32);
    }

    #[test]
    fn gate_b_injected_error_5_has_paths_and_win32_code() {
        let root = TestRoot::new("gb-injected-5");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        let error = save_snapshot_with_replacer(
            &paths,
            &snapshot,
            failing_replacer(5, InjectedLayout::LeaveIntact),
        )
        .expect_err("injected ERROR_ACCESS_DENIED must fail");
        assert_error_context(&error, &paths, 5);
    }

    #[test]
    fn gate_b_simulated_legacy_1175_keeps_both_artifacts() {
        // 1175 is a legacy ReplaceFileW ERROR_UNABLE_TO_REMOVE_REPLACED:
        // both files keep their original names and both are complete.
        let root = TestRoot::new("gb-legacy-1175");
        let paths = config_paths_from_root(root.path().to_owned());
        let first = fixture_snapshot();
        let second = second_snapshot();
        save_snapshot(&paths, &first).unwrap();
        let original = std::fs::read(&paths.shell_snapshot_file).unwrap();
        let error = save_snapshot_with_replacer(
            &paths,
            &second,
            failing_replacer(1175, InjectedLayout::LeaveIntact),
        )
        .expect_err("simulated legacy 1175 must fail");
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
        let temps = temporary_files(root.path());
        assert_eq!(temps.len(), 1);
        assert_eq!(
            std::fs::read(&temps[0]).unwrap().as_slice(),
            encoded_snapshot(&second).as_bytes()
        );
        assert_error_context(&error, &paths, 1175);
    }

    #[test]
    fn gate_b_simulated_legacy_1177_keeps_temporary() {
        // 1177 is a legacy ReplaceFileW ERROR_UNABLE_TO_MOVE_REPLACEMENT_2:
        // the replaced file survives under a different, unknown name.
        let root = TestRoot::new("gb-legacy-1177");
        let paths = config_paths_from_root(root.path().to_owned());
        let second = second_snapshot();
        save_snapshot(&paths, &fixture_snapshot()).unwrap();
        let error = save_snapshot_with_replacer(
            &paths,
            &second,
            failing_replacer(1177, InjectedLayout::RenameTarget),
        )
        .expect_err("simulated legacy 1177 must fail");
        assert!(!paths.shell_snapshot_file.exists());
        assert!(root.path().join("shell_snapshot.renamed").exists());
        let temps = temporary_files(root.path());
        assert_eq!(temps.len(), 1);
        assert_eq!(
            std::fs::read(&temps[0]).unwrap().as_slice(),
            encoded_snapshot(&second).as_bytes()
        );
        assert_error_context(&error, &paths, 1177);
    }

    #[test]
    fn gate_b_toml_v1_round_trip_is_unchanged() {
        let root = TestRoot::new("gb-toml-v1");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        save_snapshot(&paths, &snapshot).unwrap();
        let text = std::fs::read_to_string(&paths.shell_snapshot_file).unwrap();
        assert_eq!(FORMAT_VERSION, 1);
        assert!(text.contains("format_version = 1"));
        for field in [
            "captured_at_unix_seconds",
            "process_id",
            "entries",
            "target",
            "value_type",
            "result",
            "character_count",
            "digest",
        ] {
            assert!(text.contains(field), "missing TOML v1 field {field}");
        }
        assert_eq!(load_snapshot(&paths).unwrap().unwrap(), snapshot);
    }

    #[test]
    fn gate_b_snapshot_failure_is_error_not_fatal_or_success() {
        // main.rs:191-207 is intentionally unchanged: an Err is downgraded to
        // a Warning notice (shell_recovery.rs:162-168), never propagated with
        // `?` and never a panic. This asserts the contract that enables it.
        let root = TestRoot::new("gb-not-fatal");
        let paths = config_paths_from_root(root.path().to_owned());
        let error = save_snapshot_with_replacer(
            &paths,
            &fixture_snapshot(),
            failing_replacer(5, InjectedLayout::LeaveIntact),
        )
        .expect_err("failure must be returned, not swallowed");
        let _: &dyn std::error::Error = &error;
        assert_eq!(error.operation, "replace snapshot");
        assert!(!error.message.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn gate_b_real_exclusive_handle_conflict_keeps_temporary() {
        // REAL MoveFileExW failure: no injection. An external exclusive handle
        // makes the replacement fail with a genuine Win32 error.
        let root = TestRoot::new("gb-real-handle");
        let paths = config_paths_from_root(root.path().to_owned());
        save_snapshot(&paths, &fixture_snapshot()).unwrap();
        let handle = open_exclusive_handle(&paths.shell_snapshot_file);
        let error = save_snapshot(&paths, &second_snapshot())
            .expect_err("real MoveFileExW failure with an exclusive external handle");
        close_exclusive_handle(handle);
        // Emitted so host/VM validation scripts can record the real Win32 code,
        // both paths, file states and the cleanup decision.
        eprintln!("[gate-b-real-conflict] {}", error.message);
        assert_eq!(error.operation, "replace snapshot");
        assert!(error.message.contains("MoveFileExW replacement failed"));
        assert!(error
            .message
            .contains(&paths.shell_snapshot_file.to_string_lossy().to_string()));
        assert!(
            !temporary_files(root.path()).is_empty(),
            "the complete temporary artifact must be preserved on a real failure"
        );
    }

    #[test]
    fn gate_b_cleanup_decision_matrix_is_conservative() {
        let cases = [
            (
                ArtifactState::Missing,
                ArtifactState::Complete,
                false,
                false,
            ),
            (
                ArtifactState::Unverifiable,
                ArtifactState::Complete,
                false,
                false,
            ),
            (
                ArtifactState::Complete,
                ArtifactState::Unverifiable,
                false,
                false,
            ),
            (
                ArtifactState::Incomplete,
                ArtifactState::Complete,
                false,
                false,
            ),
            (
                ArtifactState::Complete,
                ArtifactState::Incomplete,
                false,
                false,
            ),
            (
                ArtifactState::Complete,
                ArtifactState::Complete,
                false,
                false,
            ),
            // Only proven byte-identical complete copies may be removed.
            (ArtifactState::Complete, ArtifactState::Complete, true, true),
        ];
        for (target, temporary, identical_bytes, removable) in cases {
            let assessment = FailureAssessment {
                target,
                temporary,
                identical_bytes,
            };
            let decision = decide_cleanup(&assessment);
            assert_eq!(
                decision == CleanupDecision::Remove,
                removable,
                "unexpected cleanup decision for {assessment:?}: {decision:?}"
            );
        }
    }

    #[test]
    fn gate_b_proven_redundant_artifacts_are_removed() {
        // The narrow, provably safe removal: saving the very same snapshot
        // twice makes target and temporary byte-identical.
        let root = TestRoot::new("gb-redundant");
        let paths = config_paths_from_root(root.path().to_owned());
        let snapshot = fixture_snapshot();
        save_snapshot(&paths, &snapshot).unwrap();
        let original = std::fs::read(&paths.shell_snapshot_file).unwrap();
        let error = save_snapshot_with_replacer(
            &paths,
            &snapshot,
            failing_replacer(32, InjectedLayout::LeaveIntact),
        )
        .expect_err("injected failure must be reported");
        assert!(error.message.contains("removed (proven byte-identical"));
        assert!(temporary_files(root.path()).is_empty());
        assert_eq!(std::fs::read(&paths.shell_snapshot_file).unwrap(), original);
    }

    #[test]
    fn gate_b_create_and_replace_loop_leaves_no_residue() {
        // First creation followed by 50 atomic replacements on the same target.
        let root = TestRoot::new("gb-create-replace-loop");
        let paths = config_paths_from_root(root.path().to_owned());
        for iteration in 0..50_u32 {
            let mut snapshot = fixture_snapshot();
            snapshot.process_id = 1000 + iteration;
            save_snapshot(&paths, &snapshot).unwrap();
            assert_eq!(load_snapshot(&paths).unwrap().unwrap(), snapshot);
            assert!(
                temporary_files(root.path()).is_empty(),
                "temporary residue at iteration {iteration}"
            );
        }
    }
}
