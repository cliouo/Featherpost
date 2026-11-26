// @vitest-environment jsdom
import { act, createElement as h, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import {
  closeAllPopovers,
  StudioCommandDialog,
  type StudioCommandAnnouncement,
  type StudioCommandGroup,
} from "../src/ui";

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

function flushTimers(ms = 0) {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

function flushPaint() {
  return act(async () => {
    await new Promise((resolve) =>
      requestAnimationFrame(() => setTimeout(resolve, 0)),
    );
  });
}

function dispatchKey(
  target: EventTarget,
  key: string,
  init: KeyboardEventInit & { keyCode?: number } = {},
) {
  const { keyCode, ...rest } = init;
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...rest,
  });
  if (keyCode !== undefined) {
    Object.defineProperty(event, "keyCode", { value: keyCode });
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
  vi.useRealTimers();
});

describe("StudioCommandDialog primitive (src/ui/Command.tsx)", () => {
  it("protects Chinese IME via isComposing, Safari keyCode 229, and capture-phase composingRef fallback without calling preventDefault", async () => {
    const onRun = vi.fn();
    const onClose = vi.fn();

    const groups: StudioCommandGroup[] = [
      {
        id: "actions",
        label: "操作",
        items: [
          {
            id: "compose",
            label: "写邮件",
            keywords: ["compose", "new"],
            onSelect: onRun,
          },
        ],
      },
    ];

    function Harness() {
      const [open, setOpen] = useState(true);
      return h(StudioCommandDialog, {
        open,
        onOpenChange: (next) => {
          setOpen(next);
          if (!next) onClose();
        },
        title: "命令面板",
        inputLabel: "搜索命令",
        groups,
        emptyTitle: "无匹配",
      });
    }

    mount(h(Harness));
    await flushPaint();

    const input = document.body.querySelector<HTMLInputElement>(
      "input.command-palette-input",
    )!;
    expect(input).not.toBeNull();
    input.focus();

    // 1. Standard isComposing: true on Enter & Escape -> ignored, NOT defaultPrevented.
    const enterComposing = dispatchKey(input, "Enter", { isComposing: true });
    expect(onRun).not.toHaveBeenCalled();
    expect(enterComposing.defaultPrevented).toBe(false);

    const escComposing = dispatchKey(input, "Escape", { isComposing: true });
    expect(onClose).not.toHaveBeenCalled();
    expect(escComposing.defaultPrevented).toBe(false);

    // 2. Safari WebKit keyCode === 229 on Enter & Escape -> ignored, NOT defaultPrevented.
    const enter229 = dispatchKey(input, "Enter", { keyCode: 229 });
    expect(onRun).not.toHaveBeenCalled();
    expect(enter229.defaultPrevented).toBe(false);

    const esc229 = dispatchKey(input, "Escape", { keyCode: 229 });
    expect(onClose).not.toHaveBeenCalled();
    expect(esc229.defaultPrevented).toBe(false);

    // 3. Fallback composingRef when BOTH isComposing and keyCode === 229 are missing during composition.
    act(() => {
      input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    });
    const enterFallback = dispatchKey(input, "Enter");
    expect(onRun).not.toHaveBeenCalled();
    expect(enterFallback.defaultPrevented).toBe(false);

    const escFallback = dispatchKey(input, "Escape");
    expect(onClose).not.toHaveBeenCalled();
    expect(escFallback.defaultPrevented).toBe(false);

    // End composition -> normal Enter now executes highlighted command.
    act(() => {
      input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
    });
    const plainEnter = dispatchKey(input, "Enter");
    expect(plainEnter.defaultPrevented).toBe(true);
    expect(onRun).toHaveBeenCalledTimes(1);
  });

  it("blocks modified Enter (Meta/Ctrl/Alt/Shift+Enter) and repeated Enter from executing highlighted items", async () => {
    const onRun = vi.fn();
    const groups: StudioCommandGroup[] = [
      {
        id: "actions",
        label: "Actions",
        items: [
          {
            id: "sync",
            label: "Sync now",
            onSelect: onRun,
          },
        ],
      },
    ];

    mount(
      h(StudioCommandDialog, {
        open: true,
        onOpenChange: () => {},
        title: "Command palette",
        inputLabel: "Search commands",
        groups,
        emptyTitle: "No results",
      }),
    );
    await flushPaint();

    const input = document.body.querySelector<HTMLInputElement>(
      "input.command-palette-input",
    )!;
    input.focus();

    for (const init of [
      { metaKey: true },
      { ctrlKey: true },
      { altKey: true },
      { shiftKey: true },
      { repeat: true },
    ]) {
      const event = dispatchKey(input, "Enter", init);
      expect(event.defaultPrevented).toBe(true);
      expect(onRun).not.toHaveBeenCalled();
    }

    // Plain Enter executes once.
    dispatchKey(input, "Enter");
    expect(onRun).toHaveBeenCalledTimes(1);
  });

  it("supports two-step Escape inside input and single-step Escape when focus is outside input", async () => {
    const onEmptyEnter = vi.fn();
    const groups: StudioCommandGroup[] = [
      {
        id: "actions",
        label: "Actions",
        items: [{ id: "sync", label: "Sync now", onSelect: () => {} }],
      },
    ];

    function Harness() {
      const [open, setOpen] = useState(true);
      return h(
        "div",
        null,
        h(
          "button",
          { type: "button", className: "reopen", onClick: () => setOpen(true) },
          "Reopen",
        ),
        h(StudioCommandDialog, {
          open,
          onOpenChange: setOpen,
          title: "Command palette",
          inputLabel: "Search commands",
          groups,
          emptyTitle: "No results",
          onEmptyEnter,
          footer: ({ query }) =>
            query.trim()
              ? h(
                  "button",
                  { type: "button", className: "footer-fallback" },
                  `Search mail: ${query}`,
                )
              : null,
        }),
      );
    }

    const { container } = mount(h(Harness));
    await flushPaint();

    let input = document.body.querySelector<HTMLInputElement>(
      "input.command-palette-input",
    )!;
    input.focus();
    setInputValue(input, "sync");
    expect(input.value).toBe("sync");

    // Step 1: Escape clears non-empty input and keeps dialog open (event consumed).
    const firstEsc = dispatchKey(input, "Escape");
    expect(firstEsc.defaultPrevented).toBe(true);
    expect(input.value).toBe("");
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();

    // Step 2: Escape on empty input closes the dialog (event consumed so Workspace selection stays intact).
    const secondEsc = dispatchKey(input, "Escape");
    await flushTimers();
    expect(secondEsc.defaultPrevented).toBe(true);
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();

    // Reopen, type query, Tab/focus onto footer fallback button -> single Escape closes immediately.
    act(() => {
      container.querySelector<HTMLButtonElement>("button.reopen")!.click();
    });
    await flushPaint();
    input = document.body.querySelector<HTMLInputElement>(
      "input.command-palette-input",
    )!;
    setInputValue(input, "invoice");
    const fallbackBtn = document.body.querySelector<HTMLButtonElement>(
      "button.footer-fallback",
    )!;
    expect(fallbackBtn).not.toBeNull();
    act(() => {
      fallbackBtn.focus();
    });
    const outsideInputEsc = dispatchKey(fallbackBtn, "Escape");
    await flushTimers();
    expect(outsideInputEsc.defaultPrevented).toBe(true);
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("allocates Safari 16 paired fallback z-index, exempts .toast from inert, keeps hidden title semantic, never hijacks global shortcuts, and uses localized announcements", async () => {
    const onRunShortcutItem = vi.fn();
    const announcements: string[] = [];

    const groups: StudioCommandGroup[] = [
      {
        id: "actions",
        label: "操作",
        items: [
          {
            id: "compose",
            label: "写邮件",
            shortcut: "mod+n",
            onSelect: onRunShortcutItem,
          },
        ],
      },
    ];

    mount(
      h(
        "div",
        { className: "app-root" },
        h("button", { type: "button", className: "bg-btn" }, "Background"),
        h("div", { className: "toast", role: "status" }, "已同步"),
        h(StudioCommandDialog, {
          open: true,
          onOpenChange: () => {},
          title: "命令面板",
          inputLabel: "搜索命令、文件夹或账户",
          groups,
          emptyTitle: "没有匹配的命令",
          announcement: (change: StudioCommandAnnouncement) => {
            const text =
              change.type === "results"
                ? change.count === 0
                  ? "没有匹配的命令"
                  : `找到 ${change.count} 项命令`
                : `已移除 ${change.label}`;
            announcements.push(text);
            return text;
          },
          pendingAnnouncement: "正在执行…",
        }),
      ),
    );
    await flushPaint();

    // Safari 16 fallback z-index (jsdom has no showPopover/top-layer): paired backdrop < content.
    const backdrop = document.body.querySelector<HTMLElement>(".command-palette-overlay")!;
    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"].command-palette-dialog')!;
    expect(backdrop).not.toBeNull();
    expect(dialog).not.toBeNull();
    const backdropZ = Number(backdrop.style.zIndex);
    const dialogZ = Number(dialog.style.zIndex);
    expect(backdropZ).toBeGreaterThan(0);
    expect(dialogZ).toBeGreaterThan(backdropZ);

    // Visually hidden dialog title preserves ARIA accessible name.
    const titleId = dialog.getAttribute("aria-labelledby");
    expect(titleId).toBeTruthy();
    const titleEl = document.getElementById(titleId!);
    expect(titleEl?.classList.contains("visually-hidden")).toBe(true);
    expect(titleEl?.textContent).toBe("命令面板");

    // Input has accessible name.
    const input = document.body.querySelector<HTMLInputElement>(
      "input.command-palette-input",
    )!;
    expect(input.getAttribute("aria-label")).toBe("搜索命令、文件夹或账户");

    // .toast is exempt from inert while background shell is inert.
    const bgBtn = document.body.querySelector<HTMLElement>(".bg-btn")!;
    const toast = document.body.querySelector<HTMLElement>(".toast")!;
    expect(bgBtn.closest("[inert]")).not.toBeNull();
    expect(toast.closest("[inert]")).toBeNull();

    // Shortcut is displayed via CommandShortcut ONLY and NEVER bound as a hotkey.
    const shortcutEl = dialog.querySelector(".command-palette-shortcut");
    expect(shortcutEl?.textContent?.length).toBeGreaterThan(0);
    dispatchKey(document, "n", { metaKey: true });
    dispatchKey(document, "n", { ctrlKey: true });
    expect(onRunShortcutItem).not.toHaveBeenCalled();

    // CommandEmpty sits outside the listbox.
    const listbox = dialog.querySelector('[role="listbox"]')!;
    const emptyEl = dialog.querySelector(".command-palette-empty")!;
    expect(listbox.contains(emptyEl)).toBe(false);

    // Localized screen-reader announcement after debounce (500ms in @ui-sdk).
    setInputValue(input, "zzzz-no-hit");
    await flushTimers(600);
    expect(announcements).toContain("没有匹配的命令");
    expect(announcements.some((text) => /No results|results/i.test(text))).toBe(false);
  });
});
