# Task14 GUI 基础框架设计规格

**日期：** 2026-09-24  
**状态：** 已确认设计，待编写实现计划  
**范围：** RustNT Shell 阶段的最小 GUI 宿主

## 1. 目标

Task14 为 RustNT 增加一个独立的 Windows GUI 程序，建立后续启动器、任务切换器、
文件管理器和状态栏所需的宿主基础。

本 Task 完成以下能力：

- 使用 `eframe` 提供 `winit + egui` 宿主和事件循环。
- 新增独立的 `rustnt-gui` workspace crate，并生成 `rustnt-gui.exe`。
- 提供一个可启动、可关闭、可切换主题的最小首屏。
- 将 GUI 设置保存到 `%APPDATA%\\RustNT\\gui.toml`。
- 通过运行标记识别上一次未正常退出，并进入安全恢复状态。
- 为 GUI 后续接入现有只读能力预留清晰边界。

## 2. 非目标

Task14 不实现以下内容：

- 启动器、任务切换器、快捷键、状态栏或通知入口，这些属于 Task15。
- 文件浏览和文件操作，这些属于 Task16。
- 替换 Windows Explorer、任务栏、桌面或 Shell，这些属于 Task17 及以后。
- 窗口布局、虚拟桌面或工作区管理，这些属于 Task18。
- GUI 直接调用 LocalSystem 服务、绕过现有 capability 授权或执行任意命令。
- 新的 Named Pipe 协议、服务协议、内核接口、驱动或显示合成器。
- 把整个 GUI 进程运行成 SYSTEM。

## 3. 架构

新增目录：

```text
crates/rustnt-gui/
  Cargo.toml
  src/
    main.rs
    app.rs
    config.rs
    recovery.rs
```

### 3.1 `main.rs`

负责：

- Windows GUI 进程入口。
- 创建应用数据目录和运行状态对象。
- 安装不会再次触发 panic 的 panic hook。
- 启动 `eframe` 原生窗口。
- 区分正常退出、启动失败和运行时错误。
- 在正常退出路径删除运行标记。

### 3.2 `app.rs`

实现 `eframe::App`，负责：

- 首屏布局和窗口标题 `RustNT`。
- 显示 GUI 基础框架状态。
- 显示当前主题和安全恢复状态。
- 提供深色/浅色主题切换。
- 提供保存配置操作。
- 在检测到上次异常退出时显示恢复提示。

GUI 层只依赖本地配置和恢复状态，不在 Task14 中连接服务或读取系统能力数据。

### 3.3 `config.rs`

负责配置模型、路径解析、加载、校验和保存。配置文件使用 TOML，结构保持简单，
便于后续增加字段时兼容旧文件。

### 3.4 `recovery.rs`

负责运行标记和安全恢复状态：

- 启动时检查上一轮运行是否留下标记。
- 当前进程启动后创建新的运行标记。
- 正常关闭时删除当前运行标记。
- panic 或非正常终止时保留标记，使下一次启动进入恢复状态。
- 记录受限的崩溃信息，记录失败不能阻止程序退出。

## 4. 依赖和边界

`rustnt-gui` 使用以下依赖：

- `eframe`：提供 egui 应用宿主、winit 窗口和原生事件循环。
- `serde`：配置结构的序列化和反序列化。
- `toml`：配置文件读写。

版本号在实现计划阶段根据当前 Rust 工具链和 crates.io 兼容性确定，并固定在
workspace 的依赖配置中。GUI 不引入第二套窗口后端。

应用数据目录通过 Windows 的 `%APPDATA%` 环境变量解析，不依赖 PowerShell、
外部脚本或用户当前工作目录：

```text
%APPDATA%\\RustNT\\gui.toml
%APPDATA%\\RustNT\\gui.running (legacy) and %APPDATA%\\RustNT\\gui.running.*.marker (per-instance sidecars)
%APPDATA%\\RustNT\\gui-crash.log
```

如果 `%APPDATA%` 缺失或路径不可用，程序启动失败并返回清晰错误；不会静默写入
项目目录或临时目录。

## 5. 配置模型

初始配置包含以下字段：

```toml
theme = "dark"
window_width = 960.0
window_height = 640.0
window_x = 120.0
window_y = 80.0
```

约束如下：

- `theme` 只接受 `dark` 和 `light`，其他值回退为 `dark`。
- 窗口宽度和高度必须为有限数值，并限制在合理的最小值以上。
- 窗口位置允许为空或为有限数值；无效位置使用默认位置。
- 缺少配置文件时使用安全默认值。
- TOML 解析失败时使用安全默认值，并在 GUI 首屏显示配置已回退的状态。
- 保存时先写入同目录临时文件，再替换目标文件，避免进程中断留下半个 TOML 文件。
- 配置保存失败显示可读错误，但不能破坏运行标记清理路径。

安全默认值为：深色主题、`960 x 640` 窗口，以及不强制设置窗口位置。设计稿中的
`120, 80` 仅作为正常启动时的默认位置建议；实现阶段需根据 `eframe` 当前 viewport
API 确定是否能可靠恢复位置。

## 6. 启动和恢复流程

启动顺序固定为：

1. 解析 `%APPDATA%\\RustNT`，创建目录。
2. 检查 legacy `gui.running` 和当前实例 sidecar 运行标记，并把结果保存为
   `previous_run_incomplete`。Windows 上按 marker 的 PID 判断进程是否仍然存活；
   marker 含 `creation_time_100ns` 时还必须匹配该 PID 当前进程的创建时间。
3. 读取并校验 `gui.toml`。
4. 写入当前进程独占的 sidecar 运行标记，内容至少包含 PID、启动时间和 Windows
   进程创建时间；写入失败则终止启动。
5. 如果存在上一轮运行标记，进入安全恢复状态：使用安全默认窗口尺寸和主题，忽略持久化
   的窗口几何信息，并在首屏显示恢复提示。
6. 创建 eframe 窗口并运行应用。

正常退出定义为：`eframe` 事件循环返回，且应用没有发生 panic。正常退出时执行：

1. 保存当前配置；
2. 删除当前运行标记；
3. 返回成功退出码。

配置保存失败时记录错误并继续删除运行标记，避免下一次启动被误判为崩溃。

panic hook 执行以下操作：

- 将时间、线程信息和 panic 摘要追加到 `gui-crash.log`。
- 保留 `gui.running`，让下一次启动进入安全恢复状态。
- 不在 hook 中调用可能再次 panic 的 GUI 或复杂日志逻辑。
- 崩溃日志写入失败时忽略该写入错误，保留原始 panic 行为。

运行标记契约如下：

- Windows 上 `inspect` 对 marker PID 执行非阻塞 liveness 检查。明确已退出的进程对应
  marker 会被清理；无法确认状态时保留 marker，避免因权限或瞬时错误误删运行实例。
- marker 含 `creation_time_100ns` 时，PID 存活还不够；只有 PID 与创建时间同时匹配
  才能保留 marker。创建时间不匹配表示 PID 已复用，marker 必须被清理。
- 新实例只创建自己的 sidecar，并只删除自己的 sidecar。legacy `gui.running` 仍可被
  读取以保持兼容，但不会被新实例覆盖；多个 GUI 实例可以并行运行，`inspect` 会收敛
  所有可见 marker 的状态，不实现单实例锁。
- 不含创建时间的 legacy marker 按兼容规则处理：运行中的 PID 保留，已退出或无效的
  PID 清理；无法确认 liveness 时保留。

## 7. 首屏行为

首屏必须包含：

- 窗口标题 `RustNT`。
- `RustNT GUI foundation status` 状态文本。
- 当前主题状态。
- 安全恢复状态或正常启动状态。
- 深色/浅色主题切换控件。
- 保存配置控件。
- 上次异常退出提示（仅恢复状态显示）。

关闭窗口使用 eframe 原生关闭行为。Task14 不捕获系统全局快捷键，也不修改 Windows
Shell 注册表或启动项。

## 8. 测试和验证

实现阶段至少覆盖以下测试：

### 8.1 配置单元测试

- 缺失文件返回默认配置。
- 合法 TOML 能够完整往返读写。
- 非法主题回退到 `dark`。
- 非法尺寸和位置被拒绝或替换为默认值。
- 保存过程不会直接留下不完整目标文件。

### 8.2 恢复单元测试

- 没有运行标记时判定为正常启动。
- 已有运行标记时判定为上次未完成运行。
- 创建运行标记后能够读取诊断字段。
- 正常清理会删除运行标记。
- Windows 上真实启动非当前进程，验证其运行中 marker 保留、退出后 stale marker 清理；
  无法启动 `cmd.exe` 时测试必须明确 skip/return。
- Windows 上覆盖 PID liveness、creation-time 不匹配清理、sidecar ownership 和多实例
  收敛；无法确认 liveness 时 marker 保留。
- 崩溃日志写入失败不会让清理逻辑 panic。

测试使用注入的临时应用数据目录，不读写真实用户 `%APPDATA%`，避免测试之间互相影响。

### 8.3 Windows 构建和手工冒烟

在 Windows MSVC 工具链上执行：

```text
cargo fmt --all -- --check
cargo test -p rustnt-gui
cargo check -p rustnt-gui
cargo clippy -p rustnt-gui --all-targets -- -D warnings
cargo run -p rustnt-gui
```

手工冒烟需要确认：窗口能打开和关闭、主题切换可见、配置文件能够生成、再次启动能
读取配置、留下运行标记后重新启动会出现恢复提示，正常退出后运行标记被删除。

## 9. 验收标准

Task14 满足以下条件才算完成：

- workspace 包含独立的 `rustnt-gui` crate 和 GUI 二进制目标。
- GUI 可以在 Windows MSVC 环境编译和启动。
- 首屏具备状态展示、主题切换和配置保存能力。
- 配置路径固定为 `%APPDATA%\\RustNT\\gui.toml`，解析失败有安全回退。
- 运行标记通过 Windows PID liveness 和 creation-time 检查区分运行中、stale 和无法
  确认的状态；stale marker 会被清理，无法确认时保留。
- 每个 GUI 实例拥有独立 sidecar，正常清理只删除自身 marker；多实例检查能够收敛而
  不互相覆盖或误删。
- 异常退出后下一次启动进入安全恢复状态并给出可见提示。
- 正常退出会删除运行标记，配置保存失败不会留下错误恢复状态。
- 新增测试通过，既有 workspace 测试、格式检查和 Clippy 不回退。
- 文档明确 GUI 仍运行在 Windows 用户态，并沿用现有服务和 capability 边界。

## 10. 后续演进

Task15 可以在 `app.rs` 的首屏框架上增加启动器、任务切换器和通知入口；Task16 可以
接入 Task12A 的只读文件系统能力。GUI 与现有服务之间的连接必须沿用固定 capability、
请求 ID、审计和授权边界，不通过 GUI 增加任意命令执行入口。
