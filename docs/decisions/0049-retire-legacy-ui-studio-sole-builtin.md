# ADR-0049: 退役原版内置界面，以 Featherpost Studio 作为唯一内置前端

- **状态**：已接受（2026-11）
- **关联**：[ADR-0024](0024-builtin-frontend-tauri-migration.md)、[ADR-0031](0031-tauri-frontend-shell-and-contribution-registry.md)、[ADR-0039](0039-native-recovery-and-tauri-default.md)、[ADR-0040](0040-release-gate-and-compatibility-matrix.md)、[ADR-0044](0044-macos-single-dock-identity.md)、[ADR-0047](0047-decoupled-brand-and-open-source-readiness.md)、[ADR-0048](0048-locale-preference-and-decoupled-i18n.md)

## 背景

在 Featherpost Studio（`dev.featherpost.studio`）演进为默认前端并接入单 Dock 就地承载（ADR-0044）之后，仓库与 macOS 安装包仍同时构建、嵌入、签名并验证两份完整的 Tauri 二进制（原版 `dev.featherpost.ui` 与 `dev.featherpost.studio`）：

1. **双倍体积与构建成本**：`Featherpost.app` 同时携带两份 universal2 Tauri 可执行文件（单宿主副本 + 恢复副本共 4 份 Mach-O），`build-universal.sh` 需编译两遍 `maild-ui`，签名与验证链路需对两套 payload 与 host 二进制重复执行。
2. **双轨心智与漂移风险**：Rust 侧存在 `#[cfg(feature = "studio")]`、`STUDIO_FRONTEND_ID`、`LEGACY_PAYLOAD` / `STUDIO_PAYLOAD`、`tauri.conf.json` / `tauri.studio.conf.json` 等双份配置与分支；前端存在已无人使用的原版邮件视图、设置壳与数百条孤立 i18n 文案。
3. **版本对齐陷阱**：此前随包与独立分发的 Studio 前端清单版本为 `0.3.4`，而 workspace Rust crate 仍停留在 `0.1.0`。若直接把 `dev.featherpost.studio` 合并为唯一内置前端但保持 `0.1.0`，现有用户的 `0.3.4` 安装会被 `RefreshOutcome::KeptNewer` 视为“更新版本”而拒绝升级，且发布门禁 `release-bundle.sh` 要求四个 crate、四份 plist、前端清单与 Git tag 版本完全一致。

同时，Featherpost 的核心架构承诺——**第三方可插拔前端体系**（`FrontendCatalog`、`IdentitySeam`、权限与 l10n schema、原生修复窗口 `builtin-repair`、独立版本的非就地回退）——必须原封不动地保留。

## 决策

### 1. `dev.featherpost.studio` 成为唯一内置前端，由 `brand.toml` 统一驱动

- 将仓库根 `brand.toml` 的 UI 身份字段更新为：
  - `ui_bin = "featherpost-studio"`
  - `ui_bundle_name = "Featherpost Studio.app"`
  - `ui_bundle_id = "dev.featherpost.studio"`
- 通过 `scripts/sync-brand.sh` 将该身份同步到 `maild-brand`、前端 `brand.ts`、`packaging/macos/brand.env`、`crates/maild-ui/Cargo.toml`、`crates/maild-ui/tauri.conf.json` 与 `crates/maild-ui/package/frontend.toml`。
- 将 `crates/maild-ui/tauri.studio.conf.json` 合并进 `crates/maild-ui/tauri.conf.json` 并删除前者；移除 `crates/maild-ui/Cargo.toml` 的 `studio` feature 及 Rust 源码中的全部 `#[cfg(feature = "studio")]` 分支。
- `crates/maild`、`crates/maild-client`、`crates/maild-launcher` 收敛为单一 `BUNDLED_FRONTEND_ID`（`dev.featherpost.studio`）、单一恢复载荷 `BUNDLED_PAYLOAD` 与单一 `ui_executable()`，删除 `STUDIO_FRONTEND_ID`、`STUDIO_BIN`、`LEGACY_PAYLOAD`/`STUDIO_PAYLOAD` 双轨结构。
- 原生修复窗口（`builtin-repair`）的恢复目标自动收敛为唯一内置前端 `BUNDLED_FRONTEND_ID`；`resyncBuiltinFrontend` IPC 返回字段仍保留 `id`、`ids`、`replaced`、`replaced_ids` 以保持协议向后兼容。

### 2. Workspace 版本统一升至 `0.4.0`

- 将 workspace 内 11 个 Rust crate 与前端 workspace 包版本统一升级至 `0.4.0`（第三方示例插件 `plugins/nodeseek-email` 保持独立版本号 `0.1.6`），macOS 打包脚本默认 `short_version` 同步更新为 `0.4.0`。
- 升级后 `0.4.0 > 0.3.4`，已有 Studio 安装通过 `RefreshOutcome::Upgraded` 无缝原地升级，且完全满足 `release-bundle.sh` 的版本一致性检查。

### 3. 原版 `dev.featherpost.ui` 用户的平滑、无损迁移

沿用 ADR-0039 退役 `builtin-egui` 的“只读迁移、不静默删文件”原则：

- 在 `crates/maild-client/src/lib.rs` 新增 `pub const LEGACY_UI_FRONTEND: &str = "dev.featherpost.ui";`，并将其纳入 `is_builtin_frontend(id)`（与 `BUNDLED_FRONTEND_ID`、`REPAIR_FRONTEND_ID`、`LEGACY_EGUI_FRONTEND` 并列）。
- **读取时迁移默认前端**：`maild::frontend::migrate_default` 与 `maild_client::native_recovery::migrate_legacy_default` 在读取 `.default.json` 或 `featherpost.toml` 时，将 `dev.featherpost.ui`（与 `builtin-egui` 一样）映射为 `DEFAULT_FRONTEND`（`dev.featherpost.studio`），不主动改写用户手动编辑过的配置文件。
- **未修改的首启模板自动迁移**：`maild_launcher::bootstrap` 若检测到用户配置与旧版自动生成的 `dev.featherpost.ui` 模板逐字节一致且无持久 `.default.json`，仍直接升级为当前 `dev.featherpost.studio` 默认配置。
- **磁盘残留不静默删除**：`FrontendCatalog::load` 跳过受管目录下残留的 `frontends/dev.featherpost.ui/`，既不将其列为可用第三方前端，也不在启动时静默删除用户磁盘目录；`setDefaultFrontend` 与 `claim` 拒绝已退役的 id；显式调用 `openFrontend("dev.featherpost.ui")` 时路由到原生修复窗口，与 `builtin-egui` 一致。
- **单实例锁过渡探测**：`maild-launcher` 与 `maild-menu` 保留对旧独立版 Studio 锁文件 `runtime_dir/studio/.main-ui.lock` 的只读探测，以便升级过渡期内仍能定位并平滑退出旧进程。

### 4. macOS 打包与 CI 收敛为单前端载荷

- 删除独立 Studio 打包与双轨辅助脚本：`packaging/macos/build-studio-binary.sh`、`packaging/macos/studio-payload.sh`、`packaging/macos/build-studio.sh`、`packaging/macos/studio/`（含 `.command` 脚本与独立 plist）及 `.github/workflows/studio-macos.yml`。
- `packaging/macos/templates/UI.Info.plist.in` 增加 `__BRAND_UI_DISPLAY_NAME__`（取 `${BRAND_UI_BUNDLE%.app}`）与 `CFBundleIconFile = __BRAND_ICON_NAME__`，`build-bundle.sh` 与 `verify-bundle.sh` 同步为恢复副本安装并校验 `${BRAND_ICON_NAME}.icns`。
- `build-universal.sh`、`build-bundle.sh`、`seal-recovery-payload.sh`、`verify-bundle.sh`、`build-preview-dmg.sh`、`release-bundle.sh`、`verify-release.sh` 与 `dry-run.sh` 统一只处理一套内置前端（顶层单宿主 `Contents/MacOS/$BRAND_UI_BIN` + 恢复载荷 `Contents/Resources/frontends/$BRAND_UI_BUNDLE_ID/`），打包回归测试继续验证退役 `dev.featherpost.ui` 二进制无法冒充内置前端。
- 更新 `studio_notice_identity_failure` 文案，引导用户通过原生修复与后台管理入口恢复前端包，不再提及已删除的 `安装 Studio.command`。

### 5. 前端包合并为单一 `packages/featherpost-studio`

- 提取旧设置页 `Accounts.tsx` 中被共享表单复用的 `Field` 组件至独立 `Field.tsx`，删除全部仅服务于原版 UI 的 18 个源文件（约 5,500 行）、对应废弃测试以及未被引用的原版 i18n 词条。
- 将所有被 Studio 复用的共享模块（`bridge/`、`host/`、`compose/`、`mailbox/`、`settings/`、`app/`、`styles/`、`brand.ts`、`i18n.ts`、`preferences.ts`）与 Vitest 测试并入 `packages/featherpost-studio/`，删除 `packages/maild-ui/`。
- `scripts/sync-brand.sh`、`pnpm-workspace.yaml`、根目录 `package.json` 及 CI workflow 全部指向单一前端包 `packages/featherpost-studio`。

## 保持不变的架构边界

- **第三方前端架构**：`FrontendCatalog`、`frontend.toml` schema v2、权限声明与 l10n 校验、外部进程隔离与第三方前端切换全部保留。
- **单宿主与版本回退**：若用户在受管目录安装了不同版本的 `dev.featherpost.studio`（`DifferentVersion`）或未登记，启动器仍通过 `InPlace::Unavailable` 回退为独立启动，不强行以顶层二进制冒名顶替。
- **品牌单源**：`brand.toml` + `scripts/sync-brand.sh --check` 仍是全仓库唯一品牌事实来源，换牌测试无需改任何脚本即可通过。

## 后果

- **收益**：macOS 安装包少打一份 universal2 Tauri 二进制（包体积与 `build-universal.sh` 编译/签名耗时显著下降）；Rust 侧消除全部双内置前端条件编译与分叉逻辑；前端精简为单一包、无跨包相对路径导入与死代码。
- **代价与约束**：已显式选择 `dev.featherpost.ui` 的旧用户升级后将自动切至 Studio，原版界面不再随包提供；第三方前端仍可通过受管前端包机制独立安装与切换。
