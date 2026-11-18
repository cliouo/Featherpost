# ADR-0017：M4-01 插件身份与认证缝

- 状态：已接受，M4-01 已实现（hermetic e2e 钉死；无真机外部证据项）
- 日期：2026-08-22
- 适用范围：maild RPC 协议、external/frontend 插件装配、maild-client、maildctl；用户级工具（桌面壳、Menu、既有 maildctl 用法）行为零变化

## 背景

M3.6 把第三方前端与外部进程做成同用户信任域内的独立进程：manifest `capabilities`
只作兼容性路由声明，socket/pipe/HTTP 认证的都只是"当前用户"，Agent 无法区分同用户下
的具体进程。`docs/plugins/README.md` 明确承诺：逐插件 token、握手与方法级 enforcement
必须作为后续安全协议单独实现，不能把 manifest 文本当授权。

M4-02 要把 capability 升级为真实授权表，前提是 dispatch 知道"这次请求来自哪个插件"。
本 ADR 只冻结这个前提——主体识别与认证缝；enforcement 矩阵、安装事务、containment
分别归 M4-02/03/04。

### 愿景对照

`ARCHITECTURE.md` 的原始承诺与本缝一一对应：

- §13.5 明文列出「逐插件凭据、方法级授权……是后续独立工程」——本缝就是该项工程的
  第一半（凭据），第二半（方法级授权）归 M4-02。不是新增 scope，是执行既定计划。
- §6 要求 provider 插件「装上即多一种接入方式，卸掉即消失（可逆副作用）」——孤儿
  条目收敛与移除即吊销是这句话在身份维度的直接落地。
- §6 的缝分类法（receive/transport/notify/alias/transfer/dns/credential）保持不变；
  变化的只是机制——原稿写作时的进程内 `ctx` 缝已按 ADR-0014 冻结演化为进程外
  版本化契约（第三方永不加载动态库）。缝的角色分工（Provider/Consumer）在 RPC
  两侧依旧成立。
- 薄内核约束不受损：本缝不新增 spine，只加一个注册表文件、两个 additive 方法和
  dispatch 上一个查询点。

## 目标

1. Agent 能把一条已建立的连接绑定到某个已安装插件主体；主体由服务端从凭证推导，
   客户端无法自报或伪造。
2. 三种插件形态都能获得身份：supervised 子进程自动注入；independent 外部进程与
   on_demand 前端由用户显式授权后取用。
3. 凭证可轮换、可随包吊销；磁盘上只存哈希，明文只在发放瞬间出现。
4. 完全 additive：不认证的消费者行为与今天逐字节一致。

## 非目标

- 不防御同用户的恶意进程。同用户信任域内任何"秘密"都只是尽力而为；进程级对抗归 M5
  沙箱。本缝的价值是**问责与最小授权的契约基础**：合作插件获得可归因、可回收、可限权
  的身份，拒绝合作的进程与今天的普通用户工具无异。
- 不做 pid 归因（macOS/Linux API 分歧与 pid 复用竞态），不做同意 UI、审计流、
  enforcement 矩阵（M4-02 起）与逐插件网络/资源配额。
- HTTP 回环 Bearer 不参与插件身份：Bearer 认证的是用户，插件不走 HTTP。
- 不做市场、下载签名校验与自动分发（延续冻结决策，网络下载永不直连受管目录）。

## 决策

### 1. 主体模型与会话绑定

`RequestOrigin` 保持不变（传输维度）。新增会话级主体：

```text
Principal::User                    // 缺省，等价现状
Principal::Plugin  { id }          // external package（plugins/<id>/）
Principal::Frontend{ id }          // frontend package（frontends/<id>/）
```

主体在**连接 accept 后、首个请求前**确定，之后整条连接不可更改。dispatch 上下文
（`Context` / `dispatch_with_origin`）增加 principal 字段；M4-01 自身不改任何既有
方法的判定，只为 M4-02 提供查询点。

### 2. 凭证与注册表

- 格式：`mdp_` + base64url(32 bytes)，共 47 个 ASCII 可见字符；字节来自
  `getrandom::fill`（复用 account_catalog 的系统随机源用法）。
- 注册表：活动配置同目录的 `plugin_auth.toml`（平台模式即
  `~/Library/Application Support/maild/plugin_auth.toml`）。沿用 accounts.toml 的全部
  文件纪律：仅由 Agent 写入、0600、owner 校验、单硬链接、拒符号链接、64 KiB 上限、
  未知 `format_version` 在任何副作用前 fail-closed、临时文件 + fsync + 原子 rename +
  父目录 fsync 发布。`format_version = 1` 起。
- 条目字段：`kind`（plugin/frontend）、`id`（必须等于受管目录名）、
  `token_hash`（SHA-256 hex）。**明文 token 绝不落盘**——Agent 只在发放/注入/reveal
  瞬间持有明文，比对一律走常量时间哈希比较。
- 发放时机：包扫描通过 manifest 检查时自动补发缺失条目（supervised 是否在 allowlist
  不影响发放；未启用只是休眠）。启动扫描后收敛孤儿条目：注册表存在而包目录消失的
  条目直接删除（对齐账户孤儿 revision 收敛语义；解析失败绝不清理）。
- 新增依赖：`sha2`（纯 Rust）。

### 3. 认证协议

新增两个 additive JSON-RPC 方法：

```text
authenticate {"token": "mdp_…"}   → {} 或错误
whoami {}                         → {"principal": "user" | "plugin:<id>" | "frontend:<id>"}
```

规则：

1. `authenticate` 只允许作为连接上**第一个请求**；成功后该连接永久绑定为对应主体。
2. token 无效、对应包已移除或注册表停摆 → `-32014 AUTH_REJECTED`，**应答后立即关闭
   连接**——绝不静默降级为 User（fail-closed）。
3. 已绑定后再发 `authenticate` → 同样 `-32014` 并关闭。
4. 允许来源仅 Unix socket 与 Windows named pipe；stdio/HTTP 上收到 `authenticate`
   → `-32015 AUTH_UNSUPPORTED_ORIGIN`（连接保留，可继续以 User 身份正常调用）。
5. 不发送 `authenticate` 的连接保持 `User` 主体——桌面壳、Menu、maildctl 与所有现存
   消费者零迁移。
6. 新增错误码：`AUTH_REJECTED = -32014`、`AUTH_UNSUPPORTED_ORIGIN = -32015`
   （接续 -32013 之后）。

### 4. 凭证通道

- **supervised external**：spawn 时在现有空环境中追加 `MAILD_PLUGIN_TOKEN=<明文>`，
  且**每次 spawn 生成新 token**并替换注册表哈希——旧 token 对新连接立即失效，会话
  归因天然绑定到 spawn 代。这是对「子进程环境仅有 MAILD_CONFIG」契约的 additive
  修订，需同步 plugins/operations 文档。
- **independent external 与 on_demand frontend**（LaunchServices 拉起，无法注入）：
  稳定 token，由用户执行 `maildctl plugin-token <id>` 一次性显示到 stdout（对齐
  App 专用密码的使用心智；绝不进 argv/env/日志/RPC 应答）。CLI 子命令命名沿用
  `notification-status` 式扁平动词。

### 5. 生命周期语义

- 包移除 → 孤儿收敛删除条目；**已建立连接不受影响**（绑定发生在 accept 时），新
  连接被拒——吊销在重连边界生效，文档明确此窗口。
- 手动轮换：实现期固化为「reveal 即轮换」——`maildctl plugin-token <id>` 每次调用
  都生成新凭证并使旧凭证对新连接失效，无需独立 `--rotate` 开关。
- 注册表损坏 / 未来版本 / 超限 → **身份缝整体停摆**而非核心退出：supervised 包一律
  不再 spawn（宁可拒绝运行无法归因的第三方代码）、`authenticate` 返回
  `-32015`、输出有界诊断；Agent 其余能力照常。比 accounts.toml 的配置停止更软，
  因为注册表不含用户数据。

### 6. 协议代数

新增方法、错误码与环境变量均为 additive，按 ADR-0016 规则**不提升**
`PROTOCOL_VERSION`；`getAgentInfo` 判定表不受影响。

### 7. 客户端契约（实现期固化）

认证是连接级状态，而 `maild-client::RpcClient` 的便捷调用对 IPC **每请求新开一条
连接**。因此 `authenticate` 只存在于 `StreamConnection`（长连接句柄）上；
`RpcClient` 保持无主体语义（恒为 `user`），仅提供 `whoami` / `revealPluginToken`
这类无状态调用。插件 SDK 文档必须引导认证消费者使用长连接路径。

## 备选方案与取舍

六个决策轴上被否决的选项与理由，供后续翻案时对照：

| 轴 | 选定 | 否决项与理由 |
|---|---|---|
| 身份机制 | 会话级 token | **OS pid 归因**：macOS peer-pid API 属半私有且 Linux/macOS 分歧、pid 复用竞态、connect 与查表间 TOCTOU；independent/on_demand 非Agent 拉起根本无 pid 可查。**二者组合**：在已接受的威胁模型下不产生新增保证，只增加复杂度 |
| 凭证落盘 | 中心注册表只存哈希 | **Keychain 逐插件 item**：generic password 的 ACL 挡不住同用户应用，Windows 无 provider 会造成平台能力断裂，孤儿清理跨 Keychain 难做；**明文注册表**：违反秘密只写不读原则；**token 放插件目录内**：把 Agent 管理的秘密混入第三方可写内容，安装事务还要特殊保全 |
| 呈现协议 | JSON-RPC 首请求 `authenticate` | **专用握手行**：结构上无法误用，但破坏既有帧解析、逼近 breaking change（可能触发协议代数提升）；**逐请求签名**：本地 IPC 无离线重放面，为每个 DTO 加签名成本远超收益 |
| supervised 注入 | env 变量 | **继承 fd 传递**：暴露窗更小，但要新增 dup2/handle 继承管道、跨平台差异大、SDK 复杂；在"同用户本可全权连接"的前提下属过度设计（见安全声明）。**写进注入的 MAILD_CONFIG**：秘密进文件，比注册表明文更糟 |
| 注册表损坏 | 身份缝软停摆 | **硬配置停止**（对齐 accounts.toml）：注册表无用户数据，连坐收信链路不成比例；**降级继续 spawn**：恰在校验失效时运行无法归因的第三方代码，违背 fail-closed |
| 强制姿态 | 契约式 opt-in | **技术强制识别**：同用户信任域内原理上不可能（拒绝认证的进程与用户工具不可区分）；能做到的强制（pid 比对）已在上行否决。诚实记录残余风险，由 M5 沙箱收口 |

## 兼容性

- 旧 daemon 对 `authenticate`/`whoami` 返回 -32601：新插件应答失败时应按"宿主过旧"
  处理而非崩溃（复用 ADR-0016 的 method-not-found 判据）。
- 旧插件在新 daemon 下继续以 User 身份工作，一切照旧；生态契约（SDK/文档）要求
  合作插件认证，但不认证不会被技术性阻止（见信任边界）。
- `listExternalPlugins` / frontends 清单各增加 `has_identity: bool` 观测字段；
  绝不返回 token 或其哈希。

## 安全与信任边界（诚实声明）

- **决定性事实**：今天任何同用户进程本就能不带 token 连接受管 socket 并以 User 主体
  获得全部权限——偷到插件 token 在当前威胁模型下不增益任何攻击能力。token 保密的
  真正价值是**向前兼容**：M5 收紧 User 会话权限后，已按契约认证的插件世界无需迁移。
- 同用户进程可以读取注册表文件（拿到的是哈希，不可逆用），也可能读取 supervised
  子进程的环境（Linux `/proc/<pid>/environ`、macOS `ps -E` 对同用户开放）——env 注入
  的保密性在同用户域内是尽力而为。未来可改 fd 传递缩小暴露窗，v1 接受并以本文档
  声明。
- 因此本缝不是恶意软件防线：它的可验证产出是——合作插件的每次连接都可归因、可单独
  吊销、可在 M4-02 中按 capability 限权；非合作进程维持现状。该残余风险由 M5 沙箱
  收口。
- token 出现在日志、RPC 应答、`listExternalPlugins`、core dump 之外的一切路径都视为
  违规；stderr 审计行只含主体 id 与来源。

## 测试策略（hermetic）

- 单测：token 格式与哈希往返、常量时间比较、注册表校验矩阵（未来版本/超限/坏
  UTF-8/孤儿收敛）、per-spawn 轮换状态机。
- e2e（`tests/plugin_identity.rs`，平台布局临时 HOME）：fixture 包 + 假 supervised
  子进程认证后 `whoami` 返回插件主体；错 token 拒绝且连接关闭；二次 authenticate
  拒绝；HTTP 来源返回 -32015；移除包后重连被拒；注册表损坏 → 不 spawn + -32015 +
  核心存活；无认证会话 `whoami == "user"`（兼容锚点）。
- Windows named pipe 认证路径进 CI job；本机交叉编译只证明可编译。

## 后果

正：M4-02/03/08（enforcement、安装事务、transfer 审计）有了主体地基；插件故障排查
可归因；吊销与轮换成为一等操作。

负：多一份受管状态文件及其纪律成本；supervised spawn 多一次注册表原子写；插件 SDK
文档需要解释"为什么认证了反而权限变小"；同用户域内的安全幻觉风险靠本文档的诚实
声明对冲。

## 参考

- `ARCHITECTURE.md` §6/§13：缝分类法、可逆副作用要求与安全边界 v1 声明（愿景出处）
- ADR-0014 §7–10：插件生命周期与 capability 元数据现状
- ADR-0015 §9：RequestOrigin 来源门禁先例（本缝在其上叠加主体维度）
- ADR-0016：协议代数与 -32601 判据（本缝不触发代数提升）
- `docs/plugins/README.md`：逐插件 token 承诺的出处
