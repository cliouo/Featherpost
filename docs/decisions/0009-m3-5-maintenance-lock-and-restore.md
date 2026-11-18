# ADR-0009：M3.5 用持久锁文件保护单实例与可回滚恢复

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：文件型 SQLite store、daemon 单实例和离线恢复

## 背景

SQLite 自己的事务锁只能约束已经打开同一数据库 inode 的连接，不能保护“检查 daemon 已停止后，
另一个进程又启动”与“把路径原子替换后，旧进程继续写旧 inode”之间的竞态。恢复一旦在这种状态
下执行，路径上的新库和运行中进程持有的旧库会永久分叉。

恢复还必须同时解决三个故障窗口：输入备份可能损坏；替换前可能失败；替换已经完成但目录项尚未
持久化。直接覆盖当前数据库或只保留 WAL/SHM 文件都不能给出可靠回滚故事。

## 决策

- 每个非内存 `Store` 在打开 SQLite 之前，先对规范化数据库路径旁的
  `.<database-name>.maild.lock` 取得非阻塞独占文件锁，并持有到 `Store` drop。第二个 daemon、
  `Store` 或维护操作立即失败，不等待也不猜测 PID；内存 store 不创建文件锁。
- 锁文件长期保留，只释放锁而不删除 inode。删除锁路径会让已经打开旧 inode 的等待者与重新创建
  路径的进程分别锁住不同文件。Unix 锁文件必须是当前 uid 拥有的普通文件且权限不超过 `0600`；
  规范化数据库目录必须由当前 uid 拥有且不可被 group/other 写入，祖先只允许当前 uid/root，
  root sticky 公共目录可以作为祖先。Unix 数据库 leaf 符号链接与多硬链接都 fail-closed，避免同一
  inode 经不同文件名生成两把锁；现有主库必须由当前 uid 拥有并会在 SQLite 打开前收紧为 `0600`，
  新库也先以 `0600` 创建；已有 `-wal`、`-shm`、`-journal` 在 SQLite 打开前按同样的 owner、
  单链接、no-follow/nonblocking 边界校验并收紧为 `0600`，特殊文件快速 fail-closed。
- Windows 锁句柄不共享 delete，且以 `OPEN_REPARSE_POINT` 打开并拒绝 reparse lock 文件；已存在
  数据库会先完整 canonicalize 到最终 leaf，再派生锁名，避免大小写或 NTFS 8.3 文件名别名形成
  第二把锁。多硬链接同样通过 handle metadata fail-closed。
- 新增显式离线维护命令：

  ```text
  maild restore \
    --database <CURRENT.sqlite3> \
    --input <BACKUP.sqlite3> \
    --rollback <NEW-ROLLBACK.sqlite3>
  ```

  `database` 必须是现有普通文件；Unix 下不接受多硬链接。`rollback` 为必填且必须不存在，命令不
  提供 `--force`。`input` leaf 不允许符号链接/reparse；`input` 与 `rollback` 只要位于当前库目录
  且文件名以 `-wal`、`-shm` 或 `-journal` 结尾就一律拒绝，不自行猜测 APFS/NTFS 的 Unicode
  大小写等价。精确相对配置文件名 `restore` 成为保留字，可写成 `maild ./restore`。
- restore 先取得与 daemon 完全相同的维护锁，再分别对当前库和输入执行当前 schema 校验与
  `PRAGMA quick_check(10)`。输入会先通过 SQLite Online Backup 生成同目录、`0600`、已同步的
  replacement 临时文件；任何校验或准备失败都不产生回滚文件、不修改当前库。
- replacement 准备好后，用已有 Online Backup 路径把当前库发布到用户指定的 rollback，执行
  no-clobber、文件同步和目录同步。只有回滚快照完整持久化后，才允许改变当前库。
- 发布前对当前库执行 `wal_checkpoint(TRUNCATE)` 并切换到 DELETE journal mode，关闭连接后清理
  旧 `-wal`/`-shm`，再以同目录 rename 原子替换当前路径。replacement 文件此前已经同步；Unix
  发布后继续同步数据库父目录。恢复输入与 rollback 永不删除。
- 回滚发布后的任何替换前失败都明确返回 rollback 路径，并保证原数据库路径仍可用；替换成功但
  目录同步失败则明确报告“新数据库已发布、回滚仍存在、掉电持久性未确认”，不能误报成未恢复。
- 与在线备份一致，相同 uid 的其他进程属于本地维护信任域；本切片不试图抵御同 uid 主动替换
  临时路径或锁路径。其他普通用户由 Unix 目录和锁文件约束排除。

## 后果

- 文件型 store 现在天然是单实例；重复 daemon 在 store 装配阶段明确失败，不会继续持有第二个
  SQLite writer。备份仍可在线运行，因为它只读源库且不申请维护锁。
- 恢复必须停止 daemon；若忘记停止，命令在创建 rollback 或临时 replacement 前失败。
- 锁文件是数据库生命周期的一部分，不应当作 stale 临时文件删除。卸载若保留数据库，也必须保留
  该锁文件；只有明确删除整个数据集时才能一并删除。
- 恢复不会自动选择或清理 rollback，也不会自动重启 daemon。运维者验证恢复结果后自行决定保留
  周期；当前没有任何会删除唯一可用数据库的自动策略。

## 验证

`tests/store_restore.rs` 覆盖：

- 同进程第二个 Store 和跨进程 `maild restore` 都被活动 owner 拒绝，owner drop 后可重开；配置了
  store 的第二个 daemon 不会在装配失败后降级成无 store 运行；
- Unix 可写目录、不安全锁权限、数据库符号/硬链接 fail-closed，既有主库权限会收紧为 `0600`；
- 成功恢复安装输入中的消息/OTP 标记，保留原库 rollback、输入不变、旧 sidecar 不进入发布结果；
- 损坏输入、未来 schema、既有 rollback 与当前库 sidecar 路径冲突均在替换前失败且当前库不变；
- rollback 已发布后的人为写权限故障保留原库和完整 rollback；
- CLI 成功输出、帮助、必填参数和退出码稳定。
