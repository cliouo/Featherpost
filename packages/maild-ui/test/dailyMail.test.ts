import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import type { MessageIdentity, UiSummary } from "../src/bridge/types";
import { MailboxController, messageKey } from "../src/mailbox/controller";
import { FolderNav } from "../src/mailbox/FolderNav";
import { runShortcut, SHORTCUT_COMMANDS } from "../src/mailbox/keyboard";
import { isEditableTarget, shortcutFor, type ShortcutAction } from "../src/mailbox/shortcuts";
import {
  localDayEndSecs,
  localDayStartSecs,
  parseLocalDateYmd,
  SearchBar,
} from "../src/mailbox/SearchBar";
import type { ServerSearchOptions } from "../src/bridge/types";
import { Welcome } from "../src/mailbox/Welcome";
import { SettingsController } from "../src/settings/controller";
import { Notifications } from "../src/settings/Notifications";
import { deferred, FakeBridge, flush, folder, message, offlineError, online, summary } from "./fakeBridge";
import { FakeSettings, account, rules } from "./fakeSettings";

const controllers: MailboxController[] = [];
afterEach(() => controllers.splice(0).forEach((c) => c.stop()));

async function ready(rows: UiSummary[] = []) {
  const bridge = new FakeBridge();
  const c = new MailboxController(bridge);
  controllers.push(c);
  c.acceptSnapshot(online);
  for (const r of bridge.folderRequests) r.d.resolve([folder("INBOX", "inbox"), folder("Archive", "archive")]);
  bridge.mailbox.queries[0].d.resolve({ messages: rows, total: rows.length });
  await flush();
  return { bridge, c };
}

describe("快捷键路由", () => {
  const key = (k: string, extra: object = {}) => ({ key: k, ...extra });
  const body = { tagName: "BODY", closest: () => null };

  it.each<[string, ShortcutAction]>([
    ["j", "next"],
    ["k", "previous"],
    ["e", "archive"],
    ["s", "star"],
    ["u", "read"],
    ["r", "reply"],
    ["a", "replyAll"],
    ["f", "forward"],
    ["c", "compose"],
    ["/", "search"],
  ])("%s → %s", (k, action) => {
    expect(shortcutFor(key(k), body, false)).toBe(action);
  });

  it("? 需要 Shift 也能触发；其他单键按住 Shift 不触发", () => {
    expect(shortcutFor(key("?", { shiftKey: true }), body, false)).toBe("help");
    expect(shortcutFor(key("J", { shiftKey: true }), body, false)).toBeNull();
  });

  it("可编辑控件、模态对话框、输入法组字时一律放行", () => {
    for (const tagName of ["INPUT", "TEXTAREA", "SELECT"])
      expect(shortcutFor(key("j"), { tagName }, false)).toBeNull();
    expect(shortcutFor(key("j"), { tagName: "DIV", isContentEditable: true }, false)).toBeNull();
    expect(
      shortcutFor(key("j"), { tagName: "SPAN", closest: (sel: string) => (sel.includes("contenteditable") ? {} : null) }, false),
    ).toBeNull();
    expect(shortcutFor(key("j"), body, true)).toBeNull();
    expect(shortcutFor(key("j", { isComposing: true }), body, false)).toBeNull();
    expect(isEditableTarget(null)).toBe(false);
  });

  it("⌘R / Ctrl+R 在输入框里也是立即同步；其他修饰键组合不拦截", () => {
    expect(shortcutFor(key("r", { metaKey: true }), { tagName: "INPUT" }, false)).toBe("sync");
    expect(shortcutFor(key("R", { ctrlKey: true }), body, false)).toBe("sync");
    expect(shortcutFor(key("r", { metaKey: true }), body, true)).toBeNull();
    expect(shortcutFor(key("r", { metaKey: true, shiftKey: true }), body, false)).toBeNull();
    expect(shortcutFor(key("j", { metaKey: true }), body, false)).toBeNull();
    expect(shortcutFor(key("j", { altKey: true }), body, false)).toBeNull();
  });

  it("j / k 打开相邻邮件；写入口经宿主命令；星标与已读直接切换", async () => {
    const { bridge, c } = await ready(["1", "2", "3"].map((uid) => summary("INBOX", uid)));
    const executed: string[] = [];
    const calls: string[] = [];
    const deps = {
      controller: c,
      execute: (id: string) => executed.push(id),
      writable: () => true,
      focusSearch: () => calls.push("search"),
      toggleHelp: () => calls.push("help"),
      sync: () => calls.push("sync"),
    };
    runShortcut("next", deps);
    expect(c.state.selected?.uid).toBe("1");
    runShortcut("next", deps);
    runShortcut("next", deps);
    runShortcut("next", deps);
    expect(c.state.selected?.uid).toBe("3");
    runShortcut("previous", deps);
    expect(c.state.selected?.uid).toBe("2");
    expect(bridge.detailRequests.map((r) => r.identity.uid)).toEqual(["1", "2", "3", "2"]);
    for (const action of ["archive", "reply", "replyAll", "forward", "compose"] as const) runShortcut(action, deps);
    expect(executed).toEqual([
      "maild.mailbox.archive",
      "maild.compose.reply",
      "maild.compose.reply-all",
      "maild.compose.forward",
      "maild.compose.new",
    ]);
    expect(Object.keys(SHORTCUT_COMMANDS)).toHaveLength(5);
    runShortcut("star", deps);
    await flush();
    expect(bridge.mailbox.writes.at(-1)).toMatchObject({ action: "mark", flag: "flagged", value: true, uids: ["2"] });
    runShortcut("read", deps);
    await flush();
    expect(bridge.mailbox.writes.at(-1)).toMatchObject({ action: "mark", flag: "seen", value: false, uids: ["2"] });
    runShortcut("search", deps);
    runShortcut("help", deps);
    runShortcut("sync", deps);
    expect(calls).toEqual(["search", "help", "sync"]);
    const writes = bridge.mailbox.writes.length;
    runShortcut("star", { ...deps, writable: () => false });
    await flush();
    expect(bridge.mailbox.writes).toHaveLength(writes);
  });
});

describe("立即同步", () => {
  it.each([undefined, "Archive"])("tracks an explicit sidebar sync (%s) through completion and suppresses duplicates", async (name) => {
    const { bridge, c } = await ready();
    const originalScope = c.state.scope;
    const sync = deferred<"complete" | "background">();
    const asked: Array<[string, string | undefined]> = [];
    bridge.mailbox.syncScope = (account, folder) => {
      asked.push([account, folder]);
      return sync.promise;
    };
    const scope = { account: "demo", folder: name };
    const run = c.syncNow(scope);
    await c.syncNow({ account: "demo", folder: "INBOX" });
    expect(c.state.syncingScopes.get("demo")).toEqual({ folder: name });
    expect(asked).toEqual([["demo", name]]);
    expect(bridge.folderRequests).toHaveLength(1);
    if (name) sync.reject(offlineError());
    else sync.resolve("complete");
    await flush();
    expect(c.state.syncingScopes.get("demo")).toEqual({ folder: name });
    bridge.folderRequests.at(-1)!.d.resolve([folder("INBOX", "inbox"), folder("Renamed")]);
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await run;
    expect(c.state.syncingScopes.has("demo")).toBe(false);
    expect(c.isSyncing()).toBe(false);
    expect(c.state.scope).toEqual(originalScope);
    expect(c.foldersOf("demo").map((folder) => folder.name)).toEqual(["INBOX", "Renamed"]);
    if (name) expect(c.state.syncError).toContain("后台连接不可用");
    else expect(c.state.syncError).toBeNull();
    expect(c.state.syncNotice).toBeNull();
  });

  it("shows a non-error background sync notice and clears the spinner before refreshing", async () => {
    const { bridge, c } = await ready();
    const sync = deferred<"complete" | "background">();
    bridge.mailbox.syncScope = () => sync.promise;
    const run = c.syncNow({ account: "demo" });
    expect(c.isSyncing("demo")).toBe(true);

    sync.resolve("background");
    await flush();
    expect(c.isSyncing("demo")).toBe(false);
    expect(c.state.syncNotice).toBe("同步已启动，仍在后台进行。");
    expect(c.state.syncError).toBeNull();

    bridge.folderRequests.at(-1)!.d.resolve([folder("INBOX", "inbox")]);
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await run;
    expect(c.state.syncNotice).toBe("同步已启动，仍在后台进行。");
  });

  it("lets another account finish while a scoped sync is stalled", async () => {
    const { bridge, c } = await ready();
    const stalled = deferred<"complete" | "background">();
    const independent = deferred<"complete" | "background">();
    bridge.mailbox.syncScope = (account) => account === "demo" ? stalled.promise : independent.promise;
    const first = c.syncNow({ account: "demo", folder: "Archive" });
    const second = c.syncNow({ account: "other" });
    expect(c.isSyncing("demo")).toBe(true);
    expect(c.isSyncing("other")).toBe(true);

    independent.resolve("complete");
    await flush();
    bridge.folderRequests.at(-1)!.d.resolve([folder("INBOX", "inbox")]);
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await second;
    expect(c.isSyncing("other")).toBe(false);
    expect(c.isSyncing("demo")).toBe(true);

    stalled.reject(offlineError());
    await flush();
    bridge.folderRequests.at(-1)!.d.resolve([folder("INBOX", "inbox")]);
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await first;
    expect(c.isSyncing()).toBe(false);
  });

  it("先敲醒后台（带当前账户），再刷新列表，期间显示同步中", async () => {
    const { bridge, c } = await ready();
    c.selectFolder(c.foldersOf("demo")[0]);
    bridge.pageRequests.at(-1)!.d.resolve({ messages: [], total: 0 });
    await flush();
    const sync = deferred<void>();
    const asked: Array<string | undefined> = [];
    bridge.mailbox.syncNow = (account) => {
      asked.push(account);
      return sync.promise;
    };
    const pages = bridge.pageRequests.length;
    const run = c.syncNow();
    expect(c.isSyncing()).toBe(true);
    expect(asked).toEqual(["demo"]);
    expect(bridge.pageRequests).toHaveLength(pages);
    sync.resolve();
    await flush();
    expect(bridge.pageRequests.length).toBeGreaterThan(pages);
    for (const r of bridge.folderRequests.slice(1)) r.d.resolve([folder("INBOX", "inbox")]);
    bridge.pageRequests.at(-1)!.d.resolve({ messages: [summary("INBOX", "9")], total: 1 });
    await run;
    expect(c.isSyncing()).toBe(false);
    expect(c.state.syncError).toBeNull();
    expect(c.state.rows.map((r) => r.uid)).toEqual(["9"]);
  });

  it("旧后台没有 syncNow：照常刷新；离线错误如实提示", async () => {
    const { bridge, c } = await ready();
    const queries = bridge.mailbox.queries.length;
    const run = c.syncNow();
    await flush();
    expect(bridge.mailbox.queries.length).toBeGreaterThan(queries);
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    for (const r of bridge.folderRequests.slice(1)) r.d.resolve([]);
    await run;
    expect(c.state.syncError).toBeNull();
    bridge.mailbox.syncNow = async () => {
      throw offlineError();
    };
    const again = c.syncNow();
    await flush();
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    for (const r of bridge.folderRequests.slice(1)) r.d.resolve([]);
    await again;
    expect(c.state.syncError).toContain("后台连接不可用");
  });
});

describe("服务器历史搜索与按需拉取", () => {
  const remote = (uid: string, subject = `old ${uid}`): UiSummary => ({ ...summary("INBOX", uid, subject), sort_at: Number(uid) });

  it("旧后台没有 serverSearch 时不提供入口", async () => {
    const { c } = await ready();
    c.search({ query: "发票" });
    expect(c.serverSearchSupported).toBe(false);
    expect(c.canSearchServer).toBe(false);
  });

  it("搜索 → 远端结果（去掉本地已有）→ 点开拉回本地并原位渲染", async () => {
    const { bridge, c } = await ready();
    const searches: Array<[string, string, string]> = [];
    const page = deferred<UiSummary[]>();
    bridge.mailbox.serverSearch = (account, folderName, query) => {
      searches.push([account, folderName, query]);
      return page.promise;
    };
    const fetches: MessageIdentity[] = [];
    const fetched = deferred<ReturnType<typeof message> | null>();
    bridge.mailbox.fetchServerMessage = (account, folderName, uid) => {
      fetches.push({ account, folder: folderName, uid });
      return fetched.promise;
    };
    c.search({ query: "发票" });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [summary("INBOX", "500")], total: 1 });
    await flush();
    expect(c.canSearchServer).toBe(true);
    const run = c.searchServer();
    expect(c.state.remote.status).toBe("loading");
    expect(searches).toEqual([["demo", "INBOX", "发票"]]);
    page.resolve([remote("10"), remote("500"), remote("20"), { ...remote("30"), account: "intruder" }]);
    await run;
    expect(c.state.remote.status).toBe("ready");
    expect(c.state.remote.rows.map((r) => r.uid)).toEqual(["20", "10"]);
    const target = c.state.remote.rows[1];
    expect(c.isRemote(target)).toBe(true);
    c.openRemote(target);
    expect(c.state.selected?.uid).toBe("10");
    expect(c.state.loadingDetail).toBe(true);
    expect(fetches).toEqual([{ account: "demo", folder: "INBOX", uid: "10" }]);
    expect(bridge.detailRequests).toHaveLength(0);
    fetched.resolve(message("INBOX", "10", { has_html: true, body: "历史邮件正文" }));
    await flush();
    expect(c.state.loadingDetail).toBe(false);
    expect(c.state.detail?.body).toBe("历史邮件正文");
    expect(c.state.remote.fetched).toEqual([messageKey(target)]);
    // 已拉回：再打开走本地详情通道。
    c.clearSelection();
    c.openRemote(target);
    expect(bridge.detailRequests.map((r) => r.identity.uid)).toEqual(["10"]);
    // j / k 在服务器结果里移动。
    c.step(-1);
    expect(c.state.selected?.uid).toBe("20");
    expect(fetches.map((f) => f.uid)).toEqual(["10", "20"]);
  });

  it("服务器上已不存在、桥未实现命令时如实提示；换范围后迟到结果作废", async () => {
    const { bridge, c } = await ready();
    bridge.mailbox.serverSearch = async () => {
      throw new Error("command ui_server_search not found");
    };
    c.search({ query: "合同" });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await flush();
    await c.searchServer();
    expect(c.state.remote.status).toBe("error");
    expect(c.state.remote.error).toContain("服务器搜索暂不可用");

    const late = deferred<UiSummary[]>();
    bridge.mailbox.serverSearch = () => late.promise;
    const run = c.searchServer();
    c.selectSmart("inbox");
    late.resolve([remote("1")]);
    await run;
    expect(c.state.remote).toMatchObject({ status: "idle", rows: [] });

    bridge.mailbox.serverSearch = async () => [remote("7")];
    bridge.mailbox.fetchServerMessage = async () => null;
    c.search({ query: "合同" });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await flush();
    await c.searchServer();
    c.openRemote(c.state.remote.rows[0]);
    await flush();
    expect(c.state.detailError).toContain("服务器上已找不到");
    // 未拉回本地的服务器结果不能写。
    expect(await c.toggleSelected("flagged")).toBeNull();
  });

  it("searchServer 透传 since/until 日期筛选，并对忽略参数的桥做纵深后置过滤", async () => {
    const { bridge, c } = await ready();
    const receivedCalls: Array<{
      account: string;
      folder: string;
      query: string;
      options?: ServerSearchOptions;
    }> = [];
    // 假桥故意忽略 options 返回越界与无日期（sort_at = 0）摘要，验证 controller 纵深过滤
    bridge.mailbox.serverSearch = async (account, folderName, query, options) => {
      receivedCalls.push({ account, folder: folderName, query, options });
      return [
        { ...remote("101"), sort_at: 999 }, // 早于 since
        { ...remote("102"), sort_at: 1000 }, // 恰好等于 since
        { ...remote("103"), sort_at: 1500 }, // 区间内
        { ...remote("104"), sort_at: 2000 }, // 恰好等于 until
        { ...remote("105"), sort_at: 2001 }, // 晚于 until
        { ...remote("106"), sort_at: 0 }, // 缺失日期兜底 0
      ];
    };
    c.search({ query: "报销", since: 1000, until: 2000 });
    bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
    await flush();
    await c.searchServer();
    expect(receivedCalls).toEqual([
      {
        account: "demo",
        folder: "INBOX",
        query: "报销",
        options: { since: 1000, until: 2000 },
      },
    ]);
    expect(c.state.remote.status).toBe("ready");
    expect(c.state.remote.rows.map((r) => r.uid)).toEqual(["104", "103", "102"]);
  });
});

describe("SearchBar 日期边界：夏令时跳变与午夜切换安全", () => {
  const proc = (globalThis as { process?: { env: Record<string, string | undefined> } }).process;

  function withTz<T>(tz: string, fn: () => T): T {
    if (!proc) return fn();
    const prev = proc.env.TZ;
    proc.env.TZ = tz;
    try {
      return fn();
    } finally {
      if (prev === undefined) delete proc.env.TZ;
      else proc.env.TZ = prev;
    }
  }

  it("显式按次日本地零点减 1 秒计算 until，覆盖 23 小时、25 小时与午夜夏令时时区", () => {
    expect(parseLocalDateYmd("2026-02-29")).toBeNull();
    expect(parseLocalDateYmd("2026-04-31")).toBeNull();
    expect(parseLocalDateYmd("2024-02-29")).toEqual({ y: 2024, m: 2, d: 29 });

    withTz("America/Chicago", () => {
      // 普通 24 小时日：86400 - 1 = 86399 秒
      const normStart = localDayStartSecs("2026-11-18")!;
      const normEnd = localDayEndSecs("2026-11-18")!;
      expect(normEnd - normStart).toBe(86_399);
      expect(normEnd + 1).toBe(localDayStartSecs("2026-11-19"));

      // 北美夏令时拨快日（23 小时）：82800 - 1 = 82799 秒
      const springStart = localDayStartSecs("2026-03-08")!;
      const springEnd = localDayEndSecs("2026-03-08")!;
      expect(springEnd - springStart).toBe(82_799);
      expect(springEnd + 1).toBe(localDayStartSecs("2026-03-09"));

      // 北美夏令时拨慢日（25 小时）：90000 - 1 = 89999 秒
      const fallStart = localDayStartSecs("2026-11-01")!;
      const fallEnd = localDayEndSecs("2026-11-01")!;
      expect(fallEnd - fallStart).toBe(89_999);
      expect(fallEnd + 1).toBe(localDayStartSecs("2026-11-02"));

      // 跨年边界
      expect(localDayEndSecs("2026-12-31")! + 1).toBe(localDayStartSecs("2027-01-01"));
    });

    // 午夜 00:00 跳变的夏令时时区（America/Havana）：日末始终紧接次日日始前 1 秒
    withTz("America/Havana", () => {
      expect(localDayEndSecs("2026-03-07")! + 1).toBe(localDayStartSecs("2026-03-08"));
      expect(localDayEndSecs("2026-03-08")! + 1).toBe(localDayStartSecs("2026-03-09"));
      expect(localDayEndSecs("2026-10-31")! + 1).toBe(localDayStartSecs("2026-11-01"));
      expect(localDayEndSecs("2026-11-01")! + 1).toBe(localDayStartSecs("2026-11-02"));
    });
  });

  it("SearchBar 按本地日历回显起止日期筛选标签", async () => {
    const { bridge, c } = await ready();
    withTz("Asia/Shanghai", () => {
      const since = localDayStartSecs("2026-11-01")!;
      const until = localDayEndSecs("2026-11-18")!;
      c.search({ query: "账单", since, until });
      bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [], total: 0 });
      const html = renderToStaticMarkup(createElement(SearchBar, { controller: c }));
      expect(html).toContain("从：2026-11-01");
      expect(html).toContain("到：2026-11-18");
    });
  });
});

describe("零账户欢迎与账户修复入口", () => {
  it("欢迎卡列出主流服务商与钥匙串说明", () => {
    const html = renderToStaticMarkup(createElement(Welcome, { onSetup: () => {} }));
    expect(html).toContain("连接你的第一个邮箱");
    for (const name of ["QQ 邮箱", "网易 163", "Gmail", "iCloud", "企业邮箱", "其他邮箱"]) expect(html).toContain(name);
    expect(html).toContain("凭据仅保存在系统钥匙串");
    expect(html).toContain("后台以极低内存静默收信");
  });

  it("欢迎卡按钮把服务商交给设置向导（main.tsx 的接线等价于此）", () => {
    const settings = new SettingsController(new FakeSettings());
    settings.connection(online);
    const chosen: Array<string | undefined> = [];
    const element = Welcome({ onSetup: (p) => chosen.push(p) });
    // 找到 Gmail 按钮并触发 onClick。
    const buttons: Array<{ props: { onClick?: () => void; children?: unknown } }> = [];
    const walk = (node: unknown) => {
      if (!node || typeof node !== "object") return;
      if (Array.isArray(node)) return node.forEach(walk);
      const n = node as { type?: unknown; props?: { onClick?: () => void; children?: unknown } };
      if (n.type === "button" && n.props) buttons.push(n as never);
      walk(n.props?.children);
    };
    walk(element);
    const gmail = buttons.find((b) => JSON.stringify(b.props.children).includes("Gmail"))!;
    gmail.props.onClick!();
    expect(chosen).toEqual(["gmail"]);
    settings.openSetup(chosen[0]);
    expect(settings.store.getSnapshot().editor?.draft.incoming.server).toBe("imap.gmail.com");
  });

  it("侧栏对需重新认证 / 连接故障的账户给出提醒与修复入口", async () => {
    const bridge = new FakeBridge();
    const c = new MailboxController(bridge);
    controllers.push(c);
    c.acceptSnapshot({
      ...online,
      accounts: [
        { account: "demo", state: "needs_credentials", detail: "" },
        { account: "ok", state: "online", detail: "" },
        { account: "broken", state: "error", detail: "tls handshake" },
      ],
    });
    const html = renderToStaticMarkup(
      createElement(FolderNav, { controller: c, onOpenMailbox: () => {}, onRepair: () => {} }),
    );
    expect(html).toContain("需重新认证");
    expect(html).toContain("授权码或应用专用密码已失效");
    expect(html).toContain("连接失败，请检查服务器参数或网络");
    expect(html.match(/aria-label="修复 /g)).toHaveLength(2);
    const settings = new SettingsController(new FakeSettings());
    settings.connection(online);
    settings.store.patch({ section: "maild.settings.addresses" });
    settings.openAccounts();
    expect(settings.store.getSnapshot().section).toBe("maild.settings.accounts");
  });
});

describe("通知与规则：新邮件通知与按账户静音", () => {
  it("旧后台不回传新键：如实标为不可用，并展示非持久状态", async () => {
    const bridge = new FakeSettings();
    const c = new SettingsController(bridge);
    c.connection(online);
    c.activate("maild.settings.notifications");
    await c.load();
    await flush();
    const html = renderToStaticMarkup(createElement(Notifications, { controller: c }));
    expect(html).toContain("收件箱新邮件系统通知");
    expect(html).toContain("当前后台版本尚未提供此开关");
    expect(html).toContain("静音 工作邮箱");
    expect(html).toContain("后台尚未说明规则是否持久保存");
  });

  it("新后台：开关与静音名单可编辑，保存回执决定持久化文案", async () => {
    const bridge = new FakeSettings();
    bridge.rules = async () => ({ rules: { ...rules(), notify_new_mail: true, muted_accounts: [] }, persisted: true });
    bridge.profileRows = [account(), { ...account(), id: "acct-b", display_name: "个人", address: "me@qq.com" }];
    const c = new SettingsController(bridge);
    c.connection(online);
    c.activate("maild.settings.notifications");
    await c.load();
    await flush();
    expect(c.store.getSnapshot().rulesPersisted).toBe(true);
    let html = renderToStaticMarkup(createElement(Notifications, { controller: c }));
    expect(html).toContain("规则已持久保存，重启后台后继续生效");
    expect(html).not.toContain("当前后台版本尚未提供此开关");
    expect(html).toContain("静音 个人");
    c.setRules({ ...c.store.getSnapshot().rules!, notify_new_mail: false, muted_accounts: ["acct-b"] });
    await c.saveRules();
    expect(bridge.savedRules.at(-1)).toMatchObject({ notify_new_mail: false, muted_accounts: ["acct-b"] });
    expect(c.store.getSnapshot().rulesPersisted).toBe(false);
    html = renderToStaticMarkup(createElement(Notifications, { controller: c }));
    expect(html).toContain("规则仅在当前后台运行期间生效");
  });
});
