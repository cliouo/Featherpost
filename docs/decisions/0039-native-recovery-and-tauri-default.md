# ADR-0039：G05/09 独立原生修复入口与 Tauri 默认交付

- 状态：已接受；09-A、09-B、09-C 均已实现（2026-11-12）
- 日期：2026-09-23
- 取代：ADR-0024 决策 3 的 egui 恢复前端、ADR-0014 的前端故障导致内核停止条款
- 前置：[交付计划](../design/tauri/IMPLEMENTATION-PLAN.md)、[设置与包管理](0038-settings-and-managed-packages.md)

用户已要求彻底废弃 egui，并授权按原型和计划继续实施。按 09-A/B/C 顺序先落地恢复能力，再移除旧 crate
和其构建依赖。恢复窗口只管理启动、后台与内置前端，不提供降级文本邮箱。

## 交付切片

- **09-A（已完成）**：固定副本来源、安全预装/修复事务、离线维护锁与前端目录独占、UserOnly IPC、损坏默认状态刷新与故障注入测试。
- **09-B（已完成）**：原生 launcher 修复窗口、后台控制、Menu/设置入口和前端故障隔离。
- **09-C（已完成）**：四二进制打包、首次预装接线、Tauri 默认切换与旧 ID 迁移，移除 egui crate/依赖。

下面的目标布局即 09-C 起的安装产物；实施细化见文末「09-C 实施记录」。

## 目标布局与职责

```text
Maild.app
  native maild-launcher  -> 正常打开默认前端；故障时显示系统原生修复对话框
  LoginItems/Agent       -> 收信、存储、API、通知、前端路由
  Helpers/Menu          -> 日用入口与显式修复入口
  Resources/frontends/com.maild.ui/
    frontend.toml + recovery.sha256 + Maild UI.app  -> 经过校验的离线恢复副本
         |
         v
用户 frontends/com.maild.ui/      -> 与第三方相同的受管 Tauri 包
```

启动器使用 Rust + macOS NSAlert / ServiceManagement 绑定，其他平台保留 rfd 原生适配，无 webview、egui 或新语言。
macOS 对话框属于启动器自身，支持独占窗口聚焦；无父窗口的 rfd 对话框走系统通知，不用于此入口。
普通启动先补齐无凭据默认配置及首次预装，连接后台并激活默认前端。原生恢复可独立启动，即使
Tauri 包或 daemon 不可用。顶层窗口只显示后台状态、打开邮箱、恢复内置前端和后台控制。

正式签名版本由用户显式注册 SMAppService 登录项。未签名 Preview 显式启动一次内嵌 Agent，
由 Agent 生命周期锁阻止重复；阅读窗口关闭后继续收信，原生后台入口可显式停止。这取代旧的
“egui 窗口寿命决定 Preview 后台寿命”，不伪装成登录项注册。

## 故障隔离与兼容（09-B/C）

09-B 增补：`maild-launcher` 提供独立原生修复窗口；09-C 切换包布局前，旧 `maild-desktop --repair`
仅转入相同的原生窗口，不初始化 egui。两种入口均不传递邮件身份。旧 `builtin-egui` 的默认选择
暂保持，09-C 再执行旧 ID 迁移；09-B 的缺包/坏包/能力不足回落统一交给 `builtin-repair`，仅携带 background。
前端目录无法安全读取或独占时仅保留原生恢复路由，拒绝目录变更，不停止收信与存储。

恢复窗口在同一 runtime 下持有独占 `.recovery-ui.lock`；重复打开只尝试激活已有窗口。
发起方限制短时间内的重复启动，避免前端请求造成进程与等待线程累积。旧壳恢复窗口打开邮箱时
显式启动正常旧阅读入口，传入当前 socket/pipe 以禁用其 Preview 自动后台托管，再退出恢复窗口，避免相同 bundle 身份把请求交回自身；09-C 删除此过渡。
其他前端只确认异步激活请求，修复窗口保留；independent 前端明确显示请求入队，不谎报打开失败。
取得窗口锁失败时只输出固定诊断并退出，不留下无主阻塞对话框。
子进程仅继承 HOME/XDG/APPDATA 等平台路径与临时目录/语言环境，API 端点覆盖和凭据不继承，
隔离用户目录不能在启动修复或后台时跳回真实用户目录。

目录准备失败会显式标记为不可用，不向管理接口提供根或占用锁。只有持锁且成功扫描的目录
才可对账身份或执行常规包变更；恢复状态保留既有凭证，成功显式修复后才重启正常目录刷新。

后台停止使用新增 `stopManagedAgent {}`：仅同用户 IPC 的 User 主体、且仅平台 Agent 装配控制服务。
它唤醒正常 LIFO 退出流程；原生窗口不按 RPC 返回的 PID 发送信号。登录项注册/撤销和启动一次
均由用户在原生窗口显式操作。开发/端点覆盖模式不启动或修复平台后台。

09-C 将默认前端变为 com.maild.ui；09-B 已使用恢复标识 builtin-repair。缺失或损坏前端只使激活进入原生修复，
不会关闭邮件内核；恢复入口只声明 background 能力，不声称能阅读或写信。旧配置/持久选择的
09-C 将 builtin-egui 在读取时迁移为默认 Tauri；显式请求旧恢复 ID 则作为 builtin-repair 路由别名。
不重写用户 TOML，不发送邮件身份到 argv、URL 或日志。

## 首次预装与恢复事务

首次预装为 no-clobber，并写一次性标记。标记存在后，即使用户移除包，后续启动/升级也不自动
装回。已有同名包原样保留。恢复按钮明确说明只替换 com.maild.ui 并改回默认，其他前端、账户、
邮件、凭据均保留；同版本损坏也能修复。被替换包和损坏选择文件保留在隐藏恢复目录，发布失败
还原旧包，不把覆盖失败说成成功。

在线修复走新的同用户 UserOnly repairBuiltinFrontend，无路径参数，只能使用当前完整 Maild.app
内的固定副本；与现有包事务串行。它是受限原生维护动作，不授权给前端或后台扩展，补充
ADR-0030 对管理授权的边界。离线修复必须取得 Agent 生命周期锁；配置启用持久库时，另取配置中实际数据库的维护锁。
所有启用 frontend 的后台（包括另一配置/数据库或无 Store 的 CLI daemon）及离线修复共同争用
`frontends/.owner.lock`。它由 frontend 插件持有，卸载时撤回；在线修复任务持有 Arc 延续至任务结束，
防止主进程退出期间并发写入。安装/移除/回滚/改默认也必须取得并持有此服务；frontend 卸载后即拒绝新写请求。
旧版本裸 CLI 尚不参与此锁，切换前必须退出。
修复不打开或迁移 SQLite。端点覆盖/开发运行不隐式改平台状态。

在线恢复的复制、发布、目录刷新和身份对账在同一不可取消的后台任务中持有包事务锁；
请求断连不提前释放锁。目录刷新失败会冻结默认选择与移除，避免旧缓存允许删除新默认。
发布成功但目录/身份刷新失败会明确返回“已恢复到磁盘，请重启”，不声称整个操作成功。
副本校验覆盖清单、固定路径、Mach-O 头、属主执行权限、plist 身份/版本及整个二进制的 SHA-256。
09-C 打包必须从已验证的 embedded 可执行文件生成 `recovery.sha256`（64 位小写十六进制摘要加换行，共 65 字节）；
摘要用于检测复制/截断/损坏，不是来源认证。预装与修复发布前均检查 64 个包的目录容量，替换不占新槽。
这不替代 09-C/10 的代码签名与真实激活验收。

## 构建与验收

顶层 launcher、Agent、Menu 和 embedded Tauri UI 四个二进制进入同一架构/身份/版本校验。
保留既有 no-clobber 安装、Preview DMG 与签名发布流程，并把嵌套 UI bundle 纳入签名、notary
与验证。先构建前端产物，再编译 embedded UI；daemon 的单独构建不需要 Node。

整个 G05/09 覆盖 core、launcher、菜单栏、构建脚本、测试、CI 与文档。09-A 仅修改恢复内核、维护接口、测试与文档。最终验收包括缺包、
坏包、用户移除后不重装、同版本修复、失败回退、未知/损坏默认、后台仍收信、权限拒绝，以及真实
隔离 bundle 的启动与恢复。正式签名凭据、Windows 真机和长期日用仍归 M4.5-10，不能用构建通过
代替这些证据。代码回退可用 Git，修复事务保留原包，不删除邮件或账户数据。

参考：[Apple SMAppService](https://developer.apple.com/documentation/ServiceManagement/SMAppService)、
[Apple 应用激活配置](https://developer.apple.com/documentation/appkit/nsworkspace/openconfiguration)、
[Tauri macOS bundle](https://v2.tauri.app/distribute/macos-application-bundle/)。本片复用系统机制，
不自建登录项协议或 HTML 引擎。

## 09-C 实施记录（2026-11-12）

- **首次预装放在 Agent 启动内**：前端插件取得 `frontends/.owner.lock` 后、首次扫描前调用 no-clobber
  预装，因此登录项启动、Preview 和升级后的 Agent 都会覆盖，启动器不需要另取离线锁。
  `preinstall_offline` 保留为离线入口，目前无调用方。预装失败只记诊断，默认路由回落原生修复。
- **启动器正常打开**：补齐无凭据默认配置（新配置 `default = "com.maild.ui"`）→ 后台未运行时，
  Preview 身份（`dev.maild.desktop`）启动一次内嵌 Agent，正式身份仅在登录项已启用时等待；
  其余情况直接显示原生窗口 → 后台在线后调用 `openFrontend`，平台已受理激活即退出。
  异步激活失败由 Agent 的完成回调转入原生修复。09-B 的旧阅读器直启过渡已删除。
- **旧 ID**：`builtin-egui` 不再是目录条目。配置与 `.default.json` 读取时映射为 `com.maild.ui`，
  不改写文件；显式请求旧 ID 视为 `builtin-repair` 别名，只携带 background；该 ID 继续保留，
  第三方包不得声明。
- **打包**：`build-bundle.sh` 以 `--launcher-bin/--ui-bin` 生成新布局。前端清单由模板生成，
  路由与授权由 maild 测试强制与开发清单一致。签名会改写 Mach-O，所以签名链为
  `Maild UI.app` → `seal-recovery-payload.sh` 重算 `recovery.sha256` → Agent/Menu → 顶层 App；
  `verify-bundle.sh` 校验摘要与当前字节一致。嵌套于 `Resources` 的已签名 UI 通过
  `codesign --verify --deep --strict=all`。
- **内嵌形态契约**：`maild-ui` 只在 `--features embedded` 下编入 `maild-ui-embedded:com.maild.ui` 标记；
  `build-bundle.sh`/`verify-bundle.sh` 拒绝任一 slice 缺标记的 UI，防止打入会从本机端口加载 `devUrl` 的开发形态。
- **窗口所有权与异步失败**：启动器在启动/等待 Agent 期间持有修复窗口锁，发出激活请求前释放，
  使 Agent 在异步激活失败时拉起的 `--repair` 能取得窗口；激活未被受理时重新取锁，若已有修复窗口则直接退出。
- **随应用升级内置前端**：首次预装之后，Agent 每次启动比较已装 `com.maild.ui` 与内置副本的版本
  （仅 `X[.Y[.Z]]` 数字形式可比）。只有内置副本严格更新时才替换，旧包移入 `.repair-backups/upgrade-*`，
  默认选择不变。包不存在（用户已移除）、清单不可读、版本相同、更新或不可比较时一律不动，
  损坏留给显式修复。不追踪包的来源：`com.maild.ui` 由本应用发布，同名开发安装同样按版本跟随。
- **09-C 之前的安装包**：旧布局（`maild-desktop` 入口）只在开发者本机装过，从未分发，因此打包与
  `manage-app.sh` 不做兼容；换装时先停后台、手动移走旧 Maild.app 再安装新包，用户数据目录不受影响。

仍归 M4.5-10：正式签名/公证实跑、Windows 真机、长期日用；真实隔离 bundle 内由 LaunchServices
启动的 UI 会继承登录会话环境而非隔离 HOME，图形激活闭环需在真实用户目录下按发布门禁验收。

