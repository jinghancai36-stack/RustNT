# Task12A 文件系统只读能力实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改变 RustNT 服务协议和现有命令行为的前提下，增加 Windows 用户态只读文件系统能力和五个 `rustnt fs` CLI 子命令。

**Architecture:** 在 `rustnt-core` 新增 Windows-only `filesystem` 模块，负责路径规范化、Win32 文件元数据、目录枚举、卷容量、原始 ACL 和受控递归搜索。`rustnt-cli` 只负责 `fs` 参数解析、调用核心 API、人类可读渲染和退出码映射；核心模块不返回句柄，所有 Win32 资源在 API 返回前释放。

**Tech Stack:** Rust 2021、Cargo workspace、现有 `windows-sys 0.59`、Win32 File System/Security/Time APIs、Rust 标准库、现有 workspace 测试/build/Clippy/rustfmt 工具链。

## Global Constraints

- Task12A 只处理当前用户态进程可直接访问的路径，不启动服务、不请求提权、不绕过 Windows 安全模型。
- 不实现文件创建、删除、复制、移动、重命名、写入、内容读取或内容搜索。
- 不实现权限修改、继承修改或当前用户有效权限计算。
- 不跟随 symbolic link、junction 或其他 reparse point。
- 使用现有 `windows-sys` 依赖和原生 Win32 API，不新增第三方 crate。
- 不新增 LocalSystem 服务 capability，不改变 Named Pipe 协议版本、命令码或审计模型。
- 不改变现有 process、monitor、service 命令的行为。
- 核心 API 是 Windows-only、只读、结构化 API；返回值不包含打开句柄。
- CLI 只输出人类可读文本，不增加 JSON、数据库、持久化索引、通知或后台服务。
- 搜索默认 `max_depth = 16`、`max_results = 1000`；结果达到上限后设置 `truncated = true`。
- 搜索名称按不区分大小写的包含匹配处理，目录顺序和结果顺序稳定。
- `GetDiskFreeSpaceExW` 映射必须为：`free_bytes = lpTotalNumberOfFreeBytes`、`total_bytes = lpTotalNumberOfBytes`、`available_bytes = lpFreeBytesAvailableToCaller`。
- ACL 只输出原始 owner/DACL/ACE 数据；ACE 仅接受允许和拒绝类型，不支持的 ACE 类型必须报错。
- 参数错误返回退出码 `2`，文件系统或 Win32 操作错误返回退出码 `1`，成功返回退出码 `0`。

---

## 文件结构

本计划只修改以下文件：

- Create: `crates/rustnt-core/src/filesystem.rs`
  - 文件系统数据模型、错误类型、Win32 资源封装、路径/时间辅助函数、元数据、目录、容量、搜索和 ACL 实现。
- Modify: `crates/rustnt-core/src/lib.rs:1-7`
  - 导出 `pub mod filesystem;`，不改现有进程、监控和服务实现。
- Modify: `Cargo.toml:17-30`
  - 仅在 CLI 本地时间渲染需要时加入已存在的 `Win32_System_Time` feature；不新增 crate。
- Modify: `crates/rustnt-cli/src/main.rs:1-180,630-760,760-980`
  - `fs` 顶层路由、参数解析、命令执行、固定格式渲染和 CLI 单元测试。
- Modify: `README.md`
  - 添加五个只读 `rustnt fs` 命令的最小使用说明和安全边界。
- Modify: `docs/roadmap.md:31-37,79-82`
  - 标记 Task12A 已完成，并将 Task13 作为下一步；明确 Task12A 仍不包含高权限服务访问。
- Create: `.superpowers/sdd/task-12a-report.md`
  - 记录实现提交、测试数量、实际命令验证和环境限制；该文件为验证记录，不承载新的行为要求。

---

### Task 1: 文件系统核心模型与元数据

**Files:**
- Create: `crates/rustnt-core/src/filesystem.rs`
- Modify: `crates/rustnt-core/src/lib.rs:1-7`
- Test: `crates/rustnt-core/src/filesystem.rs` 内 `#[cfg(test)]` 模块

**Interfaces:**

- Produces:
  ```rust
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
      pub created: Option<std::time::SystemTime>,
      pub modified: Option<std::time::SystemTime>,
      pub accessed: Option<std::time::SystemTime>,
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

  pub fn stat_path(path: &str) -> Result<FileMetadata, FileSystemError>;
  ```

- Internal helpers used by later tasks:
  ```rust
  fn path_to_wide(path: &str) -> Result<Vec<u16>, FileSystemError>;
  fn normalize_path(path: &str) -> Result<(String, Vec<u16>), FileSystemError>;
  fn file_kind_from_attributes(attributes: u32) -> FileKind;
  fn filetime_to_system_time(filetime: windows_sys::Win32::Foundation::FILETIME)
      -> Option<std::time::SystemTime>;
  fn metadata_from_attributes(path: String, data: &WIN32_FILE_ATTRIBUTE_DATA)
      -> FileMetadata;
  ```

**Implementation details:**

- `path_to_wide` rejects an empty path and any input containing an embedded NUL,
  appends exactly one terminating NUL, and returns UTF-16 units.
- `normalize_path` calls `GetFullPathNameW` with a growable buffer. If the first
  call reports that the buffer is too small, allocate the reported size plus one
  and retry. Return the normalized path without the trailing NUL.
- Map Win32 error codes `ERROR_FILE_NOT_FOUND`, `ERROR_PATH_NOT_FOUND`,
  `ERROR_ACCESS_DENIED`, `ERROR_INVALID_NAME`, and `ERROR_INVALID_PARAMETER`
  to the corresponding `FileSystemError` variants; preserve all other codes in
  `FileSystemError::Win32`.
- `file_kind_from_attributes` checks `FILE_ATTRIBUTE_REPARSE_POINT` first,
  then `FILE_ATTRIBUTE_DIRECTORY`, then `FILE_ATTRIBUTE_DEVICE`; ordinary
  non-directory paths map to `File`.
- File size combines `nFileSizeHigh` and `nFileSizeLow`; directory size is
  `None`. The three FILETIME fields are converted with checked arithmetic from
  the Windows 1601 epoch to `SystemTime`; underflow/overflow returns `None`.
- `stat_path` uses `GetFullPathNameW` and `GetFileAttributesExW` only. It never
  opens a file for content access and never follows a reparse point for metadata
  classification.

- [ ] **Step 1: Add failing pure tests before the implementation**

Add tests with the following behavior and exact assertions:

```rust
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
    assert_eq!(file_kind_from_attributes(FILE_ATTRIBUTE_DEVICE), FileKind::Other);
}

#[test]
fn filetime_conversion_rejects_values_before_unix_epoch() {
    let before_epoch = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    assert_eq!(filetime_to_system_time(before_epoch), None);
}
```

Add a Windows integration test that creates a unique temporary directory and
file, calls `stat_path`, and asserts `FileKind::File`, `size_bytes == Some(5)`,
an absolute path, and at least one readable timestamp. The test must remove the
temporary tree with `std::fs::remove_dir_all` before returning.

- [ ] **Step 2: Run the focused tests and verify the intended RED state**

Run:

```text
cargo test -p rustnt-core filesystem::tests -- --nocapture
```

Expected: compilation/test failure because `filesystem` and its helpers do not
exist yet. Fix only test setup errors; do not implement production behavior
before observing the feature-missing failure.

- [ ] **Step 3: Add the module export and minimal core implementation**

Add `pub mod filesystem;` to `crates/rustnt-core/src/lib.rs`, then implement the
types and functions above. Use the existing crate-level `#![cfg(windows)]`,
`GetLastError`, and the same short `unsafe` safety comments used by the
existing modules.

For `stat_path`, the Win32 call shape must be:

```rust
let mut data = WIN32_FILE_ATTRIBUTE_DATA {
    ..unsafe { std::mem::zeroed() }
};
let ok = unsafe {
    GetFileAttributesExW(
        wide_path.as_ptr(),
        GetFileExInfoStandard,
        &mut data as *mut _ as *mut std::ffi::c_void,
    )
};
if ok == 0 {
    return Err(error_from_last_error("read file attributes", &normalized));
}
Ok(metadata_from_attributes(normalized, &data))
```

Use `GetFullPathNameW` for normalization and do not substitute a shell command,
PowerShell call, or `std::fs::canonicalize`, because the latter can follow
reparse points and changes the specified Win32 semantics.

- [ ] **Step 4: Run focused tests and the existing core suite**

Run:

```text
cargo test -p rustnt-core filesystem::tests -- --nocapture
cargo test -p rustnt-core -- --nocapture
```

Expected: all new metadata tests and all pre-existing core tests pass.

- [ ] **Step 5: Commit the independently testable metadata slice**

```text
git add crates/rustnt-core/src/lib.rs crates/rustnt-core/src/filesystem.rs
git commit -m "feat: add filesystem metadata primitives"
```

---

### Task 2: Directory enumeration, disk space, and bounded search

**Files:**
- Modify: `crates/rustnt-core/src/filesystem.rs`
- Test: `crates/rustnt-core/src/filesystem.rs`内 `#[cfg(test)]` 模块

**Interfaces:**

- Produces:
  ```rust
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

  pub fn list_directory(path: &str) -> Result<Vec<DirectoryEntry>, FileSystemError>;
  pub fn disk_space(path: &str) -> Result<DiskSpace, FileSystemError>;
  pub fn search_path(
      root: &str,
      query: &str,
      limits: SearchLimits,
  ) -> Result<SearchReport, FileSystemError>;
  ```

- Consumes Task 1 `FileMetadata`, `FileKind`, `FileSystemError`,
  `normalize_path`, and `file_kind_from_attributes`.

**Implementation details:**

- `list_directory` validates that the root is a directory, builds a
  `root\*` UTF-16 search pattern, and uses `FindFirstFileExW`,
  `FindNextFileW`, and `FindClose`. Ignore only the `.` and `..` names.
- Construct child metadata directly from `WIN32_FIND_DATAW`; preserve
  attributes, file size, and timestamps. Convert a child name with
  `String::from_utf16_lossy`, append it to the normalized root, and retain
  reparse points as entries.
- Sort entries by `(name.to_lowercase(), full_path.to_lowercase())`. Do not
  use locale-dependent ordering or insertion order.
- `disk_space` calls `GetDiskFreeSpaceExW` on the normalized path. Use three
  distinct locals so the mapping cannot be inverted:
  `free_bytes` receives `lpTotalNumberOfFreeBytes`,
  `total_bytes` receives `lpTotalNumberOfBytes`, and
  `available_bytes` receives `lpFreeBytesAvailableToCaller`.
  Derive the display root with a pure helper for drive-letter and UNC paths;
  the API call itself may use the full normalized file or directory path.
- Search treats the root as depth `0`. It enumerates one directory at a time
  using the same stable ordering as `list_directory`. A matching ordinary file
  or directory is appended before descending. A reparse point is never added
  to `matches` and increments `skipped_reparse`. Descend only when
  `depth < limits.max_depth`, the entry is a directory, and it is not a
  reparse point.
- A non-empty `max_results` limit stops traversal immediately when reached and
  sets `truncated = true`. A zero limit returns no matches and
  `truncated = true` without enumerating children.
- An access-denied child directory increments `skipped_access` and continues.
  The root access error returns `Err(FileSystemError::AccessDenied(...))`.
  Non-access Win32 failures from a child enumeration return an error rather
  than being silently counted as access failures.
- Matching uses `entry.metadata.path` basename converted with
  `to_lowercase()` and `query.to_lowercase()`, with `contains`. The CLI will
  reject an empty query.
- Add the shared basename helper:
  ```rust
  fn file_name(path: &str) -> &str;
  ```
  It returns the substring after the final `\` or `/`, or the full input when
  neither separator exists. Use this helper in both production search logic
  and the pure matching tests.

- [ ] **Step 1: Add failing tests for pure matching, limits, and capacity mapping**

Add tests with these exact behaviors:

```rust
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
    let space = disk_space_from_values(r"C:\\".to_string(), 20, 100, 15);
    assert_eq!(space.free_bytes, 20);
    assert_eq!(space.total_bytes, 100);
    assert_eq!(space.available_bytes, 15);
}
```

The pure search helper may be private and test-only in shape, but it must share
the production matching and limit logic; do not duplicate matching logic only
inside the test.

Add a Windows integration test that creates:

```text
root\
  Notes.TXT
  nested\
    old-notes.txt
    other.bin
```

Then assert that `search_path(root, "notes", SearchLimits { max_depth: 16,
max_results: 1000 })` returns exactly the two note paths in stable order,
`skipped_access == 0`, and `truncated == false`. Add a second assertion with
`max_results: 1` that returns one match and `truncated == true`.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```text
cargo test -p rustnt-core filesystem::tests::search -- --nocapture
cargo test -p rustnt-core filesystem::tests::disk_space -- --nocapture
```

Expected: the new tests fail because enumeration, capacity, and search
functions are not implemented. Resolve only compile/test harness mistakes
before writing the production implementation.

- [ ] **Step 3: Implement directory enumeration and disk space**

Implement `list_directory` with an owned `FindHandle` wrapper whose `Drop`
calls `FindClose`. Keep the Win32 search buffer local to the function and
convert each `WIN32_FIND_DATAW.cFileName` only up to its first NUL.

Implement the capacity conversion helper first:

```rust
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
```

Then call `GetDiskFreeSpaceExW` with three separate output variables in the
specified order and pass them to this helper.

- [ ] **Step 4: Implement bounded search using the directory API**

Use a stack of `(String, usize)` directory paths. For each directory:

```rust
while let Some((directory, depth)) = pending.pop() {
    let entries = match list_directory(&directory) {
        Ok(entries) => entries,
        Err(FileSystemError::AccessDenied(_)) if depth > 0 => {
            report.skipped_access += 1;
            continue;
        }
        Err(error) => return Err(error),
    };

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
            pending.push((entry.metadata.path, depth + 1));
        }
    }
}
```

Adjust the loop so a zero `max_results` truncates before enumeration and so
child directories are pushed in reverse sorted order when a LIFO stack is used,
preserving the same output order as recursive lexical traversal.

- [ ] **Step 5: Run all core tests and verify the Windows fixture**

Run:

```text
cargo test -p rustnt-core filesystem::tests -- --nocapture
cargo test -p rustnt-core -- --nocapture
```

Expected: all Task 1 and Task 2 tests pass, including the temporary-tree
search fixture and existing process/service/monitor tests.

- [ ] **Step 6: Commit the traversal slice**

```text
git add crates/rustnt-core/src/filesystem.rs
git commit -m "feat: add readonly filesystem enumeration and search"
```

---

### Task 3: Original ACL and security descriptor reading

**Files:**
- Modify: `crates/rustnt-core/src/filesystem.rs`
- Test: `crates/rustnt-core/src/filesystem.rs`内 `#[cfg(test)]` 模块

**Interfaces:**

- Produces:
  ```rust
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

  pub fn read_permissions(path: &str) -> Result<FilePermissions, FileSystemError>;
  ```

- Consumes Task 1 path normalization and `FileSystemError`.

**Implementation details:**

- Open the normalized path with `CreateFileW` using `READ_CONTROL`,
  `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`,
  `OPEN_EXISTING`, `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`.
  Wrap the handle in the same close-on-drop pattern used elsewhere in the
  module.
- Call `GetSecurityInfo` with `SE_FILE_OBJECT` and
  `OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION`.
- Treat the returned security descriptor as `LocalFree`-owned memory. Wrap
  it in a small guard that calls `LocalFree` exactly once.
- Call `GetSecurityDescriptorOwner`, `GetSecurityDescriptorDacl`, and
  `GetSecurityDescriptorControl`. `dacl_present == false` means no DACL;
  `dacl_present == true` with a null ACL means present-but-empty.
- Iterate `ACL.AceCount` with `GetAce`. Accept only
  `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_DENIED_ACE_TYPE`; extract `Mask`,
  the SID starting at `SidStart`, and `ACE_HEADER.AceFlags & INHERITED_ACE`.
  Any other ACE type returns `FileSystemError::Win32` with operation
  `"read supported file ACE"`.
- Convert owner and ACE SIDs with `ConvertSidToStringSidW`, copy the UTF-16
  string, and free the returned string with `LocalFree`.
- `SE_DACL_PROTECTED` sets `dacl_protected`. Do not call Authz APIs, expand
  group membership, or calculate effective access.

- [ ] **Step 1: Add failing pure mapping tests**

Add tests before the Win32 implementation:

```rust
#[test]
fn ace_type_mapping_accepts_only_allow_and_deny() {
    assert_eq!(map_ace_type(ACCESS_ALLOWED_ACE_TYPE).unwrap(), AllowOrDeny::Allow);
    assert_eq!(map_ace_type(ACCESS_DENIED_ACE_TYPE).unwrap(), AllowOrDeny::Deny);
    assert!(map_ace_type(0x7f).is_err());
}

#[test]
fn inherited_flag_mapping_reads_the_ace_flag_bit() {
    assert!(ace_is_inherited(INHERITED_ACE));
    assert!(!ace_is_inherited(0));
}
```

Add a Windows integration test that creates a temporary file, calls
`read_permissions`, asserts `owner_sid.is_some()`, `dacl_present == true`,
and verifies every returned entry has a non-empty SID and a valid
`AllowOrDeny`. The test must clean up the file in all normal return paths.

- [ ] **Step 2: Run the focused ACL tests and verify RED**

Run:

```text
cargo test -p rustnt-core filesystem::tests::ace -- --nocapture
cargo test -p rustnt-core filesystem::tests::permissions -- --nocapture
```

Expected: failure because ACL mapping and `read_permissions` do not exist.

- [ ] **Step 3: Implement ACL resource guards and raw descriptor parsing**

Use explicit guards for:

```rust
struct LocalAllocGuard(*mut std::ffi::c_void);
struct OwnedHandle(HANDLE);
```

The guard `Drop` implementations must check for null and call the matching
Win32 release function. For security descriptor memory returned by
`GetSecurityInfo`, call `LocalFree`; for SID strings returned by
`ConvertSidToStringSidW`, also call `LocalFree`.

Keep pointer validation local to the parser:

```rust
fn map_ace_type(ace_type: u32) -> Result<AllowOrDeny, FileSystemError> {
    match ace_type {
        ACCESS_ALLOWED_ACE_TYPE => Ok(AllowOrDeny::Allow),
        ACCESS_DENIED_ACE_TYPE => Ok(AllowOrDeny::Deny),
        other => Err(FileSystemError::Win32 {
            operation: format!("read supported file ACE type {other}"),
            code: ERROR_INVALID_DATA,
        }),
    }
}
```

Before dereferencing an ACE-specific structure, verify that `GetAce` succeeded,
the returned pointer is non-null, and the ACE header type is supported. Keep
the unsafe block narrow and explain why the ACL/ACE layout is valid for the
reported `AceSize`.

- [ ] **Step 4: Run all core tests, Clippy, and formatting**

Run:

```text
cargo test -p rustnt-core -- --nocapture
cargo clippy -p rustnt-core --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: all core tests pass with no Clippy warnings or formatting changes.

- [ ] **Step 5: Commit the ACL slice**

```text
git add crates/rustnt-core/src/filesystem.rs
git commit -m "feat: read original filesystem ACLs"
```

---

### Task 4: CLI integration, documentation, and final verification

**Files:**
- Modify: `crates/rustnt-cli/src/main.rs:1-180,630-760,760-980`
- Modify: `Cargo.toml:17-30` only if `Win32_System_Time` is required for local timestamp rendering
- Modify: `README.md`
- Modify: `docs/roadmap.md:31-37,79-82`
- Create: `.superpowers/sdd/task-12a-report.md`
- Test: `crates/rustnt-cli/src/main.rs`内 `#[cfg(test)]` 模块

**Interfaces:**

- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  enum FileSystemCommand {
      Stat { path: String },
      List { path: String },
      Space { path: String },
      Permissions { path: String },
      Search { path: String, name: String },
  }

  fn parse_filesystem_command(args: &[String]) -> Result<FileSystemCommand, String>;
  fn run_filesystem_command(command: FileSystemCommand) -> Result<(), String>;
  ```

- Consumes all public `rustnt_core::filesystem` APIs from Tasks 1-3 and the
  existing `format_bytes` helper.

**Implementation details:**

- Route top-level `fs` before the existing `process` fallback:
  `args = ["fs", ...]` calls `parse_filesystem_command(&args[1..])`.
- Require exactly one `--path` for every subcommand. Require exactly one
  `--name` for `search`; reject empty names, duplicate flags, missing values,
  unknown flags, extra positional arguments, and unknown subcommands.
- Extend usage with exactly:
  ```text
  usage: rustnt fs stat --path <path>
  usage: rustnt fs list --path <directory>
  usage: rustnt fs space --path <path>
  usage: rustnt fs permissions --path <path>
  usage: rustnt fs search --path <directory> --name <text>
  ```
- `run_filesystem_command` calls one core function and maps all
  `FileSystemError` values to `Err(error.to_string())`. The top-level `main`
  maps parser errors to `ExitCode::from(2)` and operation errors to
  `ExitCode::from(1)`.
- Render stat fields in the fixed order:
  `PATH`, `TYPE`, `ATTRIBUTES`, `SIZE`, `CREATED`, `MODIFIED`, `ACCESSED`,
  `REPARSE POINT`. Use `N/A` for missing optional values.
- Render `list` with `TYPE`, `SIZE`, `NAME` columns. Use only the final path
  component for `NAME`, `format_bytes` for present sizes, and `N/A` for
  directory/reparse sizes.
- Render `space` with `ROOT`, `FREE`, `AVAILABLE`, `TOTAL`, and `USED`.
  Compute `USED` from `total_bytes - free_bytes`; if `free_bytes > total_bytes`
  or total is zero, print `N/A`.
- Render permissions with owner, DACL flags, and ACE rows. Use lowercase
  `yes`/`no`, `ALLOW`/`DENY`, hexadecimal masks, and `N/A` only for absent
  owner.
- Render search matches one per line, followed by the exact summary labels
  `MATCHES`, `SKIPPED ACCESS`, `SKIPPED REPARSE`, and `TRUNCATED`.
- For local timestamp rendering without a third-party crate, add
  `Win32_System_Time` to the existing `windows-sys` feature list and convert
  `SystemTime` back to `FILETIME`, call `FileTimeToSystemTime`, then
  `SystemTimeToTzSpecificLocalTime`. Format `SYSTEMTIME` as
  `YYYY-MM-DD HH:MM:SS`. If conversion fails, print `N/A`.
- Do not add a service call, capability, protocol command, or elevation path.

- [ ] **Step 1: Add failing CLI parser and renderer tests**

Extend the CLI test imports with the new parser and render helpers. Add tests
with these assertions:

```rust
#[test]
fn parses_all_filesystem_commands() {
    assert_eq!(
        parse_filesystem_command(&strings(&["stat", "--path", r"C:\Temp\a.txt"])).unwrap(),
        FileSystemCommand::Stat { path: r"C:\Temp\a.txt".to_string() }
    );
    assert_eq!(
        parse_filesystem_command(&strings(&[
            "search", "--path", r"C:\Temp", "--name", "notes"
        ])).unwrap(),
        FileSystemCommand::Search {
            path: r"C:\Temp".to_string(),
            name: "notes".to_string()
        }
    );
}

#[test]
fn rejects_invalid_filesystem_arguments() {
    for args in [
        vec!["stat"],
        vec!["list", "--path"],
        vec!["space", "--path", "x", "--path", "y"],
        vec!["permissions", "--unknown", "x"],
        vec!["search", "--path", "x"],
        vec!["search", "--path", "x", "--name", ""],
        vec!["unknown", "--path", "x"],
    ] {
        assert!(parse_filesystem_command(&strings(&args)).is_err());
    }
}

#[test]
fn renders_filesystem_missing_values_and_search_summary() {
    let metadata = sample_directory_metadata_with_missing_times();
    let output = render_file_metadata(&metadata);
    assert!(output.contains("TYPE              DIRECTORY"));
    assert!(output.contains("SIZE              N/A"));
    assert!(output.contains("CREATED           N/A"));

    let report = sample_search_report();
    let output = render_search(&report);
    assert!(output.contains("MATCHES"));
    assert!(output.contains("SKIPPED ACCESS"));
    assert!(output.contains("TRUNCATED         no"));
}
```

Add renderer tests for `space` field order and `permissions` ACE rows, and
assert that existing process/monitor renderer tests remain unchanged.

- [ ] **Step 2: Run CLI tests and verify RED**

Run:

```text
cargo test -p rustnt-cli tests::parses_all_filesystem_commands -- --nocapture
cargo test -p rustnt-cli tests::rejects_invalid_filesystem_arguments -- --nocapture
```

Expected: failure because the `fs` parser, command type, and render helpers do
not exist. Fix only test setup errors before adding the implementation.

- [ ] **Step 3: Implement parser, routing, and renderers**

Add a top-level branch immediately after the existing `monitor` branch:

```rust
if args.first().map(String::as_str) == Some("fs") {
    let command = match parse_filesystem_command(&args[1..]) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("usage error: {error}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    return match run_filesystem_command(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    };
}
```

Keep argument parsing explicit and consistent with the existing process/service
parsers; do not add a CLI parsing dependency.

- [ ] **Step 4: Add local timestamp formatting and verify feature compilation**

If the compiler reports missing time APIs, add exactly:

```toml
"Win32_System_Time",
```

to the existing `windows-sys` feature list in `Cargo.toml`, then run:

```text
cargo check -p rustnt-cli
```

Expected: the CLI and core compile on the Windows target without new
dependencies.

- [ ] **Step 5: Add usage documentation and verification report**

Update `README.md` with the five commands and the following boundaries:

- commands are user-mode and read-only;
- `search` is capped at depth 16 and 1000 results;
- reparse points are not followed;
- `permissions` reports raw ACL data and does not calculate effective access;
- protected paths still fail under the current user token; no implicit elevation
  or service access occurs.

Update `docs/roadmap.md` so Task12A is marked complete and Task13 is the next
planned capability. Create `.superpowers/sdd/task-12a-report.md` with:

```markdown
# Task12A Verification Report

## Status

Complete after implementation and review.

## Commands

- `rustnt fs stat --path ...`
- `rustnt fs list --path ...`
- `rustnt fs space --path ...`
- `rustnt fs permissions --path ...`
- `rustnt fs search --path ... --name ...`

## Automated Verification

Record the actual counts and outputs for workspace tests, build, Clippy,
rustfmt, and `git diff --check`.

## Boundaries

Record any environment-gated ACL or reparse-point checks without claiming
coverage that did not run.
```

Replace the sample wording with actual results; do not leave placeholder
values in the committed report.

- [ ] **Step 6: Run the complete verification suite**

Run:

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
git status --short --branch
```

Expected: all workspace tests pass, build and Clippy finish without errors,
format and diff checks are clean, and only intended Task12A files are changed.

Run the four required normal-path commands on a temporary fixture or a known
accessible directory:

```text
cargo run -p rustnt-cli -- fs stat --path <fixture-file>
cargo run -p rustnt-cli -- fs list --path <fixture-directory>
cargo run -p rustnt-cli -- fs space --path <fixture-directory>
cargo run -p rustnt-cli -- fs search --path <fixture-directory> --name notes
```

Also run `permissions` on the same fixture when the current token can read its
security descriptor. Record any access-denied result as an environment
boundary rather than changing the implementation to elevate.

- [ ] **Step 7: Commit CLI and documentation integration**

```text
git add Cargo.toml crates/rustnt-cli/src/main.rs README.md docs/roadmap.md .superpowers/sdd/task-12a-report.md
git commit -m "feat: add filesystem readonly CLI"
```

---

## Plan Self-Review

### Spec coverage

- Core model, path normalization, metadata, FILETIME conversion: Task 1.
- Direct directory enumeration and stable sorting: Task 2.
- `GetDiskFreeSpaceExW` three-field mapping: Task 2.
- Search matching, depth 16, result cap 1000, truncation, access count, and
  reparse avoidance: Task 2.
- Raw owner/DACL/ACE reading and resource cleanup: Task 3.
- Five CLI commands, usage, rendering, local time, exit codes: Task 4.
- No service/protocol/capability changes: Global Constraints and Task 4.
- Tests, workspace verification, real command smoke checks, report and roadmap:
  Task 1-4.

### Placeholder Scan

The plan must not contain unresolved markers or unspecified test steps. Every
test step names the
behavior, command, and expected result.

### Type consistency

- Task 2 consumes `FileMetadata`, `FileKind`, `FileSystemError` from Task 1.
- Task 3 consumes `FileSystemError` and path normalization from Task 1.
- Task 4 consumes all public Task 1-3 structs and functions without changing
  their signatures.
- `SearchLimits` and `SearchReport` names match the approved spec and are used
  consistently in Task 2 and Task 4.
