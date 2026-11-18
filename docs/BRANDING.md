# 品牌切换指南

Featherpost 的产品名、Bundle ID、CLI / 二进制名、图标、URI scheme、Keychain service 等**所有面向用户与操作系统的
名字**只在一个地方手写：仓库根目录的 [`brand.toml`](../brand.toml)。其余文件都由
[`scripts/sync-brand.sh`](../scripts/sync-brand.sh) 确定性生成，CI 用 `--check` 拦截漂移。决策背景见
[ADR-0047](decisions/0047-decoupled-brand-and-open-source-readiness.md)。

## 10 秒切换

```sh
$EDITOR brand.toml                              # 1. 改名字与标识
cp ~/my-icon.jpg assets/brand/icon-artwork.jpg  # 2.（可选）换图标原图：正方形、≥ 1024 px
bash scripts/sync-brand.sh --icons              # 3. 同步全部派生文件；--icons 重绘图标（仅 macOS）
bash scripts/sync-brand.sh --check              # 4. 只读复核：输出 "N derived files match brand.toml"
```

不换图标时第 3 步去掉 `--icons` 即可（任何装有 Python 3.11+ 的平台都能运行）。之后重新构建：

```sh
cargo build --workspace --locked                # 二进制名、编译期常量随之变化
packaging/macos/tests/test_bundle.sh            # 打包脚本直接读取新的 brand.env，无需改脚本
packaging/macos/build-universal.sh --output "$PWD/dist/macos/<app_bundle_name>"
```

`sync-brand.sh` 的写模式是幂等的：第二次运行输出 `already in sync`，`--check` 在写完后必然通过。

## `brand.toml` 字段

| 字段 | 当前值 | 影响 |
| --- | --- | --- |
| `product_name` | `Featherpost` | 窗口标题、`CFBundleDisplayName`、菜单栏「退出 …」、前端清单 `name`、发布产物名 `<product_name>-vX.Y.Z-universal2.zip`、Preview DMG 卷名 |
| `slug` | `featherpost` | 平台数据/配置目录（`~/Library/Application Support/<slug>`）、`<slug>.toml` / `<slug>.sqlite3` / `<slug>.sock`、临时目录前缀 |
| `env_prefix` | `FEATHERPOST` | 所有生产环境变量：`<P>_CONFIG`、`<P>_SOCKET`、`<P>_URL`、`<P>_TOKEN(_FILE)`、`<P>_PLUGIN_*`、`<P>_MAINTENANCE_LOCK_FD` / `_VALIDATOR`、打包用 `<P>_BUNDLE_SHORT_VERSION` / `_BUILD_VERSION`、`<P>_UI_ORIGIN` |
| `cli_daemon` / `cli_ctl` | `featherpostd` / `featherpostctl` | 守护进程与控制 CLI 的 Cargo `[[bin]]` 名 |
| `launcher_bin` / `agent_bin` / `menu_bin` / `ui_bin` | `featherpost-launcher` / `-agent` / `-menu` / `-ui` | App 内四个可执行文件（`CFBundleExecutable`）与对应 `[[bin]]` 名 |
| `app_bundle_name` / `agent_bundle_name` / `menu_bundle_name` / `ui_bundle_name` | `Featherpost.app` / `Featherpost Agent.app` / `Featherpost Menu.app` / `Featherpost UI.app` | 顶层与内嵌 bundle 目录名；安装位 `~/Applications/<app_bundle_name>` |
| `icon_name` | `Featherpost` | `Contents/Resources/<icon_name>.icns` 与 `CFBundleIconFile` |
| `app_bundle_id` / `agent_bundle_id` / `menu_bundle_id` / `ui_bundle_id` | `dev.featherpost.desktop` / `.agent` / `.menu` / `.ui` | plist `CFBundleIdentifier`、`SMAppService` 登录项、`UNUserNotificationCenter` 通知身份、编译期 identity contract、内置前端包 id 与 `frontends/<ui_bundle_id>/`、Tauri `identifier` |
| `uri_scheme` | `featherpost-mail` | HTML 阅读器隔离文档的自定义协议，同步进 Tauri CSP `frame-src` |
| `keychain_service` | `featherpost.account.credentials.v1` | macOS Keychain 中账户凭据的 service 名 |

`sync-brand.sh` 在写任何文件之前校验字段：`slug` 只含小写字母、数字与 `-`；`env_prefix` 只含大写字母、数字与 `_`；
二进制名符合 Cargo 规则；bundle 名以 `.app` 结尾且不含 `/`；bundle ID 是反向 DNS；`product_name` 不含引号与反斜杠
（允许空格与 `&`，plist 渲染时做 XML 转义）；未知字段或缺字段一律报错。

## 会生成哪些文件

| 派生文件 | 内容 |
| --- | --- |
| `crates/maild-brand/src/generated.rs` | 全部 Rust 品牌常量与编译期字面量宏（`bin_exe_daemon!`、`agent_id_contract!` 等） |
| `packages/maild-ui/src/brand.ts` | 前端 webview 用的 `BRAND` 常量 |
| `packaging/macos/brand.env` | macOS 打包脚本、测试与 GitHub Actions 读取的 `BRAND_*` 变量 |
| `packaging/macos/{app,agent,menu}-bundle-id.txt` | Rust 侧在编译期包含的 bundle identity；打包脚本要求与 `brand.env` 一致 |
| `crates/maild-ui/tauri.conf.json` | `productName`、`identifier`、CSP `frame-src <uri_scheme>:` |
| `crates/maild-ui/package/frontend.toml` | 开发期前端清单的 `id` / `name` |
| `crates/{maild,maild-launcher,maild-menu,maild-ui}/Cargo.toml` | `# BEGIN/END GENERATED BRAND BINS` 之间的 `[[bin]]` 名 |
| （`--icons`）`assets/brand/icon-1024.png`、`packaging/macos/assets/AppIcon.icns`、`crates/maild-ui/icons/icon.{png,ico}` | 从 `assets/brand/icon-artwork.jpg` 重绘的全套图标 |

macOS 打包模板（`packaging/macos/templates/*.in`）本身是品牌中立的，只含 `__BRAND_<KEY>__` 占位符，构建时由
`build-bundle.sh` 从 `brand.env` 渲染，所以**不需要**被同步脚本改写。

## 有意保持不变的东西

- **Cargo 包名与源码目录**：`crates/maild*`、`packages/maild-ui`、`-p maild` 是内部名，改它们只会制造巨大 diff 而
  用户看不见。
- **编译期 identity 扫描前缀**：`maild-agent-bundle-id:`、`maild-menu-bundle-id:`、`maild-ui-embedded:` 是打包门禁
  在 Mach-O 中搜索的稳定标记；跟在前缀之后的 bundle ID 才随品牌变化。
- **历史 ADR**：`docs/decisions/0001–0046` 以早期工作代号 `maild` 书写，作为架构演进档案原样保留。

## 改名之前要想清楚的事

在**还没有公开发布**时切换品牌没有任何代价。一旦有用户装过某个品牌的正式版本，下列字段就成了用户数据与系统登记的
「地址」，再改就等于发布一个全新的 App：

- `slug`：配置、邮件库、备份与 socket 的目录随之改变，新版本看不到旧目录里的数据（不会自动迁移）。
- `*_bundle_id`：系统会把它当成另一个 App——登录项需要重新登记、通知授权需要重新请求、`manage-app.sh` 拒绝跨 ID 升级。
- `keychain_service`：旧凭据仍留在钥匙串里，但新版本按新 service 读取，账户需要重新登录。
- `env_prefix`：脚本、launchd plist 或 shell 配置里的旧环境变量不再生效。
- `uri_scheme`：只影响运行时阅读器，可以随时改。

仓库默认的 `dev.<slug>.*` bundle ID 是**开发身份**：`release-bundle.sh`、`verify-release.sh` 与
`release-macos.yml` 会在读取任何签名或公证凭据前拒绝它（以及改名前的 `dev.maild.*`）。正式发布前，把四个
`*_bundle_id` 改成你组织控制的反向域名、运行 `sync-brand.sh`、提交 `packaging/macos/team-id.txt`，之后这些值
就不应再改。

## CI 门禁

- `.github/workflows/ci.yml` 的 `brand` job 运行 `bash scripts/sync-brand.sh --check`，有漂移就打印 diff 并失败。CI 已改为仅手动触发，提交前请在本机运行该命令。
- 打包测试（`packaging/macos/tests/test_bundle.sh`）包含一个「换牌」夹具：把 `packaging/macos` 复制到临时目录、换上
  另一份 `brand.env`（产品名 `Quill & Ink`），不改任何脚本就能生成并验证完整 bundle。
- `ci.yml` 与 `release-macos.yml` 中所有产物名都从 `brand.env` 读取；发布所需的 secrets / variables 都是品牌中立的
  `MACOS_*` 名字（见 [运维手册](operations/README.md#macos-发布-workflow-的仓库配置)），改品牌后 CI 无需手改。

## 常见问题

**`--check` 报告漂移怎么办？** 有人手改了派生文件或改了 `brand.toml` 却没同步。运行 `bash scripts/sync-brand.sh`
重新生成并提交；不要手改派生文件。

**`build-bundle.sh` 报 `*-bundle-id.txt differs from brand.env`？** 同上，说明打包输入与 `brand.toml` 不一致。

**打包时报 `does not contain exactly the expected compiled identity contract`？** 二进制是改名前编译的。改完
`brand.toml` 后需要重新 `cargo build`。

**可以只改显示名、不改 bundle ID 吗？** 可以。每个字段相互独立，只改 `product_name` 不会动任何标识。
