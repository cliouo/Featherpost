# ADR-0050: 邮件翻译架构与热插拔外部翻译 Provider 插件总线

- **状态**：已接受（2026-11）
- **关联**：[ADR-0017](0017-m4-1-plugin-identity-and-auth-seam.md)、[ADR-0018](0018-m4-2-method-capability-enforcement.md)、[ADR-0019](0019-m4-3-plugin-install-transactions.md)、[ADR-0020](0020-m4-4-process-containment.md)、[ADR-0021](0021-m4-5-external-transport-provider.md)、[ADR-0030](0030-frontend-authorization-closure.md)、[ADR-0033](0033-html-reading-isolation-and-resource-channel.md)、[ADR-0041](0041-low-footprint-background-receive.md)、[ADR-0048](0048-locale-preference-and-decoupled-i18n.md)

## 背景

Featherpost Studio 需要支持安全、低开销的邮件正文与主题翻译，同时满足以下五项产品与架构约束：

1. **热插拔外部翻译 Provider**：允许第三方以独立进程包提供翻译能力（如 DeepL、Google、本地大模型或企业私有网关），在运行时安装、启停、升级、回滚、卸载而无需重编主程序；同时内置 V1 **OpenAI 兼容接口**（OpenAI / DeepSeek / 中转 / 本地 Ollama），复用 `maild-proxy` 全局代理路由。
2. **严守 ADR-0041 后台常驻资源红线**：常驻邮件守护进程（`featherpostd` / `featherpost-agent`）不得引入 HTML 重写器（`lol_html`）、翻译引擎 crate（`maild-translate`）或任何新的 HTTP/TLS 依赖与 feature。守护进程只承载轻量的 Cordis 注册表与反向 JSON-RPC 转发总线。
3. **严守 ADR-0033 沙箱隔离边界**：HTML 译文渲染与双语对照必须在 Rust 侧完成结构提取与安全重写，通过 `featherpost-mail://` 协议层输出至 `<iframe sandbox="">` 零 JS 容器，绝不在 WebView 注入脚本或让前端 JS 决定外发目的地。
4. **严格的外发授权、可取消并发与可撤销代次**：慢翻译绝不能阻塞同一条 Studio IPC 连接上的取消请求或读取下一封邮件；外部插件停用/卸载/升级/回滚/断线必须立即撤销已注册代次（包括 `independent` 插件），绝不静默降级至其他 Provider。
5. **极简前台阅读体验**：默认展示模式（双语对照 / 仅译文）收口在「设置 › 翻译」（`order: 85`）；阅读区工具栏仅保留一个一键「翻译 / 取消 / 显示原文」按钮加单行轻量外语检测横幅（未配置时隐藏横幅、保留工具栏按钮），不增加模式下拉菜单。

## 决策

### 1. 双层 Cordis 热插拔体系与进程职责边界

将邮件翻译拆分为彼此解耦的三层，避免后台守护进程膨胀：

| 层级 | 位置 | 职责与边界 |
|---|---|---|
| **守护进程热插拔总线** | `crates/maild/src/translation_provider.rs` + `external.rs` / `provider.rs` / `api.rs` | 通过 Cordis `ctx.provide` 挂载 `TranslationProviderRegistry`。管理外部 `translation_provider` 插件的动态注册、可撤销代次、并发/队列/帧大小预算、反调转发与 `translation/providers` 推送。**禁止**依赖 `maild-translate`、`lol_html` 或新增 HTTP/TLS 依赖/feature。 |
| **纯 Rust 翻译核心引擎** | `crates/maild-translate`（PR 2–3，仅由 `crates/maild-ui` 依赖） | 定义 `TranslationProvider` / `TranslationRegistry`、有序文本块与局部槽 DTO、内置 `OpenAiCompatibleProvider`（走 `maild-proxy` `ProxyPurpose::Translation`）、no-follow `0600` fail-closed 配置存储、内容寻址 SQLite 缓存、`detect.rs` 语种检测与防 `lol_html` 隐式闭合的双遍安全重写器。 |
| **前端壳（Tauri）** | `crates/maild-ui` + `packages/featherpost-studio`（PR 3–5） | 透传真实活动 `--config` 目录与缓存命名空间；通过 `ExternalTranslationProviderAdapter` 对接守护进程总线；经 `featherpost-mail://` 输出译文沙箱视图；在进入 `UiError` 前完成错误脱敏。 |

**ADR-0041 资源门槛**：因 `maild` 当前经 `maild-client` 已有 `reqwest 0.13.4` 依赖链，资源门禁以相同 target/feature 前后的 `cargo tree --offline --locked -p maild -e normal` 零依赖/零 feature 增量，以及 ADR-0041 同场景（含无插件、已注册空闲、并发峰值与排空后）的 release `phys_footprint` 实测为准。

### 2. 外部 `translation_provider` 包清单契约与保留命名空间

- 扩展 `ExternalCapability` 枚举新增 `TranslationProvider`（`snake_case`：`"translation_provider"`），支持 `schema_version = 2`（含 `description` 与 `[l10n.<tag>]`）、`protocol_version = 1`、`lifecycle = "supervised" | "independent"` 与可选 `ui_executable`。
- **保留内置命名空间**：`external.rs`、`plugin_transactions.rs` 与 `maild-client::inspect_plugin_package` 一律拒绝 `builtin` 及 `builtin.*`（大小写不敏感）作为外部插件 ID，防止外部包抢占 `builtin.openai_compatible` 等内建 Provider 标识。
- **真实环境变量名**：与 `crates/maild-brand/src/generated.rs` 保持唯一单源——`FEATHERPOST_PLUGIN_TOKEN`、`FEATHERPOST_PLUGIN_DATA_DIR`、`FEATHERPOST_CONFIG`。
- **V1 外部插件配置方式**：沿用可选的 `ui_executable`（复用受管单实例 Open 流程，仅传配置路径与数据目录，不注入 provider token）；无 GUI 的插件通过自有 CLI 或数据目录配置说明完成配置。
- `maild-client::inspect_plugin_package` 同步支持 `schema_version = 2` 的 `description`/`l10n` 字段（保持 v1 拒绝这两个键的 fail-closed 语义）。

### 3. 可撤销的多角色端点、单调代次与迟到应答修复

- **显式区分迟到应答**：在 `crates/maild/src/provider.rs` 引入 `ResponseRouteOutcome::{NotResponse, Routed, DroppedLateResponse}`。合法 JSON-RPC 应答帧若已无对应 pending（因超时、取消或撤销移除），计入迟到计数并返回 `DroppedLateResponse`，在 `api.rs` 读循环直接 `continue`，**绝不**落入 `parse_request` 误发 `-32600` 错误帧。
- **双角色连接隔离**：同一插件长连接允许同时注册 `transport_provider` 与 `translation_provider`。两者共享同一 `SessionCallState` 的单调数字 ID 分配器与 pending 路由表（按帧形状区分双向同号帧并原样回显 ID 类型），但每条 pending 携带 `CallOwner::{Transport(owner_id), Translation(generation)}`。替换或撤销任一角色只失败该角色对应的未决调用，绝不误伤同连接上的另一角色。
- **单调递增 `generation` 与全路径撤销**：
  - 每次 `translationProviderRegister` 分配全局单调递增的 `generation`（`>= 1`），latest-wins 顶替时立即撤销旧代次。
  - `setPluginEnabled(false)`（**覆盖 `independent` 插件**）、`removePlugin`、`commitPluginInstall`（升级）、`rollbackPlugin`、`refreshPlugins` 发现版本/能力变化、连接 EOF 与 daemon 停机，均立即撤销旧代次，唤醒排队等待者并以 `-32018` 失败在途调用。
  - 注册与每次派发（进队列前与拿到并发槽后）均向 `ExternalPluginManager` 重验包安装态、`enabled` 状态、包版本一致性与当前活跃 `generation`。

### 4. 同连接非阻塞长任务、有界并发与可取消通路

- **读循环非阻塞派发**：在 `api.rs::serve_with_shutdown` 中，仅将已鉴权的翻译长任务（`invokeTranslation`、`testTranslationProvider`）在同步完成鉴权、参数校验与预算预留后派发到会话级 `JoinSet` 中异步执行；`authenticate` 与其他会话变更保持在读循环串行执行。响应通过现有单写任务 `mpsc` 通道有序写回。
- **有界并发与预算**：
  - 每端点并发受 `Semaphore(max_concurrency.clamp(1, 4))` 约束；
  - 每端点排队深度上限 `4`、每调用会话在途上限 `8`、全局在途任务上限 `16`、全局在途载荷字节预算 `4 MiB`，满载立即返回 `-32022`（`PROVIDER_BUSY`）；
  - 批次上限取 `min(宿主上限, 插件声明上限)`（宿主上限 `64` 项 / `64,000` 字符）；
  - 双向序列化帧严格校验含末尾换行符 `\n` 不超过 `1 MiB`。
- **控制通路取消**：`cancelTranslation` 在读循环即时执行，校验调用会话与 `job_id`（跨会话取消拒绝 `-32016`），立即中止本地排队/等待并向已发出请求的插件尽力发送 `translation/cancel { request_id }` 通知（不阻塞调用方）。调用会话断开时自动取消该会话名下全部在途任务。
- **外部插件非阻塞要求**：示例插件 `plugins/example-translator` 采用读写分离 + 每翻译请求独立任务，确保慢翻译进行中仍能即时读取 `translation/cancel` 帧并中止工作。

### 5. 5 条 Additive JSON-RPC 方法、权限矩阵与稳定错误码

不提升 `PROTOCOL_VERSION`（维持 `1`）；全部为 additive 扩展：

| 方法 | 允许来源 | 允许主体与能力 | 核心契约 |
|---|---|---|---|
| `translationProviderRegister` | 同 UID 流式 IPC | 已认证且已启用 `Principal::Plugin` + `translation_provider` | 动态元数据收窄为 `supported_languages`、`max_batch_items`、`max_batch_chars`、`max_concurrency (1..=4)`、`supports_cancel`、`variant_revision`；展示名/l10n/包版本取自 manifest；触发 `translation/providers` 推送。 |
| `listTranslationProviders` | 任意已授权传输 | 持有 `plugins_manage`（与 `listExternalPlugins` 一致） | 合并已安装包、`enabled`/运行态、`has_ui`、`version`、`l10n` 与实时 `generation`/能力；返回目录事件 `generation`。 |
| `invokeTranslation` | 同 UID 流式 IPC | 受托 Studio `Principal::Frontend(dev.featherpost.studio)` + `messages_read` | 显式传入 `provider_id` + 期望 `generation` + `job_id` + 总 deadline；离线或换代即返 `-32018`，**绝不**静默回退至其他 Provider；严格校验返回项 ID 集合唯一且完整。 |
| `cancelTranslation` | 同 UID 流式 IPC | 受托 Studio `Principal::Frontend(dev.featherpost.studio)` + `messages_read` | 校验调用会话与 `job_id`（拒绝跨会话取消）；控制通路立即取消本地等待并尽力通知原插件。 |
| `testTranslationProvider` | 同 UID 流式 IPC | 受托 Studio `Principal::Frontend(dev.featherpost.studio)` + `settings_read` | 发送固定无邮件样本 `translation/probe { target_lang }`（超时上限 15s）；返回有限枚举的 `stage`/`reason`、`latency_ms` 与脱敏 `detail`。 |

- **服务端推送 `translation/providers`**：按 `plugins_manage` 权限过滤，携带单调递增目录 `generation` 供前端按代次合并（防止慢连接 `try_send` 丢帧导致状态陈旧；前端在首次、重连、滞后及进入设置页时拉取 `listTranslationProviders` 对账）。
- **稳定错误映射**：
  - `-32018`（`PROVIDER_UNAVAILABLE`，`data.reason = "unavailable"`）：未注册、已停用、已卸载、版本变化、代次不匹配、连接断开或被顶替；
  - `-32019`（`PROVIDER_TIMEOUT`，`data.reason = "timeout"`）：总预算（含排队与等待应答）超时；
  - `-32020`（`PROVIDER_ERROR`，`data.reason = "provider_error" | "invalid_response"`）：插件返回业务错误或畸形响应（详情经控制字符剥离与 256 字节截断脱敏）；
  - `-32022`（`PROVIDER_BUSY`，`data.reason = "busy"`）：端点队列、会话或全局并发/字节预算满载；
  - `-32023`（`PROVIDER_CANCELLED`，`data.reason = "cancelled"`）：已被 `cancelTranslation` 或调用方断线取消。
- **老 daemon 兼容**：不单靠 `-32601` 判定（已认证前端调用未知方法在老 daemon 上会命中默认 `UserOnly` 而返回 `-32016`）；前端结合兼容探测与能力降级，在老 daemon 下仅提供内置 Provider，绝不把已选外部 Provider 的请求改发内置。

### 6. 下游安全与极简 UX 约束（PR 2–5 锁定）

- **凭据与规范化服务身份绑定**：内置 OpenAI 兼容 Provider 的 `api_key` 绑定规范化服务身份（`scheme + host + effective port`，共用 origin 网关含 API 根路径）；修改 Base URL 后测试或保存均禁止继承旧 Key，拒绝同时 `clear` 与 `set`；等待首个有效代理快照后再允许外发。
- **活动配置路径与 fail-closed 存储**：`maild-ui` 透传真实活动 `--config` 父目录与缓存命名空间；`translation_preference.toml` 使用 no-follow 句柄校验、跨进程锁与 `0600`/Windows 私有 ACL 原子写入；持久化 SQLite 缓存独立存放（天然排除在主库备份外），通过 `clear_epoch` 防止清库竞态回写。
- **安全 HTML 双遍重写**：针对 `lol_html 2.9.0` 隐式闭合元素不触发 `on_end_tag` 的行为，建立可证明的文本归属状态机与原文回退契约，严格区分原文字面占位符与安全链接槽位；暗色模式通过可信生成属性施加样式，不受发件人伪造 class 影响。
- **极简阅读体验**：工具栏单按钮（直包 `<StudioTooltip>`，无原生 `title=`）驱动「翻译 / 取消 / 显示原文」；默认展示模式在「设置 › 翻译」（`order: 85`），仅改展示模式就地重绘已缓存结果、零外发；自动翻译显式绑定所选 Provider，切换 Provider 时暂停并需重新确认。

### 7. 分阶段 PR 交付

1. **PR 1（本阶段）**：守护进程热插拔总线、`ExternalCapability::TranslationProvider`、保留 `builtin.*`、`provider.rs` 迟到应答修复与双角色隔离、5 条 RPC + 非阻塞派发 + `translation/providers` 推送、`maild-client`/`maild-app` DTO 与事件接线、最小可运行示例插件 `plugins/example-translator`、ADR-0050 与协议/插件文档。
2. **PR 2**：`maild-proxy` 新增 `ProxyPurpose::Translation` + 新建 `crates/maild-translate` 核心引擎（配置存储、内置 OpenAI 兼容 Provider、SQLite 内容寻址缓存、语种检测）。
3. **PR 3**：安全 HTML/纯文本重写器 + `crates/maild-ui` Tauri 桥接、`featherpost-mail://` 译文视图与新增 reason 双语映射。
4. **PR 4**：Studio「设置 › 翻译」页面（`order: 85`）、插件热发现、脏守卫与扩展预览 `translation_provider` i18n。
5. **PR 5**：极简 ReadingPane 一键按钮、外语检测横幅、`⌘K` 命令接入、完整第三方插件开发者指南与真 WKWebView + ADR-0041 实测验收。

## 后果

- **收益**：守护进程以极小增量获得可撤销、可并发、可取消的热插拔翻译插件总线，完全不引入 HTML 解析或额外 TLS/HTTP 运行时开销；同步修复了既有 `provider.rs` 迟到应答误落入请求解析与双角色连接替换互伤两个隐患。
- **约束**：外部原生插件与守护进程仍处于同 UID 本地信任域（`getProxyConfig` 等本机能力边界已在文档明示）；强制要求受托 Studio 主体调用 `invokeTranslation` / `cancelTranslation` / `testTranslationProvider`，未来若需委托第三方前端，必须另行设计显式外发授权并评估 manifest 兼容。
