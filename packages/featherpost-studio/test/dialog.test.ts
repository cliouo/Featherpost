// @vitest-environment jsdom
import { createElement as h, useRef, useState, type ReactNode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import * as ts from "typescript";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { AlertDialog, StudioDialog } from "../src/app/Dialog";
import { StudioDropdownMenu, StudioDropdownMenuItem } from "../src/app/DropdownMenu";
import { closeAllPopovers, usePersistentOverlay } from "../src/ui";
import { ComposeController } from "../src/compose/controller";
import { Editor } from "../src/compose/Editor";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { Dialog } from "../src/mailbox/Dialog";
import { DocumentController } from "../src/mailbox/document";
import { LinkSheet } from "../src/mailbox/ReaderParts";
import { Choice, ChoiceRenderer } from "../src/settings/Choice";
import { StudioSelect } from "../src/StudioSelect";
import { isShortcutBlockedTarget } from "../src/Workspace";
import { FakeBridge } from "./fakeBridge";
import { FakeCompose } from "./fakeCompose";

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
  mountedRoots.push({ root, container });
  return {
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
}

function flushTimers() {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function flushPaint() {
  return act(async () => {
    await new Promise((resolve) =>
      requestAnimationFrame(() => setTimeout(resolve, 0)),
    );
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

function pointerDown(target: EventTarget, init: PointerEventInit = {}) {
  act(() => {
    target.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        button: 0,
        pointerType: "mouse",
        ...init,
      }),
    );
  });
}

function pointerUp(target: EventTarget, init: PointerEventInit = {}) {
  act(() => {
    target.dispatchEvent(
      new PointerEvent("pointerup", {
        bubbles: true,
        cancelable: true,
        button: 0,
        pointerType: "mouse",
        ...init,
      }),
    );
  });
}

function outsideClick(target: EventTarget, init: PointerEventInit = {}) {
  pointerDown(target, init);
  pointerUp(target, init);
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

describe("Unified @ui-sdk StudioDialog / Dialog / AlertDialog", () => {
  it("moves initial focus in, traps Tab cycling, marks background inert, closes on Escape, and restores focus to invoker", async () => {
    function Harness() {
      const [open, setOpen] = useState(false);
      const firstInputRef = useRef<HTMLInputElement>(null);
      return h(
        "div",
        { className: "app-shell" },
        h(
          "button",
          {
            type: "button",
            className: "invoker",
            onClick: () => setOpen(true),
          },
          "Open dialog",
        ),
        open &&
          h(
            Dialog,
            {
              title: "Edit group",
              onClose: () => setOpen(false),
              initialFocusRef: firstInputRef,
            },
            h("input", { ref: firstInputRef, "aria-label": "Group name" }),
            h("button", { type: "button", className: "save-btn" }, "Save"),
          ),
      );
    }

    const { container } = mount(h(Harness));
    const invoker = container.querySelector<HTMLButtonElement>("button.invoker")!;

    act(() => {
      invoker.focus();
      invoker.click();
    });
    await flushPaint();

    // Uses @ui-sdk portal (no native <dialog>, no .dialog-positioner) with modal & shortcut-gating attributes.
    expect(document.body.querySelector("dialog")).toBeNull();
    expect(document.body.querySelector(".dialog-positioner")).toBeNull();
    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    const overlay = document.body.querySelector<HTMLElement>(".dialog-overlay")!;
    expect(dialog).not.toBeNull();
    expect(overlay).not.toBeNull();
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(dialog.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(overlay.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(overlay.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(isShortcutBlockedTarget(dialog)).toBe(true);

    // Background content is marked inert while modal dialog is open.
    expect(invoker.closest("[inert]")).not.toBeNull();

    // Initial focus respects initialFocusRef.
    const input = dialog.querySelector<HTMLInputElement>("input")!;
    const closeBtn = dialog.querySelector<HTMLButtonElement>("header button")!;
    const saveBtn = dialog.querySelector<HTMLButtonElement>("button.save-btn")!;
    expect(document.activeElement).toBe(input);

    // Tab wraps from the last tabbable element back to the first, and Shift+Tab wraps back to the last.
    act(() => {
      saveBtn.focus();
    });
    const tabForward = keydown(saveBtn, "Tab");
    expect(tabForward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(closeBtn);

    const tabBackward = keydown(closeBtn, "Tab", { shiftKey: true });
    expect(tabBackward.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(saveBtn);

    // Escape closes the dialog, clears inert on background, and returns focus to the invoker.
    keydown(dialog, "Escape");
    await flushTimers();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    expect(invoker.closest("[inert]")).toBeNull();
    expect(document.activeElement).toBe(invoker);
  });

  it("dismisses Dialog on outside click (not drag-out), while AlertDialog ignores outside clicks", async () => {
    const onDialogClose = vi.fn();
    const first = mount(
      h(
        StudioDialog,
        { title: "Standard Dialog", onClose: onDialogClose },
        h("p", null, "Body"),
      ),
    );
    await flushTimers();

    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    const overlay = document.body.querySelector<HTMLElement>(".dialog-overlay")!;
    expect(overlay).not.toBeNull();

    // Dragging a text selection from inside the dialog onto the backdrop does not dismiss.
    pointerDown(dialog);
    pointerUp(overlay);
    await flushTimers();
    expect(onDialogClose).not.toHaveBeenCalled();

    // A full outside click (pointerdown + pointerup on backdrop) dismisses the dialog.
    outsideClick(overlay);
    await flushTimers();
    expect(onDialogClose).toHaveBeenCalledTimes(1);
    first.unmount();

    const onAlertClose = vi.fn();
    mount(
      h(
        AlertDialog,
        { title: "Destructive Confirm", onClose: onAlertClose },
        h("p", null, "Are you sure?"),
      ),
    );
    await flushTimers();

    const alertDialog = document.body.querySelector<HTMLElement>('[role="alertdialog"]')!;
    expect(alertDialog).not.toBeNull();
    const alertOverlay = document.body.querySelector<HTMLElement>(".dialog-overlay")!;
    outsideClick(alertOverlay);
    await flushTimers();
    expect(onAlertClose).not.toHaveBeenCalled();

    // Escape still closes AlertDialog when not busy.
    keydown(alertDialog, "Escape");
    await flushTimers();
    expect(onAlertClose).toHaveBeenCalledTimes(1);
  });

  it("blocks Escape, outside-click, and close button while busy=true", async () => {
    const onClose = vi.fn();
    const view = mount(
      h(
        Dialog,
        { title: "Deleting messages", busy: true, onClose },
        h("p", null, "Working..."),
      ),
    );
    await flushTimers();

    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    const overlay = document.body.querySelector<HTMLElement>(".dialog-overlay")!;
    const closeBtn = dialog.querySelector<HTMLButtonElement>("header button")!;
    expect(closeBtn.disabled).toBe(true);

    act(() => {
      closeBtn.click();
    });
    keydown(dialog, "Escape");
    outsideClick(overlay);
    await flushTimers();
    expect(onClose).not.toHaveBeenCalled();

    // Once busy clears, Escape closes normally.
    view.rerender(
      h(
        Dialog,
        { title: "Deleting messages", busy: false, onClose },
        h("p", null, "Done"),
      ),
    );
    await flushTimers();
    expect(closeBtn.disabled).toBe(false);
    keydown(dialog, "Escape");
    await flushTimers();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("allocates paired monotonic z-index for stacked dialogs in Safari 16 fallback and handles layer-by-layer Escape & busy guard", async () => {
    function StackedHarness({ secondBusy = false }: { secondBusy?: boolean }) {
      const [firstOpen, setFirstOpen] = useState(true);
      const [secondOpen, setSecondOpen] = useState(false);
      return h(
        "div",
        null,
        firstOpen &&
          h(
            StudioDialog,
            {
              title: "Outer Dialog",
              contentClassName: "outer-dialog",
              overlayClassName: "outer-overlay",
              onClose: () => setFirstOpen(false),
            },
            h(
              "button",
              {
                type: "button",
                className: "open-inner",
                onClick: () => setSecondOpen(true),
              },
              "Open Inner",
            ),
            secondOpen &&
              h(
                StudioDialog,
                {
                  title: "Inner Dialog",
                  contentClassName: "inner-dialog",
                  overlayClassName: "inner-overlay",
                  busy: secondBusy,
                  onClose: () => setSecondOpen(false),
                },
                h("p", null, "Nested content"),
              ),
          ),
      );
    }

    const view = mount(h(StackedHarness, { secondBusy: true }));
    await flushTimers();

    const outerDialog = document.body.querySelector<HTMLElement>(".outer-dialog")!;
    const outerOverlay = document.body.querySelector<HTMLElement>(".outer-overlay")!;
    const openInner = outerDialog.querySelector<HTMLButtonElement>("button.open-inner")!;

    act(() => {
      openInner.click();
    });
    await flushTimers();

    const innerDialog = document.body.querySelector<HTMLElement>(".inner-dialog")!;
    const innerOverlay = document.body.querySelector<HTMLElement>(".inner-overlay")!;
    expect(innerDialog).not.toBeNull();
    expect(innerOverlay).not.toBeNull();

    // Paired monotonic z-index: backdrop1 < dialog1 < backdrop2 < dialog2 (and below toast=2000).
    const zBackdrop1 = Number(outerOverlay.style.zIndex);
    const zDialog1 = Number(outerDialog.style.zIndex);
    const zBackdrop2 = Number(innerOverlay.style.zIndex);
    const zDialog2 = Number(innerDialog.style.zIndex);
    expect(zBackdrop1).toBeGreaterThan(1000);
    expect(zDialog1).toBeGreaterThan(zBackdrop1);
    expect(zBackdrop2).toBeGreaterThan(zDialog1);
    expect(zDialog2).toBeGreaterThan(zBackdrop2);
    expect(zDialog2).toBeLessThan(2000);

    // While Inner Dialog is busy, Escape closes neither Inner nor Outer Dialog.
    keydown(innerDialog, "Escape");
    await flushTimers();
    expect(document.body.querySelector(".inner-dialog")).not.toBeNull();
    expect(document.body.querySelector(".outer-dialog")).not.toBeNull();

    // Once busy clears, Escape closes Inner Dialog first, leaving Outer Dialog open.
    view.rerender(h(StackedHarness, { secondBusy: false }));
    await flushTimers();
    keydown(innerDialog, "Escape");
    await flushTimers();
    expect(document.body.querySelector(".inner-dialog")).toBeNull();
    expect(document.body.querySelector(".outer-dialog")).not.toBeNull();

    // Second Escape closes Outer Dialog.
    keydown(outerDialog, "Escape");
    await flushTimers();
    expect(document.body.querySelector(".outer-dialog")).toBeNull();
  });

  it("supports nested StudioSelect and StudioDropdownMenu inside Dialog without Escape leak or inert trap", async () => {
    const onClose = vi.fn();
    const onChoose = vi.fn();
    const onMenuAction = vi.fn();
    function MoveHarness() {
      const [folder, setFolder] = useState("Archive");
      return h(
        ChoiceRenderer.Provider,
        { value: StudioSelect },
        h(
          Dialog,
          { title: "Move message", onClose },
          h(
            Choice,
            {
              "aria-label": "Target folder",
              value: folder,
              onValueChange: (next) => {
                setFolder(next);
                onChoose(next);
              },
              children: [
                h("option", { key: "Archive", value: "Archive" }, "Archive"),
                h("option", { key: "Receipts", value: "Receipts" }, "Receipts"),
              ],
            },
          ),
          h(
            StudioDropdownMenu,
            {
              trigger: h("button", { type: "button", className: "nested-menu-trigger" }, "Options"),
            },
            h(StudioDropdownMenuItem, { onSelect: onMenuAction }, "Menu Action"),
          ),
        ),
      );
    }

    mount(h(MoveHarness));
    await flushPaint();

    const dialog = document.body.querySelector<HTMLElement>('[role="dialog"]')!;
    const dialogZ = Number(dialog.style.zIndex);
    const combobox = dialog.querySelector<HTMLButtonElement>('[role="combobox"]')!;
    expect(combobox).not.toBeNull();

    // Open Select via keyboard inside Dialog.
    act(() => {
      combobox.focus();
    });
    keydown(combobox, "ArrowDown");
    await flushTimers();

    const listbox = document.body.querySelector<HTMLElement>('[role="listbox"]')!;
    expect(listbox).not.toBeNull();
    expect(listbox.closest("[inert]")).toBeNull();
    expect(Number(listbox.style.zIndex)).toBeGreaterThan(dialogZ);
    const options = [...listbox.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options).toHaveLength(2);

    // Pressing Escape while Select is open closes only Select, keeping the parent Dialog open.
    keydown(options[0], "Escape");
    await flushTimers();
    expect(document.body.querySelector('[role="listbox"]:not([hidden])')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();

    // Reopen and select via pointerdown + click without closing the parent Dialog.
    act(() => {
      combobox.focus();
    });
    keydown(combobox, "ArrowDown");
    await flushTimers();
    const reopened = document.body.querySelector<HTMLElement>('[role="listbox"]:not([hidden])')!;
    const reopenedOptions = [...reopened.querySelectorAll<HTMLElement>('[role="option"]')];
    keydown(reopenedOptions[0], "ArrowDown");
    await flushTimers();
    pointerDown(reopenedOptions[1]);
    act(() => {
      reopenedOptions[1].click();
    });
    await flushTimers();
    expect(onChoose).toHaveBeenCalledWith("Receipts");
    expect(onClose).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();

    // Open nested StudioDropdownMenu inside Dialog: not inert, stacked above dialog, Escape closes only menu.
    const menuTrigger = dialog.querySelector<HTMLButtonElement>("button.nested-menu-trigger")!;
    pointerDown(menuTrigger);
    await flushTimers();
    const nestedMenu = document.body.querySelector<HTMLElement>('[role="menu"]:not([hidden])')!;
    expect(nestedMenu).not.toBeNull();
    expect(nestedMenu.closest("[inert]")).toBeNull();
    expect(Number(nestedMenu.style.zIndex)).toBeGreaterThan(dialogZ);

    keydown(nestedMenu, "Escape");
    await flushTimers();
    expect(document.body.querySelector('[role="menu"]:not([hidden])')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
  });

  it("keeps .toast non-inert and non-dismissing alongside an open modal Dialog", async () => {
    const onClose = vi.fn();
    function ToastHarness() {
      const toastRef = useRef<HTMLDivElement>(null);
      usePersistentOverlay(toastRef);
      return h(
        "div",
        { className: "studio" },
        h("main", { className: "studio-workspace" }, h("button", { type: "button", className: "bg-btn" }, "Background")),
        h(
          "div",
          {
            ref: toastRef,
            className: "toast show",
            role: "status",
            "aria-live": "polite",
          },
          "Copied link",
        ),
        h(StudioDialog, { title: "Link Details", onClose }, h("p", null, "Details")),
      );
    }

    const { container } = mount(h(ToastHarness));
    await flushPaint();

    const bgBtn = container.querySelector<HTMLButtonElement>("button.bg-btn")!;
    const toast = container.querySelector<HTMLElement>(".toast")!;
    expect(bgBtn.closest("[inert]")).not.toBeNull();
    expect(toast.closest("[inert]")).toBeNull();

    // Clicking on the persistent toast must not dismiss the open modal Dialog.
    outsideClick(toast);
    await flushTimers();
    expect(onClose).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
  });

  it("promotes .toast into native top-layer with popover='manual' when supportsTopLayer() is true", () => {
    const showPopover = vi.fn();
    const hidePopover = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "showPopover", {
      configurable: true,
      value: showPopover,
    });
    Object.defineProperty(HTMLElement.prototype, "hidePopover", {
      configurable: true,
      value: hidePopover,
    });
    try {
      function TopLayerToastHarness() {
        const toastRef = useRef<HTMLDivElement>(null);
        usePersistentOverlay(toastRef);
        return h("div", { ref: toastRef, className: "toast", role: "status" }, "Saved");
      }

      const view = mount(h(TopLayerToastHarness));
      const toast = view.container.querySelector<HTMLElement>(".toast")!;
      expect(toast.getAttribute("popover")).toBe("manual");
      expect(showPopover).toHaveBeenCalledTimes(1);

      view.unmount();
      expect(hidePopover).toHaveBeenCalledTimes(1);
    } finally {
      delete (HTMLElement.prototype as { showPopover?: unknown }).showPopover;
      delete (HTMLElement.prototype as { hidePopover?: unknown }).hidePopover;
    }
  });

  it("closes an open StudioDropdownMenu when a Dialog opens and blocks Cmd+N global shortcut", async () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const composer = new ComposeController(bridge.compose, () => {}, () => {});
    const createSpy = vi.spyOn(composer, "create");
    const bundle = composeModule(host, composer);

    function Harness() {
      const [dialogOpen, setDialogOpen] = useState(false);
      return h(
        "div",
        null,
        h(
          StudioDropdownMenu,
          {
            trigger: h("button", { type: "button", className: "menu-trigger" }, "More"),
          },
          h(StudioDropdownMenuItem, null, "Item"),
        ),
        h(
          "button",
          {
            type: "button",
            className: "open-modal",
            onClick: () => setDialogOpen(true),
          },
          "Modal",
        ),
        dialogOpen &&
          h(Dialog, { title: "Confirm", onClose: () => setDialogOpen(false) }, h("p", null, "Hi")),
      );
    }

    try {
      const { container } = mount(h(Harness));
      const menuTrigger = container.querySelector<HTMLButtonElement>("button.menu-trigger")!;
      pointerDown(menuTrigger);
      await flushTimers();
      expect(document.body.querySelector('[role="menu"]:not([hidden])')).not.toBeNull();

      act(() => {
        container.querySelector<HTMLButtonElement>("button.open-modal")!.click();
      });
      await flushTimers();
      expect(document.body.querySelector('[role="dialog"]')).not.toBeNull();
      expect(document.body.querySelector('[role="menu"]:not([hidden])')).toBeNull();

      // Cmd+N must not trigger compose while modal dialog is open.
      keydown(window, "n", { metaKey: true });
      expect(createSpy).not.toHaveBeenCalled();
    } finally {
      bundle.dispose?.();
      composer.stop();
    }
  });
});

describe("LinkSheet migration", () => {
  it("focuses Cancel initially, dismisses on Escape and outside click, and restores focus", async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const notify = vi.fn();
    const compose = vi.fn();
    const dismissSpy = vi.fn();

    function Harness() {
      const [show, setShow] = useState(false);
      return h(
        "div",
        null,
        h(
          "button",
          { type: "button", className: "invoker", onClick: () => setShow(true) },
          "Open link",
        ),
        show &&
          h(LinkSheet, {
            link: {
              token: "tok",
              index: 0,
              href: "https://example.test/docs",
              kind: "https",
              host: "example.test",
            },
            documents: {
              ...documents,
              dismissLink() {
                dismissSpy();
                setShow(false);
              },
              openPendingLink: () => Promise.resolve(true),
            } as unknown as DocumentController,
            notify,
            compose,
          }),
      );
    }

    const { container } = mount(h(Harness));
    const invoker = container.querySelector<HTMLButtonElement>("button.invoker")!;
    act(() => {
      invoker.focus();
      invoker.click();
    });
    await flushTimers();

    const sheet = document.body.querySelector<HTMLElement>('.sheet[role="dialog"]')!;
    expect(sheet).not.toBeNull();
    expect(sheet.getAttribute("aria-modal")).toBe("true");
    expect(sheet.getAttribute("aria-labelledby")).toBeTruthy();
    expect(sheet.getAttribute("aria-describedby")).toBeTruthy();
    expect(sheet.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(sheet.textContent).toContain("https://example.test/docs");

    // Initial focus lands on the safe Cancel button.
    expect(document.activeElement?.textContent).toBe("取消");

    // Outside click dismisses LinkSheet and restores focus to invoker.
    const overlay = document.body.querySelector<HTMLElement>(".dialog-overlay")!;
    outsideClick(overlay);
    await flushTimers();
    expect(dismissSpy).toHaveBeenCalledTimes(1);
    expect(document.body.querySelector(".sheet")).toBeNull();
    expect(document.activeElement).toBe(invoker);

    // Reopen and verify Escape also dismisses cleanly.
    act(() => {
      invoker.click();
    });
    await flushTimers();
    keydown(document.body.querySelector(".sheet")!, "Escape");
    await flushTimers();
    expect(dismissSpy).toHaveBeenCalledTimes(2);
    expect(document.body.querySelector(".sheet")).toBeNull();
    expect(document.activeElement).toBe(invoker);
    documents.stop();
  });
});

describe("Compose Editor Confirm migration", () => {
  it("renders AlertDialog, focuses Back button, blocks outside click, stops Cmd+Enter leak, and executes confirm action", async () => {
    const bridge = new FakeCompose();
    const controller = new ComposeController(bridge, () => {}, () => {});
    controller.setOnline(true);
    await controller.refresh();
    await controller.create();
    controller.change("to", "alice@example.test");
    controller.change("subject", "Hello");
    const sendSpy = vi.spyOn(controller, "send");

    try {
      const { container } = mount(h(Editor, { controller }));
      const discardTrigger = container.querySelector<HTMLButtonElement>("button.danger-action")!;

      act(() => {
        discardTrigger.focus();
        discardTrigger.click();
      });
      await flushTimers();

      const alertDialog = document.body.querySelector<HTMLElement>('[role="alertdialog"]')!;
      expect(alertDialog).not.toBeNull();
      expect(alertDialog.getAttribute("aria-modal")).toBe("true");
      expect(alertDialog.hasAttribute("data-shortcuts-off")).toBe(true);
      // Initial focus lands on the safe Back button.
      expect(document.activeElement?.textContent).toBe("返回编辑");

      // Outside click must NOT dismiss AlertDialog.
      outsideClick(document.body.querySelector<HTMLElement>(".dialog-overlay")!);
      await flushTimers();
      expect(controller.store.getSnapshot().confirm).toBe("discard");

      // Cmd+Enter inside Confirm must NOT leak to the parent composer <form>.
      keydown(alertDialog, "Enter", { metaKey: true });
      expect(sendSpy).not.toHaveBeenCalled();

      // Escape dismisses Confirm and restores focus to the discard trigger.
      keydown(alertDialog, "Escape");
      await flushTimers();
      expect(controller.store.getSnapshot().confirm).toBeNull();
      expect(document.body.querySelector('[role="alertdialog"]')).toBeNull();
      expect(document.activeElement).toBe(discardTrigger);

      // Reopen and confirm discard.
      act(() => {
        discardTrigger.click();
      });
      await flushTimers();
      const confirmBtn = document.body.querySelector<HTMLButtonElement>(
        '[role="alertdialog"] button.danger-button',
      )!;
      expect(confirmBtn).not.toBeNull();
      act(() => {
        confirmBtn.click();
      });
      await flushTimers();
      expect(controller.store.getSnapshot().draft).toBeNull();
    } finally {
      controller.stop();
    }
  });
});

describe("Unified Dialog hygiene", () => {
  it("leaves no raw native <dialog> or manual .sheet-backdrop modals in Studio sources", () => {
    const sources = import.meta.glob<string>("../src/**/*.tsx", {
      eager: true,
      query: "?raw",
      import: "default",
    });
    const nativeDialogs: string[] = [];
    for (const [path, source] of Object.entries(sources)) {
      const file = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: ts.Node) => {
        if (
          (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) &&
          node.tagName.getText(file) === "dialog"
        ) {
          nativeDialogs.push(path);
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    const manualBackdrops = Object.entries(sources)
      .filter(([, source]) => /className="sheet-backdrop"/.test(source))
      .map(([path]) => path);
    expect(nativeDialogs).toEqual([]);
    expect(manualBackdrops).toEqual([]);
  });
});
