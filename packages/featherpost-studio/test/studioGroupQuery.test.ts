import { expect, it } from "vitest";
import { groupedBridge } from "../src/groupQuery";
import { searchMail } from "../src/navigation";
import { MailboxController, queryOf } from "../src/mailbox/controller";
import { FakeBridge, deferred, flush, folder, online, summary } from "./fakeBridge";
import type { MailboxQuery } from "../src/mailbox/types";
import type { UiPage } from "../src/bridge/types";

it("merges complete member streams across page boundaries without losing older matches or reading outsiders", async () => {
  const original = new FakeBridge();
  const a = Array.from({ length: 120 }, (_, n) => ({
    ...summary("INBOX", `a-${n}`),
    account: "a",
    sort_at: 1000 - n,
  }));
  const b = [999, 970, 850].map((at, n) => ({
    ...summary("INBOX", n === 0 ? "a-0" : `b-${n}`),
    account: "b",
    sort_at: at,
  }));
  const calls: { query: MailboxQuery; offset: number }[] = [];
  original.mailbox.query = async (query, offset) => {
    calls.push({ query, offset });
    expect(query).not.toHaveProperty("accounts");
    expect(["a", "b"]).toContain(query.account);
    const source = query.account === "a" ? a : b;
    return { messages: source.slice(offset, offset + 7), total: source.length };
  };
  const bridge = groupedBridge(original);
  const q = { accounts: ["a", "b", "a"], role: "inbox" };
  const first = await bridge.mailbox.query(q, 0);
  const second = await bridge.mailbox.query(q, 50);
  const third = await bridge.mailbox.query(q, 100);
  const expected = [...a, ...b].sort(
    (x, y) => y.sort_at - x.sort_at || (x.account < y.account ? -1 : 1),
  );
  expect(first.total).toBe(123);
  expect(first.messages).toHaveLength(50);
  expect(second.messages).toHaveLength(50);
  expect(third.messages).toHaveLength(23);
  expect([...first.messages, ...second.messages, ...third.messages]).toEqual(
    expected,
  );
  expect(
    calls.filter((c) => c.query.account === "a" && c.offset === 0),
  ).toHaveLength(1);
  const arbitrary = await bridge.mailbox.query(q, 70);
  expect(arbitrary.messages).toEqual(expected.slice(70, 120));
});

it("never turns an empty group into an unscoped all-mailbox query", async () => {
  const original = new FakeBridge();
  original.mailbox.query = async () => {
    throw new Error("must not call native bridge");
  };
  expect(
    await groupedBridge(original).mailbox.query(
      { accounts: [], role: "inbox" },
      0,
    ),
  ).toEqual({ messages: [], total: 0 });
});

it("keeps unread list scopes and full-text search separate while retaining the group and search return", () => {
  const controller = new MailboxController(new FakeBridge());
  controller.selectGroup("project", "项目", ["a", "b"]);
  controller.setUnreadFilter(true);
  expect(queryOf(controller.state.scope)).toEqual({
    accounts: ["a", "b"],
    role: "inbox",
    seen: false,
  });
  searchMail(controller, "hello");
  expect(queryOf(controller.state.scope)).toMatchObject({
    accounts: ["a", "b"],
    search: true,
    query: "hello",
  });
  expect(queryOf(controller.state.scope)).not.toHaveProperty("role");
  expect(queryOf(controller.state.scope)).not.toHaveProperty("seen");
  controller.exitSearch();
  expect(queryOf(controller.state.scope)).toEqual({
    accounts: ["a", "b"],
    role: "inbox",
    seen: false,
  });
  controller.stop();
});

it("expands group search into supported single-account native queries", async () => {
  const original = new FakeBridge();
  const calls: MailboxQuery[] = [];
  original.mailbox.query = async (query) => {
    calls.push(query);
    return { messages: [], total: 0 };
  };
  await groupedBridge(original).mailbox.query(
    { accounts: ["a", "b"], search: true, query: "needle" },
    0,
  );
  expect(calls).toEqual([
    { account: "a", search: true, query: "needle" },
    { account: "b", search: true, query: "needle" },
  ]);
});

it("rejects cross-account native results instead of mislabelling them", async () => {
  const original = new FakeBridge();
  original.mailbox.query = async () => ({
    messages: [summary("INBOX", "wrong-account")],
    total: 1,
  });
  await expect(
    groupedBridge(original).mailbox.query({ accounts: ["a"] }, 0),
  ).rejects.toThrow("group account mismatch");
});

it("does not return an obsolete group's late results after the list has changed", async () => {
  const original = new FakeBridge();
  const pending = deferred<UiPage>();
  original.mailbox.query = (query) =>
    query.account
      ? pending.promise
      : Promise.resolve({ messages: [], total: 0 });
  const bridge = groupedBridge(original);
  const old = bridge.mailbox
    .query({ accounts: ["a"] }, 0)
    .catch((error: unknown) => error);
  await bridge.mailbox.query({ role: "inbox" }, 0);
  pending.resolve({
    messages: [{ ...summary("INBOX", "old"), account: "a" }],
    total: 1,
  });
  expect(await old).toBeInstanceOf(Error);
});

it("preserves class bridge methods and their receiver", async () => {
  const original = new FakeBridge();
  const bridge = groupedBridge(original);
  await bridge.refresh();
  expect(original.refreshes).toBe(1);
  expect(typeof bridge.mailbox.profiles).toBe("function");
  expect(typeof bridge.mailbox.thread).toBe("function");
});

it("handles 2-account >50-message structured unread+starred search pagination, mark-read/unstar write, refresh, and subsequent pagination without unreadPin drift", async () => {
  const original = new FakeBridge();
  const store = [
    ...Array.from({ length: 35 }, (_, i) => ({
      ...summary("INBOX", `a-${i}`, `Report A${i}`),
      account: "a",
      sort_at: 2000 - i * 2,
      seen: false,
      flagged: true,
    })),
    ...Array.from({ length: 35 }, (_, i) => ({
      ...summary("INBOX", `b-${i}`, `Report B${i}`),
      account: "b",
      sort_at: 1999 - i * 2,
      seen: false,
      flagged: true,
    })),
  ];

  original.mailbox.query = async (query, offset) => {
    const matching = store
      .filter(
        (row) =>
          (!query.account || row.account === query.account) &&
          (query.seen === undefined || row.seen === query.seen) &&
          (query.flagged === undefined || row.flagged === query.flagged),
      )
      .sort((x, y) => y.sort_at - x.sort_at);
    return {
      messages: matching.slice(offset, offset + 50),
      total: matching.length,
    };
  };
  original.mailbox.writeHook = async (req) => {
    if (req.action === "mark" && req.flag && req.value !== undefined) {
      for (const row of store) {
        if (
          row.account === req.account &&
          row.folder === req.folder &&
          req.uids.includes(row.uid)
        ) {
          row[req.flag] = req.value;
        }
      }
    }
    return {
      queued: req.uids.length,
      queued_uids: [...req.uids],
      unchanged_uids: [],
      missing_uids: [],
      mode: null,
    };
  };

  const controller = new MailboxController(groupedBridge(original));
  try {
    controller.acceptSnapshot({
      ...online,
      accounts: [
        { account: "a", state: "online", detail: "" },
        { account: "b", state: "online", detail: "" },
      ],
    });
    for (const req of original.folderRequests) {
      req.d.resolve([{ ...folder("INBOX", "inbox"), account: req.account }]);
    }
    await flush();

    // Structured group search across 2 accounts (70 unread+starred messages).
    controller.search(
      { accounts: ["a", "b"], seen: false, flagged: true },
      { datePreset: "any" },
    );
    await flush();

    expect(controller.state.total).toBe(70);
    expect(controller.state.rows).toHaveLength(50);
    expect(controller.canLoadMore).toBe(true);

    // Load page 2 -> all 70 rows loaded.
    await controller.loadMore();
    expect(controller.state.rows).toHaveLength(70);
    expect(controller.canLoadMore).toBe(false);

    // Select first row and mark it read -> unreadPin must NOT pin inside search scope (!q.search).
    const first = controller.state.rows[0]!;
    controller.selectMessage(first);
    await controller.perform({ action: "mark", flag: "seen", value: true }, [first]);
    await flush();
    expect(controller.state.total).toBe(69);
    expect(controller.state.rows).toHaveLength(69);
    expect(controller.state.rows.some((r) => r.uid === first.uid && r.account === first.account)).toBe(false);

    // Unstar another message from account "b", refresh, and verify total + pagination remain consistent.
    const second = controller.state.rows.find((r) => r.account === "b")!;
    await controller.perform({ action: "mark", flag: "flagged", value: false }, [second]);
    await flush();
    expect(controller.state.total).toBe(68);
    expect(controller.state.rows).toHaveLength(68);

    // Re-entering the same canonical query without meta hits normalized cache and preserves all 68 loaded rows.
    controller.exitSearch();
    await flush();
    controller.search({ accounts: ["a", "b"], seen: false, flagged: true });
    await flush();
    expect(controller.state.rows).toHaveLength(68);
    expect(controller.state.total).toBe(68);
    expect(controller.canLoadMore).toBe(false);

    // A new uncached 2-account structured unread search (69 unread total after 1 mark-read + 1 unstar) paginates cleanly 50 -> 69.
    controller.search({ accounts: ["a", "b"], seen: false });
    await flush();
    expect(controller.state.rows).toHaveLength(50);
    expect(controller.state.total).toBe(69);
    expect(controller.canLoadMore).toBe(true);
    await controller.loadMore();
    expect(controller.state.rows).toHaveLength(69);
    expect(controller.canLoadMore).toBe(false);
  } finally {
    controller.stop();
  }
});
