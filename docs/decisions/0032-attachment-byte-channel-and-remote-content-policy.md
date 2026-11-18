# ADR-0032：M4.5-G02 附件字节通道与远程内容策略

- 状态：已接受，M4.5-G02 已实现（hermetic e2e 钉死）
- 日期：2026-09-12
- 适用范围：`maild` 协议新增 `readAttachment` / `getReadingPolicy` / `setReadingPolicy` 三个方法与
  `-32003` / `-32004` 两个错误码、`getMessage` 应答的 additive 字段、附件元数据的 `stored` 位、
  新模块 `reading_policy`、`maild-client` 类型化 API、`maildctl attachment` / `reading-policy` 子命令
- 关联：ADR-0025（附件落库形状与上限）、ADR-0026（消息身份是 `(account, folder, uid)`）、
  ADR-0030（授权词表）、ADR-0024 决策 5（正文渲染隔离，由 M4.5-04 消费本片）、
  [交付计划](../design/tauri/IMPLEMENTATION-PLAN.md) §4 G02 行、[UX-SPEC](../design/tauri/UX-SPEC.md) §4

## 背景

ADR-0025 把附件字节存进了库（单 part ≤ 2 MiB、单封累计 ≤ 8 MiB 内联，超限只留元数据），
但**没有任何协议方法能把它们读出来**。`getMessage` 返回的 `attachments` 只有元数据；
`Store::attachment_by_content_id` 与 `attachment_content` 是给 M4.5-04 预留的库内接口，
从未暴露到 API。于是：

- `cid:` 内联图片在任何前端都渲染不出来——HTML 原位阅读（用户最在意的那件事）被卡在这里；
- 「保存附件」在任何前端都做不了；
- 设计稿要求「原文中远程图片、远程样式和网络字体默认不联网」并提供可信发件人名单，
  但内核没有这份名单的持久状态，也没有读写接口。换一个前端就换一套隐私策略，
  这与 ADR-0024「放行策略在内核侧，换前端不绕过」相悖。

同时有一条硬约束：协议单帧上限 `MAX_FRAME_BYTES` = 1 MiB（ADR-0025 决策 6 的帧预算就是它倒推的）。
2 MiB 的内联上限本身就装不进一帧，base64 之后更装不进。交付计划因此把 G02 的验收写成
「不通过 1 MiB JSON 帧塞大附件」，并把传输形式留给本 ADR 决定。

## 目标

1. 一个前端主体能按 `(account, folder, uid, part_index)` 取回已落库的附件字节，
   任何一元不匹配都拒绝；「不存在」与「存在但未存字节」可区分。
2. 任何大小（≤ 内联上限）的附件都能在不改帧上限的前提下完整取回。
3. 可信发件人名单与远程内容默认策略是 **daemon 拥有的持久状态**，重启后仍在，
   任何前端读到同一个决定。
4. 上述能力不依赖未交付的 UI 即可验证（`maildctl`）。

## 非目标（明确留给后续切片）

- **内核代取远程资源。** 内核不发起、不代理、不缓存任何 `http(s)` 请求；策略是一个「决定」，
  执行者是 M4.5-04 的渲染隔离层。见决策 6 的诚实边界。
- 超限附件按需回源（IMAP `BODY[n]` 局部 FETCH）——需要连接管理与进度模型，是独立切片。
- 新文件附件上传——G04（staging），但 G04 的写通道**沿用本片的分块形状**。
- 链接点击策略、表单提交拦截——渲染层（04）的事，内核没有对应状态。
- 按封的一次性放行（「本封加载远程图片」）——UI 会话内状态，不持久、不进内核（UX-SPEC §4）。
- 把附件字节塞进 `listMessages` / 通知帧——ADR-0025 决策 6 不变。

## 决策

### 1. 传输形式：分块 base64，走既有 JSON-RPC 帧

新增 `readAttachment { account, folder, uid, part_index, offset?, length? }`：

- 单次最多返回 `MAX_ATTACHMENT_CHUNK_BYTES` = **512 KiB 原始字节**（base64 后约 700 KiB，
  加上应答信封留有余量落在 1 MiB 帧内）；`length` 缺省即上限，取值 1..=524288；
- 应答携带该 part 的完整元数据（含 `size`）与 `offset` / `data` / `eof`，客户端按 `offset += 已收字节`
  循环到 `eof`；`offset == size` 时返回空 `data` 与 `eof = true`，让循环终止条件不需要特判零长附件；
- `offset > size` 是 `-32602`，不是「空块」——越界读是调用方 bug，不该静默成功。

考虑过并放弃的形状：

| 形状 | 放弃理由 |
|---|---|
| 单帧 base64 整体返回 | 2 MiB 内联上限本身就超过 1 MiB 帧；「小附件能用、真实照片必失败」的功能比没有更糟（ADR-0028 对上传下过同样的判断） |
| 连接升级为原始二进制流 | 打破「一行一个 JSON 对象」的框架契约，HTTP 端点没有对应形态，client 要养第二套读循环 |
| HTTP `GET /attachments/...` | 主传输是同用户 Unix socket；HTTP 端点是可选的、无通知的、恒为 `user` 的；给字节另开一条 HTTP 路等于第二套鉴权面 |
| daemon 写到导出目录返回路径 | 引入第二个存储位置，ADR-0025 决策 8 拒绝过：备份/恢复/账户删除级联全要各写一遍；路径还会泄进 webview |

代价是 33% 的 base64 膨胀与最多 5 个往返（2 MiB）。个人邮箱量级下这是本地 socket 上的毫秒级事，
换来的是**零新传输、零新鉴权面、HTTP 与 IPC 行为一致**。G04 的写通道反向沿用同一形状。

### 2. 键是完整四元组，`folder` 必填

`getMessage` 允许省略 `folder`，按「收件箱优先、其次落库序最早」消歧（ADR-0026 决策 1）——
那是给「用户点了摘要里的一封」这类读取用的便利。字节读取不给这个便利：

- `folder` 必须是非空字符串。UID 只在 mailbox 内唯一，收件箱 UID 42 与已发送 UID 42 是两封信，
  返回错误那封的附件字节是**数据串封**，比返回错误的正文更难被用户察觉；
- 消息必须**可见**（`pending_removal = 0`，ADR-0027 决策 5）：正在乐观删除/移动的信对列表不可见，
  它的附件也不该还能读出来；
- `part_index` 必须是该消息 `attachments` 里的一项。库里附件按 `(account, folder_id, uid, part_index)`
  唯一，查询本身就是四元组全匹配——没有「按 part_index 猜到别的账户」的路径。

### 3. 三种结果可区分，元数据先说实话

| 情形 | 错误码 | 名称 |
|---|---:|---|
| `(account, folder, uid)` 解析不到可见消息 | `-32002` | Message not found（沿用） |
| 消息存在，但 `part_index` 不是它的附件 | `-32003` | Attachment not found |
| part 存在，但字节未落库（超过 ADR-0025 内联上限，或迁移前的历史缓存） | `-32004` | Attachment not stored |

三者不合并：前端对「没这个附件」与「有但没存」要说不同的话（UX-SPEC §4：「尚未保存此附件」），
而「消息不存在」意味着列表已经过期。

同时给 `getMessage.attachments[]` 元数据加 additive 字段 **`stored: bool`**（= 库里
`content IS NOT NULL`；`listMessages` 与通知帧照旧不携带附件）。UI 据此决定要不要画一个能点的
下载按钮，**而不是先做一个按钮再让它失败**。旧 daemon 缺省为 `false`——不确定时不制造可点的入口。

### 4. `readAttachment` 归 `messages_read`，不新增授权词

能调 `getMessage` 的主体已经能读这封信的完整 HTML 原文、全部头部与附件元数据。附件字节属于
**同一封信的同一份内容**，不是新的权限面；把它拆成一个新词只会让每个已安装的只读前端
在升级后突然读不了 `cid:` 图片。这与 ADR-0029 决策 12「换一个检索维度不构成新权限面」是同一判断。

`getReadingPolicy` 归 `settings_read`、`setReadingPolicy` 归 `settings_write`：它们是应用级开关，
与 `getSettings` / `setSettings` 同域。默认前端 `com.maild.ui` 已声明 `settings` 路由，manifest 不动。

### 5. 远程内容策略是 daemon 拥有的文件状态，不进邮件库

模型（`reading_policy` 模块）：

```toml
format_version = 1
remote_content = "block"          # "block" | "allow"；缺省 block
trusted_senders = [
  { value = "newsletter@example.com", kind = "address", added_at = 1757600000 },
  { value = "example.org",            kind = "domain",  added_at = 1757600100 },
]
```

- `value` 一律小写存储；`kind` 由是否含 `@` 判定；地址过 `addresses::validate_address` 同一道语法门，
  域名要求非空、无 `@`、无空白与控制字符、不以点开头结尾、不含 `..`、≤ 253 字节；
- 条目上限 **500**（应答要落在一帧内，且没人会手动信任五百个发件人）；
- `setReadingPolicy { remote_content?, trust?: [..], untrust?: [..] }` 是一个**原子 patch**：任一项非法整包
  `-32602`、不落盘；`trust` 已存在与 `untrust` 不存在都是幂等 no-op；同一次 patch 里同时出现在
  `trust` 与 `untrust` 是 `-32602`；
- 文件 `reading_policy.toml` 与活动配置同目录，纪律逐条对齐 `plugin_state.toml`（ADR-0019 §3）：
  0600、拒符号链接、单硬链接、64 KiB 上限、未知 `format_version` fail-closed、临时文件 + fsync +
  原子 rename。**损坏即停摆**：读返回缺省策略且 `persisted = false`，写返回 `-32001`——
  绝不把一个解析失败的文件当成「用户没设过」然后覆盖掉。

`getMessage` 应答新增**同级**字段（不进 `message` 对象——它不是这封信的事实，是此刻的策略结论）：

```json
{ "message": { ... }, "remote_content": { "decision": "block", "reason": "default" } }
```

`reason` 取 `default` / `trusted_address` / `trusted_domain` / `allow_all`。匹配规则固定在内核：
从 `From` 头抽第一个 addr-spec（复用 `compose::extract_addresses`），小写后先比地址、再比**精确**域名。
子域不匹配父域：`news.example.com` 的信不因用户信任了 `example.com` 而放行——用户在界面上信任的
是他看到的那个域，静默放大到整个域树是替他做决定。`From` 解析不出地址时永远是 `default`。

**为什么不进 SQLite（schema v8）**：这是隐私偏好，不是邮箱数据。把它放进邮件库意味着
`maild restore` 一份旧备份会**静默恢复旧的信任名单**——用户上周撤销了对某发件人的信任，
恢复备份后它又回来了，而用户不会想到「恢复邮件」会动隐私设置。反过来，重装后信任名单要重建
是可接受的（它本来就是随手可补的偏好）。运行时 rules 已经有「设置不在库里」的先例。
代价如实记录：`maild backup` **不覆盖**这个文件，它随配置目录走。

### 6. 诚实边界：内核给决定，不给保证

ADR-0030 决策 1 的那句话在这里同样成立：这不是沙箱。

- 内核**不知道**渲染层实际加载了什么。`remote_content.decision` 是内核的结论，执行它是 04 的
  隔离文档与请求拦截的责任，验收也在那里（「网络请求计数证明默认拦截」）。
- 一个 `user` 主体或任何持有 `settings_write` 的前端可以把 `remote_content` 改成 `allow`。
  本片挡的是「换前端换策略」与「策略只活在某个前端的 localStorage 里」，不是恶意前端。
- `readAttachment` 拿到的字节交给前端之后怎么落盘（文件名清洗、重名处理、不删已有目标）是
  前端与 `maildctl` 各自的责任；协议只保证字节与 `size`、`mime_type` 一致。

### 7. `maildctl` 让 G02 不依赖 UI 即可验证

- `maildctl attachment save --account A --folder F --uid U --part N --out PATH`：分块读取（共享
  client 已核对实收字节数 = `size`）到同目录临时文件、fsync，再以 `hard_link` 发布到目标——
  它在目标已存在时原子地失败，因此**绝不覆盖**（UX-SPEC §4「失败不删除已有目标」在 CLI 上同样成立）。
- `maildctl reading-policy show | set --remote-content block|allow | trust <v>... | untrust <v>...`。

### 8. 共享 client 的类型化 API

`maild-client` 新增 `read_attachment_chunk`（单块）与 `read_attachment`（循环拼装，带两道护栏：
应答 `size` 中途改变即报错；累计字节超过首块宣称的 `size` 即报错——一个说谎的服务端不能让
客户端无界分配）、`reading_policy` / `set_reading_policy`，以及 `MessageResult.remote_content`
（`Option`，旧 daemon 缺省 `None`）。DTO 全部 `serde(default)`，旧 client 读新 daemon 忽略新字段。

## 兼容性

- 协议：三个新方法、两个新错误码、`getMessage` 应答一个新同级字段、附件元数据一个新布尔字段，
  **全部 additive**；`PROTOCOL_VERSION` 不提升。旧 client 对 `-32003`/`-32004` 按未知 rpc 错误处理，
  与它们此前对「没有这个方法」的处理同级。
- store：**无 schema 变更**。`list_attachments` 多选一列 `content IS NOT NULL`；新增 `read_attachment`
  查询在同一个锁内先验消息可见性再取 part。
- 授权：策略表新增三行；`method_policy_table_pins_every_dispatched_method` 同步。已安装包的有效
  权限逐字节不变（`messages_read` 本就覆盖 `getMessage`）。
- 文件系统：配置目录新增 `reading_policy.toml`（仅在首次 `setReadingPolicy` 时创建；只读从不落盘）。

## 测试策略（hermetic）

- `mime` / `store`：解析出的附件 `stored` 与 `content.is_some()` 一致；`list_attachments` 回读 `stored`；
  `read_attachment` 四种结果（消息缺失 / part 缺失 / 未存 / 已存）与 `pending_removal` 不可见。
- `reading_policy` 模块：缺省文件 → 缺省策略；存取往返；地址/域名校验；上限；符号链接、宽权限、
  未来 `format_version`、超长文件 → 停摆且不写；匹配规则（地址优先、精确域、子域不匹配、大小写）。
- `api` 单测：策略表钉三个新方法；`readAttachment` 参数校验、分块边界（`offset`/`length`/`eof`/
  零长）、**最大块的应答帧 ≤ 1 MiB**、三个错误码；`getMessage.remote_content` 四种 `reason`；
  `setReadingPolicy` 原子拒绝、幂等、冲突。
- `tests/attachments.rs` e2e（真 daemon + 预置 SQLite）：声明 `messages_read` 的前端主体跨块取回
  1.2 MiB 附件且字节逐一相等；错账户/错文件夹/错 part 分别 `-32002`/`-32003`；超限 part `-32004`；
  没有 `settings_write` 的前端改策略得 `-32016`；`user` 信任发件人后 `getMessage` 决定翻转；
  **daemon 重启后策略仍在**；损坏的策略文件不被覆盖。
- `maild-client`：`read_attachment` 对说谎服务端（`size` 变化 / 超量）报错而不是无界拼装。

## 后果

正：`cid:` 与「保存附件」第一次有了协议路径；M4.5-04 只剩渲染隔离本身；隐私策略从此有唯一
权威副本，换前端不换规则；G04 的写通道有了现成形状可沿用；`maildctl` 能在没有任何 UI 的机器上
验收整条链。

负：base64 膨胀与多次往返（本地 socket 上不可感知，但它是真实开销）；配置目录多一个不在
`maild backup` 覆盖范围内的状态文件；`stored: false` 缺省意味着旧 daemon 上的新前端不会提供
下载入口——这是有意的，宁可少一个按钮也不要一个必失败的按钮。

## 参考

- ADR-0025 决策 6（帧预算）、决策 7（附件内联上限）、决策 8（拒绝文件系统存附件）
- ADR-0026 决策 1（UID 只在 mailbox 内唯一）、ADR-0027 决策 5（`pending_removal` 不可见）
- ADR-0030 决策 1（边界的真实含义）、决策 8（归类判断）
- ADR-0019 §3（状态文件纪律）、ADR-0024 决策 5（渲染隔离）
- [UX-SPEC.md](../design/tauri/UX-SPEC.md) §4、[IMPLEMENTATION-PLAN.md](../design/tauri/IMPLEMENTATION-PLAN.md) §4
