# ADR-0030：M4.5-G01 前端授权闭环与受托管理

- 状态：已接受，M4.5-G01 已实现（hermetic 单测 + e2e 钉死）
- 日期：2026-09-11
- 适用范围：frontend manifest（新增可选 `permissions`，schema_version 2）、`method_policy` 表、
  `listFrontends` 观测字段、external plugin capability 词表（新增一项）；User 主体行为零变化
- 关联：ADR-0014（前端激活与包生命周期）、ADR-0017（主体与认证缝）、ADR-0018（方法级 capability
  enforcement）、ADR-0024（默认前端迁移 Tauri）、ADR-0027（信箱写路径）、ADR-0028（发信与回复）

## 背景

[前端设计评审](../design/tauri/IMPLEMENTATION-PLAN.md) 在实现前核验代码，发现两个互相咬合的缺口。

**其一，ADR-0024 决策 8 那句「本切片不新增、不修改任何 JSON-RPC 方法……capability → 授权翻译表
原样使用」是错的。** 按 ADR-0018 的翻译表，一个前端主体最多拿到 `messages_read`、
`messages_send`、`settings_read`、`settings_write` 四个词。而日用邮箱的主路径——
`markMessages` / `moveMessages` / `deleteMessages` / `listPendingOps` / `resolvePendingOp`
（ADR-0027）、`prepareReply` / `sendFromAccount`（ADR-0028）、全部账户与地址方法、
`status`——**全是 `UserOnly`**。也就是说：一个诚实声明身份的第三方前端，连「把邮件标成已读」
都做不到。可替换前端在纸面上成立、在代码里不成立。

**其二，ADR-0024 决策 2 那句「内置前端不享有特权路径」今天是假的。** `maild-desktop` 与
`maild-app` 全仓没有一处调用 `authenticate`（本次核验 grep 确认），于是内置 egui 壳连上
socket 就是 `Principal::User`，全量授权。它不是「走同一条链」，它是根本没走那条链。

这两条合起来是同一个后果：**frontend 主体这套机制对任何真实前端都不可用，于是每个前端都
只能当 `user`**。一个从来没有主体使用的授权面，不是保守，是死代码。

M4.5-03 的 Tauri 壳必须在这件事定下来之后再写：它是第一个要装载第三方 UI 扩展的前端
（[界面扩展设计](../design/tauri/UI-EXTENSIONS.md)），宿主用什么身份连 daemon，决定了扩展
代理层出 bug 时的爆炸半径。

## 目标

1. 让一个**诚实声明身份的前端**能完整承担日用邮箱主路径，第三方与内置走同一张表。
2. 让「前端能做什么」成为**用户可读、可审计、可撤销的声明**，而不是「反正都是 user」。
3. 把新增授权面限制在显式声明上：既有已安装包的有效权限**逐字节不变**。
4. 如实写清这条边界能挡什么、不能挡什么，不把声明包装成沙箱。

## 非目标

- 不收紧 `Principal::User`。能打开 socket 的就是机主，机主对自己的邮件有全部权限——这是设计，
  不是洞。`maildctl`、Menu、既有桌面壳行为不变。
- 不实现前端内 host ↔ extension 的隔离与代理（那是 E02，且那才是可强制的边界）。
- 不改 origin 门禁、不改错误码、不改协议代数、不动 store schema。
- 不给 external plugin 开放账户/地址/包管理面（见决策 5）。

## 决策

### 1. 两条边界分开命名，各自如实

| | 边界 | 可强制吗 | 挡什么 |
|---|---|---|---|
| A | daemon ↔ frontend 主体 | **只在前端自愿认证后成立** | 宿主代理层的 bug、越权的误用、看不见的权限蔓延 |
| B | 前端宿主 ↔ UI 扩展 | 可强制（宿主控制命令面，扩展无 socket） | 不可信扩展的越权（E02 实装） |

A 不是对抗恶意前端包的沙箱：前端进程与用户同权，它随时可以开一条不认证的连接变回 `user`
（它甚至必须先这么做一次才能取到自己的 token，见决策 6）。ADR-0014 结尾早已写明这件事，
本 ADR 不推翻它。

**那 A 还值什么？** 值三样，都真实：

- **对 bug 的纵深防御。** 宿主一旦丢掉 user 连接、只留认证连接，代理层写错也**不可能**超出
  声明上限——越权需要刻意重连，不是一个 bug 能做到的。这正是 Tauri 壳要代理第三方扩展请求
  的场景。
- **可审计的声明。** `listFrontends` 把每个前端声明的 permissions 与 `denied_calls` 一起交给
  用户；装一个前端之前能看见它要什么。
- **逼出完整词表。** 只要默认前端也必须靠这张表活着，第三方前端就不可能做不到内置能做的事。
  这是 ADR-0024「可替换前端」唯一的落地形式。

### 2. manifest 把「路由」与「授权」拆成两个字段

frontend manifest 的 `capabilities` 自 M3.6 起同时承担两个语义：intent 路由声明与 API 授权
来源（`docs/plugins/README.md` 原文）。这是欠账——**路由和授权本来就不是同一件事**。

往 `message` 这类路由词上挂写权限，会让每一个已安装的只读前端在升级后静默获得删邮件的能力。
所以新增一个独立字段：

```toml
schema_version = 2
capabilities = ["inbox", "message", "compose", "settings"]  # 路由，语义不变
permissions  = ["mailbox_write", "accounts_manage"]         # 纯授权，缺省空表
```

- `capabilities` 的语义、翻译表、回落规则**一个字不改**：`inbox|message → messages_read`、
  `compose → messages_send`、`settings → settings_read + settings_write`、`background → 无`。
- `permissions` 可选、缺省空表、fail-closed（未知词 → 整包拒绝，不是忽略）。
- 有效授权 = 路由翻译所得 ∪ `permissions` 声明所得。

### 3. schema_version 接受 1 与 2

`permissions` 只允许出现在 `schema_version = 2`；v1 manifest 写了 `permissions` 直接拒包。

这样「旧包」与「新包装在旧 daemon 上」两个方向都有清楚的结果：旧 v1 包在新 daemon 上照旧
工作、零权限变化；新 v2 包在旧 daemon 上被 `deny_unknown_fields` 拒绝而不是降权装上——
**一个需要写权限才能工作的前端，不该以静默零权限的形态被装上去**。这是 fail-closed 的正确
方向，不是遗憾。

### 4. 新增四个授权词

| 词 | 覆盖方法 | 为什么是独立的一个词 |
|---|---|---|
| `mailbox_write` | `markMessages`、`moveMessages`、`deleteMessages`、`listPendingOps`、`resolvePendingOp` | ADR-0027 决策 9 拒绝把它并进 `messages_read`（可读即可删）或 `messages_send`（投递不是信箱状态）。那条判断仍然成立，缺的只是它自己的词 |
| `accounts_manage` | `listAccountProfiles`、`probeAccount`、`createAccount`、`updateAccount`、`setAccountEnabled`、`removeAccount` | 账户配置面。与秘密无关：秘密另有 origin 门禁，见决策 7 |
| `addresses_manage` | `listAddresses`、`createAddress`、`updateAddress`、`removeAddress` | ADR-0022 地址层与账户同属管理面，但换一个前端可能只要其一 |
| `plugins_manage` | `listExternalPlugins`、`openExternalPluginUi`、`commitPluginInstall`、`removePlugin`、`rollbackPlugin`、`setPluginEnabled`、`refreshPlugins` | 受管包生命周期。设置页的「扩展」要它，否则装插件永远得回 CLI |

`plugins_manage` 是这四个里最重的：**声明它等于声明「我可以代表用户安装任意受管包」。**
安装界面因此必须显示包来源与它将获得的 permissions，不能做成一个没有上下文的确认按钮。
这条写进 08-C 的验收，不是文档里的建议。

### 5. 三个管理词是 frontend-only，`mailbox_write` 两类主体都可声明

- `mailbox_write` 同时进入 external plugin 的 capability 词表。一个自动归档、自动打标的规则
  插件是这个仓库的题中之义，而它现在无路可走。作为**独立声明**开放它，正好满足 ADR-0027
  当初拒绝的那件事：不要静默，要显式。
- `accounts_manage` / `addresses_manage` / `plugins_manage` **不进** external 词表，manifest
  校验直接拒。理由是攻击面形状：后台插件没有理由改账户配置，更没有理由装下一个插件——
  **禁止自我繁殖**是这里唯一重要的性质。管理面是用户委托给「自己的管理界面」的，那是前端。

### 6. 永久 UserOnly 的边界

2026-09-23 补遗：[ADR-0039](0039-native-recovery-and-tauri-default.md) 新增 `repairBuiltinFrontend` 原生维护例外：仅同用户 IPC 的 User 主体可调用，固定应用内副本且无路径参数。前端与后台插件均无此权限。下面保留最初的凭证边界。

`revealPluginToken` 对任何插件/前端主体永久拒绝。ADR-0018 的提权防线原样保留并且现在
**更重要**：持有一份凭证的主体不得铸造凭证，否则声明上限可以自己给自己改。

由此引出 bootstrap 的真实形状，写清楚免得后来者以为有更干净的路：前端启动时先开一条**不认证**
的连接（= `user`）调 `revealPluginToken { kind: "frontend", id }` 取回并轮换自己的 token，
关闭它，再开一条连接 `authenticate`，此后只用这条。第一条连接是短暂的、必要的、无法消除的
——这恰恰是决策 1 说「A 不可强制」的原因。M4.5-03 的验收要求宿主在此之后**不再持有任何
user 连接**；上限只有在这个前提下才成立。

（LaunchServices 不传环境变量，所以 external plugin 那条「spawn 时注入 token」的路对
`macos_application` 前端不存在。这不是疏漏，是激活方式决定的。）

### 7. 授权与 origin 是两条正交防线，都不放宽

`probeAccount` / `createAccount` / `updateAccount` 另有 `ensure_secret_origin`：携带秘密的
账户方法只接受同用户 Unix socket。**持有 `accounts_manage` 的前端走 loopback HTTP 依然提交
不了密码。** 本 ADR 不碰这条，也不碰 `authenticate` 的 origin 门禁。

同理，`listAccountProfiles` 返回的凭据状态是 `saved` / `missing` 字符串，从不是秘密本身；
`accounts_manage` 因此不等于「能读出密码」。

### 8. 三处归类修正

- `status` 从 `UserOnly` 改为 `messages_read`。它返回版本 + 账户状态 + 落库计数，而
  `getAgentInfo`（Open）已给版本、`listAccounts`（messages_read）已给账户状态——**它不比
  已授权的方法多暴露任何东西**，把它留在 UserOnly 只是让前端少一个诊断入口。
- `sendFromAccount` 从 `UserOnly` 改为 `messages_send`。ADR-0018 当时的理由是
  「credential-backed 发信不因 `messages_send` 放开」，但 M3.7 之后**受管账户就是发信的
  唯一正路**，`send` 走全局 transport 反而是遗留形态。能发信却只能用遗留路径发，是把前端
  往错误的 API 上推。选哪个账户/别名不是比「能发信」更大的权限——都是用户自己的地址。
- `prepareReply` 从 `UserOnly` 改为 `messages_read`。它是纯读：查原信、按 ADR-0028 拼
  `In-Reply-To` / `References` / 收件人并返回草稿，不落库、不发信。**能读原文才谈得上引用
  原文**，而投递仍然要 `messages_send`。
- `requestNotificationAuthorization` 从 `UserOnly` 改为 `settings_write`。它只能触发系统
  提示，不能授予自己权限；与 `setSettings` 同属应用级开关域。

### 9. 内置前端不做例外，且这次是真的

`com.maild.ui` 在自己的 manifest 里声明它需要的 permissions，和第三方走同一张表、同一个
`principal_has_capability`。代码里没有一处 `if id == default_frontend`。

`builtin-egui` 的内置记录 permissions 为**空表**。它的路由 capability 仍是全量（intent 回落
要靠它），但按 ADR-0024 决策 3 它收缩为恢复界面，读账户摘要、读纯文本、改默认前端项——
这些落在路由翻译已有的词里。

本片**不给 `maild-desktop` 接 authenticate**。它即将在 M4.5-09 收缩甚至移除，给一个要拆的
壳加一条 bootstrap 只是白改。如实记录现状：**在 09 之前，内置 egui 壳仍以 `user` 主体运行。**
决策 1 的上限从 Tauri 壳（M4.5-03）开始生效，不是从今天。

### 10. 扩展不继承宿主的 permissions

宿主处理扩展请求时用的是「插件身份 ∩ 视图会话 ∩ 邮件作用域 ∩ 操作白名单」的交集，
**不是把扩展的消息转发成宿主自己的请求**。用户授予默认前端的账户管理能力，不自动流向它
承载的任何 UI 插件。本 ADR 只固化这条契约；实现与负向测试归 E02。

### 11. 补遗（2026-09-11，M4.5-03 实施期）

写 Tauri 壳时撞出两处本 ADR 漏掉的地方，同日补上（理由与测试见
[ADR-0031](0031-tauri-frontend-shell-and-contribution-registry.md) 决策 8）：

- **前端主体隐含 `frontend_open`，且 `claimNextOpenIntent` 只能 claim 自己的 id。** 按决策 2 的
  翻译表，一个认证了的前端拿不到 `frontend_open`，也就 claim 不到排给自己的 intent——而那是
  ADR-0014 里前端的定义性动作。所以这个词不是可选声明：`api_capabilities_of` 对每个前端记录追加
  它；`claimNextOpenIntent` 对 `Principal::Frontend(id)` 校验 `frontend == id`，否则 `-32602`。
  `listFrontends` / `openFrontend` 随之可达。
- **`notifications_read` 进 `permissions` 词表。** 「设置 → 通知」要读系统通知授权状态；不塞进
  `settings` 路由翻译（那会静默改变已装 v1 包的有效权限），走 schema 2 的显式声明。

决策 2「已安装包的有效权限逐字节不变」在第一条上有一个如实的例外：**所有前端记录**（含 v1 包与
builtin）从此多出 `frontend_open`。它只覆盖 `listFrontends`（读清单）、`claimNextOpenIntent`
（限自己）与 `openFrontend`（激活默认前端），不触及邮件、账户、包管理，属于把前端「本来就该能做
的事」补齐，而不是提权。

## 兼容性

- **已安装包的有效权限逐字节不变**（决策 11 的 `frontend_open` 例外除外）。 新词只能从新字段来，新字段只能从 schema_version 2 来。
- User 主体零变化：`maildctl`、Menu、现有桌面壳、一切未认证连接照旧。
- 协议代数不晋升。方法名、参数、响应字段、错误码全不变；`listFrontends` 新增
  `permissions` 数组为 additive（`#[serde(default)]`，旧 client 读到空表）。
- 旧 daemon + 新前端包：manifest 被拒 → 该包不出现在 catalog → intent 回落 `builtin-egui`。
  可恢复，且失败方向正确。
- ADR-0018 决策 1 的策略表与决策 2 的「frontend manifest v1 无法表达其余词表」被本 ADR
  取代；其余部分（推送过滤、错误码、拒绝计数、fail-closed 解析）原样继承。

## 测试策略（hermetic）

- 策略表穷尽性单测继续成立：dispatch 每个方法臂都必须在表里有非默认归类。
- manifest：v2 解析 permissions；v1 带 permissions 拒包；未知词拒包；external manifest 声明
  三个管理词拒包、声明 `mailbox_write` 通过。
- 授权矩阵 e2e（`tests/frontend_permissions.rs`）：
  - 只声明路由的前端 → `markMessages` / `moveMessages` / `deleteMessages` /
    `listPendingOps` / `resolvePendingOp` / 账户 / 地址 / 包管理全部 `-32016`，`denied_calls`
    递增；
  - 声明 `mailbox_write` → 可标记，但账户与包管理仍拒；
  - 声明 `accounts_manage` → 可列账户档案，但 `revealPluginToken` 仍拒（提权防线）；
  - 声明全部四词 → `revealPluginToken` **依然**拒；
  - `prepareReply` 在 `messages_read` 下通过、在只有 `compose` 路由时拒；
  - `sendFromAccount` 在 `compose` 路由下通过；
  - User 主体全部照旧。
- `listFrontends` 的 `permissions` 字段与 `denied_calls` 一同断言。

## 后果

正：可替换前端第一次真的可替换；默认 Tauri 壳有一个不靠自觉的权限天花板，扩展代理层的
bug 撞到 daemon 就停；用户能看见每个前端要什么；`mailbox_write` 顺带给规则/自动化插件开了
一条一直缺的路。

负：frontend manifest 多一个字段和一个 schema 版本，包作者多一张表要读；四个新词是永久的
词表债，删不掉只能弃用；`plugins_manage` 把「安装任意包」变成一个可声明的能力，安装 UI 的
呈现质量从此是安全面的一部分，不只是体验问题。

如实说在前面：**这四个词挡不住一个恶意前端包**——它不认证就是 `user`。它们挡的是 bug、
是权限蔓延、是「装了才发现它能干什么」。M5 的进程沙箱之前，这个仓库没有更强的东西可给，
把它说成沙箱只会让人依赖一个不存在的保证。

## 参考

- ADR-0018：方法级 capability enforcement（本 ADR 扩充其策略表与词表）
- ADR-0014 §10 / 结尾：capability 的原始语义与同用户信任域声明
- ADR-0027 决策 9：信箱写入为何不并进 `messages_read` / `messages_send`
- ADR-0028：`prepareReply` 的纯读形态与 `sendFromAccount` 的账户绑定
- [界面扩展设计](../design/tauri/UI-EXTENSIONS.md) §5：宿主代理不得把扩展请求转成宿主请求
- [交付计划](../design/tauri/IMPLEMENTATION-PLAN.md) G01：本片的输入与验收
