# 任务进展记录（交接文档）

> 最后更新：2026-11-18（2026-11-18 [ADR-0047](decisions/0047-decoupled-brand-and-open-source-readiness.md) 品牌解耦与开源就绪：对外品牌定为 **Featherpost**，`brand.toml` 成为产品名 / bundle ID / CLI 名 / 图标的唯一来源，`scripts/sync-brand.sh` 生成全部派生文件并由 CI `--check` 把关；B0 `f853c2f` 落地 `brand.toml`、`maild-brand`、图标与 MIT OR Apache-2.0，B3 让 `packaging/macos/**` 与 GitHub Actions 全部改读 `brand.env`、重写开源 README 并新增 [BRANDING.md](BRANDING.md)；B1 `7cd4d65`（后端/路径/client/launcher/menu/NodeSeek 插件）与 B2 `56ccfbe`（Tauri 前端与 `maild-ui` 宿主）运行时改读品牌常量，B4 在 `cliouo/brand-featherpost-integration` 汇合 B1+B2（`3d7dca3`）+B3 `a992a86`（`d43b422`）并通过全量门禁与 Featherpost Release 构建。同日合入 [ADR-0046](decisions/0046-menubar-lifecycle-and-dock-quit.md) 菜单栏与后台生命周期绑定：点击菜单栏「退出 Maild」一并关闭窗口、菜单栏与后台守护，点击窗口红叉、`⌘Q` 或 Dock 右键「退出」均仅关闭窗口并彻底清空 Dock 图标；同日早些时候合入 ADR-0045 菜单栏常驻窗口语义：黄灯原生最小化、`frontend/intent` 实时唤醒；此前 2026-11-17 **仓库交付收官**：M4.5 全部仓库内切片（01–10、G01–G05）完成后，同日完成日常收信增强四条分支——低占用后台收信 [ADR-0041](decisions/0041-low-footprint-background-receive.md)、SMTPS 465 / `AUTH LOGIN` 与服务商自动发现 [ADR-0042](decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md)、智能通知与规则持久化 [ADR-0043](decisions/0043-persistent-rules-and-smart-notifications.md)、Tauri 10 秒登录向导与日常交互——及其全栈汇合，随后合入 macOS 单 Dock 图标身份 [ADR-0044](decisions/0044-macos-single-dock-identity.md)（运行时 + 单宿主打包）；收官波次用 release `maild` 子进程实测后台资源（3 账户 IDLE 稳态 RSS ≈ 14–15 MiB、`phys_footprint` ≈ 5 MiB、空闲 CPU ≈ 0.001 %、推送 P50 ≈ 4 ms），真实 bundle 构建/签名复验与全套门禁通过。剩余 1% 全部是需要正式凭据或真机的外部门禁，见[下文清单](#剩余外部门禁1-边界)）。
> M2.5 封板 commit：`71df1d2`；M3-01 commit：`c4abc91`；M3-02 commit：`0275a8d`；M3-03 commit：`207d2c9`；M3-04 commit：`bc22f65`；M3-05 commit：`dbf0d71`；M3-06 commit：`fc011de`；M3-07 commit：`7378537`；M3.5-01 commit：`cb92111`；M3.5-02 commit：`90a0a25`；M3.5-03 commit：`8c84709`；M3.5-04 commit：`a823adc`；M3.5-05 commit：`8c03676`；M3.5-06 commit：`a726cf2`；M3.6-01/02 commit：`c5607a9`；
> **M3.7 切片**：设计 `cdf9fde`；M3.7-01 基础层 `95933a6`、API/client/CLI/e2e `52dae46`、文档 `8799146`；M3.7-02 实现 `02a26df`、文档 `37437ee`；M3.7-03 恢复矩阵 `4141265`、DMG 证据 `0722a09`；M3.7-04 设计 `2cd532e`、实现 `21cea7e`、文档 `782efb5`；DMG 重建（含握手 + build_rev 注入）`9b7b5c8`。
>
> **日常收信增强与收官（2026-11-17）**：cordis 上游同步 `64046ad`；ADR-0042 `b4775d3`；Tauri 登录/日常 `14ec389`；ADR-0043 `9f50d15`；ADR-0041 `8d11160`；全栈汇合 `5bcb9d6`；收官实测与文档 `d56f56e`；ADR-0044 单 Dock 运行时 `64d6103`、单宿主打包与 `Maild.icns` `a08b8fe`，二者在 `cliouo/dock-integration-dmg` 合入回归 `9d4c36e`；ADR-0045 运行时 `965168a`（M1）与 `LSUIElement` 打包 `983cd63`（M2）在 `cliouo/tray-minimize-integration` 合入并收敛为系统原生最小化语义 `ae51399`；ADR-0046 菜单栏「退出 Maild」整体停机 `687bbce`（Q1，`cliouo/menubar-quit-all`）与 Dock「退出」/ `⌘Q` 零残留 + `agent/stopping` 前端链路 `11fe854`（Q2，`cliouo/dock-quit-zero-residue`）在 `cliouo/menubar-dock-quit-integration` 合入并补跨 crate 端到端测试 `7d71747`；随后将完整窗口/Dock/菜单栏交互守则与不退化红线统一固化至 `ARCHITECTURE.md`、`docs/design/tauri/UX-SPEC.md` 与 `docs/operations/README.md`。
>
> **M4 切片**：规划 `6f86d37`；ADR-0017 草案 `a41f50e`、冻结 `6fccb02`；M4-01 实现 `3933b37`、文档 `dd7b3dc`；ADR-0018 冻结 `a7b101b`；M4-02 实现 `888a2e5`；ADR-0019 冻结 `7b332af`；M4-03 实现 `e38aa27`；ADR-0020 冻结 `34a1a7f`；M4-04 实现 `4b7b8c3`；ADR-0021 冻结 `0837991`；M4-05 client `a4fe69b`、实现 `464558f`、桌面 `da7b32d`；ADR-0022 冻结 `e54fa29`；M4-06 实现与文档（当前工作树）；ADR-0023 与 M4-07 NodeSeek Email provider（当前工作树，未提交）。

## 当前前端工作

用户要求先设计完整 UI/UX，体验满意后再写 Tauri 前端。已提供 [设计评审稿 v1.1](design/tauri/README.md)：
离线交互原型、视觉规范、完整任务流、状态矩阵、浏览器截图及后续顺序。**用户已认可原型并授权推进实现；Tauri 前端与内核已完成 03-A + E01、04、08-A/B/C、09-A/B/C 及 10 仓库内发布门禁（ADR-0040），egui 已删除；2026-11-17 又补齐 10 秒登录向导（12 类服务商预设、自动发现、分阶段探测建议）与日常交互（立即同步、服务器历史检索与按需拉回、验证码一键复制、HTML 深色适配、键盘流、新邮件通知与按账户静音）。剩余正式凭据签名公证、真实会话图形激活、Windows 真机与多日日用观察属外部门禁。**

已补 [界面扩展设计](design/tauri/UI-EXTENSIONS.md) 与“邮件摘录”交互样例：菜单/工具栏、阅读侧栏、工作页、分离/收回窗口及启停回收。E01 注册底座进入 03，E02 权限/隔离与 E03 窗口分别在后续切片验收。

核验出前端主体 UserOnly 授权缺口（**已由 M4.5-G01 / ADR-0030 闭合**）、附件字节/可信名单通道、持久草稿与新附件上传缺口；
用户要求废弃 egui，与 ADR-0024 的保留恢复界面条款需要后续修订。具体依赖与估算见
[交付计划](design/tauri/IMPLEMENTATION-PLAN.md)。本轮检查结果见 [设计验证记录](design/tauri/REVIEW.md)，
与下方历史 Rust 测试基线分开。

## 项目一句话

**maild** —— 邮件领域的「VSCode」：薄内核 + 插件生态，headless 优先（守护进程常驻收信/通知，前端按需拉起）。内核用 **Rust 重实现 cordis（时空可组合性）方法论**，平台 macOS + Windows。

## 里程碑状态

| 里程碑 | 状态 | 证据 |
|---|---|---|
| 架构设计（ARCHITECTURE.md） | ✅ 完成 | 产品骨架/spine/事件/插件缝/前端选型 |
| cordis → Rust 移植设计 | ✅ 完成 | docs/rust-port-design.md |
| TS M0 spike（vendor + Bun 门禁） | ✅ 完成（规格对照用） | packages/spike/，docs/m0-spike.md |
| R0（服务表 + 效果 + emit） | ✅ | tests/m0_parity.rs（6 测试） |
| R1（5 事件模式 + inject + 生命周期） | ✅ | tests/r1.rs（9 测试） |
| R2（realm 作用域 + 事件 scope） | ✅ | tests/r2.rs（7 测试） |
| R3（loader + 配置装配 + intercept） | ✅ | tests/r3.rs（9 测试）+ crates/maild tests/loader.rs（10 测试） |
| R4（领域服务：收信→验证码→通知） | ✅ | tests/domain.rs + tests/e2e.rs：SMTP 发信→IMAP IDLE 收→store 落库→otp 提取→通知 |
| R4.5 第一批（多账户 + 去重持久化 + 轮询兜底） | ✅ | tests/e2e.rs（3 测试，含多账户事件隔离）+ domain.rs 补测；轮询已迁入 scheduler |
| R1.5（级联卸载时序精化） | ✅ | tests/r1_5.rs（4 测试）：withdrawal 提前可见 + 依赖者先逆 + 值校验撤销 |
| filters（waterfall 过滤链实战） | ✅ | crates/maild/src/filters.rs（5 单测）+ e2e 垃圾邮件用例 |
| transport + rules（spine 补齐） | ✅ | transport：SMTP 发信 e2e；rules：notify/route + quiet-hours bail 消费（敏感开关、关键词、时间窗口，6 单测 + 4 e2e） |
| TLS（STARTTLS + 隐式 TLS，hermetic） | ✅ | receive tls=none/starttls/implicit + transport tls=none/starttls；假服务器 rustls/rcgen；receive e2e ×2 + SMTP STARTTLS e2e ×3（成功/拒绝降级/证书拒绝） |
| 真服务器交叉验证（Stalwart） | ✅ | `tests/real_server.rs`（#[ignore]）+ `.tools/stalwart/`：transport(SMTP STARTTLS)+receive(IMAP STARTTLS+IDLE) 对独立实现 Stalwart 0.15.5 跑通「收信→验证码→通知」 |
| api + CLI（M2 headless） | ✅ | 同一 JSON-RPC v1 支持 stdio / Bearer 本地 HTTP / Unix socket / Windows named pipe；`maildctl` 支持查询、完整详情、发信、运行时 rules settings 与插件清单；API 单测 35 |
| M2.5 headless 产品化收尾 | ✅ | IPC 多 session/推送/关闭、Unix 路径安全、Windows 当前用户 DACL、协议/CLI 封板、资源上限、Stalwart 实跑和 macOS/Windows CI 已落地；外部残余见 `docs/testing/README.md` |
| scheduler（spine #8 定时任务） | ✅ | `src/scheduler.rs`：全局/账户作用域、固定延迟 interval、一次性任务、可取消句柄、有限抖动、关闭等待；receive 轮询与 rules 静默时段刷新已接入 |
| M3（前端壳与邮箱基础体验） | ✅ 封板 | M3-01 至 M3-07：shared client、连接 actor、egui 壳、邮箱读取、写信、rules 设置与跨平台验收均完成；外部残余见 `docs/testing/README.md` |
| M3.5-01（store schema migration） | ✅ | SQLite schema v1；v0 事务迁移与数据保留；当前结构校验；未来版本 fail-closed；6 个迁移测试 |
| M3.5-02（SQLite 在线备份） | ✅ | `maild backup`；WAL 在线一致快照；schema/quick_check；文件与目录同步；no-clobber；7 个备份测试 |
| M3.5-03（维护锁与恢复） | ✅ | 文件型 Store 跨进程独占并收紧 Unix 数据文件权限；特殊 sidecar/配置 store 装配失败时 daemon 快速退出；`maild restore` 先生成 rollback，再收束 WAL/SHM 并同目录原子替换；13 个恢复测试 |
| M3.5-04（平台路径与配置发现） | ✅ | 独立 `maild-paths`；三类进程共享配置优先级；相对 DB/socket 锚定 data/runtime；legacy 冲突 fail-closed；SIGTERM 有序关闭 |
| M3.5-05（macOS bundle 与后台 Agent） | ✅ | `Maild.app` 内嵌 Login Item；独立 `maild-agent` 复用 daemon runner；SMAppService 显式状态/注册 UI；hermetic bundle 生成与结构验证 |
| M3.5-06（macOS 原生通知） | ✅ | Agent identity 下的 `UNUserNotificationCenter`；查询不弹窗、显式授权单飞；32 条有界单 worker 投递；API/CLI/桌面状态闭环；无脚本回退 |
| M3.5-07（macOS 发布生命周期） | ✅ 仓库实现完成 | Agent 生命周期锁/事务级维护租约；固定用户级安装；可恢复升级/回滚；保留数据卸载；universal2、Developer ID、notary、staple、Gatekeeper 与 checksum 门禁；正式凭据/真机证据仍为外部项 |
| M3.6-01（前端激活协议） | ✅ 完成 | `FrontendBroker` + `OpenIntent`；通知点击经 Agent 内存映射唤起内置 egui；启动/聚焦领取并可直接打开页外邮件 |
| M3.6-02（菜单栏与 Preview DMG） | ✅ 完成 | 独立 `Maild Menu.app`；Agent supervisor；Inbox/Compose/Settings/后台动作；真实 universal2 bundle 与 ad-hoc Preview DMG 挂载验收；已安装 App 首启安全生成无凭据配置；首次 `SMAppService.NotFound` 可登记，Agent 可恢复受管 stale socket |
| M3.6-03（前端清单与默认选择） | ✅ 完成 | 受检 `frontend.toml`、托管目录、默认消费者、能力路由、`listFrontends` 与 `builtin-egui` 恢复入口 |
| M3.6-04（外部进程生命周期） | ✅ 完成 | 受检 `plugin.toml`、显式 allowlist、supervised/independent、指数退避、LIFO 停止与 `listExternalPlugins` 观测 |
| M3.7（账号管理与凭据闭环） | ✅ 仓库实现完成（剩真机外部验收） | ADR-0015 已冻结合同；**01/02 完成、03 仓库内全部完成、04 仓库实现完成**（版本握手 ADR-0016）；剩余仅真实邮箱全链路、Keychain 锁定/签名复用与升级演练的真机外部验收 |
| M3.7-01（headless 账户闭环） | ✅ 仓库实现完成 | 不编辑 TOML 经 Unix socket 动态添加 → 不重启即 online → 收信落库（account=不可变 id）→ sendFromAccount → 重启自动恢复收信 → disable/enable → remove 软归档（凭据删除、邮件保留）；见 `tests/account_lifecycle.rs` 与 protocol/operations/testing 文档同步更新 |
| M3.7-02（桌面账户中心） | ✅ 仓库实现完成 | 空状态「添加邮箱账户」入口、账户列表（状态点/凭据状态/runtime detail）、iCloud/Gmail/Manual preset 向导（显式 endpoint + 官方帮助链接由桌面打开）、probe→保存并连接、编辑/停用/移除（单次 in-flight + generation 防旧结果，移除确认固定说明本地邮件保留）、密码不回填且提交/关闭后清零、compose 发件账户选择（恰一自动选/多账户显式选/无 outgoing 回退全局 transport，走 sendFromAccount 防伪造 From）；reducer 与无窗口渲染测试钉死 |
| M3.7-03（恢复与真实验收，仓库内部分） | ✅ 仓库内完成 | `tests/account_recovery.rs` 六项：目录损坏/未来版本 fail-closed 不清理凭据、凭据丢失→needs_credentials→重输恢复、发布失败回滚 staged 凭据并可重试、孤儿 revision 启动收敛、密码撤销→认证失败不回滚配置→重输恢复；假服务器新增 LOGIN 校验与服务端改密。真机外部项：Keychain 锁定/签名复用、真实 iCloud/Gmail App Password 全链路、新 Preview DMG 全点击验收 |
| M3.7-04（内核版本握手） | ✅ 仓库实现完成 | 起因：2026-08-21 验收实录——覆盖安装后旧 agent 占住受管 socket 致 -32601（ADR-0016）。`getAgentInfo` + 协议代数（单源 `maild-client::PROTOCOL_VERSION`，首发 1）；判定表与 -32601 遗留降级（client 假服务器 + 真实 daemon e2e）；`maildctl` stderr 警告不改退出码；桌面握手按 epoch 单次、不一致禁用账户/写信入口并显示横幅，一键重启（pid→SIGTERM→10s→SIGKILL 兜底→内嵌 bundle→重握手，单飞行、失败重试、手动指引降级）；`tests/version_handshake.rs` 钉死优雅停止删 socket 与硬停残留受管回收。真机「覆盖安装→横幅→一键重启」升级演练归 M3.7-03 外部验收同批 |
| M4-01（插件身份与认证缝） | ✅ 仓库实现完成 | ADR-0017（2026-08-22 冻结，含六轴取舍与愿景对照）：会话主体 user/plugin:`<id>`/frontend:`<id>`，`authenticate` 仅长连接首请求、-32014 应答即断连不降级，stdio/HTTP 与缝停摆 -32015；`plugin_auth.toml` 只存 SHA-256 哈希（accounts.toml 同纪律），扫描对账补发+孤儿收敛；supervised 每 spawn 注入 `MAILD_PLUGIN_TOKEN` 并轮换，停摆时拒绝 spawn（`blocked`）；`maildctl plugin-token` reveal 即轮换；`listExternalPlugins`/`listFrontends` 增 `has_identity`。`tests/plugin_identity.rs` ×5 + 模块单测 ×8；协议代数未提升 |
| M4-02（方法级 capability enforcement） | ✅ 仓库实现完成 | ADR-0018（2026-08-22 冻结；策略表已被 ADR-0030 取代，见下方 M4.5-G01 行）：方法策略表单源（Open/Capability/UserOnly，**表外默认 UserOnly** fail-closed）；`revealPluginToken`/`status`/插件清单/全部账户方法对插件主体 UserOnly（提权防线）；前端 route 固定翻译（inbox/message→messages_read、compose→messages_send、settings→read+write、background→无）；服务端推送经 SharedPrincipal 投递前过滤；越权 `-32016` 不断连并计入 `denied_calls`（两个清单 API 可见）。`tests/plugin_capabilities.rs` ×2 + 策略表/route 翻译/计数单测；协议代数未提升 |
| M4-03（插件安装事务与生命周期管理） | ✅ 仓库实现完成 | ADR-0019（2026-08-22 冻结）：Agent 是受管目录唯一写者——CLI 只把包复制进 `.incoming/<rand>` 再经 RPC 指挥发布；install-or-upgrade（升级要求三段数字版本严格更高，旧版进 `.rollback/<id>/`），单深度互换回滚（再执行一次即切回），移除经 `.removed/`；`.incoming`/`.removed`/`.rollback` 点前缀目录永不激活、崩溃残留不阻塞扫描；enable/disable 持久化 `plugin_state.toml`（fail-closed），supervised 即时启停、independent 只记意图；refresh 差异化启停（新增拉起/移除停止/版本变化重启，不重启未变化项）；五个事务方法 UserOnly，失败 `-32017` 且坏 staging 清理；不停服安装，升级后 supervised 子进程自动以新代凭证重启。`tests/plugin_transactions.rs` ×2 + 事务生命周期/staging 名规则单测；协议代数未提升 |
| M4-04（supervised 插件进程 containment） | ✅ 仓库实现完成 | ADR-0020（2026-08-23 冻结并按实现修正诚实边界）：Unix 子进程自为进程组长（std `process_group(0)`），停止走 killpg SIGTERM→SIGKILL 阶梯 + 组空探测；Windows Job Object kill-on-close（后代无法脱离）；一切退出路径统一收尾——外部停止用完整阶梯，**正常 exit 0 也组清扫**违约后台成员（前台运行契约）；containment 装配失败拒绝启动；清扫后仍有存活成员的病态残留以新终态 `leaked` + stderr 告警可见；setsid 主动逃逸不可探测（已离开组），文档化残余风险归 M5 沙箱。`tests/plugin_containment.rs` ×2（孤儿覆盖、TERM 忽略升级击杀）+ containment 单测 ×2；LIFO 预算不变、无重启风暴；协议代数未提升 |
| M4-05（外部 transport provider） | ✅ 仓库实现完成 | ADR-0021（2026-08-23 冻结）：daemon→插件反向 JSON-RPC——已认证插件以 `providerRegister`（capability `transport_provider`，manifest 第 8 词）注册连接为 provider 端点（latest-wins、断连属主注销、未决调用立即失败）；守护进程侧出站 pending 表按 id 路由应答、迟到丢弃计数，预算 probe 10s/send 30s/receive 60s（hermetic 缝 `MAILD_TEST_PROVIDER_TIMEOUT_MS` 仅测试进程）。受管账户目录 format_version 1→2：incoming/outgoing 新增 `{kind="plugin", provider, poll_secs?}` 委托变体，v1 双读禁 plugin 变体、写入落盘 v2，旧 daemon 走既有 FutureVersion fail-closed；委托方向凭据 revision 必须为空且拒绝提交秘密（凭据不跨缝），probe/send/receive 反向合同单调十进制 uid 游标（库内 max(uid)，批次 ≤20 收敛于单帧上限）；委托收信复用 store/filters/OTP/notify/推送全链路、(account,uid) 唯一索引幂等；委托方向**绝不回退内置 transport**——断线 `-32018`、超时 `-32019`、插件业务失败 `-32020`（detail 有界脱敏）；`listExternalPlugins.provider_registered` 观测位；client 解码新增 Request 帧 + 暂存缓冲 + 按 id 应答原语，profile 视图容忍缺省字段；桌面账户卡显示「由插件接管」徽标并锁定向导编辑入口。`tests/plugin_transport.rs` ×2 + `tests/plugin_transport_timeout.rs` ×1（独立进程预算缝）+ provider/registry/client 视图单测；协议代数未提升 |
| M4-06（alias 与 disposable 地址层） | ✅ 仓库实现完成 | ADR-0022（2026-08-23 冻结）：SQLite schema v2 新增 `addresses` 与 `messages.recipient`，v1 打开/恢复均先在候选库完成迁移、精确结构与 integrity 校验再发布；地址为账户从属资源、跨账户唯一、每账户 1024 条原子上限，账户归档后级联删除且中断可重试，历史 recipient 保留。四个 UserOnly RPC（list/create/update/remove）含有界分页；alias 显式地址，disposable 用 OS RNG 生成 12 位 base32 local part、冲突重试 5 次并支持 TTL 惰性过期；`sendFromAccount.from_address` 只接受同账户 active 地址，仍复用父账户凭据、transport、预算与规则，provider `transport/send.from` 同步支持。IMAP To 首值/provider 可选 recipient 贯穿 store/摘要/详情/推送；桌面账户中心管理、compose 选择、详情 chip 与在线世代刷新闭环，大列表按账户索引、默认折叠并本地每页 100 条。上游 catch-all/DNS、转发链 To 改写、SPF/DKIM 是明确外部边界；协议代数未提升 |
| M4-07（NodeSeek Email 第三方 provider） | ✅ 仓库实现完成，真实验收待执行 | ADR-0023（2026-08-23 冻结）：新增社区维护的 `app.maild.provider.nodeseek-email` supervised provider 与 companion CLI；支持 mailbox list/check/create/import、quota 以及纯文本单收件人的 probe/receive/send；API key 仅存 macOS Keychain，生产 origin 固定为 `https://seek.li/openapi/v1`，禁环境代理与 redirect；插件可变数据进入宿主创建的 0700 私有目录，package 升级、回滚、移除时保留；SQLite 持久化 remote mail id 到 synthetic uid；只读请求有界重试，create/send 不自动重放，远端结果不确定时 fail-closed。仓库实现与 hermetic 覆盖已落地，真实插件 list/quota 与收发闭环尚未验收；每次 create/send 必须由用户确认具体目标。Cloudflare/DNS catch-all 方案冻结，不属于本切片 |
| M4.5（日常可用邮箱客户端） | ✅ 仓库内切片全部完成（01–10、G01–G05 全部闭环，egui 已删除；正式凭据公证/Windows 真机/长期日用留外部门禁） | ADR-0024（2026-08-29 接受）固化迁移方向，ADR-0039 与 [ADR-0040](decisions/0040-release-gate-and-compatibility-matrix.md) 闭环独立恢复、默认交付与发布兼容门禁。十个切片见 [ROADMAP](ROADMAP.md#m45日常可用邮箱客户端)；G01–G05 见 [交付计划](design/tauri/IMPLEMENTATION-PLAN.md) |
| M4.5-G01（前端授权闭环） | ✅ 仓库实现完成 | ADR-0030（2026-09-11 接受）：核验发现 ADR-0024 决策 8 那句「现有授权完全足够」是错的——一个诚实声明身份的第三方前端连「把邮件标成已读」都做不到，因为写回、账户、地址与包管理全是 `UserOnly`；同时决策 2 那句「内置前端不享有特权路径」也是假的——`maild-desktop` 全仓没有一处调用 `authenticate`，内置 egui 壳连上 socket 就是 `user`。两条合起来的后果是 **frontend 主体这套机制对任何真实前端都不可用**。本片把 frontend manifest 的 intent 路由（`capabilities`）与纯授权（`permissions`，`schema_version = 2` 起）拆成两个字段——往 `message` 这类路由词挂写权限会让每个已安装的只读前端静默获得删邮件的能力；新增 `mailbox_write`（external 词表同步新增，规则插件终于有路可走）/`accounts_manage`/`addresses_manage`/`plugins_manage` 四词，后三个只有前端可声明（**禁止自我繁殖**：后台插件没有理由装下一个插件）；三处归类修正：`status` 与 `prepareReply` 归 `messages_read`、`sendFromAccount` 归 `messages_send`、`requestNotificationAuthorization` 归 `settings_write`；`revealPluginToken` 保持唯一永久 `UserOnly`（持证者不得铸证）。**如实写进 ADR 的两件事**：这条上限是自愿声明而非沙箱（前端与用户同权，取 token 时甚至必须先以 `user` 连一次），它挡的是宿主扩展代理层的 bug 与权限蔓延；以及在 M4.5-09 之前内置 egui 壳仍以 `user` 运行，上限从 M4.5-03 的 Tauri 壳开始生效。已安装包的有效权限逐字节不变，协议代数不提升 |
| M4.5-03-A + E01（Tauri 前端骨架与贡献注册底座） | ✅ 仓库实现完成 | ADR-0031（2026-09-11 接受）：**技术栈** Tauri 2.11 + React 19 + TypeScript + Vite，无 CSS 框架（DESIGN.md 的 token 直接落 `tokens.css`），pnpm workspace 包 `packages/maild-ui`，Rust crate `crates/maild-ui`。**两种构建形态**：默认形态从 `devUrl` 加载、内嵌形态（`--features embedded` = `tauri/custom-protocol`）把 `dist` 编进二进制；tauri codegen 只在内嵌形态检查前端产物，所以 `cargo test --workspace` 永远不需要 Node。**身份 bootstrap 住在 `maild-app`**（`ActorOptions.identity: Option<FrontendIdentity>`）：每次建立 IPC 连接后先用一条短命 `user` 连接 `revealPluginToken`、长连接首帧 `authenticate`、再 `whoami` 校验；失败落 `ViewState.identity_failure` 并按退避重试（用户可能正在装包）；HTTP 端点 + 身份在 spawn 即拒。**桥**（`maild_ui::bridge::Bridge`）是壳与 daemon 之间唯一通道：七个类型化 command、快照事件 `maild://state`、意图事件 `maild://intent`；DTO 投影不含 `last_error`/`body_html`（前者可能带 socket 路径，后者等 04 的隔离文档），错误只有 `offline/denied/not_found/invalid/failed` 四类固定文案；Tauri ACL 为每个 command 生成 `allow-*` 并在主窗口 capability 逐条列出。**E01 注册底座**：命名空间 ID、整包原子提交、按 token 作用域的 dispose、必需角色 `mailbox`/`settings`、`order` + ID 排序；内置 `maild.mailbox`/`maild.settings` 与示例扩展 `maild.sample-excerpt` 走同一个 `register`，坏模块只记诊断不阻塞邮箱。**ADR-0030 决策 11 补遗**：前端主体隐含 `frontend_open` 且只能 claim 自己的 intent（否则 `-32602`）、`notifications_read` 成为可声明词。**真 WKWebView 验证**（dev_stack 假数据）：daemon 日志 `会话绑定主体 frontend:com.maild.ui`；三栏读信、离线账户缓存可读、设置三分区、启用示例扩展后工具栏/菜单/侧栏/工作页同步出现，停用后同步消失。**如实边界**：不是日用版本——无 HTML 原位、无写信、无写回、无账户向导；深色主题只有 CSS 未做交互验证；egui 壳在 09 之前仍以 `user` 运行 |
| M4.5-04（HTML 原位阅读） | ✅ 仓库实现完成 | ADR-0033（2026-09-12 接受）：用户那句「肯定不能接受文本版邮件」在默认前端里第一次成立。**隔离形态**：主 webview 里的 `<iframe sandbox="">`，文档来自自定义协议 `maild-mail://localhost/doc/<token>`（不用 srcdoc——它继承父 CSP；不用子 webview——原生视图与 DOM 布局打架，正是用户否定 egui + webview 时说的 z-order 摩擦）。**三层防线各自成立**：sandbox 属性；文档级 CSP `default-src 'none'; img-src maild-mail: data: [https: http:]; style-src 'unsafe-inline'; font-src/media-src/frame-src/object-src 'none'` 同时写在响应头与 meta；Rust 侧 `lol_html` 重写（脚本/iframe/object/form/base/meta/`on*` 去势，`cid:` → 令牌 part URL，远程 `src` 阻止时挪到 `data-maild-src`、CSS `url()` 中和成 `data:,`，链接 → `maild-mail://localhost/link/<token>/<n>`）+ wry `on_navigation` 对**每个**导航动作否决（只放应用自身与 `/doc/`）+ `on_new_window` Deny。做映射与去势而不是白名单净化——白名单会毁掉邮件版式（`<center>`/`<font>`/`bgcolor`/`<style>`），安全由 sandbox + CSP + 导航否决承担。**令牌作用域的资源通道**：128 位随机令牌绑一封信，`/part/<token>/<n>` 经前端主体认证连接调 `readAttachment`，别的令牌、未落库 part、已释放令牌都是 404；webview 从头到尾不知道消息身份以外的 daemon 细节。**远程内容**：按 `getMessage.remote_content` 拦截，横幅「已拦截 N 处远程内容」；「加载本封」只记会话；「始终允许」写回内核名单；放行只放图片，样式表/字体/媒体永远不加载。**链接先看见再决定**：点击被 Rust 取消并翻译成事件，确认面板显示完整目标与主机，http(s) 才交系统浏览器（Rust 再校验一次），`mailto`/`javascript:`/`file:` 明确分类不执行。**实测修正**：最初把链接改写成未注册的 `maild-link:` scheme，真 WKWebView 上 sandbox 框架对它静默丢弃、根本不到 delegate；改到已注册协议的 `/link/` 路径后成立——UX-SPEC §4「真实拦截能力需 WKWebView 验收」说的就是这种事。**真 WKWebView 计数验证**（dev_stack 新增 fixture 邮件 + 本机远程图片计数服务器）：表格/内联 CSS/CJK/`cid:` logo 渲染、无脚本弹窗、计数 **0**；「加载本封」后计数 **2**；「始终允许」后横幅换成信任依据、计数 4；设置「阅读与隐私」移除条目后回到拦截；`javascript:` 链接面板写明不执行、`mailto` 面板给复制地址；「保存」弹出系统面板且建议文件名正确。截图：`docs/design/tauri/screenshots/m4.5-04-*.png`。**如实边界**：这是引擎 sandbox + 两道 Rust 门，不是进程沙箱；内核不记录一封信原本有没有 HTML，历史缓存只能中性地写「此缓存中没有 HTML 版式」；⌘点击/中键无效；Windows 协议形态只有代码路径没有验收；主窗口配置从 JSON 挪进代码。**仍不是日用版本**：无写信、无写回 |
| M4.5-G03（daemon 拥有的持久草稿） | ✅ 仓库实现完成 | ADR-0034（2026-09-13 接受）：写信这条路上原先只有两端是真的——`prepareReply` 能成型、`sendFromAccount` 能投递，**中间那一段「用户正在写的那封信存在哪里」不存在**；旧 egui 把 compose 状态放在进程内存里，而 ADR-0024 之后常驻的是 daemon、UI 随时会被关掉，等于把最不该丢的东西放在最容易消失的地方。**草稿是字段不是报文**：`Date` 与 `Message-ID` 仍在投递那一刻生成（烤进草稿会让周一起草周五发出的信带着周一的日期，且「复制一份草稿」会变成两封同 ID 的信）。**进邮件库（schema v7→v8，两张新表 `drafts` / `draft_attachments`，既有表一列不动）**——与阅读策略（ADR-0032 决策 5 刻意留在库外）作**相反**的选择，理由是同一套：草稿是用户写的内容、与邮件同类，恢复一份三天前的备份就该把当时那封没写完的信一起带回来；收件人列存 JSON 数组而不是逗号连接（显示名形式里本来就有逗号）。**`version` 是唯一并发仲裁**：`updateDraft` 必须带 `base_version`，不符返回 `-32006` 且**一个字节都不写**、`data.version` 给出当前值；不用时间戳（两个进程时钟不可比，而要回答的本来就是「你读到的那份还在不在」），不做三方合并（正文不是代码）。这条规则同时定义了「已保存」的唯一含义——回执里的 `version`/`updated_at` 是事务提交之后才产生的，前端在此之前只能显示「未保存的更改」。**patch 语义**：缺席不动、出现即整体替换，未知字段名一律 `-32602`（自动保存每几秒一次，被静默忽略的字段意味着用户看着「已保存」而内容没存）。**上限一律拒绝绝不截断**（`-32007` 带 `field`/`limit`）：正文没有第二份副本，悄悄砍掉一半比拒绝保存恶劣得多；数量到顶时拒绝新建而不淘汰最旧的一份。收件人**不做**地址语法校验（草稿要装得下打到一半的 `alice@`），但头部字段里的换行与控制字符在写入时就被拒——头注入的最早一道门。**附件是引用不是字节**：指向库里已有的 `(account, folder, uid, part_index)`，写入时按 G02 的同一判定当场校验（`-32002`/`-32003`/`-32004`），读回时的 `stored` 是现查的；写入时校验不是保证，08-A 发信必须再查一次。**两个新授权词 `drafts_read` / `drafts_write`**（frontend 与 external 词表同步）：不并进 `messages_read`（收件箱里的信是既成事实，草稿是用户还没决定要不要发的字），也不并进 `messages_send`（能改草稿的主体可以在用户按下发送之前换掉收件人）；拆读写两词是照 `settings_read`/`settings_write` 的形状，且「AI 起草回复、用户审阅后自己发送」这类插件要的正是「能写草稿但不能发信」；`compose` 路由**不隐含** `drafts_write`，默认前端本片不声明这两个词（08-A 才用得到）。**不同步到服务器 Drafts**：IMAP 没有「更新一封信」，每次自动保存都要 `APPEND` + `EXPUNGE` 留一串墓碑，且会让「保存成功」变成异步的——诚实后果是手机上看不到电脑上写了一半的信。**不推草稿事件**：推送流的授权词是 `messages_read`，推草稿正好把刚划的界抹掉。共享 client 的 `ClientError` 补上 `rpc_data`——daemon 往 `data` 里放的是可据以行动的东西（当前版本号、字段与上限），丢掉它调用方就只能显示一句笼统的失败。`maildctl draft list/show/new/set/attach/detach/rm` 让整条链不依赖 UI 即可验收，两个终端就能把「过期更新不覆盖」演一遍。协议 additive，代数未提升 |
| M4.5-G02（附件字节通道与远程内容策略） | ✅ 仓库实现完成 | ADR-0032（2026-09-12 接受）：ADR-0025 把附件字节存进了库却**没有任何协议方法能读出来**，`cid:` 图片与「保存附件」在任何前端都做不了；可信发件人名单也没有持久状态与接口，换一个前端就换一套隐私策略。**传输形式**：`readAttachment` 按完整四元组 `(account, folder, uid, part_index)` 分块 base64，单块 ≤ 512 KiB 原始字节走**既有** 1 MiB 帧——不扩帧、不引第二种传输（原始二进制升级打破一行一 JSON 的框架契约且 HTTP 无对应形态；HTTP `GET` 等于第二套鉴权面；导出目录是 ADR-0025 拒绝过的第二存储位置）；代价是 33% 膨胀与 2 MiB 最多 5 个往返，本地 socket 上不可感知。**`folder` 必填不消歧**：UID 只在 mailbox 内唯一，读错一封信的附件字节比读错正文更难察觉；消息必须可见（乐观删除中的读不到）。**三种「拿不到」可区分**：`-32002` 消息不存在、`-32003` part 不是它的附件、`-32004` part 存在但字节未落库（超限或历史缓存）；附件元数据加 `stored` 位，UI 据此决定要不要画一个能点的下载按钮而不是先做按钮再让它失败，旧 daemon 缺省 `false`。**授权**：`readAttachment` 归 `messages_read`（同一封信的同一份内容，不是新权限面）；`getReadingPolicy` / `setReadingPolicy` 归 `settings_read` / `settings_write`。**策略是 daemon 文件状态**（`reading_policy.toml`，与活动配置同目录，纪律同 `plugin_state.toml`：0600、拒符号链接、64 KiB、未知版本 fail-closed、临时文件 + fsync + 原子 rename）**而不进邮件库**——恢复一份旧邮件备份不该静默恢复旧的信任名单；损坏即停摆：读缺省、写 `-32001`、绝不覆盖。`setReadingPolicy` 是原子 patch（`remote_content` / `trust` / `untrust`，任一项非法整包拒、幂等、上限 500、未知键拒）；`getMessage` 应答与 `message` 同级带回 `remote_content {decision, reason}`，匹配规则固定在内核：先看全局放行，再比地址，再比**精确**域名（子域不匹配父域）。**诚实边界**：内核只给结论不代取远程资源，执行是 04 渲染隔离层的事；这不是沙箱。`maildctl attachment save`（`hard_link` 发布，目标已存在即失败不覆盖）与 `maildctl reading-policy show/set/trust/untrust` 让整条链不依赖 UI 即可验收；已在 dev_stack 上实跑：184 320 字节附件逐字节对上、重复保存被拒、策略文件 0600 落盘。共享 client 的 `read_attachment` 带两道护栏（`size` 中途变化 / 累计超量即报错）。协议代数未提升，store 无 schema 变更 |
| M4.5-01（消息模型与 store schema v3） | ✅ 仓库实现完成 | ADR-0025（2026-08-29 接受）：schema v2→v3 一次迁移覆盖全部结构——`messages` 增 15 列（`message_id`/`in_reply_to`/`reference_ids`/`cc`/`bcc`/`reply_to`/`date_at`/`sort_at`/`body_html`/`snippet`/`has_attachments` + 四个标记位）、新表 `folders`（结构预置，M4.5-02 首个写者）与 `attachments`、两个排序索引；迁移前先精确校验来源 v2 结构，使畸形历史库报 schema 错误而非含混的 SQLite 列缺失；`MIN_SUPPORTED_SCHEMA_VERSION` 保持 1，v1/v2 备份恢复后在候选库迁移到 v3 再发布，v2 因此获得自己固化的结构快照与保留对象拒绝表。**两个正确性缺陷已修**：`Date` 头经 `mailparse::dateparse` 落 `date_at`（早于 1971 年的解析结果视为垃圾丢弃），排序改用 `sort_at`——落在 `received_at` 前十年/后 24 小时之外的 `Date` 不可信、回退投递观测时间，但 `date_at` 原样保留，伪造未来日期无法霸榜；正文抽取换成单遍字节状态机（无新依赖），`script`/`style`/`head`/`title`/`textarea`/`noscript` 的**元素内容整体丢弃**，正确处理注释/CDATA/属性值内的 `>`/具名与数字实体/块级换行，闭标签缺失时不吞正文。HTML 原文有界落库（256 KiB，截断追加 `<!-- maild: truncated -->`），纯文本 128 KiB；附件元数据全落、内容按 2 MiB/part 与 8 MiB/封上限内联，超限只留元数据且 `size` 如实，`content_id` 建索引供 M4.5-04 的 `cid:` 解析。载荷边界由单帧上限倒推：`listMessages` 与 `message/received` 一律不携带 HTML 与附件，HTML 只经 `getMessage`。`sort_at` 由 store 在落库时重算而非照抄载荷。协议 additive（新字段全部 serde default + 空值省略），provider 合同新增可选 `date`/`message_id`/`cc`/`reply_to`；协议代数未提升 |
| M4.5-02（文件夹与增量同步，store schema v4） | ✅ 仓库实现完成 | ADR-0026（2026-08-29 接受）：**一封邮件的身份从 `(account, uid)` 变成 `(account, folder_id, uid)`**——v3 的唯一索引会把收件箱的 UID 42 和已发送的 UID 42 当成同一封；附件同步分维，否则同号 UID 的附件会互相顶掉。schema v3→v4：`messages`/`attachments` 增 `folder_id`，`folders` 增 `display_name`/`present`，换掉两个唯一索引并新建文件夹排序索引；历史行**断言式**回填到自动创建的 `INBOX`（v4 之前 receive 只 SELECT 一个 mailbox，委托 provider 也只有一个信箱），游标同时设为该文件夹已有的最大数字 UID，升级后不重下收件箱。收信改为 **per-folder UID 游标增量抓取**（`{last+1}:*`，并按 RFC 3501 的 `N:*` 语义过滤掉服务器回带的最大 UID 那条），取代此前每次连上都 `1:*` 拉全量；`UIDVALIDITY` 与库内不符即在单事务内重建该文件夹（删消息+附件、清游标），服务器不返回 `UIDVALIDITY` 则跳过该文件夹。**连接预算每账户恒一条**：IDLE 只挂主文件夹，其余文件夹在同一条连接上按 `folder_poll_secs`（默认 300s，下界 30s）sweep，同步文件夹数上限 64。role 判定 `INBOX` 保留名 → RFC 6154 SPECIAL-USE → 常见中英文名启发式 → **猜不出留空**；mailbox 名的 modified UTF-7 自解（只解不编，解码失败回退原名）。**只有收件箱走 `MailReceived`/`MailFiltered` 管线**，其余文件夹直接落库，不触发 filters/OTP/通知。服务器上消失的文件夹只标 `present=0`，不删本地邮件。协议 additive：新增 `listFolders`（`messages_read` 域），`listMessageSummaries`/`getMessage` 加可选 `folder`，摘要与消息增 `folder`/`folder_role`；协议代数未提升。实现期定位到一条硬约束并写入 ADR 决策 11：受管 receive 的停止有超时上限、超时即放弃 join，**收信循环不得跨 await 持有 `Arc<Store>`**，否则数据库锁会被扣在一个已放弃的任务里导致下次启动打不开库 |
| M4.5-05（写路径与离线队列，store schema v5） | ✅ 仓库实现完成 | ADR-0027（2026-08-29 接受）：已读/星标/已回复/删除、归档移动可用，**本地乐观写与入队在同一个 IMMEDIATE 事务里**，点了立刻生效不等网络。核心语义是**「结果未知」是独立终态**：`pending → in_flight → done / failed / unknown`，`unknown`（命令已发出、未见 tagged 响应）对可重放的 flag 类自动重试（上限 5 次后转 failed），对不可重放的 move 类是**终态**，交用户经 `resolvePendingOp` 裁决——重放会在目标文件夹产生第二份副本而我们无从判断第一次成没成。崩溃时停在 `in_flight` 的 op 在下次连接建立时按同一规则收敛。`MOVE` 降级链：宣告 `MOVE` 走原子 `UID MOVE`，否则 `UID COPY` + `+FLAGS \Deleted`；**没有 `UIDPLUS` 就不 `EXPUNGE`**——裸 `EXPUNGE` 会连带清除其他客户端标记 `\Deleted` 的邮件，那是别人的数据。删除策略在内核侧、**入队时**判定：有 role=trash 的文件夹就移进去，否则落 `\Deleted`，因此队列里每条 op 的可重放性在入队那一刻就已确定。乐观移动的本地表现是「从源文件夹消失」（`pending_removal`）而不是改 `folder_id`——目标 UID 由服务器分配，本地无从预知；成功删源行、失败放回来、未知保持隐藏等裁决。对账双护栏（`flags_local_at` 的 60 秒时间窗口 + 未落定 op 的 `(folder, uid)`）缺一不可：前者挡「op 刚 done、服务器这次 FETCH 还没反映出来」，后者挡长时间离线堆积的写；CONDSTORE 时按 `HIGHESTMODSEQ` 短路整段对账、否则全量并顺带做存在性对账（本地有服务器没有 = 被其他客户端 EXPUNGE）。写路径复用收信那条连接（ADR-0026 决策 3），API 只做「改写 + 入队 + 敲醒」，读写因此天然串行。五个方法（`markMessages`/`moveMessages`/`deleteMessages`/`listPendingOps`/`resolvePendingOp`）全部 `UserOnly`——并进 `messages_read` 会让任何可读插件删用户的邮件；协议代数未提升 |
| M4.5-06（发信补全、回复/转发与 Sent 归档，store schema v6） | ✅ 仓库实现完成 | ADR-0028（2026-08-29 接受）：发信路径自 R4 起没动过——三个头、一个纯文本正文、一个收件人。本片补全为完整的 RFC 5322 报文。`compose` 模块负责 MIME 组装、RFC 5322 日期（civil-from-days，不引日期库）、`Message-ID` 生成、RFC 2047 encoded-word（在**字符边界**折行）与回复/转发成型。**`Date` 与 `Message-ID` 自己生成**：本地知道自己发出去那封信的 ID，`APPEND` 到已发送的副本与投递出去的那封才是同一个 ID，会话链因此闭合。正文编码只在 `7bit` 与 `base64` 之间选（不实现 quoted-printable：对中文膨胀 3 倍，且少一个编码器少一处出错）——**副作用是组装结果一定是纯 ASCII**，因而能原样进 `pending_ops.payload` 这个 TEXT 列。**Bcc 只进 SMTP 信封，绝不进头**；收件人字段接受字符串或数组，且**逐项**过头注入防线（校验每个 addr-spec 而不是拼接后的串），合计上限 100。新增 `prepareReply`（UserOnly）在内核成型 reply/reply_all/forward：主题反复剥离 `Re:`/`RE:`/`答复:`/`Fwd:`/`FW:`/`转发:` 前缀后只加一个，`References` 继承原信 References + 原信 Message-ID（缺省退回 `In-Reply-To`），超限时**保留首条与最后若干条**——首条是会话根，丢了整条会话就断；全部回复排除账户自身地址与其全部别名（ADR-0022）并去掉已在 To 里的人；转发开启新会话（不继承会话头）并携带原信的**非内联**附件引用。投递成功后把**同一份报文**入队 `append` op 归档到已发送——`APPEND` 失败不影响「已经发出去」这个事实（否则用户会以为没发出去然后重发），且 `APPEND` 不可重放，「结果未知」是终态。schema v5→v6：`pending_ops.kind` 增加 `'append'`，SQLite 不能改 `CHECK`，走表重建（新建→拷贝→删除→改名）。委托 provider 合同**不扩展**（ADR-0021 保持单收件人纯文本），传 Cc/Bcc/HTML/附件明确拒绝而非静默降级。**新文件附件上传是明确非目标**：1 MiB 单帧 base64 后装不下真实照片，做一个用起来就失败的功能更糟；正确形状是 ADR-0019 的 staging 目录，属独立切片。协议代数未提升 |
| M4.5-07（本地检索与线程聚合，store schema v7） | ✅ 仓库实现完成 | ADR-0029（2026-08-30 接受）：库里从 M4.5-01 起就存着搜索与线程需要的全部事实，但没有一个方法用得上。本片补上，且**不引入新的索引引擎依赖**——`libsqlite3-sys` 的 bundled 构建已带 `-DSQLITE_ENABLE_FTS5`，索引与邮件同库，备份/恢复一并覆盖。**最关键的一个决定是中文分词**：`unicode61` 把连续中文当成一个 token（「发票」搜不到「发票已开具」），`trigram` 又要求查询词 ≥3 字符（排除掉中文里最常用的两字词）。改为内核在写入索引前把 CJK 字符**逐字空格隔开**，查询侧同样切分并作为**短语**下发——FTS5 短语要求 token 位置相邻，恰好等价于原文子串；任意长度的中文查询都能命中且不跨词误命中，日文假名同样处理，**韩文谚文不切**（韩语本就用空格分词节）。`messages_fts` 是**独立内容表而非 external content**：后者的删除必须提供原始列值，一旦某行尚未回填就发 `'delete'` 会把索引写坏且不报错；独立表的 `DELETE ... WHERE rowid = ?` 对不存在的行是无害 no-op。**查询表达式由内核构造，绝不把用户输入透传给 `MATCH`**（`"`/`*`/`-`/`(`/`:`/`NEAR`/`AND`/`OR` 全是语法元素，透传轻则报错重则跨列检索），代价是用户写不了查询语法——这是有意的。结构化过滤（发件人、时间范围、有无附件、文件夹、账户）走 SQL `WHERE`，时间用 `sort_at` 与列表显示的时间同源。结果**按时间倒序不按 bm25**：邮箱搜索的用户模型是「我知道有这么一封」，而相关度排序会让同一次搜索在不同时间给出不同顺序。线程物化 `thread_id`，边表 `thread_refs` 记录自身 `Message-ID` + `In-Reply-To` + `References` 的每一个 id；**边包含尚未见过的 id**，因此到达顺序无关（回复先到会留下指向未来父信的边），回填与实时落库可以并发且最终收敛；引用超限时保留首条与最近若干条（首条是会话根）。**主题不参与线程判定**——「你好」「发票」这类主题天天重复，错并比漏并严重得多。schema v6→v7 **一行历史消息都不索引**：迁移跑在打开数据库的立即事务里，几万封邮件的分词会让 daemon 启动挂起、WAL 膨胀、崩溃全回滚；只记下 `MAX(id)` 作为回填起点，由后台任务分批推进（每批 200 条，游标与索引写入同事务提交，中断即续），任务只持 `Weak<Store>`，且卸载逆是**异步**的——只发停止信号加 `abort()` 不够，正在 `spawn_blocking` 里的那一批持有真 `Arc`，会把数据库锁扣在一个已经没人引用的任务里导致下次打不开库；必须有界等它收尾（ADR-0026 决策 11 的另一半，实现期实测到）。**删除方向由 SQL 触发器兜底**，写入留在 Rust（切分是 Rust 函数）：漏写入只是搜不到，漏删除会让搜索命中一封已经不存在的邮件，而删消息的路径有三条。FTS5 建表语句与触发器体在打开数据库时**逐字节校验**——换掉分词器不报错，只会让所有中文查询静默失效。新增 `searchMessages`/`getThread`/`getSearchIndexStatus` 三个 `messages_read` 方法（换检索维度不构成新权限面），`scope: "server"` 走服务器 `SEARCH` 且**复用收信那条连接**（ADR-0026 把连接预算上限定成了 1）；协议代数未提升 |
| ADR-0041（低占用后台收信） | ✅ 仓库实现完成并接线 | `initial_sync_limit` 冷启动窗口与 50 封分批、O(folders) 去重环、keepalive/阶段超时/指数退避、`trigger_sync`/`fetch_message_by_uid`、SQLite 运行时收紧、内存归还与 Agent QoS、`status.runtime`；`tests/receive_footprint.rs` ×7 + release `footprint_bench` |
| ADR-0042（SMTPS 465 / AUTH LOGIN / 自动发现） | ✅ 仓库实现完成并接线 | 隐式 TLS greeting 状态机、AUTH PLAIN/LOGIN 协商与秘密清零、超时与可行动诊断、`autodiscover` 知识库与 SSRF 边界、`discoverEmailProvider`/`listEmailPresets`、`maildctl account discover`；`tests/smtp_implicit_and_auth.rs` ×10 |
| ADR-0043（智能通知与规则持久化） | ✅ 仓库实现完成并接线 | 上下文验证码、收件箱新邮件通知与 10 秒 3 封限流、`rules_state.toml` 覆盖层原子侧车、`setSettings {persist}`、菜单栏「立即同步」、首启配置默认开启；`tests/smart_notify_and_rules.rs` ×18 + `maild-ui/tests/daily_mail_bridge.rs` |
| Tauri 登录向导与日常交互 | ✅ 仓库实现完成并接线 | 12 类服务商预设、自动发现、探测分阶段建议、零账户欢迎卡；立即同步、服务器历史检索 + 按需拉回、验证码条、HTML 深色适配、键盘流；Vitest 新增 73 项 |
| 收官：release 资源实测、打包与文档 | ✅ 完成（2026-11-17） | `crates/maild/examples/footprint_bench.rs` 实测；真实 `Maild.app` 构建 + ad-hoc 由内而外签名 `codesign --deep --strict` 复验；`bash -n`/dry-run/test_bundle/test_release/test_lifecycle 全 PASS；全量回归见下方记录 |
| ADR-0044（macOS 单 Dock 图标身份） | ✅ 仓库实现完成并合入（2026-11-17） | `maild-launcher` 延迟 AppKit、`StartOnce` 经 `NSWorkspace` 在独立 Coalition 打开 `Maild Agent.app`（消除 `exited-with-subordinates`）、健康内置默认前端原位 `exec` 同包 `Contents/MacOS/maild-ui`；`maild-ui` `productName = "Maild"`、`.main-ui.lock` 单实例、`RunEvent::Reopen` 有界串行排空意图；通知/菜单栏激活顶层 `Maild.app`；打包单宿主 `maild-ui` + `Maild.icns` + 显式 `--identifier com.maild.ui --options runtime` 签名与负向门禁；见[合入回归](testing/README.md#2026-11-17单-dock-身份adr-0044合入回归) |
| ADR-0045（关闭退回菜单栏、最小化原生、`frontend/intent` 唤醒） | ✅ 仓库实现完成并合入（2026-11-18） | 顶层 `Maild.Info.plist` `LSUIElement = true`（打包门禁断言）；`maild-ui` 启动切 `Regular` 并激活，`×` / `⌘Q` / 放行的 `ExitRequested` 先 `retire_window`（隐藏 + `Accessory` + `deactivate`）再 `exit(0)`；黄灯 / `⌘M` 保持系统原生最小化（不劫持标题栏按钮与菜单）；守护进程 `FrontendIntentQueued` → IPC `frontend/intent` 只投给目标前端主体 → `maild-client` `FrontendIntent` → `maild-app` `intent_epoch` → 状态泵 `show_and_activate_main_window`（含 `unminimize`）+ 领取；启动器对运行中窗口排收件箱意图并聚焦；见[合入回归](testing/README.md#2026-11-18关闭退回菜单栏adr-0045合入回归) |
| ADR-0047（品牌解耦 `brand.toml` 与开源就绪） | ✅ B0 `f853c2f`、B1 `7cd4d65`、B2 `56ccfbe`、B3 `a992a86` 于 B4（`cliouo/brand-featherpost-integration`，合并 `3d7dca3` / `d43b422`）汇合；门禁：前端 typecheck + 191 测试 + build，`cargo test --workspace --all-targets --locked` 829 通过 / 0 失败（`maild-ui/tests/bridge.rs` 11/11），macOS 与 `x86_64-pc-windows-gnu` Clippy `-D warnings`，`dry-run.sh` / `test_bundle.sh` / `test_release.sh` / `test_lifecycle.sh` 与 `sync-brand.sh --check`（12 派生文件同步）全部通过；Release 产出 `featherpostd` / `featherpostctl` / `featherpost-agent` / `featherpost-launcher` / `featherpost-menu` / `featherpost-ui`；恢复测试改为严格读取 `frontend.toml.in`（删除旧模板回退）。残留扫描仅剩有意保留项：`maild://` 桥事件、`maild-*-bundle-id:` / `maild-ui-embedded:` 扫描标记、`MAILD_TEST_*` 缝、launcher 旧配置头兼容、发布脚本拒收旧 `dev.maild.*` provisional ID 与测试夹具 | `brand.toml` → `scripts/sync-brand.sh` 生成 `maild-brand` 常量、`brand.ts`、`brand.env`、`*-bundle-id.txt`、Tauri 配置、开发期前端清单与 `[[bin]]`；`packaging/macos` 模板改为 `App/Agent/Menu/UI.Info.plist.in` + `frontend.toml.in` 的 `__BRAND_*__` 占位符，删除 `Maild.icns` 改用 `assets/AppIcon.icns`，全部脚本 / 测试 source `brand.env`（旧 `MAILD_BUNDLE_*`、`MAILD_MAINTENANCE_*` 兼容回退），provisional 门禁为 `dev.<slug>.*`；`ci.yml` 新增 `brand` job，`release-macos.yml` 产物名取自 `brand.env`、仓库变量改为 `MACOS_*`；`test_bundle.sh` 换牌夹具（`Quill & Ink`）证明零脚本改动换品牌；README / BRANDING / 运维镜像说明，`reference/`、`paper.pdf`、`vendor/cordis` 引用改为上游公开链接 |
| ADR-0046（菜单栏与后台生命周期绑定、Dock「退出」零残留） | ✅ 仓库实现完成并合入（2026-11-18） | 菜单栏 `✉` 与 Agent 生命周期绑定：menu supervisor 在 Agent 运行期间无论菜单如何退出都按退避重启；菜单最后一项「退出 Maild」调 `stopManagedAgent`（离线兜底经 `.main-ui.lock` 结束遗留主 UI），守护进程先向已认证 `Principal::Frontend` 长连接推送 `agent/stopping`（user/插件收不到），排空窗口 350 ms 后正常 LIFO 退出并清理 socket；`maild-client` `AgentStopping` → `maild-app` `ViewState.shutdown_requested`（不进快照缓冲、置位不回落）→ 状态泵 `request_close_and_exit(AgentStopping)`（500 ms 草稿兜底）。红灯 `×`、`⌘Q`、Dock 右键「退出」经运行时注入的 `applicationShouldTerminate:` 收进同一退出流水线（尊重前端否决，先隐藏 + `Accessory` 再 `exit(0)`），仅关闭窗口并彻底清空 Dock 图标，后台与菜单栏继续运行；系统注销/重启同步降级并返回 `NSTerminateNow`。`maild-ui/tests/bridge.rs` 真 daemon 端到端钉住 `stopManagedAgent → agent/stopping → shutdown_requested` 先于 daemon 退出到达；见[合入回归](testing/README.md#2026-11-18菜单栏生命周期绑定adr-0046合入回归) |

**日常收信增强实施记录（2026-11-17）**：四条分支并行实现后在 `5bcb9d6` 汇合。内核侧：冷启动只同步每个文件夹
最新 `initial_sync_limit`（缺省 200）封，更早历史经 `searchMessages {scope:"server"}` 定位、`fetchServerMessage`
在同一条收信连接上按需拉回；会话去重改为每文件夹 256 槽环形窗口；半开连接靠 keepalive 与阶段超时识别，断线
指数退避封顶 300 秒且可被 `syncMessages` 立即打断；SQLite 页缓存 1 MiB、WAL ≤ 4 MiB、无 mmap，冷启动与回填
后归还内存；`reqwest` 移出生产依赖，release 开 thin LTO + strip。SMTP 465 先读 greeting，AUTH 在 PLAIN/LOGIN
间协商；`discoverEmailProvider` 零网络命中主流服务商、显式允许时才做带 SSRF 关卡的公网探测。验证码识别改为
上下文感知，收件箱新邮件通知按账户 10 秒 3 封限流；`rules_state.toml` 覆盖层侧车让界面改过的规则重启保留。
协议全部 additive，协议代不提升。

**收官实测记录（2026-11-17）**：`crates/maild/examples/footprint_bench.rs` 以 release `maild` 独立子进程对 3 个回环
TLS 账户（主信箱 1,500 封、另两个各 300 封）实测，连续 3 次全部通过：冷启动到 3 × 200 封落库且 FTS 收敛
185–200 ms；稳态 RSS 14.2–15.4 MiB、`phys_footprint` 4.95–5.08 MiB；5 秒空闲 CPU 0.0009–0.0013 %（1 次唤醒）；
30 封实时 IDLE 推送 P50 3.8–4.1 ms / P95 7.4–10.5 ms；验证码通知恰好 6 条、24 封普通信限流为 12 条；注入后
footprint +0.19～+0.33 MiB、WAL 恒 4 MiB；窗口外第 5 封经服务器检索 + 按需拉回正文逐字一致；守护进程
SMTPS 465 + `AUTH LOGIN` 发信回环 4–6 ms。详表见
[testing](testing/README.md#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)。
打包：前端生产构建 + release `maild`/`maild-agent`/`maild-launcher`/`maild-menu`/`maild-ui --features embedded`；
`bash -n`、`dry-run.sh`、`test_bundle.sh`、`test_release.sh`、`test_lifecycle.sh` 全 PASS；另用真实二进制生成
28 MB `Maild.app`，`verify-bundle.sh` 通过、`recovery.sha256` 与 `maild-ui` 一致，按 ADR-0040 由内而外 ad-hoc
签名 + `seal-recovery-payload.sh` 后 `codesign --verify --deep --strict=all` 通过，`dist/` 已清理。
全量回归：`pnpm --dir packages/maild-ui typecheck && test && build` 通过（Vitest 191 passed / 12 文件）；
`CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked` **795 passed / 0 failed / 2 ignored**；
macOS 与 `x86_64-pc-windows-gnu` 全工作区 `cargo clippy --all-targets --locked -- -D warnings` 均零警告。
详见 [testing](testing/README.md#收官全量回归2026-11-17)。

**M4.5-08-C 实施记录（2026-09-23）**：[ADR-0038](decisions/0038-settings-and-managed-packages.md)。
原窗口设置接入账户新增/探测/编辑/凭据更新/启停/软移除、运行时规则、显式通知授权、分页地址管理，
以及受管包安装/升级/回滚/启停/移除与默认前端选择。输入只留会话内存，已有秘密不回填；
离开与关窗保护覆盖等待草稿保存的全过程，通知定位等待设置丢弃确认。混合收发 transport 独立编辑。
安装使用原生目录选择、同一份 staging 副本与完整授权预览；令牌有界、过期回收且单次消费，失败不自动重试。
包刷新重扫目录后再读清单。默认前端隐藏状态文件原子持久化；移除旧 TOML 默认后重启仍使用新选择。
一并修正账户 outgoing:null 的省略/移除区分，以及已启用扩展移除后残留启用记录导致重启失败的问题。
schema 保持 v10；验证结果与未完成的原生视觉复核见 [测试清单](testing/README.md#m45-08-c账户设置与受管扩展adr-0038)。
**M4.5-G05/09-A 实施记录（2026-09-23）**：[ADR-0039](decisions/0039-native-recovery-and-tauri-default.md)。
固定副本只从当前 Maild.app 的 launcher/Agent 布局发现；首次预装 no-clobber 与持久标记保留用户移除意图。
显式修复仅替换 `com.maild.ui` 并重置默认；旧包/损坏选择留在 `.repair-backups`，发布失败回退。
离线入口取得 Agent、前端目录与启用持久库时的实际数据库维护锁，不打开 SQLite；在线 IPC 仅 User 主体可调，
复制和刷新在断连后仍持有包事务锁。测试与限制见 [测试记录](testing/README.md#m45-g0509-a恢复内核adr-0039)。
自动化：完整工作区 761 passed / 8 ignored，新增真实 socket 用例后的恢复专项 27 passed（重叠计数），新增用例合计 26。Clippy 通过；Windows GNU 全目标编译通过，既有警告见测试记录。
本片没有接线实际预装、切默认或删除 egui，也未交付原生窗口/前端故障隔离。
**M4.5-09-B 实施记录（2026-09-24）**：独立 `maild-launcher` 提供原生修复与后台管理，
旧壳 `--repair` 暂时桥接且不初始化 egui；Menu 可离线打开，Tauri 设置仅提供导航入口。
`builtin-repair` 只接 `background`。缺包、损坏默认或前端目录不可用不停止 Store/API；
保留原默认选择及身份凭证，拒绝失效目录上的包变更。`stopManagedAgent` 仅本机 User IPC，走正常退出。
复审修正隔离 HOME 的旧阅读器二次启动、异步激活过早关窗和无主错误窗口累积；
原生模拟窗口已验证恢复确认/取消、启动/停止状态、错误提示、返回与退出，不操作真实后台或邮件。
核心 248 passed / 1 ignored、client 55 passed、launcher 5 passed；前端 116 passed、build 与全工作区 Clippy 通过。
**验收已封板（2026-11-12 补验）**：清理增量缓存后重跑完整工作区 779 passed / 0 failed / 8 ignored
（含 Tauri bridge 9 passed）、全工作区 Clippy、`test_release.sh` 与 `test_lifecycle.sh` 均通过。
详情见 [测试清单](testing/README.md#m45-09-b原生修复窗口与故障隔离adr-0039)。
**M4.5-09-C 实施记录（2026-11-12）**：`maild-launcher` 成为 `Maild.app` 顶层可执行文件，负责无凭据
默认配置（新配置默认 `com.maild.ui`）、Preview 启动一次内嵌 Agent、请求 Agent 打开默认前端；
做不到时显示原生窗口。首次预装放进 Agent 前端插件启动：持目录租约、首次扫描前 no-clobber 执行，
覆盖登录项、Preview 和升级三条路径。`builtin-egui` 不再是目录条目，配置与持久选择在读取时
映射为 `com.maild.ui`（不改写文件），显式请求旧 ID 走 `builtin-repair`。
打包改为四二进制，并新增 `Resources/frontends/com.maild.ui`（模板生成的清单、`recovery.sha256`、
`Maild UI.app`）。签名后由 `seal-recovery-payload.sh` 重算摘要，再签顶层 App。
Agent 每次启动时，若已装 `com.maild.ui` 版本严格旧于内置副本即替换（旧包备份、默认不变），应用升级会带上新界面。
09-C 之前的旧布局只在本机装过、从未分发，不做打包兼容。`crates/maild-desktop` 与 egui
依赖删除（Cargo.lock 约 -330 行）。验证：工作区 712 passed / 2 ignored（含 bridge 9）、Clippy、
Windows 交叉检查（maild-ui 的既有 `icon.ico` 缺口除外）、三套打包测试；真实本机构建 + ad-hoc 签名 Preview
在隔离 HOME 下验证预装、移除不装回、在线修复、旧默认迁移。详见 [测试清单](testing/README.md#m45-09-ctauri-默认交付与删除-eguiadr-0039)。
顺序：**10 发布门禁**（见下）。

**M4.5-10 实施记录（2026-11-12）**：[ADR-0040](decisions/0040-release-gate-and-compatibility-matrix.md)。
补齐 Tauri 默认前端的 ADR-0016 协议代握手与双端写闸门：按在线 `connection_epoch` 单次 `getAgentInfo` 握手，
跨代断连或重连自动丢弃过期结果并重握；不一致时显示常驻横幅与侧栏只读状态、禁用在线写命令贡献，
Rust 侧 `Bridge::guard` 拦截全部非只读 RPC 与包暂存目录写入（`UiErrorCode::Incompatible`），仅豁免设置页打开
`builtin-repair` 原生修复窗口（不从非特权前端按 PID 发信号）。修正 `BundledFrontend::refresh` 接受历史协议代
（`1..=API_PROTOCOL_VERSION`）执行随包自动升级，同时钉死用户移除后升级永不装回、应用回退遇未来协议代保持不覆盖
并回落 `builtin-repair`（显式修复可恢复）、预装包运行时目录权限无豁免三组矩阵。实证并固化嵌套 `Maild UI.app`
由内而外签名 + `seal-recovery-payload.sh` 重算摘要 + 单归档公证 + 仅顶层 `Maild.app` 执行 `stapler staple` 的封条规则。
补齐 `crates/maild-ui/icons/icon.ico`、消除非 Unix/非 macOS 条件编译警告，更新 `ci.yml` 与 `release-macos.yml`。
验证：全工作区 Rust 717 passed / 0 failed / 2 ignored（含 `maild-ui` 22 单测 + 10 bridge e2e、`recovery` 44 单测），
webview Vitest 118 passed / 9 文件，`tsc --noEmit`、前端生产构建、`maild-ui --features embedded`、macOS 与
`x86_64-pc-windows-gnu` 全工作区 Clippy（`-D warnings`），以及 `dry-run.sh`/`test_bundle.sh`/`test_release.sh`/`test_lifecycle.sh`
全部通过。详见 [测试清单](testing/README.md#m45-10发布门禁扩展与兼容降级矩阵adr-0040)。正式 Developer ID/notary 凭据实跑、
真实会话图形激活、Windows 真机与多日真实邮箱日用观察留外部门禁。

**M4.5-08-B 实施记录（2026-09-23）**：[ADR-0037](decisions/0037-mailbox-workspace-and-sync-activity.md)。
统一收件箱按 role=inbox 在 SQL 分页，未读/星标/全部邮件、账户文件夹树和折叠已接入；全身份
多选按账户/文件夹分组写入，回执区分新入队、已经满足与未处理 UID。归档/移动用真实映射，
删除显示实际策略，无跨账户移动或虚假撤销。中文本地搜索支持结构化筛选、索引覆盖提示和
原位阅读；线程跨文件夹分页，连接/选择代次阻止旧结果覆盖。普通到信保留当前 HTML 文档。
活动页含 settled 失败、ID 分页与受控摘要，APPEND 原文不出库进摘要通道，邮件主题有界读取
且额外要求 messages_read；未知结果确认后才能重试。新通知携带 folder，旧通知回到账户列表。
本片没有 schema 迁移，未切换默认安装。最终 workspace 727 passed / 0 failed / 8 ignored，
前端 77 passed；Clippy、前端与 embedded 构建通过。深色/窄窗原生补验因自动化通道中断留待工具恢复。
测试及原生证据见 [testing](testing/README.md#m45-08-b统一工作区与同步活动adr-0037)。
08-C 已由后续切片完成，见上。

**M4.5-08-A 实施记录（2026-09-23）**：[ADR-0036](decisions/0036-compose-workspace-and-durable-send.md)。
原窗口新建/转发、阅读区内回复/全部回复、账户/活动别名 From、To/Cc/Bcc、500ms 自动保存、
本地草稿分页恢复、冲突保留输入、关窗/⌘Q 等待保存。原生文件选择/拖入由 Rust 持文件句柄，
JS 只收 token/进度；分块上传、取消、重试、移除已接 G04。发送以 daemon 保存版本为准，
schema v10 持久记录 editing/failed/sending/unknown/sent；明确失败保留草稿，未知不自动重发，
已发送与 Sent 副本入队分开呈现。未改变 HTML 阅读；正文编辑目前是纯文本，富文本另行切片。
回归包括保存/附件/发送并发、三项授权、主地址 From、真本机 TLS SMTP、重启与旧库迁移。
最终 workspace 721 passed / 0 failed / 8 ignored；前端 45 passed；Clippy、前端与 embedded 构建通过。
验证证据与真机残余见 [testing](testing/README.md#m45-08-a原窗口写信与持久发送adr-0036)。
08-B 已在后续完成（见上）；剩余顺序是 **G05/09 独立修复入口与移除 egui**、10 发布验收。

**M4.5-G04 实施记录（2026-09-23）**：新增分块上传四个方法、主体与草稿绑定、匿名 staging、
版本化 finish（字节/引用/版本同事务）、取消/TTL 清理；schema v9 的新附件随草稿备份恢复。
3 MiB 文件已通过真实 socket/CLI/假 SMTP/MIME 字节往返。复核补齐旧客户端零索引兼容性、
文件名参数编码及损坏 BLOB/元数据复验。后续 08-A 已接入文件选择、进度、取消与发送状态，见下。

**G04 最终自动化基线：`cargo test --workspace --all-targets --locked` 共 712 passed / 0 failed / 8 ignored；
相对 G03 新增 14 项（uploads 集成 ×10、uploads 模块 ×2、草稿授权 ×1、client 兼容性 ×1）。
前端 vitest 25 项通过，TypeScript 检查与生产构建通过。

**M4-06 封板时的自动化基线：内核 35 + maild-paths 7 + maild 270 + maild-client 39 + maild-app 15 + maild-desktop 55 + maild-menu 0 = 421 个通过，另有 8 个 ignored**（Stalwart 真服务器 ×1、macOS Keychain 真机冒烟 ×1、桌面离屏视觉调试 ×6）。
maild 的 270 个通过项包含 lib 141、地址层 e2e 2、schema v1→v2 migration 与 v1 restore 回归；
shared client 的 39 个通过项覆盖地址 DTO/分页/别名发信兼容，maild-app 为 15，
maild-desktop 的 55 个通过项覆盖地址 reducer、在线世代预取、compose 同步与有界列表渲染；
**M4.5-G03 时点的自动化基线：内核 35 + maild-paths 7 + maild 435 + maild-client 51 + maild-app 20
+ maild-desktop 76 + maild-menu 0 + maild-ui 24 + nodeseek-email 50 = 698 个通过，ignored 仍为 8；
另有 webview 侧 `pnpm --dir packages/maild-ui test` 25 个通过（G03 无 webview 改动，数量不变）。**
（M4.5-03-A 时点为 643；G02 增量 +20：`tests/attachments.rs` ×4 + `reading_policy` 模块单测 ×8 + api 单测 ×4
+ maild-client ×3 + maild-client DTO ×1；04 增量 +18：`maild-ui` 的 `html` ×8、`documents` ×6、`system` ×3、
桥投影 ×1；vitest +6：`document.test.ts`；G03 增量 +17：`tests/drafts.rs` ×8 + `drafts` 模块单测 ×6
+ `tests/store_migrations.rs` ×2 + api 单测 ×1。见下文。）
maild 的增量为 `tests/message_model.rs` ×17（M4.5-01 消息模型）、`tests/folders.rs` ×10
（M4.5-02 文件夹身份/游标/重建/清单 + 假服务器多 mailbox 的发现、增量与通知边界）、
`tests/write_path.rs` ×16（M4.5-05 状态机、对账护栏 + 假服务器的 MOVE/COPY 降级链、
不 EXPUNGE 与 SELECT 拒绝分类断言）、`tests/compose.rs` ×13（M4.5-06 报文往返、Bcc 边界、回复/转发成型与 Sent 归档）、
`tests/search.rs` ×19（M4.5-07 中文检索、查询语法中和、结构化过滤、删除不留幻影命中、
线程图与顺序无关、回填可中断可续，含假服务器上的 `UID SEARCH`、连接预算与卸载不变量断言）、
`tests/frontend_permissions.rs` ×6（M4.5-G01 授权矩阵：路由不带写回与管理面、声明后可承担
日用主路径、四词齐全仍铸不出凭证、v1 包写 permissions 整包拒绝且不连累同目录、User 主体零
变化、前端只能 claim 自己的 intent 且 `notifications_read` 须显式声明）、
`tests/drafts.rs` ×8（M4.5-G03：草稿比写它的 daemon 活得久、摘要不带正文、两个编辑者并发时过期
`base_version` 得 `-32006` 且 `data.version` 给出当前值而内容与版本号都没动、超限 `-32007` 带
`field`/`limit` 且被拒后版本号不变、头部字段里的 CR/LF 在写入时就 `-32602`、跨账户引用库内 part
可写入并读回快照、四元组错配与未落库分别 `-32002`/`-32003`/`-32004` 且整次写入回滚不清空既有引用、
源邮件消失后引用读回 `stored = false`、只持 `drafts_read` 读得到写不了而只持 `drafts_write` 反之、
只持 `messages_read` 的主体四个方法全被拒、带错版本号的丢弃被拒而带对的成功且再读 `-32005`、
单账户上限到顶拒绝新建且不淘汰最旧的一份），
`tests/attachments.rs` ×4（M4.5-G02：`messages_read` 前端跨三块取回 1 MiB+3 字节附件且逐字节相等、
每帧 ≤ 1 MiB、同 uid 的已发送那封是另一份字节；错账户/文件夹/uid/part/超限分别 -32002/-32003/-32004、
`folder` 缺省与 `length` 超限 -32602；策略对每个主体同一结论、`message` 路由改不了策略而 `settings`
路由可以、整包拒绝、精确域名、allow_all、**daemon 重启后策略仍在**；损坏策略文件读缺省写 -32001 且
一个字节不动），`folders` ×6、`compose` ×8、`reading_policy` ×8（缺省/往返/0600、原子幂等冲突、
地址与域名语法门、决策优先级与子域不匹配、损坏/未来版本停摆不覆盖、超长文件、符号链接与宽权限、
500 条上限），api 单测 +4（分块边界与最大块整帧 ≤ 1 MiB、三错误码与 pending_removal 不可见、
`getMessage` 的 `stored` 与四种 `remote_content` 理由、patch 整包校验与未装配 -32001）、
`search` ×11 与 frontend/external manifest ×5 模块单测，以及 store migration/restore 的
v2→v3→v4→v5→v6→v7 回归（含分词器/触发器被篡改时拒绝打开）。maild-app 增量：假 Unix socket
服务器上的前端身份 bootstrap ×4（reveal 走短命连接、长连接首帧 authenticate、包不在册落
identity_failure 并重试、主体不匹配拒绝、不声明身份零变化）+ HTTP 端点拒绝 ×1。maild-ui：
`tests/bridge.rs` ×3（真 daemon + 预置 SQLite：主体为 frontend、经桥调 UserOnly 得 -32016、
文件夹/列表/详情投影、四类错误码、包未安装不降级、HTTP 拒绝）+ 投影单测 ×3。webview 侧
vitest ×19（注册表命名空间/原子性/dispose 作用域/必需角色/排序、控制器旧响应丢弃/纪元失效/
离线只读/范围校验、宿主坏模块隔离/停用回邮箱/受限命令上下文、偏好只存两键）。
ignored 的 `real_server` 已于 2026-08-20 对 Stalwart 0.15.5 实际运行并通过；
ignored 的 macOS Keychain 冒烟测试需真机显式运行。workspace clippy `-D warnings`
与 Windows GNU target check/clippy 通过。M3.7 的 hermetic 测试缝
（`MAILD_TEST_CREDENTIAL_STORE=memory`、`MAILD_TEST_ACCEPT_INVALID_CERTS=1`）
只在 debug 构建生效，release fail-closed。

### M4-06 非阻断后续债

- `listAddresses` 已有 1..=1000 的响应分页与 `spawn_blocking` 隔离，但当前仍先在 SQLite
  单连接下物化过滤范围再切页；若规模测试接近 64×1024 的理论上限，应下推 SQL
  `LIMIT/OFFSET` 与状态过滤。
- 桌面为 compose 离线可用会在每个 Online 世代预载全部账户地址；账户中心渲染已有按账户索引、
  默认折叠和每页 100 条边界，但 compose 打开选择器时仍扫描内存缓存。规模压测后再决定按需加载/
  按账户 compose 索引，不改变本切片协议。
- Account Center 的 generation 会拒绝旧操作结果，重连还会强制 reload；outcome 本身尚未携带
  `connection_epoch`。未来若账户任务改为跨 epoch 保活，应像 mailbox/compat 一样显式带 epoch。
- JSON wire 变更保持 additive（旧 client 忽略 `recipient`）；但 `maild-client` 0.1 的公开
  `MailMessage`/`MessageSummary` 新增 Rust 字段会影响仓库外 struct literal。正式发布 SDK 前应
  冻结 builder/构造器或以版本升级声明源码兼容边界。

## 已定死的关键决策（不要重新争论）

1. **内核 = Rust 重实现 cordis 方法论**，不是用 TS cordis 库。规格来源：上游论文 [cordiverse/paper](https://github.com/cordiverse/paper)（形式化定义）+ 上游 [cordiverse/cordis](https://github.com/cordiverse/cordis) `v4.0.0-rc.13`（`bc9fae7`，TS 可执行规格）；仓库内的 `paper.pdf`、`vendor/cordis` 与 `packages/spike` 已随 ADR-0047 移除。
2. **事件 API**：trait-per-mode（`EmitEvent`/`ParallelEvent`/`SerialEvent`/`BailEvent`/`WaterfallEvent`）+ 模式命名方法（`on_emit`/`on_serial`…）。
3. **异步 teardown**：统一 `Disposer` 枚举（Sync|Async）+ `dispose()` LIFO await；同步清理走 RAII；禁止 Drop 里阻塞 I/O。
4. **bail/serial 语义**：`Option<Ret>`，`None`=不拦截、`Some(v)`=拦截（首个 Some 短路）。⚠️ `Some(false)` = 以 false 拦截，与 cordis 的 `false`=不拦截不同（移植 cordis 插件时 `return false` 要译成 `None`）。
5. **锁纪律**：快照后释放——锁内只做 O(1) 的 clone/insert/remove，用户代码与 await 一律锁外（防死锁/重入/中毒，按构造无死锁）。std `Mutex` + `.unwrap()` fail-fast。
6. **R2 realm 模型**：per-key `isolate::<T>()` **严格解析（无回退）**——回退会破坏级联卸载（曾踩坑，见 rust-port-design.md §6）。未隔离的 key 共享 ROOT。事件走 `scoped(realm)`（分发达本 scope + ROOT）。
7. **前端**：JSON-RPC 纯 consumer，语言无关。M3 选 egui；**2026-08-29 起默认前端改为 Tauri**（[ADR-0024](decisions/0024-builtin-frontend-tauri-migration.md)）——HTML 邮件必须真浏览器引擎渲染，而 egui 内嵌 webview 是两套栈叠加。恢复入口由 ADR-0039 改为独立原生修复窗口 `builtin-repair`，egui 壳已于 09-C 删除（旧 `builtin-egui` 选择在读取时映射为 `com.maild.ui`）。硬约束仍是**不用 Electron 级常驻 webview**：常驻的是纯 Rust Agent（release 实测稳态 `phys_footprint` ≈ 5 MiB），UI 按需拉起，webview 不进常驻集。
8. **runtime**：内核无运行时依赖（纯 futures）；**maild 守护进程用 tokio current_thread**（R4 起落地，邮件 I/O 密集、单线程够用）。receive 插件 `apply` 需在 tokio 运行时内（无运行时则 apply 报错、fiber 置 Idle）。
9. **M3.7 账号管理**：主 `maild.toml` 不由 GUI 改写；受管账户存独立版本化文件，秘密只存 macOS Keychain；旧 `[[accounts]]` 只读兼容；移除账户默认保留本地邮件。完整合同见 ADR-0015。

## 代码现状

store 当前为 **schema v10**（`crates/maild/src/store/migrations.rs` 的 `SCHEMA_VERSION`）；JSON-RPC 协议代
（`maild-client::PROTOCOL_VERSION`）自 ADR-0016 起未提升，此后扩展全部 additive。各 crate 的测试数随切片
增长，以最近一次全量回归为准（见上方收官记录），下面不再逐项写死。

```
crates/cordis/        Rust 内核（cordis v4.0.0-rc.13 语义）：Context provide/get/effect/dispose/plugin/
                      isolate/scoped/intercept、5 事件模式、realm、Fiber 生命周期、Disposer
crates/maild-paths/   平台目录与配置来源：config/data/runtime/log、配置歧义检测、runtime 回退
crates/maild-client/  CLI、Tauri 与 launcher 共用的 JSON-RPC client：endpoint 发现、HTTP/Unix socket/
                      named pipe、协议 DTO（含 RuntimeMetrics / SyncMessagesResult / ProviderDiscovery）、
                      流式通知与反向请求、PROTOCOL_VERSION、原生修复与插件包辅助
crates/maild-app/     GUI 框架无关应用层：连接 actor、连接世代、退避重连、快照归并、前端身份 bootstrap
crates/maild-launcher/ Maild.app 顶层原生启动器：无凭据默认配置（新配置默认 com.maild.ui、notify_new_mail、
                      rules persist）、Preview 拉起内嵌 Agent、打开默认前端、原生修复与后台管理窗口
crates/maild-ui/      默认 Tauri 2 前端壳（frontend:com.maild.ui 主体）：bridge（快照泵、版本握手与写闸门）、
                      mailbox/compose/settings/settings_packages 命令、HTML 重写与令牌文档通道（html/documents）、
                      原生文件 token、系统交互；examples/dev_stack.rs 假数据 daemon；
                      tests/bridge.rs、tests/daily_mail_bridge.rs 真 daemon e2e
crates/maild-menu/    独立 macOS 菜单栏 companion：只调用本地 API（打开收件箱/写信/设置、立即同步邮件、后台动作）
crates/maild/         守护进程库 + bin/maild（daemon 与 backup/restore/paths）、bin/maild-agent（平台后台入口）、
                      bin/maildctl（CLI）
  src/loader.rs config.rs     TOML 声明式装配与配置覆盖
  src/events.rs               领域事件与载荷（MailReceived/MailFiltered/OtpDetected/AccountState/通知路由）
  src/receive.rs              每账户一条 IMAP 连接：IDLE/轮询、多文件夹增量、冷启动有界窗口与分批（ADR-0041）、
                              写队列、服务器 SEARCH、按需单封拉取、trigger_sync、keepalive/阶段超时/指数退避
  src/store.rs + store/       SQLite（schema v10）：消息/文件夹/附件/地址/草稿/上传/发送记录/pending_ops/FTS5 与
                              线程图；migrations（含低占用 pragma）、backup、restore、lock、mailbox_queries、
                              draft_send、uploads；storage_metrics/shrink_memory
  src/mime.rs folders.rs pending.rs compose.rs search.rs   消息模型、文件夹 role、写路径状态机、MIME 组装、CJK 检索
  src/drafts.rs uploads.rs    daemon 拥有的草稿与新附件分块 staging（ADR-0034/0035）
  src/filters.rs otp.rs       过滤链；上下文感知验证码识别（ADR-0043）
  src/rules.rs + rules/       通知路由与静默时段；mail_notify（新邮件通知与限流）、persistence（rules_state.toml）
  src/notify.rs + notify/     dev-log / macOS UNUserNotificationCenter 提供者，有界单 worker 与显式授权
  src/transport.rs probe.rs   SMTP none/STARTTLS/implicit 465、AUTH PLAIN/LOGIN、超时；IMAP/SMTP 分阶段探测
  src/autodiscover.rs         服务商知识库、公网域名与解析结果的 SSRF 关卡（ADR-0042）
  src/accounts.rs account_catalog.rs credential.rs provider.rs addresses.rs
                              受管账户目录与动态 manager、Keychain 凭据缝、外部 transport provider、别名/一次性地址
  src/api.rs + api/           JSON-RPC 2.0 over stdio/HTTP/Unix socket/named pipe：方法策略表、推送过滤、
                              syncMessages/fetchServerMessage/discoverEmailProvider/status.runtime 等
  src/frontend.rs + frontend/ FrontendBroker、受检 frontend manifest、默认选择、目录租约与平台激活
  src/recovery.rs + recovery/ 内置前端预装、随包升级、移除不装回与显式修复（ADR-0039/0040）
  src/external.rs containment.rs plugin_identity.rs plugin_state.rs plugin_transactions.rs
                              外部进程插件、进程组/Job Object containment、插件身份、启用状态、安装事务
  src/reading_policy.rs       远程内容策略与可信发件人（reading_policy.toml）
  src/scheduler.rs menu.rs agent_control.rs agent_lifecycle.rs daemon.rs paths.rs
                              定时任务、Menu 监督、Agent 控制与生命周期锁、daemon runner（内存读数/归还/QoS）、生产路径装配
  maild.example.toml          示例配置
  tests/                      hermetic 集成测试（e2e、账户生命周期/恢复、插件、存储迁移/备份/恢复、文件夹、写路径、
                              发信、检索、草稿、上传、附件、版本握手、receive_footprint、smtp_implicit_and_auth、
                              smart_notify_and_rules 等）；real_server.rs 为 ignored Stalwart 交叉验证；
                              common/fake_mail.rs 是 SMTP/IMAP 假服务器（明文/STARTTLS/隐式 TLS、AUTH 机制可配）
  examples/assemble.rs        R3 装配演示
  examples/footprint_bench.rs release 子进程多账户回环 TLS 资源基准（ADR-0041 收官实测）
plugins/nodeseek-email/       首个仓库内第三方 transport provider 插件（ADR-0023）
packages/maild-ui/            Tauri webview：React 19 + TypeScript + Vite，Vitest（app/bridge/compose/host/mailbox/settings）
packaging/macos/              顶层 App（launcher + 单宿主 UI + 应用图标 + Agent + Menu + 内置 UI 恢复副本）hermetic/universal2 构建、
                              Preview DMG、seal-recovery-payload、生命周期管理、签名/notary/验签工具与 tests/；全部名字读 brand.env，
                              模板为品牌中立的 templates/*.in，图标源 assets/AppIcon.icns（ADR-0047）
brand.toml                    品牌唯一事实来源；scripts/sync-brand.sh 生成派生文件（ADR-0047、docs/BRANDING.md）
assets/brand/                 图标原图与 1024 px 渲染
crates/maild-brand/           brand.toml 的 Rust 常量（generated.rs 由 sync-brand.sh 生成）
docs/                         README（索引）、ROADMAP、PROGRESS、BRANDING、protocol/、operations/、testing/、plugins/、
                              decisions/（ADR-0001…0047）、design/、rust-port-design.md、m0-spike.md
```

上游参考（不再 vendor 或本地克隆入文档）：[cordiverse/cordis](https://github.com/cordiverse/cordis)、
[cordiverse/paper](https://github.com/cordiverse/paper)、[deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)。

## 环境与工具链（重要）

- **开发工具统一由 mise 管理**：仓库根 `mise.toml` 固定 Rust 1.97.1（含 clippy、rustfmt 与 universal2 / windows-gnu
  target）、Node 24.21.0、pnpm 11.21.0、Python 3.12、Stalwart 0.15.5。首次进入仓库 `mise trust && mise install`，
  此后在仓库内直接用 cargo / pnpm / python3；不再使用仓库内 `.rustup/` + `.cargo-home/`。
- 仓库不再提交 `.cargo/config.toml`（ADR-0047），默认直连 crates.io；国内网络在用户级 `~/.cargo/config.toml` 自配 rsproxy 等镜像，见[运维手册](operations/README.md#可选国内-cratesio-镜像)。切换源后本地 registry 缓存按源分目录，`--offline` 前需在当前源下 `cargo fetch --locked` 一次。
- 跑测试：`CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`（2026-11-17 收官基线：795 passed / 0 failed / 2 ignored（webview Vitest 191 passed / 12 文件）；ignored 为 Stalwart 真服务器与 macOS Keychain 冒烟，需显式 `--ignored`）。Clippy 门禁：`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `--target x86_64-pc-windows-gnu`（后者另需 mise 之外的 mingw-w64）。
- webview 侧门禁：`pnpm install`（仓库根，单一 lockfile）→ `pnpm --dir packages/maild-ui typecheck && pnpm --dir packages/maild-ui test && pnpm --dir packages/maild-ui build`；内嵌形态 `cargo build -p maild-ui --features embedded`（需先 build 出 `dist`）。默认形态的 `cargo test --workspace` 不需要 Node。
- 体验 Tauri 壳而不碰真实安装：`cargo run -p maild-ui --example dev_stack`（预置假数据的临时 daemon）→ 按它打印的命令起壳；细节见 `crates/maild-ui/README.md`。
- 守护进程：`cargo run -p maild --bin maild -- --config maild.example.toml`；`maild paths` 查看默认目录。
- 资源基准：`cargo build --release -p maild --examples --bins --locked && target/release/examples/footprint_bench`（约 25 秒，任一验收不成立即非零退出；`--json` 只输出报告）。
- 打包门禁：先 `pnpm --dir packages/maild-ui build` 与 release 构建（`cargo build --workspace --release --locked` + `cargo build --release --locked -p maild-ui --features embedded`），再跑 `bash -n packaging/macos/*.sh packaging/macos/tests/*.sh`、`packaging/macos/dry-run.sh` 与 `packaging/macos/tests/test_{bundle,release,lifecycle}.sh`；生成的 `dist/` 在 `.gitignore` 内，验证后删除。
- npm 缓存有 EPERM 问题，TS 侧一律用 pnpm（store 在工作区 `.pnpm-store/`）。
- 网络：GitHub release 下载超时，npm/rsproxy 镜像可用（用户级配置，不入库）。

## 开放问题 / 遗留

1. ~~**R1.5**~~ ✅ 已精化：卸载时先撤回绑定（值校验 revoke_if，withdrawal 提前可见）→ 级联收集依赖者 → 依赖者先执行逆（跨组件 LIFO）→ 本 fiber 逆最后；apply 失败撤回中途 provide 的残留。`tests/r1_5.rs`（4 测试）钉死。
2. ~~**R4.5 真实服务器验证**~~ ✅ 已做：`tests/real_server.rs`（#[ignore]）+ `.tools/stalwart/`（README + start.sh）对独立实现 **Stalwart 0.15.5** 交叉验证通过——transport(SMTP STARTTLS) + receive(IMAP STARTTLS + IDLE) 跑通「收信→验证码→通知」。⚠️ 关键踩坑（已记 `.tools/stalwart/README.md`）：Mailpit 无 IMAP（改自带假服务器）；Stalwart 0.16 配置层重构无 TOML（锁 0.15.5）；SMTP 默认要求认证（`session.auth.require=false`）；spam 过滤把测试邮件判 spam 进 Junk（`spam-filter.enable=false`）。剩余：Dovecot/Gmail 交叉验证（受环境限制：brew 沙箱挡、需 root）。
3. ~~**TLS 未接**~~ ✅ 已接（Step 1 hermetic）：receive 三模式 `tls=none/starttls/implicit`（手动 STARTTLS 协商 + 证书校验开关 `accept_invalid_certs`），假服务器 rustls/rcgen 自签证书，e2e 跑通 STARTTLS/隐式 TLS 通道全链路。剩余：真实 CA / 生产凭据；真服务器交叉验证见第 2 条。
4. ~~重放去重未持久化~~ ✅ R4.5 已修：store 增 `otp_notifications` 表（(account, uid) 主键），otp「先查后通知」，双 otp 实例/重启重放只通知一次（domain.rs 测试钉死）。
5. ~~多账户~~ ✅ R4.5 已做：`[[accounts]]` + `ctx.scoped(r).isolate_at::<Receive>(r).isolate_at::<ReceiveConfig>(r)` 每账户装配；e2e 钉死事件隔离（账户 scope 探针只见本账户邮件）。receive 的会话去重自 ADR-0041 起是每文件夹 256 槽环形窗口（O(folders) 恒定内存；重启重放仍靠 store 幂等落库兜底，otp 不重通知）。
6. **插件 panic 隔离**：属守护进程顶层故障隔离（进程级 catch / abort 策略），与锁无关，落地守护进程时再定。
7. **HMR**：Rust 动态库 + 重载，二期，不在 v1 承诺。
8. ~~**rules 插件（第一批）**~~ ✅：`NotifyRouteEvent` + `QuietHoursQuery`（bail）已消费——敏感总开关、关键词与固定偏移时间窗口静默；缺 scheduler 的配置会在启动时明确报错。剩余：per-account 规则、关键词重要度。
9. **transport 插件 ✅**：SMTP 发信（tls=none/starttls，严格 capability 检查 + 系统证书校验 + TLS 后二次 EHLO，再走 AUTH/MAIL/RCPT/DATA），e2e 覆盖明文、STARTTLS 成功、拒绝降级和无效证书。剩余：resend/mailgun 类 API provider。
10. ~~**settings 持久化边界**~~ ✅ ADR-0043 已修：`setSettings` 仍从不改写 TOML，但 `[plugins.rules.config] persist = true` 或请求 `persist: true` 时原子写入配置同目录的 `rules_state.toml` 覆盖层（0600、损坏 fail-closed 不覆盖），`persisted` 如实报告；不暴露任何凭据。
11. **本地 API 身份边界**：HTTP 使用回环 IP + Bearer，CLI 禁代理/重定向并校验配置权限；Unix socket 校验完整路径祖先、父目录 0700 和 socket 0600；Windows named pipe 使用当前用户 SID 显式 DACL。跨用户 Windows 运行复核仍是外部证据。
12. ~~**SQLite schema 无版本**~~ ✅ M3.5-01 已修：`user_version=1`；v0 在单事务中建表、校验和写版本，历史邮件/OTP 标记保留；当前版本重开校验结构，未来版本在任何 DDL/WAL 变更前拒绝。
13. ~~**SQLite 在线备份缺失**~~ ✅ M3.5-02 已修：独立只读连接走 Online Backup，分步复制运行中 WAL；当前 schema 与 quick_check 通过并同步后才 no-clobber 发布。
14. ~~**单实例/安全恢复缺失**~~ ✅ M3.5-03 已修：文件型 Store 持有数据库旁的持久跨进程锁；restore 在同一锁下校验输入和当前库、持久化 no-clobber rollback、收束 WAL/SHM 后同目录原子替换。活动 daemon、损坏/未来库与任何发布前故障都 fail-closed。
15. ~~**平台目录与后台停止语义未统一**~~ ✅ M3.5-04 已修：`maild-paths` 统一配置/data/runtime；生产相对路径不依赖 cwd，legacy 资产冲突时拒绝切换；SIGTERM 与 Ctrl+C 共用 LIFO teardown。
16. ~~**macOS 缺少稳定 bundle identity 与后台入口**~~ ✅ M3.5-05 已修：顶层 `Maild.app` 内嵌独立 Login Item Agent；桌面端只在用户操作时通过 SMAppService 管理，Agent 只用平台配置并复用 daemon teardown。
17. ~~**macOS 通知仍依赖脚本且授权不可控**~~ ✅ M3.5-06 已修：只有身份匹配的内嵌 Agent 调用 `UNUserNotificationCenter`；查询不弹窗，显式请求单飞；拒绝/不支持/策略缺失均 fail-closed，且不回退脚本。
18. ~~**macOS 缺少安全发布生命周期**~~ ✅ M3.5-07 已修：Agent 生命周期锁证明停服；固定 `~/Applications/Maild.app` no-clobber 安装；升级/回滚有显式 recover；卸载只移动 App；正式 workflow 固定 universal2、内到外签名、notary Accepted、staple/Gatekeeper、复验与 SHA-256。正式身份、凭据和真机证据仍为外部门禁。
19. ~~**账号管理仍要求手改 TOML**~~ headless、桌面与仓库内恢复矩阵均已修（M3.7-01/02 + 03 仓库内）：受管 `accounts.toml` + 平台凭据库 + 动态 AccountManager + additive RPC/CLI + 桌面账户中心 + 故障恢复矩阵已落地。剩余全部为真机外部证据：Keychain 锁定/签名复用、真实 iCloud/Gmail App Password 全链路、新 Preview DMG 全点击验收。

## 建议的下一步顺序

1. ~~**R3**~~ ✅、~~**R4**~~ ✅、~~**R4.5 第一批**~~ ✅、~~**R1.5**~~ ✅、~~filters~~ ✅、~~transport/rules 第一批~~ ✅、~~**TLS Step 1**~~ ✅、~~**Stalwart 交叉验证**~~ ✅、~~**api 插件（JSON-RPC over stdio）**~~ ✅。
2. ~~**api 的 HTTP/Unix socket/Windows named pipe + CLI + settings API**~~ ✅。JSON-RPC v1 已封板。
3. ~~**scheduler 雏形**~~ ✅（统一轮询、账户作用域任务、取消/关闭语义、抖动 seam；rules 静默时段已接入）。
4. ~~transport 的 SMTP STARTTLS~~ ✅。Dovecot/Gmail 交叉验证仍受环境限制。
5. ~~**M3.7-01 headless 账户闭环**~~ ✅ 仓库实现完成：AccountCatalog、凭据缝（macOS Keychain + 内存测试 provider）、动态 manager、probe、SMTP implicit TLS、per-account 收发、API/client/CLI 与生命周期 e2e。
6. ~~**M3.7-02 桌面账户中心**~~ ✅ 仓库实现完成：空状态入口、preset 向导、编辑/停用/移除、compose 账户选择，全部走已封板的 additive RPC。
7. ~~**M3.7-03 恢复与真实邮箱验收（仓库内部分）**~~ ✅：恢复矩阵 hermetic 测试钉死，协议/运维/测试文档已同步。剩余外部项：真实 iCloud/Gmail App Password 全链路、Keychain 锁定与签名复用真机验证、基于新 DMG（`Maild-preview-m3.7.dmg`）的全点击验收（均需用户参与）。
8. ~~**M4-07 NodeSeek Email 第三方 provider（仓库内部分）**~~ ✅：社区 provider、companion CLI、私有插件数据目录、synthetic uid、限流与不确定结果语义已落地。剩余外部项：插件只读 list/quota 验收，以及经用户逐次确认具体目标后的 create/send 验收。

9. ~~**M4.5 全部切片、日常收信增强（ADR-0041/0042/0043）与收官实测**~~ ✅（2026-11-17）：仓库内交付约 99%，剩余见[外部门禁清单](#剩余外部门禁1-边界)。
10. ~~**macOS 单 Dock 图标身份（ADR-0044）**~~ ✅（2026-11-17）：运行时与单宿主打包合入，全量门禁与真实 bundle 由内而外签名 + `codesign --verify --deep --strict=all` 通过；真机 Dock 观察列入外部门禁 2。
11. ~~**关闭退回菜单栏与 `frontend/intent` 唤醒（ADR-0045）**~~ ✅（2026-11-18）：M1 运行时与 M2 `LSUIElement` 打包合入，最小化回到系统原生语义；全量门禁与 release 二进制通过；真机 Dock 观察列入外部门禁 2。
12. ~~**菜单栏与后台生命周期绑定、Dock「退出」零残留（ADR-0046）**~~ ✅（2026-11-18）：Q1 守护进程/菜单栏「退出 Maild」整体停机与 Q2 前端 `agent/stopping` / `applicationShouldTerminate:` 零残留合入，补真 daemon 端到端测试；全量门禁与 release 二进制通过；真机 Dock / 菜单栏观察列入外部门禁 2。

**M2.5、M3、M3.5、M3.6-01 至 04、M3.7 全部仓库内工作以及 M4-01 至 M4-07 已完成：登录只起 headless Agent，通知/Menu 按需拉起前端；受管账户可不改 TOML，经 CLI、本地 IPC 或桌面账户中心动态增删并跨重启复用凭据，故障恢复矩阵有 hermetic 测试钉死，新 ad-hoc Preview DMG 已构建并挂载验证。M3.7 剩余全部为真机外部验收（见下方交接清单），完成后即可封板。M4 Wave 1 已按 ADR-0017 至 0020 完成身份、授权、安装事务和进程 containment。Wave 2 已按 ADR-0021 至 0023 完成 transport provider 缝、地址层与首个 NodeSeek Email 第三方 provider。M4-07 的仓库实现状态不代表真实插件验收通过，当前只记录了直接 OpenAPI 只读侦察；插件自身的 list/quota 和收发闭环仍待分层执行。每次远端 create/send 都需要用户确认具体目标，本地操作不存在远端 delete。Cloudflare/DNS catch-all 方案继续冻结，不算 M4-07 交付。M4-08 transfer 继续保持高风险独立确认流。正式 macOS 公开发布仍需组织 bundle ID、Developer ID/notary 凭据和干净 macOS 13+ 真机证据；Windows 真机项按用户决定暂缓。**

## 剩余外部门禁（1% 边界）

仓库内能自动化证明的部分已全部闭环（全量回归、打包门禁、release 资源实测）。以下各项**必须**由持有正式凭据
的人在真机或真实服务上执行，仓库脚本已对它们 fail-closed，未执行前不得记为通过：

1. **正式发布身份与公证**：冻结 `packaging/macos/{app,agent,menu}-bundle-id.txt` 与 `team-id.txt` 的组织正式值并
   重编译；注入 Developer ID 与 Notary API 凭据实跑 `release-macos.yml`，保存 `Accepted` 记录；在 quarantine/离线的
   干净 macOS 13+ 账户上验证 staple、Gatekeeper 与最终 ZIP 复验（ADR-0013 / ADR-0040）。
2. **真机图形会话**：从 Dock 打开 `Maild.app` 只有一个 `Maild` 图标亮灯、`⌘Q` 后灭灯而 `lsappinfo` 中 `Maild Agent` 仍在、窗口在前台时点击通知或菜单栏「写邮件」经 `RunEvent::Reopen` 立即跳转（ADR-0044）；红灯 `×` / `⌘Q` 后 Dock 无 `Maild` 活动或最近使用图标、`maild-ui` 进程消失而菜单栏 `✉` 仍在，黄灯 / `⌘M` 为原生最小化，最小化或已关闭时点菜单栏 `✉`、通知、Dock 或 Finder 打开都能还原窗口并跳转（ADR-0045）；Dock 右键「退出」与 `⌘Q` 后 `lsappinfo` 不再出现 `exited-with-subordinates`、Dock 无幽灵图标而后台与菜单栏 `✉` 仍在，点击菜单栏「退出 Maild」后窗口、菜单栏与 `Maild Agent` 一并消失，系统注销/重启不被 Maild 中断（ADR-0046）；LaunchServices 正常打开 Tauri 默认前端、损坏已装 UI 自动落入原生修复窗口并恢复；登录项批准
   保留；首次只在用户点击「允许通知」后出现一次归属 Maild Agent 的授权框（拒绝后无脚本回退）；覆盖安装出现版本
   横幅并经修复窗口重启恢复；升级/回滚/卸载/重装且用户数据保留。
3. **真实服务商账户**：iCloud/Gmail App Password 以及 QQ / 网易 163 等 SMTPS 465 + `AUTH LOGIN` 服务商的
   添加 → 收信 → 验证码通知 → 打开 → 发信 → 改密 → 停用/启用 → 移除全链路；`account discover --network`
   对真实自定义域名；macOS Keychain 锁定与 ad-hoc / Developer ID 签名后的 item 复用（M3.7-03）。
4. **长期与大规模运行**：5 万封级真实信箱的冷启动窗口与按需历史；合盖休眠/换网后的半开连接恢复；24 小时以上
   挂机的 `phys_footprint` / WAL 曲线；多日真实日用观察。回环基准（`footprint_bench`）只证明 1,500 封量级与
   本机网络下的行为。
5. **Windows 真机**：named pipe 跨用户 ACL 拒绝、WebView2 渲染与窗口资源释放、前端激活清单（`launch.kind`）扩展。
6. **第三方 provider live 验收**：NodeSeek Email 插件只读 list/quota 与逐次确认的 create/send；Dovecot 或公共
   provider 的通用兼容复测（Stalwart 0.15.5 已于 2026-08-20 通过）。

明确不在这 1% 内的是已登记的后续功能（OAuth2 / XOAUTH2、M4-08 transfer、SRV/MX 自动发现、富文本写信、M5
内核扩展性），它们按 ROADMAP 独立立项。

## M3.7 外部验收交接清单（待专人执行）

按序执行；每项的证据追加到 [`testing/README.md`](testing/README.md) 对应行，凭据与邮箱地址不入库。

1. **Keychain 冒烟（本机即可）**
   ```sh
   mise install   # 工具链由仓库根 mise.toml 固定
   cargo test -p maild --lib credential::tests::macos_real_keychain -- --ignored
   ```
   会写当前用户 login keychain（随机 acct_，测试自清理）。记录通过/失败与 macOS 版本。

2. **安装 Preview DMG 并全点击验收**
   - 产物：`dist/macos/Maild-preview-m3.7.dmg`（SHA-256 见 testing 文档；ad-hoc 签名，非 notarized——首次打开需右键 → 打开绕过 Gatekeeper。2026-08-22 重建版已含 ADR-0016 版本握手与一键重启，第 3 步升级演练用同一安装位覆盖本包即可触发）。
   - 路径：挂载 → 拷入「应用程序」→ 启动 Maild → 「后台」启用 Agent → 「账户」→ 添加邮箱账户。
   - 用自有 iCloud 或 Gmail App Password 走完整链路：preset 向导 → 探测 → 保存并连接 → 收信 → OTP 通知点击打开邮件 → compose 从该账户发信 → 编辑改密 → 停用/启用 → 移除（确认「本地邮件会保留」）。
   - 同时验证：全程不打开配置文件或终端；重启 Mac 后账户自动恢复收信；密码框不回填。

3. **Keychain 锁定与签名复用**
   - 钥匙串访问中锁定 login keychain 后操作账户 → 应进入 `needs_credentials`，解锁重输后恢复；
   - 用新构建覆盖安装（ad-hoc）后再启动 → 已存账户应能继续取到凭据；若失效只允许进入 `needs_credentials`，不得回退明文。
   - ⚠️ 已知坑（2026-08-21 实录）：覆盖安装不会终止旧版 `maild-agent`，它会继续占住受管 socket，让新界面报 -32601。实现 ADR-0016 前的手工恢复：kill 旧 agent 与 menu 进程，从 `/Applications/Maild.app` 重启（勿用 `open -a Maild`，LaunchServices 可能解析到 dist 内旧 bundle），再经「后台」或直接打开内嵌 Maild Agent.app 拉起新内核。

4. **正式发布前置（独立任务）**
   - 改三个 bundle ID manifest（`packaging/macos/*-bundle-id.txt`）→ 重编译三项 → Developer ID 签名/notary/staple 流程见 `packaging/macos/README.md` 与 ADR-0013；
   - 注意：`hdiutil` 在受限沙箱内会被 macOS 拒绝（EPERM），DMG 构建需要完整文件访问环境。

验收全部通过后：更新 testing 文档证据行，把 ROADMAP/PROGRESS 中 M3.7 状态改为 ✅ 封板并补 commit 号。
完整 M3 拆分见 [`docs/ROADMAP.md`](ROADMAP.md)，共享边界见
[`docs/decisions/0001-m3-shared-client-boundary.md`](decisions/0001-m3-shared-client-boundary.md) 与
[`docs/decisions/0002-m3-connection-actor.md`](decisions/0002-m3-connection-actor.md)。协议契约见
[`docs/protocol/README.md`](protocol/README.md)，运行边界见
[`docs/operations/README.md`](operations/README.md)，外部兼容性残余见
[`docs/testing/README.md`](testing/README.md)。

交接时先读：`docs/README.md` → `docs/PROGRESS.md` → `docs/ROADMAP.md` → `README.md` → `ARCHITECTURE.md`，再跑一遍 `cargo test` 确认基线。
