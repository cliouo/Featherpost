// @vitest-environment jsdom
import {
  act,
  createElement as h,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { isGlobalShortcutBlocked, STUDIO_SHORTCUT_KEYS } from "../src/shortcuts";
import {
  acquireFallbackLayer,
  closeAllPopovers,
  StudioDialog,
  StudioDropdownMenu,
  StudioDropdownMenuItem,
  StudioPopover,
  StudioTooltip,
  StudioTooltipProvider,
  Z_INDEX_TOKENS,
  type StudioTooltipProps,
} from "../src/ui";

function tip(
  props: Omit<StudioTooltipProps, "children">,
  child: StudioTooltipProps["children"],
) {
  return h(StudioTooltip, { ...props, children: child });
}

(
  globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

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
    globalThis.PointerEvent =
      PolyfillPointerEvent as unknown as typeof PointerEvent;
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

function visibleTooltips(): HTMLElement[] {
  return Array.from(
    document.body.querySelectorAll<HTMLElement>(
      '[role="tooltip"]:not([hidden])',
    ),
  );
}

function pointerEnter(el: Element, pointerType = "mouse") {
  act(() => {
    el.dispatchEvent(
      new PointerEvent("pointerenter", {
        bubbles: false,
        pointerType,
      }),
    );
  });
}

function pointerLeave(el: Element, pointerType = "mouse") {
  act(() => {
    el.dispatchEvent(
      new PointerEvent("pointerleave", {
        bubbles: false,
        pointerType,
        clientX: 500,
        clientY: 500,
      }),
    );
  });
}

function pointerDown(el: Element, pointerType = "mouse") {
  act(() => {
    el.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        button: 0,
        pointerType,
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

let fakeNow = 10_000;
let perfNowSpy: ReturnType<typeof vi.spyOn> | null = null;

function useMonotonicFakeTimers() {
  fakeNow += 60_000;
  vi.useFakeTimers({
    toFake: [
      "setTimeout",
      "clearTimeout",
      "setInterval",
      "clearInterval",
      "Date",
      "requestAnimationFrame",
      "cancelAnimationFrame",
    ],
  });
  perfNowSpy?.mockRestore();
  perfNowSpy = vi.spyOn(performance, "now").mockImplementation(() => fakeNow);
}

function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(0);
  });
  fakeNow += ms;
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

afterEach(() => {
  perfNowSpy?.mockRestore();
  perfNowSpy = null;
  vi.useRealTimers();
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

describe("StudioTooltip & StudioTooltipProvider (@ui-sdk/react/tooltip)", () => {
  it("enforces 450ms cold-hover delay, 0ms warm-up data-instant handoff, and >400ms cooldown", () => {
    useMonotonicFakeTimers();
    const { container } = mount(
      h(
        StudioTooltipProvider,
        null,
        tip(
          { label: "First action", roleType: "label" },
          h("button", { type: "button", className: "btn-a" }, "A"),
        ),
        tip(
          { label: "Second action", roleType: "label" },
          h("button", { type: "button", className: "btn-b" }, "B"),
        ),
      ),
    );

    const btnA = container.querySelector<HTMLButtonElement>("button.btn-a")!;
    const btnB = container.querySelector<HTMLButtonElement>("button.btn-b")!;

    // Cold hover: must stay closed before 450ms and open at 450ms.
    pointerEnter(btnA);
    advance(400);
    expect(visibleTooltips()).toHaveLength(0);

    advance(50);
    let tips = visibleTooltips();
    expect(tips).toHaveLength(1);
    expect(tips[0]!.textContent).toBe("First action");
    expect(tips[0]!.hasAttribute("data-instant")).toBe(false);

    // Move quickly to btnB within warm window -> opens immediately (0ms) with data-instant.
    pointerLeave(btnA);
    advance(50);
    expect(visibleTooltips()).toHaveLength(0);

    pointerEnter(btnB);
    tips = visibleTooltips();
    expect(tips).toHaveLength(1);
    expect(tips[0]!.textContent).toBe("Second action");
    expect(tips[0]!.hasAttribute("data-instant")).toBe(true);

    // Leave and wait > 400ms cooldown -> next hover requires full 450ms delay again.
    pointerLeave(btnB);
    advance(450);
    expect(visibleTooltips()).toHaveLength(0);

    pointerEnter(btnA);
    advance(400);
    expect(visibleTooltips()).toHaveLength(0);
    advance(50);
    tips = visibleTooltips();
    expect(tips).toHaveLength(1);
    expect(tips[0]!.textContent).toBe("First action");
    expect(tips[0]!.hasAttribute("data-instant")).toBe(false);
  });

  it("cancels/invalidates in-flight cold-hover timers on window.blur, visibilitychange(hidden), pagehide, and closeAllPopovers", () => {
    useMonotonicFakeTimers();
    const { container } = mount(
      h(
        StudioTooltipProvider,
        null,
        tip(
          { label: "Sync now" },
          h("button", { type: "button", className: "sync-btn" }, "Sync"),
        ),
      ),
    );
    const btn = container.querySelector<HTMLButtonElement>("button.sync-btn")!;

    const dismissers: Array<[string, () => void]> = [
      ["window.blur", () => window.dispatchEvent(new Event("blur"))],
      [
        "visibilitychange(hidden)",
        () => {
          Object.defineProperty(document, "hidden", {
            value: true,
            configurable: true,
          });
          document.dispatchEvent(new Event("visibilitychange"));
          Object.defineProperty(document, "hidden", {
            value: false,
            configurable: true,
          });
        },
      ],
      ["window.pagehide", () => window.dispatchEvent(new Event("pagehide"))],
      ["closeAllPopovers", () => closeAllPopovers()],
    ];

    for (const [name, triggerDismiss] of dismissers) {
      // Wait out any cooldown so every iteration tests a true 450ms cold hover.
      advance(500);

      pointerEnter(btn);
      advance(200);
      expect(visibleTooltips(), `${name}: before dismiss`).toHaveLength(0);

      act(() => {
        triggerDismiss();
      });

      // Even after the original 450ms elapses, the invalidated timer MUST NOT open!
      advance(500);
      expect(
        visibleTooltips(),
        `${name}: in-flight cold-hover timer leaked open after dismiss`,
      ).toHaveLength(0);

      // A genuine fresh pointerenter after returning to the window still opens normally.
      pointerLeave(btn);
      pointerEnter(btn);
      advance(450);
      expect(
        visibleTooltips(),
        `${name}: fresh hover after dismiss should open`,
      ).toHaveLength(1);

      // Dismissal also closes an already-open tooltip immediately.
      act(() => {
        triggerDismiss();
      });
      advance(50);
      expect(
        visibleTooltips(),
        `${name}: already-open tooltip should close`,
      ).toHaveLength(0);
      pointerLeave(btn);
    }
  });

  it("opens on keyboard focus, does not open on mouse-click focus, and Escape closes Tooltip only while keeping Dialog/Popover open and focus on trigger", async () => {
    useMonotonicFakeTimers();

    function DialogAndPopoverHarness() {
      const [dialogOpen, setDialogOpen] = useState(true);
      const [popoverOpen, setPopoverOpen] = useState(false);
      const triggerRef = useRef<HTMLButtonElement>(null);
      return h(
        StudioTooltipProvider,
        null,
        dialogOpen &&
          h(
            StudioDialog,
            {
              title: "Group manager",
              initialFocusRef: triggerRef,
              onClose: () => setDialogOpen(false),
            },
            tip(
              {
                label: "Delete group explanation",
                roleType: "description",
              },
              h(
                "button",
                {
                  ref: triggerRef,
                  type: "button",
                  className: "dialog-action",
                },
                "Delete",
              ),
            ),
          ),
        h(
          StudioPopover,
          {
            open: popoverOpen,
            onOpenChange: setPopoverOpen,
            title: "Filter panel",
            trigger: h(
              "button",
              { type: "button", className: "open-popover-btn" },
              "Filters",
            ),
          },
          tip(
            {
              label: "Close filters",
              roleType: "label",
            },
            h(
              "button",
              {
                type: "button",
                className: "popover-inner-btn",
                "aria-label": "Close filters",
              },
              "×",
            ),
          ),
        ),
        tip(
          { label: "Mouse target" },
          h("button", { type: "button", className: "mouse-btn" }, "Click me"),
        ),
      );
    }

    const { container } = mount(h(DialogAndPopoverHarness));
    // Flush microtasks so `@ui-sdk` modality tracking is installed even in isolated test runs.
    await act(async () => {});
    advance(50);

    const dialogAction = document.body.querySelector<HTMLButtonElement>(
      "button.dialog-action",
    )!;
    expect(dialogAction).not.toBeNull();

    // Keyboard modality + focus opens tooltip immediately inside Dialog.
    keydown(document.body, "Tab");
    act(() => {
      dialogAction.blur();
      dialogAction.focus();
    });
    advance(20);
    expect(visibleTooltips()).toHaveLength(1);
    expect(visibleTooltips()[0]!.textContent).toBe("Delete group explanation");

    // Escape closes ONLY the Tooltip — Dialog stays open and focus stays on dialogAction!
    keydown(dialogAction, "Escape");
    advance(20);
    expect(visibleTooltips()).toHaveLength(0);
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
    expect(document.activeElement).toBe(dialogAction);

    // Second Escape closes the Dialog itself.
    keydown(dialogAction, "Escape");
    advance(50);
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();

    // Open Popover, open Tooltip on inner button via keyboard focus -> Escape closes Tooltip ONLY!
    const openPopoverBtn = container.querySelector<HTMLButtonElement>(
      "button.open-popover-btn",
    )!;
    act(() => {
      openPopoverBtn.click();
    });
    await act(async () => {});
    advance(50);
    const popoverInnerBtn = document.body.querySelector<HTMLButtonElement>(
      "button.popover-inner-btn",
    )!;
    expect(popoverInnerBtn).not.toBeNull();

    keydown(document.body, "Tab");
    act(() => {
      popoverInnerBtn.blur();
      popoverInnerBtn.focus();
    });
    advance(20);
    expect(visibleTooltips()).toHaveLength(1);
    expect(visibleTooltips()[0]!.textContent).toBe("Close filters");

    keydown(popoverInnerBtn, "Escape");
    advance(20);
    expect(visibleTooltips()).toHaveLength(0);
    expect(
      document.body.querySelector("button.popover-inner-btn"),
    ).not.toBeNull();
    expect(document.activeElement).toBe(popoverInnerBtn);

    // Close Popover, then verify mouse-click focus does NOT open tooltip.
    keydown(popoverInnerBtn, "Escape");
    advance(50);
    expect(document.body.querySelector("button.popover-inner-btn")).toBeNull();

    const mouseBtn = container.querySelector<HTMLButtonElement>(
      "button.mouse-btn",
    )!;
    pointerDown(mouseBtn);
    act(() => {
      mouseBtn.focus();
    });
    advance(500);
    expect(visibleTooltips()).toHaveLength(0);
  });

  it("composes dual-triggers (Menu & Popover) with hover->pointerdown, ArrowDown/Enter, programmatic open/close, stable DOM anchors, and non-overlay aria-expanded support", () => {
    useMonotonicFakeTimers();

    function DualTriggerHarness() {
      const [popoverOpen, setPopoverOpen] = useState(false);
      const [folderExpanded, setFolderExpanded] = useState(true);
      return h(
        StudioTooltipProvider,
        null,
        h(
          StudioDropdownMenu,
          {
            className: "reader-menu",
            trigger: tip(
              { label: "More actions", roleType: "label" },
              h(
                "button",
                {
                  type: "button",
                  className: "menu-trigger-btn",
                  "aria-label": "More actions",
                },
                "⋯",
              ),
            ),
          },
          h(StudioDropdownMenuItem, null, "Mark unread"),
          h(StudioDropdownMenuItem, null, "View thread"),
        ),
        h(
          StudioPopover,
          {
            open: popoverOpen,
            onOpenChange: setPopoverOpen,
            title: "Advanced filters",
            trigger: tip(
              {
                label: "Filter messages",
                roleType: "label",
                suppress: popoverOpen,
              },
              h(
                "button",
                {
                  type: "button",
                  className: "popover-trigger-btn",
                  "aria-label": "Filter messages",
                },
                "Filters",
              ),
            ),
          },
          h("div", { className: "popover-body" }, "Fields"),
        ),
        h(
          "button",
          {
            type: "button",
            className: "toggle-popover-programmatic",
            onClick: () => setPopoverOpen((prev) => !prev),
          },
          "Toggle",
        ),
        tip(
          {
            label: folderExpanded ? "Collapse Inbox" : "Expand Inbox",
            roleType: "label",
          },
          h(
            "button",
            {
              type: "button",
              className: "folder-disclosure-btn",
              "aria-expanded": folderExpanded,
              "aria-label": folderExpanded ? "Collapse Inbox" : "Expand Inbox",
              onClick: () => setFolderExpanded((prev) => !prev),
            },
            "⌄",
          ),
        ),
      );
    }

    const { container } = mount(h(DualTriggerHarness));
    const menuTrigger = container.querySelector<HTMLButtonElement>(
      "button.menu-trigger-btn",
    )!;
    const popoverTrigger = container.querySelector<HTMLButtonElement>(
      "button.popover-trigger-btn",
    )!;
    const toggleProg = container.querySelector<HTMLButtonElement>(
      "button.toggle-popover-programmatic",
    )!;
    const folderBtn = container.querySelector<HTMLButtonElement>(
      "button.folder-disclosure-btn",
    )!;

    expect(menuTrigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(popoverTrigger.getAttribute("aria-haspopup")).toBe("dialog");

    // 1. Hover -> pointerdown on Menu trigger closes Tooltip, opens Menu, preserves exact DOM anchor.
    pointerEnter(menuTrigger);
    advance(450);
    expect(visibleTooltips()).toHaveLength(1);

    pointerDown(menuTrigger);
    advance(50);
    expect(visibleTooltips()).toHaveLength(0);
    expect(
      document.body.querySelector('[role="menu"]:not([hidden])'),
    ).not.toBeNull();
    expect(
      container.querySelector<HTMLButtonElement>("button.menu-trigger-btn"),
    ).toBe(menuTrigger);

    // While menu is open, re-hovering menuTrigger must NOT show tooltip.
    pointerLeave(menuTrigger);
    pointerEnter(menuTrigger);
    advance(500);
    expect(visibleTooltips()).toHaveLength(0);

    // Escape closes menu and restores focus to the same trigger DOM node.
    keydown(document.activeElement ?? document.body, "Escape");
    advance(50);
    expect(
      document.body.querySelector('[role="menu"]:not([hidden])'),
    ).toBeNull();
    expect(document.activeElement).toBe(menuTrigger);

    // 2. ArrowDown opens Menu via keyboard and suppresses Tooltip.
    keydown(menuTrigger, "ArrowDown");
    advance(50);
    expect(
      document.body.querySelector('[role="menu"]:not([hidden])'),
    ).not.toBeNull();
    expect(visibleTooltips()).toHaveLength(0);
    keydown(document.activeElement ?? document.body, "Escape");
    advance(50);

    // 3. Programmatic Popover open/close preserves exact DOM anchor & suppresses while open.
    act(() => {
      toggleProg.click();
    });
    advance(50);
    expect(document.body.querySelector(".popover-body")).not.toBeNull();
    expect(
      container.querySelector<HTMLButtonElement>("button.popover-trigger-btn"),
    ).toBe(popoverTrigger);

    pointerEnter(popoverTrigger);
    advance(500);
    expect(visibleTooltips()).toHaveLength(0);

    act(() => {
      toggleProg.click();
    });
    advance(50);
    expect(document.body.querySelector(".popover-body")).toBeNull();
    expect(
      container.querySelector<HTMLButtonElement>("button.popover-trigger-btn"),
    ).toBe(popoverTrigger);

    // 4. Non-overlay `aria-expanded="true"` (folder disclosure) STILL shows its tooltip!
    expect(folderBtn.getAttribute("aria-expanded")).toBe("true");
    pointerEnter(folderBtn);
    advance(450);
    expect(visibleTooltips()).toHaveLength(1);
    expect(visibleTooltips()[0]!.textContent).toBe("Collapse Inbox");
  });

  it("short-circuits natively disabled triggers without dev warnings while preserving outer refs, props, and DOM identity across disabled->enabled", () => {
    useMonotonicFakeTimers();
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    let capturedRef: HTMLButtonElement | null = null;
    function DisabledTransitionHarness() {
      const [disabled, setDisabled] = useState(true);
      return h(
        StudioTooltipProvider,
        null,
        tip(
          { label: "Archive" },
          h(
            "button",
            {
              ref: (el: HTMLButtonElement | null) => {
                capturedRef = el;
              },
              type: "button",
              className: "archive-btn",
              disabled,
              "data-custom": "keep",
            },
            "Archive",
          ),
        ),
        h(
          "button",
          {
            type: "button",
            className: "enable-btn",
            onClick: () => setDisabled(false),
          },
          "Enable",
        ),
      );
    }

    try {
      const { container } = mount(h(DisabledTransitionHarness));
      const btnBefore = container.querySelector<HTMLButtonElement>(
        "button.archive-btn",
      )!;
      expect(btnBefore.disabled).toBe(true);
      expect(btnBefore.getAttribute("data-custom")).toBe("keep");
      expect(capturedRef).toBe(btnBefore);
      expect(warnSpy).not.toHaveBeenCalled();

      // Transition disabled -> enabled MUST keep the exact same DOM node & ref.
      act(() => {
        container
          .querySelector<HTMLButtonElement>("button.enable-btn")!
          .click();
      });
      const btnAfter = container.querySelector<HTMLButtonElement>(
        "button.archive-btn",
      )!;
      expect(btnAfter).toBe(btnBefore);
      expect(capturedRef).toBe(btnBefore);
      expect(btnAfter.disabled).toBe(false);

      // Focus and hover now work on the enabled button.
      act(() => {
        btnAfter.focus();
      });
      expect(document.activeElement).toBe(btnAfter);

      pointerEnter(btnAfter);
      advance(450);
      expect(visibleTooltips()).toHaveLength(1);
      expect(visibleTooltips()[0]!.textContent).toBe("Archive");
      expect(warnSpy).not.toHaveBeenCalled();
    } finally {
      warnSpy.mockRestore();
    }
  });

  it("preserves dynamic Drafts aria-label, merges existing aria-describedby, formats shortcuts cleanly, omits data-shortcuts-off, and keeps Safari 16 fallback zIndex below toast after 120 hovers", () => {
    useMonotonicFakeTimers();

    const { container } = mount(
      h(
        StudioTooltipProvider,
        null,
        tip(
          { label: "草稿", roleType: "description" },
          h(
            "button",
            {
              type: "button",
              className: "drafts-btn",
              "aria-label": "草稿（3）",
              "aria-describedby": "existing-desc",
            },
            "3",
          ),
        ),
        h("span", { id: "existing-desc" }, "Unsent drafts"),
        tip(
          {
            label: "写邮件",
            shortcut: STUDIO_SHORTCUT_KEYS.composeNew,
            roleType: "description",
          },
          h("button", { type: "button", className: "compose-btn" }, "写邮件"),
        ),
        tip(
          {
            label: "键盘快捷键",
            shortcut: STUDIO_SHORTCUT_KEYS.openShortcuts,
            roleType: "label",
          },
          h(
            "button",
            {
              type: "button",
              className: "shortcuts-btn",
              "aria-label": "键盘快捷键",
            },
            "?",
          ),
        ),
      ),
    );

    const draftsBtn = container.querySelector<HTMLButtonElement>(
      "button.drafts-btn",
    )!;
    const composeBtn = container.querySelector<HTMLButtonElement>(
      "button.compose-btn",
    )!;
    const shortcutsBtn = container.querySelector<HTMLButtonElement>(
      "button.shortcuts-btn",
    )!;

    // 1. Dynamic Drafts count aria-label is NOT clobbered by aria-labelledby,
    //    and existing aria-describedby is preserved & merged with the tooltip content ID!
    expect(draftsBtn.getAttribute("aria-label")).toBe("草稿（3）");
    expect(draftsBtn.getAttribute("aria-labelledby")).toBeNull();
    const describedBy = draftsBtn.getAttribute("aria-describedby") ?? "";
    const descTokens = describedBy.split(/\s+/).filter(Boolean);
    expect(descTokens).toContain("existing-desc");
    expect(descTokens.length).toBe(2);
    const tooltipId = descTokens.find((token) => token !== "existing-desc")!;
    expect(document.getElementById(tooltipId)?.textContent).toBe("草稿");

    // 2. Shortcuts: `mod+n` sets cross-platform aria-keyshortcuts and aria-hidden visual badge;
    //    `?` omits aria-keyshortcuts (WAI-ARIA 1.2) while still showing visual `?` badge.
    expect(composeBtn.getAttribute("aria-keyshortcuts")).toBe(
      "Meta+N Control+N",
    );
    expect(shortcutsBtn.getAttribute("aria-keyshortcuts")).toBeNull();

    pointerEnter(composeBtn);
    advance(450);
    const openTip = visibleTooltips()[0]!;
    expect(openTip).toBeDefined();
    expect(openTip.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(openTip.hasAttribute("data-shortcuts-off")).toBe(false);
    // Hovering a tooltip to inspect a shortcut MUST NOT block global shortcuts!
    expect(
      isGlobalShortcutBlocked({
        defaultPrevented: false,
        target: document.body,
      }),
    ).toBe(false);
    const badge = openTip.querySelector<HTMLElement>(".studio-tooltip-shortcut");
    expect(badge).not.toBeNull();
    expect(badge!.getAttribute("aria-hidden")).toBe("true");
    pointerLeave(composeBtn);
    advance(50);

    // 3. Safari 16 fallback zIndex isolation: hold an open fallback Dialog layer
    //    and hover 120 times -> tooltip zIndex stays above Dialog and strictly < toast (2000)!
    const dialogLayer = acquireFallbackLayer();
    try {
      for (let i = 0; i < 120; i += 1) {
        pointerEnter(draftsBtn);
        advance(450);
        const tip = visibleTooltips()[0]!;
        expect(tip).toBeDefined();
        const z = Number(tip.style.zIndex);
        expect(Number.isFinite(z)).toBe(true);
        expect(z).toBeGreaterThan(dialogLayer.content);
        expect(z).toBeLessThan(Z_INDEX_TOKENS.toast);
        pointerLeave(draftsBtn);
        advance(10);
      }
    } finally {
      dialogLayer.release();
    }
  });
});
