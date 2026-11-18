# ADR-0016：内核版本握手与升级一致性

- 状态：已接受，M3.7-04 已实现（覆盖安装升级演练的真机证据待补）
- 日期：2026-08-22（事故发生于 2026-08-21 M3.7 外部验收）
- 日期：2026-08-22（事故发生于 2026-08-21 M3.7 外部验收）
- 适用范围：maild RPC 协议、maild-client、桌面壳、maildctl；Windows 重启 UX 延后

## 背景

2026-08-21 M3.7 Preview DMG 全点击验收中，点击「添加」得到
`操作失败：method not found: listAccountProfiles (-32601)`。根因不是包内容：中午安装的旧版
`maild-agent` 在晚间覆盖安装后仍然存活，继续持有受管 socket（`run/maild.sock`）；新桌面壳连上
旧守护进程，而 `listAccountProfiles` 是 M3.7 新增方法。人工恢复需要 vnode 级取证（对比 `lsof`
txt NODE 与磁盘 inode）后 kill 旧进程再重启——这不是用户能完成的操作。

结构性原因：

1. ADR-0011 有意分离 GUI 与 daemon 生命周期：升级只替换磁盘文件，Login Item 进程不受影响，
   可以无限期以旧代码服务新前端。
2. 受管 socket 是复用的单端点，客户端连接时不了解对端身份。
3. `status` 虽返回 `version`，但 preview 期 `CARGO_PKG_VERSION` 恒为 0.1.0，且无人在客户端侧
   强制比对。
4. 既有防线都不覆盖此形态：catalog `format_version` fail-closed 保护的是数据层（旧 daemon 读到
   新格式目录会拒绝服务而不是写坏数据）；stale socket 回收只在新 agent 自己启动时生效；
   SMAppService 注册状态与运行中进程的实际二进制无关。

故障类别比 -32601 更宽：同名方法的参数或语义漂移会产生费解错误甚至静默错误行为；反向错位
（新内核 + 旧应用）同理。每次覆盖安装都会复现，必须在产品内闭环。

## 目标

1. 客户端在连接后一次往返内可靠判定「对端内核是否与自身同一协议代」。
2. 必须覆盖早于握手机制部署的守护进程——它们没有握手方法，这恰是本次事故形态。
3. 检测到不一致时给出可操作 UX：一键重启内核；重启必须由用户明确触发（ADR-0011 冻结决策）。
4. 握手无秘密、全来源可用，不改变既有方法的契约与门禁。
5. 判定不依赖 semver、git rev 或文件系统启发式。

## 不在本阶段

- 自动重启（违反 ADR-0011「修复后的重启需要用户明确操作」）；
- 多守护进程并存与端点协商；
- 第三方 frontend 的能力协商与按方法 capability（归 M4 插件身份）；
- per-method 能力位协商——单一代数足够当前阶段，需要时再演进；
- Windows 的重启 UX（握手方法本身跨平台，named pipe 同样适用）；
- `version` 字段的真实 semver 化发布流程。

## 决策

### 1. 显式协议代数 `protocol_version`

引入单一整数协议代数，代码内常量，首发为 1。判定只看它，不派生自 semver（恒 0.1.0）、
git rev（本地构建频繁变化）或安装路径（不可靠且可被 LaunchServices 解析到错误 bundle）。

晋升规则（写入 protocol README，并作为 review checklist 项）：

- 必须 +1：删除或重命名方法；既有方法的参数或结果 schema 不兼容变更；错误码语义变更；
  来源/秘密门禁收紧到旧客户端会违规的程度。
- 不晋升：新增方法；响应新增可选字段；纯文案与诊断字段变化。

### 2. 新增 `getAgentInfo` 方法

| 方法 | 参数 | 结果 | 门禁 |
|---|---|---|---|
| `getAgentInfo` | `{}` | `{version, protocol_version, pid, started_at?, build_rev?, executable_path?}` | 无 |

- 方法名**必须**是全新的：旧守护进程对它返回 -32601，「方法不存在」本身就是遗留内核的判定
  信号。复用 `status` 无法做到——旧内核会成功应答一个同样的 0.1.0，客户端一无所获。
- `version`：`CARGO_PKG_VERSION`，仅供展示。
- `protocol_version`：决策 1 的代数常量，唯一参与判定的字段。
- `pid`：`std::process::id()`，供定向 SIGTERM。
- `started_at`：进程启动时刻（RFC3339），诊断用。
- `build_rev`：打包脚本注入 `MAILD_BUILD_REV`（git short rev），开发构建缺省省略；诊断展示用。
- `executable_path`：`current_exe()`，用于诊断「从错误位置启动」（如 dist 内的旧 bundle）；
  仅展示。
- **判定只依据 `protocol_version`；其余字段一律不得参与逻辑**，防止路径与启发式判断腐化。

参数沿用 `require_empty_params` 的严格空对象校验。

### 3. 客户端判定与降级

客户端构建时常量 `REQUIRED_PROTOCOL_VERSION = N`。连接建立后（以及每个重连 epoch 变更后）
调用一次 `getAgentInfo`：

| 结果 | 判定 |
|---|---|
| `Ok` 且 `protocol_version == N` | 兼容 |
| `Ok` 且 `< N` | 内核过旧 → 引导重启内核 |
| `Ok` 且 `> N` | 应用过旧 → 引导更新 Maild |
| Err `-32601` | 对端早于握手机制 ⇒ 视为 `protocol_version = 0` ⇒ 内核过旧 |
| 其他传输错误 | 不做版本结论，归既有连接状态机 |

`maild-client` 提供类型化调用与 compat 判定纯函数，但**不做透明握手**：hermetic 测试的假服务器
大量存在且不实现该方法，透明化会让全部测试误报遗留内核。执行策略归各客户端。

各端策略：

- 桌面壳：连接时 + 每个 epoch + 账户中心/compose 等突变入口前置校验；不一致时顶部横幅
  「后台内核与当前应用版本不一致」，禁用突变入口，保留只读诊断。
- `maildctl`：stderr 一行警告，不改变退出码（保持脚本可用）。
- Menu：内部记录，不阻断。
- 第三方/HTTP 客户端：方法公开可查，策略自理，本阶段不承诺。

### 4. 一键重启内核（macOS）

横幅按钮「重启内核」，用户点击后执行：

1. `getAgentInfo` 取 `pid`；缺失则降级为手动指引文案，不猜测目标。
2. SIGTERM 该 pid；等待受管 socket 消失（优雅 teardown 会删除它，已有测试钉死），上限 10s。
3. 超时则 SIGKILL——跳过 teardown 留下的 socket 由既有 `recover_managed_unix_socket` 在新
   agent 启动时回收（同有测试钉死），因此强杀路径安全。
4. 从 `current_exe()` 解析兄弟路径 `Contents/Library/LoginItems/Maild Agent.app`（复用
   `menu_executable_from_agent` 的布局校验思路），经 workspace 打开，由 launchd 作为父进程
   启动，保持 Login Item 语义；非标准 bundle 布局（开发直跑）降级为手动指引文案。
5. 既有重连回退接管；连上后重新握手，一致则横幅消除。限时（30s）失败 → 失败文案 +
   「打开系统设置 → 登录项」（复用 `openSystemSettingsLoginItems`）。
6. 单飞行保护：流程进行中按钮禁用，不重入。

被否决的替代：daemon 自 exec 替换在 SMAppService 身份模型下脆弱；`shutdownAgent` RPC 对旧
内核不存在——恰是本设计要修复的场景。SIGTERM 对任何代数的内核都有效；pid 来自同用户 socket
的对端应答，处于既有信任边界内，signal 失败显性化为界面消息而非崩溃。

### 5. 测试与验收

仓库内（hermetic）：

- api 单测：结果形状、空参数校验、全来源可达；既有 -32601 行为不回归。
- maild-client 单测（假服务器）：上表五行判定路径全覆盖；重连 epoch 后重新握手。
- e2e：起真实 daemon（代数 N）+ 要求 N+1 的客户端 → 判定内核过旧；走重启流程 helper
  （SIGTERM → 重启 → 重握手恢复）。
- 桌面 reducer：横幅状态机（检测 → 重启中 → 恢复/失败）、单飞行保护、无 pid 降级文案。

外部（真机，加入 M3.7 验收清单的升级演练）：安装现行 Preview DMG → 新构建覆盖安装 → 应出现
横幅 → 一键重启 → 向导可用、账户与凭据无损。

## 后果

- 覆盖安装后的版本错位从「-32601 费解报错 + 人工取证」变为「横幅 + 一键修复」。
- 每个不兼容协议变更多一条纪律：晋升 `protocol_version` 并在 protocol README 记录。
- 桌面壳新增一个正交状态（版本一致性）：授权状态、连通性、配置健康、版本一致性四者独立呈现，
  遵守 ADR-0011「状态互不推断」的原则并向后延展。
- 重启流程是新的 macOS 特有代码，需要真机证据；开发直跑与 CI 不覆盖该路径。
- 握手为每次连接增加一次小往返；epoch 频率下开销可忽略。

## 参考

- 事故记录：`docs/testing/README.md` 真实邮箱全链路行（2026-08-21）
- ADR-0011（GUI/daemon 生命周期分离、用户明确操作重启、SMAppService 升级语义）
- ADR-0015（来源门禁与秘密边界、未知容错原则）
- [Apple：Updating helper executables from earlier versions of macOS](https://developer.apple.com/documentation/servicemanagement/updating-helper-executables-from-earlier-versions-of-macos)
