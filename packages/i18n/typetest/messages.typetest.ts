/**
 * 类型夹具（ADR-0048 决策 8「类型」）：只参与 `tsc`，从不运行。
 *
 * 验证 Paraglide 以 `--emit-ts-declarations` 生成的声明能让 `tsc` 直接报出 key 拼错与参数错。
 * 每条 `@ts-expect-error` 都必须真的产生错误，否则 `tsc` 报 TS2578（未使用的指令），夹具失败。
 */
import { m } from "../src/messages";
import { applyEffectiveLocales, getUiLocale, type UiLocale } from "../src/index";

// 正确用法：无参数、带参数、显式指定语言。
const plain: string = m.mailbox_folder_inbox();
const withParam: string = m.mailbox_account_folders_label({ account: "工作" });
const english: string = m.mailbox_folder_inbox({}, { locale: "en" });
void [plain, withParam, english];

// @ts-expect-error key 拼错
m.mailbox_folder_inbx();

// @ts-expect-error 不存在的 key 前缀
m.inbox();

// @ts-expect-error 缺少全部参数
m.mailbox_account_folders_label();

// @ts-expect-error 缺少必需参数
m.mailbox_account_folders_label({});

// @ts-expect-error 参数名拼错
m.mailbox_account_folders_label({ acount: "工作" });

// @ts-expect-error 参数为 null
m.extensions_lifecycle_unknown({ value: null });

// @ts-expect-error 不支持的语言
m.mailbox_folder_inbox({}, { locale: "de" });

// 运行时 API 的类型：协商结果只可能是受支持的语言。
const negotiated: UiLocale = applyEffectiveLocales(["en-US"]);
const current: "zh-CN" | "en" = getUiLocale();
void [negotiated, current];

// @ts-expect-error 界面语言不是任意字符串
const bogus: UiLocale = "fr";
void bogus;
