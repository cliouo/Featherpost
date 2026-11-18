# ADR-0011：M3.5 macOS App bundle 与登录 Agent

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：macOS 13+ 顶层 App、后台登录 Agent、Service Management 注册和后续发布流程

## 背景

M3.5-04 已统一 daemon、CLI 和桌面端的平台目录与配置发现，但开发态仍直接运行独立二进制。
macOS 登录时启动后台收信需要稳定的 bundle identity、系统可见的授权状态，以及不会依赖 shell、
当前目录或临时构建路径的进程入口。直接写 `~/Library/LaunchAgents`、调用 `launchctl`，或让桌面端
自行维护后台进程，都会绕开 macOS 13 起的 Background Items 授权和状态管理。

Apple 在 macOS 13+ 要求用 `SMAppService` 管理放在主 App bundle 内的 Login Item。注册 Login
Item 后它会立即启动并在后续登录时启动；崩溃或非零退出会触发系统重启。配置错误若沿用普通
fatal exit，会形成无意义的重启循环。

## 决策

### Bundle 结构固定为顶层 App 加嵌套 Login Item

正式形态只有一个可安装单元 `Maild.app`：

```text
Maild.app/
  Contents/
    Info.plist
    MacOS/
      maild-desktop
    Library/
      LoginItems/
        Maild Agent.app/
          Contents/
            Info.plist
            MacOS/
              maild-agent
```

- `maild-desktop` 是用户打开的顶层 GUI，只负责界面、状态展示和显式的 Agent 管理操作。
- `maild-agent` 是无 Dock 图标、无窗口的每用户 Login Item，复用现有 daemon 装配和 LIFO
  teardown，不启动 GUI，也不通过 shell 再查找或拉起 bundle 外的 `maild` 可执行文件。
- Agent 的 `Info.plist` 设置 `LSUIElement=true`。顶层 App 和 Agent 的最低系统版本均为 macOS
  13.0，`CFBundleShortVersionString` 与 `CFBundleVersion` 必须一致。
- 不引入 LaunchDaemon、`SMJobBless`、root helper、手写 LaunchAgent plist 或
  `SMLoginItemSetEnabled` 兼容分支。macOS 12 及更早版本不在首期支持范围。

### Bundle ID 目前只是开发默认值

本阶段使用以下 provisional 默认值：

- 顶层 App：`dev.maild.desktop`
- Login Item Agent：`dev.maild.agent`

这两个值只用于本地实现、测试和未发布构建，不构成发布身份冻结。正式发布前，负责分发的组织
必须选择其控制的反向域名 ID，并在同一次发布身份决策中替换两个值。使用 `dev.maild.*` 的产物
不能通过正式发布门禁，也不能被描述为将来可原位升级的稳定版本。

两个 ID 的唯一仓库来源是 `packaging/macos/app-bundle-id.txt` 与
`packaging/macos/agent-bundle-id.txt`。构建脚本把它们写入两份 plist，Rust 桌面端和 Agent 在编译
时直接包含 Agent manifest，脚本拒绝环境变量单独覆盖 ID。正式发布前修改 manifest 后必须重新
编译 desktop 与 Agent，再生成和验证 bundle；生成器从两个 executable 的每个 Mach-O slice 读取
编译期 identity contract，拒绝陈旧、缺失或 slice 间不一致的 ID。禁止只改 plist。正式 ID 一旦随
公开版本发布，后续升级不得改变，否则系统会把它视为新的 Login Item 和新的授权主体。Agent 作为
唯一原生通知身份的授权与投递边界见 [ADR-0012](0012-m3-5-native-notifications.md)。

### 注册、注销和状态均由用户显式触发或查看

顶层 App 通过 `SMAppService.loginItem(identifier: <Agent CFBundleIdentifier>)` 取得唯一 service。
首次启动 App、打开设置页或发现 Agent 未运行时都不得自动注册。用户开启“登录时运行 Maild”后
才调用一次 `register()`；用户关闭该选项后调用 `unregister()`，等待完成后再更新界面。

每次操作前后都重新读取 `service.status`，界面按四种状态显示：

- `notRegistered`：未注册，可以由用户发起注册；
- `enabled`：已注册且允许运行，不等同于 daemon 已连接或配置健康；
- `requiresApproval`：已注册但仍需用户在 System Settings 操作，只在用户点击时调用
  `openSystemSettingsLoginItems()`；
- `notFound`：首次登记前可能只是 Service Management 尚无数据库记录，UI 允许用户执行一次
  `register()`；只有调用失败或登记后仍不可用时才视为 bundle/identity 错误，不自动循环重试。

`register()` 和 `unregister()` 的错误必须保留原错误域与错误码供诊断，面向 UI 的文本需要去除
控制字符并限制长度。不得记录配置内容、Bearer token、账户凭据或完整环境变量。状态刷新不使用
轮询重试风暴；App 激活、用户操作完成和明确的手动刷新是状态重读时机。

### Agent 只使用平台配置，配置错误不进入重启循环

已安装 Agent 必须通过 `maild-paths` 取得平台默认配置的绝对路径，再进入现有生产装配。它不读取
cwd，也不继承 `MAILD_CONFIG`、`MAILD_API_*` 或 shell 的 PATH 来改变配置和 endpoint。顶层 App、
Agent 与客户端因此仍遵守 ADR-0010 的同一 config/data/runtime namespace。

缺少配置、TOML 无效、相对路径越界、legacy 冲突、目录权限不安全、数据库锁已被占用，以及插件
装配所需的本地配置错误，都属于不可通过系统重启自行恢复的 configuration stop。Agent 输出一条
有界且不含秘密的诊断，完成已经取得资源的 teardown，然后以成功状态退出，避免
`SMAppService` 因非零退出持续重启。`service.status == enabled` 只说明系统仍允许这个 Login Item，
顶层 App 必须把 daemon 连接状态和配置预检结果单独展示。

用户修正配置后，由顶层 App 的“重新启动后台服务”操作显式执行一次受控的注销、状态确认和重新
注册。任何一步失败都停止后续动作并展示当前 `SMAppService` 状态，不用 `launchctl kickstart` 或
隐藏循环补偿。网络断开、邮件服务器暂时不可用和账户重连由 daemon 现有调度逻辑处理，不退出
Agent。未捕获 panic、进程崩溃等意外故障保留非零退出，让系统按 Login Item 语义重启并节流。

系统注销服务、用户退出登录或系统关闭时，Agent 接收停止信号并走现有 SIGTERM/Ctrl+C 的 LIFO
teardown。关闭 IPC listener、邮件连接和 Store 后以成功状态退出。退出顶层 GUI 不注销或停止
Agent。

### 顶层 App 与 Agent 属于同一 uid 信任域

Agent 以当前登录用户运行，不提权，也不访问其他用户的配置、数据库或 socket。配置、数据库、
锁、备份和 Unix socket 继续使用 ADR-0009、ADR-0010 已定义的 owner、权限、no-follow 和单实例
检查。顶层 App 不通过 argv、环境变量或临时文件向 Agent 传递 token 和账户凭据，Agent 自己从
同一用户的平台配置读取所需内容。

本地同 uid 的其他进程仍属于已接受的维护信任域。代码签名和 Team ID 用来证明 bundle 来源、
满足 Gatekeeper 与 Service Management 的身份要求，不承诺抵御同 uid 主动调试、注入或替换
用户自己可写的数据。首期不增加 XPC 特权边界或独立沙箱容器。

### 签名与 Team ID 是正式发布的外部门禁

本地开发可以生成未签名或 ad hoc 签名的 bundle 来做结构测试，但不能据此声明 Login Item 发布
链路通过。正式发布必须满足：

- 顶层 App、Agent 及其所有嵌套 Mach-O 使用同一个真实 Apple Developer Team ID；
- 先签 Agent 和其他嵌套代码，再签顶层 App，顶层签名封装最终 bundle 字节；
- 两份 plist、Rust 内的 Agent identifier、版本号、架构和最低系统版本一致；
- entitlements 采用最小集合，签名后逐个读取验证，不允许 Agent 获得未记录的额外权限；
- 在干净的 macOS 13+ 用户账户上通过 Gatekeeper、notarization、stapling、首次授权、重新登录、
  注销和再次注册验证。

完成这些检查需要组织拥有的 bundle IDs、Developer ID 证书、Team ID、notary 凭据和可交互真机，
因此属于外部门禁。仓库内 CI 只能验证 bundle 结构、plist 一致性、代码编译和无签名状态机测试。

### 升级和卸载只操作 bundle，默认保留全部用户数据

M3.5-05 不实现安装器或自动更新器，只冻结后续 M3.5-07 必须遵守的边界：

- 注册只能从稳定安装位置中的完整 `Maild.app` 发起，不能从 DMG、App Translocation、临时构建
  目录或已经准备删除的旧 bundle 注册。
- 升级替换整个顶层 `Maild.app`，不得单独覆盖 `Maild Agent.app` 或正在运行的 Agent executable。
  替换前请求 Agent 有序退出；替换失败必须保留可运行的旧 bundle。是否需要重新注册以及是否保留
  用户批准，要用相同正式 bundle IDs 和 Team ID 的签名候选在真机验证，未通过前不得启用自动
  更新。
- 升级不复制、移动、重命名或删除平台 config/data/runtime/log/backups/release 目录，也不自动选择
  backup、执行 restore 或改变 `MAILD_CONFIG`。Store 自己的事务型 schema migration 仍按
  ADR-0007 执行，它不等于文件系统数据迁移。
- 正常卸载先由用户显式注销 Agent 并确认状态，再删除顶层 bundle。卸载默认保留配置、数据库、
  WAL/SHM、持久锁文件、备份和日志，方便重新安装或人工恢复。
- 删除用户数据必须是后续单独设计的高风险操作，需要停服、明确列出目标并再次确认；普通卸载器
  不提供宽泛递归删除。不得调用 `sfltool resetbtm`，因为它会重置其他应用的 Background Items。
- 用户直接把 App 拖入废纸篓可能让 System Settings 的条目暂时保留。后续安装可以显示状态并提供
  显式清理，不扫描、迁移或删除未知路径。

## 后果

- Maild 在 macOS 上成为一个顶层产品 bundle，后台常驻由系统 Login Item 管理，GUI 生命周期与
  daemon 生命周期分离。
- 系统授权状态、daemon 连通性和配置健康是三个不同状态，界面不能用其中一个推断另外两个。
- 登录项状态与原生通知授权也是两个独立状态，通知权限只由 Agent 在用户显式操作后请求。
- 配置错误会停止 Agent 而不制造重启风暴；修复后的重启需要用户明确操作。
- 正式 bundle identity、签名、notarization 和升级保持授权仍需外部组织与真机证据，provisional
  ID 只允许开发。
- 安装、升级和卸载都不自动迁移或删除用户数据，恢复仍使用已有 backup/restore 流程。

## 验证

仓库内测试需要覆盖：

- 构建产物只有一个顶层 `Maild.app`，Agent 位于
  `Contents/Library/LoginItems/Maild Agent.app`，两份 plist 与 Rust identifier 完全一致；
- 四种 `SMAppService.Status` 到 UI 状态的映射，注册、注销、错误和手动刷新均有确定行为；
- 缺失或损坏配置、路径冲突和 Store 占用时 Agent 完成 teardown 并以成功状态退出，不在测试窗口
  内反复启动；意外非零退出与正常 SIGTERM 分开验证；
- Agent 启动参数和环境不接受 cwd、`MAILD_CONFIG` 或 `MAILD_API_*` 覆盖，数据库和 socket 仍落在
  ADR-0010 的平台路径；
- 打包检查拒绝 ID、版本、最低系统版本或架构不一致，也拒绝把 provisional ID 标记为 release。

正式发布前还必须在干净 macOS 13+ 账户上验证：注册后立即启动、`requiresApproval` 引导、登录后
重启、注销时终止、配置错误不形成重启风暴、同身份升级、卸载后数据保留，以及签名、Gatekeeper、
notarization 和 stapling。缺少其中任一证据时只能交付开发构建。

## 参考

- [Apple：Updating helper executables from earlier versions of macOS](https://developer.apple.com/documentation/servicemanagement/updating-helper-executables-from-earlier-versions-of-macos)
- [Apple：SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Apple：Placing content in a bundle](https://developer.apple.com/documentation/bundleresources/placing-content-in-a-bundle)
