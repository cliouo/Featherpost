# ADR-0018：M4-02 方法级 capability enforcement

- 状态：已接受，M4-02 已实现（hermetic e2e 钉死）；**决策 1 的策略表与决策 2 的 frontend
  词表限制已被 [ADR-0030](0030-frontend-authorization-closure.md) 取代**——本文的判定机制、
  推送过滤、错误码、拒绝计数与 fail-closed 解析原样继续有效，变的是哪些方法落在哪个词上，
  以及前端 manifest 现在可以用 `permissions` 表达路由翻译之外的授权
- 日期：2026-08-22
- 适用范围：maild RPC dispatch、external/frontend 插件清单语义、`listExternalPlugins`/`listFrontends` 观测；User 主体行为零变化

## 背景

ADR-0017 交付了会话主体：合作插件可以把连接绑定为 `plugin:<id>` / `frontend:<id>`。
但 dispatch 尚不消费主体——manifest `capabilities` 仍是 M3.6 以来的兼容性元数据
（`docs/plugins/README.md` 明言「不是安全权限」）。M4 的授权模型要求：声明了
`messages_send` 的插件才可发信、无凭证的插件读不到邮件、插件永远碰不到账户管理与
凭证发放。本 ADR 把 capability 升级为真实授权表。

## 目标

1. 方法→capability 映射单源化；插件主体按声明授权，**默认拒绝**。
2. User 主体（桌面壳、Menu、maildctl、一切未认证连接）行为与今天逐字节一致。
3. 服务端推送与请求同等受控：无 `messages_read` 的插件不收邮件/账户事件。
4. 越权可观测：明确错误码 + 按主体的脱敏拒绝计数。

## 非目标

- 不做逐方法粒度的自定义授权、同意 UI、配额（后续切片按需）。
- 不改变 capability 的声明语法；不新增 capability 词汇（沿用 M3.6 七项）。
- 不做 User 主体的任何收紧——同用户信任域边界原样保留（M5 沙箱收口）。

## 决策

### 1. 方法策略表（单源，穷尽）

dispatch 入口处对每个方法查一次策略；新增方法时编译期穷尽匹配强制补行：

| 策略 | 方法 |
|---|---|
| `Open`（任何主体） | `authenticate`、`whoami`、`getAgentInfo` |
| `Capability("messages_read")` | `listAccounts`、`listMessageSummaries`、`listMessages`、`getMessage` |
| `Capability("messages_send")` | `send` |
| `Capability("settings_read")` / `("settings_write")` | `getSettings` / `setSettings` |
| `Capability("notifications_read")` | `getNotificationStatus` |
| `Capability("frontend_open")` | `listFrontends`、`claimNextOpenIntent`、`openFrontend` |
| `UserOnly`（插件主体永远拒绝） | `status`、`listExternalPlugins`、`revealPluginToken`、全部账户方法（`listAccountProfiles`/`probeAccount`/`createAccount`/`updateAccount`/`setAccountEnabled`/`removeAccount`/`sendFromAccount`） |

要点：

- `revealPluginToken` 对插件主体 UserOnly 是**提权防线**：持有一个插件凭证的主体
  不得铸造其他主体（含自己）的新凭证。
- 账户方法整体 UserOnly：M3.7 受管账户与凭据是用户域，插件 v1 无任何入口；
  `sendFromAccount` 亦然（credential-backed 发信不因 `messages_send` 放开）。
- `requestNotificationAuthorization` UserOnly：OS 授权是用户同意域，不属于任何
  capability。
- `status` UserOnly：聚合诊断（版本/账户数/落库量）不向零授权主体暴露。

### 2. 主体 → capability 解析

- `User`：全量（现状不变）。
- `Plugin(id)`：`ExternalPluginManager` 中该 id 的 manifest `capabilities` 原词表；
  **id 不在册（包被移除/未装配）→ 空表**（fail-closed，连接保持但一事无成）。
- `Frontend(id)`：frontend manifest 的 route 声明经**固定翻译表**映射为 API
  capability：`inbox|message → messages_read`；`compose → messages_send`；
  `settings → settings_read + settings_write`；`background → 无 API 授权`
  （路由语义，不授权 API）。frontend manifest v1 无法表达
  `notifications_read`/`events_stream`/`frontend_open` 之外的其余词表——即前端主体
  拿不到它们（fail-closed，未来以 additive 清单版本扩展）。
- 解析在每次 dispatch 现查（锁内 BTreeMap 查找，邮件事件频率下无性能顾虑）。

### 3. 服务端推送过滤

`MailFiltered` 通知要求 `messages_read`；`AccountState` 通知要求 `messages_read`
（与 `listAccounts` 同域）。会话主体存于共享句柄（`SharedPrincipal`），推送监听器
持同一句柄在投递前检查；不满足则静默丢弃该条（与既有慢消费者丢弃语义一致）。

### 4. 错误码与观测

- 新错误码 `METHOD_NOT_PERMITTED = -32016`：message 含方法名与缺失的 capability
  名（均为非秘密）。连接不断开（与 -32014 的断连语义区分）。
- 拒绝计数：`IdentitySeam` 维护按主体字符串的内存计数（重启清零）；
  `listExternalPlugins` / `listFrontends` 各增 `denied_calls` 观测字段
  （serde default，旧 daemon 为 0）。stderr 审计行同款脱敏格式。

### 5. 协议代数

不晋升。enforcement 只作用于 M4-01 引入的插件主体；User 主体行为不变，无 schema
变更，且整个 M4 Wave 1 处于同一预发布代内——插件生态尚不存在，不存在迁移对象。

## 兼容性

- 未认证消费者零变化；M4-01 的 e2e（`tests/plugin_identity.rs`）继续全绿——
  它们只断言主体绑定与认证面，不触碰受 capability 管控的领域方法。
- 旧 daemon 对插件主体不设限：新插件连旧内核会「多拿到」能力而非报错——升级方向
  安全（fail-open 只存在于旧内核，与 ADR-0016 的遗留降级同性质，由版本握手引导
  升级解决）。
- 已发布的 fixture/文档中声明 `messages_read` 的示例插件不受影响；声明与实际
  使用不符的插件从「静默成功」变为「明确 -32016」——这正是本切片的目的。

## 测试策略（hermetic）

- 单测：策略表穷尽性（对 dispatch 全部方法臂逐一断言策略非默认）、route 翻译表、
  未知 id 空表、拒绝计数增减。
- e2e（`tests/plugin_capabilities.rs`）：`messages_read` 插件可 `getMessage` 但
  `send` → `-32016` 且 `denied_calls` 递增；`revealPluginToken`/账户方法对插件主体
  拒绝；frontend route 翻译（compose 前端可 `send`、不可 `getMessage`）；User 主体
  全部照旧；未知插件 id 全拒绝。
- 推送过滤：单测覆盖判定函数；端到端事件流过滤随 M4 后续事件缝切片一并验证。

## 后果

正：manifest 声明第一次有了真实含义；M4-05 transport provider 等业务插件可以在
明确的授权面上构建；越权从静默变为可观测信号。

负：插件作者多一张必须理解的表；声明遗漏从「碰巧能用」变为运行时错误（生态契约
成本，SDK 文档承担）；dispatch 每请求多一次 capability 解析（可忽略）。

## 参考

- ADR-0017：主体与认证缝（本表在其会话模型上叠加判定）
- ADR-0014 §10：capability 作为兼容性元数据的原始语义（本 ADR 终止该状态）
- `docs/plugins/README.md`：manifest 词表与七项 capability 定义
