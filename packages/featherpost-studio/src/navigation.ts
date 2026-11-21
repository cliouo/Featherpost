import type { UiFolder } from "../../maild-ui/src/bridge/types";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import { queryOf } from "../../maild-ui/src/mailbox/controller";
import type { Scope } from "../../maild-ui/src/mailbox/types";
import type { IconName } from "../../maild-ui/src/host/types";

export interface FolderNode {
  folder: UiFolder;
  children: FolderNode[];
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
          a.folder.display_name.localeCompare(b.folder.display_name),
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
export function selectedFolder(scope: Scope, account: string, name: string) {
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
  const folder =
    query.account && query.folder
      ? controller.foldersOf(query.account).find((f) => f.name === query.folder)
      : undefined;
  const name =
    folder?.display_name ||
    query.folder ||
    (query.role === "inbox"
      ? "收件箱"
      : query.flagged
        ? "星标邮件"
        : "全部邮件");
  return {
    account: query.account,
    folder,
    name,
    label: query.account
      ? `${controller.label(query.account)} / ${name}`
      : `全部账户 / ${name}`,
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
      // Only account/folder scoping is supported by both protocols.
      ...(all
        ? {}
        : {
            account: queryOf(source ?? scope).account,
            folder: queryOf(source ?? scope).folder,
          }),
      query,
    });
  else if (scope.kind === "search") controller.exitSearch();
}
export function filterUnread(controller: MailboxController, unread: boolean) {
  controller.setUnreadFilter(unread);
}
