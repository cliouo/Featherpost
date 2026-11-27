// @vitest-environment jsdom
import { applyEffectiveLocales } from "@featherpost/i18n";
import { act, createElement as h, StrictMode, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { ComposeController } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { studioMailbox } from "../src/mailbox";
import { SettingsController } from "../src/settings/controller";
import { LanguageController } from "../src/settings/language";
import { SETTINGS_VIEW, settingsModule } from "../src/settings/module";
import { Studio } from "../src/Shell";
import { ShortcutKeys } from "../src/ShortcutKeys";
import {
  STUDIO_SHELL_MODULE_ID,
  STUDIO_SHORTCUTS_COMMAND_ID,
  STUDIO_SHORTCUT_KEYS,
  buildShortcutCatalog,
  detectShortcutPlatform,
  formatAriaKeyShortcuts,
  formatShortcutParts,
  formatShortcutText,
  formatShortcutTooltip,
  isQuestionMarkShortcut,
  isShortcutBlockedTarget,
} from "../src/shortcuts";
import { closeAllPopovers } from "../src/ui";
import { FakeBridge, flush, folder, online, summary } from "./fakeBridge";
import { account } from "./fakeSettings";

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
  return {
    container,
    unmount() {
      const idx = mountedRoots.findIndex((entry) => entry.root === root);
      if (idx >= 0) mountedRoots.splice(idx, 1);
      act(() => {
        root.unmount();
      });
      container.remove();
    },
  };
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

function keydown(
  target: EventTarget,
  key: string,
  init: KeyboardEventInit & { keyCode?: number; altGraph?: boolean } = {},
) {
  const { keyCode, altGraph, ...rest } = init;
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...rest,
  });
  if (keyCode !== undefined) {
    Object.defineProperty(event, "keyCode", { value: keyCode });
  }
  if (altGraph) {
    Object.defineProperty(event, "getModifierState", {
      value: (mod: string) => mod === "AltGraph",
    });
  }
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

function createStudioHarness(options?: { strict?: boolean }) {
  const bridge = new FakeBridge();
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
    snapshot: { ...online },
  });

  const studioElement = h(Studio, {
    host,
    mailbox: mailbox.controller,
    composer,
    language,
    setup: mailbox.setup,
    manageAccounts: () => openSettingsWith(() => settings.openAccounts()),
    openSettingsSection,
  });

  const mounted = mount(
    options?.strict ? h(StrictMode, null, studioElement) : studioElement,
  );

  cleanups.push(() => {
    bundles.forEach((bundle) => bundle.dispose?.());
    host.stop();
  });

  return {
    ...mounted,
    bridge,
    host,
    mailbox: mailbox.controller,
    composer,
    settings,
    language,
    openSettingsSection,
  };
}

function getHelpDialog() {
  return document.body.querySelector<HTMLElement>(
    '[role="dialog"].shortcut-help-dialog',
  );
}

function getHelpBackdrop() {
  return document.body.querySelector<HTMLElement>(".shortcut-help-overlay");
}

function getPaletteInput() {
  return document.body.querySelector<HTMLInputElement>(
    "input.command-palette-input",
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
  delete document.body.dataset.studioSettings;
  localStorage.clear();
});

describe("Unified Shortcut Center formatting, ARIA, and explicit catalog", () => {
  it("formats macOS and Windows/Linux shortcuts structurally and renders ShortcutKeys without splitting formatted strings", () => {
    expect(detectShortcutPlatform({ platform: "MacIntel" })).toBe("mac");
    expect(detectShortcutPlatform({ platform: "Win32" })).toBe("other");
    expect(detectShortcutPlatform({ userAgentData: { platform: "macOS" } })).toBe("mac");
    expect(detectShortcutPlatform({ userAgentData: { platform: "Linux" } })).toBe("other");
    expect(detectShortcutPlatform({ userAgent: "Mozilla/5.0 (Windows NT 10.0)" })).toBe("other");

    expect(formatShortcutParts(STUDIO_SHORTCUT_KEYS.focusSearch, "mac")).toEqual([
      "⌘",
      "⇧",
      "F",
    ]);
    expect(formatShortcutParts(STUDIO_SHORTCUT_KEYS.focusSearch, "other")).toEqual([
      "Ctrl",
      "Shift",
      "F",
    ]);
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.focusSearch, "mac")).toBe("⌘ ⇧ F");
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.focusSearch, "other")).toBe(
      "Ctrl+Shift+F",
    );
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.sendMessage, "mac")).toBe("⌘ ↩");
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.sendMessage, "other")).toBe(
      "Ctrl+Enter",
    );
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.openSettings, "mac")).toBe("⌘ ,");
    expect(formatShortcutText(STUDIO_SHORTCUT_KEYS.openSettings, "other")).toBe(
      "Ctrl+,",
    );
    expect(formatShortcutTooltip("Command palette", "mod+k", "mac")).toBe(
      "Command palette ⌘ K",
    );
    expect(formatShortcutTooltip("Command palette", "mod+k", "other")).toBe(
      "Command palette Ctrl+K",
    );

    // WAI-ARIA 1.2: expands mod+k to Meta+K Control+K, omits layout-dependent `?`.
    expect(formatAriaKeyShortcuts(STUDIO_SHORTCUT_KEYS.openPalette)).toBe(
      "Meta+K Control+K",
    );
    expect(formatAriaKeyShortcuts(STUDIO_SHORTCUT_KEYS.focusSearch)).toBe(
      "Meta+Shift+F Control+Shift+F",
    );
    expect(formatAriaKeyShortcuts(STUDIO_SHORTCUT_KEYS.openShortcuts)).toBeUndefined();

    const macView = mount(
      h(ShortcutKeys, {
        shortcut: STUDIO_SHORTCUT_KEYS.focusSearch,
        variant: "keycap",
        platform: "mac",
      }),
    );
    expect(
      Array.from(macView.container.querySelectorAll("kbd")).map((el) => el.textContent),
    ).toEqual(["⌘", "⇧", "F"]);
    expect(macView.container.querySelectorAll(".shortcut-keys-plus").length).toBe(0);

    const otherView = mount(
      h(ShortcutKeys, {
        shortcut: [
          STUDIO_SHORTCUT_KEYS.nextMessage,
          STUDIO_SHORTCUT_KEYS.nextMessageArrow,
        ],
        variant: "keycap",
        platform: "other",
      }),
    );
    expect(
      Array.from(otherView.container.querySelectorAll("kbd")).map(
        (el) => el.textContent,
      ),
    ).toEqual(["J", "↓"]);
    expect(otherView.container.querySelector(".shortcut-keys-alt")?.textContent).toBe(
      "/",
    );
  });

  it("builds shortcut catalog strictly from handler-backed bindings, updates live on locale switch, ignores bogus extension shortcuts, and never hides offline rows", () => {
    const app = createStudioHarness();

    // Register an optional extension command claiming a fake unbound shortcut.
    act(() => {
      app.host.registerOptional("ext.fake", {
        title: "Fake Plugin",
        defaultEnabled: true,
        bundle: () => ({
          moduleId: "ext.fake",
          apiVersion: 1,
          title: "Fake Plugin",
          commands: [
            {
              id: "ext.fake.unbound",
              title: "Bogus Unbound Action",
              shortcut: "e",
              locations: ["palette"],
              execute: () => {},
            },
          ],
        }),
      });

      // Go offline so transient `canExecute` for `maild.compose.new` becomes false.
      app.host.state.patch({
        snapshot: { ...online, phase: "disconnected", revision: 2 },
      });
    });
    expect(app.host.canExecute("maild.compose.new")).toBe(false);

    const zhCatalog = buildShortcutCatalog(app.host.registry.getSnapshot());
    const zhItems = zhCatalog.flatMap((g) => g.items);
    const zhKeys = zhItems.flatMap((i) => i.keys);

    // Must NOT contain bogus extension command or unbound Gmail-style keys.
    expect(zhItems.some((i) => i.label.includes("Bogus"))).toBe(false);
    for (const unbound of ["e", "r", "a", "f", "s", "u"]) {
      expect(zhKeys).not.toContain(unbound);
    }

    // Compose row remains visible even while offline, with explicit scope note.
    const zhCompose = zhItems.find((i) => i.commandId === "maild.compose.new");
    expect(zhCompose?.label).toBe("写邮件");
    expect(zhCompose?.scope).toBe("后台在线时可用");
    expect(
      zhItems.find((i) => i.commandId === STUDIO_SHORTCUTS_COMMAND_ID)?.label,
    ).toBe("键盘快捷键");

    // Live locale switch updates titles immediately from registry getters.
    act(() => {
      applyEffectiveLocales(["en-US"]);
    });
    const enCatalog = buildShortcutCatalog(app.host.registry.getSnapshot());
    const enItems = enCatalog.flatMap((g) => g.items);
    expect(enItems.find((i) => i.commandId === "maild.compose.new")?.label).toBe(
      "New message",
    );
    expect(
      enItems.find((i) => i.commandId === STUDIO_SHORTCUTS_COMMAND_ID)?.label,
    ).toBe("Keyboard shortcuts");
    expect(enItems.find((i) => i.commandId === "maild.settings.open")?.label).toBe(
      "Open Settings",
    );
  });
});

describe("`?` trigger, keyboard guards, focus restoration, and two-way handoff", () => {
  it("opens via `?`, toggles closed via `?`, closes via Esc/backdrop without clearing selection, and restores focus (with disconnected fallback)", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    const row = summary("INBOX", "1", "Important thread");
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [row], total: 1 });
    await act(async () => {
      await flush();
    });
    act(() => {
      app.mailbox.selectMessage(row);
    });
    await flushPaint();

    // 1. Open via `?` from a focusable non-input button, toggle closed via `?` inside dialog -> restores focus.
    const syncBtn = app.container.querySelector<HTMLButtonElement>("button.sync-status")!;
    act(() => {
      syncBtn.focus();
    });
    keydown(syncBtn, "?", { shiftKey: true });
    await flushPaint();
    const dialog = getHelpDialog();
    expect(dialog).not.toBeNull();

    // Repeat or IME `?` inside dialog does NOT close it.
    keydown(dialog!, "?", { shiftKey: true, repeat: true });
    expect(getHelpDialog()).not.toBeNull();
    keydown(dialog!, "?", { shiftKey: true, isComposing: true });
    expect(getHelpDialog()).not.toBeNull();

    // Clean `?` inside dialog toggles closed and restores focus to invoker.
    keydown(dialog!, "?", { shiftKey: true });
    await flushTimers();
    expect(getHelpDialog()).toBeNull();
    expect(document.activeElement).toBe(syncBtn);

    // 2. Open via `?`, close via Esc -> message selection in Workspace stays intact!
    keydown(syncBtn, "?", { shiftKey: true });
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();
    expect(getHelpDialog()).toBeNull();
    expect(app.mailbox.store.getSnapshot().selected?.uid).toBe("1");
    expect(document.activeElement).toBe(syncBtn);

    // 3. Backdrop click closes dialog cleanly.
    keydown(syncBtn, "?", { shiftKey: true });
    await flushPaint();
    const backdrop = getHelpBackdrop()!;
    expect(backdrop).not.toBeNull();
    act(() => {
      backdrop.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          cancelable: true,
          button: 0,
          pointerType: "mouse",
        }),
      );
      backdrop.dispatchEvent(
        new PointerEvent("pointerup", {
          bubbles: true,
          cancelable: true,
          button: 0,
          pointerType: "mouse",
        }),
      );
    });
    await flushTimers();
    expect(getHelpDialog()).toBeNull();

    // 4. Disconnected invoker falls back to stable sidebar shortcuts button instead of body.
    const transientBtn = document.createElement("button");
    transientBtn.textContent = "Transient";
    app.container.appendChild(transientBtn);
    act(() => {
      transientBtn.focus();
    });
    keydown(transientBtn, "?", { shiftKey: true });
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    transientBtn.remove();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();
    expect(getHelpDialog()).toBeNull();
    const sidebarTrigger = app.container.querySelector<HTMLButtonElement>(
      '.sidebar-footer button[aria-label="键盘快捷键"]',
    )!;
    expect(document.activeElement).toBe(sidebarTrigger);
  });

  it("performs seamless two-way focus handoff: Search input -> Cmd+K -> Shortcuts Help -> Esc -> SAME Search input", async () => {
    const app = createStudioHarness();
    await flushPaint();

    const searchInput = app.container.querySelector<HTMLInputElement>(
      "input[data-mail-search]",
    )!;
    expect(searchInput).not.toBeNull();

    // Focus search input -> open palette with Cmd+K.
    act(() => {
      searchInput.focus();
    });
    expect(document.activeElement).toBe(searchInput);
    keydown(searchInput, "k", { metaKey: true });
    await flushPaint();

    const paletteInput = getPaletteInput()!;
    expect(paletteInput).not.toBeNull();
    expect(document.activeElement).toBe(paletteInput);

    // Search for shortcuts command and run it via Enter.
    setInputValue(paletteInput, "键盘快捷键");
    await flushTimers();
    keydown(paletteInput, "Enter");
    await flushPaint();

    // Palette closed cleanly and ShortcutHelpDialog opened with focus inside it.
    expect(getPaletteInput()).toBeNull();
    const helpDialog = getHelpDialog();
    expect(helpDialog).not.toBeNull();
    expect(helpDialog!.contains(document.activeElement)).toBe(true);

    // Press Esc -> closes Help and restores focus directly to the ORIGINAL Search input!
    keydown(helpDialog!, "Escape");
    await flushTimers(20);
    expect(getHelpDialog()).toBeNull();
    expect(searchInput.closest("[inert]")).toBeNull();
    expect(document.activeElement).toBe(searchInput);
  });

  it("blocks `?` on all text inputs, contenteditable variants, chip inputs, ARIA widgets, IME, fullwidth ？, Shift+/, modifiers, repeats, shortcuts-off, and modals", async () => {
    const app = createStudioHarness();
    await flushPaint();

    // Unit-level recognizer checks.
    expect(
      isQuestionMarkShortcut({
        key: "?",
        ctrlKey: false,
        metaKey: false,
        altKey: false,
        repeat: false,
        defaultPrevented: false,
        isComposing: false,
        keyCode: 191,
      }),
    ).toBe(true);
    // Rejects Shift+/ (when key is "/"), fullwidth ？, modifiers, AltGraph, repeat, IME, defaultPrevented.
    expect(
      isQuestionMarkShortcut({
        key: "/",
        ctrlKey: false,
        metaKey: false,
        altKey: false,
        repeat: false,
        defaultPrevented: false,
        isComposing: false,
        keyCode: 191,
      }),
    ).toBe(false);
    expect(
      isQuestionMarkShortcut({
        key: "\uFF1F",
        ctrlKey: false,
        metaKey: false,
        altKey: false,
        repeat: false,
        defaultPrevented: false,
        isComposing: false,
        keyCode: 191,
      }),
    ).toBe(false);
    expect(
      isQuestionMarkShortcut({
        key: "?",
        ctrlKey: false,
        metaKey: false,
        altKey: false,
        repeat: false,
        defaultPrevented: false,
        isComposing: false,
        keyCode: 191,
        getModifierState: (m) => m === "AltGraph",
      }),
    ).toBe(false);

    // Target-level contenteditable checks (bare, empty, true, plaintext-only, inherited, and false).
    const ceBare = document.createElement("div");
    ceBare.setAttribute("contenteditable", "");
    const ceChild = document.createElement("span");
    ceBare.appendChild(ceChild);
    const cePlain = document.createElement("div");
    cePlain.setAttribute("contenteditable", "plaintext-only");
    const ceFalse = document.createElement("div");
    ceFalse.setAttribute("contenteditable", "false");
    const chipBox = document.createElement("div");
    chipBox.setAttribute("data-chip-input", "");
    const chipChild = document.createElement("span");
    chipBox.appendChild(chipChild);

    expect(isShortcutBlockedTarget(ceBare)).toBe(true);
    expect(isShortcutBlockedTarget(ceChild)).toBe(true);
    expect(isShortcutBlockedTarget(cePlain)).toBe(true);
    expect(isShortcutBlockedTarget(ceFalse)).toBe(false);
    expect(isShortcutBlockedTarget(chipChild)).toBe(true);

    for (const role of [
      "textbox",
      "searchbox",
      "spinbutton",
      "checkbox",
      "switch",
      "combobox",
      "listbox",
      "option",
      "menu",
      "menuitem",
    ]) {
      const el = document.createElement("div");
      el.setAttribute("role", role);
      expect(isShortcutBlockedTarget(el), `role=${role}`).toBe(true);
    }

    // Live Studio keydown checks: none of these should open Help.
    const inputs: HTMLElement[] = [
      app.container.querySelector<HTMLInputElement>("input[data-mail-search]")!,
      document.createElement("textarea"),
      document.createElement("select"),
      ceBare,
      cePlain,
      chipBox,
    ];
    for (const el of inputs) {
      if (!el.isConnected) app.container.appendChild(el);
      el.tabIndex = 0;
      el.focus();
      keydown(el, "?", { shiftKey: true });
      expect(getHelpDialog()).toBeNull();
    }

    // Blur to body; reject Shift+/, fullwidth ？, IME, keyCode 229, Ctrl/Meta/Alt/AltGraph, repeat, defaultPrevented.
    (document.activeElement as HTMLElement | null)?.blur();
    keydown(document.body, "/", { shiftKey: true, code: "Slash" });
    keydown(document.body, "\uFF1F");
    keydown(document.body, "?", { isComposing: true });
    keydown(document.body, "?", { keyCode: 229 });
    keydown(document.body, "?", { ctrlKey: true });
    keydown(document.body, "?", { metaKey: true });
    keydown(document.body, "?", { altKey: true });
    keydown(document.body, "?", { altGraph: true });
    keydown(document.body, "?", { repeat: true });
    const prevented = new KeyboardEvent("keydown", {
      key: "?",
      bubbles: true,
      cancelable: true,
    });
    prevented.preventDefault();
    act(() => {
      window.dispatchEvent(prevented);
    });
    expect(getHelpDialog()).toBeNull();

    // Visible [data-shortcuts-off] blocks `?`, hidden does NOT.
    const gate = document.createElement("div");
    gate.setAttribute("data-shortcuts-off", "");
    document.body.appendChild(gate);
    keydown(document.body, "?");
    expect(getHelpDialog()).toBeNull();
    gate.hidden = true;

    // Open modal blocks `?`.
    const modal = document.createElement("div");
    modal.setAttribute("aria-modal", "true");
    document.body.appendChild(modal);
    keydown(document.body, "?");
    expect(getHelpDialog()).toBeNull();
    modal.remove();

    // Now unblocked `?` opens Help cleanly.
    keydown(document.body, "?");
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
  });

  it("opens Help from all discoverable UI triggers and keeps dirty Settings completely untouched without leave-guard confirmation", async () => {
    const app = createStudioHarness();
    await flushPaint();

    // 1. Sidebar footer trigger.
    const sidebarBtn = app.container.querySelector<HTMLButtonElement>(
      '.sidebar-footer button[aria-label="键盘快捷键"]',
    )!;
    expect(sidebarBtn).not.toBeNull();
    expect(sidebarBtn.getAttribute("title")).toBeNull();
    expect(sidebarBtn.getAttribute("aria-keyshortcuts")).toBeNull();
    const shortcutsTooltipId = sidebarBtn.getAttribute("aria-labelledby");
    expect(shortcutsTooltipId).toBeTruthy();
    const shortcutsTooltip = document.getElementById(shortcutsTooltipId!);
    expect(shortcutsTooltip?.getAttribute("role")).toBe("tooltip");
    expect(shortcutsTooltip?.textContent).toContain("?");
    act(() => sidebarBtn.click());
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();

    // 2. Workspace list footer trigger.
    const footerBtn = app.container.querySelector<HTMLButtonElement>(
      "button.list-footer-help",
    )!;
    expect(footerBtn).not.toBeNull();
    act(() => footerBtn.click());
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();

    // 3. Empty ReadingPane trigger.
    const emptyBtn = app.container.querySelector<HTMLButtonElement>(
      "button.empty-shortcuts-button",
    )!;
    expect(emptyBtn).not.toBeNull();
    act(() => emptyBtn.click());
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();

    // 4. Dirty Settings invariance: open Settings with unsaved Account edits.
    act(() => {
      app.host.openView(SETTINGS_VIEW);
      app.settings.beginAccount(account());
      app.settings.setDisplayName("未保存的账户名");
    });
    await flushPaint();
    expect(app.settings.dirty).toBe(true);
    const selectSectionSpy = vi.spyOn(app.settings, "selectSection");

    // 4a. Open & close Help via `?` while Settings is dirty -> ZERO confirm dialog, ZERO selectSection, dirty field preserved.
    (document.activeElement as HTMLElement | null)?.blur();
    keydown(document.body, "?", { shiftKey: true });
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    expect(app.settings.store.getSnapshot().confirm).toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();
    expect(getHelpDialog()).toBeNull();
    expect(selectSectionSpy).not.toHaveBeenCalled();
    expect(app.settings.store.getSnapshot().confirm).toBeNull();
    expect(app.settings.store.getSnapshot().editor?.draft.display_name).toBe(
      "未保存的账户名",
    );

    // 4b. Open & close Help via Cmd+K -> Shortcuts while Settings is dirty -> ZERO confirm dialog!
    keydown(document.body, "k", { metaKey: true });
    await flushPaint();
    setInputValue(getPaletteInput()!, "键盘快捷键");
    await flushTimers();
    keydown(getPaletteInput()!, "Enter");
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    expect(app.settings.store.getSnapshot().confirm).toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();
    expect(selectSectionSpy).not.toHaveBeenCalled();
    expect(app.settings.store.getSnapshot().editor?.draft.display_name).toBe(
      "未保存的账户名",
    );

    // 5. Switch to Appearance section and test the read-only "View keyboard shortcuts" button.
    act(() => {
      app.settings.store.patch({ editor: null });
      app.settings.activate("maild.settings.appearance");
    });
    await flushPaint();
    const appearanceBtn = Array.from(
      app.container.querySelectorAll<HTMLButtonElement>(
        ".studio-appearance-shortcuts button",
      ),
    )[0]!;
    expect(appearanceBtn).not.toBeNull();
    expect(appearanceBtn.type).toBe("button");
    act(() => appearanceBtn.click());
    await flushPaint();
    expect(getHelpDialog()).not.toBeNull();
    expect(app.settings.store.getSnapshot().confirm).toBeNull();
    keydown(getHelpDialog()!, "Escape");
    await flushTimers();
    expect(getHelpDialog()).toBeNull();
  });

  it("handles React StrictMode mount/unmount/remount with exactly 1 maild.studio.shortcuts command and cleans up on unmount", () => {
    const app = createStudioHarness({ strict: true });

    const matches = app.host.registry
      .getSnapshot()
      .commands.filter(({ value }) => value.id === STUDIO_SHORTCUTS_COMMAND_ID);
    expect(matches).toHaveLength(1);
    expect(app.host.registry.has(STUDIO_SHELL_MODULE_ID)).toBe(true);

    app.unmount();
    expect(app.host.registry.findCommand(STUDIO_SHORTCUTS_COMMAND_ID)).toBeUndefined();
    expect(app.host.registry.has(STUDIO_SHELL_MODULE_ID)).toBe(false);
  });

  it("enforces hardened mod+n guards in composeModule (IME, shortcuts-off, Alt/Shift/repeat/defaultPrevented)", async () => {
    const app = createStudioHarness();
    const createSpy = vi
      .spyOn(app.composer, "create")
      .mockImplementation(async () => {});

    // Blocked cases: IME isComposing, keyCode 229, Alt, Shift, repeat, defaultPrevented, visible shortcuts-off.
    keydown(window, "n", { metaKey: true, isComposing: true });
    keydown(window, "n", { metaKey: true, keyCode: 229 });
    keydown(window, "n", { metaKey: true, altKey: true });
    keydown(window, "n", { metaKey: true, shiftKey: true });
    keydown(window, "n", { metaKey: true, repeat: true });
    const prevented = new KeyboardEvent("keydown", {
      key: "n",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    prevented.preventDefault();
    act(() => {
      window.dispatchEvent(prevented);
    });
    const gate = document.createElement("div");
    gate.setAttribute("data-shortcuts-off", "");
    document.body.appendChild(gate);
    keydown(window, "n", { metaKey: true });
    gate.remove();

    expect(createSpy).not.toHaveBeenCalled();

    // Clean Meta+N triggers composer.create() once.
    keydown(window, "n", { metaKey: true });
    expect(createSpy).toHaveBeenCalledTimes(1);
  });

  it("leaves zero hardcoded ⌘ glyph literals in Studio UI components outside shortcuts.ts", () => {
    const sources = import.meta.glob<string>("../src/**/*.{ts,tsx}", {
      eager: true,
      query: "?raw",
      import: "default",
    });
    const offenders: string[] = [];
    for (const [path, source] of Object.entries(sources)) {
      if (path.endsWith("/shortcuts.ts")) continue;
      const stripped = source
        .replace(/\/\*[\s\S]*?\*\//g, "")
        .replace(/\/\/[^\n]*/g, "");
      if (stripped.includes("⌘")) {
        offenders.push(path);
      }
    }
    expect(offenders).toEqual([]);
  });
});
