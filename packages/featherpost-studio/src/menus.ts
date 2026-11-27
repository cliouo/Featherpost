import { m } from "@featherpost/i18n/messages";
import { compatBlocksWrites } from "./bridge/types";
import type { UiFolder, UiSummary } from "./bridge/types";
import type { IconName } from "./host/types";
import {
  messageKey,
  type MailboxController,
  type MailboxState,
} from "./mailbox/controller";
import { addressOnly, displayName } from "./mailbox/format";
import { folderIcon, folderNavigationLabels } from "./navigation";
import { STUDIO_SHORTCUT_KEYS, formatShortcutText } from "./shortcuts";

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
  return m.studio_menu_quoted({ text: flat.length > max ? `${flat.slice(0, max)}…` : flat });
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
  const folders = deps.controller
    .foldersOf(account)
    .filter((f) => f.present && !rows.some((r) => r.folder === f.name));
  const labels = folderNavigationLabels(folders);
  return folders.map((f) => ({
      kind: "item",
      label: labels.get(f)!,
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
    !opened && !reading && { kind: "item", label: m.studio_menu_open(), icon: "mail", run: () => controller.selectMessage(row) },
    separator,
    { kind: "item", label: m.studio_menu_reply(), icon: "reply", disabled: !writable, run: () => deps.reply(row, "reply") },
    { kind: "item", label: m.studio_menu_reply_all(), disabled: !writable, run: () => deps.reply(row, "reply_all") },
    { kind: "item", label: m.studio_menu_forward(), icon: "forward", disabled: !writable, run: () => deps.reply(row, "forward") },
    separator,
    {
      kind: "item",
      label: row.seen ? m.studio_menu_mark_unread() : m.studio_menu_mark_read(),
      disabled: !writable,
      run: () => void controller.perform({ action: "mark", flag: "seen", value: !row.seen }, rows),
    },
    {
      kind: "item",
      label: row.flagged ? m.studio_menu_unstar() : m.studio_menu_star(),
      icon: "star",
      disabled: !writable,
      run: () => void controller.perform({ action: "mark", flag: "flagged", value: !row.flagged }, rows),
    },
    separator,
    {
      kind: "item",
      label: m.studio_menu_archive(),
      icon: "archive",
      disabled: !writable || !hasArchive(deps, row),
      run: () => void controller.perform({ action: "archive" }, rows),
    },
    {
      kind: "submenu",
      label: m.studio_menu_move_to(),
      icon: "folder",
      disabled: !writable,
      entries: moveTargets(deps, rows),
      empty: m.studio_menu_no_other_folders(),
    },
    separator,
    !reading && {
      kind: "check",
      label: m.studio_menu_add_to_selection(),
      checked,
      disabled: s.busy,
      run: () => controller.toggle(row),
    },
    {
      kind: "submenu",
      label: m.studio_menu_copy_info(),
      icon: "clip",
      entries: tidy([
        !!address && { kind: "item", label: m.studio_menu_sender_address(), run: () => deps.copy(address, m.studio_menu_what_sender_address()) },
        !!row.subject && { kind: "item", label: m.studio_menu_subject(), run: () => deps.copy(row.subject, m.studio_menu_what_subject()) },
      ]),
    },
    !!address && {
      kind: "item",
      label: m.studio_menu_find_from({ name: displayName(row.sender) || address }),
      icon: "search",
      run: () => deps.findSender(address),
    },
    separator,
    {
      kind: "item",
      label: m.studio_menu_delete(),
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
    { kind: "label", label: m.studio_menu_selected_count({ count: rows.length }) },
    { kind: "item", label: m.studio_menu_mark_all_read(), disabled: !writable, run: mark("seen", true) },
    { kind: "item", label: m.studio_menu_mark_all_unread(), disabled: !writable, run: mark("seen", false) },
    { kind: "item", label: m.studio_menu_star(), icon: "star", disabled: !writable, run: mark("flagged", true) },
    { kind: "item", label: m.studio_menu_unstar(), disabled: !writable, run: mark("flagged", false) },
    separator,
    {
      kind: "item",
      label: m.studio_menu_archive(),
      icon: "archive",
      disabled: !writable,
      run: () => void controller.perform({ action: "archive" }, rows),
    },
    {
      kind: "submenu",
      label: oneAccount ? m.studio_menu_move_to() : m.studio_menu_move_to_single_account(),
      icon: "folder",
      disabled: !writable || !oneAccount,
      entries: oneAccount ? moveTargets(deps, rows) : [],
      empty: m.studio_menu_no_other_folders(),
    },
    separator,
    { kind: "item", label: m.studio_menu_deselect(), disabled: controller.state.busy, run: () => controller.selectLoaded(false) },
    {
      kind: "item",
      label: m.studio_menu_delete_count({ count: rows.length }),
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
    { kind: "item", label: m.studio_menu_compose_to({ address }), icon: "drafts", disabled: !writable, run: () => deps.compose(address) },
    { kind: "item", label: m.studio_menu_copy_address(), icon: "clip", run: () => deps.copy(address, m.studio_menu_what_address()) },
    named && { kind: "item", label: m.studio_menu_copy_name_address(), run: () => deps.copy(sender.trim(), m.studio_menu_what_name_address()) },
    separator,
    { kind: "item", label: m.studio_menu_find_exchanged(), icon: "search", run: () => deps.findSender(address) },
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
    { kind: "item", label: m.studio_menu_open(), icon: folderIcon(folder), disabled: unavailable, run: deps.open },
    { kind: "item", label: m.studio_menu_unread_only(), disabled: unavailable, run: deps.openUnread },
    { kind: "item", label: m.studio_menu_search_folder(), icon: "search", disabled: unavailable, run: deps.searchIn },
    separator,
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? m.studio_menu_collapse_subfolders() : m.studio_menu_expand_subfolders(),
      run: deps.expansion.toggle,
    },
    separator,
    { kind: "item", label: m.studio_menu_sync_now(), icon: "refresh", disabled: !deps.canSync, run: deps.sync },
    { kind: "item", label: m.studio_menu_copy_folder_path(), icon: "clip", run: () => deps.copy(folder.name, m.studio_menu_what_folder_path()) },
  ]);
}

export interface AccountMenuDeps {
  open(): void;
  openUnread(): void;
  canOpen: boolean;
  sync(): void;
  canSync: boolean;
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
    { kind: "item", label: m.studio_menu_open_inbox(), icon: "inbox", disabled: !deps.canOpen, run: deps.open },
    { kind: "item", label: m.studio_menu_unread_only(), disabled: !deps.canOpen, run: deps.openUnread },
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? m.studio_menu_collapse_folders() : m.studio_menu_expand_folders(),
      run: deps.expansion.toggle,
    },
    { kind: "item", label: m.studio_menu_sync_now(), icon: "refresh", disabled: !deps.canSync, run: deps.sync },
    separator,
    {
      kind: "submenu",
      label: m.studio_menu_move_to_group(),
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
        { kind: "item", label: m.studio_menu_manage_groups(), run: deps.manageGroups },
      ]),
    },
    { kind: "item", label: m.studio_menu_copy_mailbox_address(), icon: "clip", run: () => deps.copy(address, m.studio_menu_what_mailbox_address()) },
    separator,
    { kind: "item", label: m.studio_menu_account_settings(), icon: "settings", run: deps.settings },
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
    { kind: "item", label: m.studio_menu_group_inbox(), icon: "inbox", run: deps.open },
    { kind: "item", label: m.studio_menu_unread_only(), run: deps.openUnread },
    deps.expansion && {
      kind: "item",
      label: deps.expansion.expanded ? m.studio_menu_collapse_group() : m.studio_menu_expand_group(),
      run: deps.expansion.toggle,
    },
    separator,
    { kind: "item", label: m.studio_menu_manage_group(), run: deps.manage },
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
    { kind: "item", label: m.studio_menu_copy(), icon: "clip", hint: formatShortcutText(STUDIO_SHORTCUT_KEYS.copySelection), run: () => deps.copy(text, m.studio_menu_what_selection()) },
    !!deps.search && query.length <= 200 && {
      kind: "item",
      label: m.studio_menu_search_mail({ query: quoted(query) }),
      icon: "search",
      run: () => deps.search!(query),
    },
  ]);
}
