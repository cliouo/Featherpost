import { m } from "@featherpost/i18n/messages";
import { folderLabel, uiLocale } from "../../maild-ui/src/i18n";
import type { UiFolder } from "../../maild-ui/src/bridge/types";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import { queryOf } from "../../maild-ui/src/mailbox/controller";
import type { Scope } from "../../maild-ui/src/mailbox/types";
import type { IconName } from "../../maild-ui/src/host/types";

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
        : query.flagged
          ? m.studio_scope_starred()
          : m.studio_scope_all_mail());
  return {
    account: query.account,
    accounts: query.accounts,
    group: group?.name,
    folder,
    name,
    label: query.account
      ? `${controller.label(query.account)} / ${name}`
      : m.studio_scope_all_accounts_path({ name }),
  };
}
export function searchMail(
  controller: MailboxController,
  text: string,
  all = false,
  source?: Scope,
) {
  const scope = controller.store.getSnapshot().scope;
  const query = text.trim();
  if (query)
    controller.search({
      // The installed native bridge separates full-text queries from list filters.
      // The Studio bridge expands account sets before invoking either native protocol.
      ...(all
        ? {}
        : {
            account: queryOf(source ?? scope).account,
            ...(queryOf(source ?? scope).accounts
              ? { accounts: queryOf(source ?? scope).accounts }
              : {}),
            folder: queryOf(source ?? scope).folder,
          }),
      query,
    });
  else if (scope.kind === "search") controller.exitSearch();
}
export function filterUnread(controller: MailboxController, unread: boolean) {
  controller.setUnreadFilter(unread);
}
