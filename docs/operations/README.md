# Featherpost 运行手册

本文覆盖 headless 守护进程（及平台托管的 Agent）的启动、健康判断、后台收信资源边界、关闭、IPC 恢复与质量门禁。
协议字段与错误码以 [`../protocol/README.md`](../protocol/README.md) 为准。

> **命名约定（[ADR-0047](../decisions/0047-decoupled-brand-and-open-source-readiness.md)）**：面向用户与操作系统的名字都来自
> 仓库根的 [`brand.toml`](../../brand.toml)。本文沿用早期工作代号书写命令时，`maild` / `maildctl` / `maild-agent` 依次指
> `brand.toml` 的 `cli_daemon` / `cli_ctl` / `agent_bin`（当前为 `featherpostd` / `featherpostctl` / `featherpost-agent`），
> `MAILD_*` 环境变量指 `<env_prefix>_*`（当前 `FEATHERPOST_*`），`maild.toml` / `maild.sqlite3` / `maild.sock` 指
> `<slug>.toml` / `<slug>.sqlite3` / `<slug>.sock`（当前 `featherpost.*`），`Maild.app` 指 `app_bundle_name`
> （当前 `Featherpost.app`）。Cargo 包名（`-p maild`、`-p maild-ui` 等）是内部稳定名，不随品牌变化。改名流程见
> [`../BRANDING.md`](../BRANDING.md)。

## 启动

生产配置不要直接修改示例文件。先查看本机统一目录，再把配置放到输出的 `config=` 路径：

```sh
maild paths
mkdir -p "/absolute/platform/path"
chmod 700 "/absolute/platform/path"
chmod 600 "/absolute/platform/path/maild.toml"
maild --config "/absolute/platform/path/maild.toml"
```

显式 positional/`--config` 优先，其次是 `MAILD_CONFIG`（必须绝对路径）。自动发现时，平台默认
配置与 cwd `maild.toml` 只有一处存在才会选中；两处指向不同文件会要求显式选择，同一文件的
路径别名不会误报。相对
`store.db_path` 锚定 data 目录，
配置派生的相对 Unix `api.listen` 锚定 runtime 目录。旧 cwd 数据库/sidecar/lock 或 socket 存在
时启动会 fail-closed，不会自动搬迁或创建另一套空数据；请写绝对旧路径继续使用，或停服后用
backup/restore 显式迁移。

Unix socket 的父目录必须由当前用户拥有且权限为 0700。Windows 使用
`transport = "named_pipe"`，服务端会为当前用户 SID 创建显式 DACL。HTTP 只能监听回环
地址，并要求至少 32 个 ASCII 可见字符的 bearer token。

日志只写 stderr；stdio 模式下 stdout 是 JSON-RPC 协议流，不能重定向普通日志到 stdout。

### macOS 后台 Agent

macOS 13+ 的正式进程形态是顶层 `Maild.app` 内嵌 `Maild Agent.app`。桌面端仅在用户点击时通过
Service Management 注册或注销 Agent，关闭桌面窗口不会停止或注销后台进程。Agent 不接受
`--config`、cwd `maild.toml` 或 `MAILD_CONFIG`，只读取 `maild paths` 所示的平台默认配置。

首次启动已安装的精确 `Maild.app` 时，如果没有显式配置/API 覆盖且平台配置不存在，桌面端会
no-clobber 创建 `~/Library/Application Support/maild/maild.toml`。该文件权限为 0600，只包含
本地 Unix API、store、rules、原生通知、内置前端和 Menu 的无凭据默认项；`[[accounts]]` 仅以
注释示例出现。已有路径（包括符号链接）不会覆盖。用户添加账户后，再在“后台”面板启用 Agent；
首启不会自动注册 Agent、猜测凭据或弹出通知授权。

缺失/损坏配置、路径冲突、Store 已被占用或 listener 无法装配时，Agent 会输出一条有界诊断并以
成功状态停止，避免 Login Item 的非零退出重启风暴；这不表示配置健康。修复配置后需从桌面端显式
重新注册/启动。bundle/发布脚本不执行注册；开发产物与正式签名产物的边界详见
[`../../packaging/macos/README.md`](../../packaging/macos/README.md)。

覆盖安装不会终止正在运行的旧版 Agent——它会继续持有受管 socket 并以旧协议代服务新前端。
桌面端在每个连接 epoch 做一次版本握手（ADR-0016）：不一致时禁用账户与写信入口并显示横幅，
「重启内核」按钮经 pid → SIGTERM（10s 超时升级 SIGKILL，残留 socket 由新内核受管回收）→
LaunchServices 打开内嵌 Agent → 重握手完成恢复；全程用户显式触发且单飞行。`maildctl` 在
stderr 警告但不阻断脚本。握手前部署的遗留内核以 -32601 应答识别，此时无法定位 pid，横幅
降级为手动指引（注销重登或重启 Mac）。

macOS 原生通知只在内嵌 Agent 的 bundle identity 下可用。读取状态不会弹窗；只有用户在桌面后台
面板点击“允许通知”或显式运行下列命令时才请求系统授权：

```sh
maildctl --socket "/absolute/private/path/maild.sock" notification-status
maildctl --socket "/absolute/private/path/maild.sock" request-notification-authorization
```

权限请求保持单飞，等待用户决定期间不会重复请求或空转轮询。拒绝授权、Agent identity 不匹配、
非 macOS 平台以及未成功挂载 rules 策略时都 fail-closed，不回退 `osascript`。系统通知仅是瞬时
提醒：checking/not-determined 阶段和队列满时直接丢弃，不在授权后补发已经持久化标记的旧验证码。

### 前端与外部进程插件

`maild paths` 会额外输出 `frontends=`、`plugins=` 和 `plugin-data=`。前两个 package 目录属于
受管 data root，**唯一写者是 Agent**（ADR-0019）：CLI 只把 package 复制进 `.incoming/<rand>` staging 再经 RPC 让 Agent
校验发布，升级要求版本严格更高且旧版自动进 `.rollback/<id>/`。Agent 不必停用：

```sh
maildctl --socket "/absolute/private/path/maild.sock" plugin install --source /path/to/package [--frontend]
maildctl --socket "/absolute/private/path/maild.sock" plugin enable <id>     # supervised 即时启动
maildctl --socket "/absolute/private/path/maild.sock" plugin disable <id>
maildctl --socket "/absolute/private/path/maild.sock" plugin rollback <id> [--frontend]
maildctl --socket "/absolute/private/path/maild.sock" plugin remove <id> [--frontend]
maildctl --socket "/absolute/private/path/maild.sock" frontends
maildctl --socket "/absolute/private/path/maild.sock" plugins
```

启用意图持久化在配置同目录的 `plugin_state.toml`（仅 Agent 写入、未知版本 fail-closed）；
`refreshPlugins` 做差异刷新——新启用的拉起、移除的停止、版本变化的重启，其余不受打扰。
`plugin-data/<id>` 是宿主按插件 ID 创建的私有可变数据目录：supervised spawn 通过
`MAILD_PLUGIN_DATA_DIR` 注入，升级、回滚或移除 package 时保留；它不是 API key 等秘密的存储位。
`.incoming`/`.removed`/`.rollback` 点前缀目录永不激活，崩溃残留不阻塞扫描。事务失败返回
`-32017`。frontend 的默认选择写在 `[plugins.frontend.config].default`。当前没有 marketplace
或沙箱；supervised child 必须保持前台，不能 daemonize。完整 manifest 和信任边界见
[`../plugins/README.md`](../plugins/README.md)。

### 插件身份注册表（M4-01）

Agent 在活动配置同目录维护 `plugin_auth.toml`（0600、原子发布、仅 Agent 写入），为每个
通过 manifest 检查的第三方包保存一条 SHA-256 凭证哈希。supervised 子进程每次 spawn 经
`MAILD_PLUGIN_TOKEN` 注入新凭证；independent/on_demand 由用户 `maildctl plugin-token <id>`
一次性 reveal（即轮换）。注册表损坏或未来版本时身份缝整体停摆：supervised 一律不再启动
（清单状态 `blocked`）、`authenticate` 返回 `-32015`，其余能力照常——修复需人工处理该文件后
重启 Agent，Agent 自身绝不带病清理。认证后的插件主体按 manifest `capabilities` 授权
（ADR-0018）：越权调用返回 `-32016` 并累计到清单的 `denied_calls`；邮件/账户推送只投递给
持有 `messages_read` 的主体。该身份体系是合作插件的问责与限权契约，不是同用户恶意进程
防线；进程级 containment 已按 ADR-0020 落地（进程组/Job Object 清扫 + `leaked` 终态），对同用户恶意进程的沙箱隔离仍归 M5。

### 文件夹同步运行边界（M4.5-02）

每个账户**恒用一条 IMAP 连接**。`IDLE` 只挂在主文件夹（配置里的 `mailbox`，缺省即
`role = inbox` 的那个）上；其余文件夹在同一条连接上按 `folder_poll_secs` 周期 sweep
（默认 300 秒，下界 30 秒），因此非收件箱的新邮件最长有一个周期的延迟，且**不产生通知**——
只有收件箱走过滤链、OTP 与通知管线。单账户同步的文件夹数上限为 64，超出部分仍会出现在
`listFolders` 里但不同步。

抓取是增量的：每个文件夹记 `UIDVALIDITY` 与末位 UID，重连只请求游标之后的 UID。
`UIDVALIDITY` 与库内不符时该文件夹**全量重建**（旧消息与附件删除、游标清零）——UID 空间
已重编号，旧 UID 与新 UID 没有对应关系。服务器不返回 `UIDVALIDITY` 的文件夹会被跳过并在
日志中说明。首次升级到 schema v4 后，每个账户会做一次 `LIST` 并对每个新发现的文件夹全量
抓取一次，之后才进入增量稳态；历史消息被回填到 `INBOX`，收件箱不会重下。

服务器上消失的文件夹只标记为缺席（`listFolders` 的 `present: false`），本地邮件保留；
文件夹重命名因此会留下一条旧记录和一条新记录，需要用户自行清理。标记位
（已读/星标/已回复/已删除）是**首次抓取时的快照**，服务器侧此后的变化要等 M4.5-05 的对账。

### 写路径与离线队列运行边界（M4.5-05）

已读、星标、归档、删除都是**先本地生效、再排队写回**。写回执行在收信那条连接上（不另开
连接），在 IDLE 的间隙进行；账户离线时 op 留在库里，恢复连接后按序执行。

`listPendingOps` 是这条队列的观测口。`state` 里最需要注意的是 **`unknown`——命令已发出但
没看到服务器的应答**。对 flag 类操作它会自动重试（最多 5 次）；对 move 类它是**终态**，
必须由用户裁决（`resolvePendingOp` 的 `retry` / `discard`），因为重放会在目标文件夹产生
第二份副本。运维上看到堆积的 `unknown` move，说明该账户的连接在写入窗口反复中断。

删除策略在内核侧判定：账户有 role 为 `trash` 的文件夹就移进去，否则落 `\Deleted`。
**服务器不宣告 UIDPLUS 时不会发 `EXPUNGE`**——裸 `EXPUNGE` 会连带清除其他客户端标记
`\Deleted` 的邮件。此时被删的信留在原文件夹并带 `\Deleted` 标记，这是 RFC 3501 的模型。

每次连接建立时对每个文件夹做一次标记对账。对账**不会**覆盖最近 60 秒内的本地改动，也不会
覆盖还有未落定 op 的消息。服务器宣告 CONDSTORE 时按 `HIGHESTMODSEQ` 短路整段对账；不支持
时退回全量，并顺带发现被其他客户端 `EXPUNGE` 的消息（只在全量轮次）。

### 发信与归档运行边界（M4.5-06）

发出去的信由内核组装：带 `Date` 与**我们自己生成的 `Message-ID`**（本地因此能把它和会话对上），
非 ASCII 主题/文件名走 RFC 2047，非 ASCII 正文整体 base64——**报文一定是纯 ASCII**，
用 `grep` 直接看原始报文会看到编码后的形式，这是正确的。

`bcc` 只出现在 SMTP 信封的 `RCPT TO` 里，绝不写进报文头。收件人是**逐项**校验的
（每个 addr-spec 单独过头注入防线），不是校验拼接后的字符串。

投递成功后，`sendFromAccount` 会把**同一份报文**入队归档到该账户的已发送文件夹。
归档是写队列里的一条 `append` op：**它失败不影响「已经发出去」这个事实**，可在
`listPendingOps` 里看到并重试。`APPEND` 不可重放（会在已发送里留下两份），因此和 `move`
一样，「结果未知」是终态、需要用户裁决。账户没有 role 为 `sent` 的文件夹时不入队。

委托给 provider 的账户仍然只支持单收件人纯文本，传 Cc/Bcc/HTML/附件会被明确拒绝。

**新文件附件上传尚不支持**：协议单帧上限 1 MiB，base64 之后装不下真实照片。转发可以携带原信
的附件（字节已经在库里）。

### 本地检索索引与线程（M4.5-07）

检索用 SQLite 自带的 FTS5，**不引入新的索引引擎**，索引与邮件在同一个库里，因此
`maild-backup` / `maild-restore` 一并覆盖，不需要单独维护。

升级到 schema v7 时**不索引任何历史消息**——几万封邮件的分词放进打开数据库的那个事务里，
会让 daemon 启动挂起、WAL 膨胀，崩溃还要整体回滚重来。历史消息由后台任务分批回填
（每批 200 条，批间停顿 200ms，约 1000 封/秒），游标与索引写入在同一个事务里提交，
**中断后从上次提交处续跑**。回填期间检索结果是部分的，`getSearchIndexStatus` 的 `pending`
给出剩余条数；新到的邮件不经回填，落库即可搜。回填完成后任务自行退出。

索引损坏或想强制重来时，删除索引并重置游标即可（`Store::rebuild_search_index`）；
分词器与删除触发器的定义在打开数据库时**逐字节校验**——换掉分词器不会报错，只会让所有
中文查询静默失效，所以这种改动必须在打开时就被拦住。

服务器侧 `SEARCH`（`scope=server`）走的是收信那条连接，不新建连接。它只对本地索引覆盖不到的
历史有意义：超出 64 个同步文件夹上限的信箱、未纳入同步的信箱、以及回填未完成的窗口。
非 ASCII 查询会带 `CHARSET UTF-8`，不接受该字符集的服务器会回 `BAD`，错误原样透出。

### transport provider 委托（M4-05）

受管账户的 incoming/outgoing 可以整体委托给声明 `transport_provider` 的插件：账户目录
`accounts.toml` 升至 format_version 2（v1 仍可读、写入一律 v2），方向配置用
`{ kind = "plugin", provider = "<插件 id>" }`（收信侧带 `poll_secs`，默认 60）。委托是
惰性绑定：创建/更新时强制探测经 `transport/probe` 走 provider，运行中 provider 未注册
或掉线则该方向快速失败 `-32018`，**绝不回退内置 transport**；超时 `-32019`；provider
业务失败 `-32020`。委托方向的账户没有 maild 凭据条目——凭据不跨缝，provider 插件的
API key 在其自身域内保管。注册观测：`listExternalPlugins` 各项的 `provider_registered`
反映活跃连接。切换 transport 类型可能更换 uid 空间，推荐「移除后重加」；完整插件侧
合同见 [`../plugins/README.md`](../plugins/README.md#transport-provider-契约adr-0021)。

测试缝：`MAILD_TEST_PROVIDER_TIMEOUT_MS` 可覆盖反向调用预算（毫秒），仅限 hermetic
测试进程设置；生产环境与 CI 均不注入，守护进程不读取其他来源。

### 地址层运行边界（M4-06）

别名和 disposable 地址持久化在 SQLite `addresses` 表（schema v2 引入），账户移除时级联删除；
历史消息只保留 `recipient` 文本。若进程在归档发布与 SQLite 删除之间退出，下一次启动会按
已归档 catalog 自动收敛地址残留。旧 schema 库会事务迁移到当前版本；恢复 v1 或 v2 备份时，
候选库会先在临时位置完成迁移、结构校验和 `integrity_check`，通过后才发布，失败不会替换当前库。

地址层不负责在上游创建地址。IMAP 账户必须先由服务商开启别名或 catch-all；自定义域、MX/DNS
验证归 M4-07。收件 recipient 取自邮件 `To` 首值，转发链或邮件列表可能改写它，因此只能作为
展示/溯源元数据，不能作为投递依据。别名发信只复用父账户 SMTP/provider；SPF、DKIM 和服务商
对 From 的许可仍由上游决定，部署前应使用目标 provider 实测收发与垃圾邮件判定。

### macOS 安装、升级与卸载

正式安装位固定为 `~/Applications/<app_bundle_name>`（当前 `~/Applications/Featherpost.app`）。管理脚本不提权、不调用 Service Management，也不
删除 config、数据库、WAL/SHM、备份、runtime 或日志。仓库开发 bundle 只能显式使用
`--development`；默认生产模式要求签名且已 staple 的正式产物：

```sh
packaging/macos/manage-app.sh --development install \
  --source /absolute/path/Featherpost.app
packaging/macos/manage-app.sh --development inventory
```

安装后从稳定安装位打开 App，由用户在“后台运行”面板显式启用 Agent。升级、回滚或卸载前，
先在同一面板关闭 Agent，等待状态完成，再运行带 `--confirm-unregistered` 的命令；已安装 Agent
会先取得生命周期锁，再通过 `maintenance-exec` 把锁 FD 交给执行整个文件事务的管理进程：

```sh
packaging/macos/manage-app.sh --development upgrade \
  --source /absolute/path/Featherpost.app --confirm-unregistered
packaging/macos/manage-app.sh --development rollback --confirm-unregistered
packaging/macos/manage-app.sh --development finalize
packaging/macos/manage-app.sh --development uninstall --confirm-unregistered
```

升级保留上一个完整 bundle，`rollback` 会交换当前版与回滚版，`finalize` 才把回滚版 no-clobber
移入废纸篓。进程在两个 rename 之间崩溃时，事务目录会阻止继续操作；确认 Agent 仍已注销后执行
`recover --confirm-unregistered`。旧版 Agent 若没有 `maintenance-exec`，脚本会拒绝猜测其进程状态；
首个迁移需人工确认旧进程退出后移走旧 bundle，再走 fresh install，不能用脚本参数绕过。

正式 universal2 构建和发布命令、签名/notary 凭据要求见
[`../../packaging/macos/README.md`](../../packaging/macos/README.md) 与
[`../decisions/0013-m3-5-macos-release-lifecycle.md`](../decisions/0013-m3-5-macos-release-lifecycle.md)。
打包脚本的全部名字（bundle、二进制、bundle ID、图标、`Application Support/<slug>`、`<ENV_PREFIX>_MAINTENANCE_*`、
`<ENV_PREFIX>_BUNDLE_*_VERSION`）都从 `packaging/macos/brand.env` 读取。

### macOS 发布 workflow 的仓库配置

`.github/workflows/release-macos.yml` 只在受保护的 `macos-release` Environment 中运行；为节省 GitHub Actions 额度已关闭 tag 推送自动触发，需在 Actions 页手动 `workflow_dispatch` 并选择 `vX.Y.Z` tag 作为 ref。产物名
（`<product_name>-vX.Y.Z-universal2.zip` 等）从 `brand.env` 读取。所需配置均为品牌中立名字，改品牌时无需改动：

| 类型 | 名称 | 内容 |
| --- | --- | --- |
| secret | `MACOS_DEVELOPER_ID_P12_BASE64` / `MACOS_DEVELOPER_ID_P12_PASSWORD` | Developer ID Application 证书（base64 p12）及其密码 |
| secret | `MACOS_NOTARY_KEY_P8_BASE64` | App Store Connect API key（base64 p8） |
| variable | `MACOS_SIGNING_IDENTITY` | `codesign --sign` 用的证书名或 SHA-1 |
| variable | `MACOS_NOTARY_KEY_ID` / `MACOS_NOTARY_ISSUER_ID` | notary API key ID 与 issuer UUID |

ADR-0047 之前的 workflow 读 `vars.MAILD_SIGNING_IDENTITY` / `MAILD_NOTARY_KEY_ID` / `MAILD_NOTARY_ISSUER_ID`；已配置过的
仓库需按上表改名。正式发布前还必须在 `brand.toml` 冻结组织控制的 bundle ID（不能是 `dev.<slug>.*`）、运行
`bash scripts/sync-brand.sh` 并提交 `packaging/macos/team-id.txt`。

## 健康与就绪

守护进程能响应 `status` 表示 API 和插件装配主循环存活：

```sh
maildctl --socket "/absolute/private/path/maild.sock" --json status
```

`accounts` 为空表示没有装配账户。已配置账户全部为 `online` 才表示收信链路就绪；
`connecting` / `reconnecting` 是过渡状态，`offline` 或非空 `detail` 应作为需要处理的诊断。
`message_count` 是当前 store 中的落库数量，不是服务健康探针。

启动时配置解析、API 安全校验或 listener 绑定失败会写 stderr 并以非零状态退出。单个领域
插件配置/装配失败会写明确警告，其他插件仍可继续运行；对应 API 方法返回 `-32001`，
不会让整个 API 退出。`status.accounts[].detail` 提供账户连接错误，`offline` 表示当前离线。

### 资源观测：`status.runtime`（ADR-0041）

`maildctl --json status` 原样输出 additive 的 `runtime` 对象：`rss_bytes`、`phys_footprint_bytes`
（macOS Activity Monitor「内存」列口径，Linux/Windows 为 `null`）、`store_db_bytes`、`store_wal_bytes`、
`stored_messages`、`stored_attachment_bytes`。它是观测量而不是健康探针：判断收信是否就绪仍看
`accounts[].state`。2026-11-17 release 实测基线（3 账户 IDLE、600 封窗口已落库）：稳态 RSS
14–15 MiB、`phys_footprint` ≈ 5 MiB、5 秒空闲 CPU ≈ 0.001 %，WAL 恒 ≤ 4 MiB；数值与复现命令见
[testing](../testing/README.md#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)。
`phys_footprint` 持续高于约 20 MiB 或 `store_wal_bytes` 长期超过 4 MiB 都值得排查（大附件同步、
长事务读者阻塞 checkpoint 等）。

## 后台收信与日常收信运行边界（ADR-0041 / 0042 / 0043）

### 冷启动窗口：`initial_sync_limit`

- `[[accounts]]` 的 `initial_sync_limit`（缺省 **200**，受管账户使用同一缺省）只作用于**没有持久游标**
  的文件夹：信箱超过窗口时只下载最新 N 封正文，每条正文 `UID FETCH` 最多 50 个 UID，游标直接推进到
  清单最大 UID。已有游标的文件夹永远只做增量，不受此值影响；改大它**不会**回补已经跳过的历史。
- `0` 表示不限制（v4 行为：冷启动整箱流式抓取）。只在小信箱或离线归档场景使用——5 万封量级的
  信箱会把全部 MIME 走一遍网络、解析与 waterfall。
- 截断冷启动中，只有主文件夹最新 5 封走过滤/验证码/通知实时链路，其余静默落库；`Date` 早于 24 小时
  的积压信不会触发新邮件通知。窗口之外的历史经 `searchMessages {scope: "server"}`（每次最多
  200 个命中，30 秒时限）定位，再用 `fetchServerMessage` 在同一条收信连接上按需拉回并幂等落库
  （30 秒时限，账户离线时立即 `-32001`）。两者都不新开 IMAP 连接（每账户恒一条）。
- 断线退避：首次失败按 `reconnect_secs`，之后指数增长封顶 300 秒并加 ≤10 % 抖动；
  `syncMessages`（`maildctl sync [--account <id>]`、菜单栏「立即同步」、界面 ⌘R）立即打断
  IDLE、轮询间隔与退避睡眠。半开连接由 TCP keepalive（macOS 120 s + 30 s × 4）在约 4 分钟内识别。
- SQLite 文件库运行时：页缓存约 1 MiB、`journal_size_limit` 4 MiB、`mmap_size = 0`；冷启动与索引回填
  完成后执行 `shrink_memory` + 被动 checkpoint，并把分配器空闲页归还内核。平台托管的 `maild-agent`
  常驻线程使用 macOS `QOS_CLASS_UTILITY`。

### SMTPS 465 与自动发现（ADR-0042）

- transport / 账户 outgoing 的 `tls = "implicit"` 是 465 风格隐式 TLS：TLS 握手后先读 `220` greeting
  再 `EHLO`。AUTH 按 EHLO 宣告协商：同时支持时用 `PLAIN`，只宣告 `LOGIN`（含旧式 `AUTH=LOGIN`）时用
  `AUTH LOGIN`；秘密缓冲发送后清零。DNS 解析 10 s、单地址连接 10 s（双栈逐地址尝试），会话内每步 30 s，`DATA` 结束后
  等待最终应答 180 s。端口与 TLS 模式配反会在 TLS 阶段失败或超时，并给出可行动诊断。
- `discoverEmailProvider`（`maildctl account discover <地址或域名> [--network]`）内置知识库命中时零
  网络；未命中且未显式允许网络时只返回启发式建议。允许网络时先做公网域名语法关，再对每个候选
  主机做解析关——任一解析结果落在回环/私网/link-local/CGNAT/保留段即整台放弃，只连接已校验的
  地址（防 DNS rebinding）。结果只是建议，账户仍须经 `probeAccount` 探测通过才保存。

### 规则侧车 `rules_state.toml`（ADR-0043）

- 位置：**活动配置文件同目录**的 `rules_state.toml`。`[plugins.rules.config] persist = true` 时装配期
  挂载；否则 `setSettings {persist: true}` 在守护进程有受管配置路径时运行时挂载（无路径 → `-32001`）。
- 内容是覆盖层（`format_version = 1` + `[rules]`），只记录用户在界面/CLI 改过的字段；生效配置 =
  TOML 基线 ⊕ 覆盖层。想恢复 TOML 默认，停服后删除该文件即可（不会自动重建，直到下一次持久写入）。
- 纪律与 `reading_policy.toml` / `plugin_state.toml` 相同：0600、当前用户拥有、单硬链接、拒绝符号链接
  与非普通文件、64 KiB 上限、未知 `format_version`/未知字段 fail-closed；写入是同目录临时文件 +
  `sync_all` + 原子 rename + 父目录 `sync_all`。侧车损坏或不安全时读取回落 TOML 基线、写入一律
  `-32001`，**绝不覆盖**该文件——人工检查后修复或删除再重启。
- 备份：侧车不在 SQLite 里，`maild backup` 不包含它；迁移机器时与 `maild.toml` 一起复制。
- 新邮件通知（`notify_new_mail`，缺省关闭）只针对收件箱未读新信：同账户 10 秒窗口内超过 3 封合并为
  一条提示；验证码信只发验证码通知；`muted_accounts` 最多 64 项。

### 语言偏好 `locale_preference.toml`（ADR-0048）

- 位置：活动配置文件同目录；文件不存在即「跟随系统」，首次 `setLocalePreference` 才创建。纪律与
  `rules_state.toml` 相同；损坏或来自未来版本时读到缺省偏好（`persisted: false`），写入 `-32001`
  且不覆盖，人工检查后修复或删除再重启。`maild backup` 不包含它。
- CLI：`maildctl locale` 查看，`maildctl locale set en zh-Hans` 设置有序列表，`maildctl locale set --system`
  恢复跟随系统。maildctl 的人读结果与警告随该偏好成文；连不上 daemon 时用系统首选语言。
  `maild backup` / `restore` / `paths` 不连 daemon，成功信息与失败标题按系统首选语言成文，报错详情不翻译。

## 受管账户目录与凭据（M3.7）

Agent 以平台默认配置运行时（`config_path == maild paths` 的 `config=` 下的
`maild.toml`），受管账户写入口开放；显式 `--config`、`MAILD_CONFIG` 或开发态 bare
daemon 继续使用旧 `[[accounts]]`，不会隐式创建新文件。

- **accounts.toml**：位于配置目录（macOS 为
  `~/Library/Application Support/maild/accounts.toml`），仅由 Agent 写入。
  owner 必须是当前用户，普通文件、单硬链接、无符号链接，Unix mode 0600；
  同目录临时文件 + fsync + 原子 rename + 父目录 fsync 发布。未知
  `format_version` 在任何凭据或网络副作用前拒绝启动。首次保存账户才创建，
  空安装不制造状态。
- **凭据库**：秘密只存 macOS Keychain（generic password item）。service 固定为
  `maild.account.credentials.v1`，account 属性为 `<acct_id>/<incoming|outgoing>/<revision>`，
  label 只含 Maild 产品名。密码不落入 TOML、SQLite、日志或 API 响应。
- **事务顺序**：校验 → 强制 probe → 写 Keychain 新 revision → 原子发布目录 →
  重挂 runtime → 成功后删除旧 revision。文件发布失败回滚内存目录并清理本次
  staged item；runtime 构造失败恢复上一版文件并按旧凭据重启。启动时成功解析
  目录后收敛本 service 下未被任何 profile 引用的孤儿 item；解析失败绝不清理。
- **移除**：软归档——停 runtime、删凭据、保留非秘密资料；本地邮件一律保留。
  归档记录不引用有效 revision。
- **故障行为**：目录损坏或未来 `format_version` → 配置停止且绝不清理凭据；
  凭据丢失（Keychain 项被删）→ profile 保留并进入 `needs_credentials`，重输密码
  即恢复，不回退 TOML 明文；服务端密码撤销 → 认证失败进入错误/重连，
  已验证配置不回滚，重输新密码即恢复；目录发布失败 → 删除本次 staged 凭据并
  回滚内存目录，清除障碍后可直接重试。
- **CLI**：`maildctl account list|test|add|provider-test|provider-add|edit|enable|disable|remove`；秘密只在
  TTY 无回显提示或 stdin 管道中输入，绝不进 argv/env。写命令需要 Agent 的
  Unix socket 传输（HTTP/stdio 会因来源门禁被拒）。

测试缝（仅 debug 构建）：`MAILD_TEST_CREDENTIAL_STORE=memory` 用按目录共享的内存
凭据库替代 Keychain，`MAILD_TEST_ACCEPT_INVALID_CERTS=1` 允许 hermetic 自签证书。
release 构建忽略两者，受管 profile 本身也不携带任何明文或自签开关。

## 关闭与恢复

用 Ctrl+C 或 SIGTERM 正常关闭。两种信号在账户上线等待和常驻阶段都会进入同一 LIFO 卸载，停止 IPC/HTTP listener，通知
流式 session 退出，关闭 receive/scheduler，并释放 SQLite 连接。流式写端若持续阻塞会在
关闭期限后被取消，避免守护进程无限等待。

不要把强制终止作为日常关闭方式。崩溃可能留下 Unix socket inode；交互式 `maild` 对任何
已存在的 socket 路径都 fail-closed。确认旧进程已经退出后手动删除该路径再启动。macOS 的
受管 Agent 是唯一例外：它持有 Agent 生命周期锁，并且只会在私有 managed runtime 内对同 UID、
inode 未变化且连接明确被拒绝的 socket 做恢复。正常退出只删除仍指向本进程所创建 inode 的路径。

### macOS 窗口、Dock 与菜单栏生命周期不退化红线（ADR-0044 / 0045 / 0046）

后续修改 `packaging/macos`、`crates/maild-launcher`、`crates/maild-ui`、`crates/maild-menu` 或 `crates/maild` 停机链路时，必须保持以下六条红线，防止重现「双 Dock 图标」「关闭后最近使用残留」「Dock 右键退出留『正在后台运行』僵尸图标」或「菜单栏与后台脱节」问题：

1. **单宿主执行与显式签名（[ADR-0044](../decisions/0044-macos-single-dock-identity.md)）**：`Maild.app/Contents/MacOS/maild-launcher` 通过 `NSWorkspace` 独立启动 `Maild Agent.app`（`activates = false`）后，必须原位 `exec` 同包 `Contents/MacOS/maild-ui`；签名时必须先对辅助二进制 `Contents/MacOS/maild-ui` 显式附带 `--identifier com.maild.ui --options runtime` 签名，再签外层 `Maild.app`（由 `verify-bundle.sh` 门禁守护）。
2. **顶层 `LSUIElement = true` 与动态 `Regular` / `Accessory` 切换（[ADR-0045](../decisions/0045-tray-only-minimize-and-close.md)）**：`packaging/macos/templates/App.Info.plist.in` 必须保持 `<key>LSUIElement</key><true/>`（并保留 `CFBundleIconFile = __BRAND_ICON_NAME__`，渲染为 `brand.toml` 的 `icon_name`），使 `Dock.app` 永不将退出后的顶层 App 留在「最近使用」槽位；UI（`brand.toml` 的 `ui_bin`）仅在主窗口打开或原生最小化期间切为 `ActivationPolicy::Regular` 并显式激活应用。
3. **黄灯最小化保持 100% 系统原生（[ADR-0045](../decisions/0045-tray-only-minimize-and-close.md)）**：严禁改写 `NSWindowButton::MiniaturizeButton` 的 target/action 或拦截 `⌘M`。点击黄灯 `–` / `⌘M` 必须走系统原生最小化，保留 Dock 应用图标与废纸篓旁的窗口缩略图。
4. **红叉 `×`、`⌘Q` 与 Dock 右键「退出」统一降级后再退出（[ADR-0046](../decisions/0046-menubar-lifecycle-and-dock-quit.md)）**：由于 `tao` 默认未实现 `applicationShouldTerminate:`，`crates/maild-ui/src/macos_window.rs` 必须在 `.setup()` 向 `NSApp.delegate()` 运行时类注入 `applicationShouldTerminate:` 并通过 `setDelegate(None); setDelegate(Some(&delegate))` 刷新 AppKit 选择子缓存：
   - 常规退出返回 `NSTerminateCancel` 并统一进入 `request_close_and_exit` → 刷盘草稿（尊重用户发起关闭时的上传/确认否决）→ 主线程 `orderOut:` + `setActivationPolicy(Accessory)` + `deactivate()` → 等待 50 ms CFRunLoop 提交窗口使 `launchservicesd` 落定 `type="UIElement"` → `app.exit(0)`，彻底杜绝 `type="Foreground" (exited-with-subordinates)` 导致的「Maild 正在后台运行」Dock 僵尸图标；
   - 当前 AppleEvent 含 `keyAEQuitReason`（系统注销/重启/关机）时必须同步降级 `Accessory` 并直接返回 `NSTerminateNow`，不得阻断系统注销。
5. **顶部菜单栏与后台生命周期强绑定，仅保留「退出 Maild」（[ADR-0046](../decisions/0046-menubar-lifecycle-and-dock-quit.md)）**：`crates/maild/src/menu.rs` 在守护进程运行期间持续守护 `maild-menu`（不得因 `0` 退出码停止拉起）；菜单最后一项固定为 **「退出 Maild」**，点击后调 `stopManagedAgent`，由守护进程先向已认证前端推送 `agent/stopping`（350 ms 排空窗口，前端带 500 ms 兜底退出），一并干净关闭前端窗口、Dock 图标、菜单栏、外部插件与后台守护（若守护进程已离线，`maild-menu` 兜底通过 `.main-ui.lock` 终止残留主窗口并自行退出）。
6. **真机快速核验命令**：任何涉及上述链路的改动在真机验收时，可用 `lsappinfo info "Maild"` 与 `ps -axo pid,ppid,command | grep -i Maild` 核对：窗口通过红叉、`⌘Q` 或 Dock 右键「退出」关闭后，`lsappinfo` 中不得残留 `type="Foreground" (exited-with-subordinates)` 条目；点击菜单栏「退出 Maild」后，所有 `maild-agent` / `maild-menu` / `maild-ui` 进程均应全部退出。

## 在线备份

备份可以在 daemon 运行时执行。`--database` 必须指向实际 SQLite 文件；命令不会读取配置文件，
因此配置里的相对 `db_path` 要按 `maild paths` 输出的 `data=` 目录解析成明确路径：

```sh
maild backup \
  --database "/absolute/path/maild.sqlite3" \
  --output "/absolute/path/backups/maild-2026-08-20.sqlite3"
```

目标父目录必须已存在，目标文件必须不存在。命令会先固定父目录的规范化路径；Unix 下解析后的
目录必须由当前用户拥有，并移除 group/other 写权限（例如
`chmod go-w /absolute/path/backups`）。命令使用 SQLite Online Backup 读取包含 WAL 最新提交
的一致快照，在同目录临时文件中完成 schema 与 `PRAGMA quick_check` 校验、同步落盘后才发布；
所有发布前失败都不会覆盖既有目标。Unix 最终文件权限为 0600，发布后同步目标目录；若这一步
失败，错误会明确说明完整目标已存在但掉电持久性未确认。成功信息写 stdout，错误写 stderr；
该 stdout 不属于 daemon 的 stdio JSON-RPC 会话。

备份是未压缩、未加密的完整邮件数据库，目标目录和离线介质必须按敏感数据保护。当前没有自动
轮转或删除。强制终止备份进程可能留下 `.maild-backup-*` 文件；
确认没有备份命令运行后才可人工删除。

## 单实例与恢复

每个文件型 store 会在数据库旁长期保留 `.<database-name>.maild.lock`，运行期间持有跨进程独占
锁。不要删除这个文件来“解锁”；进程退出会释放内核锁，保留 inode 是为了避免两个进程锁住
不同文件。Unix 主库与锁文件都要求当前 uid 所有，Store 打开前会把主库及已有 WAL/SHM/journal
权限收紧为 0600；符号链接和多硬链接路径会被拒绝。重复 daemon 会在 store 装配阶段失败，restore 在 daemon
运行时也会立即失败。

恢复前先正常停止 daemon，再明确指定当前库、输入备份和一个尚不存在的 rollback 路径：

```sh
maild restore \
  --database "/absolute/path/maild.sqlite3" \
  --input "/absolute/path/backups/maild-2026-08-20.sqlite3" \
  --rollback "/absolute/path/backups/pre-restore-2026-08-20.sqlite3"
```

命令先取得维护锁，校验当前库和输入的 schema/quick_check，并在当前库目录准备已同步的 replacement；
随后用 Online Backup 把当前库完整写入 rollback，只有 rollback 文件和目录都同步成功后，才 checkpoint
当前 WAL、切换 DELETE journal、清理旧 `-wal`/`-shm` 并原子替换。任何发布前失败都不删除当前库
或 rollback；发布后的目录同步失败会明确说明新库已经生效但掉电持久性未确认。

成功后先启动 daemon 并用 `maildctl status`、消息列表和一封详情验证，再决定 rollback 的离线保留
周期。命令不会自动重启 daemon，也没有 `--force` 或自动删除策略。精确相对配置名 `backup`、
`restore` 和 `paths` 是维护子命令保留字；确需使用这些相对文件名时写
`maild ./backup`、`maild ./restore` 或 `maild ./paths`。

## 资源边界

| 边界 | 当前值/行为 |
|---|---|
| JSON-RPC 请求帧 | 1 MiB，超限后返回 `-32600` 并关闭流式连接 |
| JSON-RPC 响应 | 1 MiB，超限返回 `-32603`，不截断详情 |
| IPC session | 最多 64 个；超过上限的新连接立即关闭 |
| 每个 session 输出队列 | 32 条；慢消费者的通知会丢弃，请用查询 API 补历史 |
| 推送正文/文本字段 | 64 KiB / 8 KiB，在 UTF-8 边界截断 |
| `listMessages` | 默认 50、最多 500；大正文时应减小分页 |
| `listMessageSummaries` | 默认 50、最多 500；可按 account 过滤且不返回正文，桌面列表首选 |
| CLI deadline | IPC 连接 5 秒；每次 RPC 总时限 30 秒 |
| 原生通知字段 | 标题 256 bytes、正文 2 KiB；控制字符清理并在 UTF-8 边界截断 |
| 原生通知队列 | 已授权投递最多 32 条；单 worker、单 native in-flight；满队列或未授权直接丢弃 |
| 通知 worker 停机 | cordis disposer 最多等待 2 秒；`Drop` 不阻塞，停机时丢弃剩余瞬时通知 |
| frontend package / intent | 最多 64 个 package、manifest 64 KiB、待领取 intent 64 条 |
| external package / child | 最多 32 个 package；只有显式 allowlist 启动；指数退避且 LIFO 总等待 2 秒 |
| 冷启动正文窗口 | 每个无游标文件夹最新 `initial_sync_limit`（缺省 200）封；每条正文 FETCH ≤ 50 个 UID |
| 服务器检索 / 按需拉取 | 每次最多 200 个命中；各 30 秒时限；共用该账户唯一 IMAP 连接，排队满即拒绝 |
| 收信会话去重 | 每文件夹 256 个 UID 环形窗口，最多 64 个文件夹；常驻内存与收信总量无关 |
| 断线重连 | 指数退避封顶 300 秒 + ≤10 % 抖动；`syncMessages` 立即打断 |
| SQLite 运行时 | 页缓存约 1 MiB；WAL checkpoint 后截回 ≤ 4 MiB；不使用 mmap |
| 新邮件通知限流 | 同账户 10 秒窗口 3 封逐封 + 1 条合并；去重记忆 4096 条；跟踪 ≤ 256 个账户 |
| `rules_state.toml` | 64 KiB；0600 原子写；损坏 fail-closed 不覆盖 |

SQLite 邮件总量、单封原始邮件大小和长期日志目前没有全局配额；它们属于后续持续质量项。
生产部署应监控数据库与日志目录，并避免把 API 通知当作可靠队列。

## 质量门禁

```sh
bash scripts/sync-brand.sh --check        # brand.toml 与全部派生文件一致（需 python3 ≥ 3.11）
pnpm install --frozen-lockfile
pnpm --dir packages/featherpost-studio typecheck && pnpm --dir packages/featherpost-studio test && pnpm --dir packages/featherpost-studio build
CARGO_INCREMENTAL=0 cargo test --workspace --all-targets --locked
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings
bash -n packaging/macos/*.sh packaging/macos/tests/*.sh
packaging/macos/dry-run.sh
packaging/macos/tests/test_bundle.sh
packaging/macos/tests/test_release.sh
packaging/macos/tests/test_lifecycle.sh
# 资源基准（release 子进程，约 25 秒；任一验收不成立即非零退出）
cargo build --release -p maild --examples --bins --locked
target/release/examples/footprint_bench
```

以上命令用到的 cargo / clippy / pnpm / python3 都由仓库根 `mise.toml` 固定版本（`mise install` 一次装齐，含 clippy、
rustfmt 与 `aarch64-apple-darwin`、`x86_64-apple-darwin`、`x86_64-pc-windows-gnu` 三个 target）。Windows GNU 交叉
Clippy 还需要 mise 之外的 mingw-w64 C 工具链（macOS 上 `brew install mingw-w64`），供带 C 代码的依赖的 build script 使用。

`.github/workflows/ci.yml` 已关闭 push / PR 自动触发（节省 GitHub Actions 额度），仅保留手动 `workflow_dispatch`；日常门禁在本机跑。其 `brand` job 运行 `bash scripts/sync-brand.sh --check`；`rust` job 在 macOS 和 Windows
运行 workspace 测试与 clippy，并在 macOS 构建、复验 actual universal2 bundle、Preview DMG（名字取自 `brand.env`）
和全部 hermetic packaging fixtures。Windows
named pipe 的真实 ACL/连接行为以 Windows job 为准；本机交叉编译只能证明目标代码可编译。

## 可选：国内 crates.io 镜像

仓库**不再提交** `.cargo/config.toml`（ADR-0047）：默认直连 crates.io，开源贡献者与 CI 不会被某个镜像绑定。中国大陆
网络可以在**用户级** `~/.cargo/config.toml`（或 `$CARGO_HOME/config.toml`）里自行替换源，例如 rsproxy：

```toml
[source.crates-io]
replace-with = "rsproxy-sparse"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
```

rustup 也可走同一镜像：`export RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup`。
npm 依赖可选 `pnpm config set registry https://registry.npmmirror.com`（同样是用户级设置，不改 `pnpm-lock.yaml`）。

注意事项：

- 镜像只改下载来源，`Cargo.lock` 的 checksum 与 crates.io 相同，`--locked` 照常生效；不要把镜像配置提交回仓库。
- Cargo 的本地 registry 缓存按**源**分目录。切换镜像（或删掉镜像配置）后第一次构建会重新下载索引与 crate，此前
  在另一个源下预热的缓存不能满足 `--offline`；离线构建前先在当前源下联网 `cargo fetch --locked` 一次。

## 新附件上传与草稿恢复（M4.5-G04）

上传通道见 [ADR-0035](../decisions/0035-draft-attachment-staging.md)。Unix staging 在数据库旁
`.数据库文件名.attachments-staging/`，必须是本用户属主、0700 的真实目录；不接受 symlink。
上传写入匿名文件，无可访问的附件路径；不要手动往该目录放文件。daemon 拒绝不安全目录，
不会通过 chmod 修改既有目录。Windows 路径拒绝 reparse point，使用系统临时文件删除语义；
本轮验收平台为 macOS，Windows 真机仍需发布门禁验证。

未完成上传 30 分钟无活动过期，每分钟回收；进程退出关闭句柄，重新启动需重新上传。
Unix 在写第一字节之前 unlink；若恰在 create/unlink 之间被强杀，最多留下零字节随机名字，
不会留下附件内容。上传失败/主动取消不会删除用户选中的源文件。

finish 成功后的新附件在邮件库里，与草稿一同参与 backup/restore；删除草稿或移除附件会删除
对应 BLOB（不承诺对 SQLite 空闲页做安全擦除）。恢复旧备份会恢复其中的草稿和附件。
v8 → v9 自动迁移，旧版本二进制不能打开 v9；降级须恢复升级前备份。原有入站附件的 2 MiB
缓存上限不变，**新上传附件**每文件 20 MiB；发信附件合计 25 MiB，MIME 后体积更大。

冲突或 finish 回执不确定时，保留 upload_id，先查草稿再重试 finish，不重新生成上传任务。
`maildctl draft finish-upload UPLOAD --version VERSION` / `cancel-upload UPLOAD` 可操作已有上传。


## 写信与投递恢复（M4.5-08-A）

草稿与新附件由 daemon 保存，Tauri 显示“已保存到本机”后才有持久保证；关闭窗口/⌘Q 会等
最后一笔保存，失败或上传未完成时保留窗口。强杀 UI 只能恢复最近已确认的版本，关闭 UI 不停 daemon。

schema v9 自动迁移到 v10，投递状态与备份一起保存。重启将遗留 sending 收敛为 unknown；
先核对已发送文件夹/收件人，再在编辑器明确解锁，不能把重启当作重发指令。Sent 副本入队失败
不等于投递失败。恢复较早备份不能撤销已经发生的投递，旧版本草稿发送前也须核对。
未发送完成的原生文件 token 不跨 UI 重启保留；未提交的 staging 会按既有 TTL/重启清理。
已上传完成的附件字节随草稿恢复。具体状态与权限见 [协议](../protocol/README.md)。


## 邮件写回与活动恢复（M4.5-08-B）

Tauri 的“已排队”只表示本机接受操作，服务器离线时会继续等待；“已是目标状态”无需再建
操作。部分未确认时先刷新并查看同步活动，不要连续重复移动。活动记录的主题可能因源消息
已移走而缺失；APPEND 副本使用账户/目标文件夹/提交时间识别，不为了展示而读取整封报文。

failed 可以主动重试；unknown 的移动/APPEND 先核对目标文件夹，再明确确认是否重试，可能
产生重复副本。“不再重试”不会撤销服务器动作。活动摘要故意不显示服务器错误原文，以免
服务端回显凭据或邮件内容；详细服务诊断继续按运维日志的本地边界处理。

本片不迁移数据库（仍为 v10），不更改默认前端安装。新通知有完整文件夹身份；旧通知只打开
账户列表，不猜同 UID 的邮件。账户管理 UI 已在 08-C 接入，最终移除 egui 和签名安装包仍按后续任务交付。

## Tauri 账户、设置与受管扩展（M4.5-08-C）

设置中的账户探测不会保存账户或创建远程邮箱。已有密码不回填；更新密码时重新输入，版本
冲突后先刷新实际资料。移除账户保留本地邮件并删除受管凭据，不删除服务器邮件；先检查同步
活动里的未完成操作。插件私有凭据仍在该插件自己的管理窗口配置。

规则保存回执为 `persisted:false`，只对当前后台运行生效。通知权限只在显式点击后申请。
本地地址登记不等于服务商已创建别名或允许发信，SMTP 别名需要用户确认服务商支持。

安装入口选择完整包目录，Rust 复制到连接中 daemon 的 staging 并取得完整校验预览，确认后
安装同一副本；取消会清理 staging。升级与回滚保留现有单层互换契约，回滚也要确认目标权限。
操作报错后点击扩展页“刷新”，重扫目录再核对版本，不自动重试原操作。
移除已启用包先持久停用，再删除目录；若删除失败，包可能已停用，以刷新后的清单为准。
停用 independent 包只保存启用意图，不承诺停止由用户自行启动的外部进程。

默认前端选择保存在前端安装根 `.default.json`，不存在时使用 TOML 种子，选定后不会自动关闭
当前窗口。旧 TOML 默认可在改选后移除。状态文件损坏不会被覆盖；发布落盘结果不确定时冻结
后续选择/前端移除，先修复存储问题并重启后台。此功能不改变安装包的默认交付策略。
