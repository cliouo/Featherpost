import {followMailIntent} from '../src/mailbox/navigation';
import { afterEach, describe, expect, it, vi } from "vitest";
import { MailboxController, messageKey } from "../src/mailbox/controller";
import { ActivityController } from "../src/mailbox/activityController";
import {
  FakeBridge,
  online,
  summary,
  folder,
  message,
  flush,
  deferred,
} from "./fakeBridge";
import { BridgeError } from "../src/bridge/errors";
import type { UiSummary } from "../src/bridge/types";
import type { Operation, Receipt } from "../src/mailbox/types";
const controllers: Array<{ stop: () => void }> = [];
async function ready(rows: UiSummary[] = []) {
  const bridge = new FakeBridge();
  const c = new MailboxController(bridge);
  controllers.push(c);
  c.acceptSnapshot({
    ...online,
    accounts: [
      ...online.accounts,
      { account: "other", state: "offline", detail: "" },
    ],
  });
  for (const r of bridge.folderRequests)
    r.d.resolve(
      ["INBOX", "Archive", "Sent"].map((name, i) => ({
        ...folder(name, ["inbox", "archive", "sent"][i]),
        account: r.account,
      })),
    );
  bridge.mailbox.queries[0].d.resolve({ messages: rows, total: rows.length });
  await flush();
  return { bridge, c };
}
afterEach(() => {
  controllers.splice(0).forEach((c) => c.stop());
  vi.useRealTimers();
});
describe("08-B mailbox workspace", () => {
  describe("mark as read after opening", () => {
    const unread = (uid: string) => ({ ...summary("INBOX", uid), seen: false });
    const detail = (uid: string) => message("INBOX", uid, { seen: false });

    it("starts the dwell when the detail opens and updates list and detail before the receipt", async () => {
      const { bridge, c } = await ready([unread("1")]);
      vi.useFakeTimers();
      const receipt = deferred<Receipt>();
      bridge.mailbox.writeHook = () => receipt.promise;
      c.selectMessage(c.state.rows[0]);
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes).toEqual([]);
      bridge.detailRequests[0].d.resolve(detail("1"));
      await vi.advanceTimersByTimeAsync(1499);
      expect(c.state.detail?.seen).toBe(false);
      expect(bridge.mailbox.writes).toEqual([]);
      await vi.advanceTimersByTimeAsync(1);
      expect(bridge.mailbox.writes).toEqual([
        { action: "mark", flag: "seen", value: true, account: "demo", folder: "INBOX", uids: ["1"] },
      ]);
      expect(c.state.rows[0].seen).toBe(true);
      expect(c.state.selectedRow?.seen).toBe(true);
      expect(c.state.detail?.seen).toBe(true);
      receipt.resolve({ queued: 0, queued_uids: [], unchanged_uids: ["1"], missing_uids: [], mode: null });
      await vi.advanceTimersByTimeAsync(0);
      expect(c.state.detail?.seen).toBe(true);
      expect(c.state.report).toBeNull();
    });

    it("leaves messages skimmed with the keyboard unread, including a late detail response", async () => {
      const { bridge, c } = await ready(["1", "2", "3"].map(unread));
      vi.useFakeTimers();
      c.selectMessage(c.state.rows[0]);
      c.step(1);
      bridge.detailRequests[1].d.resolve(detail("2"));
      await vi.advanceTimersByTimeAsync(1000);
      c.step(1);
      bridge.detailRequests[0].d.resolve(detail("1"));
      bridge.detailRequests[2].d.resolve(detail("3"));
      await vi.advanceTimersByTimeAsync(1499);
      expect(bridge.mailbox.writes).toEqual([]);
      await vi.advanceTimersByTimeAsync(1);
      expect(bridge.mailbox.writes.map((write) => write.uids)).toEqual([["3"]]);
      expect(c.state.rows.map((row) => row.seen)).toEqual([false, false, true]);
    });

    it.each([
      ["closing the message", (c: MailboxController) => c.clearSelection()],
      ["leaving the mailbox view", (c: MailboxController) => c.invalidateNavigation()],
      ["disposing the controller", (c: MailboxController) => c.stop()],
    ] as const)("cancels the dwell when %s", async (_label, cancel) => {
      const { bridge, c } = await ready([unread("1")]);
      vi.useFakeTimers();
      c.selectMessage(c.state.rows[0]);
      bridge.detailRequests[0].d.resolve(detail("1"));
      await vi.advanceTimersByTimeAsync(1000);
      cancel(c);
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes).toEqual([]);
      expect(c.state.rows[0].seen).toBe(false);
    });

    it("skips already-read detail and passive detail reloads", async () => {
      const { bridge, c } = await ready([unread("1")]);
      vi.useFakeTimers();
      c.selectMessage(c.state.rows[0]);
      bridge.detailRequests[0].d.resolve(message("INBOX", "1"));
      await vi.advanceTimersByTimeAsync(1500);
      c.reloadSelected();
      bridge.detailRequests[1].d.resolve(detail("1"));
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes).toEqual([]);
      expect(c.state.detail?.seen).toBe(false);
    });

    it("marks a remote result only after the local fetch succeeds and it stays open", async () => {
      const { bridge, c } = await ready();
      const fetch = deferred<ReturnType<typeof detail> | null>();
      bridge.mailbox.serverSearch = async () => [unread("1"), unread("2")];
      bridge.mailbox.fetchServerMessage = async (_account, _folder, uid) =>
        uid === "1" ? null : fetch.promise;
      c.search({ query: "needle" });
      expect(bridge.mailbox.queries.at(-1)!.query).toEqual({ search: true, query: "needle" });
      bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
      await c.searchServer();
      vi.useFakeTimers();
      c.openRemote(c.state.remote.rows[0]);
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes).toEqual([]);
      c.openRemote(c.state.remote.rows[1]);
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes).toEqual([]);
      fetch.resolve(detail("2"));
      await vi.advanceTimersByTimeAsync(1500);
      expect(bridge.mailbox.writes.map((write) => write.uids)).toEqual([["2"]]);
      expect(c.state.remote.rows.map((row) => row.seen)).toEqual([false, true]);
    });

    it("rolls back optimistic read state if the write is unconfirmed", async () => {
      const { bridge, c } = await ready([unread("1")]);
      vi.useFakeTimers();
      const receipt = deferred<Receipt>();
      bridge.mailbox.writeHook = () => receipt.promise;
      c.selectMessage(c.state.rows[0]);
      bridge.detailRequests[0].d.resolve(detail("1"));
      await vi.advanceTimersByTimeAsync(1500);
      expect(c.state.detail?.seen).toBe(true);
      receipt.resolve({ queued: 0, queued_uids: [], unchanged_uids: [], missing_uids: ["1"], mode: null });
      await vi.advanceTimersByTimeAsync(0);
      expect(c.state.rows[0].seen).toBe(false);
      expect(c.state.selectedRow?.seen).toBe(false);
      expect(c.state.detail?.seen).toBe(false);
      expect(c.state.detailError).toBeNull();
      expect(c.state.report).toMatchObject({ requested: 1, unconfirmed: 1, error: expect.any(String) });
    });
  });

  it("starts with one daemon-paged inbox query and preserves equal UIDs across accounts", async () => {
    const { bridge, c } = await ready([
      summary("INBOX", "1"),
      { ...summary("INBOX", "1"), account: "other" },
    ]);
    expect(bridge.mailbox.queries[0].query).toEqual({ role: "inbox" });
    expect(c.state.rows).toHaveLength(2);
    c.selectMessage(c.state.rows[1]);
    expect(bridge.detailRequests[0].identity.account).toBe("other");
  });
  it("late unified results never enter the next folder", async () => {
    const { bridge, c } = await ready();
    c.selectSmart("starred");
    const old = bridge.mailbox.queries.at(-1)!;
    c.selectFolder(c.foldersOf("demo")[1]);
    old.d.resolve({ messages: [summary("INBOX", "1")], total: 1 });
    await flush();
    expect(c.state.rows).toEqual([]);
    expect(c.state.scope.kind).toBe("folder");
  });
  it("selects loaded rows only and supports a shift range with full identities", async () => {
    const { c } = await ready(["1", "2", "3"].map((i) => summary("INBOX", i)));
    c.toggle(c.state.rows[0]);
    c.toggle(c.state.rows[2], true);
    expect(c.state.checked).toHaveLength(3);
    c.selectLoaded(false);
    expect(c.state.checked).toEqual([]);
    c.selectLoaded(true);
    expect(c.state.checked).toHaveLength(3);
  });
  it("groups writes by account and folder, and keeps the failed group selected", async () => {
    const rows = [
      summary("INBOX", "1"),
      summary("Sent", "1"),
      { ...summary("INBOX", "1"), account: "other" },
    ];
    const { bridge, c } = await ready(rows);
    c.selectLoaded(true);
    bridge.mailbox.writeHook = async (r) => {
      if (r.account === "other")
        throw new BridgeError({ code: "failed", message: "refused" });
      return { queued: r.uids.length, mode: null };
    };
    const report = await c.perform({
      action: "mark",
      flag: "seen",
      value: true,
    });
    expect(
      bridge.mailbox.writes.map((r) => [r.account, r.folder, r.uids]),
    ).toEqual([
      ["demo", "INBOX", ["1"]],
      ["demo", "Sent", ["1"]],
      ["other", "INBOX", ["1"]],
    ]);
    expect(report?.queued).toBe(2);
    expect(report?.unconfirmed).toBe(1);
    expect(c.state.checked.map(messageKey)).toEqual([messageKey(rows[2])]);
  });
  it("a partial queue receipt is not reported as complete success", async () => {
    const { bridge, c } = await ready(
      ["1", "2"].map((i) => summary("INBOX", i)),
    );
    c.selectLoaded(true);
    bridge.mailbox.writeHook = async () => ({ queued: 1, mode: "move" });
    const result = await c.perform({ action: "move", target: "Archive" });
    expect(result).toMatchObject({ queued: 1, unconfirmed: 1 });
    expect(result?.error).toMatch(/刷新/);
    expect(c.state.checked).toHaveLength(2);
  });
  it("freezes the click target and blocks double submissions during navigation", async () => {
    const { bridge, c } = await ready([summary("INBOX", "1")]);
    c.selectLoaded(true);
    const pending = deferred<Receipt>();
    bridge.mailbox.writeHook = () => pending.promise;
    const action = c.perform({ action: "mark", flag: "seen", value: true });
    await c.perform({ action: "delete" });
    c.selectSmart("starred");
    pending.resolve({ queued: 1, mode: null });
    await action;
    expect(bridge.mailbox.writes).toHaveLength(1);
    expect(bridge.mailbox.writes[0].uids).toEqual(["1"]);
    expect(c.state.checked).toEqual([]);
    expect(c.state.scope).toEqual({ kind: "smart", name: "starred" });
  });
  it("does not continue remaining write groups after a connection epoch change", async () => {
    const { bridge, c } = await ready([
      summary("INBOX", "1"),
      { ...summary("INBOX", "1"), account: "other" },
    ]);
    c.selectLoaded(true);
    bridge.mailbox.writeHook = async () => {
      c.acceptSnapshot({
        ...c.state.snapshot,
        revision: 9,
        phase: "retry_waiting",
      });
      return { queued: 1, mode: null };
    };
    const report = await c.perform({
      action: "mark",
      flag: "seen",
      value: true,
    });
    expect(bridge.mailbox.writes).toHaveLength(1);
    expect(report?.unconfirmed).toBe(1);
  });
  it("splits each folder into at most 500 UID writes", async () => {
    const { bridge, c } = await ready();
    await c.perform(
      { action: "mark", flag: "seen", value: true },
      Array.from({ length: 531 }, (_, i) => summary("INBOX", String(i))),
    );
    expect(bridge.mailbox.writes.map((w) => w.uids.length)).toEqual([500, 31]);
  });
  it("uses the actual archive role and refuses cross-account moves", async () => {
    const { bridge, c } = await ready([
      summary("INBOX", "1"),
      { ...summary("INBOX", "1"), account: "other" },
    ]);
    c.selectLoaded(true);
    await c.perform({ action: "move", target: "Archive" });
    expect(bridge.mailbox.writes).toHaveLength(0);
    await c.perform({ action: "archive" }, [c.state.rows[0]]);
    expect(bridge.mailbox.writes[0].target).toBe("Archive");
  });
  it("keeps search filters in the kernel query and restores the previous folder", async () => {
    const { bridge, c } = await ready();
    c.selectFolder(c.foldersOf("demo")[1]);
    bridge.pageRequests
      .at(-1)!
      .d.resolve({ messages: [summary("Archive", "9")], total: 1 });
    await flush();
    c.selectMessage(c.state.rows[0]);
    c.search({
      query: "印刷",
      account: "demo",
      from: "print",
      has_attachments: true,
    });
    expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
      search: true,
      query: "印刷",
      account: "demo",
      from: "print",
      has_attachments: true,
    });
    c.exitSearch();
    expect(c.state.scope).toMatchObject({ kind: "folder", folder: "Archive" });
    expect(c.state.selected?.uid).toBe("9");
  });
  it("search coverage reflects incomplete indexing", async () => {
    const { bridge, c } = await ready();
    bridge.mailbox.indexStatus = { pending: 82, done: false };
    c.search({ query: "发票" });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await flush();
    expect(c.state.index).toEqual({ pending: 82, done: false });
  });
  it("thread selection keeps cross-folder identity without injecting it into list results", async () => {
    const row = { ...summary("INBOX", "1"), thread_id: 5 };
    const { bridge, c } = await ready([row]);
    c.selectMessage(row);
    bridge.detailRequests
      .at(-1)!
      .d.resolve(message("INBOX", "1", { thread_id: 5 }));
    await flush();
    const opening = c.openThread();
    bridge.mailbox.threads[0].d.resolve({
      thread_id: 5,
      messages: [row, { ...summary("Sent", "1"), thread_id: 5 }],
      total: 2,
    });
    await opening;
    c.selectMessage(c.state.threadRows[1], true);
    expect(c.state.selected?.folder).toBe("Sent");
    expect(c.state.rows).toEqual([row]);
    expect(c.state.threadOpen).toBe(true);
    expect(bridge.detailRequests.at(-1)!.identity.folder).toBe("Sent");
  });
  it("switching messages invalidates an old thread page", async () => {
    const { bridge, c } = await ready([
      summary("INBOX", "1"),
      summary("INBOX", "2"),
    ]);
    c.selectMessage(c.state.rows[0]);
    const pending = c.openThread();
    c.selectMessage(c.state.rows[1]);
    bridge.mailbox.threads[0].d.resolve({
      thread_id: 1,
      messages: [summary("Sent", "1")],
      total: 1,
    });
    await pending;
    expect(c.state.threadRows).toEqual([]);
    expect(c.state.selected?.uid).toBe("2");
  });
  it("refreshing a paginated list does not erase failed selections on page two", async () => {
    const { bridge, c } = await ready(
      Array.from({ length: 50 }, (_, i) => summary("INBOX", String(i))),
    );
    c.store.patch({ total: 51 });
    const more = c.loadMore();
    bridge.mailbox.queries
      .at(-1)!
      .d.resolve({ messages: [summary("INBOX", "50")], total: 51 });
    await more;
    c.toggle(c.state.rows[50]);
    const refreshing = c.refresh();
    const head = bridge.mailbox.queries.at(-1)!;
    head.d.resolve({
      messages: Array.from({ length: 50 }, (_, i) =>
        summary("INBOX", String(i)),
      ),
      total: 51,
    });
    await flush();
    bridge.mailbox.queries
      .at(-1)!
      .d.resolve({ messages: [summary("INBOX", "50")], total: 51 });
    for (const r of bridge.folderRequests.slice(2)) r.d.resolve([]);
    await refreshing;
    expect(c.state.checked[0].uid).toBe("50");
    expect(c.state.rows).toHaveLength(51);
  });
});
const op = (id: number, state = "failed"): Operation => ({
  id,
  account: "demo",
  folder: "Sent",
  uid: "1",
  kind: "append",
  state,
  attempts: 1,
  created_at: 1,
  updated_at: 2,
  detail: "server refused",
  needs_decision: true,
  target: null,
  flag: null,
  value: null,
});
describe("08-B activity", () => {
  it("retains settled failures and uses ID pagination", async () => {
    const bridge = new FakeBridge(),
      c = new ActivityController(bridge.mailbox);
    controllers.push({ stop: () => c.hide() });
    c.setConnection(true, 1);
    c.show();
    bridge.mailbox.activities[0].d.resolve({
      ops: [op(20, "done"), op(19)],
      has_more: true,
    });
    await flush();
    expect(c.store.getSnapshot().ops[1].state).toBe("failed");
    const more = c.refresh(true);
    expect(bridge.mailbox.activities.at(-1)!.before).toBe(19);
    bridge.mailbox.activities
      .at(-1)!
      .d.resolve({ ops: [op(18, "unknown")], has_more: false });
    await more;
    expect(c.store.getSnapshot().ops).toHaveLength(3);
  });
  it("hiding activity invalidates old responses and stops polling", async () => {
    vi.useFakeTimers();
    const bridge = new FakeBridge(),
      c = new ActivityController(bridge.mailbox);
    controllers.push({ stop: () => c.hide() });
    c.setConnection(true, 1);
    c.show();
    c.hide();
    bridge.mailbox.activities[0].d.resolve({ ops: [op(1)], has_more: false });
    await vi.advanceTimersByTimeAsync(15000);
    expect(c.store.getSnapshot().ops).toEqual([]);
    expect(bridge.mailbox.activities).toHaveLength(1);
  });
  it("unknown future operation kinds cannot be replayed", async () => {
    const bridge = new FakeBridge(),
      c = new ActivityController(bridge.mailbox);
    c.setConnection(true, 1);
    await c.resolve({ ...op(1), kind: "future-delete-all" }, "retry");
    expect(bridge.mailbox.resolutions).toHaveLength(0);
  });
});

it("distinguishes already-satisfied flags from newly queued writes", async () => {
  const { bridge, c } = await ready([
    summary("INBOX", "1"),
    summary("INBOX", "2"),
  ]);
  c.selectLoaded(true);
  bridge.mailbox.writeHook = async () => ({
    queued: 1,
    mode: null,
    queued_uids: ["2"],
    unchanged_uids: ["1"],
    missing_uids: [],
  });
  const report = await c.perform({ action: "mark", flag: "seen", value: true });
  expect(report).toMatchObject({
    queued: 1,
    unchanged: 1,
    unconfirmed: 0,
    error: null,
  });
  expect(c.state.checked).toEqual([]);
});
it("single-row starring preserves unrelated checked messages", async () => {
  const { c } = await ready(["1", "2", "3"].map((i) => summary("INBOX", i)));
  c.toggle(c.state.rows[0]);
  c.toggle(c.state.rows[1]);
  await c.perform({ action: "mark", flag: "flagged", value: true }, [
    c.state.rows[2],
  ]);
  expect(c.state.checked.map((r) => r.uid)).toEqual(["1", "2"]);
});
it("a late notification cannot replace a later manual reading selection", async () => {
  const { bridge, c } = await ready(["1", "2"].map((i) => summary("INBOX", i)));
  const pending = deferred<ReturnType<typeof message>>();
  bridge.mailbox.locate = () => pending.promise;
  const locating = c.locate("demo", "1", "INBOX");
  c.selectMessage(c.state.rows[1]);
  pending.resolve(message("INBOX", "1"));
  await locating;
  expect(c.state.selected?.uid).toBe("2");
});
it("legacy notifications without folder identity never guess a matching UID", async () => {
  const { bridge, c } = await ready([summary("INBOX", "1")]);
  const locate = vi.spyOn(bridge.mailbox, "locate");
  await c.locate("demo", "1");
  expect(locate).not.toHaveBeenCalled();
  expect(c.state.selected).toBeNull();
  expect(c.state.rowsError).toMatch(/文件夹信息/);
});
it("reopening a thread during its old request creates a fresh load", async () => {
  const { bridge, c } = await ready([summary("INBOX", "1")]);
  c.selectMessage(c.state.rows[0]);
  const first = c.openThread();
  await c.openThread();
  const next = c.openThread();
  expect(bridge.mailbox.threads).toHaveLength(2);
  bridge.mailbox.threads[0].d.resolve({
    thread_id: 1,
    messages: [summary("INBOX", "old")],
    total: 1,
  });
  await first;
  expect(c.state.threadRows).toEqual([]);
  bridge.mailbox.threads[1].d.resolve({
    thread_id: 1,
    messages: [summary("INBOX", "1")],
    total: 1,
  });
  await next;
  expect(c.state.threadLoading).toBe(false);
  expect(c.state.threadRows[0].uid).toBe("1");
});
it("thread membership changes cannot silently skip messages at an offset", async () => {
  const { bridge, c } = await ready([summary("INBOX", "1")]);
  c.selectMessage(c.state.rows[0]);
  const first = c.openThread();
  bridge.mailbox.threads[0].d.resolve({
    thread_id: 1,
    messages: [summary("INBOX", "1")],
    total: 3,
  });
  await first;
  const next = c.moreThread();
  bridge.mailbox.threads[1].d.resolve({
    thread_id: 1,
    messages: [summary("INBOX", "3")],
    total: 2,
  });
  await next;
  expect(c.state.threadError).toBeTruthy();
  expect(c.state.threadRows.map((r) => r.uid)).toEqual(["1"]);
  expect(c.state.threadOffset).toBe(1);
});
it("exiting search restores a thread message outside the current list even after a write clears caches", async () => {
  const { bridge, c } = await ready([summary("INBOX", "1")]);
  c.selectMessage(c.state.rows[0]);
  const first = c.openThread();
  bridge.mailbox.threads[0].d.resolve({
    thread_id: 1,
    messages: [summary("INBOX", "1"), summary("Sent", "9")],
    total: 2,
  });
  await first;
  c.selectMessage(c.state.threadRows[1], true);
  c.search({ query: "new" });
  bridge.mailbox.queries
    .at(-1)!
    .d.resolve({ messages: [summary("INBOX", "4")], total: 1 });
  await flush();
  await c.perform({ action: "mark", flag: "seen", value: true }, [
    c.state.rows[0],
  ]);
  c.exitSearch();
  expect(c.state.selected).toEqual({
    account: "demo",
    folder: "Sent",
    uid: "9",
  });
  expect(bridge.detailRequests.at(-1)!.identity.folder).toBe("Sent");
});
it("a reconnect retries an interrupted selected-message read", async () => {
  const { bridge, c } = await ready([summary("INBOX", "1")]);
  c.selectMessage(c.state.rows[0]);
  const old = bridge.detailRequests[0];
  c.acceptSnapshot({
    ...c.state.snapshot,
    phase: "retry_waiting",
    revision: 3,
  });
  c.acceptSnapshot({
    ...c.state.snapshot,
    phase: "online",
    revision: 4,
    connection_epoch: 2,
  });
  for (const r of bridge.folderRequests.slice(2))
    r.d.resolve([folder("INBOX", "inbox")]);
  bridge.mailbox.queries
    .at(-1)!
    .d.resolve({ messages: [summary("INBOX", "1")], total: 1 });
  await flush();
  expect(bridge.detailRequests).toHaveLength(2);
  old.d.resolve(message("INBOX", "1", { body: "stale" }));
  bridge.detailRequests[1].d.resolve(
    message("INBOX", "1", { body: "current" }),
  );
  await flush();
  expect(c.state.detail?.body).toBe("current");
});
it("history pagination is not erased by activity polling", async () => {
  vi.useFakeTimers();
  const bridge = new FakeBridge(),
    c = new ActivityController(bridge.mailbox);
  controllers.push({ stop: () => c.hide() });
  c.setConnection(true, 1);
  c.show();
  bridge.mailbox.activities[0].d.resolve({
    ops: Array.from({ length: 40 }, (_, i) => op(100 - i)),
    has_more: true,
  });
  await vi.advanceTimersByTimeAsync(0);
  const more = c.refresh(true);
  bridge.mailbox.activities[1].d.resolve({ ops: [op(59)], has_more: true });
  await more;
  await vi.advanceTimersByTimeAsync(10000);
  expect(c.store.getSnapshot().ops).toHaveLength(41);
  expect(bridge.mailbox.activities).toHaveLength(2);
});


it('manual navigation also wins while a notification waits for draft persistence',async()=>{
 const {bridge,c}=await ready(['1','2'].map(i=>summary('INBOX',i)));const saved=deferred<void>();const open=vi.fn(),locate=vi.spyOn(bridge.mailbox,'locate');
 const intent=followMailIntent(c,{route:'message',account:'demo',folder:'INBOX',uid:'1'},()=>saved.promise,open,()=>{});
 c.selectMessage(c.state.rows[1]);saved.resolve();await intent;expect(open).not.toHaveBeenCalled();expect(locate).not.toHaveBeenCalled();expect(c.state.selected?.uid).toBe('2');
});
it('background arrivals preserve the current HTML document and reading position',async()=>{
 const {bridge,c}=await ready([summary('INBOX','1')]);c.selectMessage(c.state.rows[0]);const detail=message('INBOX','1',{has_html:true});bridge.detailRequests[0].d.resolve(detail);await flush();vi.useFakeTimers();
 c.acceptSnapshot({...c.state.snapshot,revision:8});await vi.advanceTimersByTimeAsync(250);
 for(const r of bridge.folderRequests.slice(2))r.d.resolve([folder('INBOX','inbox')]);bridge.mailbox.queries.at(-1)!.d.resolve({messages:[summary('INBOX','new'),summary('INBOX','1')],total:2});await vi.advanceTimersByTimeAsync(0);
 expect(bridge.detailRequests).toHaveLength(1);expect(c.state.detail).toBe(detail);
});

it('a missing archive mapping gives a specific recovery instruction without a write',async()=>{
 const {bridge,c}=await ready([{...summary('INBOX','1'),account:'other'}]);const refresh=c.ensureFolders('other',true);bridge.folderRequests.at(-1)!.d.resolve([{...folder('INBOX','inbox'),account:'other'}]);await refresh;c.selectLoaded(true);const report=await c.perform({action:'archive'});expect(bridge.mailbox.writes).toHaveLength(0);expect(report?.error).toMatch(/没有可用归档文件夹/);
});

it('a notification waits for settings navigation approval before locating its message',async()=>{
 const {bridge,c}=await ready([summary('INBOX','1')]);const located=deferred<import('../src/bridge/types').UiMessage>();const locate=vi.spyOn(bridge.mailbox,'locate').mockImplementation(()=>located.promise);const approval=deferred<boolean>(),open=vi.fn(()=>{c.invalidateNavigation();return true;});
 const p=followMailIntent(c,{route:'message',account:'demo',folder:'Sent',uid:'9'},async()=>{},open,()=>{},()=>approval.promise);
 await flush();expect(locate).not.toHaveBeenCalled();
 approval.resolve(true);await flush();expect(open).toHaveBeenCalledOnce();
 located.resolve(message('Sent','9'));await p;expect(c.state.selected?.folder).toBe('Sent');expect(c.state.selected?.uid).toBe('9');
});
