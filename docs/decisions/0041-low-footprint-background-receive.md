# ADR-0041：极致低占用的后台常驻收信引擎与 SQLite / 内存治理

- 状态：已接受；`maild` 内核侧实现并由 `crates/maild/tests/receive_footprint.rs` 验收（2026-11-17）；同日全栈汇合（`5bcb9d6`）已接线 `syncMessages` / `fetchServerMessage` / `status.runtime`、`maildctl sync`、菜单栏「立即同步」与 Tauri 服务器历史检索 + 按需拉回；release 守护进程实测见 `crates/maild/examples/footprint_bench.rs` 与 [testing](../testing/README.md#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)
- 日期：2026-11-17
- 关联：[ADR-0015](0015-m3-7-account-management-and-credentials.md)、[ADR-0026](0026-folders-and-incremental-sync.md)、[ADR-0027](0027-mailbox-write-path-and-offline-queue.md)、[ADR-0029](0029-local-search-index-and-threading.md)

## 背景

`maild` 的核心承诺是「最小资源占用的后台常驻收信」：没有常驻 WebView，一个纯 Rust 守护进程为每个账户维持一条 IMAP IDLE 连接。真实日常大邮箱与笔电长期挂机场景暴露出几处与这一承诺相悖的问题：

1. **会话内去重无界增长**：`serve()` 里的 `seen: HashSet<(String, u32)>` 每收一封信就多一个条目和一份文件夹名 `String` 副本，连接不断就永不回收。挂机数周后常驻内存随收信量线性增长。
2. **冷启动整箱拉正文**：没有游标的文件夹直接发 `UID FETCH 1:* (UID FLAGS BODY.PEEK[])`。一个 5 万封的老邮箱会把数 GB 的 MIME 流过网络、过解析器、过 waterfall，并一次性推出几百上千条通知。
3. **`N:*` 重传最后一封**：RFC 3501 规定 `N:*` 在 N 大于最大 UID 时仍返回最大 UID 那一条。每次 IDLE 唤醒、每次 sweep 的每个文件夹（最多 64 个）都会把最后一封旧邮件的完整正文（可能带几 MB 附件）重新下载一遍，只为了在客户端丢弃它。
4. **休眠唤醒后的半开连接**：没有 TCP keepalive，`login` / `IDLE` / `DONE` 也没有时限。合盖休眠后网络已变、对端早已断开，但本机 socket 看起来仍然可写，收信协程会永远挂在一次读上。
5. **断线固定 5 秒重连**：断网（飞机上、无 Wi-Fi）时每 5 秒唤醒一次 CPU、取一次钥匙串、做一次 DNS，而且关断信号要等这次睡眠结束才被看到。
6. **SQLite 与堆的高水位不回落**：默认 2 MiB 页缓存、WAL 在一次大写入后保持膨胀、内存映射页计入 RSS；分配器在冷启动高峰之后不把空闲页还给内核，Activity Monitor 的「内存」列居高不下。
7. **生产二进制链接了测试才用的 HTTP 客户端**：`reqwest` 只在 `api.rs` 的 `#[cfg(test)]` 里用到，却在 `[dependencies]` 中。

## 决策

### 1. 冷启动有界窗口与分批抓取

- `ReceiveConfig` 新增 `initial_sync_limit: usize`（serde 默认与 `Default` 均为 **200**；`Merge` 遵循既有「零值 = 不覆盖」约定；配置为 `0` 表示不限制）。受管账户使用同一默认值。
- `SelectedMailbox` 额外记录 `SELECT` 返回的 `EXISTS` 与 `UIDNEXT`。`UIDNEXT` **只在刚 `SELECT` 完的那一刻可信**，由 `sync_folder` 取走即作废——之后同一选中状态上的同步回到 `N:*`，因此不会因为陈旧快照漏信。
- 抓取计划（`plan_fetch`）：
  | 条件 | 行为 |
  |---|---|
  | 刚 `SELECT` 过且 `UIDNEXT ≤ start_uid` 或 `EXISTS = 0` | **零 FETCH**：游标之后不可能有邮件 |
  | 冷启动（无游标）且 `EXISTS > initial_sync_limit` | 先只取 UID：`UID SEARCH <EXISTS-limit+1>:*`（序号区间，只回最新 `limit` 个 UID）；结果为空或服务器拒绝时回退 `UID FETCH 1:* (UID)`。截取最新 `limit` 个 UID 分批抓正文，游标直接推进到清单里最大的 UID |
  | 刚 `SELECT` 过且已知积压 `UIDNEXT - start_uid > 50`（长时间离线后重连） | `UID SEARCH UID N:*`（回退 `UID FETCH N:* (UID)`），滤掉 `< start_uid` 的那条后分批抓正文 |
  | 其余（小增量常态、`EXISTS ≤ limit` 的冷启动） | 原样 `UID FETCH N:* (UID FLAGS BODY.PEEK[])` 一条命令流式处理——与 v4 逐封一致 |
- 分批：每条正文 `UID FETCH` 最多携带 `FETCH_BATCH_SIZE = 50` 个 UID（逗号清单）。报文以流的形式逐封解析，同一时刻只有一封驻留内存。
- 冷启动**发生截断**时，主文件夹只有最新 5 封走 `MailReceived` → `MailFiltered` 实时链路（过滤、OTP、通知），其余静默 `insert_message` 落库，避免一次性轰炸通知。未截断的冷启动（例如所有 1～20 封的既有测试信箱）行为不变。
- 冷启动 / 大批补拉结束后调用 `Store::shrink_memory()` 与 `daemon::release_unused_memory()`。
- 更早的历史：服务器检索（ADR-0029 决策 11）定位 UID，再经决策 4 的 `fetch_message_by_uid` 按需拉回。

**取舍**：选用 `SELECT` 已经带回的 `EXISTS` / `UIDNEXT` 判定，而不是任务草案里「每次增量先 `UID FETCH N:* (UID)` 探测」。探测方案每次唤醒多一个往返，且既有验收（`folders.rs` 断言每会话只有一条无正文 FETCH、首条正文抓取是 `1:*`、增量是 `2:*`；`search.rs` 断言第一条 `UID SEARCH` 是用户检索）会被打破。`UIDNEXT` 方案零额外命令，在 sweep（每轮都会重新 `SELECT` 非主文件夹，多文件夹账户的主文件夹也会被重新 `SELECT`）这条最大的浪费源上完全消除了 `N:*` 重传；仅剩「单文件夹账户、IDLE 唤醒后同一选中状态上的增量」仍走 `N:*`，那正是有新邮件的时刻。冷启动 UID 清单用序号区间 `SEARCH` 而不是 `SEARCH ALL`，5 万封信箱的应答从约 300 KB 降到 `limit` 个 UID。

### 2. 会话内去重 O(folders) 恒定

- `seen` / `cursors` / `reconciled` 收拢为 `SyncSession.folders: HashMap<String, FolderSession>`，条目数受 `MAX_SYNCED_FOLDERS`（64）约束。
- 每个 `FolderSession` 只有定长状态：无 store 时的会话游标、是否已对账、以及 `RecentUids { highest, floor, ring: VecDeque<u32> }`（容量 256）。新邮件 UID 严格递增（RFC 3501 §2.3.1.1），被挤出环的 UID 一律视为已处理，`floor` 记住被挤出的最大值即可。`UIDVALIDITY` 翻转时整环清零。
- 每次进入 IDLE 前清空 async-imap 的未请求响应通道（容量 100，每条可能引用一整块读缓冲；库在通道满时丢弃新条目，但已满的 100 条会一直驻留）。

### 3. 半开连接恢复、TCP keepalive 与指数退避

- `connect_tcp`：`lookup_host` 解析全部地址逐个尝试，**单地址 10s、总计 20s**（双栈环境里不通的 IPv6 不再吃满系统默认的 75s）；`set_nodelay(true)`；Unix 上 `SO_KEEPALIVE`，macOS `TCP_KEEPALIVE = 120s`（Linux 为 `TCP_KEEPIDLE`），`TCP_KEEPINTVL = 30s`、`TCP_KEEPCNT = 4`——死连接约 4 分钟内由内核报错。全部通过 `libc::setsockopt`，不引入新 crate；Windows 保持系统默认（尽力而为）。
- 阶段超时：TLS / STARTTLS 握手 20s、`LOGIN` 15s、`IDLE` 启动 15s、`DONE` 10s。超时即视为连接失效，走重连。
- 单次 IDLE 等待 = `until_sweep.min(25 min).max(1 s)`（RFC 2177 建议 ≤ 29 分钟续期）。
- 退避：`run_loop` 维护 `consecutive_failures`。本次会话曾进入 `Online`（`Receive::is_online()`）即清零；否则延迟 = `reconnect_secs × 2^min(failures, 6)`，封顶 300s（配置的基数本身更大时尊重配置），再加 ≤10% 的随机抖动错开多账户。首次失败仍按基数（5s），既有重连时序不变。凭据读取失败走同一退避。
- 退避睡眠可被 `trigger_sync()` 与关断**立即打断**：用户点「立即同步」不必等倒计时；`AccountManager::stop_runtime` 不再因为一次长退避而撞上停止超时。

### 4. 两个新原语（供 API / 界面接线）

- `Receive::trigger_sync(&self)`：经 `sync_wake: watch::Sender<u64>` 计数唤醒。打断 IDLE（`DONE` 后立刻同步主文件夹并**强制本轮 sweep 所有文件夹**、清写队列）、轮询等待（立即跑一整轮）与断线退避睡眠（立即重连）。
- `Receive::fetch_message_by_uid(&self, folder, uid) -> Result<Option<MailMessage>, String>`：与 `server_search` 同构——有界 `mpsc`（容量 4，满则立即拒绝）+ `oneshot` 投递给 `serve`，在**同一条** IMAP 连接上执行（ADR-0026 连接预算 = 1），总时限 30s；离线时立即报错。确认选中 `folder` → `UID FETCH <uid> (UID FLAGS BODY.PEEK[])` → 解析并设置 `folder` / `folder_role` / 标记位 → 有 `Store` 时 `insert_message` 幂等落库 → `Ok(Some(msg))`；服务器上不存在则 `Ok(None)`。一次失败不带走收信连接。
- IDLE 与轮询两条分支的 `tokio::select!` 都处理 `sync_wake` 与 `fetch_rx`。
- 附带公开 `Receive::is_online()`。

### 5. SQLite 运行时收紧

- 文件型数据库在迁移准备完成后设置（`migrations::apply_low_footprint_pragmas`，只影响连接运行时，不改任何表 / 索引 schema，既有 schema 校验不受影响；内存库的页就是数据本身，不设置）：
  - `cache_size = -1024`（页缓存约 1 MiB）
  - `temp_store = FILE`
  - `journal_size_limit = 4194304`（checkpoint 后 WAL 截回 4 MiB 内）
  - `mmap_size = 0`
- `Store::shrink_memory()`：锁内 `PRAGMA shrink_memory` + `PRAGMA wal_checkpoint(PASSIVE)`（不阻塞读写），尽力而为。在检索索引回填完成（`progress.done`）与收信冷启动 / 大批补拉完成后调用。
- `Store::storage_metrics() -> StoreMetrics { db_bytes, wal_bytes, message_count, stored_attachment_bytes }`：观测接口，读失败的分量按 0 计。内存库的 `db_bytes` 为页数 × 页大小。

### 6. 进程内存归还与后台 QoS

- `daemon::release_unused_memory()`：macOS 调 `malloc_zone_pressure_relief(NULL, 0)` 把所有 zone 的空闲页还给内核（直接降低 `phys_footprint`，即 Activity Monitor「内存」列）；其他平台 no-op。调用点：守护进程完成账户首次上线等待之后、检索索引回填完成时、冷启动批量同步完成时。不放在热路径上。
- `daemon::current_process_memory_bytes() -> Option<(u64, Option<u64>)>`：macOS 经 `proc_pid_rusage(RUSAGE_INFO_V4)` 返回 `(ri_resident_size, Some(ri_phys_footprint))`；Linux 读 `/proc/self/statm` 返回 `(rss, None)`；其他平台 `None`。供状态监控与基准测试断言。
- `daemon::apply_background_qos()`：macOS 把调用线程标为 `QOS_CLASS_UTILITY`。只由平台托管的 `maild-agent` 在常驻路径上调用（维护子命令与交互式 `maild` 不调用）。选 `UTILITY` 而不是 `BACKGROUND`：后者的 I/O 与定时器节流会拖慢界面对本地 API 的请求。

### 7. 构建瘦身

- `crates/maild/Cargo.toml`：`reqwest` 移入 `[dev-dependencies]`。
- 根 `Cargo.toml` 新增 `[profile.release]`：`lto = "thin"`、`codegen-units = 1`、`strip = "symbols"`（只剥符号表，保留 unwind 信息，panic 回溯仍可用）。

## 后果

- 长连接常驻内存与挂机时长、收信总量脱钩：会话状态 O(folders)，SQLite 页缓存 ~1 MiB / 库，WAL ≤ 4 MiB。
- 大邮箱首次添加时只下载最新 200 封正文；本地列表起初只覆盖这段窗口，更早的邮件要靠服务器检索 + 按需拉取。界面经「服务器检索 → 按需拉回」提供更早邮件（已于全栈汇合接线）。
- 截断冷启动中被静默落库的邮件不经过过滤规则与 OTP 提取。这是有意的：它们是历史邮件，不是新到邮件。
- 断网期间重连尝试频率从每 5 秒一次降到最终每 5 分钟一次；用户可随时 `trigger_sync()` 立即重试。
- `ReceiveConfig` 新增公开字段：用结构体字面量构造它的代码（`accounts.rs::build_receive_config`、`tests/domain.rs`）需要补上该字段。

## 验收

`crates/maild/tests/receive_footprint.rs`（hermetic）：

1. 假 IMAP 服务器预置 500 封，`initial_sync_limit = 50` 冷启动：本地恰好落库 UID 451..=500，游标推进到 500；正文 FETCH 日志里没有任何 < 451 的 UID、没有 `1:*`、每条清单 ≤ 50；随后第 501 封经 IDLE 实时落库。
2. `fetch_message_by_uid("INBOX", 10)` 在同一条连接上拉回早期第 10 封并持久化（`get_message` 可读、幂等、不新建连接）；不存在的 UID 返回 `Ok(None)`。
3. `trigger_sync()` 打断 IDLE 并立即 sweep 非主文件夹（`folder_poll_secs = 3600` 下 Archive 新信秒级落库）；`reconnect_secs = 3600` 的断线退避被 `trigger_sync()` 立即打断重连，关断也立即生效。
4. 小信箱冷启动仍是单条 `1:*`。
5. `storage_metrics()` / `shrink_memory()` 在文件库与内存库上工作正常；`current_process_memory_bytes()` 在 macOS 上给出非零 RSS 与 `phys_footprint`。

`receive.rs` 单元测试覆盖去重环有界性、退避曲线与封顶、`initial_sync_limit` 的默认值与合并语义。

### Release 子进程实测（2026-11-17）

`crates/maild/examples/footprint_bench.rs`：release `maild` 独立子进程、3 个回环 TLS 账户（主信箱 1,500 封历史，
另两个各 300 封）、`initial_sync_limit = 200`。连续 3 次：冷启动到 3 × 200 封落库且 FTS 收敛 185–200 ms；
稳态 RSS 14.2–15.4 MiB、`phys_footprint` 4.95–5.08 MiB；5 秒空闲 CPU 0.0009–0.0013 %（1 次唤醒）；30 封实时
IDLE 推送 P50 3.8–4.1 ms、P95 7.4–10.5 ms；注入后 footprint 增量 +0.19～+0.33 MiB、WAL 恒 4 MiB；
窗口外第 5 封经服务器检索 + 按需拉回正文逐字一致。
