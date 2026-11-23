import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { applyEffectiveLocales } from "@featherpost/i18n";
import { afterEach, expect, it, vi } from "vitest";
import { ContextMenuHost } from "../../featherpost-studio/src/ContextMenu";
import { clipboardCopier } from "../../featherpost-studio/src/studioActions";
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
} from "../../featherpost-studio/src/menus";
import { folder, online, summary } from "./fakeBridge";

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
  const account = accountMenu("me@example.test", {
    open: vi.fn(), openUnread: vi.fn(), canOpen: true, groups: [{ id: "a", name: "工作" }, { id: "b", name: "其他" }],
    group: "b", assign, manageGroups: vi.fn(), copy, settings: vi.fn(),
  });
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

it("wraps the Studio root without adding markup", () => {
  const html = renderToStaticMarkup(
    h(ContextMenuHost, { selection: () => [], children: h("div", { className: "studio" }, "x") }),
  );
  expect(html).toBe('<div class="studio" data-state="closed" style="-webkit-touch-callout:none">x</div>');
});

afterEach(() => {
  applyEffectiveLocales([]);
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
      manageGroups: vi.fn(), copy, settings: vi.fn(), expansion: { expanded: false, toggle: vi.fn() },
    }),
    ...groupMenu({ open: vi.fn(), openUnread: vi.fn(), manage: vi.fn(), expansion: { expanded: true, toggle: vi.fn() } }),
    ...selectionMenu("quarterly review", { copy, search: vi.fn() }),
  ];
  const texts = flatten(all);
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
