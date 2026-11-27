// @vitest-environment jsdom
import { createElement as h, useRef, useState, type ReactNode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { StudioDialog } from "../src/app/Dialog";
import {
  StudioDropdownMenu,
  StudioDropdownMenuItem,
  StudioDropdownMenuMeta,
  StudioDropdownMenuSeparator,
} from "../src/app/DropdownMenu";
import {
  acquireFallbackLayer,
  attachWindowDismissal,
  closeAllPopovers,
  type WindowDismissDocumentLike,
  type WindowDismissWindowLike,
} from "../src/ui";
import { ReaderMoreMenu } from "../src/ReadingPane";
import { AccountMore } from "../src/settings/Accounts";
import { SettingsController } from "../src/settings/controller";
import { isShortcutBlockedTarget } from "../src/Workspace";
import { online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
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
  if (!Element.prototype.hasPointerCapture) {
    Element.prototype.hasPointerCapture = () => false;
  }
  if (!Element.prototype.setPointerCapture) {
    Element.prototype.setPointerCapture = () => {};
  }
  if (!Element.prototype.releasePointerCapture) {
    Element.prototype.releasePointerCapture = () => {};
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];

function mount(node: ReactNode) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  const handle = {
    root,
    container,
    rerender(next: ReactNode) {
      act(() => {
        root.render(next);
      });
    },
    unmount() {
      act(() => {
        root.unmount();
      });
      container.remove();
    },
  };
  mountedRoots.push({ root, container });
  return handle;
}

function flushTimers() {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function pointerOpen(element: HTMLElement) {
  act(() => {
    element.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        button: 0,
        pointerType: "mouse",
      }),
    );
  });
}

function keydown(target: EventTarget, key: string, init: KeyboardEventInit = {}) {
  act(() => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
        ...init,
      }),
    );
  });
}

function openMenu(): HTMLElement | null {
  return document.body.querySelector<HTMLElement>('[role="menu"]:not([hidden])');
}

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  closeAllPopovers();
  document.body.innerHTML = "";
});

class FakeTarget implements WindowDismissWindowLike {
  private listeners = new Map<string, EventListener[]>();

  addEventListener(type: string, listener: EventListener) {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }

  removeEventListener(type: string, listener: EventListener) {
    const list = this.listeners.get(type);
    if (!list) return;
    this.listeners.set(
      type,
      list.filter((entry) => entry !== listener),
    );
  }

  dispatch(type: string) {
    for (const listener of [...(this.listeners.get(type) ?? [])]) {
      listener({ type } as Event);
    }
  }

  listenerCount(type: string) {
    return (this.listeners.get(type) ?? []).length;
  }
}

class FakeDocument extends FakeTarget implements WindowDismissDocumentLike {
  hidden = false;
}

describe("window-lifecycle dismissal helper", () => {
  it("dismisses once on window blur, pagehide, hidden visibilitychange, or closeAllPopovers", () => {
    for (const [expectedReason, trigger] of [
      ["window-blur", (_doc: FakeDocument, win: FakeTarget) => win.dispatch("blur")],
      ["window-hidden", (_doc: FakeDocument, win: FakeTarget) => win.dispatch("pagehide")],
      [
        "window-hidden",
        (doc: FakeDocument) => {
          doc.hidden = true;
          doc.dispatch("visibilitychange");
        },
      ],
      ["external", () => closeAllPopovers()],
    ] as const) {
      const doc = new FakeDocument();
      const win = new FakeTarget();
      const dismiss = vi.fn();
      const cleanup = attachWindowDismissal({ onDismiss: dismiss, doc, win });

      // Visible visibilitychange is ignored.
      doc.hidden = false;
      doc.dispatch("visibilitychange");
      expect(dismiss).not.toHaveBeenCalled();

      trigger(doc, win);
      // Duplicate events do not fire a second time.
      win.dispatch("blur");
      expect(dismiss).toHaveBeenCalledTimes(1);
      expect(dismiss).toHaveBeenCalledWith(expectedReason);

      cleanup();
      expect(win.listenerCount("blur")).toBe(0);
      expect(win.listenerCount("pagehide")).toBe(0);
      expect(doc.listenerCount("visibilitychange")).toBe(0);
    }
  });
});

describe("StudioDropdownMenu (@ui-sdk/react/menu)", () => {
  it("supports keyboard open, ArrowUp/ArrowDown/Home/End roving focus, typeahead, disabled protection, and Escape focus return", async () => {
    const onSelect = vi.fn();
    const { container } = mount(
      h(
        StudioDropdownMenu,
        {
          className: "test-menu",
          trigger: h("button", { type: "button", className: "trigger" }, "Actions"),
        },
        h(StudioDropdownMenuItem, { onSelect: () => onSelect("alpha") }, "Alpha"),
        h(StudioDropdownMenuItem, { disabled: true, onSelect: () => onSelect("disabled") }, "Disabled"),
        h(StudioDropdownMenuItem, { onSelect: () => onSelect("beta") }, "Beta"),
        h(StudioDropdownMenuItem, { onSelect: () => onSelect("gamma") }, "Gamma"),
      ),
    );

    const trigger = container.querySelector<HTMLButtonElement>("button.trigger")!;
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");

    act(() => {
      trigger.focus();
    });
    keydown(trigger, "ArrowDown");
    await flushTimers();

    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    const menu = openMenu()!;
    expect(menu).not.toBeNull();
    expect(menu.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(menu.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(isShortcutBlockedTarget(menu)).toBe(true);

    const items = [...menu.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(["Alpha", "Disabled", "Beta", "Gamma"]);
    expect(document.activeElement).toBe(items[0]);

    // ArrowDown moves to Disabled (focusable per WAI-ARIA APG with aria-disabled="true"),
    // and activating it does not fire onSelect or close the menu.
    keydown(items[0], "ArrowDown");
    await flushTimers();
    expect(document.activeElement).toBe(items[1]);
    expect(items[1].getAttribute("aria-disabled")).toBe("true");
    keydown(items[1], "Enter");
    await flushTimers();
    expect(onSelect).not.toHaveBeenCalled();
    expect(openMenu()).not.toBeNull();

    // ArrowDown continues to Beta.
    keydown(items[1], "ArrowDown");
    await flushTimers();
    expect(document.activeElement).toBe(items[2]);

    // Typeahead jumps directly to Gamma.
    keydown(items[2], "g");
    await flushTimers();
    expect(document.activeElement).toBe(items[3]);

    // End moves to Gamma; ArrowDown loops back to Alpha.
    keydown(items[2], "End");
    await flushTimers();
    expect(document.activeElement).toBe(items[3]);
    keydown(items[3], "ArrowDown");
    await flushTimers();
    expect(document.activeElement).toBe(items[0]);

    // ArrowUp loops to Gamma; Home jumps back to Alpha.
    keydown(items[0], "ArrowUp");
    await flushTimers();
    expect(document.activeElement).toBe(items[3]);
    keydown(items[3], "Home");
    await flushTimers();
    expect(document.activeElement).toBe(items[0]);

    // Escape closes the menu and restores focus to the trigger.
    keydown(items[0], "Escape");
    await flushTimers();
    expect(openMenu()).toBeNull();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger);
  });

  it("dismisses on outside pointerdown and closes the first menu when another opens", async () => {
    const { container } = mount(
      h(
        "div",
        null,
        h("button", { type: "button", className: "outside" }, "Outside"),
        h(
          StudioDropdownMenu,
          {
            trigger: h("button", { type: "button", className: "first-trigger" }, "First"),
          },
          h(StudioDropdownMenuItem, null, "First Item"),
        ),
        h(
          StudioDropdownMenu,
          {
            trigger: h("button", { type: "button", className: "second-trigger" }, "Second"),
          },
          h(StudioDropdownMenuItem, null, "Second Item"),
        ),
      ),
    );

    const outside = container.querySelector<HTMLButtonElement>("button.outside")!;
    const first = container.querySelector<HTMLButtonElement>("button.first-trigger")!;
    const second = container.querySelector<HTMLButtonElement>("button.second-trigger")!;

    pointerOpen(first);
    await flushTimers();
    expect(document.body.querySelectorAll('[role="menu"]:not([hidden])')).toHaveLength(1);
    expect(document.body.textContent).toContain("First Item");

    // Outside click dismisses the open menu.
    act(() => {
      outside.dispatchEvent(
        new PointerEvent("pointerdown", { bubbles: true, cancelable: true, button: 0 }),
      );
    });
    await flushTimers();
    expect(openMenu()).toBeNull();

    // Opening the second menu while the first is open leaves only the second open.
    pointerOpen(first);
    await flushTimers();
    expect(first.getAttribute("aria-expanded")).toBe("true");

    pointerOpen(second);
    await flushTimers();
    expect(first.getAttribute("aria-expanded")).toBe("false");
    expect(second.getAttribute("aria-expanded")).toBe("true");
    const menus = document.body.querySelectorAll('[role="menu"]:not([hidden])');
    expect(menus).toHaveLength(1);
    expect(menus[0].textContent).toContain("Second Item");
  });

  it("dismisses once on window blur (without double-firing), pagehide, hidden visibilitychange, and closeAllPopovers without stealing focus", async () => {
    const onOpenChange = vi.fn();
    const { container } = mount(
      h(
        "div",
        null,
        h("button", { type: "button", className: "other" }, "Other"),
        h(
          StudioDropdownMenu,
          {
            onOpenChange,
            trigger: h("button", { type: "button", className: "trigger" }, "Menu"),
          },
          h(StudioDropdownMenuItem, null, "Item"),
        ),
      ),
    );
    const trigger = container.querySelector<HTMLButtonElement>("button.trigger")!;
    const other = container.querySelector<HTMLButtonElement>("button.other")!;

    for (const fire of [
      () => window.dispatchEvent(new Event("blur")),
      () => window.dispatchEvent(new Event("pagehide")),
      () => {
        Object.defineProperty(document, "hidden", { value: true, configurable: true });
        document.dispatchEvent(new Event("visibilitychange"));
        Object.defineProperty(document, "hidden", { value: false, configurable: true });
      },
      () => closeAllPopovers(),
    ]) {
      onOpenChange.mockClear();
      pointerOpen(trigger);
      await flushTimers();
      expect(openMenu()).not.toBeNull();
      expect(onOpenChange).toHaveBeenCalledWith(true);
      onOpenChange.mockClear();

      act(() => {
        other.focus();
      });
      act(() => {
        fire();
      });
      await flushTimers();
      expect(openMenu()).toBeNull();
      // Exactly one false transition (no duplicate firing between @ui-sdk window-blur and windowDismiss).
      expect(onOpenChange).toHaveBeenCalledTimes(1);
      expect(onOpenChange).toHaveBeenCalledWith(false);
      // Must not steal focus back to trigger (e.g. when user clicked into reading-pane iframe).
      expect(document.activeElement).not.toBe(trigger);
    }
  });

  it("hands focus from a menu item cleanly to an opened Dialog without stealing focus back, and restores trigger focus after Dialog closes", async () => {
    function HandoffHarness() {
      const [dialogOpen, setDialogOpen] = useState(false);
      const cancelRef = useRef<HTMLButtonElement>(null);
      return h(
        "div",
        null,
        h(
          StudioDropdownMenu,
          {
            trigger: h("button", { type: "button", className: "menu-trigger" }, "Account"),
          },
          h(
            StudioDropdownMenuItem,
            { onSelect: () => setDialogOpen(true) },
            "Remove account",
          ),
        ),
        dialogOpen &&
          h(
            StudioDialog,
            {
              title: "Remove account?",
              initialFocusRef: cancelRef,
              onClose: () => setDialogOpen(false),
            },
            h(
              "button",
              {
                ref: cancelRef,
                type: "button",
                className: "dialog-cancel",
                onClick: () => setDialogOpen(false),
              },
              "Cancel",
            ),
          ),
      );
    }

    const { container } = mount(h(HandoffHarness));
    const trigger = container.querySelector<HTMLButtonElement>("button.menu-trigger")!;
    act(() => {
      trigger.focus();
    });
    keydown(trigger, "ArrowDown");
    await flushTimers();

    const item = openMenu()!.querySelector<HTMLElement>('[role="menuitem"]')!;
    act(() => {
      item.click();
    });
    await flushTimers();

    expect(openMenu()).toBeNull();
    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(dialog).not.toBeNull();
    const cancelBtn = dialog.querySelector<HTMLButtonElement>("button.dialog-cancel")!;
    // Menu close must NOT steal focus back to the trigger; focus stays on the Dialog's initialFocus target.
    expect(document.activeElement).toBe(cancelBtn);

    // Closing the Dialog via Escape restores focus to the original Menu trigger.
    keydown(dialog, "Escape");
    await flushTimers();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("allocates monotonic Safari 16 fallback inline zIndex when supportsTopLayer() is false", async () => {
    const baseLayer = acquireFallbackLayer();
    try {
      const { container } = mount(
        h(
          StudioDropdownMenu,
          {
            trigger: h("button", { type: "button", className: "z-trigger" }, "Layer"),
          },
          h(StudioDropdownMenuItem, null, "Item"),
        ),
      );
      pointerOpen(container.querySelector<HTMLButtonElement>("button.z-trigger")!);
      await flushTimers();

      const menu = openMenu()!;
      expect(menu).not.toBeNull();
      const z = Number(menu.style.zIndex);
      expect(Number.isFinite(z)).toBe(true);
      expect(z).toBeGreaterThan(baseLayer.content);
      expect(z).toBeLessThan(2000);
    } finally {
      baseLayer.release();
    }
  });

  it("closes immediately when forceClosed becomes true and refuses reopening while locked", async () => {
    const renderMenu = (forceClosed: boolean) =>
      h(
        StudioDropdownMenu,
        {
          forceClosed,
          trigger: h("button", { type: "button", className: "trigger" }, "Locked"),
        },
        h(StudioDropdownMenuItem, null, "Action"),
      );

    const view = mount(renderMenu(false));
    const trigger = view.container.querySelector<HTMLButtonElement>("button.trigger")!;
    pointerOpen(trigger);
    await flushTimers();
    expect(openMenu()).not.toBeNull();

    view.rerender(renderMenu(true));
    await flushTimers();
    expect(openMenu()).toBeNull();

    pointerOpen(trigger);
    await flushTimers();
    expect(openMenu()).toBeNull();
  });
});

describe("ReaderMoreMenu and AccountMore integration", () => {
  it("renders ReaderMoreMenu over @ui-sdk Menu with keyboard navigation and action callbacks", async () => {
    const onToggleSeen = vi.fn();
    const onOpenThread = vi.fn();
    const { container } = mount(
      h(ReaderMoreMenu, {
        seen: true,
        threadOpen: false,
        canToggleSeen: true,
        canOpenThread: true,
        onToggleSeen,
        onOpenThread,
      }),
    );

    const trigger = container.querySelector<HTMLButtonElement>("button.icon-button")!;
    expect(trigger.getAttribute("title")).toBeNull();
    expect(trigger.getAttribute("aria-label")).toBeTruthy();
    const tooltipId = trigger.getAttribute("aria-labelledby");
    expect(tooltipId).toBeTruthy();
    expect(document.getElementById(tooltipId!)?.getAttribute("role")).toBe("tooltip");
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");

    act(() => {
      trigger.focus();
    });
    keydown(trigger, "ArrowDown");
    await flushTimers();

    const items = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items).toHaveLength(2);
    expect(items[0].textContent).toBe("标为未读");
    expect(items[1].textContent).toBe("查看会话");

    // Navigate with ArrowDown to second item and select it.
    keydown(items[0], "ArrowDown");
    await flushTimers();
    expect(document.activeElement).toBe(items[1]);
    act(() => {
      items[1].click();
    });
    await flushTimers();
    expect(onOpenThread).toHaveBeenCalledTimes(1);
    expect(onToggleSeen).not.toHaveBeenCalled();
    expect(openMenu()).toBeNull();
  });

  it("renders AccountMore with danger styling, confirmation trigger, and selectable non-menu metadata", async () => {
    const c = new SettingsController(new FakeSettings());
    c.connection(online);

    const { container } = mount(h(AccountMore, { profile: account(), controller: c }));
    const trigger = container.querySelector<HTMLButtonElement>(".account-more > button")!;
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(container.innerHTML).not.toContain("<details");

    pointerOpen(trigger);
    await flushTimers();

    const menu = document.body.querySelector<HTMLElement>(".account-more-panel")!;
    expect(menu).not.toBeNull();
    const items = [...menu.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(["暂停同步", "移除账户"]);
    expect(items[1].getAttribute("data-tone")).toBe("danger");

    // Non-menu metadata text is inside .studio-dropdown-meta and clicking it does not dismiss the menu.
    const meta = menu.querySelector<HTMLElement>(".studio-dropdown-meta")!;
    expect(meta).not.toBeNull();
    expect(meta.textContent).toContain("acct-a");
    act(() => {
      meta.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, button: 0 }));
      meta.click();
    });
    await flushTimers();
    expect(document.body.querySelector(".account-more-panel")).not.toBeNull();

    // Selecting Remove triggers account removal confirmation cleanly.
    act(() => {
      items[1].click();
    });
    await flushTimers();
    expect(c.store.getSnapshot().confirm?.title).toContain("移除");
    act(() => {
      c.cancelConfirmation();
    });
    c.dispose();
  });

  it("keeps StudioDropdownMenuSeparator and StudioDropdownMenuMeta composable without errors", async () => {
    const { container } = mount(
      h(
        StudioDropdownMenu,
        { trigger: h("button", { type: "button", className: "t" }, "Open") },
        h(StudioDropdownMenuItem, null, "One"),
        h(StudioDropdownMenuSeparator),
        h(StudioDropdownMenuMeta, null, h("small", null, "meta-id")),
      ),
    );
    pointerOpen(container.querySelector<HTMLButtonElement>("button.t")!);
    await flushTimers();
    expect(document.body.querySelector('[role="separator"]')).not.toBeNull();
    expect(document.body.querySelector(".studio-dropdown-meta")?.textContent).toBe("meta-id");
  });

  it("leaves no floating <details> popovers or retired PopoverMenu usages in Studio sources", () => {
    const sources = import.meta.glob<string>("../src/**/*.tsx", {
      eager: true,
      query: "?raw",
      import: "default",
    });
    const forbiddenDetails = Object.entries(sources)
      .filter(([, source]) => /<details[^>]*className="(?:account-more|reader-menu)"/.test(source))
      .map(([path]) => path);
    const legacyPopoverMenu = Object.entries(sources)
      .filter(([, source]) => /\bPopoverMenu\b/.test(source))
      .map(([path]) => path);
    expect(forbiddenDetails).toEqual([]);
    expect(legacyPopoverMenu).toEqual([]);
  });
});
