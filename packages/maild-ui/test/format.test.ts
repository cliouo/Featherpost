import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";

import { ComposeController } from "../src/compose/controller";
import { Editor } from "../src/compose/Editor";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { Activity } from "../src/mailbox/Activity";
import { ActivityController } from "../src/mailbox/activityController";
import { MailboxController } from "../src/mailbox/controller";
import {
  clockTime,
  dateOnly,
  expiryLabel,
  fullDate,
  isoDateTime,
  localIsoDate,
  MAX_SAFE_UNIX_SECS,
  shortDate,
  toDate,
  UI_LOCALE,
} from "../src/mailbox/format";
import { MessageList } from "../src/mailbox/MessageList";
import { Reader } from "../src/mailbox/Reader";
import { DocumentController } from "../src/mailbox/document";
import { Addresses } from "../src/settings/Addresses";
import { SettingsController } from "../src/settings/controller";
import { FakeBridge, flush, folder, message, online, summary } from "./fakeBridge";
import { FakeCompose } from "./fakeCompose";
import { account, FakeSettings } from "./fakeSettings";

const proc = (globalThis as { process?: { env: Record<string, string | undefined> } }).process;
const initialTz = proc?.env.TZ;

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

afterEach(() => {
  if (!proc) return;
  if (initialTz === undefined) delete proc.env.TZ;
  else proc.env.TZ = initialTz;
});

describe("format.ts：边界与防崩保护", () => {
  it("UI_LOCALE 固定为 zh-CN，toDate 严格拦截 <= 0、非有限值与超上限秒数", () => {
    expect(UI_LOCALE).toBe("zh-CN");
    expect(MAX_SAFE_UNIX_SECS).toBe(8_640_000_000_000);

    for (const bad of [
      null,
      undefined,
      0,
      -0,
      -1,
      -86400,
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
      MAX_SAFE_UNIX_SECS + 1,
      9e15,
    ]) {
      expect(toDate(bad)).toBeNull();
      expect(() => shortDate(bad)).not.toThrow();
      expect(shortDate(bad)).toBe("—");
      expect(() => fullDate(bad)).not.toThrow();
      expect(fullDate(bad)).toBe("—");
      expect(() => isoDateTime(bad)).not.toThrow();
      expect(isoDateTime(bad)).toBeUndefined();
      expect(dateOnly(bad)).toBe("—");
      expect(clockTime(bad)).toBe("");
      expect(localIsoDate(bad)).toBe("");
      expect(expiryLabel(bad)).toBe("");
    }

    expect(toDate(1)?.toISOString()).toBe("1970-01-01T00:00:01.000Z");
    expect(toDate(MAX_SAFE_UNIX_SECS)?.getTime()).toBe(MAX_SAFE_UNIX_SECS * 1000);
    expect(isoDateTime(1_700_000_000)).toBe("2023-11-14T22:13:20.000Z");
  });

  it("西半球时区下 sort_at = 0 返回 '—'，绝不出现 1969/12/31", () => {
    withTz("America/New_York", () => {
      expect(shortDate(0)).toBe("—");
      expect(fullDate(0)).toBe("—");
      expect(dateOnly(0)).toBe("—");
      expect(isoDateTime(0)).toBeUndefined();
    });
  });

  it("系统时区变化时（如最小化期间跨时区出行）惰性缓存自动重绑到新时区", () => {
    // 2026-11-18T00:00:00Z = 北京时间 11/18 08:00 = 纽约时间 11/17 19:00
    const ts = Math.floor(Date.UTC(2026, 10, 18, 0, 0, 0) / 1000);
    const shanghaiClock = withTz("Asia/Shanghai", () => ({
      clock: clockTime(ts),
      ymd: localIsoDate(ts),
      full: fullDate(ts),
    }));
    const newYorkClock = withTz("America/New_York", () => ({
      clock: clockTime(ts),
      ymd: localIsoDate(ts),
      full: fullDate(ts),
    }));
    const shanghaiAgain = withTz("Asia/Shanghai", () => clockTime(ts));

    expect(shanghaiClock.clock).toBe("08:00");
    expect(shanghaiClock.ymd).toBe("2026-11-18");
    expect(newYorkClock.clock).toBe("19:00");
    expect(newYorkClock.ymd).toBe("2026-11-17");
    expect(shanghaiClock.full).not.toBe(newYorkClock.full);
    expect(shanghaiAgain).toBe("08:00");
  });

  it("expiryLabel 在当天（含已过时刻）、次日与 < 48 小时内始终带具体时刻", () => {
    withTz("Asia/Shanghai", () => {
      const now = new Date(2026, 10, 18, 14, 0, 0); // 2026-11-18 14:00
      const sameDayFuture = Math.floor(new Date(2026, 10, 18, 20, 30, 0).getTime() / 1000);
      const sameDayPast = Math.floor(new Date(2026, 10, 18, 8, 15, 0).getTime() / 1000);
      const tomorrow = Math.floor(new Date(2026, 10, 19, 9, 5, 0).getTime() / 1000);
      const yesterday = Math.floor(new Date(2026, 10, 17, 23, 45, 0).getTime() / 1000);
      // 跨两个午夜但距今仅 26 小时（< 48h）：now 为 18 日 23:00，到期为 20 日 01:00
      const lateNow = new Date(2026, 10, 18, 23, 0, 0);
      const twoMidnightsUnder48h = Math.floor(new Date(2026, 10, 20, 1, 0, 0).getTime() / 1000);
      const nextMonth = Math.floor(new Date(2026, 11, 25, 12, 0, 0).getTime() / 1000);

      expect(expiryLabel(sameDayFuture, now)).toBe("今天 20:30 到期");
      expect(expiryLabel(sameDayPast, now)).toBe("今天 08:15 到期");
      expect(expiryLabel(tomorrow, now)).toBe("明天 09:05 到期");
      expect(expiryLabel(yesterday, now)).toBe("昨天 23:45 到期");
      expect(expiryLabel(twoMidnightsUnder48h, lateNow)).toBe("2026/11/20 01:00 到期");
      expect(expiryLabel(nextMonth, now)).toBe("2026/12/25 到期");
    });
  });

  it("所有 6 处界面时间渲染点面对 0 与超上限秒数均不抛错、不渲染 1969/Invalid Date", async () => {
    const huge = MAX_SAFE_UNIX_SECS + 999;
    const bridge = new FakeBridge();
    const mailbox = new MailboxController(bridge);
    mailbox.acceptSnapshot(online);
    for (const r of bridge.folderRequests) r.d.resolve([folder("INBOX", "inbox")]);
    const badZero = { ...summary("INBOX", "1", "零秒邮件"), sort_at: 0, received_at: 0, date_at: null };
    const badHuge = { ...summary("INBOX", "2", "越界邮件"), sort_at: huge, received_at: huge, date_at: huge };
    bridge.mailbox.queries[0]!.d.resolve({ messages: [badZero, badHuge], total: 2 });
    await flush();

    // 1. MessageList (Row & RemoteRow)
    mailbox.store.patch({
      scope: { kind: "search", query: { search: true, query: "test" } },
      remote: {
        status: "ready",
        query: "test",
        accounts: ["demo"],
        rows: [{ ...summary("INBOX", "9", "远端无日期"), sort_at: 0 }],
        fetched: [],
        error: null,
      },
    });
    bridge.mailbox.serverSearch = async () => [];
    const listHtml = renderToStaticMarkup(createElement(MessageList, { controller: mailbox }));
    expect(listHtml).toContain("零秒邮件");
    expect(listHtml).toContain("远端无日期");
    expect(listHtml).not.toContain("1969");
    expect(listHtml).not.toContain("Invalid Date");

    // 2. Reader
    const host = new Host(bridge);
    const docs = new DocumentController(bridge);
    mailbox.selectMessage(badZero);
    bridge.detailRequests.at(-1)!.d.resolve({
      ...message("INBOX", "1", { body: "正文" }),
      sort_at: 0,
      received_at: 0,
      date_at: null,
    });
    await flush();
    const readerHtml = renderToStaticMarkup(
      createElement(Reader, { host, controller: mailbox, documents: docs }),
    );
    expect(readerHtml).toContain("收到 未知");
    expect(readerHtml).not.toContain("1969");

    // 3. Addresses
    const settings = new SettingsController(new FakeSettings());
    settings.connection(online);
    settings.store.patch({
      profiles: [account()],
      addressAccount: "acct-a",
      addresses: {
        total: 2,
        addresses: [
          {
            id: "a1",
            account: "acct-a",
            address: "temp@example.test",
            kind: "disposable",
            label: "测试",
            state: "expired",
            created_at: 0,
            expires_at: huge,
          },
          {
            id: "a2",
            account: "acct-a",
            address: "zero@example.test",
            kind: "disposable",
            label: "零",
            state: "active",
            created_at: 0,
            expires_at: 0,
          },
        ],
      },
    });
    const addrHtml = renderToStaticMarkup(createElement(Addresses, { controller: settings }));
    expect(addrHtml).not.toContain("Invalid Date");
    expect(addrHtml).not.toContain("1969");

    // 4. Activity
    const activity = new ActivityController(bridge.mailbox);
    activity.store.patch({
      online: true,
      ops: [
        {
          id: 1,
          account: "demo",
          folder: "INBOX",
          uid: "1",
          kind: "flag",
          state: "done",
          attempts: 1,
          created_at: huge,
          updated_at: 0,
          detail: "",
          needs_decision: false,
          target: null,
          flag: "seen",
          value: true,
        },
      ],
    });
    const actHtml = renderToStaticMarkup(
      createElement(Activity, { controller: activity, mailbox, openMailbox: () => {} }),
    );
    expect(actHtml).toContain("—");
    expect(actHtml).not.toContain("Invalid Date");

    // 5 & 6. Editor & Drafts list (composeModule)
    const composeBridge = new FakeCompose();
    const composer = new ComposeController(composeBridge, () => {}, () => {});
    composer.setOnline(true);
    await composer.create();
    const snap = composer.store.getSnapshot();
    composer.store.patch({
      draft: { ...snap.draft!, updated_at: huge },
      drafts: [
        {
          id: snap.draft!.id,
          account: snap.draft!.account,
          to: snap.draft!.to,
          subject: snap.draft!.subject,
          snippet: "",
          updated_at: 0,
          version: snap.draft!.version,
          attachment_count: 0,
        },
      ],
      draftTotal: 1,
    });
    const editorHtml = renderToStaticMarkup(createElement(Editor, { controller: composer }));
    expect(editorHtml).toContain("已保存到本机");
    expect(editorHtml).not.toContain("Invalid Date");

    const origWin = (globalThis as { window?: unknown }).window;
    (globalThis as { window?: unknown }).window = {
      addEventListener: () => {},
      removeEventListener: () => {},
    };
    try {
      const bundle = composeModule(host, composer);
      const DraftsView = bundle.views!.find((v) => v.id === "maild.compose.drafts")!.render;
      const draftsHtml = renderToStaticMarkup(createElement(DraftsView, { active: true }));
      expect(draftsHtml).toContain("—");
      expect(draftsHtml).not.toContain("Invalid Date");
      bundle.dispose?.();
    } finally {
      (globalThis as { window?: unknown }).window = origWin;
      composer.stop();
      mailbox.stop();
    }
  });
});
