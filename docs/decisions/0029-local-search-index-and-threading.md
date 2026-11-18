# ADR-0029：M4.5-07 本地搜索索引与线程聚合（schema v7）

- 状态：已接受（2026-08-30）
- 关联：ADR-0007（schema 迁移）、ADR-0025（消息模型）、ADR-0026（文件夹与增量同步）、
  ADR-0027（写队列）、ADR-0028（发信与会话头）；ROADMAP M4.5-07

## 背景

M4.5-01 把 `Message-ID` / `In-Reply-To` / `References` / `Date` / 正文 / 摘要全部落了库，
M4.5-02 把身份扩到 `(account, folder, uid)`。库里已经有搜索和线程需要的全部事实，
但没有任何一个方法能用上它们：

- **搜不了。** `listMessageSummaries` 只有分页、账户与文件夹三个维度。找一封三个月前的
  发票邮件，只能自己往下翻。
- **没有线程。** 一次来回八封的邮件在列表里就是散开的八行，回复关系存在库里却没人读。
- **文件夹一多，翻页就废了。** M4.5-02 之后一个账户可能有几十个文件夹，逐个翻不是办法。

这一片把这两件事补上，并且**不引入任何新的索引引擎依赖**。

## 目标

1. 主题、发件人、收件人、正文的全文检索，中文可用。
2. 结构化过滤：发件人、时间范围、有无附件、文件夹、账户。
3. 用已落库的 header 聚合线程，不二次解析原文。
4. 索引重建可中断可续——升级不能让 daemon 卡在一次长事务里。
5. 服务器侧 `SEARCH` 作为本地索引覆盖不到时的补充。
6. 协议 additive，代数不提升。

## 非目标

- **相关度排序（BM25）**：见决策 6。
- **用户可写的查询语法**（`AND`/`OR`/`NEAR`/前缀通配）：见决策 4 的诚实边界。
- **按主题兜底聚合线程**：见决策 8。
- **搜索结果高亮片段（snippet/highlight）**：需要 FTS5 保留原文列，与决策 2 冲突；
  列表已有 `snippet` 字段可用，高亮留给前端在正文里做。
- **跨账户线程合并**：线程只在单个账户内聚合，见决策 7。

## 决策

### 1. 索引引擎用 SQLite 自带的 FTS5

`libsqlite3-sys` 的 bundled 构建已经带 `-DSQLITE_ENABLE_FTS5`，我们只是启用一张虚表。
没有新 crate，没有新进程，没有需要单独备份的第二份数据——`backup_database` 的 Online Backup
按页复制，FTS5 的影子表随之而来，恢复路径一个字都不用改。

这条是 ROADMAP 给这一片钉死的验收点，也是这一片所有其他决策的边界条件。

### 2. `messages_fts` 是独立内容表，不是 external content

FTS5 的 external content 模式（`content='messages'`）省掉一份正文拷贝，代价是**删除必须
提供原始列值**。这在部分索引的场景下是有毒的：一行还没被回填索引过就被删除，此时发出的
`'delete'` 指令会把索引里本不存在的词条计数减成负数，索引从此不可信，而且**不报错**。

独立内容表没有这个陷阱：`DELETE FROM messages_fts WHERE rowid = ?` 对不存在的行是无害
no-op，重复执行也是。代价是索引里多一份文本。

这份文本不是「第二份正文」而是**派生的分析结果**（见决策 3，它和原文并不相同），查询只取
`rowid` 再回 `messages` 取展示字段，任何路径都不会把它当正文读出来。存储上限由 ADR-0025
已经定下的 128 KiB 正文封顶承接，不再叠加第二道截断——多一道就多一个「搜不到但看得见」的
静默缺口。

### 3. 中文靠 Rust 侧逐字切分 + `unicode61`，查询侧发短语

这是这一片最关键的一个决定。SQLite 自带的三个分词器对中文都不成立：

- `unicode61`（默认）把连续的中日文当成**一个** token。「发票已开具」是一个词，
  搜「发票」搜不到。中文邮件的全文检索直接失效。
- `porter` 是英文词干还原叠在 `unicode61` 上，中文问题原样保留。
- `trigram` 按三字窗口切，中文能搜，但**查询词必须 ≥3 字符**。而中文里最常用的正是两字词
  ——「发票」「账单」「合同」「密码」。

所以在写入索引前，由内核把 CJK 字符逐个用空格隔开：

```
"发票已开具"  →  "发 票 已 开 具"
"发票invoice" →  "发 票 invoice"
```

`unicode61` 于是把每个汉字当成独立 token。查询侧做**同样的**切分，并把切出多个 token 的
词作为**短语**下发：`"发 票"`。FTS5 的短语匹配要求 token 位置相邻，这恰好等价于原文里
「发票」是一个连续子串。任意长度的中文查询都能命中，且不会跨词误命中。

拉丁文本原样交给 `unicode61` 自己按非字母数字切分，整词行为不变；`zhang@example.test`
切成 `zhang example test` 三个 token，查完整地址时同样作为短语下发，一样命中。

日文假名按同样规则逐字切分。**韩文谚文不切**——韩语本来就用空格分词节，逐字切会把一个
词素拆成无意义的字母。

分词器写死在 `CREATE VIRTUAL TABLE` 语句里，并由 schema 校验逐字节比对（决策 10）：
换掉分词器会让所有中文查询静默失效，这种事必须在打开数据库时就被拦住。

### 4. 查询表达式由内核构造，绝不把用户输入透传给 `MATCH`

用户串里的 `"`、`*`、`-`、`(`、`:`、`NEAR`、`AND`/`OR`/`NOT` 全是 FTS5 语法。透传轻则
一个引号引发语法错误让搜索整个失败，重则 `subject:xxx` 这样的列限定绕过我们的检索意图。

内核自己按空白切词、逐词做决策 3 的切分、逐词加引号（内部的 `"` 加倍转义）、用 `AND` 连接。
词数与总长有上限，切完为空的词直接丢弃；全部丢弃则退化为纯过滤查询而不是报错。

**代价是显式的**：用户写不了 FTS5 语法。这是有意的——邮箱搜索框的正确行为是把输入当文本，
不是当查询语言。需要精确控制的维度都以结构化参数暴露（决策 5）。

### 5. 结构化过滤走 SQL `WHERE`，不进 `MATCH`

发件人、时间范围、有无附件、文件夹、账户都是 `messages` / `folders` 的普通列，用 SQL 表达
既精确又能走既有索引。把它们编码进 `MATCH` 表达式只会同时损失精确性和可读性。

时间范围过滤用 `sort_at`，与列表排序键一致（ADR-0025 决策 3）——用户看到的时间和搜索用的
时间必须是同一个，否则「按时间筛」会筛掉屏幕上正显示着的那封。

### 6. 结果按时间倒序，不按相关度

FTS5 的 `bm25()` 几乎免费，我们仍然不用它。邮箱搜索的用户模型是「我知道有这么一封，
帮我找出来」，最强的信号是时间而不是词频；而 bm25 会让同一次搜索在不同时间给出不同顺序
（新邮件改变词频统计），这在「我刚才明明看见它在第三行」的场景里是纯粹的伤害。

排序键与列表完全一致：`ORDER BY sort_at DESC, id DESC`。

### 7. 线程用 `Message-ID` 图物化，边存在 `thread_refs`

每封消息贡献若干条边 `(account, ref_id, message_row_id)`：自己的 `Message-ID`、
`In-Reply-To`、以及 `References` 里的每一个 id。落库时：

1. 查这些 `ref_id` 已经关联到哪些 `thread_id`；
2. 一个都没有 → 用**自己的行 id** 作为新 `thread_id`（天然唯一，不需要额外计数器）；
3. 有多个 → 取最小者，其余线程整体改写并入（合并）。

关键性质：**边包含尚未见过的 id**。所以到达顺序无关——

- A 先到（`Message-ID: <a>`），B 后到（`In-Reply-To: <a>`）：B 查到 A 的边，并入。
- B 先到：B 建线程并留下 `<a> → B` 这条边；A 后到时用自己的 `<a>` 查到它，并入。

这条性质同时让**回填与实时落库可以并发进行**（决策 9）：回填到一半时新到的邮件正常建线程，
稍后回填到的旧邮件会把两边合并到一起，最终结果与顺序无关。

`References` 可能很长，每封消息的边数有上限，超限时保留**首条与最近若干条**——首条是会话根，
丢了整条线程会断成两截，比丢中间几条严重得多（与 ADR-0028 决策 6 的截断规则同源）。

线程只在**单个账户内**聚合。跨账户合并需要先判定两个账户是不是同一个人，那是地址层的问题
（ADR-0022），不是线程的问题。

### 8. 主题不参与线程判定

很多客户端在 `Message-ID` 图之外用「主题相同且以 Re: 开头」兜底。我们不做。

理由是两类错误不对称：**漏并**只是让用户多看到一条线程，代价是轻微的不便；**错并**会把
两个陌生人的邮件放进同一条线程——「你好」「发票」「合同」这类主题在真实邮箱里天天重复，
主题兜底会稳定地制造这种错误。而线程视图一旦错并，用户对它的信任就没了。

只用 `Message-ID` 图。图断了就断了，两条线程好过一条错的。

### 9. 回填是可中断可续的后台过程，不在迁移事务里

v7 迁移只做三件事：建表、建触发器、把 `MAX(id)` 记进 `index_state` 作为回填起点。
**一行历史消息都不在迁移里索引。**

理由是迁移跑在打开数据库的立即事务里。几万封邮件的分词与索引写入放进去，等于让 daemon
启动挂起几十秒、WAL 膨胀到几百 MB，而且中途崩溃全部回滚、下次重来——正是 ADR-0007
定的迁移原则要避免的形状。

回填由一个后台任务分批推进，每批一个事务，**游标与索引写入在同一个事务里提交**，
所以崩溃后从上次提交处续跑，不会重复也不会漏。游标从 `MAX(id)` 向下走：最近的邮件最先
可搜，这是用户最可能搜的那部分。

回填期间搜索结果是**部分的**，这是事实，所以它可查询——`getSearchIndexStatus` 返回剩余
条数，前端可以据此提示「索引中」而不是让用户以为邮件丢了。

后台任务只持 `Weak<Store>`，每批临时 upgrade。这是 ADR-0026 决策 11 的直接应用：
那次 `account_lifecycle` 挂死的根因就是一个游离任务把 `Arc<Store>` 攥过了 await 点，
把数据库锁一起攥住了。

**但只有 `Weak` 不够**——这一片实现时就撞上了：一批正在 `spawn_blocking` 里跑的时候，
那个闭包持有的是真 `Arc`。只发停止信号加 `abort()`，被 abort 的是外层 async 任务，
阻塞闭包会继续跑完并在这期间一直扣着数据库锁；daemon 卸载完成后紧接着重新打开数据库
就会失败。所以卸载逆必须是**异步**的：先发停止信号，再有界等待在途批次收尾，
确认没有游离引用之后 dispose 才继续。这条和 ADR-0026 决策 11 是同一个教训的两半——
不跨 await 持有还不够，还得保证卸载时没有别处正持有。

没有 Tokio runtime 时（直接 `Store::open` 的单元测试）不启动任务，回填只在显式调用时推进。

### 10. 删除由 SQL 触发器兜底，写入留在 Rust

索引写入需要决策 3 的切分，那是 Rust 函数，SQL 触发器调不到（除非注册自定义 SQL 函数，
那会让每一条打开数据库的路径都多一个隐式前置条件，备份与恢复首当其冲）。所以**写入由
`Store` 显式驱动**。

但**删除必须是触发器**：

```sql
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
  DELETE FROM messages_fts WHERE rowid = OLD.id;
  DELETE FROM thread_refs WHERE message_row_id = OLD.id;
END;
```

两个方向的遗漏后果不对称。漏了写入，邮件搜不到——可见、可诊断、重建索引就能修。漏了删除，
索引里留下指向已删行的词条，搜索会**命中一封不存在的邮件**；`rebuild_folder`（UIDVALIDITY
翻转）、移动落定、以及对账发现的服务端 EXPUNGE 三条路径都会删消息，将来还会有第四条。
让 SQL 层兜住这个方向，新增的删除路径不可能忘。

`messages` 的索引列（主题、发件人、收件人、正文）落库后**从不被 UPDATE**——写路径只改
标记位与 `pending_removal`——所以不需要 UPDATE 触发器。

### 11. 服务器侧 `SEARCH` 复用同一条连接，且是显式可选的

首次同步就拉 `1:*`（ADR-0026），本地索引覆盖已同步的一切。服务器搜索真正有意义的只有三种
情况：超出 `MAX_SYNCED_FOLDERS` 上限的文件夹、未纳入同步的文件夹、以及回填尚未完成的窗口。

所以它不是默认路径：`searchMessages` 默认只查本地，`scope: "server"` 才发 `UID SEARCH`，
且必须指定单个文件夹，返回条数有上限。

**它复用收信循环那条连接**，不另开一条。ADR-0026 决策 3 把连接预算的上限定成了 1，
为一次搜索破例会让那个上限变成「1 加上用户手速」。实现形状与 ADR-0027 决策 8 的写队列唤醒
同源：API 侧把请求投进 channel，收信循环从 IDLE 里退出来执行，用 oneshot 回传结果；
账户离线时立即返回明确错误，而不是排队等一个不知道什么时候到来的连接。

非 ASCII 查询发 `CHARSET UTF-8` + 引用串。RFC 3501 严格来说要求用 literal 传 8bit 数据，
那需要续行握手；服务器不接受时会回 `BAD`，我们把它原样透出，而不是假装搜过了。

### 12. 协议 additive，代数不提升

新增 `searchMessages`、`getThread`、`getSearchIndexStatus` 三个方法，`MessageSummary`
与 `MailMessage` 增加 `thread_id` 字段。既有方法的请求与响应逐字节不变，旧客户端靠
serde default 忽略新字段。`PROTOCOL_VERSION` 不动。

三个方法都是 `Capability("messages_read")`：它们读的是已经能通过 `listMessages` 读到的
同一批数据，换一个检索维度不构成新的权限面。

## Schema v7

```sql
ALTER TABLE messages ADD COLUMN thread_id INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_messages_thread ON messages(account, thread_id, sort_at DESC, id DESC);

CREATE TABLE thread_refs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account TEXT NOT NULL,
  ref_id TEXT NOT NULL,
  message_row_id INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_thread_refs_edge ON thread_refs(account, ref_id, message_row_id);
CREATE INDEX idx_thread_refs_message ON thread_refs(message_row_id);

CREATE TABLE index_state (
  name TEXT PRIMARY KEY NOT NULL,
  cursor INTEGER NOT NULL,
  done INTEGER NOT NULL DEFAULT 0
);

CREATE VIRTUAL TABLE messages_fts USING fts5(...);   -- 语句原文由 schema 校验逐字节比对
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages ...;
```

校验沿用 ADR-0007 的做法：`validate_schema_v6` 在 v6 → v7 之前精确校验来源结构，
畸形历史库报告 schema 错误而不是含混的 SQLite 列错误。FTS5 虚表与触发器用 `sqlite_master`
里的建表语句**原文逐字节比对**——分词器与触发器体是语义的一部分，列名对上不代表行为对上。
FTS5 的影子表（`messages_fts_data` 等）连同上述对象一起进 `RESERVED_V7_OBJECTS`，
低版本输入里出现这些名字一律拒绝。

## 后果

**得到**：中文可用的全文检索、五个维度的结构化过滤、可信的线程聚合、可中断可续的索引重建，
全部建立在 SQLite 自带能力上。

**付出**：数据库体积增加一份派生索引（含逐字切分后的中文文本）；升级后有一段索引不完整的
窗口；用户写不了查询语法。

**留下的边界**：相关度排序、跨账户线程、搜索高亮、以及「服务器搜到但本地没有的邮件如何
纳入本地库」——最后一条目前只返回摘要，不落库，因为落库就要处理它属于哪个文件夹游标区间的
问题，那是同步模型的改动而不是搜索的改动。
