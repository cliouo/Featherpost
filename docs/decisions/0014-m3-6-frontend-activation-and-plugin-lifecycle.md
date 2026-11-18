# ADR-0014：M3.6 前端激活与插件生命周期

- 状态：已接受，M3.6-01 至 M3.6-04 已实现
- 日期：2026-08-21
- 修订：2026-08-29，[ADR-0024](0024-builtin-frontend-tauri-migration.md) 收窄 `builtin-egui` 的能力边界

> **ADR-0024 修订范围**：本 ADR 的激活协议、`OpenIntent` 路由、manifest 检查、回落规则与
> 生命周期四分类**全部不变**，且新的默认前端（随包预装的 Tauri package `com.maild.ui`）正是
> 沿 §9 的 `on_demand` + `macos_application` 路径交付、走 §7 的既有分类，不新增机制。
> 唯一变化是 §5 的 `builtin-egui` 从「随包恢复前端（功能完整）」收窄为**冻结的恢复界面**：
> 只保留读列表、读纯文本正文、改 frontend 默认项与账户启停、看 daemon 状态。
> §9「默认前端损坏 → Agent 配置停止；能力不足 → 该次 intent 回落 `builtin-egui`」保持不变。

> 2026-09-23：[ADR-0039](0039-native-recovery-and-tauri-default.md) 已决定取代下文的 egui 恢复壳与前端故障停内核行为。09-A 已交付恢复事务；新路由和故障隔离归 09-B/C，尚未切换。

## 背景

maild 的常态是 headless 内核：登录后由 `Maild Agent.app` 收信、落库、执行规则并发送
系统通知；完整 egui 窗口不应随登录启动。用户需要查看邮件、写信或修改设置时，才启动
前端。通知点击必须能打开精确邮件，同时不能把账户、UID、验证码、正文或 API token 放入
argv、环境变量、URL 或进程列表。

前端也是插件消费者。内置 egui 是随发行包提供的恢复前端，但不能成为内核不可替换的
组成；第三方前端以后可以成为默认消费者。第三方本地代码不以内核 Rust 动态库形式加载。

## 决策

1. 内核新增 `FrontendBroker` spine 服务和版本化 `OpenIntent` DTO。首版路由为：
   `inbox`、`message {account, uid}`、`compose`、`settings`、`background`。
2. intent 是有界、一次性的内存队列。默认总容量 64；队满淘汰最旧项；字段和 frontend ID
   有硬上限并拒绝控制字符或路径形态。
3. `claimNextOpenIntent {frontend}` 是 additive JSON-RPC 方法。调用者仍经过既有本地
   HTTP Bearer、Unix socket 或 Windows named pipe 身份边界；领取成功后该 intent 立即删除。
4. macOS 通知 identifier 只含进程内生成的普通序号。通知 target 保存在 Agent 内最多 64 项
   的临时映射；点击后删除并送入 broker，系统通知数据库、URL 与 argv 不出现邮件身份。
5. 内置 frontend ID 固定为 `builtin-egui`。Agent 只从严格的嵌套 bundle 路径通过
   `NSWorkspace` 激活顶层 `Maild.app`；激活失败时 intent 仍可由用户手动启动 UI 后领取。
6. egui 在启动和窗口重新获得焦点时领取 intent。`message` 可直接调用 `getMessage` 打开
   不在当前摘要页中的邮件；不会等待或扫描分页列表。
7. 后续生命周期分四类：
   - `embedded`：仅仓库内可信、ABI 同步的轻量核心插件；
   - `supervised`：独立子进程跟随 Agent，例如菜单栏 companion；
   - `on_demand`：按需启动的 egui 或第三方 GUI；
   - `independent`：用户自行管理的 CLI/TUI/自动化消费者。
8. 第三方插件默认进程隔离，通过版本化 API/事件协议交互。任意第三方 Rust `cdylib`、
   不稳定 ABI、同进程卸载/HMR 不属于当前实现；未来若需要高性能沙箱，另行设计 WASI seam。
9. frontend package v1 放在受管 `frontends/<id>/frontend.toml`，支持 `on_demand` 和
   `independent`。默认前端损坏会让 Agent 配置停止；能力不足时自动回到 `builtin-egui`，Menu
   也保留显式恢复入口。
10. 通用外部 package v1 放在受管 `plugins/<id>/plugin.toml`。`supervised` 只有进入配置
    allowlist 才由 Agent 启动，并使用有界指数退避和 LIFO 停止；`independent` 只登记，不由
    Agent 管理。声明 capability 是兼容性元数据，不是按进程授权。

## 兼容性

- 既有 `status` 和所有 DTO 不变；老 daemon 对新领取方法返回 `METHOD_NOT_FOUND`，仅影响
  激活功能，普通邮箱 UI 继续工作。
- 未装配 `[plugins.frontend]` 时返回 `SERVICE_UNAVAILABLE`，通知仍可显示但点击不会启动 UI。
- `OpenIntent` 对未知 route 解码为 `unknown` 并忽略，避免旧前端因未来路由整体崩溃。

## 已完成切片

- ~~M3.6-02：独立 `Maild Menu.app`、Agent supervisor、Inbox/Compose/Settings 动作和 Preview DMG。~~ ✅
- ~~M3.6-03：受检 frontend manifest、安装目录、默认前端切换与内置恢复入口。~~ ✅
- ~~M3.6-04：外部插件 capability、生命周期协议、故障退避与观测。~~ ✅

外部进程 v1 仍属于当前用户信任域：本地 API 不能识别具体 child identity，也没有 sandbox、
逐插件 token 或方法级 enforcement。supervised 插件不得 daemonize 或把工作转移给脱离监督的
后代进程；进程组/Job Object containment、市场签名和热更新需要后续独立协议。

通知点击的真实系统回调、已安装 bundle 激活和焦点跳转仍需要签名后的 macOS 13+ 真机验收。
