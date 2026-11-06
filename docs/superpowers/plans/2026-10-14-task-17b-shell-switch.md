# Task17B 手动 Shell 切换实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 RustNT 增加默认关闭、需用户明确确认和 UAC 授权、可验证并可恢复的 Windows Shell 切换，以及 RustNT 启动失败时只回退一次 Explorer 的引导器。

**Architecture:** 新建纯 Rust `rustnt-shell-switch` 库，承载固定目标策略、事务/备份格式、状态机和 adapter traits；Windows 注册表/ACL 适配器只编入一次性 `rustnt-shell-helper`，进程启动适配器只编入 `rustnt-shell-bootstrap`。GUI 只显示只读状态并发起固定操作；所有离线测试使用临时目录和 fake adapters。

**Tech Stack:** Rust 2021 Cargo workspace、`windows-sys`、`serde` + TOML、Windows Registry/ACL/known-folder/process APIs、现有 `eframe`/egui GUI。

## Global Constraints

- 普通安装、更新、卸载和 GUI 启动不得修改 Winlogon；切换默认关闭。
- 仅访问 `HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon` 的 `Shell` 与 `Userinit`；切换只写 `Shell`，`Userinit` 只读和备份。
- GUI 不接收注册表路径、值名、可执行路径、任意参数、命令或脚本；助手仅接受固定操作参数。
- 管理员助手必须是一次性管理员进程；不得向 LocalSystem 服务增加注册表写入能力。
- 引导器不得写注册表、解释外部命令行、依赖 PowerShell，或因 GUI 退出而终止其他进程。
- 事务文件只保存固定事件、状态、摘要和受限错误类别；原始 Shell/Userinit 只保存在受保护备份中，日志不得写出原始值。
- 系统级文件使用 `%ProgramData%\\RustNT\\ShellSwitch`，仅 SYSTEM、Administrators 和 RustNT 管理边界可访问；GUI 状态仍放在 `%APPDATA%\\RustNT`。
- 路径、注册表类型、字符串长度、嵌入 NUL、事务版本、nonce、摘要、安装清单和 ACL 校验失败时拒绝写入。
- 每次注册表写入后必须读取并验证；恢复或回滚仅在当前值仍等于本事务预期值时执行，不覆盖外部修改。
- 自动化测试和当前开发机不得写真实 HKLM；真实 Shell 切换只在可快照回滚的 Windows 虚拟机验收。
- Task17A 现有启动事务、快照和恢复语义保持不变；Task18 需等本计划全部验收完成。

## File Map

- Modify `Cargo.toml`: 将新库、helper 和 bootstrap crate 加入 workspace。
- Create `crates/rustnt-shell-switch/Cargo.toml` and `src/lib.rs`: 固定策略、领域类型、事务协调、备份格式及 adapter traits；此 crate 不链接 Windows Registry/ACL 写入 API。
- Create `crates/rustnt-shell-switch/src/registry.rs`: 固定 registry value 类型验证和 `WinlogonStore` trait，不包含 Win32 adapter。
- Create `crates/rustnt-shell-switch/src/storage.rs`: 版本化事务、备份序列化、原子替换接口、大小上限和摘要校验，不包含 ProgramData ACL adapter。
- Create `crates/rustnt-shell-switch/src/transaction.rs`: prepare/switch/restore 状态机及并发锁。
- Create `crates/rustnt-shell-helper/Cargo.toml`, `src/main.rs`, `src/windows_registry.rs`, and `src/windows_storage.rs`: 严格参数解析、安装清单/ACL 验证、固定 Winlogon/ProgramData adapters 及一次性 UAC 助手入口。
- Create `crates/rustnt-shell-bootstrap/Cargo.toml` and `src/main.rs`: 验证安装、启动 GUI、等待健康握手、超时/崩溃时单次启动 Explorer。
- Create `crates/rustnt-shell-bootstrap/src/launch.rs`: 注入式启动器和等待策略。
- Modify `crates/rustnt-gui/Cargo.toml`: 依赖 `rustnt-shell-switch` 的固定操作 client。
- Create `crates/rustnt-gui/src/shell_switch.rs`: 普通权限状态读取、固定 helper 操作和 bootstrap 健康握手。
- Modify `crates/rustnt-gui/src/main.rs`, `app.rs`, and `navigation.rs`: 接入状态页/操作确认和受限恢复提示，不在 GUI 中写注册表。
- Modify helper/bootstrap/GUI manifests and workspace `Cargo.toml`: 启用各 Windows adapter 用到的精确 `windows-sys` features；不增加通用 shell/命令执行依赖。
- Create `docs/superpowers/verification/2026-10-14-task-17b-vm-acceptance.md`: VM 验收记录模板和手工恢复步骤。
- Modify `docs/roadmap.md`: 标记 17B 的实现计划和验收状态。

---

### Task 1: 建立跨平台策略库和固定领域类型

**Files:** Create `crates/rustnt-shell-switch/Cargo.toml`, `src/lib.rs`; modify workspace `Cargo.toml`.

**Interfaces:**
- `ShellOperation::{PrepareSwitch, Switch, Restore, ShowRecovery}`。
- `ShellState::{Explorer, RustNt, External, Unknown}`。
- `TransactionState::{Prepared, BackedUp, ShellWritten, ReadBackVerified, Committed, Failed, RestoreRequested, Restored}`。
- `RegistryType::{String, ExpandString, Other(u32)}`；`RegistryValue { value_type: RegistryType, raw: Vec<u8> }`。
- `Nonce([u8; 16])`，`RequestContext { pid: u32, timestamp: u64, nonce: Nonce }`，`ValidatedInstall { root: PathBuf, helper: PathBuf, bootstrap: PathBuf, gui: PathBuf }`；`ValidatedInstall` 字段私有，只由 manifest verifier 构造。
- `SwitchErrorKind::{InvalidArguments, UnsupportedRegistryType, InvalidRegistryString, InvalidInstall, InvalidManifest, UnsafeAcl, InvalidTransaction, BackupUnavailable, ExternalModification, LockTimeout, RegistryFailure, StorageFailure, UacCancelled, LaunchFailure}`；`SwitchError { kind: SwitchErrorKind, win32_code: Option<u32> }` 不存 OS 文本或原始注册表值。
- `ShellValueMetadata { value_type: RegistryType, byte_len: usize, sha256: [u8; 32] }`；`RegistrySnapshot::{Missing, Present(ShellValueMetadata)}`；`ShellTransaction { format_version: u32, nonce: Nonce, state: TransactionState, created_at: u64, request_pid: u32, shell_before: RegistrySnapshot, userinit_before: RegistrySnapshot, shell_target: ShellValueMetadata, events: Vec<AuditEvent>, error: Option<SwitchErrorKind> }`。
- `AuditEvent` 仅包含固定事件 enum、时间戳、状态和固定错误类别；原始字节只在事务内存和受保护备份使用。
- `SwitchError::kind(&self) -> SwitchErrorKind`；`RegistryValue::new(value_type: RegistryType, raw: Vec<u8>) -> RegistryValue`；所有类型派生 `Clone`, `Debug`, `Eq`, `PartialEq`，持久化类型额外派生 serde traits。

- [ ] **Step 1: 写状态转换失败测试**

在 `src/lib.rs` 的测试模块中定义每个允许转换和一个禁止跳转：

```rust
#[test]
fn transaction_state_rejects_skipping_backup_before_shell_write() {
    assert!(!TransactionState::Prepared.can_transition_to(TransactionState::ShellWritten));
    assert!(TransactionState::Prepared.can_transition_to(TransactionState::BackedUp));
}
```

- [ ] **Step 2: 运行测试确认失败**

运行 `cargo test -p rustnt-shell-switch transaction_state_rejects_skipping_backup_before_shell_write`。预期：crate/类型不存在导致编译失败。

- [ ] **Step 3: 添加最小 crate 和状态转换表**

实现仅允许规格中的顺序转换，`Failed` 只能从尚未提交的失败操作进入；`RestoreRequested` 只能从 `Committed` 或经 registry/backup 校验确认可恢复的 `Failed` 状态进入。拒绝所有未列出的状态边。

- [ ] **Step 4: 验证并提交**

运行 `cargo test -p rustnt-shell-switch`，预期全部通过；提交 `feat: add shell switch policy crate`。

### Task 2: 固定路径、注册表类型和安装清单校验

**Files:** Create `crates/rustnt-shell-switch/src/registry.rs`; create `crates/rustnt-shell-helper/src/windows_registry.rs`; extend `src/lib.rs` and helper manifest.

**Interfaces:**
- `trait WinlogonStore { fn read(&self, value: WinlogonValue) -> Result<Option<RegistryValue>, SwitchError>; fn write(&self, value: WinlogonValue, data: &RegistryValue) -> Result<(), SwitchError>; }` 只定义在纯 Rust policy crate。
- `WindowsWinlogonStore` 只定义在 helper crate；GUI 与 bootstrap 不依赖或编译该 adapter。
- `WinlogonValue::{Shell, Userinit}` 固定对应唯一值名；调用方不能传任意 key/path/name。
- `validate_registry_string(&RegistryValue, max_bytes: usize) -> Result<String, SwitchError>` 只接受 `REG_SZ`、合法 UTF-16、NUL 终止和规定长度。
- `InstallManifest` 只允许规格固定的三个 exe 文件名及格式版本；所有解析路径必须是固定 Program Files 安装根目录的子路径。

- [ ] **Step 1: 先写拒绝未知值测试**

```rust
#[test]
fn registry_string_validation_rejects_expand_string_and_embedded_nul() {
    let expand = RegistryValue::new(RegistryType::ExpandString, utf16z(r"%SystemRoot%\explorer.exe"));
    assert_eq!(validate_registry_string(&expand, MAX_REGISTRY_VALUE_BYTES).unwrap_err().kind(), SwitchErrorKind::UnsupportedRegistryType);
    let embedded = RegistryValue::new(RegistryType::String, vec![b'e', 0, 0, 0, 0, 0]);
    assert_eq!(validate_registry_string(&embedded, MAX_REGISTRY_VALUE_BYTES).unwrap_err().kind(), SwitchErrorKind::InvalidRegistryString);
}

fn utf16z(value: &str) -> Vec<u8> {
    value.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}
```

- [ ] **Step 2: 运行定向测试验证红灯**

运行 `cargo test -p rustnt-shell-switch registry_string_validation_rejects_expand_string_and_embedded_nul`。预期：缺少接口而失败。

- [ ] **Step 3: 实现固定目标和 Win32 adapter**

helper 的 `WindowsWinlogonStore` 固定打开 `HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon`，只读写 `Shell`/`Userinit`，并按精确类型映射 `REG_SZ`。helper 的 manifest verifier 使用 known-folder API 获取 Program Files，不信任环境变量或 GUI 传入路径；规范化并确认 manifest、helper、bootstrap、GUI 均位于 `Program Files\\RustNT`，且该目录不能由普通用户写入。策略库只处理注入的 trait，不链接 Registry API。

- [ ] **Step 4: 覆盖边界后验证**

定义 `MAX_REGISTRY_VALUE_BYTES: usize = 4096` 和 `RegistryValue::new`。增加未知 registry type、缺失值、空值、未终止 UTF-16、超长值、重解析点/路径逃逸、未知 manifest 字段、缺少 exe、普通用户可写安装目录拒绝测试。运行 `cargo test -p rustnt-shell-switch`；提交 `feat: validate fixed shell switch targets`。

### Task 3: 受保护事务存储和原子备份

**Files:** Create `crates/rustnt-shell-switch/src/storage.rs` and `crates/rustnt-shell-helper/src/windows_storage.rs`; extend switch/helper manifests.

**Interfaces:**
- `trait TransactionStore { fn load(&self) -> Result<Option<ShellTransaction>, SwitchError>; fn save_atomic(&self, tx: &ShellTransaction) -> Result<(), SwitchError>; fn save_backup_atomic(&self, backup: &ShellBackup) -> Result<(), SwitchError>; fn load_backup(&self, nonce: Nonce) -> Result<ShellBackup, SwitchError>; }`。
- `ShellTransaction` 有 `format_version`, `nonce`, `state`, `created_at`, `request_pid`, Shell/Userinit 元数据、摘要、事件和受限错误类别；不得含原始字符串。
- `ShellBackup` 保存原始类型和值，仅写入 ProgramData 受限 ACL 目录；TOML 事务文件和备份分别有固定最大字节数。
- `FileTransactionStore` trait 实现在 policy crate；`WindowsTransactionStore` 和 DACL 创建/验证只实现在 helper crate，GUI 不依赖 ACL 写入实现。

- [ ] **Step 1: 写敏感值隔离测试**

```rust
#[test]
fn transaction_serialization_contains_no_raw_registry_value() {
    let (tx, _) = fixture_transaction("original-shell-secret");
    let encoded = toml::to_string(&tx).unwrap();
    assert!(!encoded.contains("original-shell-secret"));
    assert!(encoded.contains("format_version"));
    assert!(encoded.contains("nonce"));
}
```

- [ ] **Step 2: 验证测试因缺少存储类型而失败**

运行 `cargo test -p rustnt-shell-switch transaction_serialization_contains_no_raw_registry_value`。

- [ ] **Step 3: 实现版本化、限长、原子存储**

先写同目录唯一临时文件，再 flush/close，原子替换目标；重新读取并校验格式版本、nonce 和 SHA-256 摘要。未知版本、损坏文件或超限文件必须保留原字节并拒绝覆盖。helper 的 Windows 存储 adapter 使用 known-folder API 获取 ProgramData，创建并验证目录 DACL 仅授予 SYSTEM、Administrators 和指定 RustNT 管理主体；临时/备份文件继承并验证同一受限 ACL。policy crate 的文件格式与原子替换逻辑通过注入的目录测试，不调用 ACL API。

- [ ] **Step 4: 验证失败路径和提交**

增加原子替换失败保留旧文件、备份摘要不匹配、损坏/未知版本保留、nonce 不匹配、超限、ACL 建立失败拒绝写注册表的测试。运行 `cargo test -p rustnt-shell-switch`；提交 `feat: add protected shell switch transaction storage`。

### Task 4: 实现 switch/restore 事务协调器

**Files:** Create `crates/rustnt-shell-switch/src/transaction.rs`; extend `src/lib.rs`.

**Interfaces:** `switch_shell(registry: &impl WinlogonStore, store: &impl TransactionStore, target: &ValidatedInstall, request: RequestContext) -> Result<ShellTransaction, SwitchError>`；`restore_shell(registry: &impl WinlogonStore, store: &impl TransactionStore, target: &ValidatedInstall, request: RequestContext) -> Result<ShellTransaction, SwitchError>`。`ValidatedInstall` 只能由 manifest/path verifier 构造；helper 将自身 `WindowsWinlogonStore`、`WindowsTransactionStore` 注入 policy crate。

- [ ] **Step 1: 写成功序列与外部修改拒绝测试**

```rust
#[test]
fn switch_backs_up_before_write_and_verifies_readback() {
    let (registry, store, install) = fixtures();
    let tx = switch_shell(&registry, &store, &install, request(41)).unwrap();
    assert_eq!(store.events(), ["backup_written", "state_backed_up", "shell_written", "readback_verified", "committed"]);
    assert_eq!(tx.state, TransactionState::Committed);
}

#[test]
fn restore_refuses_to_overwrite_an_external_shell_change() {
    let (registry, store, install) = committed_fixture("other-shell.exe");
    assert_eq!(restore_shell(&registry, &store, &install, request(42)).unwrap_err().kind(), SwitchErrorKind::ExternalModification);
    assert_eq!(registry.write_count(), 0);
}
```

- [ ] **Step 2: 验证红灯**

运行 `cargo test -p rustnt-shell-switch switch_backs_up_before_write_and_verifies_readback`。预期：协调器不存在而失败。

- [ ] **Step 3: 实现事务顺序和条件回滚**

获取有界等待的跨进程事务锁；读取并验证 Shell/Userinit；保存并重新读取备份后才能写 Shell；每次写后读取验证。失败时仅当当前 Shell 仍与本事务预期值完全一致才恢复原值。`Userinit` 全流程只读。恢复先核对备份 nonce/摘要和当前 Shell，外部值不匹配时不写任何值；成功验证后保留备份并进入 `Restored`。

- [ ] **Step 4: 覆盖中断和竞态并提交**

对注册表写失败、读回失败、锁超时、备份失败、事务中断后恢复、旧 nonce、Userinit 不变、外部并发改值和恢复读回失败各加注入测试。运行 `cargo test -p rustnt-shell-switch`；提交 `feat: implement guarded shell switch transactions`。

### Task 5: 创建固定操作的管理员助手和 GUI UAC client

**Files:** Create `crates/rustnt-shell-helper/Cargo.toml`, `src/main.rs`; create GUI `src/shell_switch.rs`; modify workspace and GUI manifests.

**Interfaces:**
- helper parser 只接受单一参数 `--prepare-switch`、`--switch`、`--restore` 或 `--show-recovery`；拒绝零参数、额外参数、路径、引号拼接和未知操作。
- GUI `ShellSwitchClient::request(operation: ShellOperation) -> Result<(), SwitchError>` 只由固定安装目录解析 helper 路径，不暴露字符串参数或路径参数。
- `--prepare-switch` 只验证安装、当前配置、备份目录和回退入口，不修改 Winlogon；`--show-recovery` 只读显示状态。
- `parse_args<I, S>(args: I) -> Result<ShellOperation, HelperError>` 只接受恰好一个 UTF-16 参数 token；空参数、未知 token 和多参数统一返回 `InvalidArguments`。
- GUI `UacLauncher` trait 接收 `&ValidatedInstall` 与 `ShellOperation`；仅 Windows `ShellExecuteExW` adapter 执行 `runas`，helper 入口永远不由用户字符串构造。

- [ ] **Step 1: 写固定参数解析测试**

```rust
#[test]
fn helper_rejects_extra_arguments_and_arbitrary_paths() {
    assert_eq!(parse_args(["--switch".into(), "C:\\temp\\payload.exe".into()]), Err(HelperError::InvalidArguments));
    assert_eq!(parse_args(["--restore".into()]), Ok(ShellOperation::Restore));
}
```

- [ ] **Step 2: 确认红灯并实现最窄入口**

运行 `cargo test -p rustnt-shell-helper helper_rejects_extra_arguments_and_arbitrary_paths`，再添加严格 allowlist parser 和通过 validated install resolver 获得的固定路径。

- [ ] **Step 3: 用 ShellExecuteExW 固定启动 `runas`**

GUI 明确确认后才发起 UAC；只能传一个 allowlist 参数，无自由文本。helper 启动后验证自身映像路径、Program Files ACL、manifest 和固定 sibling 文件。用户拒绝 UAC 时作为取消返回，不重试、不静默提权。

- [ ] **Step 4: 验证 helper 与 GUI client**

用注入的 launcher 测试操作到参数的一对一映射、UAC 取消、未知 exe、路径可写/manifest 错误拒绝、helper 退出状态和只读状态查询。运行 `cargo test -p rustnt-shell-helper -p rustnt-gui shell_switch`；提交 `feat: add fixed shell helper operations`。

### Task 6: 实现最小启动引导器和单次 Explorer 回退

**Files:** Create `crates/rustnt-shell-bootstrap/Cargo.toml`, `src/main.rs`, `src/launch.rs`; modify workspace `Cargo.toml`.

**Interfaces:** `trait ProcessLauncher { type Child; fn launch_gui(&self, install: &ValidatedInstall, ready_signal: &ReadySignal) -> Result<Self::Child, LaunchError>; fn wait_ready(&self, child: &Self::Child, timeout: Duration) -> ReadyOutcome; fn wait_exit(&self, child: &Self::Child) -> ExitStatus; fn launch_explorer_once(&self, system_path: &ValidatedSystemPath) -> Result<(), LaunchError>; }`。`ReadySignal` 是具名、带随机 nonce 的当前用户本地事件；`ReadyOutcome::{Ready, TimedOut, ChildExited, Failed}`；`ValidatedSystemPath` 只能由 Windows system-directory API 解析出固定 `explorer.exe`。

- [ ] **Step 1: 写 fake launcher 测试**

```rust
#[test]
fn gui_start_failure_launches_explorer_once() {
    let launcher = FakeLauncher::gui_start_fails();
    run_bootstrap(&launcher, &valid_install(), Duration::from_secs(30));
    assert_eq!(launcher.events(), ["launch_gui", "launch_explorer"]);
    assert_eq!(launcher.explorer_launch_count(), 1);
}
```

- [ ] **Step 2: 确认红灯**

运行 `cargo test -p rustnt-shell-bootstrap gui_start_failure_launches_explorer_once`，预期入口和函数缺失。

- [ ] **Step 3: 实现启动检查、健康握手和回退**

验证自身路径、manifest、事务状态以及固定 GUI 路径；创建带随机 nonce 的当前用户本地事件，将受限事件名传给 GUI 固定 bootstrap 参数并等待 ready，超时后启动系统目录 API 解析出的 Explorer 一次。GUI 健康后等待其退出；不杀死 GUI 或其他进程。引导器拒绝除该内部 handshake 参数之外的命令行参数且不写注册表。GUI 只接受严格格式的 bootstrap 参数，不据此加载路径或执行程序。

- [ ] **Step 4: 覆盖失败矩阵**

测试 GUI 创建失败、ready 超时、ready 前立即退出、健康后崩溃、manifest/事务不可恢复、Explorer 启动失败和不会重复回退。GUI 健康并正常退出时引导器直接退出，不额外启动 Explorer；只有启动失败、超时、崩溃或状态不可恢复时才启动 Explorer 一次。测试传入任意参数不改变行为。运行 `cargo test -p rustnt-shell-bootstrap`；提交 `feat: add shell bootstrap recovery fallback`。

### Task 7: GUI 状态、明确确认和独立恢复提示

**Files:** Modify `crates/rustnt-gui/src/app.rs`, `navigation.rs`, `main.rs`; create/extend `src/shell_switch.rs`.

- [ ] **Step 1: 写 UI action mapping 测试**

```rust
#[test]
fn shell_switch_actions_emit_only_fixed_operations_after_confirmation() {
    let mut state = ShellSwitchViewState::default();
    assert_eq!(state.confirm_switch(false), None);
    assert_eq!(state.confirm_switch(true), Some(ShellOperation::Switch));
    assert_eq!(state.restore(), Some(ShellOperation::Restore));
}
```

- [ ] **Step 2: 验证失败并实现安全 UX**

普通 Overview 可展示只读 Shell 快照和事务状态；切换前展示风险与恢复途径并要求单独确认。GUI 只调 `ShellSwitchClient` 固定操作 API。状态未知、损坏、外部修改或无有效备份时禁用切换，提供受限恢复提示。UAC 取消、helper 错误和外部修改各显示固定类别，不显示原始注册表内容。

- [ ] **Step 3: 接入 bootstrap readiness**

只有 GUI 完成初始化、恢复检查和首屏构造后才发送 bootstrap ready 信号；普通直接启动不需要该信号。增加测试确保启动失败时不发送 ready。

- [ ] **Step 4: 验证 GUI 与旧行为**

运行 `cargo test -p rustnt-gui`，检查 Task17A 恢复、配置、通知和普通启动测试仍通过；提交 `feat: integrate manual shell switch controls`。

### Task 8: 安装布局、更新/卸载保护和人工恢复文档

**Files:** Create `docs/superpowers/verification/2026-10-14-task-17b-vm-acceptance.md`; modify `README.md`, `docs/roadmap.md`; add manifest/path tests to switch crate.

- [ ] **Step 1: 写安装清单和卸载保护测试**

测试固定安装布局包含 `rustnt-shell-helper.exe`、`rustnt-shell-bootstrap.exe`、`rustnt-gui.exe` 和版本化 manifest；文件缺失或目录 ACL 不安全时拒绝启用。新增 `InstallGuard::ensure_restored_before_replace(&TransactionStore)`，当当前事务为 `Committed`、`Failed` 且可恢复、或事务损坏/未知时拒绝覆盖安装目录；只有无事务或 `Restored` 才允许未来的更新/卸载调用方继续。

- [ ] **Step 2: 实现受控安装验证**

安装根固定为 Windows Program Files 下的 RustNT 目录；helper/bootstrap 只解析固定 manifest 文件名。仓库目前没有 installer/updater/uninstaller，因此本 Task 只提供 `InstallGuard` policy API 和部署契约文档，不新增不存在的安装流程。未来打包/更新/卸载入口必须先调用 `ensure_restored_before_replace`。不得新增脚本解释器或任意命令执行入口。

- [ ] **Step 3: 写清人工恢复和 VM 矩阵**

验收文档逐项记录设计规格 7.2 的七个场景、VM 快照 ID、切换前后 Shell/Userinit 类型与摘要、UAC 结果、Explorer 回退结果和恢复证据。提供从任务管理器运行固定 `rustnt-shell-helper.exe --restore` 的手工步骤；不得要求当前开发机执行。

- [ ] **Step 4: 同步状态并提交**

路线图更新时间改为 2026-10-14，Task17B 状态固定为“设计已确认；实现计划完成；实现与 VM 验收待执行”。运行 `git diff --check`；提交 `docs: document Task17B deployment and VM acceptance`。

### Task 9: 全量验证和安全边界审查

**Files:** 全 workspace；只修复 Task17B 引入的问题。

- [ ] **Step 1: 跑定向离线测试**

运行 `cargo test -p rustnt-shell-switch -p rustnt-shell-helper -p rustnt-shell-bootstrap -p rustnt-gui`。预期：全部通过；不访问当前用户真实 HKLM。

- [ ] **Step 2: 跑 workspace 验证**

依次运行 `cargo fmt --all -- --check`、`cargo test --workspace --all-targets`、`cargo build --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`。记录现有基线警告并修复新增警告。

- [ ] **Step 3: 静态边界扫描**

确认生产代码中 HKLM 写入只在 helper 的固定 adapter；GUI/bootstrap 无 Registry write API；不存在 `Command::new`、PowerShell、`reg.exe` 或外部输入构造的执行路径；`Userinit` 没有写调用；Explorer 最多启动一次；事务/日志不序列化原始 Shell/Userinit。

- [ ] **Step 4: 仅在可回滚 VM 做真实验收**

记录规格 7.2 的全部矩阵证据。没有可用的快照虚拟机时，将 Task17B 标记为“离线验证完成，VM 验收待执行”，不得标成 Task 完成，不得开始 Task18。

- [ ] **Step 5: 完成审查和最终记录**

更新路线图和 VM 报告；确认当前开发机没有 Winlogon 写入记录、所有离线测试与扫描通过、VM 恢复证据齐全后，才将 Task17B 标记完成并提交 `docs: record Task17B verification`。

---

## 规格覆盖自检

- 固定 key/value/type/path 与安装清单验证：Task 2、Task 8。
- 安全备份、版本、nonce、摘要、原子写入和损坏文件保留：Task 3。
- 状态顺序、并发锁、切换读回、失败回滚和外部修改保护：Task 1、Task 4。
- 管理员一次性 helper、固定操作和 GUI UAC：Task 5。
- 引导器健康等待、超时/崩溃回退 Explorer 一次：Task 6、Task 7。
- GUI 风险确认、只读状态和独立恢复提示：Task 7、Task 8。
- 不改变 Userinit、LocalSystem/service 边界和 Task17A：Global Constraints、Task 4、Task 9。
- 注入式测试、workspace/Clippy/格式/边界扫描、VM 验收：Task 1-9。
- 更新/卸载前必须恢复：Task 8、Task 9。

## 执行前置条件

本计划的离线实现不得写真实 Winlogon。执行 Task 9 的真实切换前，必须准备可快照和回滚的 Windows VM，并确认可从恢复控制台或任务管理器启动固定 helper。若实现过程中发现固定安装目录或 manifest 的部署条件与现有发布方式冲突，应先更新设计规格并评审，再继续 helper 的注册表写入实现。
