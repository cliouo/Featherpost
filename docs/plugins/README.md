# 外部 frontend package v1

M3.6 的前端是 **maild JSON-RPC 的独立进程消费者**，不是加载进 Agent 的 Rust 动态库。
原生恢复入口的稳定 ID 是 `builtin-repair`，只提供 `background` 路由；默认前端损坏或不可打开时进入修复窗口，不提供文本邮箱。09-B 过渡期仍保留旧 `builtin-egui` 阅读器。

> **默认前端同样走本文档的 package 路径，不享有特权通道。** 09-C 将预装并默认选择
> Tauri package `com.maild.ui`（`lifecycle = "on_demand"`、`kind = "macos_application"`），
> 安装/升级/回滚/移除复用下文事务；随后删除 egui。原生修复独立于前端包。见
> [ADR-0039](../decisions/0039-native-recovery-and-tauri-default.md)。

## 界面扩展设计（2026-09-11，尚未实现）

用户要求插件也能在默认 Tauri 前端中贡献命令、局部面板、工作页和独立窗口。
设计见 [UI-EXTENSIONS.md](../design/tauri/UI-EXTENSIONS.md)，与完整 frontend package 和现有 `ui_executable` 管理进程区分。
默认邮件模块将使用同一贡献注册机制；外部 UI 扩展需独立 schema、权限作用域和视图生命周期，现有 v1 manifest/路由并不自动支持。以下仍描述当前已实现格式。

## 安装目录

`maild paths` 输出 `frontends=`。每个第三方前端占一个与 ID 同名的子目录：

```text
frontends/
└── org.example.reader/
    ├── frontend.toml
    └── Reader.app/
```

Agent 启动时扫描这一层，最多检查 64 个 package；安装/升级/移除事务（下文）会即时重载
目录，无需重启 Agent。目录、manifest 和 launch path
不得是符号链接；Unix 还要求由当前用户拥有且不能由 group/other 写入。manifest 最大 64 KiB，任一
相对 launch path 都不得包含 `..` 或逃出 package。未知或损坏 package 不会被激活；如果配置
中的默认 ID 未通过检查，实际默认回落为 `builtin-repair`，保留原选择且不停止邮件内核。
目录无法安全扫描或独占时，拒绝包变更并保留既有身份凭证，显式修复成功后才恢复正常目录刷新。

自 M4（ADR-0019）起安装/升级/移除走**事务 RPC**，Agent 不必停用：CLI 把完整 package
复制进受管目录的 `.incoming/<rand>` staging（这是 Agent 之外唯一合法的写动作），然后
`maildctl plugin install --source <目录> --frontend` 让 Agent 校验并原子发布——目标不存在即
安装，已存在则要求三段数字版本**严格更高**才允许升级，旧版自动移入 `.rollback/<id>/`。
回滚用 `maildctl plugin rollback --frontend <id>`（当前版与回滚位互换，再执行一次切回，
来回切换）。删除前先用 `setDefaultFrontend` 切换到另一个已安装阅读器或 `builtin-repair`，再用
`maildctl plugin remove --frontend <id>`。`.incoming` / `.removed` / `.rollback` 点前缀
目录永不激活，崩溃残留不阻塞后续扫描。事务失败返回 `-32017`，坏 staging 由 Agent 清理。
marketplace、下载签名与自动升级不在 v1 内：不能把网络下载直接接到这个受信目录。

## Manifest

```toml
schema_version = 2             # 1 仍受支持；permissions 需要 2
protocol_version = 1
id = "org.example.reader"       # 必须等于目录名
name = "Example Reader"
version = "1.0.0"
lifecycle = "on_demand"        # on_demand | independent
capabilities = ["inbox", "message", "compose", "settings", "background"]
permissions = ["mailbox_write", "accounts_manage"]   # 可选，缺省空表

[launch]
kind = "macos_application"
path = "Reader.app"             # 只能是 package 内相对路径
```

- `on_demand` 必须有 `launch`。首版只接受 `macos_application`，由 LaunchServices 激活且不传
  token、账户、UID 或正文；frontend raw executable 的单实例、回收与跨平台激活尚未定义，
  因此 v1 fail-closed 拒绝。
- `independent` 不得有 `launch`。它由用户或其他进程管理；Agent 只排队 intent，已运行的
  consumer 自行调用 `claimNextOpenIntent`。
- capability 同时承担两个语义：**intent 路由声明**（默认前端缺少某 route 时该次 intent
  自动交给内置恢复前端）与 **API 授权来源**（ADR-0018：`inbox|message → messages_read`、
  `compose → messages_send`、`settings → settings_read + settings_write`、
  `background → 无 API 授权`）。
- `permissions` 是**纯授权声明**，与路由分开（[ADR-0030](../decisions/0030-frontend-authorization-closure.md)
  决策 2）。路由回答「这个 intent 交给谁」，授权回答「它能调哪些方法」；把写权限挂到
  `message` 这类路由词上，会让每一个已安装的只读前端在升级后静默获得删邮件的能力。
  有效授权 = 路由翻译所得 ∪ `permissions` 所得。词表：

  | 词 | 覆盖 |
  |---|---|
  | `mailbox_write` | `markMessages`、`moveMessages`、`deleteMessages`、`listPendingOps`、`resolvePendingOp` |
  | `accounts_manage` | `listAccountProfiles`、`probeAccount`、`createAccount`、`updateAccount`、`setAccountEnabled`、`removeAccount` |
  | `addresses_manage` | `listAddresses`、`createAddress`、`updateAddress`、`removeAddress` |
  | `plugins_manage` | `listExternalPlugins`、`openExternalPluginUi`、`commitPluginInstall`、`removePlugin`、`rollbackPlugin`、`setPluginEnabled`、`refreshPlugins` |
  | `notifications_read` | `getNotificationStatus`（ADR-0030 决策 11：走显式声明，不塞进 `settings` 路由） |

  另有一个**隐含**的词：每个前端记录都自动持有 `frontend_open`（`listFrontends`、
  `claimNextOpenIntent`、`openFrontend`）——claim 排给自己的 intent 是前端的定义性动作，一个不能
  claim 的前端不是前端。但前端主体只能 claim **自己的** id，替别的前端 claim 得到 `-32602`。

  `permissions` 只允许出现在 `schema_version = 2`；v1 manifest 写了它**整包拒绝**，不降权
  静默装上——一个需要写权限才能工作的前端，不该以零权限的形态被装上去。同理，未知词、
  重复词、超过 16 项都是整包拒绝而非忽略。
- **`plugins_manage` 是四个里最重的：声明它等于声明「我可以代表用户安装任意受管包」。**
  安装界面因此必须显示包来源与它将获得的 permissions；`maildctl frontend list` 会把每个
  前端的 permissions 单独列出来。
- 两个保留 ID `builtin-egui` / `builtin-repair` 不允许被包占用，也不发放前端身份凭证。
  原生修复作为本机用户维护入口，权限不经前端声明扩大；Tauri 只能导航到它。
- 默认 Tauri 前端 `com.maild.ui` 的 manifest 在 `crates/maild-ui/package/frontend.toml`
  （[ADR-0031](../decisions/0031-tauri-frontend-shell-and-contribution-registry.md) 决策 6）：
  schema 2、全部路由、五个授权词。开发期 `lifecycle = "independent"`（自己起进程），
  `maildctl plugin install --frontend crates/maild-ui/package` 装好后壳才认得出自己；没装时它
  显示「尚未注册为前端包」而**不会**退成 `user`。M4.5-09-C 改为 `on_demand` + `.app`，id 与授权声明不变。
- frontend manifest 不接受 `supervised`；通用进程使用下方独立的 external manifest。逐插件
  凭据和细粒度 API capability enforcement 已按 ADR-0017/0018 落地（route 翻译表见上），
  当前 manifest 仍 fail-closed 拒绝未定义值。

## API

第三方前端先调用 `listFrontends` 确认自身已被发现，再在启动/聚焦时调用
`claimNextOpenIntent { frontend: "org.example.reader" }`。Menu 或其他同一信任域 consumer 可
调用 `openFrontend { frontend?, intent }`；省略 `frontend` 使用默认值，显式传
`builtin-repair` 并使用 `background` 是稳定恢复入口。

所有调用仍经过现有 Unix socket / Windows named pipe / loopback HTTP Bearer 边界。前端从
平台默认配置发现 endpoint，不应把 token 或邮件身份放入 argv、环境变量或 URL。

## 通用外部进程 package v1

非前端的自动化、桥接器或 TUI package 安装到 `maild paths` 输出的
`plugins/<id>/plugin.toml`。Agent 最多检查 32 个 package，沿用同样的 owner、mode、symlink、
64 KiB manifest 和相对路径约束：

```toml
schema_version = 1
protocol_version = 1
id = "org.example.automation"
name = "Example Automation"
version = "1.0.0"
lifecycle = "supervised"       # supervised | independent
capabilities = ["events_stream", "messages_read", "frontend_open"]
executable = "bin/automation"  # 只有 supervised 必填
ui_executable = "bin/manage"    # 可选；由插件页打开的原生管理界面
```

可声明的 capability 为 `events_stream`、`messages_read`、`messages_send`、`settings_read`、
`settings_write`、`frontend_open`、`notifications_read`、`transport_provider`，以及
ADR-0030 新增的 `mailbox_write`。经 ADR-0017/0018/0030 它们是真实授权表：
认证后的插件主体只能调用声明覆盖的方法（如发信需 `messages_send`、自动归档需
`mailbox_write`），未声明即 `-32016` 并计入 `denied_calls` 观测字段。`messages_read` 自
ADR-0032 起同时覆盖 `readAttachment`——附件字节属于同一封信的同一份内容，能读完整 HTML 原文与
附件元数据的主体读它的字节不构成新的权限面；`getReadingPolicy` / `setReadingPolicy` 分别归
`settings_read` / `settings_write`。

`accounts_manage` / `addresses_manage` / `plugins_manage` **不在 external 词表内**，写了直接
拒包：后台插件没有理由改账户配置，更没有理由装下一个插件——**禁止自我繁殖**是这里唯一重要
的性质。`revealPluginToken` 对任何插件/前端主体永久拒绝（持证者不得铸证）。服务端推送同受
管控——无 `messages_read` 的主体不收邮件事件。
对未认证连接（User 主体）的信任域边界不变：同一用户下的任意进程仍可以用户身份全量访问。
**这也意味着上面这张表挡不住一个恶意包**——它不认证就是 `user`。它挡的是 bug、是权限蔓延、
是「装了才发现它能干什么」。M5 的进程沙箱之前没有更强的东西可给，把它说成沙箱只会让人依赖
一个不存在的保证。

`supervised` package 只有被 `maildctl plugin enable <id>` 启用（或配置 allowlist 预置）时才
执行；enable/disable 即时生效并持久化到 `plugin_state.toml`（仅 Agent 写入、未知版本
fail-closed）。安装/升级/移除/回滚走 ADR-0019 事务 RPC（见上文 frontend 一节），升级后
Agent 会自动以新包重启子进程，每次 spawn 轮换新凭证。子进程收到空环境中的
`MAILD_CONFIG=<绝对路径>` 与
`MAILD_PLUGIN_TOKEN=<凭证>`（ADR-0017），以及
`MAILD_PLUGIN_DATA_DIR=<绝对路径>`（按受检插件 ID 隔离的 0700 可变数据目录）；stdio 关闭，工作目录固定为 package；它自行用共享
client 连接 API，并应先以该凭证调用 `authenticate`（见下节）。正常 `exit 0` 表示主动停止且不
重启，非零退出按 2s 起、60s 封顶的指数退避，连续重启默认最多 8 次；存活 60s 后重置连续
失败预算——每次重新 spawn 都会轮换出全新凭证。身份缝停摆时 supervised 插件拒绝启动
（状态 `blocked`）。Agent LIFO teardown 会先停止所有 supervised 子进程，并以总计 2s 为上限等待。
supervised executable 必须保持前台运行，不得 daemonize、double-fork 或把所有权移交给脱离
监督的后代。自 M4-04（ADR-0020）起该契约有执行机制：Unix 上子进程自为进程组长，任何退出路径
（停止、disable、升级重启、remove、Agent 停机，含正常 exit 0）都会对整组 SIGTERM→SIGKILL 清扫，
父进程退出后遗留的后台组员一并终止；Windows 上子进程进 Job Object kill-on-close，后代无法脱离。
主动 setsid 逃逸者已离开进程组、不可探测，仍是文档化残余风险——清扫后组内仍有存活成员的病态
情况会以清单状态 `leaked` 与 stderr 告警可见。插件与 Agent
同属当前用户信任域，安装前仍需把它当作可以读取该用户邮件与配置的本地程序审查。

package 目录是可升级/回滚/移除的不可变交付物；数据库、缓存等运行时状态必须写
`MAILD_PLUGIN_DATA_DIR`。package remove 不删除该目录，也不授权插件或 Agent 删除任何远端
资源。插件自己的秘密应进入平台凭据库，不能写入 data 目录、manifest、argv 或日志。
GUI 和 CLI staging 会拒绝符号链接、特殊文件、过深或过大的目录树；macOS 上会把
`com.apple.quarantine` 从源目录和文件复制到受管 package，安装事务不会移除 Gatekeeper
隔离标记。系统拒绝启动未经信任的下载插件时，Maild 不会绕过该检查。

`ui_executable` 对两种 lifecycle 都是可选项，与 supervised provider 进程相互独立。用户在
Maild 插件页点击“管理”后，Agent 重新校验该相对路径仍位于 package 内且不是符号链接，再以
空环境启动；仅注入 `MAILD_CONFIG` 与同一 `MAILD_PLUGIN_DATA_DIR`，不注入插件 token、API
token 或邮箱内容。每个插件同时最多一个管理进程；自然退出后可重新打开，package 升级、移除、
重扫发现 package 消失或 Agent 停机时会终止旧管理进程及其同一 containment 内的后代。该回收
与插件是否启用无关；打开管理界面也不会隐式启用插件。

`independent` package 的业务进程只登记，永不由 Agent 启动或停止。`listExternalPlugins` 返回每项的
`lifecycle`、capability、是否 enable、当前 `state` 与 `restart_count` 以及 `has_identity`
和 `has_ui` 观测位；不返回 stderr、环境、邮件内容或任意插件诊断文本。

## 插件身份与认证（ADR-0017）

合作插件可以获得可归因、可吊销的身份：连接级主体 `plugin:<id>` / `frontend:<id>`，
未认证连接恒为 `user`。这是契约性边界——拒绝认证的进程与普通用户工具不可区分；
对同用户恶意进程的隔离属 M5 沙箱。

- **认证**：以凭证调用 `authenticate {token}`，必须是连接上第一个请求；成功后本连接
  主体不可变。无效或已吊销凭证返回 `-32014` 且连接立即关闭；stdio/HTTP 来源返回
  `-32015`。客户端必须用长连接（`StreamConnection::authenticate`）；每次调用新开
  连接的便捷客户端无法持有插件主体。
- **凭证获取**：supervised 由 Agent 每 spawn 注入 `MAILD_PLUGIN_TOKEN`；independent
  与 on_demand frontend 由用户执行 `maildctl plugin-token <id>`（frontend 加
  `--frontend`）一次性取用——reveal 即轮换，旧凭证对新连接立即失效。
- **注册表**：`plugin_auth.toml` 与活动配置同目录，只存 SHA-256 哈希，文件纪律与
  `accounts.toml` 相同（仅 Agent 写入、0600、原子发布、未知版本 fail-closed）。
  注册表损坏时身份缝整体停摆：supervised 拒绝启动、authenticate 返回 `-32015`，
  其余能力照常。

## transport provider 契约（ADR-0021）

声明 `transport_provider` 的 external 插件可以把受管账户的收信/发信整体接管：
用户在账户配置中把某方向的 `kind` 设为 `"plugin"` 并点名本插件 id（惰性绑定，
安装顺序不耦合）。插件用长连接认证后调用 `providerRegister {}` 注册当前连接，
守护进程即可在该连接上反向调用：

- `transport/probe {account, address, direction}`：端到端可用性检查；创建/更新账户时的
  强制探测也走这里（draft 阶段 `account` 为空串）。应答 `{ok, stage: "provider",
  detail?}`。
- `transport/send {account, account_address, from, to, subject, body}`：发送一封信。
  `account_address` 是 profile 的规范地址；`from` 由内核固定为 profile.address，或由
  M4-06 地址层验证为该账户的 active 别名。业务失败以
  error 对象应答，内核映射 `-32020`（message
  有界脱敏后透出）。
- `transport/receive {account, address, since_uid, max}`：返回新消息批次。`address` 是
  profile 的规范地址；**uid 必须是同一
  账户上严格单调递增的十进制数字符串**（IMAP UID 心智模型）；游标是库内最大已存
  uid，由守护进程计算并回传。消息进入与内置 IMAP 相同的落库/过滤/OTP/通知链路；
  (account, uid) 唯一索引保证重复投递幂等。消息可带 additive `recipient` 字符串；缺省
  为空，供 M4-06 地址溯源展示。响应必须落在 1 MiB 单帧内。

预算：probe 10s、send 30s、receive 60s；超时应答按迟到丢弃，调用方得 `-32019`。
未注册或断线时委托方向快速失败 `-32018`，**绝不回退内置 transport**。

凭据红线：委托方向的账户没有 maild 凭据条目，三类载荷都不含任何账户秘密——
provider 插件的 API key 等完全在自己的域内保管（同用户可读，尽力而为）。
邮件正文会出现在插件进程内存里，安装前请把它当作可以读取该用户邮件的程序审查。
切换账户的 transport 类型可能更换 uid 空间；推荐改配置走「移除后重加」流程。
