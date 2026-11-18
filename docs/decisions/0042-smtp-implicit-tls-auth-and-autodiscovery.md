# ADR-0042：SMTP 隐式 TLS greeting、AUTH 机制协商与邮箱服务商自动发现

- 状态：已接受；内核修复、假服务器覆盖与 `autodiscover` 模块已实现并验收（2026-11-17）；同日全栈汇合（`5bcb9d6`）已接线 `discoverEmailProvider` / `listEmailPresets` RPC、`maildctl account discover` 与 Tauri 登录向导；release 守护进程经 SMTPS 465 + `AUTH LOGIN` 发信回环见 [testing](../testing/README.md#2026-11-17release-守护进程后台收信资源实测adr-0041--0042--0043-收官)
- 日期：2026-11-17
- 关联：[ADR-0005](0005-m3-compose-and-send.md)、[ADR-0015](0015-m3-7-account-management-and-credentials.md)、[ADR-0028](0028-compose-reply-and-sent-append.md)

## 背景

审计真实邮箱登录链路时定位到三处直接阻断主流邮箱（QQ、网易 163/126、阿里、腾讯企业邮、Fastmail、Zoho、Gmail 465、Exchange 等）的内核缺口：

1. **SMTPS 465 漏读 `220` greeting**。RFC 8314 §3.3 规定隐式 TLS 提交端口在 TLS 握手完成后，服务器**先发送 `220 <domain> ESMTP` 欢迎行**，客户端读完后才能 `EHLO`。`transport.rs` 的 `implicit` 分支以 `expect_greeting = false` 进入事务、`probe.rs` 的 `run_outgoing` 在隐式 TLS 下也直接 `EHLO`，于是把 `220` 当成 EHLO 响应：探测在所有真实 465 服务器上必报「服务器未声明 AUTH 能力」，发信则每一步错位一拍并在 `DATA` 处失败。`tests/common/fake_mail.rs` 从未实现 SMTP 隐式 TLS，所以一直没测出来。
2. **只会 `AUTH PLAIN`**。部分服务器（及老式网关）只宣告 `AUTH LOGIN` 或旧式 `AUTH=LOGIN`；且秘密拼接缓冲在发送后仍滞留堆上。
3. **无超时与不可行动诊断**。`TcpStream::connect("host:port")` 在双栈网络遇到 IPv6 黑洞时会挂到系统超时；会话内读写无单步超时，服务器假死会挂住发信；探测失败只有「TLS 握手失败」「认证被拒绝」，用户无从判断是端口配反还是没用授权码。

另外，添加账户向导需要一个不依赖外部服务的服务商识别引擎。

## 决策

### 1. 隐式 TLS greeting 状态机（RFC 8314 §3.3）

| 模式 | 明文阶段 | TLS 后 |
|---|---|---|
| `none` | 读 `220` → EHLO → … | — |
| `starttls` | 读 `220` → EHLO（须宣告 STARTTLS）→ `STARTTLS` 期望 `220` | **不再有 greeting**（RFC 3207），直接重新 EHLO |
| `implicit` | 无（connect 后立即握手） | **先读 `220`** → EHLO → … |

- `Transport::deliver` 的 `implicit` 分支改为 `run_transaction(tls, expect_greeting = true, …)`；`starttls` 保持 `false`。
- `probe::run_outgoing` 在 `SecurityMode::Implicit` 下握手后 8s 内读 greeting：读不到归 `ProbeStage::Tls`（附端口/加密方式提示），非 2xx 归 `ProbeStage::Connect`（服务器拒绝连接）。
- `FakeMail::start_smtp(SmtpTlsMode::Implicit, …)` 严格按 465 行为实现（握手后才发 `220 fake-mail ESMTP`），回归测试在去掉修复后 4 项失败。

### 2. AUTH PLAIN / AUTH LOGIN 协商与秘密缓冲清零

- `SmtpReply::auth_mechanisms()` 解析 EHLO 中所有 AUTH 能力行，同时兼容 `250-AUTH PLAIN LOGIN` 与旧式 `250-AUTH=PLAIN LOGIN`，关键字与机制名大小写不敏感，结果大写、去重、保序；`AUTHX` 之类前缀不误判。
- 选择规则（`select_auth_mechanism`）：宣告了 `PLAIN`，或只有裸 `AUTH` 未列子机制 → `AUTH PLAIN <b64(\0user\0pass)>`；未宣告 `PLAIN` 但宣告了 `LOGIN` → 两步 `AUTH LOGIN`（`334` → b64(user) → `334` → b64(pass) → `2xx`）；只宣告其他机制（如仅 `XOAUTH2`）→ 探测明确报「未提供 PLAIN/LOGIN 认证方式」。完全未宣告 AUTH 时，**发信**保留既有行为（硬试 PLAIN 由服务器裁决），**探测**报「服务器未声明 AUTH 能力」并提示使用 465/587 提交端口。
- 所有含秘密的临时缓冲（明文拼接、base64 结果、整行命令）用 `zeroize::Zeroizing` 包裹，写出后立即 `drop` 清零；错误文本只有阶段与响应码（`SMTP AUTH 被拒（收到 535）`），绝不含服务器原文或秘密。
- 认证失败按 `SmtpAuthErrorKind { Io, Temporary(4xx), Rejected(5xx/非预期码) }` 分类，probe 据此给固定文案，transport 直接使用无秘密的 `detail`。

### 3. 连接与单步超时

- 发信：`tokio::net::lookup_host((server, port))` 多地址解析（10s），逐个地址 10s 超时连接，失败带最后一个地址的错误；会话内每条命令写入、每个响应读取 30s；`DATA` 正文写入按大小放宽（30s + 每 16 KiB 1s，封顶 15 分钟），结束符后等待最终响应 180s（服务器常在此做内容扫描；超时仍按 `uncertain` 处理，不会误判为未投递）。TLS 握手 30s。
- 探测：逐地址 5s 连接超时；明文 greeting、隐式 TLS greeting 与 TLS 握手各 8s，保证在 15s 总 deadline 内给出**可行动**诊断而不是笼统的「探测超时」。

### 4. 可行动的探测诊断（不回显服务器原文）

- 端口与加密方式配反：`465/993 + STARTTLS` → 「端口 465 通常使用 TLS 直连，当前配置为 STARTTLS，请改为 TLS」；`25/143/587 + TLS` → 对称提示；其余端口给通用规则「993/465 通常使用 TLS，143/587 通常使用 STARTTLS」。附加在 TLS 阶段的握手失败、握手超时、greeting 失败与 TLS 阶段 deadline 超时上。
- 认证被拒（IMAP `LOGIN` 失败、SMTP `535` 等）：统一为「认证被拒绝（若使用 Gmail/iCloud/QQ/网易邮箱，请确认已开启 IMAP/SMTP 并填入应用专用密码或客户端授权码，而非网页登录密码）」，以 `probe::AUTH_REJECTED_DETAIL` 公开供前端识别。
- 纪律不变：detail 只能是本模块生成的固定文案。为容纳中文提示，模块内固定文案上限放宽到 480 字节；**外部来源**（provider 插件 detail、`accounts` / `receive` 调用的 `sanitize_detail`）仍按 160 字节截断与去控制字符。`ProbeStage` 取值与序列化不变，既有 `stage == "ready"` 断言与 `SMTP TLS 握手失败` 前缀均兼容。

### 5. `autodiscover` 知识库

`crates/maild/src/autodiscover.rs`，零外部依赖、全部类型 `Serialize/Deserialize/Clone/Debug/PartialEq/Eq`：

- `ProviderDiscovery { provider_id, display_name, source, confidence, incoming, mailbox, idle, outgoing, username_format, suggested_username, auth_kind, strip_secret_spaces, notes, help_url }`；`source ∈ preset | srv | tcp_probe | heuristic`，`confidence ∈ verified | high | guessed`；`mailbox = "INBOX"`、`idle = true` 为推荐值。
- `AuthGuideKind`：`app_password | authorization_code | account_password | local_bridge | oauth2_required_notice`；`UsernameFormat`：`full_address | local_part`。
- `list_presets()`：每服务商一张卡片（Gmail、iCloud、QQ、网易 163/126/yeah、腾讯企业邮、阿里邮箱个人版/企业版、Outlook、Fastmail、Yahoo、Zoho、Proton Bridge、新浪、搜狐、139、189）。
- `detect_from_address(input)`：纯本地、零网络；按域名精确匹配（含 `googlemail.com`、`me.com`、`mac.com`、`foxmail.com`、`vip.qq.com`、`vip.163.com`、`188.com`、`hotmail.com`、`live.com`、`msn.com`、`ymail.com`、`zohomail.cn`、`pm.me`、`protonmail.com` 等别名域）；同一服务商的地区/VIP 变体共享 `provider_id` 但给出各自正确的服务器（如 `vip.163.com` → `imap.vip.163.com`、`zohomail.cn` → `imap.zoho.com.cn`）。输入完整地址时 `suggested_username` 直接填好。
- Outlook 个人版诚实提示微软已停用基础密码认证（`oauth2_required_notice`），企业版或仍支持应用密码的账户可继续连接；Proton 走本机 Bridge（`127.0.0.1:1143/1025` STARTTLS），并提示需信任 Bridge 自签证书。
- `discover_provider(input, allow_network)`：先 `detect_from_address`；未命中则校验域名，`allow_network = true` 时在 **2.5s 总预算**内并发探测 `imap.<d>:993`、`mail.<d>:993`、`smtp.<d>:465`、`smtp.<d>:587` 的 TCP 可达性（握手成功立即断开，不发任何应用层字节），任一方向命中即返回 `tcp_probe / high`（未命中方向保留启发式值）；否则返回 `imap.<d>:993 TLS + smtp.<d>:465 TLS` 的 `heuristic / guessed`。
- 本阶段不做 SRV / MX 查询（`tokio` 只提供 A/AAAA 解析，引入 DNS 解析库超出依赖边界）；`source = "srv"` 为后续保留。

### 6. 自定义域名发现的 SSRF 边界

- **语法关**（`validate_public_domain`，任何网络动作之前）：小写化、去尾点；拒绝 IP 字面量（含 `[...]` 形式与纯数字顶级标签）、非 ASCII（要求 punycode）、单标签主机名、超长/非 LDH 标签，以及 `localhost`、`local`、`internal`、`intranet`、`lan`、`home`、`corp`、`localdomain`、`arpa`/`home.arpa`、`invalid`、`test`、`example`、`onion` 后缀。
- **解析关**（`is_public_ip`）：每个候选主机先解析，**任一**结果落在 loopback、RFC1918、link-local（含 `169.254.169.254` 元数据地址）、unspecified、CGNAT `100.64/10`、广播/组播、文档/基准测试保留段、`240/4`、IPv6 ULA `fc00::/7`、link-local `fe80::/10`、site-local、`2001:db8::/32`、`::/16`、`64::/16`（含 NAT64）或 IPv4 映射到上述段的地址，即整台主机放弃；只连接已校验的那个 `SocketAddr`，不二次解析，避免 DNS rebinding 换址。
- 预设里的 `127.0.0.1`（Proton Bridge）来自内置表，不是用户输入，不受此限制。

## 验收

- `crates/maild/tests/smtp_implicit_and_auth.rs`：465 隐式 TLS 的 `probe_outgoing` 与 `Transport::send_message`（中文主题 + 附件 + Cc，服务端字节与组装报文一致）端到端成功；`AUTH LOGIN` / `AUTH=LOGIN` 仅 LOGIN 服务器在隐式 TLS 与 STARTTLS 下探测与投递均回退成功且从不尝试 PLAIN；同时宣告时优先 PLAIN；错误密码探测停在 `auth` 阶段并给出授权码提示、发信返回 `535` 且不回显密码、不投递；端口/加密方式配反两方向都停在 `tls` 阶段并给出提示；`autodiscover` 覆盖 QQ、163、126、Gmail、iCloud、Outlook、Fastmail、Proton Bridge 精确匹配、别名域、序列化往返、启发式回退与 SSRF 拦截。
- `transport.rs` 单测覆盖 AUTH 机制解析两种语法、选择规则与 DATA 超时缩放；`probe.rs` 单测覆盖端口提示映射与固定文案/外部 detail 的不同截断上限；`autodiscover.rs` 单测覆盖 IP 过滤、输入解析、候选顺序与知识库域名唯一性。
- 门禁：`cargo test -p maild --all-targets --locked`、`cargo clippy -p maild --all-targets --locked -- -D warnings` 与 `--target x86_64-pc-windows-gnu` 交叉 Clippy 全绿。

## 后续

- ~~在 JSON-RPC / `maildctl` / Tauri 添加账户向导中接入 `list_presets` / `discover_provider`，并据 `strip_secret_spaces` 规整粘贴的应用密码~~（全栈汇合已完成）。
- OAuth2（XOAUTH2 / OAUTHBEARER）登录，覆盖 Outlook 个人版与 Gmail 无应用密码账户。
- 在依赖边界允许时补 SRV（RFC 6186）/ MX 查询，识别使用自有域名的腾讯企业邮、阿里企业邮与 Google Workspace / Microsoft 365。
