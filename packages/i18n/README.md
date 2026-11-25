# @featherpost/i18n

默认前端 `packages/featherpost-studio` 使用的界面消息与语言运行时，
实现 [ADR-0048](../../docs/decisions/0048-locale-preference-and-decoupled-i18n.md) 决策 8。
消息用 [Paraglide JS](https://paraglidejs.com) v2（inlang）在**本包里编译一次**，前端从这里导入；
Vite 配置里不挂 Paraglide 插件。

```text
packages/i18n/
├── project.inlang/settings.json   # 语言列表、基础语言、消息文件路径（只提交这个文件）
├── messages/<locale>/<命名空间>.json # 消息；zh-CN 为基础语言
├── src/index.ts                   # 语言运行时：协商、推送、订阅
├── src/messages.ts                # 导出 m（先加载运行时）
├── src/react.ts                   # useUiLocale()
├── src/paraglide/                 # 编译产物（生成，不提交）
├── typetest/                      # 类型夹具，只参与 tsc
├── scripts/lint.mjs               # i18n:lint
└── cjk-allowlist.txt              # 中文字面量白名单（迁移期基线，只减不增）
```

## 编译产物：生成，不提交

`src/paraglide/` 由 `paraglide-js compile` 生成，在 `.gitignore` 里。选择生成而不提交，原因有两条：

- Paraglide 给每条消息生成一个模块，再加一个汇总的 `_index.js`。提交产物会让并行分支在生成文件上互相冲突，
  而这些冲突本来只需要重新编译就能解决。
- 编译不联网、约 2 秒：inlang 的消息格式插件从本包的 `node_modules` 本地加载
  （`settings.json` 的 `modules` 写的是本地路径，不是 CDN），产物只取决于 `messages/` 与 `settings.json`。

保证产物存在且是最新的入口：

| 入口 | 怎么拿到产物 |
|---|---|
| 任意 `pnpm install`（CI 与仓库根 `pnpm install --frozen-lockfile`） | 本包的 `postinstall` 编译一次 |
| `pnpm --dir packages/featherpost-studio {dev,build,typecheck,test}`（含 Tauri `beforeBuildCommand`） | 脚本先跑 `pnpm --dir ../i18n compile` |
| `pnpm --dir packages/featherpost-studio exec tsc …`（直接 exec，不经脚本） | 依赖安装时的产物；改了消息后先 `pnpm i18n:compile` |

开发时改消息：另开一个终端跑 `pnpm i18n:watch`，Vite 会热更新。

## 消息文件布局（后续任务照此执行）

每种语言一个目录，每个模块一个文件：

```text
messages/zh-CN/{common,mailbox,compose,settings,extensions,studio}.json
messages/en/{common,mailbox,compose,settings,extensions,studio}.json
```

- ADR 原写法是每种语言一个文件（`messages/{zh-CN,en}.json`）。改为按命名空间分文件，是为了让多个分支同时给
  不同模块加消息时互不冲突：迁移 `compose` 的分支只改 `compose.json`，迁移 `settings` 的分支只改 `settings.json`。
  inlang 消息格式插件的 `pathPattern` 支持数组，编译时把所有文件合并成一个消息表。
- 命名空间 = 文件名 = key 前缀。`mailbox_*` 只能写在 `mailbox.json` 里，`i18n:lint` 会检查。
- 两种语言的同名文件 key 集合必须相同，同一 key 的占位符集合必须相同。
- 文件内 key 按字典序排列（`$schema` 在最前）。排好序的文件在同一命名空间内并行插入时也很少冲突。
  `pnpm i18n:lint --sort` 自动排序。
- 值只能是字符串，占位符写 `{name}`。需要复数时在代码里用 `Intl.PluralRules` 选不同的消息，不用复数变体。
- 每个文件都保留 `"$schema": "https://inlang.com/schema/inlang-message-format"`，没有消息的文件也保留。

**不要用 inlang 的编辑器（Fink、Sherlock 等）写回消息**：`pathPattern` 是数组时，SDK 导出会把全部消息写进
每一个文件。只手工或用脚本编辑 JSON。

新增命名空间：在 `settings.json` 的 `pathPattern` 数组加一行 `./messages/{locale}/<名字>.json`，
在每个语言目录下建同名文件（内容只有 `$schema` 也可以）。lint 从 `pathPattern` 读取合法前缀。

## key 命名

- 合法标识符：小写字母、数字、下划线，形如 `<命名空间>_<对象>_<含义>`，例如 `mailbox_folder_inbox`、
  `mailbox_account_state_online`、`extensions_permission_drafts_read`。
- 前缀只能是 `common_`、`mailbox_`、`compose_`、`settings_`、`extensions_`、`studio_`。
  多个模块共用的文案（「取消」「保存」）放 `common_*`。
- 调用写 `m.mailbox_folder_inbox()`、`m.studio_folder_expand({ folder })`；不用 `m["..."]`。
- 按用途命名，不按文字命名：同样是「全部邮件」，`mailbox_folder_all`（系统文件夹名）与
  `mailbox_smart_all`（智能文件夹）分成两个 key，英文可以各自措辞。
- `aria-label`、`title` 与可见文字一样要翻译。

## 新增一条消息

1. 在 `messages/zh-CN/<命名空间>.json` 与 `messages/en/<命名空间>.json` 的字典序位置各加一行。
2. 组件里 `import { m } from "@featherpost/i18n/messages";`，调用 `m.<key>(...)`。
3. 组件里调用一次 `useUiLocale()`（从 `../i18n` 或 `@featherpost/i18n/react` 导入），切换语言时它会重新渲染。
   根组件 `Studio` 已订阅，整棵树会随之重新渲染；组件自己订阅是为了不依赖这一点。
4. 不要在模块顶层调用 `m.x()` 把结果存成常量，那样切换语言后不会变。要建映射表，就存消息函数本身
   （参照 `featherpost-studio/src/i18n.ts` 的 `folderNames`）。
5. 跑 `pnpm i18n:lint` 与 `pnpm --dir packages/featherpost-studio typecheck`。

## 收紧白名单

`cjk-allowlist.txt` 是本包创建时 `packages/*/src` 下含中文字面量（字符串、模板、正则、JSX 文本；注释不算）
的文件列表。

- 迁移完一个文件，就删掉它那一行。文件已经没有中文字面量却还在单上，`i18n:lint` 会报错，提醒删除。
- 新文件不得加进白名单。`node scripts/lint.mjs --write-allowlist` 会用当前违规文件重写白名单，只用于删减条目；
  它新增的行在代码评审里能直接看到。
- 不属于界面文案的中文（例如 `featherpost-studio/src/mailbox/otp.ts` 的验证码识别关键词，见 ADR-0048 非目标），在该语句前
  加注释 `// i18n-lint-allow-cjk: <理由>`，lint 会跳过这条语句（含其子节点），然后把文件移出白名单。

## 运行时 API

```ts
import {
  applyEffectiveLocales, // (effective: readonly string[]) => UiLocale：壳推送偏好列表
  getUiLocale,           // () => UiLocale：当前协商结果，默认 "zh-CN"
  subscribeLocale,       // (listener) => unsubscribe
  negotiateLocale,       // (effective) => UiLocale：纯函数
  matchTag,              // (tag) => UiLocale | undefined：单个标签的匹配，协商的基本单元
  type UiLocale,         // "zh-CN" | "en"
} from "@featherpost/i18n";
import { useUiLocale } from "@featherpost/i18n/react"; // React：订阅并返回当前语言
import { m } from "@featherpost/i18n/messages";
```

- 唯一的 Paraglide 策略是 `custom-featherpost`（编译参数 `--strategy custom-featherpost baseLocale`），
  不用 URL、cookie、`navigator.language`、localStorage。
- `applyEffectiveLocales` 协商后若语言变化：切换消息语言（`setLocale(…, { reload: false })`，从不重载页面）、
  更新 `<html lang>`、通知订阅者。没有推送过时为 `zh-CN`，即今天的行为。
- `featherpost-studio/src/i18n.ts` 另外导出 `uiLocale()`（等于 `getUiLocale()`）与 `useUiLocale()`，用于 `Intl`
  格式化与 `localeCompare`；它取代了原来的 `UI_LOCALE` 常量。

- `matchTag` 只判断一个 BCP 47 标签能映射到哪种界面语言，不能就返回 `undefined`；`negotiateLocale` 就是对
  偏好列表逐项调用它。反方向也用它：包作者在 manifest `[l10n.<tag>]` 里写的标签各式各样（`zh-Hans`、`zh`、
  `en-US`），默认前端渲染扩展名称与描述时，取所有 `matchTag(tag) === getUiLocale()` 的条目，逐字段用第一个
  有值的，缺省回落顶层 `name` / `description`（`featherpost-studio/src/i18n.ts` 的 `packageText`）。两个方向共用一套规则，界面语言与包文案
  才不会对不上。

  negotiateLocale,       // (effective) => UiLocale：纯函数
  matchTag,              // (tag) => UiLocale | undefined：单个标签的匹配，协商的基本单元
：逐项看偏好列表，第一个能匹配的即为结果，全部不匹配回落 `zh-CN`。

| 偏好标签 | 结果 |
|---|---|
| `en`、`en-*` | `en` |
| `zh-Hans*`；无书写系统的 `zh`、`zh-CN`、`zh-SG` 及其它非繁体地区 | `zh-CN` |
| `zh-Hant*`；`zh-TW`、`zh-HK`、`zh-MO` | 不匹配，继续看下一项 |

大小写不敏感，`_` 视同 `-`。

## 类型检查

编译带 `--emit-ts-declarations`，Paraglide 为每条消息生成 `.d.ts`。`typetest/messages.typetest.ts`
（`pnpm i18n:typecheck`）证明 `tsc` 能直接报出：key 拼错、缺参数、参数名拼错、参数为 `null`、
`locale` 不是 `zh-CN | en`。参数值的类型是 `NonNullable<unknown>`，任何非空值都能插值，这是 Paraglide 的设计，
不算缺口，所以没有加 `i18n:dts` 脚本。不带该参数时产物是无声明的 `.js`，严格模式下 `tsc` 报 TS7016，
因此这个参数不能去掉。

## 命令

| 命令（仓库根） | 作用 |
|---|---|
| `pnpm i18n:compile` | 编译消息到 `src/paraglide/` |
| `pnpm i18n:watch` | 监听消息文件并重新编译 |
| `pnpm i18n:lint` | 守门：消息一致性、`m.<key>` 引用、中文字面量白名单；`--sort` 自动排序 |
| `pnpm i18n:typecheck` | 编译后对运行时与类型夹具跑 `tsc` |

`i18n:lint` 与 `i18n:typecheck` 都在 CI 的 macOS Rust job 中「Lint default-frontend i18n」一步里运行。

## 版本

固定 `@inlang/paraglide-js` 2.29.3 与 `@inlang/plugin-message-format` 4.6.0（ADR 选定 v2）。
Paraglide 3.0 已于 2026-11-03 发布，主要移除 `additionalFiles` 与旧 SDK 兼容层，本包均未使用；升级另行评估。
