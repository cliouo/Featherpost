# ADR-0047：品牌与代码解耦（`brand.toml` 单一事实来源）与开源发布就绪

- 状态：已接受。B0（`brand.toml`、`maild-brand` crate、`scripts/sync-brand.sh`、Featherpost 图标、双许可证）已合入
  `f853c2f`；B3（macOS 打包脚本与 GitHub Actions 品牌参数化、开源门面文档）由本分支实现；B1/B2（Rust 运行时与 Tauri 前端
  改读 `maild-brand` 常量）在并行分支实现，B4 汇合后以真实二进制跑 `packaging/macos/dry-run.sh` 与 universal2 构建。
- 日期：2026-11-18
- 关联：[ADR-0011](0011-m3-5-macos-bundle-and-agent.md)、[ADR-0013](0013-m3-5-macos-release-lifecycle.md)、
  [ADR-0039](0039-native-recovery-and-tauri-default.md)、[ADR-0040](0040-release-gate-and-compatibility-matrix.md)、
  [ADR-0044](0044-macos-single-dock-identity.md)

## 背景

项目以工作代号 `maild` 起步，产品名、bundle 名、bundle ID、二进制名、数据目录、环境变量前缀、Keychain service、阅读器
URI scheme 散落在 Rust 源码、TypeScript、Tauri 配置、Cargo `[[bin]]`、五份 plist / 清单模板、十二个打包脚本与测试、两个
GitHub workflow 和全部文档里——仅 `packaging/macos` 就有约 680 行含写死的 `Maild` / `maild-*` / `com.maild.ui`。确定对外
品牌 **Featherpost** 时暴露了三个问题：

1. **改名不可审计**：任何一次手工全局替换都会漏掉某处（例如只改 plist 不改编译期 identity contract，签名后才在
   `SMAppService` 或通知授权上失败），而 ADR-0011 要求 bundle identity 在 Rust、plist 与打包门禁三处严格一致。
2. **开源门面缺失**：README 是内部交接文档；仓库引用本地未入库的 `reference/cordis/`、`reference/deepseek-harness/` 与
   `paper.pdf`，外部贡献者无法打开；`.cargo/config.toml` 把所有人绑定到 rsproxy 镜像；没有许可证。
3. **CI 与品牌耦合**：workflow 里写死 `Maild.app`、`Maild-preview.dmg`、`Maild-v*.zip` 与 `vars.MAILD_*`，换品牌必须同时
   改 CI。

## 决策

### 1. `brand.toml` 是唯一手写的品牌来源（B0）

仓库根 `brand.toml` 定义 20 个字段（`product_name`、`slug`、`env_prefix`、六个二进制名、四个 bundle 名、`icon_name`、
四个 bundle ID、`uri_scheme`、`keychain_service`）。`scripts/sync-brand.sh`（仅依赖 Python 3.11+ 标准库 `tomllib`）
先校验字段，再确定性生成 12 个派生文件：`crates/maild-brand/src/generated.rs`、`packages/maild-ui/src/brand.ts`、
`packaging/macos/brand.env`、三个 `*-bundle-id.txt`、`tauri.conf.json`（`productName` / `identifier` / CSP
`frame-src`）、开发期 `frontend.toml` 的 `id` / `name`，以及四个 crate `Cargo.toml` 中 `# BEGIN/END GENERATED BRAND BINS`
之间的 `[[bin]]`。写模式幂等，`--check` 只读并打印 diff，`--icons`（macOS）从 `assets/brand/icon-artwork.jpg` 重绘全套
图标。Cargo 包名与源码目录（`crates/maild*`）、编译期扫描前缀（`maild-agent-bundle-id:` / `maild-menu-bundle-id:` /
`maild-ui-embedded:`）**有意保持不变**：前者用户不可见，后者是二进制契约，只有跟在前缀后的 bundle ID 随品牌变化。

### 2. macOS 打包完全由 `brand.env` 驱动（B3）

- **品牌中立模板**：`templates/App.Info.plist.in`、`Agent.Info.plist.in`、`Menu.Info.plist.in`、`UI.Info.plist.in`、
  `frontend.toml.in` 只含 `__BRAND_<KEY>__` 占位符（产品名、四个二进制名、图标名、四个 bundle 名与 ID、两个版本号）。
  `build-bundle.sh` 以字面替换渲染（兼容 macOS `/bin/bash` 3.2 与 bash 5.2 的 `patsub_replacement`），plist 值做 XML
  转义；渲染后残留任何 `__BRAND_*__` / `__MAILD_*__` 即失败。顶层 App、Agent、Menu 继续固定 `LSUIElement = true`
  （ADR-0045）。`frontend.toml.in` 严格保持 FrontendManifest schema（不新增字段），只参数化 `id`、`name`、`version` 与
  `[launch] path`，与 B0 生成的开发期清单逐键对齐，供 recovery 单测比较。
- **图标**：删除 `packaging/macos/Maild.icns`，改用 B0 落位的 `packaging/macos/assets/AppIcon.icns`，复制为
  `Contents/Resources/${BRAND_ICON_NAME}.icns`。
- **全部脚本与测试**（`build-bundle`、`verify-bundle`、`seal-recovery-payload`、`build-preview-dmg`、`build-universal`、
  `release-bundle`、`verify-release`、`manage-app`、`dry-run` 与三套测试）启动时执行 `. brand.env`；bundle 名、二进制名、
  `frontends/${BRAND_UI_BUNDLE_ID}`、顶层 UI 显式签名 `--identifier "$BRAND_UI_BUNDLE_ID"`、`Application Support/${BRAND_SLUG}`、
  回滚 / 事务目录名与发布产物名都来自变量。
- **identity 双重来源的一致性**：生成器与验证器以 `brand.env` 为准，并要求三个 `*-bundle-id.txt`（Rust 编译期包含）与之
  逐字相同，否则提示重新运行 `sync-brand.sh`；拒绝 `${BRAND_ENV_PREFIX}_{APP,AGENT,MENU,UI}_BUNDLE_ID` 与旧
  `MAILD_*_BUNDLE_ID` 环境覆盖（ADR-0011 的 fail-closed 语义不变）。
- **兼容回退**：版本变量读 `${BRAND_ENV_PREFIX}_BUNDLE_SHORT_VERSION` / `_BUILD_VERSION`，缺省时回退旧
  `MAILD_BUNDLE_*`（品牌前缀优先）。`manage-app.sh` 读 `${BRAND_ENV_PREFIX}_MAINTENANCE_LOCK_FD` / `_VALIDATOR`，
  也接受改名前 Agent 设置的 `MAILD_MAINTENANCE_*`；两种来源都必须经 `maintenance-validate-fd` 证明锁仍被持有，且
  validator 必须是被验证的已安装 Agent，因此回退不放宽信任边界。`build-universal.sh` 同时导出 `MAILD_BUILD_REV`（当前
  crate 读取）与 `${BRAND_ENV_PREFIX}_BUILD_REV`。
- **开发身份门禁**：provisional 判定从写死的 `dev.maild.*` 改为 `dev.${BRAND_SLUG}.*`（并继续拒绝 `dev.maild.*`）。
  仓库默认 `dev.featherpost.*` 因此仍不可正式发布，与 ADR-0013 一致。

### 3. GitHub Actions 与品牌解耦（B3）

- `ci.yml` 新增 `brand` job（ubuntu + Python 3.12）运行 `bash scripts/sync-brand.sh --check`；macOS 打包步骤 source
  `brand.env` 组装 `${BRAND_APP_BUNDLE}` 与 `${BRAND_PRODUCT_NAME}-preview.dmg`。
- `release-macos.yml` 把 `BRAND_PRODUCT_NAME` / `BRAND_SLUG` / `BRAND_APP_BUNDLE` 写入 `GITHUB_ENV`，artifact 名与路径
  用 `${{ env.BRAND_PRODUCT_NAME }}`；provisional 门禁从 `brand.env` 取 slug；签名相关仓库变量改为品牌中立的
  `MACOS_SIGNING_IDENTITY` / `MACOS_NOTARY_KEY_ID` / `MACOS_NOTARY_ISSUER_ID`（secrets 本就是 `MACOS_*`），临时文件名去掉
  品牌前缀。发布 workflow 继续只用 SHA 固定的 action；python 版本不确定的 macOS runner 上不跑 `sync-brand.sh --check`，
  品牌漂移由同一次 push 触发的 `ci.yml` `brand` job 拦截，打包侧另有 `*-bundle-id.txt` 一致性门禁兜底。

### 4. 开源就绪（B0 + B3）

- 双许可证 **MIT OR Apache-2.0**（`LICENSE-MIT`、`LICENSE-APACHE`、workspace 与每个 crate 的 `license` 字段）。
- 删除 `paper.pdf`、`vendor/cordis`、`packages/spike` 与 rsproxy 专用 `.cargo/config.toml`；镜像改为用户级可选配置，
  写入[运维手册](../operations/README.md#可选国内-cratesio-镜像)。
- 根 `README.md` 重写为开源首页（中文主叙事 + 英文一句话定位，资源数字均引用 ADR-0041 的可复现基准）；新增
  [`docs/BRANDING.md`](../BRANDING.md) 品牌切换指南。
- `ARCHITECTURE.md` 与现行文档中指向本地 `reference/cordis/`、`reference/deepseek-harness/`、`paper.pdf`、`vendor/cordis`
  的路径改为上游公开链接：[cordiverse/cordis](https://github.com/cordiverse/cordis)、
  [cordiverse/paper](https://github.com/cordiverse/paper)、
  [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)。
- ADR-0001–0046 作为历史档案原样保留，`docs/README.md` 注明它们使用早期工作代号 `maild`。

## 后果

- 换品牌 = 改 `brand.toml`（+ 可选图标原图）→ `bash scripts/sync-brand.sh` → 重新编译；打包脚本、测试与 CI 零改动。
  `test_bundle.sh` 的换牌夹具（另一份 `brand.env`，产品名 `Quill & Ink`，含空格与 `&`）持续证明这一点。
- 公开发布之后，`slug`、bundle ID、`keychain_service`、`env_prefix` 成为用户数据与系统登记的地址：再改等于发布新 App
  （数据目录、登录项、通知授权、钥匙串条目都不迁移）。`docs/BRANDING.md` 明确列出这些代价。
- 已配置过旧 `vars.MAILD_*` 的仓库需要把三个 variables 改名为 `MACOS_*`。
- 运行时代码（B1/B2）合入前，仓库内真实二进制仍可能读旧环境变量名；打包脚本的 `MAILD_*` 回退保证这一窗口内本地与 CI
  构建不中断。
- 历史 ADR 与部分运维文档仍以 `maild` 书写命令；`docs/operations/README.md` 顶部给出名称对照，后续触及相关章节时再逐步
  改写，不做一次性全文替换。

## 验证

```sh
bash scripts/sync-brand.sh --check
bash -n packaging/macos/*.sh packaging/macos/tests/*.sh
packaging/macos/tests/test_bundle.sh     # 含换牌夹具、identity 漂移、旧版本变量回退与优先级
packaging/macos/tests/test_release.sh    # provisional dev.<slug>.* 门禁、冻结 identity 后的签名链与 Team ID 门禁
packaging/macos/tests/test_lifecycle.sh  # <ENV_PREFIX>_MAINTENANCE_* 与旧 MAILD_* 伪造 validator 均被拒绝
```

B3 分支上述门禁全部通过；`dry-run.sh` 以 C 夹具运行，真实二进制的 universal2 构建在 B4 汇合 B1/B2 后执行。
