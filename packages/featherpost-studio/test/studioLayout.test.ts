import { applyEffectiveLocales } from "@featherpost/i18n";
import { afterEach, describe, expect, it } from "vitest";
import type { MailboxState } from "../src/mailbox/controller";
import {
  createGrouping,
  focusedAccount,
  groupAccounts,
  namedGroup,
  OTHER_GROUP,
  readGrouping,
  readStudioView,
  removeGroup,
  saveGrouping,
  saveStudioView,
} from "../src/layout";

const accounts = Array.from({ length: 20 }, (_, n) => `mail-${n + 1}`);
function storage() {
  const data = new Map<string, string>();
  return {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => {
      data.set(key, value);
    },
  };
}
afterEach(() => {
  applyEffectiveLocales([]);
});
describe("Studio workflows and grouping", () => {
  it("keeps twenty mailboxes exactly once, including new and unassigned accounts", () => {
    const g = createGrouping(accounts);
    const groups = groupAccounts([...accounts, "new-mailbox"], g);
    const flattened = groups.flatMap((group) => group.accounts);
    expect(new Set(flattened).size).toBe(21);
    expect(flattened).toHaveLength(21);
    expect(
      groups.find((group) => group.id === OTHER_GROUP)?.accounts,
    ).toContain("new-mailbox");
  });
  it("persists assignments, names, collapsed groups and the selected workflow", () => {
    const s = storage();
    let g = namedGroup(createGrouping(accounts), "project", "项目");
    g = {
      ...g,
      assignments: { ...g.assignments, "mail-17": "project" },
      collapsed: ["project"],
    };
    saveGrouping(g, s);
    saveStudioView("unified", s);
    expect(readGrouping(s)).toEqual(g);
    expect(readStudioView(s)).toBe("unified");
    expect(
      groupAccounts(accounts, readGrouping(s)!).find(
        (group) => group.id === "project",
      )?.accounts,
    ).toEqual(["mail-17"]);
  });
  it("removes only a grouping, retaining all mailboxes and moving its members to Other", () => {
    let g = namedGroup(createGrouping(accounts), "project", "项目");
    g.assignments["mail-17"] = "project";
    g.collapsed.push("project");
    const next = removeGroup(g, "project");
    expect(next.assignments["mail-17"]).toBe(OTHER_GROUP);
    expect(next.collapsed).not.toContain("project");
    expect(
      groupAccounts(accounts, next)
        .flatMap((group) => group.accounts)
        .sort(),
    ).toEqual([...accounts].sort());
    expect(removeGroup(next, OTHER_GROUP)).toEqual(next);
    expect(g.assignments["mail-17"]).toBe("project");
  });
  it("rejects blank/duplicate names without changing existing assignments", () => {
    const g = createGrouping(accounts);
    expect(() => namedGroup(g, "new", "  ")).toThrow();
    expect(() => namedGroup(g, "new", "常用")).toThrow();
    expect(namedGroup(g, "frequent", " 工作 ").groups[0].name).toBe("工作");
    expect(g.groups[0].name).toBe("常用");
  });
  it("names default groups and validation errors in the current UI language", () => {
    applyEffectiveLocales(["en"]);
    const g = createGrouping(accounts);
    expect(g.groups.map((group) => group.name)).toEqual(["Frequent", "Other"]);
    expect(() => namedGroup(g, "new", "  ")).toThrow(
      "Group names must be 1–40 characters",
    );
    expect(() => namedGroup(g, "new", "frequent")).toThrow(
      "A group with this name already exists",
    );
  });
  it("repairs damaged group references rather than losing accounts", () => {
    const s = storage();
    s.setItem(
      "featherpost.studio.mailbox-groups",
      JSON.stringify({
        groups: [{ id: "p", name: "项目" }, { id: "p", name: "重复" }, null],
        assignments: { "mail-17": "missing" },
        collapsed: ["missing", 1],
      }),
    );
    const g = readGrouping(s)!;
    expect(g.groups.map((group) => group.id)).toEqual(["p", OTHER_GROUP]);
    expect(g.collapsed).toEqual([]);
    expect(
      groupAccounts(accounts, g).find((group) => group.id === OTHER_GROUP)
        ?.accounts,
    ).toHaveLength(20);
  });
  it("survives unavailable or corrupt browser storage", () => {
    const denied = {
      getItem() {
        throw new Error("denied");
      },
      setItem() {
        throw new Error("denied");
      },
    };
    expect(readGrouping(denied)).toBeNull();
    expect(readStudioView(denied)).toBe("standard");
    expect(() => {
      saveGrouping(createGrouping(accounts), denied);
      saveStudioView("unified", denied);
    }).not.toThrow();
    const corrupt = storage();
    corrupt.setItem("featherpost.studio.mailbox-groups", "{");
    expect(readGrouping(corrupt)).toBeNull();
  });
  it("follows message 17 instead of a previously browsed mailbox, even during account removal", () => {
    const state = {
      selectedRow: { account: "mail-17" },
      scope: {
        kind: "folder",
        account: "mail-1",
        folder: "INBOX",
        title: "收件箱",
      },
      snapshot: { accounts: [{ account: "mail-1" }] },
    } as MailboxState;
    expect(focusedAccount(state, "mail-1")).toBe("mail-17");
    state.selectedRow = null;
    expect(focusedAccount(state, "mail-17")).toBe("mail-1");
  });
});
