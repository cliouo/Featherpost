# ADR-0003：M3 使用 egui 原生壳、系统 CJK 字体与按需重绘

- 状态：**选型结论已于 2026-08-29 被 [ADR-0024](0024-builtin-frontend-tauri-migration.md) 取代**；
  M3 封板记录与实现细节保留有效
- 日期：2026-08-20
- 适用范围：M3 桌面呈现层

> **取代说明**：本 ADR 选 egui 的隐含前提是「邮件正文以纯文本消费」。日常使用证伪了该前提，
> 忠实渲染 HTML 邮件只有真浏览器引擎能做到。默认前端已改为随包预装的 Tauri frontend package
> `com.maild.ui`；`builtin-egui` 退守为冻结的恢复前端（能力边界见 ADR-0024 决策 3）。
> 本文其余部分——系统 CJK 字体发现、按需重绘、三栏响应式布局、无领域逻辑复制——作为
> M3 封板记录继续有效，其中「前端不复制领域逻辑」的边界对新前端同样适用。

## 背景

M3-01/02 已把 endpoint 发现、JSON-RPC、重连、快照和通知归并收进 `maild-client` 与
`maild-app`。桌面层还需要验证 macOS/Windows 原生窗口、中文渲染、响应式布局和空闲时
不持续重绘，同时不能把分页、SQLite、IMAP、SMTP 或 rules 语义复制进组件。

egui 默认字体不覆盖中文。直接把完整 CJK 字体提交进仓库会增加十几到几十 MiB 的源码与
产物体积，也会引入额外字体许可证资产管理。macOS 和 Windows 的目标系统本身已经提供
成熟的简体中文 UI 字体。

## 决策

新增 `maild-desktop` workspace crate：

- 使用 eframe/egui 0.36 的 Glow renderer 和 AccessKit；桌面二进制接受与 `maildctl`
  一致的 `--config`、`--url`、`--socket`、`--pipe` 参数，并复用 `discover_endpoint`；
- GUI 持有一个单 worker Tokio runtime 和 `ConnectionHandle`，绘制阶段只克隆最新
  `ViewState`，不执行阻塞 I/O；
- 一个后台 watch task 在 revision 变化时调用 `Context::request_repaint()`，不设置固定
  帧率、不使用 shimmer，也不在空闲时调用 `request_repaint_after()`；
- 启动时只在平台系统字体目录内枚举有界数量的候选文件，按文件名预筛 PingFang SC/
  Hiragino Sans GB 或 Microsoft YaHei UI/Microsoft YaHei；单文件读取上限 32 MiB、候选总读取
  预算 128 MiB，不扫描用户字体目录，也不使用共享内存映射。随后用零拷贝 `ttf-parser`
  限制字体集合 face 数并验证必要中文字形，不解析或复制字体 name 表；选中字体仅作 egui
  默认字体后的 CJK fallback；
- 视觉采用暖灰浅色表面、冷青状态色和 `{4, 8, 14}` 三档圆角。1080px 及以上显示
  账户/消息/阅读三栏，720px 至 1079px 显示账户/消息两栏，更窄时显示单栏；
- loading、empty、error 和 ready 由 `ViewState` 的连接阶段与快照内容纯映射。在线但
  `partial_snapshot` 或总数非零而列表为空时显示“快照不完整”，绝不把未知误报为空。ready
  可以展示已有账户和消息摘要；消息列表只布局可见行，所有摘要、版本和诊断文本先流式截断。
  应用层拒绝超过请求页大小或摘要字段边界的恶意快照。选择、分页、详情与键盘导航由
  [`0004-m3-mailbox-reading.md`](0004-m3-mailbox-reading.md) 落地。

若系统缺少候选 CJK 字体，窗口仍能启动并明确显示字体诊断；标准 macOS/Windows 环境由
平台测试钉死候选字体与字形有效性。

## 后果

- 桌面壳没有 maild daemon crate 依赖，不直接访问邮箱、数据库或凭据。
- 系统字体避免仓库携带大型二进制资产，但最终发布必须在两平台真机复核具体字体观感。
- HTTP 通过 actor 轮询，IPC 通过通知更新，两者对组件暴露相同的 `ViewState`。
- 窗口关闭会请求 actor 有序停止，然后释放独立 runtime；守护进程生命周期不由桌面端托管，
  托管和后台启动仍属于 M3.5。

## 验证

- 单测覆盖 CJK 候选字体实际字形、响应式断点、状态映射、Unicode 安全摘要和
  500/800/1280 三档无窗口 egui 绘制；
- macOS 实际启动验证错误态和 HTTP ready 三栏，系统 CJK 字体可用，actor 状态变化能
  唤醒 UI；
- workspace test/clippy 与 Windows GNU all-targets 编译作为提交门禁；M3-07 已完成 macOS
  release 窗口、CJK、长正文、断线恢复和资源基线，Windows 真机窗口与资源作为发布前外部门禁。
