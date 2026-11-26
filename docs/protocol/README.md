# maild JSON-RPC v1

本文档固定当前 headless API 的方法、帧格式、传输和 CLI 行为。协议使用 JSON-RPC
2.0；stdio、Unix domain socket 与 Windows named pipe 共享同一套行协议，HTTP
共享同一套方法与错误对象。

## 基本约定

- 请求必须是单个 JSON 对象，不支持 batch。
- `jsonrpc` 必须为 `"2.0"`，`method` 必须为非空字符串。
- `params` 可省略；出现时必须是对象或数组。当前方法均使用对象参数。
- `id` 可为字符串、数字或 `null`，其 JSON 编码最多 4 KiB；类型或大小无效时返回
  `-32600` 且响应 `id` 为 `null`。省略或设为 `null` 表示通知：服务端执行方法但不响应。
- 成功响应只含 `result`，失败响应只含 `error`；两者不能同时出现。
- 单个请求帧和单个响应均最多 1 MiB。响应超过上限时返回 `-32603`，不会截断。
- 字符串均为 UTF-8。时间戳是 Unix 秒。

请求：

```json
{"jsonrpc":"2.0","method":"status","params":{},"id":1}
```

成功响应：

```json
{"jsonrpc":"2.0","result":{"version":"0.1.0","accounts":[],"message_count":0},"id":1}
```

失败响应：

```json
{"jsonrpc":"2.0","error":{"code":-32001,"message":"服务未装配: store（检查 [plugins.store] 配置）"},"id":1}
```

## 方法表

| 方法 | `params` | `result` | 所需插件 |
|---|---|---|---|
| `status` | `{}` | `{version, accounts, message_count, runtime}` | 无 |
| `syncMessages` | `{account?}` | `{triggered, online_count}` | 账户（ADR-0041） |
| `fetchServerMessage` | `{account, folder, uid}` | `{message, remote_content}` | `store` + 该账户在线（本地已缓存时不需要） |
| `discoverEmailProvider` | `{input, allow_network?}` | `ProviderDiscovery`；`input == ""` 时 `{presets}` | 无（ADR-0042） |
| `listEmailPresets` | `{}` | `{presets}` | 无 |
| `listAccounts` | `{}` | `{accounts}` | 无 |
| `listFolders` | `{account?}` | `{folders}` | `store` |
| `listMessageSummaries` | `{account?, folder?, role?, seen?, flagged?, limit?, offset?}` | `{messages, total}` | `store` |
| `listMessages` | `{limit?, offset?}` | `{messages, total}` | `store` |
| `getMessage` | `{account, uid, folder?}` | `{message, remote_content}` | `store` |
| `readAttachment` | `{account, folder, uid, part_index, offset?, length?}` | `{attachment, offset, length, data, eof}` | `store` |
| `getReadingPolicy` | `{}` | `{policy, persisted}` | 无（配置文件方式启动） |
| `setReadingPolicy` | `{remote_content?, trust?, untrust?}` | `{policy, persisted}` | 无（配置文件方式启动） |
| `getLocalePreference` | `{}` | `{preference, effective, persisted}` | 无（配置文件方式启动） |
| `setLocalePreference` | `{mode, languages?}` | `{preference, effective, persisted}` | 无（配置文件方式启动） |
| `searchMessages` | `{query?, account?, folder?, from?, to?, subject?, since?, until?, hasAttachments?, seen?, flagged?, limit?, offset?, scope?}` | `{messages, total, scope, truncated?}` | `store`（`scope=server` 另需该账户在线） |
| `searchContacts` | `{account, query?, limit?}` | `{contacts: [{address, name?}]}` | `store` |
| `getThread` | `{account, uid?, threadId?, folder?, limit?, offset?}` | `{threadId, messages, total}` | `store` |
| `getSearchIndexStatus` | `{}` | `{pending, done}` | `store` |
| `markMessages` | `{account, folder, uids, flag, value}` | `{ok, queued, queued_uids, unchanged_uids, missing_uids}` | `store` |
| `moveMessages` | `{account, folder, uids, target}` | `{ok, queued, mode, queued_uids, unchanged_uids, missing_uids}` | `store` |
| `deleteMessages` | `{account, folder, uids}` | `{ok, queued, mode, queued_uids, unchanged_uids, missing_uids}` | `store` |
| `listPendingOps` | `{account?, include_settled?, limit?, before_id?, summary_only?}` | `{ops, has_more}` | `store` |
| `resolvePendingOp` | `{id, resolution, summary_only?}` | `{op}` | `store` |
| `send` | `{from, to, cc?, bcc?, subject?, body?, body_html?, in_reply_to?, references?, attachments?}` | `{ok, message_id}` | `transport` |
| `prepareReply` | `{account, uid, folder?, mode, locale?}` | `{draft}` | `store` |
| `listDrafts` | `{account?, limit?, offset?}` | `{drafts, total}` | `store` |
| `getDraft` | `{id}` | `{draft}` | `store` |
| `createDraft` | `{account, from?, to?, cc?, bcc?, subject?, body?, body_html?, in_reply_to?, references?, attachments?}` | `{draft}` | `store` |
| `updateDraft` | `{id, base_version, ...同上字段}` | `{draft}` | `store` |
| `deleteDraft` | `{id, base_version?}` | `{deleted, draft}` | `store` |
| `sendDraft` / `resolveDraftSend` | `{id, base_version}` | `{draft}`（含持久投递状态） | `store` / 账户管理 |
| `beginAttachmentUpload` | `{draft_id, filename, mime_type, size}` | `{upload:{upload_id,offset,size,chunk_bytes}}` | `store`，`drafts_write` |
| `writeAttachmentUpload` | `{upload_id, offset, data}`（base64） | `{upload}` | `store`，`drafts_write` |
| `finishAttachmentUpload` | `{upload_id, base_version}` | `{draft}` | `store`，`drafts_write` |
| `cancelAttachmentUpload` | `{upload_id}` | `{cancelled:true}` | `store`，`drafts_write` |
| `getSettings` | `{}` | `{rules, persisted}` | `rules` |
| `setSettings` | `{rules, persist?}` | `{rules, persisted}` | `rules` |
| `getNotificationStatus` | `{}` | 通知授权状态 | `notify` |
| `requestNotificationAuthorization` | `{}` | 通知授权状态 | `notify` |
| `claimNextOpenIntent` | `{frontend}` | `{intent}` | `frontend` |
| `listFrontends` | `{}` | `{default, frontends}` | `frontend` |
| `getPluginInstallRoot` | `{kind?: "plugin"\|"frontend"}` | `{root}`，仅本机原生复制器使用 | external / frontend，`plugins_manage` |
| `previewPluginPackage` | `{kind?, staging? , rollback_id?}`，后二者恰有一个 | `{kind,id,name,description?,l10n?,version,lifecycle,capabilities,permissions,has_ui}` | external / frontend，`plugins_manage` |
| `setDefaultFrontend` | `{id}` | `{default,persisted:true}` | frontend，`plugins_manage` |
| `stopManagedAgent` | `{}`；不接受 PID/路径 | `{stopping:true}`；已接受正常退出请求 | 仅同用户 Unix socket / named pipe 的 User；仅平台 Agent 装配 |
| `repairBuiltinFrontend` | `{}`；不接受路径/id | `{id,version,backup}`；backup 是 `.repair-backups` 下相对目录名 | 仅同用户 Unix socket / named pipe 的 User；需完整应用内固定副本 |
| `listExternalPlugins` | `{}` | `{plugins}` | `external` |
| `openExternalPluginUi` | `{id}` | `{id, launched, already_running}`；启动 package 声明的原生管理界面，已运行时不重复启动 | `external` |
| `openFrontend` | `{frontend?, intent}` | `{queued, activation_requested}` | `frontend` |
| `getAgentInfo` | `{}` | `{version, protocol_version, pid, started_at?, build_rev?, executable_path?}` | 无 |
| `authenticate` | `{token}` | `{}`；仅 Unix socket / named pipe 的连接首个请求 | 身份缝 |
| `whoami` | `{}` | `{principal: "user"\|"plugin:<id>"\|"frontend:<id>"}` | 无 |
| `revealPluginToken` | `{id, kind?: "plugin"\|"frontend"}` | `{token}`；即轮换，仅同用户 IPC | 身份缝 |
| `commitPluginInstall` | `{staging, kind?: "plugin"\|"frontend", expected_preview?}` | `{action: installed\|upgraded, id, from_version?, version}`；提交 `.incoming/<staging>`（ADR-0019） | external / frontend |
| `removePlugin` | `{id, kind?}` | `{removed: id}`；移入 `.removed` 后删除并刷新 | 同上 |
| `rollbackPlugin` | `{id, kind?, expected_preview?}` | `{id, version}`；当前版与回滚位互换，再执行一次切回 | 同上 |
| `setPluginEnabled` | `{id, enabled}` | `{id, enabled}`；写状态文件，supervised 即时启停 | external |
| `refreshPlugins` | `{}` | `{external: {spawned?, stopped?, restarted?, removed?}, frontend: {reloaded, detail?}}`；差异刷新不重启未变化的插件 | external / frontend |
| `listAccountProfiles` | `{include_archived?}` | `{profiles}` | 受管账户目录 |
| `probeAccount` | `{draft, credentials, id?, expected_revision?}` | `{incoming, outgoing?}` | 受管账户目录 |
| `createAccount` | `{draft, credentials}` | `{profile, persisted: true, active}` | 受管账户目录 |
| `updateAccount` | `{id, expected_revision, patch, credentials?}` | profile 视图 | 受管账户目录 |
| `setAccountEnabled` | `{id, expected_revision, enabled}` | profile 视图 | 受管账户目录 |
| `removeAccount` | `{id, expected_revision}` | `{archived, credentials_deleted, messages_retained}` | 受管账户目录 |
| `sendFromAccount` | `{account, to, cc?, bcc?, subject?, body?, body_html?, in_reply_to?, references?, attachments?, from_address?}` | `{ok, archived}`；可选地址须属于该账户且 active；outgoing 委托给插件时反向调用 `transport/send`（ADR-0021/0022） | 受管账户目录 |
| `listAddresses` | `{account?, include_inactive?, limit?, offset?}` | `{addresses, total}`；无分页键时兼容全量响应 | 受管账户目录 + store |
| `createAddress` | `{account, kind, address?, label?, expires_secs?}` | 地址视图；`kind` 为 `alias` 或 `disposable` | 受管账户目录 + store |
| `updateAddress` | `{id, state?, label?}` | 地址视图；`state` 只接受 `active` / `disabled` | 受管账户目录 + store |
| `removeAddress` | `{id}` | `{ok: true}`；历史消息的 recipient 文本保留 | 受管账户目录 + store |
| `providerRegister` | `{}` | `{}`；把当前连接登记为该插件的 transport provider 端点（latest-wins，断连自动注销）。仅限已认证插件主体且 manifest 声明 `transport_provider`（ADR-0021） | external |

### 守护进程 → 插件的反向请求（ADR-0021）

注册了 provider 的连接上，守护进程会以普通 JSON-RPC 请求帧（带自增 id）调用插件，
超时预算内未应答按 `-32019` 处理；插件必须以匹配 id 的 result/error 应答：

| 方法 | `params` | `result` | 预算 |
|---|---|---|---|
| `transport/probe` | `{account, address, direction: "incoming"\|"outgoing"}`；draft 探测阶段 `account` 为空串，`address` 仍是待绑定的规范邮箱地址 | `{ok, stage: "provider", detail?}` | 10s |
| `transport/send` | `{account, account_address, from, to, subject, body}`；`account_address` 是 profile 规范地址，`from` 可为它或已验证的 active 别名 | `{}` 或 error（映射 `-32020`） | 30s |
| `transport/receive` | `{account, address, since_uid, max}`；`address` 是 profile 规范地址，uid 为严格单调递增的十进制数字符串 | `{messages: [{uid, sender, recipient?, subject, body, received_at, date?, message_id?, cc?, reply_to?}]}`；`date` 起为 ADR-0025 可选键（`date` 是 RFC 5322 `Date` 头原文），缺省保持既有行为；单帧 ≤1 MiB，`max` 由内核设定 | 60s |

委托方向**绝不静默回退内置 transport**：provider 未注册/掉线 → `-32018`。凭据不跨缝：
委托方向的账户没有本目录凭据条目，载荷不含任何秘密。未注册 provider 的连接不会收到
任何反向请求——旧客户端/旧插件行为不变。

`address` / `account_address` 是 ADR-0023 的 additive 定位键：旧 provider 可以忽略，API 型
provider 不应从可能变化的 alias `from` 猜测远端 mailbox。

### 地址层（ADR-0022）

地址是受管账户的从属资源。地址视图为 `{id, account, address, kind, label, state,
created_at, expires_at?}`；`kind` 为 `alias|disposable`，`state` 为
`active|disabled|expired`。任何读取或发信使用都会把 `expires_at <= 当前时间` 的地址按
`expired` 处理；过期态不可重新启用。每个未归档账户最多 1024 条地址，地址字符串跨账户唯一。

`listAddresses` 默认只返回 active 地址；`include_inactive: true` 包含 disabled/expired。
`limit` 为 1..=1000，`offset` 为非负整数；只提供 `offset` 时默认页长 500。为兼容 ADR 冻结时
的首版调用，无 `limit`/`offset` 时返回当前过滤范围的全量列表；`total` 始终是分页前数量。

`createAddress` 的 alias 必须显式给出完整地址；disposable 省略 address 时由 daemon 在父账户域
生成 12 位小写 base32 local part，连续冲突最多重试 5 次。`expires_secs` 只用于 disposable，
缺省为永久。`updateAddress` 至少提供 state 或 label，`expired` 是派生态而非可写状态。

`sendFromAccount.from_address` 省略时仍使用 profile.address；提供时接受相同的 profile.address 或属于同一账户且当前
active 的别名地址。它只改变 SMTP 信封/From 或 provider 的 `transport/send.from`，凭据、预算、
per-account 规则和错误映射全部沿用父账户。四个地址 RPC 需要 `addresses_manage`
（[ADR-0030](../decisions/0030-frontend-authorization-closure.md) 决策 4），该词只能由前端
manifest 声明；未声明的主体得到 `-32016`。这些都是 additive 变化，不提升协议代数。

M3.7 账户方法（ADR-0015 §9）的完整契约：

- **profile 视图**只含非秘密字段：`{id, revision, display_name, address, enabled,
  archived_at?, source: managed|legacy, editable, incoming, outgoing?,
  credentials: {incoming, outgoing?}, runtime: {state, detail, detail_code?, server_detail?}}`。`credentials.*`
  取值 `saved | missing | unknown | shared | embedded`；`runtime.state` 取值
  `connecting | online | reconnecting | disabled | needs_credentials | error`。
  客户端必须把未知状态字符串按原文展示，不得让整个账户页解码失败。
  `runtime.detail_code` / `runtime.server_detail` 与账户状态对象同义（见下节），只在有离线原因时出现。
- **秘密只写不读**：`credentials` 形如 `{"incoming": "…", "outgoing": "…"}`，任何响应、
  通知与错误 `data` 都不回显。受管账户不允许明文（`security` 仅 `implicit|starttls`），
  也不存在自签开关；首版上限：64 个未归档账户、display name 128 字节、地址/用户名
  320 字节、host 253 字节、mailbox 255 字节、秘密 4096 字节、port 1..=65535。
- **来源门禁**：`probeAccount`、`createAccount` 与携带新秘密的 `updateAccount`
  首版只接受同用户 Unix socket 传输；stdio / HTTP / named pipe 一律返回
  `-32600`。来源由服务端传输层决定，客户端无法自报。纯资料编辑与
  enable/disable/remove/sendFromAccount 不携带秘密，走任意本地传输。
- **事务语义**：create/update 强制探测（失败返回 `-32012` 并在 `error.data`
  携带分阶段报告）；成功后凭据先写平台凭据库、再原子发布 `accounts.toml`、
  最后重挂 runtime；endpoint/username/secret 变化会先停旧 realm 再挂新 realm，
  不允许两个 IDLE 连接并存。remove 是软归档：删除凭据、保留非秘密资料，
  本地邮件一律保留。插件 transport 的 `(provider id, address)` 在所有未归档 profile 间唯一；
  address 仅规范化域名大小写，create/update 在同一 mutation lock 内、probe 与发布前检查，
  冲突返回 `-32011`。
- **legacy 兼容**：旧 `[[accounts]]` 以 `source=legacy, editable=false` 出现在
  列表中，ID 沿用原账户名；旧配置与受管 ID 冲突时 Agent 配置停止。

### `status` / `listAccounts`

`accounts` 是账户状态数组。每项格式为：

```json
{"account":"personal","state":"online","detail":""}
```

`state` 当前可能为 `connecting`、`online`、`offline` 或 `reconnecting`。未装配
accounts 或 store 时，`status` 分别返回空数组和 `message_count: 0`。

离线原因（ADR-0048 L6，additive）：`state = offline` 时，`detail` 仍是收信循环拼的中文整句
（日志同一句，老客户端照旧显示），另带两个可选键：

```json
{"account":"personal","state":"offline","detail":"IMAP 登录失败: no response: [AUTHENTICATIONFAILED] bad",
 "detail_code":"login_failed","server_detail":"no response: [AUTHENTICATIONFAILED] bad"}
```

- `detail_code`：稳定 snake_case，客户端按它用界面语言成文，**不要**解析 `detail`。首批取值：
  `credential_unavailable`（从凭据库取回秘密失败）、`credential_invalid`（秘密不是 UTF-8 文本）、
  `connect_failed`、`connect_timeout`、`tls_failed`、`tls_timeout`、`starttls_unsupported`、
  `greeting_invalid`、`login_failed`、`login_timeout`、`idle_failed`、`idle_timeout`、
  `sync_failed`（LIST / SELECT / FETCH / 对账 / 游标 / 写队列命令在连接层失败）、`poll_failed`、
  `provider_unavailable`、`provider_timeout`、`provider_failed`、`provider_response_invalid`，
  兜底 `internal`。新增取值不算破坏性变更：认不出的 code 显示通用文案加 `server_detail`；没有
  `detail_code`（老 daemon）时照旧显示 `detail`。
- `server_detail`：服务器或系统的原文（去掉 daemon 写的前缀，去控制字符、≤160 字节），作为数据原样
  展示、不翻译。原因全由 daemon 判断（超时、凭据库、本地库、装配缺失、provider 连接已断）时省略。
- 在线、连接中与重连中不带这两个键。

`status.runtime`（ADR-0041 决策 6，additive）：`rss_bytes`、`phys_footprint_bytes`（macOS
Activity Monitor「内存」口径；其他平台 `null`）、`store_db_bytes`、`store_wal_bytes`、
`stored_messages`、`stored_attachment_bytes`。拿不到的口径为 `null`；`maildctl status --json`
原样输出。

### `syncMessages` / `fetchServerMessage`（ADR-0041 决策 4）

- `syncMessages {account?}`：唤醒匹配账户的收信循环立即跑一轮增量同步（打断 IDLE、轮询间隔与
  断线退避）。只敲门、不等完成；`triggered` 是被敲醒的账户名，`online_count` 是其中此刻在线
  的个数。指定的账户不存在或未启用 → `-32602`。授权归 `messages_read`。
- `fetchServerMessage {account, folder, uid}`：`uid` 是十进制正整数字符串。本地已缓存直接返回；
  否则在该账户的收信连接上按 UID 拉回并幂等落库，返回形状与 `getMessage` 相同。服务器上也没有
  → `-32002`；账户离线/拉取失败 → `-32001`。授权归 `messages_read`（读的是 `searchMessages
  scope=server` 已能看到的同一批数据，落库只是缓存）。

**冷启动窗口与本地视图的关系**：账户的 `initial_sync_limit`（缺省 200，见
[运维手册](../operations/README.md#冷启动窗口initial_sync_limit)）决定首次同步只落库每个文件夹最新
N 封。`listMessageSummaries` / `searchMessages`（`scope` 缺省 `local`）只看本地已缓存的数据，因此
「本地 0 命中」不代表服务器上没有。客户端补历史的固定路径是：`searchMessages {account, folder,
query, scope: "server"}`（每次最多 200 个命中，`truncated` 标明是否截断，命中摘要不含正文）→ 对需要
的 UID 调 `fetchServerMessage` → 之后该封同样出现在本地列表与本地检索中。两者在该账户唯一的收信
连接上排队执行，并发请求过多时立即返回错误而非排长队；`message/received` 不会因按需拉取而推送。

### `discoverEmailProvider` / `listEmailPresets`（ADR-0042）

`discoverEmailProvider {input, allow_network?}`：`input` 为邮箱地址或域名（≤ 320 字节）。内置知识库
命中时零网络；未命中时先做公网域名校验（拒绝 IP 字面量、`localhost` 与内网后缀），
`allow_network = true`（缺省 `false`）才做有界的 TCP 可达性探测，否则返回启发式建议。无法识别的
输入 → `-32602`。结果字段：`provider_id`、`display_name`、`source`（`preset`/`srv`/`tcp_probe`/
`heuristic`）、`confidence`、`incoming`/`outgoing`（`{server, port, security}`）、`mailbox`、`idle`、
`username_format`、`suggested_username`、`auth_kind`、`strip_secret_spaces`、`notes`、`help_url`。
两者授权归 `accounts_manage`。

### `claimNextOpenIntent`

前端在启动或重新获得焦点时领取一条一次性导航意图。`frontend` 是最多 128 字节的稳定
插件 ID；原生恢复固定使用 `builtin-repair`，仅接受 `background`。09-B 过渡期保留旧阅读器 `builtin-egui`。没有待处理意图时返回 `{ "intent": null }`。

```json
{
  "intent": {
    "route": "message",
    "account": "personal",
    "uid": "42",
    "folder": "INBOX"
  }
}
```

当前 route 为 `inbox`（可带 `account`）、`message`、`compose`、`settings` 和
`background`。服务端最多保存 64 条有界意图，领取后立即删除。通知点击的账户和 UID 只在
Agent 内存与本地鉴权 API 中传递，不进入 URL、argv 或通知 identifier。未知 route 应由前端
忽略，以便协议后续 additive 扩展。入队成功即向目标前端推送 `frontend/intent` 唤醒信号
（见[服务端通知](#服务端通知)，ADR-0045）。

`message.folder` 为可选的服务器文件夹原名（非空且 ≤512 字节，不含控制字符）。新 OTP 通知从
来源邮件传入。旧意图省略仍可解码，但 Tauri 只退回该账户列表，不能根据重复 UID 猜测邮件。
等待草稿保存期间的后续手动导航也必须使原通知失效（ADR-0037）。

`listFrontends` 返回内置恢复前端和通过受管 manifest 检查的第三方前端。每项包含稳定 `id`、
显示 `name`、可选的 `description` / `l10n`（见下文）、`version`、`lifecycle`、`capabilities`、
`permissions` 和 `builtin`。字符串生命周期
与 capability 允许客户端保留未来未知值；安装格式见
[外部 frontend package v1](../plugins/README.md)。

`permissions` 是 ADR-0030 的纯授权声明，与作为 intent 路由的 `capabilities` 分开
（`#[serde(default)]`，旧 daemon 读到空表）。**它是给用户看的**：装一个前端之前应当能看见
它要什么，尤其是 `plugins_manage`——声明它等于声明「我可以代表用户安装任意受管包」。

#### 包的多语言名称与描述（ADR-0048 决策 5）

`listFrontends` 的每项、`listExternalPlugins` 的每项与 `previewPluginPackage` 的应答另有两个可选字段，
均来自包作者的 manifest（frontend `schema_version = 3`、external `schema_version = 2` 起）：

```json
{
  "id": "org.example.reader",
  "name": "Example Reader",
  "description": "A reader for Featherpost",
  "l10n": {
    "en": { "description": "A reader for Featherpost" },
    "zh-Hans": { "name": "示例阅读器", "description": "Featherpost 的阅读器" }
  }
}
```

- `description`：兜底描述（非空、≤1024 字节、无控制字符）；manifest 没写时**省略该键**。
- `l10n`：BCP 47 标签 → `{name?, description?}`。标签已由 daemon 规范大小写（`zh-hans` → `zh-Hans`），
  每项至少有 `name` 或 `description` 之一，缺的字段**省略**而不是 `null`；最多 16 种语言。映射为空时
  **省略该键**；内置 `builtin-repair` 恒无这两个字段。
- daemon **原样透传、不按语言偏好挑选**：前端拿自己协商出的语言查 `l10n`，找不到的字段回落到顶层
  `name` / `description`。
- `previewPluginPackage` 的这两个字段与其余字段一样参与 `expected_preview` 逐字段比对（用户批准的是
  看到的文字），客户端应把收到的预览对象**原样**回传，不要只挑认识的字段重组。
- 旧 daemon 不返回这两个键，共享 client 的 DTO 均为 `#[serde(default)]`。

`openFrontend` 由菜单栏或其他同一信任域内的本地插件提交同一 DTO。省略 `frontend` 使用默认
消费者，显式值只允许已安装 ID；`builtin-repair` 是原生恢复入口。服务端先入队，再尝试激活，
因此 `queued` 成功时恒为 true，而 `activation_requested: false` 表示 independent frontend 或
平台激活失败、intent 仍可被领取。默认前端缺失、损坏、激活失败或缺少该 route 时回落到原生恢复，
回落意图只有 `background`，不携带账户/UID。有效默认返回恢复 ID，但保留用户原有选择，重装后可恢复。显式指定但能力
不匹配则 `-32602`。参数结构、字段长度和未知 route 都在入队前 fail-closed 校验。

> **分片迁移（[ADR-0039](../decisions/0039-native-recovery-and-tauri-default.md)）**：09-B 保留旧阅读器默认，恢复改用 `builtin-repair`。09-C 才预装并切换默认 Tauri 包 `com.maild.ui`、迁移旧 ID、删除 egui。协议代数不变。

`listExternalPlugins` 是只读、粗粒度生命周期观测。每项为
`{id,name,description?,l10n?,version,lifecycle,capabilities,enabled,state,restart_count,has_ui}`
（`description` / `l10n` 见上文[包的多语言名称与描述](#包的多语言名称与描述adr-0048-决策-5)）；`has_ui`
表示 package 声明了可由 Agent 打开的原生管理界面。`state` 当前可为
`independent`、`disabled`、`starting`、`running`、`backoff`、`stopped`、`leaked` 或 `failed`。`leaked`（ADR-0020）表示停止流程结束后进程组内仍有存活成员；客户端应把
未来未知字符串原样保留。`openExternalPluginUi` 仅启动管理界面，不会自动启用插件；配置和安全边界见
[外部 package 文档](../plugins/README.md)。


前端主体（`frontend:<id>`）只能 claim 自己的 `frontend`；替别的前端 claim 返回 `-32602`——授权已过，越界的是对象。`user` 与插件主体沿用 ADR-0014 的同信任域语义（ADR-0030 决策 11）。
### `listMessages`

`limit` 必须是 1 到 500 的整数，默认 50；`offset` 必须是非负整数，默认 0。
出现其他字段、错误类型或越界值时返回 `-32602`。结果按 `sort_at` 从新到旧，
同秒时以落库序稳定（ADR-0025）。每个 `messages` 项是完整的纯文本消息：

```json
{
  "account": "personal",
  "uid": "42",
  "sender": "sender@example.com",
  "recipient": "alias@example.test",
  "subject": "hello",
  "body": "plain text body",
  "received_at": 1710000000,
  "message_id": "abc@example.com",
  "in_reply_to": "parent@example.com",
  "references": "<root@example.com> <parent@example.com>",
  "cc": "cc@example.com",
  "reply_to": "reply@example.com",
  "date_at": 1709990000,
  "sort_at": 1709990000,
  "snippet": "plain text body",
  "has_attachments": false,
  "seen": false,
  "flagged": false,
  "answered": false,
  "deleted": false
}
```

**`received_at` 是本机第一次观测到这封信的时刻，不是发信时间。** 发信时间是
`date_at`（`Date` 头解析结果，缺失或不可解析时省略该键）。`sort_at` 是列表排序键：
可信的 `date_at`，否则回退 `received_at`——远未来的 `Date` 不参与排序，但仍如实出现在
`date_at` 里。

ADR-0025 新增的键全部 additive：空字符串、`null` 与空数组一律省略，旧客户端按缺省处理即可。
`folder` 与 `folder_role` 标明这封信属于哪个文件夹（ADR-0026）。
`listMessages` **不返回** `body_html` 与 `attachments`——它一次可返回 500 条完整消息，
带上 HTML 会越过单帧上限。分页响应仍受 1 MiB 约束；正文较大时应减小 `limit`。

### `listFolders`

`account` 省略或为 null 时返回全部账户的文件夹，非空字符串时只返回该账户。每项：

```json
{
  "account": "personal",
  "name": "&XfJT0ZAB-",
  "display_name": "已发送",
  "role": "sent",
  "present": true,
  "total": 42,
  "unseen": 0
}
```

- **`name` 是服务器线上的原名**，可能是 modified UTF-7（RFC 3501 §5.1.3）。它是
  `listMessageSummaries` 的 `folder` 参数、也是 `getMessage` 的 `folder` 参数要传的值。
- `display_name` 是解码后的展示名；解码失败时等于 `name`。
- `role` 为 `inbox`/`sent`/`drafts`/`trash`/`archive`/`spam` 之一，或空串表示用户自建文件夹。
  判定顺序是 `INBOX` 保留名 → RFC 6154 SPECIAL-USE 属性 → 常见名字启发式 → 空。**猜不出就留空**，
  客户端应把未来出现的未知取值原样保留。
- `present` 为 false 表示它不在服务器最近一次 `LIST` 结果里：不再同步，但历史消息保留。
  文件夹重命名会留下一条 `present: false` 的旧记录和一条新记录（ADR-0026 决策 8）。
- `total` / `unseen` 是**本地库内**的计数，不是服务器侧计数。

### `listMessageSummaries`

`limit` 与 `offset` 规则同 `listMessages`。`account` 省略或为 null 时查询全部账户，非空
字符串时只返回该账户。`folder` 取 `listFolders` 返回的 `name`；省略或为 null 时不按文件夹
过滤，与 ADR-0026 之前逐字节一致。total 只统计当前过滤范围。每项不含 body：

ADR-0037 增加可选 `role`（inbox/sent/drafts/archive/spam/trash）、`seen`、`flagged`。
过滤发生在 SQL 的排序/分页/计数之前，可与 account/folder 组合；省略保留原有范围。
统一收件箱用 `role:"inbox"`，不拼各账户第一页。查询页和 total 从同一次数据库锁保护下读取。

```json
{
  "account": "personal",
  "uid": "42",
  "sender": "sender@example.com",
  "recipient": "alias@example.test",
  "subject": "hello",
  "received_at": 1710000000,
  "date_at": 1709990000,
  "sort_at": 1709990000,
  "snippet": "plain text body",
  "has_attachments": false,
  "seen": false,
  "flagged": false,
  "answered": false,
  "deleted": false,
  "folder": "&XfJT0ZAB-",
  "folder_role": "sent"
}
```

桌面列表应优先使用该方法，选中后再调用 `getMessage` 读取完整正文。`listMessages` 为既有
脚本保持兼容，不改变返回结构。

### `getMessage`

`account` 与 `uid` 都必须是非空字符串。不存在时返回 `-32002`。

**`uid` 只在 mailbox 内唯一**（ADR-0026 决策 1）：收件箱的 UID 42 和已发送的 UID 42 是两封
不同的信。因此应当带上 `folder`——摘要项里已经返回了它。省略 `folder` 时消歧规则是固定的：
收件箱优先，其次落库序最早的一条。

`message` 的字段与 `listMessages` 项相同，并**额外**包含两项（ADR-0025）：

- `body_html`：HTML 原文，最长 256 KiB。超限时末尾追加 `<!-- maild: truncated -->`；
  为空时省略该键。渲染前必须按 ADR-0024 的隔离上下文处理，内核不做净化。
- `attachments`：`[{part_index, filename, mime_type, content_id, inline, size, stored}]`，
  按 part 序。**只有元数据，没有字节**——附件内容走 `readAttachment`（下文）。`content_id` 是剥去
  尖括号的 `Content-ID`，供正文里的 `cid:` 引用解析；`inline` 为真表示它被正文引用而非独立附件，
  不计入 `has_attachments`。`size` 是解码后字节数，即使内容超过内联上限也如实记录。
  `stored`（ADR-0032）为真表示字节已落库、`readAttachment` 能读到；为假表示超过 ADR-0025 的内联
  上限或是迁移前的历史缓存——UI 据此决定要不要画一个能点的下载入口，而不是先做按钮再让它失败。

应答里与 `message` **同级**还有一个 `remote_content`（ADR-0032 决策 5），是此刻对这封信远程内容
的策略结论，不是这封信的事实：

```json
{ "message": { "...": "..." }, "remote_content": { "decision": "block", "reason": "default" } }
```

`decision` 取 `block` / `allow`；`reason` 取 `default`（无条目命中，采用默认策略）、
`trusted_address`、`trusted_domain`、`allow_all`。匹配规则固定在内核：从 `From` 头抽第一个
addr-spec，小写后先比地址、再比**精确**域名（`news.example.com` 不因信任了 `example.com` 而放行）。
**内核只给结论，不代取、不代理任何远程资源**；执行结论是渲染隔离层（M4.5-04）的责任。

### `readAttachment`

按**完整四元组** `(account, folder, uid, part_index)` 分块读取已落库的附件字节（ADR-0032 决策 1–3）。

- `folder` **必填**且不做消歧：UID 只在 mailbox 内唯一，读错一封信的附件字节比读错正文更难被
  察觉。消息必须可见（乐观删除/移动中的信读不到）。
- `offset` 缺省 0；`length` 缺省即上限 **524288**（512 KiB 原始字节），取值 1..=524288。单块 base64
  后约 700 KiB，加应答信封落在 1 MiB 帧内——这就是 2 MiB 内联上限的附件不必扩帧也能完整取回的原因。
- 应答：`attachment` 是该 part 的完整元数据（同上，`stored` 恒为真）；`data` 是本块的 base64；
  `length` 是本块原始字节数；`eof` 为真表示 `offset + length == size`。客户端按 `offset += 已收字节`
  循环到 `eof`。`offset == size` 时返回空 `data` 与 `eof = true`（零长附件同理）；`offset > size`
  是 `-32602`。
- 三种「拿不到」可区分：`(account, folder, uid)` 解析不到可见消息 `-32002`；消息存在但 `part_index`
  不是它的附件 `-32003`；part 存在但字节未落库 `-32004`（元数据里 `stored = false` 的那些）。

```json
{"jsonrpc":"2.0","method":"readAttachment","params":{"account":"personal","folder":"INBOX","uid":"42","part_index":2,"offset":524288},"id":7}
{"jsonrpc":"2.0","result":{"attachment":{"part_index":2,"filename":"季度报告.pdf","mime_type":"application/pdf","content_id":"","inline":false,"size":700000,"stored":true},"offset":524288,"length":175712,"data":"JVBERi0xLj...","eof":true},"id":7}
```

共享 client 的 `read_attachment` 循环拼装并带两道护栏：应答 `size` 中途改变即报错；累计字节超过
首块宣称的 `size` 即报错。`maildctl attachment save` 用它落盘，目标已存在即失败且不覆盖。

### `getReadingPolicy` / `setReadingPolicy`

远程内容默认策略与可信发件人名单（ADR-0032 决策 5）。这是 **daemon 拥有的持久状态**
（`reading_policy.toml`，与活动配置同目录，纪律同 `plugin_state.toml`），任何前端读到同一份。

```json
{
  "policy": {
    "remote_content": "block",
    "trusted_senders": [
      { "value": "news@example.com", "kind": "address", "added_at": 1757600000 },
      { "value": "example.org", "kind": "domain", "added_at": 1757600100 }
    ]
  },
  "persisted": true
}
```

- `remote_content` 缺省 `block`：原文中的远程图片、样式与字体默认不联网（UX-SPEC §4）。
- `setReadingPolicy` 是一个**原子 patch**：`remote_content`（`block` / `allow`）、`trust`、`untrust`
  三者至少给一项；`trust` 的每一项是地址（含 `@`，过地址语法门）或精确域名（不含 `@`，至少一个点），
  一律小写存储；任一项非法整包 `-32602` 不落盘；已存在的 `trust` 与不存在的 `untrust` 是幂等 no-op；
  同一项同时出现在两边是 `-32602`；名单上限 500 条；未知键拒绝。
- `persisted = false` 表示策略文件损坏或来自未来版本：读到的是**缺省策略**，`setReadingPolicy`
  返回 `-32001` 且**绝不覆盖**磁盘上那份。
- 只读从不创建文件；`maild backup` **不覆盖**这个文件（它是隐私偏好，不是邮箱数据；恢复一份旧邮件
  备份不该静默恢复旧的信任名单）。

### `getLocalePreference` / `setLocalePreference`

用户的语言偏好（ADR-0048 决策 1/2）。与阅读策略一样是 **daemon 拥有的持久状态**
（`locale_preference.toml`，与活动配置同目录，纪律同 `reading_policy.toml`），内核界面、默认前端与
第三方前端读到同一份。协议下发**有序偏好列表**而不是「定好的一种语言」：各前端拿 `effective`
与自己支持的语言协商，内核不替它挑。

```json
{
  "preference": { "mode": "system", "languages": [] },
  "effective": ["zh-Hans-CN", "en-US"],
  "persisted": true
}
```

- 授权：`getLocalePreference` 任何主体可用（与 `getAgentInfo` 同级，后台插件也可能要按用户语言
  渲染自己的文字）；`setLocalePreference` 要 `settings_write`，不新增授权词。
- `mode` 缺省 `system`（文件不存在时）；`system` 时 `languages` 恒为空数组。
- `effective` 是解析后的有序列表，**永不为空**：`explicit` 时等于 `languages`；`system` 时是 Agent 读到的
  系统首选语言（macOS：`+[NSLocale preferredLanguages]`，即 `CFLocaleCopyPreferredLanguages`；其他平台按
  `LC_ALL` → `LC_MESSAGES` → `LANG`），规范化、去重后截到 8 项；读取失败或为空时为 `["zh-Hans"]`。
  `system` 模式下每次调用都重读系统值。
- `setLocalePreference` 是**整包替换**：`mode = "system"` 时不得带 `languages`（`[]` 也不行）；
  `mode = "explicit"` 时 `languages` 必填，1..=8 个 BCP 47 标签（RFC 5646 形状，不查注册表），
  每个不超过 35 字节，规范化大小写（`zh-hans-cn` → `zh-Hans-CN`）后去重存储。任一项非法、超长、
  超过 8 项或带未知键，整包 `-32602` 不落盘。成功应答与 `getLocalePreference` 同形；`effective`
  因此变化时向所有流式连接推送 `preferences/locale`（见「服务端通知」）。
- `getLocalePreference` 的参数必须为空对象。
- `persisted = false` 表示偏好文件损坏或来自未来版本：读到的是**缺省偏好**（`system`），
  `setLocalePreference` 返回 `-32001` 且**绝不覆盖**磁盘上那份。
- 只读从不创建文件，首次写入才创建；`maild backup` **不覆盖**这个文件（它是偏好，不是邮箱数据）。
- 老 daemon 对 `getLocalePreference` 返回 `-32601`：前端回落自己的基础语言。

### `searchMessages` / `getThread` / `getSearchIndexStatus`

三者都在 `messages_read` 域（ADR-0029 决策 12）：换一个检索维度不构成新的权限面，读到的是
`listMessages` 已经能读的同一批数据。

**`query` 是纯文本，不是查询语法。** 内核自己切词并构造 FTS5 表达式：`"`、`*`、`-`、`(`、
`:`、`NEAR`、`AND`/`OR`/`NOT` 都被当成普通字符，既不会引发语法错误，也不会跨列检索。
需要精确控制的维度以结构化参数暴露：`from`（发件人子串，大小写不敏感）、`to`（收件人 `recipient`
或 `cc` 子串，大小写不敏感，字面匹配，不解析 `%`/`_` 通配符）、`subject`（主题子串，
大小写不敏感）、`since` / `until`（unix 秒，按列表显示的那个时间即 `sort_at`，须不超过
`i64::MAX` 且 `since <= until`）、`hasAttachments`（布尔）、`seen`（布尔，`false` 表示未读、
`true` 表示已读）、`flagged`（布尔）、`folder`、`account`。文本筛选字段受 512 字节上限约束，
空串或超长返回 `-32602`，仅含空白的字符串按省略处理；省略 `query` 或只给空白时退化为纯过滤查询。

中文按**逐字切分 + 短语**索引（ADR-0029 决策 3）：「发票」能命中「发票已开具」，
但不会命中「发货……票据」——短语要求字符相邻，等价于原文子串。查询词之间是 `AND`。

结果按 `sort_at DESC, id DESC` 排序，与 `listMessageSummaries` 完全一致，**不按相关度**。
`limit` 取 1–200（默认 50）。

`scope: "server"` 改走该账户 IMAP 连接上的 `UID SEARCH`，用于本地索引覆盖不到的历史
（超出 64 个同步文件夹的信箱、未同步的信箱、回填未完成的窗口）。它必须同时给出
`account`、`folder` 与非空 `query`，最多返回 200 条，**结果不落库**（只有摘要，没有正文与
附件）；且不接受非空的 `to`、`subject`、`seen`、`flagged` 筛选字段（提供时直接返回
`-32602`，不静默忽略也不在 200 条截断后做客户端过滤）。账户离线立即返回错误，不排队。

`getThread` 返回一条会话的全部消息，**时间正序**且**跨文件夹**——收件箱里的来信与已发送里的
回复本来就是同一条会话。线程只按 `Message-ID` / `In-Reply-To` / `References` 判定，
**不按主题兜底**：主题重复在真实邮箱里很常见，错并比漏并严重得多。`threadId` 为 0 表示该
消息还没进入索引回填，此时 `messages` 为空。摘要与消息里的 `thread_id` 是同一个值。

ADR-0037 的 `getThread` 支持 `limit` 1..200 与非负 `offset`，返回当前过滤范围 `total`；
非零 offset 必须带 limit。省略 limit 保留原有全量行为。Tauri 每页 30 条，发现续页 total
与已读页不同或 thread_id 改变时要求重新展开，不能把错位页拼成看似完整的会话。

`getSearchIndexStatus` 的 `pending` 是还没进索引的历史消息条数。升级到 schema v7 之后
历史消息由后台任务分批回填，**回填期间检索结果是部分的**；前端应据此提示「索引中」，
而不是让用户以为邮件丢了。新到的邮件不经回填，落库即可搜。

### `searchContacts`

按发件账户检索近期与历史联系人建议（只读，授权归 `messages_read`）。

- `account` 必填（非空字符串，≤256 字节，不含控制字符），严格绑定单账户，绝不跨账户泄露。
- `query` 可选纯文本或 `null`（≤256 字节，不含控制字符）；省略或空白时返回该账户的近期高频联系人。
- `limit` 可选正整数或 `null`（缺省 8，上限钳制至 20）；非正整数或未知键返回 `-32602`。
- **ADR-0034 权限边界**：绝不读取 `drafts` 表；仅扫描指定 `account` 下存在且角色非 `spam`/`trash`/`drafts` 文件夹内的可见未删消息（`pending_removal = 0` 且 `deleted = 0`）。
- 排序综合 `Sent` 文件夹收件人加权、时间衰减与己方主地址/别名降权；每条结果返回经发送校验的裸 ASCII `addr-spec`（保留 local-part 大小写、domain 小写）与可选纯文本显示名 `name`。

```json
{"jsonrpc":"2.0","method":"searchContacts","params":{"account":"personal","query":"ali","limit":8},"id":9}
{"jsonrpc":"2.0","result":{"contacts":[{"address":"Alice@example.com","name":"Alice"}]},"id":9}
```

### 写路径：`markMessages` / `moveMessages` / `deleteMessages`

三者连同 `listPendingOps` / `resolvePendingOp` 需要 `mailbox_write`
（[ADR-0030](../decisions/0030-frontend-authorization-closure.md) 决策 4）。
[ADR-0027](../decisions/0027-mailbox-write-path-and-offline-queue.md) 决策 9 拒绝把信箱写入并进
`messages_read`（可读即可删）或 `messages_send`（投递不是信箱状态）——那条判断仍然成立，缺的
只是它自己的词。该词同时进入 external plugin 词表：自动归档/打标的规则插件需要它，而且现在
必须**显式声明**才拿得到。

`account` 与 `folder` 必填，`folder` 取 `listFolders` 返回的 `name`；`uids` 是 1 到 500 个
非空字符串。`markMessages` 的 `flag` 取 `seen` / `flagged` / `answered` / `deleted`，
`value` 是布尔。`moveMessages` 的 `target` 是目标文件夹的线上原名，且不能等于 `folder`。

**三者都立刻改变本地状态，然后把写回排队。** 应答里的 `queued` 是实际入队的条数——
本地已不存在的 uid 会被跳过。
ADR-0037 新增 `queued_uids` / `unchanged_uids` / `missing_uids`，在同一写事务内归类：
前者为本次新入队，unchanged 为已满足目标标记，missing 为未处理（不存在或源消息已在移动中）。
`queued` 仍等于 queued_uids 长度，旧客户端不受影响；客户端不能用 requested−queued 推导失败数。`mode` 说明删除走了哪条路径：`move`（移进废纸篓）
或 `flag`（该账户没有废纸篓，落 `\Deleted`）。

账户离线时**不报错**，op 留在队列里等下次连接。因此前端必须能显示队列深度，
否则用户会以为操作丢了。

移动的本地表现是「从源文件夹消失」，而不是「换了个文件夹」：目标文件夹里的 UID 由服务器
分配，本地无从预知。它会在目标文件夹的下一次同步时出现，最长晚一个 `folder_poll_secs`。

### `listPendingOps`

`account` 省略或为 null 时返回全部账户。`include_settled` 缺省 false，即只返回未落定的
（`pending` / `in_flight` / `unknown`）。每项：

```json
{
  "id": 12,
  "account": "personal",
  "folder": "INBOX",
  "uid": "42",
  "kind": "move",
  "payload": "{\"target\":\"Archive\"}",
  "state": "unknown",
  "attempts": 1,
  "created_at": 1710000000,
  "updated_at": 1710000005,
  "detail": "连接中断，命令是否送达未知",
  "needs_decision": true
}
```

`state` 的取值与语义：

| state | 含义 |
|---|---|
| `pending` | 排队中，等下一次连接或下一个执行窗口 |
| `in_flight` | 已取出，命令正在发送 |
| `done` | 服务器返回 OK |
| `failed` | 服务器**明确拒绝**（NO / BAD），或用户放弃 |
| `unknown` | **命令已发出，未见 tagged 响应** |

**`unknown` 不是 `failed` 的一种。** 对可重放的操作（`kind` 为 `flag`）它会自动重试，
因此不会出现在清单里；对不可重放的操作（`kind` 为 `move` 或 `append`）它是**终态**——重放会在目标
文件夹产生第二份副本，而我们无从判断第一次到底成没成。`needs_decision` 为真时前端应当
把事实原样告诉用户（「这封信的归档结果未知，请检查目标文件夹」），并提供重试与放弃两个选择。

客户端应把未来出现的未知 `state` / `kind` 取值原样保留，不要当成已知值处理。

ADR-0037 扩展列表参数：`limit` 1..200（默认 200），`before_id` 为正整数游标，按 id 倒序续页，
`has_more` 表示还有更早记录。`include_settled:true` 才能看到 failed/done，活动 UI 始终显式请求。

`summary_only:true` 返回安全摘要，额外要求 `messages_read`，与原有 `mailbox_write` 同时满足；
缺任一项为 -32016。摘要无 payload，APPEND 原文在 SQL 查询中就被跳过；其他操作仅投影
已知 target/flag/value，错误 detail 映射为受控文案（保留可信“用户放弃”标记），并附稳定的
`detail_code`（`discarded` / `rejected` / `server_rejected` / `unconfirmed`，ADR-0048 决策 3）：前端按 code 用界面语言成文，
`detail` 的中文只是兼容老前端的兜底。收信循环写下的服务器命令失败（`APPEND 失败: …`、`STORE 失败: …`、
`SELECT <文件夹> 失败: …` 等）给 `server_rejected`，并在 `server_detail` 附去掉前缀的服务器原文（有界脱敏，
作为数据展示）；`unconfirmed` 的说明里有服务器原文时同样附上（ADR-0048 L6）。源消息存在时
另查最多 512 字符 subject，源消息消失或 APPEND 无源 UID 时为 null。这个通道不读取正文。
省略 summary_only 保留原有原始队列接口及授权，供原有受信任客户端使用。

### `resolvePendingOp`

`id` 是正整数，`resolution` 取 `retry` 或 `discard`。只对已落定但未成功的 op 生效——
`pending`、`in_flight` 与 `done` 会原样返回，不做改动。不存在的 id 返回 `-32002`。

`resolvePendingOp` 也接受 summary_only；摘要回执与列表同形且同样要求读写双权限，
授权检查先于状态改写。未知 state/kind 必须保留显示，UI 不擅自裁决。

`discard` 不是「撤销」：它只表示用户不再要求重试。对 `move` 类它会清掉本地的乐观移除标记，
让消息回到列表，由下一次全量对账收敛真相（消息真的移走了，对账会把它删掉）。

### `send` / `sendFromAccount`

`from` 与每一个收件人都必须是单个 ASCII addr-spec，不接受显示名、地址列表、引号 local-part
或国际化域名；每项最多 320 字节。local-part 支持常见字母数字、点号和 `+` 等 RFC atext
字符，域名逐 label 校验字母数字与连字符。
`subject` 最多 998 字节且不能含控制字符，`body` / `body_html` 各最多 512 KiB 且不能含 NUL。
正文换行会规范为 CRLF 并执行 SMTP dot-stuffing。表外的键一律 `-32602`——拼错的字段不会被
静默忽略。

`to` / `cc` / `bcc` 接受**单个字符串或字符串数组**，前者等价于单项数组；三者合计最多 100 个
收件人，且至少要有一个。

> **`bcc` 只进 SMTP 信封，绝不写进报文头。** 校验是**逐项**做的——校验每一个 addr-spec，
> 而不是拼接后的字符串。这是头注入防线（[ADR-0028](../decisions/0028-compose-reply-and-sent-append.md)
> 决策 2），不是可选的实现细节。

`in_reply_to` 与 `references` 用来把回复挂回会话，取 `prepareReply` 返回的值即可；它们会被
剥掉尖括号与控制字符后重新格式化。

`attachments` 只接受**库内已有的 part**（转发场景）：
`[{account, folder, uid, part_index}]`。**不接受新文件上传**——协议单帧上限 1 MiB，
base64 之后装不下任何一张真实照片；做一个用起来就失败的功能比不做更糟。附件上传需要一个
staging 目录路径（形状见 ADR-0019 的 `.incoming`），属独立切片。

组装出来的报文带 `Date` 与我们自己生成的 `Message-ID`（`send` 在应答里返回它）。非 ASCII 的
主题与文件名走 RFC 2047 `=?UTF-8?B?...?=`，非 ASCII 正文整体 base64——因此报文一定是纯 ASCII，
用 `grep` 直接看原文会看到编码后的形式。

`sendFromAccount` 投递成功后会把**同一份报文**入队归档到该账户的已发送文件夹，应答里的
`archived` 说明是否入队。归档是队列里的一条 `append` op：它失败**不影响**「已经发出去」这个
事实，可在 `listPendingOps` 里看到并重试。账户没有 `role = sent` 的文件夹时不入队。

委托给 provider 的账户（ADR-0021）仍然只支持**单收件人纯文本**：传 `cc`/`bcc`/`body_html`/
`attachments` 会被明确拒绝，而不是静默丢弃。

### `prepareReply`

`account` 与 `uid` 必填，`folder` 可选（省略时按 `getMessage` 的规则消歧），
`mode` 取 `reply` / `reply_all` / `forward`。消息不存在返回 `-32002`。
`locale` 可选（BCP 47 字符串或 `null`），决定引用块的语言，见下文。出现其他字段或类型不对时返回 `-32602`。

回复与转发的成型在**内核**完成——换前端不该换掉 `References` 的正确性。返回：

```json
{
  "draft": {
    "to": ["alice@example.com"],
    "cc": [],
    "subject": "Re: 周会纪要",
    "body": "\n\n在 Tue, 1 Mar 2022 10:00:00 +0000，Alice 写道：\n> 原文",
    "in_reply_to": "parent@example.com",
    "references": "<root@example.com> <parent@example.com>",
    "attachments": []
  }
}
```

- **主题归一**：反复剥掉 `Re:` / `RE:` / `答复:` / `回复:` / `Fwd:` / `FW:` / `转发:` 再加
  一个前缀，不会累积成 `Re: Re: Re:`。
- **`References` 继承**：原信的 `References` 追加原信的 `Message-ID`；原信没有 `References`
  时退回 `In-Reply-To`。超长时保留**首条与最后若干条**——首条是会话根。
- **`reply_all`** 的 `cc` 是原信 `To` ∪ `Cc` 去掉账户自身地址与其全部别名（ADR-0022），
  也去掉已经在 `to` 里的人。
- **`forward`** 的 `to` 为空、不继承 `in_reply_to`/`references`（转发开启新会话），
  `attachments` 是原信的**非内联**附件引用，可直接放进 `send` 的 `attachments`。
- **引用块语言（ADR-0048 决策 7）**：引用块会写进发出去的正文，是**邮件内容的语言**，可以与界面语言
  不同。回复引用行（`在 …，… 写道：` / `On …, … wrote:`）、转发头（分隔线与发件人/日期/主题/收件人四行）
  以及其中给人读的本地日期（`2026年11月18日 周三 19:35（UTC+08:00）` /
  `Wed, Nov 18, 2026 at 19:35 (UTC+08:00)`）都随协商结果成文：给了 `locale` 就只用它与内核支持的语言
  （`zh-Hans`、`en`）协商，省略或 `null` 时用语言偏好的 `effective`（见 `getLocalePreference`）协商；
  协商不出结果（如 `ja`、繁体 `zh-Hant*`）回落 `zh-Hans`。`locale` 按 `setLocalePreference` 的同一规则校验
  （BCP 47 形状、≤35 字节），非法即 `-32602`。原信日期不可信（早于 1971）时回退的 RFC 5322 日期与语言无关。
  `effective` 首选中文时不带 `locale` 的输出与引入本参数之前逐字节一致。
- 主题前缀与 `locale` **无关**：归一照旧识别上面全部已知前缀，加上的前缀恒为 `Re:` / `Fwd:`。
- 老 daemon 拒绝未知参数：共享 client 的 `prepare_reply` 只在给了 `locale` 时才发送该键。

共享 client 的 typed send 会在请求进入传输前执行同一组字段校验。桌面 compose 为避免
大文本编辑器持续布局超长正文，另设 64 KiB 输入上限；这不是协议收窄，`maildctl` 与其他
consumer 仍可使用 512 KiB 协议上限。

发信侧自 ADR-0028 起支持 Cc/Bcc、HTML 备选正文、会话头与转发原附件；读取侧自 ADR-0025 起返回
HTML 原文与附件元数据。全文检索与线程自 ADR-0029 起可用（见上文 `searchMessages` / `getThread`）。
附件字节取回与远程内容策略自 ADR-0032 起可用（见上文 `readAttachment` /
`getReadingPolicy`）。草稿自 ADR-0034 起由 daemon 持久拥有（见下文草稿一节）。
JSON-RPC v1 仍不承诺**新文件附件上传**、超限附件按需回源、文件夹的创建/重命名/删除，
也不承诺把草稿同步到服务器；这些能力按 M4.5 的切片顺序以新增方法扩展，
不改变现有字段语义。文件夹自 ADR-0026 起**只读同步**。
`seen`/`flagged`/`answered`/`deleted` 自 ADR-0027 起**可写**（见上文写路径），并在每次连接
建立时与服务器对账一次。对账不会覆盖用户最近的本地改动，也不会覆盖还有未落定 op 的消息。

### 草稿：`listDrafts` / `getDraft` / `createDraft` / `updateDraft` / `deleteDraft`

**daemon 拥有**的持久草稿（ADR-0034）。草稿存的是**字段而不是报文**——`Date` 与 `Message-ID`
仍然在投递那一刻生成，组装仍然只发生在发信时。

授权自成一域：读走 `drafts_read`，写走 `drafts_write`。**不属于 `messages_read`**
（收件箱里的信是既成事实，草稿是用户还没决定要不要发的字），也**不属于 `messages_send`**
（能改草稿的主体可以在用户按下发送之前换掉收件人）。前端的 `compose` 路由不隐含写草稿的授权。

```json
{
  "draft": {
    "id": "draft_qk3m7r2vbz5s4t6u8w9xa2c4e6",
    "account": "personal",
    "from": "",
    "to": ["alice@example.com"],
    "cc": [], "bcc": [],
    "subject": "十月共读：报名与选书",
    "body": "各位好：",
    "body_html": "",
    "in_reply_to": "", "references": "",
    "attachments": [
      {"account":"personal","folder":"INBOX","uid":"77","part_index":2,
       "filename":"校样.pdf","mime_type":"application/pdf","size":11,"stored":true}
    ],
    "version": 3,
    "send_state": "editing",
    "sent_archive": null,
    "created_at": 1757600000,
    "updated_at": 1757600420
  }
}
```

- **`version` 是并发仲裁。** 创建时为 1，每次成功更新 +1。`updateDraft` 必须带 `base_version`；
  与当前值不符返回 `-32006`，**一个字节都不写**，`error.data.version` 给出当前版本。
  `deleteDraft` 的 `base_version` 可选：省略即不校验版本，带上即「除非它在别处改过」；sending 状态始终拒绝删除。
- **回执是「已保存」的唯一来源。** `version` 与 `updated_at` 是 SQLite 事务提交之后才产生的；
  前端在拿到回执之前只能显示「未保存的更改」。
- **patch 语义。** 缺席的字段不动，出现的字段整体替换（清空正文是 `"body": ""`）。
  `account` 可版本化切换至存在的账户并受单账户草稿上限约束，已上传字节仍归该草稿；
  `attachments` 出现即替换整张列表，顺序即存储顺序。未知字段名一律 `-32602`——
  自动保存每隔几秒发一次，一个被静默忽略的字段意味着用户看着「已保存」而内容没存。
- **上限一律拒绝，绝不截断**（`-32007`，`error.data` 给出 `field` 与 `limit`）：
  `body` / `body_html` 各 256 KiB，`subject` 2 KiB，单条收件人 320 字节，
  `to + cc + bcc` 合计 100，附件引用 50 条，单账户草稿 200 份。
- **收件人不做地址语法校验**：草稿必须装得下打到一半的 `alice@`。但 `from` / `to` / `cc` /
  `bcc` / `subject` / `in_reply_to` / `references` 里的换行与控制字符在写入时就被拒（`-32602`）——
  头注入的最早一道门。真正的 addr-spec 校验在投递那一刻。
- **已有邮件附件仍是引用。** 每条指向库里已有的 `(account, folder, uid, part_index)`，
  写入时当场校验：解析不到消息 `-32002`、part 不存在 `-32003`、字节未落库 `-32004`
  （与 `readAttachment` 同义）。`account` 缺省为草稿自己的账户，写出来即可跨账户转发。
  读回时的 `stored` 是**现查的**：源邮件可能在草稿写完之后被删掉，UI 据此提前把话说在前面。
  发信时必须再查一次——写入时校验不是保证。
- `listDrafts` 只返回摘要（含 `snippet` 与 `attachment_count`，**不含正文**），
  按 `updated_at` 倒序；`limit` 1..=200，缺省 50。
- 草稿**不同步到服务器的 Drafts 文件夹**：IMAP 没有「更新一封信」，每次自动保存都要
  `APPEND` + `EXPUNGE`，且会让「保存成功」变成异步的。在手机上看不到电脑上写了一半的信，
  这是明确的边界。
- 草稿**进邮件库**（v8 起，G04 的 v9 同时保存新附件字节），因此随 `maild backup` / `restore` 一起走——
  与阅读策略刻意留在库外（ADR-0032 决策 5）相反，两处的理由各写在自己的 ADR 里。

### 版本化发送：`sendDraft` / `resolveDraftSend`（ADR-0036）

两个方法都要求 `messages_send` + `drafts_read` + `drafts_write`；缺少任一项返回 `-32016`。
参数只接受 `{id, base_version}`，不接受第二份正文；旧 `sendFromAccount` 契约继续可用。
`sendDraft` 校验保存字段和附件后用 CAS 将 editing/failed 变为 sending，再执行网络投递。
每次状态迁移递增 version；过期版本 `-32006`，状态不允许 `-32021`。sending 不可修改、删除或
完成附件；unknown/sent 不可普通编辑或重发，但可显式丢弃。

结果 `{draft}` 新增 `send_state` 与 nullable `sent_archive`（schema v10，v9 自动迁移）：

- `editing`：普通草稿；`failed`：确认未投递，可修改并主动重试。
- `sending`：已锁定并开始一次投递；`unknown`：不能确证是否投递，不自动重试。
- `sent`：已获成功回执。`sent_archive=true` 仅表示 Sent APPEND 已入队，非完成同步；
  `false` 表示未入队，`null` 表示尚未记录入队结论。三者都不得重新投递。

SMTP/provider 总期限 60 秒；明确 SMTP 拒绝记 failed，DATA 内容写出后失去最终回执记 unknown。
最终 DATA 2xx 之后 QUIT 失败仍是 sent；provider 不可确证的错误保守记 unknown。网络调用取消
以 guard 落 unknown；重启把遗留 sending 改为 unknown。成功先保存 sent，再排队归档。
`resolveDraftSend` 仅把 unknown 改为 failed，不投递；调用前 UI 要用户核对投递情况并确认。
状态与字节随草稿备份恢复；恢复旧备份可能缺少后来发生的投递事实，仍须人工核对。

Tauri Rust 桥按 8 条摘要分页，避免极长收件人列表超出 1 MiB 帧；webview 不取得 HTML 草稿
原文、本机文件路径或文件字节。编辑器对版本冲突保留本地输入，要求重新载入核对或先复制内容保留。

### 新文件附件：上传、完成与取消（ADR-0035）

四个上传方法需要 `drafts_write`，上传属主取自认证会话，不接受调用方传 owner 或本机路径。
先创建草稿，再调用 begin、连续 write，最后以草稿当前版本 finish。

```json
{"draft_id":"draft_example","filename":"报告.pdf","mime_type":"application/pdf","size":3145728}
```

begin 返回 `{upload:{upload_id,offset:0,size,chunk_bytes:524288}}`。write 的 `data` 是原始块的
base64，解码后每块 1..512 KiB，整体仍在 1 MiB 帧内；空文件无需 write。相同已确认区间、相同
字节重传成功，返回当前 offset；间隙、不同字节、跨已确认边界的重叠写返回 `-32009`。

finish 必须接收够声明的 size，且 `base_version` 相符。草稿版本冲突 `-32006` 时上传仍保留，
可读回草稿后决定是否重试。成功在一个事务中保存附件 BLOB/引用并增加 version，返回完整草稿。
同主体重复 finish（包括丢回执后重启）返回当前草稿，不重复添加；已被移除的附件返回不存在。
新附件的视图增加 `upload_id`，`folder`/`uid` 为空、`part_index` 为 0；不返回字节或属主。

- 更新/重排附件：`updateDraft.attachments` 中用 `{upload_id}` 指向**本草稿已有**的新附件。
  同时保留旧四元组引用。不能用另一草稿的 upload_id 导入；失败回滚整个 patch。
- 发送：`send` / `sendFromAccount.attachments` 新增 `{draft_id,upload_id}` 形状，额外要求
  `drafts_read`（外层仍需 `messages_send`），且重新查字节。缺失 `-32003`，整封不发送。
  发信不会自动删除草稿或附件。
- 原始大小限制：每文件 20 MiB，发送时附件合计 25 MiB，finish 时也校验草稿合计；最多 50 项。
  MIME 编码后更大，服务商可能有更低上限。
- 全局最多 8 个上传、128 MiB 声明总量；超限 `-32007`，不会驱逐其他上传。
- cancel 只释放未完成上传；要移除已完成附件，用带版本的 updateDraft。删除草稿同时取消其上传。
- 30 分钟无活动过期，每分钟回收；重启后未完成上传失效，需要重新选择文件。已完成附件随草稿
  保存、备份与恢复，不受 TTL 影响。尚未 finish 不能显示「附件已保存」。
- filename 是显示名：不能是路径、`.`/`..` 或含控制字符；mime_type 只接受 `type/subtype`。
  `-32008` 对不存在、过期、非属主上传使用同一错误形状；不返回磁盘路径。

CLI：`maildctl draft attach-file DRAFT --version VERSION /path/to/file --mime-type application/pdf`。
上传完成回执不确定或版本冲突会打印 upload_id；可用
`draft finish-upload UPLOAD --version VERSION` 重试，或 `draft cancel-upload UPLOAD` 取消。
客户端读文件传字节，daemon 不读取这个文件路径；Unix CLI 拒绝符号链接和非普通文件。

### `getSettings` / `setSettings`

当前只开放运行时 rules：

```json
{
  "show_sensitive": true,
  "silent_keywords": [],
  "quiet_hours": [{"start":"22:00","end":"07:00"}],
  "timezone_offset_minutes": 480
}
```

ADR-0043 起另有 `notify_new_mail`（缺省 `false`，为 `false` 时不出现在 JSON 里）与
`muted_accounts`（空时不出现）；默认配置下 `rules` JSON 逐字节不变。

`setSettings.rules` 是部分更新，`params` 只能含 `rules` 与可选 `persist`。修改立即生效，
从不写回 TOML。`persisted` 如实报告这次写入是否已原子落盘到活动配置同目录的
`rules_state.toml`（TOML `[plugins.rules.config] persist = true` 时由装配挂载；或请求里
`persist: true` 在守护进程有受管配置目录时运行时挂载，无路径 → `-32001`）。侧车损坏/不安全时
写入一律 `-32001` 且绝不覆盖该文件；落盘失败 `-32603`；规则非法 `-32602`。`getSettings` 同样
返回 `persisted`。

`rules_state.toml` 是覆盖层而非完整快照（`format_version = 1` + `[rules]`，只含经 `setSettings` 改过的
字段）：生效 rules = TOML 基线 ⊕ 覆盖层，用户从未改过的字段继续跟随 TOML。文件纪律与
`reading_policy.toml` 相同（0600、当前用户拥有、单硬链接、拒符号链接、64 KiB、未知版本/字段
fail-closed、临时文件 + `fsync` + 原子 rename）；首次持久写入才创建文件。客户端不应直接读写它，
一律经 `getSettings` / `setSettings`。

shared client 的 typed settings 调用会在传输前校验 patch，并校验服务端返回的关键词数量与
字节、静默时段、时间格式、静音账户和时区偏移。`setSettings` 成功响应必须明确包含
`persisted`（`true` 或 `false`）。桌面端只发送相对已加载基线发生变化的字段，成功后以服务端返回的完整
rules 为权威值；界面不会把运行时应用描述成配置持久化。

### 通知授权

`getNotificationStatus` 和 `requestNotificationAuthorization` 不扩展既有 `status`，两者返回同一
组粗粒度字段：

```json
{
  "provider": "macos",
  "availability": "available",
  "authorization": "not_determined",
  "request_in_flight": false,
  "fresh": true
}
```

两个方法都不接受业务字段；`params` 省略或为空对象时有效，数组和非空对象返回 `-32602`。
显式的 `params: null` 沿用本协议对请求结构的校验，返回 `-32600`。未装配 notify 服务时返回
`-32001`。

`getNotificationStatus` 只刷新系统设置，不触发权限窗口。查询失败或超时时返回最近缓存，并把
`fresh` 设为 `false`；客户端必须将其显示为过期状态，不能从旧值推断当前是否可以投递。

`requestNotificationAuthorization` 是唯一能发起系统权限请求的 RPC，必须来自用户显式操作。
只有最新状态为 `not_determined` 时才调用系统请求；已拒绝、已授权或不适用时直接返回当前状态。
系统窗口未完成时响应可以是 `authorization: "checking"`、`request_in_flight: true`，客户端随后
调用 `getNotificationStatus` 刷新。并发请求合并为同一个系统操作，不会重复弹窗。

`availability` 当前可能为 `available`、`unsupported`、`identity_mismatch` 或 `unknown`；
`authorization` 当前可能为 `checking`、`not_determined`、`denied`、`authorized`、
`provisional`、`ephemeral`、`not_applicable` 或 `unknown`。客户端必须接受 `unknown`，不能因未来
平台增加状态而拒绝整个响应。

在 macOS 上，只有 bundle ID 与 Agent identity manifest 一致的 `Maild Agent.app` 可以使用原生
provider。裸二进制返回 `identity_mismatch`；非 macOS 平台返回 `unsupported`。状态查询会返回
这些值，授权请求则返回 `-32001`，两种情况都不回退到 `osascript` 或 `dev-log`。

响应不包含队列深度、投递计数和任意平台错误文本，避免状态查询泄漏 OTP 活动或系统诊断。
checking、未授权和拒绝期间收到的 OTP 在写入持久化通知标记后丢弃，授权成功后不会回放；容量
32 的内部队列只承载当前已授权的瞬时投递。完整决策见
[`decisions/0012-m3-5-native-notifications.md`](../decisions/0012-m3-5-native-notifications.md)。

## 版本握手（ADR-0016）

`getAgentInfo` 返回内核身份与协议代数。**判定只依据 `protocol_version`**；`pid`、
`started_at`、`build_rev`、`executable_path` 仅诊断展示，客户端不得让其参与逻辑。

- 协议代数是单一整数常量，单源于 `maild-client::PROTOCOL_VERSION`，首发为 1。
- **必须 +1**：删除或重命名方法；既有方法的参数或结果 schema 不兼容变更；错误码语义
  变更；来源/秘密门禁收紧到旧客户端会违规的程度。
- **不晋升**：新增方法；响应新增可选字段；纯文案与诊断字段变化。每次晋升在本节记录一行。
- 客户端在连接建立后（以及每个重连 epoch）调用一次：相等兼容；对端较小 → 内核过旧；
  对端较大 → 应用过旧；**对端以 -32601 应答视为遗留内核（代数 0）**——这正是升级后旧
  守护进程仍占住受管 socket 的形态；传输错误不做版本结论。
- 桌面壳在不一致时禁用突变入口并展示横幅，一键重启由用户显式触发（ADR-0011）；CLI 在
  stderr 警告但不改变退出码。

| 代数 | 变更 |
|---:|---|
| 1 | M3.7 基线：账户方法（ADR-0015 §9）+ `getAgentInfo` 握手 |

## 插件身份认证（ADR-0017）

M4-01 新增会话级主体：未认证连接恒为 `user`（历史行为不变），合作插件可用逐插件凭证
把一条连接绑定为 `plugin:<id>` 或 `frontend:<id>`。这是问责与最小授权的契约基础，
不是同用户域内的恶意进程防线。

- **绑定规则**：`authenticate {token}` 只允许作为连接上**第一个请求**；成功后该连接
  主体不可变。凭证格式 `mdp_` + 43 个 base64url 字符。
- **fail-closed**：凭证无效/已吊销/包已移除 → `-32014`，应答后服务端立即关闭连接，
  绝不静默降级为 `user`；stdio/HTTP 来源或身份缝停摆 → `-32015`（连接可继续以
  `user` 身份使用）。
- **凭证通道**：supervised 子进程经空环境中的 `MAILD_PLUGIN_TOKEN` 注入且每次 spawn
  轮换（旧凭证对新连接立即失效）；independent/on_demand 由用户执行
  `maildctl plugin-token <id>` reveal——reveal 即轮换，明文只在本次 stdout 出现。
- **客户端契约**：认证只能走长连接（`maild-client::StreamConnection::authenticate`）；
  `RpcClient` 的便捷调用每请求独立连接，永远保持 `user` 主体。
- 注册表只存 SHA-256 哈希；`listExternalPlugins` / `listFrontends` 各新增
  `has_identity` 布尔观测字段（旧 daemon 缺省 false），绝不返回凭证或哈希。
- **方法级授权（ADR-0018）**：插件主体按 manifest `capabilities` 授权，默认拒绝；
  未声明的调用返回 `-32016`（连接不断开），拒绝计数经 `denied_calls` 观测字段
  可见。`authenticate`/`whoami`/`getAgentInfo`/`getLocalePreference` 任何主体可用。前端 route 经
  固定翻译映射：`inbox|message → messages_read`、`compose → messages_send`、
  `settings → settings_read + settings_write`、`background → 无`。服务端推送同受
  管控：无 `messages_read` 的主体不收邮件/账户事件通知。完整策略表见
  ADR-0018 与 ADR-0030；未登记的未来方法对插件主体一律 fail-closed 拒绝。
- **前端授权闭环（ADR-0030）**：frontend manifest `schema_version = 2` 起可另外声明
  `permissions`，与路由 `capabilities` 分开。新增四个词——`mailbox_write`（信箱写回与
  离线队列）、`accounts_manage`、`addresses_manage`、`plugins_manage`；后三个只有前端
  可声明，后台插件没有理由改账户配置，更没有理由装下一个插件。同时修正三处归类：
  `status` 与 `prepareReply` 归 `messages_read`（前者不比 `getAgentInfo` + `listAccounts`
  多暴露什么，后者是纯读，拼草稿不落库也不投递）、`sendFromAccount` 归 `messages_send`、
  `requestNotificationAuthorization` 归 `settings_write`。**`revealPluginToken` 永久
  仅限用户主体**（ADR-0039 另增下述原生修复例外）：持有一份凭证的主体不得铸造凭证。已安装包的有效权限逐字节不变——
  新词只能从新字段来，新字段只能从 schema 2 来。不新增方法、参数、错误码，协议代数不提升。
  **决策 11 补遗**（ADR-0031 实施期）：每个前端记录隐含 `frontend_open`，`claimNextOpenIntent`
  对前端主体校验 `frontend == 自己的 id`（否则 `-32602`）；`notifications_read` 成为第五个可
  声明的授权词。
- **默认 Tauri 前端（ADR-0031）**：`com.maild.ui` 是本协议第一个以 `frontend:` 主体运行的真实
  消费者：一条短命 `user` 连接 `revealPluginToken` 后即关，长连接首帧 `authenticate`，再以
  `whoami` 校验；之后壳不持有任何 `user` 连接。webview 侧不接触 endpoint、token 或本协议——
  只经壳的类型化命令拿投影后的 DTO 与四类错误码（`offline` / `denied` / `not_found` / `invalid`
  / `failed`）。协议本身零改动。
- **安装事务（ADR-0019）**：受管目录的唯一写者是 Agent。CLI 只把包复制进
  `.incoming/<rand>` 再调 `commitPluginInstall`；升级要求三段数字版本严格更高，
  旧版留在 `.rollback/<id>/`（单深度互换回滚，再执行一次切回）；`.incoming`/
  `.removed`/`.rollback` 点前缀目录永不激活，崩溃残留不阻塞扫描。事务失败返回
  `-32017`。初始 ADR-0019 为用户主体；ADR-0030 起受托前端可用 `plugins_manage`，后台插件
  不能获得管理词。additive、不晋升协议代数。
- **原生修复（ADR-0039，09-A）**：`repairBuiltinFrontend {}` 为永久 UserOnly 维护例外，HTTP/stdio、
  已认证前端与后台插件全部拒绝；不增加 capability，不提供 webview 桥。恢复只替换固定 `com.maild.ui`
  并重设默认；旧包和损坏状态留在 `.repair-backups`，复制/发布/刷新期间与包管理串行。
  发布失败返回 `-32017` 并尝试回退；回退不确定、磁盘已恢复但刷新失败均明确说明，禁止盲目重试。
  请求断连后事务继续持锁完成。离线原生入口必须持有生命周期锁、前端目录独占锁以及启用持久库时的数据库维护锁，是 Agent 唯一写者约束的维护例外。
  每个启用 frontend 的新版后台均持有 `.owner.lock`；在线后台任务延续该锁直到发布和刷新结束。
  无固定副本的开发态/旧安装包不可使用此 RPC；09-B 提供原生图形入口，09-C 接线实际安装包，当前没有面向用户的 CLI 子命令。
- **原生后台控制（ADR-0039，09-B）**：`stopManagedAgent {}` 同为永久 UserOnly、本机 IPC 专用，
  前端/后台插件、HTTP/stdio 与多余参数全部拒绝。裸 CLI daemon 不装配控制服务。它触发正常 LIFO 退出，
  回执仅表示接受，不代表已退出；被接受时守护进程先向已认证前端长连接推送 `agent/stopping`，
  再经约 350 ms 排空窗口停机并清理 socket（ADR-0046，见[服务端通知](#服务端通知)）；被拒绝的请求不推送。
  菜单栏「退出 Maild」即调用此方法，一并关闭窗口、菜单栏与后台守护；不向原生窗口提供 PID 杀进程权限。Tauri 设置仅能请求打开原生入口。
  前端目录不可用时，不暴露安装根、不执行包变更或身份清空；收信、Store 和 API 继续运行。
- **设置管理补充（ADR-0038）**：`getPluginInstallRoot` 返回当前 daemon 的对应受管根，只供
  原生复制器使用，不透传到 webview。`previewPluginPackage` 对 staging 或回滚位执行完整
  manifest/权限/入口校验，不发布也不运行。`expected_preview` 可选，提交/回滚前重新校验后
  逐字段比对，不一致返回 -32017；旧客户端省略仍兼容。新 Tauri 流程必须带预览确认值。
  `setDefaultFrontend` 仅接受在册 id，原子保存 `.default.json` 后影响后续激活，重启优先读取
  该状态而非已被移除的 TOML 种子。删除有效默认被拒；状态损坏或发布结果不确定时拒绝前端
  移除。包管理事务和默认选择串行。移除启用的 external 包先持久停用，失败时不保证仍启用。
  `updateAccount.patch.outgoing` 明确区分省略（不变）、null（移除发信）与对象（替换）。
  `probeAccount` 的 id 与 expected_revision 须同时提供或同时省略。提供时检查活跃受管账户
  并在网络等待前后核对版本（冲突 -32011），向 provider 传真实账户 id；省略保持新建探测。
  两条路径都不保存账户，也不创建远程邮箱。
- **transport provider 反向缝（ADR-0021）**：manifest 新增第 8 个 capability 词
  `transport_provider`（frontend route 翻译表不含它——前端主体永远注册不上）。
  已认证插件以 `providerRegister {}` 把当前连接登记为该插件 id 的活跃 provider
  端点（latest-wins、断连自动注销）；守护进程随后可在该连接上发起
  `transport/probe|send|receive` 反向请求（见上文方法表）。受管账户目录
  format_version 升至 2：`incoming/outgoing` 支持 `kind="plugin"` 委托变体，
  v1 目录仍可读（禁止 plugin 变体），任何写入落盘 v2。委托方向凭据不跨缝、
  绝不回退内置 transport；`listExternalPlugins` 各项新增
  `provider_registered` 观测位。
- 全部为 additive 变更：不晋升协议代数。

## 错误码

| code | 名称 | 含义 |
|---:|---|---|
| `-32700` | Parse error | 请求不是有效 JSON |
| `-32600` | Invalid Request | JSON-RPC 结构无效，或流式请求超过 1 MiB |
| `-32601` | Method not found | 方法不存在 |
| `-32602` | Invalid params | 方法参数无效 |
| `-32603` | Internal error | 查询、发送或响应构造失败；也用于响应超过 1 MiB |
| `-32001` | Service unavailable | 方法依赖的插件未装配，或 Agent 未以平台默认配置运行（无受管账户目录） |
| `-32002` | Message not found | `getMessage` / `readAttachment` 找不到（可见的）消息 |
| `-32003` | Attachment not found | `readAttachment`：消息存在，但 `part_index` 不是它的附件（ADR-0032） |
| `-32004` | Attachment not stored | `readAttachment`：part 存在，但字节未落库——超过内联上限或迁移前的历史缓存（ADR-0032） |
| `-32005` | Draft not found | `getDraft` / `updateDraft` / `deleteDraft`：草稿 id 不存在（ADR-0034） |
| `-32006` | Draft conflict | `base_version` 与当前版本不符，**一个字节都没写**；`error.data.version` 是当前版本（ADR-0034） |
| `-32007` | Draft too large | 超过草稿的字段/条数上限；`error.data` 给出 `field` 与 `limit`。**内容不被截断**（ADR-0034） |
| `-32021` | Draft delivery locked | 草稿投递状态不允许该操作；sending/unknown/sent 禁止普通编辑或再次发送（ADR-0036） |
| `-32008` | Upload not found | 上传不存在、过期或不属于当前主体（相同形状，不暴露属主） |
| `-32009` | Upload state mismatch | offset、重传内容或完成长度不符；data 给当前 `offset` / 声明 `size` |
| `-32010` | Account not found | 账户 ID 不存在或已归档不可操作 |
| `-32011` | Account conflict | revision 不匹配，或 legacy / 目录版本冲突 |
| `-32012` | Account probe failed | create/update 强制探测未通过；`error.data` 携带分阶段报告 |
| `-32013` | Credential unavailable | 平台凭据库不支持、锁定或访问失败 |
| `-32014` | Auth rejected | 插件凭证无效/已吊销，或会话重复 `authenticate`；应答后连接关闭 |
| `-32015` | Auth unavailable | `authenticate` 在 stdio/HTTP 上、身份缝未装配或停摆 |
| `-32016` | Method not permitted | 插件主体缺少方法所需 capability，或方法仅限用户主体；连接不断开 |
| `-32017` | Plugin transaction failed | 插件事务失败：包校验未通过、升级版本不严格更高、staging 非法或缺失、回滚位不存在等（ADR-0019） |
| `-32018` | Provider unavailable | 委托方向无活跃 provider 注册、连接刚断或被顶替；快速失败不回退（ADR-0021） |
| `-32019` | Provider timeout | 反向调用超过各自预算（probe 10s / send 30s / receive 60s）（ADR-0021） |
| `-32020` | Provider error | provider 插件应答的业务失败；`message` 来自插件且有界脱敏（ADR-0021） |

`error.data` 是可选字段。客户端不能依赖中文 `message` 做分支判断，应依赖 `code`。
方法参数会先于插件依赖校验；因此即使 store 或 transport 未装配，非法参数仍返回
`-32602`，只有参数有效但依赖缺失时才返回 `-32001`。

## 服务端通知

stdio、Unix socket 与 named pipe 连接可收到无 `id` 的服务端通知。通知可能先于某个
请求的响应到达，客户端必须按 `id` 匹配响应。

| method | `params` |
|---|---|
| `message/received` | 消息对象（**不含** `body_html` 与 `attachments`） |
| `account/state` | 账户状态对象（离线时可带 `detail_code` 与 `server_detail`，见 `status` / `listAccounts`） |
| `frontend/intent` | `{"frontend":"<id>"}`——只投给主体**恰好**是 `frontend:<id>` 的已认证长连接（ADR-0045） |
| `agent/stopping` | `{}`——只投给主体为 `frontend:*` 的已认证长连接；守护进程接受 `stopManagedAgent` 后、停机前发出（ADR-0046） |
| `preferences/locale` | `{"effective":[...]}`——投给**所有**流式连接；`setLocalePreference` 成功且 `effective` 变化时发出（ADR-0048） |

```json
{"jsonrpc":"2.0","method":"message/received","params":{"account":"personal","uid":"42","sender":"sender@example.com","recipient":"alias@example.test","subject":"hello","body":"plain text body","received_at":1710000000}}
```

通知是尽力推送：本地 IPC 最多同时保留 64 个流式连接，超过上限的新连接会被关闭；
每个连接的输出队列容量为 32，慢消费者可能丢通知。推送中的单个
文本字段最多 8 KiB，邮件正文最多 64 KiB，并在 UTF-8 字符边界截断。可靠历史查询
应使用 `listMessageSummaries` / `listMessages` / `getMessage`，不能把通知当作持久队列。
出于同一理由，通知帧不携带 HTML 原文与附件（ADR-0025）：它是「有新邮件」的信号，
需要正文渲染时再调 `getMessage`。

`frontend/intent` 是唤醒信号（ADR-0045）：`openFrontend`、菜单栏或通知点击给某个前端排入意图后，
守护进程在释放意图队列锁之后向该前端已认证的长连接推送
`{"jsonrpc":"2.0","method":"frontend/intent","params":{"frontend":"com.maild.ui"}}`。帧内不含
route、账户或 UID；前端收到后自行 `claimFrontendIntent` 排空队列（最后一个意图决定页面）。user、
插件与其他前端（含 id 前缀相同者）都收不到；推送丢失时意图仍留在队列，下一次领取照常取得。

`agent/stopping` 是停机信号（ADR-0046）：用户点击菜单栏「退出 Maild」，菜单经本地 IPC 以 user 主体调用
`stopManagedAgent`；守护进程接受后立即向所有已认证前端长连接推送
`{"jsonrpc":"2.0","method":"agent/stopping","params":{}}`，随后经约 350 ms 排空窗口正常退出并删除 socket。
user、插件与未认证连接收不到。参数恒为空对象，客户端不应校验其内容。前端收到后应刷盘草稿、撤下窗口并随后台一起退出
（`maild-app` 置位 `ViewState.shutdown_requested`，置位后不回落、不进快照同步缓冲）。点击窗口红叉、`⌘Q` 或 Dock
右键「退出」均仅关闭窗口并彻底清空 Dock 图标，不调用 `stopManagedAgent`，后台与菜单栏继续运行。

`preferences/locale` 是语言偏好变化信号（ADR-0048）：`setLocalePreference` 成功且解析后的列表变化时，
守护进程向所有流式连接（不论主体，`getLocalePreference` 本身对任何主体开放）推送
`{"jsonrpc":"2.0","method":"preferences/locale","params":{"effective":["en","zh-Hans"]}}`。前端收到后
用新列表重新协商语言并重新渲染。`system` 模式下系统语言变化**目前不主动推送**；推送也是尽力而为，
前端应在窗口聚焦或重连时调用 `getLocalePreference` 追上。共享 client 用
`Notification::into_server_notification()` 把它解码为类型化的 `ServerNotification::LocalePreference { effective }`
（参数形状不对是协议错误，与其他已知通知一致）；只关心这一条的消费者也可以直接用
`Notification::locale_preference_changed()`。`maild-app` 的连接 actor 在每次（重）连接与刷新的快照里另取一次
`getLocalePreference`，推送即时更新 `ViewState.locale`。

HTTP 不承载通知。

## 传输

### stdio

stdin 和 stdout 使用一行一个 JSON 对象的 UTF-8 帧，换行符为 `\n`，输入也接受
`\r\n`。stdout 只用于协议；日志写 stderr。stdio 适合由桌面壳或父进程启动并持有
maild 的场景，不需要 token，信任边界是父子进程已建立的管道句柄。

```sh
printf '%s\n' '{"jsonrpc":"2.0","method":"status","id":1}' \
  | maild maild.toml
```

### HTTP

HTTP 只允许绑定回环 IP 字面量。客户端向 `http://LISTEN/rpc` 发送 `POST`，使用
`Authorization: Bearer TOKEN`；JSON-RPC 通知成功时返回 HTTP 204，普通请求的
JSON-RPC 成功或失败均返回 HTTP 200。

```sh
curl --fail-with-body \
  -H "Authorization: Bearer $MAILD_API_TOKEN" \
  -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"status","id":1}' \
  http://127.0.0.1:7394/rpc
```

信任边界是回环地址加 bearer token。回环 TCP 本身不能证明监听进程就是 maild；
token 应通过 `MAILD_API_TOKEN` 注入，或存入 Unix 权限为 0600 的配置文件。HTTP 是
明文传输，不应转发到非本机网络。

### Unix domain socket

配置使用绝对路径；父目录必须已存在：

```toml
[plugins.api]
[plugins.api.config]
transport = "unix"
listen = "/Users/alice/Library/Application Support/maild/maild.sock"
```

帧格式与 stdio 相同。maild 把 socket 文件权限设为 0600；普通 daemon 重复启动或发现路径已
存在时一律拒绝绑定，不会自动删除 stale 文件，避免把活跃 listener 的路径误删。请在确认旧进程
已退出后手动删除 stale socket，再重新启动。macOS 受管 Agent 仅在 managed runtime、同 UID、
inode 未变化且连接明确返回 `ConnectionRefused` 时恢复系统强制终止遗留的 socket；这不改变通用
API bind 的 fail-closed 语义。正常退出时只删除仍指向自己创建 inode 的路径。
客户端不使用 bearer token。

信任边界是 socket 路径、整个祖先目录链和 socket 文件权限。maild 要求父目录由当前用户
拥有且为 0700，路径祖先不能是符号链接、不能由其他普通用户拥有，也不能对 group/other
可写（root 拥有的 sticky 临时目录除外）；仅有 0600 socket 文件不能补救一个可被他人替换路径的公共父目录。

### Windows named pipe

```toml
[plugins.api]
[plugins.api.config]
transport = "named_pipe"
listen = "\\\\.\\pipe\\maild"
```

帧格式与 stdio 相同。服务端拒绝远程客户端并以 first-instance 语义阻止同名服务端
并存；客户端不使用 bearer token。named pipe 使用当前进程用户 SID 的显式 DACL，只有
该用户可访问；远程拒绝仍作为额外边界。当前用户的不同 Windows logon session 不会被
进一步区分，Windows ACL 运行时测试需在 Windows 主机执行。

`transport = "ipc"` 在 Unix 上等同 `unix`，在 Windows 上等同 `named_pipe`。
兼容别名 `unix_socket` / `socket` 和 `pipe` 可被读取；新配置应使用表中的规范名称。

## 共享客户端实现

`crates/maild-client` 是 CLI 与 M3 桌面端共用的 v1 消费库，负责 endpoint/config 发现、
HTTP/IPC、1 MiB 帧上限、总 deadline、id/响应校验、协议 DTO 和通知解码。IPC 连接可拆成
独立读写半部，读半部在等待被取消时不会丢失已经消费的半帧。它不启动守护
进程，也不缓存或解释邮件领域状态；重连、断线期间状态和恢复后的快照归并由应用层负责。

HTTP 客户端按单次请求使用；Unix socket / Windows named pipe 可建立持久流，在同一连接上
按 id 匹配响应并接收无 id 通知。断线通知不重放，消费者恢复连接后必须重新拉取快照。
边界决策见
[`decisions/0001-m3-shared-client-boundary.md`](../decisions/0001-m3-shared-client-boundary.md)。

M3 桌面端通过 `maild-app::ConnectionHandle` 消费上述原语：IPC 断线后重连并重新拉取
`status` / `listAccounts` / `listMessageSummaries`，HTTP 则定时轮询同一快照。应用层行为见
[`decisions/0002-m3-connection-actor.md`](../decisions/0002-m3-connection-actor.md)，它不改变本页 wire contract。

## maildctl

连接选择优先级是显式参数或同名环境变量，其次是所选配置。配置本身按显式 `--config` >
`MAILD_CONFIG` > 自动发现；平台默认 `maild.toml` 与 cwd 兼容文件若指向不同文件，自动发现会
fail-closed 并要求显式选择；同一文件的路径别名选择平台路径：

| 参数 | 环境变量 | 传输 |
|---|---|---|
| `--url URL` | `MAILD_API_URL` | HTTP |
| `--socket PATH` | `MAILD_API_SOCKET` | Unix socket |
| `--pipe NAME` | `MAILD_API_PIPE` | named pipe |

三个参数互斥。HTTP token 优先取 `MAILD_API_TOKEN`，其次取配置中的
`bearer_token`；IPC 不读取 token。配置发现按 `plugins.api.config.transport` 选择传输。
配置路径只接受不超过 1 MiB 的 UTF-8 普通文件，避免设备/FIFO 或异常大文件阻塞客户端。
配置派生的相对 Unix socket 锚定 `maild-paths` runtime 目录；显式 `--socket` 或
`MAILD_API_SOCKET` 仍按调用者给出的路径处理。
stdio 不能由独立 `maildctl` 连接。

```sh
maildctl --socket /tmp/maild.sock --json status
maildctl sync [--account personal]                                # syncMessages：立即同步，只敲门不等完成
maildctl settings
maildctl set-settings --persist --rules-json '{"notify_new_mail":true,"muted_accounts":["work"]}'
maildctl --pipe '\\.\pipe\maild' accounts
maildctl --url http://127.0.0.1:7394 messages --limit 20
maildctl notification-status
maildctl request-notification-authorization
maildctl frontends
maildctl plugins
maildctl message --account personal --folder INBOX --uid 42      # 详情含附件元数据与 remote_content
maildctl attachment save --account personal --folder INBOX --uid 42 --part 2 --out ~/Downloads/报告.pdf
maildctl reading-policy show
maildctl reading-policy set --remote-content block|allow
maildctl reading-policy trust news@example.com example.org
maildctl reading-policy untrust example.org
maildctl locale                                                   # getLocalePreference：mode、显式列表与 effective
maildctl locale set en zh-Hans                                    # setLocalePreference：有序列表，整包替换
maildctl locale set --system                                      # 跟随系统首选语言
maildctl draft list [--account personal]
maildctl draft new --account personal --to alice@example.com --subject 周会 --body-file -
maildctl draft show draft_qk3m...
maildctl draft set draft_qk3m... --version 2 --subject 周会纪要 --body-file notes.txt
maildctl draft attach draft_qk3m... --version 3 --folder INBOX --uid 77 --part 2
maildctl draft detach draft_qk3m... --version 4 --position 0
maildctl draft rm draft_qk3m... --version 5
```

`attachment save` 分块取回到同目录临时文件、核对实收字节数等于 `size` 后以 `hard_link` 发布：
目标已存在即失败且**不覆盖**（UX-SPEC §4「失败不删除已有目标」在 CLI 上同样成立）。

`draft set` / `attach` / `detach` 的 `--version` 是**你上次读到的那个版本号**：过期就被拒绝
（`-32006`），不会合并、也不会覆盖。`draft rm` 的 `--version` 可省略（无条件丢弃）。
`--body-file -` 从 stdin 读正文，大正文不必经过 shell 参数。
`attach` / `detach` 是一次读-改-写：先取当前引用列表，再带着同一个 `--version` 整体写回。

`locale` 读写 daemon 拥有的语言偏好（见「`getLocalePreference` / `setLocalePreference`」）；`locale` 与
`locale show` 等价，`set` 的语言与 `--system` 二选一且至少给一个，标签由 daemon 校验（非法 `-32602`，
退出码 4）。`--json` 打印与协议应答同形的一行 `{preference, effective, persisted}`。

maildctl 自己给人读的结果、确认、警告与输入提示（插件安装/升级/移除/回滚/启停、凭证轮换提醒、
协议代警告、草稿与策略的提示、秘密输入提示）随语言偏好成文（ADR-0048 L5）：连上 daemon 时用
`effective` 与内核支持的语言（`zh-Hans`、`en`）协商；连不上、老 daemon 返回 `-32601` 或读取失败时
改读系统首选语言（与 daemon `system` 模式同一来源），仍无结果回落 `zh-Hans`。`locale set` 的人读输出
按写入后的新偏好成文。不随语言变化的有：`--json` 输出、`key: value` 行的字段名与取值、`--help`、
以及错误行（`featherpostctl: …` 与 `--json` 的 `error.message`）——后者与协议 `error.message` 一样
面向开发者。

### maildctl account

```sh
maildctl account list [--all]
maildctl account add --display-name 主邮箱 --address me@example.com \
    --imap-host imap.example.com --imap-username me@example.com \
    [--imap-port 993] [--imap-security implicit|starttls] [--imap-mailbox INBOX]
    [--no-imap-idle --imap-poll-secs 60]
    [--smtp-host smtp.example.com --smtp-port 587|465 --smtp-security starttls|implicit
     --smtp-username me@example.com --smtp-credential incoming|separate]
maildctl account test …   # 与 add 相同参数，只探测不保存
maildctl account discover alice@qq.com             # discoverEmailProvider：内置知识库，零网络
maildctl account discover example.org --network    # 未知公网域名做有界探测（拒绝 IP/内网目标）
maildctl account provider-test --display-name NodeSeek --address user@seek.li \
    --provider app.maild.provider.nodeseek-email [--poll-secs 60] [--receive-only]
maildctl account provider-add …   # 与 provider-test 相同参数，探测通过后保存
maildctl account edit --id acct_… [--display-name …] [--address …]
    [--imap-host …] [--update-imap-password] [--remove-smtp]
    [--smtp-host …] [--update-smtp-password]
maildctl account enable <acct_…>
maildctl account disable <acct_…>
maildctl account remove <acct_…>
```

`account discover` 只输出建议的 IMAP/SMTP 端点、用户名格式与授权码提示，不保存任何东西；用建议值
`account test` 探测通过后再 `account add`。`--smtp-security implicit` 即 465 风格隐式 TLS（ADR-0042）。
provider-test/provider-add 不接收 maild transport 凭据；provider 自己的 API key 由其 companion
CLI 与平台凭据库管理。其他账户命令的秘密输入规则：TTY 上提示输入并关闭终端回显；非交互模式只从 stdin 读一行。
命令行参数与环境变量永远不接受秘密，JSON 输出也不回显。enable/disable/remove
自动读取当前 revision 做乐观并发检查；并发修改会返回 `-32011`。Windows 写命令在
凭据 provider 落地前由服务端拒绝（`-32013`）。

`notification-status` 只查询，不弹权限窗口。`request-notification-authorization` 是显式授权动作；
它可能在系统窗口仍等待用户操作时返回 `checking`，此后用前一个命令重新读取状态。旧 daemon 对这
两个命令返回 `-32601`，不会影响 `status`、邮箱读取或其他既有命令。
`frontends` 对应 `listFrontends`，`plugins` 对应 `listExternalPlugins`；它们只读清单和粗粒度
生命周期。旧 daemon 对这些 additive 命令返回 `-32601`。

安装事务（ADR-0019）由 `plugin` 子命令驱动——CLI 只把包复制进受管目录的
`.incoming/<rand>` staging，再经 RPC 让 Agent 校验发布：

```text
maildctl plugin install --source <目录> [--frontend]
maildctl plugin remove <id> [--frontend]
maildctl plugin rollback <id> [--frontend]
maildctl plugin enable <id>
maildctl plugin disable <id>
```

install 输出 `{action: installed|upgraded, id, from_version?, version}`；rollback 再执行
一次即切回另一版。事务失败以 `-32017` 退出（退出码 4），坏 staging 由 Agent 或 CLI 清理，
不留现场。

成功时，`--json` 在 stdout 输出一行服务端 `result`，不增加包装层。应用错误在 stderr
输出一行稳定 JSON：

```json
{"error":{"kind":"rpc","message":"服务未装配: store（检查 [plugins.store] 配置）","rpc_code":-32001}}
```

`kind` 为 `connection` 或 `rpc`；非 RPC 错误的 `rpc_code` 为 `null`。退出码固定为：

| 退出码 | 含义 |
|---:|---|
| `0` | 成功 |
| `2` | clap 参数或用法错误；沿用 clap 的 stderr 格式 |
| `3` | 配置、连接、鉴权或超时错误 |
| `4` | JSON-RPC 错误、无效响应或响应超过上限 |

不加 `--json` 时保留面向人的文本输出；控制字符会在终端输出中被清理。IPC 连接超时
为 5 秒，单次 RPC 从写入请求到收到响应共用 30 秒总时限；HTTP 单次请求总超时为 30 秒。
