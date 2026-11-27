import { m } from "@featherpost/i18n/messages";
import { folderLabel, uiLocale } from "./i18n";
import type { UiFolder } from "./bridge/types";
import type { MailboxController } from "./mailbox/controller";
import { queryOf } from "./mailbox/controller";
import type { MailboxQuery, Scope } from "./mailbox/types";
import type { IconName } from "./host/types";
import {
  hasActiveSearch,
  scopeFieldsForTier,
  type SearchFilterMeta,
  type SearchScopeTier,
} from "./searchFilters";

export interface FolderNode {
  folder: UiFolder;
  children: FolderNode[];
}

/** Keep colliding translated sibling labels distinct without merging server identities. */
export function folderNavigationLabels(folders: UiFolder[], nested = false) {
  const labels = folders.map((folder) => {
    const displayName =
      nested && folder.display_name === folder.name && /[/.]/.test(folder.name)
        ? folder.name.split(/[/.]/).pop()!
        : folder.display_name || folder.name;
    return folderLabel({ ...folder, display_name: displayName });
  });
  const counts = new Map<string, number>();
  for (const label of labels) counts.set(label, (counts.get(label) ?? 0) + 1);
  return new Map(
    folders.map((folder, index) => {
      const label = labels[index]!;
      const original =
        folder.display_name && folder.display_name !== label
          ? folder.display_name
          : folder.name;
      return [folder, counts.get(label)! > 1 ? `${label} · ${original}` : label];
    }),
  );
}

/** Search displayed and server names; rank exact matches first for account navigation. */
export function matchingFolderNames(folders: UiFolder[], filter: string) {
  const needle = filter.trim().toLocaleLowerCase(uiLocale());
  const exact = new Set<string>();
  const matches = new Set<string>();
  const visit = (nodes: FolderNode[], nested = false) => {
    const labels = folderNavigationLabels(nodes.map((n) => n.folder), nested);
    for (const { folder, children } of nodes) {
      if (folder.present) {
        const names = [folder.name, folder.display_name, folderLabel(folder), labels.get(folder)!]
          .map((name) => name.toLocaleLowerCase(uiLocale()));
        if (names.includes(needle)) exact.add(folder.name);
        else if (names.some((name) => name.includes(needle))) matches.add(folder.name);
      }
      visit(children, true);
    }
  };
  visit(folderTree(folders));
  return new Set([...exact, ...matches]);
}

const order: Record<string, number> = {
  inbox: 0,
  sent: 1,
  drafts: 2,
  archive: 3,
  trash: 9,
};
/** Nest only under folders the server actually returned, not invented path segments. */
export function folderTree(folders: UiFolder[]): FolderNode[] {
  const nodes = folders.map((folder) => ({
    folder,
    children: [] as FolderNode[],
  }));
  const roots: FolderNode[] = [];
  for (const node of nodes) {
    const parent = nodes
      .filter(
        (n) =>
          n !== node &&
          n.folder.account === node.folder.account &&
          ["/", "."].some((separator) =>
            node.folder.name.startsWith(n.folder.name + separator),
          ),
      )
      .sort((a, b) => b.folder.name.length - a.folder.name.length)[0];
    (parent ? parent.children : roots).push(node);
  }
  const prune = (items: FolderNode[]): FolderNode[] =>
    items
      .map((n) => ({ ...n, children: prune(n.children) }))
      .filter((n) => n.folder.present || n.children.length)
      .sort(
        (a, b) =>
          (order[a.folder.role] ?? 4) - (order[b.folder.role] ?? 4) ||
          folderLabel(a.folder).localeCompare(folderLabel(b.folder), uiLocale()),
      );
  return prune(roots);
}
export function folderIcon(folder: UiFolder): IconName {
  return (
    (
      {
        inbox: "inbox",
        sent: "sent",
        drafts: "drafts",
        archive: "archive",
        trash: "trash",
        junk: "spam",
        spam: "spam",
      } as Record<string, IconName>
    )[folder.role] ?? "folder"
  );
}
export function selectedFolder(
  scope: Scope,
  account: string,
  name: string,
  message?: { account: string; folder: string } | null,
): boolean {
  if (message) return message.account === account && message.folder === name;
  if (scope.kind === "unread") return selectedFolder(scope.base, account, name);
  return scope.kind === "folder"
    ? scope.account === account && scope.folder === name
    : scope.kind === "search" &&
        scope.query.account === account &&
        scope.query.folder === name;
}
export function scopeContext(
  controller: MailboxController,
  scope = controller.store.getSnapshot().scope,
) {
  const query = queryOf(scope);
  const base = scope.kind === "unread" ? scope.base : scope;
  const group = base.kind === "smart" ? base.group : undefined;
  const folder =
    query.account && query.folder
      ? controller.foldersOf(query.account).find((f) => f.name === query.folder)
      : undefined;
  const name =
    (folder && folderLabel(folder)) ||
    query.folder ||
    (group
      ? m.studio_scope_group_inbox({ group: group.name })
      : query.role === "inbox"
        ? query.account
          ? m.studio_scope_inbox()
          : m.studio_scope_all_inboxes()
        : !query.search && query.flagged
          ? m.studio_scope_starred()
          : m.studio_scope_all_mail());
  return {
    account: query.account,
    accounts: query.accounts,
    group: group?.name,
    folder,
    folderName: query.folder,
    name,
    label: query.account
      ? `${controller.label(query.account)} / ${name}`
      : m.studio_scope_all_accounts_path({ name }),
  };
}
export function applySearchQuery(
  controller: MailboxController,
  query: MailboxQuery,
  meta?: SearchFilterMeta,
) {
  if (hasActiveSearch(query)) {
    controller.search(query, meta);
  } else if (controller.store.getSnapshot().scope.kind === "search") {
    controller.exitSearch();
  }
}
export function searchMail(
  controller: MailboxController,
  text: string,
  all: boolean | SearchScopeTier = false,
  source?: Scope,
) {
  const scope = controller.store.getSnapshot().scope;
  const query = text.trim();
  const baseQuery = queryOf(source ?? scope);
  const scopeFields =
    all === true || all === "all"
      ? {}
      : typeof all === "string"
        ? scopeFieldsForTier(all, {
            account: baseQuery.account,
            accounts: baseQuery.accounts,
            folderName: baseQuery.folder,
          })
        : {
            ...(baseQuery.account ? { account: baseQuery.account } : {}),
            ...(baseQuery.accounts ? { accounts: baseQuery.accounts } : {}),
            ...(baseQuery.folder ? { folder: baseQuery.folder } : {}),
          };
  applySearchQuery(controller, {
    // The installed native bridge separates full-text queries from list filters.
    // The Studio bridge expands account sets before invoking either native protocol.
    ...scopeFields,
    ...(query ? { query } : {}),
  });
}
export function filterUnread(controller: MailboxController, unread: boolean) {
  controller.setUnreadFilter(unread);
}
