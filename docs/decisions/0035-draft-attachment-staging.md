# ADR-0035：G04 新附件分块上传与草稿持有字节

- 状态：已接受，M4.5-G04 已实现；workspace 712 passed / 0 failed / 8 ignored
- 日期：2026-09-23
- 关联：[ADR-0034](0034-daemon-owned-drafts.md)、[ADR-0032](0032-attachment-byte-channel-and-remote-content-policy.md)、[交付计划](../design/tauri/IMPLEMENTATION-PLAN.md)

## 决策

G04 完成从本地文件到可发送附件的内核通道。文件选择和进度 UI 由 08-A 消费；本片提供共享客户端及 CLI 验收入口，不访问真实邮箱。

### 1. 分块上传，不接受服务器文件路径

新增 `beginAttachmentUpload {draft_id, filename, mime_type, size}`、
`writeAttachmentUpload {upload_id, offset, data}`（base64）、
`finishAttachmentUpload {upload_id, base_version}`、`cancelAttachmentUpload {upload_id}`。
四个方法均需 `drafts_write`。上传绑定调用主体和既有草稿；随机 ID 不代替主体校验。

每块原始字节不超过 512 KiB，沿用 1 MiB 帧。仅接受连续 offset；已确认区间的逐字节相同重传
返回当前 offset，不同内容或间隙拒绝。完成前校验接收长度等于声明长度。零字节文件合法。
单文件 20 MiB、每封信附件合计 25 MiB；这些是原始字节上限，不是 SMTP 服务商承诺。
全局同时最多 8 个上传、声明总量最多 128 MiB；到顶拒绝，不驱逐其他上传。

filename 只是显示名，拒绝路径分隔符、点路径和控制字符；MIME 类型只接受 type/subtype。
客户端读取用户选定的文件并传字节，daemon 从不按 RPC 提供的路径读文件。

### 2. 临时态用匿名文件，完成态进邮件库

daemon 在数据库旁的专用 staging 目录创建匿名临时文件。Unix 目录要求当前属主、0700，拒绝
符号链接；Unix 通过打开的目录句柄创建文件，避免目录被替换后沿新路径写入。
临时文件不公开路径，取消、超时或进程退出关闭句柄，OS 回收文件；Unix 在写字节前 unlink；若恰在 create/unlink 之间强杀，最多留下零字节随机文件，不含附件内容。
内存数据库使用私有临时目录。同一 UID 的恶意进程不属于此边界提供的隔离能力。

上传 30 分钟无活动过期，每分钟回收；每次访问也检查。重启后未完成上传需要重新选择文件。
这不能显示为「附件已保存」。已有完成附件不受此期限影响。

### 3. finish 是草稿事务，不是易失的 ready 文件

schema v8 → v9：`draft_attachments` 新增 nullable `upload_id`、`upload_owner`、`content BLOB`
和 upload_id 唯一索引。旧 part 引用的三个新字段均为 NULL，既有形状与行为保留。

finish 在同一事务中检查草稿存在、base_version、数量与总字节上限，插入附件字节/元数据，
推进草稿 version 后返回完整草稿。冲突返回 G03 的 `-32006`，临时文件仍可用，调用方读回草稿后
决定是否以新版本重试。提交回执才意味着附件已保存。重复 finish 通过 upload_id + owner 查询
已完成行，不重复添加、不再次推进版本；返回当前草稿，若已删除/移除则报告不存在。

完成字节与草稿一起备份/恢复。`updateDraft.attachments` 允许 `{upload_id}` 保留、重排本草稿
已有新附件；不得从另一草稿导入 ID。替换列表及字节保留处于同一事务；移除附件或删除草稿会
删除对应 BLOB。用户数据无 TTL。不允许仅持 ID 把别人的未完成上传接入草稿。

### 4. 发送引用已保存数据，并复验

`send` / `sendFromAccount.attachments` 兼容旧四元组，并接受 `{draft_id, upload_id}`。
新的引用分支额外要求 `drafts_read`，不能只靠 `messages_send` 读取用户草稿附件。
按草稿与 ID 联合查字节，缺失拒绝整次发信；读取时还校验持久化 MIME/文件名和实际 BLOB 长度，SQL 在把过大字节复制到内存前拒绝，损坏附件显示 stored=false，finish 不继续扩展该草稿。发送不自动删除草稿或附件。SMTP 与 Sent APPEND
继续使用既有 MIME 组装和队列语义，用户是否丢弃草稿由 08-A 发送状态机处理。

新客户端对旧四元组保持原来的序列化（包括 `part_index: 0`），以兼容仍在协议 v1 的旧 daemon。
MIME 文件名的中文、引号、分号与长参数采用 RFC 2231 编码/续行；不把文件名作为额外参数解释。

### 5. 错误、取消与客户端

- `-32008`：上传不存在、过期、其他主体上传（相同对外形状）。
- `-32009`：上传状态不符，附 expected offset / size（仅属主可见）。
- `-32007`：数量或字节配额超限；沿用草稿上限错误。
- `-32602`：参数、base64、文件名或 MIME 无效。
- `-32005` / `-32006`：草稿不存在 / 版本冲突。
- `-32603`：存储失败；不回显文件系统路径。

cancel 仅释放当前主体未完成上传，不移除已加入草稿的附件。删除草稿也释放其未完成上传。
共享客户端提供类型化四个方法；CLI `draft attach-file` 从选定文件分块上传、校验读取总长度、
完成后打印新草稿。读取/上传失败尝试 cancel；提交回执不确定时保留 ID 供 finish 重试，不猜测
成功也不自动重新上传。Unix CLI 拒绝符号链接和非普通文件；daemon 侧根本没有路径输入面。

## 验证与回退

验收覆盖大于帧及接收端 2 MiB 上限的真实二进制往返、MIME 解码、零长度、错 offset、不同内容
重传、未完成 finish、配额、取消、过期、主体越界、符号链接与路径输入拒绝、草稿冲突保持
上传、重复完成、detach/delete 字节清理、重启与备份恢复、v8 迁移及保留对象拒绝。
全 workspace test/clippy 和前端现有 typecheck/test/build 为门禁，不以 CLI 验收替代 08-A UI。

数据库升级后旧二进制拒绝打开；降级使用升级前备份。临时上传不参与备份，已完成附件参与。
内存组装 MIME 仍存在，25 MiB 总量限制控制一次组装的内存开销；不新增流式 SMTP 架构。

参考：[tempfile 匿名文件清理保证](https://docs.rs/tempfile/latest/tempfile/fn.tempfile_in.html)、
[Rust Unix open flags](https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html)。

文件名编码参考：[RFC 2231](https://www.rfc-editor.org/rfc/rfc2231.html)。
