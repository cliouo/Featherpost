# ADR-0013：macOS 用户级发布生命周期与正式发布门禁

- 状态：Accepted
- 日期：2026-08-21
- 适用范围：M3.5 macOS 13+ 安装、升级、回滚、卸载、签名和 notarization

## 背景

ADR-0011 已固定一个顶层 `Maild.app` 和内嵌 Login Item Agent，ADR-0012 已把原生通知身份收口到
Agent。仓库现在可以生成并验证未签名 bundle，但还缺少三个发布边界：用户怎样安装或替换整个
bundle、失败后怎样回到旧版本，以及正式产物怎样证明签名和公证完整。

首版不引入自动更新器、DMG/PKG 安装脚本、root helper 或 Windows 安装器。当前 provisional
`dev.maild.*` identity 也不能被误标为正式可分发版本。

## 决策

### 唯一安装位是当前用户的 Applications 目录

正式管理命令只操作当前 UID 的 passwd 记录所指向的 `~/Applications/Maild.app`，不信任环境变量
伪造的 `HOME`，不写 `/Applications`，不提权，也不接受任意安装目标。首次安装先把候选复制到同一
受信父目录的私有 staging，完整验证后再以 no-clobber rename 发布。目标已存在、父目录或其祖先
不安全、路径含符号链接或事务残余未处理时全部 fail-closed。

安装完成后不自动注册 Agent。用户必须从稳定安装位置首次打开 App，再在“后台运行”面板显式
启用。来自下载目录、DMG/App Translocation、临时构建目录或 staging 的候选不能替用户调用
`SMAppService`。

仓库测试允许显式 development 模式和隔离的临时 `HOME`，只验证结构化开发 bundle；默认生产
模式只接受通过正式 release verifier 的签名且已 staple 产物。development 模式不是发布证明。

### Agent 停止由用户操作，生命周期锁提供机器可验证证据

升级、回滚和卸载前，用户先在已安装 App 中显式关闭后台运行。管理脚本不调用
`SMAppService`、`launchctl`、`kill` 或 `sfltool`，也不自动重新注册。

`maild-agent` 在读取配置和装配插件前取得固定的当前用户生命周期锁，并持有到 daemon LIFO
teardown 完成。只读 maintenance probe 可诊断锁状态；真正的升级、回滚、恢复和卸载则由
`maintenance-exec` 取得同一把锁，清除真实锁 FD 的 `CLOEXEC` 后直接替换为管理事务进程。管理进程
重新校验继承 FD 的固定路径身份和锁状态，并持有该 FD 到事务退出；不存在“helper 先死、事务继续”
的独立生命周期。锁忙、旧 Agent 不支持继承或验证失败时操作立即拒绝。维护入口不启动 daemon、
不读取 TOML，也不接受配置或 API 环境覆盖。显式的“已注销”确认和贯穿事务的锁必须同时满足，
脚本才会改动 bundle。

生命周期锁只证明进程退出，不证明 Service Management 的批准状态。相同正式 ID/Team 升级后是否
保留后台项和通知授权仍必须由签名候选在真机验证；在此之前升级完成后保持未注册，由用户重新启用。

### 升级是分阶段原子发布与可恢复回滚

候选在移动旧版之前完成结构、bundle ID、版本、架构、签名、Team ID、entitlements 和 staple
验证。新旧正式 bundle 的 App/Agent ID、Team ID 和架构必须一致，新版本必须严格更高。

文件事务只在 `~/Applications` 的同一文件系统中进行：

1. 候选复制到私有 staging 并再次验证；
2. 旧 `Maild.app` 以一次 rename 发布为唯一 rollback；
3. staging 以一次 rename 发布为新的 `Maild.app`；
4. 新安装位再次验证，rollback 保留到用户显式 finalize。

普通错误在进程仍可执行时恢复旧版。若进程在两个 rename 之间崩溃，下次命令检测事务残余并拒绝
继续，要求显式 `recover`/`rollback`；不得猜测或覆盖。这是“每次发布原子、事务可恢复”，不是
两个非空目录的 crash-atomic swap。首版不为 `renameatx_np(RENAME_SWAP)` 再增加第三个签名 helper。

`rollback` 同样要求 Agent 已注销且维护租约已取得。`finalize` 是唯一会移除旧 bundle rollback 的显式
动作；普通升级不会静默删除最后一个可运行版本。

### 普通卸载只移动 App，永远保留用户数据

卸载要求相同的注销确认与生命周期锁。验证精确安装位后，把整个 `Maild.app` no-clobber 移入
当前用户废纸篓；冲突时拒绝，不递归覆盖。卸载不删除或改写 config、数据库、WAL/SHM、持久锁、
备份、runtime、release 目录和日志。

本阶段不提供 purge。未来若增加“擦除数据”，必须单独设计固定 allowlist、二次确认、停服证明和
symlink/hardlink 防护；不得声称能在 SSD 上安全擦除。任何流程都禁止使用会影响其他应用的
`sfltool resetbtm`。

### 正式发布必须内到外签名并通过 Apple notarization

无密钥 CI 构建固定 `MACOSX_DEPLOYMENT_TARGET=13.0` 的 arm64 与 x86_64 release 二进制，用
`lipo` 生成 universal2，再复用 bundle 结构和逐 Mach-O slice identity 门禁。普通 PR/push 不读取
任何签名或 notary 凭据。

正式发布脚本必须在任何签名或上传前拒绝 provisional `dev.maild.*` ID、未冻结的可信 Team ID、
dirty checkout、非 `vX.Y.Z` tag、版本漂移和非 universal2 产物。Team ID 只从仓库内受评审的
`team-id.txt` 读取，不接受候选签名或 workflow 参数自证。签名顺序固定为 Agent 在前、顶层 App
在后；两者使用同一 Developer ID Application identity、Hardened Runtime 和 secure timestamp。
禁止用 `codesign --deep` 执行签名，不授予 entitlements，尤其拒绝 `get-task-allow`。

验证必须分别检查 Agent 和 App 的签名、Team ID、runtime flag、timestamp、空 entitlements、
bundle ID 和架构。随后用 `notarytool` 等待明确 `Accepted`，staple App 并运行 `stapler validate` 与
Gatekeeper `spctl`。最终分发 ZIP 在 staple 后重新生成，再解包重复验证并生成 SHA-256 文件。

签名 workflow 只能由受保护环境和 release tag 触发，所有 Action 固定完整 commit SHA；同一 job
必须在导入凭据前完成 workspace test/clippy、packaging fixtures、universal2 构建和 release preflight。
凭据通过临时 keychain 和权限为 0600 的临时 API key 注入，清理步骤无论成功失败都执行，且在凭据
清除并复核产物 allowlist/checksum 后才调用上传 Action。首版只上传 CI artifact，不自动创建或覆盖
GitHub Release。

## 外部门禁

以下证据需要组织控制的正式 App/Agent IDs、Developer ID 证书、Team ID、notary 凭据和干净的
macOS 13+ 用户账户，仓库内测试不能替代：

- quarantine 下载后的首次 Gatekeeper 启动和离线 staple 验证；
- Agent 显式注册、立即启动、重新登录、注销和配置停止行为；
- 通知授权确实归属于 Maild Agent，拒绝、撤权和重启不会重复弹窗；
- 同 ID/Team 升级是否保留登录项批准和通知授权；
- 升级失败、显式 rollback、卸载和重装时数据保持；
- notarization Accepted 记录、签名 Team/entitlements 与最终 ZIP checksum。

正式 IDs 未冻结或任一外部门禁缺失时，只能称为通过 hermetic 发布工具链，不能称为正式已发布。
Windows named pipe、Toast/AUMID、安装器和真机 UI/资源证据继续按用户决定暂缓。

## 验证

仓库内自动化覆盖：

- actual universal2 locked build、两个 executable 的逐 slice identity 和 bundle 结构；
- hermetic fixture 覆盖签名顺序、修改嵌套 Agent 后顶层签名失效，以及 provisional/ad-hoc/错误
  Team 和 entitlements 提取边界；正式 Developer ID 的 runtime/timestamp/空 entitlements 正反证留外部门禁；
- 首装 no-clobber、升级成功、注入失败恢复、崩溃残余 recover、rollback/finalize；
- 生命周期锁忙时升级/卸载拒绝，且维护租约期间 Agent 无法抢占；卸载只移动 App，平台数据 sentinel 原字节保留；
- shell 语法、workspace test/clippy 和 Windows GNU 交叉编译门禁。

## 参考

- [Apple：Creating distribution-signed code for macOS](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/)
- [Apple：Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Apple：SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
