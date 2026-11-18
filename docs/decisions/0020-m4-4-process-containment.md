# ADR-0020：M4-04 supervised 插件进程 containment

- 状态：已接受（设计冻结；实现归 M4-04）
- 日期：2026-08-23
- 适用范围：`external.rs` supervised 子进程 spawn/停止路径、`RunningChild`、清单观测字段；不改变 ADR-0017/0018/0019 的身份、授权与安装事务语义

## 背景

当前 `supervise` 的停止路径只对**直接子进程** `start_kill()`（Unix 上是 SIGKILL 单
pid）。supervised 插件自己 fork 出的 helper、worker 或 double-fork 的守护进程在
Agent 停机、插件 disable、升级重启、remove 时全部成为孤儿继续运行——它们仍持有旧
`MAILD_PLUGIN_TOKEN` 之外的一切资源（打开的文件、网络连接、邮件数据副本）。插件文档
早已把「必须保持前台、不得 daemonize」写成契约，但没有任何机制让违约可观测。M4-01
的凭证轮换让逃逸进程的**身份**失效，M4-04 要让逃逸的**进程**失效。

## 目标

1. supervised 子进程的**全部后代**（未主动脱离进程组的）在停止、disable、升级重启、
   remove 与 Agent LIFO teardown 时一并终止。
2. Windows 上 containment 完备：后代无法脱离 Job Object。
3. 违约可观测：任何退出路径（含正常 `exit 0`）都做组内清扫；清扫后组内仍有成员
   （病态残留）→ 清单状态 `leaked` + stderr 告警，而不是静默。
4. 不引入重启风暴：差异化刷新（ADR-0019 §4）语义不变；停止预算仍受
   `MAX_SHUTDOWN_WAIT` 约束。

## 非目标

1. 阻止 setsid/双重 fork 主动逃逸——那是同用户恶意进程防线，归 M5 沙箱；本切片只做
   **检测与报告**（macOS 无 /proc，跨平台扫描进程参数不可靠且高成本）。
2. on_demand frontend（LaunchServices 激活的 .app）的 containment——它们不是 Agent
   子进程。
3. per-plugin 可配置停止宽限期——沿用全局 2s 预算。
4. Linux cgroup——产品目标平台是 macOS/Windows。

## 决策

### 1. Unix：子进程即进程组长，killpg 阶梯升级

spawn 时用 `Command::process_group(0)`（std 1.64+ 稳定 API，tokio Command 经
`as_std_mut` 透传）让子进程成为新进程组的组长，pgid = 子 pid。停止阶梯：

1. `killpg(pgid, SIGTERM)` → 等待直接子进程退出（复用现有 2s 总预算）；
2. 超时 → `killpg(pgid, SIGKILL)` → reap；
3. 收尾探测 `killpg(pgid, 0)`：`ESRCH` = 组空，正常 `stopped`；仍有成员（如卡在
   不可中断 IO 的进程，允许 3×100ms 重试）→ 状态 `leaked` + stderr 告警。

组员身份不因父进程死亡而消失，所以 double-fork 但**未 setsid** 的后代同样被覆盖；
孤儿由 launchd/init 收割，不产生长期 zombie。诚实边界：**setsid 主动逃逸者已离开
进程组，对 killpg 与组空探测都不可见**——本切片把它从「静默存活」变成「契约 + 组内
containment + 残余风险文档化」，真正的不可逃逸隔离归 M5 沙箱。

### 2. Windows：Job Object kill-on-close

spawn 时 `CreateJobObjectW` + `SetInformationJobObject(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)`
+ `AssignProcessToJobObject(子进程句柄)`；job 句柄存入 `RunningChild`。停止 =
`TerminateJobObject` + drop job 句柄（双保险）。Windows 后代**无法**脱离 job
（无对应 API），containment 完备，探测语义天然满足。需要给 windows-sys 增加
`Win32_System_JobObjects` feature；unsafe 仅限这一条路径，集中在一个
`#[cfg(windows)]` 模块内。

### 3. 停止路径统一化

`RunningChild` 增加平台 containment 句柄（Unix pgid / Windows job）。所有退出路径都
走同一个收尾函数：外部停止信号、直接子进程非零退出、**以及正常 `exit 0`**——契约
要求前台运行，父进程退出却留下组内后台成员即属违约，一律 SIGKILL 清扫并告警。
`set_enabled` 的 detach 与 `shutdown` 的 LIFO 沿既有 stop 信号通道，因此自动获得同一
行为；`MAX_SHUTDOWN_WAIT` 语义不变（LIFO 总预算，任务 abort 兜底保留）。

### 4. 观测：新增终态字符串 `leaked`

`listExternalPlugins` 的 `state` 字段新增 `leaked`（停止流程结束后组内仍有存活
成员）。协议文档已规定客户端必须原样保留未知状态字符串，additive 无协议代晋升。
stderr 同步输出一行有界告警（含插件 id 与 pgid）。

### 5. 备选方案对照

| 轴 | 选项 | 取舍 |
|---|---|---|
| Unix containment | (a) process_group + killpg 阶梯 ✅ | std 稳定 API、零 unsafe；覆盖一切未 setsid 后代（含孤儿） |
| | (b) 扫描进程表找后代 | macOS/Windows API 分叉、PID 复用竞态、高成本 |
| | (c) cgroup | Linux-only，出产品范围 |
| Windows containment | (a) Job Object kill-on-close ✅ | 后代无法脱离，语义完备；一条集中 unsafe 路径 |
| | (b) `taskkill /T` | 外部进程、locale 依赖、竞态 |
| 逃逸防线 | (a) 停止后组空探测 → `leaked` ✅ | 可移植、诚实；把违约从静默变信号 |
| | (b) spawn 时阻止 daemonize | 不可移植、先验不可判定 |
| | (c) 只靠文档契约 | 即现状，违约不可观测——被本切片否定 |
| 宽限期 | (a) 沿用全局 2s ✅ | 不加配置面 |
| 组长身份 | (a) 子进程自为组长 ✅ | pgid=pid 精确；子秒退则 ESRCH 自然收敛 |
| 范围 | (a) 仅 supervised external ✅ | on_demand .app 不是 Agent 子进程 |

## 兼容性

- 无新方法、无新配置、无协议代晋升；`state` 新值 `leaked` 为 additive。
- 对守约插件（前台运行、正常退出）行为不变：SIGTERM 后正常退出即 `stopped`。
- 对违约插件：从「后代静默存活」变为「后代被终止」或「`leaked` 可见」。
- `plugin.toml` manifest 无新字段。

## 测试策略（hermetic）

1. **孤儿覆盖**（Unix e2e）：supervised 脚本 spawn 一个后台 `sleep` 型 grandchild
   （心跳写 marker），父进程随即正常退出；disable 后 grandchild 必须死亡（marker
   停止增长 / 进程消失）——证明 killpg 覆盖孤儿后代。
2. **TERM 忽略 + 升级**（Unix e2e）：父进程 `trap '' TERM` 并 spawn grandchild；
   停止路径升级 SIGKILL，父子全部在预算内死亡，终态 `stopped`。
3. **组空探测单测**：对已死进程组 probe 返回 clean；探测函数的 ESRCH/重试语义单测。
4. **Windows**：编译门（host + windows-gnu clippy）；Job Object 行为按惯例归真机/CI
   job 证据。
5. 既有 `tests/plugin_identity.rs`（spawn/轮换/停摆）与
   `tests/plugin_transactions.rs`（升级重启/移除吊销）必须原样通过——containment
   不得破坏 ADR-0017/0019 语义。

## 后果

- unsafe 面收窄到 Windows Job Object 一个模块；Unix 全安全 API。
- `RunningChild` 携带平台句柄，`external.rs` 停止路径收敛为单一 `terminate`。
- setsid 主动逃逸仍是文档化残余风险（`leaked` 使其可观测），最终解在 M5 沙箱——
  与「薄内核 + 契约先行、沙箱后置」的架构愿景一致：先把违约变成信号，再把信号
  变成不可能。

## 参考

- ADR-0017（凭证轮换使逃逸进程身份失效）、ADR-0019（差异化刷新与停止预算）
- `std::process::Command::process_group`（1.64+）、`killpg(2)`、
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
