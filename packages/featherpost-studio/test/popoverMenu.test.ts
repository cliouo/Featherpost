import { createElement as h, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  attachPopoverDismissal,
  closeAllPopovers,
  computePopoverSide,
  type PopoverDocumentLike,
  type PopoverRootLike,
  type PopoverWindowLike,
} from "../src/app/popover";
import { PopoverMenu } from "../src/app/PopoverMenu";
import { AccountMore } from "../src/settings/Accounts";
import { SettingsController } from "../src/settings/controller";
import { online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";

interface ListenerEntry {
  listener: EventListener;
  capture: boolean;
}

class FakeTarget implements PopoverWindowLike {
  private listeners = new Map<string, ListenerEntry[]>();

  addEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions) {
    const capture = typeof options === "boolean" ? options : !!options?.capture;
    const list = this.listeners.get(type) ?? [];
    list.push({ listener, capture });
    this.listeners.set(type, list);
  }

  removeEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions) {
    const capture = typeof options === "boolean" ? options : !!options?.capture;
    const list = this.listeners.get(type);
    if (!list) return;
    this.listeners.set(
      type,
      list.filter((entry) => !(entry.listener === listener && entry.capture === capture)),
    );
  }

  dispatch(type: string, event: Record<string, unknown> = {}) {
    const payload = {
      type,
      target: null,
      defaultPrevented: false,
      propagationStopped: false,
      preventDefault() {
        this.defaultPrevented = true;
      },
      stopPropagation() {
        this.propagationStopped = true;
      },
      ...event,
    };
    for (const entry of [...(this.listeners.get(type) ?? [])]) {
      entry.listener(payload as unknown as Event);
    }
    return payload;
  }

  listenerCount(type: string) {
    return (this.listeners.get(type) ?? []).length;
  }

  hasCaptureListener(type: string) {
    return (this.listeners.get(type) ?? []).some((entry) => entry.capture);
  }
}

class FakeDocument extends FakeTarget implements PopoverDocumentLike {
  hidden = false;
  modals: PopoverRootLike[] = [];
  querySelectorAll(selector: string) {
    return selector.includes("dialog[open]") ? this.modals : [];
  }
}

function fakeRoot(...inside: unknown[]): PopoverRootLike {
  const members = new Set(inside);
  return { contains: (target) => members.has(target) };
}

function capture(render: () => ReactNode) {
  let root: ReactNode;
  function Probe() {
    root = render();
    return root;
  }
  const html = renderToStaticMarkup(h(Probe));
  const elements: ReactElement<Record<string, unknown>>[] = [];
  const visit = (node: ReactNode) => {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node || typeof node !== "object" || !("props" in node)) return;
    const el = node as ReactElement<Record<string, unknown>>;
    elements.push(el);
    visit(el.props.children as ReactNode);
  };
  visit(root!);
  return { html, elements };
}

afterEach(() => {
  closeAllPopovers();
});

describe("shared popover dismissal", () => {
  it("closes on outside pointerdown/mousedown in capture phase without swallowing the click or stealing focus", () => {
    const doc = new FakeDocument();
    const win = new FakeTarget();
    const trigger = { id: "trigger" };
    const accountIdText = { id: "account-id" };
    const outsideButton = { id: "manage-button" };
    const dismiss = vi.fn();

    const cleanup = attachPopoverDismissal({
      root: fakeRoot(trigger, accountIdText),
      onDismiss: dismiss,
      doc,
      win,
    });
    expect(doc.hasCaptureListener("pointerdown")).toBe(true);
    expect(doc.hasCaptureListener("mousedown")).toBe(true);

    // Clicking trigger or selecting text inside the popover must not dismiss it.
    doc.dispatch("pointerdown", { target: trigger });
    doc.dispatch("pointerdown", { target: accountIdText });
    doc.dispatch("mousedown", { target: accountIdText });
    expect(dismiss).not.toHaveBeenCalled();

    // Clicking outside closes the popover once and lets the underlying click proceed.
    const outside = doc.dispatch("pointerdown", { target: outsideButton });
    doc.dispatch("mousedown", { target: outsideButton });
    expect(dismiss).toHaveBeenCalledTimes(1);
    expect(dismiss).toHaveBeenCalledWith("outside-pointer", false);
    expect(outside.defaultPrevented).toBe(false);
    expect(outside.propagationStopped).toBe(false);

    cleanup();
    expect(doc.listenerCount("pointerdown")).toBe(0);
    expect(doc.listenerCount("keydown")).toBe(0);
    expect(win.listenerCount("blur")).toBe(0);
  });

  it("closes on Escape in capture phase, stops propagation, and requests focus return to the trigger", () => {
    const doc = new FakeDocument();
    const win = new FakeTarget();
    const dismiss = vi.fn();

    const cleanup = attachPopoverDismissal({
      root: fakeRoot(),
      onDismiss: dismiss,
      doc,
      win,
    });

    // Non-Escape keys and IME composition Escape do not close the popover.
    doc.dispatch("keydown", { key: "Enter" });
    doc.dispatch("keydown", { key: "Escape", isComposing: true });
    expect(dismiss).not.toHaveBeenCalled();

    const esc = doc.dispatch("keydown", { key: "Escape" });
    expect(esc.defaultPrevented).toBe(true);
    expect(esc.propagationStopped).toBe(true);
    expect(dismiss).toHaveBeenCalledTimes(1);
    expect(dismiss).toHaveBeenCalledWith("escape", true);

    cleanup();
  });

  it("lets an external modal dialog handle Escape before dismissing a background popover", () => {
    const doc = new FakeDocument();
    const root = fakeRoot();
    doc.modals = [{ contains: () => false }];
    const dismiss = vi.fn();

    const cleanup = attachPopoverDismissal({ root, onDismiss: dismiss, doc, win: new FakeTarget() });
    const esc = doc.dispatch("keydown", { key: "Escape" });
    expect(dismiss).not.toHaveBeenCalled();
    expect(esc.defaultPrevented).toBe(false);
    cleanup();
  });

  it("closes an open popover when a second popover opens so two are never open at once", () => {
    const doc = new FakeDocument();
    const win = new FakeTarget();
    const dismissFirst = vi.fn();
    const dismissSecond = vi.fn();

    const cleanupFirst = attachPopoverDismissal({
      root: fakeRoot({ id: "first" }),
      onDismiss: dismissFirst,
      doc,
      win,
    });
    const cleanupSecond = attachPopoverDismissal({
      root: fakeRoot({ id: "second" }),
      onDismiss: dismissSecond,
      doc,
      win,
    });

    expect(dismissFirst).toHaveBeenCalledTimes(1);
    expect(dismissFirst).toHaveBeenCalledWith("other-popover", false);
    expect(dismissSecond).not.toHaveBeenCalled();

    closeAllPopovers();
    expect(dismissSecond).toHaveBeenCalledTimes(1);
    expect(dismissSecond).toHaveBeenCalledWith("other-popover", false);

    cleanupFirst();
    cleanupSecond();
  });

  it("closes on window blur, pagehide, or hidden visibilitychange (window close / iframe click)", () => {
    for (const trigger of [
      (_doc: FakeDocument, win: FakeTarget) => win.dispatch("blur"),
      (_doc: FakeDocument, win: FakeTarget) => win.dispatch("pagehide"),
      (doc: FakeDocument) => {
        doc.hidden = true;
        doc.dispatch("visibilitychange");
      },
    ]) {
      const doc = new FakeDocument();
      const win = new FakeTarget();
      const dismiss = vi.fn();
      const cleanup = attachPopoverDismissal({ root: fakeRoot(), onDismiss: dismiss, doc, win });
      // Visible visibilitychange is ignored.
      doc.hidden = false;
      doc.dispatch("visibilitychange");
      expect(dismiss).not.toHaveBeenCalled();

      trigger(doc, win);
      expect(dismiss).toHaveBeenCalledTimes(1);
      expect(dismiss.mock.calls[0]?.[1]).toBe(false);
      cleanup();
    }
  });

  it("closes when keyboard focus moves outside, but stays open for focus inside or in a portaled context menu", () => {
    const doc = new FakeDocument();
    const win = new FakeTarget();
    const insideButton = { id: "pause" };
    const contextMenu = {
      closest: (selector: string) => (selector.includes(".studio-menu") ? {} : null),
    };
    const outsideButton = { id: "next-account" };
    const dismiss = vi.fn();

    const cleanup = attachPopoverDismissal({
      root: fakeRoot(insideButton),
      onDismiss: dismiss,
      doc,
      win,
    });

    doc.dispatch("focusin", { target: insideButton });
    doc.dispatch("focusin", { target: contextMenu });
    expect(dismiss).not.toHaveBeenCalled();

    doc.dispatch("focusin", { target: outsideButton });
    expect(dismiss).toHaveBeenCalledTimes(1);
    expect(dismiss).toHaveBeenCalledWith("focus-outside", false);
    cleanup();
  });

  it("keeps the popover below its trigger when space permits and flips above near the bottom edge", () => {
    expect(
      computePopoverSide({
        triggerTop: 120,
        triggerBottom: 148,
        panelHeight: 160,
        viewportTop: 64,
        viewportBottom: 700,
      }),
    ).toBe("bottom");

    expect(
      computePopoverSide({
        triggerTop: 580,
        triggerBottom: 608,
        panelHeight: 160,
        viewportTop: 64,
        viewportBottom: 680,
      }),
    ).toBe("top");

    // When neither side fits completely, keep below if there is still more room below.
    expect(
      computePopoverSide({
        triggerTop: 90,
        triggerBottom: 118,
        panelHeight: 160,
        viewportTop: 64,
        viewportBottom: 220,
      }),
    ).toBe("bottom");
  });
});

describe("PopoverMenu and account more menu integration", () => {
  it("renders a button trigger with menu ARIA semantics and hides the panel until opened", () => {
    const closed = renderToStaticMarkup(
      h(PopoverMenu, {
        className: "account-more",
        triggerLabel: "More actions",
        trigger: "...",
        open: false,
        children: h("button", { type: "button" }, "Pause"),
      }),
    );
    expect(closed).toContain('type="button"');
    expect(closed).toContain('aria-haspopup="menu"');
    expect(closed).toContain('aria-expanded="false"');
    expect(closed).not.toContain("<details");
    expect(closed).not.toContain("Pause");

    const opened = renderToStaticMarkup(
      h(PopoverMenu, {
        className: "account-more",
        triggerLabel: "More actions",
        trigger: "...",
        open: true,
        children: h("button", { type: "button" }, "Pause"),
      }),
    );
    expect(opened).toContain('aria-expanded="true"');
    expect(opened).toContain('aria-controls="');
    expect(opened).toContain('data-side="bottom"');
    expect(opened).toContain('data-tauri-drag-region="false"');
    expect(opened).toContain("Pause");
  });

  it("renders AccountMore via PopoverMenu instead of raw <details>", () => {
    const c = new SettingsController(new FakeSettings());
    c.connection(online);

    const studio = capture(() => AccountMore({ profile: account(), controller: c }));
    expect(studio.html).not.toContain("<details");
    expect(studio.html).toContain('aria-haspopup="menu"');
    expect(studio.html).toContain('aria-expanded="false"');
    expect(studio.elements[0]?.type).toBe(PopoverMenu);

    // Selecting an action from AccountMore triggers confirmation cleanly.
    const remove = studio.elements.find(
      (el) => el.type === "button" && el.props.children === "移除账户",
    );
    expect(remove).toBeDefined();
    (remove!.props.onClick as () => void)();
    expect(c.store.getSnapshot().confirm?.title).toContain("移除");
    c.cancelConfirmation();
    c.dispose();
  });

  it("leaves no floating <details> popovers in Studio sources", () => {
    const sources = import.meta.glob<string>(
      "../src/**/*.tsx",
      { eager: true, query: "?raw", import: "default" },
    );
    const forbidden = Object.entries(sources)
      .filter(([, source]) => /<details[^>]*className="(?:account-more|reader-menu)"/.test(source))
      .map(([path]) => path);
    expect(forbidden).toEqual([]);
  });
});
