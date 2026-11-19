# ADR-0044：macOS 单 Dock 图标运行时——脱离 Agent Coalition、`Maild.app` 原位承载内置 UI 与 Reopen 意图唤醒

- 状态：已接受；运行时（launcher / agent / maild-ui / maild-client）已实现（2026-11-17，`64d6103`）；打包侧单宿主 `Maild.app/Contents/MacOS/maild-ui`、`Maild.icns` 与由内而外显式签名已交付（`a08b8fe`），两者在 `cliouo/dock-integration-dmg` 合入并完成全量门禁与真实 bundle 验签（见 [testing](../testing/README.md#2026-11-17单-dock-身份adr-0044合入回归)）
- 日期：2026-11-17
- 关联：[ADR-0024](0024-builtin-frontend-tauri-migration.md)、[ADR-0030](0030-frontend-authorization-closure.md)、[ADR-0039](0039-native-recovery-and-tauri-default.md)、[ADR-0040](0040-release-gate-and-compatibility-matrix.md)、[ADR-0041](0041-low-footprint-background-receive.md)

## 背景

用户从 Dock 打开打包后的 `Maild.app` 时，Dock 同时出现两个图标：常驻亮灯的「Maild」与「Maild UI」。真机 `lsappinfo` 验证的两个根因：

1. **幽灵 `Maild`（`exited-with-subordinates`）**：`maild_launcher::run()` 入口无条件 `prepare_ui()`，把进程设为 `Regular` 前台应用；`Operation::StartOnce` 再用 `Command::spawn()`（`fork+exec`）直接拉起 `maild-agent`。前台 App fork 出后台子进程后退出，LaunchServices / RunningBoard 看到同一 Coalition 内仍有存活子进程（`maild-agent`、`maild-menu`），把已退出的 `Maild.app` 永久挂在 `type="Foreground" (exited-with-subordinates)`，无窗口的 `Maild` 图标一直亮灯。
2. **第二个 Bundle ID**：真正开窗的是 `~/Library/Application Support/maild/frontends/com.maild.ui/Maild UI.app`（`CFBundleIdentifier = com.maild.ui`），所以固定在 Dock 的 `Maild.app` 右侧会多出一个 `Maild UI` 图标；`tauri.conf.json` 的 `productName` 也让系统菜单显示 `Maild UI`。另外 `maild-ui` 只在 `WindowEvent::Focused(true)` 时领取意图，窗口已在前台时点击菜单栏「写邮件 / 设置」或通知横幅不会再触发聚焦，意图滞留。

## 决策

### 1. 后台脱离 Coalition（`maild-launcher`）

- `Operation::StartOnce` 在真实用户会话（`maild_client::system_home()` 为真，且 `managed()` 已排除自定义连接覆盖）下，改用 `NSWorkspace.openApplicationAtURL:configuration:completionHandler:`（`activates = false`、`createsNewApplicationInstance = false`）打开 `NativeAppLayout::agent_app_bundle()`，即 `Contents/Library/LoginItems/Maild Agent.app`。`launchd` 以独立 `LSUIElement` 应用托管 Agent，与 `Maild.app` 的 Coalition 无关；关窗（`⌘Q`）后 `Maild` 图标立即灭灯，后台继续收信。完成回调最多等 10 秒：明确报错才返回失败；未应答交给调用方既有的 10 秒在线等待判断。
- 隔离 `HOME`（`/private/tmp` 等，`system_home() == false`）保留 `native_child_command` 直接子进程：LaunchServices 会以真实用户目录启动，测试隔离不能被破坏。
- `system_home()` 从 launcher 私有实现上移到 `maild-client`（`native_recovery.rs`），launcher 的登录项判定与 Agent 的单 Dock 判定共用同一份。

### 2. 延迟 AppKit 初始化与修复窗口的 ActivationPolicy

- 从 `maild_launcher::run()` 顶部移除 `platform::prepare_ui()`。只有 `show_recovery()` 与 `ui::show_error()` 真正要显示原生对话框时才初始化 AppKit；原位承载或交给第三方前端的正常打开全程不成为前台应用，没有 Dock 闪烁。
- `prepare_ui()` 先探测 `runtime_dir/.main-ui.lock`：已有主邮件窗口持锁（例如在邮件窗口「设置 → 打开修复窗口」触发 `maild-launcher --repair`）时，修复进程设为 `NSApplicationActivationPolicy::Accessory` 并 `activateIgnoringOtherApps(true)`，对话框浮在最前而不产生第二个 Dock 图标；主界面未运行时仍为 `Regular`。

### 3. `Maild.app` 原位承载内置 UI

`NativeAppLayout` 新增从 `Contents/MacOS/maild-ui` 反查 `Maild.app`，以及 `app_bundle()`、`ui_executable()`、`agent_app_bundle()`、`bundled_frontend_payload()`。`ui_executable()` / `agent_app_bundle()` 复用 `checked_executable` 全链校验：任一组件是符号链接、非当前用户所有、组/其他可写，或可执行文件无执行位都拒绝。

`maild-launcher`（非 `--repair`）在确保 Agent 在线后调用 `listFrontends`，由纯函数 `launch_route` 分流：

| 目录状态 | 结果 |
|---|---|
| 默认是 `com.maild.ui`、同包 `Contents/MacOS/maild-ui` 合法、已装受管包完好 | **原位**：`LaunchOutcome::InPlace` |
| 默认是 `com.maild.ui`，但本包没有合法的 `maild-ui`（旧包、开发二进制、自定义连接） | 照旧 `openFrontend(Inbox)`，由 Agent 激活 |
| 默认是第三方前端 | `openFrontend(Inbox)` 后静默退出（未初始化 AppKit，无 Dock 闪烁） |
| 默认是 `builtin-repair`（内置前端被移除或无法加载）、或默认不在目录中 | 原生修复窗口 |
| 已装 `com.maild.ui` 与随包载荷**同版本但可执行文件字节不一致**，且清单与可执行文件本身合法（同版本重打包覆盖安装留下的旧构建） | 请求运行于本应用包的 Agent `repairBuiltinFrontend` 同步后原位打开；同步失败或 Agent 不在本包内才进原生修复窗口 |
| 已装 `com.maild.ui` 可执行文件缺失、无执行位或清单无效 | 原生修复窗口（ADR-0039/0040 故障隔离） |

已装包检查由 `maild::frontend::check_installed_bundled_frontend(installed, bundled)` 完成：`listFrontends` 已证明包存在、未被移除、权限合规且协议代兼容；此处再按 `load_package_at` 重新加载已装清单，要求其 `Maild UI.app/Contents/MacOS/maild-ui` 是当前用户所有、非组/其他可写、可执行的普通文件；若与随包 `frontend.toml` 版本相同，其 SHA-256 必须等于随包 `recovery.sha256`。版本不同（待升级、回滚）不比字节；随包载荷不可读不归咎于已装包。校验和读取与计算抽成 `read_recovery_checksum` / `executable_checksum`，与 `inspect_bundled_frontend` 共用。

同版本字节不一致的修订：本地开发每次改代码重打包、或同一版本号重签名发布，`frontend.toml` 版本都不变而 UI 可执行文件变化。此前 Agent 启动的 `refresh` 只在随包版本严格更高时替换，覆盖安装后已装包永远停在旧构建，每次打开都被当作损坏弹出修复窗口，用户手动“恢复内置前端”后才正常。现在 `installed_bundled_frontend_is_stale` 识别“合法但来自另一构建”的已装包：`refresh` 对其整包重发布（旧包照常备份，结果为 `RefreshOutcome::Resynced`）；覆盖安装时旧 Agent 仍在运行（不会重启、也就不会 `refresh`）的情况由启动器在原位检查前请求同包 Agent 执行 `repairBuiltinFrontend`。可执行文件缺失、无执行位、清单无效等真实损坏仍走原生修复窗口。

原位分支：

1. 释放 `.recovery-ui.lock`（`window_owner`），邮件窗口之后自己打开 `--repair` 时可以取得修复窗口。
2. 探测 `.main-ui.lock`：已有 `maild-ui` 持锁则 `platform::focus(pid)` 后直接返回，不产生新进程。
3. 否则**不**额外排队 `OpenIntent::Inbox`：它会冲掉通知点击或菜单栏刚排队的 `Message` / `Compose` / `Settings`，而 `maild-ui` 无待领意图时本就展示收件箱。
4. 以 `native_child_command`（清空 API 凭据类环境，仅保留平台路径变量）`CommandExt::exec()` 替换为同目录的 `Contents/MacOS/maild-ui`。`exec` 保持 PID 与 LaunchServices ASN 不变，`NSBundle.mainBundle` 仍是 `/Applications/Maild.app`，所以亮灯开窗的就是用户点的那一个 `Maild` 图标。`exec` 失败则重新建立控制器并回落原生修复窗口。

`maild-ui` 的 `Bridge` 仍以 `frontend:com.maild.ui` 主体连接守护进程：主体来自同用户 IPC 上的身份凭证，与可执行文件路径无关，ADR-0030 的最小权限不被削弱。`Contents/Resources/frontends/com.maild.ui/` 恢复载荷与受管安装目录的契约不变。

### 4. 通知点击与菜单栏统一唤醒顶层 `Maild.app`（`maild::frontend`）

`PlatformLauncher::activate` 对 `MacosApplication` 目标先经 `single_dock_host`：只有 `frontend.info.id == BUNDLED_FRONTEND_ID` 才会求值宿主；macOS 上 `platform_single_dock_host` 要求无自定义连接、`system_home()`、`NativeAppLayout::discover()` 命中完整 `Maild.app` 且 `ui_executable()` 合法，并且已装 `Maild UI.app` 通过 `validate_application` 基本完整性检查。满足时激活 `layout.app_bundle()`（顶层 `Maild.app`），否则照旧激活已装 `Maild UI.app`（后者自身失败仍按既有路径转原生修复）。

- `Maild.app` 未运行：系统拉起 `maild-launcher` → 原位 `exec` `maild-ui` → 上线后领取队列中的意图。
- `Maild.app` 已运行：`NSWorkspace`（`createsNewApplicationInstance = false`、`activates = true`）激活既有进程并投递 `RunEvent::Reopen` → 立即领取意图跳转。

### 5. `maild-ui` 单实例锁、Reopen 与菜单名

- `tauri.conf.json`：`productName` 改为 `"Maild"`，系统菜单与 Dock 名称一致。
- `instance_lock.rs`：无自定义连接（命令行 `--config/--url/--socket/--pipe` 或对应环境变量）且 `AppPaths::discover()` 可用时，`main` 开始即在 `runtime_dir` 取非阻塞排他锁 `.main-ui.lock`（`0600`、`O_NOFOLLOW | O_CLOEXEC`、`nlink == 1`、当前用户所有，写入 PID），守卫活到 `main` 结束。已被其他进程持有时读取 PID、经 `NSRunningApplication` 聚焦后 `exit(0)`。runtime 目录尚未就绪等锁失败退化为无锁启动，不拒绝展示邮件。launcher 的探测用共享锁且瞬时释放，持有方对 `WouldBlock` 重试 5 次（间隔 20 ms），不会把探测误当成另一个窗口。锁原语放在 `maild-client`（`claim_main_ui` / `running_main_ui`），launcher 与 `maild-ui` 共用。
- `RunEvent::Reopen`：取 `"main"` 窗口执行 `unminimize` / `show` / `set_focus` 并领取意图。
- `spawn_intent_claim` 升级为排空循环 `drain_intents`：持续 `claim_intent` 直到队列为空、出错或单轮达到 16 条；全局 `tokio::sync::Mutex` 串行化聚焦、上线与 Reopen 同时触发的排空，意图按入队顺序到达 webview。

## 不变量

- 隔离 `HOME`、自定义连接、开发二进制的行为与 ADR-0039 一致：不经 LaunchServices，不原位 `exec`，不取平台单实例锁。
- 内置前端被移除或损坏时依旧落入原生修复窗口；修复事务、预装标记、升级规则（ADR-0039）未改动。
- 邮件身份从不进入 argv / 环境变量：`exec` 与 Agent 启动都只经 `native_child_command` 或 LaunchServices。
- 第三方前端的激活路径与权限不变。

## 验收

- `maild-client`：`in_place_ui_resolves_the_app_and_its_agent_bundle`、`in_place_ui_and_agent_bundle_reject_symlinks_and_wide_permissions`、`main_ui_lock_is_single_owner_and_reopens_after_release`、`main_ui_lock_rejects_symlinks`。
- `maild-launcher`：`healthy_bundled_default_is_hosted_in_place`、`bundled_default_without_in_place_ui_still_asks_the_agent`、`third_party_default_uses_open_frontend_without_in_place_checks`、`missing_or_damaged_bundled_frontend_shows_recovery`、`recovery_beside_the_main_window_never_adds_a_dock_icon`。
- `maild`：`only_the_bundled_frontend_is_hosted_by_the_top_level_app`、`installed_bundled_frontend_must_match_same_version_bundle_bytes`、`refresh_resyncs_a_same_version_copy_from_a_different_build`。
- `maild-ui`：`second_instance_focuses_the_first_and_release_allows_reopen`、`unsafe_lock_never_focuses_or_follows_links`、`custom_connections_never_take_the_platform_lock`、`draining_delivers_every_queued_intent_in_order`、`draining_is_bounded_and_stops_on_errors`。
- 门禁：`cargo test --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、同命令 `--target x86_64-pc-windows-gnu`。`Cargo.lock` 只为既有 crate 增加依赖边（launcher → `block2`，maild-ui → `objc2-app-kit`），不引入新的第三方 crate。
- 真机验证（D2 打包后）：从 Dock 打开 `Maild.app` 只有一个 `Maild` 图标亮灯；`⌘Q` 后图标灭灯而 `lsappinfo` 中 `Maild Agent` 仍在；窗口在前台时点击通知或菜单栏「写邮件」立即跳转。
