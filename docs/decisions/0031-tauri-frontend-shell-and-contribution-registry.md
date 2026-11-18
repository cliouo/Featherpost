# ADR-0031：M4.5-03 Tauri 默认前端骨架与贡献注册底座

- 状态：已接受，M4.5-03-A + E01 已实现（hermetic e2e + vitest 钉死）
- 日期：2026-09-11
- 适用范围：新 crate `crates/maild-ui`（Tauri 后端）、新 pnpm 包 `packages/maild-ui`（webview 侧）、
  `maild-app` 新增前端身份 bootstrap、frontend package `com.maild.ui` 的 manifest、ADR-0030 两条补遗
- 关联：ADR-0024（迁移决定与形态）、ADR-0030（前端授权闭环）、ADR-0014（激活与包生命周期）、
  ADR-0002（连接 actor）、[设计评审稿](../design/tauri/README.md)、[界面扩展设计](../design/tauri/UI-EXTENSIONS.md)

## 背景

ADR-0024 定了方向：默认前端从 egui 换成随包预装、可替换的 Tauri package。设计评审稿把界面、
任务流与扩展模型定下来了，用户认可主界面总体方向并要求默认前端本身是 UI 插件宿主。ADR-0030
补上了前端主体能拿到日用主路径所需的授权。

本 ADR 是第一片写代码的前端切片。它要回答的不是「界面长什么样」（设计稿已答），而是：

- 生产 JS 框架与构建链是什么，怎样让 daemon/Agent 的门禁**永远不需要 Node**；
- Tauri 壳以什么身份连 daemon，token 与端点细节怎样做到**不进 webview**；
- 「默认功能也走注册底座」在代码里是什么形状，怎样保证坏模块不拖垮邮箱；
- 一个还不能读 HTML、不能写信的骨架，怎样诚实地交付而不被误当日用版本。

## 目标

1. 可运行的 Tauri 壳：连接状态、账户、文件夹、列表、纯文本详情；离线只读、旧响应不覆盖。
2. 壳以 `frontend:com.maild.ui` 认证，bootstrap 之后不持有任何 `user` 连接（ADR-0030 决策 6）。
3. E01 贡献注册底座从第一天承载全部内置模块；重复 ID、坏模块隔离，不阻塞邮箱。
4. 前端产物可复现构建：lockfile 入库，pnpm 版本钉死；`cargo test --workspace` 不需要 Node。

## 非目标（明确留给后续切片）

- HTML 原位渲染、`cid:` 资源、远程图片策略——M4.5-04（依赖 G02 附件字节通道）。
  **已于 2026-09-12 由 [ADR-0033](0033-html-reading-isolation-and-resource-channel.md) 落地**；本 ADR
  决策 4「`body_html` 不出桥」仍成立——HTML 只以隔离文档的 URL 形式交给 iframe。
- 写信、回复、草稿、附件上传——08-A（依赖 G03/G04）。
- 已读/星标/归档/删除等写回、搜索、线程、批量——08-B。
- 账户向导、规则、地址、包管理界面——08-C。
- 第三方 UI 扩展加载与隔离——E02；扩展窗口——E03。
- `.app` 打包、签名、预装、默认前端切换——M4.5-09/10。

**本片交付物不是日用版本。** 它能浏览本地缓存，仅此而已；文档与 UI 内的空态文案都不得暗示更多。

## 决策

### 1. 技术栈：Tauri 2 + React 19 + TypeScript + Vite，无 CSS 框架

- **React**：贡献注册底座的自然形态是「一个贡献 = 一个组件」；第三方 UI 扩展的作者池以 React 为最大。
  Svelte/Solid 运行时更小但作者池小得多；Preact 兼容但生态摩擦不值得省那几十 KB。
- **无 UI 组件库、无 CSS 框架**：设计稿的 token 与规则直接落成 CSS 自定义属性（`tokens.css`），
  避免「两套组件样式系统」（DESIGN.md §2 明令）。宿主 CSS 不进邮件文档，邮件文档 CSS 不进宿主。
- **Vite** 做开发服务器与产物构建；**vitest** 只测宿主逻辑（注册表、控制器、偏好），不做组件快照。
- **pnpm workspace**：仓库根已经是 pnpm 项目（M0 spike），`packages/maild-ui` 登记为 workspace 包，
  单一 `pnpm-lock.yaml` 在根；`packageManager` 字段钉死 pnpm 版本。

### 2. 两种构建形态，门禁只用其一

| 形态 | Cargo feature | 资产来源 | 用途 |
|---|---|---|---|
| 开发 | 默认（无 `custom-protocol`） | `devUrl` → Vite 开发服务器 | HMR 迭代 |
| 内嵌 | `embedded` = `tauri/custom-protocol` | `frontendDist` 编译期打进二进制 | **交付形态**；M4.5-10 的 `.app` 用它 |

Tauri 的 codegen 只在非 dev 形态检查 `frontendDist` 是否存在。于是 `cargo test --workspace --all-targets`
编译 `maild-ui` 的默认形态**不需要任何前端产物**，daemon/Agent 门禁与 Node 彻底解耦——这是 ADR-0024
「Node 不进 daemon/Agent 构建」的落地形式。内嵌形态由独立步骤验证：`pnpm build` 后
`cargo build -p maild-ui --features embedded`。

### 3. 身份 bootstrap 住在 `maild-app`，不在壳里

`ActorOptions` 新增 `identity: Option<FrontendIdentity>`。给出身份时，actor 在每次建立 IPC 连接后、
发第一个快照请求前执行 ADR-0030 决策 6 的序列：

1. `RpcClient::call("revealPluginToken", { id, kind: "frontend" })`——IPC 下 `call` 每请求开一条
   新连接、应答即关，这就是那条**无法消除的短命 user 连接**；
2. 在长连接上 `authenticate(token)`，token 随即丢弃；
3. `whoami` 校验主体等于 `frontend:<id>`，写入 `ViewState.principal`。

三步任一失败 → `ViewState.identity_failure = Some(原因)`，按连接失败退避重试（用户可能正在安装包）。
**HTTP 端点直接拒绝**：`authenticate` 只接受同用户 IPC（ADR-0017），HTTP 会话恒为 `user`，
一个声称要以前端主体运行的壳不该静默降级成 `user`。

放在 `maild-app` 的理由：它是 ADR-0024 决策 4「Tauri 后端直接 link `maild-client` + `maild-app`」
的直接受益者——重连、退避、pending、快照都已在那里，身份只是连接建立后多三步；而且它在
`maild-app` 里可以用假 socket 服务器钉死，不需要 Tauri。

### 4. 壳持有的只是一个「桥」，webview 看不到协议

`maild_ui::bridge::Bridge` 是唯一与 daemon 说话的对象，包着一个 `ConnectionHandle`。Tauri command
是它的薄封装，每个 command 只接受结构化参数、只返回投影后的 DTO：

- `ui_snapshot` / 事件 `maild://state`：阶段、修订号、连接纪元、账户、主体、身份失败原因；
- `ui_list_folders`、`ui_list_messages`、`ui_get_message`、`ui_refresh`、`ui_frontend_info`、`ui_claim_intent`。

错误经 `UiError { code, message }` 投影：`offline` / `not_found` / `denied` / `failed`，**message 不带
路径、端点、原始异常**——那些留在 Rust 的 stderr。webview 侧的 `bridge.ts` 不实现协议、发现、DTO
校验或重连；它连 endpoint 是什么都不知道。

Tauri 的 ACL 层面：`build.rs` 用 `AppManifest::commands` 为上述 command 生成 `allow-*` 权限，
主窗口 capability 逐条列出。这不是对抗恶意扩展的边界（E02 才是），是「命令面是显式清单」的
最低纪律——UI-EXTENSIONS §8 要求的「schema 小、类型安全的 broker 命令是唯一入口」从此开始。

### 5. E01 注册底座的契约

`ContributionRegistry` 是可信、编译期模块的注册表（第三方经 E02 的独立 broker 进入，不直接 register）。

- **命名空间**：`moduleId` 形如 `maild.mailbox`；每个贡献 ID 必须以 `<moduleId>.` 为前缀。
- **原子提交**：一个模块的贡献整包校验、整包生效；校验失败整包拒绝，**已注册的其他模块不受影响**。
- **dispose 句柄按 token 作用域**：过期句柄不能吊销后来者的替换。
- **必需角色**：宿主声明 `mailbox`、`settings` 为必需角色，删掉唯一提供者的 dispose 被拒——
  「没有替代者时不能停用仍被使用的必需角色」（UI-EXTENSIONS §1）。
- **排序**：`order` 升序，相同时按 ID 字典序打破平局——渲染结果不依赖注册顺序。
- **三类贡献**：`commands`（带 `locations: navigation | toolbar | menu` 与受限的 `enabledWhen`）、
  `views`（工作区视图，可带 `role` 与侧栏伴随组件）、`settings`（设置分区）。
  UI-EXTENSIONS 列出的其余扩展点（徽标、阅读侧栏、底部面板、窗口）在对应切片按同一契约增加。
- **失败可见**：宿主用 try/catch 包住每次注册，失败记入诊断列表并显示在「设置 → 扩展」里，
  邮箱照常启动。

内置模块（`maild.mailbox`、`maild.settings`）与示例扩展（`maild.sample-excerpt`）用**同一个** `register`。
示例扩展是一个能在设置里开关的贡献包：工具栏命令、溢出菜单命令、工作区视图、设置分区各一，
用来在没有第三方之前就演示「同一命令出现在菜单与工具栏」「停用无遗留」两条验收。

### 6. 前端包 manifest 与开发安装

`crates/maild-ui/package/frontend.toml` 是 `com.maild.ui` 的 manifest（schema 2，全部路由 +
`mailbox_write` / `accounts_manage` / `addresses_manage` / `plugins_manage` / `notifications_read`）。
开发期 `lifecycle = "independent"`：用户自己 `cargo run -p maild-ui`，daemon 只排队 intent。
M4.5-10 改为 `on_demand` + `macos_application` 指向打好的 `.app`——**同一个 id、同一份授权声明**。

安装走已有事务：`maildctl plugin install --frontend crates/maild-ui/package`。壳未安装时不降级、
不回落 `user`：界面显示「Maild UI 尚未注册为前端包」与上面这条命令。

### 7. 偏好存储的边界

localStorage 只存 `theme` 与 `density` 两个非敏感值（`maild.ui.preferences.v1`）。账户、文件夹、
邮件身份、草稿、token、扩展状态一律不进浏览器存储（UI-EXTENSIONS §8）。文件夹选择与滚动位置
只活在本次会话内存。

### 8. ADR-0030 两条补遗

实现期撞出 ADR-0030 漏掉的两处，同日补进该 ADR 并在此记录理由：

- **前端主体隐含 `frontend_open`，且只能 claim 自己的 intent。** 「claim 排给自己的 intent」是
  前端的定义性动作（ADR-0014）；一个不能 claim 的前端不是前端，所以这不该是可选声明。
  `claimNextOpenIntent` 对 `Principal::Frontend(id)` 加校验：`frontend` 参数必须等于 `id`，否则
  `-32602`——不能替别的前端消费导航意图。`listFrontends` / `openFrontend` 保持在同一个词下。
- **`notifications_read` 进 `permissions` 词表。** 「设置 → 通知」要读系统通知授权状态；ADR-0018
  当年说前端 manifest 无法表达它、「未来以 additive 清单版本扩展」——schema 2 的 `permissions`
  正是那条路。不塞进 `settings` 路由翻译：那会静默改变已安装 v1 包的有效权限。

### 9. 窗口生命周期

单主窗口，label `main`。关闭窗口 = 退出 UI 进程（最后一个前端窗口关闭释放 UI 资源，UX-SPEC §11）；
daemon 不受影响——壳没有任何停止后台的调用面。macOS 用 `titleBarStyle = Overlay`，红绿灯落在
侧栏品牌栏（设计稿即如此），品牌栏是拖动区域。扩展窗口、多窗口归 E03。

启动与窗口聚焦时 `claimNextOpenIntent { frontend: "com.maild.ui" }`，意图经事件 `maild://intent`
交给 webview 导航。这是 ADR-0014 seam 第一次被一个真实前端消费。

## 兼容性

- daemon 协议零改动：无新方法、参数、错误码；`claimNextOpenIntent` 只对前端主体多一条参数校验。
- `maild-app`：`ActorOptions` 新增字段有 `Default`，`ViewState` 新增两个字段——`maild-desktop`
  不声明身份、行为不变（ADR-0030 决策 9 如实记录：egui 壳在 M4.5-09 之前仍以 `user` 运行）。
- `Cargo.lock` 新增 Tauri 依赖树；daemon/Agent 二进制**不 link** 任何 Tauri crate（独立 package）。
- 根 `pnpm-workspace.yaml` 增加 `packages` 列表；M0 spike 的根脚本不变。

## 测试策略（hermetic）

- `maild`：策略表与 e2e 补前端主体的 `claimNextOpenIntent` 自身/他人分野、`listFrontends` 可达、
  `notifications_read` 声明后 `getNotificationStatus` 可达而未声明拒绝。
- `maild-app`：假 Unix socket 服务器——reveal 走短命连接、长连接首帧必须是 `authenticate`、
  `whoami` 不匹配拒绝、reveal 失败落 `identity_failure` 且继续退避、HTTP 端点 + 身份在 spawn 即拒。
- `maild-ui`（Rust）：真 daemon（`maild::run_daemon_until`）+ 预置 SQLite 的 hermetic e2e——
  主体为 `frontend:com.maild.ui`；经桥调 `UserOnly` 方法得 `-32016`（证明桥走的是认证连接而不是
  `user`）；文件夹/列表/详情投影正确；错误投影不含路径。
- `packages/maild-ui`（vitest）：注册表的命名空间/重复/原子性/dispose 作用域/必需角色/排序；
  控制器的旧响应丢弃、纪元变化失效、离线只读；偏好读写只含两个键。
- 手工：`pnpm build` + `--features embedded` 起壳连 `dev-stack` 示例（预置假数据的临时 daemon），
  截图对照设计稿；WKWebView 上的真机行为（DPI、菜单、焦点）归 M4.5-10。

## 后果

正：前端轨第一次有了可运行、可测试的骨架；ADR-0014 的 seam 与 ADR-0030 的授权链被一个真实前端
消费；内置模块从第一天就走注册底座，后面接第三方不需要重写壳；daemon 门禁与 Node 解耦。

负：仓库多一条 Node 工具链与几百个 Rust 依赖（只影响 `maild-ui`）；React 的选择是永久性的
生态承诺；示例扩展是要维护的代码；`user` → `frontend` 主体切换意味着壳能做的事**少于**旧 egui 壳
——这是有意的，缺什么词就去 ADR-0030 的表里显式加，而不是回退成 `user`。

## 参考

- ADR-0024 决策 4（Rust 后端复用连接 actor）、决策 5（正文渲染隔离，本片未触及）
- ADR-0030 决策 1/6/9（边界的真实含义、bootstrap 形状、内置不做例外）
- ADR-0014 §5/§9（回落规则与 launch.kind v1）
- [DESIGN.md](../design/tauri/DESIGN.md) §2/§5/§9、[UX-SPEC.md](../design/tauri/UX-SPEC.md) §8/§11、
  [UI-EXTENSIONS.md](../design/tauri/UI-EXTENSIONS.md) §1/§7/§8/§9
