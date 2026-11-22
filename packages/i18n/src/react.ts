import { useSyncExternalStore } from "react";

import { getUiLocale, subscribeLocale, type UiLocale } from "./index";

/**
 * 订阅当前界面语言：语言切换时调用它的组件重新渲染（不重载页面、不重挂载、不丢组件状态）。
 * 渲染消息的组件都应调用一次；返回值同时可作 `Intl` / `localeCompare` 的 locale 参数。
 */
export function useUiLocale(): UiLocale {
  return useSyncExternalStore(subscribeLocale, getUiLocale, getUiLocale);
}
