# ADR-0036：08-A 原窗口写信、持久草稿与发送状态

- 状态：已接受，仓库实现完成
- 日期：2026-09-23
- 前置：[ADR-0034](0034-daemon-owned-drafts.md)、[ADR-0035](0035-draft-attachment-staging.md)、[UI 规格](../design/tauri/UX-SPEC.md)

## 决策

沿用已确认原型与既有 CSS token：墨绿强调、灰白侧栏、系统字族、6/8/12px 圆角；深色外壳保持
邮件原色。新邮件/转发在原窗口工作区编辑；回复/全部回复在阅读区下方展开，可收起后继续读信。
写信与草稿作为内置贡献模块注册，入口、命令与视图仍走 E01 注册底座。正文编辑为纯文本，
HTML 邮件阅读保持完整；不提供未实现的富文本、撤回或离线自动投递。

### 1. 状态由 daemon 保管，UI 回执驱动

schema v10 为 drafts 增加 `send_state`（editing/failed/sending/unknown/sent）和 nullable
`sent_archive`。发送前以版本 CAS 锁定草稿，发送中禁止编辑、删稿和追加附件；已发送/未知禁止
普通编辑或重发。重启把 sending 恢复为 unknown，绝不自动重放。sent/unknown 可显式丢弃。

新增 `sendDraft {id, base_version}`、`resolveDraftSend {id, base_version}`，同时要求
messages_send + drafts_read + drafts_write。后者只把 unknown 解锁为 failed，由用户明确确认
已核对投递情况后调用。发送记录随草稿备份恢复，不依赖 UI 进程是否还在。

sendDraft 只发送 daemon 中该版本的字段和附件，不接收第二份正文。先校验字段与引用，再锁定；
SMTP/provider 阶段有 60 秒总期限。发送中调用被取消或 daemon 崩溃时宁可保留未知，不重发。
明确未发出/SMTP 拒绝变 failed；内容开始写出后没有最终应答变 unknown；最终 DATA 2xx 是已
投递事实，QUIT 失败不改变它。委托 provider 不能确证的错误按 unknown；不靠错误字符串猜阶段。

成功先记录 sent，再独立排队 Sent APPEND；`sent_archive` 仅代表副本已入队，不能说已经同步。
无法入队显示“邮件已发送，副本未加入同步队列”。后续 APPEND 状态由同步活动页处理，已在 [08-B / ADR-0037](0037-mailbox-workspace-and-sync-activity.md) 落地。
若成功后持久结果无法写入，保留 sending/unknown 并要求核对，不能当明确失败自动重试。

### 2. 保存与切换

输入后 500ms 防抖，单编辑器串行保存。保存中的新输入保留在本地，旧回执只推进 version，不
覆盖新字段；随后再保存增量。冲突不覆盖，保留文字、提供复制内容与确认后重新载入；不可悄悄改用新版本。
回复草稿按 account/folder/uid/mode 缓存对应关系，切换邮件不会显示另一封的回复。
From 可选择账户主地址和活动别名；切换账户是版本化草稿 patch，校验账户存在与数量上限，
已上传附件仍属该草稿。旧邮件附件保留原始账户身份。

关闭窗口及 ⌘Q 都先经过保存检查；保存失败/上传未完成时留在窗口。等待期间冻结输入，以免
“最后回执”之后又产生新字。界面没有后台 stop 能力；退出 UI 不停止 daemon。
不在 localStorage 保存正文、收件人或文件路径。系统强杀只能恢复最近确认保存的版本。

### 3. 文件只由 Rust 读取

系统选择器和原生文件拖入提供文件路径给 Rust。打开普通文件后留持有句柄，以随机 token 给
webview；JS 不接触本机路径或附件字节，不获得通用文件 API。512 KiB 分块使用 G04，进度事件
只有 token/offset/size。取消标记与上传协作，失败可重试或重新选择；finish 与自动保存串行，
版本冲突保留上传供用户决定。移除已完成附件用草稿版本更新，不删除原文件。

### 4. 用户交互

编辑器保留 From、To、Cc/Bcc、Subject、正文、附件、保存状态和底部发送区。逐项校验，错误邻接
字段并聚焦；无主题/正文提到附件但未添加时提示确认。发送时字段冻结；failed 可改/主动重试；
unknown 不出现普通重试按钮；sent 显示投递成功及副本状态，返回阅读可继续工作。
草稿列表支持分页恢复多份草稿与显式丢弃，列表不携带完整正文；已发送记录保留至显式移除。发件人选项感知 provider 的单收件人/
无附件约束。mailto 只预填到当前编辑器，不自动发送，不解释非邮件参数。

## 验收

内核：v9→v10/旧备份、草稿并发锁、重启未知态、明确拒绝/投递后断线/QUIT 失败、Bcc 和附件
MIME；桥：真实 frontend 身份、冲突/上限固定错误投影、文件 token 作用域及取消；前端：防抖、
旧回执、冲突保留输入、关闭等待、跨邮件回复隔离、发送未知不重试。真 WKWebView 验证原窗口
写信、文件选择、草稿重开与关窗保存；浅/深色及窄窗口截图。只用虚构账户与本机测试服务器。

实施证据与平台残余见 [测试记录](../testing/README.md#m45-08-a原窗口写信与持久发送adr-0036)。

参考：[RFC 5321](https://www.rfc-editor.org/rfc/rfc5321)、[Tauri 窗口事件](https://v2.tauri.app/reference/javascript/api/namespacewindow/)。
