# ADR-0026：M4.5-02 文件夹与增量同步

- 状态：已接受（2026-08-29）
- 关联：ADR-0007（`user_version` 迁移框架）、ADR-0018（方法授权表）、ADR-0021（委托收信）、
  ADR-0025（消息模型与 schema v3）；ROADMAP M4.5-02

## 背景

M4.5-01 把一封邮件在库里存完整了，但它仍然只存在于**一个**信箱里：`receive` 从头到尾只
`SELECT` 配置里的那一个 mailbox（`ReceiveConfig::mailbox`，默认 `INBOX`），库里没有文件夹概念，
协议也没有任何按文件夹检索的入口。已发送、草稿、垃圾邮件、归档全部不可见——一个只能看
收件箱的客户端不是邮箱客户端。

同一条路径上还有一个规模问题：每次连上（含每次断线重连）都执行 `UID FETCH 1:*` 拉取整个信箱，
靠一个内存 `HashSet` 加 `(account, uid)` 唯一索引去重。邮箱越大越贵，而重连在移动网络下是常态。
真正需要的是 per-folder 的 UID 游标。

## 目标

1. 服务器上的全部 mailbox 可见、可选、可按文件夹翻页。
2. 收信改为**增量**：只抓游标之后的 UID，重连不重下全量。
3. `UIDVALIDITY` 翻转能被识别并正确处理，不产生重复行也不产生指错的 UID。
4. 连接预算有明确上限，且这个上限对得起 headless 资源观。
5. 协议 additive：新增 `listFolders`，`listMessageSummaries` 增可选 `folder`；代数不提升。

## 非目标

- **写路径**：`STORE` 标记、`MOVE`/`COPY`/`EXPUNGE`、`pending_ops` 全部归 M4.5-05。本片只读。
- **标记位对账**：本片在**首次抓取**时记录 `\Seen`/`\Flagged`/`\Answered`/`\Deleted`，
  但不追踪此后的变化——服务器改标记不改 UID，UID 游标看不见它。全量对账与 CONDSTORE
  `MODSEQ` 短路归 M4.5-05。
- **文件夹的创建、重命名、删除**：只读同步，不改服务器的文件夹结构。
- **`LSUB` / 订阅**：一律 `LIST` 全量，不按订阅过滤。
- **线程与搜索**：M4.5-07。
- **本地删除服务器上已消失的文件夹**：见决策 8 的诚实边界。

## 决策

### 1. 一封邮件的身份是 `(account, folder, uid)`

IMAP 的 UID 只在 mailbox 内唯一。v3 的 `idx_messages_account_uid` 会把收件箱的 UID 42 和
已发送的 UID 42 当成同一封——这是多文件夹落地前必须先拆掉的假设。schema v4 把唯一键换成
`(account, folder_id, uid)`。

**同一封信出现在两个文件夹 = 两行。** 不做「`Message-ID` 去重 + `message_folders` 多对多」：
`Message-ID` 既不保证存在也不保证唯一（转发器和邮件列表会重写），而 IMAP 自己的移动语义本来
就是「行随文件夹走」——`MOVE` 等于目标 mailbox 里出现一个新 UID、源 mailbox 里那个 UID 被
`EXPUNGE`。M4.5-05 的移动因此是删一行插一行，而不是改一条关联。跟着协议的模型走，比自建一个
更聪明但要靠不可靠标识符维系的模型可靠。

### 2. schema v3 → v4

- `folders` 增 `display_name TEXT NOT NULL DEFAULT ''`（解码后的展示名）与
  `present INTEGER NOT NULL DEFAULT 1`（最近一次 `LIST` 是否仍列出它）。
  `name` 保持**服务器线上的原名**（可能是 modified UTF-7），因为它同时是 `SELECT` 的实参
  和唯一键。**不做回编码**：只解不编，回编码是 bug 的温床。
- `messages` 增 `folder_id INTEGER NOT NULL DEFAULT 0`。
- 删 `idx_messages_account_uid`，建 `idx_messages_account_folder_uid`（唯一）。
- 建 `idx_messages_folder_sort(folder_id, sort_at DESC, id DESC)`。
- 回填：为 `messages` 里出现过的每个 account 建一行 `INBOX` / `role='inbox'` 的 folder，
  把该账户的历史消息全部挂上去。

回填是**断言而不是推断**：v4 之前 `receive` 只 `SELECT` 一个 mailbox，委托 provider（ADR-0021）
也只有一个信箱，所以历史行确实来自单一信箱。如果用户曾把 `ReceiveConfig::mailbox` 配成
`INBOX` 以外的值，那些历史行会被记到 `INBOX` 名下。代价是这一个文件夹需要一次重拉，
不是数据损坏——写在这里，不悄悄发生。

### 3. 连接预算 = 每账户恒一条

IMAP `IDLE` 只对**当前选中的** mailbox 生效。要对 N 个文件夹同时推送就要 N 条 TLS 长连接，
一个五文件夹三账户的普通用户会变成十五条常驻连接。这与「headless 内核是一个后台进程而不是
一个连接池」的资源观直接冲突。

决策：**每账户恒一条连接**。`IDLE` 只挂在主文件夹（`role = 'inbox'`）上；其余文件夹在周期性
sweep 中同步——`DONE` 退出 IDLE → 逐个 `EXAMINE` + 增量抓取 → 重新 `SELECT` 主文件夹 →
重新 `IDLE`。sweep 周期为 `folder_poll_secs`（默认 300 秒）。

代价是非收件箱最长有一个 sweep 周期的延迟。这是正确的取舍：没有人需要「归档」被实时推送。
「多文件夹并行 IDLE 的连接预算有上限」这条验收因此以最强的形式满足——上限是 1。

同步的文件夹数另设硬上限 `MAX_SYNCED_FOLDERS = 64`，防一个病态账户（或恶意服务器）用几千个
mailbox 把 sweep 拖垮。超出部分不同步，`listFolders` 仍然列出。

### 4. 增量抓取用 per-folder UID 游标

`folders.last_seen_uid` 保存该文件夹已处理的最大 UID，抓取范围是 `{last+1}:*`。首次同步
（游标为空）才是全量。

会话内仍保留一个内存 `seen` 集合，但它的职责收窄为「同一次会话内 IDLE 唤醒重叠时不重复推送」，
不再是去重的主力。跨重启的去重仍由 `(account, folder_id, uid)` 唯一索引兜底。

`UID FETCH` 的属性从 `BODY.PEEK[]` 扩为 `(UID FLAGS BODY.PEEK[])`：标记位在首次抓取时一并记录。
这不改变 `BODY.PEEK` 不置 `\Seen` 的性质。

### 5. `UIDVALIDITY` 变化 = 该文件夹全量重建

`SELECT`/`EXAMINE` 返回的 `UIDVALIDITY` 与库里记录的不同，且库里记录非空 → 该文件夹的
**所有消息行与附件删除、游标清零**，然后当作新文件夹全量抓取。

理由：`UIDVALIDITY` 翻转的定义就是「UID 空间已重编号」，旧 UID 与新 UID 之间没有任何对应关系。
保留旧行只有两种结局——同一封信两行，或者某个 UID 指向另一封信。删掉是唯一正确的选择。

重建在单个 IMMEDIATE 事务内完成（删消息 + 删附件 + 清游标 + 写新 `uid_validity`），
中途崩溃则整体回滚，下次连接重放。

服务器不返回 `UIDVALIDITY`（RFC 允许）时不重建也不抓取该文件夹：没有 UID 空间的稳定性保证，
增量游标就没有意义，宁可跳过并在状态里说明。

### 6. role 映射优先 RFC 6154，名字启发式只是兜底

`LIST` 返回 `\Sent`/`\Drafts`/`\Trash`/`\Archive`/`\Junk`/`\All`（RFC 6154 SPECIAL-USE）时直接采信。
不支持 SPECIAL-USE 的服务器仍然很多，退到名字匹配：大小写不敏感的常见英文名，加上常见中文名
（已发送/草稿箱/已删除/垃圾邮件 等）。`INBOX` 恒为 `inbox`（RFC 3501 保留名，大小写不敏感）。

都匹配不到 → `role` 为空串。该文件夹照常同步，只是前端不给它特殊图标。**不猜**——把
「客户/项目」这类用户自建文件夹猜成归档，比没有图标糟糕得多。

`\NoSelect` 的名字（纯层级节点）登记但不同步。

### 7. mailbox 名的 modified UTF-7 自己解

RFC 3501 §5.1.3 的 modified UTF-7：`&` 转义、base64 变体用 `,` 代替 `/`、`&-` 表示字面 `&`。
国内服务商（163、QQ）的中文文件夹名全走这个编码，不解就是一串 `&XfJT0ZAB-`。约 80 行，
不引入依赖——理由与 ADR-0025 的 HTML→文本一致：需要的是一个窄函数，不是一个库。

解码失败（非法 base64、孤立 `&`）时**回退为原名**而不是报错：一个显示成乱码的文件夹，
比一个因为名字编码不合规就整账户同步失败的客户端好。

### 8. 服务器上消失的文件夹不删本地数据

`LIST` 是「服务器上现在有哪些文件夹」的权威来源，但它**不是**删除本地邮件的授权。
不在最近一次 `LIST` 结果里的文件夹：`present` 置 0、不再同步、`listFolders` 照常列出并带上
这个标记，消息行原样保留。

诚实边界：因此**文件夹重命名会产生两条记录**（旧名 `present=0` 保留历史，新名从零同步）。
自动识别重命名需要 `UIDVALIDITY` + UID 集合比对，属于启发式，会在猜错时删掉用户邮件。
留给后续切片，或者留给用户手动清理——但绝不由本片替用户猜。

### 9. 只有收件箱走过滤与通知管线

`MailReceived` waterfall → `MailFiltered` → store / OTP / 通知 这条链是为「新到的信」设计的。
把已发送、垃圾邮件、废纸篓的历史邮件灌进去会产生大量错误通知和 OTP 误报。

决策：**`role = 'inbox'` 的文件夹走既有 waterfall + emit 链路（行为完全不变）；其余文件夹由
同步路径直接 `store.insert_message` 落库**，不经过 filters、不触发 OTP、不发通知。

这条规则同时说明了为什么 rules 仍然是 per-account 而不需要 per-folder：规则的作用对象是
「收进来的信」，而收进来的信只有一个入口。

### 10. 协议 additive

- 新增 `listFolders { account? } → { folders: [...] }`，在 ADR-0018 的方法策略表内登记为
  `Capability("messages_read")`，与其余读取方法同域。
- `listMessageSummaries` 增可选 `folder`（IMAP 线上原名）。省略 = 不按文件夹过滤，
  旧客户端行为逐字节不变。
- `MailMessage` / `MessageSummary` 增 `folder`（线上原名）与 `folder_role`，
  `#[serde(default)]` + 空值省略。
- 委托 provider（ADR-0021）不受影响：它们没有文件夹概念，消息落进账户的合成 `INBOX`。
  `transport/receive` 合同不变。

### 11. 同步路径绝不跨 await 持有 store 强引用

实现期间发现的一个真实约束，记在这里而不是留在代码注释里。

受管账户的停止是**尽力而为**的：`AccountManager::stop_runtime` 发出关断信号后只等
`RUNTIME_STOP_TIMEOUT`，超时就放弃 join，任务可能继续存活。这是刻意的设计——一个卡住的
IMAP 服务器不该挡住守护进程退出。

代价是：收信循环里任何长期持有的 `Arc<Store>` 都会把**数据库文件锁**扣在一个已经被放弃的
任务里，让下一次启动打不开库（ADR-0009 的维护锁按设计 fail-closed）。这在 M4.5-02 之前不成立，
因为收信路径根本不碰 store——所有落库都发生在 `MailFiltered` 的订阅者一侧。

规则：同步路径按操作现取 store，用完即放，不跨任何 `.await` 持有。代价是每条消息多一次
服务容器查找，与 MIME 解析相比可以忽略。

## 备选方案与取舍

| 方案 | 为何不选 |
|---|---|
| 每文件夹一条 IDLE 连接（带数量上限） | 上限本身就是承认这个模型不对。五文件夹三账户 = 15 条常驻 TLS 连接，与 headless 资源观冲突；而非收件箱根本不需要推送 |
| `message_folders` 多对多 + `Message-ID` 去重 | `Message-ID` 不保证存在也不保证唯一；IMAP 自己的移动语义就是「行随文件夹走」。跟协议的模型走 |
| `UIDVALIDITY` 变化时保留旧行、只重置游标 | 旧 UID 与新 UID 无对应关系，必然产生重复行或指错的 UID |
| 用 `Message-ID` 跨 `UIDVALIDITY` 重新对齐 | 同上，且需要全量下载才能比对——重建的代价一样，复杂度高得多 |
| `LSUB` 只同步已订阅文件夹 | 订阅状态由其他客户端设置，用户在 maild 里看不到也改不了，会表现为「文件夹莫名消失」 |
| 服务器 `LIST` 里消失就删本地消息 | `LIST` 是文件夹清单的权威，不是删除用户邮件的授权。网络抖动或临时故障会变成数据丢失 |
| 名字启发式优先于 SPECIAL-USE 属性 | 属性是服务器的明示声明，启发式是猜测；明示永远优先 |
| 猜不出 role 时按名字相似度挑最近的 | 把用户自建的「客户」文件夹猜成归档，比没有图标糟糕得多 |
| 引入 utf7 crate | 需要的是一个窄函数（只解不编），不是一个库；与 ADR-0025 的取舍一致 |
| 所有文件夹都走 `MailFiltered` | 已发送/垃圾邮件的历史邮件会触发大量错误通知与 OTP 误报 |
| 收信循环持有 `Arc<Store>` 整个会话 | 受管账户的停止有超时上限，超时后任务被放弃；长期持有会把数据库锁扣在一个已放弃的任务里（决策 11） |

## 后果

- 首次升级到 v4 后，每个账户会执行一次 `LIST` 并对**每个新发现的文件夹做一次全量抓取**。
  邮件多的账户第一次会有明显的流量和时间开销，之后才进入增量稳态。
- 收件箱的历史行被回填到 `INBOX`，游标随之设为该文件夹已有的最大 UID——升级后不会重下收件箱。
- 非收件箱文件夹的新邮件最长有一个 `folder_poll_secs` 的延迟，且不产生通知。
- 标记位只在首次抓取时快照。在 M4.5-05 落地前，服务器上「已读」的变化不会反映到本地，
  UI 应把标记位当作「上次抓到时的状态」。
- 回滚到 v4 之前的二进制无法打开已升级的库（ADR-0007 fail-closed），升级前备份是唯一退路。
- 文件夹重命名会留下一条 `present=0` 的旧记录和一条新记录（决策 8）。

## 验证

`crates/maild/tests/` 覆盖：

- v3 → v4 迁移：历史消息挂到自动创建的 `INBOX`，唯一键换成 `(account, folder_id, uid)` 且
  跨文件夹同 UID 不再冲突，游标回填为该文件夹的最大 UID；v1/v2/v3 备份仍可恢复并迁移到 v4；
  畸形 v3 结构 fail-closed 且整体回滚；
- modified UTF-7 解码（中文文件夹名、`&-` 字面量、非法输入回退原名）；
- role 映射：SPECIAL-USE 属性优先、英文/中文名兜底、`INBOX` 恒为 inbox、猜不出即为空；
- 增量抓取：第二次连接只请求 `{last+1}:*`，不重复下载已有消息；
- `UIDVALIDITY` 翻转：旧行与附件删除、游标清零、重新全量、不产生重复行；
- 非收件箱文件夹落库但**不**触发通知/OTP；收件箱行为逐字节不变；
- `listFolders` 返回结构、计数与 `present` 标记；`listMessageSummaries` 的 `folder` 过滤；
  省略 `folder` 时与旧行为一致；
- `listFolders` 在方法策略表里归 `messages_read`，插件越权仍 `-32016`。
