# 邮件领域元框架 — 架构设计 v0.1

> ⚠️ **方向更新（2026-08-18）**：内核从「Bun + TS cordis」转向「Rust 重实现 cordis 方法论」。本文件的产品架构（spine / 事件协议 / 插件缝 / 前端选型）仍有效，但语言相关部分（§2.3 Bun、§12 目录、M0 里程碑）以 [docs/rust-port-design.md](docs/rust-port-design.md) 为准。

> 对外品牌 **Featherpost**；早期工作代号 **maild**（mail daemon）仍是 crate 与源码目录的内部名。所有面向用户与操作系统的
> 名字来自仓库根的 [`brand.toml`](brand.toml)（[ADR-0047](docs/decisions/0047-decoupled-brand-and-open-source-readiness.md)、
> [品牌切换指南](docs/BRANDING.md)）。本文是蓝图，不是承诺：API 细节以 spike 验证为准。
> 方法论底座：[cordis](https://github.com/cordiverse/cordis)（A Meta-Framework of Spatiotemporal Composability，[论文](https://github.com/cordiverse/paper)）。
> 参考实现：[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（"everything is a plugin" 的 agent 领域实例）。

## 1. 愿景与形态

**一句话**：邮件领域的 "VSCode" —— 薄内核 + 插件生态，headless 优先。

- 核心功能只有收发件 + IMAP 同步，薄到极致；其余全部是插件。
- **两种运行形态**：
  - 守护进程（常驻，占 95% 时间）：同步邮件、跑规则、发系统通知。无窗口。
  - 按需前端（占 5% 时间）：改配置、写长邮件、批量操作时才拉起，用完即关。
- **首期平台**：macOS + Windows（通知与前端壳优先适配）；Linux 后补。
- **运行时**：Bun（详见 §2.3）。

形态约束（用户定下的硬要求）：

1. 最小资源占用：常驻部分只保留必要连接与内存。
2. 插件系统保证定制化（别名、转让、隐私邮箱等现有分散项目全部收编为插件）。
3. 前端可选、可无、可低占用；**不用 Electron 级常驻 webview**（放宽后的约束，见 §10）。

## 2. 方法论根基

### 2.1 cordis 五概念（摘自 harness 的 cordis-primer）

| 概念 | 在本项目的落点 |
|---|---|
| 插件 = 实现 Service 的对象 | 一切功能都是插件：IMAP 连接、通知、别名、验证码提取…… |
| 上下文 = 服务容器 | `ctx.accounts` / `ctx.store` / `ctx.receive` 等按 key 查找 |
| `inject` 声明依赖 | 启动顺序由依赖推导，无手工编排 |
| 类型化事件（emit / waterfall / parallel / serial / bail） | 邮件过滤链用 waterfall，通知广播用 emit |
| 注册是可逆副作用（`ctx.effect`） | 热重载、账户增删、插件启停自动撤销资源 |

### 2.2 vendor 策略

> 历史决策（TS M0 阶段）。Rust 内核落地后，TS 规格对照副本 `vendor/cordis/` 已随 ADR-0047 移除；`crates/cordis` 按上游
> [cordiverse/cordis](https://github.com/cordiverse/cordis) 的 tag 对齐（当前 `v4.0.0-rc.13`，`bc9fae7`），同步记录见
> [docs/rust-port-design.md](docs/rust-port-design.md)。

cordis 当前为 `4.0.0-rc.8`，README 明示 **API 未稳定**。做法与 harness 一致：

- 把 cordis 源码 **vendor 进 `vendor/cordis/`**，锁定 commit；
- rescope 为 `@maild/cordis`（harness rescope 为 `@deepseek-ai/cordis` 的先例）；
- 升级走同步流程：记录上游 SHA + 重放本地修改 + 全量测试（照搬 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) `vendor/README.md` 的流程）。

### 2.3 运行时选型：Bun（附降级预案）

- 理由：常驻内存更低（约 Node 的一半）、内置 SQLite（`bun:sqlite`）、原生 TS 执行（省 tsx）、可编译单文件二进制。
- 风险：cordis 的 Proxy 式 Context、loader 的 `!!js` 表达式求值在 Bun 下未经验证。
- **M0 门禁已通过**（2026-08-18）：Bun 1.3.14 下服务挂载 / 注入 / 类型化事件 / 可逆副作用全过，且 Bun 能正确剥离类型-only 导入直跑 cordis TS 源码。维持 Bun，无降级。loader 的 `!!js` 求值留待 M1 验证（见 [docs/m0-spike.md](docs/m0-spike.md)）。

### 2.4 语言与类型

- 全部 TypeScript，`strict: true`，ESM（harness 同款约束）。
- 事件、服务接口用声明合并注册，编译期校验（见 §5）。

## 3. 总体架构

```
                        ┌────────────────────────────────────────┐
                        │  云上邮件服务器（IMAP/SMTP / JMAP / Gmail） │
                        └───────────────┬────────────────────────┘
                          IDLE 长连接（每账户一条，空闲≈零流量）
                                        │
┌───────────────────────────────────────▼───────────────────────────────────────┐
│  maild 守护进程（Bun + TypeScript + vendor cordis）——常驻，~20–30MB             │
│                                                                               │
│  内核（spine 服务）： accounts · store · receive · transport · filters · api    │
│  插件： notify / alias / transfer / otp / disposable / rules / ……             │
│  配置： cordis.yml + overlay（支持 `!!js` 表达式）                              │
│                                                                               │
│  ┌──────────────┐  ┌──────────────┐  ┌─────────────────────────────┐         │
│  │ JSON-RPC     │  │ 本地 HTTP     │  │ 系统通知                     │         │
│  │ stdio/socket │  │ 127.0.0.1     │  │ macOS: UNUserNotification   │         │
│  │ （前端壳用）  │  │ （脚本/改设置）│  │ Win: toast (BurntToast/WinRT)│         │
│  └──────┬───────┘  └──────────────┘  └─────────────────────────────┘         │
└─────────┼────────────────────────────────────────────────────────────────────┘
          │ 按需拉起（95% 时间不存在）
┌─────────▼──────────────────────┐        ┌────────────────────┐
│ 前端壳（可选，未来里程碑）：      │        │ 脚本 / CI / 其他工具 │
│ Tauri（默认目标）/ 原生修复入口 / TUI │        │ 走 HTTP/JSON-RPC   │
│ 只是 API 的一个 consumer        │        └────────────────────┘
└────────────────────────────────┘
```

要点：**前端与守护进程之间只有 JSON-RPC 一个协议面**。前端是纯消费者，不承担任何业务逻辑——这保证了前端可换、可无、可极薄。

## 4. 服务骨架（spine）

内核只包含以下服务。每个都是独立 Service，可按需被插件替换实现。

| key | 职责 | 说明 |
|---|---|---|
| `ctx.accounts` | 账户与凭据 | 每账户：endpoint、协议、凭据引用（不存明文，指向 OS keychain）、同步状态 |
| `ctx.store` | 消息存储 | `bun:sqlite` + FTS5；表：messages / accounts / settings / aliases / transfer_vault |
| `ctx.receive` | 接收同步 | 每账户一条 IDLE 长连接，断线退避重连；新消息 → 发 `mail/received` |
| `ctx.transport` | 发送 | 走 §6 的 transport seam（SMTP / Resend 类 API） |
| `ctx.filters` | 规则引擎 | 本质是 `mail/received` waterfall 的编排者：规则可排序、可停用、可 per-account |
| `ctx.notify` | 系统通知 | 走 §6 的 notify seam；含"敏感内容预览"总开关 |
| `ctx.api` | headless 接口 | JSON-RPC server + 本地 HTTP，见 §8 |
| `ctx.scheduler` | 定时任务 | 轮询兜底、别名过期、静默时段，用 cordis 的 timer 包 |

反例（不进内核）：别名、转让、验证码提取、隐私邮箱、通知路由规则——全部是插件。

## 5. 核心事件协议

模式约定即公开契约（harness 用 `@mode` JSDoc 标注并由生成器校验，照搬）。

| 事件 | 模式 | 载荷 → 语义 |
|---|---|---|
| `mail/received` | **waterfall** | 过滤链：每个监听器 `next()` 委托，不调 `next()` 直接返回 = 丢弃该邮件 |
| `mail/filtered` | emit | 过滤完成后的观察广播（索引、学习、统计） |
| `mail/sent` | emit | 发送成功后的观察广播 |
| `otp/detected` | emit | 验证码提取结果（通知、剪贴板等消费者订阅） |
| `account/state` | emit | 账户连接状态变化（离线/在线/重连中） |
| `notify/route` | bail | 通知路由决策：策略监听器可直接返回最终通道 |

事件声明示例（声明合并，编译期校验分发模式与载荷类型）：

```ts
// packages/core/mail-events/src/index.ts
import type { Scoped } from '@maild/scope'   // per-account 作用域，harness 有先例

declare module '@maild/cordis' {
  interface Events {
    /** 新邮件进入过滤链。必须调用 next() 委托；直接返回即短路丢弃。@mode waterfall */
    'mail/received'(this: Scoped<MailboxCtx>, msg: IncomingMessage,
      next: () => Promise<FilterDecision>): Promise<FilterDecision>
    /** 验证码提取成功。@mode emit */
    'otp/detected'(hit: OtpHit): void
    /** 通知路由决策，返回最终通知通道。@mode bail */
    'notify/route'(route: NotifyRoute, ctx: RouteCtx): NotifyRoute
  }
}
```

## 6. 插件缝（capability seam）：Definition / Provider / Consumer

沿用 harness 的铁律：**一条缝三个角色，缺一不可**；角色只有独立演化时才拆分。

| 缝 | Provider（现成实现） | Consumer |
|---|---|---|
| `receive` | imap-idle（v1 主力）、jmap、gmail-api、imap-poll（轮询兜底） | `ctx.receive`、store、filters |
| `transport` | smtp、resend、mailgun（走 Web API） | 发送路径（UI/API 命令） |
| `notify` | macos（UNUserNotification）、windows（WinRT toast）、dev-log（测试用） | otp、rules、api 推送 |
| `alias` | catch-all（自托管域名）、plus-addressing、simplelogin 类 API、temp-inbox | 别名管理命令、compose 时的发件人选择 |
| `transfer` | self-hosted（Maddy/Mailcow API：干净重签）、provider-api、encrypted-bundle（凭据加密包，显式标注高风险） | 转让命令、市场类集成 |
| `dns` | cloudflare、porkbun 等（自定域的 MX/SPF/DKIM/DMARC 配通） | 自定域插件 |
| `credential` | macos-keychain、windows-credential（v1 可先 shell out，v2 原生库） | accounts |

每个 provider 都是一个插件：装上即多一种接入方式，卸掉即消失，不留残留（可逆副作用）。

## 7. 目标场景 → 插件映射

| 场景（来自既有分散项目） | 插件设计 |
|---|---|
| 无限别名 + 自定域 | alias 缝 + dns 缝：catch-all 接收、按需生成、自动配 DNS/SPF/DKIM/DMARC |
| Nodeseek 式邮箱转让 | transfer 缝：自托管后端走 API 重签（干净路径）；第三方无 API 时提供加密凭据包（高风险路径，UI 强制二次确认 + 风险文案） |
| 隐私随机邮箱 | disposable 插件：随机前缀 + `ctx.scheduler` 定时过期 + 到期自动转发主箱 |
| 验证码实时收 | otp 插件：监听 `mail/received` → 正则提取 → `ctx.notify` 直出验证码 → v2 自动进剪贴板 |
| 通知路由 | rules 插件：per-account / 静默时段 / 关键词重要度，走 `notify/route` bail 事件 |
| 资源占用焦虑 | 内核形态本身：守护进程 + 通知，无 UI 常驻 |

## 8. Headless API 形态

一个接口面，两种传输，都是插件的 consumer：

- **JSON-RPC over stdio**：前端壳用，并承载 `message/received` / `account/state` 服务端推送。
- **本地 HTTP `127.0.0.1`**：独立 CLI、脚本与"webapi 改设置"用；单 bearer token，token 存配置文件（Unix 强制 0600 权限）或由环境变量注入。HTTP 不承载推送。
- 可选：unix domain socket（macOS）/ named pipe（Windows），同协议换传输。

当前方法集（v1）：`status` / `listAccounts` / `listMessageSummaries` / `listMessages` / `getMessage` / `send` / `getSettings` / `setSettings` / `getNotificationStatus` / `requestNotificationAuthorization` / `openFrontend` / `claimNextOpenIntent` / `listFrontends` / `listExternalPlugins`；服务端推送：`message/received`、`account/state`。alias/transfer 等高级插件继续延后。

当前 `getSettings` / `setSettings` 只开放不含秘密的 rules 配置，采用运行时部分更新；不返回账户凭据、SMTP 凭据或 API token，也不写回 TOML（响应明确返回 `persisted: false`）。

**前端壳不需要任何特权**：它就是一个 JSON-RPC 客户端 + 渲染。

## 9. 通知子系统（两平台现实路径）

| 平台 | v1 实现 | 已知坑 | v2 方向 |
|---|---|---|---|
| macOS | 内嵌 Login Item Agent 通过 Rust `objc2` 调用 `UNUserNotificationCenter` | 正式授权归属仍需签名安装包和干净账户真机验证 | 增加 sound、actions 等可选能力，不改变 Agent 唯一身份 |
| Windows | 尚未实现，provider 明确返回 unsupported | toast 需要稳定 AUMID、安装身份和真机权限验证 | WinRT toast + 固定安装身份，Windows 阶段单独落地 |

统一进 `notify` seam，平台差异全部关在 provider 里。macOS 查询授权不弹窗，只有桌面或 CLI 的
显式动作会请求权限；拒绝、身份不匹配和非 macOS 平台都不回退脚本通知。**验证码进通知体**
必须受“敏感内容预览”与静默规则控制；策略服务未成功装配时 fail-closed。

## 10. 前端路径（资源占用 + HTML 渲染）

### 10.1 M3 选型标准与对比（历史记录）

> 以下标准与对比表是 M3 阶段的决策依据，保留为历史记录。其结论已被 §10.2 取代——
> 当时的隐含前提「邮件正文以纯文本消费」在日常使用下被证伪。

选型标准（M3 版）：**开窗期间空闲 CPU ≈ 0 + RAM 基线最低**。两点前提认知：

- 原生 ≠ 最省：AppKit/WinUI/XAML 同样加载 OS UI 运行时；macOS Window Server 与 Windows DWM 合成器本就强制 GPU 合成。
- GPU 参与不是问题：合成无法避免；只要框架实现按需重绘（immediate mode 的 egui/GPUI 均已实现），窗口静止时 CPU ≈ 0%。

对比（极小文本 UI 的量级基线，非精测；以 M3 基准 spike 实测为准）：

| 框架 | RAM 基线 | 空闲 CPU | 许可证 | 备注 |
|---|---|---|---|---|
| ratatui (TUI) | 5–10MB | ~0% | MIT | 绝对下限，永远保留为"终端视图" |
| Slint | 10–20MB | ~0% | GPLv3 / royalty-free（桌面免费、无需署名）/ 商业 | 官方 FAQ：运行时 <300KiB，UI 编译为原生码，为嵌入式设计；GPU/软件渲染可切换 |
| egui | 15–30MB | ~0%（按需重绘） | MIT/Apache | 生态最大，immediate mode |
| iced | 20–40MB | ~0% | MIT | retained mode，Elm 风格 |
| GPUI | 50–90MB | 低 | Apache-2.0 | 文本栈最强但最重；crates.io 仅 unstable，靠 git 依赖/gpui-unofficial；Windows 最弱（Zed 移植多年仍不稳） |
| 原生 AppKit/WinUI | 40–100MB | ~0% | — | 验证了"原生不更省" |
| Flutter | 60–120MB | 低 | BSD | 技术栈割裂，排除 |
| Tauri + React | ~90–180MB（实测，4 进程） | 空闲 ~0%（webview 挂起） | MIT/Apache | macOS 用系统 WKWebView（轻）；**Windows 换 WebView2（Chromium），footprint 显著更高，需在 Windows 重测** |
| Electron（对照组） | 300–800MB | — | MIT | 自带 Chromium（6+ 进程），正是要避免的 |

**内存口径**：跨应用对比看「内存」（phys_footprint = 实际内存 + 压缩内存），不要只看「实际内存」——webview 进程常有一大块 React 的 JS/DOM 闲置后被系统压缩，实际重量被隐藏了。

**硬约束修正**：从「无 webview」放宽为「**无 Electron 级常驻 webview**」。理由：本架构前端按需开关，webview 的重量只在开窗时瞬时存在，常驻的是 Bun 守护进程（~20–30MB）。因此前端框架不必追求绝对最低，DX 与生态权重上升。

**M3 结论（已被取代）：默认前端 egui**——MIT/Apache、crates.io 稳定发布、生态大、macOS/Windows 均成熟、有软件渲染降级、内存更低、无 borrow 检查器摩擦。GPUI 仅保留为「文本渲染质量兜底」。

低耦合由 JSON-RPC seam 保证，与框架选择无关；未来可增补其他语言的前端（SwiftUI / WinUI / 任意），都只是 seam 的另一个 consumer。这一条在换框架后依然成立，并且正是换框架代价可控的原因。

### 10.2 2026-08-29 修订：HTML 渲染成为硬需求，默认前端改为 Tauri

M3 选 egui 的隐含前提是**邮件正文以纯文本消费**——这也正是 JSON-RPC v1 明确不承诺 HTML
渲染的原因。日常使用证伪了这个前提：现代邮件以 HTML 为主，`text/plain` 分支常缺失或退化，
纯文本降级不构成可用的阅读体验。

忠实渲染 HTML 邮件只有真浏览器引擎能做到——Apple Mail 用 WebKit、Thunderbird 用 Gecko、
Mailspring 用 Chromium，无一例外；HTML 邮件重度依赖 table 布局、内联 CSS 与 media query，
自研等价于重写一个 CSS 引擎。在 egui 内嵌 wry child webview 可以做到同窗口渲染，但结果是
wgpu 栈**叠加**完整 webview 栈，严格重于单一 webview 前端，且原生视图恒定覆盖在
immediate-mode surface 之上、与 egui 弹窗互斥。既然进程内终究要有 webview，混合形态没有正收益。

**默认前端改为 Tauri**，经既有 frontend seam 以随包预装的 frontend package（`com.maild.ui`）
交付；**egui 将完全移除**，独立原生入口管理后台与修复默认前端，不承担邮件阅读。
[ADR-0039](docs/decisions/0039-native-recovery-and-tauri-default.md) 取代早期保留 egui 的决定。
当前 09-A 已实现固定副本恢复事务与维护 IPC；09-B 已实现独立原生窗口、后台控制与故障隔离，待全量回归。安装包切换及删除旧壳归 09-C。

资源观不受影响——它从来不由 UI 框架承担：常驻的是纯 Rust Agent（每账户一条 IMAP IDLE），
UI 按需拉起、用完即关；对照组（常驻 GUI 的邮件客户端）必须让整个 webview 栈 24 小时驻留
才能维持同步。纯 Rust 的 blitz（Stylo + Taffy + Vello）在形态上与邮件契合且可与 egui 原生合成，
列为将来重评的触发条件，当前成熟度不足。

完整决策、备选方案、正文渲染隔离边界与信任边界声明见
[ADR-0024](docs/decisions/0024-builtin-frontend-tauri-migration.md)。

### 10.3 前端激活与插件边界

M3.6 起，内核以有界一次性的 `OpenIntent` 路由前端激活：登录只启动 Agent，不启动完整
UI；通知点击、菜单栏或其他可信插件向 `FrontendBroker` 提交 intent，默认 frontend 再经本地
API 领取。内置恢复前端是随包交付项，不是内核的一部分；默认前端同样走 frontend package
路径，**不享有特权通道**。第三方前端默认使用独立进程，不把
不稳定 Rust ABI 或任意动态库加载进守护进程。受检 frontend manifest 提供默认消费者和能力
路由；通用 external manifest 把进程明确分为跟随 Agent 的 supervised 与用户自管的 independent。
v1 capability 只表达兼容性，不构成按进程授权。完整决策见
[ADR-0014](docs/decisions/0014-m3-6-frontend-activation-and-plugin-lifecycle.md)。

### 10.4 macOS 桌面单宿主、窗口关闭/最小化与菜单栏生命周期守则

macOS 交付包采取 **「菜单栏锚定后台生命周期 + 按需单宿主窗口」** 交互契约，后续任何前端、启动器或打包改动均不得退化以下五条不变量（完整决策见 [ADR-0044](docs/decisions/0044-macos-single-dock-identity.md)、[ADR-0045](docs/decisions/0045-tray-only-minimize-and-close.md)、[ADR-0046](docs/decisions/0046-menubar-lifecycle-and-dock-quit.md)）：

| 场景 / 用户动作 | 交互表现（Dock / 窗口 / 顶部菜单栏） | 进程与资源语义 |
|---|---|---|
| **打开 `Maild.app`** | 全程**最多只出现一个** `Maild` Dock 图标（绝不同时出现 `Maild` 与 `Maild UI` 两个图标） | `maild-launcher` 经 `NSWorkspace` 独立拉起 `Maild Agent.app`（`LSUIElement = true`），随后原位 `exec` 同包 `Contents/MacOS/maild-ui`，持有 `.main-ui.lock` 单实例锁 |
| **点击窗口红叉 `×` / 按 `⌘Q` / Dock 右键图标点「退出」** | **Dock 图标彻底消失**（无活动图标、无「最近使用」残留、无「Maild 正在后台运行」幽灵图标），仅保留顶部菜单栏 `✉` | 先刷盘未保存草稿（支持上传中/未保存设置的确认否决），主线程同步隐藏窗口并切回 `ActivationPolicy::Accessory`、待 WindowServer 提交后退出 `maild-ui`（WebView 内存归零），后台守护继续运行 |
| **点击窗口黄杠 `–` / 按 `⌘M` / 双击标题栏** | 保持 **macOS 原生最小化**（保留 Dock 应用图标与废纸篓旁的窗口缩略图，不劫持系统黄灯语义） | `maild-ui` 保持 `ActivationPolicy::Regular` 就绪态，点击 Dock 图标、缩略图或菜单栏均可一键丝滑还原 |
| **顶部菜单栏 `✉` 常驻与点击「退出 Maild」** | 顶部菜单栏 `✉` **与整个后台生命周期强绑定**（不设单独的「退出菜单栏」）；点击最后一项 **「退出 Maild」** 后，窗口、Dock 图标与菜单栏 `✉` 一并消失 | `MenuSupervisor` 在后台存活期间持续守护 `maild-menu`；点击「退出 Maild」调用 `stopManagedAgent`，守护进程向已连接前端推送 `agent/stopping`（350 ms 排空窗口），前端窗口、外部插件、菜单栏与后台守护全部干净退出 |
| **从菜单栏 `✉` / 系统通知 / Finder 唤醒** | 无论窗口处于已关闭态还是黄杠原生最小化态，均毫秒级还原主窗口、置顶聚焦并跳转对应视图（收件箱 / 写信 / 设置） | `FrontendIntentQueued` 经已认证 IPC 实时推送 `frontend/intent`（配合 `RunEvent::Reopen`），触发 `Regular` + `unminimize` + `show` + `set_focus` + `activate` 并一次性排空意图队列 |

## 11. 资源预算与实时性

- 常驻：Rust Agent + 可选轻量 Menu companion + 每账户一条 IMAP IDLE 连接；默认前端（Tauri）、
  恢复前端与第三方 on-demand frontend 均不随登录启动。**常驻占用不含任何 webview**——这是
  资源观的落点，与 UI 框架无关（ADR-0024 §愿景对照）。实际签名安装包的 footprint 继续以
  Activity Monitor 记录。
- 实时：IDLE 即推送，无需轮询；轮询 provider 仅作为兜底（scheduler 驱动，可配间隔）。
- 断线：指数退避 + 抖动，避免多账户同时风暴重连。
- 存储：SQLite 单文件；保留策略做成插件（按账户/时长裁剪）。

## 12. 目录结构（monorepo，harness 布局简化版）

> 历史蓝图（TS 版）。现行 Rust 仓库布局以 [README「仓库布局」](README.md#仓库布局) 为准；上游 cordis 以链接引用，不再 vendor。

```
vendor/cordis/            vendored cordis（rescope @maild/cordis；已于 ADR-0047 移除）
packages/
  core/mail-events/       事件协议与类型（§5）
  core/accounts/          账户服务
  core/store/             SQLite 消息存储
  core/receive/           接收服务 + imap-idle provider
  core/filters/           规则引擎
  core/api/               JSON-RPC + HTTP
  ext/notify/             notify 缝 + macos/windows/dev providers
  ext/transport/          smtp / resend providers
  ext/alias/              alias 缝 + providers
  ext/transfer/           transfer 缝 + providers
  ext/otp/                验证码提取
  ext/disposable/         隐私随机邮箱
  ext/rules/              通知路由规则
  ext/cli/                maild 命令行
  app/daemon/             组装入口（cordis.yml + bundle）
examples/                 可运行样例
docs/                     设计文档与决策记录
```

## 13. 安全边界

1. **凭据不持久化为明文**：只保存 keychain 引用；认证时以有界、短生命周期、可清零的
   secret buffer 临时取用，不能进入长期配置、Debug、日志、ViewState 或通知；`credential` 缝
   隔离平台差异。M3.7 的执行契约见
   [`docs/decisions/0015-m3-7-account-management-and-credentials.md`](docs/decisions/0015-m3-7-account-management-and-credentials.md)。
2. **转让信任模型**：干净路径（自托管 API 重签）与高风险路径（凭据加密包）在产品上分开呈现，高风险路径有强制确认与告警。
3. **通知敏感内容开关**：全局 + per-account 两层。
4. **本地 HTTP 只绑回环 IP** + bearer token；CLI 同样拒绝非回环 URL，不对外网开放。
5. **插件信任模型 v1**：只信任本地安装的插件（用户主动安装）；受管目录、manifest、显式
   allowlist 与进程隔离不是 sandbox。当前 API 认证同一用户而非具体 child identity，第三方市场、
   逐插件凭据、方法级授权与 process-group/Job Object containment 是后续独立工程。

## 14. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| M0 spike ✅ | Bun 上跑 cordis 最小样例；vendor + rescope 打通；**Bun 兼容性门禁** | 通过（Bun 1.3.14 + Node 24），见 [docs/m0-spike.md](docs/m0-spike.md) |
| M1 收发链路 | imap-idle provider + store + filters + otp + notify；用 Mailpit（本地假邮件服务器）做 e2e | 「收信 → 提取验证码 → 系统通知」跑通，两平台通知可见 |
| M2 headless | JSON-RPC + HTTP + CLI：改设置、发信、查信 | 脚本发信 / 改配置不拉 UI 可用 |
| M3 前端壳 ✅ | 协议面稳定后，做第一个壳（egui）；实测 CJK 渲染与资源占用 | 已完成；选型结论后被 ADR-0024 取代 |
| M4 生态插件群 | alias（catch-all + 自定域 DNS）、transfer、disposable | 三个既有分散场景在一个内核上全部可插拔 |
| M4.5 日常可用邮箱 | 邮件领域模型补齐（文件夹/标记/线程/附件/HTML/搜索/写路径）+ 默认前端迁移到 Tauri | 能当主力邮箱日用；见 [ROADMAP](docs/ROADMAP.md#m45日常可用邮箱客户端) 与 ADR-0024 |

## 15. 风险与开放问题

1. **cordis API 漂移**：按上游 tag 锁定对齐 + 升级流程（§2.2）是唯一防线；接受"内核升级滞后于上游"。
2. **Bun 兼容**：M0 门禁已通过（Bun 1.3.14）；loader 的 `!!js` 求值留待 M1 验证。
3. **平台发布外部证据**：macOS 已用独立 Agent bundle identity 收口通知与后台生命周期，但正式 ID、Developer ID/notary、干净账户授权/登录启动仍需外部门禁；Windows toast/AUMID、安装器和真机行为继续暂缓。
4. **IDLE 长连接的运营商/防火墙超时**：断线重连 + 可配置轮询兜底。
5. **转让的合规性**：凭据包转让仅面向用户自有资产；不做匿名托管市场（那不是插件能解决的产品问题）。

## 16. 上游参考

- [cordiverse/cordis](https://github.com/cordiverse/cordis) — cordis 本体（Rust 移植对齐 `v4.0.0-rc.13`，`bc9fae7`），读 `packages/core/src/{context,events,service,registry}.ts`
- [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) — 最佳实践样本：`docs/cordis-primer.zh.md`、`docs/architecture.zh.md`、`packages/core/*`、`vendor/README.md`（vendor 流程）
- [cordiverse/paper](https://github.com/cordiverse/paper) — 上游论文《A Programming Paradigm for Spatiotemporal Composability》
