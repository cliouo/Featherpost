# ADR-0021：M4-05 外部 transport provider

- 状态：已接受，M4-05 实现中
- 日期：2026-08-23
- 适用范围：maild RPC 协议（双向化）、maild-client、external manifest 词表、受管账户 schema、receive/smtp/probe 调用点、桌面 profile 视图；User 主体的既有方法行为零变化，legacy TOML 账户不可委托

## 背景

Wave 1（ADR-0017–0020）交付了插件主体、方法级授权、安装事务与进程 containment，
但生态里还只有「插件调用内核」一个方向。愿景（`ARCHITECTURE.md` §6/§7）的核心是
反方向：**provider 插件装上即多一种接入方式**——JMAP/Gmail-API 收信、Resend/Mailgun
发信、以及后续 alias/dns/transfer 缝全部依赖「内核按需调用插件」的能力。当前协议
通道严格单向：客户端请求→服务端应答＋两种通知；服务端没有任何外呼机制
（无出站 id 分配、无 pending 表），客户端解码器甚至会拒绝带 `method` 的入站帧。

本切片冻结这条双向缝，并以 transport 为第一个消费方：受管账户的 incoming/outgoing
可以整体委托给一个受授权插件。

### 愿景对照

- §6 transport/receive 缝的 Provider 列（smtp/resend/mailgun/jmap/gmail-api）——
  本切片把这一列从「未来注记」变成可装载的契约；「装上即多一种接入方式，卸掉即消失」
  由既有安装事务与连接生命周期直接兑现。
- §7 的场景映射（无限别名、自定域、转让、隐私随机邮箱）全部要跨这条缝调用插件——
  本切片交付的是它们共用的传输原语，而非任何一个具体业务。
- 最小资源形态不受损：provider 不在线时委托方向快速失败，内核不为它常驻任何轮询
  以外的资源；内置 IMAP/SMTP 路径一行不改。

## 目标

1. 守护进程能在**已认证的插件长连接**上发起 JSON-RPC 请求并等待其应答（有界超时）。
2. 受管账户的 incoming/outgoing 可显式委托给指定插件；未注册/掉线时快速失败，
   绝不静默回退内置 transport。
3. 委托只能由用户经账户方法建立（插件不能自我挂载）；每次委托操作有脱敏审计；
   账户凭据绝不跨越本缝。
4. 完全 additive：协议代数不晋升；旧插件、旧客户端、legacy 账户行为逐字节不变。

## 非目标

- 不做 IDLE 式推送收信（v1 仅 poll，间隔复用 `poll_secs`）；不做已读/删除/旗标同步
  （追加式收件箱视图）。
- 不做 OAuth2/Exchange provider 本身——那些是第三方插件的业务。
- 不做委托的桌面向导（RPC/CLI 先行；桌面只保证能渲染委托态 profile，见决策 8）。
- 不做逐插件网络/资源配额、同意 UI、provider 市场与分发（延续冻结决策）。
- supervisor 不引入「等插件连上才算启动」的门：注册是运行时状态，不可用走快速失败
  （对齐 M4-04 以来「观测而不等待」的姿态）。

## 决策

### 1. 反向调用通道（本切片的核心基建）

在既有帧协议（行分帧、1 MiB 上限）上做最小双向化：

- **服务端出站请求**：每个流会话的输出 mpsc 旁新增一张 per-connection 出站表：
  守护进程分配单调 id，写入 `{"jsonrpc":"2.0","id":N,"method":…,"params":…}` 帧，
  并在 pending 表登记 `oneshot` 派生通道；读循环解析到带 `result`/`error` 且 id
  命中 pending 的帧时完成对应条目（未知 id 的迟到应答静默丢弃并计数）。请求方
  `tokio::time::timeout` 兜底；超时移除 pending 条目，迟到的应答自然落入「未知 id」。
- **客户端入站请求**：`maild-client::Incoming` 增加 `Request { id, method, params }`
  变体；`StreamConnection` 增加可选的入站处理器注册——插件为处理器 spawn 一个任务，
  把应答用匹配 id 写回同一 writer。便捷 `RpcClient` 不暴露该面（保持无主体语义）。
  未注册处理器的连接收到入站请求时返回 `-32601` 应答（诚实降级，不断连）。
- 方向规则：守护进程只会向**已注册 provider 的连接**发起请求（见决策 2），因此
  旧客户端/旧插件永远不会收到看不懂的帧——双向化对不参与者不可见。

### 2. 注册与发现

- manifest 词表新增第 8 个 capability：`transport_provider`（仅 external manifest；
  frontend route 翻译表不变，frontend 主体永远无法注册）。旧 daemon 加载新词清单
  在扫描期 fail-closed 拒绝——插件在旧宿主上惰性化，正是 ADR-0016 遗留降级的既定
  形态，由版本握手引导升级。
- 新增插件→内核方法 `providerRegister {}`，策略 `Capability("transport_provider")`。
  成功即把**当前连接**登记为该插件 id 的活跃 provider 端点（持有输出句柄与 pending
  表）。同一插件 id 再次注册时 latest-wins 顶替旧端点；连接关闭（含 -32014 断连）
  自动注销。
- 注册表是 Context 内的服务（`ProviderRegistry`）：`BTreeMap<plugin_id, Endpoint>`；
  注销时该端点上未决的反向请求立即以 `-32018` 失败。注册/顶替/注销写 stderr 审计行。
- `listExternalPlugins` 增加 `provider_registered: bool` 观测字段（serde default，
  旧 daemon 为 false）；不暴露端点句柄或地址。

### 3. 账户委托绑定（catalog format_version 1 → 2）

- `IncomingSettings` / `OutgoingSettings` 各新增变体
  `Plugin { provider: String }`（tag `kind="plugin"`；`provider` 是插件包 id，
  只做语法校验——**惰性绑定**，安装顺序不耦合，未就绪时调用期报 -32018）。
- `CATALOG_FORMAT_VERSION` 升到 2：读取接受 1 与 2，任何写入一律落盘 2；v1 目录
  校验禁止出现 plugin 变体（正常产物不可能，防御手编）。既有 `FutureVersion`
  fail-closed 路径原样覆盖「旧 daemon 读新目录」——比让 serde 报 unknown variant
  清晰得多。否决了「不升版本只加字段」：profile 层未知字段会被旧 daemon 静默忽略，
  恰好制造我们必须禁止的静默回退。
- 校验规则：委托方向的凭据 revision 必须为空（凭据不跨缝）；`enabled=false` /
  archived 的既有语义不变；legacy TOML 账户不在受管目录内，天然不可委托。

### 4. 反向方法与语义

三个守护进程→插件方法（命名沿用 `message/received` 的斜杠风格）：

| 方法 | 参数 | 应答 | 超时 |
|---|---|---|---|
| `transport/probe` | `{account, direction: "incoming"\|"outgoing"}` | `{ok, stage, detail?}`（映射进既有 `ProbeOutcome`，新增 `stage="provider"`） | 10s |
| `transport/send` | `{account, from, to, subject, body}` | `{}` | 30s |
| `transport/receive` | `{account, since_uid, max}` | `{messages: [{uid, sender, subject, body, received_at}]}` | 60s |

- **send**：`sendFromAccount` 在 outgoing 为 Plugin 变体时改走插件；`from` 固定为
  profile.address，由内核填入。插件应答 error 对象 → 映射 `-32020`（detail 有界脱敏）。
- **receive 合同**：`uid` 为十进制数字符串，且在同一账户上**严格单调递增**（IMAP UID
  心智模型）；游标就是库内最大已存 uid（新增 store 只读辅助查询，不引入新持久化）。
  `max` 由内核设定（≤20），响应必须落在单帧上限内；一批满则继续拉取直到短批或时间
  预算耗尽。取回的消息进入既有 `MailReceived` waterfall（store/filters/OTP/notify/
  推送全链路复用），uid 重复由既有入库幂等性兜底。
- **probe**：`probeAccount` 对委托方向改为调插件（端到端可用性检查，含插件自己的
  上游）；插件不可达即探测失败，detail=`provider unavailable`。
- 插件崩溃/断线发生在调用中途：pending 条目随注销立即失败（-32018），调用方拿到
  明确错误而非挂起。

### 5. 错误码

接续 -32017：

```text
PROVIDER_UNAVAILABLE = -32018  // 无活跃注册、连接刚断、注销竞态
PROVIDER_TIMEOUT     = -32019  // 反向调用超过各自预算
PROVIDER_ERROR       = -32020  // 插件应答的业务失败（detail 来自插件，有界脱敏）
```

### 6. 授权、审计与凭据边界

- 委托绑定只能经 UserOnly 的账户方法修改（createAccount/updateAccount 的参数校验
  自然覆盖）；插件主体对自己或其他账户的绑定无任何入口（ADR-0018 已冻结账户方法
  UserOnly，此处是其直接受益者）。
- stderr 审计行（脱敏格式，同既有纪律）：注册/顶替/注销、委托绑定变更、每次委托
  probe/send/receive 的结果（principal、account、direction、outcome）。**不含收件人、
  主题、正文或任何秘密。**
- 凭据绝不跨越本缝：委托方向的账户没有凭据条目；provider 插件的 API key 等 secrets
  完全在插件自己的域内管理（env/自有文件），maild 既不保管也不注入。

### 7. 失败隔离（provider 故障不拖垮内置 transport）

- 所有跨缝调用有硬超时；调用期间不持有任何跨账户锁；每账户运行时本就独立成任务，
  一个委托账户的故障不影响其他账户的 IMAP/SMTP 循环。
- **永不静默回退**：委托方向在 provider 不可用时按方向语义失败（send → -32018 返给
  调用方、compose 保留草稿；receive → 记退避重试，账户状态可见）。用户明确选择了
  provider，静默换道内置 SMTP 会违背选择（例如经隐私中继发信的预期）。
- Agent 停机路径先注销注册表再走 LIFO teardown：未决反向请求即刻失败，停机预算
  （2s）绝不为 provider 调用延长。

### 8. 客户端视图与桌面

- `maild-client` 的 `IncomingView`/`OutgoingView` 改为容忍缺省字段并增加
  `provider: Option<String>`：否则新 daemon 的委托态 profile 会让旧桌面的整个
  profile 列表反序列化失败。桌面账户列表对 plugin kind 显示「由插件 `<id>` 接管」
  摘要徽标；向导 preset 不变（插件 onboarding 走 RPC，属后续切片）。

### 9. 协议代数与兼容性

- 新增方法、错误码、capability 词、catalog 版本均为 additive，`PROTOCOL_VERSION`
  不晋升；`getAgentInfo` 判定表不变。manifest 的 `protocol_version` 保持 1（线上
  变更 additive）。
- 四个方向的矩阵：新 daemon+新插件=完整功能；新 daemon+旧插件=照旧以 User 或
  认证主体工作、只是没人注册 provider（委托给它的账户 -32018，诊断清晰）；
  旧 daemon+新插件=manifest 扫描期拒绝（遗留降级）；旧客户端+新 daemon=零接触
  （不会收到反向帧）。

## 备选方案与取舍

| 轴 | 选定 | 否决项与理由 |
|---|---|---|
| 调用方向机制 | 已认证长连接上的反向 JSON-RPC | **任务队列拉取**（插件 poll 领任务）：交互式发信延迟不可控、任务状态机复杂度全落在协议里、断线恢复语义含糊；**插件自建 socket 服务**：信任倒置——daemon 反过来要认证到插件，绕开 containment 与身份缝，端点发现又添一个攻击面 |
| 注册时机 | 显式 `providerRegister`（capability 门控） | **authenticate 自动注册**：把「有身份」和「提供服务」两个正交概念焊死，纯前端型插件被迫声明 transport_provider 才能认证，破坏最小授权 |
| 绑定存储 | catalog v2 新增 Plugin 变体 | **仅加可选字段不升版**：旧 daemon 静默忽略未知字段 → 静默回退内置 transport，正是必须禁止的行为；**独立 delegation.toml**：多一份状态文件与 accounts.toml 的一致性（revision/enabled/archived）无从校验 |
| 收信游标 | 库内 max(uid)，契约要求单调递增 uid | **opaque cursor 持久化**：要动 store schema 与迁移；**插件自持久化**：supervised 每 spawn 重置，状态无处安放；单调 uid 让游标成为派生值，零新增持久化 |
| 错误建模 | 三个专用码 | **单一 PROVIDER_FAILED**：CLI 脚本无法区分「没起来」「超时」「上游拒绝」，排障与自动化都受损；**透传插件任意 error.code**：把第三方代码空间倒灌进稳定协议 |

## 安全与信任边界（诚实声明）

- 本缝不改变同用户信任域：一个恶意 provider 插件今天就能以 User 身份连 socket 全量
  作恶。缝的价值是**结构化的合作面**——显式授权（用户点名插件 id）、可归因（主体 +
  审计行）、可吊销（remove 包 / 解除绑定）、最小授权（没有 transport_provider 就永远
  注册不上）。对抗同用户恶意进程仍归 M5 沙箱。
- 反向调用把「插件执行第三方代码处理邮件内容」变成常态：邮件正文会出现在插件进程
  内存里。这与「插件可以读用户邮件」的同域现实一致，但文档必须明示，安装前审查
  提示（plugins README 已有措辞）同步覆盖 provider 场景。
- provider 自管的 secrets 不在本缝保护范围内（同用户可读，尽力而为）；maild 侧的
  红线只有一条：账户凭据绝不序列化进任何跨缝载荷。
- 1 MiB 帧上限同时约束请求与应答；receive 分批参数由此推导，防止恶意 max 撑爆帧
  （内核侧 clamp，不信插件）。

## 测试策略（hermetic）

- 单测：出站 id 分配/pending 完成/迟到丢弃；注册表 latest-wins/关闭注销/未决失败；
  catalog v1/v2 读取矩阵与 v1 禁 plugin 变体；委托校验（凭据 revision 必须为空）；
  策略表钉住 `providerRegister`；client 解码器接受 Request 帧、未注册处理器回 -32601。
- e2e（`tests/plugin_transport.rs`，Unix 先行，Windows 行为归真机/CI 证据）：fixture
  provider 包以长连接认证并注册，托管脚本应答三类反向调用——
  1. sendFromAccount 到委托账户落到 provider 脚本（内容核对）且审计行出现；
  2. provider 未注册时 sendFromAccount 即得 -32018，内置其他账户不受影响；
  3. transport/receive 取回的消息走完 store/filters/OTP waterfall，重启后游标
     （max uid）正确推进不重复投递；慢 provider 触发 -32019；插件应答 error 得
     -32020 且 detail 有界。
- 桌面：委托态 profile 的视图反序列化与徽标渲染进既有 reducer/渲染测试。

## 后果

正：愿景 §6/§7 的全部业务缝有了统一调用原语；M4-06/07/08 直接复用注册表与反向
通道；「装上即多一种接入方式」第一次成为可测试的事实。

负：协议从单向变双向，两端解码器都多一条分支（复杂度税永久支付）；插件作者多理解
一套 provider 合同（单调 uid、帧上限、超时语义）；catalog 多一个版本号的双读逻辑；
审计行增加（量可控：仅委托账户的操作）。

## 参考

- `ARCHITECTURE.md` §6/§7：seam 分类法与场景→插件映射（愿景出处）
- ADR-0017：主体与会话绑定（反向调用只面向已认证连接）
- ADR-0018：capability 授权表（`transport_provider` 词与 `providerRegister` 策略行）
- ADR-0015：受管账户目录与 format_version fail-closed 先例（catalog v2 沿用）
- ADR-0016：协议代数与遗留降级判据（本切片不晋升代数）
