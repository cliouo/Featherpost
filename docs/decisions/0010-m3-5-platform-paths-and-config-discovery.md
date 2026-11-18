# ADR-0010：M3.5 统一平台目录、配置发现与后台停止语义

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：daemon、maildctl、desktop 与后续安装/后台 Agent

## 背景

开发态的三个进程默认从当前目录读取 `maild.toml`，相对 SQLite 与 Unix socket 也受启动目录
影响。交互式 shell 通常有稳定 cwd，但 macOS 登录项和 Windows 后台进程没有这个前提；不同
进程若解析到不同配置或 socket，前端会表现为“找不到 daemon”，更危险的是 daemon 可能在
新目录静默创建空数据库。

后台管理器停止进程时也不会模拟终端 Ctrl+C。macOS launchd 会先发送 SIGTERM；旧入口在账户
上线等待期间尚未监听停止信号，可能绕过插件 LIFO teardown。

## 决策

新增无业务依赖的 `maild-paths` crate，所有进程只从这里取得稳定 namespace `maild`：

- 配置：`ProjectDirs::config_local_dir()/maild.toml`；
- 数据：`ProjectDirs::data_local_dir()`，相对 `store.db_path` 锚定此目录；
- 运行时：`ProjectDirs::runtime_dir()`，平台不提供时回退到 `data_local_dir()/run`；相对 Unix
  `api.listen` 锚定此目录；
- 日志：macOS 使用 `~/Library/Logs/maild`，其他平台使用 state/local data 下 `logs`；
- 备份和 release 目录仅计算位置，本切片不创建、迁移或删除其中数据。

配置选择顺序固定为：显式 positional/`--config` > `MAILD_CONFIG` > 自动发现。自动发现只在
平台默认配置或兼容兜底的 cwd `maild.toml` 单独存在时选择；两条路径指向不同文件时会
fail-closed，要求用户显式选定，避免不同配置指向不同数据库时静默切库；若规范化后是同一文件
则选择平台路径。显式路径与环境变量读取失败时不继续
fallback；空或相对 `MAILD_CONFIG` 是配置错误。endpoint 的既有顺序不变：显式
`--url/--socket/--pipe` > `MAILD_API_*` > 所选配置。

生产装配走 `assemble_with_paths`；原 `assemble` 保留给 hermetic 测试和嵌入者。绝对数据库和
socket 原样使用，空 `db_path` 继续表示内存库。受管相对路径拒绝 `..`、根目录和平台前缀，
生产装配仅创建实际使用的 data/runtime 叶目录；Unix 要求当前 uid 所有且权限不宽于 0700。
若相对路径的新目标与 cwd 旧目标不同，而旧数据库、
lock/WAL/SHM/journal 或 socket 仍存在，启动 fail-closed，并要求用户显式写绝对旧路径或通过
backup/restore 手工迁移。不会自动复制、移动、覆盖或删除任何旧文件。

daemon 保留 `maild CONFIG`，新增互斥的 `maild --config PATH` 和只读 `maild paths`。后续后台
Agent 必须传绝对 `--config`，不依赖 cwd 或 ambient 环境。daemon 在读取配置和装配任何资源前
同步安装停止信号接收器；Unix 的 SIGTERM 与 Ctrl+C 共享同一个可抢占等待和 LIFO teardown
路径，包括账户上线等待阶段。

## 后果

- daemon、maildctl 与 desktop 默认读取同一配置，并从配置派生同一 Unix socket；
- Windows config/data 使用 local 而非 roaming 目录，避免含凭据 TOML 与 WAL 数据进入漫游同步；
- namespace `maild` 成为磁盘兼容面，未来产品显示名变化不能顺带改目录；
- 当前配置类型仍以 UTF-8 字符串承载插件路径。Unix 平台根若不能表示为 UTF-8，生产装配
  fail-closed，不用 lossy 路径访问另一位置；
- 本切片不注册登录项、不安装 bundle、不改变通知 provider。它只提供这些工作的稳定前置。

## 验证

- `maild-paths` 单测覆盖显式/环境优先级、双配置歧义、空/相对环境变量、runtime 回退和受管相对路径；
- maild 单测覆盖受管 data/runtime、legacy DB/socket 冲突、Unix 私有目录和非路径 transport；
- 子进程测试覆盖 `maild paths`、CLI 参数互斥、删除 cwd 后的绝对配置，以及 SIGTERM 后 socket
  删除和 Store 锁释放；
- shared client 覆盖配置派生的相对 socket；
- workspace 原生测试/clippy 与 Windows GNU all-targets 门禁保持。
