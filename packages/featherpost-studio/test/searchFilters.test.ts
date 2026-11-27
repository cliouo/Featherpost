// @vitest-environment jsdom
import { applyEffectiveLocales } from "@featherpost/i18n";
import { act, createElement as h, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { ComposeController } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { studioMailbox } from "../src/mailbox";
import { MailboxController, queryOf } from "../src/mailbox/controller";
import {
  activeFilterChips,
  activeFilterCount,
  availableScopeTiers,
  canonicalScopeKey,
  clearStructuredFilters,
  emptyFilterDraft,
  fromMailboxQuery,
  hasActiveSearch,
  hasStructuredFilters,
  localDayEndUnix,
  localDayStartUnix,
  removeFilterChip,
  resolveDatePresetSince,
  toMailboxQuery,
  validateSearchFilterDraft,
} from "../src/searchFilters";
import { SettingsController } from "../src/settings/controller";
import { LanguageController } from "../src/settings/language";
import { SETTINGS_VIEW, settingsModule } from "../src/settings/module";
import { isGlobalShortcutBlocked, Studio } from "../src/Shell";
import { closeAllPopovers } from "../src/ui";
import {
  FakeBridge,
  flush,
  folder,
  online,
  summary,
} from "./fakeBridge";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

beforeAll(() => {
  if (typeof globalThis.matchMedia !== "function") {
    globalThis.matchMedia = () =>
      ({
        matches: false,
        media: "",
        onchange: null,
        addListener() {},
        removeListener() {},
        addEventListener() {},
        removeEventListener() {},
        dispatchEvent: () => false,
      }) as MediaQueryList;
  }
  if (typeof globalThis.ResizeObserver === "undefined") {
    globalThis.ResizeObserver = class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  if (typeof globalThis.PointerEvent === "undefined") {
    class PolyfillPointerEvent extends MouseEvent {
      readonly pointerId: number;
      readonly pointerType: string;
      constructor(type: string, params: PointerEventInit = {}) {
        super(type, params);
        this.pointerId = params.pointerId ?? 1;
        this.pointerType = params.pointerType ?? "mouse";
      }
    }
    globalThis.PointerEvent = PolyfillPointerEvent as unknown as typeof PointerEvent;
  }
  if (!Element.prototype.hasPointerCapture) {
    Element.prototype.hasPointerCapture = () => false;
  }
  if (!Element.prototype.setPointerCapture) {
    Element.prototype.setPointerCapture = () => {};
  }
  if (!Element.prototype.releasePointerCapture) {
    Element.prototype.releasePointerCapture = () => {};
  }
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = () => {};
  }
});

const mountedRoots: Array<{ root: Root; container: HTMLElement }> = [];
const cleanups: Array<() => void> = [];

function mount(node: ReactNode) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(node);
  });
  mountedRoots.push({ root, container });
  return { container };
}

function flushPaint() {
  return act(async () => {
    await new Promise((resolve) =>
      requestAnimationFrame(() => setTimeout(resolve, 0)),
    );
  });
}

function flushTimers(ms = 0) {
  return act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  act(() => {
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

function keydown(
  target: EventTarget,
  key: string,
  init: KeyboardEventInit & { keyCode?: number } = {},
) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  if (init.keyCode !== undefined) {
    Object.defineProperty(event, "keyCode", { value: init.keyCode });
  }
  act(() => {
    target.dispatchEvent(event);
  });
  return event;
}

function createStudioHarness(accounts = online.accounts) {
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const composer = new ComposeController(
    bridge.compose,
    (id) => host.openView(id),
    (text) => host.notify(text),
  );
  const settings = new SettingsController(bridge.settings, () => {});
  const language = new LanguageController(bridge.locale);
  const openSettingsWith = (action: () => void) => {
    if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
      action();
      return;
    }
    host.runAfterLeave(SETTINGS_VIEW, () => {
      if (host.openView(SETTINGS_VIEW)) action();
    });
  };
  const openSettingsSection = (sectionId: string) => {
    if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
      settings.selectSection(sectionId);
      return;
    }
    openSettingsWith(() => {
      settings.activate(sectionId);
    });
  };
  const mailbox = studioMailbox(host, composer, () =>
    openSettingsWith(() => settings.openSetup()),
  );
  const bundles = [
    mailbox.bundle,
    composeModule(host, composer),
    settingsModule(host, settings, language),
  ];
  bundles.forEach((bundle) => host.registerBuiltin(bundle));
  host.finishAssembly();
  host.state.patch({
    snapshot: { ...online, accounts },
  });

  const { container } = mount(
    h(Studio, {
      host,
      mailbox: mailbox.controller,
      composer,
      language,
      setup: mailbox.setup,
      manageAccounts: () => openSettingsWith(() => settings.openAccounts()),
      openSettingsSection,
    }),
  );

  cleanups.push(() => {
    bundles.forEach((bundle) => bundle.dispose?.());
    host.stop();
  });

  return {
    container,
    bridge,
    host,
    mailbox: mailbox.controller,
    composer,
    settings,
  };
}

function getFilterTrigger(container: HTMLElement) {
  return container.querySelector<HTMLButtonElement>("button.search-filter-trigger")!;
}

function getFilterPopover() {
  return document.body.querySelector<HTMLElement>(".search-filter-popover");
}

afterEach(() => {
  while (mountedRoots.length) {
    const entry = mountedRoots.pop()!;
    act(() => {
      entry.root.unmount();
    });
    entry.container.remove();
  }
  while (cleanups.length) {
    cleanups.pop()!();
  }
  closeAllPopovers();
  applyEffectiveLocales([]);
  document.body.innerHTML = "";
  localStorage.clear();
});

describe("pure search filter logic (searchFilters.ts)", () => {
  it("computes DST-safe local date bounds using local next-day 00:00:00 minus 1 second", () => {
    for (const day of ["2026-03-08", "2026-06-15", "2026-11-01"]) {
      const start = localDayStartUnix(day)!;
      const end = localDayEndUnix(day)!;
      expect(start).toBeGreaterThan(0);
      expect(end).toBeGreaterThan(start);

      const [y, mo, d] = day.split("-").map(Number);
      const expectedStart = Math.floor(new Date(y!, mo! - 1, d!, 0, 0, 0, 0).getTime() / 1000);
      const expectedEnd =
        Math.floor(new Date(y!, mo! - 1, d! + 1, 0, 0, 0, 0).getTime() / 1000) - 1;
      expect(start).toBe(expectedStart);
      expect(end).toBe(expectedEnd);
      // Local next day 00:00:00 is strictly end + 1 regardless of 23h/24h/25h DST day.
      expect(end + 1).toBe(Math.floor(new Date(y!, mo! - 1, d! + 1, 0, 0, 0, 0).getTime() / 1000));
    }

    // Rejects invalid / rolled-over / negative pre-1970 dates.
    expect(localDayStartUnix("2026-02-31")).toBeNull();
    expect(localDayEndUnix("not-a-date")).toBeNull();
    expect(localDayStartUnix("1969-01-01")).toBeNull();
  });

  it("validates custom date ranges and detects startDate > endDate", () => {
    const invalid = validateSearchFilterDraft({
      ...emptyFilterDraft("all"),
      datePreset: "custom",
      startDate: "2026-05-10",
      endDate: "2026-05-01",
    });
    expect(invalid.valid).toBe(false);
    expect(invalid.invalidField).toBe("startDate");
    expect(invalid.error).toBeTruthy();

    const sameDay = validateSearchFilterDraft({
      ...emptyFilterDraft("all"),
      datePreset: "custom",
      startDate: "2026-05-10",
      endDate: "2026-05-10",
    });
    expect(sameDay.valid).toBe(true);

    const openEndedStart = validateSearchFilterDraft({
      ...emptyFilterDraft("all"),
      datePreset: "custom",
      startDate: "2026-05-10",
      endDate: "",
    });
    expect(openEndedStart.valid).toBe(true);
  });

  it("recognizes hasActiveSearch for seen: false and all structured filters while ignoring routing fields", () => {
    expect(hasActiveSearch({})).toBe(false);
    expect(hasActiveSearch({ search: true, account: "demo", folder: "INBOX" })).toBe(false);
    expect(hasActiveSearch({ search: true, accounts: ["a", "b"], query: "   " })).toBe(false);

    expect(hasActiveSearch({ query: "hello" })).toBe(true);
    expect(hasActiveSearch({ seen: false })).toBe(true);
    expect(hasActiveSearch({ flagged: true })).toBe(true);
    expect(hasActiveSearch({ has_attachments: true })).toBe(true);
    expect(hasActiveSearch({ from: "alice" })).toBe(true);
    expect(hasActiveSearch({ to: "bob" })).toBe(true);
    expect(hasActiveSearch({ subject: "invoice" })).toBe(true);
    expect(hasActiveSearch({ since: 1700000000 })).toBe(true);
    expect(hasActiveSearch({ until: 1700000000 })).toBe(true);

    expect(hasStructuredFilters({ query: "hello" })).toBe(false);
    expect(hasStructuredFilters({ query: "hello", seen: false })).toBe(true);
  });

  it("normalizes canonicalScopeKey excluding Scope.meta and never leaks meta via queryOf", () => {
    const scopeA = {
      kind: "search" as const,
      query: { account: "demo", from: " alice@example.com ", seen: false, since: 1000 },
      meta: { datePreset: "7d" as const },
    };
    const scopeB = {
      kind: "search" as const,
      query: { seen: false, since: 1000, from: "alice@example.com", account: "demo" },
      meta: undefined,
    };
    expect(canonicalScopeKey(scopeA)).toBe(canonicalScopeKey(scopeB));

    const wire = queryOf(scopeA);
    expect(wire).toEqual({
      search: true,
      account: "demo",
      from: "alice@example.com",
      seen: false,
      since: 1000,
    });
    expect("meta" in wire).toBe(false);
  });

  it("resolves date presets once on Apply and round-trips draft / chips / removal cleanly", () => {
    const fixedNow = new Date(2026, 4, 20, 15, 30, 0, 0);
    const built = toMailboxQuery(
      {
        from: " alice@example.com ",
        to: "cc@example.com",
        subject: "Q2",
        unreadOnly: true,
        flaggedOnly: true,
        hasAttachments: true,
        datePreset: "30d",
        startDate: "",
        endDate: "",
        scope: "account",
      },
      "budget",
      { account: "demo", folderName: "INBOX" },
      fixedNow,
    );
    expect(built.query).toEqual({
      account: "demo",
      query: "budget",
      from: "alice@example.com",
      to: "cc@example.com",
      subject: "Q2",
      seen: false,
      flagged: true,
      has_attachments: true,
      since: resolveDatePresetSince("30d", fixedNow),
    });
    expect(built.meta).toEqual({ datePreset: "30d" });
    expect(activeFilterCount(built.query)).toBe(7);

    const hydrated = fromMailboxQuery(built.query, built.meta, "account");
    expect(hydrated).toEqual({
      from: "alice@example.com",
      to: "cc@example.com",
      subject: "Q2",
      unreadOnly: true,
      flaggedOnly: true,
      hasAttachments: true,
      datePreset: "30d",
      startDate: "",
      endDate: "",
      scope: "account",
    });

    const chips = activeFilterChips(built.query, built.meta);
    expect(chips.map((c) => c.id)).toEqual([
      "from",
      "to",
      "subject",
      "seen",
      "flagged",
      "has_attachments",
      "date",
    ]);

    const withoutDate = removeFilterChip(built.query, built.meta, "date");
    expect(withoutDate.query.since).toBeUndefined();
    expect(withoutDate.meta).toBeUndefined();

    const cleared = clearStructuredFilters(built.query);
    expect(cleared).toEqual({ account: "demo", query: "budget" });
  });

  it("computes Scheme A 3-tier scope options per view context", () => {
    expect(availableScopeTiers({ account: "demo", folderName: "Archive" })).toEqual([
      "folder",
      "account",
      "all",
    ]);
    expect(availableScopeTiers({ accounts: ["a", "b"] })).toEqual(["group", "all"]);
    expect(availableScopeTiers({})).toEqual(["all"]);
  });

  it("reuses controller cache across meta-only differences without refetching", async () => {
    const bridge = new FakeBridge();
    const controller = new MailboxController(bridge);
    try {
      controller.acceptSnapshot(online);
      bridge.folderRequests.forEach((r) => r.d.resolve([folder("INBOX", "inbox")]));
      bridge.mailbox.queries[0]!.d.resolve({ messages: [], total: 0 });
      await flush();

      controller.search({ from: "alice", since: 1000 }, { datePreset: "7d" });
      bridge.mailbox.queries.at(-1)!.d.resolve({
        messages: [summary("INBOX", "m1", "Match 1")],
        total: 1,
      });
      await flush();
      expect(controller.state.rows.map((r) => r.uid)).toEqual(["m1"]);

      // Re-entering same canonical query with different meta immediately hits cached rows.
      controller.search({ from: "alice", since: 1000 }, undefined);
      expect(controller.state.rows.map((r) => r.uid)).toEqual(["m1"]);
    } finally {
      controller.stop();
    }
  });
});

describe("SearchFilterPopover, 3-tier scope sync, chip bar, and guarded submit path", () => {
  it("opens Popover with zero RPCs, supports IME/Esc/Cancel/Reset/windowDismiss, validates invalid custom dates, and applies filters cleanly", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox"), folder("Archive", "archive")]);
    }
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [], total: 0 });
    await act(async () => {
      await flush();
    });

    // Navigate to a Standard folder so all 3 scope tiers are available.
    act(() => {
      app.mailbox.selectFolder(folder("Archive", "archive"));
    });
    app.bridge.pageRequests.at(-1)?.d.resolve({ messages: [], total: 0 });
    await act(async () => {
      await flush();
    });

    const queriesBefore = app.bridge.mailbox.queries.length;
    const trigger = getFilterTrigger(app.container);
    expect(trigger).not.toBeNull();
    expect(trigger.getAttribute("data-tauri-drag-region")).toBe("false");
    expect(trigger.getAttribute("title")).toBeTruthy();

    // Topbar Choice exposes all 3 tiers: folder, account, all.
    const scopeSelect = app.container.querySelector<HTMLSelectElement>(
      '.studio-search select[aria-label="搜索范围"]',
    )!;
    expect(Array.from(scopeSelect.options).map((o) => o.value)).toEqual([
      "folder",
      "account",
      "all",
    ]);

    // 1. Open Popover and edit draft -> ZERO RPCs!
    act(() => {
      trigger.click();
    });
    await flushPaint();
    const popover = getFilterPopover()!;
    expect(popover).not.toBeNull();
    expect(popover.hasAttribute("data-shortcuts-off")).toBe(true);
    expect(popover.getAttribute("data-tauri-drag-region")).toBe("false");

    const fromInput = popover.querySelector<HTMLInputElement>('input[name="from"]')!;
    setInputValue(fromInput, "alice@example.com");
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);

    // 2. IME Enter while composing MUST NOT submit the form!
    const imeEv = keydown(fromInput, "Enter", { isComposing: true, keyCode: 229 });
    expect(imeEv.defaultPrevented).toBe(true);
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
    expect(getFilterPopover()).not.toBeNull();

    // 3. Cancel via Escape discards draft and fires zero RPCs.
    keydown(fromInput, "Escape");
    await flushTimers();
    expect(getFilterPopover()).toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);

    // 4. Reopen -> draft was re-hydrated clean; window blur dismisses cleanly.
    act(() => {
      trigger.click();
    });
    await flushPaint();
    expect(getFilterPopover()!.querySelector<HTMLInputElement>('input[name="from"]')!.value).toBe("");
    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    await flushTimers();
    expect(getFilterPopover()).toBeNull();

    // 5. Reopen, enter invalid custom date range -> inline role="alert", no close, zero RPCs.
    act(() => {
      trigger.click();
    });
    await flushPaint();
    const pop = getFilterPopover()!;
    const customDateBtn = Array.from(pop.querySelectorAll<HTMLButtonElement>("button")).find(
      (b) => b.textContent === "自定义",
    )!;
    act(() => {
      customDateBtn.click();
    });
    await flushTimers();
    const startInput = pop.querySelector<HTMLInputElement>('input[name="startDate"]')!;
    const endInput = pop.querySelector<HTMLInputElement>('input[name="endDate"]')!;
    setInputValue(startInput, "2026-06-20");
    setInputValue(endInput, "2026-06-10");

    const applyBtn = pop.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    act(() => {
      applyBtn.click();
    });
    await flushTimers();
    const alert = pop.querySelector('[role="alert"]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain("开始日期不能晚于结束日期");
    expect(document.activeElement).toBe(startInput);
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
    expect(getFilterPopover()).not.toBeNull();

    // 6. Reset clears draft fields without firing RPCs.
    const resetBtn = pop.querySelector<HTMLButtonElement>("button.search-filter-reset")!;
    act(() => {
      resetBtn.click();
    });
    await flushTimers();
    expect(pop.querySelector('[role="alert"]')).toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);

    // 7. Fill structured filters + select "Current account" tier -> Apply executes ONCE!
    setInputValue(pop.querySelector<HTMLInputElement>('input[name="from"]')!, "alice@example.com");
    setInputValue(pop.querySelector<HTMLInputElement>('input[name="to"]')!, "bob@example.com");
    setInputValue(pop.querySelector<HTMLInputElement>('input[name="subject"]')!, "Invoice");
    const switches = Array.from(pop.querySelectorAll<HTMLButtonElement>('[role="switch"]'));
    expect(switches).toHaveLength(3);
    act(() => {
      switches[0]!.click(); // Unread only (seen: false)
      switches[2]!.click(); // Has attachments
    });
    const accountScopeBtn = Array.from(
      pop.querySelectorAll<HTMLButtonElement>(".search-filter-segmented button"),
    ).find((b) => b.textContent === "当前账户")!;
    act(() => {
      accountScopeBtn.click();
    });
    act(() => {
      applyBtn.click();
    });
    await flushTimers();

    expect(getFilterPopover()).toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore + 1);
    expect(app.bridge.mailbox.queries.at(-1)!.query).toEqual({
      search: true,
      account: "demo",
      from: "alice@example.com",
      to: "bob@example.com",
      subject: "Invoice",
      seen: false,
      has_attachments: true,
    });
    // Topbar Choice stayed in sync with Popover's "account" tier!
    expect(scopeSelect.value).toBe("account");
    // Filter badge shows 5 active filters and trigger has .is-active.
    expect(trigger.classList.contains("is-active")).toBe(true);
    expect(trigger.querySelector(".search-filter-badge")?.textContent).toBe("5");
    // Topbar shows clear × button even though keyword is empty!
    expect(app.container.querySelector('button[aria-label="清除搜索"]')).not.toBeNull();

    // 8. Active filter chips bar renders inside .list-heading with NO data-shortcuts-off!
    const chipBar = app.container.querySelector<HTMLElement>(".list-heading .search-filter-chips")!;
    expect(chipBar).not.toBeNull();
    expect(chipBar.hasAttribute("data-shortcuts-off")).toBe(false);
    expect(chipBar.querySelector("[data-shortcuts-off]")).toBeNull();
    // Global shortcuts remain UNBLOCKED while chip bar is permanently visible!
    expect(isGlobalShortcutBlocked({ defaultPrevented: false, target: document.body })).toBe(false);

    const chipEls = Array.from(chipBar.querySelectorAll(".search-filter-chip"));
    expect(chipEls).toHaveLength(5);

    // Clicking a chip body re-opens the Popover.
    act(() => {
      chipEls[0]!.querySelector<HTMLButtonElement>(".search-filter-chip-body")!.click();
    });
    await flushPaint();
    expect(getFilterPopover()).not.toBeNull();
    keydown(getFilterPopover()!, "Escape");
    await flushTimers();

    // Clicking chip remove × removes only that filter and re-searches.
    const removeFromBtn = chipBar.querySelector<HTMLButtonElement>(
      'button.search-filter-chip-remove[aria-label*="发件人"]',
    )!;
    act(() => {
      removeFromBtn.click();
    });
    await flushTimers();
    expect(app.bridge.mailbox.queries.at(-1)!.query).toEqual({
      search: true,
      account: "demo",
      to: "bob@example.com",
      subject: "Invoice",
      seen: false,
      has_attachments: true,
    });
    expect(trigger.querySelector(".search-filter-badge")?.textContent).toBe("4");

    // Clicking "Clear filters" with no keyword exits search cleanly back to Archive folder!
    const clearFiltersBtn = app.container.querySelector<HTMLButtonElement>(
      "button.search-filter-chips-clear",
    )!;
    act(() => {
      clearFiltersBtn.click();
    });
    await flushTimers();
    expect(app.mailbox.store.getSnapshot().scope).toMatchObject({
      kind: "folder",
      account: "demo",
      folder: "Archive",
    });
    expect(app.container.querySelector(".search-filter-chips")).toBeNull();
    expect(app.container.querySelector(".filter-tabs")).not.toBeNull();
  });

  it("enforces Single Guarded Submit Path on dirty Settings: Stay = 0 RPC and state unchanged; Confirm = applies once", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [], total: 0 });
    await act(async () => {
      await flush();
    });

    // Open dirty Settings (network proxy section).
    act(() => {
      app.host.openView(SETTINGS_VIEW);
      app.settings.activate("maild.settings.network");
    });
    await act(async () => {
      await flush();
    });
    act(() => {
      app.settings.store.patch({
        proxyDraft: {
          mode: "custom",
          protocol: "socks5",
          host: "127.0.0.1",
          port: "1080",
          authEnabled: false,
          username: "",
          password: "",
          clearPassword: false,
          bypassText: "",
        },
        proxyBaseline: JSON.stringify({
          mode: "direct",
          protocol: "http",
          host: "",
          port: "",
          authEnabled: false,
          username: "",
          password: "",
          clearPassword: false,
          bypassText: "",
        }),
      });
    });
    expect(app.settings.dirty).toBe(true);
    const queriesBefore = app.bridge.mailbox.queries.length;

    // Open Popover, fill filter, click Apply -> Popover closes FIRST, confirm dialog appears, 0 RPCs!
    const trigger = getFilterTrigger(app.container);
    act(() => {
      trigger.click();
    });
    await flushPaint();
    const pop = getFilterPopover()!;
    setInputValue(pop.querySelector<HTMLInputElement>('input[name="from"]')!, "boss@corp.test");
    act(() => {
      pop.querySelector<HTMLButtonElement>('button[type="submit"]')!.click();
    });
    await flushTimers();

    expect(getFilterPopover()).toBeNull();
    expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
    expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);

    // Click "Stay" -> zero RPCs, mailbox still not in search scope, dirty Settings preserved!
    act(() => {
      app.settings.cancelConfirmation();
    });
    await flushTimers();
    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
    expect(app.mailbox.store.getSnapshot().scope.kind).not.toBe("search");
    expect(app.settings.store.getSnapshot().proxyDraft?.host).toBe("127.0.0.1");

    // Reopen, apply, and confirm discard -> runs exactly 1 search query!
    act(() => {
      trigger.click();
    });
    await flushPaint();
    const pop2 = getFilterPopover()!;
    setInputValue(pop2.querySelector<HTMLInputElement>('input[name="from"]')!, "boss@corp.test");
    act(() => {
      pop2.querySelector<HTMLButtonElement>('button[type="submit"]')!.click();
    });
    await flushTimers();
    act(() => {
      app.settings.confirm();
    });
    await flushTimers();

    expect(app.bridge.mailbox.queries.length).toBe(queriesBefore + 1);
    expect(app.bridge.mailbox.queries.at(-1)!.query).toEqual({
      search: true,
      from: "boss@corp.test",
    });
  });

  it("preserves current applied scope & structured filters in Command Palette fallback search and opens Popover via palette action", async () => {
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox"), folder("Work", "")]);
    }
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [], total: 0 });
    await act(async () => {
      await flush();
    });

    act(() => {
      app.mailbox.selectFolder(folder("Work", ""));
    });
    await act(async () => {
      await flush();
    });

    // Apply structured filter in account scope.
    act(() => {
      app.mailbox.search({ account: "demo", from: "alice@example.com", has_attachments: true });
    });
    await act(async () => {
      await flush();
    });

    // Open Command Palette and execute fallback mail search -> preserves account + from + has_attachments!
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    const paletteInput = document.body.querySelector<HTMLInputElement>("input.command-palette-input")!;
    setInputValue(paletteInput, "quarterly-xyz");
    await flushTimers();
    keydown(paletteInput, "Enter");
    await flushTimers();

    expect(app.mailbox.store.getSnapshot().scope).toEqual({
      kind: "search",
      query: {
        search: true,
        account: "demo",
        query: "quarterly-xyz",
        from: "alice@example.com",
        has_attachments: true,
      },
    });

    // Command Palette "高级搜索筛选" action opens the SearchFilterPopover!
    keydown(window, "k", { metaKey: true });
    await flushPaint();
    const paletteInput2 = document.body.querySelector<HTMLInputElement>("input.command-palette-input")!;
    setInputValue(paletteInput2, "高级搜索筛选");
    await flushTimers();
    keydown(paletteInput2, "Enter");
    await flushPaint();
    expect(getFilterPopover()).not.toBeNull();
  });

  it("renders zero CJK characters in English locale across Popover and active filter chips", async () => {
    applyEffectiveLocales(["en"]);
    const app = createStudioHarness();
    for (const req of app.bridge.folderRequests) {
      req.d.resolve([folder("INBOX", "inbox")]);
    }
    app.bridge.mailbox.queries[0]?.d.resolve({ messages: [], total: 0 });
    await act(async () => {
      await flush();
    });

    act(() => {
      app.mailbox.search(
        { from: "alice@example.com", seen: false, flagged: true, has_attachments: true, since: 1700000000 },
        { datePreset: "7d" },
      );
    });
    await act(async () => {
      await flush();
    });

    const chipBar = app.container.querySelector<HTMLElement>(".search-filter-chips")!;
    expect(chipBar.textContent).toContain("From: alice@example.com");
    expect(chipBar.textContent).toContain("Unread only");
    expect(chipBar.textContent).toContain("Starred only");
    expect(chipBar.textContent).toContain("Has attachments");
    expect(chipBar.textContent).toContain("Last 7 days");
    expect(chipBar.textContent).not.toMatch(/[\u4e00-\u9fff]/);

    act(() => {
      getFilterTrigger(app.container).click();
    });
    await flushPaint();
    const pop = getFilterPopover()!;
    expect(pop.textContent).toContain("Advanced filters");
    expect(pop.textContent).toContain("Apply");
    expect(pop.textContent).not.toMatch(/[\u4e00-\u9fff]/);
  });
});
