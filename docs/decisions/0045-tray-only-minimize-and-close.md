# ADR-0045：关闭彻底退回菜单栏（零 Dock 痕迹）、最小化保持系统原生，与 `frontend/intent` 实时唤醒

- 状态：已接受；运行时（maild / maild-client / maild-app / maild-ui / maild-launcher）已实现（2026-11-17），打包侧 `Maild.Info.plist.in` 的 `LSUIElement = true`（M2）已合入（2026-11-18）。2026-11-18 修订：撤销黄灯 / `⌘M` 劫持，最小化回到 macOS 原生行为
- 日期：2026-11-17（2026-11-18 修订）
- 关联：[ADR-0014](0014-m3-6-frontend-activation-and-plugin-lifecycle.md)、[ADR-0018](0018-m4-2-method-capability-enforcement.md)、[ADR-0030](0030-frontend-authorization-closure.md)、[ADR-0031](0031-tauri-frontend-shell-and-contribution-registry.md)、[ADR-0044](0044-macos-single-dock-identity.md)

## 背景

ADR-0044 之后 Dock 上只剩一个 `Maild` 图标，但用户真机反馈两处残留，最初要求：「点击叉号或者最小化之后，dock 栏不出现图标，只有顶部 menu 出现就好，完全的最小化」。

1. **红灯 `×`**：`maild-ui` 退出了，但 `Maild.app` 是以 `Regular` 退出、包也没有 `LSUIElement`，Dock 的「最近使用 / 活动应用」区仍挂着一个 `Maild`。
2. **黄灯 `–`**：`NSWindow` 默认 `performMiniaturize:`，Dock 应用区留着 `Maild`，右侧下载与废纸篓之间还多一个窗口缩略图。

首版实现把两者都收进菜单栏（黄灯 target/action 改投自定义 `minimizeToMenuBar:`、Window → Minimize 换成自定义菜单项、webview `⌘M` 命令 `ui_minimize_to_menu`、原生最小化兜底收回）。真机对照后确认采用职责清晰的菜单栏常驻语义：

- **红灯 `×` / `⌘Q`**＝彻底退回顶部菜单栏：Dock 零活动图标、零最近使用图标，WebView 内存全部释放，只留菜单栏 `✉`；
- **黄灯 `–` / `⌘M` / 双击标题栏**＝macOS 原生最小化：窗口暂存在 Dock（保留应用图标与废纸篓旁的窗口缩略图）——这是用户可预期、可从 Dock 一键还原的系统语义，不再劫持。

而一旦窗口隐藏、进程切到 `Accessory`，从菜单栏 `✉`、系统通知或再次打开 `Maild.app` 经 `NSWorkspace.openApplicationAtURL` 唤醒时，部分 macOS 版本**不保证**给 `Accessory` 进程派发 `RunEvent::Reopen`——单靠 ADR-0044 的 Reopen 领取意图不够。

## 决策

### 1. 可见即 `Regular`，关闭即 `Accessory`（`crates/maild-ui`）

M2 让包带 `LSUIElement = true`，进程以 `Accessory` 起步，退出后也不进 Dock 的最近应用槽位。运行时：

- **窗口存活期间是 `Regular`**：系统菜单栏（`⌘C/⌘V`、`⌘W`、`⌘M`、`⌘Q`）与 `⌘Tab` 照常。`setup` 在建主窗口前切 `Regular`，建完后 `activateIgnoringOtherApps(true)`。原生最小化期间进程仍是 `Regular`，所以 Dock 保留图标与缩略图。
- **`show_and_activate_main_window`**（任意线程可调，动作排到主线程）：`Regular` → `unminimize` → `show` → `set_focus` → 激活应用。`RunEvent::Reopen` 与 IPC 唤醒都走它；窗口处于原生最小化态时由 `unminimize` 从 Dock 还原。
- **`retire_window`**：隐藏主窗口 → `Accessory` → 若窗口仍处于隐藏态则 `NSApp.deactivate()` 把前台还给上一个应用（期间已被唤醒就不动，避免把刚弹出的窗口推到后面）。只用于关闭 / 退出路径。

### 2. 最小化保持 macOS 原生

`crates/maild-ui/src/macos_window.rs` 只保留两个安全的 `NSApplication` 调用 `activate_app()` / `deactivate_app()`（`MainThreadMarker` 守卫，非主线程什么也不做）；不定义任何 Objective-C 类（无 `define_class!`），不改标题栏按钮的 target/action，不替换 tauri 默认应用菜单。黄灯 `–`、Window → Minimize（`⌘M`）、双击标题栏全部走系统 `performMiniaturize:`；没有 `ui_minimize_to_menu` 命令、webview `⌘M` 拦截或 `is_minimized` 自动收回兜底。

### 3. 关闭与退出先撤 Dock 图标再 `exit(0)`

- `ui_finish_close`（守卫确认后的红灯 `×` / `⌘Q`）：`close_allowed = true` → `retire_window` → `app.exit(0)`。WindowServer 在进程退出前就收走 Dock 图标，`exit(0)` 释放全部 WebView 进程内存；后台只剩 `Maild Agent` 与菜单栏 `✉`。
- 守卫未就绪（或已放行）时的 `CloseRequested`：不再让「最后一个窗口关闭」隐式退出，而是 `prevent_close` → `close_allowed = true` → `retire_window` → `app.exit(0)`，走同一条路。
- 放行的 `ExitRequested`（如守卫未就绪时的 `⌘Q`）同样先 `retire_window`。守卫就绪时仍 `prevent_exit` 并发 `maild://close-request`，草稿守卫语义不变。

### 4. `frontend/intent`：意图入队即实时唤醒目标前端

原生最小化或进程已切 `Accessory` 时不依赖 Reopen。链路：

1. **守护进程**（`crates/maild/src/{events.rs, frontend.rs, api.rs}`）：新 `EmitEvent` `FrontendIntentQueued`（载荷 `String`＝目标 frontend id，不含任何路由数据）。`FrontendBroker` 持有可选 `IntentWaker`（`Arc<dyn Fn(&str) + Send + Sync>`），`enqueue` 在**释放队列锁之后**调用它——监听器同步执行、可能回头 claim。`FrontendPlugin::apply` 注入 `ctx.emit::<FrontendIntentQueued>`，并用 `effect_sync` 在卸载时撤回（断开 broker → ctx 的引用）。`serve_with_shutdown` 为每个 IPC 长连接订阅该事件，经 `frontend_intent_notification` 只投给主体**恰好**是 `Principal::Frontend(target)` 的会话：`{"jsonrpc":"2.0","method":"frontend/intent","params":{"frontend":"<id>"}}`。user、插件、其他前端（含前缀相同的 id）一律收不到；HTTP 传输本就没有推送。意图本身仍只能 `claimFrontendIntent` 一次性领取。
2. **客户端**（`crates/maild-client`）：`ServerNotification::FrontendIntent { frontend }`；参数形状不对按既有通知一样报协议错误，未知通知兼容逻辑不变。
3. **连接 actor**（`crates/maild-app`）：`ViewState.intent_epoch: u64`（初值 0，单调、跨重连保留）。收到发给本连接所声明前端的 `FrontendIntent` 即递增并广播；它不是快照状态，不进同步缓冲（否则快照收尾会重放、重复计数），同步期间也立即生效。
4. **Tauri 状态泵**（`crates/maild-ui/src/lib.rs`）：`spawn_state_pump` 记住已见的 `intent_epoch`，变大即 `show_and_activate_main_window` 并 `spawn_intent_claim`（排空队列，最后一个意图决定页面）。多次推送合并成一次快照变化时只需一次唤醒。
5. **启动器**（`crates/maild-launcher/src/runtime.rs`）：`open_on_launch` 走到原位分支时若 `.main-ui.lock` 已有主（窗口可能正原生最小化在 Dock 或位于其他桌面），先 `request_mailbox(com.maild.ui)` 入队收件箱意图——Agent 随即推 `frontend/intent`，窗口自己回到前台——再 `platform::focus(pid)`，返回新增的 `LaunchOutcome::Focused` 并退出。`host_in_place` 的持锁复查只剩竞态兜底。

菜单栏 `✉`（`maild-menu` 的 `openFrontend`）与通知点击（Agent 的 `broker.open`）本来就经 `FrontendBroker::open` 入队，无需改动即获得实时唤醒；ADR-0044 的 `NSWorkspace` 激活与 Reopen 照旧保留，重复唤醒无害。

## 取舍

- **启动器排的收件箱意图可能盖过刚排队的意图**：ADR-0044 曾因此刻意不在原位路径排 `Inbox`。现在只在「窗口已在运行、却又起了一个新的 `Maild.app` 进程」时排——LaunchServices 正常会把再次打开投给已运行的 `maild-ui`，不会新起启动器，所以这条路径罕见；而它一旦出现，正是 Reopen 不可靠、必须靠推送唤醒的情形。用户从 Finder / Spotlight 主动打开应用，落在收件箱是合理的。`background` 意图会跳到「后台活动」页，不是中性的唤醒，所以不用它。
- **为什么不收纳最小化**：首版把黄灯也收进菜单栏，代价是 `define_class!` + 改写 `NSControl` target 的 `unsafe` 代码、替换默认菜单、webview 快捷键与基于失焦/延迟复查的兜底，且偏离用户对 `–` 的系统预期（Dock 缩略图一键还原）。把「彻底离开」交给 `×`，`–` 留给系统原生最小化，代码与心智模型都更简单。
- **依赖**：`maild-ui` 的 macOS 依赖新增 `objc2 0.6.4`（只为 `MainThreadMarker`；与 tauri、`maild-menu` 同一版本，`Cargo.lock` 只多一条依赖边，不引入新 crate），`objc2-app-kit` 增开 `NSApplication / NSResponder` feature。
- **非 macOS**：最小化同样是系统原生；关闭路径的 `retire_window` 只是先隐藏窗口。

## 验证

- `maild`：`enqueue_wakes_the_target_frontend_outside_the_queue_lock`（锁外唤醒、回调内 claim 不死锁、无效意图不唤醒、撤回后不再唤醒）；`frontend_intent_push_reaches_only_the_exact_frontend_principal`；`user_sessions_never_see_frontend_intent_pushes`（真实 serve 会话）。
- `maild-client`：`frontend/intent` 解码为 `FrontendIntent`，参数类型错误被拒。
- `maild-ui` 集成测试 `the_shell_binds_as_its_frontend_and_projects_the_mailbox`（真 daemon）：user 经 `openFrontend` 给 `com.maild.ui` 排意图 → 已认证的 `frontend:com.maild.ui` 长连接收到推送，`ViewState.intent_epoch` 恰好 +1；同时在听推送的 user 长连接收不到 `frontend/intent`；意图仍可 claim。状态泵的 `each_new_intent_epoch_wakes_the_window_exactly_once`。
- 打包（M2）：`test_bundle.sh` / `verify-bundle.sh` 断言顶层 `Maild.app` 的 `LSUIElement = true`。
- 真机：`×` / `⌘Q` 后 Dock 无 `Maild` 活动图标或最近使用残留、`maild-ui` 进程消失、菜单栏 `✉` 仍在；黄灯 / `⌘M` / 双击标题栏为原生最小化（Dock 保留图标与缩略图）；最小化或已关闭状态下点菜单栏 `✉`、通知横幅、Dock 图标或 Finder 双击 `Maild.app` 都能把窗口还原到前台并跳到对应页面。
