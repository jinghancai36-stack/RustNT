# Task13 窗口和会话查看器设计规格

**日期：** 2026-09-22

## 目标

为 RustNT 增加 Windows 用户态、只读、可测试的窗口和会话查看能力：

- 枚举当前进程所在 Session 的当前交互桌面上的所有顶层窗口
- 查看窗口句柄、标题、类名、线程、进程关系和窗口状态
- 查询当前前台窗口
- 枚举本机 Windows Session 及其基本连接信息
- 通过 CLI 输出稳定、可读的诊断信息

Task13 不跨 Session 枚举窗口，不启动服务，不请求提权，不注入进程，
不改变窗口状态，也不实现 GUI。GUI 基础框架属于 Task14。

## 范围

新增以下 CLI 命令：

```text
rustnt window list
rustnt window foreground
rustnt session list
```

窗口命令只处理当前进程所在 Session 的当前交互桌面。`window list` 保留
所有顶层窗口，包括隐藏窗口和空标题窗口，不枚举子窗口。

Session 命令枚举本机可见的 Windows Session。Session 信息不是窗口枚举
代理，不承诺从其他 Session 获取窗口对象。

本任务仍然只使用当前用户进程可直接访问的 Windows API。读取失败时按本
规格降级为缺失字段或可读错误，不通过 LocalSystem 服务或隐式提权绕过
安全边界。

## 架构

核心逻辑放入新的 `rustnt_core::window` 模块。CLI 只负责参数解析、调用
核心 API、渲染结构化结果和映射退出码。

数据流如下：

```text
CLI 参数
  -> 参数校验
  -> rustnt_core::window
  -> User32 / WTS API
  -> 结构化结果
  -> 人类可读文本
```

实现使用现有 `windows-sys` 依赖和原生 Win32 API，不新增第三方 crate。
核心模块沿用当前 `rustnt-core` 的 Windows-only 架构。

主要责任：

- `window.rs` 负责窗口枚举、前台窗口查询、窗口字段采集、当前 Session
  和当前桌面识别、WTS Session 枚举以及 Win32 资源释放。
- `rustnt-cli/src/main.rs` 负责 `window`/`session` 路由、严格参数解析和
  固定文本输出。
- 现有 `rustnt_core::list_processes()` 用于补充进程名和进程路径，避免
  为窗口查看器复制一套 Tool Help 进程枚举实现。

## 数据模型

### 窗口

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub hwnd: usize,
    pub title: String,
    pub class_name: String,
    pub pid: u32,
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub thread_id: u32,
    pub desktop_name: Option<String>,
    pub session_id: u32,
    pub visible: bool,
    pub minimized: bool,
    pub foreground: bool,
}
```

字段规则：

- `hwnd` 保存窗口句柄的数值表示，不返回原始句柄。
- `title` 和 `class_name` 为空字符串是合法结果；API 读取失败时使用
  `N/A` 对应的缺失表示。
- `process_name` 和 `process_path` 复用现有进程快照匹配结果；进程已退出、
  无法打开或路径不可读时为 `None`。
- `thread_id` 来自 `GetWindowThreadProcessId`。
- `desktop_name` 来自窗口线程的桌面对象；无法读取时为 `None`。
- `session_id` 是窗口所属进程的 Session。
- `visible` 使用 `IsWindowVisible`。
- `minimized` 使用 `IsIconic`。
- `foreground` 使用当前 `GetForegroundWindow` 结果与窗口句柄比较。

### 窗口快照

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub current_session_id: u32,
    pub current_desktop: String,
    pub windows: Vec<WindowInfo>,
    pub skipped_windows: u32,
}
```

规则：

- `windows` 保留 `EnumWindows` 返回的 Z 顺序，不按标题或 PID 重排。
- 只保留 `session_id == current_session_id` 且属于当前交互桌面的窗口。
- 隐藏窗口、空标题窗口和没有可读进程路径的窗口仍然保留。
- 窗口在枚举期间关闭或无法获得必要身份信息时，增加
  `skipped_windows`，继续枚举其他窗口。
- `current_desktop` 是当前线程所属桌面的名称；无法读取时整个窗口命令
  返回错误，因为无法证明枚举范围符合规格。

### Session

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Connected,
    ConnectQuery,
    Shadow,
    Disconnected,
    Idle,
    Listen,
    Reset,
    Down,
    Init,
    Other(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: u32,
    pub state: SessionState,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub client_name: Option<String>,
    pub is_current_session: bool,
    pub is_active_console: bool,
}
```

规则：

- `WTSEnumerateSessionsW` 返回的 Session 按 `session_id` 升序排序。
- `WTS_CONNECTSTATE_CLASS` 的已知值映射为固定 `SessionState`；未知值保留
  为 `Other(value)`。
- `is_current_session` 对应当前进程所属 Session。
- `is_active_console` 对应 `WTSGetActiveConsoleSessionId()` 返回值。
- 用户名、域名和客户端名读取失败时为 `None`，不使其他 Session 消失。
- `WTS_SESSION_INFO` 和查询结果缓冲区必须在核心函数返回前释放。

### 错误

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    Win32 { operation: String, code: u32 },
    Enumeration(String),
    InvalidData(String),
}
```

错误策略：

- 无法确定当前进程 Session：窗口命令失败。
- 无法确定当前桌面：窗口命令失败。
- 无法启动 `EnumWindows`：窗口命令失败。
- 单个窗口的标题、类名、桌面名、进程信息读取失败：保留窗口并使用
  缺失字段；只有无法确定 PID、Session 或线程身份时才计入
  `skipped_windows`。
- `GetForegroundWindow` 返回空句柄时，`foreground_window()` 返回
  `Ok(None)`。
- 无法启动 Session 枚举：Session 命令失败。
- 单个 Session 的用户、域或客户端信息读取失败：对应字段为 `None`。
- 不把访问受限直接转换成提权请求或服务调用。

## 核心 API

```rust
pub fn enumerate_windows() -> Result<WindowSnapshot, WindowError>;
pub fn foreground_window() -> Result<Option<WindowInfo>, WindowError>;
pub fn list_sessions() -> Result<Vec<SessionInfo>, WindowError>;
```

API 返回结构化值，不返回窗口句柄、桌面句柄、WTS 指针或进程句柄。
所有临时句柄和 API 分配的缓冲区必须在返回前释放。

`foreground_window()` 使用与 `enumerate_windows()` 相同的当前 Session 和
当前桌面过滤规则；如果前台窗口属于其他 Session 或其他桌面，返回
`Ok(None)`，不跨边界读取。

## Win32 API 选择

### 窗口

- `EnumWindows`
- `GetForegroundWindow`
- `GetWindowThreadProcessId`
- `GetWindowTextLengthW`
- `GetWindowTextW`
- `GetClassNameW`
- `IsWindowVisible`
- `IsIconic`
- `GetThreadDesktop`
- `GetUserObjectInformationW`
- `ProcessIdToSessionId`

进程名称和路径使用现有 `list_processes()` 的结果，通过 PID 建立只读
查找表。窗口查询不打开进程句柄，也不重复实现进程路径读取。

### Session

- `WTSEnumerateSessionsW`
- `WTSQuerySessionInformationW`
- `WTSGetActiveConsoleSessionId`
- `WTSFreeMemory`

需要的 Windows API feature 只加入现有 `windows-sys` feature 列表，不引入
其他依赖。

### 资源管理

使用局部 RAII guard 管理：

- WTS 枚举缓冲区
- WTS 查询字符串缓冲区
- 桌面句柄的生命周期约束

窗口句柄来自系统枚举，不由 RustNT 保存或关闭。任何由 RustNT 打开的
临时对象都必须在核心函数返回前释放。

所有 `unsafe` 调用必须使用短安全注释，说明输入缓冲区、句柄或回调上下文
在调用期间的有效性。

## CLI 契约

### 参数

```text
rustnt window list
rustnt window foreground
rustnt session list
```

参数解析必须：

- 拒绝缺少子命令
- 拒绝未知顶层命令和未知子命令
- 拒绝任何额外参数
- 拒绝重复或不适用的选项
- 参数错误返回退出码 `2`

当前版本不增加筛选、排序、跨 Session 或 JSON 选项。

### `window list`

输出固定的快照头部：

```text
RustNT Windows

SESSION         1
DESKTOP         Default
WINDOWS         12
SKIPPED         0
```

每个窗口按枚举顺序输出：

```text
HWND            0x0000000000012345
TITLE           Example
CLASS           CabinetWClass
PID             1234
PROCESS         explorer.exe
PATH            C:\Windows\explorer.exe
THREAD          5678
DESKTOP         Default
SESSION         1
VISIBLE         yes
MINIMIZED       no
FOREGROUND      yes
```

缺失的文本字段输出 `N/A`。空标题可以输出空值，但字段名必须保留。
窗口之间使用一个空行分隔。

### `window foreground`

有前台窗口时输出与 `window list` 相同的窗口字段，不输出列表头部。
无符合当前 Session/桌面范围的前台窗口时输出：

```text
RustNT Foreground Window

WINDOW          none
```

### `session list`

每个 Session 按 ID 升序输出：

```text
RustNT Sessions

SESSION         1
STATE           ACTIVE
USER            user
DOMAIN          DOMAIN
CLIENT          N/A
CURRENT         yes
CONSOLE         yes
```

Session 之间使用一个空行分隔。缺失的用户、域或客户端字段输出 `N/A`。

### 退出码

- 成功：`0`
- 参数错误：`2`
- Win32、枚举或结构化操作错误：`1`

结构化结果写入 stdout，错误和 usage 写入 stderr。

## 测试要求

### 核心纯单元测试

必须覆盖：

- Session 状态值到 `SessionState` 的映射
- 未知 Session 状态保留为 `Other(value)`
- UTF-16 缓冲区转字符串和 NUL 截断
- 窗口句柄格式化
- `yes`/`no` 状态格式化
- 缺失窗口和 Session 字段渲染为 `N/A`
- CLI 子命令的严格参数解析

### Windows 核心测试

在 Windows 环境下必须覆盖：

- 当前进程 Session ID 能够获取
- 当前桌面名称能够获取
- `enumerate_windows()` 成功返回或在无可见窗口时返回空列表
- 返回窗口全部属于当前 Session
- 返回窗口不包含子窗口枚举结果
- 每个返回窗口的 HWND、PID 和线程 ID 非零
- 当前前台窗口为空时不会 panic
- `list_sessions()` 返回按 Session ID 升序的结果
- 当前 Session 至少在 Session 列表中正确标记

测试不要求当前账户为 Administrator 或 LocalSystem，也不要求存在特定
第三方窗口。窗口数量和标题必须使用宽松断言，避免桌面环境差异造成
脆弱测试。

### CLI 测试

必须覆盖：

- 三个合法命令
- 缺少子命令
- 未知命令
- 额外参数
- 窗口字段固定顺序
- Session 字段固定顺序
- 空前台窗口渲染
- 现有 process、monitor、fs、service 测试保持通过

实现后运行：

```text
cargo test --workspace -- --nocapture
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

正常路径验证：

```text
cargo run -p rustnt-cli -- window list
cargo run -p rustnt-cli -- window foreground
cargo run -p rustnt-cli -- session list
```

## 非目标

- 不跨 Session 枚举窗口。
- 不枚举子窗口、控件树或 UI Automation 树。
- 不实现窗口移动、调整大小、显示/隐藏、激活、关闭或置顶。
- 不实现输入注入、消息注入、DLL 注入或远程线程。
- 不实现窗口截图、像素读取或内容识别。
- 不新增 LocalSystem 服务 capability。
- 不改变 Named Pipe 协议版本、命令码或审计模型。
- 不实现 JSON、数据库、持久化索引、通知或后台监控。
- 不实现 GUI；GUI 基础框架属于 Task14。

## 验收标准

Task13 完成需要满足：

1. 三个 `rustnt window`/`rustnt session` 命令按本规格可用。
2. 窗口列表覆盖当前 Session 当前桌面的所有顶层窗口，并保留隐藏和空标题窗口。
3. 前台窗口查询不跨 Session 或桌面边界。
4. Session 列表包含当前 Session 和活动控制台标记。
5. 核心 API 为只读、结构化、Windows-only，并正确释放 Win32 资源。
6. 参数错误、操作错误和成功结果使用固定退出码。
7. 现有 process、monitor、fs、service 命令和服务协议行为不变。
8. 工作区测试、构建、Clippy、格式和 diff 检查全部通过。
9. 三个正常路径 CLI 命令在当前 Windows 环境完成实际运行验证。
