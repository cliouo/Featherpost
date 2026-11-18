# maild 路线图与剩余工作

> 最后更新：2026-11-17（**M4.5 全部仓库内切片（01–10、G01–G05）已完成**；同日落地低占用后台收信 [ADR-0041](decisions/0041-low-footprint-background-receive.md)、SMTPS 465 / `AUTH LOGIN` 与服务商自动发现 [ADR-0042](decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md)、智能通知与规则持久化 [ADR-0043](decisions/0043-persistent-rules-and-smart-notifications.md) 以及 Tauri 10 秒登录向导与日常收信体验，并完成全栈汇合与 release 守护进程资源实测（稳态 `phys_footprint` ≈ 5 MiB、空闲 CPU ≈ 0.001 %，见 [testing](testing/README.md#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)）。仓库内交付约 99%；剩余只有正式签名凭据、真机与真实服务商长期观察这类外部门禁，见 [PROGRESS](PROGRESS.md#剩余外部门禁1-边界)。）
>
> 这份文档描述 M2 headless 完成后的剩余主线。它是计划，不替代 `docs/PROGRESS.md` 的实现状态；每个阶段完成后先更新进度，再回填本页状态。

## 当前基线

已完成的基础能力：

- Rust cordis 内核 R0-R4、R1.5、realm、生命周期和 loader。
- 多账户 receive、SQLite store、filters、OTP、notify、rules、scheduler。
- SMTP 明文/STARTTLS、IMAP STARTTLS/隐式 TLS，以及 Stalwart 交叉验证。
- JSON-RPC over stdio、本地 HTTP、`maildctl`、消息详情和 rules settings API。
- M3-01 已抽出独立 `maild-client`，统一 CLI/桌面端的 endpoint 发现、传输、协议 DTO 与流式 IPC 原语；`maildctl` 已迁移复用。
- M3-02 已新增 `maild-app`，统一连接状态、pending、退避重连、通知归并、快照 reconcile 与 HTTP 轮询。
- M3-03 已新增 `maild-desktop`，完成 egui 窗口、系统 CJK 字体、按需重绘、响应式三栏与基础状态骨架（M4.5-09-C 起已删除，默认前端为 Tauri `maild-ui`）。
- M3-04 已完成账户导航、摘要分页、选择保持、按需完整正文、长正文虚拟化与键盘刷新。
- M3-05 已完成纯文本写信、字段校验、发送状态、失败保留和草稿离开确认。
- M3-06 已完成运行时 rules 加载、差异 patch、本地校验、失败保留和 runtime-only 边界提示。
- M3-07 已完成连接世代恢复、协议上限长正文、macOS release 窗口与资源验收，以及 native/Windows 门禁封板。
- M3.5-01 已为 SQLite store 建立 schema v1、事务迁移、结构校验和未来版本 fail-closed 边界。
- M3.5-02 已提供 SQLite Online Backup CLI，运行时一致快照经 schema/quick_check 校验且永不覆盖目标。
- M3.5-03 已让文件型 store 持有跨进程维护锁，并提供先落盘 rollback 再原子替换的离线恢复。
- M3.5-04 已统一平台配置/data/runtime 目录、三类进程的配置发现与 SIGTERM 有序关闭。
- M3.5-05 已建立 macOS 13+ 顶层 App、内嵌 Login Item Agent、显式注册状态 UI 和 hermetic bundle 门禁。
- M3.5-06 已让 Agent 以稳定 bundle identity 独占 `UNUserNotificationCenter`，并提供显式授权、状态 API 与有界单 worker 投递。
- M3.5-07 已提供 Agent 生命周期锁、用户级 no-clobber 安装、可恢复升级/回滚、保留数据卸载，以及 universal2/签名/notary/staple/Gatekeeper 门禁。
- M3.6-01 至 04 已完成前端激活、Menu、受检 frontend manifest 与外部进程生命周期骨架。
- M3.7 已完成受管账户目录、macOS Keychain 凭据缝、动态账户 manager、桌面账户中心、恢复矩阵 hermetic 钉死与内核版本握手一键重启（ADR-0015/0016）；剩余为真机外部验收。
- M4-01 至 07 已完成仓库实现；M4-07 的 NodeSeek Email provider 真实验收仍待执行，Cloudflare/DNS catch-all 方案保持冻结。
- M4-06 封板时的自动化基线：内核 35 + maild-paths 7 + maild 270 + maild-client 39 + maild-app 15 + maild-desktop 55 + maild-menu 0 = 421 个通过，另有 Stalwart 真服务器与 macOS Keychain 真机冒烟及桌面离屏视觉调试共 8 个 ignored 测试。M4.5-01 时点为 531 个通过、ignored 仍为 8；M4.5-G01 时点为 630 个通过、ignored 仍为 8。
- M4.5-01 已补齐消息模型并把 store 升到 schema v3：`Date`/会话头/Cc/Bcc/Reply-To、HTML 原文、附件与标记位落库，列表改按可信发信时间排序（[ADR-0025](decisions/0025-message-model-and-store-schema-v3.md)）。
- M4.5-06 已落地发信补全与 Sent 归档（schema v6）：Cc/Bcc/多收件人、会话头、HTML 备选正文与转发原附件，回复/转发草稿在内核成型（[ADR-0028](decisions/0028-compose-reply-and-sent-append.md)）。当时新文件附件上传是非目标（1 MiB 单帧装不下真实照片），后由 M4.5-G04 的分块 staging 落地（[ADR-0035](decisions/0035-draft-attachment-staging.md)）。
- M4.5-05 已落地写路径与离线队列（schema v5）：已读/星标/归档/删除本地立刻生效、写回排队，「结果未知」是独立终态，重连后按双护栏对账（[ADR-0027](decisions/0027-mailbox-write-path-and-offline-queue.md)）。
- M4.5-02 已落地文件夹同步与 schema v4：一封邮件的身份变成 `(account, folder, uid)`，收信改为 per-folder UID 游标增量抓取，`UIDVALIDITY` 翻转触发该文件夹重建，每账户仍只用一条 IMAP 连接（[ADR-0026](decisions/0026-folders-and-incremental-sync.md)）。
- M4.5-03 至 10、G01 至 G05 已全部完成：Tauri 默认前端（HTML 隔离阅读、写信/草稿/新附件、统一工作区、账户与设置、受管扩展）、原生修复窗口、egui 删除、发布门禁与兼容降级矩阵（ADR-0031 至 ADR-0040）；store 当前为 schema v10。
- 2026-11-17 日常收信增强已落地并全栈接线：冷启动 `initial_sync_limit` 有界窗口 + `syncMessages` / `fetchServerMessage` + `status.runtime`（ADR-0041），SMTPS 465 greeting / `AUTH LOGIN` / `discoverEmailProvider`（ADR-0042），上下文验证码、新邮件通知限流与 `rules_state.toml` 侧车（ADR-0043），以及 Tauri 登录向导、立即同步、服务器历史检索与按需拉回。`crates/maild/examples/footprint_bench.rs` 用 release 子进程实测资源基线。

M2.5、M3、M3.5 与 M3.6 的仓库内实现均已封板；正式 macOS 发布仍需组织身份、签名凭据与干净真机证据，Windows 真机项按用户决定暂缓。

**2026-08-29 调研结论**：外壳（插件事务、containment、凭据缝、Agent/通知/签名发布、协议）已越过
同类开源客户端，但**邮件领域模型仍停在 R4 演示形态**——一张扁平的
`messages(uid, account, sender, recipient, subject, body, received_at)`，没有文件夹、标记位、
会话线程、附件、HTML 原文、搜索，也没有任何回写服务器的路径。`protocol/README.md` 里那句
「v1 不承诺附件、HTML 渲染、全文搜索、文件夹管理、已读/未读或草稿」现在正好就是缺口清单本身。
M4.5 补齐这一层，并因 HTML 渲染的硬需求把默认前端迁移到 Tauri（[ADR-0024](decisions/0024-builtin-frontend-tauri-migration.md)）。

调研中同时定位到两个正确性缺陷，归入 M4.5-01：`parse_message` 用 `SystemTime::now()` 充当
`received_at` 且全仓不读 `Date` 头（列表按插入序排，回填/重同步顺序即错）；`extract_text`
的 `<[^>]*>` 正则只删标签不删标签间内容，`<style>`/`<script>` 的内容会原样进入正文。
**两者已随 M4.5-01 修复**（[ADR-0025](decisions/0025-message-model-and-store-schema-v3.md)）：
`Date` 头落 `date_at`，排序改用带可信度判定的 `sort_at`；正文抽取换成单遍字节状态机，
raw-text 元素的内容整体丢弃。
需要外部主机或 provider 的兼容性证据继续按 [`testing/README.md`](testing/README.md) 补充，
不再改变 JSON-RPC v1 的基础契约。

## 阶段总览

| 阶段 | 目标 | 状态 | 依赖 |
|---|---|---|---|
| M2.5 headless 产品化 | socket、协议封板、安全边界、真实服务和运行质量 | ✅ 完成 | M2 API/CLI 已完成 |
| M3 前端壳与邮箱基础体验 | egui 桌面客户端、列表/详情/发信/设置 | ✅ 完成 | M2.5 协议稳定；外部平台残余独立记录 |
| M3.5 桌面平台化与发布 | 数据升级、原生通知、安装、启动、签名 | ✅ 仓库实现完成 | 正式 macOS 身份/证书/notary/真机与 Windows 真机是外部门禁；macOS credential 在 M3.7 恢复 |
| M3.6 headless 前端与外部进程骨架 | 按需前端、Menu、manifest、生命周期 | ✅ 完成 | M3.5 Agent/bundle；方法级授权仍属后续 |
| M3.7 账号管理与凭据闭环 | 无 TOML onboarding、Keychain、动态账户、per-account 收发 | ✅ 仓库实现完成（01–04；剩真实邮箱全链路、Keychain 锁定/签名复用与升级演练的真机外部验收） | M3.6 UI/Agent；见 ADR-0015/0016 |
| M4 生态插件 | 插件信任边界、安装事务、containment（Wave 1），随后 transport provider、地址层、第三方 provider 与 transfer（Wave 2） | ✅ Wave 1（01–04）与 Wave 2 的 M4-05/06/07 仓库实现完成；M4-08 transfer 📋 规划中（需独立 ADR） | M3.7 credential seam 已交付；逐切片 ADR 先行 |
| M4.5 日常可用邮箱客户端 | 邮件领域模型补齐（文件夹/标记/线程/附件/HTML/搜索/写路径）+ 默认前端迁移到 Tauri | ✅ 仓库实现完成（01–10、G01–G05；正式签名公证、Windows 真机与多日日用观察属外部门禁） | M4 Wave 2 已交付；ADR-0024 至 ADR-0040 |
| 日常收信增强（低占用后台收信 / 登录协议与自动发现 / 智能通知） | 冷启动有界窗口与按需历史、半开连接恢复与资源治理；SMTPS 465 + `AUTH LOGIN` + 服务商自动发现；上下文验证码、新邮件通知限流、规则持久化；Tauri 10 秒登录向导与日常交互 | ✅ 仓库实现完成并全栈接线（2026-11-17）；release 守护进程资源实测通过 | ADR-0041 / 0042 / 0043；真实 provider 465 与长期挂机观察属外部门禁 |
| M5 内核扩展性 | HMR、动态插件、沙箱、插件分发 | 远期 | 插件 ABI、版本和安全模型 |
| 长期质量与规模化 | schema migration、备份、压测、fuzzing、可观测性 | 持续 | 随每个阶段逐步补齐 |

依赖主线：

```text
M2.5 API/IPC 封板
        ├──> M3 前端壳
        │       └──> M3.5 桌面发布 ──> M3.6 前端骨架 ──> M3.7 账号管理
        └──> 真实服务/安全验证

M3.7 credential seam ──> alias / disposable / dns / transfer
插件 ABI/权限 ────> HMR / 第三方插件生态

M4.5-01 消息模型 ──┬──> M4.5-02 文件夹与增量同步 ──┬──> M4.5-05 写路径
                   │                              ├──> M4.5-06 发信补全/回复转发
                   │                              └──> M4.5-07 搜索与线程
                   └──> M4.5-04 正文渲染
M4.5-03 Tauri 骨架 ─┴──> M4.5-08 功能对齐 ──> M4.5-09 独立修复/移除 egui ──> M4.5-10 发布门禁
```

M4.5-01 与 M4.5-03 无相互依赖，可并行；**M4.5-01 是唯一有时间成本的前置**——历史邮件的
`Date`、HTML 原文与附件越晚补越难回填。

## 已封板阶段：M2.5 headless 产品化

| 任务 | 状态 | 封板证据 |
|---|---|---|
| H-01 协议与范围 | ✅ | [`protocol/README.md`](protocol/README.md) 固化 v1 方法、错误、上限、通知与明确不承诺范围 |
| H-02 本地传输 | ✅ | stdio / HTTP / Unix socket / Windows named pipe 共用 dispatch；多 session、半包、推送和关闭有测试 |
| H-03 身份与生命周期 | ✅ 本地封板 | Unix 路径全祖先检查、0600 socket、inode 安全清理；Windows 当前用户显式 DACL，跨用户运行复核留外部矩阵 |
| H-04 CLI | ✅ | socket/pipe/HTTP 发现、稳定 JSON/退出码、连接和 RPC 总 deadline、完整消息与 settings |
| H-05 真实服务 | ✅ | 2026-08-20 Stalwart 0.15.5 ignored 测试实跑通过；用户侧真实邮箱基础收发已确认，provider 扩展记录持续补充 |
| H-06 质量门禁 | ✅ | workspace test/clippy、Windows target check、macOS/Windows CI、关闭/资源边界运行手册 |
| H-07 文档切换 | ✅ | 协议、运维、测试分目录，README/PROGRESS/ROADMAP 同步，M3 成为下一阶段 |

### H-01 协议与范围决策

- 固化 JSON-RPC 方法、参数、错误码、响应上限和通知格式。
- 明确 socket 是否使用操作系统权限作为身份边界，HTTP 继续使用回环地址加 bearer token。
- 决定 settings 是否继续运行时-only；若持久化，单独设计版本化 settings 文件、原子写入和权限策略。
- 明确当前 API 不承诺附件、HTML 渲染、全文搜索和已读状态，避免前端阶段出现隐式扩 scope。

验收：协议文档、README 示例、CLI 行为和 e2e 测试对同一份契约一致。

### H-02 统一本地传输层

- 保留 stdio 作为子进程/前端启动通道。
- 增加 Unix domain socket，复用现有泛型 `serve`/`dispatch`。
- 增加 Windows named pipe，保持与 Unix socket 相同的 RPC 语义。
- 处理多客户端策略、连接关闭、半包、重连、服务端推送和优雅停机。
- 为三类本地传输补 hermetic 测试；Windows 专属测试在 Windows CI 执行。

验收：同一请求在 stdio、HTTP、Unix socket、named pipe 上得到相同结果，推送只在支持的连接上出现。

### H-03 本地身份与生命周期

- Unix socket 文件限制为当前用户可访问；启动遇到已存在路径时 fail-closed，人工确认 stale 后再删除，退出时删除自己创建的 socket。
- Windows named pipe 使用当前用户 ACL，并验证其他用户无法连接。
- 处理端口/socket 已占用、守护进程重复启动和异常退出后的恢复。
- 保持 HTTP 的 bearer token 不出现在命令行和普通日志中。

验收：权限测试和异常生命周期测试通过，文档能说明每种传输的信任边界；Windows ACL 的运行时
验证在 Windows 主机完成，当前用户的不同 logon session 不作进一步区分。

### H-04 CLI 与协议消费体验

- 为 socket 增加 CLI 连接选项，同时保留 HTTP 配置发现和环境变量覆盖。
- 固化 JSON 输出字段，保证脚本可依赖而不依赖人类文本输出。
- 补充稳定的错误退出码、超时和连接失败提示。
- 在 `docs/protocol/` 落地 JSON-RPC 方法表和示例。

验收：脚本可以在不启动 UI 的情况下完成查询、发信、修改运行时规则和读取完整消息。

### H-05 真实服务验证

- 在可控环境补至少一个独立实现或公共邮箱 provider 的 IMAP/SMTP 验证。
- 使用真实 CA 和真实认证配置验证 STARTTLS、证书主机名和重连。
- 记录服务端差异、限流、文件夹/UID 行为和复现步骤到 `docs/testing/`。
- 保留 Stalwart hermetic/ignored 测试作为独立实现交叉验证。

验收：至少一个独立真实服务完成“收信→落库→OTP/通知”和“SMTP 发信”全链路，凭据不入库、不入日志；公共 provider 继续扩充兼容性矩阵。

### H-06 运行质量与门禁

- 补 health/readiness、插件装配失败和账户离线的诊断信息。
- 验证优雅关闭：socket、HTTP、IMAP IDLE、scheduler、SQLite 连接均能释放。
- 运行全量测试、协议测试、真实服务测试和安全检查。
- 修复当前 workspace clippy 门禁问题，恢复 `-D warnings`。
- 记录长期运行、断线重连、大正文和大邮箱的资源边界。

验收：新环境按文档可启动；测试无失败、无新增警告；异常场景有明确错误而不是静默失效。

### H-07 文档与阶段切换

- 更新 `docs/PROGRESS.md` 的 commit、测试基线和已完成证据。
- 更新 `README.md` 的 socket 使用方式和安全说明。
- 将协议文档、运行手册、真实服务验证记录分别放入约定目录。
- M2.5 完成前不开始 UI 业务页面，避免 API 反复返工。

## 后续阶段摘要

### M3：前端壳与邮箱基础体验

M3 按下面的可独立验收任务推进。架构边界见
[`decisions/0001-m3-shared-client-boundary.md`](decisions/0001-m3-shared-client-boundary.md)。

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M3-01 共享 client | 独立 `maild-client`；统一发现、HTTP/IPC、DTO、帧/超时/id 校验；`maildctl` 迁移 | ✅ 完成 | CLI 行为兼容；HTTP/Unix/Windows 条件测试；可拆分读写半部消费通知和乱序响应 |
| M3-02 连接 actor 与视图模型 | 单一 IPC 读循环、pending request、连接状态、退避重连、通知归并、重连后快照 reconcile；HTTP 降级轮询 | ✅ 完成 | 断线不阻塞 UI；pending 明确失败；恢复后状态不依赖丢失通知 |
| M3-03 egui 基准壳 | 新建桌面 crate、窗口/主题/CJK 字体、三栏响应式布局、loading/empty/error 骨架 | ✅ 完成 | macOS 实际开窗与 CJK 已验证；Windows GNU 编译门禁；按需 repaint；无领域逻辑复制 |
| M3-04 邮箱读取体验 | 账户导航、分页消息列表、选择保持、完整纯文本详情、长正文安全展示、刷新 | ✅ 完成 | 摘要页不携带正文；过期页不覆盖；详情超限可恢复；键盘可完成导航 |
| M3-05 写信体验 | compose、字段校验、发送中/成功/失败、草稿离开确认 | ✅ 完成 | 不泄漏凭据；失败内容不丢；重复提交受控 |
| M3-06 rules 设置 | 展示和部分更新敏感开关、关键词、静默时段与时区偏移，明确 `persisted: false` | ✅ 完成 | 与 API 运行时语义一致；校验错误可定位；不暗示已持久化 |
| M3-07 M3 验收封板 | 跨平台构建、CJK/键盘/离线恢复/长正文、资源基线、文档和截图证据 | ✅ 完成 | workspace 与 Windows GNU 门禁绿；macOS release 实窗与资源已记录；Windows 真机留发布前外部矩阵 |

搜索、已读/未读、草稿持久化和同步进度需要新增守护进程能力，不夹带进现有 JSON-RPC v1
字段语义。完成 M3 基础体验后再按真实使用反馈排序，必要时以新增方法扩展协议。

前端只消费协议，不复制 receive、rules、store 或 transport 逻辑。`maild-client` 负责连接与
协议原语，M3-02 actor 负责重连和可重建视图状态，egui 组件只负责呈现与用户意图。

egui 的验收重点是 macOS/Windows CJK 渲染、空闲 CPU、内存、离线恢复、键盘操作和长正文显示。

> **2026-08-29 更新**：M3-03 的 egui 选型结论已被 [ADR-0024](decisions/0024-builtin-frontend-tauri-migration.md)
> 取代——默认前端迁移到 Tauri，egui 退守为冻结的恢复前端。本节保留为 M3 的封板记录。
> M3 结尾预留的「搜索、已读/未读、草稿持久化和同步进度需要新增守护进程能力」正是 M4.5 的内容。

### M3.5：桌面平台化与发布

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M3.5-01 store schema migration | SQLite schema v1、v0 事务迁移、结构校验、未来版本拒绝 | ✅ 完成 | 历史邮件/OTP 标记保留；失败不提升版本；新程序不误开未来库 |
| M3.5-02 SQLite 在线备份 | `maild backup`、WAL 一致快照、schema/quick_check、文件与目录同步、no-clobber | ✅ 完成 | daemon 运行时包含最新提交；发布前失败无目标；既有文件不覆盖 |
| M3.5-03 单实例/维护锁与恢复 | 文件型 Store 独占持久锁；输入/当前库校验；rollback；WAL 收束；原子替换 | ✅ 完成 | daemon 活跃时拒绝；发布前原库不丢；发布后保留明确 rollback |
| M3.5-04 平台路径与配置发现 | 独立 `maild-paths`；相对 DB/socket；共享配置优先级；SIGTERM | ✅ 完成 | 不依赖 cwd；旧数据冲突 fail-closed；后台停止走 LIFO |
| M3.5-05 macOS bundle 与后台 Agent | 顶层 App + 登录 Agent；显式注册/注销；状态可见 | ✅ 完成 | macOS 13+；每用户单实例；配置错误不形成重启风暴 |
| M3.5-06 macOS 原生通知 | Agent 持有 UNUserNotificationCenter 身份、授权状态与有界投递 | ✅ 完成 | 拒绝时不回退脚本；不重复请求；敏感规则继续生效 |
| M3.5-07 安装/升级/卸载与发布 | bundle 生成、可恢复原子发布/回滚、保留数据卸载、签名/notary 门禁 | ✅ 仓库实现完成 | 无 root；生命周期锁证明停服；失败可 recover；正式签名/notary/真机仍属外部门禁 |
| Windows 外部门禁 | named pipe 跨用户、toast/AUMID、真机 CJK/资源和安装行为 | 暂缓 | 有 Windows 真机时补证据，不阻塞当前非 Windows 基础工作 |

macOS 发布工具链已收口。用户已把“无需理解 TOML 即可添加真实邮箱”提升为下一优先级，因此
M3.7 恢复最小 credential seam；Windows Credential Manager、OAuth2 与业务 provider 仍延后。

### M3.6：headless 前端与外部进程骨架

进入广义生态前已完成 M3.6 的前端插件化运行骨架：

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M3.6-01 前端激活协议 | `FrontendBroker`、`OpenIntent`、通知点击、egui 精确邮件跳转 | ✅ 完成 | UI 不随登录启动；敏感路由不进 argv/URL；队列有界且一次性 |
| M3.6-02 菜单栏与 Preview DMG | 独立 Menu companion、Agent supervisor、基础动作、可体验镜像 | ✅ 完成 | Menu 不承载内核；可禁用；完整 UI 仍按需启动；首次安装生成无凭据配置；首次登记和受管 socket 重启已真机验证 |
| M3.6-03 前端清单与默认选择 | 受检 manifest、安装/发现、默认 frontend、内置恢复前端 | ✅ 完成 | 第三方进程隔离；未知 package 忽略、选中的损坏默认项配置停止 |
| M3.6-04 外部生命周期协议 | supervised/on-demand/independent、capability、退避与观测 | ✅ 完成 | allowlist、防重启风暴、有序停止；不加载第三方 Rust 动态库 |

### M3.7：账号管理与凭据闭环

执行合同见
[`decisions/0015-m3-7-account-management-and-credentials.md`](decisions/0015-m3-7-account-management-and-credentials.md)。
三个切片必须独立提交和验收：

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M3.7-01 headless 账户闭环 | 受管账户目录、macOS Keychain、动态 manager、probe、per-account 收发、API/CLI | ✅ 仓库实现完成 | 不编辑 TOML、不重启 Agent 即可添加；重启后凭据复用；旧配置只读兼容。hermetic 生命周期 e2e 与 Keychain 真机冒烟（ignored）见 testing 文档；真实邮箱验收归 M3.7-03 |
| M3.7-02 桌面账户中心 | 空状态入口、向导、编辑/停用/移除、preset、compose 账户选择 | ✅ 仓库实现完成 | 全点击流程有 reducer/渲染测试钉死；秘密不回填、提交后清零；Preview DMG 实装全点击验收归 M3.7-03 |
| M3.7-03 恢复与真实验收 | Keychain/文件/磁盘故障恢复、文档、真实邮箱、Preview DMG | 🚧 仓库内完成 | 恢复矩阵 hermetic 钉死（`tests/account_recovery.rs`）；iCloud/Gmail App Password 全链路与新 Preview DMG 全点击验收为真机外部证据，失败只进入 reauth，不回退明文 |
| M3.7-04 内核版本握手 | `getAgentInfo` + 协议代数、客户端判定降级、桌面横幅与一键重启内核 | ✅ 仓库实现完成 | 起因：2026-08-21 验收发现覆盖安装后旧 agent 存活占住 socket 致 -32601。判定表/遗留降级/e2e 重启恢复 hermetic 钉死（`tests/version_handshake.rs`）；「覆盖安装 → 横幅 → 一键重启」升级演练为真机外部证据，归 M3.7-03 验收同批 |

本阶段只恢复账户所需的 credential seam：macOS Keychain + 测试内存 provider。Windows、OAuth2、
Outlook/Exchange、自动迁移旧 TOML 和第三方 frontend 账户写权限均不在范围。

### M4：生态插件

先补齐第三方插件的信任边界与产品化安装（Wave 1），再进入广义业务插件（Wave 2）。每个切片
启动前先以 ADR 冻结合同（沿用 M3.7 先设计后实现的节奏），切片间独立提交与验收。

Wave 1 —— 插件平台。现状是 M3.6 的「同用户信任域」骨架：capability 仅作兼容性元数据，
无逐插件身份与方法级 enforcement，安装靠停服手工复制。本 wave 把这些升级为产品边界：

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M4-01 插件身份与认证缝 | Agent 为每个已安装插件建立调用者主体（user / plugin:`<id>` / frontend:`<id>`）；supervised 子进程经 env 注入获得且每 spawn 轮换，independent/on_demand 由用户显式 reveal（即轮换）；注册表只存哈希 | ✅ 仓库实现完成（[ADR-0017](decisions/0017-m4-1-plugin-identity-and-auth-seam.md)） | `authenticate` 仅长连接首请求、失败 -32014 立即断连不降级、stdio/HTTP 与缝停摆 -32015；`plugin_auth.toml` 对齐 accounts.toml 纪律；损坏软停摆 supervised 拒 spawn（`blocked`）核心照常；e2e `tests/plugin_identity.rs` ×5 + 模块单测钉死；协议代数未提升 |
| M4-02 方法级 capability enforcement | manifest `capabilities` 从兼容性元数据升级为真实授权表；方法→capability 映射单源化；默认拒绝，builtin 全量，user 主体维持现有边界 | ✅ 仓库实现完成（[ADR-0018](decisions/0018-m4-2-method-capability-enforcement.md)） | 策略表 fail-closed 默认（未登记方法对插件主体一律拒绝）；前端 route 固定翻译；推送过滤；`-32016` 不断连 + `denied_calls` 计数可见；`revealPluginToken`/账户方法对插件主体 UserOnly（提权防线）；e2e `tests/plugin_capabilities.rs` ×2 + 单测钉死；协议代数未提升 |
| M4-03 插件安装事务与生命周期管理 | `maildctl plugin install/remove/rollback/enable/disable` 事务；Agent 是受管目录唯一写者，CLI 只 staging + RPC 指挥；enable/disable 持久化 `plugin_state.toml` | ✅ 仓库实现完成（[ADR-0019](decisions/0019-m4-3-plugin-install-transactions.md)） | 升级要求三段版本严格更高，旧版进 `.rollback/<id>/`，单深度互换回滚（再执行即切回）；`.incoming`/`.removed`/`.rollback` 点前缀永不激活、崩溃残留不阻塞扫描；不停服安装，升级自动重启 supervised 子进程并轮换新代凭证；事务失败 -32017 且坏 staging 清理；e2e `tests/plugin_transactions.rs` ×2 + 单测钉死；协议代数未提升 |
| M4-04 进程 containment | Unix 子进程自为进程组长 + killpg 阶梯清扫；Windows Job Object kill-on-close；一切退出路径统一收尾 | ✅ 仓库实现完成（[ADR-0020](decisions/0020-m4-4-process-containment.md)） | 停止/disable/升级/remove/Agent 停机与正常 exit 0 都覆盖整组后代；setsid 逃逸不可探测为文档化残余风险，病态残留以新终态 `leaked` 可见；containment 装配失败拒绝启动；e2e `tests/plugin_containment.rs` ×2 + 组空探测单测钉死；Windows 行为归真机/CI 证据；协议代数未提升 |

Wave 2 —— 业务插件。依赖 Wave 1 的授权模型与 M3.7 credential seam，按风险从低到高排序：

| 任务 | 内容 | 状态 | 验收重点 |
|---|---|---|---|
| M4-05 外部 transport provider | daemon→插件反向 JSON-RPC（注册表 + pending 表 + 预算）；账户 incoming/outgoing 委托给受授权插件（catalog v2 `kind="plugin"`）；probe/send/receive 合同与单调 uid 游标 | ✅ 仓库实现完成（[ADR-0021](decisions/0021-m4-5-external-transport-provider.md)） | `providerRegister` 由 `transport_provider` capability 门控（User/无词表拒绝）；委托方向凭据不跨缝、绝不回退内置 transport（断线 `-32018`、超时 `-32019`、插件业务失败 `-32020`）；委托收信复用 store/filters/OTP/notify 链路且 (account, uid) 幂等；`listExternalPlugins.provider_registered` 观测；e2e `tests/plugin_transport.rs` ×2 + `plugin_transport_timeout.rs` ×1 + provider/registry 单测钉死；协议代数未提升 |
| M4-06 alias 与 disposable 地址 | 别名地址与一次性地址的 store/协议/桌面最小闭环 | ✅ 仓库实现完成（[ADR-0022](decisions/0022-m4-6-alias-and-disposable-addresses.md)） | store schema v2（addresses + messages.recipient）与 v1 恢复迁移；四个 UserOnly 地址 RPC、可选分页和每账户 1024 条原子上限；disposable 随机地址 + TTL 惰性过期；sendFromAccount 可选 active `from_address` 且沿用父账户 transport/凭据/规则；桌面管理、compose 选择和收件别名标记；上游 catch-all/DNS、To 改写与 SPF/DKIM 留作外部边界 |
| M4-07 NodeSeek Email 第三方 provider | 首个仓库内第三方业务插件：适配 `@nodeseek.org` / `@seek.li` 邮箱，提供 companion CLI 与 provider 收发闭环，并补齐按插件隔离的私有数据目录 | ✅ 仓库实现完成（[ADR-0023](decisions/0023-m4-7-nodeseek-email-provider.md)） | API key 仅存 macOS Keychain；生产请求固定 `https://seek.li/openapi/v1`；远端邮件 ID 映射为持久 synthetic uid；create/send 不重放且每次真实变更都需用户确认具体目标。真实插件验收仍待执行，Cloudflare/DNS catch-all 方案已冻结，不算本切片交付 |
| M4-08 transfer | 账户间邮件迁移 | 📋 已规划 | 高风险：单独确认流 + 审计日志 + 可回滚，实现前必须先出独立 ADR |

非目标（延续冻结决策）：Rust 动态库、HMR、WASI/沙箱、市场与签名分发属 M5；OAuth2 与
Outlook/Exchange provider 继续延后；Windows named pipe 放开秘密变更待 Windows credential
provider；网络下载不接入受管插件目录。

启动顺序：M4-01 的 ADR 最先；Wave 1 完成前不启动 Wave 2 实现。M3.7 外部验收若发现缺陷，
其修复优先于新切片。

### M4.5：日常可用邮箱客户端

目标是让 maild 能当主力邮箱日用。补齐顺序不按功能清单走，按 **模型 → 写路径 → 检索** 走：
先把一封邮件在库里存完整，再让客户端能改服务器状态，最后才是找得到、用得顺。前端迁移
（[ADR-0024](decisions/0024-builtin-frontend-tauri-migration.md)）与内核侧并行推进。

每个切片仍按既有纪律：**独立 ADR 先行、独立提交、独立验收**，协议扩展一律 additive。

#### 切片表

| 任务 | 内容 | 依赖 | 状态 | 验收重点 |
|---|---|---|---|---|
| M4.5-01 消息模型（schema v3） | messages 增 `message_id`/`in_reply_to`/`reference_ids`/`cc`/`bcc`/`reply_to`/`date_at`/`sort_at`/`body_html`/`snippet`/`has_attachments` 与 `seen`/`flagged`/`answered`/`deleted` 标记位；新表 `folders`（结构预置，02 首个写者）、`attachments`；`parse_message` 读 `Date` 头、保留 HTML 原文、附件 part 落盘；正文抽取换成真 HTML→text | — | ✅ 仓库实现完成（[ADR-0025](decisions/0025-message-model-and-store-schema-v3.md)） | v2 库打开自动升 v3 且行内容保持；v1/v2 备份恢复后迁移路径；排序改用 `Date` 头且缺失时回退投递时间；`<style>`/`<script>` 内容不再进正文；附件按 content-id 可取回；协议代数不提升 |
| M4.5-02 文件夹与增量同步（schema v4） | IMAP `LIST` 全部 mailbox；folder role 映射（RFC 6154 优先、名字兜底、猜不出留空）；mailbox 名 modified UTF-7 解码；per-folder `UIDVALIDITY` + 末位 UID 游标，`UIDVALIDITY` 变化时该文件夹全量重建；消息与附件的身份变为 `(account, folder, uid)`；新增 `listFolders`，`listMessageSummaries`/`getMessage` 加 `folder` 参数 | 01 | ✅ 仓库实现完成（[ADR-0026](decisions/0026-folders-and-incremental-sync.md)） | 多文件夹并行 IDLE 的连接预算有上限；`UIDVALIDITY` 翻转不产生重复行；旧 client 忽略新字段（serde default）；additive 扩展 |
| M4.5-G01 前端授权闭环 | frontend manifest 把 intent 路由（`capabilities`）与纯授权（`permissions`，schema_version 2）拆成两个字段；新增 `mailbox_write`（external 词表同步新增）/`accounts_manage`/`addresses_manage`/`plugins_manage` 四词；`status`、`prepareReply` 归 `messages_read`，`sendFromAccount` 归 `messages_send`，`requestNotificationAuthorization` 归 `settings_write`；`revealPluginToken` 保持唯一永久 UserOnly；`listFrontends` 与 `maildctl frontend list` 暴露 permissions | 设计确认 | ✅ 仓库实现完成（[ADR-0030](decisions/0030-frontend-authorization-closure.md)） | 只声明路由的前端仍拿不到写回与管理面；四词齐全也铸不出凭证；v1 包写 `permissions` 整包拒绝且不连累同目录；已安装包有效权限逐字节不变；User 主体零变化；协议代数不提升 |
| M4.5-03 Tauri 前端骨架 | `crates/maild-ui`（Tauri 2 壳）+ `packages/maild-ui`（React 宿主）；Tauri 后端直接 link `maild-client` + `maild-app`，连接 actor 原样复用并新增前端身份 bootstrap（`frontend:com.maild.ui`，bootstrap 后不持 `user` 连接）；快照事件单向推送 + 七个类型化 command；E01 贡献注册底座从第一天承载内置模块与示例扩展；开发 / 内嵌两种构建形态，`cargo test --workspace` 不需要 Node | G01 | ✅ 03-A + E01 仓库实现完成（[ADR-0031](decisions/0031-tauri-frontend-shell-and-contribution-registry.md)）；**不是日用版本**——无 HTML、无写信、无写回 | webview 侧不实现协议/发现/DTO/重连；**webview 永不接触 API token 与 endpoint**；`body_html` 不出桥；lockfile 入库、`packageManager` 钉死；真 WKWebView 上验证过三栏读信与扩展启停 |
| M4.5-G02 附件字节通道与远程内容策略 | `readAttachment` 按 `(account, folder, uid, part_index)` 分块 base64 读字节（单块 ≤ 512 KiB 原始字节，走既有 1 MiB 帧）；`-32003` / `-32004` 区分「没这个 part」与「有但没存」；附件元数据加 `stored`；`reading_policy.toml` 持久化远程内容默认策略与可信发件人（地址 / 精确域名），`getReadingPolicy` / `setReadingPolicy` 原子 patch；`getMessage` 带回 `remote_content` 结论；`maildctl attachment save` / `reading-policy` | 01, 02, G01 | ✅ 仓库实现完成（[ADR-0032](decisions/0032-attachment-byte-channel-and-remote-content-policy.md)） | 不扩帧、不引第二种传输；`folder` 必填不消歧；策略不进邮件库（恢复旧备份不该恢复旧信任名单）；内核给结论不代取远程资源；e2e `tests/attachments.rs` ×4 |
| M4.5-04 正文渲染 | sandbox iframe + 自定义协议 `maild-mail://localhost/doc/<token>` 的隔离文档；文档级 CSP `default-src 'none'`（头 + meta）；`lol_html` 在 Rust 侧去势（脚本/表单/iframe/object/base/meta/`on*`）并映射 `cid:` → 令牌 part URL、链接 → 惰性 `maild-link:`；wry `on_navigation` 对每个导航动作否决、`on_new_window` Deny；远程内容按 `getMessage.remote_content` 拦截，「加载本封」会话内、「始终允许」写回内核；链接确认面板；附件条与系统保存面板；「阅读与隐私」设置分区 | 01, 03, G02 | ✅ 仓库实现完成（[ADR-0033](decisions/0033-html-reading-isolation-and-resource-channel.md)） | 恶意 HTML fixture 断言 CSP 存在、远程资源默认被拒、`javascript:`/`file://` 被拦、`cid:` 不发起网络请求；放行策略在内核侧，换前端不绕过 |
| M4.5-G03 daemon 拥有的持久草稿（schema v8） | 新表 `drafts` / `draft_attachments`；`listDrafts` / `getDraft` / `createDraft` / `updateDraft` / `deleteDraft`；`version` 乐观并发，过期 `base_version` 返回 `-32006` 且一个字节都不写；patch 语义（缺席不动、出现即替换）；上限一律 `-32007` 拒绝**绝不截断**；头部字段的换行在写入时就被拒；附件是库内 part 的引用，写入时按 G02 的判定当场校验、读回时现查 `stored`；授权新增 `drafts_read` / `drafts_write` 两词（frontend 与 external 词表同步）；`maildctl draft` | 01, 02, G01, G02 | ✅ 仓库实现完成（[ADR-0034](decisions/0034-daemon-owned-drafts.md)） | 草稿比写它的 daemon 活得久；两个编辑者并发时后到的过期写入被拒绝而非合并；被拒之后库里那份原封不动（版本号都不动）；草稿不并进 `messages_read` 也不并进 `messages_send`；不同步到服务器 Drafts；e2e `tests/drafts.rs` ×8 |
| M4.5-G04 新附件 staging（schema v9） | 四个分块上传 RPC；主体/草稿绑定、512 KiB 块、20 MiB 单文件；版本化 finish 原子保存字节与引用；重复完成幂等、取消/超时回收；新附件发送引用需 drafts_read；CLI attach-file | G02、G03 | ✅ 仓库实现完成（[ADR-0035](decisions/0035-draft-attachment-staging.md)） | 3 MiB 经真实 socket/CLI/SMTP MIME 字节往返；主体越界/冲突/备份恢复/符号链接拒绝；08-A UI 已接入 |
| M4.5-05 写路径与离线队列（schema v5） | IMAP `STORE` 写 `\Seen`/`\Flagged`/`\Answered`/`\Deleted`；`MOVE`（降级 `COPY`+`\Deleted`，无 UIDPLUS 不 `EXPUNGE`）支撑归档/删除/移动；`pending_ops` 状态机与「结果未知」终态；重连后 flag 对账（时间窗口 + 未落定队列双护栏，CONDSTORE 短路，全量轮次顺带存在性对账）；五个 UserOnly 方法 | 01, 02 | ✅ 仓库实现完成（[ADR-0027](decisions/0027-mailbox-write-path-and-offline-queue.md)） | **「结果未知」是独立终态**——提交后失联既不算成功也不算失败，交用户裁决；对账跳过最近 N 秒本地改动以免覆盖乐观写；支持 CONDSTORE 时按 `MODSEQ` 短路整段对账；失败在 UI 可见可重试 |
| M4.5-06 发信补全与回复/转发（schema v6） | send/sendFromAccount 扩展 Cc/Bcc（**Bcc 只进信封**）、多收件人、`In-Reply-To`+`References`、text+html multipart、转发携带库内附件；`Date` 与 `Message-ID` 自己生成；RFC 2047 头部编码；投递成功后经写队列 `APPEND` 到 Sent；新增 `prepareReply` 在内核成型回复/全部回复/转发（头继承、引用正文、Re:/Fwd: 主题归一） | 01, 02 | ✅ 仓库实现完成（[ADR-0028](decisions/0028-compose-reply-and-sent-append.md)） | Bcc 走信封而非头注入（现有 `validate_message` 的头注入防线不得削弱）；`APPEND` 失败不影响已投递事实，单独可重试；别名发信（M4-06）语义不变 |
| M4.5-07 搜索与线程视图（schema v7） | SQLite `FTS5` 索引 subject/from/recipients/body + 结构化过滤（发件人、时间范围、有无附件、文件夹、账户）；CJK 逐字切分 + 短语查询让中文两字词可搜；查询表达式由内核构造不透传；`Message-ID` 图物化 `thread_id`；IMAP `SEARCH` 复用同一条连接兜底服务器侧历史 | 01, 02 | ✅ 仓库实现完成（[ADR-0029](decisions/0029-local-search-index-and-threading.md)） | 不引入新的索引引擎依赖；索引重建可中断可续；线程聚合用 M4.5-01 已落库的 header，不二次解析原文 |
| M4.5-08 前端功能对齐 | 写信、账户中心与向导、settings/rules、插件管理、地址层，对齐现有 egui 已交付的全部能力 | 03（并消费 05/06/07） | ✅ 08-A/B/C 仓库实现完成；原生视觉矩阵待补验 | 覆盖面对齐现有 egui reducer 测试所钉的状态机；秘密不回填、提交后清零；generation 防旧结果；握手不一致时禁用写入口 |
| M4.5-08-A 写信与回复（schema v10） | 原窗口编辑/回复/转发、主地址/别名、自动保存、文件选择/分块上传、关窗保护；版本化发送与持久结果 | 04、G03、G04 | ✅ 仓库实现完成（[ADR-0036](decisions/0036-compose-workspace-and-durable-send.md)） | 真 frontend→本机 TLS SMTP、Bcc/附件、并发与重启测试；正式跨平台/日用门禁仍属 10 |
| M4.5-08-B 邮箱工作区 | role/标记分页、统一列表、中文搜索、跨文件夹线程、多选写回、精确 UID 回执、活动安全摘要与显式恢复、通知 folder 身份 | 03-A、G01、05、07 | ✅ 仓库实现完成（[ADR-0037](decisions/0037-mailbox-workspace-and-sync-activity.md)） | schema 保持 v10；新摘要需读写双权限，旧接口兼容；原生窗口使用隔离测试库验收 |
| M4.5-08-C 账户与设置 | 账户探测/版本化编辑、运行时规则、通知权限、地址、受管包完整预览与事务、持久默认前端 | G01、08-A/B | ✅ 仓库实现完成（[ADR-0038](decisions/0038-settings-and-managed-packages.md)） | 秘密不回填、未保存与关闭保护、失败不重试、权限不足可区分；原生视觉补验单独记录 |
| M4.5-09 独立修复与默认切换 | 09-A 恢复内核 → 09-B 原生窗口/故障隔离 → 09-C 打包/切默认/删除 egui | 08 | ✅ 09-A/B/C 完成（2026-11-12） | ADR-0039 取代 egui 恢复壳与前端故障停内核；先验收独立图形恢复再删旧壳 |
| M4.5-10 发布门禁扩展 | 双 `.app` 的签名/notary/staple；预装 no-clobber + 一次性标记；恢复矩阵与降级矩阵 | 09 | ✅ 仓库内门禁完成（[ADR-0040](decisions/0040-release-gate-and-compatibility-matrix.md)）；正式签名公证/Windows 真机/日用观察属外部门禁 | 流程与证据格式沿用 ADR-0013，只扩载荷；用户手动移除默认前端后升级**不得**装回；frontend 目录权限约束运行时校验，预装不豁免；旧 daemon + 新前端 / 新 daemon + 旧配置/独立原生修复的兼容矩阵 |

#### 顺序与工作量估算

估算单位为**人日**，按当前工作节奏（AI 辅助 + 既有测试基线）给出区间，**不含**真机验收、
正式签名凭据与 notary 排队等外部门禁。

| 阶段 | 切片 | 估算 | 可并行 |
|---|---|---|---|
| 零 | M4.5-G01 前端授权闭环 | 2–4（✅ 已完成） | 03 的前置 |
| 一 | M4.5-01 消息模型 | 3–4（✅ 已完成） | 与 03 并行 |
| 一 | M4.5-03 Tauri 骨架 + E01 | 4–5（✅ 03-A + E01 已完成） | 与 01 并行 |
| 二 | M4.5-02 文件夹与增量同步 | 3–4（✅ 已完成） | 与 04 并行 |
| 二 | M4.5-04 正文渲染 | 3–4（✅ 已完成） | 与 02 并行 |
| 三 | M4.5-05 写路径与离线队列 | 5–6（✅ 已完成） | 与 06 并行 |
| 三 | M4.5-06 发信补全与回复/转发 | 3–4（✅ 已完成） | 与 05 并行 |
| 四 | M4.5-07 搜索与线程视图 | 3–4（✅ 已完成） | — |
| 四 | M4.5-08 前端功能对齐 | 6–8（✅ 08-A/B/C 已完成） | 与 07 并行 |
| 五 | M4.5-09 独立修复/移除 egui与默认切换 | 2–3（✅ 09-A/B/C 已完成） | — |
| 五 | M4.5-10 发布门禁扩展 | 3–4（✅ 仓库内门禁已完成） | — |
| | **合计** | **35–46 人日** | |

分组小计：**内核侧**（01/02/05/06/07）17–22 人日，**前端侧**（03/04/08/09/10）18–24 人日。
**内核侧五片已全部完成**（2026-08-30）：文件夹、已读/星标、归档删除、回复转发、搜索与线程都已可用，
剩余主切片在前端轨。2026-09-11 设计核验发现前端授权、附件字节通道、可靠草稿等仍有跨内核依赖，不能再称 03 是唯一阻塞点；详见下节。

#### 2026-09-11：UI/UX 先行与日用补齐提案

用户要求先设计完整前端，满意后再实现。当前交付为 [UI/UX 评审稿 v1.1](design/tauri/README.md)，
含可点击离线原型、视觉规范、任务流、错误状态和验收矩阵。设计确认不等于新接口/权限自动获批。

代码核验发现：`prepareReply`、写路径、账户管理等**当时**是 UserOnly，现有 frontend capability 不可直接调用；
附件字节与可信发件人名单已由 G02（ADR-0032）补齐；草稿仍是旧 UI 内存状态，新附件上传仍未实现。
**G01 已于 2026-09-11 落地**（[ADR-0030](decisions/0030-frontend-authorization-closure.md)）：
一个诚实声明身份的前端第一次能完整承担日用主路径，第三方与内置走同一张表。同时如实记录了
两件事——这条上限是**自愿声明**而不是沙箱（前端与用户同权，随时能不认证变回 `user`，取 token
时甚至必须先这么做一次），以及**核验时内置 egui 壳根本没有调用 `authenticate`**，因此上限从
M4.5-03 的 Tauri 壳开始生效，不是从 G01 落地当天。

**03-A + E01 已于同日落地**（[ADR-0031](decisions/0031-tauri-frontend-shell-and-contribution-registry.md)）：
壳能浏览本地缓存、以前端主体运行、经注册底座装配内置模块与示例扩展，并在真 WKWebView 上验证
过启停时工具栏/菜单/侧栏/工作页同步出现与消失。后续 HTML（04）、写信（08-A）与写回/搜索/活动（08-B）均已实现。
08-C 账户/设置/扩展管理、09 原生修复与 Tauri 默认交付、10 仓库内发布门禁与兼容矩阵均已完成（[ADR-0040](decisions/0040-release-gate-and-compatibility-matrix.md)）；正式凭据公证、Windows 真机与长期日用观察留外部门禁。

按 [交付计划](design/tauri/IMPLEMENTATION-PLAN.md) 先设计授权 G01，再做 03；G02 已落地并支撑 04；
G03/G04 草稿与新附件支撑完整写信；用户要求废弃 egui 与旧 ADR 的恢复界面条款存在冲突，
G05 提议独立最小原生修复窗口，确认后修订 ADR，再执行 09。

以上是**待确认补齐范围**，不倒改已经完成的 01/02/05/06/07。旧剩余前端 18–24 人日仅对应旧范围；
新增依赖后单人完整日用交付暂估 29–43 人日（约 6–9 工作周，另加用户迭代、正式签名等待与日用观察），
不是已承诺日期。用户随后认可主界面总体方向，并要求默认前端承载界面扩展与独立窗口。
新增 E01 贡献注册底座（进入 03）、E02 外部 UI 隔离/授权、E03 视图与窗口管理，见
[界面扩展设计](design/tauri/UI-EXTENSIONS.md)。新增暂估 7–12 人日，全范围 36–55 人日，约 8–11 工作周；
具体顺序和每片验收见交付计划。业务高级插件仍在日用之后，扩展底座从骨架开始采用。

两个排期事实值得单独记住：

1. **内核侧五片可独立交付价值。** 即使前端仍是冻结前的 egui，17–22 人日后就已经有文件夹、
   已读/星标、归档删除、回复转发、搜索和线程——邮箱可用性的绝大部分提升在这一半。
2. **M4.5-01 是唯一有时间窗口的。** `Date`、HTML 原文与附件不落库就永远补不回来，历史邮件
   每多积累一天，回填代价越高。它同时不依赖前端选型，应当最先开工。

#### 2026-11-17：日常收信增强与收官（已落地）

在 M4.5 仓库内切片全部完成之后，围绕「最小资源占用的后台收信」与「10 秒登录」再补一轮，按四条并行分支实现、
一次全栈汇合、一轮 release 实测收官：

| 切片 | 内容 | 状态 |
|---|---|---|
| 低占用后台收信（[ADR-0041](decisions/0041-low-footprint-background-receive.md)） | `initial_sync_limit`（缺省 200）冷启动有界窗口与 50 封分批；O(folders) 会话去重；TCP keepalive、阶段超时与指数退避；`trigger_sync` / `fetch_message_by_uid`；SQLite 页缓存 1 MiB、WAL ≤ 4 MiB；内存归还与 Agent 后台 QoS；`status.runtime` | ✅ |
| 登录协议与自动发现（[ADR-0042](decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md)） | SMTPS 465 先读 greeting；AUTH PLAIN/LOGIN 协商与秘密清零；连接/单步超时与可行动诊断；服务商知识库与 SSRF 边界的 `discoverEmailProvider` | ✅ |
| 智能通知与规则持久化（[ADR-0043](decisions/0043-persistent-rules-and-smart-notifications.md)） | 上下文验证码识别；收件箱新邮件通知（已读/静音/积压跳过、10 秒 3 封限流、脱敏）；`rules_state.toml` 覆盖层原子侧车；菜单栏「立即同步」 | ✅ |
| Tauri 登录向导与日常交互 | 12 类服务商预设与 `@domain` 匹配、自动发现、探测分阶段建议；立即同步、服务器历史检索与按需拉回、验证码一键复制、HTML 深色适配、键盘流 | ✅ |
| 全栈汇合 | RPC（`syncMessages`/`fetchServerMessage`/`discoverEmailProvider`/`listEmailPresets`、真实 `persisted`、`status.runtime`）、`maild-client` DTO、`maildctl sync` / `account discover` / `set-settings --persist`、Tauri 桥与兼容闸门；协议代不提升 | ✅ |
| 收官实测 | release `maild` 子进程 3 账户回环 TLS 基准（`footprint_bench`）、真实 bundle 构建 + ad-hoc 由内而外签名复验、全套打包门禁与全量回归 | ✅ |

#### 非目标（本阶段明确不做）

- **OAuth2 登录**：需注册 Google/Microsoft 应用并处理 client secret 分发；M4-05 的 external
  provider 缝更适合承载，不塞进内核。
- **POP3**：无 flag、无文件夹、无服务端状态，与 M4.5-05 的整个写路径模型不兼容。
- **Tantivy 等外部索引引擎**：SQLite 已在依赖内，`FTS5` 对个人邮箱量级足够。
- **HTML 富文本编辑器**：写信侧维持纯文本 + 可选 Markdown→HTML（协议接受调用方提供的 `body_html`）。
- ~~**新文件附件上传**~~：已作为独立切片 M4.5-G04 落地（分块上传 + daemon 拥有的 staging，
  [ADR-0035](decisions/0035-draft-attachment-staging.md)），不再是非目标。
- **看板、翻译、WebDAV 备份**等增值功能：属「其他邮箱没有的高级能力」一档，且更适合做成插件，
  排在日用一段时间之后。
- **Windows 前端交付**：受 ADR-0014 §9 `launch.kind` v1 只接受 `macos_application` 限制，
  需独立切片扩 manifest 并自行解决单实例。

### M5：内核扩展性

包括插件 panic 隔离、版本化 ABI、Rust 动态库/HMR、插件权限、沙箱、签名和分发市场。它们不属于 v1 基础闭环，必须在插件边界和安全模型稳定后再做。

## 持续质量项

这些工作不单独阻塞阶段切换，但每个阶段都应顺手推进：

- ~~SQLite schema migration、在线备份、维护锁与可回滚恢复~~ ✅；剩余保留策略随安装/升级切片设计。
- 邮件解析、HTML/附件和大正文的安全边界。
- API/SMTP/IMAP 的 fuzzing、属性测试和长时间压测（多账户 release 资源基准 `footprint_bench` 已落地；5 万封级真实大邮箱、休眠唤醒与 24 小时 soak 仍待补）。
- 结构化日志、metrics、崩溃诊断和数据脱敏。
- 真实服务和 macOS/Windows CI 的可重复测试。
