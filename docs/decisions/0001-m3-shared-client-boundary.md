# ADR-0001：M3 共用 JSON-RPC client，前端保持纯消费者

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：M3 桌面前端及现有 `maildctl`

## 背景

M2.5 已封板 JSON-RPC v1、本地 HTTP、Unix socket、Windows named pipe 与 `maildctl`。
M3 需要连接守护进程、接收推送并展示邮箱状态。如果桌面端复制 CLI 的 endpoint 发现、
token 权限检查、超时、帧上限和响应校验，两个消费者会很快产生不一致。

同时，前端不应持有 receive、store、rules 或 SMTP 领域逻辑。守护进程是唯一事实来源，
桌面端只维护可丢弃、可重建的视图状态。

## 决策

新增独立 workspace crate `maild-client`，并让 `maildctl` 与 M3 桌面端共同依赖它。

`maild-client` 负责：

- 根据显式参数、环境变量和 `[plugins.api]` 配置发现 endpoint；
- 对 HTTP 执行回环地址、bearer token 与 Unix 配置文件权限检查；
- 实现 HTTP 单次请求以及 Unix socket / Windows named pipe 流式连接；
- 固化 1 MiB 帧上限、连接/请求总时限、JSON-RPC id 和响应结构校验；
- 提供 v1 方法 DTO、类型化便捷调用和已知通知解码；
- 暴露持久 IPC 连接的 `send_request` / `next_incoming` 原语及可独立持有的读写半部，让上层
  可以持续读取推送并匹配并发响应；读半部在 future 被取消时保留已消费的半帧。

`maild-client` 不负责：

- 启动或托管 maild；M3 不以 stdio 子进程作为默认连接方式；
- 自动重连、退避、刷新快照或 UI 状态归并；这些属于 M3-02 应用连接 actor；
- 缓存邮件、修改规则语义或直接访问 SQLite/IMAP/SMTP；
- GUI 线程调度、窗口生命周期、通知展示或凭据存储。

HTTP v1 仍是请求/响应传输，不提供服务端推送。需要实时通知的桌面端应优先使用本地
IPC；连接恢复后必须重新拉取 `status`、账户与消息快照，不能假设断线期间的通知可重放。

## 后果

- CLI 与桌面端共用同一套安全和协议行为；CLI 只保留参数解析、输入读取和输出格式化。
- GUI 可以在不依赖 `maild` 守护进程 crate 的情况下做 hermetic 测试。
- 持久连接允许后续 actor 用专用读循环处理响应和通知，同时从写半部发送请求；但 pending request 表、重连和
  快照 reconcile 仍需由 M3-02 明确实现。
- v1 DTO 的兼容性成为共享库公共边界；新增字段应保持可向后兼容，破坏性变化必须升级协议。

## 验证

- `maild-client` 覆盖 HTTP、Unix socket、Windows named pipe（Windows 条件测试）、通知夹在
  响应之前、乱序响应、id 校验、超时、超限帧和类型化响应。
- maild integration test 使用共享 client 直接连接真实 Unix socket / Windows named pipe
  服务端，避免两侧只对各自 fake 通过。
- `maildctl` 迁移后保留既有命令、JSON 输出、错误类别和退出码测试。
