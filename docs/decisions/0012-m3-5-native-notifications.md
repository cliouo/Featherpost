# ADR-0012：macOS 原生通知只由登录 Agent 持有

- 状态：Accepted
- 日期：2026-08-21
- 适用范围：M3.5 macOS 原生通知、授权 UI、JSON-RPC 和通知服务生命周期

## 背景

旧 `macos` provider 为每条消息启动 `osascript`。系统看到的是脚本宿主，不是 Maild 的稳定
bundle identity；daemon 也无法查询授权状态，前端只能从一次投递是否成功猜测权限。无界子线程和
shell 参数还会扩大退出、资源和敏感内容泄漏风险。

ADR-0011 已把后台进程固定为 `Maild.app` 内的 `Maild Agent.app`。系统通知需要继续使用这个
Agent 的 bundle identity，授权查询和权限请求也必须在同一进程完成。

## 决策

### Agent 是唯一的原生通知身份

`provider = "macos"` 使用 `UNUserNotificationCenter`，不再调用 `osascript`。provider 创建时读取
`NSBundle.mainBundle.bundleIdentifier`，并与编译期包含的
`packaging/macos/agent-bundle-id.txt` 严格比较。只有完整 bundle 中的 `maild-agent` 可以创建
通知中心并请求或投递通知。

裸 `maild`、开发目录中的 `maild-agent` 和顶层 `maild-desktop` 都不满足该身份，状态返回
`identity_mismatch`，不会调用通知中心。非 macOS 构建把 `macos` provider 报为 `unsupported`。
两种情况都不会降级到脚本通知或 `dev-log`。

桌面端与 Agent 的每个 Mach-O slice 都包含编译期 Agent ID。bundle 生成器和验证器逐 slice
读取该值，并与 Agent plist、identity manifest 对照；缺失、陈旧或 slice 间不一致都会停止打包。
正式发布仍需沿用 ADR-0011 的签名、Team ID 和真机门禁。

### 查询不会弹窗，授权请求只能由用户发起

Agent 启动、收到邮件、桌面端打开后台面板和普通状态刷新都只能调用
`getNotificationSettings`。这些路径不得调用 `requestAuthorization`。

桌面端在最新状态为 `available + not_determined` 时显示“允许通知”按钮。用户点击后才调用
`requestNotificationAuthorization`。`maildctl request-notification-authorization` 提供同样的
显式命令，适合没有桌面 UI 的安装。

只有 `not_determined` 会向系统发起一次授权请求。`denied`、`authorized`、`provisional`、
`ephemeral` 和 `not_applicable` 都直接返回当前状态，不重复请求。并发显式请求使用 single-flight，
同一时间只存在一个系统请求。

系统权限窗口由人操作，RPC 不等待用户完成。请求发起后可以返回 `checking` 且
`request_in_flight: true`；客户端随后用 `getNotificationStatus` 刷新。用户拒绝后，界面只说明如何
在系统设置中修改，不再次弹窗，也不调用私有设置 URL。

### 通知状态使用独立的粗粒度协议

既有 `status` 响应保持 `{version, accounts, message_count}`，不增加通知字段。新增两个独立方法：

- `getNotificationStatus`：刷新并返回通知授权状态，不触发权限窗口；
- `requestNotificationAuthorization`：显式开始授权请求，并返回当前状态。

两者共用以下结果：

```json
{
  "provider": "macos",
  "availability": "available",
  "authorization": "not_determined",
  "request_in_flight": false,
  "fresh": true
}
```

`availability` 当前为 `available`、`unsupported`、`identity_mismatch` 或 `unknown`。
`authorization` 当前为 `checking`、`not_determined`、`denied`、`authorized`、`provisional`、
`ephemeral`、`not_applicable` 或 `unknown`。客户端把未知字符串映射为 `unknown`，避免新系统状态让
旧客户端解码失败。

`fresh: true` 表示最近一次系统状态查询成功。查询超时或失败时返回最近缓存并设为 `false`，调用方
只能把它显示为过期状态，不能据此投递或请求授权。任意平台错误文本、队列深度和投递计数均留在
daemon 内部，不进入 JSON-RPC。这样不会通过低权限状态查询暴露 OTP 活动、通知流量或
`NSError` 细节。

未装配 notify 服务时，两个方法返回 `-32001`。身份不匹配或平台不支持时，状态查询仍返回上述
粗粒度结果；授权请求返回 `-32001`。旧 daemon 不认识新方法时返回既有 `-32601`，桌面端只在后台
面板中展示该错误，不影响邮箱快照、重连或现有 CLI 命令。

### 未授权通知不会在授权后回放

OTP 插件先写入持久化 `notified` 标记，再执行 rules 路由和通知提交。处于 `checking`、
`not_determined`、`denied`、`unsupported`、`identity_mismatch` 或停止状态时，当前 OTP 直接丢弃，
不会在授权后回放，也不会撤销持久化标记。这样可以避免用户授权后集中出现已经过期的验证码。

容量 32 的队列只接收当前已授权的投递。`authorized`、`provisional`、`ephemeral` 和开发态的
`not_applicable` 可以入队；队列满时立即丢弃，邮件事件循环不等待。worker 在实际投递前再次查询
授权，期间被撤销的通知同样丢弃。该队列是瞬时通知的背压边界，不是可靠消息队列；邮件内容仍由
store 和查询 API 提供。

标题最多 256 字节，正文最多 2 KiB，控制字符会被替换。`dev-log` 只写通知 ID，不写标题、正文或
验证码。测试使用的最近通知记录最多保留 64 条，内部计数和记录都不通过远程协议公开。

rules 仍在入队前处理敏感开关、静默关键词和静默时段。配置了 rules 但服务没有成功挂载时，OTP
通知 fail-closed；无效 rules 或 notify provider 在创建受管目录和其他生产资源前停止装配。

### worker 关闭有明确上限

所有系统查询、授权和投递由一个 worker 串行处理；邮件事件线程只执行有界字段处理和非阻塞
`try_send`。空闲 worker 由命令唤醒，不做持续轮询。

notify 插件通过 cordis async disposer 参与 daemon 的 LIFO teardown。关闭先拒绝新通知并唤醒
worker，排队中的瞬时通知直接丢弃；最多等待 2 秒，系统回调没有结束时分离 worker，让 daemon
继续退出。`Drop` 只设置停止标记和唤醒 worker，不等待线程或系统 I/O。

## 后果

- macOS 通知授权稳定归属于嵌套 Agent，顶层 GUI 和裸二进制不能冒用该身份。
- 用户可以单独查看登录项状态、daemon 连接状态和通知授权状态，三者不能互相推断。
- 首次授权前收到的 OTP 不会补发。需要验证码时应在授权完成后请求新的验证码，历史邮件仍可在
  邮箱中读取。
- 公共协议可以表达是否支持、是否授权和状态是否新鲜，但不能用来推断通知数量或 OTP 活动。
- `dev-log` 保留给开发和 hermetic 测试；它不是 macOS provider 失败时的自动 fallback。
- Windows 原生 toast 与 AUMID 不在本决策范围内，非 macOS 构建只保留可编译的 unsupported 路径。

## 验证

仓库内测试覆盖：

- 查询状态不触发授权，显式请求 single-flight，未决的人机授权不会卡住 RPC deadline；
- denied、checking 和未知状态 fail-closed，只有已授权投递进入容量 32 的队列；
- rules 未挂载、无效 rules、未知 provider 和过长 OTP 不会泄漏敏感通知；
- 字段、测试记录和队列均有上限，日志不包含通知正文；
- async disposer 最多等待 2 秒，`Drop` 不等待系统回调，停止后不会开始新的投递；
- `status` 旧 DTO 保持不变，新 DTO 接受未知平台状态；桌面端只有显式按钮产生授权 job；
- `maildctl` 的两个新子命令、JSON 输出和既有退出码保持一致；
- macOS bundle 检查 desktop 与 Agent 的每个 Mach-O slice，拒绝缺失和混合 identity；
- workspace 原生门禁和 Windows GNU all-targets 门禁保持通过。

正式发布前还需在干净 macOS 13+ 账户上验证：启动 Agent 不弹窗；点击一次“允许通知”后权限窗口
归属于 Maild Agent；允许后能收到新 OTP；拒绝后不再请求且没有脚本 fallback；系统设置中的权限
变化能被状态刷新看到；SIGTERM 在系统权限窗口未关闭时仍能有界退出。M3.5-07 已落地签名、
notarization 和 stapling 的 fail-closed 工具链，正式 identity/凭据与真机正向证据仍由外部门禁证明。

## 参考

- [ADR-0011：M3.5 macOS App bundle 与登录 Agent](0011-m3-5-macos-bundle-and-agent.md)
- [Apple：UNUserNotificationCenter](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter)
- [Apple：Requesting authorization for user notifications](https://developer.apple.com/documentation/usernotifications/asking-permission-to-use-notifications)
- [Apple：UNNotificationSettings](https://developer.apple.com/documentation/usernotifications/unnotificationsettings)
