# ADR-0025：M4.5-01 消息模型与 store schema v3

- 状态：已接受（2026-08-29）
- 关联：ADR-0007（`user_version` 迁移框架）、ADR-0008/0009（备份与恢复）、ADR-0022（地址层与 v2）、
  ADR-0024（内置前端迁移）；ROADMAP M4.5-01

> **后续时间语义增补（2026-11）**：IMAP 同步（增量抓取、按需单封拉取）与 `scope = "server"` 服务器检索在 `UID FETCH` 中携带 `INTERNALDATE`，将经 `[1971-01-01T00:00:00Z, now + 24h]` 钳制的服务器到达时间写入 `received_at`（服务器未返回或越界时回退本机观测时刻），使早于 10 年或缺失 `Date` 头的历史邮件仍按真实历史时刻排序；`parse_date_header` 与 provider `received_at` 同时增设四位年份上界 `9999-12-31T23:59:59Z`（`253_402_300_799`），阻断毫秒时间戳与超界年份。schema 与协议版本保持不变。

## 背景

maild 至今把一封邮件存成 `(account, uid, sender, recipient, subject, body, received_at)` 七元组。
这个模型足够跑通「收到 → 过滤 → 通知 → 列表 → 详情」的闭环，却缺了作为日常邮箱客户端不可回避的
东西，而且缺得比「少几个功能」更严重——**缺的是不落库就再也补不回来的原始事实**：

1. **没有 `Date` 头。** `received_at` 直接取 `SystemTime::now()`，是「本进程第一次看见这封信的时刻」，
   不是发信时间。首次全量拉取时所有历史邮件会拿到几乎相同的时间戳，列表实际按插入顺序排列。
2. **HTML 原文被丢弃。** `extract_text` 拿 `<[^>]*>` 正则把标签替换成空格。这既让 ADR-0024 决定的
   Tauri 正文渲染无米可炊，也有一个当下就存在的正确性缺陷：正则只删标签本身，`<style>` 与
   `<script>` 元素的**内容**会作为正文文本泄漏进列表摘要、规则匹配和通知。
3. **没有会话头。** 没有 `Message-ID`/`In-Reply-To`/`References`，就没有线程（M4.5-07），
   回复也无法正确挂到会话上（M4.5-06）。
4. **没有 Cc/Bcc/Reply-To。** 回复与全部回复无从谈起。
5. **没有附件。** part 解析后即丢弃，`cid:` 内联图片无法在渲染时取回。
6. **没有标记位。** `\Seen`/`\Flagged`/`\Answered`/`\Deleted` 无处存放，写路径（M4.5-05）没有落点。

这些字段全部只存在于 IMAP 原始报文里。报文本身不落库，一旦服务器侧过期、账户迁移或用户清理，
就永久丢失。因此这一片**有时间窗口**：历史邮件每多积累一天，回填代价越高。它同时不依赖
ADR-0024 的前端选型，是 M4.5 里唯一应当无条件先做的切片。

## 目标

1. 让一封邮件在库里**存完整**：解析出的 RFC 5322 事实全部落库，不可再生的原文（HTML、附件）有界保留。
2. 列表排序基于发信时间而非落库顺序，且对伪造 `Date` 有防御。
3. 修掉 `<style>`/`<script>` 内容进正文这个既有缺陷。
4. 一次迁移（v2 → v3）覆盖本切片全部结构变更，用户库只承受一次 DDL 事务。
5. 协议保持 additive：不新增方法、不提升协议代数、旧客户端不受影响。

## 非目标

- **文件夹同步逻辑**（LIST、role 映射、per-folder 游标、`UIDVALIDITY` 翻转重建）归 M4.5-02。
  本片只预置 `folders` 结构。
- **写路径**（`STORE`/`MOVE`/`pending_ops`）归 M4.5-05。本片只提供标记位列，不写服务器。
- **HTML 渲染与安全策略**（CSP、导航拦截、`cid:` scheme handler、远程图片白名单）归 M4.5-04。
  本片只负责把 HTML 原文与附件字节安全地存下来。
- **全文检索与线程聚合**归 M4.5-07。本片只落 `References` 原始文本，不建索引、不做聚合。
- **保留整封 RFC 822 原始报文。** 见「备选方案与取舍」。
- **附件的按需回源下载。** 超限附件本片只记元数据；回源需要 M4.5-02 的连接管理。

## 决策

### 1. 消息行是「已解析的 RFC 5322 快照」

`messages` 增列（全部 `ALTER TABLE ADD COLUMN` + 非空默认值，旧行零回填成本）：

| 列 | 类型 | 语义 |
|---|---|---|
| `message_id` | `TEXT NOT NULL DEFAULT ''` | `Message-ID` 头原文（含尖括号已剥离） |
| `in_reply_to` | `TEXT NOT NULL DEFAULT ''` | `In-Reply-To` 头 |
| `reference_ids` | `TEXT NOT NULL DEFAULT ''` | `References` 头原文，空格分隔 |
| `cc` / `bcc` / `reply_to` | `TEXT NOT NULL DEFAULT ''` | 对应头原文 |
| `date_at` | `INTEGER`（可空） | 解析成功的 `Date` 头（unix 秒）；缺失或不可解析为 `NULL` |
| `sort_at` | `INTEGER NOT NULL DEFAULT 0` | 物化排序键，见决策 3 |
| `body_html` | `TEXT NOT NULL DEFAULT ''` | HTML 原文，有界，见决策 6 |
| `snippet` | `TEXT NOT NULL DEFAULT ''` | 列表预览（正文前 200 字符，空白折叠） |
| `has_attachments` | `INTEGER NOT NULL DEFAULT 0` | 是否存在非内联附件 part |
| `seen` / `flagged` / `answered` / `deleted` | `INTEGER NOT NULL DEFAULT 0` | IMAP 标记位镜像 |

两处**命名偏离 ROADMAP 草稿**，理由是 SQL 层面的硬约束而非风格：

- `references` 是 SQLite 关键字，作列名必须处处加引号，任何一处漏引号都是运行时错误 → `reference_ids`；
- `date` 与 SQLite 日期函数同名，虽可用但极易在表达式里产生歧义 → `date_at`，
  与既有 `received_at`、`created_at`、`expires_at` 的命名惯例一致。

偏离只发生在 SQL 层。**协议与领域类型仍用 RFC 的名字** `references`——它是这个头的真名，
而列名是被 SQLite 关键字逼出来的实现细节，不该外溢到客户端。映射只出现在三条 SQL 语句里。

`received_at` 语义**不变但被正名**：它是「本机第一次观测到这封信的时刻」，不是发信时间。
既有行的含义因此不需要迁移，只是不再被当作发信时间使用。

### 2. schema v2 → v3 一次迁移覆盖全部结构

沿用 ADR-0007 的框架：`IMMEDIATE` 事务内读版本 → 顺序执行缺失的迁移步 → 精确结构校验 →
写 `user_version` → 提交；任何一步失败整体回滚，下次打开重放。v3 一步内完成：

- `messages` 的 15 个 `ADD COLUMN`；
- 旧行 `sort_at` 回填为 `received_at`（旧行没有 `date_at`，这正是正确值）；
- 新索引 `idx_messages_sort(sort_at DESC, id DESC)` 与 `idx_messages_account_sort(account, sort_at DESC, id DESC)`；
- 新表 `folders`、`attachments` 及其索引。

`MIN_SUPPORTED_SCHEMA_VERSION` 保持 1：v1 与 v2 备份都能进新库，恢复后在临时 replacement 上
迁移到 v3 再发布。恢复输入校验因此需要 `validate_schema_v2`——v2 的精确结构此前只作为「当前
版本」被 `validate_schema` 覆盖，v3 之后它成为一个历史版本，必须有自己的固化结构快照。

`folders` 在本片**没有写者**。预置而不推迟到 M4.5-02，理由是迁移的代价在用户库上而不在代码里：
一次 DDL 事务比两次少一次崩溃窗口、少一次备份格式跨越。ADR 记录此事，M4.5-02 是它的首个写者。

```sql
CREATE TABLE folders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  name TEXT NOT NULL,                    -- IMAP mailbox 名（已解码）
  role TEXT NOT NULL DEFAULT ''          -- '' | inbox | sent | drafts | trash | archive | spam
    CHECK(role IN ('', 'inbox', 'sent', 'drafts', 'trash', 'archive', 'spam')),
  uid_validity INTEGER,                  -- NULL = 尚未同步
  last_seen_uid INTEGER,
  synced_at INTEGER
);
CREATE UNIQUE INDEX idx_folders_account_name ON folders(account, name);

CREATE TABLE attachments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  uid TEXT NOT NULL,
  part_index INTEGER NOT NULL,           -- 深度优先遍历序，稳定
  filename TEXT NOT NULL DEFAULT '',
  mime_type TEXT NOT NULL DEFAULT '',
  content_id TEXT NOT NULL DEFAULT '',   -- cid: 引用键，尖括号已剥离
  inline INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL,                 -- 解码后字节数，即使内容未内联也可信
  content BLOB                           -- NULL = 超限未内联
);
CREATE UNIQUE INDEX idx_attachments_part ON attachments(account, uid, part_index);
CREATE INDEX idx_attachments_cid ON attachments(account, uid, content_id);
```

附件按 `(account, uid)` 而非 `messages.id` 外键关联。理由：落库路径是 `INSERT OR IGNORE`
（重连重放幂等），拿不到稳定的 `last_insert_rowid`；而 `(account, uid)` 已有唯一索引，
是这条消息真正的自然键。

### 3. 排序键是物化列，且对伪造 `Date` 有防御

列表排序改为 `ORDER BY sort_at DESC, id DESC`，`sort_at` 在插入时算好写入：

```
sort_at = if date_at 可信 { date_at } else { received_at }
```

**「可信」不等于「解析成功」。** 垃圾邮件常伪造远未来的 `Date` 把自己顶到列表最上方，这是
真实存在的滥用手法。判据：`date_at` 落在 `[received_at - 10 年, received_at + 24 小时]` 之外
即视为不可信，`sort_at` 回退 `received_at`。`date_at` 本身**原样保留**——显示发信人声称的时间
是诚实的，让它决定排序不是。

选择物化列而非 `ORDER BY COALESCE(date_at, received_at)` 表达式：表达式索引在
`pragma_index_xinfo` 里列名为 `NULL`，与既有 `validate_index` 的精确列名校验不兼容；
为一个排序键放宽 schema 校验不划算。物化列的代价是插入时多算一次，且未来若要重排需要
一次 `UPDATE`——都在可接受范围。

### 4. `Date` 解析用 mailparse 既有能力

`mailparse::dateparse` 已在依赖内，覆盖 RFC 5322 与常见畸形写法（两位年份、缺时区、
过时时区缩写），不引入新依赖。它对完全无法识别的输入**不报错而是返回纪元附近的值**
（实测中文自由文本返回 `0`），因此下界取 1971-01-01 而不是「非负」：RFC 822 之前不存在
电子邮件，早于该点的解析结果一律按不可解析处理。把垃圾当成 1970 年显示给用户比不显示更糟。

### 5. HTML → 文本用内核自带状态机，不引入 HTML 解析依赖

替换 `<[^>]*>` 正则。新实现是一个单遍字符状态机：

- **`script` / `style` / `head` / `title` / `textarea` / `noscript` 的元素内容整体丢弃**
  （这是本片要修的缺陷）；找不到匹配闭标签时**不跳过**——宁可多一点噪音，
  也不要因为一个畸形 `<head>` 丢掉整封正文；
- 正确跳过注释 `<!-- -->`、CDATA、以及标签属性值内的 `>`（`<a href="a>b">` 不会被截断）；
- `br` / `p` / `div` / `tr` / `li` / `h1`–`h6` / `blockquote` / `table` 边界产生换行；
- 解码具名实体（`&nbsp; &amp; &lt; &gt; &quot; &apos;` 等）与数字实体（`&#N;` / `&#xN;`），
  数字实体解出控制字符时丢弃（`&#0;` 不得把 NUL 送进正文与 SQLite TEXT 列）；
- 折叠连续空白，行首行尾去空格，最多保留一个空行。

不引入 `html2text`/`scraper`/`html5ever`：本片需要的是「取纯文本」，不是构建 DOM；
真正需要 DOM 语义的是渲染，而渲染由 ADR-0024 决定的系统 webview 承担，内核侧再养一份
HTML 解析器是重复且是新的攻击面。状态机约 150 行，可直接对恶意输入写断言。

### 6. HTML 原文落库但有界，且不进列表与通知

三条边界，全部由**帧预算**倒推：`MAX_FRAME_BYTES` 是 1 MiB。

- **存储上限**：`body_html` 截断到 256 KiB，纯文本 `body` 截断到 128 KiB，均在 UTF-8 字符边界。
  截断时在 `body_html` 末尾追加 `<!-- maild: truncated -->` 标记，使前端可如实告知用户。
- **`listMessages` 不返回 HTML**：该方法一次可返回 500 条完整消息，是既有遗留分页方法
  （协议文档已建议前端改用 `listMessageSummaries`）。它的 `SELECT` 不含 `body_html`，
  该字段在这条路径上恒为空串并被 `skip_serializing_if` 略去。
- **`message/received` 通知不携带 HTML**：`mail_notification` 已有逐字段截断，本片增加
  一步清空 `body_html`。通知是「有新邮件」的信号而非持久队列，协议文档已如此声明。

HTML 正文只经 `getMessage` 取回。它是单条查询，256 KiB + 128 KiB + 头部在 1 MiB 帧内有充分余量。

### 7. 附件元数据全落，内容按上限内联

- 遍历 MIME 树。一个叶子 part 是**正文候选**当且仅当它是 `text/plain` 或 `text/html`、
  没有 `Content-Disposition: attachment`、且没有文件名参数；其余叶子一律记为附件。
  因此 `multipart/alternative` 里被选作正文的 part 不会重复记录，而 `notes.txt`
  这类带文件名的文本附件不会被误当正文。
- 每 part 内容 ≤ **2 MiB** 且单封累计 ≤ **8 MiB** 时内联存 BLOB，否则 `content = NULL`，
  `size` 仍如实记录。前端据此区分「可离线打开」与「需联网获取」，回源属 M4.5-02 之后。
- `content_id` 剥尖括号后建索引，供 M4.5-04 的 `cid:` scheme handler 直接命中。
- **「内联」的判据是正文引用了它**（存在 `Content-ID`），而不是发信方声明了
  `Content-Disposition: inline`——缺省 disposition 本就是 inline，照它判断会把只带
  `name=` 参数的真附件误判成内联。
- `has_attachments` 只统计**非内联**附件（`inline = 0`），与用户对「这封信有附件」的直觉一致：
  正文里的签名图片不该点亮回形针。

### 8. 协议 additive，代数不动

- `MailMessage` / `MessageSummary` 的新字段全部 `#[serde(default)]`，
  重字段额外 `skip_serializing_if`。旧 daemon + 新客户端、新 daemon + 旧客户端双向可用。
- 不新增方法、不改方法名、不动 ADR-0018 的能力→方法映射（新字段仍在 `messages_read` 域内）。
- provider 合同（`transport/receive`，ADR-0021）新增可选键 `date`、`message_id`、`cc`、
  `reply_to`；缺省保持既有行为。不新增必填键，现有 provider 无需改动。
- `MailMessage` 派生 `Default`，使新增字段不再要求每个构造点逐字段书写。这是为**下游可演进性**
  付的代价：域事件本不该有「空消息」这个合法值，但让 15 个构造点在每次加字段时全部改写，
  实际后果是加字段变贵、字段被塞进 `body` 里凑合。

## 备选方案与取舍

| 方案 | 为何不选 |
|---|---|
| 保留整封 RFC 822 原始报文，字段全部按需重解析 | 存储翻倍（正文已解析一份），且每次列表/规则/搜索都要重跑 MIME 解析。真正的收益（解析器升级后可重放）可以在需要时用一次全量重拉换取，不值得让每封信常驻两份 |
| `messages` 增 `folder` 列，本片就做文件夹 | 文件夹是多对多（IMAP 允许同一信出现在多个 mailbox），一个列会立刻不够用；且 `UIDVALIDITY` 语义必须与同步逻辑一起设计。归 M4.5-02 |
| 排序用 `ORDER BY COALESCE(date_at, received_at)` 表达式索引 | 表达式索引的列名在 `pragma_index_xinfo` 中为 `NULL`，与既有精确 schema 校验冲突（决策 3） |
| 直接信任 `Date` 头排序 | 伪造未来日期是常见垃圾邮件手法，会永久占据列表顶部 |
| 引入 `html2text` / `scraper` 做正文抽取 | 为「取纯文本」引入完整 HTML 解析器，是重复的攻击面；DOM 语义由系统 webview 承担（ADR-0024） |
| 附件存文件系统而非 BLOB | 会引入第二个需要与数据库保持一致的存储位置：备份（ADR-0008 Online Backup）、恢复（ADR-0009）、维护锁和账户删除级联全部要各写一遍两阶段逻辑。上限内联 + 超限只存元数据在个人邮箱量级足够 |
| 附件不设上限全部内联 | 25 MiB 附件会让 Online Backup 快照与恢复窗口不可控 |
| `body_html` 不设上限 | `getMessage` 单帧上限 1 MiB，会在真实邮件上失败 |
| 推迟到 M4.5-02 一起做 schema v3 | 用户库要承受两次 DDL 事务与两次备份格式跨越；且本片是唯一有时间窗口的（背景） |

## 后果

- v2 库首次打开自动升 v3，既有行内容不变，`sort_at` 回填为 `received_at`——即旧数据的排序
  行为与升级前完全一致，不会因为迁移而重排。
- **历史邮件不会被回填。** `date_at`、`body_html`、附件只对迁移之后新收到的消息可用。
  旧消息的 `date_at` 为 `NULL`、`body_html` 为空。想补齐需要清库重拉，属用户显式动作，
  本片不自动触发，也不提供隐式重拉。
- 单封消息的存储占用上升（HTML 原文 + 内联附件）。个人邮箱量级下可接受，但这是首次让
  数据库体积与附件大小挂钩，备份耗时随之增长。
- 回滚到 v3 之前的二进制**无法**打开已升级的库（`user_version` 高于其 `SCHEMA_VERSION`，
  按 ADR-0007 fail-closed）。这是既定策略，升级前的备份是唯一回退路径。
- 规则匹配（sender/subject）与 OTP 提取的输入不变，但正文文本因为修掉了 `<style>`/`<script>`
  泄漏而**更干净**——个别依赖泄漏文本误命中的规则可能不再命中，这是修复而非回归。

## 验证

`crates/maild/tests/` 覆盖：

- v2 库迁移到 v3，消息、地址、OTP 标记全部保留，`sort_at` 等于原 `received_at`；
- v1 库直接迁移到 v3（跨两级）；
- v1、v2 备份恢复后迁移到 v3 并通过当前结构校验；v3 库重开幂等；
- 未来版本仍 fail-closed 且不改动 journal mode 与数据；
- 畸形 v2 结构迁移失败并整体回滚；
- `parse_message` 解析 `Date` 并落 `date_at`；缺失 `Date` 时 `date_at` 为 `NULL`、`sort_at`
  回退 `received_at`；伪造未来 `Date` 不改变 `sort_at` 但保留 `date_at`；
- `<style>`/`<script>` 内容不出现在 `body` 与 `snippet`；属性值内的 `>` 不截断标签；
  实体解码与换行规则；
- 附件按 `content_id` 可取回；超限 part 只留元数据且 `size` 如实；
  内联图片不点亮 `has_attachments`；
- `listMessages` 响应不含 `body_html`，`getMessage` 含；`message/received` 通知不含；
- 新字段在旧客户端 DTO 上以 serde default 反序列化成功（协议 additive 回归）。
