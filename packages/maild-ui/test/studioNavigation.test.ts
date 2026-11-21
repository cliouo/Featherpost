import { afterEach, expect, it } from "vitest";
import { MailboxController } from "../src/mailbox/controller";
import {
  filterUnread,
  searchMail,
  folderTree,
  selectedFolder,
} from "../../featherpost-studio/src/navigation";
import { FakeBridge, online, folder, summary, flush } from "./fakeBridge";

const controllers: MailboxController[] = [];
it("identifies a unified message's real folder without changing its search/list scope", () => {
  const scope = { kind: "search" as const, query: { query: "Account 17" } };
  const row = { account: "qa17", folder: "QA Projects" };
  expect(selectedFolder(scope, "qa17", "QA Projects", row)).toBe(true);
  expect(selectedFolder(scope, "qa17", "INBOX", row)).toBe(false);
  expect(selectedFolder(scope, "other", "QA Projects", row)).toBe(false);
  expect(selectedFolder(scope, "qa17", "QA Projects")).toBe(false);
  expect(
    selectedFolder(scope, "qa17", "INBOX", { ...row, folder: "INBOX" }),
  ).toBe(true);
  expect(scope).toEqual({ kind: "search", query: { query: "Account 17" } });
});
afterEach(() => controllers.splice(0).forEach((c) => c.stop()));
async function ready() {
  const bridge = new FakeBridge();
  const query = bridge.mailbox.query.bind(bridge.mailbox);
  bridge.mailbox.query = (q, offset) => {
    if (
      q.search &&
      (q.role !== undefined || q.seen !== undefined || q.flagged !== undefined)
    )
      throw new Error(
        "Native bridge: full-text search does not accept list filters",
      );
    return query(q, offset);
  };
  const controller = new MailboxController(bridge);
  controllers.push(controller);
  controller.acceptSnapshot(online);
  bridge.folderRequests.forEach((r) =>
    r.d.resolve([folder("INBOX", "inbox", 12), folder("Archive", "archive")]),
  );
  bridge.mailbox.queries[0]!.d.resolve({
    messages: [summary("INBOX", "read")],
    total: 1,
  });
  await flush();
  return { bridge, controller };
}

it("asks the daemon for unread mail in the selected folder instead of filtering the loaded page", async () => {
  const { bridge, controller: c } = await ready();
  c.selectFolder(c.foldersOf("demo")[0]!);
  bridge.pageRequests
    .at(-1)!
    .d.resolve({ messages: [summary("INBOX", "read")], total: 1 });
  await flush();
  filterUnread(c, true);
  const request = bridge.mailbox.queries.at(-1)!;
  expect(request.query).toMatchObject({
    account: "demo",
    folder: "INBOX",
    seen: false,
  });
  expect(request.offset).toBe(0);
  expect(request.query.search).not.toBe(true);
  request.d.resolve({
    messages: [{ ...summary("INBOX", "older-unread"), seen: false }],
    total: 1,
  });
  await flush();
  expect(c.state.rows.map((r) => r.uid)).toEqual(["older-unread"]);
  filterUnread(c, false);
  expect(c.state.scope).toMatchObject({
    kind: "folder",
    account: "demo",
    folder: "INBOX",
  });
  expect(bridge.pageRequests.at(-1)!.folder).toEqual({
    account: "demo",
    folder: "INBOX",
  });
});

it("uses the supported full-text protocol and restores the folder's unread lens after search", async () => {
  const { bridge, controller: c } = await ready();
  c.selectFolder(c.foldersOf("demo")[1]!);
  filterUnread(c, true);
  const source = c.state.scope;
  searchMail(c, "roadmap", false, source);
  expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
    account: "demo",
    folder: "Archive",
    query: "roadmap",
  });
  expect(bridge.mailbox.queries.at(-1)!.query.seen).toBeUndefined();
  expect(bridge.mailbox.queries.at(-1)!.query.search).toBe(true);
  // Search results do not advertise an unsupported combined unread/text filter.
  const requests = bridge.mailbox.queries.length;
  filterUnread(c, false);
  expect(bridge.mailbox.queries).toHaveLength(requests);
  searchMail(c, "");
  expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
    account: "demo",
    folder: "Archive",
    seen: false,
  });
  expect(bridge.mailbox.queries.at(-1)!.query.query).toBeUndefined();
  expect(bridge.mailbox.queries.at(-1)!.query.search).not.toBe(true);
  filterUnread(c, false);
  expect(c.state.scope).toMatchObject({
    kind: "folder",
    account: "demo",
    folder: "Archive",
  });
});

it("can search all mail and return to the original folder scope without changing accounts", async () => {
  const { bridge, controller: c } = await ready();
  c.selectFolder(c.foldersOf("demo")[1]!);
  const source = c.state.scope;
  searchMail(c, "invoice", true, source);
  expect(bridge.mailbox.queries.at(-1)!.query.account).toBeUndefined();
  expect(bridge.mailbox.queries.at(-1)!.query.folder).toBeUndefined();
  searchMail(c, "invoice", false, source);
  expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
    account: "demo",
    folder: "Archive",
    query: "invoice",
  });
});

it("keeps unified unread results limited to inbox folders", async () => {
  const { bridge, controller: c } = await ready();
  filterUnread(c, true);
  expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
    role: "inbox",
    seen: false,
  });
  filterUnread(c, false);
  expect(c.state.scope).toEqual({ kind: "smart", name: "inbox" });
  searchMail(c, "invoice");
  expect(bridge.mailbox.queries.at(-1)!.query).toMatchObject({
    query: "invoice",
    search: true,
  });
  expect(bridge.mailbox.queries.at(-1)!.query.role).toBeUndefined();
});

it("keeps real nested and non-selectable folders without inventing or mixing account paths", () => {
  const folders = [
    folder("Projects/Featherpost/Reviews"),
    { ...folder("Projects"), present: false },
    folder("Projects/Featherpost"),
    folder("INBOX", "inbox"),
    { ...folder("Projects"), account: "other" },
    folder("Missing/Parent/Child"),
  ];
  const tree = folderTree(folders);
  expect(tree[0]!.folder.name).toBe("INBOX");
  const projects = tree.find(
    (n) => n.folder.name === "Projects" && n.folder.account === "demo",
  )!;
  expect(projects.folder.present).toBe(false);
  expect(projects.children[0]!.children[0]!.folder.name).toBe(
    "Projects/Featherpost/Reviews",
  );
  expect(tree.find((n) => n.folder.account === "other")!.children).toEqual([]);
  expect(tree.some((n) => n.folder.name === "Missing/Parent/Child")).toBe(true);
  expect(folders).toHaveLength(6);
  expect(
    selectedFolder(
      {
        kind: "search",
        query: { account: "demo", folder: "Projects/Featherpost", seen: false },
      },
      "demo",
      "Projects/Featherpost",
    ),
  ).toBe(true);
  expect(
    selectedFolder(
      {
        kind: "search",
        query: { account: "other", folder: "Projects/Featherpost" },
      },
      "demo",
      "Projects/Featherpost",
    ),
  ).toBe(false);
});
