// @vitest-environment jsdom
import { act, createElement as h, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { ComposeController } from "../src/compose/controller";
import { Host } from "../src/host/host";
import { MailboxController } from "../src/mailbox/controller";
import { DocumentController } from "../src/mailbox/document";
import { isGlobalShortcutBlocked, Studio } from "../src/Shell";
import { Workspace } from "../src/Workspace";
import { FakeBridge, folder, online, summary } from "./fakeBridge";
import { FakeCompose } from "./fakeCompose";

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
  return { container };
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
});

describe("Phase 0 shortcut gating and dead-code cleanup", () => {
  it("ignores defaultPrevented Escape in Workspace so overlay dismissal does not clear selection", async () => {
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
    const row = summary("INBOX", "1", "Hello");
    bridge.mailbox.queries[0]?.d.resolve({ messages: [row], total: 1 });
    await Promise.resolve();
    controller.selectMessage(row);

    const clearSpy = vi.spyOn(controller, "clearSelection");
    mount(
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

    const handled = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    handled.preventDefault();
    act(() => {
      window.dispatchEvent(handled);
    });
    expect(clearSpy).not.toHaveBeenCalled();

    const unhandled = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    act(() => {
      window.dispatchEvent(unhandled);
    });
    expect(clearSpy).toHaveBeenCalledTimes(1);
  });

  it("blocks Shell global shortcuts when target, activeElement, or a visible overlay carries data-shortcuts-off", () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const mailbox = new MailboxController(bridge);
    const composer = new ComposeController(new FakeCompose(), () => {}, () => {});
    cleanups.push(() => {
      composer.stop();
      mailbox.stop();
      host.stop();
    });

    const openRoleSpy = vi.spyOn(host, "openRole").mockImplementation(() => true);
    const { container } = mount(
      h(Studio, {
        host,
        mailbox,
        composer,
        setup: () => {},
        manageAccounts: () => {},
      }),
    );

    const searchInput = container.querySelector<HTMLInputElement>("input[data-mail-search]")!;
    expect(searchInput).not.toBeNull();

    // Visible overlay marked data-shortcuts-off blocks Cmd+K and Cmd+,
    const popup = document.createElement("div");
    popup.setAttribute("data-shortcuts-off", "");
    const item = document.createElement("button");
    item.textContent = "Choice";
    popup.appendChild(item);
    document.body.appendChild(popup);
    item.focus();

    act(() => {
      item.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "k",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: ",",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(document.activeElement).toBe(item);
    expect(openRoleSpy).not.toHaveBeenCalled();

    // Hidden data-shortcuts-off container does NOT block global shortcuts.
    popup.hidden = true;
    item.blur();

    // Cmd+Shift+F focuses mail search (while plain Cmd+F stays free for in-page find).
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "f",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(document.activeElement).not.toBe(searchInput);

    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "f",
          metaKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(document.activeElement).toBe(searchInput);

    // Cmd+K opens the Command Palette (allowed from normal inputs like mail search).
    act(() => {
      searchInput.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "k",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    const paletteDialog = document.body.querySelector<HTMLElement>(
      '[role="dialog"].command-palette-dialog',
    );
    expect(paletteDialog).not.toBeNull();
    // Toggle palette closed with Cmd+K before testing other global shortcuts.
    act(() => {
      paletteDialog!.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "k",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(
      document.body.querySelector('[role="dialog"].command-palette-dialog'),
    ).toBeNull();

    // `/` does NOT hijack focus outside `.message-column` (e.g., on body or inputs).
    searchInput.blur();
    act(() => {
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "/",
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(document.activeElement).not.toBe(searchInput);

    // `/` DOES focus mail search when focus is inside `.message-column` (non-input target).
    const column = document.createElement("section");
    column.className = "message-column";
    const rowBtn = document.createElement("button");
    rowBtn.className = "message-open";
    rowBtn.textContent = "Row";
    column.appendChild(rowBtn);
    container.appendChild(column);
    rowBtn.focus();
    act(() => {
      rowBtn.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "/",
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(document.activeElement).toBe(searchInput);

    // defaultPrevented global shortcut is ignored too.
    const prevented = new KeyboardEvent("keydown", {
      key: ",",
      metaKey: true,
      bubbles: true,
      cancelable: true,
    });
    prevented.preventDefault();
    expect(isGlobalShortcutBlocked(prevented)).toBe(true);
    act(() => {
      window.dispatchEvent(prevented);
    });
    expect(openRoleSpy).not.toHaveBeenCalled();

    // Unblocked Cmd+, opens settings.
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: ",",
          metaKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(openRoleSpy).toHaveBeenCalledWith("settings");
  });

  it("leaves no dead closest('dialog') checks in Studio sources", () => {
    const sources = import.meta.glob<string>("../src/**/*.{ts,tsx}", {
      eager: true,
      query: "?raw",
      import: "default",
    });
    const matches = Object.entries(sources)
      .filter(([, source]) => /closest\(\s*["']dialog["']\s*\)/.test(source))
      .map(([path]) => path);
    expect(matches).toEqual([]);
  });
});
