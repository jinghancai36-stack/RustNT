use std::ffi::c_void;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_INVALID_DATA, ERROR_NO_MORE_FILES, FILETIME,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    INHERITED_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
};
use windows_sys::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindClose, FindExInfoStandard, FindExSearchNameMatch, FindFirstFileExW,
    FindNextFileW, GetDiskFreeSpaceExW, GetFileAttributesExW, GetFileExInfoStandard,
    GetFullPathNameW, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FINDEX_INFO_LEVELS, FINDEX_SEARCH_OPS,
    OPEN_EXISTING, READ_CONTROL, WIN32_FILE_ATTRIBUTE_DATA,
};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowOrDeny {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AceEntry {
    pub kind: AllowOrDeny,
    pub sid: String,
    pub mask: u32,
    pub inherited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePermissions {
    pub owner_sid: Option<String>,
    pub dacl_present: bool,
    pub dacl_protected: bool,
    pub entries: Vec<AceEntry>,
}

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

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryEntry {
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSpace {
    pub root: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    pub max_depth: usize,
    pub max_results: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchReport {
    pub matches: Vec<FileMetadata>,
    pub skipped_access: u64,
    pub skipped_reparse: u64,
    pub truncated: bool,
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn name_matches(name: &str, query: &str) -> bool {
    name.to_lowercase().contains(&query.to_lowercase())
}

fn disk_space_from_values(
    root: String,
    free_bytes: u64,
    total_bytes: u64,
    available_bytes: u64,
) -> DiskSpace {
    DiskSpace {
        root,
        free_bytes,
        total_bytes,
        available_bytes,
    }
}

fn display_root(path: &str) -> String {
    if path.starts_with(r"\\") {
        let mut parts = path.trim_start_matches('\\').split('\\');
        match (parts.next(), parts.next()) {
            (Some(server), Some(share)) => format!(r"\\{}\{}\", server, share),
            _ => path.to_string(),
        }
    } else if path.as_bytes().get(1) == Some(&b':') {
        format!(r"{}\", &path[..2])
    } else {
        path.to_string()
    }
}

struct FindHandle(*mut c_void);

impl Drop for FindHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 as isize != -1 {
            unsafe {
                // SAFETY: the handle was returned by FindFirstFileExW and is owned here.
                FindClose(self.0);
            }
        }
    }
}

struct LocalAllocGuard(*mut c_void);

impl Drop for LocalAllocGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: the pointer is owned by this guard and was returned by a LocalAlloc-owned API.
                LocalFree(self.0);
            }
        }
    }
}

struct OwnedHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                // SAFETY: the handle was returned by CreateFileW and is owned here.
                CloseHandle(self.0);
            }
        }
    }
}

fn map_ace_type(ace_type: u32) -> Result<AllowOrDeny, FileSystemError> {
    match ace_type {
        ACCESS_ALLOWED_ACE_TYPE => Ok(AllowOrDeny::Allow),
        ACCESS_DENIED_ACE_TYPE => Ok(AllowOrDeny::Deny),
        _ => Err(FileSystemError::Win32 {
            operation: "read supported file ACE".to_string(),
            code: ERROR_INVALID_DATA,
        }),
    }
}

fn ace_is_inherited(flags: u32) -> bool {
    flags & INHERITED_ACE != 0
}

fn sid_to_string(sid: PSID) -> Result<String, FileSystemError> {
    if sid.is_null() {
        return Err(FileSystemError::Win32 {
            operation: "read security descriptor SID".to_string(),
            code: ERROR_INVALID_DATA,
        });
    }
    let mut string_sid = std::ptr::null_mut();
    let ok = unsafe {
        // SAFETY: sid is validated non-null and string_sid is writable output storage.
        ConvertSidToStringSidW(sid, &mut string_sid)
    };
    if ok == 0 || string_sid.is_null() {
        return Err(error_from_last_error("convert SID to string", ""));
    }
    let _string_guard = LocalAllocGuard(string_sid.cast());
    let length = unsafe {
        // SAFETY: a successful ConvertSidToStringSidW returns a NUL-terminated UTF-16 string.
        let mut length = 0usize;
        while *string_sid.add(length) != 0 {
            length += 1;
        }
        length
    };
    Ok(String::from_utf16_lossy(unsafe {
        // SAFETY: the preceding scan found the terminator within the returned string.
        std::slice::from_raw_parts(string_sid, length)
    }))
}

fn parse_acl(acl: *const ACL) -> Result<Vec<AceEntry>, FileSystemError> {
    if acl.is_null() {
        return Ok(Vec::new());
    }
    let ace_count = unsafe {
        // SAFETY: GetSecurityDescriptorDacl returned this ACL pointer from the valid descriptor.
        (*acl).AceCount
    };
    let mut entries = Vec::with_capacity(ace_count as usize);
    for index in 0..u32::from(ace_count) {
        let mut ace = std::ptr::null_mut();
        let ok = unsafe {
            // SAFETY: acl is valid for the descriptor lifetime and ace points to writable output storage.
            windows_sys::Win32::Security::GetAce(acl, index, &mut ace)
        };
        if ok == 0 || ace.is_null() {
            return Err(error_from_last_error("read file ACE", ""));
        }
        let header = unsafe {
            // SAFETY: GetAce returned a non-null ACE pointer; the ACL API guarantees an ACE header at its start.
            &*(ace as *const ACE_HEADER)
        };
        let kind = map_ace_type(u32::from(header.AceType))?;
        let minimum_size = std::mem::size_of::<ACCESS_ALLOWED_ACE>() as u16;
        if header.AceSize < minimum_size {
            return Err(FileSystemError::Win32 {
                operation: "read supported file ACE".to_string(),
                code: ERROR_INVALID_DATA,
            });
        }
        let (mask, sid_start) = unsafe {
            // SAFETY: allow and deny ACEs have the same fixed header/mask/SID layout, and AceSize was validated.
            match kind {
                AllowOrDeny::Allow => {
                    let value = &*(ace as *const ACCESS_ALLOWED_ACE);
                    (value.Mask, std::ptr::addr_of!(value.SidStart))
                }
                AllowOrDeny::Deny => {
                    let value = &*(ace as *const ACCESS_DENIED_ACE);
                    (value.Mask, std::ptr::addr_of!(value.SidStart))
                }
            }
        };
        let sid = sid_to_string(sid_start as PSID)?;
        entries.push(AceEntry {
            kind,
            sid,
            mask,
            inherited: ace_is_inherited(u32::from(header.AceFlags)),
        });
    }
    Ok(entries)
}

pub fn read_permissions(path: &str) -> Result<FilePermissions, FileSystemError> {
    let (normalized, wide_path) = normalize_path(path)?;
    let handle = unsafe {
        // SAFETY: wide_path is NUL-terminated and the requested flags are valid for file/directory reads.
        CreateFileW(
            wide_path.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(error_from_last_error(
            "open file for security descriptor",
            &normalized,
        ));
    }
    let _handle_guard = OwnedHandle(handle);

    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        // SAFETY: output pointers are writable and the handle remains valid for this call.
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(FileSystemError::Win32 {
            operation: "read file security descriptor".to_string(),
            code: status,
        });
    }
    if descriptor.is_null() {
        return Err(FileSystemError::Win32 {
            operation: "read file security descriptor".to_string(),
            code: ERROR_INVALID_DATA,
        });
    }
    let _descriptor_guard = LocalAllocGuard(descriptor);

    let mut descriptor_owner: PSID = std::ptr::null_mut();
    let mut owner_defaulted = 0;
    let ok = unsafe {
        // SAFETY: descriptor is valid for the guard lifetime and output pointers are writable.
        GetSecurityDescriptorOwner(descriptor, &mut descriptor_owner, &mut owner_defaulted)
    };
    if ok == 0 {
        return Err(error_from_last_error("read file owner", &normalized));
    }
    let owner_sid = if descriptor_owner.is_null() {
        None
    } else {
        Some(sid_to_string(descriptor_owner)?)
    };
    let mut dacl_present = 0;
    let mut dacl_defaulted = 0;
    let ok = unsafe {
        // SAFETY: descriptor is valid for the guard lifetime and output pointers are writable.
        GetSecurityDescriptorDacl(
            descriptor,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    };
    if ok == 0 {
        return Err(error_from_last_error("read file DACL", &normalized));
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    let ok = unsafe {
        // SAFETY: descriptor is valid for the guard lifetime and output pointers are writable.
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
    };
    if ok == 0 {
        return Err(error_from_last_error(
            "read file security descriptor control",
            &normalized,
        ));
    }
    let entries = if dacl_present == 0 {
        Vec::new()
    } else {
        parse_acl(dacl)?
    };
    Ok(FilePermissions {
        owner_sid,
        dacl_present: dacl_present != 0,
        dacl_protected: control & SE_DACL_PROTECTED != 0,
        entries,
    })
}

fn metadata_from_find_data(path: String, data: &WIN32_FIND_DATAW) -> FileMetadata {
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

fn find_name(data: &WIN32_FIND_DATAW) -> String {
    let end = data
        .cFileName
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(data.cFileName.len());
    String::from_utf16_lossy(&data.cFileName[..end])
}

pub fn list_directory(path: &str) -> Result<Vec<DirectoryEntry>, FileSystemError> {
    let (normalized, _) = normalize_path(path)?;
    let root = stat_path(&normalized)?;
    if root.kind != FileKind::Directory {
        return Err(FileSystemError::InvalidPath(normalized));
    }
    let pattern = path_to_wide(&format!(r"{}\*", normalized))?;
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle = unsafe {
        // SAFETY: pattern is NUL-terminated and data is writable storage.
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoStandard as FINDEX_INFO_LEVELS,
            &mut data as *mut _ as *mut c_void,
            FindExSearchNameMatch as FINDEX_SEARCH_OPS,
            std::ptr::null_mut(),
            0,
        )
    };
    if handle as isize == -1 {
        return Err(error_from_last_error("enumerate directory", &normalized));
    }
    let _handle = FindHandle(handle);
    let mut entries = Vec::new();
    loop {
        let name = find_name(&data);
        if name != "." && name != ".." {
            let child_path = format!(r"{}\{}", normalized, name);
            entries.push(DirectoryEntry {
                metadata: metadata_from_find_data(child_path, &data),
            });
        }
        let ok = unsafe {
            // SAFETY: handle is valid while _handle is alive and data is writable.
            FindNextFileW(handle, &mut data)
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(error_from_last_error("enumerate directory", &normalized));
        }
    }
    entries.sort_by(|left, right| {
        (
            file_name(&left.metadata.path).to_lowercase(),
            left.metadata.path.to_lowercase(),
        )
            .cmp(&(
                file_name(&right.metadata.path).to_lowercase(),
                right.metadata.path.to_lowercase(),
            ))
    });
    Ok(entries)
}

pub fn disk_space(path: &str) -> Result<DiskSpace, FileSystemError> {
    let (normalized, wide_path) = normalize_path(path)?;
    let mut available_bytes = 0u64;
    let mut total_bytes = 0u64;
    let mut free_bytes = 0u64;
    let ok = unsafe {
        // SAFETY: wide_path is NUL-terminated and output pointers are valid.
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut available_bytes,
            &mut total_bytes,
            &mut free_bytes,
        )
    };
    if ok == 0 {
        return Err(error_from_last_error("read disk space", &normalized));
    }
    Ok(disk_space_from_values(
        display_root(&normalized),
        free_bytes,
        total_bytes,
        available_bytes,
    ))
}

pub fn search_path(
    root: &str,
    query: &str,
    limits: SearchLimits,
) -> Result<SearchReport, FileSystemError> {
    let (normalized, _) = normalize_path(root)?;
    let root_metadata = stat_path(&normalized)?;
    if root_metadata.kind != FileKind::Directory {
        return Err(FileSystemError::InvalidPath(normalized));
    }
    let mut report = SearchReport {
        matches: Vec::new(),
        skipped_access: 0,
        skipped_reparse: 0,
        truncated: limits.max_results == 0,
    };
    if limits.max_results == 0 {
        return Ok(report);
    }
    let mut pending = vec![(normalized, 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        let entries = match list_directory(&directory) {
            Ok(entries) => entries,
            Err(FileSystemError::AccessDenied(_)) if depth > 0 => {
                report.skipped_access += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut child_directories = Vec::new();
        for entry in entries {
            if report.matches.len() >= limits.max_results {
                report.truncated = true;
                return Ok(report);
            }
            if entry.metadata.is_reparse_point {
                report.skipped_reparse += 1;
                continue;
            }
            if name_matches(file_name(&entry.metadata.path), query) {
                report.matches.push(entry.metadata.clone());
            }
            if entry.metadata.kind == FileKind::Directory && depth < limits.max_depth {
                child_directories.push(entry.metadata.path);
            }
        }
        for child in child_directories.into_iter().rev() {
            pending.push((child, depth + 1));
        }
    }
    Ok(report)
}

#[cfg(test)]
fn search_entries_for_test(
    entries: Vec<FileMetadata>,
    query: &str,
    limits: SearchLimits,
) -> SearchReport {
    let mut report = SearchReport {
        matches: Vec::new(),
        skipped_access: 0,
        skipped_reparse: 0,
        truncated: false,
    };
    for entry in entries {
        if report.matches.len() >= limits.max_results {
            report.truncated = true;
            return report;
        }
        if entry.is_reparse_point {
            report.skipped_reparse += 1;
            continue;
        }
        if name_matches(file_name(&entry.path), query) {
            report.matches.push(entry);
        }
    }
    report
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
        capacity = (length as usize)
            .checked_add(1)
            .ok_or_else(|| FileSystemError::InvalidPath(path.to_string()))?;
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
        ace_is_inherited, disk_space_from_values, file_kind_from_attributes,
        filetime_to_system_time, map_ace_type, name_matches, path_to_wide, search_entries_for_test,
        AllowOrDeny, FileKind, FileMetadata, FileSystemError, SearchLimits,
    };
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Security::INHERITED_ACE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    use windows_sys::Win32::System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE,
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

    fn fixture_file(path: &str) -> FileMetadata {
        FileMetadata {
            path: path.to_string(),
            kind: FileKind::File,
            size_bytes: Some(0),
            created: None,
            modified: None,
            accessed: None,
            attributes: 0,
            is_reparse_point: false,
        }
    }

    #[test]
    fn search_matching_is_case_insensitive_and_uses_contains() {
        assert!(name_matches("Old-Notes.TXT", "notes"));
        assert!(!name_matches("README.md", "notes"));
    }

    #[test]
    fn search_limits_stop_at_zero_and_report_truncation() {
        let report = search_entries_for_test(
            vec![fixture_file("a.txt"), fixture_file("b.txt")],
            "txt",
            SearchLimits {
                max_depth: 16,
                max_results: 0,
            },
        );
        assert!(report.matches.is_empty());
        assert!(report.truncated);
    }

    #[test]
    fn disk_space_field_mapping_keeps_free_total_and_available_distinct() {
        let space = disk_space_from_values(r"C:\".to_string(), 20, 100, 15);
        assert_eq!(space.free_bytes, 20);
        assert_eq!(space.total_bytes, 100);
        assert_eq!(space.available_bytes, 15);
    }

    #[test]
    fn ace_type_mapping_accepts_only_allow_and_deny() {
        assert_eq!(
            map_ace_type(ACCESS_ALLOWED_ACE_TYPE).unwrap(),
            AllowOrDeny::Allow
        );
        assert_eq!(
            map_ace_type(ACCESS_DENIED_ACE_TYPE).unwrap(),
            AllowOrDeny::Deny
        );
        assert_eq!(
            map_ace_type(0x7f),
            Err(FileSystemError::Win32 {
                operation: "read supported file ACE".to_string(),
                code: windows_sys::Win32::Foundation::ERROR_INVALID_DATA,
            })
        );
    }

    #[test]
    fn inherited_flag_mapping_reads_the_ace_flag_bit() {
        assert!(ace_is_inherited(INHERITED_ACE));
        assert!(!ace_is_inherited(0));
    }

    #[test]
    #[cfg(windows)]
    fn search_path_finds_stable_matches_and_truncates_at_limit() {
        use std::fs::{self, File};
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "rustnt-search-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after Unix epoch")
                .as_nanos()
        ));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).expect("temporary tree should be created");
        File::create(root.join("Notes.TXT")).expect("file should be created");
        File::create(nested.join("old-notes.txt")).expect("file should be created");
        File::create(nested.join("other.bin")).expect("file should be created");

        let root_text = root.to_str().expect("temporary path is UTF-8");
        let report = super::search_path(
            root_text,
            "notes",
            SearchLimits {
                max_depth: 16,
                max_results: 1000,
            },
        )
        .expect("search should succeed");
        let paths: Vec<String> = report
            .matches
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].to_lowercase().ends_with(r"\notes.txt"));
        assert!(paths[1].to_lowercase().ends_with(r"\nested\old-notes.txt"));
        assert_eq!(report.skipped_access, 0);
        assert!(!report.truncated);

        let limited = super::search_path(
            root_text,
            "notes",
            SearchLimits {
                max_depth: 16,
                max_results: 1,
            },
        )
        .expect("limited search should succeed");
        assert_eq!(limited.matches.len(), 1);
        assert!(limited.truncated);

        fs::remove_dir_all(&root).expect("temporary tree should be removed");
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
        file.write_all(b"hello")
            .expect("temporary file should be writable");
        drop(file);

        let metadata = super::stat_path(file_path.to_str().expect("temporary path is UTF-8"))
            .expect("temporary file metadata should be readable");
        assert_eq!(metadata.kind, FileKind::File);
        assert_eq!(metadata.size_bytes, Some(5));
        assert!(PathBuf::from(&metadata.path).is_absolute());
        assert!(
            metadata.created.is_some()
                || metadata.modified.is_some()
                || metadata.accessed.is_some()
        );

        fs::remove_dir_all(&root).expect("temporary tree should be removed");
    }

    #[test]
    #[cfg(windows)]
    fn permissions_read_owner_dacl_and_supported_entries_from_a_temporary_file() {
        use std::fs::{self, File};
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "rustnt-permissions-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temporary directory should be created");
        let file_path = root.join("demo.txt");
        File::create(&file_path).expect("temporary file should be created");

        let permissions = super::read_permissions(
            file_path
                .to_str()
                .expect("temporary path should be representable as UTF-8"),
        )
        .expect("temporary file permissions should be readable");

        assert!(permissions.owner_sid.is_some());
        assert!(permissions.dacl_present);
        for entry in &permissions.entries {
            assert!(!entry.sid.is_empty());
            assert!(matches!(entry.kind, AllowOrDeny::Allow | AllowOrDeny::Deny));
        }

        fs::remove_dir_all(&root).expect("temporary directory should be removed");
    }
}
