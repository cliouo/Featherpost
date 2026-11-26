// @vitest-environment jsdom
import {
  act,
  createElement as h,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { applyEffectiveLocales } from "@featherpost/i18n";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { StudioDialog } from "../src/app/Dialog";
import { ContextMenuHost, useContextMenu } from "../src/ContextMenu";
import { SyncCount } from "../src/SyncCount";
import { clipboardCopier } from "../src/studioActions";
import { acquireFallbackLayer, closeAllPopovers } from "../src/ui";
import { isShortcutBlockedTarget } from "../src/Workspace";
import {
  accountMenu,
  addressMenu,
  folderMenu,
  groupMenu,
  isTextEntry,
  messageMenu,
  selectionMenu,
  separator,
  tidy,
  type MailMenuDeps,
  type MenuEntry,
} from "../src/menus";
import { folder, online, summary } from "./fakeBridge";

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

type Item = Extract<MenuEntry, { kind: "item" }>;
const labels = (entries: MenuEntry[]) =>
  entries.map((e) => (e.kind === "separator" ? "—" : e.label));
const find = (entries: MenuEntry[], label: string) =>
  entries.find((e) => e.kind !== "separator" && e.label === label) as Item;
const sub = (entries: MenuEntry[], label: string) =>
  entries.find((e) => e.kind === "submenu" && e.label === label) as Extract<MenuEntry, { kind: "submenu" }>;

function deps(state: Partial<MailMenuDeps["controller"]["state"]> = {}) {
  const row = { ...summary("INBOX", "1", "议程"), sender: "林晓 <lin@example.com>", seen: false };
  const other = summary("INBOX", "2");
  const controller = {
    state: { snapshot: online, busy: false, checked: [], selected: null, ...state },
    foldersOf: () => [folder("INBOX", "inbox"), folder("Archive", "archive"), folder("Receipts")],
    targets: vi.fn(() => [row, other]),
    perform: vi.fn(),
    requestAction: vi.fn(),
    selectMessage: vi.fn(),
    toggle: vi.fn(),
    selectLoaded: vi.fn(),
  } as unknown as MailMenuDeps["controller"];
  const d: MailMenuDeps = { controller, reply: vi.fn(), copy: vi.fn(), findSender: vi.fn() };
  return { d, row, other, controller: controller as unknown as Record<string, ReturnType<typeof vi.fn>> };
}

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

function openMenus(): HTMLElement[] {
  return [...document.body.querySelectorAll<HTMLElement>('[role="menu"]:not([hidden])')];
}

function openMenu(): HTMLElement | null {
  return openMenus()[0] ?? null;
}

function dispatchContextMenu(
  target: Element,
  init: MouseEventInit = { button: 2, clientX: 80, clientY: 90 },
): MouseEvent {
  const event = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    target.dispatchEvent(event);
  });
  return event;
}

function dispatchMouseDown(
  target: Element,
  init: MouseEventInit = { button: 2, clientX: 80, clientY: 90 },
): MouseEvent {
  const event = new MouseEvent("mousedown", {
    bubbles: true,
    cancelable: true,
    ...init,
  });
  act(() => {
    target.dispatchEvent(event);
  });
  return event;
}

function dispatchPointerDown(
  target: EventTarget,
  init: PointerEventInit = { button: 0, pointerType: "mouse" },
) {
  act(() => {
    target.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        ...init,
      }),
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

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  closeAllPopovers();
  globalThis.getSelection?.()?.removeAllRanges();
  document.body.innerHTML = "";
  applyEffectiveLocales([]);
});

it("drops empty, leading, trailing and doubled separators, also inside submenus", () => {
  const run = () => {};
  expect(
    labels(tidy([separator, false, { kind: "item", label: "A", run }, separator, null, separator, { kind: "item", label: "B", run }, separator])),
  ).toEqual(["A", "—", "B"]);
  const nested = tidy([{ kind: "submenu", label: "S", entries: [separator, { kind: "item", label: "C", run }, separator] }]);
  expect(labels((nested[0] as Extract<MenuEntry, { kind: "submenu" }>).entries)).toEqual(["C"]);
});

it("leaves the system menu to text fields only", () => {
  const el = (tagName: string, type?: string, editable = false) => ({
    tagName,
    type,
    closest: () => (editable ? {} : null),
  });
  expect(isTextEntry(el("INPUT"))).toBe(true);
  expect(isTextEntry(el("INPUT", "search"))).toBe(true);
  expect(isTextEntry(el("TEXTAREA"))).toBe(true);
  expect(isTextEntry(el("DIV", undefined, true))).toBe(true);
  expect(isTextEntry(el("INPUT", "checkbox"))).toBe(false);
  expect(isTextEntry(el("BUTTON"))).toBe(false);
  expect(isTextEntry(null)).toBe(false);
});

it("acts on the right-clicked message only, not the open one", () => {
  const { d, row, controller } = deps({ selected: summary("INBOX", "9") });
  const menu = messageMenu(row, d);
  expect(labels(menu)).toEqual([
    "打开", "—", "回复", "回复全部", "转发", "—", "标为已读", "添加星标", "—", "归档", "移动到", "—",
    "加入多选", "复制邮件信息", "查找 林晓 的邮件", "—", "删除",
  ]);
  find(menu, "标为已读").run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "mark", flag: "seen", value: true }, [row]);
  find(menu, "归档").run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "archive" }, [row]);
  find(menu, "删除").run();
  expect(controller.requestAction).toHaveBeenCalledWith("delete", [row]);
  find(menu, "回复全部").run();
  expect(d.reply).toHaveBeenCalledWith(row, "reply_all");
  find(menu, "查找 林晓 的邮件").run();
  expect(d.findSender).toHaveBeenCalledWith("lin@example.com");
});

it("offers every other present folder as a direct move target", () => {
  const { d, row, controller } = deps();
  const move = sub(messageMenu(row, d), "移动到");
  // Role folders show their localized label.
  expect(labels(move.entries)).toEqual(["归档", "Receipts"]);
  (move.entries[1] as Item).run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "move", target: "Receipts" }, [row]);
});

it("distinguishes colliding localized move destinations and preserves their server targets", () => {
  applyEffectiveLocales(["zh-CN"]);
  const { d, row, controller } = deps();
  d.controller.foldersOf = () => [folder("Drafts", "drafts"), folder("草稿箱")];
  const move = sub(messageMenu(row, d), "移动到");
  expect(labels(move.entries)).toEqual(["草稿箱 · Drafts", "草稿箱 · 草稿箱"]);
  (move.entries[0] as Item).run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "move", target: "Drafts" }, [row]);
  (move.entries[1] as Item).run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "move", target: "草稿箱" }, [row]);
});

it("disables writes while offline, busy or version-blocked, but keeps reading actions", () => {
  for (const state of [
    { snapshot: { ...online, phase: "connecting" as const } },
    { busy: true },
    { snapshot: { ...online, compat: "app_outdated" as const } },
  ]) {
    const { d, row } = deps(state);
    const menu = messageMenu(row, d);
    for (const label of ["回复", "标为已读", "归档", "删除"]) expect(find(menu, label).disabled).toBe(true);
    expect(sub(menu, "移动到").disabled).toBe(true);
    expect(find(menu, "查找 林晓 的邮件").disabled).toBeFalsy();
  }
});

it("disables archive when the account has no other archive folder", () => {
  const { d } = deps();
  const archived = { ...summary("Archive", "1") };
  expect(find(messageMenu(archived, d), "归档").disabled).toBe(true);
});

it("switches to the selection when the row is part of a multi-selection", () => {
  const { d, row, other, controller } = deps({ checked: [summary("INBOX", "1"), summary("INBOX", "2")] });
  const menu = messageMenu(row, d);
  expect(menu[0]).toEqual({ kind: "label", label: "已选 2 封邮件" });
  find(menu, "全部标为已读").run();
  expect(controller.perform).toHaveBeenLastCalledWith({ action: "mark", flag: "seen", value: true }, [row, other]);
  find(menu, "删除 2 封邮件").run();
  expect(controller.requestAction).toHaveBeenCalledWith("delete", [row, other]);
  // The reading pane always acts on the open message.
  expect(messageMenu(row, d, true)[0]).toMatchObject({ label: "回复" });
});

it("refuses cross-account moves for a mixed selection", () => {
  const { d, row, controller } = deps({ checked: [summary("INBOX", "1"), summary("INBOX", "2")] });
  controller.targets.mockReturnValue([row, { ...summary("INBOX", "3"), account: "work" }]);
  const move = sub(messageMenu(row, d), "移动到（不能跨账户）");
  expect(move.disabled).toBe(true);
  expect(move.entries).toEqual([]);
});

it("builds sender, folder, account and selection menus", () => {
  const copy = vi.fn();
  const sender = addressMenu("林晓 <lin@example.com>", { compose: vi.fn(), copy, findSender: vi.fn() }, false);
  expect(labels(sender)).toEqual(["林晓", "写邮件给 lin@example.com", "复制邮件地址", "复制名称和地址", "—", "查找来往邮件"]);
  expect(find(sender, "写邮件给 lin@example.com").disabled).toBe(true);
  find(sender, "复制邮件地址").run();
  expect(copy).toHaveBeenCalledWith("lin@example.com", "邮件地址");

  const unavailable = { ...folder("Old"), present: false };
  const folderEntries = folderMenu(unavailable, {
    open: vi.fn(), openUnread: vi.fn(), searchIn: vi.fn(), sync: vi.fn(), copy, canSync: true,
  });
  expect(find(folderEntries, "打开").disabled).toBe(true);
  expect(labels(folderEntries)).not.toContain("展开子文件夹");

  const assign = vi.fn();
  const sync = vi.fn();
  const account = accountMenu("me@example.test", {
    open: vi.fn(), openUnread: vi.fn(), canOpen: true, groups: [{ id: "a", name: "工作" }, { id: "b", name: "其他" }],
    sync, canSync: true,
    group: "b", assign, manageGroups: vi.fn(), copy, settings: vi.fn(),
  });
  const syncItem = find(account, "立即同步");
  expect(syncItem.disabled).toBe(false);
  syncItem.run();
  expect(sync).toHaveBeenCalledOnce();
  const groups = sub(account, "移到分组").entries;
  expect(groups.map((g) => g.kind === "check" && g.checked)).toEqual([false, true, false, false]);
  (groups[0] as Extract<MenuEntry, { kind: "check" }>).run();
  expect(assign).toHaveBeenCalledWith("a");

  expect(selectionMenu("   ", { copy })).toEqual([]);
  const search = vi.fn();
  const selected = selectionMenu("下周  产品评审的议程和发布计划安排表格", { copy, search });
  expect(labels(selected)).toEqual(["复制", "搜索邮件「下周 产品评审的议程和发布计划安排表…」"]);
  (selected[1] as Item).run();
  expect(search).toHaveBeenCalledWith("下周 产品评审的议程和发布计划安排表格");
});

it("uses the count slot for the sync glyph while keeping the count in place", () => {
  const idle = renderToStaticMarkup(h(SyncCount, { count: 12, busy: false }));
  const busy = renderToStaticMarkup(h(SyncCount, { count: 12, busy: true }));
  expect(idle).toContain("<span>12</span>");
  expect(idle).not.toContain("sync-count-spinner");
  expect(busy).toContain('<span class="sync-count-hidden">12</span>');
  expect(busy).toContain("sync-count-spinner");
});

it("wraps the Studio root without adding wrapper DOM markup", () => {
  const html = renderToStaticMarkup(
    h(ContextMenuHost, { selection: () => [], children: h("div", { className: "studio" }, "x") }),
  );
  expect(html).toMatch(/^<div[^>]*class="studio"[^>]*>x<\/div>$/);
  expect(html).toContain('data-slot="menu-context-trigger"');
  expect(html).toContain("data-closed");
});

it("ADR-0048: every Studio menu follows the UI language", () => {
  applyEffectiveLocales(["en"]);
  const { d, row } = deps();
  const flatten = (entries: MenuEntry[]): string[] =>
    entries.flatMap((e) =>
      e.kind === "separator" ? [] : e.kind === "submenu" ? [e.label, e.empty ?? "", ...flatten(e.entries)] : [e.label],
    );
  const copy = vi.fn();
  const all = [
    ...messageMenu({ ...row, sender: "Lin <lin@example.com>", subject: "Agenda" }, d),
    ...messageMenu(row, deps({ checked: [row, summary("INBOX", "2")] }).d),
    ...addressMenu("Lin <lin@example.com>", { compose: vi.fn(), copy, findSender: vi.fn() }, true),
    ...folderMenu(folder("Archive", "archive"), {
      open: vi.fn(), openUnread: vi.fn(), searchIn: vi.fn(), sync: vi.fn(), copy, canSync: true,
      expansion: { expanded: true, toggle: vi.fn() },
    }),
    ...accountMenu("lin@example.com", {
      open: vi.fn(), openUnread: vi.fn(), canOpen: true, groups: [], group: "", assign: vi.fn(),
      sync: vi.fn(), canSync: false,
      manageGroups: vi.fn(), copy, settings: vi.fn(), expansion: { expanded: false, toggle: vi.fn() },
    }),
    ...groupMenu({ open: vi.fn(), openUnread: vi.fn(), manage: vi.fn(), expansion: { expanded: true, toggle: vi.fn() } }),
    ...selectionMenu("quarterly review", { copy, search: vi.fn() }),
  ];
  const texts = flatten(all);
  expect(find(all, "Sync Now").disabled).toBe(false);
  expect(all.filter((entry) => entry.kind === "item" && entry.label === "Sync Now").at(-1)).toMatchObject({ disabled: true });
  expect(texts).toContain("Reply All");
  expect(texts).toContain("Delete 2 Messages");
  expect(texts).toContain("Search Mail for “quarterly review”");
  for (const text of texts) expect(text).not.toMatch(/[\u3000-\u303f\u4e00-\u9fff]/);
  find(addressMenu("Lin <lin@example.com>", { compose: vi.fn(), copy, findSender: vi.fn() }, true), "Copy Email Address").run();
  expect(copy).toHaveBeenCalledWith("lin@example.com", "email address");

  const notify = vi.fn();
  const original = globalThis.navigator;
  Object.defineProperty(globalThis, "navigator", { value: undefined, configurable: true });
  try {
    clipboardCopier(notify)("x", "email address");
  } finally {
    Object.defineProperty(globalThis, "navigator", { value: original, configurable: true });
  }
  expect(notify).toHaveBeenCalledWith("Can't access the clipboard");
});

describe("ContextMenuHost (@ui-sdk/react/menu + MenuContextTrigger)", () => {
  it("opens on offered surface, highlights [data-menu-target], marks global attributes, and dismisses on blank area click", async () => {
    const onReply = vi.fn();
    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        { className: "surface" },
        h("div", { className: "blank" }, "Blank"),
        h(
          "div",
          {
            className: "row",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "item", label: "Reply", run: onReply },
                { kind: "item", label: "Delete", tone: "danger", run: () => {} },
              ]),
          },
          "Row 1",
        ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );
    const row = container.querySelector<HTMLElement>(".row")!;
    const blank = container.querySelector<HTMLElement>(".blank")!;

    const event = dispatchContextMenu(row);
    await flushTimers();

    expect(event.defaultPrevented).toBe(true);
    const menu = openMenu()!;
    expect(menu).not.toBeNull();
    expect(row.hasAttribute("data-menu-target")).toBe(true);
    expect(menu.hasAttribute("data-agent-hidden")).toBe(true);
    expect(menu.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(menu.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(isShortcutBlockedTarget(menu)).toBe(true);

    const items = [...menu.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(["Reply", "Delete"]);
    expect(items[1]!.getAttribute("data-tone")).toBe("danger");

    // Clicking blank area dismisses the open menu and clears [data-menu-target].
    dispatchPointerDown(blank, { button: 0, pointerType: "mouse" });
    await flushTimers();
    expect(openMenu()).toBeNull();
    expect(row.hasAttribute("data-menu-target")).toBe(false);
  });

  it("passes through native spellcheck/IME menu on text entries without preventing default", async () => {
    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        {
          className: "outer",
          onContextMenu: (e) =>
            menu.offer(e, () => [{ kind: "item", label: "Outer", run: () => {} }]),
        },
        h("input", { className: "field", type: "text", defaultValue: "hello" }),
        h("textarea", { className: "area", defaultValue: "world" }),
        h("div", { className: "editable", contentEditable: true, suppressContentEditableWarning: true }, "edit"),
        h("div", { className: "row" }, "Row"),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );

    for (const selector of [".field", ".area", ".editable"]) {
      const el = container.querySelector<HTMLElement>(selector)!;
      const ev = dispatchContextMenu(el);
      await flushTimers();
      expect(openMenus()).toHaveLength(0);
      expect(ev.defaultPrevented).toBe(false);
    }

    // Also closes any currently open menu when right-clicking a text entry, while still leaving native menu untouched.
    const row = container.querySelector<HTMLElement>(".row")!;
    dispatchContextMenu(row);
    await flushTimers();
    expect(openMenu()).not.toBeNull();

    const input = container.querySelector<HTMLInputElement>(".field")!;
    const ev = dispatchContextMenu(input);
    await flushTimers();
    expect(openMenu()).toBeNull();
    expect(ev.defaultPrevented).toBe(false);
  });

  it("suppresses empty menus (blocks WKWebView Reload/Inspect) and closes any open menu on blank right-click", async () => {
    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        { className: "surface" },
        h("div", { className: "blank" }, "Empty"),
        h(
          "div",
          {
            className: "row",
            onContextMenu: (e) =>
              menu.offer(e, () => [{ kind: "item", label: "Open", run: () => {} }]),
          },
          "Row",
        ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );
    const blank = container.querySelector<HTMLElement>(".blank")!;
    const row = container.querySelector<HTMLElement>(".row")!;

    const emptyEv = dispatchContextMenu(blank);
    await flushTimers();
    expect(openMenus()).toHaveLength(0);
    expect(emptyEv.defaultPrevented).toBe(true);

    // Open menu on row, then right-click blank area -> suppresses native menu and closes open menu.
    dispatchContextMenu(row);
    await flushTimers();
    expect(openMenu()).not.toBeNull();
    expect(row.hasAttribute("data-menu-target")).toBe(true);

    dispatchPointerDown(blank, { button: 2, pointerType: "mouse" });
    const blankWhileOpen = dispatchContextMenu(blank);
    await flushTimers();
    expect(blankWhileOpen.defaultPrevented).toBe(true);
    expect(openMenu()).toBeNull();
    expect(row.hasAttribute("data-menu-target")).toBe(false);
  });

  it("clears accidental WKWebView word selection on right-click and Ctrl-click, while keeping intentional selections", async () => {
    const copy = vi.fn();
    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        { className: "surface" },
        h(
          "div",
          {
            className: "row",
            onContextMenu: (e) =>
              menu.offer(e, () => [{ kind: "item", label: "Archive", run: () => {} }]),
          },
          h("span", { className: "word" }, "Important"),
          " subject",
        ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: (text) => [{ kind: "item", label: `Copy ${text}`, run: () => copy(text) }],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );
    const word = container.querySelector<HTMLElement>(".word")!;

    const selectWord = () => {
      const range = document.createRange();
      range.selectNode(word);
      const sel = window.getSelection()!;
      sel.removeAllRanges();
      sel.addRange(range);
    };

    // 1. Right-click where WebKit auto-selects a word between mousedown and contextmenu -> cleared!
    window.getSelection()!.removeAllRanges();
    dispatchMouseDown(word, { button: 2 });
    selectWord();
    expect(window.getSelection()!.toString()).toBe("Important");
    dispatchContextMenu(word, { button: 2 });
    await flushTimers();
    expect(window.getSelection()!.toString()).toBe("");
    expect(
      [...openMenu()!.querySelectorAll('[role="menuitem"]')].map((el) => el.textContent),
    ).toEqual(["Archive"]);

    keydown(openMenu()!, "Escape");
    await flushTimers();

    // 2. Ctrl-click where WebKit auto-selects a word between mousedown and contextmenu -> cleared!
    window.getSelection()!.removeAllRanges();
    dispatchMouseDown(word, { button: 0, ctrlKey: true });
    selectWord();
    dispatchContextMenu(word, { button: 0, ctrlKey: true });
    await flushTimers();
    expect(window.getSelection()!.toString()).toBe("");
    expect(
      [...openMenu()!.querySelectorAll('[role="menuitem"]')].map((el) => el.textContent),
    ).toEqual(["Archive"]);

    keydown(openMenu()!, "Escape");
    await flushTimers();

    // 3. Intentional selection already present BEFORE mousedown -> preserved and offers Copy!
    selectWord();
    dispatchMouseDown(word, { button: 2 });
    dispatchContextMenu(word, { button: 2 });
    await flushTimers();
    const items = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(["Copy Important", "Archive"]);
    act(() => {
      items[0]!.click();
    });
    await flushTimers();
    expect(copy).toHaveBeenCalledWith("Important");
  });

  it("resolves innermost offer, supports submenus, empty submenus, checkbox items, and group labels", async () => {
    const outerAction = vi.fn();
    const innerMove = vi.fn();
    const toggleCheck = vi.fn();

    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        {
          className: "outer",
          onContextMenu: (e) =>
            menu.offer(e, () => [{ kind: "item", label: "Outer", run: outerAction }]),
        },
        h(
          "div",
          {
            className: "inner",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "label", label: "2 Selected" },
                { kind: "check", label: "In Selection", checked: true, run: toggleCheck },
                separator,
                {
                  kind: "submenu",
                  label: "Move To",
                  entries: [{ kind: "item", label: "Receipts", run: innerMove }],
                },
                {
                  kind: "submenu",
                  label: "Empty Sub",
                  entries: [],
                  empty: "Nothing here",
                },
              ]),
          },
          "Inner",
        ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );
    const inner = container.querySelector<HTMLElement>(".inner")!;

    dispatchContextMenu(inner);
    await flushTimers();

    const menu = openMenu()!;
    expect(inner.hasAttribute("data-menu-target")).toBe(true);
    expect(menu.textContent).not.toContain("Outer");

    // Group label is wrapped in role="group" with aria-labelledby.
    const group = menu.querySelector<HTMLElement>('[role="group"]')!;
    expect(group).not.toBeNull();
    const labelId = group.getAttribute("aria-labelledby")!;
    expect(document.getElementById(labelId)?.textContent).toBe("2 Selected");

    // Checkbox item shows indicator when checked and toggles + closes on select.
    const checkbox = menu.querySelector<HTMLElement>('[role="menuitemcheckbox"]')!;
    expect(checkbox.getAttribute("aria-checked")).toBe("true");
    expect(checkbox.querySelector(".studio-menu-check")).not.toBeNull();

    // Open submenu via keyboard (focus Move To trigger -> ArrowRight).
    const triggers = [...menu.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    const moveTrigger = triggers.find((el) => el.textContent === "Move To")!;
    act(() => {
      moveTrigger.focus();
    });
    keydown(moveTrigger, "ArrowRight");
    await flushTimers();

    const menus = openMenus();
    expect(menus).toHaveLength(2);
    const submenu = menus[1]!;
    expect(submenu.hasAttribute("data-agent-hidden")).toBe(true);
    expect(submenu.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(submenu.hasAttribute("data-shortcuts-off")).toBe(true);
    const receipts = submenu.querySelector<HTMLElement>('[role="menuitem"]')!;
    expect(receipts.textContent).toBe("Receipts");
    act(() => {
      receipts.click();
    });
    await flushTimers();
    expect(innerMove).toHaveBeenCalledTimes(1);
    expect(openMenus()).toHaveLength(0);
    expect(inner.hasAttribute("data-menu-target")).toBe(false);

    // Reopen and verify empty submenu renders fallback message, and checkbox closes menu on click.
    dispatchContextMenu(inner);
    await flushTimers();
    const emptyTrigger = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
      (el) => el.textContent === "Empty Sub",
    )!;
    act(() => {
      emptyTrigger.focus();
    });
    keydown(emptyTrigger, "ArrowRight");
    await flushTimers();
    expect(openMenus()[1]!.querySelector(".studio-menu-empty")?.textContent).toBe("Nothing here");

    // Click checkbox item -> runs callback and closes menu.
    const checkItem = openMenu()!.querySelector<HTMLElement>('[role="menuitemcheckbox"]')!;
    act(() => {
      checkItem.click();
    });
    await flushTimers();
    expect(toggleCheck).toHaveBeenCalledTimes(1);
    expect(openMenus()).toHaveLength(0);
    expect(outerAction).not.toHaveBeenCalled();
  });

  it("handles right-click on unselected row, keyboard contextmenu, and target refresh while menu is open", async () => {
    const actions: string[] = [];
    let selectedId = "row-a";

    function Surface() {
      const menu = useContextMenu();
      return h(
        "div",
        { className: "surface" },
        h(
          "button",
          {
            type: "button",
            className: "row-a",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "item", label: "Action A1", run: () => actions.push("A1") },
                { kind: "item", label: "Action A2", run: () => actions.push("A2") },
              ]),
          },
          "Row A",
        ),
        h(
          "button",
          {
            type: "button",
            className: "row-b",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "item", label: "Action B1", run: () => actions.push("B1") },
              ]),
          },
          "Row B",
        ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(Surface)),
      }),
    );
    const rowA = container.querySelector<HTMLButtonElement>(".row-a")!;
    const rowB = container.querySelector<HTMLButtonElement>(".row-b")!;

    // 1. Right-click unselected Row B -> opens menu for Row B without changing selectedId.
    dispatchContextMenu(rowB, { button: 2, clientX: 50, clientY: 60 });
    await flushTimers();
    expect(selectedId).toBe("row-a");
    expect(rowB.hasAttribute("data-menu-target")).toBe(true);
    expect(rowA.hasAttribute("data-menu-target")).toBe(false);
    expect(
      [...openMenu()!.querySelectorAll('[role="menuitem"]')].map((el) => el.textContent),
    ).toEqual(["Action B1"]);

    // 2. While menu is ALREADY open on Row B, right-click Row A -> refreshes target & entries cleanly.
    dispatchPointerDown(rowA, { button: 2, pointerType: "mouse" });
    dispatchContextMenu(rowA, { button: 2, clientX: 20, clientY: 30 });
    await flushTimers();
    expect(openMenu()).not.toBeNull();
    expect(rowA.hasAttribute("data-menu-target")).toBe(true);
    expect(rowB.hasAttribute("data-menu-target")).toBe(false);
    expect(
      [...openMenu()!.querySelectorAll('[role="menuitem"]')].map((el) => el.textContent),
    ).toEqual(["Action A1", "Action A2"]);

    keydown(openMenu()!, "Escape");
    await flushTimers();
    expect(openMenu()).toBeNull();

    // 3. Keyboard contextmenu (button: 0, ctrlKey: false) focuses first item and allows ArrowDown + Enter.
    act(() => {
      rowA.focus();
    });
    dispatchContextMenu(rowA, { button: 0, ctrlKey: false });
    await flushTimers();
    const items = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    expect(document.activeElement).toBe(items[0]);
    keydown(items[0]!, "ArrowDown");
    await flushTimers();
    expect(document.activeElement).toBe(items[1]);
    keydown(items[1]!, "Enter");
    await flushTimers();
    expect(actions).toEqual(["A2"]);
    expect(openMenu()).toBeNull();
  });

  it("portals context menu inside enclosing modal StudioDialog container so inert never traps it", async () => {
    const onDialogAction = vi.fn();

    function ModalApp() {
      const menu = useContextMenu();
      return h(
        StudioDialog,
        { open: true, title: "Modal Host" },
        h(
          "div",
          {
            className: "modal-surface",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "item", label: "Inside Modal Action", run: onDialogAction },
              ]),
          },
          "Right-click me in modal",
        ),
      );
    }

    mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(ModalApp)),
      }),
    );
    await flushTimers();

    const dialogContent = document.body.querySelector<HTMLElement>('[data-slot="dialog-content"]')!;
    expect(dialogContent).not.toBeNull();
    const surface = dialogContent.querySelector<HTMLElement>(".modal-surface")!;

    dispatchContextMenu(surface, { button: 2, clientX: 120, clientY: 140 });
    await flushTimers();

    const menu = openMenu()!;
    expect(menu).not.toBeNull();
    // Portaled INSIDE the modal dialog content, never a sibling under inert document.body.
    expect(dialogContent.contains(menu)).toBe(true);
    expect(menu.closest("[inert]")).toBeNull();

    const item = menu.querySelector<HTMLElement>('[role="menuitem"]')!;
    expect(item.textContent).toBe("Inside Modal Action");
    act(() => {
      item.click();
    });
    await flushTimers();
    expect(onDialogAction).toHaveBeenCalledTimes(1);
    expect(openMenu()).toBeNull();
  });

  it("allocates monotonic Safari 16 fallback inline zIndex above existing fallback layers", async () => {
    const baseLayer = acquireFallbackLayer();
    try {
      function Surface() {
        const menu = useContextMenu();
        return h(
          "div",
          {
            className: "row",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                {
                  kind: "submenu",
                  label: "Sub",
                  entries: [{ kind: "item", label: "Leaf", run: () => {} }],
                },
              ]),
          },
          "Layer Row",
        );
      }

      const { container } = mount(
        h(ContextMenuHost, {
          selection: () => [],
          children: h("div", { className: "studio" }, h(Surface)),
        }),
      );
      dispatchContextMenu(container.querySelector(".row")!);
      await flushTimers();

      const rootMenu = openMenu()!;
      const rootZ = Number(rootMenu.style.zIndex);
      expect(Number.isFinite(rootZ)).toBe(true);
      expect(rootZ).toBeGreaterThan(baseLayer.content);
      expect(rootZ).toBeLessThan(2000);

      // Opening submenu allocates a strictly higher fallback z-index.
      const subTrigger = rootMenu.querySelector<HTMLElement>('[role="menuitem"]')!;
      act(() => {
        subTrigger.focus();
      });
      keydown(subTrigger, "ArrowRight");
      await flushTimers();
      const subMenu = openMenus()[1]!;
      const subZ = Number(subMenu.style.zIndex);
      expect(subZ).toBeGreaterThan(rootZ);
      expect(subZ).toBeLessThan(2000);
    } finally {
      baseLayer.release();
    }
  });

  it("dismisses via window lifecycle events and preserves focus moved to a Dialog or Editor", async () => {
    function FocusApp() {
      const menu = useContextMenu();
      const [dialogOpen, setDialogOpen] = useState(false);
      const cancelRef = useRef<HTMLButtonElement>(null);
      const editorRef = useRef<HTMLTextAreaElement>(null);

      return h(
        "div",
        { className: "surface" },
        h("textarea", { ref: editorRef, className: "editor-input" }),
        h(
          "button",
          {
            type: "button",
            className: "row",
            onContextMenu: (e) =>
              menu.offer(e, () => [
                { kind: "item", label: "Open Dialog", run: () => setDialogOpen(true) },
                { kind: "item", label: "Focus Editor", run: () => editorRef.current?.focus() },
              ]),
          },
          "Row",
        ),
        dialogOpen &&
          h(
            StudioDialog,
            {
              open: dialogOpen,
              title: "Confirm Delete",
              initialFocusRef: cancelRef,
              onClose: () => setDialogOpen(false),
            },
            h(
              "button",
              { ref: cancelRef, type: "button", className: "dialog-cancel", onClick: () => setDialogOpen(false) },
              "Cancel",
            ),
          ),
      );
    }

    const { container } = mount(
      h(ContextMenuHost, {
        selection: () => [],
        children: h("div", { className: "studio" }, h(FocusApp)),
      }),
    );
    const row = container.querySelector<HTMLButtonElement>(".row")!;
    const editor = container.querySelector<HTMLTextAreaElement>(".editor-input")!;

    // 1. Dismiss on closeAllPopovers(), pagehide, and hidden visibilitychange.
    dispatchContextMenu(row);
    await flushTimers();
    expect(openMenu()).not.toBeNull();
    act(() => {
      closeAllPopovers();
    });
    await flushTimers();
    expect(openMenu()).toBeNull();

    dispatchContextMenu(row);
    await flushTimers();
    expect(openMenu()).not.toBeNull();
    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });
    await flushTimers();
    expect(openMenu()).toBeNull();

    dispatchContextMenu(row);
    await flushTimers();
    expect(openMenu()).not.toBeNull();
    Object.defineProperty(document, "hidden", { value: true, configurable: true });
    try {
      act(() => {
        document.dispatchEvent(new Event("visibilitychange"));
      });
      await flushTimers();
      expect(openMenu()).toBeNull();
    } finally {
      Object.defineProperty(document, "hidden", { value: false, configurable: true });
    }

    // 2. Selecting "Focus Editor" moves focus to editor and menu close does NOT steal it back.
    act(() => {
      row.focus();
    });
    dispatchContextMenu(row);
    await flushTimers();
    const focusEditorItem = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
      (el) => el.textContent === "Focus Editor",
    )!;
    act(() => {
      focusEditorItem.click();
    });
    await flushTimers();
    expect(openMenu()).toBeNull();
    expect(document.activeElement).toBe(editor);

    // 3. Selecting "Open Dialog" opens StudioDialog and menu close does NOT steal focus from dialog.
    act(() => {
      row.focus();
    });
    dispatchContextMenu(row);
    await flushTimers();
    const openDialogItem = [...openMenu()!.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(
      (el) => el.textContent === "Open Dialog",
    )!;
    act(() => {
      openDialogItem.click();
    });
    await flushTimers();
    expect(openMenu()).toBeNull();
    const cancelBtn = document.body.querySelector<HTMLButtonElement>("button.dialog-cancel")!;
    expect(cancelBtn).not.toBeNull();
    expect(document.activeElement).toBe(cancelBtn);
  });
});
