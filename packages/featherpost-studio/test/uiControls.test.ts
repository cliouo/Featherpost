// @vitest-environment jsdom
import {
  act,
  createElement as h,
  useState,
  type ReactNode,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { ComposeController } from "../src/compose/controller";
import { Host } from "../src/host/host";
import { MailboxController, messageKey } from "../src/mailbox/controller";
import { DocumentController } from "../src/mailbox/document";
import { Extensions } from "../src/settings/Extensions";
import { NetworkProxy } from "../src/settings/NetworkProxy";
import { Notifications } from "../src/settings/Notifications";
import { SettingsController } from "../src/settings/controller";
import { settingsModule } from "../src/settings/module";
import { isGlobalShortcutBlocked } from "../src/Shell";
import {
  Checkbox,
  StudioPopover,
  StudioPopoverClose,
  StudioTabs,
  StudioTabsList,
  StudioTabsPanel,
  StudioTabsTab,
  Switch,
  Z_INDEX_TOKENS,
  closeAllPopovers,
} from "../src/ui";
import { Workspace, isShortcutBlockedTarget } from "../src/Workspace";
import { FakeBridge, folder, online, summary } from "./fakeBridge";
import { FakeCompose } from "./fakeCompose";
import { FakeSettings, modernRules } from "./fakeSettings";

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
    rerender(next: ReactNode) {
      act(() => {
        root.render(next);
      });
    },
  };
}

function pressSpace(el: HTMLElement, init?: KeyboardEventInit) {
  act(() => {
    el.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: " ",
        bubbles: true,
        cancelable: true,
        ...init,
      }),
    );
    el.dispatchEvent(
      new KeyboardEvent("keyup", {
        key: " ",
        bubbles: true,
        cancelable: true,
        ...init,
      }),
    );
  });
}

function pressKey(el: HTMLElement, key: string, init?: KeyboardEventInit) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    el.dispatchEvent(event);
  });
  return event;
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
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

describe("ui/Switch", () => {
  it("toggles on click, Space, and Enter without submitting enclosing forms, and honors disabled / fieldset disabled", () => {
    const onSubmit = vi.fn((e: Event) => e.preventDefault());
    const changes: boolean[] = [];

    function Harness({
      disabled = false,
      fieldsetDisabled = false,
    }: {
      disabled?: boolean;
      fieldsetDisabled?: boolean;
    }) {
      const [checked, setChecked] = useState(false);
      return h(
        "form",
        { onSubmit: (e: React.FormEvent) => onSubmit(e.nativeEvent) },
        h(
          "fieldset",
          { disabled: fieldsetDisabled },
          h(Switch, {
            checked,
            disabled,
            "aria-label": "Enable feature",
            onCheckedChange: (next) => {
              changes.push(next);
              setChecked(next);
            },
          }),
          h("button", { type: "submit" }, "Save"),
        ),
      );
    }

    const { container, rerender } = mount(h(Harness));
    const sw = container.querySelector<HTMLButtonElement>('[role="switch"]')!;
    expect(sw).not.toBeNull();
    expect(sw.getAttribute("aria-label")).toBe("Enable feature");
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(sw.className).toContain("toggle");
    expect(sw.querySelector(".toggle-track .toggle-thumb")).not.toBeNull();

    // Click toggles on
    act(() => {
      sw.click();
    });
    expect(sw.getAttribute("aria-checked")).toBe("true");
    expect(changes).toEqual([true]);

    // Space toggles off
    sw.focus();
    pressSpace(sw);
    expect(sw.getAttribute("aria-checked")).toBe("false");
    expect(changes).toEqual([true, false]);

    // Enter toggles on without submitting the enclosing form
    const enterEvent = pressKey(sw, "Enter");
    expect(enterEvent.defaultPrevented).toBe(true);
    expect(onSubmit).not.toHaveBeenCalled();
    expect(sw.getAttribute("aria-checked")).toBe("true");
    expect(changes).toEqual([true, false, true]);

    // Explicit disabled blocks click/Space/Enter
    rerender(h(Harness, { disabled: true }));
    expect(sw.disabled).toBe(true);
    expect(sw.getAttribute("aria-disabled")).toBe("true");
    act(() => {
      sw.click();
    });
    pressSpace(sw);
    pressKey(sw, "Enter");
    expect(changes).toEqual([true, false, true]);

    // Ancestor <fieldset disabled> also blocks toggling
    rerender(h(Harness, { disabled: false, fieldsetDisabled: true }));
    expect(sw.matches(":disabled")).toBe(true);
    act(() => {
      sw.click();
    });
    pressSpace(sw);
    pressKey(sw, "Enter");
    expect(changes).toEqual([true, false, true]);
  });

  it("integrates across Notifications, Extensions, NetworkProxy, and settingsModule optional modules", async () => {
    // 1. Notifications (3 switches)
    const notifBridge = new FakeSettings();
    notifBridge.rules = async () => ({ rules: modernRules(), persisted: true });
    const notifCtrl = new SettingsController(notifBridge);
    cleanups.push(() => notifCtrl.dispose());
    notifCtrl.connection(online);
    notifCtrl.activate("maild.settings.notifications");
    await notifCtrl.load();

    const notifView = mount(h(Notifications, { controller: notifCtrl }));
    const notifSwitches = Array.from(
      notifView.container.querySelectorAll<HTMLButtonElement>('[role="switch"]'),
    );
    expect(notifSwitches).toHaveLength(3);
    expect(notifSwitches.map((el) => el.getAttribute("aria-checked"))).toEqual([
      "false",
      "false",
      "false",
    ]);
    act(() => {
      notifSwitches[1]!.click();
    });
    expect(notifCtrl.store.getSnapshot().rules?.show_sensitive).toBe(true);
    expect(notifSwitches[1]!.getAttribute("aria-checked")).toBe("true");

    // 2. Extensions (plugin switch)
    const extCtrl = new SettingsController(new FakeSettings());
    cleanups.push(() => extCtrl.dispose());
    extCtrl.connection(online);
    extCtrl.store.patch({
      packages: {
        default: "studio",
        frontends: [],
        plugins: [
          {
            id: "org.example.plugin",
            name: "Example Plugin",
            version: "1.0.0",
            lifecycle: "on_demand",
            capabilities: [],
            enabled: true,
            state: "running",
            has_ui: false,
            provider_registered: false,
          },
        ],
      },
    });
    const extView = mount(h(Extensions, { controller: extCtrl }));
    const pluginSwitch = extView.container.querySelector<HTMLButtonElement>('[role="switch"]')!;
    expect(pluginSwitch.getAttribute("aria-checked")).toBe("true");
    act(() => {
      pluginSwitch.click();
    });
    expect(extCtrl.store.getSnapshot().confirm).not.toBeNull();

    // 3. NetworkProxy (custom proxy auth switch)
    const proxyCtrl = new SettingsController(new FakeSettings());
    cleanups.push(() => proxyCtrl.dispose());
    proxyCtrl.connection(online);
    proxyCtrl.activate("maild.settings.network");
    await proxyCtrl.load();
    proxyCtrl.editProxy({ mode: "custom" });
    const proxyView = mount(h(NetworkProxy, { controller: proxyCtrl }));
    const authSwitch = proxyView.container.querySelector<HTMLButtonElement>('[role="switch"]')!;
    expect(authSwitch.getAttribute("aria-checked")).toBe("false");
    pressKey(authSwitch, "Enter");
    expect(proxyCtrl.store.getSnapshot().proxyDraft?.authEnabled).toBe(true);
    expect(authSwitch.getAttribute("aria-checked")).toBe("true");

    // 4. settingsModule optional module switch
    const host = new Host(new FakeBridge());
    const setEnabled = vi.spyOn(host, "setModuleEnabled");
    host.registerOptional("demo.optional", {
      title: "Optional Module",
      defaultEnabled: false,
      bundle: () => ({ moduleId: "demo.optional", apiVersion: 1, title: "Optional Module" }),
    });
    const bundle = settingsModule(host);
    cleanups.push(() => {
      bundle.dispose?.();
      host.stop();
    });
    const ExtensionsSection = bundle.settings!.find(
      (s) => s.id === "maild.settings.extensions",
    )!.render;
    const modView = mount(h(ExtensionsSection, { active: true }));
    const modSwitch = modView.container.querySelector<HTMLButtonElement>('[role="switch"]')!;
    expect(modSwitch.getAttribute("aria-checked")).toBe("false");
    act(() => {
      modSwitch.click();
    });
    expect(setEnabled).toHaveBeenCalledWith("demo.optional", true);
  });
});

describe("ui/Checkbox", () => {
  it("supports controlled/indeterminate states, click/Space toggle, non-toggling Enter, and disabled", () => {
    const calls: Array<{ checked: boolean; shiftKey: boolean }> = [];

    function Harness({
      initial = false as boolean | "indeterminate",
      disabled = false,
    }) {
      const [checked, setChecked] = useState<boolean | "indeterminate">(initial);
      return h(Checkbox, {
        checked,
        disabled,
        "aria-label": "Select message",
        onCheckedChange: (next, details) => {
          calls.push({ checked: next, shiftKey: details.shiftKey });
          setChecked(next);
        },
      });
    }

    const { container, rerender } = mount(h(Harness, { initial: "indeterminate" }));
    const box = container.querySelector<HTMLButtonElement>('[role="checkbox"]')!;
    expect(box).not.toBeNull();
    expect(box.tagName).toBe("BUTTON");
    expect(box.getAttribute("aria-checked")).toBe("mixed");
    expect(box.hasAttribute("data-indeterminate")).toBe(true);
    expect(box.textContent).toBe("");
    expect(box.querySelector(".studio-checkbox-indicator svg")).not.toBeNull();

    // Enter does NOT toggle a checkbox (matches native <input type="checkbox">)
    pressKey(box, "Enter");
    expect(calls).toEqual([]);
    expect(box.getAttribute("aria-checked")).toBe("mixed");

    // Space transitions indeterminate -> checked
    pressSpace(box);
    expect(calls).toEqual([{ checked: true, shiftKey: false }]);
    expect(box.getAttribute("aria-checked")).toBe("true");

    // Shift-click passes shiftKey: true and toggles off
    act(() => {
      box.dispatchEvent(
        new MouseEvent("click", { bubbles: true, cancelable: true, shiftKey: true }),
      );
    });
    expect(calls).toEqual([
      { checked: true, shiftKey: false },
      { checked: false, shiftKey: true },
    ]);
    expect(box.getAttribute("aria-checked")).toBe("false");

    // Disabled blocks click & Space
    rerender(h(Harness, { disabled: true }));
    act(() => {
      box.click();
    });
    pressSpace(box);
    expect(calls).toHaveLength(2);
  });

  it("gates Workspace single-key shortcuts without blocking Shell global Cmd shortcuts, supports row multi-select & Shift-range, keeps aria-pressed filters, and never adds role=listitem", async () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const controller = new MailboxController(bridge);
    const documents = new DocumentController(bridge);
    const composer = new ComposeController(new FakeCompose(), () => {}, () => {});
    cleanups.push(() => {
      composer.stop();
      documents.stop();
      controller.stop();
      host.stop();
    });

    controller.acceptSnapshot(online);
    for (const req of bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    const rows = [
      { ...summary("INBOX", "1", "First subject"), snippet: "Private snippet 1" },
      { ...summary("INBOX", "2", "Second subject"), snippet: "Private snippet 2" },
      { ...summary("INBOX", "3", "Third subject"), snippet: "Private snippet 3" },
    ];
    bridge.mailbox.queries[0]?.d.resolve({ messages: rows, total: rows.length });
    await Promise.resolve();

    const { container } = mount(
      h(Workspace, {
        host,
        controller,
        documents,
        composer,
        setup: () => {},
      }),
    );
    await act(async () => {
      await Promise.resolve();
    });

    // Critical invariants: no role="listitem" on <article>, filter buttons remain aria-pressed
    const articles = Array.from(container.querySelectorAll("article.message-row"));
    expect(articles).toHaveLength(3);
    for (const article of articles) {
      expect(article.getAttribute("role")).toBeNull();
    }
    const filterButtons = Array.from(
      container.querySelectorAll<HTMLButtonElement>(".filter-tabs button"),
    );
    expect(filterButtons).toHaveLength(2);
    expect(filterButtons.map((b) => b.getAttribute("aria-pressed"))).toEqual([
      "true",
      "false",
    ]);
    expect(container.querySelector('[role="tablist"]')).toBeNull();

    const checkboxes = Array.from(
      container.querySelectorAll<HTMLButtonElement>('button.row-check[role="checkbox"]'),
    );
    expect(checkboxes).toHaveLength(3);
    // Accessible label does not leak message snippet text
    expect(checkboxes[0]!.getAttribute("aria-label")).toContain("First subject");
    expect(checkboxes[0]!.getAttribute("aria-label")).not.toContain("Private snippet");
    expect(checkboxes[0]!.textContent).toBe("");

    // Shortcut safety: Workspace single-key shortcuts are blocked on Checkbox,
    // while Shell global Cmd shortcuts remain unblocked.
    expect(isShortcutBlockedTarget(checkboxes[0])).toBe(true);
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true, cancelable: true }),
      ),
    ).toBe(false);

    // Focusing row checkbox and pressing j/k/ArrowDown/Escape must NOT navigate or clear selection
    controller.selectMessage(rows[0]!);
    await act(async () => {
      await Promise.resolve();
    });
    checkboxes[0]!.focus();
    const stepSpy = vi.spyOn(controller, "step");
    const clearSpy = vi.spyOn(controller, "clearSelection");
    pressKey(checkboxes[0]!, "j");
    pressKey(checkboxes[0]!, "ArrowDown");
    pressKey(checkboxes[0]!, "Escape");
    expect(stepSpy).not.toHaveBeenCalled();
    expect(clearSpy).not.toHaveBeenCalled();

    // Clicking row 0 checkbox checks row 0 without changing selected message;
    // Shift-clicking row 2 checks the full range [0, 1, 2].
    act(() => {
      checkboxes[0]!.click();
    });
    expect(controller.state.checked.map(messageKey)).toEqual([messageKey(rows[0]!)]);
    act(() => {
      checkboxes[2]!.dispatchEvent(
        new MouseEvent("click", { bubbles: true, cancelable: true, shiftKey: true }),
      );
    });
    expect(controller.state.checked.map(messageKey)).toEqual(rows.map(messageKey));
  });
});

describe("ui/Popover", () => {
  it("opens/closes via trigger, close button, Escape, and dismisses once on window blur, visibilitychange(hidden), pagehide, and closeAllPopovers()", () => {
    const onOpenChange = vi.fn();

    mount(
      h(
        StudioPopover,
        {
          title: "Filter options",
          description: "Narrow your results",
          onOpenChange,
          trigger: h("button", { type: "button" }, "Open popover"),
        },
        h("p", null, "Popover body"),
        h(StudioPopoverClose, null, "Done"),
      ),
    );

    const trigger = document.querySelector<HTMLButtonElement>("button")!;
    expect(trigger.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(document.querySelector(".studio-popover")).toBeNull();

    // Open via trigger click
    act(() => {
      trigger.click();
    });
    const content = document.querySelector<HTMLElement>(".studio-popover")!;
    expect(content).not.toBeNull();
    expect(content.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(content.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(isShortcutBlockedTarget(content)).toBe(true);
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true, cancelable: true }),
      ),
    ).toBe(true);

    // Safari 16 fallback (!supportsTopLayer() in jsdom) assigns monotonic inline zIndex > Z_INDEX_TOKENS.popover
    const zIndex = Number(content.style.zIndex);
    expect(zIndex).toBeGreaterThan(Z_INDEX_TOKENS.popover);

    // Window blur dismisses exactly once
    act(() => {
      window.dispatchEvent(new Event("blur"));
      window.dispatchEvent(new Event("blur"));
    });
    expect(document.querySelector(".studio-popover")).toBeNull();
    expect(onOpenChange.mock.calls.filter(([open]) => open === false)).toHaveLength(1);
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true, cancelable: true }),
      ),
    ).toBe(false);

    // Reopen and dismiss via visibilitychange(hidden)
    act(() => {
      trigger.click();
    });
    expect(document.querySelector(".studio-popover")).not.toBeNull();
    const hiddenSpy = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    hiddenSpy.mockRestore();
    expect(document.querySelector(".studio-popover")).toBeNull();

    // Reopen and dismiss via pagehide
    act(() => {
      trigger.click();
    });
    expect(document.querySelector(".studio-popover")).not.toBeNull();
    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });
    expect(document.querySelector(".studio-popover")).toBeNull();

    // Reopen and dismiss via closeAllPopovers()
    act(() => {
      trigger.click();
    });
    expect(document.querySelector(".studio-popover")).not.toBeNull();
    act(() => {
      closeAllPopovers();
    });
    expect(document.querySelector(".studio-popover")).toBeNull();

    // Reopen and dismiss via StudioPopoverClose button
    act(() => {
      trigger.click();
    });
    const closeBtn = document.querySelector<HTMLButtonElement>(".studio-popover-close")!;
    act(() => {
      closeBtn.click();
    });
    expect(document.querySelector(".studio-popover")).toBeNull();
  });

  it("allocates strictly ascending fallback z-index across concurrent popovers on Safari 16", () => {
    mount(
      h(
        "div",
        null,
        h(
          StudioPopover,
          {
            open: true,
            contentClassName: "popover-one",
            trigger: h("button", { type: "button" }, "One"),
          },
          "First",
        ),
        h(
          StudioPopover,
          {
            open: true,
            contentClassName: "popover-two",
            trigger: h("button", { type: "button" }, "Two"),
          },
          "Second",
        ),
      ),
    );
    const first = document.querySelector<HTMLElement>(".popover-one")!;
    const second = document.querySelector<HTMLElement>(".popover-two")!;
    expect(Number(second.style.zIndex)).toBeGreaterThan(Number(first.style.zIndex));
  });
});

describe("ui/Tabs", () => {
  it("supports ARIA tablist/tab/tabpanel semantics, horizontal arrow navigation, loop, disabled skip, and automatic activation", () => {
    const changed: string[] = [];
    const { container } = mount(
      h(
        StudioTabs,
        {
          defaultValue: "general",
          onValueChange: (val) => {
            if (val) changed.push(val);
          },
        },
        h(
          StudioTabsList,
          { "aria-label": "Settings tabs" },
          h(StudioTabsTab, { value: "general" }, "General"),
          h(StudioTabsTab, { value: "disabled", disabled: true }, "Disabled"),
          h(StudioTabsTab, { value: "security" }, "Security"),
          h(StudioTabsTab, { value: "advanced" }, "Advanced"),
        ),
        h(StudioTabsPanel, { value: "general" }, "General panel"),
        h(StudioTabsPanel, { value: "disabled" }, "Disabled panel"),
        h(StudioTabsPanel, { value: "security" }, "Security panel"),
        h(StudioTabsPanel, { value: "advanced" }, "Advanced panel"),
      ),
    );

    const tablist = container.querySelector<HTMLElement>('[role="tablist"]')!;
    expect(tablist.getAttribute("aria-label")).toBe("Settings tabs");

    const tabs = Array.from(container.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    expect(tabs).toHaveLength(4);
    expect(tabs[0]!.getAttribute("aria-selected")).toBe("true");
    expect(tabs[0]!.tabIndex).toBe(0);
    expect(tabs[0]!.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(tabs[2]!.getAttribute("aria-selected")).toBe("false");
    expect(tabs[2]!.tabIndex).toBe(-1);

    const panel = () => container.querySelector<HTMLElement>('[role="tabpanel"]')!;
    expect(panel().textContent).toBe("General panel");
    expect(panel().getAttribute("aria-labelledby")).toBe(tabs[0]!.id);
    expect(tabs[0]!.getAttribute("aria-controls")).toBe(panel().id);

    // ArrowRight moves focus onto disabled tab without selecting it; next ArrowRight activates "security"
    tabs[0]!.focus();
    pressKey(tabs[0]!, "ArrowRight");
    expect(document.activeElement).toBe(tabs[1]);
    expect(tabs[1]!.getAttribute("aria-disabled")).toBe("true");
    expect(tabs[1]!.getAttribute("aria-selected")).toBe("false");
    expect(panel().textContent).toBe("General panel");
    act(() => {
      tabs[1]!.click();
    });
    expect(tabs[1]!.getAttribute("aria-selected")).toBe("false");

    pressKey(tabs[1]!, "ArrowRight");
    expect(document.activeElement).toBe(tabs[2]);
    expect(tabs[2]!.getAttribute("aria-selected")).toBe("true");
    expect(panel().textContent).toBe("Security panel");

    // End moves to last tab; ArrowRight loops back to first
    pressKey(tabs[2]!, "End");
    expect(document.activeElement).toBe(tabs[3]);
    expect(panel().textContent).toBe("Advanced panel");

    pressKey(tabs[3]!, "ArrowRight");
    expect(document.activeElement).toBe(tabs[0]);
    expect(panel().textContent).toBe("General panel");
    expect(changed).toEqual(["security", "advanced", "general"]);
  });

  it("supports manual activation mode (arrow moves focus without switching panel until activated)", () => {
    const { container } = mount(
      h(
        StudioTabs,
        {
          defaultValue: "one",
          activation: "manual",
          orientation: "vertical",
        },
        h(
          StudioTabsList,
          { "aria-label": "Manual tabs" },
          h(StudioTabsTab, { value: "one" }, "One"),
          h(StudioTabsTab, { value: "two" }, "Two"),
        ),
        h(StudioTabsPanel, { value: "one" }, "Panel one"),
        h(StudioTabsPanel, { value: "two" }, "Panel two"),
      ),
    );

    const tablist = container.querySelector<HTMLElement>('[role="tablist"]')!;
    expect(tablist.getAttribute("aria-orientation")).toBe("vertical");

    const tabs = Array.from(container.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
    tabs[0]!.focus();
    pressKey(tabs[0]!, "ArrowDown");

    // Focus moved to tab 2, but selection stayed on tab 1
    expect(document.activeElement).toBe(tabs[1]);
    expect(tabs[0]!.getAttribute("aria-selected")).toBe("true");
    expect(tabs[1]!.getAttribute("aria-selected")).toBe("false");
    expect(container.querySelector('[role="tabpanel"]')?.textContent).toBe("Panel one");

    // Activating tab 2 switches panel
    act(() => {
      tabs[1]!.click();
    });
    expect(tabs[1]!.getAttribute("aria-selected")).toBe("true");
    expect(container.querySelector('[role="tabpanel"]')?.textContent).toBe("Panel two");
  });
});
