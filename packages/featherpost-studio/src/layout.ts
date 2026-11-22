import {
  queryOf,
  type MailboxState,
} from "../../maild-ui/src/mailbox/controller";

export type StudioView = "standard" | "unified";
type ViewStorage = Pick<Storage, "getItem" | "setItem">;
const VIEW_KEY = "featherpost.studio.view";

/** View choice belongs to this frontend, not the daemon or a mail account. */
export function readStudioView(storage?: ViewStorage): StudioView {
  try {
    return (storage ?? globalThis.localStorage)?.getItem(VIEW_KEY) === "unified"
      ? "unified"
      : "standard";
  } catch {
    return "standard";
  }
}
export function saveStudioView(view: StudioView, storage?: ViewStorage) {
  try {
    (storage ?? globalThis.localStorage)?.setItem(VIEW_KEY, view);
  } catch {
    /* A private/locked webview still supports switching for this session. */
  }
}

/** A selected message wins over the list scope and any manually browsed account. */
export function focusedAccount(state: MailboxState, preferred?: string) {
  // Never misattribute an existing message to a fallback account, even if its
  // original account has just disappeared from the connection snapshot.
  return (
    state.selectedRow?.account ??
    queryOf(state.scope).account ??
    (state.snapshot.accounts.some((a) => a.account === preferred)
      ? preferred
      : state.snapshot.accounts[0]?.account)
  );
}

export const OTHER_GROUP = "other";
export interface MailboxGroup {
  id: string;
  name: string;
}
export interface Grouping {
  groups: MailboxGroup[];
  assignments: Record<string, string>;
  collapsed: string[];
}
const GROUP_KEY = "featherpost.studio.mailbox-groups";

export function createGrouping(accounts: readonly string[]): Grouping {
  return {
    groups: [
      { id: "frequent", name: "常用" },
      { id: OTHER_GROUP, name: "其他" },
    ],
    assignments: Object.fromEntries(
      accounts.slice(0, 3).map((id) => [id, "frequent"]),
    ),
    collapsed: [],
  };
}
export function readGrouping(storage?: ViewStorage): Grouping | null {
  try {
    const data: unknown = JSON.parse(
      (storage ?? globalThis.localStorage)?.getItem(GROUP_KEY) ?? "null",
    );
    if (
      !data ||
      typeof data !== "object" ||
      !("groups" in data) ||
      !Array.isArray(data.groups)
    )
      return null;
    const ids = new Set<string>();
    const groups: MailboxGroup[] = [];
    for (const g of data.groups) {
      if (
        !g ||
        typeof g !== "object" ||
        typeof g.id !== "string" ||
        !g.id ||
        ids.has(g.id) ||
        typeof g.name !== "string" ||
        !g.name.trim()
      )
        continue;
      ids.add(g.id);
      groups.push({ id: g.id, name: g.name.trim().slice(0, 40) });
    }
    if (!ids.has(OTHER_GROUP)) {
      ids.add(OTHER_GROUP);
      groups.push({ id: OTHER_GROUP, name: "其他" });
    }
    const raw =
      "assignments" in data &&
      data.assignments &&
      typeof data.assignments === "object"
        ? data.assignments
        : {};
    const assignments = Object.fromEntries(
      Object.entries(raw).filter(
        ([, group]) => typeof group === "string" && ids.has(group),
      ),
    );
    const collapsed =
      "collapsed" in data && Array.isArray(data.collapsed)
        ? data.collapsed.filter(
            (id): id is string => typeof id === "string" && ids.has(id),
          )
        : [];
    return { groups, assignments, collapsed };
  } catch {
    return null;
  }
}
export function saveGrouping(grouping: Grouping, storage?: ViewStorage) {
  try {
    (storage ?? globalThis.localStorage)?.setItem(
      GROUP_KEY,
      JSON.stringify(grouping),
    );
  } catch {
    /* Keep navigation usable when browser storage is unavailable. */
  }
}
export function groupAccounts(accounts: readonly string[], grouping: Grouping) {
  const ids = new Set(grouping.groups.map((g) => g.id));
  return grouping.groups.map((g) => ({
    ...g,
    accounts: accounts.filter((account) => {
      const assigned = grouping.assignments[account];
      return (ids.has(assigned) ? assigned : OTHER_GROUP) === g.id;
    }),
  }));
}
export function namedGroup(
  grouping: Grouping,
  id: string,
  name: string,
): Grouping {
  const clean = name.trim();
  if (!clean || clean.length > 40) throw new Error("分组名称须为 1–40 个字符");
  if (
    grouping.groups.some(
      (g) =>
        g.id !== id && g.name.toLocaleLowerCase() === clean.toLocaleLowerCase(),
    )
  )
    throw new Error("已有同名分组");
  const exists = grouping.groups.some((g) => g.id === id);
  const groups = exists
    ? grouping.groups.map((g) => (g.id === id ? { ...g, name: clean } : g))
    : [
        ...grouping.groups.filter((g) => g.id !== OTHER_GROUP),
        { id, name: clean },
        ...grouping.groups.filter((g) => g.id === OTHER_GROUP),
      ];
  return { ...grouping, groups };
}
export function removeGroup(grouping: Grouping, id: string): Grouping {
  if (id === OTHER_GROUP) return grouping;
  return {
    groups: grouping.groups.filter((g) => g.id !== id),
    assignments: Object.fromEntries(
      Object.entries(grouping.assignments).map(([account, group]) => [
        account,
        group === id ? OTHER_GROUP : group,
      ]),
    ),
    collapsed: grouping.collapsed.filter((g) => g !== id),
  };
}
