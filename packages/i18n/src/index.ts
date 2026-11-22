/**
 * 默认前端的语言运行时（ADR-0048 决策 8）。
 *
 * 语言只由壳推来的 `effective` 偏好列表决定：与本前端支持的语言协商，失败回落基础语言 `zh-CN`。
 * 不读 URL、cookie、`navigator.language`（WKWebView 下后者跟随 app bundle 声明的本地化，不可靠）。
 * 切换语言只通知订阅者重新渲染，从不重载页面。
 */
import {
  baseLocale,
  defineCustomClientStrategy,
  locales,
  setLocale,
  type Locale,
} from "./paraglide/runtime.js";

/** 默认前端支持的界面语言；与 `project.inlang/settings.json` 的 `locales` 一致。 */
export type UiLocale = Locale;

export const BASE_LOCALE: UiLocale = baseLocale;
export const UI_LOCALES: readonly UiLocale[] = locales;

/** 繁体中文地区：无书写系统子标签时据此判定为 Hant，不匹配 `zh-CN`。 */
const HANT_REGIONS = new Set(["tw", "hk", "mo"]);

/**
 * 单个 BCP 47 标签能否映射到某个界面语言；不能则返回 `undefined`，由调用方继续看下一项偏好。
 * 也用于反方向：从包作者提供的多语言条目（manifest `l10n`）里挑出与当前界面语言匹配的那条。
 */
export function matchTag(tag: string): UiLocale | undefined {
  const subtags = tag.trim().replace(/_/g, "-").toLowerCase().split("-");
  const language = subtags[0];
  if (language === "en") return "en";
  if (language !== "zh") return undefined;
  const second = subtags[1];
  // 第二个子标签为 4 个字母时是书写系统（`Hans` / `Hant`），否则是地区或不存在。
  if (second !== undefined && /^[a-z]{4}$/.test(second))
    return second === "hans" ? "zh-CN" : undefined;
  if (second !== undefined && HANT_REGIONS.has(second)) return undefined;
  return "zh-CN";
}

/**
 * 用户的有序偏好与界面语言协商：逐项匹配，第一个能匹配的即为结果；全部不匹配回落 `zh-CN`。
 *
 * - `en`、`en-*` → `en`
 * - `zh-Hans*`，以及无书写系统的 `zh` / `zh-CN` / `zh-SG`（与其它非繁体地区）→ `zh-CN`
 * - `zh-Hant*`、`zh-TW`、`zh-HK`、`zh-MO` 不匹配，继续看下一项
 */
export function negotiateLocale(effective: readonly string[]): UiLocale {
  for (const tag of effective) {
    if (typeof tag !== "string") continue;
    const matched = matchTag(tag);
    if (matched) return matched;
  }
  return BASE_LOCALE;
}

let current: UiLocale = BASE_LOCALE;
const listeners = new Set<() => void>();

// 唯一的语言策略（编译时 `--strategy custom-featherpost baseLocale`）：消息函数每次调用都从这里取语言。
defineCustomClientStrategy("custom-featherpost", {
  getLocale: () => current,
  setLocale: (locale) => {
    current = locale as UiLocale;
  },
});

/** 当前协商出的界面语言；消息函数与 `Intl` 格式化都用它。未推送过偏好时为 `zh-CN`。 */
export function getUiLocale(): UiLocale {
  return current;
}

/** 订阅语言变化；返回取消订阅函数。形状可直接交给 React 的 `useSyncExternalStore`。 */
export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * 壳拿到 `getLocalePreference` 应答或 `preferences/locale` 通知后调用：传入 `effective` 列表，
 * 协商出界面语言；与当前不同则切换、更新 `<html lang>` 并通知订阅者。返回协商结果。
 */
export function applyEffectiveLocales(effective: readonly string[]): UiLocale {
  const next = negotiateLocale(effective);
  if (next === current) return next;
  void setLocale(next, { reload: false });
  current = next;
  if (typeof document !== "undefined") document.documentElement.lang = next;
  for (const listener of [...listeners]) listener();
  return next;
}
