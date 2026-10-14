use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{RegistryType, RegistryValue, SwitchError, SwitchErrorKind, ValidatedInstall};

pub const MAX_REGISTRY_VALUE_BYTES: usize = 4096;
pub const INSTALL_FORMAT_VERSION: u32 = 1;
pub const INSTALL_DIRECTORY_NAME: &str = "RustNT";
pub const HELPER_EXE_NAME: &str = "rustnt-shell-helper.exe";
pub const BOOTSTRAP_EXE_NAME: &str = "rustnt-shell-bootstrap.exe";
pub const GUI_EXE_NAME: &str = "rustnt-gui.exe";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WinlogonValue {
    Shell,
    Userinit,
}

impl WinlogonValue {
    pub const fn value_name(self) -> &'static str {
        match self {
            Self::Shell => "Shell",
            Self::Userinit => "Userinit",
        }
    }
}

pub trait WinlogonStore {
    fn read(&self, value: WinlogonValue) -> Result<Option<RegistryValue>, SwitchError>;
    fn write(&self, value: WinlogonValue, data: &RegistryValue) -> Result<(), SwitchError>;
}

pub fn validate_registry_string(
    value: &RegistryValue,
    max_bytes: usize,
) -> Result<String, SwitchError> {
    if value.value_type != RegistryType::String {
        return Err(error(SwitchErrorKind::UnsupportedRegistryType));
    }
    let raw = &value.raw;
    if raw.is_empty() || raw.len() > max_bytes || !raw.len().is_multiple_of(2) {
        return Err(error(SwitchErrorKind::InvalidRegistryString));
    }

    let units = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[low, high]| u16::from_le_bytes([*low, *high]))
        .collect::<Vec<_>>();
    if units.len() < 2 || units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
        return Err(error(SwitchErrorKind::InvalidRegistryString));
    }
    String::from_utf16(&units[..units.len() - 1])
        .map_err(|_| error(SwitchErrorKind::InvalidRegistryString))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallManifest {
    pub format_version: u32,
    pub helper: String,
    pub bootstrap: String,
    pub gui: String,
}

impl InstallManifest {
    pub fn fixed() -> Self {
        Self {
            format_version: INSTALL_FORMAT_VERSION,
            helper: HELPER_EXE_NAME.to_owned(),
            bootstrap: BOOTSTRAP_EXE_NAME.to_owned(),
            gui: GUI_EXE_NAME.to_owned(),
        }
    }
}

pub fn verify_install_manifest(
    program_files: &Path,
    manifest: &InstallManifest,
) -> Result<ValidatedInstall, SwitchError> {
    if manifest.format_version != INSTALL_FORMAT_VERSION
        || manifest.helper != HELPER_EXE_NAME
        || manifest.bootstrap != BOOTSTRAP_EXE_NAME
        || manifest.gui != GUI_EXE_NAME
    {
        return Err(error(SwitchErrorKind::InvalidManifest));
    }

    let program_files = program_files
        .canonicalize()
        .map_err(|_| error(SwitchErrorKind::InvalidInstall))?;
    let root = program_files.join(INSTALL_DIRECTORY_NAME);
    let root_metadata =
        std::fs::symlink_metadata(&root).map_err(|_| error(SwitchErrorKind::InvalidInstall))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(error(SwitchErrorKind::InvalidInstall));
    }
    let root = root
        .canonicalize()
        .map_err(|_| error(SwitchErrorKind::InvalidInstall))?;
    if root.parent() != Some(program_files.as_path()) {
        return Err(error(SwitchErrorKind::InvalidInstall));
    }

    let helper = installed_executable(&root, HELPER_EXE_NAME)?;
    let bootstrap = installed_executable(&root, BOOTSTRAP_EXE_NAME)?;
    let gui = installed_executable(&root, GUI_EXE_NAME)?;
    Ok(ValidatedInstall {
        root,
        helper,
        bootstrap,
        gui,
    })
}

fn installed_executable(root: &Path, name: &str) -> Result<PathBuf, SwitchError> {
    let path = root.join(name);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| error(SwitchErrorKind::InvalidInstall))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(error(SwitchErrorKind::InvalidInstall));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| error(SwitchErrorKind::InvalidInstall))?;
    if canonical.parent() != Some(root) {
        return Err(error(SwitchErrorKind::InvalidInstall));
    }
    Ok(canonical)
}

const fn error(kind: SwitchErrorKind) -> SwitchError {
    SwitchError {
        kind,
        win32_code: None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn registry_string_validation_rejects_expand_string_and_embedded_nul() {
        let expand = RegistryValue::new(
            RegistryType::ExpandString,
            utf16z(r"%SystemRoot%\explorer.exe"),
        );
        assert_eq!(
            validate_registry_string(&expand, MAX_REGISTRY_VALUE_BYTES)
                .unwrap_err()
                .kind(),
            SwitchErrorKind::UnsupportedRegistryType
        );
        let embedded = RegistryValue::new(RegistryType::String, vec![b'e', 0, 0, 0, 0, 0]);
        assert_eq!(
            validate_registry_string(&embedded, MAX_REGISTRY_VALUE_BYTES)
                .unwrap_err()
                .kind(),
            SwitchErrorKind::InvalidRegistryString
        );
    }

    #[test]
    fn registry_string_validation_accepts_a_terminated_utf16_string() {
        let value = RegistryValue::new(RegistryType::String, utf16z("explorer.exe"));
        assert_eq!(
            validate_registry_string(&value, MAX_REGISTRY_VALUE_BYTES).unwrap(),
            "explorer.exe"
        );
    }

    #[test]
    fn registry_string_validation_rejects_malformed_or_oversized_data() {
        for raw in [
            vec![b'a', 0],
            vec![b'a', 0, 0],
            vec![0, 0],
            vec![0x00, 0xd8, 0, 0],
        ] {
            let value = RegistryValue::new(RegistryType::String, raw);
            assert_eq!(
                validate_registry_string(&value, MAX_REGISTRY_VALUE_BYTES)
                    .unwrap_err()
                    .kind(),
                SwitchErrorKind::InvalidRegistryString
            );
        }
        let too_long = RegistryValue::new(RegistryType::String, utf16z("long"));
        assert_eq!(
            validate_registry_string(&too_long, 2).unwrap_err().kind(),
            SwitchErrorKind::InvalidRegistryString
        );
    }

    #[test]
    fn registry_string_validation_rejects_unknown_registry_type() {
        let value = RegistryValue::new(RegistryType::Other(7), utf16z("explorer.exe"));
        assert_eq!(
            validate_registry_string(&value, MAX_REGISTRY_VALUE_BYTES)
                .unwrap_err()
                .kind(),
            SwitchErrorKind::UnsupportedRegistryType
        );
    }

    #[test]
    fn winlogon_values_have_only_the_fixed_value_names() {
        assert_eq!(WinlogonValue::Shell.value_name(), "Shell");
        assert_eq!(WinlogonValue::Userinit.value_name(), "Userinit");
    }

    #[test]
    fn install_manifest_verification_requires_fixed_files_under_install_root() {
        let temp = TestDirectory::new();
        let program_files = temp.path().join("Program Files");
        let install = program_files.join(INSTALL_DIRECTORY_NAME);
        fs::create_dir_all(&install).unwrap();
        for name in [HELPER_EXE_NAME, BOOTSTRAP_EXE_NAME, GUI_EXE_NAME] {
            fs::write(install.join(name), b"test").unwrap();
        }

        let validated = verify_install_manifest(&program_files, &InstallManifest::fixed()).unwrap();
        assert_eq!(validated.root(), install.canonicalize().unwrap());
        assert_eq!(validated.helper().file_name().unwrap(), HELPER_EXE_NAME);

        let mut unknown = InstallManifest::fixed();
        unknown.gui = "..\\outside.exe".into();
        assert_eq!(
            verify_install_manifest(&program_files, &unknown)
                .unwrap_err()
                .kind(),
            SwitchErrorKind::InvalidManifest
        );
    }

    #[test]
    fn install_manifest_verification_rejects_missing_files_and_symlinked_executables() {
        let temp = TestDirectory::new();
        let program_files = temp.path().join("Program Files");
        let install = program_files.join(INSTALL_DIRECTORY_NAME);
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join(HELPER_EXE_NAME), b"test").unwrap();
        assert_eq!(
            verify_install_manifest(&program_files, &InstallManifest::fixed())
                .unwrap_err()
                .kind(),
            SwitchErrorKind::InvalidInstall
        );

        let elsewhere = temp.path().join("gui.exe");
        fs::write(&elsewhere, b"test").unwrap();
        fs::write(install.join(BOOTSTRAP_EXE_NAME), b"test").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, install.join(GUI_EXE_NAME)).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&elsewhere, install.join(GUI_EXE_NAME)).unwrap();
        assert_eq!(
            verify_install_manifest(&program_files, &InstallManifest::fixed())
                .unwrap_err()
                .kind(),
            SwitchErrorKind::InvalidInstall
        );
    }

    #[test]
    fn install_manifest_verification_rejects_symlinked_install_root() {
        let temp = TestDirectory::new();
        let program_files = temp.path().join("Program Files");
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir_all(&program_files).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        for name in [HELPER_EXE_NAME, BOOTSTRAP_EXE_NAME, GUI_EXE_NAME] {
            fs::write(elsewhere.join(name), b"test").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, program_files.join(INSTALL_DIRECTORY_NAME)).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&elsewhere, program_files.join(INSTALL_DIRECTORY_NAME))
            .unwrap();
        assert_eq!(
            verify_install_manifest(&program_files, &InstallManifest::fixed())
                .unwrap_err()
                .kind(),
            SwitchErrorKind::InvalidInstall
        );
    }

    #[test]
    fn install_manifest_deserialization_rejects_unknown_fields() {
        let source = format!(
            "format_version = 1\nhelper = \"{HELPER_EXE_NAME}\"\nbootstrap = \"{BOOTSTRAP_EXE_NAME}\"\ngui = \"{GUI_EXE_NAME}\"\nextra = true\n"
        );
        assert!(toml::from_str::<InstallManifest>(&source).is_err());
    }

    fn utf16z(value: &str) -> Vec<u8> {
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("rustnt-shell-switch-test-{id}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
