# ADR-0024：内置前端从 egui 迁移到 Tauri

- 状态：已接受（2026-08-29）；03-A/04/08 已实现，09 分片实施中
- 2026-09-23 修订：[ADR-0039](0039-native-recovery-and-tauri-default.md) 取代决策 3 的 egui 恢复壳，改为独立原生修复入口；09-A 已实现，09-B/C 待实施。下文旧决策保留为历史。
- 取代：ADR-0003 的选型结论（M3 egui 壳）
- 关联：ADR-0013（macOS 发布生命周期与门禁）、ADR-0014（前端激活与插件生命周期）、ADR-0018（capability → 方法授权）、ADR-0019（插件安装事务）

## 2026-09-11 实现前设计核验

用户要求先完成 [完整 UI/UX 设计](../design/tauri/README.md)，满意后再进入 M4.5-03。
本 ADR 已接受的是迁移方向。用户随后认可主界面总体方向，要求默认前端还能装载界面插件、面板和扩展窗口；补充见 [界面扩展设计](../design/tauri/UI-EXTENSIONS.md)，具体 SDK 与安全/窗口契约仍待新 ADR。

核验发现待修订事项，详见 [交付计划](../design/tauri/IMPLEMENTATION-PLAN.md)：

- 决策 2/8 所称现有授权完全足够不成立：回复准备、信箱写回、账户与地址管理为 UserOnly，前端插件主体会被拒；正式实现前先补授权 ADR，不能旁路为内置包偷偷提权。
  **已由 [ADR-0030](0030-frontend-authorization-closure.md) 处理（2026-09-11 实现）**：frontend
  manifest 增加与路由分开的 `permissions`，新增 `mailbox_write` / `accounts_manage` /
  `addresses_manage` / `plugins_manage` 四词，`status` / `prepareReply` / `sendFromAccount` /
  `requestNotificationAuthorization` 重新归类。决策 2 那句「内置前端不享有特权路径」由此第一次
  成立——代价是必须同时如实记录：**本仓核验时 `maild-desktop` 根本没有调用 `authenticate`，
  内置 egui 壳一直以 `user` 主体运行**，该上限从 M4.5-03 的 Tauri 壳开始生效，不是从今天。
- **M4.5-03-A 已落地（2026-09-11，[ADR-0031](0031-tauri-frontend-shell-and-contribution-registry.md)）**：
  `crates/maild-ui` + `packages/maild-ui`，Tauri 2 + React；壳以 `frontend:com.maild.ui` 认证；
  决策 4「Tauri 后端 link `maild-client` + `maild-app`、连接 actor 原样复用」按字面成立，身份
  bootstrap 就加在那个 actor 上。决策 5（正文渲染隔离）本片未触及——`body_html` 不出桥。
- 决策 3 保留 egui 恢复界面与用户后来“废弃旧 egui”的指令不一致。已由 ADR-0039 修订为独立原生修复窗口，按 09-A/B/C 实施；先建立恢复闭环再删除 egui。
- 不能把“egui + webview 严格重于 Tauri”或“常驻占用必然不变”当实测结论。资源预算以同设备、同数据、同窗口生命周期下的测量为准。
- **M4.5-G02 已落地（2026-09-12，[ADR-0032](0032-attachment-byte-channel-and-remote-content-policy.md)）**：
  附件字节经 `readAttachment` 按四元组分块取回（走既有 1 MiB 帧）；决策 5 那句「远程资源放行策略
  属于内核侧状态」现在有了实体——`reading_policy.toml` 与 `getMessage.remote_content`。渲染隔离本身
  （CSP、`cid:` scheme handler、请求拦截）仍归 M4.5-04。
- **M4.5-04 已落地（2026-09-12，[ADR-0033](0033-html-reading-isolation-and-resource-channel.md)）**：
  决策 5「正文渲染上下文与应用 UI 上下文必须隔离」的实体是 sandbox iframe + 自定义协议文档 +
  文档级 CSP + Rust 侧重写与导航否决；`cid:` 经令牌通道、远程资源按内核结论拦截并以本机计数
  服务器证明默认零请求。渲染层的验收条目（脚本/表单/`javascript:`/`file:`/IPC 被拒）已由该 ADR 的
  单测与真 WKWebView 驾驶覆盖。
- HTML/附件原文当前有缓存上限（HTML 256 KiB、单 part 2 MiB 内联），超限附件只有元数据且 `stored = false`；
  不能把原文有界存储误称完整渲染已交付。

以下保留 2026-08-29 的决策记录；涉及上述冲突的具体工程条款尚待对应新 ADR 取代，不依据旧文强行开始实现。

## 背景

ADR-0003 在 M3 阶段选择 egui 作为内置前端，前提是**邮件正文以纯文本消费**——这也正是 JSON-RPC v1 明确不承诺 HTML 渲染的原因（`docs/protocol/README.md`）。

该前提在日常使用下不成立。现代邮件以 HTML 为主，`text/plain` 分支常缺失或退化；当前 `extract_text` 在没有 text/plain 时用 `<[^>]*>` 正则去标签，只删标签不删标签间内容，`<style>` 与 `<script>` 的**内容**会原样进入正文。用户明确判定纯文本降级不可接受。

忠实渲染 HTML 邮件只有真浏览器引擎能做到——Apple Mail 用 WebKit、Thunderbird 用 Gecko、Mailspring 用 Chromium，无一例外。HTML 邮件重度依赖 table 布局、内联 CSS 与 media query，自行实现等价于重写一个 CSS 引擎。

在 egui 内嵌 wry child webview 可以做到同窗口渲染，但结果是 wgpu 栈**叠加**完整 webview 栈，严格重于单一 webview 前端；同时原生视图恒定覆盖在 immediate-mode surface 之上，egui 的模态与弹窗和正文区互斥。既然进程内终究要有 webview，混合形态没有正收益。

### 愿景对照

- **「邮箱领域的 VSCode」** 指薄内核 + 插件生态 + headless 优先，与 UI 渲染器无关——VSCode 自身即 Electron 应用。本决策不触碰内核、协议或插件边界。
- **headless 资源观** 由「常驻 Agent 是纯 Rust daemon + UI 按需拉起、用完即关」实现，与 UI 技术栈正交。常驻占用在本决策前后不变；对照组（常驻 GUI 的邮件客户端）必须让整个 webview 栈 24 小时驻留才能维持同步。
- **前端生态**：第三方前端若只能用 egui 编写，开发者池将直接成为生态上限。

## 目标

1. 内置前端忠实渲染 HTML 邮件，含内联图、CJK 排版、真实字重、文本选择与无障碍。
2. 经既有 frontend seam 交付，作为随包预装的默认前端，不成为内核不可替换的组成部分。
3. 保留可用的恢复路径：默认前端损坏时仍有内置界面能读信并改回配置。
4. 内核、JSON-RPC 协议、`maildctl`、Agent、凭据缝与插件边界零改动。
5. 常驻资源占用不变。

## 非目标

- 不改 JSON-RPC v1 契约。HTML 正文与附件所需的字段扩展（`bodyHtml`、附件索引）属于 store schema v3 切片，另行 ADR。
- 不向 daemon / Agent 侧引入 Node 运行时。
- 不做同进程 egui + webview 混合渲染。
- 不在本切片实现正文渲染策略细节（sanitize、CSP、远程图片放行）——它依赖 `body_html` 落库；本 ADR 只固定其边界（见决策 5）。
- 不交付 Windows / Linux 前端（受 ADR-0014 §9 `launch.kind` 限制，见「后果」）。
- 不做第三方 Tauri 前端的市场分发与热更新。

## 决策

### 1. 引擎：忠实渲染只用系统 webview

macOS 走 WKWebView（经 Tauri）。不自带引擎，不自研渲染器。

纯 Rust 的 blitz（Stylo + Taffy + Vello）在形态上与邮件契合——无需 JS，且可与 egui 合成进同一 surface、不产生 z-order 问题。但其成熟度不足，且 table 布局恰是其弱项而正是 HTML 邮件的主力。**列为将来重新评估的触发条件，当前不采用。**

### 2. 形态：Tauri 作为随包预装的默认 frontend package

```
Maild.app/                              顶层容器（不变）
  Contents/Library/LoginItems/
    Maild Agent.app                     收信、规则、通知（不变）
  └ 极简 egui 恢复界面                   见决策 3
frontends/
  com.maild.ui/
    frontend.toml
    Maild UI.app                        Tauri，默认前端
```

manifest 使用现有 v1 字段，无扩展：

```toml
schema_version = 1
protocol_version = 1
id = "com.maild.ui"
lifecycle = "on_demand"
capabilities = ["inbox", "message", "compose", "settings", "background"]

[launch]
kind = "macos_application"
path = "Maild UI.app"
```

- 安装/升级/回滚/移除走已有 `maildctl plugin install --frontend` 原子事务（ADR-0019），无新机制。
- `on_demand` + `macos_application` 经 LaunchServices 激活，**单实例由 LaunchServices 保证**——这正是 ADR-0014 §9 只接受该 kind 的原因。
- 首次安装把 `[plugins.frontend.config].default` 置为 `com.maild.ui`。
- capability 经 ADR-0018 翻译表换取 API 授权，与第三方前端走同一条路径——**内置前端不享有特权路径**。

### 3. egui 退守为冻结的恢复前端

保留 ADR-0014 §5 的 `builtin-egui` 稳定恢复入口语义，但能力边界收窄至：

- 列出账户与消息摘要；
- 读纯文本正文（含 HTML 降级文本）；
- 查看与修改 frontend 默认项、账户启停；
- 显示 daemon 状态、连接错误与插件装配失败。

**明确不做**：写信、账户向导、插件管理、rules 编辑、地址层管理。

ADR-0014 §9 的「默认前端损坏 → Agent 配置停止；能力不足 → 该次 intent 回落 builtin-egui」不变。冻结意味着不再新增功能；协议新增字段只做 serde 容忍，不做展示。

### 4. Tauri 应用的进程内分层：状态逻辑留在 Rust

Tauri 后端直接 link `maild-client` 与 `maild-app`，连接 actor 原样复用——退避重连、pending request、连接状态、通知归并、重连后快照 reconcile、HTTP 降级轮询一律不重新实现。

- 状态经 Tauri event 单向推送到 webview；
- 用户意图经 Tauri command 回到 Rust；
- webview 侧只做渲染与意图收集，**不实现协议、endpoint 发现、DTO 或重连语义**。

理由：`maild-client`（39 测试）与 `maild-app`（15 测试）保持有效，前端重写被限定在 UI 层。这与 Pebble 一类客户端有本质区别——它们必须在应用内实现整个邮件引擎，而 maild 的引擎在 daemon 里。

### 5. 正文渲染上下文与应用 UI 上下文必须隔离

邮件正文是攻击者可控输入。本 ADR 固定边界，实现细节随 schema v3 切片交付：

- 正文**不**进入应用 UI 的同一 DOM 上下文；使用独立 webview 或 sandbox iframe；
- 渲染上下文禁用 JS；
- 注入 CSP，默认 `default-src 'none'; img-src cid: data:`，用户显式放行后才放宽 `img-src`；
- 拦截全部导航，链接交系统浏览器；`javascript:` 与 `file://` 直接拒绝；
- 禁表单提交；
- `cid:` 内联附件经自定义 scheme handler 从附件存储解析，不经网络。

远程资源放行策略（按发件人白名单）属于**内核侧**状态，不由前端各自实现——否则更换前端即绕过防护。

### 6. 打包与发布门禁的迁移

ADR-0013 / M3.5-07 建立的是**发布流程与门禁**（universal2、签名、notary、staple、Gatekeeper、可恢复升级与回滚、保留数据卸载）。流程与证据格式不变，变的是被签名的载荷多一个 `.app`：

- `Maild UI.app` 必须与顶层 bundle 同一签名身份、同一 notary 提交、独立 staple；
- frontend package 的目录约束（非符号链接、当前用户拥有、group/other 不可写）对预装路径同样适用，且**必须在运行时校验**，不因预装而豁免；
- 预装采用 **no-clobber + 一次性标记**：记录「已预装过」，用户手动移除后升级不得重新装回。这与 M3.5-07 的用户级 no-clobber 安装语义一致。

### 7. 构建与 CI

- Node/pnpm 进入**前端**构建门禁；daemon 与 Agent 侧构建不引入 Node。
- 前端产物可复现构建，lockfile 入库。
- egui 恢复前端保留在 workspace 测试门禁内。

### 8. 协议代数：零改动

本切片不新增、不修改任何 JSON-RPC 方法、参数、响应字段或错误码。`listFrontends` / `openFrontend` / `claimNextOpenIntent` 与 capability → 授权翻译表原样使用。协议版本不提升。

## 迁移切片

| # | 切片 | 依赖 |
|---|---|---|
| 1 | store schema v3 + `parse_message` 修正（`Date` 头、`body_html` 落库、附件按 content-id 可取、文本降级重写） | 无。**唯一有时间成本的前置**——历史邮件越晚补越难回填 |
| 2 | Tauri 骨架：窗口 + Rust 后端 link `maild-client`/`maild-app` + 连接状态/账户/列表/纯文本详情 | 无 |
| 3 | 正文渲染：隔离容器 + CSP + `cid:` scheme handler + 远程图片按可信发件人放行 | 1, 2 |
| 4 | 功能对齐：写信、账户中心与向导、settings/rules、插件管理、地址层 | 2 |
| 5 | egui 收缩到恢复界面；默认前端切到 `com.maild.ui` | 4 |
| 6 | 打包门禁扩展：双 `.app` 签名/notary/staple、预装 no-clobber、恢复矩阵 | 5 |

切片 1 与 2 可并行，且切片 1 独立于本决策成立——即使不换前端也必须做。

## 备选方案与取舍

| 方案 | 结论 |
|---|---|
| egui + wry child webview 同窗口混合 | 拒绝：wgpu 栈与完整 webview 栈叠加，严格重于单一 webview 前端；原生视图恒盖在 immediate-mode surface 之上，egui 模态/弹窗与正文区互斥；换来的「同窗口」在按需拉起的 UI 场景下价值有限 |
| 维持 egui + 纯文本降级 | 拒绝：现代邮件以 HTML 为主，降级正文不构成可日用的阅读体验（用户明确判定） |
| 纯 Rust HTML 引擎（blitz/Stylo）画进 egui surface | 暂拒：无 JS 需求与邮件契合，可原生合成、无 z-order 问题；但成熟度不足，且 table 布局是其弱项而正是邮件主力。列为将来重评触发条件 |
| 独立进程 HTML reader，读信弹窗 | 拒绝：每封 HTML 信弹一个窗口，日常阅读割裂；且系统 webview 本身已是多进程沙箱，独立进程不带来额外隔离收益 |
| Electron 前端 | 拒绝：自带 Chromium 使按需拉起的启动开销与体积代价显著高于系统 webview；且 Tauri 后端为 Rust，可直接复用 `maild-client`/`maild-app` |
| 完全删除 egui | 拒绝：默认前端损坏时只剩 `maildctl`，对日用邮箱过于凶险；冻结的极简恢复界面维护成本接近零 |
| 保留完整 egui 作为第二前端 | 拒绝：两套完整 UI 的持续维护成本会同时拖慢两边 |
| Tauri 直接替换顶层 `Maild.app` 的 UI，不走 frontend seam | 拒绝：会让内置前端重新成为内核不可替换的组成，违背 ADR-0014 的既定边界；且放弃了让 seam 承载主路径所暴露的缺陷 |

## 安全与信任边界（诚实声明）

- **系统 webview 的进程隔离是平台保证，不是 maild 的保证。** WKWebView 与 WebView2 自身为多进程架构，网页内容运行在系统沙箱化的独立渲染进程中，宿主持有的是代理视图。这是本决策放弃「独立 reader 进程」的依据；平台行为变化会改变这一前提。
- **禁用 JS 与 CSP 是纵深防御，不替代引擎安全更新。** 系统 webview 随 OS 更新，这本身是选择它而非自带引擎的一个理由。
- **webview 永不接触 API token。** 所有 RPC 在 Tauri 的 Rust 后端发起；凭据边界不变——秘密只经 Keychain，不进 argv、环境变量、URL 或日志。前端侧不存在可读取 endpoint 凭据的路径。
- **Tauri 前端仍在当前用户信任域内。** ADR-0014 结尾的声明继续成立：本地 API 不识别具体 child identity，无 sandbox，capability 是兼容性元数据加 ADR-0018 的方法级授权，不是进程级授权。
- **预装前端与顶层 bundle 同签名身份**，意味着前端被篡改等价于 bundle 被篡改，Gatekeeper 与 staple 覆盖其一即覆盖两者；但目录权限约束仍需运行时校验，预装不构成豁免。
- **`recipient` / 别名等隐私元数据的既有纪律不变**：不进审计与日志，前端不得写入自有存储。

## 测试策略

- **Tauri 后端**：复用 `maild-client` / `maild-app` 既有测试；新增 command 层映射测试（用户意图 → actor 调用），断言无协议逻辑旁路。
- **前端**：组件与 reducer 测试，覆盖面对齐现有 egui reducer 测试所钉的状态机。
- **正文渲染隔离**：以构造的恶意 HTML fixture 断言——CSP 注入存在、远程资源默认被拒、`javascript:`/`file://` 导航被拦、表单提交被禁、`cid:` 经 scheme handler 解析且不发起网络请求。
- **token 不泄漏**：断言 command 表不暴露凭据，前端产物内不含 endpoint 秘密。
- **打包**：hermetic bundle 门禁扩展到两个 `.app`；签名/notary/staple 断言覆盖 frontend package；预装 no-clobber（用户移除后升级不回装）。
- **恢复路径**：默认前端损坏、缺 capability → 回落 `builtin-egui`；恢复界面能改回默认项。
- **降级矩阵**：旧 daemon + 新前端、新 daemon + 冻结 egui。

## 后果

- `maild-desktop` 约 12,000 行中的完整客户端功能作废，55 个测试随之重写；reducer 与状态语义可移植进 Tauri 的 Rust 后端，协议/DTO/actor 层（`maild-client`、`maild-app`）完全不受影响。
- ADR-0013 / M3.5-07 的发布门禁被重新打开一次（载荷扩展），流程与证据格式不变。
- frontend seam 从「设计存在」变为「主路径承载」，其缺陷会第一时间暴露——这是正收益，第三方前端将走已被自己验证过的路径。
- 第三方前端的技术栈门槛降到 web，生态上限打开。
- **Windows 前端交付仍被阻塞**：ADR-0014 §9 的 `launch.kind` v1 只接受 `macos_application`，raw executable 因单实例、回收与跨平台激活未定义而 fail-closed。扩 manifest 加 `windows_application` 并自行解决单实例，需要独立切片。
- ADR-0003 的选型结论被本 ADR 取代；ADR-0014 的 `builtin-egui` 语义保留，能力边界收窄至决策 3。

## 参考

- ADR-0003（M3 egui 壳、CJK 字体与按需重绘——本 ADR 取代其选型结论）
- ADR-0013（macOS 用户级安装、可恢复升级/卸载与正式发布门禁）
- ADR-0014 §5 / §7 / §9（前端激活、`embedded`/`on_demand` 分类、manifest 与回落规则）
- ADR-0018（capability → 方法授权翻译表）
- ADR-0019（插件安装事务）
- `docs/plugins/README.md`（frontend package v1 manifest、目录约束与 API）
- `docs/protocol/README.md`（v1 不承诺范围——HTML 渲染的协议侧扩展归 schema v3 切片）
