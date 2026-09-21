use std::ffi::c_void;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{GetLastError, FILETIME};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesExW, GetFileExInfoStandard, GetFullPathNameW, WIN32_FILE_ATTRIBUTE_DATA,
    FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    ReparsePoint,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    pub path: String,
    pub kind: FileKind,
    pub size_bytes: Option<u64>,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub attributes: u32,
    pub is_reparse_point: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSystemError {
    InvalidPath(String),
    NotFound(String),
    AccessDenied(String),
    Io(String),
    Win32 { operation: String, code: u32 },
}

pub fn stat_path(path: &str) -> Result<FileMetadata, FileSystemError> {
    let (normalized, wide_path) = normalize_path(path)?;
    let mut data = WIN32_FILE_ATTRIBUTE_DATA {
        ..unsafe { std::mem::zeroed() }
    };
    let ok = unsafe {
        // SAFETY: wide_path is NUL-terminated and data points to writable storage for the API.
        GetFileAttributesExW(
            wide_path.as_ptr(),
            GetFileExInfoStandard,
            &mut data as *mut _ as *mut c_void,
        )
    };
    if ok == 0 {
        return Err(error_from_last_error("read file attributes", &normalized));
    }
    Ok(metadata_from_attributes(normalized, &data))
}

fn path_to_wide(path: &str) -> Result<Vec<u16>, FileSystemError> {
    if path.is_empty() || path.encode_utf16().any(|unit| unit == 0) {
        return Err(FileSystemError::InvalidPath(path.to_string()));
    }
    let mut wide: Vec<u16> = path.encode_utf16().collect();
    wide.push(0);
    Ok(wide)
}

fn normalize_path(path: &str) -> Result<(String, Vec<u16>), FileSystemError> {
    let input = path_to_wide(path)?;
    let mut capacity = 260usize;
    loop {
        let mut buffer = vec![0u16; capacity];
        let length = unsafe {
            // SAFETY: input and buffer are valid NUL-terminated/writable UTF-16 buffers.
            GetFullPathNameW(
                input.as_ptr(),
                buffer.len() as u32,
                buffer.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if length == 0 {
            return Err(error_from_last_error("normalize path", path));
        }
        if (length as usize) < buffer.len() {
            buffer.truncate(length as usize);
            let normalized = String::from_utf16_lossy(&buffer);
            buffer.push(0);
            return Ok((normalized, buffer));
        }
        capacity = (length as usize).checked_add(1).ok_or_else(|| {
            FileSystemError::InvalidPath(path.to_string())
        })?;
    }
}

fn file_kind_from_attributes(attributes: u32) -> FileKind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        FileKind::ReparsePoint
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        FileKind::Directory
    } else if attributes & FILE_ATTRIBUTE_DEVICE != 0 {
        FileKind::Other
    } else {
        FileKind::File
    }
}

fn filetime_to_system_time(filetime: FILETIME) -> Option<SystemTime> {
    const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116444736000000000;
    let ticks = (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime);
    let unix_ticks = ticks.checked_sub(WINDOWS_TO_UNIX_EPOCH_100NS)?;
    let seconds = unix_ticks / 10_000_000;
    let nanos = (unix_ticks % 10_000_000) * 100;
    UNIX_EPOCH.checked_add(Duration::new(seconds, nanos as u32))
}

fn metadata_from_attributes(path: String, data: &WIN32_FILE_ATTRIBUTE_DATA) -> FileMetadata {
    let kind = file_kind_from_attributes(data.dwFileAttributes);
    let size_bytes = if kind == FileKind::Directory {
        None
    } else {
        Some((u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow))
    };
    FileMetadata {
        path,
        kind: kind.clone(),
        size_bytes,
        created: filetime_to_system_time(data.ftCreationTime),
        modified: filetime_to_system_time(data.ftLastWriteTime),
        accessed: filetime_to_system_time(data.ftLastAccessTime),
        attributes: data.dwFileAttributes,
        is_reparse_point: kind == FileKind::ReparsePoint,
    }
}

fn error_from_last_error(operation: &str, path: &str) -> FileSystemError {
    let code = unsafe {
        // SAFETY: GetLastError reads the calling thread's Win32 error value.
        GetLastError()
    };
    match code {
        2 | 3 => FileSystemError::NotFound(path.to_string()),
        5 => FileSystemError::AccessDenied(path.to_string()),
        123 | 87 => FileSystemError::InvalidPath(path.to_string()),
        _ => FileSystemError::Win32 {
            operation: operation.to_string(),
            code,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        file_kind_from_attributes, filetime_to_system_time, path_to_wide, FileKind,
        FileSystemError,
    };
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    #[test]
    fn wide_path_is_nul_terminated_and_rejects_embedded_nul() {
        assert_eq!(path_to_wide(r"C:\Temp\demo").unwrap().last(), Some(&0));
        assert!(matches!(
            path_to_wide("bad\0path"),
            Err(FileSystemError::InvalidPath(_))
        ));
    }

    #[test]
    fn file_kind_prioritizes_reparse_and_directory_attributes() {
        assert_eq!(
            file_kind_from_attributes(FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY),
            FileKind::ReparsePoint
        );
        assert_eq!(
            file_kind_from_attributes(FILE_ATTRIBUTE_DIRECTORY),
            FileKind::Directory
        );
        assert_eq!(
            file_kind_from_attributes(FILE_ATTRIBUTE_DEVICE),
            FileKind::Other
        );
    }

    #[test]
    fn filetime_conversion_rejects_values_before_unix_epoch() {
        let before_epoch = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        assert_eq!(filetime_to_system_time(before_epoch), None);
    }

    #[test]
    #[cfg(windows)]
    fn stat_path_reads_metadata_from_a_temporary_file() {
        use std::fs::{self, File};
        use std::io::Write;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "rustnt-filesystem-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temporary directory should be created");
        let file_path: PathBuf = root.join("demo.txt");
        let mut file = File::create(&file_path).expect("temporary file should be created");
        file.write_all(b"hello").expect("temporary file should be writable");
        drop(file);

        let metadata = super::stat_path(file_path.to_str().expect("temporary path is UTF-8"))
            .expect("temporary file metadata should be readable");
        assert_eq!(metadata.kind, FileKind::File);
        assert_eq!(metadata.size_bytes, Some(5));
        assert!(PathBuf::from(&metadata.path).is_absolute());
        assert!(metadata.created.is_some()
            || metadata.modified.is_some()
            || metadata.accessed.is_some());

        fs::remove_dir_all(&root).expect("temporary tree should be removed");
    }
}
