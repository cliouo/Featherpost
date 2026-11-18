# NodeSeek Email provider 验收记录

本文只记录 Maild 社区维护的 NodeSeek Email 适配器证据。该适配器不是 NodeSeek 官方产品，
也不代表 NodeSeek 背书。凭据、完整邮箱地址、收件人、主题、正文和上游原始响应不得写入仓库。

## 当前状态

| 层级 | 状态 | 边界 |
|---|---|---|
| 仓库实现与 hermetic 验证 | 已完成 | provider、companion CLI、宿主私有插件数据目录、持久 synthetic uid、限流和不确定结果语义均有本地假服务或 e2e 覆盖 |
| 直接 OpenAPI 只读侦察 | 已完成（2026-08-23） | 只读 mailbox list 与 quota，不作为插件 live acceptance |
| 插件 live 只读验收 | 待执行 | 只允许 list/quota，不创建邮箱、不发信 |
| 插件 live mutation 验收 | 待用户逐次确认 | create 必须确认具体地址；send 必须确认具体发件账户和收件人 |

仓库实现完成不等于真实插件验收通过。Cloudflare/DNS catch-all 方案已冻结，也不属于本次验收。

## 2026-08-23 直接只读侦察

本次侦察直接调用用户提供的 NodeSeek OpenAPI，只执行只读请求，没有运行插件的安装、认证或
transport 闭环：

- mailbox list 返回 21 个邮箱，其中 14 个为 `@nodeseek.org`，7 个为 `@seek.li`；
- quota 响应可解码 `current` 与 `storage` 字段；
- 连续只读请求约到第 20 次时观察到 HTTP 429，这只是当次现象，不能写成稳定限额；
- 没有调用 create、send 或其他远端变更接口。

仓库只保留上述汇总，不记录邮箱完整地址、API key 或原始响应。这批证据证明当时的基础字段和
限流现象可观察，不证明当前服务状态，也不算插件 live acceptance。

## 仓库内验证

插件测试使用 loopback 假服务和测试专用 secret buffer，不访问 macOS Keychain 或真实 NodeSeek 服务：

```sh
cargo test -p maild-provider-nodeseek-email --all-targets --locked
cargo clippy -p maild-provider-nodeseek-email --all-targets --locked -- -D warnings
cargo test -p maild --test plugin_identity --test plugin_transactions --test plugin_transport --locked
```

检查范围包括固定生产 origin、禁用环境代理与 redirect、Authorization 不跨 origin、响应体和
超时上限、optional 字段 fail-closed、429 gate、只读有界重试、create/send 不重放、create
reconciliation、send `delivery_unknown`、synthetic uid 跨重启稳定、未来 schema 拒绝打开、
持久分页扫描、远端 404 tombstone、provider authenticate→register→probe→receive→send、
alias From 拒绝、receive 总时限和连续 partial、additive transport 字段与凭据保留键边界、
宿主写锁内 provider/address 唯一性、stale binding 的 draft 拒绝、import 的 live catalog
精确校验，以及 daemon-guarded 本地 unimport 级联；package 升级、回滚、移除后私有数据仍保留。

## 插件 live 只读验收

API key 只能通过无回显 TTY 或 `--api-key-stdin` 写入 macOS Keychain。禁止放入 argv、环境变量、
配置文件、日志或测试产物。完成本地配置后，先执行 companion CLI 的只读检查：

companion binary 位于已安装 package 的 `bin/nodeseek-email`，不会被安装流程隐式复制到全局
`PATH`。用 `maild paths` 查出 `plugins=` 根后执行：

```sh
NODESEEK_CLI="<plugins path>/app.maild.provider.nodeseek-email/bin/nodeseek-email"
"$NODESEEK_CLI" key-status
"$NODESEEK_CLI" mailbox list
"$NODESEEK_CLI" quota
maildctl plugin enable app.maild.provider.nodeseek-email
```

`mailbox list` 的原始输出含完整邮箱地址，只能在本机查看。提交证据时只记录日期、插件版本、
邮箱总数、两个域名的数量、quota 字段是否可解码、HTTP 状态与是否遇到限流。

还可以显式启用只读 live test。它从 Keychain 读取凭据，只调用 list/quota：

```sh
NODESEEK_LIVE_READ_ONLY=1 cargo test -p maild-provider-nodeseek-email \
  --features live-tests live_read_only_list_and_quota -- --ignored --nocapture
```

通过以上步骤后，再用一个已存在邮箱执行 `maildctl account provider-test`，确认 supervised
provider 已注册且 probe 成功。该步骤不创建远端邮箱。需要保存证据时，只记录脱敏后的结果和
错误码，不复制账户地址。

## 已归档账户的本地 binding

`maildctl account remove` 是软归档，不自动删除第三方插件状态。若同一远端地址需要用于新的
Maild account，先确认旧账户已归档、runtime 已停止，再执行：

```sh
"$NODESEEK_CLI" mailbox unimport <old-account-id> <address> --confirm
```

命令拒绝 Maild 配置、API endpoint/token 和插件数据目录的环境覆盖，以同一套平台受管路径发现
daemon 与插件私有 SQLite。认证后的本地 Maild API 必须返回精确 account ID 且已 archived；待
解除 address 必须匹配插件持久 binding，该 binding 在 profile 已迁移 provider/address 时仍是旧
地址的权威。随后才会级联删除旧 account 的 synthetic-UID 状态。它不读取 Keychain，也不访问或
删除 NodeSeek 远端邮箱；没有离线破坏性绕过，daemon、catalog 或 RPC/schema 不可验证时必须先
修复，命令保持 fail-closed。

随后重新运行 `maildctl account provider-add`，新 account ID 会获得独立 UID 命名空间。若省略
unimport，draft probe 会在远端网络访问前返回 conflict。宿主本身还会在账号写锁内拒绝两个未
归档 profile 使用相同 provider/address，因此并发 draft 与 outgoing-only 账户也不会留下一个到
首次收发才暴露故障的账户。

`mailbox import` 同样拒绝所有配置、endpoint 与插件数据目录环境覆盖；写入前必须由平台受管
daemon 返回精确、未归档、地址相同且 incoming provider 为 NodeSeek Email 的账户，防止拼错 ID
形成无法正常回收的孤儿 binding。

## 远端变更门禁

测试 key 的存在不授权远端变更。每一次 create 都要先取得用户对具体邮箱地址的明确确认；
每一次 send 都要先取得用户对具体发件账户、收件人和测试目的的明确确认。确认不能从上一轮
操作沿用，也不能把“继续验收”解释成对任意目标的长期授权。

create 与 send 都不自动重放。create 在响应丢失时只允许按 exact address 做一次 reconciliation；
无法证明结果时记为不确定，不再次创建。send 在请求写出后失去响应时记为 `delivery_unknown`，
不自动补发。

NodeSeek companion CLI 没有远端 delete 命令。删除 Maild 账户、移除插件、升级或回滚 package
都不得删除远端邮箱；当前也不存在可执行的远端删除验收项。
