# Task15 桌面交互组件设计规格

**日期：** 2026-09-25  
**状态：** 已确认范围，待用户审阅规格  
**范围：** RustNT Shell 阶段的单窗口桌面交互层

## 1. 目标

Task15 在 Task14 GUI 基础框架之上增加 RustNT 自身的桌面交互壳层，使用户可以在一个 RustNT 窗口内浏览和切换已注册的内部页面。

本 Task 完成以下能力：

- 增加单窗口 Shell 布局：左侧内部导航、顶部状态区、主内容区。
- 增加 RustNT 内部页面模型和路由，初始页面包括 Overview、System Monitor 和 Windows & Sessions。
- 为 System Monitor 和 Windows & Sessions 提供明确的只读占位页，不伪造尚未接入的系统数据。
- 增加仅作用于 RustNT 窗口焦点范围内的页面快捷键。
- 增加本地内存通知模型、通知入口、已读和清空行为，并限制通知数量。
- 保留 Task14 的主题、配置、恢复状态和正常退出流程。

## 2. 非目标和边界

Task15 不实现以下内容：

- 不启动外部程序，不实现外部应用启动器。
- 不切换、激活、移动或控制其他 Windows 窗口。
- 不注册 Windows 全局热键，不修改 Shell、任务栏、桌面、启动项或注册表。
- 不接入 LocalSystem 服务、Named Pipe、驱动、内核接口或新的系统协议。
- 不读取或展示虚构的系统监控、窗口、会话或进程数据。真实数据接入分别沿用 Task11 和 Task13 的能力边界，由后续任务完成。
- 不实现文件浏览和文件操作；这些内容属于 Task16。
- 不实现窗口布局、工作区或虚拟桌面；这些内容属于 Task18。

GUI 仍然是 Windows 用户态程序。所有后续系统能力接入必须继续经过既有 capability、请求 ID、审计和授权边界。

## 3. 交互方案

采用单窗口 Shell：左侧内部导航、顶部状态区和主内容区全部位于同一个 eframe 窗口中。导航选择更新当前路由，主内容区只渲染当前路由对应页面；顶部状态区显示当前页面标题、GUI 状态和未读通知数量。

### 3.1 页面模型

新增 PageId 枚举，初始变体固定为 Overview、SystemMonitor 和 WindowsSessions。Overview 复用 Task14 的状态、主题、恢复提示和配置保存控件；另外两个页面显示能力说明和“数据源尚未连接”的占位信息，不展示伪造数据。页面顺序固定，标题、导航标签和快捷键索引使用稳定映射，不通过字符串比较决定顺序。

### 3.2 启动器和任务切换器

本 Task 中的“启动器”仅指 RustNT 内部页面导航入口；“任务切换器”仅指 RustNT 页面间切换，不代表 Windows 应用启动器或 Alt-Tab 替代品。左侧导航按钮切换当前页面。内部切换器使用当前窗口中的轻量弹出面板，不创建第二个原生窗口。打开后显示页面列表、当前选择和快捷键提示；选择页面后关闭。

### 3.3 快捷键

快捷键仅在 RustNT 窗口内响应：Ctrl+1 切换 Overview，Ctrl+2 切换 System Monitor，Ctrl+3 切换 Windows & Sessions，Ctrl+K 打开并聚焦内部页面切换器，Esc 关闭切换器；未打开时清除当前临时通知。处理组合键时须避免破坏文本输入控件中的用户输入。不得调用 Windows 全局热键 API。

## 4. 模块设计

在 crates/rustnt-gui/src/ 新增 navigation.rs 与 notifications.rs，并最小化整合 app.rs。

### 4.1 navigation.rs

负责页面路由、稳定标签、快捷键映射和切换器状态，不依赖文件系统、Windows API 或服务连接。建议提供以下稳定接口，具体输入事件类型可按 egui 0.36.2 调整：

PageId：Overview、SystemMonitor、WindowsSessions。
NavigationState：current: PageId、switcher_open: bool。
pages() 返回固定页面顺序；page_title() 和 page_label() 返回稳定文本；page_for_shortcut() 将 Ctrl 组合键映射到页面。

### 4.2 notifications.rs

负责本地通知生命周期，不持久化、不发送系统通知、不执行命令。通知包含类型（Info、Success、Warning、Error）、消息文本和已读状态。通知中心提供添加、未读计数、标记已读和清空行为，最多保留 32 条；超出时删除最旧通知。入队前清理换行并将消息限制为 512 个字符。Task14 的配置加载回退、恢复提示、保存成功或失败接入通知中心；页面切换反馈只能在操作发生时生成一次，不得每帧重复。

### 4.3 app.rs 整合

RustNtApp 持有 NavigationState 和 NotificationCenter，保留现有配置、路径和恢复字段。每帧先处理本地输入，再应用主题和 viewport 快照，之后绘制顶部状态区、左侧导航、当前页面和可选切换器。按钮动作发出一次性的通知事件。Overview 继续调用 Task14 的 save_config；正常退出和恢复逻辑保持不变。

## 5. 数据流和错误处理

导航和通知全部驻留于 GUI 进程内存：egui 输入更新导航状态，导航状态驱动页面绘制；用户操作和 Task14 状态产生通知事件，通知中心驱动状态栏与通知面板。导航输入无效时保持当前页面且不 panic、不产生错误通知；通知换行在入队前清理，超长消息截断。通知面板问题不能影响主页面。Task15 不新增需要权限的操作，因此页面切换和通知操作不连接服务、不触发 UAC 或授权操作。后续真实数据源必须另行设计 capability、审计和授权处理。

## 6. 测试要求

导航测试覆盖三个页面及固定顺序、稳定标题和标签、Ctrl+1/2/3 映射、无 Ctrl 或未知按键不切换、切换器开关和 Esc 行为。通知测试覆盖默认未读、未读计数、已读与清空幂等、超出 32 条时只保留最新 32 条、清理换行和截断消息。GUI 测试覆盖初始 Overview、页面切换不修改配置或恢复状态、两个占位页明确未连接数据源、Task14 状态通知只产生一次，并保证原有主题、配置、viewport 和恢复测试继续通过。

验证命令：cargo fmt --all -- --check；cargo test -p rustnt-gui -- --nocapture；cargo check --workspace --all-targets；cargo build --workspace --all-targets；cargo clippy --workspace --all-targets -- -D warnings；cargo test --workspace --all-targets；git diff --check。

## 7. 验收标准

- GUI 采用单窗口 Shell，包含左侧内部导航、顶部状态区和主内容区。
- Overview 保留 Task14 状态、主题、恢复和配置保存能力。
- System Monitor 与 Windows & Sessions 显示明确的未连接数据源占位，不伪造数据。
- Ctrl+1/2/3、Ctrl+K 和 Esc 在 RustNT 窗口内按规格工作，不注册全局快捷键。
- 通知中心最多保留 32 条，支持未读计数、标记已读和清空。
- 配置回退、恢复和保存结果可在 GUI 中查看。
- 不新增外部程序启动、其他窗口控制、服务协议、任意命令执行或权限绕过。
- 新测试、Task14 既有测试、workspace 构建、Clippy、格式和 diff 检查全部通过。
- 文档明确 GUI 属于 Windows 用户态，后续能力沿用既有安全边界。

## 8. 后续演进

Task16 可在 Shell 页面模型中接入 Task12A 文件系统只读能力；Task13 窗口查看能力可接入 WindowsSessions 页面。任何真实数据源接入都需单独定义 capability、请求 ID、审计和授权路径。Task17 之前不替换 Windows Explorer、任务栏或桌面。
