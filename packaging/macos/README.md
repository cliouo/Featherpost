# macOS bundle、Agent、Menu 与发布生命周期（M3.5-05 至 M4.5-10，ADR-0047 品牌参数化）

这里生成一个未签名、未安装的顶层 App：原生启动器和同目录的单宿主主窗口可执行文件，
内嵌后台 Agent、独立菜单栏 companion 和内置 Tauri 前端的恢复副本（ADR-0039）。

## 品牌来源：`brand.env`

本目录**不写死任何产品名**。所有 bundle 名、bundle ID、二进制名、图标名、数据目录与环境变量前缀都来自
[`brand.env`](brand.env)——它和 `*-bundle-id.txt` 一样由 `bash scripts/sync-brand.sh` 从仓库根的
[`brand.toml`](../../brand.toml) 生成，CI 以 `bash scripts/sync-brand.sh --check` 拦截漂移。改名流程见
[`docs/BRANDING.md`](../../docs/BRANDING.md)。每个脚本和测试启动时都执行 `. "$script_dir/brand.env"`。

| brand.env 变量 | 当前值（Featherpost） | 用途 |
| --- | --- | --- |
| `BRAND_APP_BUNDLE` | `Featherpost.app` | 顶层 App，输出末级名称 |
| `BRAND_AGENT_BUNDLE` / `BRAND_MENU_BUNDLE` / `BRAND_UI_BUNDLE` | `Featherpost Agent.app` / `Featherpost Menu.app` / `Featherpost UI.app` | 内嵌 bundle |
| `BRAND_LAUNCHER_BIN` / `BRAND_AGENT_BIN` / `BRAND_MENU_BIN` / `BRAND_UI_BIN` | `featherpost-launcher` / `featherpost-agent` / `featherpost-menu` / `featherpost-ui` | `CFBundleExecutable` 与 Cargo `[[bin]]` 名 |
| `BRAND_APP_BUNDLE_ID` / `BRAND_AGENT_BUNDLE_ID` / `BRAND_MENU_BUNDLE_ID` / `BRAND_UI_BUNDLE_ID` | `dev.featherpost.desktop` / `.agent` / `.menu` / `.ui` | plist、签名 identifier、前端包目录 |
| `BRAND_ICON_NAME` | `Featherpost` | `Contents/Resources/Featherpost.icns` 与 `CFBundleIconFile` |
| `BRAND_SLUG` / `BRAND_ENV_PREFIX` | `featherpost` / `FEATHERPOST` | 数据目录、临时目录前缀 / 环境变量前缀 |

模板是品牌中立的，只含 `__BRAND_<KEY>__` 占位符，由 `build-bundle.sh` 渲染（plist 值做 XML 转义）：

| 模板 | 渲染到 |
| --- | --- |
| `templates/App.Info.plist.in` | `Contents/Info.plist`（`LSUIElement = true`） |
| `templates/Agent.Info.plist.in` | `Contents/Library/LoginItems/<Agent>.app/Contents/Info.plist`（`LSUIElement = true`） |
| `templates/Menu.Info.plist.in` | `Contents/Library/Helpers/<Menu>.app/Contents/Info.plist`（`LSUIElement = true`） |
| `templates/UI.Info.plist.in` | 恢复副本 `<UI>.app/Contents/Info.plist` |
| `templates/frontend.toml.in` | 恢复副本 `frontend.toml`（schema 与 `crates/maild-ui/package/frontend.toml` 一致，只改 `on_demand` + `[launch]`） |

可用占位符：`PRODUCT_NAME`、`LAUNCHER_BIN`、`AGENT_BIN`、`MENU_BIN`、`UI_BIN`、`ICON_NAME`、
`APP_BUNDLE_ID`、`AGENT_BUNDLE_ID`、`MENU_BUNDLE_ID`、`UI_BUNDLE_ID`、`APP_BUNDLE`、`AGENT_BUNDLE`、
`MENU_BUNDLE`、`UI_BUNDLE`、`BUNDLE_SHORT_VERSION`、`BUNDLE_BUILD_VERSION`。渲染后残留任何
`__BRAND_*__` / `__MAILD_*__` 都会直接失败。应用图标源文件是 [`assets/AppIcon.icns`](assets/AppIcon.icns)
（`sync-brand.sh --icons` 从 `assets/brand/icon-artwork.jpg` 重新渲染），复制为
`Contents/Resources/${BRAND_ICON_NAME}.icns`。

编译期 identity 扫描前缀 `maild-agent-bundle-id:`、`maild-menu-bundle-id:`、`maild-ui-embedded:` 是
二进制契约（`maild-brand` 的 `*_CONTRACT_PREFIX`），**有意不随品牌变化**；跟在前缀后的 bundle ID 才随品牌变化。

## Bundle 布局

以下按当前品牌展开：

```text
Featherpost.app/
└── Contents/
    ├── Info.plist
    ├── MacOS/
    │   ├── featherpost-launcher   CFBundleExecutable；就地 exec 同目录 featherpost-ui 承载主窗口
    │   └── featherpost-ui         embedded 形态 UI（与恢复副本同源），单宿主 → Dock 只有一个图标
    ├── Library/
    │   ├── LoginItems/Featherpost Agent.app/Contents/{Info.plist,MacOS/featherpost-agent}
    │   └── Helpers/Featherpost Menu.app/Contents/{Info.plist,MacOS/featherpost-menu}
    └── Resources/
        ├── Featherpost.icns       CFBundleIconFile = Featherpost；源文件 assets/AppIcon.icns
        └── frontends/dev.featherpost.ui/
            ├── frontend.toml      由 templates/frontend.toml.in 渲染，禁止手改
            ├── recovery.sha256    恢复副本 UI 可执行文件的 SHA-256（64 位小写十六进制 + 换行）
            └── Featherpost UI.app/Contents/{Info.plist,MacOS/featherpost-ui}
```

单宿主布局（消除 Dock 双图标，ADR-0044）：主窗口不以独立的 UI bundle 身份启动，而是由启动器在顶层
App 的 bundle 身份下就地 `exec` 顶层 `Contents/MacOS/${BRAND_UI_BIN}`。生成器把同一个已校验的 embedded
UI 同时安装到这里和恢复载荷中；验证器要求顶层副本是非符号链接的可执行常规文件、架构与 launcher 一致，
并且每个 slice 都带 `maild-ui-embedded:${BRAND_UI_BUNDLE_ID}` 编译期标记，防止把加载 devUrl 的开发构建
放进主窗口位置。

Agent 启动时在前端目录租约下把这份副本 no-clobber 预装到用户前端目录，只做一次（标记文件
`.preinstalled-<ui_bundle_id>`）；用户移除后不会自动装回，只能从原生修复窗口显式恢复。

基础 `build-bundle.sh` 只复制已经构建好的四个可执行文件。它不会运行 Cargo、签名/notarize、复制到
`/Applications`、调用 `SMAppService`/`launchctl`、启动进程或修改用户配置和数据。
目标已存在时会直接失败并保持原字节。输出父目录必须由当前用户拥有且不能由 group/other 写入；
这样验证完成到发布之间的 staging bundle 不会被其他本机用户替换。

## 本地生成与验证

先构建前端资源与 workspace 中的 release 二进制（UI 需 `embedded` 形态），再生成 bundle：

```sh
pnpm --dir packages/maild-ui build
cargo build --workspace --release
cargo build --release -p maild-ui --features embedded
packaging/macos/build-bundle.sh
. packaging/macos/brand.env && packaging/macos/verify-bundle.sh "dist/macos/$BRAND_APP_BUNDLE"
```

默认输入是 `target/release/${BRAND_LAUNCHER_BIN}`、`${BRAND_AGENT_BIN}`、`${BRAND_MENU_BIN}` 与
`${BRAND_UI_BIN}`；只给 `--launcher-bin` 时 Menu 与 UI 取同目录的同名文件。默认输出是
`dist/macos/${BRAND_APP_BUNDLE}`，显式 `--output` 的末级名称也必须是 `${BRAND_APP_BUNDLE}`：

```sh
packaging/macos/build-bundle.sh \
  --launcher-bin /absolute/path/featherpost-launcher \
  --agent-bin /absolute/path/featherpost-agent \
  --menu-bin /absolute/path/featherpost-menu \
  --ui-bin /absolute/path/featherpost-ui \
  --output /absolute/path/Featherpost.app
```

四份 plist 固定声明 macOS 13.0 为最低版本；顶层 App、Agent 和 Menu 都固定声明 `LSUIElement=true`。
顶层 App 以菜单栏优先方式启动：`/Applications`、启动台和
Spotlight 仍显示应用图标并可正常双击启动；只有窗口存在期间 UI 才在运行时把激活策略切到
`Regular`（黄灯 `–` / `⌘M` 为 macOS 原生最小化，Dock 保留图标与缩略图）。点击窗口红叉、`⌘Q` 或 Dock 右键
「退出」均仅关闭窗口并彻底清空 Dock 图标：统一退出流水线先隐藏窗口并切回 `Accessory` 再退出，Dock 不留应用
图标，也不进「最近使用的应用」区，后台 Agent 与菜单栏 `✉` 继续运行。菜单栏「退出 Featherpost」一并关闭
窗口、菜单栏与后台守护（ADR-0045 / ADR-0046）。若磁盘 plist 缺少 `LSUIElement`，即使进程退出前切回
`Accessory`，Dock 仍会按 plist 把它记入最近使用区，因此 `verify-bundle.sh` 要求顶层 plist 含布尔值
`LSUIElement=true`，缺失、为 `false` 或非布尔值都会被拒绝。

### Bundle identity

App / Agent / Menu 三个 ID 由 `brand.toml` 生成到 `brand.env` 与 `app-bundle-id.txt`、
`agent-bundle-id.txt`、`menu-bundle-id.txt`（Rust 侧在编译期包含后者）。生成器与验证器以 `brand.env` 为准，
并要求三个 `*-bundle-id.txt` 与之逐字相同，否则提示重新运行 `sync-brand.sh`。脚本拒绝任何
`${BRAND_ENV_PREFIX}_{APP,AGENT,MENU,UI}_BUNDLE_ID` 或旧 `MAILD_*_BUNDLE_ID` 环境覆盖——只改 plist 会让
launcher 的 `SMAppService` identifier、Agent 的 `UNUserNotificationCenter` 身份门禁与内嵌 plist 漂移。
正式签名/发布前修改 `brand.toml` 的四个 `*_bundle_id`，运行 `bash scripts/sync-brand.sh`，重新编译全部
二进制，最后生成 bundle。生成和验证时会读取每个 Mach-O slice 的编译期 identity contract；陈旧二进制、
缺失 contract 或 universal binary 内两个 slice 的 ID 不一致都会被拒绝。正式发布 identity 一旦冻结，后续
更新不得再改。

仓库默认的 `dev.${BRAND_SLUG}.*`（当前 `dev.featherpost.*`）是**开发身份**，`release-bundle.sh` /
`verify-release.sh` / `release-macos.yml` 在读取签名或 notary 凭据前硬拒绝它（也拒绝改名前的 `dev.maild.*`）。

### 版本变量

可选版本变量为 `${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION`（默认 `0.1.0`）和
`${BRAND_ENV_PREFIX}_BUNDLE_BUILD_VERSION`（默认 `1`），当前即 `FEATHERPOST_BUNDLE_SHORT_VERSION` /
`FEATHERPOST_BUNDLE_BUILD_VERSION`。旧名 `MAILD_BUNDLE_SHORT_VERSION` / `MAILD_BUNDLE_BUILD_VERSION` 仍作为
回退被读取；两者同时存在时品牌前缀优先。

## Hermetic 验证

dry-run 使用临时目录和 C fixture 可执行文件，不运行 Cargo、不读取真实配置、不运行 bundle，也不触碰
用户目录：

```sh
bash -n packaging/macos/*.sh packaging/macos/tests/*.sh
packaging/macos/dry-run.sh
packaging/macos/tests/test_bundle.sh
packaging/macos/tests/test_release.sh
packaging/macos/tests/test_lifecycle.sh
```

回归覆盖精确目录结构、plist/编译期 identity、universal2 slice、版本/最低系统版本、顶层 App/Agent/Menu
`LSUIElement=true`（顶层改为 `false`、非布尔或删除均被拒绝，改回 `true` 后恢复通过）、二进制架构一致且
逐字节复制、输入扩展属性不传播、目标 no-clobber、异常结构拒绝，以及脚本不写入测试用 `HOME`。单宿主相关的
负向断言包括：顶层 UI 可执行文件缺失、为符号链接、无执行位、非 embedded 或架构不一致，应用图标缺失/为符号
链接或 `CFBundleIconFile` 漂移，均被 `verify-bundle.sh` 拒绝；签名后篡改、剥离签名或替换顶层 UI 被
`codesign --verify --deep --strict=all` 拒绝，漏掉显式签名（只剩无 runtime 的 ad-hoc/linker 签名）被
`verify-release.sh` 拒绝。

品牌参数化另有两条回归：`test_bundle.sh` 把整个 `packaging/macos` 复制到临时目录、换上另一份
`brand.env`（产品名 `Quill & Ink`，含空格与 `&`），**不改任何脚本**即可生成并验证完整 bundle，且 plist 与
清单值中不再出现原品牌；同一夹具还断言 `*-bundle-id.txt` 与 `brand.env` 漂移时验证器在正确的门禁失败。
旧 `MAILD_BUNDLE_*` 回退与品牌前缀优先级也有断言。验证器是只读的，不会替 bundle 签名或注册登录项。

## Universal2 与正式发布门禁

无密钥 CI 使用 locked release 构建同时生成 `arm64`、`x86_64` 两个 slice：

```sh
# aarch64-apple-darwin 与 x86_64-apple-darwin 已在仓库根 mise.toml 声明，mise install 时装好
packaging/macos/build-universal.sh --output /private/tmp/featherpost/Featherpost.app
```

Cargo 包名（`-p maild`、`-p maild-launcher` 等）是内部稳定名，`--bin` 取 `brand.env` 的二进制名。
脚本会把 git work tree 的 short commit 以 `MAILD_BUILD_REV`（以及前向兼容的
`${BRAND_ENV_PREFIX}_BUILD_REV`）注入编译，使 `getAgentInfo` 应答携带 `build_rev`（ADR-0016）；
非 git 检出省略该字段。

正式发布只允许在干净且精确标记为 `vX.Y.Z` 的 checkout 上执行；`crates/maild`、`crates/maild-launcher`、
`crates/maild-menu`、`crates/maild-ui` 四个包、四份 plist、前端清单和 tag 的版本必须一致。组织冻结其控制的
正式 bundle IDs 并重新编译后，受保护的 `macos-release` GitHub Environment 才能运行
`.github/workflows/release-macos.yml`。正式 Team ID 的唯一可信来源是 `team-id.txt`；仓库默认值 `UNSET`
会让发布和生产安装门禁 fail-closed，冻结时必须随正式 identity 一同评审提交，不能从候选签名或 workflow
参数自证。发布产物名为 `${BRAND_PRODUCT_NAME}-vX.Y.Z-universal2.zip`（及 `.sha256`、`-notary.plist`）。

发布链固定为：内置 UI bundle 签名 → `seal-recovery-payload.sh` 按签名后的字节重写 `recovery.sha256` →
Agent 与 Menu 内层签名 → 顶层 `Contents/MacOS/${BRAND_UI_BIN}` 以同一 Developer ID、
`--identifier "$BRAND_UI_BUNDLE_ID" --options runtime --timestamp` 单独签名 → 顶层 App 签名 →
Developer ID/Team/runtime/timestamp/无 entitlements 校验（顶层 UI、Agent、Menu、内置 UI、App 逐一校验）→
ZIP 公证并等待 `Accepted` → staple/validate → Gatekeeper → 重建最终 ZIP → 解包复验 → SHA-256。签名不使用
`codesign --deep`；`--deep` 只用于最终的递归验证。

> **为什么顶层 UI 必须单独签：** `codesign` 签顶层 App 时只签 `CFBundleExecutable`（launcher），不会给同目录
> 辅助可执行文件加 hardened runtime。arm64 下 rustc/cc 产物自带 linker ad-hoc 签名，漏签时
> `codesign --verify --deep --strict=all` 仍会通过，但 Apple notary 会因缺少 runtime/Developer ID 拒绝；
> 因此 `verify-release.sh` 首先对它做 Identifier/Team/runtime/timestamp/无 entitlements 校验。

workflow 仅上传 Actions artifact，不创建 GitHub Release。签名身份与 notary 参数来自品牌中立的仓库变量
`MACOS_SIGNING_IDENTITY`、`MACOS_NOTARY_KEY_ID`、`MACOS_NOTARY_ISSUER_ID` 与 secrets
`MACOS_DEVELOPER_ID_P12_BASE64`、`MACOS_DEVELOPER_ID_P12_PASSWORD`、`MACOS_NOTARY_KEY_P8_BASE64`。
手工调用 `release-bundle.sh` 时，`--output-dir` 必须预先创建、由当前用户拥有且不能由 group/other 写入；
脚本还会拒绝路径中的符号链接和不安全祖先。

## 本地 Preview DMG

公开仓库的 `.github/workflows/release-macos-preview.yml` 在推送 `v*` tag 时自动构建 universal DMG，
也可通过 `workflow_dispatch` 的 `tag` 输入重建已有 tag。它在标准 `macos-15` runner 上运行，不使用
Actions cache 或 artifact，直接将 `${BRAND_SLUG}-<version>-macos-universal.dmg` 和 `.sha256` 上传到
对应的 GitHub Release（同名文件覆盖）；Release 不存在时创建，版本含 `-` 时标记为 prerelease。
这些体验包仅 ad-hoc 签名、未经公证，不代替正式的 Developer ID 发布流程；私有仓库会跳过该 job。

本地体验包在 verified bundle 基础上做 inside-out ad-hoc 签名（内置 UI → reseal → Agent → Menu → 顶层 UI →
顶层 App，最后校验顶层 UI 的 identifier 与 runtime 标志）并生成只读压缩 DMG；它不使用 Developer ID、没有
notarize，不能作为正式发布产物：

```sh
packaging/macos/build-universal.sh --output "$PWD/dist/macos/Featherpost.app"
packaging/macos/build-preview-dmg.sh \
  --app "$PWD/dist/macos/Featherpost.app" \
  --output "$PWD/dist/macos/Featherpost-preview.dmg"
```

`--output` 缺省为 `dist/macos/${BRAND_PRODUCT_NAME}-preview.dmg`，卷名为 `${BRAND_PRODUCT_NAME} Preview`。
挂载后把 App 拖到 Applications 链接或 `~/Applications`。当前 macOS 不允许 ad-hoc 签名的 Preview 以内嵌
Login Item 运行，因此 Preview 打开时会直接托管内嵌 Agent，关闭完整 UI 时一并停止；“后台与通知”可手工停止
或重新启动这个预览内核。它不会随登录启动。正式使用 Developer ID 签名的 bundle 仍由 `SMAppService` 管理独立
Agent，登录时只启动 Agent 而不启动完整 UI。启用 `[plugins.menu]` 后 Agent 会监督轻量 Menu companion：
Agent 运行期间菜单无论如何退出都会按退避重启；菜单动作和通知点击才按需拉起默认前端，菜单最后一项
「退出 Featherpost」经 `stopManagedAgent` 一并关闭窗口、菜单栏与后台守护（ADR-0046）。

> **嵌套 UI bundle 的签名与 staple 规则（[ADR-0040](../../docs/decisions/0040-release-gate-and-compatibility-matrix.md)）：**
> 内置 UI bundle 位于 `Contents/Resources/frontends/${BRAND_UI_BUNDLE_ID}/` 下，与顶层 App、Agent、Menu 使用同一
> Developer ID 身份按内到外顺序独立签名，并在同一份 ZIP 中提交公证；`stapler staple` 只对顶层 App 执行（向已
> 签名的 `Contents/Resources` 子树追加内层 `CodeResources` 会破坏顶层签名封条）。预装采用 no-clobber +
> `.preinstalled-<ui_bundle_id>` 一次性标记，用户手动移除后重启或升级均不会装回。

首次登记前，部分 macOS 版本会在已经找到内嵌 Login Item 的情况下仍把 `SMAppService.status` 报为
`notFound`。内核会把这个初始状态作为可登记状态，只有实际 `register()` 失败或登记后仍不可用时才报告
bundle/identity 错误。登记成功后 Agent 会立即运行，以后登录时也由系统启动。

首次从已安装的精确顶层 App 启动、且平台配置尚不存在时，桌面端会在
`~/Library/Application Support/${BRAND_SLUG}/${BRAND_SLUG}.toml`（当前
`~/Library/Application Support/featherpost/featherpost.toml`）原子创建一份无账户、无 token 的 0600 配置；
用户自建配置不会覆盖。`--config`、`${BRAND_ENV_PREFIX}_CONFIG`、`--url/--socket/--pipe` 或对应环境变量覆盖
存在时也绝不写平台配置。生成后需由用户手工添加 `[[accounts]]`（或在 UI 中走账户向导）。Preview 会在默认
平台配置和 endpoint 下自动托管内核，但不会注册登录项或请求通知权限；正式签名版本仍需从“后台与通知”显式
启用 Agent。

`SMAppService.unregister()` 会由系统终止正在运行的 Login Item，系统不保证 daemon 的异步 teardown 一定完成。
若因此留下 socket inode，下次只有受管 Agent 会在验证路径仍位于当前用户的私有 runtime 目录、文件仍是同 UID
的同一 socket 且连接明确返回 `ConnectionRefused` 后回收它；普通交互式 daemon 和受管目录外的自定义 socket
继续 fail-closed，不自动删除。

停止当前后台运行和取消登录启动都在同一面板执行“停用后台 Agent”。卸载前先停用，再把 App 移到废纸篓；
配置、邮件库、备份和日志默认保留。Preview 的 Gatekeeper 行为不代表正式 Developer ID + notarized 包，正式体验
仍需 M3.5-07 外部门禁。

## 用户级安装、升级、回滚和卸载

`manage-app.sh` 的生产目标固定为当前 UID 的 passwd home 下 `~/Applications/${BRAND_APP_BUNDLE}`，拒绝环境变量
伪造的 `HOME`，也不接受目标覆盖、root 或 `/Applications`。默认模式只接受 `verify-release.sh --stapled` 通过的
正式产物；仓库 fixture 和本地未签名 bundle 必须显式加 `--development`：

```sh
packaging/macos/manage-app.sh --development install \
  --source /absolute/path/Featherpost.app
packaging/macos/manage-app.sh --development inventory
```

脚本从不注册或注销 Login Item。升级、回滚、恢复和卸载前，先在已安装 App 的后台面板显式关闭 Agent；随后命令
同时要求 `--confirm-unregistered`。已安装 Agent 的 `maintenance-exec` 取得锁后，把真实锁 FD 跨 `exec` 继承给
管理脚本（环境变量 `${BRAND_ENV_PREFIX}_MAINTENANCE_LOCK_FD` / `${BRAND_ENV_PREFIX}_MAINTENANCE_VALIDATOR`；
改名前的 Agent 使用的 `MAILD_MAINTENANCE_*` 仍被接受，但无论哪种都必须通过 `maintenance-validate-fd` 证明锁
仍被持有），因此执行文件事务的进程自身会一直持锁：

```sh
packaging/macos/manage-app.sh --development upgrade \
  --source /absolute/path/Featherpost.app --confirm-unregistered
packaging/macos/manage-app.sh --development rollback --confirm-unregistered
packaging/macos/manage-app.sh --development recover --confirm-unregistered
packaging/macos/manage-app.sh --development finalize
packaging/macos/manage-app.sh --development uninstall --confirm-unregistered
```

升级只接受相同 App/Agent ID、架构和（生产模式）Team ID 的严格更高三段版本。旧 bundle 会保留为
`~/Applications/.<App>.rollback.app`，直到用户显式 `finalize`；崩溃残余（`.<App>.transaction`）要求 `recover`，
不会自动覆盖或猜测。卸载只把精确顶层 App no-clobber 移入当前用户废纸篓，配置、数据库、备份、runtime 和日志
（`~/Library/Application Support/${BRAND_SLUG}`）全部保留。

旧 Agent 若没有 `maintenance-exec`，脚本无法在整个事务中证明它持续停服，因而会 fail-closed。首次从这种开发版
迁移时，应人工确认旧进程退出并移走旧 bundle，再执行 fresh install；不要用 `kill`、`launchctl` 或 `sfltool`
代替生命周期协议。

完整决策与外部门禁见
[`../../docs/decisions/0013-m3-5-macos-release-lifecycle.md`](../../docs/decisions/0013-m3-5-macos-release-lifecycle.md)
与 [`../../docs/decisions/0047-decoupled-brand-and-open-source-readiness.md`](../../docs/decisions/0047-decoupled-brand-and-open-source-readiness.md)。
