# ADR-0043：上下文感知验证码识别、普适新邮件通知与规则原子持久化

- 状态：已接受；内核与测试已实现（2026-11-17）；全栈汇合（同日）已完成 `paths.rs` 装配 `RulesStatePath`、`api.rs` 真实 `persisted` 与错误映射、`setSettings` 可选 `persist: true` 运行时挂载侧车、`syncMessages`
- 日期：2026-11-17
- 关联：[ADR-0006](0006-m3-runtime-rules-settings.md)、[ADR-0012](0012-m3-5-native-notifications.md)、[ADR-0026](0026-folders-and-incremental-sync.md)、[ADR-0032](0032-attachment-byte-channel-and-remote-content-policy.md)

## 背景

日常后台收信体验有三个缺口：

1. **验证码误报**：`OtpPlugin` 默认 pattern `\d{6}` 是裸正则，任何含日期（`20261117` → `202611`）、订单号、邮编或手机号的普通邮件都会被当成验证码弹窗，并占掉 `otp_notifications` 去重位。
2. **普通新邮件没有通知**：非验证码的日常来信在后台不提醒；也没有按账户静音、没有防刷屏。
3. **规则重启即丢**：`setSettings` 只改内存（ADR-0006 时的有意取舍，`persisted: false`），界面里配好的静默时段、敏感预览开关、静音词在后台重启后回到 TOML。菜单栏也缺「立即同步」入口，且为两三个本地请求常驻一个多线程 tokio 调度器。

## 决策

### 1. 上下文感知的验证码识别（`otp.rs`）

- `extract_code(pattern, body, subject)` **纯正则契约不变**（`domain.rs` 直接单测）。
- 新增 `extract_smart_otp(body, subject)`：
  - **语境门**：正文或主题须含验证码关键词才继续。中文子串：验证码、校验码、动态码、确认码、安全码、认证码、登录码、提取码、授权码、激活码、一次性密码、口令；英文 ASCII 大小写不敏感：verification、verify、security code、confirmation code、login code、auth code、one-time、passcode、otp、pin code、2fa。英文关键词要求左词边界，`otp`/`2fa` 另要求右词边界（`hotpot` 不触发）。
  - **候选**：前后都不紧邻 ASCII 字母数字的 4～8 位纯数字串——长号码、URL 路径、十六进制串中间截不出码。
  - **排除**：两侧以 `- / . : ,` 连着数字（日期、时间、小数、千分位、分段电话）；`1990..=2099` 后接 `年`/`-`/`/`；8 位 `YYYYMMDD` 紧凑日期；金额（前接 `¥ ￥ $` 或后接 `元`，可隔空白）。
  - **选择**：关键词 64 字符内的候选按「距离 + 非 6 位惩罚 8 + 位于关键词之前惩罚 4」取最小；附近没有时退回首个 6 位候选，再不行只在全文唯一候选时采用。
- `OtpPlugin::apply`：`cfg.pattern == DEFAULT_PATTERN`（`\d{6}`）时用 `extract_smart_otp`；用户显式自定义 pattern 时仍按其正则原样 `find`——用户写了正则就是用户的契约。
- 新增 `OtpConfig::detect(body, subject)`：按同一语义判定「会不会被识别为验证码」，供普适通知去重，不触碰 store。

既有测试输入（`你的验证码是 246810`、`验证码 777777`、主题 `登录验证码` + 正文 `135790`）都带关键词且为独立 6 位数字，行为不变。

### 2. `RulesConfig` / `RulesPatch` additive 扩展

| 字段 | 序列化 | 说明 |
|---|---|---|
| `notify_new_mail: bool` | 缺省 `false`，为 `false` 时不出现在 JSON | 普适新邮件通知开关 |
| `muted_accounts: Vec<String>` | 空时不出现在 JSON | 按 `MailMessage::account` 精确匹配静音；≤ 64 项、每项非空、≤ 128 字节、无控制字符 |
| `persist: bool` | `skip_serializing`，**只来自 TOML** | 是否启用 `rules_state.toml` |

默认配置下 API `rules` JSON 逐字节不变（`bridge.rs` 的相等断言、旧客户端都不受影响）；`RulesPatch` 同步新增 `notify_new_mail` / `muted_accounts`，`persist` 不可经 `setSettings` 修改。

### 3. `rules_state.toml` 原子侧车持久化

- **形态**：`format_version = 1` + `[rules]` 覆盖层，只记录用户经 `setSettings` 改过的字段（`RulesPatch` 形态）。生效配置 = TOML 基线 ⊕ 覆盖层。选覆盖层而非完整快照：用户从未碰过的字段继续跟随 TOML，升级 TOML 默认值不会被一份旧侧车静默盖住；用户在界面改过的字段以界面为准（重启不回退）。
- **纪律**（逐条对齐 `reading_policy.toml` / `plugin_state.toml`）：0600、当前 UID 拥有、单硬链接、拒符号链接与非普通文件、64 KiB 上限、未知 `format_version` 与未知字段拒绝、合成后的规则须过 `validate_config`；写入走同目录 0600 临时文件 + `sync_all` + 原子 rename + 父目录 `sync_all`（复用 `plugin_state::publish_atomically`）。
- **状态机**：
  - 未挂载（`RulesSettings::ephemeral`）：patch 只改内存，`is_persisted() == false`——与 ADR-0006 行为一致；
  - 已挂载且文件不存在：基线即当前态，`is_persisted() == true`，**首次 patch 才落盘**（首次安装不制造状态文件）；
  - 已挂载且文件损坏/不安全：回退 TOML 基线、`is_persisted() == false`、patch 返回 `RulesPatchError::Unavailable`，**绝不覆盖**磁盘上的那份（也不「修好」它的权限）。
- **写入顺序**：校验 → 落盘 → 更新内存，全程持有同一把写锁；任一步失败内存不变。
- **接线缝**：`RulesPlugin` 在 `persist = true` 时取 `RulesPlugin::with_state_path` 显式路径，否则取上下文中的 `RulesStatePath` 服务；都没有则退回非持久并打一行诊断。`persist = false` 时即使有路径也不持久。`paths.rs` 只需在 load 前 `provide(RulesStatePath(rules_state_path_for(config_path)))`；`api.rs` 改用 `try_patch` 区分 `Invalid → -32602` / `Unavailable → -32001` / `Io → -32603`，并以 `is_persisted()` 填 `persisted`。旧签名 `patch() -> Result<_, String>` 保留以免打断现有调用方。

### 4. 普适收件箱新邮件通知（`rules::mail_notify`）

由 `RulesPlugin` 挂载，监听 `MailFiltered`——只有主文件夹（收件箱）新邮件会发它，非收件箱同步直接落库（ADR-0026 决策 9），天然不通知。

- **启用**：只看当前生效的 `RulesSettings`（运行时 patch 立即生效）里的 `notify_new_mail`。
- **跳过**：`seen`（别的设备已读）、`deleted`、静音账户、`Date` 头早于 24 小时（首次同步历史未读积压）、同一 `(account, folder, uid)` 重复投递（进程内有界去重，4096 条）。
- **不与 OTP 双发**：`OtpConfig` 存在且 `detect()` 命中，或 `store.is_notified(account, uid)` 为真 → 跳过，交给 `OtpPlugin` 发专属通知。两道检查覆盖两种监听器挂载顺序。普适通知**不写** `otp_notifications`，避免反向吞掉验证码通知。
- **路由**：同样走 `QuietHoursQuery` + `NotifyRouteEvent`，`sensitive: false`——静默时段与 `silent_keywords`（按主题）照常生效；`show_sensitive = false` 不静默普通新邮件，只脱敏。
- **隐私**：`show_sensitive = true` → 标题为发件人显示名（无则地址），正文为主题 + 80 字符摘要；`show_sensitive = false` → 标题 `新邮件`，正文仅 `账户 <account> 收到一封新邮件`，不含发件人、主题与摘要。
- **防刷屏**：按账户 10 秒滑动窗口，窗口内前 3 封逐封通知（`OpenIntent::Message`），超出后发一条合并提示 `账户 <account> 收到多封新邮件`（`OpenIntent::Inbox { account }`），同一窗口内合并提示只发一次，其余抑制。限流器每账户只保留最近 4 个时刻、最多跟踪 256 个账户，内存有界。被静默路由的信不占限流额度。

### 5. 默认配置与菜单栏

- `maild-launcher` 首次生成的配置在 `[plugins.rules.config]` 写入 `notify_new_mail = true` 与 `persist = true`：新安装开箱即有新邮件通知与规则重启保留。既有用户 TOML 按 bootstrap 纪律不改写，需要的用户自行加两行。`maild.example.toml` 以注释说明三个新键。
- `maild-menu`：RPC 运行时改为 `new_current_thread` 驱动在一条 256 KiB 栈的后台线程上，调用方只持 `Handle`——没有常驻多线程调度器与阻塞线程池。菜单「打开收件箱」下新增「立即同步邮件」（`syncMail:`），向守护进程发 `syncMessages({})`；失败只写一行 stderr，不崩进程。`maild-menu` 不直接依赖 `serde_json`，空对象参数经 `Value: FromIterator` 推断构造。

## 后果

- 默认配置下验证码误报大幅下降；代价是少数无关键词的验证码邮件（极简模板只发一串数字）不再弹验证码通知——开启 `notify_new_mail` 时它们仍作为普通新邮件提醒，需要旧行为的用户可把 pattern 改成非默认正则（如 `\b\d{6}\b`）。
- `rules_state.toml` 与 `reading_policy.toml` 同为 daemon 拥有的持久偏好，不进 SQLite：恢复一份旧邮件备份不应静默恢复旧的通知规则。
- 规则挂在账户 scope 与全局各一份时，两份新邮件监听器各自去重，可能各发一次；当前产品形态只装配全局 rules，留待按账户规则落地时统一。

## 下一阶段接线（已于全栈汇合完成，保留原记录）

- `paths.rs`：`effective.provide(RulesStatePath(rules_state_path_for(config_path)))`，与 `ReadingPolicy` 同处、load 之前。
- `api.rs`：`setSettings` 改用 `try_patch` 并按错误种类映射；`getSettings`/`setSettings` 的 `persisted` 改为 `settings.is_persisted()`（非持久模式仍为 `false`，既有断言不变）。
- `api.rs`：实现 `syncMessages`（菜单栏已调用；未实现前守护进程返回 method-not-found，菜单静默记一行日志）。
