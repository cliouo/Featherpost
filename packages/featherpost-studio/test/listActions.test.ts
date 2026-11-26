import { afterEach, describe, expect, it, vi } from "vitest";
import type { UiSummary } from "../src/bridge/types";
import { MailboxController, messageKey } from "../src/mailbox/controller";
import { createMessageListActions } from "../src/mailbox/listActions";
import { FakeBridge, deferred, flush, folder, message, online, summary } from "./fakeBridge";

const controllers: MailboxController[] = [];

async function setupMailbox(rows: UiSummary[]) {
  const bridge = new FakeBridge();
  const controller = new MailboxController(bridge);
  controllers.push(controller);
  controller.acceptSnapshot(online);
  for (const req of bridge.folderRequests) {
    req.d.resolve([folder("INBOX", "inbox"), folder("Archive", "archive")]);
  }
  bridge.mailbox.queries[0]!.d.resolve({ messages: rows, total: rows.length });
  await flush();
  return { bridge, controller, actions: createMessageListActions(controller) };
}

afterEach(() => {
  while (controllers.length) {
    controllers.pop()!.stop();
  }
});

describe("mailbox/listActions semantic adapter", () => {
  it("separates opening a local message from bulk checking by messageKey", async () => {
    const rows = [
      summary("INBOX", "1", "Alpha"),
      summary("INBOX", "2", "Beta"),
      summary("INBOX", "3", "Gamma"),
    ];
    const { controller, actions } = await setupMailbox(rows);
    const [k1, k2, k3] = rows.map(messageKey) as [string, string, string];

    // Opening row 2 selects it without affecting checked rows
    const opened = actions.openByKey(k2);
    expect(opened).toMatchObject({ key: k2, source: "local", row: rows[1] });
    expect(actions.isSelected(k2)).toBe(true);
    expect(actions.isChecked(k2)).toBe(false);
    expect(controller.state.checked).toEqual([]);

    // Bulk checking row 1 and Shift-range checking row 3 never changes opened row 2
    expect(actions.setCheckedByKey(k1, true)).toBe(true);
    expect(actions.setCheckedByKey(k3, true, { range: true })).toBe(true);
    expect(actions.isSelected(k2)).toBe(true);
    expect(controller.state.checked.map(messageKey)).toEqual([k1, k2, k3]);

    // Idempotent setCheckedByKey(true) does not uncheck an already checked row
    expect(actions.setCheckedByKey(k2, true)).toBe(true);
    expect(actions.isChecked(k2)).toBe(true);

    // Unchecking with range: true still unchecks that single row rather than re-checking a range
    expect(actions.setCheckedByKey(k2, false, { range: true })).toBe(true);
    expect(actions.isChecked(k2)).toBe(false);
    expect(controller.state.checked.map(messageKey)).toEqual([k1, k3]);

    // toggleCheckByKey toggles individual rows cleanly
    expect(actions.toggleCheckByKey(k2)).toBe(true);
    expect(actions.isChecked(k2)).toBe(true);

    // Clearing open selection preserves bulk-checked rows
    actions.clearOpenSelection();
    expect(actions.isSelected(k2)).toBe(false);
    expect(controller.state.checked.map(messageKey)).toEqual([k1, k3, k2]);

    // Bulk select/clear loaded rows
    actions.setAllLoadedChecked(false);
    expect(controller.state.checked).toEqual([]);
    actions.setAllLoadedChecked(true);
    expect(controller.state.checked.map(messageKey)).toEqual([k1, k2, k3]);
  });

  it("dispatches local vs remote rows consistently by messageKey and refuses remote bulk-checking", async () => {
    const localRow = summary("INBOX", "1", "Local match");
    const remoteRow1 = summary("INBOX", "101", "Server match 1");
    const remoteRow2 = summary("INBOX", "102", "Server match 2");
    const { bridge, controller, actions } = await setupMailbox([localRow]);

    const openRemoteSpy = vi.spyOn(controller, "openRemote");
    const selectSpy = vi.spyOn(controller, "selectMessage");
    bridge.mailbox.serverSearch = async () => [remoteRow1, remoteRow2];
    bridge.mailbox.fetchServerMessage = async (_account, _folder, uid) =>
      message("INBOX", uid);

    controller.search({ query: "match" });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [localRow], total: 1 });
    await flush();
    await controller.searchServer();

    const localKey = messageKey(localRow);
    const remoteKey1 = messageKey(remoteRow1);
    const remoteKey2 = messageKey(remoteRow2);

    expect(actions.resolve(localKey)?.source).toBe("local");
    expect(actions.resolve(remoteKey1)?.source).toBe("remote");
    expect(actions.resolve("missing")).toBeNull();

    // Opening local row calls selectMessage
    expect(actions.openByKey(localKey)?.source).toBe("local");
    expect(selectSpy).toHaveBeenCalledTimes(1);
    expect(openRemoteSpy).not.toHaveBeenCalled();

    // Opening remote row calls openRemote
    expect(actions.openByKey(remoteKey1)?.source).toBe("remote");
    expect(openRemoteSpy).toHaveBeenCalledWith(remoteRow1);
    expect(actions.isSelected(remoteKey1)).toBe(true);

    // Stepping inside remote rows reports moved & boundary accurately
    const moved = actions.step(1);
    expect(moved).toMatchObject({
      previousKey: remoteKey1,
      nextKey: remoteKey2,
      moved: true,
      atBoundary: false,
    });
    const boundary = actions.step(1);
    expect(boundary).toMatchObject({
      previousKey: remoteKey2,
      nextKey: remoteKey2,
      moved: false,
      atBoundary: true,
    });

    // Remote rows cannot be bulk-checked
    expect(actions.toggleCheckByKey(remoteKey1)).toBe(false);
    expect(actions.setCheckedByKey(remoteKey1, true)).toBe(false);
    expect(controller.state.checked).toEqual([]);

    // Unknown keys return null / false safely
    expect(actions.openByKey("unknown-key")).toBeNull();
    expect(actions.toggleCheckByKey("unknown-key")).toBe(false);
  });

  it("refuses check mutations while controller is busy and reports step boundaries on local lists", async () => {
    const rows = [summary("INBOX", "1", "One"), summary("INBOX", "2", "Two")];
    const { bridge, controller, actions } = await setupMailbox(rows);
    const [k1, k2] = rows.map(messageKey) as [string, string];

    expect(actions.step(1)).toMatchObject({
      previousKey: null,
      nextKey: k1,
      moved: true,
      atBoundary: false,
    });
    expect(actions.step(-1)).toMatchObject({
      previousKey: k1,
      nextKey: k1,
      moved: false,
      atBoundary: true,
    });
    expect(actions.step(1)).toMatchObject({
      previousKey: k1,
      nextKey: k2,
      moved: true,
      atBoundary: false,
    });

    // While a batch write is in flight (busy = true), check operations are refused
    const pending = deferred<{ queued: number; mode: null }>();
    bridge.mailbox.writeHook = () => pending.promise;
    const write = controller.perform({ action: "mark", flag: "seen", value: true }, [rows[0]!]);
    expect(controller.state.busy).toBe(true);
    expect(actions.toggleCheckByKey(k1)).toBe(false);
    expect(actions.setCheckedByKey(k1, true)).toBe(false);

    pending.resolve({ queued: 1, mode: null });
    await write;
    expect(controller.state.busy).toBe(false);
    expect(actions.toggleCheckByKey(k1)).toBe(true);
  });
});
