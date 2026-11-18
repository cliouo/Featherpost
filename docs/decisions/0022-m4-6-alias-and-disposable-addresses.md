# ADR-0022：M4-06 alias 与 disposable 地址（地址层）

- 状态：冻结（2026-08-23）
- 关联：ADR-0018（方法授权表）、ADR-0021（外部 transport provider）；ROADMAP M4-06

## 背景

愿景场景里有两类地址需求：「无限邮箱别名」（一个真实信箱挂多个身份）与「隐私随机邮箱」（为每个服务生成一次性地址，泄漏即弃）。当前内核没有地址层概念——账户即身份（`profile.address`），`From` 固定等于 profile 地址，消息表也不记录收件人。要在邮件领域做 VSCode，别名必须是内核一等公民，而不是每个前端各自拼字符串。

### 愿景对照

- 无限邮箱别名 = 本切片的手动别名 + M4-07 自定义域之后的全域别名；
- 隐私随机邮箱 = 本切片的 disposable（随机生成 + TTL 惰性过期）。

## 目标

1. 受管账户可挂载多个别名/一次性地址；发信可从任一 active 别名发出，凭据与传输完全复用父账户。
2. 收信时捕获收件人（To），落库标记，使「哪个别名泄漏了」可溯源。
3. 协议 additive 扩展 + 桌面最小闭环（管理 + 选择器 + 详情标记）。
4. 一切走既有管线：per-account 规则、OTP、通知、审计、预算一概不被绕过。

## 非目标

- 上游自动 provisioning：catch-all 开启、DNS/MX、注册商配置归 M4-07；本切片不管上游如何让别名可达。
- 别名粒度规则引擎：rules 维持 per-account。
- 回复自动路由/转发链。
- 插件提供的 disposable 服务（provider 插件已可经 M4-05 合同自行实现）。
- plus-addressing（`user+tag@`）解析。
- 定时器式过期扫描（见决策 4 的惰性求值）。

## 决策

### 1. 地址是 store 里的独立实体，不是 catalog 配置

新表 `addresses(id TEXT PK, account TEXT NOT NULL, address TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, label TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NULL)`，外键语义由 manager 层维护（账户删除级联移除其地址）。理由：地址是高频动态数据（数量级对齐 messages，而非 accounts.toml 这种 curated 配置），需要事务与唯一约束；catalog 变更会牵动凭据缝与 schema 版本，代价不成比例。

### 2. store schema v1 → v2

- 新建 `addresses` 表；
- `messages` 增列 `recipient TEXT NOT NULL DEFAULT ''`（`ALTER TABLE ADD COLUMN`，旧行空串）。
- 迁移在既有 migrations 框架内单步完成；恢复路径接受 v1 备份并在恢复后迁移至当前 schema（旧备份永远可进新库）。

### 3. 收件人捕获贯穿事件模型

- `MailMessage` 增 `recipient: String` 字段；IMAP 路径从 To 头首值提取（`parse_message`），M4-05 provider receive 合同增可选 `recipient` 键（serde default 空）。
- 落库写入新列；`MessageSummary` 与消息详情视图暴露 recipient。
- 标记发生在过滤/OTP/通知之前的数据装配处，但不改变 rules 匹配字段（仍 sender/subject）。

### 4. kind 与生命周期；过期是惰性的

- `kind`: `"alias"`（用户显式命名）| `"disposable"`（daemon 随机生成）。
- `state`: `active` / `disabled`（用户停用）/ `expired`（惰性判定：任何读取或使用时 `expires_at <= now` 即按 expired 对待并可回写状态，无后台定时器——headless 资源观）。
- disposable 可带 TTL（`expires_secs`，缺省永久）；TTL 到期后发信拒绝、UI 置灰，历史消息保留。

### 5. 生成语义（disposable）

`createAddress { kind: "disposable", address 缺省 }` 时 daemon 以密码学 RNG 生成 local part：小写 base32、12 字符（约 60 bit 熵），域名取父账户 `profile.address` 的域部分。冲突则重试，连续 5 次失败报错（概率上不可达）。alias 必须显式提供完整地址，语法校验沿用 mailbox grammar（≤254 字节、合法 local/domain、无控制字符）。

### 6. 发信经别名（roadmap 约束的核心）

`sendFromAccount` 增可选参数 `from_address`：

- 校验：地址存在于 `addresses` 且属于目标账户、kind 任意、状态 active（含未过期）；否则 `-32602`/`Invalid` 拒绝。
- 通过后仅替换 From 头/信封为别名地址；凭据加载、连接、per-account 规则语义、预算、审计行格式完全不变——**别名发信不可能绕过任何账户级纪律**。
- M4-05 provider 委托分支同样以别名为 `from` 传入 payload（additive JSON 键）；provider 拒绝则照常映射 `-32020`。

### 7. RPC 面与授权、审计

新增方法（全部 additive，参数与响应均新增）：

| 方法 | 语义 |
|---|---|
| `listAddresses {account?, include_inactive?}` | 列地址视图（含状态/到期时间） |
| `createAddress {account, kind, address?, label?, expires_secs?}` | 创建（disposable 可代生成） |
| `updateAddress {id, state?, label?}` | 启用/停用/改名 |
| `removeAddress {id}` | 删除（消息保留，recipient 成悬垂文本） |

授权：ADR-0018 表外方法对插件主体默认 UserOnly——**策略表零改动即达成**（这四类都是账户管理族操作，不该有插件主体调用面）。（[ADR-0030](0030-frontend-authorization-closure.md) 决策 4/5 起改归 `addresses_manage`：仍不对后台插件开放——该词只有 frontend manifest 能声明——但用户可以把地址管理界面委托给自己装的前端。）审计行只引用地址行 `id`，不写地址字符串本身（别名是隐私元数据，与收件人不入日志同一纪律）。

### 8. 客户端与桌面最小闭环

- maild-client：四个方法的 DTO + `AddressView`（字段全 `#[serde(default)]` 容忍降级）；`SendRequest` 增可选 `from_address`。
- 桌面：账户中心卡内「地址」区（列表 + 状态 chip + 新建[含随机建议] + 停用/删除）；compose 发件账户选择器下挂该账户的 active 地址；详情页显示收件别名 chip（有值时）。
- 旧前端/旧 client 对新字段天然忽略（serde default），协议向后兼容。

### 9. 协议代数

纯 additive：新方法、新可选请求键、新响应字段、store 列扩展。协议版本不提升；错误码不新增（复用 `-32602` 参数错误与 `ManagerError::Invalid` 家族）。

## 备选方案与取舍

| 方案 | 结论 |
|---|---|
| 地址存 accounts.toml / catalog v3 | 拒绝：高频 churn 数据放 curated 文件会造成 TOML 重写风暴；catalog 版本升级牵动凭据缝 |
| 每个别名一个账户 profile | 拒绝：凭据/连接/轮询成倍膨胀，违背最小资源占用路径 |
| 只做发送侧重写、不做 To 捕获 | 拒绝：丢失「泄漏溯源」这一隐私中继核心价值 |
| 过期用后台定时器扫描 | 拒绝：常驻唤醒违背 headless 资源观；惰性判定语义等价 |

## 安全与信任边界（诚实声明）

- IMAP 账户的别名可达性完全取决于上游（catch-all 或服务商别名功能）：**daemon 无法验证上游接受任意 local part**，probe 不覆盖别名；自定域（M4-07）落地前这是硬边界。
- recipient 取自 To 头首值；转发链/邮件列表可能改写 To，标记可能失真——它是展示性元数据，不是投递依据（投递仍以 IMAP 邮箱/provider 合同为准）。
- From 重写不伪造认证：SPF/DKIM 由上游 SMTP 与域策略决定，别名地址若不在父域对齐范围内可能被下游判 spam；文档如实提示，不做隐藏。
- 地址字符串不进审计与日志（同收件人纪律）；随机生成用 OS 密码学 RNG，不引入可预测性。

## 测试策略（hermetic）

- 单测：地址语法与去重、RNG local part 格式与熵长度、惰性过期状态机、`parse_message` To 提取、provider 合同 recipient 解码容忍。
- migrations：v1 库打开自动升 v2 且 messages 行数/内容保持；v1 备份恢复后迁移路径。
- e2e（FakeMail/FakeSMTP）：建别名 → 投递 `To: 别名` → 落库 recipient 列 + OTP/通知链路不受影响；`sendFromAccount from_address=别名` → FakeSMTP 断言信封 From 已改写、凭据仍走父账户；越权（跨账户地址/disabled/expired）拒绝；`listAddresses/createAddress/updateAddress/removeAddress` 全循环 + User 主体外拒绝。
- 桌面：ui_debug 第三态 fixture（别名区 + 选择器）离屏截图；reducer 单测钉住状态机。

## 后果

- 内核获得真正的地址层：M4-07 自定域直接在其上叠加「域所有权 → 可挂别名域」；transfer（M4-08）迁移时 addresses 随账户走。
- messages schema 进入 v2：restore/migration 矩阵各加一条路径，测试成本可控（框架已就绪）。
- 「无限别名」在上游 catch-all 就绪后即刻成立，不需要再次改内核。

## 参考

- ROADMAP M4-06 行（additive 约束、sendFromAccount 语义不得绕过）
- ADR-0021 §4（provider receive 合同——本切片增补可选 recipient 键）
