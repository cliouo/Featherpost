// @vitest-environment jsdom
import { applyEffectiveLocales } from "@featherpost/i18n";
import { act, createElement as h, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { BridgeError } from "../src/bridge/errors";
import { buildCommandPaletteCatalog } from "../src/commandPaletteCatalog";
import { ComposeController, COMPOSE_VIEW } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { studioMailbox } from "../src/mailbox";
import { SettingsController } from "../src/settings/controller";
import { LanguageController } from "../src/settings/language";
import { SETTINGS_VIEW, settingsModule } from "../src/settings/module";
import { Studio } from "../src/Shell";
import { closeAllPopovers } from "../src/ui";
import {
  FakeBridge,
  FakeLocale,
  flush,
  folder,
  online,
  summary,
} from "./fakeBridge";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
  if (typeof globalThis.matchMedia !== "function") {
    globalThis.matchMedia = () =>
      ({
        matches: false,
        media: "",
        onchange: null,
        addListener() {},
        removeListener() {},
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: () => false,
      }) as MediaQueryList;
  }
  if (typeof globalThis.ResizeObserver === "undefined") {
    globalThis.ResizeObserver = class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  if (typeof globalThis.PointerEvent === "undefined") {
    class PolyfillPointerEvent extends MouseEvent {
      readonly pointerId: number;
      readonly pointerType: string;
      constructor(type: string, params: PointerEventInit = {}) {
        super(type, params);
        this.pointerId = params.pointerId ?? 1;
        this.pointerType = params.pointerType ?? "mouse";
      }
    }
    globalThis.PointerEvent = PolyfillPointerEvent as unknown as typeof PointerEvent;
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];
const cleanups: Array<() => void> = [];

function mount(node: ReactNode) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  mountedRoots.push({ root, container });
  return { container };
}

function flushPaint() {
  return act(async () => {
    await new Promise((resolve) =>
      requestAnimationFrame(() => setTimeout(resolve, 0)),
    );
  });
}

function flushTimers(ms = 0) {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

function keydown(target: EventTarget, key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    target.dispatchEvent(event);
  });
  return event;
}

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  act(() => {
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function createStudioHarness(options?: {
  accounts?: typeof online.accounts;
  locale?: FakeLocale;
}) {
  const bridge = new FakeBridge();
  if (options?.locale) bridge.locale = options.locale;
  const host = new Host(bridge);
  const composer = new ComposeController(
    bridge.compose,
    (id) => host.openView(id),
    (text) => host.notify(text),
  );
  const settings = new SettingsController(bridge.settings, () => {});
  const language = new LanguageController(bridge.locale);
  const openSettingsWith = (action: () => void) => {
    if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
      action();
      return;
    }
    host.runAfterLeave(SETTINGS_VIEW, () => {
      if (host.openView(SETTINGS_VIEW)) action();
    });
  };
  const openSettingsSection = (sectionId: string) => {
    if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
      settings.selectSection(sectionId);
      return;
    }
    openSettingsWith(() => {
      settings.activate(sectionId);
    });
  };
  const mailbox = studioMailbox(host, composer, () =>
    openSettingsWith(() => settings.openSetup()),
  );
  const bundles = [
    mailbox.bundle,
    composeModule(host, composer),
    settingsModule(host, settings, language),
  ];
  bundles.forEach((bundle) => host.registerBuiltin(bundle));
  host.finishAssembly();
  host.state.patch({
    snapshot: { ...online, accounts: options?.accounts ?? online.accounts },
  });

  const { container } = mount(
    h(Studio, {
      host,
      mailbox: mailbox.controller,
      composer,
      language,
      setup: mailbox.setup,
      manageAccounts: () => openSettingsWith(() => settings.openAccounts()),
      openSettingsSection,
    }),
  );

  cleanups.push(() => {
    bundles.forEach((bundle) => bundle.dispose?.());
    host.stop();
  });

  return {
    container,
    bridge,
    host,
    mailbox: mailbox.controller,
    composer,
    settings,
    language,
    openSettingsSection,
  };
}

function getPaletteDialog() {
  return document.body.querySelector<HTMLElement>(
    '[role="dialog"].command-palette-dialog',
  );
}

function getPaletteInput() {
  return document.body.querySelector<HTMLInputElement>(
    "input.command-palette-input",
  );
}

function getPaletteOptions() {
  return Array.from(
    document.body.querySelectorAll<HTMLElement>(
      '.command-palette-list [role="option"]',
    ),
  );
}

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  while (cleanups.length) {
    cleanups.pop()!();
  }
  closeAllPopovers();
  applyEffectiveLocales([]);
  document.body.innerHTML = "";
  localStorage.clear();
});

describe("Global ⌘K Command Palette", () => {
  it("opens and toggles via ⌘K (including from normal inputs), blocks when another modal is open, and handles focus return vs focus handoff cleanly", async () => {
    const app = createStudioHarness();
    await flushPaint();

    const searchInput = app.container.querySelector<HTMLInputElement>(
      "input[data-mail-search]",
    )!;
    const triggerBtn = app.container.querySelector<HTMLButtonElement>(
      "button.palette-trigger",
    )!;
    expect(triggerBtn).not.toBeNull();
    expect(triggerBtn.getAttribute("title")).toBeTruthy();
    expect(triggerBtn.getAttribute("aria-label")).toBeTruthy();

    // 1. Open via ⌘K from a normal input (mail search), then cancel via Esc -> restores focus to invoker.
    act(() => {
      searchInput.focus();
    });
    keydown(searchInput, "k", { metaKey: true });
    await flushPaint();
    expect(getPaletteDialog()).not.toBeNull();
    expect(document.activeElement).toBe(getPaletteInput());

    keydown(getPaletteInput()!, "Escape");
    await flushTimers();
    expect(getPaletteDialog()).toBeNull();
    expect(document.activeElement).toBe(searchInput);

    // 2. Open via topbar trigger button, toggle closed with ⌘K inside palette -> restores focus to trigger button.
    act(() => {
      triggerBtn.focus();
      triggerBtn.click();
    });
    await flushPaint();
    expect(getPaletteDialog()).not.toBeNull();

    keydown(getPaletteInput()!, "k", { metaKey: true });
    await flushTimers();
    expect(getPaletteDialog()).toBeNull();
    expect(document.activeElement).toBe(triggerBtn);

    // 3. Non-focus-moving action (switch theme to Dark) updates preference and restores focus cleanly.
    act(() => {
      triggerBtn.focus();
      triggerBtn.click();
    });
    await flushPaint();
    setInputValue(getPaletteInput()!, "dark");
    await flushTimers();
    keydown(getPaletteInput()!, "Enter");
    await flushTimers();
    expect(getPaletteDialog()).toBeNull();
    expect(app.host.state.getSnapshot().preferences.theme).toBe("dark");
    expect(document.activeElement).toBe(triggerBtn);

    // 4. Focus-moving action ("Search mail") closes with finalFocus="none" first so inert lifts and searchInput keeps focus without bounce-back.
    act(() => {
      triggerBtn.focus();
      triggerBtn.click();
    });
    await flushPaint();
    setInputValue(getPaletteInput()!, "搜索邮件");
    await flushTimers();
    keydown(getPaletteInput()!, "Enter");
    await flushTimers(20);
    expect(getPaletteDialog()).toBeNull();
    expect(searchInput.closest("[inert]")).toBeNull();
    expect(document.activeElement).toBe(searchInput);

    // 5. Open modal blocks ⌘K.
    const blocker = document.createElement("div");
    blocker.setAttribute("aria-modal", "true");
    document.body.appendChild(blocker);
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    expect(getPaletteDialog()).toBeNull();
    blocker.remove();
  });

  it("scopes Selection group strictly to active mailbox view, shows truncated subject or N selected, and destructive Delete opens confirmation dialog with clean focus handoff", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox"), folder("Archive", "archive")]);
    }
    const row1 = summary(
      "INBOX",
      "1",
      "Quarterly Architecture Review with a Very Long Subject Line That Should Truncate Cleanly",
    );
    const row2 = summary("INBOX", "2", "Follow-up");
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [row1, row2], total: 2 });
    await act(async () => {
      await flush();
    });

    // Nothing selected -> Selection group hidden.
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    let groupLabels = Array.from(
      document.body.querySelectorAll(".command-palette-group-label"),
    ).map((el) => el.textContent);
    expect(groupLabels).not.toContain("当前邮件");
    keydown(getPaletteInput()!, "Escape");
    await flushTimers();

    // Select 1 message -> Selection group appears with truncated subject subtitle.
    act(() => {
      app.mailbox.selectMessage(row1);
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    groupLabels = Array.from(
      document.body.querySelectorAll(".command-palette-group-label"),
    ).map((el) => el.textContent);
    expect(groupLabels).toContain("当前邮件");
    const subtitles = Array.from(
      document.body.querySelectorAll(".command-palette-item-subtitle"),
    ).map((el) => el.textContent ?? "");
    expect(subtitles.some((text) => text.includes("Quarterly Architecture") && text.endsWith("…"))).toBe(true);

    // Trigger destructive Delete from palette -> closes palette with focusHandoff and opens confirmation dialog.
    setInputValue(getPaletteInput()!, "删除");
    await flushTimers();
    keydown(getPaletteInput()!, "Enter");
    await flushPaint();
    expect(getPaletteDialog()).toBeNull();
    expect(app.mailbox.store.getSnapshot().prompt?.kind).toBe("delete");
    const confirmDialog = document.body.querySelector<HTMLElement>(
      '[role="alertdialog"], [role="dialog"]',
    );
    expect(confirmDialog).not.toBeNull();
    expect(confirmDialog!.contains(document.activeElement)).toBe(true);
    // Dismiss confirmation dialog cleanly.
    act(() => {
      app.mailbox.closePrompt();
    });
    await flushTimers();

    // Check 2 messages -> Selection group shows "已选 2 封邮件".
    act(() => {
      app.mailbox.toggle(row1);
      app.mailbox.toggle(row2);
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    const batchSubtitles = Array.from(
      document.body.querySelectorAll(".command-palette-item-subtitle"),
    ).map((el) => el.textContent ?? "");
    expect(batchSubtitles).toContain("已选 2 封邮件");
    keydown(getPaletteInput()!, "Escape");
    await flushTimers();

    // Switch to Settings view while mailbox still holds selection -> Selection group MUST be hidden!
    act(() => {
      app.host.openView(SETTINGS_VIEW);
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    groupLabels = Array.from(
      document.body.querySelectorAll(".command-palette-group-label"),
    ).map((el) => el.textContent);
    expect(groupLabels).not.toContain("当前邮件");
  });

  it("supports bilingual cross-language search (dark in zh-CN, 深色 in en), compact emptyItems vs full folder search, and dynamic settings sections", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([
        folder("INBOX", "inbox"),
        folder("Sent", "sent"),
        folder("Projects/2026", ""),
      ]);
    }
    await act(async () => {
      await flush();
    });

    // Empty query shows accounts + fixed nav, NOT deep custom folders.
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    const emptyOptionTexts = getPaletteOptions().map((el) => el.textContent ?? "");
    expect(emptyOptionTexts.some((t) => t.includes("Projects/2026"))).toBe(false);

    // Typing a folder name searches all cached present folders (showing account path subtitle).
    setInputValue(getPaletteInput()!, "Projects/2026");
    await flushTimers();
    const folderOptions = getPaletteOptions();
    expect(folderOptions.length).toBeGreaterThan(0);
    expect(folderOptions[0]!.textContent).toContain("Projects/2026");
    expect(folderOptions[0]!.textContent).toContain("demo / Projects/2026");

    // In zh-CN locale, typing English "dark" matches "主题：深色".
    setInputValue(getPaletteInput()!, "dark");
    await flushTimers();
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("主题：深色")),
    ).toBe(true);

    // Dynamic settings section search in zh-CN via English keyword "proxy" -> jumps to Network section.
    setInputValue(getPaletteInput()!, "proxy");
    await flushTimers();
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("网络与代理")),
    ).toBe(true);
    keydown(getPaletteInput()!, "Enter");
    await flushTimers();
    expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);
    expect(app.settings.store.getSnapshot().section).toBe("maild.settings.network");

    // Switch UI locale to English -> typing Chinese "深色" matches "Theme: Dark".
    act(() => {
      applyEffectiveLocales(["en-US"]);
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    setInputValue(getPaletteInput()!, "深色");
    await flushTimers();
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("Theme: Dark")),
    ).toBe(true);
  });

  it("renders fallback search button outside the listbox, runs on 0-hit Enter or click, and respects dirty-settings leave-guard", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox"), folder("Work", "")]);
    }
    await act(async () => {
      await flush();
    });
    // Select Work folder so search scope is demo / Work.
    act(() => {
      app.mailbox.selectFolder(folder("Work", ""));
    });
    await act(async () => {
      await flush();
    });
    const queriesBeforeSearch = app.bridge.mailbox.queries.length;

    // 0-hit plain Enter executes fallback mail search with current folder scope.
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    setInputValue(getPaletteInput()!, "invoice-2026-xyz");
    await flushTimers();

    const listbox = document.body.querySelector('[role="listbox"]')!;
    const fallbackBtn = document.body.querySelector<HTMLButtonElement>(
      "button.command-palette-fallback",
    )!;
    expect(fallbackBtn).not.toBeNull();
    expect(listbox.contains(fallbackBtn)).toBe(false);
    expect(fallbackBtn.textContent).toContain("invoice-2026-xyz");
    expect(getPaletteOptions().length).toBe(0);

    keydown(getPaletteInput()!, "Enter");
    await flushTimers();
    expect(getPaletteDialog()).toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBeforeSearch + 1);
    expect(app.mailbox.store.getSnapshot().scope).toEqual({
      kind: "search",
      query: { account: "demo", folder: "Work", query: "invoice-2026-xyz", search: true },
    });

    // Now make Settings dirty and trigger fallback search button by clicking it -> leave-guard blocks until confirmed!
    act(() => {
      app.host.openView(SETTINGS_VIEW);
      app.settings.activate("maild.settings.network");
    });
    await act(async () => {
      await flush();
    });
    act(() => {
      app.settings.store.patch({
        proxyDraft: {
          mode: "custom",
          protocol: "socks5",
          host: "127.0.0.1",
          port: "1080",
          authEnabled: false,
          username: "",
          password: "",
          clearPassword: false,
          bypassText: "",
        },
        proxyBaseline: JSON.stringify({
          mode: "direct",
          protocol: "http",
          host: "",
          port: "",
          authEnabled: false,
          username: "",
          password: "",
          clearPassword: false,
          bypassText: "",
        }),
      });
    });
    expect(app.settings.dirty).toBe(true);
    const queriesBeforeDirty = app.bridge.mailbox.queries.length;

    keydown(window, "k", { metaKey: true });
    await flushPaint();
    setInputValue(getPaletteInput()!, "urgent-bill");
    await flushTimers();
    const dirtyFallbackBtn = document.body.querySelector<HTMLButtonElement>(
      "button.command-palette-fallback",
    )!;
    act(() => {
      dirtyFallbackBtn.click();
    });
    await flushTimers();

    // Confirmation dialog shown; zero queries issued; still on Settings!
    expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBeforeDirty);
    expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);

    // Stay -> still 0 queries and dirty host intact.
    act(() => {
      app.settings.cancelConfirmation();
    });
    await flushTimers();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBeforeDirty);
    expect(app.settings.store.getSnapshot().proxyDraft?.host).toBe("127.0.0.1");
  });

  it("reflects dynamic plugin registerOptional palette commands, excludes non-palette opt-ins, enforces offline/compat write disable, maps zh-Hans language preference, and reads getContextSnapshot once per catalog build", async () => {
    const fakeLocale = new FakeLocale();
    const app = createStudioHarness({ locale: fakeLocale });
    await act(async () => {
      await flush();
    });

    const pluginRan = vi.fn();
    act(() => {
      app.host.registerOptional("ext.sample", {
        title: "Sample Plugin",
        defaultEnabled: true,
        bundle: () => ({
          moduleId: "ext.sample",
          apiVersion: 1,
          title: "Sample Plugin",
          commands: [
            {
              id: "ext.sample.palette-cmd",
              title: "Run Sample Audit",
              locations: ["palette"],
              keywords: ["audit", "sample"],
              execute: pluginRan,
            },
            {
              id: "ext.sample.hidden-cmd",
              title: "Toolbar Only Action",
              locations: ["toolbar"],
              execute: () => {},
            },
          ],
        }),
      });
    });

    // Verify single getContextSnapshot() call per buildCommandPaletteCatalog() invocation.
    const snapSpy = vi.spyOn(app.host, "getContextSnapshot");
    buildCommandPaletteCatalog({
      host: app.host,
      mailbox: app.mailbox,
      view: "standard",
      language: app.language,
      focusSearch: () => {},
      focusSidebarFilter: () => {},
      changeView: () => {},
      openMailbox: () => {},
      openSettingsSection: () => {},
    });
    expect(snapSpy).toHaveBeenCalledTimes(1);
    snapSpy.mockRestore();

    // Open palette -> opted-in plugin command is present; toolbar-only command is absent.
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("Run Sample Audit")),
    ).toBe(true);
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("Toolbar Only Action")),
    ).toBe(false);

    // Language command writes "zh-Hans" (NOT "zh-CN") to daemon preference.
    setInputValue(getPaletteInput()!, "zh-Hans");
    await flushTimers();
    keydown(getPaletteInput()!, "Enter");
    await act(async () => {
      await flush();
    });
    expect(fakeLocale.updates).toEqual([
      { mode: "explicit", languages: ["zh-Hans"] },
    ]);

    // Disable optional plugin -> command disappears immediately.
    act(() => {
      app.host.setModuleEnabled("ext.sample", false);
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    expect(
      getPaletteOptions().some((el) => el.textContent?.includes("Run Sample Audit")),
    ).toBe(false);
    keydown(getPaletteInput()!, "Escape");
    await flushTimers();

    // Offline / unsupported compat disables write commands & language commands.
    fakeLocale.preferenceError = new BridgeError({
      code: "incompatible",
      reason: "locale_unsupported",
      message: "unsupported",
    });
    await act(async () => {
      await app.language.load();
    });
    act(() => {
      app.host.state.patch({
        snapshot: { ...online, phase: "disconnected", compat: "app_outdated" },
      });
    });
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    setInputValue(getPaletteInput()!, "写邮件");
    await flushTimers();
    const composeOption = getPaletteOptions().find((el) =>
      el.textContent?.includes("写邮件"),
    )!;
    expect(composeOption).toBeDefined();
    expect(composeOption.getAttribute("aria-disabled")).toBe("true");
    keydown(getPaletteInput()!, "Enter");
    await flushTimers();
    expect(app.host.state.getSnapshot().activeViewId).not.toBe(COMPOSE_VIEW);

    // Unsupported language option is also disabled.
    setInputValue(getPaletteInput()!, "English");
    await flushTimers();
    const enOption = getPaletteOptions().find((el) =>
      el.textContent?.includes("English"),
    )!;
    expect(enOption).toBeDefined();
    expect(enOption.getAttribute("aria-disabled")).toBe("true");
  });
});
