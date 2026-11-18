# ADR-0019：M4-03 插件安装事务与生命周期管理

- 状态：已接受（设计冻结；实现归 M4-03）
- 日期：2026-08-22
- 适用范围：external/frontend 受管目录、`plugin_state.toml`、maildctl、新增插件管理 RPC；不改变既有扫描/监督语义的安全边界

> 2026-09-23 补遗：[ADR-0039](0039-native-recovery-and-tauri-default.md) 增加固定内置副本的原生修复例外：
> 在线仍走 Agent 串行事务；离线必须持有 Agent、前端目录和启用持久库时的数据库维护锁。
> 前端安装/移除/回滚/默认选择也必须持有目录占用服务，插件卸载后拒绝新写请求。常规升级的版本规则保持不变。

## 背景

M3.6 的安装流程是手工的：停用 Agent → no-clobber 复制包目录 → 手改 TOML allowlist →
重启。M4-01/02 交付了身份与授权，但安装仍是产品缺口：无事务性（半复制的包会被扫描器
计为损坏）、启停要编辑 TOML、升级没有回滚。本切片把安装/升级/回滚/移除/启停做成
CLI 一等操作，且**不需要停用 Agent**。

## 目标

1. `maildctl plugin install/upgrade/rollback/remove/list/enable/disable` 全流程不停服。
2. 事务纪律对齐 manage-app.sh：staging 校验后原子发布、no-clobber、可回滚、崩溃残留
   不阻塞。
3. 启用状态离开 TOML：Agent 管理的版本化状态文件，TOML allowlist 降级为一次性种子。

## 非目标

- 不做归档包格式（zip/tar）——v1 只接受「包含 manifest 的目录」；下载签名校验、
  marketplace 与网络分发继续 fail-closed 排除（受信目录永不直连网络下载）。
- frontend 不做 enable/disable（默认前端仍由配置决定）；install/upgrade/rollback/remove
  覆盖两类包，enable/disable 仅 supervised external。
- 不做多深度回滚链：每个 id 只保留上一版。

## 决策

### 1. 唯一写者模型

受管目录（`plugins/`、`frontends/`）与 `plugin_state.toml` 的**唯一写者是 Agent**；
maildctl 只做两件事：

- 把待装包复制到受管目录下的点前缀 staging 目录 `.incoming/<rand>/`（点前缀目录被
  扫描器显式跳过——这是本切片对扫描器的唯一修改）；
- 调 RPC 让 Agent 完成校验、rename 发布与重扫。

这延续 accounts.toml/plugin_auth.toml 的单写者纪律：不存在第二个进程与 Agent 竞争
写托管内容。CLI 对 staging 目录拥有完全写权（它就在同用户文件系统上），但 staging
永远不是「已安装」。

### 2. 新增 RPC（全部落 ADR-0018 表外默认 = UserOnly）

> **2026-09-11 更新（[ADR-0030](0030-frontend-authorization-closure.md) 决策 4）**：这五个方法
> 连同 `listExternalPlugins` / `openExternalPluginUi` 现归 `plugins_manage`，该词**只有 frontend
> manifest 能声明**——后台插件仍然装不了下一个插件（禁止自我繁殖）。用户可以把包管理界面委托
> 给自己装的前端；声明 `plugins_manage` 等于声明「我可以代表用户安装任意受管包」，安装界面因此
> 必须显示包来源与它将获得的 permissions。本节其余约束（Agent 是唯一写者、staging、回滚）不变。

```text
commitPluginInstall {kind, staging}   → 校验 .incoming/<staging> 内的包并发布
removePlugin        {kind, id}        → 包目录改名进 .removed/（子进程随之停止）
rollbackPlugin      {kind, id}        → 用 .rollback/ 里的上一版换回当前版
setPluginEnabled    {id, enabled}     → 写 plugin_state.toml 并即时 spawn/stop
refreshPlugins      {}                → 重扫 + 身份对账 + 差异化启停
```

- `staging` 参数只接受 `.incoming/` 的直接子目录名（拒绝路径分隔符与 `..`，
  否则 -32602）——Agent 绝不按客户端给的任意路径动手。
- 失败统一 `-32017 PLUGIN_TRANSACTION_FAILED`，message 带非秘密细节；staging 校验
  不过时 Agent 删除该 staging 目录（坏包不留现场）。
- 全部 additive；协议代数不晋升（理由同 ADR-0018 §5：User 行为不变，插件生态未存在）。

### 3. 事务语义

- **install**：`.incoming/<rand>` → manifest 校验（复用现有 loader 校验函数）→ id 在册？
  拒绝（no-clobber）：`plugins/<id>` 已存在即失败 → rename 发布 → refresh。
- **upgrade**：manifest 三段版本必须**严格高于**在册版本（数字逐段比较，否则拒绝）→
  当前版改名进 `.rollback/<id>/`（覆盖旧回滚，单深度）→ 新版 rename 发布 → refresh
  （见 §4 重启规则）→ 成功后删除 staging。发布与回滚保留之间崩溃：id 短暂缺失，
  下次 refresh 按 removed 处理（child 停止），`rollback` 可恢复。
- **rollback**：仅当 `.rollback/<id>/` 存在且其版本严格低于当前版；换回后 refresh。
- **remove**：目录改名进 `.removed/<id>-<rand>/` 后由 Agent 删除；身份条目随下次
  reconcile 收敛（ADR-0015 孤儿语义）。本地邮件数据不受影响（插件无用户数据）。
- **enable/disable**：写 `plugin_state.toml`（0600、原子发布、format_version=1、
  仅 Agent 写入——与 plugin_auth.toml 同纪律）后立即 spawn/stop 该 supervised 子进程；
  未装配或非 supervised → -32017/-32602。
- **legacy 种子**：首次以受管模式装配且状态文件不存在时，把 TOML
  `[plugins.external.config].enabled` 导入状态文件；此后 TOML 数组被忽略（文档声明）。
  显式 --config 开发态同样使用状态文件（与 M4-01 注册表「任何模式都装配」一致）。

### 4. refresh 差异化启停

重扫得到新目录后与在册记录 diff：

| 情形 | 动作 |
|---|---|
| 新出现的 enabled supervised | spawn |
| 消失的 id | stop 子进程 + 记录移除 |
| 同 id 但 manifest version 变化 | stop 旧 + spawn 新（升级生效） |
| 其余 | 不动（无重启风暴；ADR-0011 纪律） |

同时重跑身份缝 reconcile（新包即刻可 reveal/spawn）与 `listExternalPlugins` 快照更新。

### 5. 监督结构重构（使能动态启停）

`ExternalPluginManager` 从「启动时定死」改为内部可变：records 与运行中子进程表
（每 child 独立 watch 通道 + JoinHandle）都在锁内；LIFO teardown 反序停止全部。
对外 DTO 与既有观测字段不变。

### 6. 点前缀约定

扫描器显式跳过 `.` 开头的目录名（`.incoming/`、`.removed/`、`.rollback/` 及任何
未来事务残留）。崩溃残留的 staging 由下一次同类事务或人工清理；它们永远不会被
激活或计数。

## 兼容性

- 不使用新 RPC 的用户零变化；手改 TOML allowlist 在状态文件建立后被忽略是**行为变化**，
  文档在 operations/plugins 两处同步声明迁移路径（跑一次新 Agent 即自动导入）。
- 旧 daemon 无这些方法：新 maildctl 得到 -32601，CLI 报「内核过旧」引导升级（复用
  版本握手判定）。
- ADR-0018 的 fail-closed 默认让上述全部新方法自动 UserOnly——插件主体永远不能
  安装/启停插件（提权防线），无需额外登记。

## 测试策略（hermetic）

- 单测：版本三段比较、staging 名校验、状态文件读写矩阵（缺失=空、future 版本
  fail-closed、legacy 种子导入）、点前缀跳过。
- e2e（`tests/plugin_transactions.rs`）：不停服 install → reveal 可用 → enable 即
  running → upgrade（低版本拒绝、高版本成功且 child 重启用新代凭证）→ rollback 回
  旧版 → remove 后清单消失且身份条目收敛；崩溃残留 staging 不阻塞扫描。
- Windows：目录 rename 语义差异（目标存在即失败）在实现中以先移走再发布的顺序吸收；
  CI job 钉编译与基础行为。

## 后果

正：安装成为产品能力而非文档教程；升级可回滚；启停不再碰 TOML；Wave 2 业务插件的
分发前置就绪。

负：监督结构引入内部可变性（复杂度上移）；五个新 RPC 面需要维护；单深度回滚限制
要在文档里讲清楚。

## 参考

- ADR-0014 §10 / ADR-0017 / ADR-0018：插件生命周期、身份与授权前置
- ADR-0013 / `packaging/macos/manage-app.sh`：事务纪律的原型（no-clobber、严格更高
  版本、rollback/finalize 分离——v1 取其前半）
- ADR-0015 §2：accounts.toml 文件纪律（状态文件照抄）
