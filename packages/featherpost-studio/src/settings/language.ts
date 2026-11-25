import { applyEffectiveLocales } from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";

import { BridgeError } from "../bridge/errors";
import type { LocaleBridge, UiLocalePreference, UiLocaleUpdate } from "../bridge/types";
import { Store } from "../host/store";

/** 语言选择器的取值：跟随系统，或一种显式语言（值即发给 daemon 的 BCP 47 标签）。 */
export type LanguageChoice = "system" | "zh-Hans" | "en";

/**
 * 默认前端支持的显式语言（ADR-0048 决策 8）。名字用该语言自身书写，不随界面语言翻译，
 * 所以两种语言的消息文件里值相同。
 */
export const LANGUAGE_OPTIONS: ReadonlyArray<{ value: Exclude<LanguageChoice, "system">; name: () => string }> = [
  { value: "zh-Hans", name: m.common_language_native_zh_hans },
  { value: "en", name: m.common_language_native_en },
];

/**
 * 当前偏好落在哪个选项上。别的前端（或 `maildctl`）写进了这两种以外的显式列表时为 `"custom"`，
 * 选择器显示原样列表、不替用户改写；还没读到偏好时为 `null`。
 */
export function languageChoice(preference: UiLocalePreference | null): LanguageChoice | "custom" | null {
  if (!preference) return null;
  if (preference.mode === "system") return "system";
  const [only, ...rest] = preference.languages;
  if (rest.length > 0 || only === undefined) return "custom";
  const option = LANGUAGE_OPTIONS.find(({ value }) => value.toLowerCase() === only.toLowerCase());
  return option ? option.value : "custom";
}

/** 选项 → `setLocalePreference` 参数。显式语言整包替换为这一种。 */
export function languageUpdate(choice: LanguageChoice): UiLocaleUpdate {
  return choice === "system" ? { mode: "system" } : { mode: "explicit", languages: [choice] };
}

export interface LanguageState {
  preference: UiLocalePreference | null;
  busy: boolean;
  /** 后台不提供语言偏好（老 daemon）：选择器禁用，界面停在基础语言。 */
  unsupported: boolean;
  error: string | null;
}

/**
 * 设置页的语言选择（两个默认前端共用）。偏好归 daemon（ADR-0048 决策 1）：这里只读写，
 * 不在 webview 里另存一份。写入成功后立即按应答切换界面语言，不等推送；推送到达时重读偏好，
 * 别的窗口改了也能看到。
 */
export class LanguageController {
  readonly store = new Store<LanguageState>({ preference: null, busy: false, unsupported: false, error: null });
  private stop: (() => void) | null = null;
  private disposed = false;
  private sequence = 0;

  constructor(private readonly bridge: LocaleBridge | undefined) {}

  /** 壳是否提供语言偏好通道；没有时设置页不显示选择器。 */
  get available(): boolean {
    return this.bridge !== undefined;
  }

  /** 订阅偏好推送（幂等）。 */
  start() {
    if (!this.bridge || this.stop || this.disposed) return;
    this.stop = () => {};
    void this.bridge
      .subscribe(() => void this.load())
      .then((stop) => {
        if (this.disposed) stop();
        else this.stop = stop;
      })
      .catch(() => {
        this.stop = null;
      });
  }

  async load() {
    if (!this.bridge) return;
    const sequence = ++this.sequence;
    try {
      const preference = await this.bridge.preference();
      if (sequence === this.sequence) this.store.patch({ preference, unsupported: false, error: null });
    } catch (error) {
      if (sequence !== this.sequence) return;
      if (error instanceof BridgeError && error.detail.reason === "locale_unsupported") {
        this.store.patch({ unsupported: true, error: null });
      } else {
        this.store.patch({ error: failure(error) });
      }
    }
  }

  async choose(choice: LanguageChoice) {
    const state = this.store.getSnapshot();
    if (!this.bridge || state.busy || choice === languageChoice(state.preference)) return;
    const sequence = ++this.sequence;
    this.store.patch({ busy: true, error: null });
    try {
      const preference = await this.bridge.setPreference(languageUpdate(choice));
      applyEffectiveLocales(preference.effective);
      if (sequence === this.sequence) this.store.patch({ preference, busy: false });
      else this.store.patch({ busy: false });
    } catch (error) {
      this.store.patch({ busy: false, error: failure(error) });
    }
  }

  dispose() {
    this.disposed = true;
    this.stop?.();
    this.stop = null;
  }
}

function failure(error: unknown): string {
  return error instanceof Error ? error.message : m.common_bridge_error_failed();
}
