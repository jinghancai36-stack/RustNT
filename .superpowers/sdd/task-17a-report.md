# Task17A Shell 快照、启动事务与恢复基础报告

**日期：** 2026-10-01
**状态：** 已完成文档、最终验证和边界审查
**范围：** Task17A；不包含实际 Shell 切换

## 1. 范围与结论

Task17A 为 RustNT GUI 建立了只读的 Windows Winlogon Shell 快照、启动事务、
有界恢复日志和异常退出后的恢复识别能力。快照只读取
`HKLM\\Software\\Microsoft\\Windows NT\\CurrentVersion\\Winlogon` 下的
`Shell` 和 `Userinit`，持久化内容只包含版本、时间、PID、目标、类型、结果、
受限长度和摘要，不保存完整注册表值。

Task17A 不执行 Shell 切换，不把快照作为切换授权，也不自动恢复或替换 Windows
Shell。Task17B 才进行手动 Shell 切换的独立设计评审。

## 2. 文件与实现

实现和验证涉及以下文件：

- `crates/rustnt-gui/Cargo.toml`：启用 Windows 注册表读取所需的现有
  `windows-sys` 能力。
- `crates/rustnt-gui/src/config.rs`：增加 `shell_snapshot.toml`、
  `shell_recovery.log` 和 `shell.session` 三个 `%APPDATA%\\RustNT` 路径，
  不改变既有 GUI 配置语义。
- `crates/rustnt-gui/src/shell_snapshot.rs`：注入式读取器、HKLM Winlogon
  只读适配器、版本化快照模型、受限元数据、摘要和原子保存/加载。
- `crates/rustnt-gui/src/shell_recovery.rs`：启动事务状态、版本化 session
  标记、有界事件日志、跨进程协调、nonce 所有权和正常退出清理。
- `crates/rustnt-gui/src/main.rs`：GUI 启动、快照降级通知、事务状态推进和正常
  退出顺序集成。
- `crates/rustnt-gui/src/app.rs`：启动降级通知只入队一次并显示给用户。
- `docs/roadmap.md`、`.superpowers/sdd/progress.md`：路线图和进度记录。

## 3. 提交记录

Task17A 实现和审查修正提交如下：

- `c83a3da`：增加 Task17A 应用数据路径。
- `d1795e7`、`6957c13`、`793c0fb`：注入式快照模型、元数据隐私和输入边界。
- `796e211`、`1d74606`、`ac85f30`：HKLM Winlogon 只读读取器及格式修正。
- `a055674`、`9a8c3fb`、`7edaa89`：版本化快照持久化、替换和失败保留测试。
- `5885919`、`3ddea84`、`2f11fdc`、`25f2070`：启动事务、日志边界、所有权和
  损坏文件保留修正。
- `686ef41`：GUI 启动和正常退出集成。
- `e8c4370`：Task17A GUI 集成进度记录。
- 本次文档提交：`docs: record Task17A verification`。

无关的 `.superpowers/sdd/task-14-task4-report.md` 工作树修改已保留，未加入本次
文档提交。

## 4. Task5/Task6 实现

### Task5：启动事务和恢复记录

Task5 实现了 `Starting`、`Running`、`SnapshotUnavailable`、`RecoveryRequired`、
`RecoveryRecorded` 和 `Completed` 等状态转换。启动时发现上一事务未完成会追加
一次 `recovery_required` 事件，再创建带 `format_version`、PID、启动时间和 nonce
的本次 session。恢复日志按大小限制保留最近的完整事件，session 和日志的未知版本、
损坏 TOML、无效 UTF-8、缺失字段和未知状态均返回错误并保留原始字节。

快照读取或原子保存失败只生成受限的 `snapshot_unavailable` 结果；session 创建、
session 更新或恢复日志写入失败会返回启动错误，阻止 eframe 打开窗口。正常退出先
记录 `completed`，再只删除当前持有的 session；窗口启动失败时保留恢复标记。

### Task6：GUI 启动和退出集成

Task6 在现有 GUI recovery marker 和配置加载流程旁接入 Task17A，不复制既有 PID
存活判断或崩溃日志语义。启动流程在打开 eframe 前完成 session、只读快照采集、快照
保存和状态推进；快照问题通过一条有界通知显示，GUI 可以继续启动。正常退出顺序为
保存 GUI 配置、完成 Task17A session、删除既有 GUI marker。测试覆盖通知只入队一次、
session/log 失败阻止窗口、eframe 错误保留两个恢复标记以及正常退出顺序。

## 5. 安全边界

- 注册表只读 `HKLM` Winlogon 的 `Shell` 和 `Userinit`，使用 `KEY_QUERY_VALUE`。
- 所有新文件只写入 `%APPDATA%\\RustNT`；快照和日志有版本、大小限制和失败保留策略。
- 不调用 `RegSetValue`、`RegDeleteValue` 或其他注册表写入接口。
- 不执行 Shell 切换，不修改 `Shell`、`Userinit`、启动项、策略或其他系统配置。
- 不启动、停止、重启或控制 Explorer 或其他外部进程。
- 不调用 PowerShell、`reg.exe`、WMI 或任意外部命令。
- 不申请管理员权限，不改变服务、Named Pipe、驱动、内核或授权边界。
- 不保存完整注册表值、密码、访问令牌或未经限制的环境变量内容。

边界扫描命令的输出包含测试专用匹配：`shell_recovery.rs:1048` 的
`Command::new` 用于 Windows 跨进程协调单元测试；`shell_snapshot.rs` 的
`explorer.exe` 仅出现在原始值排除断言和测试 fixture 中。未发现生产路径中的
注册表写入、外部启动或 Explorer 控制调用。该扫描结果按实际输出记录为“有测试
匹配”，没有改写成“零匹配”。

## 6. 错误策略与文件保留

快照采集失败或快照原子替换失败时，GUI 记录受限原因并显示一次降级通知，继续启动。
session 创建、更新、删除失败或恢复日志不可写时，启动流程返回错误并阻止打开 GUI。
正常退出删除的 session 必须仍属于当前 nonce，旧句柄不能删除或推进新事务。

快照、session 和恢复日志的 malformed、unknown version、无效编码或未知状态会保留
原文件并返回可读错误；新临时文件失败时只清理临时文件，不覆盖旧的有效快照或日志。

## 7. 最终验证矩阵

以下命令于 2026-10-01 在本工作树真实执行：

| 命令 | 实际结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 退出码 0，PASS |
| `cargo test --workspace --all-targets -- --test-threads=1` | 退出码 0，250 passed，0 failed |
| `cargo check --workspace --all-targets` | 退出码 0，PASS |
| `cargo build --workspace --all-targets` | 退出码 0，PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | 退出码 0，PASS |
| `git diff --check` | 退出码 0，PASS；仅有无关报告文件的 LF/CRLF 提示 |
| `rg -n "RegSetValue\|RegDeleteValue\|CreateProcess\|ShellExecute\|Command::new\|powershell\|PowerShell\|reg.exe\|explorer.exe" crates/rustnt-gui/src/shell_snapshot.rs crates/rustnt-gui/src/shell_recovery.rs crates/rustnt-gui/src/main.rs` | 退出码 0；匹配仅为测试专用 `Command::new` 和 `explorer.exe` fixture/assertion，详见第 5 节 |

Cargo 命令均重复输出既有警告：`crates/rustnt-gui/Cargo.toml` 中 `toml` 依赖版本
`1.1.6+spec-1.1.0` 的 semver metadata 会被忽略。该警告不是本次文档变更引入的，
也未导致命令失败。

## 8. GUI Smoke 状态与残余风险

本次没有执行真实 Windows GUI smoke test，因此没有把 GUI 窗口启动、强制终止、再次
启动并观察 `recovery_required` 的结果写成已执行。当前证据来自注入式单元测试、GUI
集成测试、工作区构建和代码边界审查。

残余风险是未经真实用户环境 smoke 验证的 HKLM 读取和 eframe 窗口生命周期；另有既有
Cargo manifest 警告需要后续单独处理。Task17B 仍需独立评审手动 Shell 切换、救援路径、
失败回退和用户进入 Windows 桌面的保证，不能从 Task17A 的完成推导出系统配置已改变。
