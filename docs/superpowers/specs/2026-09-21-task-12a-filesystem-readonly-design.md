# Task12A 文件系统只读能力设计规格

**日期：** 2026-09-21

## 目标

为 RustNT 增加一组 Windows 用户态、只读、可测试的文件系统能力：

- 文件和目录元数据读取
- 目录直接子项枚举
- 所在卷容量读取
- 原始 ACL 读取
- 受控递归名称搜索

Task12A 不实现高权限访问、不修改文件或权限、不读取文件内容，也不改变
现有服务协议、Named Pipe、capability 注册表和服务端行为。

## 范围

新增以下 CLI 命令：

```text
rustnt fs stat --path <路径>
rustnt fs list --path <目录>
rustnt fs space --path <路径>
rustnt fs permissions --path <路径>
rustnt fs search --path <目录> --name <名称>
```

本任务只处理当前用户态进程可直接访问的路径。受保护路径访问失败时，不
启动服务、不请求提权、不绕过 Windows 安全模型。

## 架构

核心逻辑放入新的 `rustnt_core::filesystem` 模块。CLI 只负责参数解析、
调用核心 API、渲染结构化结果和映射退出码。

数据流如下：

```text
CLI 参数
  -> 参数校验
  -> rustnt_core::filesystem
  -> Win32 API
  -> 结构化结果
  -> 人类可读文本
```

实现使用现有 `windows-sys` 依赖和原生 Win32 API，不新增第三方 crate。
核心模块沿用当前 `rustnt-core` 的 Windows-only 架构。

主要 API 责任：

```rust
pub fn stat_path(path: &str) -> Result<FileMetadata, FileSystemError>;
pub fn list_directory(path: &str) -> Result<Vec<DirectoryEntry>, FileSystemError>;
pub fn disk_space(path: &str) -> Result<DiskSpace, FileSystemError>;
pub fn read_permissions(path: &str) -> Result<FilePermissions, FileSystemError>;
pub fn search_path(
    root: &str,
    query: &str,
    limits: SearchLimits,
) -> Result<SearchReport, FileSystemError>;
```

函数均为只读操作。返回值不包含打开句柄；所有 Win32 句柄必须在核心
函数返回前关闭。

## 数据模型

### 文件类型和元数据

```rust
pub enum FileKind {
    File,
    Directory,
    ReparsePoint,
    Other,
}

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
```

规则：

- reparse point 优先报告为 `ReparsePoint`，即使其基础属性同时表示文件或目录。
- 目录大小为 `None`，不递归计算目录总大小。
- 时间不能可靠转换时为 `None`。
- `path` 使用 Windows 完整路径；无法规范化的输入由核心 API 返回错误。
- `attributes` 保留 Win32 文件属性位掩码。

### 目录项

```rust
pub struct DirectoryEntry {
    pub metadata: FileMetadata,
}
```

目录项按不区分大小写的名称排序；名称相同时按完整路径排序。目录枚举
可以返回 reparse point，但调用方不能把它作为递归入口。

### 磁盘空间

```rust
pub struct DiskSpace {
    pub root: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub available_bytes: u64,
}
```

`GetDiskFreeSpaceExW` 的字段映射必须固定为：

- `free_bytes`：`lpTotalNumberOfFreeBytes`
- `total_bytes`：`lpTotalNumberOfBytes`
- `available_bytes`：`lpFreeBytesAvailableToCaller`

`space` 命令展示三者，不把调用者可用空间误报为整个卷的总空闲空间。

### 原始 ACL

```rust
pub struct FilePermissions {
    pub owner_sid: Option<String>,
    pub dacl_present: bool,
    pub dacl_protected: bool,
    pub entries: Vec<AceEntry>,
}

pub enum AllowOrDeny {
    Allow,
    Deny,
}

pub struct AceEntry {
    pub kind: AllowOrDeny,
    pub sid: String,
    pub mask: u32,
    pub inherited: bool,
}
```

权限读取使用 `GetSecurityInfo` 读取 owner 和 DACL：

- owner SID、ACE SID 使用 `ConvertSidToStringSidW` 转换为字符串。
- DACL 不存在与存在但为空必须区分。
- `SE_DACL_PROTECTED` 映射到 `dacl_protected`。
- ACE 类型只输出允许和拒绝；遇到不支持的 ACE 类型返回可读错误，不静默
  伪造为允许或拒绝。
- `ACE_INHERITED_ACE` 映射到 `inherited`。
- 不计算当前用户有效权限，不展开组成员关系，不修改安全描述符。

### 搜索

```rust
pub struct SearchLimits {
    pub max_depth: usize,
    pub max_results: usize,
}

pub struct SearchReport {
    pub matches: Vec<FileMetadata>,
    pub skipped_access: u64,
    pub skipped_reparse: u64,
    pub truncated: bool,
}
```

CLI 固定使用 `max_depth = 16` 和 `max_results = 1000`，不在 Task12A
暴露额外的限制参数。

搜索规则：

- 根路径必须是可打开的目录；根路径失败则整个命令失败。
- 文件和目录均可成为匹配结果。
- 名称匹配不区分大小写，采用包含匹配。
- 目录遍历按名称排序，结果顺序稳定。
- 深度达到上限后不再进入子目录。
- 结果达到上限后停止遍历并设置 `truncated = true`。
- reparse point 不进入，计入 `skipped_reparse`。
- 子项或子目录遇到访问拒绝时跳过，计入 `skipped_access`。
- 不跟随符号链接、junction 或其他 reparse point。
- 不读取文件内容。

## Win32 API 选择

核心模块使用以下原生 API：

- 路径和属性：`GetFullPathNameW`、`GetFileAttributesExW`
- 目录枚举：`FindFirstFileExW`、`FindNextFileW`、`FindClose`
- 磁盘容量：`GetDiskFreeSpaceExW`
- 原始 ACL：`GetSecurityInfo`、`GetSecurityDescriptorControl`、
  `GetSecurityDescriptorDacl`、`GetSecurityDescriptorOwner`、
  `GetAce`、`ConvertSidToStringSidW`、`LocalFree`

UTF-16 参数和输出缓冲区必须使用带长度的 Rust 容器，并显式保证 NUL
终止。所有 unsafe 调用要保留简短的安全性说明。

## 错误策略

核心错误类型：

```rust
pub enum FileSystemError {
    InvalidPath(String),
    NotFound(String),
    AccessDenied(String),
    Io(String),
    Win32 {
        operation: String,
        code: u32,
    },
}
```

错误需要保留操作上下文和路径，但 CLI 不直接输出原始错误码作为主要
信息。CLI 应输出可读的错误描述；内部测试可以断言错误分类和 Win32 code。

命令行为：

- `stat`、`space`、`permissions`：目标失败，输出错误并返回退出码 `1`。
- `list`：根目录打开失败返回 `1`；单个子项元数据失败时保留路径、
  将不可用字段显示为 `N/A`，同时向 stderr 输出警告。
- `search`：根目录失败返回 `1`；子项访问失败跳过并计数，命令仍可
  成功返回 `0`。
- 参数缺失、重复、未知或不合法：输出 usage，返回退出码 `2`。
- 成功完成：返回退出码 `0`。
- 警告输出到 stderr，结构化结果输出到 stdout。

## CLI 输出

### `stat`

```text
PATH              C:\Users\demo
TYPE              DIRECTORY
ATTRIBUTES        0x00000010
SIZE              N/A
CREATED           2026-09-21 10:30:00
MODIFIED          2026-09-21 10:35:12
ACCESSED          2026-09-21 10:35:12
REPARSE POINT     no
```

无法获得的字段统一显示 `N/A`。

### `list`

```text
TYPE        SIZE        NAME
FILE        12.4 KB     notes.txt
DIRECTORY   N/A         src
REPARSE     N/A         link
```

### `space`

```text
ROOT              C:\
FREE              400.0 GB
AVAILABLE         377.4 GB
TOTAL             952.6 GB
USED              60.4%
```

`USED` 基于 `total_bytes - free_bytes` 计算；容量无效时显示 `N/A`。

### `permissions`

```text
OWNER SID          S-1-5-21-...
DACL PRESENT       yes
DACL PROTECTED     no

TYPE      SID              MASK          INHERITED
ALLOW     S-1-5-18         0x001F01FF    no
DENY      S-1-5-32-Users   0x00000002    yes
```

### `search`

```text
C:\Users\demo\notes.txt
C:\Users\demo\archive\old-notes.txt

MATCHES             2
SKIPPED ACCESS      1
SKIPPED REPARSE     3
TRUNCATED           no
```

## 测试要求

### 核心单元测试

必须覆盖：

- 路径规范化和 UTF-16 NUL 终止
- 文件、目录、reparse point 和 other 类型映射
- Win32 时间转换成功与失败
- 磁盘空间三个 Win32 输出字段的准确映射
- 目录项稳定排序
- 大小写不敏感的包含匹配
- 最大深度和最大结果数
- reparse point 不递归
- 搜索访问错误计数和 `truncated`
- ACL owner、DACL 状态、ACE 类型、mask 和继承标记的映射
- 错误分类和 CLI 格式化

### Windows 集成测试

使用临时目录创建普通文件、子目录和可识别的 reparse point，验证：

- `stat` 能读文件和目录
- `list` 只枚举直接子项
- `space` 返回非零总容量和一致的字段关系
- `search` 返回稳定结果并遵守深度和数量限制
- reparse point 不被递归进入

权限测试只断言当前测试账户可稳定控制的安全描述符字段；不要求测试
环境必须具备 Administrator 或 LocalSystem 身份。

### CLI 测试

必须覆盖：

- 五个子命令的合法参数
- 缺失、重复、未知参数
- 带空格路径
- 人类可读输出中的固定列和 `N/A`
- 成功、操作失败和 usage 错误的退出码

实现后运行：

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

## 非目标

- 不实现文件创建、删除、复制、移动、重命名或写入。
- 不实现权限修改、继承修改或有效权限计算。
- 不读取文件内容，不实现内容搜索。
- 不跟随 symbolic link、junction 或其他 reparse point。
- 不新增 LocalSystem 服务 capability。
- 不改变 Named Pipe 协议版本、命令码或审计模型。
- 不实现 JSON、数据库、持久化索引、通知或后台服务。
- 不实现 GUI 文件管理器；GUI 属于后续 Task16 及 Shell 阶段。

## 验收标准

Task12A 完成需要满足：

1. 五个 `rustnt fs` 命令按本规格可用。
2. 核心 API 为只读、结构化、Windows-only，并正确释放 Win32 资源。
3. 搜索遵守深度、数量、访问错误和 reparse point 规则。
4. ACL 输出是原始安全描述符信息，不伪造有效权限。
5. 现有 process、monitor、service 命令和服务协议行为不变。
6. 工作区测试、构建、Clippy、格式和 diff 检查全部通过。
7. 实际运行至少验证 `stat`、`list`、`space` 和 `search` 的正常路径。
