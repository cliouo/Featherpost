# ADR-0004：M3 邮箱读取使用摘要分页与按需详情

- 状态：Accepted
- 日期：2026-08-20
- 适用范围：M3 邮箱读取体验

## 背景

既有 `listMessages` 返回完整正文。它适合脚本兼容和小批量导出，但桌面列表若直接复用，
一封长邮件就可能让 1 MiB 响应上限拒绝整页，也会让 watch 快照和 egui 列表反复持有正文。
M3-04 同时需要账户过滤、连续分页、稳定选择和可恢复的完整正文读取。

## 决策

- 新增兼容方法 `listMessageSummaries`，参数为 `{account?, limit?, offset?}`。`account` 省略或
  为 null 表示全部账户，响应只含 account、uid、sender、subject、received_at 与过滤范围内
  的 total；既有 `listMessages` 保持不变；
- `maild-app::ViewState` 只保存摘要。通知中的完整 `MailMessage` 在 reducer 边界立即降为摘要，
  避免每次 watch 发布克隆正文；
- 桌面层以 50 条为一页，账户切换、加载更多和刷新都携带 generation。过期响应不能覆盖新
  账户，列表页和详情各自最多保留一个活动请求，并把快速选择合并为最后一次意图；
- 选择键由 `(account, uid)` 组成。刷新后该键仍在第一页则保持选择，否则清空详情；
- 完整正文只在选中后调用 `getMessage`。响应超过既有 1 MiB 上限时显示可重试错误，不截断
  数据。帧内长正文预先生成有界字节区间，阅读区只布局可见行，复制操作仍取得完整正文；
- 三栏模式常驻阅读区。两栏和单栏在列表与详情之间切换；方向键移动选择，Enter 打开，
  Escape 返回，F5 刷新。

搜索、已读状态、草稿、HTML 和附件仍需要新的守护进程能力，不纳入本决策。

## 后果

- SQLite 摘要查询不读取 body，并可在 SQL 层按账户统计与分页；大列表的网络和 UI 成本由
  页面大小而不是正文大小决定。
- `listMessageSummaries` 是 JSON-RPC v1 的兼容新增方法，旧 consumer 不受影响；使用新版
  桌面端时守护进程与客户端需要一并升级。
- egui 只提交用户意图和呈现 reducer 状态，仍不直接读取 SQLite、IMAP 或 SMTP。

## 验证

- API 测试覆盖账户过滤、最近优先、total 与摘要不含 body；
- reducer 测试覆盖过期账户页丢弃、刷新选择保持、Unicode 正文分行；
- 500/800/1280 三档无窗口绘制覆盖单栏、两栏、三栏；workspace 与 Windows GNU
  all-targets test/clippy 继续作为提交门禁；
- macOS release 长正文、CJK 与断线恢复已在 M3-07 验收；Windows 真机键盘、CJK 和资源
  基线作为 M3.5 发布前外部门禁。
