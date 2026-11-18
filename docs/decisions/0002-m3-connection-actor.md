# ADR-0002：M3 使用单连接 actor 与可重建视图状态

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：M3 桌面应用层

## 背景

`maild-client` 已提供安全的 endpoint、一次性 HTTP 调用和可拆分的持久 IPC 流，但不负责
应用生命周期。桌面端仍需解决断线、乱序响应、断线期间通知不可重放，以及 UI 线程不能
等待网络 I/O 等问题。这些行为若直接写进 egui 组件，会让视图同时承担连接和领域状态。

## 决策

新增框架无关的 `maild-app` crate。`ConnectionHandle` 是 GUI 唯一需要持有的运行时入口：

- `watch<ViewState>` 发布最新连接与邮箱快照，慢消费者只取最新状态；
- `broadcast<AppEvent>` 发布服务端通知，允许后续 toast 等瞬时消费者独立订阅；
- 有界 command channel 承接 RPC 意图，只有 `Online` 状态接受业务请求，离线时立即失败；
- 请求从入队、写入到响应共用一个总 deadline，pending 同样有明确数量上限；
- `shutdown()` 使用独立取消信号抢占连接、写入和 HTTP 等待，并在 `Stopped` 已发布后才返回。

IPC 使用一个 actor 持有拆分后的 reader/writer：writer 分配 id，pending 表按 id 匹配乱序
响应并施加总 deadline；reader 持续消费通知。连接断开时所有 pending 显式失败，随后按
250ms 起、30s 封顶的指数退避重连。

每次连接成功或手动刷新都连续发出 `status`、`listAccounts`、`listMessageSummaries` 三个快照请求，
再按 JSON-RPC `id` 同时等待响应。
`status` 是连接可用性的必要条件；账户或消息查询失败时保留可用/既有数据并标记
`partial_snapshot`，但可选请求的连接错误仍进入重连。同步期间的通知会在账户已知时立即
应用，同时按 256 条/2 MiB 双重上限缓存并在快照落地后重放去重；页外新消息或缓冲溢出会
立即再做最多一次权威快照，避免从当前分页反推全局总数；连续活跃时以 `partial_snapshot`
结束本轮而不忙循环，手动 `refresh()` 的 waiter 会等到最终结果。通知字段还会按服务端
8 KiB 文本、64 KiB 正文约束复核，未知账户不会扩张视图状态。

HTTP v1 没有推送，因此 actor 以同样的三请求快照启动，并默认每 30 秒轮询（配置下限 1 秒）；连接错误进入
同一退避状态。快照和断线前数据只存在内存，断线时保留供 UI 展示，恢复后以 daemon 快照
覆盖，不增加第二份持久化事实来源。

`ViewState.connection_epoch` 在每个新连接首次产生权威快照时递增，周期轮询和手动刷新不递增。
桌面端据此重建当前账户页并清除连接期错误，不依赖 egui 是否观察到中间的 Connecting 或
RetryWaiting 状态；`watch` 合并快速状态变化也不会漏掉恢复动作。

## 不在本层处理

- endpoint/config 发现、token 和帧校验仍属于 `maild-client`；
- daemon 启动托管、安装和后台登录启动属于 M3.5；
- 选中项、滚动位置、窗口布局和绘制属于 M3-03 之后的 egui 层；
- 搜索、已读、草稿等守护进程尚未提供的能力不由本地状态伪造。

## 后果

- egui 组件只读取 `ViewState` 并发送用户意图，不直接管理 socket 或重连计时器。
- IPC 与 HTTP 的实时性不同，但向 UI 暴露相同的连接阶段与快照结构。
- broadcast 消费者可能因落后收到 lag 错误；最新可恢复状态始终以 watch 快照为准。
- 状态序列可用 hermetic duplex/HTTP 测试，不需要开窗口验证连接语义。

## 验证

- reducer 测试覆盖消息去重、排序、页大小上限和总数更新；
- duplex 测试覆盖乱序快照响应、同步期间通知、刷新完成语义、用户请求和断线 pending 失败；
- HTTP 测试覆盖首次失败、指数退避、恢复上线、连接世代、下一轮轮询、半途断线和可抢占关闭；
- reducer/资源测试覆盖页外通知总数、账户边界、字段与通知缓冲上限；
- 运行时缺失、非法资源参数、离线请求快速失败、事件通道关闭和有序停止均有测试。
