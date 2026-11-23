import * as ts from "typescript";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Workspace } from "../../featherpost-studio/src/Workspace";
import { ContextMenuHost } from "../../featherpost-studio/src/ContextMenu";
import { StudioActionsContext } from "../../featherpost-studio/src/studioActions";
import { ComposeController } from "../src/compose/controller";
import { Host } from "../src/host/host";
import { MailboxController } from "../src/mailbox/controller";
import { DocumentController } from "../src/mailbox/document";
import { MessageList } from "../src/mailbox/MessageList";
import {
  SCROLL_DURATION_MS,
  computeRevealScrollTop,
  createListScroller,
  measureRevealScrollTop,
  measureStickyInsets,
  type ScrollSurface,
} from "../src/mailbox/listScroll";
import { FakeBridge, flush, folder, online, summary } from "./fakeBridge";

const readAsset = (rel: string) =>
  ts.sys.readFile(
    decodeURIComponent(new URL(rel, import.meta.url).pathname).replace(
      /^\/([A-Za-z]:)/,
      "$1",
    ),
  ) ?? "";
const studioCss = readAsset("../../featherpost-studio/src/studio.css");
const appCss = readAsset("../src/styles/app.css");
const mailboxCss = readAsset("../src/mailbox/mailbox.css");

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("computeRevealScrollTop (issue #23)", () => {
  it("returns null when the selected row is already fully visible", () => {
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 22,
        itemBottom: 80,
      }),
    ).toBeNull();
    expect(
      computeRevealScrollTop({
        scrollTop: 100,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 100,
        itemBottom: 600,
      }),
    ).toBeNull();
  });

  it("scrolls minimally when moving Down past the bottom of the viewport", () => {
    // 500px viewport: rows 0..7 fit, row 8 is 486..544 -> align bottom to 544 (scrollTop = 44)
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 486,
        itemBottom: 544,
      }),
    ).toBe(44);
    // Next row down (544..602) from scrollTop = 44 -> scrollTop = 102 (+58px)
    expect(
      computeRevealScrollTop({
        scrollTop: 44,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 544,
        itemBottom: 602,
      }),
    ).toBe(102);
  });

  it("scrolls minimally when moving Up past the top of the viewport", () => {
    expect(
      computeRevealScrollTop({
        scrollTop: 200,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 142,
        itemBottom: 200,
      }),
    ).toBe(142);
    // Partially clipped top row (180..238) -> minimal scroll up to 180
    expect(
      computeRevealScrollTop({
        scrollTop: 200,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 180,
        itemBottom: 238,
      }),
    ).toBe(180);
  });

  it("accounts for sticky top and bottom bars so the selected row is never obscured", () => {
    // Sticky tab bar (36px) + sticky date header (22px) = 58px top inset.
    // Viewport shows content [258..700], so a row at 220..278 is partially hidden behind sticky headers.
    expect(
      computeRevealScrollTop({
        scrollTop: 200,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 220,
        itemBottom: 278,
        topInset: 58,
      }),
    ).toBe(162);

    // Sticky bottom bar (30px): visibleBottom is scrollTop + 470.
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 450,
        itemBottom: 508,
        bottomInset: 30,
      }),
    ).toBe(38);
  });

  it("reveals a preceding non-sticky date group header when scrolling Up, without shifting already-visible clicks", () => {
    // Row 0 is at 22..80 with "Today" date-group at 0..22.
    // Scrolling up from scrollTop=80 reveals the date-group too -> scrollTop=0.
    expect(
      computeRevealScrollTop({
        scrollTop: 80,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 22,
        itemBottom: 80,
        groupHeaderTop: 0,
      }),
    ).toBe(0);

    // When scrollTop=22, row 0 itself fits at top, while "Today" (0..22) is clipped just above.
    // Keyboard Up reveals "Today"; mouse click does not shift the row under the cursor.
    expect(
      computeRevealScrollTop({
        scrollTop: 22,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 22,
        itemBottom: 80,
        groupHeaderTop: 0,
        revealGroupHeader: true,
      }),
    ).toBe(0);
    expect(
      computeRevealScrollTop({
        scrollTop: 22,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 22,
        itemBottom: 80,
        groupHeaderTop: 0,
        revealGroupHeader: false,
      }),
    ).toBeNull();
  });

  it("handles crossing date group headers downward and upward", () => {
    // "Earlier" header at 486..508, first "Earlier" row at 508..566.
    // Moving Down reveals both the group header and full row (scrollTop = 66).
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 508,
        itemBottom: 566,
        groupHeaderTop: 486,
      }),
    ).toBe(66);

    // Moving Up back to last "Today" row (428..486) from scrollTop=508 -> scrollTop = 428.
    expect(
      computeRevealScrollTop({
        scrollTop: 508,
        viewportHeight: 500,
        scrollHeight: 3000,
        itemTop: 428,
        itemBottom: 486,
      }),
    ).toBe(428);
  });

  it("handles hidden viewports, clamping, and items taller than the viewport", () => {
    expect(
      computeRevealScrollTop({
        scrollTop: 100,
        viewportHeight: 0,
        scrollHeight: 3000,
        itemTop: 200,
        itemBottom: 258,
      }),
    ).toBeNull();

    // Clamps to maxScrollTop (1000 - 500 = 500).
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 500,
        scrollHeight: 1000,
        itemTop: 960,
        itemBottom: 1020,
      }),
    ).toBe(500);

    // Item taller than viewport aligns to topInset.
    expect(
      computeRevealScrollTop({
        scrollTop: 0,
        viewportHeight: 80,
        scrollHeight: 1000,
        itemTop: 200,
        itemBottom: 320,
        topInset: 20,
      }),
    ).toBe(180);
  });
});

describe("measureStickyInsets & measureRevealScrollTop", () => {
  const FOLLOWING = 4;
  const PRECEDING = 2;

  function mockNode(opts: {
    classes?: string[];
    top: number;
    height: number;
    position?: string;
    stickyTop?: string;
    stickyBottom?: string;
    scrollPaddingTop?: string;
    scrollPaddingBottom?: string;
    order?: number;
    previous?: unknown;
  }) {
    const classSet = new Set(opts.classes ?? []);
    const node = {
      classList: { contains: (c: string) => classSet.has(c) },
      getBoundingClientRect: () => ({
        top: opts.top,
        bottom: opts.top + opts.height,
        height: opts.height,
      }),
      contains: (other: unknown) => other === node,
      compareDocumentPosition: (other: { __order?: number }) => {
        const a = opts.order ?? 0;
        const b = other.__order ?? 0;
        if (a < b) return FOLLOWING;
        if (a > b) return PRECEDING;
        return 0;
      },
      previousElementSibling: opts.previous ?? null,
      __order: opts.order ?? 0,
      __style: {
        position: opts.position ?? "static",
        top: opts.stickyTop ?? "auto",
        bottom: opts.stickyBottom ?? "auto",
        scrollPaddingTop: opts.scrollPaddingTop ?? "auto",
        scrollPaddingBottom: opts.scrollPaddingBottom ?? "auto",
      },
    };
    return node;
  }

  it("computes stacked sticky top insets without double-counting overlapping date headers", () => {
    vi.stubGlobal("Node", {
      DOCUMENT_POSITION_FOLLOWING: FOLLOWING,
      DOCUMENT_POSITION_PRECEDING: PRECEDING,
    });
    vi.stubGlobal("HTMLElement", Object);

    const stickyTabs = mockNode({
      classes: ["filter-tabs"],
      top: 0,
      height: 36,
      position: "sticky",
      stickyTop: "0px",
      order: 1,
    });
    const stickyToday = mockNode({
      classes: ["date-group"],
      top: 36,
      height: 22,
      position: "sticky",
      stickyTop: "36px",
      order: 2,
    });
    const stickyEarlier = mockNode({
      classes: ["date-group"],
      top: 36,
      height: 22,
      position: "sticky",
      stickyTop: "36px",
      order: 4,
    });
    const stickyAfter = mockNode({
      classes: ["date-group"],
      top: 36,
      height: 80,
      position: "sticky",
      stickyTop: "36px",
      order: 10,
    });
    const target = mockNode({
      classes: ["message-row"],
      top: 40, // 18px obscured behind 36+22=58 sticky stack!
      height: 58,
      order: 5,
      previous: stickyEarlier,
    });

    const container = {
      scrollTop: 200,
      clientTop: 0,
      clientHeight: 500,
      scrollHeight: 2000,
      children: [stickyTabs, stickyToday, stickyEarlier, target, stickyAfter],
      querySelectorAll: () => [stickyTabs, stickyToday, stickyEarlier, stickyAfter],
      getBoundingClientRect: () => ({ top: 0, bottom: 500, height: 500 }),
      __style: {
        scrollPaddingTop: "0px",
        scrollPaddingBottom: "0px",
      },
    };

    vi.stubGlobal("window", {
      getComputedStyle: (el: { __style: Record<string, string> }) => el.__style,
    });

    // stickyAfter follows target so its 80px height must be ignored;
    // stickyToday and stickyEarlier share top: 36px so max(36, 36+22) = 58 (not 36+22+22=80).
    expect(
      measureStickyInsets(
        container as unknown as HTMLElement,
        target as unknown as HTMLElement,
      ),
    ).toEqual({ topInset: 58, bottomInset: 0 });

    // target content top is 200 + 40 = 240; with topInset=58 -> scrollTop = 240 - 58 = 182.
    expect(
      measureRevealScrollTop(
        container as unknown as HTMLElement,
        target as unknown as HTMLElement,
      ),
    ).toBe(182);
  });
});

describe("createListScroller (issue #24)", () => {
  function fakeClock() {
    let time = 1000;
    let nextId = 1;
    const frames = new Map<number, (t: number) => void>();
    const cancelled: number[] = [];
    return {
      now: () => time,
      requestFrame: (cb: (t: number) => void) => {
        const id = nextId++;
        frames.set(id, cb);
        return id;
      },
      cancelFrame: (id: number) => {
        if (frames.delete(id)) cancelled.push(id);
      },
      advance(ms: number) {
        time += ms;
      },
      step(ms: number) {
        time += ms;
        const batch = [...frames.entries()];
        frames.clear();
        for (const [, cb] of batch) cb(time);
      },
      pending: () => frames.size,
      cancelled,
    };
  }

  function surface(scrollTop = 0): ScrollSurface {
    return { scrollTop, clientHeight: 500, scrollHeight: 3000 };
  }

  it("eases smoothly over ~120ms on single steps and reaches the exact target", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => false,
    });

    scroller.scrollTo(list, 100);
    expect(clock.pending()).toBe(1);

    clock.step(16);
    expect(list.scrollTop).toBeGreaterThan(25);
    expect(list.scrollTop).toBeLessThan(100);

    clock.step(SCROLL_DURATION_MS);
    expect(list.scrollTop).toBe(100);
    expect(clock.pending()).toBe(0);
  });

  it("cancels prior animations and adapts during key repeat without queuing or lagging", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => false,
    });

    // Simulate holding Down at 33ms repeat interval across 5 rows (58px each).
    for (let i = 1; i <= 5; i++) {
      scroller.scrollTo(list, i * 58, { repeat: true });
      // Never more than 1 frame queued at a time.
      expect(clock.pending()).toBeLessThanOrEqual(1);
      clock.step(16);
      clock.step(17);
    }

    // By the end of the 5th repeat interval, scroll has caught up completely.
    expect(list.scrollTop).toBe(5 * 58);
    expect(clock.pending()).toBe(0);
  });

  it("snaps immediately under ultra-fast key repeat (<= 20ms) to prevent lag", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => false,
    });

    scroller.scrollTo(list, 58, { repeat: true });
    clock.step(15);
    scroller.scrollTo(list, 116, { repeat: true });
    expect(list.scrollTop).toBe(116);
    expect(clock.pending()).toBe(0);
  });

  it("uses instant scroll with zero animation frames when prefers-reduced-motion is set", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => true,
    });

    scroller.scrollTo(list, 240);
    expect(list.scrollTop).toBe(240);
    expect(clock.pending()).toBe(0);
  });

  it("stops immediately if the user manually scrolls mid-animation", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => false,
    });

    scroller.scrollTo(list, 300);
    clock.step(16);
    expect(list.scrollTop).toBeGreaterThan(0);

    // User grabs scrollbar / trackpad and scrolls to 50.
    list.scrollTop = 50;
    clock.step(16);
    expect(list.scrollTop).toBe(50);
    expect(clock.pending()).toBe(0);
  });

  it("retargets in place without cancelling and starving the pending animation frame", () => {
    const clock = fakeClock();
    const list = surface(0);
    const scroller = createListScroller({
      now: clock.now,
      requestFrame: clock.requestFrame,
      cancelFrame: clock.cancelFrame,
      prefersReducedMotion: () => false,
    });

    scroller.scrollTo(list, 58, { repeat: false });
    expect(clock.pending()).toBe(1);
    expect(clock.cancelled).toHaveLength(0);

    // Second retarget arrives before the first animation frame fires (e.g. 25ms gap).
    // The pending frame must stay alive instead of being cancelled and pushed back.
    clock.advance(25);
    scroller.scrollTo(list, 116, { repeat: true });
    expect(clock.pending()).toBe(1);
    expect(clock.cancelled).toHaveLength(0);

    clock.step(10);
    expect(list.scrollTop).toBeGreaterThan(0);
    expect(list.scrollTop).toBeLessThan(116);
    clock.step(60);
    expect(list.scrollTop).toBe(116);
  });
});

describe("selection transition CSS & SSR rendering (issues #23 & #24)", () => {
  it("defines 100-150ms selection transitions and disables them under prefers-reduced-motion", () => {
    // Studio: background and ::before left accent bar transitions + reduced-motion disable
    expect(studioCss).toMatch(/\.message-row\s*\{[^}]*transition:\s*background-color\s+120ms/s);
    expect(studioCss).toMatch(/\.message-row::before\s*\{[^}]*transition:[^}]*opacity\s+120ms[^}]*transform\s+120ms/s);
    expect(studioCss).toMatch(
      /@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{[^}]*\.message-row,\s*\.message-row::before[^}]*transition:\s*none;/s,
    );

    // maild-ui: background/border transition + global reduced-motion rule
    expect(appCss).toMatch(/\.message-row\s*\{[^}]*transition:\s*background-color\s+120ms/s);
    expect(appCss).toMatch(/@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{\s*\*\s*\{\s*transition:\s*none\s*!important;\s*\}\s*\}/);
    expect(mailboxCss).toMatch(/\.remote-mark\s*\{[^}]*transition:\s*background-color\s+120ms/s);
  });

  it("renders Studio Workspace and legacy MessageList cleanly across tabs and folders", async () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const controller = new MailboxController(bridge);
    const documents = new DocumentController(bridge);
    const composer = new ComposeController(bridge.compose, () => {}, () => {});
    try {
      controller.acceptSnapshot(online);
      bridge.folderRequests.forEach((r) =>
        r.d.resolve([folder("INBOX", "inbox", 2), folder("Archive", "archive")]),
      );
      const nowSecs = Math.floor(Date.now() / 1000);
      const rows = [
        { ...summary("INBOX", "1", "Today mail"), seen: false, sort_at: nowSecs },
        { ...summary("INBOX", "2", "Earlier mail"), seen: true, sort_at: nowSecs - 86400 * 3 },
      ];
      bridge.mailbox.queries[0]!.d.resolve({ messages: rows, total: 2 });
      await flush();
      controller.selectMessage(controller.state.rows[0]!);

      const renderStudio = () =>
        renderToStaticMarkup(
          createElement(
            StudioActionsContext.Provider,
            { value: { search: () => {}, copy: () => {} } },
            createElement(ContextMenuHost, {
              selection: () => [],
              children: createElement(Workspace, {
                host,
                controller,
                documents,
                composer,
                setup: () => {},
              }),
            }),
          ),
        );

      const allHtml = renderStudio();
      expect(allHtml).toContain("message-scroll");
      expect(allHtml).toContain("date-group");
      expect(allHtml).toContain("message-row selected");

      controller.setUnreadFilter(true);
      bridge.mailbox.queries.at(-1)!.d.resolve({ messages: [rows[0]!], total: 1 });
      await flush();
      controller.selectMessage(controller.state.rows[0]!);
      const unreadHtml = renderStudio();
      expect(unreadHtml).toContain("message-row selected");

      controller.selectFolder(controller.foldersOf("demo")[0]!);
      bridge.pageRequests.at(-1)!.d.resolve({ messages: rows, total: 2 });
      await flush();
      controller.selectMessage(controller.state.rows[1]!);
      const folderHtml = renderStudio();
      expect(folderHtml).toContain("message-row selected");

      const legacyHtml = renderToStaticMarkup(
        createElement(MessageList, { controller }),
      );
      expect(legacyHtml).toContain("message-list");
      expect(legacyHtml).toContain("message-row selected");
    } finally {
      controller.stop();
      documents.stop();
      host.stop();
    }
  });
});
