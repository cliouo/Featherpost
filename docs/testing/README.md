# 真实服务与平台验证

这里记录不能只靠内置假服务器证明的兼容性证据。任何凭据、token、邮箱正文和私有服务
地址都不得提交到仓库。

## 当前证据矩阵

| 层级 | 覆盖 | 当前证据 |
|---|---|---|
| Hermetic | SMTP 明文/STARTTLS、IMAP STARTTLS/隐式 TLS、IDLE、重连、OTP/原生通知授权与有界 worker、SQLite schema migration/在线备份/可回滚恢复、平台路径/SIGTERM、后台 Agent/生命周期锁、受管 stale socket 恢复、前端 intent/Menu supervisor、frontend/external manifest 与进程退避、共享 client、连接 actor、桌面邮箱读取/写信/rules/后台状态与首启配置、受管账户目录事务与形状校验、凭据缝（内存 provider + 注入失败）、账户 RPC 来源门禁与错误码、maildctl account 参数解析、版本握手判定与遗留降级、插件身份注册表/凭证轮换/会话绑定、方法 capability 授权表与拒绝计数、插件安装事务与生命周期、supervised 进程 containment、transport provider 反向调用缝、alias/disposable 地址生命周期与 recipient 溯源、消息模型与 schema v3 迁移/HTML 抽取/附件边界、文件夹发现与增量同步/UIDVALIDITY 重建、写路径状态机与对账护栏、MIME 组装与回复/转发成型、前端授权声明与受托管理词表、前端身份 bootstrap 与 Tauri 桥投影、附件字节分块通道与阅读策略文件纪律、邮件 HTML 重写器与令牌文档通道、daemon 草稿的版本化并发与上限拒绝 | `cargo test --workspace --all-targets --locked` + `pnpm --dir packages/maild-ui test`（数量随切片增长，以本地实跑为准；M4-06 封板基线 421 passed，M4.5-10 时点 717 passed，2026-11-17 收官基线 795 passed / 0 failed / 2 ignored（webview Vitest 191 passed / 12 文件），见 PROGRESS） |
| 低占用后台收信（ADR-0041） | 冷启动有界窗口与 50 封分批、按需单封拉取、`trigger_sync` 打断 IDLE/退避、store 指标与内存归还；release 子进程多账户回环 TLS 实测（冷启动、稳态 RSS/`phys_footprint`、空闲 CPU、IDLE 推送 P50/P95、通知限流、WAL 有界、服务器检索 + 按需拉回） | `tests/receive_footprint.rs` ×7 + `examples/footprint_bench.rs`（2026-11-17，数值见[下文](#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)） |
| SMTPS 465 / AUTH LOGIN / 自动发现（ADR-0042） | 隐式 TLS 先读 greeting、AUTH PLAIN/LOGIN 协商、端口配反诊断、provider 知识库与 SSRF 边界 | `tests/smtp_implicit_and_auth.rs` ×10 + `autodiscover` 单测 ×4 + `footprint_bench` 守护进程 465 发信回环 |
| 智能通知与规则持久化（ADR-0043） | 上下文验证码识别、普适新邮件通知与限流、`rules_state.toml` 原子侧车 | `tests/smart_notify_and_rules.rs` ×18 + `maild-ui/tests/daily_mail_bridge.rs` + `footprint_bench` 通知计数 |
| macOS 单 Dock 图标身份（ADR-0044） | `Maild.app` 原位承载同包 `maild-ui`、`NSWorkspace` 独立 Coalition 启动 Agent、`.main-ui.lock` 单实例、`RunEvent::Reopen` 意图排空、顶层 Bundle 激活路由；单宿主 `Contents/MacOS/maild-ui` + `Maild.icns` 布局与显式 runtime 签名负向门禁 | `maild-client`/`maild-launcher`/`maild`/`maild-ui` 单测（清单见 ADR-0044 验收）+ `test_bundle.sh`/`test_release.sh`/`test_lifecycle.sh` + 真实 release bundle 由内而外签名复验（[2026-11-17](#2026-11-17单-dock-身份adr-0044合入回归)）；真机 Dock 观察为外部门禁 |
| 关闭退回菜单栏与 `frontend/intent` 唤醒（ADR-0045） | 意图入队锁外唤醒、`frontend/intent` 只投给恰好的前端主体（user/插件/前缀相同的前端收不到）、客户端解码、`intent_epoch` 单调且不进同步缓冲、状态泵每个新 epoch 只唤醒一次；顶层 `Maild.app` `LSUIElement = true` 打包断言；黄灯 / `⌘M` 保持原生最小化（无劫持代码） | `maild`/`maild-client`/`maild-app`/`maild-ui` 单测与 `maild-ui/tests/bridge.rs` 真 daemon 推送用例 + `test_bundle.sh`/`verify-bundle.sh`（[2026-11-18](#2026-11-18关闭退回菜单栏adr-0045合入回归)）；真机 Dock 观察为外部门禁 |
| 菜单栏与后台生命周期绑定、Dock「退出」零残留（ADR-0046） | `stopManagedAgent` 只接受 user IPC、被接受时先向已认证前端长连接推 `agent/stopping`（user/插件/未认证收不到、被拒请求不推送）、排空窗口后正常退出；menu supervisor 在 Agent 运行期间无条件重启菜单；客户端解码 `AgentStopping`、`shutdown_requested` 只发布一次且不进同步缓冲、状态泵只触发一次退出流水线；跨 crate 端到端：真 daemon `stopManagedAgent → agent/stopping → maild-app shutdown_requested` 先于 daemon 退出到达，随后 socket 被清理。点击菜单栏「退出 Maild」一并关闭窗口、菜单栏与后台守护；点击窗口红叉、`⌘Q` 或 Dock 右键「退出」均仅关闭窗口并彻底清空 Dock 图标 | `maild`/`maild-client`/`maild-app`/`maild-ui` 单测 + `maild-ui/tests/bridge.rs::stop_managed_agent_reaches_the_frontend_before_the_daemon_exits`（[2026-11-18](#2026-11-18菜单栏生命周期绑定adr-0046合入回归)）；真机 Dock / 菜单栏 / 注销观察为外部门禁 |
| 受管账户闭环（M3.7-01） | 动态添加 → 不重启即 online → 收信落库（account=不可变 id）→ sendFromAccount → 重启后自动恢复收信 → disable/enable → remove 软归档（凭据删除、邮件保留）→ revision 冲突与归档后不可操作；假服务器为 IMAP 隐式 TLS + SMTP STARTTLS/AUTH PLAIN，凭据走按目录共享的内存 store | `tests/account_lifecycle.rs`（hermetic，debug 测试缝见 operations 文档） |
| 恢复矩阵（M3.7-03 仓库内） | 目录损坏 / 未来 format_version fail-closed 且不清理凭据；凭据丢失 → needs_credentials → 重输密码恢复在线；目录发布失败回滚 staged 凭据且可重试成功；启动孤儿 revision 收敛只删未引用项；服务端密码撤销 → 认证失败 detail、配置不回滚、重输后恢复 | `tests/account_recovery.rs`（hermetic：假服务器支持 LOGIN 校验与服务端改密） |
| 版本握手与重启恢复（M3.7-04） | `getAgentInfo` 身份字段与协议代数；判定表相等/过旧/过新与 -32601 遗留降级（client 假服务器 + 真实 daemon）；优雅停止删除 socket → 新内核重握手兼容；硬停残留 socket 由受管回收清理后恢复兼容；桌面横幅状态机（epoch 单次握手、单飞行重启、失败重试、Compatible 复位）有 reducer 测试 | `tests/version_handshake.rs` + maild-client/maild-desktop 单测；真机「覆盖安装 → 横幅 → 一键重启 → 数据无损」升级演练为外部证据，归 M3.7-03 验收同批 |
| 插件身份与认证缝（M4-01） | 注册表纪律（缺失=空表、未来版本/坏哈希/重复条目 fail-closed 不清理、重开持久化）、reveal 即轮换且吊销旧凭证、主体字符串稳定；e2e：未认证恒 user、authenticate 绑定 plugin 主体且连接内不可变、重复认证 -32014 断连、无效凭证断连、HTTP 来源 authenticate/-32015 且 reveal 走 -32600、包移除重启后孤儿收敛+旧凭证拒绝、注册表损坏软停摆（supervised 拒 spawn 状态 blocked、auth -32015、status 存活）、每 spawn 注入新代凭证且旧代失效 | `tests/plugin_identity.rs` ×5 + plugin_identity 模块单测 ×8（2026-08-22，`3933b37`）；Windows named pipe 认证与吊销路径由 `tests/client.rs::named_pipe_supports_plugin_authentication_and_revocation` 钉死（Windows CI 执行） |
| 方法 capability 授权表（M4-02） | 策略表逐方法钉死（Open/Capability/UserOnly + fail-closed 默认）、route→API capability 固定翻译（builtin 全 route 与未知 id 空表）、拒绝计数按主体隔离且忽略 user；e2e：`messages_read` 插件 getMessage 与 status 走到领域语义而 send/清单/发放/账户/信箱写回全部 -32016、连接不断开、denied_calls 可见、user 主体同调用走到依赖层；compose 前端 send 可达（-32001）而 getMessage -32016、frontend 命名空间计数可见 | `tests/plugin_capabilities.rs` ×2 + api/frontend/plugin_identity 单测（2026-08-22，`888a2e5`；2026-09-11 随 ADR-0030 更新归类） |
| Tauri 前端骨架与注册底座（M4.5-03-A + E01） | `maild-app`：假 Unix socket 服务器——reveal 走短命 user 连接、长连接首帧必须是 authenticate、whoami 不匹配拒绝、包不在册落 `identity_failure` 且持续退避重试并在装好后自动绑定、不声明身份的消费者零变化、HTTP 端点 + 身份在 spawn 即拒；`maild-ui`（Rust）：真 daemon + 预置 SQLite 的 hermetic e2e——主体为 `frontend:com.maild.ui`、经桥调 `revealPluginToken` 得 -32016（证明桥不是 user）、文件夹/列表/详情投影正确且 JSON 无 `body_html`、四类错误码、daemon 退出后 offline 文案不含 socket 路径、包未安装报 `identity_failure` 不降级；投影单测钉住 `last_error` 不出桥；`packages/maild-ui`（vitest）：注册表命名空间/重复/原子提交/dispose 作用域/必需角色/排序、控制器旧响应丢弃/纪元失效/离线只读/跨账户范围拒绝、宿主坏模块隔离/停用回邮箱/受限命令上下文/命令抛错不崩壳、偏好只存两键。手工：内嵌形态起壳连 `dev_stack`，Orca computer-use 在真 WKWebView 上走完读信、设置、扩展启停 | `maild-app` 单测 ×5 + `crates/maild-ui/tests/bridge.rs` ×3 + `maild-ui` 单测 ×3 + `pnpm --dir packages/maild-ui test` ×19（2026-09-11，ADR-0031） |
| HTML 原位阅读（M4.5-04） | `html` 重写器对敌意 fixture：`<script>`（含 svg 内）/`<iframe>`/`<object>`/`<embed>`/`<link>`/`<base>`/`<meta refresh>`/`<form>`/`on*`/`ping`/`target`/`download` 全部消失而表格、`<font>`、`bgcolor`、`<style>` 内容、CJK 文本逐字节保留；`cid:` 映射到带令牌的 part URL（大小写与尖括号容忍、svg `image`、CSS `url(cid:)`），缺失 cid 去 `src` 保 `alt`；远程引用计数一致，阻止时 `src` 挪到 `data-maild-src`、`srcset` 删、CSS 远程 `url()` 中和成 `data:,`，放行时保留且 CSP 加 `https: http:` 但样式表/字体/媒体仍无外部源；链接表序号与原 href 对应、实体 `&amp;` 解码、`javascript&colon;` 归 Other、片段链接原样、伪造的协议路径重新编号；无 `<head>` 时补 head；8 KiB 拆块的 `<style>` 攒齐再重写；截断标记与内核常量逐字相等。`documents`：令牌随机十六进制、按令牌取/释放/LRU 上限、路由只认合法令牌、导航裁决（应用 URL 与 `/doc/` 放行、`/link/<token>/<n>` 翻译成事件、`/part/` 与 `http(s)`/`file:`/`javascript:`/未注册 scheme 拒绝）、响应头（CSP、no-referrer、nosniff、no-store、Content-Type 注入拒绝）。`system`：只有 http/https 交浏览器、文件名清洗、原子写不留临时文件且目标目录缺失不创建。桥 e2e（真 daemon）：准备文档→脚本消失、cid 变令牌 URL、远程只以 `data-maild-src` 存在、链接变惰性路径→按路由取 part 字节等于预置→未落库 part/非本封 part/别的令牌 404→放行版 CSP 含 `https:`→释放后 404→无 HTML 的信无令牌→`read_attachment` 与策略读写经桥可达且 `getMessage.remote_content` 随信任翻转。vitest：文档控制器换信释放令牌与迟到应答归还、无 HTML 不发请求、准备失败回退文本、「加载本封」只记会话且不写内核、链接事件只认当前令牌且只开 web 链接。**真 WKWebView**（Orca computer-use 驾驶内嵌形态 + dev_stack 计数服务器）：渲染、计数 0 → 加载本封 2 → 始终允许 4、链接三种面板、设置分区移除条目回到拦截、保存面板 | `maild-ui` 单测 ×21（html 8 / documents 6 / system 3 / bridge 4）+ `tests/bridge.rs` 增量断言 + vitest `document.test.ts` ×6（2026-09-12，ADR-0033） |
| 附件字节通道与远程内容策略（M4.5-G02） | 真 daemon + 预置 SQLite e2e：`messages_read` 前端跨三块取回 1 MiB+3 字节附件且逐字节相等、每帧 ≤ 1 MiB、往返数 = ceil(size/chunk)、同 uid 的已发送那封读到自己的字节、`cid:` 目标按 part 定位；错账户/文件夹/uid → -32002、错 part → -32003、超限未存 → -32004、`folder` 缺省/offset 越界/length 超限 → -32602；策略对每个主体同一结论、只声明 `message` 路由的前端读改策略都 -32016、`settings` 路由可改且另一主体的 `getMessage.remote_content` 同步翻转、精确域名、整包拒绝、allow_all、daemon 重启后策略仍在；损坏策略文件读缺省写 -32001 且文件一个字节不动、附件照读。模块单测：解析 `stored` = `content.is_some()`、回读 `stored` = `content IS NOT NULL`、四元组读取四种结果；策略文件缺省/往返/0600、原子幂等冲突、地址与域名语法门、决策优先级与子域不匹配、损坏/未来版本停摆不覆盖、超长/符号链接/宽权限拒绝、500 条上限；api：分块边界、最大块整帧 ≤ 1 MiB、零长附件、`pending_removal` 不可见、`stored` 位与四种理由、patch 整包校验、未装配 -32001、策略表钉三方法；client：按已收字节推进 offset 的拼装、对 `size` 中途变化/累计超量/eof 早到/offset 错位/length 不符五种说谎服务端报错、新旧 daemon DTO 双向容忍。手工：`maildctl attachment save` / `reading-policy` 对 dev_stack 实跑（字节逐一对上、重复保存被拒、策略文件 0600） | `tests/attachments.rs` ×4 + `reading_policy` 单测 ×8 + api 单测 ×4 + `tests/message_model.rs` 增量断言 + maild-client ×3（2026-09-12，ADR-0032） |
| 前端授权闭环（M4.5-G01） | manifest：v2 解析 `permissions` 并与路由翻译取并集、v1 写 `permissions` 整包拒绝、未知/重复词拒绝且不连累同目录好包、builtin 授权表为空；external 词表接受 `mailbox_write` 但拒绝三个管理词；策略表补齐 `providerRegister` 使其真正覆盖每个 dispatch 方法；e2e：只声明路由的前端对 5 个写回 + 5 个管理方法全部 -32016 且 `denied_calls` 逐次递增、声明后同一批方法进入领域层、`mailbox_write` 单独声明不顺带管理面也不带 `messages_send`、四词齐全仍拒 `revealPluginToken`、`permissions` 在 `listFrontends` 可审计、User 主体零变化。正向断言同时排除 -32601，避免拼错的方法名静默「通过」 | `tests/frontend_permissions.rs` ×5 + frontend/external/api 单测 ×4（2026-09-11，ADR-0030） |
| 插件安装事务与生命周期（M4-03） | 事务生命周期单测（install→同版本拒绝→低版本拒绝且 staging 清理→高版本升级留回滚位→互换回滚来回切换→移除）与 staging 名规则（路径/短名/点前缀拒绝）；e2e：崩溃残留 `.incoming`/`.removed` 不阻塞扫描、不停服 install 后 reveal 即可用（has_identity=true）、enable 即 spawn 且子进程拿到注入凭证、低/同版本升级 -32017 且坏 staging 清理、高版本升级 from_version 正确且 supervised 子进程以新代凭证重启（旧代 -32014、新代 whoami 绑定）、rollback 清单版本即时切回、remove 后清单消失且末代凭证吊销；independent enable/disable 只记意图、frontend kind 事务走前端根目录 | `tests/plugin_transactions.rs` ×2 + plugin_transactions/plugin_state 单测（2026-08-23，`e38aa27`） |
| supervised 进程 containment（M4-04） | 组空探测语义（ESRCH=干净；存活组 SIGKILL 后清空）单测；e2e：父进程正常 exit 0 后遗留的组内后台心跳被清扫冻结且终态 stopped（孤儿覆盖）；trap 忽略 SIGTERM 的插件在 disable 时于预算内被 SIGKILL 升级、父子与后代一并终止且终态 stopped；setsid 主动逃逸不可见为文档化残余风险，不在 hermetic 断言范围 | `tests/plugin_containment.rs` ×2 + containment 单测 ×2（2026-08-23，`4b7b8c3`）；Windows Job Object 行为归真机/CI job 证据 |
| transport provider 反向缝（M4-05） | 注册表 latest-wins/属主注销/未决失败/迟到应答丢弃单测；client 解码区分反向请求与通知、外出调用暂存反向帧、按 id 应答原语；委托态 profile 视图容忍缺省字段解码；e2e：User 主体注册拒绝、无 `transport_provider` 词表 -32016 且 denied 计入；双向委托账户 probe→create→send 全链路落到 provider（from 由内核填充）；委托收信经既有落库链路且重复 uid 幂等、新 uid 追加（游标推进）；provider 断线后 sendFromAccount 快速失败 -32018 绝不回退、`provider_registered` 观测位随注册/断线翻转；慢 provider 超预算 -32019、插件业务 error 映射 -32020 且 detail 有界脱敏 | `tests/plugin_transport.rs` ×2 + `tests/plugin_transport_timeout.rs` ×1（独立进程：`MAILD_TEST_PROVIDER_TIMEOUT_MS=300` 预算缝）+ provider/registry/client 单测（2026-08-23，见 PROGRESS 切片行） |
| alias/disposable 地址层（M4-06） | schema v1→v2 精确迁移及 v1 备份先迁移/校验候选再发布；地址语法、唯一约束、每账户 1024 条事务上限、随机 local part、TTL 惰性过期、未知 store 状态 fail-closed；四个 RPC（ADR-0030 起归 `addresses_manage`）的循环/分页/错误码；同账户 active 别名发信只改 From，跨账户/disabled/expired 拒绝；IMAP To 与 provider 可选 recipient 贯穿摘要/详情/推送；账户归档级联及发布/DELETE 间中断后的幂等重试；client 旧 wrapper 兼容；桌面地址管理、在线世代预取、compose 选择与有界列表 reducer | `tests/addresses.rs` ×2 + `tests/store_migrations.rs` / `store_restore.rs` + maild/maild-client/maild-desktop 单测（2026-08-23，ADR-0022） |
| 消息模型与 store schema v3（M4.5-01） | v0/v1/v2 三条迁移路径与数据保留、`sort_at` 回填等于 `received_at`、v2 携带 v3 保留对象时 fail-closed 且列与索引一起回滚；v1/v2 备份恢复后在候选库迁移到 v3；`Date` 解析与缺失/垃圾回退、伪造未来日期保留但不参与排序、store 落库重算排序键；`<style>`/`<script>`/`<head>` 内容不进正文与摘要、属性值内 `>` 不截断标签、实体解码与块级换行、闭标签缺失不吞正文；HTML 原文上限与截断标记；附件按 `content_id`/part 取回、超限只留元数据、内联图片不点亮回形针；`listMessages` 与 `message/received` 不携带 HTML 与附件而 `getMessage` 携带；新旧 DTO 双向 serde 兼容 | `tests/message_model.rs` ×17 + `tests/store_migrations.rs` / `store_restore.rs` 增量 + api/mime 单测（2026-08-29，ADR-0025） |
| 文件夹与增量同步（M4.5-02） | store 层：跨文件夹同 UID 是两封且附件不互相顶掉、`folder` 过滤与省略时的旧行为、清单的 role/计数/`present` 标记、`UIDVALIDITY` 重建删消息与附件并清游标、游标不回退、`LIST` 之后 role 由服务器声明纠正且不新建行、省略 `folder` 时 `getMessage` 的固定消歧规则；`folders` 模块：modified UTF-7 解码（中文名、`&-` 字面量、畸形回退原名）、SPECIAL-USE 优先于名字、`INBOX` 恒为收件箱、层级末段匹配、歧义与自建名不猜；假服务器多 mailbox e2e：LIST 发现与 role 落库、非收件箱落库但**不**触发通知/OTP、FLAGS 首次快照、第二次同步只请求 `{last+1}:*` 且不因 `N:*` 回带最大 UID 而重复落库；协议：`listFolders` 结构/计数/`present` 与 `folder` 过滤、参数校验、方法策略表归 `messages_read` | `tests/folders.rs` ×10 + `folders` 模块单测 ×6 + api 单测（2026-08-29，ADR-0026） |
| 写路径与离线队列（M4.5-05） | store 层：乐观写与入队的原子性、已是目标态与不存在的 uid 不入队、移动立刻从源文件夹隐藏；状态机：flag 的 `unknown` 自动重试且有上限、move 的 `unknown` 是终态、失败还原点击前状态、成功的移动删源行而失败的放回来、崩溃后 in_flight 的收敛、retry/discard 裁决与「未落定不接受裁决」；对账：以服务器为准但保护最近本地改动与有未落定 op 的消息、时间窗口单独生效、存在性对账只在全量轮次、CONDSTORE modseq 往返；假服务器 e2e：标记写回抵达服务器、宣告 MOVE 时走原子 MOVE、无 MOVE 时降级 COPY + `\Deleted`、**无 UIDPLUS 时绝不 EXPUNGE**、目标文件夹不存在时是 failed 而非「结果未知」；协议：五个方法归 `mailbox_write`（ADR-0030 之前是 `UserOnly`）、删除的废纸篓策略、参数校验与 `-32002` | `tests/write_path.rs` ×16 + api 单测（2026-08-29，ADR-0027） |
| 本地检索与线程（M4.5-07） | 中文两字词命中长串（`发票` → `发票已开具`）且**短语要求相邻**（`发货……票据` 不命中）、拉丁词与完整地址、大小写不敏感、多词 AND；`"` / `*` / `-` / `(` / `NEAR` / `subject:` / `OR` 七种 FTS5 语法输入既不报错也不跨列检索；发件人子串、时间窗口、有无附件、文件夹与账户过滤，无查询词时退化为纯过滤；结果排序与 `listMessageSummaries` 同键；`pending_removal` 的行不可见；`rebuild_folder` 删消息后索引与线程边**由触发器一并消失**（不留幻影命中）；线程：回复并入、乱序到达收敛、双线程合并取小 id、同主题不并、跨文件夹同线程、不跨账户；回填按批推进且 pending 单调下降、中断可续、完成后空转，回填与实时落库并发收敛到同一线程；schema：分词器被换/触发器被改或删除时拒绝打开；假服务器 e2e：`UID SEARCH` 带 `CHARSET UTF-8`、结果标注来源信箱与标记位、空结果不是错误、**全程只有一条 IMAP 连接**；**卸载不变量：StorePlugin 卸载返回的那一刻数据库必须可以立刻再打开**（该用例在「只发停止信号 + abort」的旧写法下必然失败，正在 `spawn_blocking` 里的那一批持有真 `Arc`）；协议：三方法参数校验与错误码、策略表钉死在 `messages_read` | `tests/search.rs` ×19 + `search` 模块单测 ×11 + api 单测 ×2（2026-08-30，ADR-0029） |
| 发信补全与回复/转发（M4.5-06） | 组装出的报文一律用 `mailparse` **解回来**断言：纯文本 7bit 可读、中文主题与正文往返一致且报文纯 ASCII、长主题折行不超 998 且不切碎字符；**Bcc 不出现在报文任何位置但在信封收件人里**、信封保序去重；text+html alternative 与带附件 mixed 的 part 树与附件字节逐字节还原；会话头写入；`Message-ID`/`References` 输入的头注入清洗；RFC 5322 日期覆盖闰年与世纪边界、`Message-ID` 唯一且带发件域；主题前缀不累积、`References` 超限保留会话根、全部回复排除自身与别名、转发只带非内联附件且字节已在库内；`append` op 只在有 Sent 文件夹时入队且归档的是同一份字节、其「结果未知」是终态；假服务器 e2e：`APPEND` 抵达已发送；协议：`prepareReply` 三种模式与错误码、收件人逐项校验与未知键拒绝 | `tests/compose.rs` ×13 + `compose` 模块单测 ×8 + api 单测（2026-08-29，ADR-0028） |
| NodeSeek Email provider（M4-07） | 社区 provider 与 companion CLI；固定生产 origin、禁环境代理与 redirect、macOS Keychain 秘密边界、按插件隔离的私有数据目录、synthetic uid、429 gate、只读有界重试、create/send 不重放与不确定结果；Cloudflare/DNS catch-all 已冻结 | 仓库实现与 hermetic 覆盖已完成；2026-08-23 直接 OpenAPI 只读侦察已记录，插件 live 只读与 mutation 验收仍待执行。具体边界和脱敏记录见 [`nodeseek-email.md`](nodeseek-email.md) |
| macOS Keychain 真机 | generic password 写入/读回/更新/删除/枚举收敛、label 清洗；锁定/授权弹窗、ad-hoc 升级与 Developer ID 签名后的 item 复用 | 2026-08-21 本机 login keychain 冒烟通过（`cargo test -p maild --lib credential::tests::macos_real_keychain -- --ignored`，1 passed：写入→读回→更新→枚举→删除收敛）；锁定/授权弹窗、ad-hoc 升级与 Developer ID 签名后的 item 复用仍为真机外部证据 |
| Preview DMG（M3.7-03） | universal2 构建、bundle 结构与逐 slice identity、ad-hoc 签名 deep-strict 验证、UDZO 挂载校验 | 2026-08-22 `dist/macos/Maild-preview-m3.7.dmg`（SHA-256 `063fde3a9df150b60b5e0f85aaba8cb7a491ca7bf7654cf3a367ebde4a6f9f98`，v0.1.0，x86_64+arm64，codesign --deep --strict 通过；已含 M3.7-04 版本握手，agent 内探针命中 `getAgentInfo`/账户方法并嵌入 build_rev `0141f480845c`）；hdiutil 需要完整文件访问，沙箱内构建须显式提权 |
| 真实邮箱全链路（M3.7-03） | iCloud 或 Gmail App Password：add→receive→OTP notification→open message→send→change password→disable/enable→remove；凭据不进仓库/日志/测试产物 | 待真机执行（需用户自有 App 专用密码）：安装上述 DMG 后全点击完成；Keychain 锁定/签名复用同批验证。2026-08-21 验收实录：覆盖安装后中午旧安装的 `maild-agent` 存活并占住受管 socket，新桌面壳报 -32601 `listAccountProfiles`（包内容无错，vnode 取证确认）；kill 旧 agent/menu 并从 `/Applications` 重启后 RPC 恢复。产品化方案见 ADR-0016 |
| 独立实现 | Stalwart 0.15.5：SMTP STARTTLS → IMAP STARTTLS/IDLE → store/OTP/notify | 2026-08-20 实跑 ignored 测试通过 |
| 真实邮箱 | 用户侧已确认基础收发可用 | provider 与日志未入库，不作为自动化证据 |
| Unix IPC | 权限、占用、清理、推送、多 session、阻塞关闭 | macOS hermetic 测试 |
| Windows IPC | named pipe 编译、显式非空 DACL、请求/响应测试 | CI workflow 已加入，首次远程运行待 push；跨用户拒绝仍需 Windows 主机人工复核 |
| 桌面壳 | macOS 实际开窗、系统 CJK、HTTP 快照、写信、rules、长正文和断线恢复；连接世代、键盘、协议上限正文和三档渲染有自动化边界；M3.7-02 账户中心（preset endpoint 冻结、向导校验、generation 防旧结果、秘密清零、离开确认、compose 发件账户选择规则）有无窗口渲染与 reducer 测试 | macOS release 实窗与资源基线通过；Windows GNU workspace 编译通过；Windows 真机留发布前外部矩阵；Preview DMG 全点击账户验收归 M3.7-03 |
| macOS bundle/Agent | 顶层 App、内嵌 Login Item、plist/架构/版本/逐 slice identity、生命周期锁、no-clobber 安装、可恢复升级/回滚、保留数据卸载 | hermetic 事务测试与实际 universal2 构建/复验通过；正式 ID、Developer ID/notary/staple/Gatekeeper 正向链及授权/登录启动留外部门禁 |

## M3-07 桌面验收记录

2026-08-20 在 macOS 26.3 arm64 使用 `cargo build -p maild-desktop --release --locked`
构建，并以回环 HTTP 假服务驱动真实窗口。验收数据只使用 `example.test` 地址和合成中文正文。

| 项目 | 结果 |
|---|---|
| CJK 与截图 | 1180×792 三栏窗口使用 Hiragino Sans GB；截图目视确认中文账户、摘要和正文无缺字，测试数据不含真实邮箱或 token |
| 长正文 | 约 300 KiB 中文/ASCII 混合正文按可视行虚拟化；AccessKit 树只包含当前可视正文行；自动化另覆盖 512 KiB 协议上限 |
| 断线恢复 | 关闭假服务后立即进入“等待重试”并保留已有列表；服务恢复后回到“在线”，选中账户范围重新拉取，旧刷新错误被清除 |
| 键盘 | ArrowUp/ArrowDown、Enter、Escape、F5 在无窗口 egui 回归中覆盖；当前自动化无法把无 bundle-id 测试窗口设为键盘焦点，因此不把真实键盘注入记为通过。⚠️ 本行是 M3 egui 壳的封板证据；默认前端迁移到 Tauri 后（ADR-0024）需重新建立键盘与无障碍证据 |
| release 资源 | 二进制 11,911,488 bytes（11.36 MiB）；加载长正文后的稳态 RSS 约 100–103 MiB，采样中一次瞬时 155 MiB 后回落；6 秒 `top` 采样 5 次 0.0% CPU、1 次 HTTP 轮询 0.7%，进程 CPU time 增加 0.01 秒；5–7 threads |

这些数值是当前开发机的基线，不是跨机器性能承诺。M3 只要求无固定帧率空转和无长正文
线性布局放大；正式安装包、签名构建与 Windows release 数值属于 M3.5。

Stalwart 的固定环境和踩坑见 [`.tools/stalwart/README.md`](../../.tools/stalwart/README.md)。
本轮实际命令：

```sh
cargo test -p maild --test real_server -- --ignored --nocapture
```

结果为 1 passed。测试会生成本次动态验证码，因此邮箱里存在旧测试邮件时也只等待本次通知。

## 真实 provider 复测

`tests/real_server.rs` 同时是通用真实邮箱入口。通过安全的环境注入方式提供下列变量，
不要把秘密直接写入 shell 历史或仓库文件：

| 变量 | 说明 |
|---|---|
| `REAL_IMAP_HOST/PORT/USER/PASS` | 收信端点和认证 |
| `REAL_IMAP_TLS` | `starttls` 或 `implicit` |
| `REAL_IMAP_ACCEPT_INVALID_CERTS` | 真实服务必须为 `false` |
| `REAL_SMTP_HOST/PORT/USER/PASS` | 发信端点和认证 |
| `REAL_SMTP_TLS` | 当前生产验证使用 `starttls` |
| `REAL_SMTP_ACCEPT_INVALID_CERTS` | 真实服务必须为 `false` |
| `REAL_SMTP_FROM/TO` | provider 允许的发件地址和目标收件箱 |

然后运行同一条 ignored 测试。验收需要同时看到：账户进入 `online`、SMTP 发信成功、IMAP
收到本次邮件、OTP 通知包含本次验证码，并且测试输出不含凭据。真实 CA 验证必须保持开启；
自签 Stalwart 才允许两个 `ACCEPT_INVALID_CERTS=true`。

每次 provider 验证只记录以下非敏感信息：日期、OS、provider 名、IMAP/SMTP TLS 模式、
是否通过、限流或文件夹/UID 差异、重连表现。失败日志在脱敏后再入库。

## 外部残余

- Windows 上用不同本地用户连接 named pipe，确认 ACL 拒绝；同一用户不同 logon session
  当前明确视为同一信任域。
- 再选一个 Dovecot 或公共邮箱 provider 重跑通用测试，记录真实 CA、限流、文件夹和 UID
  差异。这是兼容性证据扩充，不再阻塞 M3 JSON-RPC consumer 开发。
- 1,500 封量级的多账户回环基准已由 `footprint_bench` 覆盖（见文末）；5 万封级真实大邮箱、超大正文、
  长期断线/休眠唤醒和 24 小时 soak 继续作为持续质量项，不属于当前功能封板。
- Windows 真机启动默认前端 `maild-ui`（WebView2），复核中文字体、窗口布局、键盘焦点与空闲资源；
  这是正式发布前外部门禁（09-C 已删除旧 egui 壳 `maild-desktop`，M3-07 记录保留为历史证据）。
- 在干净的 macOS 13+ 用户账户安装签名 bundle，确认首次启动不自动弹通知授权；只有用户点击“允许通知”
  后出现一次归属于 Maild Agent 的系统权限框。分别验证允许后的 OTP 通知、拒绝后无脚本回退、重启不重复
  请求，以及在系统设置撤销权限后状态可刷新。
- 用组织正式 App/Agent ID、Developer ID 和 notary 凭据运行受保护 release workflow，保存 Accepted 记录，
  在 quarantine/离线环境验证 staple 与 Gatekeeper；再验证注销后的升级、rollback、卸载、重装和用户数据保留。
  仓库脚本已经 fail-closed 覆盖这些步骤，但没有凭据和干净真机时不能把它们记为正式发布通过。

## M4.5-G04：新附件 staging（ADR-0035）

`crates/maild/tests/uploads.rs` ×10 验证版本化完成与幂等回执、顺序块/相同重传/不完整拒绝、
取消、重排与移除、跨草稿引用拒绝、文件/总量/条数上限、重启与备份恢复、v8→v9 和保留对象
拒绝、路径/头部/base64 参数拒绝，以及真实 Unix socket + CLI 上传 3 MiB → 假 SMTP → MIME
解码字节一致。只用临时目录和虚构地址；未向真实邮箱投递。

`uploads` 模块 ×2 钉住 TTL/全局配额/匿名文件与 unsafe staging 拒绝；`tests/drafts.rs` 新增
主体越界与 drafts_read/write/messages_send 授权矩阵；maild-client 新增旧 root part 的零索引
兼容性用例。损坏持久行的 MIME、BLOB 超限和 size 不符均不能继续发送或推进草稿版本。
带引号/分号的文件名用真实 MIME 解析器回读，RFC 2231 编码保持中文及参数边界。

G04 当时没有 UI 变更；后续 08-A 已接入文件选择/进度/取消/自动保存/发送状态，见下。macOS 本地实跑与
Windows 真机验收分开记录，不能把 core hermetic 通过写成完整日用或签名发布已通过。


## M4.5-08-A：原窗口写信与持久发送（ADR-0036）

2026-09-23，Apple Silicon macOS，本机隔离目录、虚构账户与 TLS 假邮件服务器：

- `cargo test --workspace --all-targets --locked`：721 passed / 0 failed / 8 ignored。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`、前端 `npm run build`、
  `cargo build -p maild-ui --features embedded --locked` 均退出 0。
- `packages/maild-ui` 的 `npm test`：45 passed，含 20 项写信/并发回归；生产装配注册、
  防抖、旧回执、新输入保留、保存失败关窗、跨信件回复、发送冻结、未知态和冲突均覆盖。
- `crates/maild/tests/draft_send.rs`：版本锁阻止并发编辑/删除/重复发送，重启未知态、显式
  解锁、账户切换保留附件、v9→v10、DATA 成功后 QUIT 失败、明确拒绝与结果未知。
- `crates/maild-ui/tests/bridge.rs`：真实 frontend 身份，经 Rust 原生文件 token 上传超过
  1 MiB 的附件，显式主地址 From + Bcc → 本机 TLS SMTP；收到的 MIME 不含 Bcc 头，附件
  字节保持；重复投递被拒，重启后仍是 sent。另有三项发送权限组合与 provider 外层取消清理测试。
- 真 WKWebView：原窗口新建、阅读区回复/收起、草稿重开、系统文件面板选合成文件并显示
  “已保存到草稿”、本机发送后显示成功及副本入队；关窗/⌘Q 退出后读取隔离库确认最后输入持久。
  检查浅色/深色与缩窄窗口截图，修正内嵌回复挤压原文时与附件条重叠的问题。

开发复现：`cargo run -p maild-ui --example dev_stack`，再启动 `packages/maild-ui` 的 dev 服务与
打印出来的带 `--socket` 壳命令。测试示例只向本机 TLS 假服务器投递；凭据测试缝仅 debug 生效。
不要把默认启动的真实用户目录当作测试目录。观察页面内成功、daemon 草稿 sent 与 SMTP 回执，
不能仅以按钮变化判断投递完成。

剩余门禁：Windows/Linux 原生面板与拖放、真实服务商兼容性、签名包/安装升级与长期日用仍属
后续 10。本次原生拖放只完成实现及作用域静态复核；没有把系统拖放真机操作或长时间离线使用
记作已验收。Sent 同步活动页与部分批量失败恢复已由后续 08-B 完成，证据见下。

## M4.5-08-B：统一工作区与同步活动（ADR-0037）

2026-09-23，Apple Silicon macOS，仓库实现与关键路径已验证；完整视觉/跨平台矩阵尚未封板。

- `cargo test --workspace --all-targets --locked`：727 passed / 0 failed / 8 ignored。
- 前端 `npm test`：77 passed，包含 29 项工作区/活动回归；新增保护覆盖全身份选择、500 UID
  分组、精确入队/无变化/未处理计数、部分失败、普通到信不重建 HTML 文档、重连恢复详情、
  搜索退出恢复、线程重开与分页变化、以及等待草稿保存期间的后续手动导航优先。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`、前端 `npm run build` 与
  `cargo build -p maild-ui --features embedded --locked` 均退出 0。
- `tests/mailbox_queries.rs`：role 过滤与同 UID 多账户分页、跨文件夹线程、超过 2 MiB 的
  APPEND 原文不进入活动摘要、settled 失败及 ID 游标、事务内精确写入回执。
- `maild-ui/tests/bridge.rs`：真实 frontend 身份经 Unix socket 做过滤/搜索/线程/标记/活动裁决；
  同 UID 的 Sent 定位不会读成 Inbox；摘要无 payload、无模拟的服务器敏感回显。源消息主题
  单独有界读取，已认证但只有 mailbox_write 的前端可读旧队列接口，两条摘要路由都被 -32016 拒绝。
- 原生 WKWebView（合成邮件、隔离 daemon）：统一收件箱只见 Inbox；跨账户多选标记显示
  “1 封已排队，2 封已是目标状态”；部分归档失败保留选择；缺映射给明确说明；同账户移动
  到实际 Sent 文件夹入队；中文“印刷”搜到 Inbox 与 Sent 两封，可原位切换会话。活动中可见
  失败/未知、源消息主题、重试风险确认；取消不重放，确认“不再重试”后显示“已停止重试”。
  有浅色桌面宽度的现场截图。没有向真实邮箱投递或操作真实账户。

现场曾复现缺归档映射被泛化为“部分未确认”的问题，新增测试先失败（退出 1），修正后
完整前端测试通过。审查发现的导航、刷新、幂等回执与线程状态问题均补有回归。

**未完成的视觉复核：**原生主题菜单自动化未能稳定改变选项，随后工具报
`Sky Computer Use native pipe closed before response`；重置后仍报 `native pipe startup failed`。
因此没有把本片深色、窄窗口以及最后一次 HTML 现场复核记为通过；既有 HTML 隔离测试仍在
全量测试中通过。这些作为后续可用工具恢复后的补验项，不把工具故障判为已证明的应用缺陷。
Windows/Linux、实际服务商长时同步、安装/签名包与日用试用继续归 M4.5-10。

## M4.5-08-C：账户、设置与受管扩展（ADR-0038）

2026-09-23，Apple Silicon macOS，仓库实现与自动化验证完成，视觉验收尚未封板。

- `cargo test --workspace --all-targets --locked`：736 passed / 0 failed / 8 ignored。
- 前端 `npm test`：115 passed，包括设置控制器、真实组件表单输出以及通知等待设置确认的回归。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`、前端 `npm run build`、
  `cargo build -p maild-ui --features embedded --locked` 通过。
- Rust 桥对隔离 daemon 和本机 TLS 假 IMAP/SMTP，以真实 frontend 主体完成账户探测（不保存）、
  创建、改名、版本冲突、停发、启停、移除、运行时规则与地址登记/停用/移除。返回资料不含输入秘密。
  既有插件账户的只读探测保留绑定身份，过期版本在请求 provider 前拒绝，账户配置不被探测改写；NodeSeek 真实 provider 的本地绑定库也在探测后保持不变。
- 包测试覆盖 staging 源变化后仍安装已确认副本、单次令牌、升级、回滚权限不符拒绝、取消清理、
  independent 启用后重启、移除后重启、持久新默认、移除旧 TOML 默认后实际重启。
  只读前端对管理方法全部得到 -32016，默认文件未被创建。
- 默认选择单测覆盖 0600、损坏/未来版本保留、符号链接拒绝、隐藏元数据不占 64 包限额，
  以及模拟 rename 后 fsync 失败时保护磁盘上可见的选择并冻结后续写入。
- 前端覆盖探测与当前输入绑定、保存冲突清空临时秘密、断线/纪元失效、丢弃/取消、关窗等待与
  批准后锁定、账户绑定的地址表单、迟到安装令牌释放、独立包权限加载、显式目录重扫与单次安装。

先失败后修复的现场回归包括：启用包移除后重启失败、旧 TOML 默认阻止持久新默认、outgoing:null
未移除发信、关闭确认被导航替换，以及地址表单和迟到包令牌的问题。表单渲染测试检查混合收发
transport、24:00 的可编辑表示和既有合法配置范围；这些是组件输出证据，不是原生视觉截图。

**未完成的视觉复核：**本轮调用原生工具仍返回 `Sky Computer Use native pipe startup failed`；
in-app browser 与 Chrome 通道均返回 `Browser is not available`。因此没有把账户向导、系统目录
面板、安装确认的真实点击，或深色/窄窗口视觉检查记为通过。工具恢复后补验，并一并完成 08-B
遗留的深色/窄窗口/最终 HTML 现场复核。没有操作真实账户、真实插件或系统通知授权。


## M4.5-G05/09-A：恢复内核（ADR-0039）

2026-09-23，本片交付恢复内核与维护接口，图形窗口/实际安装包归 09-B/C。

- `recovery::tests`：一次性 no-clobber 预装、移除后不重装、同版本/坏包修复、旧包与坏默认字节保留、三个发布阶段失败回退、回退冲突明确报不确定；校验和拒截断/改字节，plist 身份/版本/固定路径、属主执行位、符号链接和不安全权限拒绝；满 64 项拒插入但允许替换。
- 离线验证使用隔离配置和不透明数据库字节：持有 Agent 或真实数据库维护锁均拒绝，不打开 SQLite；另配置且无 Store 的后台也被前端目录锁覆盖。原生副本来自模拟的固定 bundle，未执行夹具二进制。
- `api::recovery_tests`：真实同用户 Unix socket 往返完成修复，再认证为前端验证拒绝；User IPC 正向恢复并刷新默认/目录/身份；前端与后台插件、HTTP/stdio、任意路径/id 参数拒绝；缺副本无写入；客户端断连并丢弃后台上下文后，任务仍持有进程内和文件系统锁直到发布/刷新完成。
- 既有 `attachments` 套件验证后台退出后可立即重启：目录锁由 frontend 插件生命周期管理，不能遗留在根 Context。其余 workspace 测试覆盖正常收信、草稿、包管理及 Tauri 桥。

正式签名、真实 `.app` 激活/恢复、GUI 视觉、Windows 真机与日用未在 09-A 验收；旧版裸 CLI 不参与新增目录锁，迁移前需退出。SHA-256 仅校验构建副本完整性，不能代替签名或运行验收。

本片自动化记录：`cargo test --workspace --all-targets --locked` **761 passed / 0 failed / 8 ignored**；
其后增加真实 socket 用例并修正断连测试的并行等待条件，`cargo test -p maild --lib recovery --locked`
**27 passed / 0 failed**（与工作区用例重叠，不相加）。恢复相关新用例共 26 项。

`cargo clippy --workspace --all-targets --locked -- -D warnings` 通过；
`cargo check -p maild --all-targets --target x86_64-pc-windows-gnu --locked` 通过。
Windows 交叉检查仍报告两处基线 `unused_mut`（`maild-client/plugin_package.rs` 与 `maild/uploads.rs` 的 Unix 目录权限分支），本片未改；交叉编译不代表 Windows 运行验收。

## M4.5-09-B：原生修复窗口与故障隔离（ADR-0039）

2026-09-24 代码与专项验证完成；2026-11-12 补完全量回归，本片验收封板（仍不是可发布安装包，见 09-C/10）。

- `CARGO_INCREMENTAL=0 cargo test -p maild --lib --locked --offline`：248 passed / 0 failed / 1 ignored。
  覆盖缺失默认仍启动真实 Unix socket 和 Store、User IPC 停止后数据库可重开；默认缺包/坏包/激活失败
  只回落 `background`；失败目录不暴露安装根或锁、不删除既有身份；显式修复后恢复目录刷新。
- 同参数 `cargo test -p maild-client --lib`：55 passed；`cargo test -p maild-launcher --lib`：5 passed。
  覆盖固定 bundle 路径与替换拒绝、子进程路径环境保留/凭据清除、旧阅读器显式 socket 禁用自身后台托管、
  independent 入队与异步接受区分、窗口独占/重复聚焦/释放后重开/不跟随符号链接。
- `CARGO_INCREMENTAL=0 cargo check -p maild-launcher -p maild-ui -p maild-menu -p maild-desktop --all-targets --locked --offline`
  及 `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked --offline -- -D warnings` 通过。
- `npm --prefix packages/maild-ui test`：116 passed；typecheck 与 production build 通过。
  设置中的原生修复导航覆盖在线/离线渲染，权限仍在 Rust 桥和 Agent 执行。
- 打包脚本语法检查与 `packaging/macos/tests/test_bundle.sh` 通过；`test_release.sh`、
  `test_lifecycle.sh` 首轮因磁盘不足未完成，2026-11-12 补验通过（见下）。
- macOS 原生交互使用 `maild-launcher/examples/recovery_preview.rs`，只改变内存模拟状态。
  临时 ad-hoc app 的 NSAlert 视觉检查通过；实际点击验证修复取消/确认、模拟恢复回执、
  离线→启动→在线→停止确认→离线、返回、邮箱不可用错误、关闭后测试进程退出。
  未操作真实账户、ServiceManagement 注册或真实 Agent，也未把截图当成真实修复事务验收。

补验（2026-11-12，提交 `0257514` 之上，清理 `target/debug/incremental` 后）：

- `CARGO_INCREMENTAL=0 cargo test --workspace --locked`：779 passed / 0 failed / 8 ignored；
  其中 `maild-ui` 的 `tests/bridge.rs` 9 passed（首轮因 `No space left on device` 未运行）。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `packaging/macos/tests/test_release.sh`：PASS。
- `packaging/macos/tests/test_lifecycle.sh`：PASS；日志中的 `Killed: 9` 来自脚本注入的
  `MAILD_MANAGE_TEST_CRASH=upgrade-old-moved` 崩溃恢复用例，属预期。

09-C 仍负责四二进制真实打包、固定恢复副本、首次预装、默认/旧 ID 迁移及删除 egui。
正式签名/公证、Windows 运行、真实服务商日用观察仍归 10。本片协议代数与数据库 schema 不变。

## M4.5-09-C：Tauri 默认交付与删除 egui（ADR-0039）

2026-11-12。本片删除 `crates/maild-desktop`，因此工作区总数比 09-B 少了 egui 壳自身的用例。

- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`：708 passed / 0 failed / 2 ignored；
  `maild-ui` 的 `tests/bridge.rs` 9 passed（测试配置保留旧 `default = "builtin-egui"`，端到端钉住读取时迁移）。
  新增/改写：旧 ID 读取迁移且不改写文件、显式旧 ID 走恢复别名、恢复入口空授权；Agent 启动在目录租约下
  只预装一次、用户移除后不装回且不写偏好；打包清单模板与开发清单路由/授权一致并可通过预装检查；
  启动器配置引导（私有、无凭据、no-clobber、新默认 `com.maild.ui`）；布局发现不再接受 `maild-desktop`。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `cargo check -p maild -p maild-client -p maild-launcher -p maild-app -p maild-menu -p maild-paths --all-targets --target x86_64-pc-windows-gnu`
  通过，仅两处既有 `unused_mut`。`maild-ui` 的 Windows 构建因既有缺少 `icons/icon.ico` 在 tauri-build 失败，归 10。
- `packaging/macos/tests/test_bundle.sh`、`test_release.sh`、`test_lifecycle.sh` 与 `dry-run.sh` 均 PASS。
  新覆盖：四二进制架构一致；UI 字节复制、65 字节摘要、清单/plist 版本；改动 UI 字节或手改清单被拒，
  重算后通过；签名 UI 后旧摘要被拒、重算后内层到外层签名通过 `codesign --verify --deep --strict=all`；
- 真实构建：`pnpm --dir packages/maild-ui build`、本机 release 构建 Agent/Launcher/Menu 与
  `maild-ui --features embedded`，`build-bundle.sh` 校验真实启动器中的 Agent 身份契约，
  `build-preview-dmg.sh` 对真实 UI ad-hoc 签名、重算摘要并通过严格深度校验。
- 隔离 HOME（`/private/tmp` 下，只运行 DMG 内已签名 Agent，不启动 GUI）：首次启动日志「已预装内置前端」，
  `maildctl frontends` 默认 `com.maild.ui`，权限完整，预装副本签名有效；删除包后重启不装回、默认回落 `builtin-repair`；
  `repairBuiltinFrontend` 重装并设为默认，保留备份；配置改回 `builtin-egui` 后默认仍为 `com.maild.ui`，TOML 未改写。
  人为损坏已安装 UI 可执行文件时，目录加载不检查其字节，按设计只在激活失败时转入原生修复；该图形路径未在本片执行。

独立复审后的修正：`maild-ui` 内嵌形态标记（真实 `--features embedded` 构建被接受；同机默认形态构建被
`build-bundle.sh` 拒绝；打包测试新增缺标记 UI 的负例）；启动器在激活请求前释放窗口锁。

随应用升级内置前端（同日追加）：新增版本解析、仅更旧时替换并备份、默认不变、相同/更新/不可比较/损坏/已移除不动、
Agent 启动路径升级共 4 组用例。真实构建 0.1.0 与 0.2.0 两个包，在 `/private/tmp` 隔离 HOME 下依次运行其 Agent：
首次 0.1.0 预装；换 0.2.0 后日志「内置前端已从 0.1.0 升级到 0.2.0，旧包已备份」，`maildctl frontends` 显示 0.2.0，
`.repair-backups/upgrade-*` 保留旧包；再运行 0.1.0 时保持 0.2.0 不降级。同时移除 09-C 之前布局的打包兼容
（该布局只在开发者本机装过）。最终重跑：工作区 712 passed / 0 failed / 2 ignored、Clippy、三套打包测试与 dry-run 均通过。

受限项：`/tmp` 是符号链接，Agent 的生命周期锁会拒绝该祖先路径，隔离试验必须使用 `/private/tmp`。
LaunchServices 启动的 `Maild UI.app` 继承登录会话环境而非隔离 HOME，所以本片没有在隔离环境中执行
图形激活（启动器打开邮箱、激活失败转入修复窗口）。正式签名/公证、真实用户目录下的图形闭环、Windows 运行均归 M4.5-10。

## M4.5-10：发布门禁扩展与兼容降级矩阵（ADR-0040）

2026-11-12。收口仓库内全部发布、兼容降级与跨平台静态门禁（见 [ADR-0040](../decisions/0040-release-gate-and-compatibility-matrix.md)）。

- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`：**717 passed / 0 failed / 2 ignored**
  （相对 09-C 新增 5 项）：
  - `crates/maild-ui/src/bridge.rs`：`compat_gate_only_lets_reads_through_on_mismatch`（只读 RPC 放行、写操作与未知新方法默认拦截返回 `incompatible`）；
  - `crates/maild-ui/tests/bridge.rs`：共 10 passed，含新增端到端 `version_mismatch_blocks_writes_keeps_reads_and_recovers_on_reconnect`（真实 daemon 上模拟 `kernel_outdated` / `app_outdated`：邮箱只读查询仍通，标记/写信/阅读策略/包安装暂存全被拦截且 `.incoming` 无残留，设置页打开 `builtin-repair` 修复窗口豁免放行，匹配版本重握手后写能力恢复）；
  - `crates/maild/src/recovery/tests.rs`：共 44 passed，新增 3 项矩阵用例：
    1. `agent_upgrade_after_user_removal_never_reinstalls_bundled_frontend`（用户删除 `com.maild.ui` 后升级应用到 0.2.0，Agent 启动绝不装回，默认回落 `builtin-repair`）；
    2. `cross_protocol_upgrade_and_rollback_matrix`（协议代晋升时 `refresh_up_to` 接受历史协议代自动升级旧包并备份；应用回退遇更高协议代已装包时 `refresh` 保持不动、目录扫描跳过并回落 `builtin-repair`，显式 `repair` 可恢复匹配版本并备份）；
    3. `runtime_permission_checks_do_not_exempt_preinstalled_frontend`（预装包目录权限被改为 `0775` 时运行时扫描照常拒绝并回落 `builtin-repair`，无预装豁免）。
- `pnpm --dir packages/maild-ui exec tsc --noEmit -p tsconfig.json`：PASS；
  `pnpm --dir packages/maild-ui test`：**118 passed**（9 个测试文件；`host.test.ts` 新增版本不一致禁用在线写命令、跨 `connection_epoch` 过期快照丢弃与重连恢复断言）；
  `pnpm --dir packages/maild-ui build` 与 `CARGO_INCREMENTAL=0 cargo build -p maild-ui --features embedded --locked`：PASS。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings`：PASS（零警告）。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings`：PASS（零警告；已补齐 `crates/maild-ui/icons/icon.ico` 并消除非 Unix / 非 macOS 条件编译告警）。
- `bash -n packaging/macos/*.sh packaging/macos/tests/*.sh`、`packaging/macos/dry-run.sh`、
  `packaging/macos/tests/test_bundle.sh`、`packaging/macos/tests/test_release.sh`、
  `packaging/macos/tests/test_lifecycle.sh`：全部 PASS。已用 `/usr/bin/codesign` 实证顶层签名封条覆盖
  `Contents/Resources/frontends/com.maild.ui/Maild UI.app`，固化“由内而外签名 → `seal-recovery-payload.sh`
  重算摘要 → 单归档公证 → 仅顶层 `Maild.app` 执行 `stapler staple`”规则；`ci.yml` 与 `release-macos.yml`
  已同步接入 `--locked`、webview 类型检查/单测及 `menu-bundle-id.txt` 预检。

外部门禁（不在仓库自动化中虚报通过）：
1. 冻结正式 App/Agent/Menu bundle IDs 与 `team-id.txt`，注入 Apple Developer ID 与 Notary API 凭据实跑 `release-macos.yml`，保留 `Accepted` 公证记录并在干净 macOS 13+ 账户完成 quarantine/离线 Gatekeeper、登录项批准保留、通知归属与升级/回滚/卸载真机演练；
2. 真实 macOS 登录会话下的 LaunchServices 图形激活闭环（正常打开 Tauri 默认前端、损坏已装 UI 自动落入原生修复窗口并恢复、覆盖安装出现版本横幅并经修复窗口重启恢复）；
3. Windows 真机运行验收（named pipe 跨用户 ACL、WebView2 渲染、关闭窗口资源释放）与后续 Windows 激活清单扩展；
4. 真实服务商邮箱账户下的多日连续日用观察。

## 2026-11-17：Release 守护进程后台收信资源实测（ADR-0041 / 0042 / 0043 收官）

### Release `maild` 子进程低占用基准（`crates/maild/examples/footprint_bench.rs`）

完全自包含、本地回环 TLS 的真实多账户压测：基准进程起 3 个自签 TLS 假邮件服务器（IMAP 993 风格隐式
TLS + SMTP 465 风格隐式 TLS，SMTP **只宣告 `AUTH LOGIN`** 且校验凭据），主信箱 `main` 预置
**1,500 封**、`work` / `alerts` 各 300 封历史邮件（中文纯文本 / 中文 HTML 备选 / HTML + `cid:`
内联图片 / 纯文本 + 2 KiB 普通附件四种 MIME 形态轮换，`Date` 在 2025 年）；再以**独立子进程**拉起
同目录的 release `maild`（无任何 GUI / WebView 进程），装配 `store` + 3 × `receive(tls=implicit,
idle=true, initial_sync_limit=200)` + `filters` + `rules(persist, notify_new_mail)` + `otp` +
`notify(dev-log)` + `scheduler` + `transport(implicit 465, AUTH LOGIN)` + `api(unix)`，全部经 Unix
socket JSON-RPC 与 `message/received` 推送测量。任一验收不成立即非零退出。

```sh
cargo build --release -p maild --examples --bins --locked
target/release/examples/footprint_bench          # Markdown 表 + JSON 报告
target/release/examples/footprint_bench --json   # 只输出 JSON
```

实测环境：macOS（Darwin 27.2）arm64，`[profile.release]` thin LTO + `strip = "symbols"`。
2026-11-17 连续 3 次运行全部通过（`failures: []`，SIGTERM 退出码 0，临时目录自动清理），数值如下：

| 指标 | 运行 1 | 运行 2 | 运行 3 |
|---|---|---|---|
| 冷启动：进程拉起 → socket 就绪 | 13 ms | 14 ms | 15 ms |
| 冷启动：3 账户全部 `online` | 65 ms | 71 ms | 83 ms |
| 冷启动：3 × 200 封有界同步落库 + FTS 索引 `pending = 0` | **185 ms** | **197 ms** | **200 ms** |
| 冷启动后 RSS / `phys_footprint` | 17.9 / 3.14 MiB | 18.0 / 3.05 MiB | 18.0 / 3.08 MiB |
| 稳态空闲 `status.runtime` RSS / `phys_footprint` | **14.2 / 4.95 MiB** | **14.7 / 4.95 MiB** | **15.4 / 5.08 MiB** |
| 5 秒空闲窗口 CPU（`proc_pid_rusage` 累计 CPU 时间 / 墙钟） | 0.0013 %（0.06 ms） | 0.0013 %（0.07 ms） | 0.0009 %（0.05 ms） |
| 5 秒空闲唤醒次数（idle + interrupt wakeups）/ `ps pcpu` 峰值 | 1 / 0.0 % | 1 / 0.0 % | 1 / 0.0 % |
| 30 封实时投递（SMTPS 465 → IMAP IDLE → `message/received`）到达 | 30/30 | 30/30 | 30/30 |
| 推送延迟 P50 / P95 / max | **3.9 / 9.1** / 38.5 ms | **4.1 / 10.5** / 10.7 ms | **3.8 / 7.4** / 11.1 ms |
| 验证码通知（6 封验证码信，期望恰好 6 条） | 6 | 6 | 6 |
| 普通新邮件通知（24 封，3 账户 × 8 封突发；期望每账户 3 逐封 + 1 合并 = 12） | 12 | 12 | 12 |
| 注入后 RSS / `phys_footprint`（Δ footprint） | 14.7 / 5.25 MiB（+0.30） | 15.4 / 5.28 MiB（+0.33） | 16.0 / 5.27 MiB（+0.19） |
| 注入后 DB / WAL | 1.66 / 4.00 MiB | 1.66 / 4.00 MiB | 1.66 / 4.00 MiB |
| 守护进程 `send`（全局 transport，SMTPS 465 + `AUTH LOGIN`）→ IDLE 收回 | 4 ms | 6 ms | 5 ms |
| 冷启动正文抓取最低 UID（窗口外不得抓取） | main 1301 / work 101 / alerts 101 | 同左 | 同左 |
| `searchMessages(scope="server")` 定位第 5 封 / `fetchServerMessage` 拉回 | 3 / 1 ms | 3 / 2 ms | 4 / 2 ms |
| 结束时 RSS / `phys_footprint` / 落库消息数 | 16.3 / 5.23 MiB / 632 | 16.4 / 5.31 MiB / 632 | 16.6 / 5.25 MiB / 632 |

逐项验收断言（由基准程序自动判定）：

- 冷启动只落库 3 × 200 = 600 封；三个服务器的正文 `UID FETCH` 日志里没有任何窗口外 UID；
  2025 年的历史积压信**零通知**（24 小时积压阈值生效）。
- 30 封实时邮件全部经 IDLE 推送到达；验证码信只发验证码通知（不与新邮件通知双发），普通信按
  10 秒滑动窗口限流为每账户「3 封逐封 + 1 条合并提示」；注入后 `phys_footprint` 增量 < 16 MiB、
  WAL < 8 MiB（实测 +0.19～+0.33 MiB，WAL 恒为 `journal_size_limit` 的 4 MiB）。
- 冷启动窗口外的第 5 封：本地 `searchMessages` 0 命中 → `scope="server"` 恰好命中 UID 5 →
  `fetchServerMessage` 拉回的正文与生成时的中文正文逐字一致 → `getMessage` 可读（已幂等落库）。
- 全部 SMTP 认证（30 封注入 + 1 封守护进程发信）都走 `AUTH LOGIN` 且成功。

口径说明：`phys_footprint` 是 Activity Monitor「内存」列口径；RSS 含只读映射的共享代码页，因此
远大于 footprint。延迟从基准进程**发起** SMTPS 投递（含 TLS 握手与 `AUTH LOGIN`）计到推送帧
到达，是端到端上界。空闲 CPU 用 `proc_pid_rusage` 的纳秒级累计 CPU 时间计算——`ps -o time` 只有
10 ms 分辨率，在这个空闲水平上恒读 0。回环 TLS 不代表公网 RTT；真实 provider 的推送延迟取决于
服务端 IDLE 推送时机。

### SMTPS 465 / `AUTH LOGIN` / 自动发现（ADR-0042）

- `crates/maild/tests/smtp_implicit_and_auth.rs` ×10（hermetic）：隐式 TLS 探测先读 `220` greeting
  再 `EHLO`；隐式 TLS 投递中文主题 + Cc + 附件 + 点开头行的富报文；只宣告 `AUTH LOGIN` / 旧式 `AUTH=LOGIN` 时回落
  `AUTH LOGIN`；同时宣告时优先 `PLAIN`；错误密码报「认证被拒绝」且不回显服务器原文；端口配反
  （implicit 对 STARTTLS 服务器、STARTTLS 对 implicit 服务器）分别在 TLS 阶段失败或超时并给出
  可行动提示；主流 provider 知识库匹配；SSRF 目标（IP 字面量、`localhost`、内网后缀、私网解析）
  一律拒绝；无网络时回落启发式建议。`autodiscover` 模块单测 ×4。
- `footprint_bench`：release 守护进程的全局 transport 以 `tls = "implicit"` 对只宣告 `AUTH LOGIN`
  的回环 465 服务器发信并经 IDLE 收回（上表）。
- 真实 QQ / 网易 / Gmail 465 账户与公网自定义域名的自动发现仍属外部证据（见下方剩余边界）。

### 智能通知与规则持久化（ADR-0043）

- `crates/maild/tests/smart_notify_and_rules.rs` ×18：中英文验证码模板命中、账单/会议/物流/发票/
  链接等普通邮件不误报、不从更长数字串里截码、旧 `extract_code` 契约不变；`rules_state.toml`
  跨重启保留且 0600 原子写、非法 patch 不落盘、未来 `format_version`/未知字段/宽权限/符号链接
  fail-closed、只在 `persist = true` 时挂载侧车；普适新邮件通知的已读/静音/关闭跳过、验证码信只发
  验证码通知、账单不是验证码但是新邮件、脱敏预览、静默关键词与静默时段、10 封突发合并。
- `crates/maild-ui/tests/daily_mail_bridge.rs`（真 daemon e2e）：前端主体经桥 `syncMessages`、
  `discoverEmailProvider`、服务器检索、按需拉回历史并经隔离文档通道渲染；运行时
  `setSettings {persist: true}` 挂载侧车后跨重启保留。
- `footprint_bench`：release 子进程上验证码精准通知与普通新邮件限流（上表）。

### Tauri 登录向导与日常交互回归

`packages/maild-ui` Vitest（含 `providers.test.ts`、`dailyMail.test.ts`、`otp.test.ts`、
`preferences.test.ts`）：12 类 provider 预设与 `@domain` 自动匹配、服务器面板在 dns/connect/tls/
mailbox 探测失败时自动展开、TLS↔STARTTLS 标准端口联动、授权码分组空格剥离、未知域名自动发现
回落 `imap.`/`smtp.` 猜测；「立即同步」对旧后台静默降级、服务器历史检索 + 按需拉回渲染、验证码条
一键复制、HTML 深色适配只在宿主侧滤镜（CSP 与 sandbox 不变）、键盘流与快捷键面板、新邮件通知
开关与按账户静音的持久化状态如实展示。最终计数见下方门禁。

### macOS bundle 与生命周期门禁实跑

前端 `pnpm --dir packages/maild-ui build`（`tsc --noEmit` + Vite 生产构建），release
`cargo build --workspace --release --locked` 与 `cargo build --release --locked -p maild-ui --features embedded`
（`maild` 9.0 MB、`maild-agent` 9.0 MB、`maild-launcher` 4.8 MB、`maild-menu` 4.5 MB、`maild-ui` 10.9 MB）后：

- `bash -n packaging/macos/*.sh packaging/macos/tests/*.sh`：PASS
- `packaging/macos/dry-run.sh`：PASS（临时未签名 bundle 验证后删除）
- `packaging/macos/tests/test_bundle.sh` / `test_release.sh` / `test_lifecycle.sh`：全部 PASS
- 真实二进制：`packaging/macos/build-bundle.sh` → 28 MB `dist/macos/Maild.app`，`verify-bundle.sh` 通过，
  `recovery.sha256` 等于 `maild-ui` 的 SHA-256；按 ADR-0040 顺序 ad-hoc 签名内嵌 `Maild UI.app` →
  `seal-recovery-payload.sh` 重算摘要（与签名后的 `maild-ui` 一致）→ 签 Agent / Menu → 签顶层，
  `codesign --verify --deep --strict=all` 通过；随后删除 `dist/`。正式 Developer ID / notary 仍属外部门禁。

### 收官全量回归（2026-11-17）

- `pnpm --dir packages/maild-ui typecheck && pnpm --dir packages/maild-ui test && pnpm --dir packages/maild-ui build`：
  PASS，Vitest **191 passed / 12 文件**。
- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`：**795 passed / 0 failed / 2 ignored**
  （ignored 为 Stalwart 真服务器与 macOS Keychain 冒烟；含 `receive_footprint` 7、`smtp_implicit_and_auth` 10、
  `smart_notify_and_rules` 18、`maild-ui` `bridge` 10 + `daily_mail_bridge` 2；`footprint_bench` 作为 example
  参与编译，实跑结果见上表）。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings`：PASS（零警告）
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings`：PASS（零警告；`footprint_bench` 在非 Unix 目标编译为提示性空 `main`）

## 2026-11-17：单 Dock 身份（ADR-0044）合入回归

分支 `cliouo/dock-integration-dmg` 合入运行时 `cliouo/single-dock-runtime`（`64d6103`）与打包
`cliouo/macos-bundle-icns-packaging`（`a08b8fe`），两者无文件重叠。合入后发现既有用例
`external::tests::ui_launch_is_single_instance_reaps_exit_and_shutdown_kills_child` 在高负载下偶发
`ParseIntError { kind: Empty }`：测试脚本 `echo $! > descendant.pid` 中 shell 先创建空文件再写入，而用例只等文件存在。
修正为等到读到以换行结尾、可解析的完整 PID（仅测试代码），单测与全量回归随后通过。

- `pnpm --dir packages/maild-ui typecheck && pnpm --dir packages/maild-ui test && pnpm --dir packages/maild-ui build`：
  PASS，Vitest **191 passed / 12 文件**。
- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`：**811 passed / 0 failed / 2 ignored**
  （较收官基线 795 新增 16 项 ADR-0044 单测；ignored 仍为 Stalwart 真服务器与 macOS Keychain 冒烟）。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings`：PASS（零警告）
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings`：PASS（零警告）
- `bash -n packaging/macos/*.sh packaging/macos/tests/*.sh`、`packaging/macos/dry-run.sh`、
  `packaging/macos/tests/test_bundle.sh`、`test_release.sh`、`test_lifecycle.sh`：全部 PASS
  （`test_lifecycle.sh` 输出中的 `Killed: 9` 是 `MAILD_MANAGE_TEST_CRASH` 故意注入的崩溃）。
- Release 二进制：`cargo build --release --locked -p maild -p maild-launcher -p maild-menu` 与
  `cargo build --release --locked -p maild-ui --features embedded`（`maild` 9.0 MB、`maild-agent` 9.0 MB、
  `maild-launcher` 2.1 MB、`maild-menu` 1.9 MB、`maild-ui` 10.9 MB）。
- 真实 bundle（临时目录）：`build-bundle.sh` → 33 MB `Maild.app`，`Contents/MacOS/{maild-launcher,maild-ui}`、
  `Contents/Resources/Maild.icns`、`CFBundleIconFile = Maild`；顶层 `maild-ui` 与 release 产物逐字节一致，
  `recovery.sha256` 等于恢复副本 `maild-ui` 的 SHA-256；`verify-bundle.sh` 通过。随后按 ADR-0040/0044
  由内而外 ad-hoc 签名：内置 `Maild UI.app` → `seal-recovery-payload.sh` 重算摘要（与签名后字节一致）→ Agent →
  Menu → 顶层 `Contents/MacOS/maild-ui`（`--identifier com.maild.ui --options runtime`）→ 顶层 App；再次
  `verify-bundle.sh` 通过，`codesign --verify --deep --strict=all` 报 `valid on disk` / `satisfies its Designated
  Requirement`，顶层 `maild-ui` 为 `Identifier=com.maild.ui`、`flags=0x10002(adhoc,runtime)`；临时 `.app` 已删除。
- 未在本记录中执行：真机从 Dock 打开只亮一个 `Maild` 图标、`⌘Q` 后 Agent 仍在、前台窗口点击通知/菜单栏立即跳转
  （见 [PROGRESS 外部门禁 2](../PROGRESS.md#剩余外部门禁1-边界)）。

## 2026-11-18：关闭退回菜单栏（ADR-0045）合入回归

分支 `cliouo/tray-minimize-integration` 合入 M1 运行时（`965168a`，`cliouo/tray-only-minimize-close`）与 M2 打包
（`983cd63`，`cliouo/plist-lsuielement-packaging`，顶层 `Maild.Info.plist.in` `LSUIElement = true` 与门禁），两者无文件重叠。
合入后收敛最小化为系统原生语义：删除黄灯 target/action 改写（`define_class!` `MinimizeTarget`）、自定义
Window → Minimize 菜单、`ui_minimize_to_menu` 命令与 capability、`is_minimized` 自动收回兜底，以及 webview
`windowShortcuts.ts` 与其测试；`macos_window.rs` 只留 `activate_app` / `deactivate_app`。红灯 `×` / `⌘Q` 的
`retire_window` 与 `frontend/intent` 唤醒链路不变。

- `pnpm --dir packages/maild-ui typecheck && pnpm --dir packages/maild-ui test && pnpm --dir packages/maild-ui build`：
  PASS，Vitest **191 passed / 12 文件**（M1 的 `windowShortcuts.test.ts` 随功能撤销而删除）。
- `CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked`：**815 passed / 0 failed / 2 ignored**
  （较 ADR-0044 回归 811 新增 ADR-0045 推送/唤醒单测；ignored 仍为 Stalwart 真服务器与 macOS Keychain 冒烟）。
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings`：PASS（零警告）
- `CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings`：PASS（零警告）
- `bash -n packaging/macos/*.sh packaging/macos/tests/*.sh`、`packaging/macos/dry-run.sh`、
  `packaging/macos/tests/test_bundle.sh`（含 `LSUIElement = true` 断言）、`test_release.sh`、`test_lifecycle.sh`：全部 PASS
  （`test_lifecycle.sh` 输出中的 `Killed: 9` 是 `MAILD_MANAGE_TEST_CRASH` 故意注入的崩溃）。
- Release 二进制（前端 `build` 之后）：`cargo build --release --locked -p maild -p maild-launcher -p maild-menu` 与
  `cargo build --release --locked -p maild-ui --features embedded` 零警告（`maild` 9.0 MB、`maild-agent` 9.0 MB、
  `maild-launcher` 2.1 MB、`maild-menu` 1.9 MB、`maild-ui` 10.9 MB）；`maild-ui` 内嵌本次构建的 `index-DULzBrkm.js`，
  不含 `ui_minimize_to_menu`。
- 未在本记录中执行：真实 `.app` / `.dmg` 生成与签名（由协调方基于上述 release 产物完成），以及真机 Dock 观察
  （见 [PROGRESS 外部门禁 2](../PROGRESS.md#剩余外部门禁1-边界)）。

## 2026-11-18：菜单栏生命周期绑定（ADR-0046）合入回归

分支 `cliouo/menubar-dock-quit-integration` 以 Q1（`687bbce`，`cliouo/menubar-quit-all`：守护进程 `agent/stopping` 推送、
菜单栏「退出 Maild」调 `stopManagedAgent`、menu supervisor 与 Agent 生命周期绑定、排空窗口 350 ms）为基，合入 Q2
（`11fe854`，`cliouo/dock-quit-zero-residue`：`maild-client` `AgentStopping`、`maild-app` `shutdown_requested`、`maild-ui`
统一退出流水线与运行时 `applicationShouldTerminate:`），零文件冲突。合入后语义：点击菜单栏「退出 Maild」一并关闭窗口、
菜单栏与后台守护；点击窗口红叉、`⌘Q` 或 Dock 右键「退出」均仅关闭窗口并彻底清空 Dock 图标。

新增跨 crate 端到端 `crates/maild-ui/tests/bridge.rs::stop_managed_agent_reaches_the_frontend_before_the_daemon_exits`：真 `maild`
（`with_managed_agent_control()`）+ 以 `frontend:com.maild.ui` 认证的 `maild-app` 连接；user 侧 `RpcClient` 调 `stopManagedAgent`
后断言 `ViewState.shutdown_requested` 在 daemon 任务尚未结束、连接仍 `online` 时置位（即排空窗口内），旁听 user 长连接收不到
`agent/stopping`，随后 daemon 无需外部信号自行正常退出并删除 socket，断线后 `shutdown_requested` 不回落。单独连跑 6 次均通过（≈1.4 s）。

- `pnpm --dir packages/maild-ui typecheck && pnpm --dir packages/maild-ui test && pnpm --dir packages/maild-ui build`：
  PASS，Vitest **191 passed / 12 文件**。
- `cargo test --workspace --all-targets --locked`：**822 passed / 0 failed / 2 ignored**
  （较 ADR-0045 回归 815 新增 Q1/Q2 停机推送、解码、reducer 与本端到端用例；ignored 仍为 Stalwart 真服务器与 macOS Keychain 冒烟）。
- `cargo clippy --workspace --all-targets --locked -- -D warnings`：PASS（零警告）。
- `cargo clippy --target x86_64-pc-windows-gnu --workspace --exclude maild-ui --all-targets --locked -- -D warnings`：PASS（零警告）。
- Release 二进制（前端 `build` 之后）：`cargo build --release --locked -p maild -p maild-launcher -p maild-menu`
  （`maild-agent` 是 `maild` 包内的 bin 而非独立包，`-p maild-agent` 会被 cargo 拒绝）与
  `cargo build --release --locked -p maild-ui --features embedded` 零警告：`maild` 9.1 MB、`maild-agent` 9.0 MB、
  `maild-launcher` 2.1 MB、`maild-menu` 1.9 MB（含「退出 Maild」菜单项）、`maild-ui` 10.9 MB（内嵌 `index-DULzBrkm.js`）。
- `packaging/macos/dry-run.sh`、`packaging/macos/tests/test_bundle.sh`、`test_release.sh`、`test_lifecycle.sh`：全部 PASS。
- 未在本记录中执行：真实 `.app` / `.dmg` 生成与签名，以及真机 Dock / 菜单栏 / 注销观察
  （见 [PROGRESS 外部门禁 2](../PROGRESS.md#剩余外部门禁1-边界)）。

### 剩余边界（需正式凭据或真机）

正式 bundle ID + Developer ID / notary 实跑与干净 macOS 13+ Gatekeeper；真机图形会话（LaunchServices 激活、
登录项、通知授权归属、升级/回滚/卸载演练）；真实 QQ / 网易 / Gmail / iCloud 账户的 465 + `AUTH LOGIN` 全链路与
自定义域名自动发现；5 万封级信箱、休眠唤醒与 24 小时 soak；Windows 真机；NodeSeek provider live 验收。
完整清单见 [PROGRESS](../PROGRESS.md#剩余外部门禁1-边界)。
