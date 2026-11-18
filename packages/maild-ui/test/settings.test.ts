import { describe, it, expect, vi } from "vitest";
import { SettingsController } from "../src/settings/controller";
import { FakeSettings, account, modernRules, preview } from "./fakeSettings";
import { deferred, online } from "./fakeBridge";
import type {
  AccountProfile,
  Probe,
  Packages,
  AddressPage,
} from "../src/settings/types";
const setup = () => {
  const bridge = new FakeSettings(),
    c = new SettingsController(bridge);
  c.connection(online);
  return { bridge, c };
};
describe("08-C management workflows", () => {
  it("preserves literal spaces in notification keywords", async () => {
    const { bridge, c } = setup();
    c.activate("maild.settings.notifications");
    await c.load();
    c.setRules({
      ...c.store.getSnapshot().rules!,
      silent_keywords: ["  invoice  ", ""],
    });
    await c.saveRules();
    expect(bridge.savedRules[0].silent_keywords).toEqual(["  invoice  "]);
  });
  it.each([
    { kind: "plugin", provider: "test.provider" },
    null,
    { ...account().outgoing!, credential: "incoming" },
  ])(
    "clears incompatible outgoing secrets when transport changes to %j",
    async (outgoing) => {
      const { bridge, c } = setup();
      c.beginAccount(account());
      c.editAccount({
        ...c.store.getSnapshot().editor!,
        draft: {
          ...c.store.getSnapshot().editor!.draft,
          outgoing: { ...account().outgoing!, credential: "separate" },
        },
        secrets: { incoming: "incoming-secret", outgoing: "smtp-only-secret" },
      });
      c.editAccount({
        ...c.store.getSnapshot().editor!,
        draft: { ...c.store.getSnapshot().editor!.draft, outgoing },
      });
      await c.probe();
      expect(bridge.probes[0].secrets).toEqual({ incoming: "incoming-secret" });
    },
  );
  it("loads profiles without ever prefilling saved credentials", async () => {
    const { c } = setup();
    await c.load();
    c.beginAccount(account());
    expect(c.store.getSnapshot().editor?.secrets).toEqual({});
    expect(c.dirty).toBe(false);
  });
  it("requires a successful probe of the current new account before create", async () => {
    const { bridge, c } = setup();
    c.beginAccount();
    await c.saveAccount();
    expect(bridge.accountWrites).toHaveLength(0);
    await c.probe();
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      draft: {
        ...c.store.getSnapshot().editor!.draft,
        address: "other@example.test",
      },
    });
    await c.saveAccount();
    expect(bridge.accountWrites).toHaveLength(0);
    await c.probe();
    await c.saveAccount();
    expect(bridge.accountWrites).toHaveLength(1);
    expect(c.store.getSnapshot().editor).toBeNull();
  });
  it("does not create if SMTP probe failed", async () => {
    const { bridge, c } = setup();
    bridge.probe = async () => ({
      incoming: { ok: true, stage: "ready" },
      outgoing: { ok: false, stage: "auth" },
    });
    c.beginAccount();
    await c.probe();
    await c.saveAccount();
    expect(bridge.accountWrites).toHaveLength(0);
  });
  it("new receive-only account needs no SMTP probe", async () => {
    const { bridge, c } = setup();
    bridge.probe = async () => ({ incoming: { ok: true, stage: "ready" } });
    c.beginAccount();
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      draft: { ...c.store.getSnapshot().editor!.draft, outgoing: null },
    });
    await c.probe();
    await c.saveAccount();
    expect(bridge.accountWrites).toHaveLength(1);
  });
  it("keeps nonsecret input after save conflict, clears entered secrets", async () => {
    const { bridge, c } = setup();
    bridge.saveAccount = async () => {
      throw Error("version conflict");
    };
    c.beginAccount(account());
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      secrets: { incoming: "temporary-secret" },
    });
    await c.saveAccount();
    expect(c.store.getSnapshot().editor?.draft.address).toBe("me@example.test");
    expect(c.store.getSnapshot().editor?.secrets).toEqual({});
    expect(c.store.getSnapshot().error).toContain("conflict");
  });
  it("save is single-flight and bound to the captured revision", async () => {
    const { bridge, c } = setup(),
      d = deferred<AccountProfile>();
    bridge.saveAccount = vi.fn(() => d.promise);
    c.beginAccount(account());
    const first = c.saveAccount();
    await c.saveAccount();
    expect(bridge.saveAccount).toHaveBeenCalledTimes(1);
    expect(vi.mocked(bridge.saveAccount).mock.calls[0][0].revision).toBe(2);
    d.resolve(account());
    await first;
  });
  it("drops stale probe on disconnect and clears transient secrets", async () => {
    const { bridge, c } = setup(),
      d = deferred<Probe>();
    bridge.probe = () => d.promise;
    c.beginAccount(account());
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      secrets: { incoming: "secret" },
    });
    const p = c.probe();
    c.connection({ ...online, phase: "retry_waiting" });
    d.resolve({ incoming: { ok: true, stage: "ready" } });
    await p;
    expect(c.store.getSnapshot().probe).toBeNull();
    expect(c.store.getSnapshot().editor?.secrets).toEqual({});
  });
  it("drops old profile load after connection epoch changes", async () => {
    const { bridge, c } = setup(),
      d = deferred<AccountProfile[]>();
    bridge.profiles = () => d.promise;
    const p = c.load();
    c.connection({ ...online, connection_epoch: 2 });
    d.resolve([account()]);
    await p;
    expect(c.store.getSnapshot().profiles).toBeNull();
  });
  it("does not let a previous page load fill a new section", async () => {
    const { bridge, c } = setup(),
      d = deferred<Packages>();
    bridge.packages = () => d.promise;
    const p = c.load();
    await Promise.resolve();
    c.activate("maild.settings.appearance");
    d.resolve(bridge.catalog);
    await p;
    expect(c.store.getSnapshot().packages).toBeNull();
  });
  it("requires discard confirmation to leave dirty settings", () => {
    const { c } = setup(),
      next = vi.fn();
    c.beginAccount(account());
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      secrets: { incoming: "secret" },
    });
    expect(c.requestLeave(next)).toBe(false);
    c.cancelConfirmation();
    expect(next).not.toHaveBeenCalled();
    expect(c.store.getSnapshot().editor).not.toBeNull();
    expect(c.requestLeave(next)).toBe(false);
    c.confirm();
    expect(next).toHaveBeenCalledOnce();
    expect(c.store.getSnapshot().editor).toBeNull();
  });
  it("window close waits for explicit discard or cancellation", async () => {
    const { c } = setup();
    c.beginAccount(account());
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      secrets: { incoming: "secret" },
    });
    const first = c.beforeClose();
    c.cancelConfirmation();
    expect(await first).toBe(false);
    const second = c.beforeClose();
    c.confirm();
    expect(await second).toBe(true);
    expect(c.store.getSnapshot().editor).toBeNull();
  });
  it("busy settings block close and navigation", async () => {
    const { bridge, c } = setup(),
      d = deferred<Probe>();
    bridge.probe = () => d.promise;
    c.beginAccount(account());
    const p = c.probe();
    expect(c.requestLeave(vi.fn())).toBe(false);
    expect(await c.beforeClose()).toBe(false);
    d.resolve({ incoming: { ok: true, stage: "ready" } });
    await p;
  });
  it("does not mutate account on canceled removal", () => {
    const { bridge, c } = setup();
    c.confirmAccount(account(), true);
    expect(c.store.getSnapshot().confirm?.body).toContain("本地邮件保留");
    c.cancelConfirmation();
    expect(bridge.accountActions).toHaveLength(0);
  });
  it("removal uses displayed identity and revision", async () => {
    const { bridge, c } = setup();
    c.confirmAccount(account(), true);
    c.confirm();
    await Promise.resolve();
    expect(bridge.accountActions[0]).toEqual({
      action: "remove",
      id: "acct-a",
      revision: 2,
    });
  });
  it("notification read never requests authorization", async () => {
    const { bridge, c } = setup();
    c.activate("maild.settings.notifications");
    await c.load();
    expect(bridge.notificationRequests.every((x) => !x)).toBe(true);
  });
  it("rules show runtime-only persistence and refresh cannot overwrite dirty input", async () => {
    const { c } = setup();
    c.activate("maild.settings.notifications");
    await c.load();
    c.setRules({ ...c.store.getSnapshot().rules!, show_sensitive: true });
    await c.load();
    expect(c.store.getSnapshot().rules?.show_sensitive).toBe(true);
    await c.saveRules();
    expect(c.store.getSnapshot().notice).toContain("本次后台运行");
    expect(c.dirty).toBe(false);
  });
  it("address pagination ignores late response from another account", async () => {
    const { bridge, c } = setup(),
      d = deferred<AddressPage>();
    bridge.addresses = (id) =>
      id === "a" ? d.promise : Promise.resolve({ addresses: [], total: 5 });
    const p = c.selectAddressAccount("a");
    await c.selectAddressAccount("b");
    d.resolve({ addresses: [], total: 99 });
    await p;
    expect(c.store.getSnapshot().addressAccount).toBe("b");
    expect(c.store.getSnapshot().addresses?.total).toBe(5);
  });
  it("provider primary-only account cannot advertise a new sendable alias", async () => {
    const { bridge, c } = setup();
    c.store.patch({
      profiles: [
        {
          ...account(),
          outgoing: { kind: "plugin", provider: "test.provider" },
        },
      ],
      addressAccount: "acct-a",
      addressForm: {
        account: "acct-a",
        kind: "alias",
        address: "alias@example.test",
        label: "",
        days: "",
      },
    });
    await c.createAddress();
    expect(bridge.addressWrites).toHaveLength(0);
    expect(c.store.getSnapshot().error).toContain("主地址");
  });
  it("registers an explicit SMTP alias only on submit", async () => {
    const { bridge, c } = setup();
    c.store.patch({
      profiles: [account()],
      addressAccount: "acct-a",
      addressForm: {
        account: "acct-a",
        kind: "alias",
        address: "alias@example.test",
        label: "客户支持",
        days: "",
      },
    });
    await c.createAddress();
    expect(bridge.addressWrites[0]).toEqual({
      action: "create",
      account: "acct-a",
      kind: "alias",
      address: "alias@example.test",
      label: "客户支持",
      expires_secs: null,
    });
  });
  it("selection does not install, cancel releases native staging token", async () => {
    const { bridge, c } = setup();
    await c.pickPackage("plugin");
    expect(bridge.installs).toEqual([]);
    c.cancelPackage();
    expect(bridge.cancels).toEqual(["native-token"]);
  });
  it("installation consumes approval after an unknown outcome and never auto-retries", async () => {
    const { bridge, c } = setup();
    bridge.installPackage = vi.fn(async () => {
      throw Error("connection lost");
    });
    await c.pickPackage("plugin");
    await c.install();
    await c.install();
    expect(bridge.installPackage).toHaveBeenCalledTimes(1);
    expect(c.store.getSnapshot().choice).toBeNull();
  });
  it("cancels outstanding package snapshot on disconnect", async () => {
    const { bridge, c } = setup();
    await c.pickPackage("plugin");
    c.connection({ ...online, phase: "disconnected" });
    expect(c.store.getSnapshot().choice).toBeNull();
    expect(bridge.cancels).toEqual(["native-token"]);
  });
  it("requires explicit approval of rollback permissions", async () => {
    const { bridge, c } = setup();
    bridge.rollback = vi.fn(async () => {});
    await c.previewRollback("plugin", "test.provider");
    expect(bridge.rollback).not.toHaveBeenCalled();
    await c.rollback();
    expect(bridge.rollback).toHaveBeenCalledWith(preview());
  });
  it("package mutation confirmation lists affected accounts", () => {
    const { bridge, c } = setup();
    c.store.patch({
      profiles: [
        {
          ...account(),
          incoming: { kind: "plugin", provider: "test.provider" },
        },
      ],
    });
    c.confirmPackage("plugin", "test.provider", "disable");
    expect(c.store.getSnapshot().confirm?.body).toContain("工作邮箱");
    c.cancelConfirmation();
    expect(bridge.packageWrites).toHaveLength(0);
  });
  it("navigation cannot replace a pending window-close continuation", async () => {
    const { c } = setup();
    c.beginAccount(account());
    c.editAccount({
      ...c.store.getSnapshot().editor!,
      secrets: { incoming: "secret" },
    });
    const close = c.beforeClose(),
      next = vi.fn();
    c.requestLeave(next);
    c.confirm();
    expect(await Promise.race([close, Promise.resolve("stranded")])).toBe(true);
    expect(next).not.toHaveBeenCalled();
  });
  it("late native selection is explicitly released after disconnect", async () => {
    const { bridge, c } = setup(),
      d = deferred<import("../src/settings/types").PackageChoice | null>();
    bridge.pickPackage = () => d.promise;
    const p = c.pickPackage("plugin");
    c.connection({ ...online, phase: "disconnected" });
    d.resolve({ token: "late", source: "package", preview: preview() });
    await p;
    expect(bridge.cancels).toEqual(["late"]);
  });
  it("package inventory remains accessible without account-management permission", async () => {
    const { bridge, c } = setup();
    bridge.profiles = async () => {
      throw Error("denied");
    };
    c.activate("maild.settings.extensions");
    await c.load();
    expect(c.store.getSnapshot().packages).toEqual(bridge.catalog);
    expect(c.store.getSnapshot().profiles).toBeNull();
  });
  it("editing an existing address does not erase an unsaved new address form", async () => {
    const { c } = setup();
    const f = {
      account: "acct-a",
      address: "new@example.test",
      label: "未保存",
      kind: "alias" as const,
      days: "",
    };
    c.store.patch({ addressAccount: "acct-a", addressForm: f });
    c.confirmAddress(
      {
        id: "address-existing",
        account: "acct-a",
        address: "old@example.test",
        kind: "alias",
        state: "active",
        label: "",
        created_at: 0,
      },
      false,
    );
    c.confirm();
    await new Promise((r) => setTimeout(r, 0));
    expect(c.store.getSnapshot().addressForm).toEqual(f);
  });
  it("close approval locks further edits until close succeeds or is released", async () => {
    const { bridge, c } = setup();
    c.beginAccount(account());
    expect(await c.beforeClose()).toBe(true);
    c.beginAccount(account());
    expect(c.store.getSnapshot().editor).toBeNull();
    await c.pickPackage("plugin");
    expect(c.store.getSnapshot().choice).toBeNull();
    expect(bridge.installs).toEqual([]);
    c.releaseClose();
    c.beginAccount(account());
    expect(c.store.getSnapshot().editor).not.toBeNull();
  });
  it("address input stays bound to its original account after refresh fallback", async () => {
    const { bridge, c } = setup();
    c.store.patch({
      addressAccount: "other",
      profiles: [{ ...account(), id: "other" }],
      addressForm: {
        account: "acct-a",
        kind: "alias",
        address: "new@example.test",
        label: "",
        days: "",
      },
    });
    await c.createAddress();
    expect(bridge.addressWrites).toEqual([]);
    expect(c.store.getSnapshot().error).toContain("所属账户");
  });
  it("package recovery explicitly reconciles directory state before reloading", async () => {
    const { bridge, c } = setup();
    c.store.patch({ section: "maild.settings.extensions" });
    await c.reconcilePackages();
    expect(bridge.packageWrites).toEqual([{ action: "refresh" }]);
    expect(c.store.getSnapshot().packages).toEqual(bridge.catalog);
  });
  it("preserves follow_system_timezone across load and save for new daemons while omitting it for old daemons", async () => {
    const { bridge, c } = setup();
    c.activate("maild.settings.notifications");
    await c.load();
    expect(c.store.getSnapshot().rules?.follow_system_timezone).toBeUndefined();
    c.setRules({ ...c.store.getSnapshot().rules!, show_sensitive: true });
    await c.saveRules();
    expect("follow_system_timezone" in bridge.savedRules[0]!).toBe(false);

    bridge.rules = async () => ({
      rules: { ...modernRules(), follow_system_timezone: true },
      persisted: true,
    });
    await c.load();
    expect(c.store.getSnapshot().rules?.follow_system_timezone).toBe(true);
    c.setRules({ ...c.store.getSnapshot().rules!, follow_system_timezone: false });
    await c.saveRules();
    expect(bridge.savedRules.at(-1)?.follow_system_timezone).toBe(false);
  });
});
