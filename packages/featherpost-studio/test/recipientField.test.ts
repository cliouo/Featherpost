// @vitest-environment jsdom
import { act, createElement as h, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { applyEffectiveLocales } from "@featherpost/i18n";

import { BridgeError } from "../src/bridge/errors";
import type { UiContactSuggestion } from "../src/bridge/types";
import { ComposeController } from "../src/compose/controller";
import { Editor } from "../src/compose/Editor";
import { Host } from "../src/host/host";
import { MailboxController } from "../src/mailbox/controller";
import { DocumentController } from "../src/mailbox/document";
import { isGlobalShortcutBlocked } from "../src/Shell";
import { closeAllPopovers } from "../src/ui";
import { Workspace } from "../src/Workspace";
import { deferred, FakeBridge, folder, online, summary } from "./fakeBridge";
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
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];
const controllers: ComposeController[] = [];
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

function dispatchPaste(input: HTMLInputElement, text: string) {
  const event = new Event("paste", { bubbles: true, cancelable: true }) as ClipboardEvent;
  Object.defineProperty(event, "clipboardData", {
    value: {
      getData: (type: string) => (type === "text/plain" ? text : ""),
    },
  });
  act(() => {
    input.dispatchEvent(event);
  });
  return event;
}

async function setupCompose() {
  const bridge = new FakeCompose();
  const controller = new ComposeController(
    bridge,
    () => {},
    () => {},
  );
  controllers.push(controller);
  controller.setOnline(true);
  await controller.refresh();
  await controller.create();
  return { bridge, controller };
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
  controllers.splice(0).forEach((c) => c.stop());
  closeAllPopovers();
  applyEffectiveLocales([]);
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("RecipientField & Compose Editor chip integration", () => {
  it("commits chips on delimiter/Enter, handles multi-line/tab paste replacing selection, and displays friendly name + clear address", async () => {
    const { controller } = await setupCompose();
    const { container } = mount(h(Editor, { controller }));

    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    expect(toInput).not.toBeNull();
    toInput.focus();

    // Delimiter inside quoted display name does NOT split prematurely.
    setInputValue(toInput, '"Doe');
    const innerComma = dispatchKey(toInput, ",");
    expect(innerComma.defaultPrevented).toBe(false);

    // Committing full quoted display name + angle address normalizes to bare addr-spec and remembers display name.
    setInputValue(toInput, '"Doe, Alice" <Alice@EXAMPLE.COM>');
    const outerComma = dispatchKey(toInput, ",");
    expect(outerComma.defaultPrevented).toBe(true);

    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "Alice@example.com",
    ]);
    expect(controller.store.getSnapshot().recipientNames["Alice@example.com"]).toBe(
      "Doe, Alice",
    );

    // Chip renders both display name and clear address, plus accessible remove button with aria-label and title.
    const chip = container.querySelector<HTMLElement>(".recipient-chip")!;
    expect(chip.textContent).toContain("Doe, Alice");
    expect(chip.textContent).toContain("Alice@example.com");
    const removeBtn = chip.querySelector<HTMLButtonElement>(".recipient-chip-remove")!;
    expect(removeBtn.getAttribute("aria-label")).toContain("Alice@example.com");
    expect(removeBtn.getAttribute("title")).toContain("Alice@example.com");

    // Multi-line/tab paste replacing current selection commits all entries cleanly.
    setInputValue(toInput, "prefixREPLACEsuffix");
    toInput.setSelectionRange(0, "prefixREPLACEsuffix".length);
    dispatchPaste(toInput, "bob@example.com\tcarol@example.com\ndave@example.com");
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "Alice@example.com",
      "bob@example.com",
      "carol@example.com",
      "dave@example.com",
    ]);
    expect(controller.store.getSnapshot().recipients?.to.pending).toBe("");
  });

  it("supports in-place editing via double-click and focused chip Enter while preserving order, with Escape restoring original value", async () => {
    const { controller } = await setupCompose();
    controller.change("to", "first@x.com, second@x.com, third@x.com");
    const { container } = mount(h(Editor, { controller }));

    let chips = container.querySelectorAll<HTMLElement>(".recipient-chip");
    expect(chips).toHaveLength(3);

    // Double-click middle chip -> enters in-place edit preserving order.
    act(() => {
      chips[1]!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    expect(controller.store.getSnapshot().recipients?.to.editingIndex).toBe(1);
    let editInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    expect(editInput.value).toBe("second@x.com");

    // Escape restores original value without losing chip or order.
    setInputValue(editInput, "changed@x.com");
    dispatchKey(editInput, "Escape");
    expect(controller.store.getSnapshot().recipients?.to.editingIndex).toBeNull();
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "first@x.com",
      "second@x.com",
      "third@x.com",
    ]);

    // Focused chip Enter enters in-place edit, and Enter commits updated value in the same slot.
    chips = container.querySelectorAll<HTMLElement>(".recipient-chip");
    chips[1]!.focus();
    dispatchKey(chips[1]!, "Enter");
    expect(controller.store.getSnapshot().recipients?.to.editingIndex).toBe(1);
    editInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    setInputValue(editInput, "middle@x.com");
    dispatchKey(editInput, "Enter");
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "first@x.com",
      "middle@x.com",
      "third@x.com",
    ]);
  });

  it("retains invalid chips with danger styling, allows draft autosave, blocks send, and auto-expands hidden Cc/Bcc on error", async () => {
    vi.useFakeTimers();
    const { bridge, controller } = await setupCompose();
    const { container } = mount(h(Editor, { controller }));

    // Add an invalid chip to hidden Cc via controller, and keep Cc/Bcc collapsed initially.
    controller.change("to", "ok@example.com");
    controller.change("cc", "alice@");
    controller.change("subject", "Hello");

    // 500ms autosave succeeds for invalid syntax within byte/count limits.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(bridge.saves.length).toBeGreaterThanOrEqual(1);
    expect(bridge.saves.at(-1)!.patch.cc).toEqual(["alice@"]);

    // Hide Cc/Bcc via toggle button.
    const toggle = container.querySelector<HTMLButtonElement>(".cc-toggle")!;
    if (toggle.getAttribute("aria-expanded") === "true") {
      act(() => {
        toggle.click();
      });
    }
    expect(container.querySelector("#compose-cc")).toBeNull();

    // Send blocks and auto-expands Cc/Bcc, revealing the danger-styled invalid chip.
    await act(async () => {
      await controller.send();
    });
    expect(bridge.sends).toBe(0);
    expect(controller.store.getSnapshot().errors.cc).toBeTruthy();
    expect(container.querySelector("#compose-cc")).not.toBeNull();
    const invalidChip = container.querySelector<HTMLElement>(
      ".recipient-chip.invalid[data-invalid]",
    );
    expect(invalidChip).not.toBeNull();
    expect(invalidChip?.textContent).toContain("alice@");
  });

  it("retains over-limit paste (>100 recipients) for correction and stays clearly unsaved without dropping tail", async () => {
    vi.useFakeTimers();
    const { bridge, controller } = await setupCompose();
    const { container } = mount(h(Editor, { controller }));

    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    const batch = Array.from({ length: 105 }, (_, i) => `user${i}@example.com`).join("\n");
    dispatchPaste(toInput, batch);

    expect(controller.store.getSnapshot().recipients?.to.chips).toHaveLength(105);
    expect(controller.store.getSnapshot().recipients?.to.chips[104]).toBe(
      "user104@example.com",
    );
    expect(controller.store.getSnapshot().errors.to).toBeTruthy();

    // 500ms autosave MUST NOT call bridge.save or falsely show Saved.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(bridge.saves).toHaveLength(0);
    expect(controller.store.getSnapshot().revision).toBeGreaterThan(
      controller.store.getSnapshot().savedRevision,
    );
  });

  it("commits pending input once on Tab without blocking Tab traversal and once on blur leaving field+portal", async () => {
    const { controller } = await setupCompose();
    const { container } = mount(h(Editor, { controller }));

    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    const subjectInput = container.querySelector<HTMLInputElement>("#compose-subject")!;
    const commitSpy = vi.spyOn(controller, "commitRecipientInput");

    toInput.focus();
    setInputValue(toInput, "tab-user@example.com");
    const tabEvent = dispatchKey(toInput, "Tab");
    // Tab traversal is preserved (not defaultPrevented).
    expect(tabEvent.defaultPrevented).toBe(false);
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "tab-user@example.com",
    ]);
    // Immediately blurring to Subject after Tab does NOT double-commit.
    act(() => {
      toInput.dispatchEvent(
        new FocusEvent("focusout", { bubbles: true, relatedTarget: subjectInput }),
      );
    });
    expect(commitSpy).toHaveBeenCalledTimes(1);

    // Leaving via blur without Tab commits pending text once.
    toInput.focus();
    setInputValue(toInput, "blur-user@example.com");
    act(() => {
      toInput.dispatchEvent(
        new FocusEvent("focusout", { bubbles: true, relatedTarget: subjectInput }),
      );
    });
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "tab-user@example.com",
      "blur-user@example.com",
    ]);
    expect(commitSpy).toHaveBeenCalledTimes(2);
  });

  it("handles capture-phase Cmd/Ctrl+Enter once with pending commit, respects IME & repeat, and blocks plain Enter form submit", async () => {
    const { bridge, controller } = await setupCompose();
    controller.setContactSource({
      searchContacts: async () => [
        { address: "alice@example.com", name: "Alice" },
      ],
    });
    controller.change("subject", "Test subject");
    const { container } = mount(h(Editor, { controller }));

    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    toInput.focus();

    // Plain Enter on empty input with closed popup blocks implicit form submit.
    const emptyEnter = dispatchKey(toInput, "Enter");
    expect(emptyEnter.defaultPrevented).toBe(true);
    expect(bridge.sends).toBe(0);

    // Type pending recipient without committing.
    setInputValue(toInput, "pending@example.com");

    // IME composition (isComposing, keyCode 229, and compositionstart fallback) MUST NOT send.
    dispatchKey(toInput, "Enter", { metaKey: true, isComposing: true });
    dispatchKey(toInput, "Enter", { metaKey: true, keyCode: 229 });
    act(() => {
      toInput.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    });
    dispatchKey(toInput, "Enter", { metaKey: true });
    act(() => {
      toInput.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
    });
    expect(bridge.sends).toBe(0);

    // Repeated Cmd+Enter MUST NOT send.
    dispatchKey(toInput, "Enter", { metaKey: true, repeat: true });
    expect(bridge.sends).toBe(0);

    // Non-IME, non-repeat Cmd+Enter commits pending input synchronously and sends ONCE.
    await act(async () => {
      dispatchKey(toInput, "Enter", { metaKey: true });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(controller.store.getSnapshot().recipients?.to.chips).toEqual([
      "pending@example.com",
    ]);
    expect(bridge.sends).toBe(1);
  });

  it("excludes already-selected addresses, shows name-matched suggestions without SDK secondary filtering, and discards stale async responses on account switch/unmount", async () => {
    vi.useFakeTimers();
    const bridge = new FakeCompose();
    bridge.senders = async () => [
      { account: "acc-a", address: "me@a.test", label: "A", available: true, simple: false },
      { account: "acc-b", address: "me@b.test", label: "B", available: true, simple: false },
    ];
    const controller = new ComposeController(bridge, () => {}, () => {});
    controllers.push(controller);
    controller.setOnline(true);
    await controller.refresh();
    await controller.create();

    const pendingRequests: Array<{
      account: string;
      query?: string;
      d: ReturnType<typeof deferred<UiContactSuggestion[]>>;
    }> = [];
    controller.setContactSource({
      searchContacts: (account, query) => {
        const d = deferred<UiContactSuggestion[]>();
        pendingRequests.push({ account, query, d });
        return d.promise;
      },
    });

    controller.change("to", "selected@a.test");
    const { container } = mount(h(Editor, { controller }));

    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    act(() => {
      toInput.focus();
    });
    // Initial focus triggers immediate query for acc-a.
    expect(pendingRequests).toHaveLength(1);
    expect(pendingRequests[0]!.account).toBe("acc-a");

    // Type email substring "xyz" that matches by backend address even when displayed name is "Zhang Wei".
    setInputValue(toInput, "xyz");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    expect(pendingRequests).toHaveLength(2);

    // Resolve newer request first: already-selected address is excluded, name-matched result stays visible!
    await act(async () => {
      pendingRequests[1]!.d.resolve([
        { address: "selected@a.test", name: "Already Selected" },
        { address: "xyz@a.test", name: "Zhang Wei" },
      ]);
      await Promise.resolve();
    });

    const options = [...document.body.querySelectorAll<HTMLElement>(".recipient-suggest-item")];
    expect(options).toHaveLength(1);
    expect(options[0]!.textContent).toContain("Zhang Wei");
    expect(options[0]!.textContent).toContain("xyz@a.test");

    // Resolve older request #0 late -> MUST be discarded!
    await act(async () => {
      pendingRequests[0]!.d.resolve([{ address: "stale@a.test", name: "Stale" }]);
      await Promise.resolve();
    });
    expect(document.body.textContent).not.toContain("stale@a.test");

    // Switch sender account to acc-b -> immediately clears old suggestions and discards in-flight acc-a results.
    act(() => {
      controller.setSender(JSON.stringify(["acc-b", "me@b.test"]));
    });
    expect(document.body.textContent).not.toContain("Zhang Wei");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    const accBReq = pendingRequests.find((r) => r.account === "acc-b");
    expect(accBReq).toBeDefined();
    await act(async () => {
      accBReq!.d.resolve([{ address: "bob@b.test", name: "Bob B" }]);
      await Promise.resolve();
    });
    expect(document.body.textContent).toContain("bob@b.test");
  });

  it("falls back to current-account in-memory suggestions when offline or legacy daemon unsupported, while respecting denied", async () => {
    const { controller } = await setupCompose();
    controller.setContactSource({
      fallbackRows: (account) =>
        account === "demo"
          ? [
              {
                account: "demo",
                sender: '"Local Alice" <alice@local.test>',
                recipient: "me@example.test",
                sort_at: 100,
              },
            ]
          : [],
      searchContacts: async () => {
        throw new BridgeError({
          code: "failed",
          reason: "contacts_unsupported",
          message: "unsupported",
        });
      },
    });

    // Legacy unsupported -> falls back cleanly.
    const legacy = await controller.searchContacts("demo", "ali", 8);
    expect(legacy).toEqual([{ address: "alice@local.test", name: "Local Alice" }]);

    // Offline -> falls back cleanly.
    controller.setOnline(false);
    const offlineRes = await controller.searchContacts("demo", "ali", 8);
    expect(offlineRes).toEqual([{ address: "alice@local.test", name: "Local Alice" }]);

    // Real permission denial -> returns empty (no permission bypass).
    controller.setOnline(true);
    controller.setContactSource({
      searchContacts: async () => {
        throw new BridgeError({
          code: "denied",
          reason: "denied",
          message: "denied",
        });
      },
    });
    const deniedRes = await controller.searchContacts("demo", "ali", 8);
    expect(deniedRes).toEqual([]);
  });

  it("isolates inline reply chip focus from Workspace j/k/Escape shortcuts and places data-shortcuts-off ONLY on open popup", async () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const mailbox = new MailboxController(bridge);
    const documents = new DocumentController(bridge);
    const composeBridge = new FakeCompose();
    const composer = new ComposeController(composeBridge, () => {}, () => {});
    controllers.push(composer);
    cleanups.push(() => {
      documents.stop();
      mailbox.stop();
      host.stop();
    });

    mailbox.acceptSnapshot(online);
    for (const req of bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    const row1 = summary("INBOX", "1", "First");
    const row2 = summary("INBOX", "2", "Second");
    bridge.mailbox.queries[0]?.d.resolve({ messages: [row1, row2], total: 2 });
    await Promise.resolve();
    mailbox.selectMessage(row1);

    composer.setOnline(true);
    composer.setContactSource({
      searchContacts: async () => [{ address: "helper@example.test", name: "Helper" }],
    });
    await composer.refresh();
    await composer.reply(row1, "reply");

    const { container } = mount(
      h(Workspace, {
        host,
        controller: mailbox,
        documents,
        composer,
        setup: () => {},
      }),
    );
    await act(async () => {
      await Promise.resolve();
    });

    // Permanent chip container has data-chip-input, NEVER data-shortcuts-off!
    const chipsContainer = container.querySelector<HTMLElement>("[data-chip-input]")!;
    expect(chipsContainer).not.toBeNull();
    expect(chipsContainer.hasAttribute("data-shortcuts-off")).toBe(false);
    // Global shortcuts remain UNBLOCKED when popup is closed!
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true }),
      ),
    ).toBe(false);

    // Focusing a chip inside inline reply MUST NOT trigger Workspace j/k/Escape message switching!
    const chip = chipsContainer.querySelector<HTMLElement>(".recipient-chip")!;
    expect(chip).not.toBeNull();
    chip.focus();
    const clearSpy = vi.spyOn(mailbox, "clearSelection");
    dispatchKey(chip, "j");
    dispatchKey(chip, "k");
    dispatchKey(chip, "Escape");
    expect(mailbox.store.getSnapshot().selected?.uid).toBe("1");
    expect(clearSpy).not.toHaveBeenCalled();

    // Opening autocomplete popup mounts data-shortcuts-off on the open ComboboxContent ONLY.
    const toInput = container.querySelector<HTMLInputElement>("#compose-to")!;
    await act(async () => {
      toInput.focus();
      await Promise.resolve();
      await Promise.resolve();
    });
    const openPopup = document.body.querySelector<HTMLElement>(
      ".recipient-suggest-popup[data-shortcuts-off]",
    );
    expect(openPopup).not.toBeNull();
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true }),
      ),
    ).toBe(true);

    // Pressing Escape closes popup and restores global shortcuts immediately.
    dispatchKey(toInput, "Escape");
    expect(
      document.body.querySelector(".recipient-suggest-popup[data-shortcuts-off]"),
    ).toBeNull();
    expect(
      isGlobalShortcutBlocked(
        new KeyboardEvent("keydown", { key: "k", metaKey: true }),
      ),
    ).toBe(false);

    // Collapsing & expanding inline reply preserves uncommitted pending text in ComposeController!
    setInputValue(toInput, "unsaved-pending@example.test");
    act(() => {
      composer.collapse();
    });
    expect(container.querySelector("#compose-to")).toBeNull();
    act(() => {
      composer.collapse();
    });
    expect(container.querySelector<HTMLInputElement>("#compose-to")?.value).toBe(
      "unsaved-pending@example.test",
    );
  });
});
