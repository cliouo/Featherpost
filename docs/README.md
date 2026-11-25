# 文档索引

Featherpost 的文档按“入口、架构、设计、状态、路线、运维”分层。新增文档先放在对应类别下，并从本页登记；不要把交接状态、长期设计和临时实验混在同一份文件里。

> **关于命名**：项目早期工作代号为 `maild`。**ADR-0001–0046 是历史架构演进档案，使用早期工作代号 `maild`
> 书写**（`Maild.app`、`maild-ui`、`com.maild.ui`、`MAILD_*` 等），按原样保留、不做回溯改名。自
> [ADR-0047](decisions/0047-decoupled-brand-and-open-source-readiness.md) 起，所有面向用户与操作系统的名字由仓库根的
> [`brand.toml`](../brand.toml) 决定（当前品牌 Featherpost），Cargo 包名与源码目录 `crates/maild*`
> 仍是内部稳定名。阅读历史 ADR 时请按 [BRANDING.md](BRANDING.md) 的字段对照理解。

## 快速入口

| 文档 | 用途 | 维护方式 |
|---|---|---|
| [README.md](../README.md) | 项目入口、快速开始、常用命令 | 功能入口或运行方式变化时更新 |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | 产品愿景、spine、事件协议、插件缝、前端方向 | 架构决策变化时更新 |
| [ROADMAP.md](ROADMAP.md) | 阶段目标、依赖关系、当前待办 | 每个阶段切换或优先级变化时更新 |
| [PROGRESS.md](PROGRESS.md) | 当前实现状态、测试基线、交接快照 | 每轮功能完成后更新 |
| [design/tauri/UI-EXTENSIONS.md](design/tauri/UI-EXTENSIONS.md) | 默认前端的界面贡献点、插件视图与扩展窗口（评审稿） | UI 扩展契约确认与实现时更新 |
| [design/tauri/README.md](design/tauri/README.md) | Tauri 默认前端 UI/UX 评审稿、离线原型、视觉/交互规范与交付顺序 | 设计变更或切片落地时更新（03-A 已按此实现骨架） |
| [protocol/README.md](protocol/README.md) | JSON-RPC v1、传输、通知和 CLI 契约 | 协议行为变化时同步实现和 e2e |
| [plugins/README.md](plugins/README.md) | 外部 frontend package v1：受管目录、manifest、生命周期和安全边界 | manifest schema、安装规则或外部 capability 变化时更新 |
| [operations/README.md](operations/README.md) | 启动、健康、关闭、恢复、资源边界、发布 workflow 仓库配置与可选国内 crates.io 镜像 | 运行行为或限制变化时更新 |
| [BRANDING.md](BRANDING.md) | 品牌切换指南：`brand.toml` 字段、`scripts/sync-brand.sh` 派生文件、改名代价与 CI 门禁 | `brand.toml` 字段或派生文件集合变化时更新 |
| [testing/README.md](testing/README.md) | 真实服务、平台兼容性、外部验证记录与 release 守护进程资源实测基线 | 每次真实环境验证或基准复测后追加证据 |
| [testing/nodeseek-email.md](testing/nodeseek-email.md) | NodeSeek Email provider 的 hermetic 边界、只读侦察与 live 验收门禁 | 每次真实插件验收后只追加脱敏证据 |
| [decisions/0001-m3-shared-client-boundary.md](decisions/0001-m3-shared-client-boundary.md) | M3 共享 JSON-RPC client 与前端边界 | 客户端职责或进程边界变化时更新/新增 ADR |
| [decisions/0002-m3-connection-actor.md](decisions/0002-m3-connection-actor.md) | M3 连接 actor、重连和快照状态边界 | 应用状态、重连或 reconcile 语义变化时更新/新增 ADR |
| [decisions/0003-m3-egui-shell.md](decisions/0003-m3-egui-shell.md) | M3 egui 壳、系统 CJK 字体和按需重绘边界（选型结论已被 ADR-0024 取代） | 桌面渲染、字体或 viewport 策略变化时更新/新增 ADR |
| [decisions/0004-m3-mailbox-reading.md](decisions/0004-m3-mailbox-reading.md) | M3 摘要分页、选择和按需详情边界 | 邮箱列表、分页或详情读取契约变化时更新/新增 ADR |
| [decisions/0005-m3-compose-and-send.md](decisions/0005-m3-compose-and-send.md) | M3 内存草稿、字段校验与单次发送状态机 | 写信、发送或草稿离开语义变化时更新/新增 ADR |
| [decisions/0006-m3-runtime-rules-settings.md](decisions/0006-m3-runtime-rules-settings.md) | M3 rules 基线草稿、部分更新与运行时-only 边界 | rules 设置、校验或持久化语义变化时更新/新增 ADR |
| [decisions/0007-m3-5-store-schema-migrations.md](decisions/0007-m3-5-store-schema-migrations.md) | M3.5 SQLite schema 版本、迁移与 fail-closed 边界 | store schema、迁移或兼容策略变化时更新/新增 ADR |
| [decisions/0008-m3-5-online-backup.md](decisions/0008-m3-5-online-backup.md) | M3.5 SQLite 在线备份、一致性与 no-clobber 边界 | 备份、恢复、维护锁或数据文件发布策略变化时更新/新增 ADR |
| [decisions/0009-m3-5-maintenance-lock-and-restore.md](decisions/0009-m3-5-maintenance-lock-and-restore.md) | M3.5 daemon 单实例、维护锁、回滚快照与原子恢复 | store owner、锁文件、restore 或 WAL/SHM 替换策略变化时更新 |
| [decisions/0010-m3-5-platform-paths-and-config-discovery.md](decisions/0010-m3-5-platform-paths-and-config-discovery.md) | M3.5 平台目录、配置发现、相对路径和后台停止信号 | 默认目录、配置优先级、生产装配或服务停止语义变化时更新 |
| [decisions/0011-m3-5-macos-bundle-and-agent.md](decisions/0011-m3-5-macos-bundle-and-agent.md) | M3.5 macOS App bundle、Login Item Agent 与 Service Management 边界 | bundle identity、后台生命周期、签名或升级策略变化时更新 |
| [decisions/0012-m3-5-native-notifications.md](decisions/0012-m3-5-native-notifications.md) | M3.5 Agent 原生通知身份、显式授权与有界投递 | 通知身份、授权协议、隐私或关闭语义变化时更新 |
| [decisions/0013-m3-5-macos-release-lifecycle.md](decisions/0013-m3-5-macos-release-lifecycle.md) | M3.5 macOS 用户级安装、可恢复升级/卸载与正式发布门禁 | 安装位、回滚、签名、notary 或外部门禁变化时更新 |
| [decisions/0014-m3-6-frontend-activation-and-plugin-lifecycle.md](decisions/0014-m3-6-frontend-activation-and-plugin-lifecycle.md) | M3.6 前端激活、OpenIntent 与外部进程生命周期边界 | 前端路由、默认消费者、插件隔离或生命周期协议变化时更新 |
| [decisions/0015-m3-7-account-management-and-credentials.md](decisions/0015-m3-7-account-management-and-credentials.md) | M3.7 受管账户目录、macOS Keychain、动态账户生命周期与 UI 闭环 | 账户 schema、凭据、CRUD、探测、per-account 收发或迁移边界变化时更新 |
| [decisions/0016-m3-7-agent-version-handshake.md](decisions/0016-m3-7-agent-version-handshake.md) | M3.7 内核版本握手（协议代数 + `getAgentInfo`）与一键重启内核 | 协议代数晋升规则、握手契约或重启流程变化时更新 |
| [decisions/0017-m4-1-plugin-identity-and-auth-seam.md](decisions/0017-m4-1-plugin-identity-and-auth-seam.md) | M4-01 插件主体识别、逐插件凭证注册表与 `authenticate` 认证缝 | 凭证格式、认证协议、注册表纪律或信任边界声明变化时更新 |
| [decisions/0018-m4-2-method-capability-enforcement.md](decisions/0018-m4-2-method-capability-enforcement.md) | M4-02 方法→capability 授权表、推送过滤、`-32016` 与拒绝计数（策略表与 frontend 词表限制已被 ADR-0030 取代） | 方法策略表、route 翻译或观测语义变化时更新 |
| [decisions/0019-m4-3-plugin-install-transactions.md](decisions/0019-m4-3-plugin-install-transactions.md) | M4-03 受管目录唯一写者、`.incoming`/`.removed`/`.rollback` 事务、enable 状态文件与差异化刷新 | 事务语义、目录约定或状态文件纪律变化时更新 |
| [decisions/0020-m4-4-process-containment.md](decisions/0020-m4-4-process-containment.md) | M4-04 supervised 子进程进程组/Job Object containment、统一停止收尾与 `leaked` 终态 | containment 机制、停止阶梯或逃逸边界声明变化时更新 |
| [decisions/0021-m4-5-external-transport-provider.md](decisions/0021-m4-5-external-transport-provider.md) | M4-05 反向调用缝（daemon→插件请求）、transport_provider capability、账户委托（catalog v2）与 `-32018/-32019/-32020` | 反向方法合同、委托绑定或预算语义变化时更新 |
| [decisions/0022-m4-6-alias-and-disposable-addresses.md](decisions/0022-m4-6-alias-and-disposable-addresses.md) | M4-06 别名/一次性地址层、recipient 溯源、别名发信与惰性 TTL | 地址生命周期、收件人捕获、别名发信或上游可达性边界变化时更新 |
| [decisions/0023-m4-7-nodeseek-email-provider.md](decisions/0023-m4-7-nodeseek-email-provider.md) | M4-07 NodeSeek Email 第三方 provider、私有插件数据目录、固定 origin 与 synthetic uid | provider API、秘密边界、游标、限流或远端 mutation 语义变化时更新 |
| [decisions/0024-builtin-frontend-tauri-migration.md](decisions/0024-builtin-frontend-tauri-migration.md) | 内置前端从 egui 迁移到 Tauri：随包预装的默认 frontend package、egui 退守恢复前端、正文渲染隔离与打包门禁扩展 | 前端技术栈、内置前端能力边界、正文渲染上下文或预装/签名载荷变化时更新 |
| [decisions/0025-message-model-and-store-schema-v3.md](decisions/0025-message-model-and-store-schema-v3.md) | M4.5-01 消息模型（RFC 5322 头、HTML 原文、附件、标记位）、store schema v3 与排序键 | 消息字段、schema 版本、正文抽取、附件上限或排序语义变化时更新 |
| [decisions/0026-folders-and-incremental-sync.md](decisions/0026-folders-and-incremental-sync.md) | M4.5-02 文件夹同步、per-folder UID 游标与 UIDVALIDITY 重建、role 映射、store schema v4 | 文件夹模型、同步游标、连接预算、role 判定或 mailbox 名编码变化时更新 |
| [decisions/0027-mailbox-write-path-and-offline-queue.md](decisions/0027-mailbox-write-path-and-offline-queue.md) | M4.5-05 标记/移动/删除写回、pending_ops 状态机与「结果未知」终态、标记对账与 CONDSTORE 短路 | 写路径语义、队列状态机、对账护栏或 IMAP 降级链变化时更新 |
| [decisions/0028-compose-reply-and-sent-append.md](decisions/0028-compose-reply-and-sent-append.md) | M4.5-06 MIME 组装、Cc/Bcc 信封边界、回复/转发成型与 Sent 归档队列 | 发信字段、头部编码、引用/主题归一规则或归档语义变化时更新 |
| [decisions/0029-local-search-index-and-threading.md](decisions/0029-local-search-index-and-threading.md) | M4.5-07 FTS5 本地检索、CJK 逐字切分与短语查询、结构化过滤、`Message-ID` 图线程聚合、可续回填与服务器侧 `SEARCH` | 索引结构、分词与查询构造、线程判定、回填策略或检索方法契约变化时更新 |
| [decisions/0030-frontend-authorization-closure.md](decisions/0030-frontend-authorization-closure.md) | M4.5-G01 前端授权闭环：manifest 把路由与授权拆成两个字段、四个受托管理词（决策 11 补遗：隐含 `frontend_open`、`notifications_read` 可声明）、三处方法归类修正、提权防线与「声明不是沙箱」的诚实边界 | 方法策略表、frontend manifest 授权字段、capability 词表或受托管理范围变化时更新 |
| [decisions/0031-tauri-frontend-shell-and-contribution-registry.md](decisions/0031-tauri-frontend-shell-and-contribution-registry.md) | M4.5-03-A + E01：Tauri 2 + React 技术栈、开发/内嵌两种构建形态、前端身份 bootstrap 住在 `maild-app`、桥的 DTO/错误投影、E01 贡献注册底座契约、`com.maild.ui` 包与开发安装 | 前端技术栈、构建形态、桥命令面、注册底座契约或前端包 manifest 变化时更新 |
| [decisions/0032-attachment-byte-channel-and-remote-content-policy.md](decisions/0032-attachment-byte-channel-and-remote-content-policy.md) | M4.5-G02：附件字节走既有 JSON-RPC 帧的分块 base64（`readAttachment`，四元组全量校验、三种「拿不到」可区分、`stored` 位）；远程内容策略与可信发件人名单是 daemon 文件状态（`reading_policy.toml`）而非邮件库；`getMessage.remote_content` 结论；「内核给决定不给保证」的诚实边界 | 附件传输形状、错误码、策略模型/匹配规则或策略存储位置变化时更新 |
| [decisions/0033-html-reading-isolation-and-resource-channel.md](decisions/0033-html-reading-isolation-and-resource-channel.md) | M4.5-04：HTML 原位阅读——sandbox iframe + 自定义协议文档的隔离形态、三层防线（sandbox / 文档级 CSP / Rust 重写与导航否决）、`lol_html` 映射与去势而非白名单、令牌作用域的 `cid:` 资源通道、远程内容两个动作（会话内 / 写回内核）、链接先看见再决定、附件保存、「阅读与隐私」分区、主窗口程序化创建 | 隔离形态、CSP、重写规则、协议路由、链接/远程内容交互或阅读区布局变化时更新 |
| [decisions/0034-daemon-owned-drafts.md](decisions/0034-daemon-owned-drafts.md) | M4.5-G03：daemon 拥有的持久草稿——草稿是字段不是报文、进邮件库（schema v8，与阅读策略作相反选择）、`version` 是唯一并发仲裁且过期写入一律拒绝、patch 语义、上限拒绝而非截断、附件是库内 part 的引用、`drafts_read` / `drafts_write` 两个新授权词、不同步到 IMAP Drafts 的理由 | 草稿模型/表结构、并发仲裁方式、上限、附件引用形状、草稿授权词或同步边界变化时更新 |
| [decisions/0035-draft-attachment-staging.md](decisions/0035-draft-attachment-staging.md) | M4.5-G04：主体绑定分块上传、匿名 staging、版本化完成与草稿持有字节（schema v9）、幂等完成、取消/清理和发送引用 | 上传契约、配额、临时文件生命周期、附件存储或发送授权变化时更新 |
| [decisions/0036-compose-workspace-and-durable-send.md](decisions/0036-compose-workspace-and-durable-send.md) | M4.5-08-A：原窗口写信/回复、Rust 文件 token、自动保存/关闭保护、schema v10 与版本化持久发送 | 编辑/发送状态、关闭保护、文件来源或投递恢复契约变化时更新 |
| [decisions/0037-mailbox-workspace-and-sync-activity.md](decisions/0037-mailbox-workspace-and-sync-activity.md) | M4.5-08-B：统一角色分页、精确 UID 写回回执、原位搜索与会话、活动投影和通知完整身份 | 列表过滤/分页、批量语义、活动信息边界或通知定位契约变化时更新 |
| [decisions/0038-settings-and-managed-packages.md](decisions/0038-settings-and-managed-packages.md) | M4.5-08-C：账户与设置、包预览确认、默认前端持久化 | 管理界面、包事务授权或默认选择契约变化时更新 |
| [decisions/0039-native-recovery-and-tauri-default.md](decisions/0039-native-recovery-and-tauri-default.md) | G05/09：独立原生恢复与删除 egui；09-A 固定副本恢复、目录独占与维护 IPC，09-B/C 接窗口和打包 | 恢复事务、默认迁移、包布局或维护权限变化时更新 |
| [decisions/0040-release-gate-and-compatibility-matrix.md](decisions/0040-release-gate-and-compatibility-matrix.md) | M4.5-10：Tauri 界面协议代握手与双端只读闸门、跨协议代预装/升级/回退/移除不装回/运行时权限矩阵、嵌套 `Maild UI.app` 由内而外签名 + 单归档公证 + 仅顶层 staple、CI/Release 与 Windows 零警告门禁 | 发布载荷、签名公证顺序、兼容矩阵或 CI 门禁变化时更新 |
| [decisions/0041-low-footprint-background-receive.md](decisions/0041-low-footprint-background-receive.md) | 低占用后台收信：`initial_sync_limit` 冷启动有界窗口与 50 封分批、O(folders) 会话去重、TCP keepalive / 阶段超时 / 指数退避、`trigger_sync` 与 `fetch_message_by_uid`、SQLite 运行时收紧、内存归还与后台 QoS、`status.runtime` 观测 | 冷启动/增量抓取策略、连接恢复、SQLite pragma、内存治理或资源观测口径变化时更新 |
| [decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md](decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md) | SMTPS 465 隐式 TLS 先读 greeting、AUTH PLAIN/LOGIN 协商与秘密清零、连接与单步超时、可行动探测诊断、服务商知识库与自定义域名发现的 SSRF 边界 | SMTP 会话状态机、认证机制、超时、探测诊断、知识库或网络发现边界变化时更新 |
| [decisions/0043-persistent-rules-and-smart-notifications.md](decisions/0043-persistent-rules-and-smart-notifications.md) | 上下文感知验证码识别、普适新邮件通知（已读/静音/积压跳过、10 秒 3 封限流、脱敏）、`rules_state.toml` 覆盖层原子侧车、默认配置与菜单栏「立即同步」 | 验证码规则、通知策略/限流、规则持久化格式或默认配置变化时更新 |
| [decisions/0044-macos-single-dock-identity.md](decisions/0044-macos-single-dock-identity.md) | macOS 单 Dock 图标：`Maild.app` 原位 `exec` 同包 `Contents/MacOS/maild-ui` 作为唯一宿主；`StartOnce` 经 `NSWorkspace` 把 `Maild Agent.app` 放到独立 Coalition 启动，消除 `exited-with-subordinates` 幽灵图标；`.main-ui.lock` 单实例、`RunEvent::Reopen` 唤醒并按序排空意图；`Maild.icns` / `CFBundleIconFile` 与顶层 `maild-ui` 显式 runtime 签名的打包契约 | 启动路由、Agent 启动方式、单实例锁、意图唤醒、宿主 UI 布局/图标或签名链变化时更新 |
| [decisions/0045-tray-only-minimize-and-close.md](decisions/0045-tray-only-minimize-and-close.md) | 菜单栏优先窗口语义：红灯 `×` / `⌘Q` 先 `retire_window`（隐藏 + `Accessory` + 交还前台）再 `exit(0)`，配合顶层 `LSUIElement = true` 让 Dock 零残留；黄灯 `–` / `⌘M` 保持 macOS 原生最小化；`FrontendIntentQueued` → IPC `frontend/intent` 只投给目标前端主体 → `maild-app` `intent_epoch` → Tauri 状态泵 `show_and_activate_main_window` + 领取意图；启动器对运行中的窗口排收件箱意图并聚焦 | 关闭/退出路径、激活策略、最小化语义、`frontend/intent` 推送或 `LSUIElement` 打包契约变化时更新 |
| [decisions/0046-menubar-lifecycle-and-dock-quit.md](decisions/0046-menubar-lifecycle-and-dock-quit.md) | 菜单栏与后台生命周期绑定：菜单栏「退出 Maild」调 `stopManagedAgent`，守护进程向已认证前端推送 `agent/stopping` → `maild-app` `ViewState.shutdown_requested` → Tauri 状态泵走统一退出流水线，一并关闭窗口、菜单栏与后台守护；红灯 `×`、`⌘Q`、Dock 右键「退出」经运行时补上的 `applicationShouldTerminate:` 收进同一流水线（刷盘草稿 → 隐藏 + `Accessory` → 50 ms → `exit(0)`），仅关闭窗口并彻底清空 Dock 图标；系统注销/重启不被阻断 | 退出入口、`agent/stopping` 推送、排空窗口、退出流水线次序或 `applicationShouldTerminate:` 注入方式变化时更新 |
| [decisions/0047-decoupled-brand-and-open-source-readiness.md](decisions/0047-decoupled-brand-and-open-source-readiness.md) | 品牌与代码解耦：`brand.toml` 单一事实来源、`sync-brand.sh` 派生与 `--check` 门禁、macOS 打包品牌中立模板与 `brand.env` 驱动、GitHub Actions 品牌无关产物名与 `MACOS_*` 仓库变量、MIT OR Apache-2.0、上游公开链接替代本地 `reference/` / `paper.pdf` / `vendor/cordis` | 品牌字段、派生文件、打包模板占位符、provisional 门禁或开源发布边界变化时更新 |
| [decisions/0048-locale-preference-and-decoupled-i18n.md](decisions/0048-locale-preference-and-decoupled-i18n.md) | 语言偏好与解耦 i18n：用户语言选择持久化、前后端与系统菜单语言协商、`packages/i18n` 目录与构建期校验门禁 | 语言协商规则、catalog schema 或 i18n 门禁变化时更新 |
| [decisions/0049-retire-legacy-ui-studio-sole-builtin.md](decisions/0049-retire-legacy-ui-studio-sole-builtin.md) | 退役原版 `dev.featherpost.ui`，以 `dev.featherpost.studio` 为唯一内置前端：workspace 升至 `0.4.0` 避免 `0.3.4` 降级陷阱、旧配置只读迁移且不静默删磁盘目录、移除 `studio` feature 与双前端打包脚本、前端收口为单一包 | 内置前端身份、退役前端迁移语义或单载荷打包布局变化时更新 |
| [rust-port-design.md](rust-port-design.md) | cordis 方法论到 Rust 的设计映射 | 内核语义或 API 变化时更新 |
| [m0-spike.md](m0-spike.md) | TS M0 spike 的历史结果与规格对照 | 仅在重新验证 M0 时更新 |

## 目录约定

```text
README.md                    项目入口和快速开始
ARCHITECTURE.md              稳定的产品与系统架构
brand.toml                   品牌唯一事实来源（见 docs/BRANDING.md）
docs/
  README.md                  文档索引和存放规则
  BRANDING.md                品牌切换指南
  ROADMAP.md                 里程碑路线图和当前工作分解
  PROGRESS.md                实现进度与交接快照
  rust-port-design.md        Rust 内核设计
  m0-spike.md                历史实验与规格对照
  protocol/                  JSON-RPC、socket、事件协议文档
  plugins/                   外部进程插件的 manifest、安装与生命周期协议
  operations/                启动、配置、凭据、发布和故障排查手册
  decisions/                 ADR：一个文件记录一个已确认的架构决策
  testing/                   真实邮箱、平台兼容性、压测和安全测试记录
  archive/                   已失效但需要保留的历史设计
```

目前 `protocol/`、`operations/`、`testing/` 和 `decisions/` 已有对应文档；`archive/`
等第一份对应文档落地时再创建，避免空目录和无意义占位文件。

## 写作规则

- `README.md` 只回答“这是什么、如何运行”；不要放长篇设计讨论。
- `ARCHITECTURE.md` 记录跨模块稳定边界；具体实现细节放到 `docs/`。
- `ROADMAP.md` 记录还没完成什么以及先后依赖；完成项在 `PROGRESS.md` 留证据。
- `PROGRESS.md` 保持可交接，必须包含当前 commit、测试基线和已知阻塞。
- 架构决策确认后，写入 `docs/decisions/`，并在架构文档中只保留结论和链接。
- 协议变更必须同时更新实现、协议文档、CLI 示例和 e2e 测试。
- 真实服务或平台验证放在 `docs/testing/`，不要把环境凭据提交到仓库。
