import { Host } from "../src/host/host";
import { composeModule } from "../src/compose/module";
import { FakeBridge } from "./fakeBridge";
import { applyEffectiveLocales } from "@featherpost/i18n";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ComposeController, sourceKey } from "../src/compose/controller";
import { FakeCompose } from "./fakeCompose";
import { deferred } from "./fakeBridge";
import { BridgeError } from "../src/bridge/errors";
import type { Draft } from "../src/compose/types";
const controllers: ComposeController[] = [];
async function setup() {
  const bridge = new FakeCompose();
  const controller = new ComposeController(
    bridge,
    () => {},
    () => {},
  );
  controllers.push(controller);
  controller.setOnline(true);
  await controller.refresh();
  await controller.create();
  return { bridge, controller };
}
afterEach(() => {
  controllers.splice(0).forEach((c) => c.stop());
  vi.useRealTimers();
});
describe("compose", () => {
  it("debounces and waits for the durable save receipt", async () => {
    vi.useFakeTimers();
    const { bridge, controller: c } = await setup();
    c.change("body", "一");
    c.change("body", "二");
    await vi.advanceTimersByTimeAsync(499);
    expect(bridge.saves).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(bridge.saves).toHaveLength(1);
    expect(c.store.getSnapshot().savedRevision).toBe(2);
  });
  it("does not overwrite new typing with an older save response", async () => {
    const { bridge, controller: c } = await setup();
    const pending = deferred<Draft>();
    const original = await bridge.draft("draft-1");
    bridge.saveHook = () => pending.promise;
    c.change("body", "first");
    const flushing = c.flush();
    await vi.waitFor(() => expect(bridge.saves).toHaveLength(1));
    c.change("body", "second");
    bridge.saveHook = null;
    bridge.rows.set(original.id, { ...original, body: "first", version: 2 });
    pending.resolve({ ...original, body: "first", version: 2 });
    await flushing;
    expect(c.store.getSnapshot().fields?.body).toBe("second");
    expect(bridge.saves.map((v) => v.version)).toEqual([1, 2]);
    expect(c.store.getSnapshot().savedRevision).toBe(
      c.store.getSnapshot().revision,
    );
  });
  it("retains local text and stops automatic overwrites on a version conflict", async () => {
    const { bridge, controller: c } = await setup();
    bridge.rows.get("draft-1")!.version = 2;
    c.change("body", "my private text");
    await expect(c.flush()).rejects.toBeTruthy();
    expect(c.store.getSnapshot().conflict).toBe(true);
    await expect(c.flush()).rejects.toBeTruthy();
    expect(bridge.saves).toHaveLength(1);
    expect(c.store.getSnapshot().fields?.body).toBe("my private text");
  });
  it("does not close before saving, and keeps the window on failure", async () => {
    const { bridge, controller: c } = await setup();
    c.change("body", "not yet durable");
    const pending = deferred<Draft>();
    bridge.saveHook = () => pending.promise;
    const closing = c.closeWindow();
    await Promise.resolve();
    expect(bridge.closed).toBe(false);
    pending.reject(new Error("disk full"));
    await closing;
    expect(bridge.closed).toBe(false);
    expect(c.store.getSnapshot().closing).toBe(false);
    expect(c.store.getSnapshot().fields?.body).toBe("not yet durable");
  });
  it("keeps reply drafts scoped to account, folder, uid and mode", async () => {
    const { bridge, controller: c } = await setup();
    const a = { account: "demo", folder: "INBOX", uid: "1" },
      b = { ...a, folder: "Sent" };
    await c.reply(a, "reply");
    const id = c.store.getSnapshot().draft!.id;
    c.change("body", "reply to inbox");
    await c.reply(b, "reply");
    expect(c.store.getSnapshot().source).toBe(sourceKey(b));
    expect(c.store.getSnapshot().fields?.body).not.toBe("reply to inbox");
    await c.reply(a, "reply");
    expect(c.store.getSnapshot().draft!.id).toBe(id);
    expect(c.store.getSnapshot().fields?.body).toBe("reply to inbox");
    expect(bridge.replyCalls).toHaveLength(2);
  });
  it("asks for the reply quote in the current interface language (ADR-0048 decision 7)", async () => {
    const { bridge, controller: c } = await setup();
    await c.reply({ account: "demo", folder: "INBOX", uid: "1" }, "reply");
    applyEffectiveLocales(["en-US"]);
    await c.reply({ account: "demo", folder: "INBOX", uid: "2" }, "forward");
    expect(bridge.replyLocales).toEqual(["zh-CN", "en"]);
  });
  it("freezes repeated send clicks and persists accepted delivery", async () => {
    const { bridge, controller: c } = await setup();
    c.change("to", "person@example.test");
    c.change("subject", "test");
    const pending = deferred<Draft>();
    bridge.sendHook = () => pending.promise;
    const sending = c.send();
    await c.send();
    await vi.waitFor(() => expect(bridge.sends).toBe(1));
    pending.resolve({
      ...(await bridge.draft("draft-1")),
      version: 4,
      send_state: "sent",
    });
    await sending;
    await c.send();
    expect(bridge.sends).toBe(1);
  });
  it("never automatically resends after an uncertain result", async () => {
    const { bridge, controller: c } = await setup();
    c.change("to", "a@example.test");
    c.change("subject", "test");
    bridge.sendHook = async (id) => {
      bridge.rows.get(id)!.send_state = "unknown";
      throw new Error("lost receipt");
    };
    await c.send();
    await c.send(true);
    expect(bridge.sends).toBe(1);
    expect(c.store.getSnapshot().draft?.send_state).toBe("unknown");
    await c.resolve();
    expect(c.store.getSnapshot().draft?.send_state).toBe("failed");
    expect(bridge.sends).toBe(1);
  });
  it("blocks invalid recipients and missing attachments without clearing the draft", async () => {
    const { bridge, controller: c } = await setup();
    c.change("to", "unfinished@");
    c.change("body", "见附件");
    await c.send();
    expect(c.store.getSnapshot().errors.to).toBeTruthy();
    c.change("to", "a@example.test");
    c.change("subject", "文件");
    await c.send();
    expect(c.store.getSnapshot().confirm).toBe("attachment");
    expect(bridge.sends).toBe(0);
    expect(c.store.getSnapshot().fields?.body).toBe("见附件");
  });
  it("cancellation does not resurrect a removed upload on a late completion", async () => {
    const { bridge, controller: c } = await setup();
    const upload = deferred<string>();
    bridge.upload = () => upload.promise;
    await c.attach([{ Ok: { token: "f", filename: "x.pdf", size: 10 } }]);
    await c.removeUpload("f");
    upload.resolve("u");
    await c.flush();
    await Promise.resolve();
    expect(c.store.getSnapshot().uploads).toHaveLength(0);
  });
  it("definite send refusal leaves editable saved content", async () => {
    const { bridge, controller: c } = await setup();
    c.change("to", "a@example.test");
    c.change("subject", "拒绝");
    bridge.sendHook = async () => {
      throw new BridgeError({ code: "invalid", message: "bad account" });
    };
    await c.send();
    expect(c.store.getSnapshot().draft?.send_state).toBe("editing");
    expect(c.store.getSnapshot().fields?.subject).toBe("拒绝");
    expect(c.store.getSnapshot().error).toBe("bad account");
  });
});

it("registers every compose contribution through the production registry", () => {
  vi.stubGlobal("window", {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
  const host = new Host(new FakeBridge());
  const c = new ComposeController(
    host.bridge.compose,
    () => {},
    () => {},
  );
  controllers.push(c);
  const bundle = composeModule(host, c);
  expect(() => host.registerBuiltin(bundle)).not.toThrow();
  expect(host.registry.findCommand("maild.compose.reply-all")).toBeDefined();
  bundle.dispose?.();
  vi.unstubAllGlobals();
});

it("a send conflict never adopts an unseen recipient/version", async () => {
  const { bridge, controller: c } = await setup();
  c.change("to", "shown@example.test");
  c.change("subject", "确认");
  await c.flush();
  const shown = c.store.getSnapshot().draft!;
  bridge.rows.set(shown.id, {
    ...shown,
    to: ["unseen@example.test"],
    version: shown.version + 1,
  });
  bridge.sendHook = async () => {
    throw new BridgeError({ code: "conflict", message: "changed elsewhere" });
  };
  await c.send();
  await c.send(true);
  expect(bridge.sends).toBe(1);
  expect(c.store.getSnapshot().conflict).toBe(true);
  expect(c.store.getSnapshot().fields?.to).toBe("shown@example.test");
  expect(c.store.getSnapshot().draft?.version).toBe(shown.version);
});
it("a queued reply switch cannot turn a send click into reply-all delivery", async () => {
  const { bridge, controller: c } = await setup();
  c.change("to", "one@example.test");
  c.change("subject", "A");
  const pending = deferred<Draft>();
  bridge.saveHook = () => pending.promise;
  const flush = c.flush();
  await vi.waitFor(() => expect(bridge.saves).toHaveLength(1));
  const switching = c.reply(
    { account: "demo", folder: "INBOX", uid: "B" },
    "reply_all",
  );
  await c.send();
  expect(bridge.sends).toBe(0);
  bridge.saveHook = null;
  const d = { ...(await bridge.draft("draft-1")), version: 2 };
  bridge.rows.set(d.id, d);
  pending.resolve(d);
  await flush;
  await switching;
  expect(bridge.sends).toBe(0);
});
it("two attachment removals use stable identities instead of stale indices", async () => {
  const { bridge, controller: c } = await setup();
  const attachments = ["a", "b", "c"].map((id) => ({
    upload_id: id,
    account: "demo",
    folder: "",
    uid: "",
    part_index: 0,
    filename: id,
    mime_type: "text/plain",
    size: 1,
    stored: true,
  }));
  bridge.rows.get("draft-1")!.attachments = attachments;
  await c.reload();
  const pending = deferred<Draft>();
  bridge.saveHook = () => pending.promise;
  const a = c.detach(0);
  await vi.waitFor(() => expect(bridge.saves).toHaveLength(1));
  const b = c.detach(1);
  bridge.saveHook = null;
  const d = {
    ...(await bridge.draft("draft-1")),
    version: 2,
    attachments: attachments.slice(1),
  };
  bridge.rows.set(d.id, d);
  pending.resolve(d);
  await a;
  await b;
  expect(bridge.saves.at(-1)!.patch.attachments).toEqual([{ upload_id: "c" }]);
});
it("reload freezes new typing until the confirmed version arrives", async () => {
  const { bridge, controller: c } = await setup();
  const d = await bridge.draft("draft-1"),
    pending = deferred<Draft>();
  bridge.draft = () => pending.promise;
  const loading = c.reload();
  c.change("body", "typing after confirmation");
  pending.resolve(d);
  await loading;
  expect(c.store.getSnapshot().fields?.body).toBe(d.body);
  expect(c.store.getSnapshot().revision).toBe(0);
});
it("a confirmation dialog suppresses the ordinary send shortcut", async () => {
  const { bridge, controller: c } = await setup();
  c.change("to", "a@example.test");
  c.change("subject", "test");
  c.confirm("discard");
  await c.send();
  expect(bridge.sends).toBe(0);
});
it("unknown-state refresh does not adopt another client's unseen edits", async () => {
  const { bridge, controller: c } = await setup();
  const d = bridge.rows.get("draft-1")!;
  d.send_state = "unknown";
  await c.reload();
  bridge.rows.set(d.id, {
    ...d,
    send_state: "failed",
    to: ["changed@example.test"],
    version: d.version + 1,
  });
  await c.reconcile();
  expect(c.store.getSnapshot().conflict).toBe(true);
  expect(c.store.getSnapshot().fields?.to).toBe("");
});

it("releases a queued create when closing fails during an in-flight save", async () => {
  const { bridge, controller: c } = await setup();
  c.change("body", "keep me");
  const pending = deferred<Draft>();
  bridge.saveHook = () => pending.promise;
  const flushing = c.flush().catch(() => {});
  await vi.waitFor(() => expect(bridge.saves).toHaveLength(1));
  const creating = c.create();
  const closing = c.closeWindow();
  pending.reject(new BridgeError({ code: "conflict", message: "changed" }));
  await Promise.all([flushing, creating, closing]);
  expect(c.store.getSnapshot().busy).toBe(false);
  expect(c.store.getSnapshot().closing).toBe(false);
  expect(c.store.getSnapshot().fields?.body).toBe("keep me");
});
it("checks missing attachment confirmation after a queued removal completes", async () => {
  const { bridge, controller: c } = await setup();
  c.change("to", "reader@example.test");
  c.change("subject", "test");
  c.change("body", "附件见下");
  await c.flush();
  bridge.rows.get("draft-1")!.attachments = [
    {
      upload_id: "a",
      account: "demo",
      folder: "",
      uid: "",
      part_index: 0,
      filename: "a",
      mime_type: "text/plain",
      size: 1,
      stored: true,
    },
  ];
  await c.reload();
  const pending = deferred<Draft>();
  bridge.saveHook = () => pending.promise;
  const removing = c.detach(0);
  await vi.waitFor(() => expect(bridge.saves.length).toBe(2));
  const sending = c.send();
  const d = { ...(await bridge.draft("draft-1")), attachments: [], version: 3 };
  bridge.rows.set(d.id, d);
  pending.resolve(d);
  await Promise.all([removing, sending]);
  expect(bridge.sends).toBe(0);
  expect(c.store.getSnapshot().confirm).toBe("attachment");
});
it("an idempotent attachment receipt cannot adopt unseen recipients", async () => {
  const { bridge, controller: c } = await setup();
  c.change("to", "shown@example.test");
  c.change("subject", "test");
  await c.flush();
  const previous = c.store.getSnapshot().draft!;
  bridge.finish = async () => ({
    ...previous,
    to: ["unseen@example.test"],
    version: previous.version + 2,
  });
  await c.attach([{ Ok: { token: "token-1", filename: "file.txt", size: 1 } }]);
  await vi.waitFor(() => expect(c.store.getSnapshot().conflict).toBe(true));
  expect(c.store.getSnapshot().draft?.version).toBe(previous.version);
  expect(c.store.getSnapshot().fields?.to).toBe("shown@example.test");
  await c.send();
  expect(bridge.sends).toBe(0);
});

it("can return to the current conflicted editor without resaving or replacing local text", async () => {
  const { bridge, controller: c } = await setup();
  c.change("body", "local text");
  bridge.rows.get("draft-1")!.version = 2;
  await expect(c.flush()).rejects.toBeTruthy();
  const saves = bridge.saves.length;
  await c.open("draft-1");
  expect(bridge.saves).toHaveLength(saves);
  expect(c.store.getSnapshot().fields?.body).toBe("local text");
  expect(c.store.getSnapshot().conflict).toBe(true);
});
it("ordinary SMTP permits a Bcc-only envelope", async () => {
  const { bridge, controller: c } = await setup();
  c.change("bcc", "hidden@example.test");
  c.change("subject", "private copy");
  await c.send();
  expect(bridge.sends).toBe(1);
});

it("validation errors and command titles follow the UI language", async () => {
  const { controller: c } = await setup();
  applyEffectiveLocales(["en"]);
  try {
    c.change("to", "unfinished@");
    await c.send();
    expect(c.store.getSnapshot().errors.to).toBe("Enter full email addresses, separated by commas.");
    vi.stubGlobal("window", {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });
    const host = new Host(new FakeBridge());
    const bundle = composeModule(host, c);
    host.registerBuiltin(bundle);
    expect(host.registry.findCommand("maild.compose.reply-all")?.title).toBe("Reply All");
    applyEffectiveLocales([]);
    expect(host.registry.findCommand("maild.compose.reply-all")?.title).toBe("全部回复");
    bundle.dispose?.();
  } finally {
    applyEffectiveLocales([]);
    vi.unstubAllGlobals();
  }
});
