import { applyEffectiveLocales, getUiLocale } from "@featherpost/i18n";
import { m } from "@featherpost/i18n/messages";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BridgeError, bridgeErrorText } from "../src/bridge/errors";
import { followDaemonLocale, startupLocale } from "../src/bridge/locale";
import { BRIDGE_ERROR_REASONS } from "../src/bridge/reasons";
import { BRAND } from "../src/brand";
import { ComposeController } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import type { Bundle } from "../src/host/types";
import { mailboxModule } from "../src/mailbox/module";
import { sampleExcerptModule } from "../src/sample/module";
import { ChoiceRenderer, type ChoiceProps } from "../src/settings/Choice";
import { SettingsController } from "../src/settings/controller";
import { LanguageController, languageChoice, languageUpdate } from "../src/settings/language";
import { settingsModule } from "../src/settings/module";
import { Appearance as StudioAppearance } from "../../featherpost-studio/src/settings/Appearance";
import { settingsModule as studioSettingsModule } from "../../featherpost-studio/src/settings/module";
import { studioMailbox } from "../../featherpost-studio/src/mailbox";
import { FakeBridge, FakeLocale, flush, online } from "./fakeBridge";

afterEach(() => {
  vi.unstubAllGlobals();
  applyEffectiveLocales([]);
});

describe("界面语言跟随 daemon 偏好（ADR-0048 决策 8）", () => {
  it("启动时套用壳手里的偏好，之后跟随推送；null 回落基础语言", async () => {
    const locale = new FakeLocale();
    locale.effective = ["en-US", "zh-Hans"];
    const follower = followDaemonLocale(locale);
    await follower.ready;
    expect(getUiLocale()).toBe("en");
    const stop = () => follower.stop();
    locale.push(["zh-Hant-TW", "zh-Hans-CN"]);
    expect(getUiLocale()).toBe("zh-CN");
    locale.push(["en"]);
    expect(getUiLocale()).toBe("en");
    locale.push(null);
    expect(getUiLocale()).toBe("zh-CN");
    stop();
    locale.push(["en"]);
    expect(getUiLocale()).toBe("zh-CN");
  });

  it("推送先于启动值到达时以推送为准", async () => {
    const locale = new FakeLocale();
    locale.current = async () => {
      // 启动值在路上时推送先到：启动值更旧，不能覆盖推送。
      locale.push(["en"]);
      return { effective: ["zh-Hans"] };
    };
    await followDaemonLocale(locale).ready;
    await flush();
    expect(getUiLocale()).toBe("en");
  });

  it("首帧前等启动语言：已有偏好即刻就绪；后台未给出时最多等到超时", async () => {
    const known = new FakeLocale();
    known.effective = ["en"];
    await startupLocale(followDaemonLocale(known), 10_000);
    expect(getUiLocale()).toBe("en");

    applyEffectiveLocales([]);
    const pending = new FakeLocale(); // 壳还没取到偏好：current() 为 null
    const started = Date.now();
    await startupLocale(followDaemonLocale(pending), 30);
    expect(Date.now() - started).toBeGreaterThanOrEqual(25);
    expect(getUiLocale()).toBe("zh-CN");
    await startupLocale(null);
  });

  it("取不到启动值时停在基础语言，等推送", async () => {
    const locale = new FakeLocale();
    locale.current = async () => {
      throw new BridgeError({ code: "offline", reason: "offline", message: "x" });
    };
    followDaemonLocale(locale);
    await flush();
    expect(getUiLocale()).toBe("zh-CN");
    locale.push(["en-GB"]);
    expect(getUiLocale()).toBe("en");
  });
});

describe("桥错误按 reason 成文（ADR-0048 决策 3）", () => {
  // Rust 侧所有 UiError 的 reason：构造器的第一个参数、`failed` 的第三个参数、投影表元组、
  // 显式 `reason:` 字段与 compat 拒绝。
  const sources = import.meta.glob<string>("../../../crates/maild-ui/src/*.rs", {
    eager: true,
    query: "?raw",
    import: "default",
  });
  const patterns = [
    /UiError::(?:invalid|not_found)\(\s*"([a-z_]+)"/g,
    /UiError::failed\(\s*(?:"[^"]*"|[^,()]+),\s*[^,()]+,\s*"([a-z_]+)"/g,
    /UiErrorCode::\w+,\s*"([a-z_]+)"/g,
    // `reason: "x",` 是 UiError 字面量；远程内容结论的 `reason: "default".into()` 不是。
    /reason:\s*"([a-z_]+)",/g,
    /\(\s*"([a-z_]+)",\s*format!/g,
  ];
  const rustReasons = new Set(
    Object.values(sources).flatMap((text) =>
      patterns.flatMap((pattern) => [...text.matchAll(pattern)].map((match) => match[1])),
    ),
  );

  it("Rust 侧的每个 reason 在 webview 都有两种语言的文案，映射表没有多余项", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(5);
    expect(rustReasons.size).toBeGreaterThan(80);
    const missing = [...rustReasons].filter((reason) => !(reason in BRIDGE_ERROR_REASONS));
    expect(missing).toEqual([]);
    const stale = Object.keys(BRIDGE_ERROR_REASONS).filter((reason) => !rustReasons.has(reason));
    expect(stale).toEqual([]);
    for (const locale of ["zh-CN", "en"]) {
      applyEffectiveLocales([locale]);
      for (const [reason, text] of Object.entries(BRIDGE_ERROR_REASONS))
        expect(text().trim(), `${locale} ${reason}`).not.toBe("");
    }
  });

  it("按当前语言成文；中文原文只是兜底，不进英文界面", () => {
    const detail = { code: "conflict" as const, reason: "draft_conflict", message: "草稿已在别处修改。你的输入仍保留，请复制内容或重新载入。" };
    expect(new BridgeError(detail).message).toBe(detail.message);
    applyEffectiveLocales(["en"]);
    expect(new BridgeError(detail).message).toBe("The draft was changed elsewhere. Your input is kept; copy it or reload.");
    expect(bridgeErrorText({ code: "incompatible", reason: "kernel_outdated", message: "x" })).toContain(BRAND.productName);
    // 认不出的 reason：通用提示，而不是中文原文。
    expect(bridgeErrorText({ code: "failed", reason: "from_the_future", message: "未来的原因" })).toBe(m.common_bridge_error_failed());
    // webview 自己构造的错误没有 reason，message 已是当前语言。
    expect(bridgeErrorText({ code: "offline", message: "already localized" })).toBe("already localized");
  });
});

describe("语言选择器（ADR-0048 决策 8）", () => {
  it("偏好映射到选项，选项映射到 setLocalePreference 参数", () => {
    const base = { effective: ["en"], persisted: true };
    expect(languageChoice(null)).toBeNull();
    expect(languageChoice({ ...base, mode: "system", languages: [] })).toBe("system");
    expect(languageChoice({ ...base, mode: "explicit", languages: ["zh-hans"] })).toBe("zh-Hans");
    expect(languageChoice({ ...base, mode: "explicit", languages: ["en"] })).toBe("en");
    // 别的前端写进的列表原样显示，不替用户改写。
    expect(languageChoice({ ...base, mode: "explicit", languages: ["ja", "en"] })).toBe("custom");
    expect(languageChoice({ ...base, mode: "explicit", languages: ["fr"] })).toBe("custom");
    expect(languageUpdate("system")).toEqual({ mode: "system" });
    expect(languageUpdate("zh-Hans")).toEqual({ mode: "explicit", languages: ["zh-Hans"] });
    expect(languageUpdate("en")).toEqual({ mode: "explicit", languages: ["en"] });
  });

  it("写入后按应答立即切换语言；老后台禁用选择器", async () => {
    const locale = new FakeLocale();
    const controller = new LanguageController(locale);
    await controller.load();
    expect(languageChoice(controller.store.getSnapshot().preference)).toBe("system");
    await controller.choose("en");
    expect(locale.updates).toEqual([{ mode: "explicit", languages: ["en"] }]);
    expect(getUiLocale()).toBe("en");
    await controller.choose("en");
    expect(locale.updates.length, "选中当前项不重复写").toBe(1);
    await controller.choose("system");
    expect(locale.updates.at(-1)).toEqual({ mode: "system" });

    const old = new FakeLocale();
    old.preferenceError = new BridgeError({ code: "failed", reason: "locale_unsupported", message: "x" });
    const legacy = new LanguageController(old);
    await legacy.load();
    expect(legacy.store.getSnapshot()).toMatchObject({ unsupported: true, error: null });
    expect(new LanguageController(undefined).available).toBe(false);
  });

  /** 在共享的 Choice 边界上截下语言选择器的 props（两个前端都经它渲染下拉）。 */
  function renderWithChoices<P extends object>(component: ComponentType<P>, props: P) {
    const choices: ChoiceProps[] = [];
    function Spy(choice: ChoiceProps) {
      choices.push(choice);
      return createElement("select", { "aria-label": choice["aria-label"], "aria-labelledby": choice["aria-labelledby"], disabled: choice.disabled, value: choice.value, onChange: () => {} }, choice.children);
    }
    const html = renderToStaticMarkup(
      createElement(ChoiceRenderer.Provider, { value: Spy }, createElement(component, props)),
    );
    return { html, choices };
  }

  async function prepared() {
    const bridge = new FakeBridge();
    const locale = new FakeLocale();
    bridge.locale = locale;
    const host = new Host(bridge);
    host.state.patch({ snapshot: online });
    return { bridge, locale, host };
  }

  it("默认前端：设置 → 外观里有跟随系统与两种语言，选择发出正确的请求", async () => {
    const { locale, host } = await prepared();
    const bundle = settingsModule(host, new SettingsController(host.bridge.settings));
    try {
      locale.push(["zh-Hans-CN"]); // 推送触发重读偏好
      await flush();
      const section = bundle.settings!.find((s) => s.id === "maild.settings.appearance")!;
      const { html, choices } = renderWithChoices(section.render, { active: true });
      const picker = choices.find((choice) => choice["aria-label"] === m.settings_language())!;
      expect(picker.value).toBe("system");
      expect(picker.disabled).toBe(false);
      expect(html).toContain("跟随系统");
      expect(html).toContain('lang="zh-Hans">简体中文<');
      expect(html).toContain('lang="en">English<');
      picker.onValueChange("zh-Hans");
      await flush();
      expect(locale.updates).toEqual([{ mode: "explicit", languages: ["zh-Hans"] }]);
      picker.onValueChange("custom");
      await flush();
      expect(locale.updates.length, "自定义项不可选").toBe(1);

      // 英文界面下语言名不翻译，「跟随系统」翻译。
      applyEffectiveLocales(["en"]);
      const english = renderWithChoices(section.render, { active: true }).html;
      expect(english).toContain("Follow system");
      expect(english).toContain(">简体中文<");
    } finally {
      bundle.dispose?.();
    }
  });

  it("Studio：设置 → 外观里同样的选择器，写入英文后切换界面语言", async () => {
    const { locale, host } = await prepared();
    const bundle = studioSettingsModule(host, new SettingsController(host.bridge.settings));
    try {
      locale.preferenceValue = { mode: "explicit", languages: ["ja", "en"], effective: ["ja", "en"], persisted: true };
      locale.push(["ja", "en"]);
      await flush();
      const section = bundle.settings!.find((s) => s.id === "maild.settings.appearance")!;
      const { html, choices } = renderWithChoices(section.render, { active: true });
      const picker = choices.find((choice) => choice["aria-labelledby"] === "studio-language-label")!;
      expect(picker.value).toBe("custom");
      expect(html).toContain("自定义（ja, en）");
      expect(html).toContain(">简体中文<");
      picker.onValueChange("en");
      await flush();
      expect(locale.updates).toEqual([{ mode: "explicit", languages: ["en"] }]);
      expect(getUiLocale()).toBe("en");
      picker.onValueChange("system");
      await flush();
      expect(locale.updates.at(-1)).toEqual({ mode: "system" });
    } finally {
      bundle.dispose?.();
    }
  });

  it("没有语言偏好通道的壳不显示选择器", () => {
    const host = new Host(new FakeBridge());
    expect(renderWithChoices(StudioAppearance, { host }).html).not.toContain("studio-language");
  });
});

describe("切换语言后内置贡献的标题重新成文（ADR-0048 决策 8）", () => {
  type Titles = Record<string, string>;
  function titles(host: Host): Titles {
    const snapshot = host.registry.getSnapshot();
    const entries: [string, string][] = [
      ...snapshot.modules.map((module) => [`module:${module.moduleId}`, module.title] as [string, string]),
      ...snapshot.commands.map(({ value }) => [`command:${value.id}`, value.title] as [string, string]),
      ...snapshot.views.map(({ value }) => [`view:${value.id}`, value.title] as [string, string]),
      ...snapshot.settings.map(({ value }) => [`settings:${value.id}`, value.title] as [string, string]),
      ...host.optionalModules().map((module) => [`optional:${module.moduleId}`, module.title] as [string, string]),
    ];
    return Object.fromEntries(entries);
  }

  function expectEveryTitleSwitches(host: Host) {
    const chinese = titles(host);
    applyEffectiveLocales(["en"]);
    const english = titles(host);
    expect(Object.keys(english)).toEqual(Object.keys(chinese));
    const frozen = Object.keys(chinese).filter((key) => chinese[key] === english[key]);
    expect(frozen, "这些标题停在了登记时的语言").toEqual([]);
    applyEffectiveLocales(["zh-Hans"]);
    expect(titles(host)).toEqual(chinese);
    return Object.keys(chinese);
  }

  it("默认前端：邮箱、写信、设置与可选示例模块", () => {
    vi.stubGlobal("window", { addEventListener: vi.fn(), removeEventListener: vi.fn() });
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const composer = new ComposeController(bridge.compose, (id) => host.openView(id), () => {});
    const bundles: Bundle[] = [
      composeModule(host, composer),
      mailboxModule(host, composer, () => {}),
      settingsModule(host, new SettingsController(bridge.settings)),
    ];
    try {
      bundles.forEach((bundle) => host.registerBuiltin(bundle));
      host.registerOptional("maild.sample-excerpt", {
        get title() {
          return m.common_sample_excerpt_title();
        },
        bundle: () => sampleExcerptModule(host),
        defaultEnabled: true,
      });
      const keys = expectEveryTitleSwitches(host);
      expect(keys.filter((key) => key.startsWith("command:")).length).toBeGreaterThan(5);
      expect(keys).toContain("settings:maild.settings.appearance");
      expect(keys).toContain("module:maild.sample-excerpt");
    } finally {
      host.setModuleEnabled("maild.sample-excerpt", false);
      bundles.forEach((bundle) => bundle.dispose?.());
      host.stop();
    }
  });

  it("Studio：邮箱、写信与设置（设置导航、分区标题与模块列表读同一份标题）", async () => {
    vi.stubGlobal("window", { addEventListener: vi.fn(), removeEventListener: vi.fn() });
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const composer = new ComposeController(bridge.compose, (id) => host.openView(id), () => {});
    const mailbox = studioMailbox(host, composer, () => {});
    const bundles: Bundle[] = [
      mailbox.bundle,
      composeModule(host, composer),
      studioSettingsModule(host, new SettingsController(bridge.settings, () => {})),
    ];
    try {
      bundles.forEach((bundle) => host.registerBuiltin(bundle));
      const keys = expectEveryTitleSwitches(host);
      expect(keys).toContain("settings:maild.mailbox.reading-privacy");
      expect(keys).toContain("module:maild.mailbox");
      await flush();
    } finally {
      bundles.forEach((bundle) => bundle.dispose?.());
      host.stop();
    }
  });
});
