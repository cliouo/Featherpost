# ADR-0023：M4-07 NodeSeek Email 第三方 provider

- 状态：冻结（2026-08-23）
- 关联：ADR-0017（插件身份）、ADR-0019（安装事务）、ADR-0021（transport provider）、ADR-0022（地址层）；ROADMAP M4-07

## 背景

NodeSeek Email 通过 `seek.li` OpenAPI 提供 `@nodeseek.org` 与 `@seek.li` 邮箱、收信和发信能力。
它不是 IMAP/SMTP 账户，不能塞进内置 transport；M4-05 的反向 provider 缝允许把这种远端 API
适配放在独立进程里。本切片是第一个随仓库交付的第三方业务插件，也用来验证 package、
supervision、账号目录与 provider 合同能否在不写平台特例的前提下闭环。

插件稳定 ID 为 `app.maild.provider.nodeseek-email`。名称和文档必须明确它是 Maild 社区适配器，
不得暗示 NodeSeek 官方出品或背书。

## 目标

1. 以独立 Rust crate 交付 supervised provider 与 companion CLI。
2. 支持邮箱 list/check/create/import/unimport/quota，以及纯文本单收件人的 probe/receive/send 基础闭环。
3. API key 只进入插件自己的 macOS Keychain 项；不进入仓库、账户目录、argv、环境、普通日志或错误响应。
4. 插件数据与可升级 package 分离，升级、回滚、移除 package 均保留本地映射与凭据；任何本地动作都不删除远端邮箱。
5. 用 hermetic 假服务证明协议、限流、游标与失败语义；真实验收默认只读。

## 非目标

- Cloudflare catch-all、DNS/MX、域所有权验证或 SPF/DKIM；原 M4-07 DNS 自定义域方案冻结，另行排期。
- 远端邮箱 delete/release/restore。
- HTML、附件、多收件人、抄送/密送、文件夹同步、标签、联系人、过滤器、签名和转发设置。
- 桌面引导、自动下载/签名分发、Windows Credential Manager。
- 把 NodeSeek 写进内核静态 provider 注册表或把远端邮箱建模为 M4-06 `AddressRecord`。

## 决策

### 1. 一个远端邮箱对应一个受管 AccountProfile

账号 `address` 保存真实 `@nodeseek.org` 或 `@seek.li` 邮箱；incoming/outgoing 设为
`kind="plugin"` 并绑定本插件 ID，maild 侧 credentials 为空。已有远端邮箱通过 companion CLI
列出后导入，创建新邮箱是显式远端变更。M4-06 alias 仍是本地从属地址；NodeSeek 基础发信不
承诺 alias 能被远端接受。

为此，M4-05 三个反向请求作 additive 扩展：

| 方法 | 新合同 |
|---|---|
| `transport/probe` | 增 `address`；draft 阶段 `account` 仍为空串 |
| `transport/receive` | 增 `address`，用它定位远端 mailbox |
| `transport/send` | 增 `account_address`；`from` 仍可能是已验证 alias |

本插件要求 `from == account_address`，否则以业务错误拒绝；这样不会把 alias 当作远端 mailbox，
也不会发送平台未授权的 From。旧 provider 可忽略新增键，协议代数不提升。
本插件同样忽略未来 additive transport 字段，但会在解码前递归拒绝 token、password、secret、
Authorization 等凭据类保留键，确保宿主升级不能意外把秘密跨进程传给 provider。

### 2. 固定生产网络边界

生产 origin 固定为 `https://seek.li/openapi/v1`：禁用 HTTP redirect、禁用系统/环境代理、使用
系统 CA、设置连接/请求超时和响应体上限。仅 `cfg(test)`/测试构造器可注入 loopback origin；
生产 CLI 不提供 `--base-url`。Bearer 只加到该固定 origin 的请求。错误文本、日志和 SQLite
均不得包含 Authorization 值。

NodeSeek OpenAPI 的基础映射：

- `GET/POST /mailboxes`：列表与创建；
- `POST /mailboxes/check?address=...`：地址可用性；
- `GET /mailboxes/{id}`：导入/缓存复核；
- `GET /mailboxes/{id}/mails?folder=inbox&limit&offset` 与 `GET /mails/{id}`：收信；
- `POST /send`：纯文本单收件人发信；
- `GET /quotas`：当前配额视图。

OpenAPI 字段多为 optional，解码必须显式校验执行当前动作所需字段，不能用空字符串/0 猜测身份。
错误对象 `{error:{code,message}}` 只以有界、去控制字符的摘要上报。
2026-08-23 只读侦察还观察到快照未声明的顶层 `current` quota 字段；适配器只在它是有界
标量或扁平标量对象时保留并展示，其他未知复杂结构 fail-closed，`api-1.json` 不因此被改写。

### 3. API key 与可变数据归插件所有

companion CLI 仅从无回显 TTY 提示或 `--api-key-stdin` 读取 API key，凭据从首字节起就由
zeroizing buffer 持有，成功、格式错误和部分读取失败都会清零；不接受 `--api-key`、环境变量或
配置字段。macOS 使用 generic-password Keychain，
service 为 `app.maild.provider.nodeseek-email.credentials.v1`，account 使用固定的 provider profile
标识。Keychain 缺失/锁定时 fail-closed，provider 不注册可用状态。

宿主新增 `MAILD_PLUGIN_DATA_DIR`，指向按受检 package ID 隔离的 0700 私有目录。插件在这里维护
SQLite：mailbox address→remote id 缓存、remote mail id→synthetic uid 映射、schema version。
package 升级/回滚/移除只处理 package 根，不处理 data 根；本地 remove 也绝不调用远端 delete。
未来 schema 版本必须在写入前拒绝打开。

Maild 的账户 remove 是软归档，不能隐式耦合一个可能离线的第三方 provider，因此也不会删除
插件 binding。若要把同一远端地址交给新的 Maild account，必须先归档旧账户，再显式执行
companion CLI 的本地 `mailbox unimport <old-account> <address> --confirm`。该命令拒绝 Maild 配置、
API endpoint/token 和插件数据目录的环境覆盖，以同一套平台受管路径发现 daemon 与私有状态；
认证后的本地 Maild API 必须返回精确 account ID 且已 archived。待解除 address 则以插件持久
binding 为权威，即使已归档 profile 后续迁移过 provider 或 address 也能安全释放旧 binding。
全部核验通过后，命令才在单个事务里删除 binding，并由外键级联该旧账户的 synthetic-UID 状态；
它不读取 Keychain、不访问 NodeSeek、不删除远端邮箱，也没有离线破坏性绕过。daemon 不可验证时
必须先恢复平台受管 daemon，所有 endpoint、catalog 与 RPC/schema 异常都 fail-closed。

宿主还在 account `mutation_lock` 内对所有插件 transport 执行通用 `(provider id, canonical address)`
唯一性检查：忽略 archived profile 与正在更新的自身，覆盖 incoming/outgoing，并在 probe、凭据写入
和 catalog 发布前以 conflict 拒绝。这消除了两个新 draft 或长期 outgoing-only profile 同时占用
同一远端地址的窗口。插件自身的 draft probe 仍在访问远端前检查持久 address owner，拦住已归档
但尚未 unimport 的旧 binding。不得对仍活跃的同一 account 做 unimport/rebind，否则宿主保留的
游标与重置后的 UID 命名空间不兼容。

### 4. 稳定游标与分页

NodeSeek 远端 mail id 的单调性和列表顺序没有协议承诺，不能直接当 M4-05 uid。插件为每个
mailbox 在 SQLite 中持久分配严格递增 synthetic uid，并保持 remote id 唯一映射。receive
先排空已发现队列；队列为空时按持久 `scan_offset` 只抓一页、去重并推进扫描位，再补详情并按
synthetic uid 升序返回 `since_uid` 之后最多 `max` 条。这样大邮箱不会在补第一封详情前耗尽
整轮请求额度，重启后也从已持久的位置继续；扫描到末页后回绕，以适应远端排序变化。详情
404 会把原映射持久标为 tombstone 而不复用 uid，避免一封已远端删除的邮件永久堵塞后续游标。
重启、重复页和远端排序变化都不得改变既有 uid。响应仍受 1 MiB JSON-RPC 帧约束。

### 5. 限流、重试与不确定结果

所有命令与 provider 请求共享一个进程内 rate gate；读取 `X-RateLimit-*`（若服务返回）并在
429/`Retry-After` 时推进闸门。只读 GET 可在总 deadline 内做有界重试；创建和发信不自动重放。
一次 receive 另有 40 秒 operation-wide 硬上限，并预留 8 秒用于有界编码和回包；在详情阶段已
取得连续进度时遇到时限/网络/限流会返回该前缀，没有进度则返回稳定 timeout。它始终早于宿主
60 秒 provider 上限结束，避免一条慢请求占住串行 provider 连接并丢失迟到响应。

- create 在请求已写出但响应丢失时，只通过 exact address 的 list/check 结果做一次 reconciliation；
  不能证明时返回不确定结果，不再创建第二次。
- send 在请求已写出后失去响应时返回稳定业务错误 `delivery_unknown`；绝不自动重发。

这避免在限流或网络抖动下重复创建邮箱/重复投递邮件。论坛等级对应的每周创建额度不是稳定
API 合同；插件展示 `/quotas` 和服务端错误，不在本地硬编码“15/20 个”。

### 6. 安装、账号 CLI 与远端变更确认

通用 `maildctl account provider-test/provider-add` 负责测试或落盘 plugin account；它不接收
API key。NodeSeek companion CLI 负责 key set/status、mailbox list/check/create/import/unimport/quota。
`create` 和 `send` 是远端 mutation，自动化/验收不得仅因有测试 key 就执行；每次需要用户对
具体地址/收件人另行确认。默认 live acceptance 只允许 list/quota。

## 安全与信任边界

- external 插件仍与当前用户同一 OS 信任域，能看到经其收发的邮件正文；M5 沙箱未实现。
- API key 已在聊天中出现过，但实现、测试和文档不得复制它。建议用户按自己的轮换策略更换。
- `no_proxy` 防止 bearer 被环境代理转发；固定 origin + 禁 redirect 防止跨站泄露。
- 日志只记录 operation、HTTP status、provider error code、mailbox/account 的本地 ID；不记录 key、地址列表、收件人、主题、正文或完整上游响应。
- OpenAPI 没有承诺 mail id 单调、429 结构或配额重置时刻；上述边界一律按保守路径处理。

## 测试与验收

Hermetic：

- 固定 origin/禁 redirect/禁 proxy、Authorization 只到 loopback 假服务、响应大小和超时；
- optional 字段、分页重复/乱序、synthetic uid 跨重启稳定、未来 schema fail-closed；
- 429 gate、只读有界重试、create/send 不重放、create reconciliation、send `delivery_unknown`；
- secret buffer、输入上限与脱敏用测试值覆盖；CLI parser 证明不存在 argv/env key 入口，真实
  Keychain 只留给显式 live 验收；
- provider 长连接 authenticate→register→probe/receive/send，alias From 拒绝；
- receive operation deadline 与连续 partial、additive 字段兼容和凭据类保留键拒绝；
- 宿主写锁内的 provider/address 唯一性与并发 draft；stale owner 在 draft probe 阶段拒绝；
- 手动 import 必须通过平台受管 daemon 精确验证未归档账户、地址和 NodeSeek provider；
- 手动 import 先通过受管 daemon 核验未归档账户、精确地址和 NodeSeek incoming provider；
- unimport 拒绝路径/endpoint 环境覆盖，经同一受管 daemon 核验精确归档 ID，并以持久 binding
  校验 address 后精确级联 UID；daemon 不可验证时 fail-closed；
- package upgrade/rollback/remove 后 data 仍存在且无远端 delete 请求。

真实验收分层：先运行 list/quota 只读检查，再经用户对具体 mutation 明确确认后才可 create/send。
证据只记录日期、插件版本、域名分布、数量/配额字段是否可解码、HTTP 状态与限流行为，不记录
邮箱完整地址、key 或正文。

## 后果

- M4-07 从“DNS 自定义域”重排为“NodeSeek Email 第三方 provider”；前者保持冻结，不算交付。
- 通用 provider 合同获得地址定位键，后续 Resend/Mailgun/JMAP 等 API provider 可复用。
- package 与 mutable data 正式分离，未来带状态插件不再把数据库放进可替换目录。
- 第一版刻意保持基础能力；高级 NodeSeek API 只有在真实收益和独立 ADR/测试合同明确后再扩展。

## 参考

- NodeSeek Mail 公开 OpenAPI 规范：`https://seek.li/api/openapi.json`（本地离线快照置于 `.gitignore` 忽略的 `reference/nodeseek-email/openapi.json`，不纳入仓库）
- ADR-0021 §4（provider 反向请求）
- ADR-0022 §6（alias `from` 语义）
