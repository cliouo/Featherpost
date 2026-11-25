<p align="center">
  <img src="assets/brand/icon-1024.png" width="128" height="128" alt="Featherpost icon">
</p>

<h1 align="center">Featherpost</h1>

<p align="center">
  <b>A feather-light, plugin-first mail client for macOS: a ~5 MiB background daemon, no resident WebView, real-time IMAP IDLE.</b>
</p>

<p align="center">
  <a href="LICENSE-MIT"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue"></a>
  <img alt="Platform: macOS 13+" src="https://img.shields.io/badge/platform-macOS%2013%2B-lightgrey">
  <img alt="Rust" src="https://img.shields.io/badge/kernel-Rust-orange">
</p>

---

**Featherpost** 是一个「邮件领域的 VS Code」：一个常驻但几乎不占资源的 Rust 守护进程负责收信、规则、
验证码与系统通知；完整界面只在你需要时才被拉起，关掉就彻底消失。内核是
[cordis](https://github.com/cordiverse/cordis) 时空可组合性方法论的 Rust 移植，一切能力——收信、存储、
规则、通知、前端、第三方服务商——都是可插拔、可热替换的插件。

> 项目早期工作代号为 `maild`：Rust crate 与源码目录（`crates/maild*`）仍沿用这个内部
> 名字，ADR-0001–0046 也以它书写。所有面向用户与操作系统的名字都来自根目录的
> [`brand.toml`](brand.toml)，见 [品牌切换指南](docs/BRANDING.md)。

## 为什么做 Featherpost

传统桌面邮箱客户端常驻数百 MB 内存、后台持续唤醒，只为了在新邮件到达时弹一个通知。Featherpost 把
「后台收信」和「看信界面」拆成两个进程：

| | Featherpost 后台守护进程 | 说明 |
| --- | --- | --- |
| 常驻内存 | **≈ 5 MiB** `phys_footprint`（RSS ≈ 15 MiB） | Activity Monitor「内存」列口径；3 账户 release 实测 4.95–5.08 MiB |
| 空闲 CPU | **≈ 0.001 %** | 5 秒空闲窗口实测 0.0009–0.0013 %，期间仅 1 次唤醒 |
| 常驻 WebView | **0** | 界面是按需拉起的 Tauri 窗口，关闭即退出进程 |
| 实时收信 | **IMAP IDLE 推送 P50 ≈ 4 ms** | 30 封实时推送 P50 3.8–4.1 ms、P95 7.4–10.5 ms |
| 冷启动 | 进程拉起 → socket 就绪 13–15 ms | 有界首同步窗口，历史邮件按需拉取 |

数字来自仓库内可复现的 release 子进程基准（回环 TLS、3 账户，`cargo run --release -p maild --example
footprint_bench`），完整口径与原始数据见 [docs/testing/README.md](docs/testing/README.md) 与
[ADR-0041](docs/decisions/0041-low-footprint-background-receive.md)。

## 亮点

### macOS 原生体验

- **原生菜单栏常驻**：顶层 App 以 `LSUIElement` 启动，平时只在菜单栏留一个 `✉`；需要时才拉起
  **唯一一个**主窗口宿主，Dock 永远只有一个图标（[ADR-0044](docs/decisions/0044-macos-single-dock-identity.md)）。
- **关窗即零残留**：红灯 `×`、`⌘Q` 或 Dock 右键「退出」只关闭窗口，并彻底清空 Dock 图标，不进「最近使用
  的应用」；后台收信与菜单栏照常工作（[ADR-0045](docs/decisions/0045-tray-only-minimize-and-close.md)）。
- **最小化保持原生**：黄灯 `–` / `⌘M` 是 macOS 原生最小化，Dock 缩略图一切如常。
- **一键全栈退出**：菜单栏「退出 Featherpost」一并关闭窗口、菜单栏与后台守护，前端在约 350 ms 排空窗口内
  先把草稿刷盘（[ADR-0046](docs/decisions/0046-menubar-lifecycle-and-dock-quit.md)）。
- **原生通知与登录项**：后台 Agent 以独立 bundle 身份经 `SMAppService` 登记、经 `UNUserNotificationCenter`
  发通知，权限只在你明确点击时才请求。

### 日常收发

- **10 秒添加邮箱**：内置主流服务商知识库（Gmail、Outlook、iCloud、QQ、163 等）零网络给出 IMAP/SMTP 配置，
  自定义域名才做有界探测，并拒绝 IP 字面量与内网目标（[ADR-0042](docs/decisions/0042-smtp-implicit-tls-auth-and-autodiscovery.md)）。
- **智能验证码**：结合上下文识别验证码，通知里一键复制；新邮件通知按账户限流、敏感内容可脱敏
  （[ADR-0043](docs/decisions/0043-persistent-rules-and-smart-notifications.md)）。
- **三层隔离的 HTML 阅读器**：sandbox iframe + 文档级 CSP + Rust 侧重写与导航否决，远程内容默认拦截，
  链接先看见再决定（[ADR-0033](docs/decisions/0033-html-reading-isolation-and-resource-channel.md)）。
- **离线也能操作**：标记、移动、删除先落本地，再由写回队列对账；结果未知会如实告诉你，而不是假装成功
  （[ADR-0027](docs/decisions/0027-mailbox-write-path-and-offline-queue.md)）。
- **秒级本地检索**：SQLite FTS5 + CJK 逐字切分，中文、日文、英文混排都能搜，线程按 `Message-ID` 图聚合
  （[ADR-0029](docs/decisions/0029-local-search-index-and-threading.md)）。
- **守护进程拥有的草稿与附件**：自动保存、版本化并发仲裁、持久投递状态，崩溃或关窗都不丢稿。

### 架构

- **纯插件化内核**：[cordis](https://github.com/cordiverse/cordis) 方法论（论文
  [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper)）的 Rust
  实现——服务、事件、作用域与生命周期都是一等公民，插件可热加载、原地替换、失败回滚
  （[docs/rust-port-design.md](docs/rust-port-design.md)）。
- **headless 优先**：所有能力都经 JSON-RPC 暴露（stdio、Unix socket、Windows named pipe、回环 HTTP），
  CLI、Tauri 界面、菜单栏和第三方前端都是平等的消费者（[协议](docs/protocol/README.md)）。
- **外部进程权限隔离**：第三方插件以独立进程运行，逐插件凭证认证、方法级 capability 授权、受管安装事务与
  进程组 / Job Object containment（[ADR-0017](docs/decisions/0017-m4-1-plugin-identity-and-auth-seam.md)–
  [ADR-0020](docs/decisions/0020-m4-4-process-containment.md)，[插件 package v1](docs/plugins/README.md)）。
- **可替换的前端**：默认界面也只是一个受管前端包；坏了有原生修复窗口兜底
  （[ADR-0039](docs/decisions/0039-native-recovery-and-tauri-default.md)）。

## 快速开始

开发工具（Rust 及 clippy / 打包与交叉编译 target、Node.js、pnpm、Python、Stalwart）统一由
[mise](https://mise.jdx.dev) 按仓库根 [`mise.toml`](mise.toml) 固定版本安装：

```sh
mise trust && mise install
```

平台自带、mise 管不了的只有 macOS 的 Xcode Command Line Tools（打包与图标渲染）和 Windows GNU 交叉 Clippy
用的 mingw-w64。中国大陆网络可选配 crates.io 镜像，见
[运维手册](docs/operations/README.md#可选国内-cratesio-镜像)。

```sh
# 全量测试：内核 + 守护进程（含 SMTP → IMAP IDLE → store → 验证码 → 通知的 hermetic e2e）
cargo test --workspace --locked

cargo run -p maild --bin featherpostd -- paths      # 平台配置 / 数据 / socket 目录
```

示例配置 [`crates/maild/maild.example.toml`](crates/maild/maild.example.toml) 默认走 stdio JSON-RPC、
通知用不输出验证码正文的 `dev-log`。要让 CLI 连上守护进程，把它复制一份并把 `[plugins.api.config]` 改成
回环 HTTP（`transport = "http"`、`listen = "127.0.0.1:7394"`、`bearer_token` 填下面生成的 token），
再填入你的 `[[accounts]]`：

```sh
cp crates/maild/maild.example.toml featherpost.toml
cargo run -p maild --bin featherpostctl -- generate-token   # 填进 bearer_token
chmod 600 featherpost.toml                                  # 含 token 的配置必须仅本人可读
cargo run -p maild --bin featherpostd -- --config featherpost.toml

# 另一个终端
cargo run -p maild --bin featherpostctl -- --config featherpost.toml status
cargo run -p maild --bin featherpostctl -- --config featherpost.toml messages --limit 20
cargo run -p maild --bin featherpostctl -- --config featherpost.toml account discover alice@qq.com
```

默认 Tauri 界面（Featherpost Studio，`dev.featherpost.studio`）壳在 `crates/maild-ui`（webview 在 `packages/featherpost-studio`）：

```sh
pnpm install --frozen-lockfile
cargo run -p maild-ui --example dev_stack     # 预置假数据的临时守护进程，打印 socket 路径
pnpm --dir packages/featherpost-studio dev &
cargo run -p maild-ui -- --socket <dev_stack 打印的路径>
```

更多 CLI（发信、同步、规则、备份/恢复、通知授权、插件管理）见 [协议与 CLI](docs/protocol/README.md) 与
[运维手册](docs/operations/README.md)。

## 构建 macOS App

打包脚本的所有名字（`Featherpost.app`、bundle ID、二进制名、图标）都从 `packaging/macos/brand.env` 读取：

```sh
# 两个 Apple target 已在 mise.toml 声明，mise install 时随工具链装好
packaging/macos/build-universal.sh --output "$PWD/dist/macos/Featherpost.app"   # universal2，未签名
packaging/macos/verify-bundle.sh "$PWD/dist/macos/Featherpost.app"
packaging/macos/build-preview-dmg.sh \
  --app "$PWD/dist/macos/Featherpost.app" \
  --output "$PWD/dist/macos/Featherpost-preview.dmg"                            # ad-hoc 签名体验包
```

Preview DMG 只用于本机体验；正式发布走 `.github/workflows/release-macos.yml`：Developer ID 签名、Hardened
Runtime、公证、staple、Gatekeeper 与最终 ZIP 复验。仓库默认的 `dev.featherpost.*` 是开发身份，正式发布前
需要在 `brand.toml` 冻结组织自己的 bundle ID 与 `team-id.txt`。细节见
[packaging/macos/README.md](packaging/macos/README.md)。

不想用 Featherpost 这个名字？改 `brand.toml`、换 `assets/brand/` 里的图标，再运行
`bash scripts/sync-brand.sh`，整个项目的产品名、Bundle ID、CLI 名与图标就切换完毕——见
[docs/BRANDING.md](docs/BRANDING.md)。

## 项目状态

仓库内的 M0–M4.5 里程碑与 macOS 菜单栏生命周期均已实现并有测试覆盖；macOS 13+ 是首要平台，Windows 在 CI 中
编译与测试，真机验证仍在进行。公开发布前剩余的是需要正式凭据或真机的外部门禁：组织正式 bundle ID +
Developer ID / notary 实跑、干净 macOS 真机安装与通知授权、真实服务商多日日用观察、Windows 真机。
详见 [ROADMAP](docs/ROADMAP.md) 与 [PROGRESS](docs/PROGRESS.md#剩余外部门禁1-边界)。

## 仓库布局

```text
brand.toml              品牌唯一事实来源（产品名、bundle ID、CLI/二进制名、URI scheme、Keychain service）
assets/brand/           图标原图与 1024 px 渲染
scripts/sync-brand.sh   brand.toml → Rust / TypeScript / Tauri / Cargo [[bin]] / macOS 打包的确定性同步
crates/cordis/          Rust 内核：cordis 方法论移植（服务、事件、作用域、loader）
crates/maild/           守护进程库与 featherpostd / featherpost-agent / featherpostctl 二进制：
                        receive / store / filters / otp / rules / notify / scheduler / transport / api /
                        mime / folders / pending / compose / search / drafts / uploads / recovery / autodiscover
crates/maild-brand/     brand.toml 的 Rust 常量（由 sync-brand.sh 生成）
crates/maild-client/    CLI 与桌面端共用的 JSON-RPC client、传输发现和协议 DTO
crates/maild-paths/     平台目录与配置发现契约
crates/maild-app/       GUI 无关的连接 actor、视图状态与请求/通知协调
crates/maild-launcher/  顶层原生启动器：默认配置、打开默认前端、原生修复与后台管理窗口
crates/maild-ui/        默认 Tauri 前端壳（Studio，dev.featherpost.studio）
crates/maild-menu/      macOS 菜单栏 companion
packages/featherpost-studio/ 默认前端 webview（React + TypeScript + Vite，Vitest）
plugins/nodeseek-email/ 仓库内第三方 transport provider 插件示例
packaging/macos/        App bundle 构建、验证、Preview DMG、安装生命周期与签名/公证发布门禁
docs/                   文档索引、架构决策（ADR）、协议、运维、测试证据与路线图
```

## 文档

- [docs/README.md](docs/README.md) — 文档索引与写作规则
- [ARCHITECTURE.md](ARCHITECTURE.md) — 产品愿景、spine、事件协议、插件缝
- [docs/rust-port-design.md](docs/rust-port-design.md) — cordis → Rust 内核设计
- [docs/protocol/README.md](docs/protocol/README.md) — JSON-RPC v1、传输与 CLI 契约
- [docs/plugins/README.md](docs/plugins/README.md) — 外部插件 package v1
- [docs/operations/README.md](docs/operations/README.md) — 运行、备份恢复、发布与故障排查
- [docs/testing/README.md](docs/testing/README.md) — 真实服务、平台兼容与资源基准证据
- [docs/BRANDING.md](docs/BRANDING.md) — 品牌切换指南
- [docs/decisions/](docs/decisions/) — 架构决策记录，最新为
  [ADR-0049 退役原版界面、以 Studio 为唯一内置前端](docs/decisions/0049-retire-legacy-ui-studio-sole-builtin.md)

## 致谢

- [cordis](https://github.com/cordiverse/cordis) 与论文
  [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper)——内核方法论的来源。
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)——「everything is a plugin」在 agent
  领域的实践，是本项目插件化工程的参照样本。

## 许可证

Featherpost 以 **MIT OR Apache-2.0** 双许可证发布，你可以任选其一：

- MIT License（[LICENSE-MIT](LICENSE-MIT)）
- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE)）

除非你明确声明，否则你有意提交给本项目的任何贡献，按 Apache-2.0 许可证的定义，均以上述双许可证发布，
不附加任何额外条款或条件。
