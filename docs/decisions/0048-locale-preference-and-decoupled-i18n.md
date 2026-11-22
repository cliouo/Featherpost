# ADR-0048：语言偏好归内核、翻译归各前端——可替换前端下的 i18n

- 状态：提议（草稿，未实现）
- 日期：2026-11-22
- 关联：[ADR-0014](0014-m3-6-frontend-activation-and-plugin-lifecycle.md)（前端激活与可替换前端）、
  [ADR-0028](0028-compose-reply-and-sent-append.md)（回复引用块文案的「诚实边界」）、
  [ADR-0030](0030-frontend-authorization-closure.md)（授权词表）、
  [ADR-0031](0031-tauri-frontend-shell-and-contribution-registry.md)（默认前端技术栈与偏好存储边界）、
  [ADR-0032](0032-attachment-byte-channel-and-remote-content-policy.md)（daemon 拥有的偏好状态文件纪律）、
  [ADR-0038](0038-settings-and-managed-packages.md)（包预览由当前前端渲染）、
  [ADR-0039](0039-native-recovery-and-tauri-default.md)（原生启动器与修复入口）、
  [ADR-0047](0047-decoupled-brand-and-open-source-readiness.md)（开源就绪）

## 背景

界面只有中文，且没有 i18n 框架：

- 前端约 60 个 `.ts/.tsx` 文件直接写中文字面量；`packages/maild-ui/src/i18n.ts` 只有
  `UI_LOCALE = "zh-CN"`、系统文件夹名、生命周期与权限名三张小表。
- `crates/maild-ui/src/macos_locale.rs` 把 Tauri 默认英文菜单逐项换成中文。
- Rust 侧约 125 个文件含中文字符串，其中大部分是日志与配置报错，少数是用户可见的内核界面：
  系统通知（如 `otp.rs` 的「验证码」标题与正文格式）、菜单栏 companion、原生启动器对话框。
- `prepareReply` 的引用块（「在 …，… 写道：」）只有中文，ADR-0028 决策 7 把它标为后续切片。

直接套用 Web 项目的做法（URL 前缀 → cookie → 浏览器语言，一个前端一套 Provider）在这里不成立，
原因是 ADR-0014 定下的架构：**前端可替换**。用户可以安装第三方前端并设为默认，内核还有自己的
通知、菜单栏与启动器界面。于是「当前用哪种语言」不能由某一个前端决定，也不能存在某个 webview
的 cookie/localStorage 里；而「用什么翻译框架、文案怎么写」又不该由内核替第三方决定。

## 目标

1. 用户只在一处表达语言偏好，内核界面、默认前端与第三方前端读到同一份。
2. 内核不规定前端的翻译技术、文案风格与支持哪些语言。
3. 协议保持语言中立：前端渲染文字所需的一切都以结构化数据与稳定 code 提供。
4. 默认前端有一套带编译期/构建期守门的翻译方案，防止中文字面量回流。

## 非目标

- 不翻译日志、配置报错、`error.message` 等面向开发者的文字。
- 不翻译用户或服务商数据：显式的文件夹显示名、账户名、邮件内容。
- OTP 识别关键词（`otp.rs` 的「验证码 / 校验码 / 动态码 …」）是**内容识别规则**，与界面语言无关，
  不随语言切换。
- 不做 RTL 布局、运行时下载语言包、复数规则引擎（需要时用 `Intl.PluralRules`）。
- `maildctl` 输出与已冻结的 egui 恢复界面本 ADR 不迁移；`maildctl` 留到最后一个切片。

## 决策

### 1. 语言偏好是 daemon 拥有的持久状态

与「默认前端」「远程内容策略」同级，存在活动配置同目录的 `locale_preference.toml`，文件纪律照搬
`reading_policy.toml`（ADR-0032 决策 5）：0600、当前用户拥有、单硬链接、拒符号链接、64 KiB 上限、
未知版本/字段 fail-closed、临时文件 + `fsync` + 原子 rename；只读从不创建文件；首次写入才创建。
`maild backup` 不覆盖它（是偏好，不是邮箱数据）。

```toml
format_version = 1
mode = "explicit"          # "system" | "explicit"
languages = ["en", "zh-Hans"]
```

- 缺省（文件不存在）为 `mode = "system"`。
- `explicit` 时 `languages` 为 1..=8 个 BCP 47 标签，每个不超过 35 字节，规范化大小写后去重存储。
- 文件损坏或来自未来版本：读到缺省偏好，`persisted = false`，写入返回 `-32001` 且绝不覆盖磁盘文件。

**为什么不让各进程各自读系统语言**：macOS 允许按 app 单独设置语言，而 Studio、Agent、Menu、
启动器是不同 bundle，各读各的会出现「托盘是英文、窗口是中文」。由 Agent 统一解析一次再下发，
所有界面才能保持一致。

### 2. 协议下发有序偏好列表，不下发「定好的一种语言」

每个前端支持的语言不同（第三方可能只做了英文，也可能多做了日文），内核不替它挑。协议给的是用户的
有序偏好，类似浏览器的 `Accept-Language`，由各前端与自己支持的语言协商。

新增方法：

| 方法 | 授权 | 说明 |
|---|---|---|
| `getLocalePreference` | `Open` | 语言偏好不敏感；后台插件也可能要渲染文字（如自己的通知），与 `getAgentInfo` 同级 |
| `setLocalePreference` | `settings_write` | 与 `setSettings` / `setReadingPolicy` 同属应用级开关域，**不新增授权词** |

`getLocalePreference` 应答：

```json
{
  "preference": { "mode": "system", "languages": [] },
  "effective": ["zh-Hans-CN", "en-US"],
  "persisted": true
}
```

- `effective` 是**解析后的有序列表**：`explicit` 时等于 `languages`；`system` 时是 Agent 读到的系统首选语言
  （macOS：`CFLocaleCopyPreferredLanguages`）。列表永不为空；系统读取失败时为 `["zh-Hans"]`。
- `setLocalePreference` 参数为 `{mode, languages?}`：`mode = "system"` 时不得带 `languages`；
  `explicit` 时 `languages` 必填且合法，任一项非法整包 `-32602` 不落盘。成功应答与 `get` 同形。

新增服务端通知 `preferences/locale`，`params` 为 `{"effective": [...]}`，在 `setLocalePreference` 成功
或 `system` 模式下系统语言变化时推送给所有流式连接。尽力推送；丢失时前端在下次聚焦或重连时
`getLocalePreference` 即可追上。

> 实现时核验：Agent 作为登录项进程，`CFLocaleCopyPreferredLanguages` 是否受它自身的按 app 语言设置影响；
> 系统语言变化的分布式通知（`AppleLanguagePreferencesChangedNotification`）是否可靠送达。不可靠则退化为
> 每次 `getLocalePreference` 重读系统值。

> **L1 实施记录（偏差）**：
>
> - 系统语言变化**未订阅**分布式通知，直接取上面的退化路径：`system` 模式下每次 `getLocalePreference`
>   （以及 `setLocalePreference` 的应答）都重读系统值；`preferences/locale` 只在 `setLocalePreference`
>   成功且 `effective` 变化时推送。理由：Agent 的 daemon 跑在独立线程的 tokio 运行时里，主线程才有
>   AppKit run loop，订阅需要跨线程把信号送回事件总线，且送达可靠性无法在 hermetic 测试里核验；
>   前端「聚焦或重连时重读」已能追上。后续若要主动推送，在 Agent 主线程订阅后调用同一个
>   `LocalePreferenceChanged` 事件即可，协议不变。
> - macOS 读的是 `+[NSLocale preferredLanguages]`，与 `CFLocaleCopyPreferredLanguages` 同源（toll-free
>   bridged），复用 crate 已有的 objc2-foundation 绑定。按 app 语言设置是否影响 Agent 自身的读取**未核验**
>   （仍是开放问题）。其他平台按 `LC_ALL` → `LC_MESSAGES` → `LANG` 取第一个非空值。
> - 系统值同样经 BCP 47 规范化、跳过非法项、去重，并截到 8 项，使应答与推送帧有界。
> - 共享 client 没有给 `ServerNotification` 加新变体（既有消费者 `maild-app` 对它穷尽匹配），改为
>   `Notification::locale_preference_changed()`；该通知经 `ServerNotification::Unknown` 送达。

### 3. 协议本身语言中立

- daemon 返回结构化数据与稳定 code，前端负责成文。既有做法照旧：文件夹给 `role`，由前端翻译；
  显式显示名是用户/服务商数据，原样展示。
- `error.message` 面向开发者，**语言不属于协议契约**，客户端不得据此分支或直接作为用户提示
  （协议文档「错误码」一节已有此约定）。界面需要区分具体原因时，在对应方法的 `error.data` 增加稳定的
  `reason` 字符串；前端遇到不认识的 `reason` 显示通用提示，详情中附上原始 `message`。
- 新增的方法与字段遵循同一规则：不返回给人看的成品句子。

### 4. 每个前端自带翻译，内核不规定技术与文案

- 第三方前端对协议的义务只有三件：读 `getLocalePreference`、与自身支持的语言协商、监听
  `preferences/locale` 重新渲染。翻译框架、文案风格、支持哪些语言完全自定。
- **权限说明不由内核锁定。** 授权词本身（`mailbox_write`、`plugins_manage` …）是 ADR-0030 的稳定契约；
  每个词允许调用哪些方法、意味着什么，写在协议文档里作为参考，前端各自翻译与措辞。
  理由：按 ADR-0038，包预览由 daemon 校验后把权限词交给**当前已在使用的前端**渲染，被安装的包在安装前
  无法控制任何显示文字；能决定措辞的只有当前默认前端，而它本身已持有这些权限。锁定文本挡不住真实威胁，
  只会把默认前端的文案风格强加给第三方。

### 5. 跨前端展示的包元数据：manifest 可选 `l10n` 段

扩展与前端的名称、描述会出现在任何一个前端的设置页里，只能由包作者提供翻译：

```toml
name = "Featherpost Studio"        # 兜底，必填
description = "..."                # 兜底，可选

[l10n.en]
description = "..."

[l10n."zh-Hans"]
name = "Featherpost 工作室"
description = "..."
```

- `l10n` 下的键为 BCP 47 标签，值只允许 `name` 与 `description`，长度上限与兜底字段相同；最多 16 种语言。
- `listFrontends`、`listExternalPlugins`、`previewPluginPackage` 原样返回 `l10n` 映射，**由前端协商**，
  daemon 不按偏好挑选。
- 实现时核对 `frontend.toml` / `plugin.toml` 解析器对未知键的处理：若当前拒绝未知键，`l10n` 随
  `schema_version = 3` 引入，旧 daemon 对 v3 包按既有规则拒绝安装。

> **L4 实施记录（偏差）**：
>
> - 两个解析器都是 `deny_unknown_fields`，旧 daemon 见到 `l10n` / `description` 会拒绝整包，所以走**新 schema
>   版本**而不是 v2 内追加。但两份 manifest 的当前版本不同：`frontend.toml` 当前最高 v2，随 **v3** 引入；
>   `plugin.toml` 当前只有 v1，随**下一个版本 v2** 引入（而不是跳到 3）。新版本是旧版本的超集；旧版本 manifest
>   写了这两个键整包拒绝（与 `permissions` 之于 frontend v1 同理），v1/v2 包照原规则安装与运行。
> - 两份 manifest 此前都没有顶层 `description`，按本决策新增为可选兜底字段：非空、≤1024 字节、无控制字符。
>   各语言 `name` 与兜底 `name` 同限（160 字节）、`description` 与兜底同限；标签用
>   `locale_preference::normalize_tag` 校验并规范大小写，规范化后重名拒绝；每项至少一个字段。
> - 三个方法返回规范化后的映射，空时省略键；`description` 缺省同样省略。`previewPluginPackage` 的这两个字段
>   参与 `expected_preview` 逐字段比对。共享 client 的 DTO 为 `PackageL10n`（`BTreeMap<String, PackageL10nEntry>`）。

### 6. 内核自己的界面：`maild-l10n` crate

内核界面的语言 = 用 `effective` 列表与内核支持的语言（首版 `zh-Hans`、`en`）协商的结果。范围：

- 系统通知（新邮件、验证码等）：在**发出通知时**按当时的偏好成文；
- 菜单栏 companion（`maild-menu`）；
- 原生启动器与修复对话框（`maild-launcher`，ADR-0039）。

实现为一个无依赖的小 crate：每种语言一个 `const` 结构体（字段即消息，带参数的消息为函数），
**缺字段编译失败**，不需要额外的 key 一致性检查。要翻译的字符串只有几十条，Fluent 或 `rust-i18n`
在这个规模上收益不抵依赖成本；条目明显增多时再评估。

日志、配置报错、`error.message` 不进 `maild-l10n`（见非目标）。

> **L2 实施记录（偏差）**：
>
> - **通知**：验证码与新邮件通知在成文时读 `LocalePreference::effective()` 协商；上下文里没有偏好服务时按
>   `FALLBACK_LANGUAGE`（`zh-Hans`）协商。`system` 模式下 `effective` 每次重读系统语言，所以系统语言变化后
>   下一条通知也会跟随。
> - **验证码标题**：`OtpConfig::title` 改为可选，缺省时使用本地化的 `otp_title`。旧版启动器首启模板与旧配置
>   升级会把 `title = "验证码"` 写进用户的 `config.toml`，所以配置值**逐字节等于**旧出厂值「验证码」时同样
>   视为未自定义；其他用户写的标题原样使用，不随语言切换。启动器首启模板不再写 `title`；旧模板的精确匹配
>   升级改写成新模板，因此也不再写它。
> - **菜单栏**：companion 没有常驻的流式连接（每个操作各开一次短连接），因此**不监听** `preferences/locale`。
>   改为在 `menuNeedsUpdate:`（菜单即将显示时）重读 `getLocalePreference`，语言变了就重建菜单项。主线程
>   最多等 250 ms，超时则先按上次的语言显示，迟到的应答在下次打开时生效。这样做也能跟上 `system` 模式下的
>   系统语言变化（L1 不推送这种变化）。为了「切换偏好后菜单随之变化」而常驻一条连接，不值得。
> - **启动器**：同样没有常驻连接。修复窗口每次刷新状态时（每个对话框循环开始时）重读一次偏好。
>   在连上 daemon 之前就要显示的报错按系统语言显示。`resyncBuiltinFrontend` 回退到 `repairBuiltinFrontend`
>   的判断改为按 RPC 错误码 `-32601`，不再比较本地化后的文案。`maild-client` 的应用路径校验报错
>   （中文诊断）在启动器边界改为写日志，对话框显示本地化的「应用不完整」。clap 的 `--help` 文案归 L5，
>   本切片不改。

### 7. 回复引用块：`prepareReply` 增加可选 `locale`

引用块会写进发出去的邮件正文，属于**邮件内容的语言**，可以与界面语言不同（例如中文界面回复英文邮件）。

- `prepareReply` 增加可选参数 `locale`（BCP 47）。daemon 用它与 `maild-l10n` 支持的语言协商；
  缺省时用 `effective` 列表协商。
- 主题前缀归一照旧识别全部已知前缀（`Re:` / `答复:` / `回复:` / `Fwd:` / `转发:` …），与 `locale` 无关。
- 默认前端首版直接传界面语言；「按原信语言回复」留给后续。

> **L4 实施记录**：`locale` 接受字符串或 `null`（等于缺省），用 `locale_preference::normalize_tag` 校验，非法
> `-32602`。给了 `locale` 时只用 `[locale]` 协商，**不**再回落到 `effective`：协商不出结果（如 `ja`、`zh-Hant*`）
> 回落 `zh-Hans`，与 `maild-l10n::negotiate` 一致。回复引用行、转发头与其中的人读日期都来自协商结果；
> RFC 5322 回退日期与语言无关。没有装配偏好服务（纯 store 装配）时按 `effective = ["zh-Hans"]` 处理。
> 共享 client 的 `prepare_reply` 多一个 `locale: Option<&str>` 参数，`None` 时不发送该键，旧 daemon 照常可用。

### 8. 默认前端的方案

`maild-ui` 与 `featherpost-studio` 共用一份消息：

- 新增 `packages/i18n`，用 **Paraglide JS v2**（inlang）以 CLI 在此**编译一次**，两个包都从这里导入；
  不在两个 Vite 配置里各挂一个插件（会各生成一份产物，容易不一致）。
- 消息为 JSON：`packages/i18n/messages/{zh-CN,en}.json`，`zh-CN` 为基础语言。JSON 便于开源后社区翻译。
- **语言策略只用自定义策略**：从桥拿 `effective` 列表 → 与 `["zh-CN", "en"]` 协商 → 失败回落 `zh-CN`。
  不用 URL、cookie、`navigator.language`（WKWebView 下的 `navigator.language` 与 app bundle 声明的
  本地化相关，不可靠）。收到 `preferences/locale` 后切换语言并重新渲染，不重载页面。
- 协商规则：按语言子标签匹配，`zh` 额外比较书写系统——`zh-Hans*`、无书写系统的 `zh` / `zh-CN` / `zh-SG`
  匹配 `zh-CN`；`zh-Hant*` 不匹配，继续看下一项偏好。
- key 用合法标识符、按模块分前缀：`mailbox_*`、`compose_*`、`settings_*`、`extensions_*`、`studio_*`，
  调用形如 `m.mailbox_inbox()`，不用 `m["a.b.c"]`。
- 构建前守门 `i18n:lint`：两种语言 key 集合与占位符一致；key 前缀合法；调用方没有引用不存在的 key；
  **组件中不得出现中文字面量**（迁移期用白名单文件列表，逐步收紧到空）。
- 类型：先用最小样例验证 Paraglide v2 产物在 `tsc` 下能否直接报出 key 拼错与参数错；只有确实存在缺口
  时才补 `i18n:dts` 脚本。
- 现有 `packages/maild-ui/src/i18n.ts`：`folderLabel` 的别名匹配逻辑保留，中文标签换成消息函数；
  `UI_LOCALE` 常量换成当前协商结果（`Intl` 格式化同样使用它）。
- 默认前端的 Tauri 原生菜单（`macos_locale.rs`）属于默认前端而非内核，跟随该前端的协商结果，
  由壳从同一份偏好取得语言，不另读系统设置。

> 实现记录（L3 前半，`packages/i18n`，细节见该包 README）：
>
> - 消息文件改为每种语言一个目录、每个命名空间一个文件：`messages/{zh-CN,en}/<命名空间>.json`，命名空间即
>   key 前缀（`common`、`mailbox`、`compose`、`settings`、`extensions`、`studio`）。inlang 消息格式插件的
>   `pathPattern` 支持数组，编译结果与单文件相同；分文件是为了让各模块的迁移分支并行合入时互不冲突。
> - 编译产物不提交，由本包 `postinstall` 与两个前端的 `dev/build/typecheck/test` 脚本生成；插件从
>   `node_modules` 本地加载，编译不联网。
> - 类型：Paraglide 以 `--emit-ts-declarations` 生成的声明已能让 `tsc` 报出 key 拼错与缺参、参数名错，
>   不需要 `i18n:dts`。
> - 协商补充：无书写系统的 `zh-<地区>` 只有 `TW`、`HK`、`MO` 视为繁体，其余地区（如 `zh-MY`）匹配 `zh-CN`。
>   `maild-l10n` 的协商函数应采用同一规则。
> - `UI_LOCALE` 常量换成函数 `uiLocale()`（`maild-ui/src/i18n.ts`），每次调用返回当前协商结果。

> 实现记录（L3 后半，壳与默认前端接线）：
>
> - 数据通路：daemon → `maild-client` 的 `ServerNotification::LocalePreference { effective }`（新增的类型化
>   变体，取代决策 2 L1 记录里「经 `Unknown` 送达」的过渡做法）→ `maild-app` actor 的 `ViewState.locale` →
>   Tauri 壳状态泵在列表变化时发 `maild://locale`（载荷 `{effective}`）→ `bridge/tauri.ts` 调
>   `applyEffectiveLocales`。启动值经 `ui_locale` 取壳手里的缓存，不发请求；推送先到则不再用启动值。
> - actor 在每次（重）连接与刷新的快照里多发一个 `getLocalePreference`，推送即时生效且优先于同一轮快照里
>   更早的应答。**偏差**：老 daemon 对前端主体不会回 `-32601`——未登记方法对插件主体 fail-closed，回的是
>   `-32016`；所以读偏好时 `-32601` 与 `-32016` 都按「后台不提供」处理，`locale` 为 `None`，界面停在
>   `zh-CN`。超时等其他失败保留上一次的值，也不记为部分快照。HTTP 会话（恒为 user、无推送）不取偏好。
> - 桥命令：`ui_locale`、`ui_locale_preference`、`ui_set_locale_preference`（`settings_write`；两个默认前端
>   经 `settings` 路由已有），三处清单（`build.rs`、`capabilities/main.json`、`generate_handler!`）同步登记。
> - 决策 3 在壳内的落实：桥的 `UiError` 增加稳定 `reason`，webview 按它用当前界面语言成文
>   （`bridge/reasons.ts`，前端测试核对 Rust 侧每个 `reason` 都有映射）；中文 `message` 只作诊断兜底，
>   不认识的 `reason` 显示通用提示。
> - 原生菜单：状态泵用 `maild_l10n::negotiate` 协商（与 webview 同一规则），结果变化时在主线程重建并替换
>   菜单——中文按原对照表改写 Tauri 默认菜单，英文直接用 Tauri 默认标签。**偏差**：此前只有 Studio 形态
>   本地化菜单，默认 UI 形态恒为英文；现在两种形态都跟随偏好。
> - 设置 → 外观（两个前端）：跟随系统 / 简体中文 / English。语言名用其自身语言书写，作为两种语言值相同的
>   消息（`common_language_native_*`）。选显式语言整包替换为这一种（`["zh-Hans"]` / `["en"]`）；别处写入的
>   其他列表显示为只读的「自定义」项，不替用户改写。写入成功后按应答立即切换，不等推送。
> - 内置贡献（命令、视图、设置分区、模块与可选模块）的标题都写成 getter，切换语言后重新成文；Studio 设置页
>   按 ID 查标题的过渡表随之删除。第三方贡献沿用其登记的标题。
> - 已知限制：webview 首帧在启动值返回前按 `zh-CN` 渲染，英文用户可能看到一瞬中文；`identity_failure`、
>   账户 `detail` 等由 actor/daemon 成文的状态文本仍是中文。（以下补记已处理前两项。）

> 实现记录（L3 收尾，合入 L4 之后）：
>
> - manifest `l10n`（决策 5）：设置 → 扩展（列表、安装与回滚预览）、连接页的本前端信息、账户页的收信插件名
>   都按当前界面语言协商 `l10n`——规则与界面协商相同（`@featherpost/i18n` 导出的 `matchTag`），逐字段取第一个
>   匹配条目、缺的回落顶层 `name` / `description`；有说明就显示。只在渲染时本地化，`previewPluginPackage`
>   的结果原样回传给 commit。桥的 `UiFrontendInfo` 补上 `description` / `l10n`。
> - 回复引用块（决策 7 首版）：写信控制器把当前界面语言作为 `locale` 交给 `prepareReply`；早于 L4 的 daemon
>   拒绝未知参数（`-32602`），桥去掉 `locale` 重试一次，得到今天的中文引用块。
> - 状态文字的稳定 code（决策 3）：actor 的身份绑定失败另给 `identity_failure_code`（`not_installed` /
>   `rejected` / `principal_mismatch` / `unavailable`），经快照到 webview 翻译，中文原因只作诊断；
>   `listPendingOps` 摘要的固定说明另给 `detail_code`（`discarded` / `rejected` / `unconfirmed`），`detail`
>   照旧保留给老前端。账户 `detail`（`account/state`）来自同步循环，多为服务器或连接错误原文，按数据原样显示，
>   不加 code。
> - 首帧：两个前端的 `main.tsx` 在首次渲染前等启动语言就绪，最多 300 ms（壳已有偏好时只是一次读缓存的 IPC；
>   后台未连上或是老 daemon 时到时即按 `zh-CN` 渲染，之后跟推送切换）。

## 交付切片

| 切片 | 内容 | 可独立验收 |
|---|---|---|
| L1 | `locale_preference.toml`、`getLocalePreference` / `setLocalePreference`、`preferences/locale` 推送、协议文档、shared client typed 调用 | 是：`maildctl`/测试可读写偏好 |
| L2 | `maild-l10n`；通知、菜单栏、启动器改走它 | 是：切换偏好后新通知与菜单语言随之变化 |
| L3 | `packages/i18n` + Paraglide + `i18n:lint`；先迁一个模块（`FolderNav`）与原生菜单打通整条链路，再批量迁移两个前端 | 先打通一条链路，再按模块逐个迁移 |
| L4 | manifest `l10n` 段与三个列表/预览方法透传；`prepareReply` 的 `locale` 参数 | 是 |
| L5 | `maildctl` 用户可见输出 | 是 |

> **L5 实施记录**：
>
> - 文案进 `maild-l10n` 新分组 `Ctl`（独立文件 `ctl.rs`，两种语言同在一处），`Messages` 多一个 `ctl` 字段。
>   `maild` 的 `backup` / `restore` / `paths` 维护子命令也用这一组。
> - 语言来源：连上 daemon 时用 `getLocalePreference` 的 `effective` 协商；连不上、`-32601` 或其他失败时读系统
>   首选语言（复用 L1 的 `maild::locale_preference::system_preferred_languages`，与 daemon `system` 模式同源），
>   仍无结果回落 `zh-Hans`。每次调用因此多一次 `getLocalePreference` 往返；协议代握手已判定连不上时跳过，
>   不重复等待。没有新增全局参数。`maild` 维护子命令不连 daemon（恢复要求它已停止），只读系统语言。
> - 翻译范围：stdout/stderr 上给人读的结果、确认、警告与秘密输入提示。**不**翻译：`--json` 输出、`key: value`
>   行的字段名与取值、`--help`、以及进入 `AppError` 的解析/校验/IO 报错——它同时是 `--json` 错误行的
>   `message`，与协议 `error.message` 同属面向开发者的文字。唯一例外是 `account test` 的「探测未通过」：
>   它只在人读分支出现，是结论而不是诊断。`maild` 守护进程启动路径上的报错是日志，不翻译。
> - 新增 `maildctl locale [show]` / `locale set <LANGUAGE>...` / `locale set --system`，`--json` 与协议应答同形；
>   `set` 的人读输出按写入后的新偏好成文。

## 兼容性

- 新方法与通知均为 additive：老 daemon 对 `getLocalePreference` 返回 `-32601`，前端回落基础语言 `zh-CN`，
  即今天的行为。老前端忽略未知通知。
- 不新增授权词；既有已安装包的有效权限逐字节不变。
- `prepareReply` 不带 `locale` 时，`effective` 首选为中文的用户得到与今天相同的中文引用块。
- manifest `l10n` 若需 `schema_version = 3`，v2 包继续按原规则安装与运行。

## 测试策略（hermetic）

- 偏好文件：照 `reading_policy.toml` 的用例集（损坏、未来版本、符号链接、多硬链接、超限、原子写）。
- 参数校验：非法 BCP 47、超长、超过 8 项、`system` 带 `languages` 均 `-32602` 且不落盘。
- 协商函数：Rust 与 TS 各一份表驱动用例，覆盖 `zh-Hans-CN`、`zh-CN`、`zh`、`zh-Hant-TW`、`en-US`、空匹配回落。
- `maild-l10n`：缺字段即编译失败，无需运行时测试；为带参数的消息做快照测试。
- 前端：`i18n:lint` 进 CI；`packages/maild-ui/test/i18n.test.ts` 改为对两种语言分别断言。
- 推送：`setLocalePreference` 后流式连接收到 `preferences/locale`；HTTP 不承载通知。

## 后果

- 换前端不换语言：用户设一次，内核界面与任何遵守协议的前端都跟着走。
- 第三方前端零框架约束；代价是各前端的翻译质量与覆盖面不一致，这是可替换前端本身的取舍。
- 权限说明由前端各自措辞，内核只保证授权词的语义稳定、文档化。
- 中文字面量迁移是本 ADR 最大的工作量（约 60 个前端文件）；`i18n:lint` 白名单让迁移可以分批合入而不回退。
- 落地后需同步更新 `docs/protocol/README.md` 中「maild 还没有 i18n 框架」两处表述与 ADR-0028 决策 7 的边界说明。

## 未决

- ~~内核支持的语言标签写作 `zh-Hans` 还是 `zh-CN`~~ **已定（L2）**：内核用 `zh-Hans`，默认前端沿用
  `zh-CN`，两边使用同一条协商规则（决策 8），所以同一份 `effective` 列表在两边都会选中简体中文，标签不必统一。
- ~~启动器与菜单栏是否在 daemon 不可用时直接读系统语言兜底~~ **已定（L2）**：是。连不上 daemon、超时、
  旧 Agent 返回 `-32601` 或应答为空时，读本进程的系统首选语言（macOS：`+[NSLocale preferredLanguages]`），
  用同一个 `maild_l10n::negotiate` 协商；能连上后以 `effective` 为准。刷新时机见决策 6 的 L2 实施记录。
