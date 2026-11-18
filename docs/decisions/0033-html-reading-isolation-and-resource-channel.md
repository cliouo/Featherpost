# ADR-0033：M4.5-04 HTML 原位阅读——隔离文档、资源通道与远程内容拦截

- 状态：已接受，M4.5-04 已实现（Rust 单测 + hermetic e2e + 真 WKWebView 计数验证）
- 日期：2026-09-12
- 适用范围：`crates/maild-ui`（新模块 `html`、`documents`；自定义协议 `maild-mail`；导航否决；六个新桥命令；
  主窗口改为程序化创建）、`packages/maild-ui`（阅读器重构、远程内容横幅、链接确认、附件条、
  「阅读与隐私」设置分区）、`examples/dev_stack.rs`（HTML fixture 与远程请求计数服务器）。
  **daemon 协议零改动。**
- 关联：ADR-0024 决策 5（渲染隔离边界）、ADR-0031（壳与桥）、ADR-0032（附件字节通道与策略结论）、
  [UX-SPEC](../design/tauri/UX-SPEC.md) §4、[DESIGN](../design/tauri/DESIGN.md) §2/§5

## 背景

用户对这个项目最明确的一句话是「肯定不能接受文本版邮件」。ADR-0024 因此换掉 egui，ADR-0031 立起
Tauri 壳，ADR-0032 补上 `cid:` 资源与远程内容策略的内核侧。到此为止，`body_html` 仍然**不出桥**——
03-A 的阅读器只显示纯文本，并诚实地写着「原位显示在后续版本提供」。本片就是那个后续版本。

难点不在「把 HTML 塞进 webview」，而在于邮件 HTML 是**不可信内容**，却要在**可信的应用 UI 同一个窗口里
原位显示**。要同时满足：脚本、表单、应用 IPC 不可执行；`cid:` 图片能显示；远程图片/样式/字体默认
不联网且**能证明**；链接能看清真实目标再决定；发件人版式（表格、内联 CSS、CJK）原样保留；长信
内部滚动不与外壳双重滚动；深色外壳下邮件保持原色。

## 目标

1. 有 HTML 的邮件默认以 HTML 原位显示；表格、内联 CSS、CJK、`cid:` 图片可读。
2. 邮件文档里的脚本、表单提交、`javascript:`/`file:` 导航、应用 IPC 一律不可用；远程资源默认零请求，
   且用**请求计数**而不是用「配置里写了 CSP」来证明。
3. 链接点击先显示真实目标；http/https 交系统浏览器；`mailto:` 与其他协议明确分类，不静默执行。
4. 远程内容按 ADR-0032 的内核结论显示；单封放行只活在本次会话；「始终允许」写回内核名单。
5. 附件条如实区分「可保存」与「尚未保存此附件」；保存用系统对话框，失败不动已有目标。

## 非目标（明确留给后续切片）

- 邮件的「适配深色」模式（DESIGN §？记为独立实验能力；本片深色外壳 + 原色邮件）。
- 自动标已读策略另行冻结；08-B 提供手动标记已读/未读。回复/转发/`mailto:` 写信由 08-A 完成，线程视图由 08-B 完成。
- 扩展阅读侧栏与工作页（E03）；打印、附件预览、超限附件回源。
- Windows/Linux 真机验证。协议 URL 形态按平台计算（决策 4），但本片只在 macOS WKWebView 上验收。

## 决策

### 1. 隔离形态：主 webview 里的 sandbox iframe，文档来自自定义协议

邮件文档以 `<iframe sandbox="" referrerpolicy="no-referrer" src="maild-mail://localhost/doc/<token>">`
嵌在阅读器里。`sandbox=""`（不给任何 allow-*）意味着：无脚本、不透明 origin、无表单提交、无弹窗、
不能导航顶层。文档本身由 Tauri 自定义协议 `maild-mail` 的处理器提供，响应带自己的
`Content-Security-Policy` 头。

放弃的形状：

| 形状 | 放弃理由 |
|---|---|
| `srcdoc` iframe | srcdoc 文档**继承父文档的 CSP**，宿主 CSP 就得放宽到邮件需要的程度（`img-src` 远程、`style-src 'unsafe-inline'` 全局），且无法给邮件文档单独的响应头 |
| 子 webview 叠在阅读区上 | 原生视图与 DOM 布局打架：toast/菜单/扩展侧栏（1360px+ 并排）都得绕着它；这正是用户否定 egui + webview 方案时说的 z-order 摩擦 |
| 独立窗口 | 不是「原位」；UX-SPEC §4 明令不提供必须另开窗口的「高清模式」 |
| 把 HTML 净化到白名单再直接渲染进宿主 DOM | 白名单会毁掉邮件版式（表格、废弃属性、`<style>`），而且任何漏网之鱼直接跑在宿主 origin 上——一个错误就是全部 |

### 2. 三层防线，每层独立成立

| 层 | 在哪 | 挡什么 |
|---|---|---|
| **sandbox 属性** | iframe 元素 | 脚本执行、同源访问、表单提交、弹窗、顶层导航、下载 |
| **文档级 CSP** | 协议响应头 + 文档内 `<meta http-equiv>`（两处同文，任一生效即够） | `default-src 'none'; img-src maild-mail: data: [https: http:]; style-src 'unsafe-inline'; font-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'` + `Referrer-Policy: no-referrer` |
| **Rust 侧重写与导航否决** | 字节进 webview 之前（`html` 模块）+ wry 导航回调（`on_navigation`） | 见决策 3、5、6 |

Rust 侧的两道门是**不依赖 WebKit 行为**的：重写发生在字节离开 Rust 之前，导航否决发生在
WebKit 询问「是否允许这次导航」的回调里——已核对 wry 0.55 的 `navigation_policy` 对**每一个**导航动作
回调，不区分主框架与子框架，所以 iframe 里的链接点击、`<meta refresh>`、任何 `http(s)`/`file:`/
`javascript:` 导航都在 Rust 里被 `Cancel`。`on_new_window` 一律 `Deny`（sandbox 已经挡了弹窗，这是第二道）。

**诚实边界**：这是浏览器引擎的 sandbox 加我们的两道 Rust 门，不是进程沙箱。一个 WebKit 自身的
sandbox 绕过漏洞不在我们能挡的范围内；我们能保证的是「邮件里写了什么，我们都当成不可信文本处理」，
以及「就算 CSP 没生效，重写后的文档也发不出远程请求、链接也不能把用户带去任何地方」。

### 3. 重写器用 `lol_html`，做映射与去势，不做白名单净化

`html` 模块用 Cloudflare 的 `lol_html`（HTML5 流式重写器，不构建 DOM，未触碰的字节原样输出）在
Rust 侧处理 `body_html`：

- **删除**：`script` / `iframe` / `frame` / `frameset` / `object` / `embed` / `applet` / `link` / `base` /
  `meta` / `input` / `button` / `select` / `textarea` / `video` / `audio` / `source` / `track`；
  `form` 删标签保内容（版式常靠它排布）；所有 `on*` 事件属性；`a` 的 `target` / `ping` / `download`。
- **`cid:` 映射**：`img/input/table/td/th/body/image/use` 的 `src` / `background` / `href` / `xlink:href`
  里的 `cid:<id>` → `maild-mail://localhost/part/<token>/<part_index>`（按 `content_id` 查本封附件表）；
  找不到的 `cid:` 去掉属性并计数 `cid_missing`。`<style>` 文本与 `style` 属性里的 `url(cid:…)` 同样映射。
- **远程引用**：`http(s)://` 与 `//` 开头的 `src` / `srcset` / `background` / `poster` / CSS `url()`
  计数 `remote_refs`。**阻止时**把 `src` 移到 `data-maild-src`、删掉 `srcset`、CSS 里替换成
  `url("data:,")`——不是只靠 CSP 拦：这样即使 CSP 失效也发不出请求，而且不会满屏破图标。
  放行时原样保留，由 CSP 的 `img-src https: http:` 放过。`@import` 一律删除并计数。
- **链接**：`a/area` 的 `href` 分类为 `http` / `https` / `mailto` / `other`（含 `javascript:`、`file:`、
  `data:`、`cid:` 与相对地址），登记进本文档的链接表，属性改写成 `maild-mail://localhost/link/<token>/<n>`
  ——与文档同一个**已注册**协议下的惰性路径（见决策 6 的实测结论）；`#片段` 原样保留（文档内跳转）。
  原文里指向我们协议的字面引用一律当普通链接重新编号，资源属性里的则删除：邮件不能自己伸手进
  协议的 `/doc/`、`/part/`，也不能伪造 `/link/` 序号。
- **其他协议的 `src`**（`file:`、`ftp:`、相对路径）删除并计数 `dropped_refs`。
- **不注入版式**：只在 `<head>` 前置 `<meta charset="utf-8">`、CSP meta、`<meta name="referrer" content="no-referrer">`。
  没有字体、没有 dark CSS、没有行高（DESIGN §2）。

为什么不用 `ammonia` 一类白名单净化器：白名单是「只保留我认识的」，而邮件 HTML 的现实是 `<center>`、
`<font>`、`bgcolor`、`cellpadding`、成段的 `<style>`——全是白名单默认不认识的东西。用户要的是版式
如实；安全由 sandbox + CSP 承担，重写器的职责是**映射与去势**，不是判定哪些标签「安全」。

重写器住在 `maild-ui`（前端壳的 Rust 侧）而不是内核：ADR-0025 刻意让内核不养 HTML 解析器
（`mime.rs` 的注释就是这么写的），协议文档也一直说「内核不做净化」。换一个前端换一套重写策略是
正当的；换一个前端换一套隐私策略不是——所以策略在内核（ADR-0032），重写在前端。

### 4. 令牌作用域的资源通道

`documents` 模块持有进程内的 `DocumentStore`：每次 `ui_prepare_html` 生成 128 位随机令牌，记下
`(account, folder, uid)`、重写后的文档、链接表、`cid → part_index` 表与放行决定。协议处理器只认令牌：

- `GET maild-mail://localhost/doc/<token>` → 重写后的 HTML（`text/html; charset=utf-8`，CSP 头，`no-store`）；
- `GET maild-mail://localhost/part/<token>/<part_index>` → 经**前端主体的认证连接**调 `readAttachment`
  取字节（ADR-0032），响应带原 `mime_type` 与 `X-Content-Type-Options: nosniff`；`stored = false`、
  part 不属于该令牌的信、令牌已释放 → 404。
- 其余路径 404。令牌在切信时由 `ui_release_html` 释放，且最多同时保留 8 份（LRU）。

**跨信件拒绝**的形状就是这个：一份文档拿不到别的信的字节，因为它只知道自己的令牌，而令牌只绑一封信。
URL 里没有账户、文件夹、UID——webview 侧从头到尾不知道消息身份以外的任何 daemon 细节
（ADR-0031 决策 4 的纪律不变）。

平台差异：macOS/Linux 的形态是 `maild-mail://localhost/…`，Windows 是 `http://maild-mail.localhost/…`
（Tauri 文档）。基址由 Rust 按平台计算并随 `UiDocument.url` 交给 UI，导航否决同样按平台形态匹配。
本片只在 macOS 验收。

宿主 CSP（`tauri.conf.json`）同步：`frame-src maild-mail:`，其余不放宽。

### 5. 远程内容：内核给结论，UI 给两个动作，一个持久一个不持久

- 默认按 `getMessage.remote_content.decision`（ADR-0032）准备文档；`block` 时横幅写
  「已拦截 N 处远程内容」（N = `remote_refs`）。
- 「**加载本封**」：以 `allow_remote = true` 重新准备，只记在本次会话的内存里（按消息键），不持久、
  不进内核——UX-SPEC §4「不把单封『加载图片』升级为永久信任」。
- 「**始终允许来自 <地址>**」：`setReadingPolicy { trust: [addr] }`（需要 `settings_write`，默认前端已声明），
  再重新准备。名单管理在「设置 → 阅读与隐私」。
- 结论为 `trusted_address` / `trusted_domain` 时横幅写明依据并提供「停止信任」；`allow_all` 时写明
  「已按设置全局加载」。
- **无论放行与否**，远程样式表、网络字体、媒体永远不加载（`style-src` 无外部源、`font-src 'none'`、
  `media-src 'none'`）；放行只放图片。`Referrer-Policy: no-referrer` 始终生效。

### 6. 链接：先看见，再决定

点击 → iframe 尝试导航到 `maild-mail://localhost/link/<token>/<n>` → Rust `on_navigation` 取消导航
（取消的导航不会卸载当前文档，邮件原地不动）并发事件 `maild://link { token, index, href, kind, host }`
→ UI 显示链接确认面板：完整目标、主机名；`http`/`https` 给「在浏览器中打开」（`ui_open_link` 在
Rust 侧**再次**校验协议后交系统：macOS `open`、Linux `xdg-open`、Windows `rundll32 url.dll,FileProtocolHandler`）
与「复制链接」；`mailto` 给「复制地址」并说明写信归后续版本；`other` 写明「此链接使用 X 协议，
Maild 不会执行它」。⌘点击 / 中键无效（sandbox 挡弹窗，`on_new_window` Deny）——如实写在文档里。

**实测修正**：最初的设计把链接改写成一个**未注册**的 `maild-link:` scheme，指望「不注册处理器，
导航必到 delegate 被取消」。真 WKWebView 上它到不了：sandbox 框架里指向未注册 scheme 的导航被
WebKit 静默丢弃，点击没有任何反应。同一协议的 `/doc/` 导航已被证明能到 delegate（日志「放行文档
导航」），所以链接改到该协议的 `/link/` 路径；协议处理器对 `/link/` 只回 404，而 `on_navigation`
在请求到处理器之前就取消了它。这正是 UX-SPEC §4「真实拦截能力需 WKWebView 验收」说的那种事。

### 7. 阅读区布局：可信头部 + 独立滚动的正文 + 附件条

阅读区改为三段：可信 UI 的头部（主题、发件人、收件人、状态、远程内容横幅）不滚动；正文区占满
剩余高度——HTML 时是 iframe 自带滚动（长信内部纵滚、超宽表格内部横滚，不与外壳双重纵滚，
UX-SPEC §4/DESIGN §5），纯文本时是 `<pre>` 自滚；附件条固定在底部。iframe 元素背景恒为白：
邮件没设底色时在深色外壳下仍是纸色（DESIGN §7「深色外壳 + 原色邮件」），不注入任何 invert。

正文如何呈现的真值表：

| 事实 | 呈现 |
|---|---|
| `has_html` 且准备成功 | HTML 文档 |
| `has_html` 但准备失败 | 纯文本 + 错误横幅（原因是四类固定文案） |
| `!has_html` | 纯文本 + 注记「此缓存中没有 HTML 版式」 |
| 末尾有截断标记 | 额外注记「此缓存中的正文不完整（超过 256 KiB 的部分未保存）」 |

注记的措辞是中性的「此缓存中没有」：内核**不记录**一封信原本有没有 HTML（ADR-0025 之前的历史缓存
与纯文本邮件在库里长得一样），所以 UX-SPEC 那句「不能伪称原邮件只有文本」靳能靠不做断言来满足。
如实记为限制。

### 8. 附件条与保存

附件条只列**非内联**附件（`cid:` 图已在正文里，UX-SPEC「CID 图不重复列为普通附件」），显示文件名、
类型、大小；`stored` 为真给「保存」，为假写「尚未保存此附件」且没有按钮。保存：`ui_save_attachment`
在**主线程**弹系统保存面板（`rfd` 的 macOS 后端要求主线程运行模态面板），用户选定路径后经
`read_attachment` 取字节、写同目录临时文件、fsync、`rename` 到目标——面板已替用户处理重名确认，
写失败时已有目标一个字节不动。

### 9. 「阅读与隐私」是邮箱模块贡献的设置分区

远程内容默认策略与可信发件人名单的管理界面由 `maild.mailbox` 模块经 E01 注册底座贡献一个
`settings` 分区，与示例扩展走同一条路——不在设置模块里硬编码。它读写 `ui_reading_policy` /
`ui_set_reading_policy`，改动即时反映到当前打开邮件的横幅。

### 10. 主窗口改为程序化创建

`on_navigation` / `on_new_window` 只能挂在 `WebviewWindowBuilder` 上，所以主窗口从
`tauri.conf.json` 的 `app.windows` 移到 `setup` 里用同样的参数创建。窗口参数不变。

## 兼容性

- daemon：零改动。本片只消费 ADR-0032 已有的 `getMessage.remote_content`、`attachments[].stored`、
  `readAttachment`、`getReadingPolicy` / `setReadingPolicy`。
- 桥 DTO：`UiMessage` 新增 `remote_content`，`UiAttachment` 新增 `stored`（additive）；新增
  `UiDocument`、`UiReadingPolicy`、`UiLink` 与六个命令；`capabilities/main.json` 逐条列出。
- 前端包 manifest：不变（`settings` 路由已含 `settings_write`）。
- 宿主 CSP：`frame-src 'none'` → `frame-src maild-mail:`。

## 测试策略

- `html` 单测（敌意 fixture）：`<script>`（含 svg 内）、`<iframe>`、`<object>`、`<form>`、`<base>`、
  `<meta refresh>`、`on*`、`javascript:` / `file:` 链接与 `src`、`srcset`、CSS `url()` 与 `@import`、
  原文里写死的 `maild-mail:` 引用——逐项断言输出里不存在；`cid:` 映射到正确 part、缺失 cid 计数；
  远程引用阻止时无 `src` 且计数正确、放行时保留；链接表序号与原 href 对应、片段链接原样；
  CJK 文本、表格与 `<style>` 内容逐字节不变；截断标记识别；CSP 字符串两种形态。
- `documents` 单测：令牌随机且不可猜；`part` 只认自己的令牌与本封 part；释放后 404；LRU 上限；
  导航裁决函数——应用自身 URL 与 `/doc/` 放行，`/part/` 导航拒绝，`maild-link` 解析成链接事件，
  `http(s)` / `file:` / `javascript:` 一律拒绝。
- `tests/bridge.rs` e2e（真 daemon）：对预置的 HTML + `cid` 附件准备文档→文档里出现 part URL 且不含
  `<script>`→按 URL 取 part 字节等于预置→释放后取不到；`remote_content` 与 `stored` 投影正确。
- vitest：文档控制器（准备/换信作废迟到应答/单封放行会话内记忆/信任后重准备）、呈现真值表、
  附件条过滤与文案、链接分类。
- **真 WKWebView 计数验证**：`dev_stack` 增加一封 fixture 邮件（表格 + 内联 CSS + CJK + `cid:` PNG +
  指向本机计数服务器的远程图片 + `<script>alert>` + `<form>` + 各类链接 + `<meta refresh>`）与一个
  记录命中数的本机 HTTP 服务器。Orca computer-use 驱动：打开邮件 → 截图（表格/CJK/`cid` 图可读、无弹窗）
  → 计数为 0 → 「加载本封」→ 计数 ≥ 1 → 点链接 → 面板显示完整目标。数字与截图记进 PROGRESS。

## 后果

正：用户最在意的那件事第一次在默认前端里成立，且隔离的每一层都有自己的测试；`cid:`、远程策略、
链接、附件保存走完了从内核到界面的整条链；重写器与文档存储不依赖 Tauri，能在无窗口的测试里跑。

负：`maild-ui` 多了 `lol_html` 与 `rfd` 两棵依赖树；重写不是白名单，版式忠实的另一面是「我们不理解
邮件里每一个标签」——安全命题必须始终依靠 sandbox + CSP + 导航否决三层同时成立，任何一层都不能
被当成可选；主窗口配置从 JSON 挪进代码，少了一处可读的声明。Windows 的协议形态只有代码路径没有
验收。

## 参考

- ADR-0024 决策 5、ADR-0031 决策 4/9、ADR-0032 决策 3/5/6
- wry 0.55 `wkwebview/navigation.rs::navigation_policy`（每个导航动作都回调）
- Tauri 2.11 `Builder::register_asynchronous_uri_scheme_protocol`、`WebviewWindowBuilder::{from_config, on_navigation, on_new_window}`
- [UX-SPEC.md](../design/tauri/UX-SPEC.md) §4、[DESIGN.md](../design/tauri/DESIGN.md) §2/§5/§7
