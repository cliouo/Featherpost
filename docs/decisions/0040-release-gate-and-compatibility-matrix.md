# ADR-0040：M4.5-10 发布门禁扩展与兼容降级矩阵

- 状态：已接受；仓库内门禁与兼容矩阵已实现并验收（2026-11-12）；正式凭据公证、Windows 真机与长期日用留外部门禁
- 日期：2026-11-12
- 关联：[ADR-0013](0013-m3-5-macos-release-lifecycle.md)、[ADR-0016](0016-m3-7-agent-version-handshake.md)、[ADR-0024](0024-builtin-frontend-tauri-migration.md)、[ADR-0039](0039-native-recovery-and-tauri-default.md)

## 背景

09-C 删除 `maild-desktop`（egui）并将默认前端切为随包预装的 `com.maild.ui`（Tauri）后，发布与升级路径还剩四处闭环缺口：

1. **Tauri 界面的协议代握手缺失**：ADR-0016 的 `getAgentInfo` 版本握手与写操作闸门原本实现在已删除的 egui 壳内；覆盖安装后若旧 `maild-agent` 仍持有受管 socket，或回退应用后新旧组件协议代错位，新 Tauri 界面会直接发出写请求并遭遇费解错误。
2. **跨协议代的内置前端自动升级**：`BundledFrontend::refresh` 原本复用要求 `protocol_version == API_PROTOCOL_VERSION` 的严格清单检查读取已装版本；一旦未来版本晋升协议代，旧版 `com.maild.ui` 会被当作不可读包跳过自动升级，导致正常升级反而失去默认前端。
3. **嵌套 `Maild UI.app` 的签名与 staple 边界**：ADR-0024 早期草案按并列目录设想“独立 staple”，而 ADR-0039 定稿把恢复副本放在 `Maild.app/Contents/Resources/frontends/com.maild.ui/Maild UI.app`。macOS `codesign` 会把 `Contents/Resources/` 下的全部文件封入顶层 `CodeResources`；若在顶层签名后向嵌套 UI 写入票据文件，会直接破坏顶层签名封条。
4. **CI 与 Windows 编译门禁**：`maild-ui` 缺少 `icons/icon.ico` 且两处 Unix 目录权限分支在 Windows 目标下触发 `unused_mut`，导致全工作区 Windows 交叉 Clippy（`-D warnings`）与 CI 无法通过；`ci.yml` 与 `release-macos.yml` 亦需补齐 webview 测试、`pnpm` 准备和 `menu-bundle-id.txt` 预检。

## 决策

### 1. Tauri 界面的协议代握手与只读降级

- **单源判定与单调推送**：`maild-ui` 的 `Bridge` 在每个在线 `connection_epoch` 执行一次 `getAgentInfo` 握手（`CompatVerdict` 映射为 `unknown / compatible / kernel_outdated / app_outdated`）。握手期间若连接断开或进入新 epoch，旧代结果直接丢弃；新 epoch 已在线则紧接补握。握手结论变化经单一状态泵补推快照，webview 侧按 `(connection_epoch, revision, compat)` 单调接收，且首读快照绝不覆盖已到达的推送帧。
- **双端闸门**：不一致时工作区顶部常驻告警横幅、侧栏状态显示“版本不一致，只读”，宿主禁用所有依赖在线写能力的命令贡献；Rust 侧在 `compose_call`、`management_call`、`set_reading_policy` 与包安装目录暂存入口统一过 `Bridge::guard`（只放行 `get*`/`list*`/`search*`/`whoami` 只读方法，未知新方法默认按写操作拦截，返回 `UiErrorCode::Incompatible`），避免向旧后台发写请求或在前端目录留下 `.incoming` 暂存副本。
- **不越权发信号，保留原生修复出口**：与旧 egui 壳不同，Tauri 界面以非特权主体 `frontend:com.maild.ui` 运行（ADR-0030），不在前端内按 PID 发信号杀进程；设置页「修复与后台管理 → 打开修复窗口」（`openFrontend` 指向 `builtin-repair`，仅 `background`）豁免写闸门，用户始终可从菜单栏或该按钮进入原生修复窗口重启后台或重装内置界面。

### 2. 预装、升级、回退与目录权限矩阵

| 场景 | 行为与保证 |
|---|---|
| 首次启动 | Agent 在 `frontends/.owner.lock` 下执行一次性 no-clobber 预装，先落 `.preinstalled-com.maild.ui` 标记再发布包，不写 `.default.json` |
| 用户手动移除 `com.maild.ui` 后重启或升级到更高版本 | 一次性标记已存在且目标目录缺席，`preinstall` 与 `refresh` 均跳过，**永不自动装回**；默认路由回落 `builtin-repair` |
| 同协议代或跨协议代升级（已装 `protocol_version` 属于 `1..=API_PROTOCOL_VERSION` 且数字版本更旧） | `refresh` 先校验目录属主与结构，接受历史协议代读取已装版本，自动替换为随包新版并将旧包移入 `.repair-backups/upgrade-*`，用户默认前端选择保持不变 |
| 应用回退（已装版本更新，或已装 `protocol_version > API_PROTOCOL_VERSION`） | `refresh` 保持不动，绝不静默降级；若协议代高于当前后台，目录扫描跳过该包并回落 `builtin-repair`（后台收信与通知继续运行），用户在原生修复窗口点「重装内置界面」即可显式恢复匹配版本并备份旧包 |
| 旧配置（`default = "builtin-egui"`、旧 `.default.json`） | 读取时映射为 `com.maild.ui`，不改写用户 TOML；显式请求 `builtin-egui` 作为 `builtin-repair` 别名处理 |
| 运行时目录权限被放宽或含符号链接 | `FrontendCatalog`、`load_package_at`、`resolve_launch_path` 与恢复事务统一执行非符号链接、当前 UID 拥有、`mode & 0o022 == 0` 校验；**预装包不享有任何豁免**，违规时跳过该包并回落原生修复 |

### 3. 双 `.app` 签名/公证/staple 与跨平台门禁

- **嵌套恢复副本的签名与公证顺序**：沿用 ADR-0013 的单归档公证流程，固定顺序为：先签 `Contents/Resources/frontends/com.maild.ui/Maild UI.app` → `seal-recovery-payload.sh` 重算签名后 `maild-ui` 的 `recovery.sha256` → 签内嵌 `Maild Agent.app` 与 `Maild Menu.app` → 签顶层 `Maild.app` → 提交单份 ZIP 等待 Apple Notary `Accepted` → 对顶层 `Maild.app` 执行 `stapler staple`、`stapler validate`、`spctl` 与解包复验。**不对 `Contents/Resources` 内的嵌套 `Maild UI.app` 追加独立 `stapler staple`**：实测向已封条的顶层 `Contents/Resources` 子树新增 `Maild UI.app/Contents/CodeResources` 会使 `codesign --verify --deep --strict=all Maild.app` 立即失效；单次公证票据已包含归档内全部 Mach-O 的 CDHash，且 Agent 预装复制时不携带外部 quarantine 扩展属性。
- **工作流与 Windows 门禁**：`release-macos.yml` 在读取凭据前一并拒绝 `menu-bundle-id.txt` 的 `dev.maild.*` 临时身份；`ci.yml` 全工作区启用 `--locked`，在 macOS 跑 `packages/maild-ui` 的 `tsc --noEmit` 与 Vitest 后再执行 universal2 打包验证；`crates/maild-ui` 补齐 `icons/icon.ico` 并消除 Windows 目标的 `unused_mut` 警告，使 `x86_64-pc-windows-gnu` 全工作区 Clippy 零警告通过。

## 外部门禁（诚实边界）

以下项目依赖外部凭据、目标硬件或真实用户会话，不在仓库自动化中冒充已通过：

- 冻结正式 App/Agent/Menu bundle IDs 与 `team-id.txt`，注入 Developer ID 与 Notary API 凭据实跑 `release-macos.yml`，保存 `Accepted` 公证记录并在干净 macOS 13+ 账户完成 quarantine/离线 Gatekeeper、登录项批准保留、通知归属与升级/回滚/卸载演练；
- 在真实登录会话（非 `/private/tmp` 隔离 `HOME`）下验收 LaunchServices 图形激活闭环：正常打开 Tauri 默认前端、人为损坏已装 UI 后自动落入原生修复窗口并一键恢复、覆盖安装触发版本横幅并经修复窗口重启恢复；
- Windows 真机运行验收（named pipe 跨用户 ACL、WebView2 渲染、窗口资源占用）以及面向后续 Windows 前端激活清单扩展的独立切片；
- 真实邮箱服务商账户下的多日连续收发、休眠唤醒重连与资源观察。
