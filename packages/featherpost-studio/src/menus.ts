import { folderLabel } from "../../maild-ui/src/i18n";
import { compatBlocksWrites } from "../../maild-ui/src/bridge/types";
import type { UiFolder, UiSummary } from "../../maild-ui/src/bridge/types";
import type { IconName } from "../../maild-ui/src/host/types";
import {
  messageKey,
  type MailboxController,
  type MailboxState,
} from "../../maild-ui/src/mailbox/controller";
import { addressOnly, displayName } from "../../maild-ui/src/mailbox/format";
import { folderIcon } from "./navigation";

/**
 * Studio context menus are plain data: each surface builds entries from the
 * object it was opened on, and one host renders them (ContextMenu.tsx).
 */
export type MenuEntry =
  | {
      kind: "item";
      label: string;
      icon?: IconName;
      hint?: string;
      tone?: "danger";
      disabled?: boolean;
      run(): void;
    }
  | {
      kind: "check";
      label: string;
      checked: boolean;
      disabled?: boolean;
      run(): void;
    }
  | {
      kind: "submenu";
      label: string;
      icon?: IconName;
      disabled?: boolean;
      entries: MenuEntry[];
      /** Shown inside an opened submenu that has nothing to offer. */
      empty?: string;
    }
  | { kind: "label"; label: string }
  | { kind: "separator" };

type Maybe = MenuEntry | false | null | undefined;

export const separator: MenuEntry = { kind: "separator" };

/** Drops falsy entries plus leading, trailing and doubled separators. */
export function tidy(entries: readonly Maybe[]): MenuEntry[] {
  const out: MenuEntry[] = [];
  for (const entry of entries) {
    if (!entry) continue;
    if (entry.kind === "separator" && (!out.length || out.at(-1)!.kind === "separator"))
      continue;
    out.push(entry.kind === "submenu" ? { ...entry, entries: tidy(entry.entries) } : entry);
  }
  while (out.at(-1)?.kind === "separator") out.pop();
  return out;
}

/** Shortens free text for use inside a menu label. */
export function quoted(text: string, max = 18): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return `「${flat.length > max ? `${flat.slice(0, max)}…` : flat}」`;
}

/**
 * Text fields keep the system menu: it carries paste, spelling and input
 * method entries that a WebView page cannot reproduce faithfully.
 */
export function isTextEntry(target: unknown): boolean {
  const el = target as {
    tagName?: string;
    type?: string;
    closest?: (selector: string) => unknown;
  } | null;
  if (!el?.tagName) return false;
  const tag = el.tagName.toLowerCase();
  if (tag === "textarea") return true;
  if (tag === "input")
    return ![
      "button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit",
    ].includes((el.type ?? "text").toLowerCase());
  return !!el.closest?.('[contenteditable]:not([contenteditable="false"])');
}

export type ReplyMode = "reply" | "reply_all" | "forward";

export interface MailMenuDeps {
  controller: Pick<
    MailboxController,
    "state" | "foldersOf" | "targets" | "perform" | "requestAction" | "selectMessage" | "toggle" | "selectLoaded"
  >;
  reply(row: UiSummary, mode: ReplyMode): void;
  copy(text: string, what: string): void;
  findSender(address: string): void;
}

export function canWrite(state: Pick<MailboxState, "snapshot" | "busy">): boolean {
  return state.snapshot.phase === "online" && !compatBlocksWrites(state.snapshot) && !state.busy;
}

function moveTargets(deps: MailMenuDeps, rows: UiSummary[]): MenuEntry[] {
  const account = rows[0]?.account ?? "";
  return deps.controller
    .foldersOf(account)
    .filter((f) => f.present && !rows.some((r) => r.folder === f.name))
    .map((f) => ({
      kind: "item",
      label: folderLabel(f),
      icon: folderIcon(f),
      run: () => void deps.controller.perform({ action: "move", target: f.name }, rows),
    }));
}

function hasArchive(deps: MailMenuDeps, row: UiSummary) {
  return deps.controller
    .foldersOf(row.account)
    .some((f) => f.present && f.role === "archive" && f.name !== row.folder);
}

/**
 * Right-click on a message. When it is part of a multi-selection the menu
 * acts on the whole selection, like Finder or Mail; otherwise on that row only.
 * `reading` is true for the open message in the reading pane.
 */
export function messageMenu(row: UiSummary, deps: MailMenuDeps, reading = false): MenuEntry[] {
  const { controller } = deps;
  const s = controller.state;
  const key = messageKey(row);
  const checked = s.checked.some((r) => messageKey(r) === key);
  if (!reading && checked && s.checked.length > 1) return batchMenu(deps);
  const writable = canWrite(s);
  const rows = [row];
  const address = addressOnly(row.sender);
  const opened = !!s.selected && messageKey(s.selected) === key;
  return tidy([
    !opened && !reading && { kind: "item", label: "打开", icon: "mail", run: () => controller.selectMessage(row) },
    separator,
    { kind: "item", label: "回复", icon: "reply", disabled: !writable, run: () => deps.reply(row, "reply") },
    { kind: "item", label: "回复全部", disabled: !writable, run: () => deps.reply(row, "reply_all") },
    { kind: "item", label: "转发", icon: "forward", disabled: !writable, run: () => deps.reply(row, "forward") },
    separator,
    {
      kind: "item",
      label: row.seen ? "标为未读" : "标为已读",
      disabled: !writable,
      run: () => void controller.perform({ action: "mark", flag: "seen", value: !row.seen }, rows),
    },
    {
      kind: "item",
      label: row.flagged ? "取消星标" : "添加星标",
      icon: "star",
      disabled: !writable,
      run: () => void controller.perform({ action: "mark", flag: "flagged", value: !row.flagged }, rows),
    },
    separator,
    {
      kind: "item",
      label: "归档",
      icon: "archive",
      disabled: !writable || !hasArchive(deps, row),
      run: () => void controller.perform({ action: "archive" }, rows),
    },
    {
      kind: "submenu",
      label: "移动到",
      icon: "folder",
      disabled: !writable,
      entries: moveTargets(deps, rows),
      empty: "没有其他可用文件夹",
    },
    separator,
    !reading && {
      kind: "check",
      label: "加入多选",
      checked,
      disabled: s.busy,
      run: () => controller.toggle(row),
    },
    {
      kind: "submenu",
      label: "复制邮件信息",
      icon: "clip",
      entries: tidy([
        !!address && { kind: "item", label: "发件人地址", run: () => deps.copy(address, "发件人地址") },
        !!row.subject && { kind: "item", label: "邮件主题", run: () => deps.copy(row.subject, "邮件主题") },
      ]),
    },
    !!address && {
      kind: "item",
      label: `查找 ${displayName(row.sender) || address} 的邮件`,
      icon: "search",
      run: () => deps.findSender(address),
    },
    separator,
    {
      kind: "item",
      label: "删除",
      icon: "trash",
      tone: "danger",
      disabled: !writable,
      run: () => controller.requestAction("delete", rows),
    },
  ]);
}

/** Right-click on any row of a multi-selection. */
export function batchMenu(deps: MailMenuDeps): MenuEntry[] {
  const { controller } = deps;
  const rows = controller.targets();
  const writable = canWrite(controller.state);
  const mark = (flag: "seen" | "flagged", value: boolean) => () =>
    void controller.perform({ action: "mark", flag, value }, rows);
  const oneAccount = new Set(rows.map((r) => r.account)).size === 1;
  return tidy([
    { kind: "label", label: `已选 ${rows.length} 封邮件` },
    { kind: "item", label: "全部标为已读", disabled: !writable, run: mark("seen", true) },
    { kind: "item", label: "全部标为未读", disabled: !writable, run: mark("seen", false) },
    { kind: "item", label: "添加星标", icon: "star", disabled: !writable, run: mark("flagged", true) },
    { kind: "item", label: "取消星标", disabled: !writable, run: mark("flagged", false) },
    separator,
    {
      kind: "item",
      label: "归档",
      icon: "archive",
      disabled: !writable,
      run: () => void controller.perform({ action: "archive" }, rows),
    },
    {
      kind: "submenu",
      label: oneAccount ? "移动到" : "移动到（不能跨账户）",
      icon: "folder",
      disabled: !writable || !oneAccount,
      entries: oneAccount ? moveTargets(deps, rows) : [],
      empty: "没有其他可用文件夹",
    },
    separator,
    { kind: "item", label: "取消选择", disabled: controller.state.busy, run: () => controller.selectLoaded(false) },
    {
      kind: "item",
      label: `删除 ${rows.length} 封邮件`,
      icon: "trash",
      tone: "danger",
      disabled: !writable,
      run: () => controller.requestAction("delete", rows),
    },
  ]);
}

/** Right-click on a sender or recipient address in the reading pane. */
export function addressMenu(
  sender: string,
  deps: { compose(address: string): void; copy(text: string, what: string): void; findSender(address: string): void },
  writable: boolean,
): MenuEntry[] {
  const address = addressOnly(sender);
  if (!address) return [];
  const name = displayName(sender);
  const named = !!name && name !== address;
  return tidy([
    named && { kind: "label", label: name },
    { kind: "item", label: `写邮件给 ${address}`, icon: "drafts", disabled: !writable, run: () => deps.compose(address) },
    { kind: "item", label: "复制邮件地址", icon: "clip", run: () => deps.copy(address, "邮件地址") },
    named && { kind: "item", label: "复制名称和地址", run: () => deps.copy(sender.trim(), "名称和地址") },
    separator,
    { kind: "item", label: "查找来往邮件", icon: "search", run: () => deps.findSender(address) },
  ]);
}

export interface FolderMenuDeps {
  open(): void;
  openUnread(): void;
  searchIn(): void;
  sync(): void;
  copy(text: string, what: string): void;
  canSync: boolean;
  /** Present only for folders that have children. */
  expansion?: { expanded: boolean; toggle(): void };
}

/** Right-click on a folder in the sidebar tree. */
export function folderMenu(folder: UiFolder, deps: FolderMenuDeps): MenuEntry[] {
  const unavailable = !folder.present;
  return tidy([
    { kind: "item", label: "打开", icon: folderIcon(folder), disabled: unavailable, run: deps.open },
    { kind: "item", label: "只看未读", disabled: unavailable, run: deps.openUnread },
    { kind: "item", label: "在此文件夹中搜索", icon: "search", disabled: unavailable, run: deps.searchIn },
    separator,
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? "折叠子文件夹" : "展开子文件夹",
      run: deps.expansion.toggle,
    },
    separator,
    { kind: "item", label: "立即同步", icon: "refresh", disabled: !deps.canSync, run: deps.sync },
    { kind: "item", label: "复制文件夹路径", icon: "clip", run: () => deps.copy(folder.name, "文件夹路径") },
  ]);
}

export interface AccountMenuDeps {
  open(): void;
  openUnread(): void;
  canOpen: boolean;
  groups: { id: string; name: string }[];
  group: string;
  assign(group: string): void;
  manageGroups(): void;
  copy(text: string, what: string): void;
  settings(): void;
  /** Present when the folder list can be expanded inline (standard view). */
  expansion?: { expanded: boolean; toggle(): void };
}

/** Right-click on a mailbox (account) in the sidebar. */
export function accountMenu(address: string, deps: AccountMenuDeps): MenuEntry[] {
  return tidy([
    { kind: "item", label: "打开收件箱", icon: "inbox", disabled: !deps.canOpen, run: deps.open },
    { kind: "item", label: "只看未读", disabled: !deps.canOpen, run: deps.openUnread },
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? "收起文件夹" : "展开文件夹",
      run: deps.expansion.toggle,
    },
    separator,
    {
      kind: "submenu",
      label: "移到分组",
      icon: "folder",
      entries: tidy([
        ...deps.groups.map(
          (g): MenuEntry => ({
            kind: "check",
            label: g.name,
            checked: g.id === deps.group,
            run: () => deps.assign(g.id),
          }),
        ),
        separator,
        { kind: "item", label: "管理分组…", run: deps.manageGroups },
      ]),
    },
    { kind: "item", label: "复制邮箱地址", icon: "clip", run: () => deps.copy(address, "邮箱地址") },
    separator,
    { kind: "item", label: "账户设置…", icon: "settings", run: deps.settings },
  ]);
}

/** Right-click on a mailbox group heading or group filter. */
export function groupMenu(deps: {
  open(): void;
  openUnread(): void;
  manage(): void;
  expansion?: { expanded: boolean; toggle(): void };
}): MenuEntry[] {
  return tidy([
    { kind: "item", label: "查看分组收件箱", icon: "inbox", run: deps.open },
    { kind: "item", label: "只看未读", run: deps.openUnread },
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? "收起分组" : "展开分组",
      run: deps.expansion.toggle,
    },
    separator,
    { kind: "item", label: "重命名或管理分组…", run: deps.manage },
  ]);
}

/** Leading entries when the menu opens over selected, read-only text. */
export function selectionMenu(
  text: string,
  deps: { copy(text: string, what: string): void; search?(text: string): void },
): MenuEntry[] {
  const query = text.replace(/\s+/g, " ").trim();
  if (!query) return [];
  return tidy([
    { kind: "item", label: "复制", icon: "clip", hint: "⌘ C", run: () => deps.copy(text, "所选文字") },
    !!deps.search && query.length <= 200 && {
      kind: "item",
      label: `搜索邮件${quoted(query)}`,
      icon: "search",
      run: () => deps.search!(query),
    },
  ]);
}
