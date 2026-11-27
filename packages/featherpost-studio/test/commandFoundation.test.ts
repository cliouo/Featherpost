import { applyEffectiveLocales } from "@featherpost/i18n";
import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ComposeController, COMPOSE_VIEW, DRAFTS_VIEW } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { Host } from "../src/host/host";
import { ACTIVITY, MAIL, studioMailbox } from "../src/mailbox";
import { SettingsController } from "../src/settings/controller";
import { SETTINGS_VIEW, settingsModule } from "../src/settings/module";
import { Studio } from "../src/Shell";
import { FakeBridge, flush, folder, message, online, summary } from "./fakeBridge";
import { account } from "./fakeSettings";

function capture(render: () => ReactNode) {
  let root: ReactNode;
  function Probe() {
    root = render();
    return root;
  }
  const html = renderToStaticMarkup(createElement(Probe));
  const elements: ReactElement<Record<string, unknown>>[] = [];
  const visit = (node: ReactNode) => {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node || typeof node !== "object" || !("props" in node)) return;
    const el = node as ReactElement<Record<string, unknown>>;
    elements.push(el);
    visit(el.props.children as ReactNode);
  };
  visit(root!);
  return { html, elements };
}

function assembleStudio(accounts = online.accounts) {
  vi.stubGlobal("window", {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const composer = new ComposeController(
    bridge.compose,
    (id) => host.openView(id),
    (text) => host.notify(text),
  );
  const settings = new SettingsController(bridge.settings, () => {});
  const openSettingsWith = (action: () => void) => {
    if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
      action();
      return;
    }
    host.runAfterLeave(SETTINGS_VIEW, () => {
      if (host.openView(SETTINGS_VIEW)) action();
    });
  };
  const mailbox = studioMailbox(host, composer, () =>
    openSettingsWith(() => settings.openSetup()),
  );
  const bundles = [
    mailbox.bundle,
    composeModule(host, composer),
    settingsModule(host, settings),
  ];
  bundles.forEach((bundle) => host.registerBuiltin(bundle));
  host.finishAssembly();
  host.state.patch({
    snapshot: { ...online, accounts },
  });
  return {
    bridge,
    host,
    composer,
    settings,
    mailbox: mailbox.controller,
    setup: mailbox.setup,
    dispose: () => {
      bundles.forEach((bundle) => bundle.dispose?.());
      host.stop();
    },
  };
}

afterEach(() => {
  applyEffectiveLocales([]);
  vi.unstubAllGlobals();
});

describe("PR 1：脏设置离开守卫（先获准离开再产生副作用）", () => {
  it("脏设置页触发写信并选择「留下」时：零草稿创建、零视图切换、表单输入完整保留；确认后仅创建一份草稿", async () => {
    const app = assembleStudio();
    try {
      await flush();
      hostOpenSettingsDirtyAccount(app);

      // Trigger via built-in command maild.compose.new while Settings is dirty.
      app.host.execute("maild.compose.new");
      await flush();

      expect(app.settings.store.getSnapshot().confirm?.title).toBe("放弃未保存的设置？");
      expect(app.bridge.compose.rows.size).toBe(0);
      expect(app.composer.store.getSnapshot().draft).toBeNull();
      expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);

      // Choose "Stay" -> zero drafts, dirty fields preserved.
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.compose.rows.size).toBe(0);
      expect(app.composer.store.getSnapshot().busy).toBe(false);
      expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);
      expect(app.settings.store.getSnapshot().editor?.draft.display_name).toBe("未保存的工作邮箱");

      // Trigger again via direct composer.create() and confirm discard -> creates 1 draft and navigates.
      const creating = app.composer.create("bob@example.test");
      expect(app.bridge.compose.rows.size).toBe(0);
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      app.settings.confirm();
      await creating;

      expect(app.bridge.compose.rows.size).toBe(1);
      expect(app.host.state.getSnapshot().activeViewId).toBe(COMPOSE_VIEW);
      expect(app.composer.store.getSnapshot().fields?.to).toBe("bob@example.test");
      expect(app.settings.dirty).toBe(false);
    } finally {
      app.dispose();
    }
  });

  it("脏设置页触发搜索或邮箱导航并选择「留下」时：零搜索请求、不修改搜索词/范围、保留未保存表单", async () => {
    const app = assembleStudio();
    try {
      await flush();
      // Drain initial inbox queries so we can assert zero new queries on cancelled search.
      const queriesBefore = app.bridge.mailbox.queries.length;

      // Make proxy settings dirty on network section.
      app.host.openView(SETTINGS_VIEW);
      app.settings.activate("maild.settings.network");
      await flush();
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
      expect(app.settings.dirty).toBe(true);

      // Submit topbar search form and invoke actions.search from Shell while Settings is dirty.
      const tree = capture(() =>
        Studio({
          host: app.host,
          mailbox: app.mailbox,
          composer: app.composer,
          setup: app.setup,
          manageAccounts: () => {},
        }),
      );
      const searchForm = tree.elements.find(
        (el) => el.type === "form" && el.props.className === "studio-search",
      );
      expect(searchForm).toBeDefined();
      (searchForm!.props.onSubmit as (e: { preventDefault(): void }) => void)({
        preventDefault: () => {},
      });
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);

      // Stay: zero queries and proxy form remains intact.
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);
      expect(app.settings.store.getSnapshot().proxyDraft?.host).toBe("127.0.0.1");

      // Invoking StudioActionsContext search while dirty must NOT mutate search state or query mailbox on Stay.
      const actionsProvider = tree.elements.find(
        (el) =>
          el.props.value &&
          typeof (el.props.value as { search?: unknown }).search === "function",
      );
      expect(actionsProvider).toBeDefined();
      const actionsVal = actionsProvider!.props.value as {
        search(text: string, all?: boolean): void;
        findFromSender?(address: string): void;
        updateSearchFilters?(query: Record<string, unknown>): void;
      };
      actionsVal.search("invoice", true);
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      expect(app.mailbox.state.scope.kind).not.toBe("search");
      expect(app.settings.store.getSnapshot().proxyDraft?.host).toBe("127.0.0.1");

      // findFromSender and updateSearchFilters must ALSO honor dirty-settings leave-guard on Stay.
      actionsVal.findFromSender?.("alice@example.com");
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      expect(app.mailbox.state.scope.kind).not.toBe("search");

      actionsVal.updateSearchFilters?.({ has_attachments: true });
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      expect(app.mailbox.state.scope.kind).not.toBe("search");

      // Also test mailbox command open-starred while dirty -> Stay -> zero queries.
      app.host.execute("maild.mailbox.open-starred");
      expect(app.settings.store.getSnapshot().confirm).not.toBeNull();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      app.settings.cancelConfirmation();
      await flush();
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore);
      expect(app.host.state.getSnapshot().activeViewId).toBe(SETTINGS_VIEW);

      // Confirm leave -> navigates to MAIL and switches to starred once.
      app.host.execute("maild.mailbox.open-starred");
      app.settings.confirm();
      await flush();
      expect(app.host.state.getSnapshot().activeViewId).toBe(MAIL);
      expect(app.mailbox.state.scope).toEqual({ kind: "smart", name: "starred" });
      expect(app.bridge.mailbox.queries.length).toBe(queriesBefore + 1);
    } finally {
      app.dispose();
    }
  });

  it("设置页选中当前已激活分区与重复触发打开设置均为无副作用 no-op；添加账户遵守离开守卫", async () => {
    const app = assembleStudio();
    try {
      await flush();
      // Dirty notification rules on notifications section.
      app.host.openView(SETTINGS_VIEW);
      app.settings.activate("maild.settings.notifications");
      await flush();
      app.settings.setRules({
        ...app.settings.store.getSnapshot().rules!,
        show_sensitive: true,
      });
      expect(app.settings.dirty).toBe(true);

      // Selecting already-active section is a clean no-op (no prompt, no discard).
      expect(app.settings.selectSection("maild.settings.notifications")).toBe(true);
      expect(app.settings.store.getSnapshot().confirm).toBeNull();
      expect(app.settings.store.getSnapshot().rules?.show_sensitive).toBe(true);

      // Executing maild.settings.open while already on Settings is a clean no-op.
      app.host.execute("maild.settings.open");
      expect(app.settings.store.getSnapshot().confirm).toBeNull();
      expect(app.settings.store.getSnapshot().rules?.show_sensitive).toBe(true);

      // Executing maild.settings.add-account while dirty prompts before switching/opening wizard.
      app.host.execute("maild.settings.add-account");
      expect(app.settings.store.getSnapshot().confirm?.title).toBe("放弃未保存的设置？");
      expect(app.settings.store.getSnapshot().section).toBe("maild.settings.notifications");
      expect(app.settings.store.getSnapshot().editor).toBeNull();

      // Stay preserves dirty rules and opens no account editor.
      app.settings.cancelConfirmation();
      expect(app.settings.store.getSnapshot().section).toBe("maild.settings.notifications");
      expect(app.settings.store.getSnapshot().rules?.show_sensitive).toBe(true);
      expect(app.settings.store.getSnapshot().editor).toBeNull();

      // Confirm discards rules, switches to accounts, and opens the new-account editor.
      app.host.execute("maild.settings.add-account");
      app.settings.confirm();
      expect(app.settings.store.getSnapshot().section).toBe("maild.settings.accounts");
      expect(app.settings.store.getSnapshot().editor).not.toBeNull();
      expect(app.settings.store.getSnapshot().editor?.id).toBeNull();
      expect(app.settings.store.getSnapshot().rules?.show_sensitive).toBe(false);
    } finally {
      app.dispose();
    }
  });
});

describe("PR 1：内置命令元数据、活动邮箱作用域与目标重查", () => {
  it("注册全部内置 palette 命令并附带描述、双语关键词、快捷键元数据与风险等级", () => {
    const app = assembleStudio();
    try {
      const paletteCommands = app.host.registry
        .getSnapshot()
        .commands.filter(({ value }) => value.locations.includes("palette"))
        .map(({ value }) => value.id)
        .sort();

      expect(paletteCommands).toEqual([
        "maild.compose.forward",
        "maild.compose.new",
        "maild.compose.open-drafts",
        "maild.compose.reply",
        "maild.compose.reply-all",
        "maild.mailbox.archive",
        "maild.mailbox.clear-checked",
        "maild.mailbox.delete",
        "maild.mailbox.filter-all",
        "maild.mailbox.filter-unread",
        "maild.mailbox.move",
        "maild.mailbox.open-activity",
        "maild.mailbox.open-inbox",
        "maild.mailbox.open-starred",
        "maild.mailbox.sync",
        "maild.mailbox.toggle-read",
        "maild.mailbox.toggle-star",
        "maild.mailbox.toggle-thread",
        "maild.settings.add-account",
        "maild.settings.open",
      ]);

      for (const id of paletteCommands) {
        const cmd = app.host.registry.findCommand(id)!;
        expect(cmd.description?.trim().length, `${id} description`).toBeGreaterThan(0);
        expect(cmd.keywords?.length, `${id} keywords`).toBeGreaterThan(1);
        // Bilingual keywords include both Chinese and English tokens.
        expect(
          cmd.keywords!.some((kw) => /[\u4e00-\u9fff]/.test(kw)),
          `${id} zh keyword`,
        ).toBe(true);
        expect(
          cmd.keywords!.some((kw) => /[a-zA-Z]/.test(kw)),
          `${id} en keyword`,
        ).toBe(true);
      }

      expect(app.host.registry.findCommand("maild.compose.new")?.shortcut).toBe("mod+n");
      expect(app.host.registry.findCommand("maild.settings.open")?.shortcut).toBe("mod+,");
      expect(app.host.registry.findCommand("maild.mailbox.delete")?.risk).toBe("destructive");
      expect(app.host.registry.findCommand("maild.settings.add-account")?.risk).toBe("stage");
    } finally {
      app.dispose();
    }
  });

  it("非活动邮箱视图下禁用全部选中邮件命令，绝不弹出看不见的确认框或创建隐藏行内回复草稿", async () => {
    const app = assembleStudio();
    try {
      await flush();
      for (const req of app.bridge.folderRequests) {
        req.d.resolve([folder("INBOX", "inbox", 1), folder("Archive", "archive")]);
      }
      const row = { ...summary("INBOX", "10", "Quarterly Report"), seen: false, flagged: false };
      app.bridge.mailbox.queries.at(-1)?.d.resolve({ messages: [row], total: 1 });
      await flush();

      app.mailbox.selectMessage(row);
      app.bridge.detailRequests.at(-1)?.d.resolve(message("INBOX", "10"));
      await flush();
      app.mailbox.toggle(row);

      const selectionCommands = [
        "maild.mailbox.toggle-read",
        "maild.mailbox.toggle-star",
        "maild.mailbox.archive",
        "maild.mailbox.move",
        "maild.mailbox.delete",
        "maild.mailbox.toggle-thread",
        "maild.mailbox.clear-checked",
        "maild.compose.reply",
        "maild.compose.reply-all",
        "maild.compose.forward",
      ];

      // In active mailbox view, all of them are enabled.
      for (const id of selectionCommands) {
        expect(app.host.canExecute(id), `${id} enabled in mailbox`).toBe(true);
      }

      // Switch to Settings, Drafts, and Activity: every selection command must be blocked!
      for (const nonMailboxView of [SETTINGS_VIEW, DRAFTS_VIEW, ACTIVITY]) {
        expect(app.host.openView(nonMailboxView)).toBe(true);
        for (const id of selectionCommands) {
          expect(app.host.canExecute(id), `${id} disabled in ${nonMailboxView}`).toBe(false);
          app.host.execute(id);
          // Even direct command.execute bypass must remain a no-op outside active mailbox!
          app.host.registry.findCommand(id)!.execute({
            openView: (viewId) => app.host.openView(viewId),
            selection: { account: row.account, folder: row.folder, uid: row.uid },
            selectionSubject: row.subject,
            online: true,
            notify: () => {},
          });
        }
        await flush();
        expect(app.mailbox.state.prompt).toBeNull();
        expect(app.bridge.mailbox.writes).toEqual([]);
        expect(app.bridge.compose.replyCalls).toEqual([]);
        expect(app.bridge.compose.rows.size).toBe(0);
      }
    } finally {
      app.dispose();
    }
  });

  it("批量动作以 controller.targets() 为准、混合账户禁移、删除与移动只走 requestAction、同步与会话按真实状态守卫", async () => {
    const app = assembleStudio([
      { account: "demo", state: "online", detail: "" },
      { account: "work", state: "online", detail: "" },
    ]);
    try {
      await flush();
      for (const req of app.bridge.folderRequests) {
        req.d.resolve([
          { ...folder("INBOX", "inbox", 1), account: req.account },
          { ...folder("Archive", "archive"), account: req.account },
        ]);
      }
      const rowDemo = { ...summary("INBOX", "1", "Demo mail"), account: "demo", seen: false, flagged: false };
      const rowWork = { ...summary("INBOX", "2", "Work mail"), account: "work", seen: true, flagged: false };
      app.bridge.mailbox.queries.at(-1)?.d.resolve({
        messages: [rowDemo, rowWork],
        total: 2,
      });
      await flush();

      // Select rowDemo only (single account).
      app.mailbox.selectMessage(rowDemo);
      app.bridge.detailRequests.at(-1)?.d.resolve(
        message("INBOX", "1", { subject: "Demo mail", seen: false, flagged: false }),
      );
      await flush();

      expect(app.host.canExecute("maild.mailbox.move")).toBe(true);
      expect(app.host.canExecute("maild.mailbox.delete")).toBe(true);
      expect(app.host.canExecute("maild.mailbox.clear-checked")).toBe(false);

      // Delete & Move MUST only open requestAction prompt with frozen targets (zero direct writes).
      app.host.execute("maild.mailbox.delete");
      expect(app.mailbox.state.prompt?.kind).toBe("delete");
      expect(app.mailbox.state.prompt?.rows).toHaveLength(1);
      expect(app.mailbox.state.prompt?.rows[0]).toMatchObject({
        account: "demo",
        folder: "INBOX",
        uid: "1",
        subject: "Demo mail",
      });
      expect(app.bridge.mailbox.writes).toEqual([]);
      app.mailbox.closePrompt();

      app.host.execute("maild.mailbox.move");
      expect(app.mailbox.state.prompt?.kind).toBe("move");
      expect(app.mailbox.state.prompt?.rows).toHaveLength(1);
      expect(app.mailbox.state.prompt?.rows[0]).toMatchObject({
        account: "demo",
        folder: "INBOX",
        uid: "1",
        subject: "Demo mail",
      });
      expect(app.bridge.mailbox.writes).toEqual([]);
      app.mailbox.closePrompt();

      // Check both demo & work messages -> mixed accounts disables move, keeps delete/archive/clear-checked enabled!
      app.mailbox.toggle(rowDemo);
      app.mailbox.toggle(rowWork);
      expect(app.host.canExecute("maild.mailbox.move")).toBe(false);
      expect(app.host.canExecute("maild.mailbox.delete")).toBe(true);
      expect(app.host.canExecute("maild.mailbox.archive")).toBe(true);
      expect(app.host.canExecute("maild.mailbox.clear-checked")).toBe(true);

      // Executing move on mixed accounts must NOT open prompt or write.
      app.host.execute("maild.mailbox.move");
      expect(app.mailbox.state.prompt).toBeNull();
      expect(app.bridge.mailbox.writes).toEqual([]);

      // Executing delete on checked messages freezes BOTH checked rows in prompt.
      app.host.execute("maild.mailbox.delete");
      expect(app.mailbox.state.prompt?.kind).toBe("delete");
      expect(app.mailbox.state.prompt?.rows.map((r) => `${r.account}:${r.uid}`)).toEqual([
        "demo:1",
        "work:2",
      ]);
      app.mailbox.closePrompt();

      // Toggle read on mixed seen/unseen marks both seen.
      expect(app.host.registry.findCommand("maild.mailbox.toggle-read")?.title).toBe("标为已读");
      app.host.execute("maild.mailbox.toggle-read");
      await flush();
      expect(app.bridge.mailbox.writes).toHaveLength(2);
      expect(app.bridge.mailbox.writes.every((w) => w.action === "mark" && w.flag === "seen" && w.value === true)).toBe(true);

      // Clear checked selection works and disables itself afterwards.
      app.host.execute("maild.mailbox.clear-checked");
      expect(app.mailbox.state.checked).toEqual([]);
      expect(app.host.canExecute("maild.mailbox.clear-checked")).toBe(false);

      // Toggle thread opens online and can still collapse when offline.
      expect(app.host.canExecute("maild.mailbox.toggle-thread")).toBe(true);
      app.host.execute("maild.mailbox.toggle-thread");
      expect(app.mailbox.state.threadOpen).toBe(true);
      expect(app.host.registry.findCommand("maild.mailbox.toggle-thread")?.title).toBe("收起会话");

      // Offline: write commands & sync disabled, collapsing open thread still allowed.
      app.host.state.patch({ snapshot: { ...online, phase: "disconnected", revision: 2 } });
      expect(app.host.canExecute("maild.mailbox.sync")).toBe(false);
      expect(app.host.canExecute("maild.mailbox.archive")).toBe(false);
      expect(app.host.canExecute("maild.mailbox.toggle-thread")).toBe(true);
      app.host.execute("maild.mailbox.toggle-thread");
      expect(app.mailbox.state.threadOpen).toBe(false);
      // Opening thread while offline is now disabled.
      expect(app.host.canExecute("maild.mailbox.toggle-thread")).toBe(false);
    } finally {
      app.dispose();
    }
  });
});

function hostOpenSettingsDirtyAccount(app: ReturnType<typeof assembleStudio>) {
  app.host.openView(SETTINGS_VIEW);
  app.settings.beginAccount(account());
  app.settings.setDisplayName("未保存的工作邮箱");
  expect(app.settings.dirty).toBe(true);
}
