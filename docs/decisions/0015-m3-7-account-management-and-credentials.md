# ADR-0015：M3.7 账号管理、凭据与动态生命周期

- 状态：已接受，等待实现
- 日期：2026-08-21
- 适用范围：M3.7 macOS 首版账号管理；Windows 凭据 provider 与 OAuth2 延后

## 背景

当前账户只能通过主配置中的 `[[accounts]]` 装配。用户必须知道 IMAP 字段、把密码写入
TOML，并在修改后重启 Agent；桌面端和 `listAccounts` 只能查看连接状态。SMTP 又是全局单
实例，无法可靠表达“从哪个账户发信”。这条路径适合开发和兼容旧配置，不是可交付的账户体验。

M3.6 已完成 headless Agent、按需前端、Menu、通知点击和外部进程骨架。下一阶段按产品优先级
调整为 M3.7 账号管理闭环，原定插件身份、安装事务和 containment 顺延。此前延后的 credential
seam 在本阶段只恢复满足账号管理所需的最小范围，不连带恢复 alias、disposable、transfer、DNS
或平台化邮件 provider。

## 目标

用户安装 `Maild.app` 后，不编辑配置文件即可：

1. 添加、测试和立即启用 IMAP 账户；
2. 可选配置 SMTP，并从指定账户发信；
3. 查看连接、认证和重连状态；
4. 更新密码、修改非秘密资料、停用、重新启用或移除账户；
5. 重启 Agent 后保留配置，密码不落入 TOML、SQLite、日志或 API 响应；
6. 移除账户时默认保留已落库邮件，任何历史邮件删除另做破坏性功能。

## 不在本阶段

- Gmail/Microsoft OAuth2、Exchange、JMAP、Gmail API；
- 自动读取 Apple Mail 或系统“互联网账户”；
- Windows Credential Manager 和 Windows 真机账号管理验收；
- 自动改写或迁移主配置中的旧 `[[accounts]]`；
- HTML、附件、草稿持久化、已读状态和全文搜索；
- alias、disposable、transfer、DNS、自定义域和外部 transport provider；
- 第三方 frontend 的逐进程身份、方法级授权或 credential capability 下放；
- 删除账户时顺带删除本地邮件。

## 总体结构

```text
内置 egui / maildctl
         │ typed account RPC
         ▼
Agent AccountManager
   ├── AccountCatalog ───── accounts.toml（仅非秘密配置）
   ├── CredentialStore ─── macOS Keychain（仅秘密）
   ├── Incoming registry ─ builtin-imap
   ├── Outgoing registry ─ builtin-smtp
   └── Runtime reconciler ─ 每账户 realm / Receive / Transport
```

前端只提交意图和短生命周期秘密，不直接访问配置目录、Keychain、IMAP 或 SMTP。Agent 是账户
目录、凭据访问和运行时生命周期的唯一写入者。

## 决策

### 1. 独立的受管账户目录

新增 `maild-paths::AppPaths.accounts_file`，平台安装固定为配置目录下的 `accounts.toml`。在
macOS Preview/正式 Agent 中，它与 `maild.toml` 同属
`~/Library/Application Support/maild/`。该文件：

- `format_version = 1`；
- 仅由 Agent 写入；桌面和 Menu 不直接打开或改写；
- owner 必须是当前用户，普通文件、单硬链接、无符号链接，Unix mode 为 0600；
- 父目录和所有祖先沿用现有 managed-path 私有边界；
- 使用同目录临时文件、文件同步、原子 rename 和父目录同步发布；
- 未知 future `format_version` 在任何 Keychain 或网络副作用前 fail-closed；
- 首次保存账户时才创建，空安装不制造额外状态。

账户管理写方法只在 Agent 使用平台默认配置且 `accounts_file` 位于受管配置根时开放。显式
`--config`、`MAILD_CONFIG` 或开发态 bare daemon 继续使用旧 `[[accounts]]`，不隐式选择新的
写入位置。测试通过可注入 `AppPaths` 覆盖目录。

### 2. 文件模型

顶层为 `format_version` 和 `[[profiles]]`。每个 profile 固定以下字段：

| 字段 | 语义 |
|---|---|
| `id` | `acct_` 加 26 位小写 base32 随机标识；创建后不可修改 |
| `revision` | 从 1 开始，每次成功修改递增，用于乐观并发控制 |
| `display_name` | 用户可修改的展示名称 |
| `address` | 默认发件地址和账户展示地址 |
| `enabled` | 是否挂载收信运行时 |
| `archived_at` | 非空表示已移除；不挂载且不得保留有效凭据 |
| `incoming_credential_revision` | 当前 IMAP Keychain item revision |
| `outgoing_credential_revision` | 独立 SMTP 密码的 Keychain item revision；复用收件密码时为空 |

`incoming` 固定包含 `kind=imap`、server、port、security、username、mailbox、idle 和
poll_secs；`outgoing` 可省略，存在时固定包含 `kind=smtp`、server、port、security、username
和 `credential= incoming|separate`。preset 只在前端生成草稿，不进入运行时 schema；运行时始终
保存已解析的显式 endpoint，避免预设更新静默改变现有账户。

首版上限：最多 64 个未归档账户；display name 128 UTF-8 字节；地址和用户名 320 字节；host
253 字节；mailbox 255 字节；秘密 4096 字节；port 为 1..=65535。受管账户只允许
`implicit` 或 `starttls`，不允许明文或 `accept_invalid_certs`。明文和自签开关只保留给旧配置与
hermetic 测试。

### 3. 稳定身份与历史邮件

消息、事件、通知打开 intent 和发送均继续使用 `account` 字符串，但对受管账户它等于不可变
`id`，不是 display name。前端通过 `listAccountProfiles` 把 ID 映射为可读名称。

移除账户是软归档：先停用运行时，再删除凭据，保留 profile 的非秘密名称和地址供历史邮件显示。
重新启用已归档账户必须重新输入凭据、重新 probe 并产生新的 credential revision。首版不提供
“同时清空本地邮件”。

### 4. 旧 `[[accounts]]` 兼容

旧账户继续装配并出现在 profile 列表中，标记 `source=legacy`、`editable=false`，其 ID 沿用原
`account`。旧配置和受管 profile ID 冲突时，Agent 在启动任何账户网络任务前配置停止，不猜测
覆盖顺序。

本阶段不自动抽取旧明文密码，也不重写主 TOML。后续迁移必须是独立、显式、有备份且保留注释的
事务，不能夹在普通启动或首次打开 UI 中。

### 5. CredentialStore 插件缝

新增 `CredentialStore` spine service 和 provider registry：

- macOS 生产 provider：Keychain Services；
- hermetic 测试 provider：内存实现和可注入失败点；
- Windows provider：返回 unsupported，等待独立阶段。

macOS 使用 generic password item：

- service 固定为 `maild.account.credentials.v1`，不随开发或正式 bundle ID 改名；
- account 固定为 `<account-id>/<incoming|outgoing>/<revision>`；
- label 仅含 Maild 与账户展示名，不含服务器错误或秘密；
- 通过 Security framework 的 add/copy/update/delete API，不 shell out `security` 命令；
- 错误对外只暴露稳定分类和 OSStatus，不返回 Keychain localized description/userInfo。

凭据只在认证前从 Keychain 取入有上限、可清零的短生命周期缓冲；不进入长期 `ReceiveConfig`、
`TransportConfig`、Debug、日志、ViewState 或通知。协议库内部可能为一次认证临时复制字节，因此
安全承诺是“不持久化、不长期驻留、尽快清零”，不是不现实的“进程内从不出现明文”。

### 6. 跨文件事务与崩溃恢复

Keychain 与账户文件不能组成单一原子事务，使用 revision staging 收敛故障窗口：

1. 校验字段并完成需要的网络 probe；
2. 以新 revision 写入 Keychain item；
3. 原子发布引用新 revision 的 `accounts.toml`；
4. 启动或切换运行时；
5. 发布成功后删除旧 revision。

若文件发布失败，删除本次 staged item 并保持旧文件/旧运行时。若运行时构造在发布后发生内部
错误，恢复上一版文件和凭据引用；普通网络掉线不回滚已验证配置，而是进入既有重连状态。

Agent 启动并成功解析账户目录后，删除本 service 下未被任何当前或归档 profile 引用的旧
revision；解析失败时绝不清理 Keychain。归档记录不引用有效 revision，启动时会收敛上次删除
中断后遗留的 item。

### 7. 动态 AccountManager

`Accounts` 从不可变 `Vec` 演进为由 `AccountManager` 串行维护的快照服务。每个账户仍使用独立
realm 和 Receive 隔离：

- create/enable：分配 realm、挂载 Receive，并发布 connecting 状态；
- update 非连接字段：只更新 profile 快照；
- update endpoint/username/secret：停止旧 realm 后挂载新 realm，避免两个 IDLE 连接并存；
- disable/archive：先停止该账户 realm，再发布最终状态；
- shutdown：先停止 API 接受写请求，再由 manager 逆序释放所有账户 runtime。

更新时若新 runtime 的本地构造失败，恢复旧 runtime；上线后的 DNS、TCP、TLS 或认证失败使用现有
reconnect 机制，不阻塞整个 Agent。账户状态 detail 必须控制字符清理并限制长度，不回显服务器
原文、用户名或秘密。

provider registry 首版只注册 `imap` 和 `smtp`。该注册点是以后 JMAP、Gmail API 或外部
transport provider 的扩展缝，但本阶段不设计第三方进程获得凭据的协议。

### 8. 收信、探测与发信

IMAP probe 使用总 deadline，依次完成 TCP、TLS、认证、SELECT mailbox、能力检查和 LOGOUT；SMTP
probe 完成 TCP、TLS、EHLO、AUTH 和 QUIT，绝不发出 MAIL/RCPT/DATA。结果只返回稳定 stage：
`dns`、`connect`、`tls`、`auth`、`mailbox`、`capability`、`ready`。

SMTP 增加 `implicit` TLS；旧全局 Transport 与 `send` 保持兼容。新增 `sendFromAccount` 从 profile
固定选择发件地址、SMTP endpoint 和凭据，不允许请求任意伪造 From。恰有一个启用且可发信的账户
时 compose 自动选择；多账户必须显式选择；无 outgoing 的账户只收不发。

probe 与账户 mutation 在 Agent 内全局最多 4 个并发；每项有端到端 deadline，shutdown 可以
抢占。probe 失败是业务结果，不把健康 API session 判为断线。

### 9. JSON-RPC v1 additive 方法

保留既有 `status`、`listAccounts` 和 `send` 的字段与语义。新增：

| 方法 | 参数 | 结果 |
|---|---|---|
| `listAccountProfiles` | `{include_archived?: bool}` | 非秘密 profile、credential state 和运行状态 |
| `probeAccount` | `{draft, credentials}` | incoming/outgoing 分阶段结果，不保存 |
| `createAccount` | `{draft, credentials}` | `{profile, persisted: true, active}` |
| `updateAccount` | `{id, expected_revision, patch, credentials?}` | 更新后的权威 profile |
| `setAccountEnabled` | `{id, expected_revision, enabled}` | 更新后的权威 profile |
| `removeAccount` | `{id, expected_revision}` | `{archived: true, credentials_deleted, messages_retained: true}` |
| `sendFromAccount` | `{account, to, subject?, body?}` | `{ok: true}` |

秘密字段只写不读；任何响应、通知和 JSON 错误 data 都不得包含它们。client 对 profile 数量、字段
长度、枚举、revision 和响应总字节做与服务端一致的入站校验，未来未知 provider/status 字符串按
unknown 展示而不是使整个账户页解码失败。

新增错误码：

| code | 名称 | 含义 |
|---|---|---|
| `-32010` | `ACCOUNT_NOT_FOUND` | ID 不存在或已不可操作 |
| `-32011` | `ACCOUNT_CONFLICT` | revision、ID 或 legacy 冲突 |
| `-32012` | `ACCOUNT_PROBE_FAILED` | create/update 强制 probe 未通过 |
| `-32013` | `CREDENTIAL_UNAVAILABLE` | Keychain 不支持、锁定或访问失败 |

服务未装配继续使用 `SERVICE_UNAVAILABLE (-32001)`；字段错误使用 `INVALID_PARAMS (-32602)`。

`listAccountProfiles` 可走既有传输。包含秘密的 `probeAccount`、`createAccount` 和携带新秘密的
`updateAccount` 首版只接受同用户 Unix socket；Windows provider 落地后允许当前用户 named pipe。
HTTP 一律拒绝秘密 mutation。dispatch 必须携带可信 `RequestOrigin`，不能根据客户端自报字段判断。

当前本地 API 仍以同一 OS 用户为信任域，不能区分内置和第三方 frontend。M3.7 只在内置 UI
展示账户写入口；正式允许第三方 frontend 管理账户前，必须先完成插件身份和方法级 capability。

### 10. CLI 与桌面体验

`maildctl` 新增 `account list|add|test|edit|enable|disable|remove`。`add`/`edit` 的交互式秘密输入
必须关闭终端回显；非交互模式只允许从 stdin 读取秘密，不接受命令行参数或环境变量。JSON 输出
同样不回显秘密。

桌面空邮箱页提供“添加邮箱账户”；Agent 未运行时先引导用户到后台页显式启用。账户中心包含：

1. 邮箱地址与展示名称；
2. iCloud、Gmail App Password 或手动 IMAP/SMTP；
3. 收件和可选发件探测；
4. 明确保存动作，成功后立即连接。

密码框不回填，关闭或成功提交后清零草稿。状态至少区分 connecting、online、reconnecting、
disabled、needs_credentials 和 error。更新、停用、移除都需要单次 in-flight、generation 防旧结果
覆盖；移除确认固定说明“本地邮件会保留”。

首批 preset：

- iCloud：IMAP `imap.mail.me.com:993` implicit；SMTP `smtp.mail.me.com:587`
  STARTTLS；使用 App 专用密码；
- Gmail App Password：IMAP `imap.gmail.com:993` implicit；SMTP
  `smtp.gmail.com:587` STARTTLS；明确提示 OAuth2 更推荐，组织账户可能禁用 App Password；
- Manual：用户填写安全 IMAP/SMTP endpoint。

不提供看似可用但实际需要 OAuth2 的 Outlook/Exchange preset。preset 文案必须链接 provider 官方
帮助页；链接打开由桌面发起，不把邮箱地址、错误或秘密放入 URL。

## 实施切片

### M3.7-01：headless 账户闭环

范围：`maild-paths` 账户路径、AccountCatalog、CredentialStore/macOS Keychain、动态
AccountManager、IMAP/SMTP probe、SMTP implicit TLS、per-account send、additive API、typed
client 和 `maildctl account`。

独立验收：不编辑 TOML，用 CLI 添加一个账户；Agent 不重启即进入 online；重启后继续收信；从
同一账户发信；disable/enable/remove 行为与凭据、历史邮件契约一致。

### M3.7-02：桌面账户中心

范围：空状态入口、账户列表、向导、编辑/停用/移除、iCloud/Gmail/manual preset、compose 账户
选择、错误状态和离开确认。

独立验收：安装 Preview DMG 后，只通过点击完成添加、收信、OTP 通知、通知点击打开邮件和发信；
全程不打开配置文件或终端。

### M3.7-03：恢复、文档与真实服务验收

范围：Keychain 丢失/锁定、密码撤销、账户文件损坏、磁盘满和中断事务恢复；协议、运维、README、
测试记录与 Preview DMG 更新。

独立验收：一个真实 iCloud 或 Gmail App Password 账户完成 add→receive→OTP notification→open
message→send→change password→disable/enable→remove；凭据不进入仓库、日志或测试产物。

## 主要文件目标

- `crates/maild-paths/src/lib.rs`：`accounts_file`；
- `crates/maild/src/accounts.rs`：动态 manager、legacy 合并和 runtime reconcile；
- `crates/maild/src/credential.rs`、`credential/macos.rs`：credential seam；
- `crates/maild/src/account_catalog.rs`：版本化文件和事务；
- `crates/maild/src/receive.rs`、`transport.rs`：短生命周期凭据、probe、SMTP implicit；
- `crates/maild/src/api.rs`：RequestOrigin 与 additive RPC；
- `crates/maild-client/src/lib.rs`：DTO、校验和 typed calls；
- `crates/maild/src/bin/maildctl.rs`：账户子命令和无回显输入；
- `crates/maild-app`：profile 快照、重连 reconcile 和账户动作桥；
- `crates/maild-desktop`：账户中心、wizard、preset 和 compose 选择；
- `docs/protocol/README.md`、`docs/operations/README.md`、`docs/testing/README.md`：最终契约与证据。

该范围超过 8 个文件并新增两个 spine service，执行者应按三个切片分别提交和复核，不能在一个巨型
提交中同时改 core、UI 和真实服务证据。

## 测试矩阵

- catalog：权限、symlink/hardlink、路径祖先、future version、no-clobber、fsync、损坏输入；
- credential：add/get/update/delete、duplicate、missing、locked、错误清洗、orphan revision 收敛；
- transaction：Keychain/File/realm 每个阶段失败和进程重启恢复；
- account manager：create/update/disable/enable/archive、最多 64、revision conflict、legacy 冲突、
  无重复 IDLE 和 LIFO shutdown；
- probe：DNS/TCP/TLS/auth/mailbox/capability 分类、deadline、取消、SMTP 不发送邮件；
- transport：implicit TLS、STARTTLS、错误证书、注入边界和 per-account credential；
- API/client：参数白名单、字段上限、错误码、秘密不回显、HTTP mutation 拒绝、旧方法快照不变；
- CLI：TTY/非 TTY、无回显、stdin、JSON、退出码、秘密不进 argv/env/stderr；
- desktop：空状态、preset、失败保留、generation、dirty close、密码清零、多账户 compose；
- e2e：动态添加后收信/通知/发信，Agent 重启，删除后历史邮件保留；
- gates：workspace tests、native clippy、Windows GNU check/clippy、macOS bundle fixtures 与 Preview
  DMG 实际安装验证。

## 外部依赖与门禁

M3.7-01 新增的直接 Rust 依赖固定为：macOS target 使用当前 lockfile 已存在的
`security-framework = "3.7.0"` 调用 Keychain Services；跨平台使用 `zeroize = "1.9.0"` 管理短
生命周期秘密缓冲。不得用 shell 调用 `security`。Unix CLI 的无回显输入优先复用现有 `libc`
`tcgetattr`/`tcsetattr`，不为首版引入额外 TTY 依赖；Windows 账户写命令在 Credential Manager
provider 落地前明确返回 unsupported。

仓库实现不需要 provider API key。真实验收需要用户自有的一个 App 专用邮件密码，只在本机 UI
输入，不进入 issue、终端记录或仓库。OAuth client ID 不属于本阶段。

最脆弱前提是内嵌 Agent 在 ad-hoc Preview 更新和未来 Developer ID 签名后能稳定访问自己创建的
Keychain item。仓库测试只能证明 API 和 fail-closed；Preview 与正式签名都必须真机验证首次保存、
重启复用、升级复用、删除和 Keychain 锁定。若访问失效，profile 保留并进入
`needs_credentials`，要求用户重新输入；不得回退 TOML 明文。

官方实现与 preset 资料：

- Apple [Using the Keychain to Manage User Secrets](https://developer.apple.com/documentation/security/using-the-keychain-to-manage-user-secrets)；
- Apple [iCloud Mail server settings](https://support.apple.com/zh-cn/102525)；
- Google [IMAP, POP, and SMTP](https://developers.google.com/workspace/gmail/imap/imap-smtp)；
- Google [Sign in with app passwords](https://support.google.com/mail/answer/185833)。

执行者在编码前应重新核对 provider 页面；若服务端地址、认证政策或 App Password 可用性发生变化，
只更新新建向导的 preset 和提示，不静默改写已保存 profile。

## 回滚

- M3.7 代码回滚后，旧 daemon 忽略独立 `accounts.toml`，旧 `[[accounts]]` 行为不变；
- 回滚不得自动删除 accounts file 或 Keychain item；重新升级可继续引用；
- 若 M3.7 需要永久撤回，提供显式 maintenance 命令列出将删除的 Maild-owned Keychain item，用户
  确认后删除；配置和本地邮件默认保留；
- 任何自动清理只允许操作固定 service 与被 catalog/tombstone 证明归属 Maild 的 revision。

## 文档与验收顺序

每个切片完成时同步协议、运维、测试基线和路线图。最终交接先读本 ADR，再读
[`../protocol/README.md`](../protocol/README.md)、[`../operations/README.md`](../operations/README.md)
与 [`../testing/README.md`](../testing/README.md)。实现完成但未经过真实 Keychain 与邮箱验收时，
只能标记“仓库实现完成”，不能声称 M3.7 对用户可用。
