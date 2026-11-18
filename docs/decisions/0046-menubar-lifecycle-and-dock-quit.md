# ADR-0046：Dock 右键「退出」/ `⌘Q` 零残留收敛，与菜单栏「退出 Maild」联动关闭前端

- 状态：已接受；已实现并合入（2026-11-18）。前端侧（maild-client / maild-app / maild-ui）由 Q2 `11fe854` 实现；守护进程推送 `agent/stopping` 与菜单栏「退出 Maild」由 Q1 `687bbce` 实现（`crates/maild`、`crates/maild-menu`）；二者在 `cliouo/menubar-dock-quit-integration` 合入并补跨 crate 端到端测试。本 ADR 主要约定前端如何响应
- 日期：2026-11-18
- 关联：[ADR-0044](0044-macos-single-dock-identity.md)、[ADR-0045](0045-tray-only-minimize-and-close.md)

## 背景

ADR-0045 之后红灯 `×` 关闭不留任何 Dock 痕迹，但真机上另外两条退出路径会留下一个写着「Maild 正在后台运行」的幽灵图标：

- Dock 右键 `Maild` →「退出」；
- 系统菜单 `Maild → 退出 Maild`（`⌘Q`）。

### 根因（`lsappinfo` 实测）

1. Dock「退出」发送 AppleEvent `kAEQuitApplication`，AppKit 调 `[NSApp terminate:nil]`；系统菜单 `⌘Q` 也是 `terminate:`。
2. tauri 底层 `tao-0.35.3` 的 app delegate 类 `TaoAppDelegateParent` **没有实现 `applicationShouldTerminate:`**。AppKit 于是按默认 `NSTerminateNow` 直接进 `applicationWillTerminate:` 结束进程——完全绕过 `RunEvent::ExitRequested`、草稿保存（`maild://close-request`）与 ADR-0045 的 `retire_window`（切回 `Accessory`）。
3. 进程死时仍是 `ActivationPolicy::Regular`（`lsappinfo` 视角 `type="Foreground"`），而 `Maild Agent` 的 `parentASN` 指向 `Maild`。`launchservicesd` 把 `Maild` 记为 `type="Foreground" (exited-with-subordinates)`，Dock 因此保留一个「正在后台运行」的僵尸图标。
4. 红灯 `×` 之所以干净：它先 `setActivationPolicy(Accessory)` 把进程降为 `type="UIElement"`，再 `app.exit(0)`——tao 的退出走 `[NSApp stop:nil]`，不经过 `terminate:`。

另一方面，Q1 给菜单栏 `✉` 加了「退出 Maild」：守护进程向已认证前端推送 `{"jsonrpc":"2.0","method":"agent/stopping","params":{}}`，约 350 ms 后停机。窗口打开或原生最小化着的 `maild-ui` 不应在后台消失后成为孤儿，而应随后台一起干净退出。

## 决策

### 1. 唯一退出流水线（`crates/maild-ui/src/lib.rs`）

所有「请退出 UI」入口收敛到两个函数：

- **`request_close_and_exit(app, trigger)`**：若写信页已装关闭守卫（`guard_ready`）且尚未放行（`!close_allowed`），发 `maild://close-request` 让前端刷盘草稿，前端完成后经 `ui_finish_close` 进 `retire_and_exit`；否则立即 `retire_and_exit`。流水线已启动（`exiting`）时什么也不做。`trigger` 决定前端能否否决：
  - `CloseTrigger::User`（红灯 `×`、`⌘Q`、Dock「退出」、`ExitRequested`）：**尊重前端否决**，没有超时兜底。前端 `closeWindow()` 在未保存设置的确认被拒、上传未完成或发送中时保留窗口，等用户处理后再次关闭。
  - `CloseTrigger::AgentStopping`（菜单栏「退出 Maild」）：另起 500 ms 兜底任务——后台已在停机、草稿反正存不进去，前端因原生最小化节流等原因未在宽限期内回调 `ui_finish_close`，就直接 `retire_and_exit`。
- **`retire_and_exit(app)`**（幂等，`AppState::exiting` 防重入）：置 `close_allowed` → 经 `run_on_main_thread` + `oneshot` **同步等主线程**完成 `demote_to_accessory_on_main_thread(主窗口)`（`orderOut:` 同时收走普通窗口与原生最小化缩略图 → `Accessory` → `deactivate`；非 macOS 只隐藏窗口）→ `sleep(50 ms)` 让 CFRunLoop 跑几个 tick，WindowServer 与 `launchservicesd` 提交 `type="UIElement"` 并撤下 Dock 图标 → `app.exit(0)`。

先降级、再等提交、最后退出，这个次序杜绝了 `exited-with-subordinates` 竞态。

入口：

| 入口 | 处理 |
| --- | --- |
| 红灯 `×`（`WindowEvent::CloseRequested`） | `prevent_close` → `request_close_and_exit(User)` |
| `RunEvent::ExitRequested` | 未放行（`!close_allowed`）则 `prevent_exit` → `request_close_and_exit(User)`；流水线自己的 `exit(0)` 到这里已放行 |
| Dock「退出」/ `⌘Q`（`applicationShouldTerminate:`） | `request_close_and_exit(User)`，返回 `NSTerminateCancel`（见决策 2） |
| 前端刷盘完成（`ui_finish_close`） | `retire_and_exit` |
| 菜单栏「退出 Maild」（`ViewState::shutdown_requested` 首次为 `true`） | 状态泵调 `request_close_and_exit(AgentStopping)` |
| `RunEvent::Exit` | 主线程再幂等调一次 `demote_to_accessory_on_main_thread(None)`，最后一道防线 |

ADR-0045 的 `retire_window` / `deactivate_app` 由此删除。流水线启动后 `show_and_activate_main_window` 不再把窗口请回来（避免退出途中被意图唤醒切回 `Regular`）。

### 2. 运行时补上 `applicationShouldTerminate:`（`crates/maild-ui/src/macos_window.rs`）

`install_terminate_handler(app)` 在 `.setup()`（主线程）调用一次：

- `OnceLock<AppHandle>` 保存句柄；
- 取 `NSApp.delegate()` 的运行时类，`objc2::ffi::class_addMethod(cls, sel!(applicationShouldTerminate:), imp, "Q@:@")` 注入 `extern "C-unwind" fn application_should_terminate(&AnyObject, Sel, *mut AnyObject) -> usize`。若类已经实现该方法（未来 tao 升级），`class_addMethod` 返回 NO，保留原实现并打日志；
- **刷新 AppKit 的 delegate 选择子缓存**：`NSApplication` 在 `setDelegate:` 时缓存 delegate 的 `respondsToSelector:` 结果，运行时补上的方法要 `setDelegate(None); setDelegate(Some(&delegate))` 重设一次才会被看见。tao 自己持有 delegate 的强引用，交换期间我们也持有一份。

回调对常规退出调 `request_close_and_exit` 并**返回 `NSTerminateCancel`（0）**：tao 的 `app.exit(0)` 走 `[NSApp stop:]` 而非 `terminate:`，取消这次 terminate 既不会让 Dock 弹「强制退出」，也不妨碍随后的退出；Dock「退出」与 `⌘Q` 由此 100% 进入草稿刷盘 + `Accessory` 降级流水线。

**例外：系统注销 / 重启 / 关机。** 这类退出事件的当前 AppleEvent 带 `keyAEQuitReason`（`'why?'`），此时返回 Cancel 会让系统提示「Maild 中断了注销」并中止注销。回调检测到该属性时同步执行 `demote_to_accessory_on_main_thread(主窗口)`、置 `close_allowed` / `exiting`，返回 `NSTerminateNow`——草稿来不及刷盘（与本 ADR 之前一致），但不会阻断注销，也不留幽灵图标。菜单 `⌘Q` 没有当前 AppleEvent，Dock「退出」的 `kAEQuitApplication` 不带退出原因，两者都走常规分支。

### 3. `agent/stopping` 前端链路（`crates/maild-client`、`crates/maild-app`）

- `maild-client`：`ServerNotification::AgentStopping`；`agent/stopping` 帧解析为它，不校验参数内容（帧只是信号）。
- `maild-app`：`ViewState::shutdown_requested: bool`（默认 `false`，置位后不回落）。连接 actor 与 `frontend/intent` 同样把它当作**非快照状态**：不进快照同步缓冲（快照收尾不会重放），同步期间也立即生效；首次收到时置位、`publish()` 并广播 `AppEvent::Notification(AgentStopping)`，重复帧不再发布。`notification_bytes` / `validate_notification` / `apply_notification` 补齐分支（`apply_notification` 恒为 `false`）。
- `maild-ui` 状态泵：`shutdown_requested` 首次为 `true` 时 `request_close_and_exit(AgentStopping)`——刷盘草稿 → 主线程同步撤窗并切 `Accessory` → 50 ms → `exit(0)`。打开着或原生最小化的窗口都随后台一起消失；守护进程 350 ms 的停机余量覆盖 50 ms 提交窗口，但不保证覆盖 500 ms 的草稿宽限期——后台先停时前端的草稿保存会失败，流水线照常在兜底后退出。

## 后果

- Dock「退出」、`⌘Q`、红灯 `×`、菜单栏「退出 Maild」四条路径行为一致：Dock 零残留，有守卫时先刷盘草稿。
- 用户发起的关闭（`×`、`⌘Q`、Dock「退出」）保留 ADR-0045 的守卫语义：前端可以否决，否决后窗口留下（Dock「退出」时若窗口原生最小化着，它仍留在 Dock，用户还原即可看到原因）。只有后台停机时守卫变为「前端有 500 ms 完成刷盘」，宽限期过后无论否决与否都退出——后台离场后 UI 本就无法工作。
- 依赖 tao 继续不实现 `applicationShouldTerminate:`；若将来实现，`class_addMethod` 失败只会退回旧行为并打日志，不会崩溃。
- 真机验证（`lsappinfo info -only type Maild` 退出前后、Dock 观察、注销不被中断）列入外部门禁。

## 验证

- `maild-client`：`agent_stopping_frame_decodes_to_typed_notification`、`known_notifications_decode_to_typed_events`。
- `maild-app`：`agent_stopping_requests_shutdown_once_without_entering_the_snapshot_buffer`。
- `maild-ui`：`agent_stopping_triggers_the_exit_pipeline_exactly_once`。
- `maild`（Q1）：`api/agent_control_tests.rs` 的 `stop_request_pushes_agent_stopping_only_to_frontend_streams`、`rejected_stop_requests_push_nothing`、`unauthenticated_streams_never_see_agent_stopping`。
- 跨 crate 端到端：`crates/maild-ui/tests/bridge.rs::stop_managed_agent_reaches_the_frontend_before_the_daemon_exits`——真 `maild`（`with_managed_agent_control()`）+ 以 `frontend:com.maild.ui` 认证的 `maild-app` 连接；user 侧 `RpcClient` 调 `stopManagedAgent` 后，`ViewState.shutdown_requested` 在 daemon 仍运行、连接仍在线（排空窗口内）时置位，旁听的 user 长连接收不到 `agent/stopping`，随后 daemon 自行正常退出并删除 socket，断线后 `shutdown_requested` 不回落。
- 门禁：`cargo test -p maild-client -p maild-app -p maild-ui --all-targets --locked`；`cargo clippy -p maild-client -p maild-app -p maild-ui --all-targets --locked -- -D warnings`；`cargo clippy --target x86_64-pc-windows-gnu -p maild-client -p maild-app --all-targets --locked -- -D warnings`。
