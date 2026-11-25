import { applyEffectiveLocales } from "@featherpost/i18n";
import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import {
  Accounts,
  AccountMore,
} from "../src/settings/Accounts";
import { StudioDropdownMenuItem } from "../src/app/DropdownMenu";
import { Appearance } from "../src/settings/Appearance";
import { settingsModule } from "../src/settings/module";
import { SettingsController } from "../src/settings/controller";
import { Host } from "../src/host/host";
import { FakeBridge, online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";
import { Extensions } from "../src/settings/Extensions";
import { BRAND } from "../src/brand";

afterEach(() => {
  vi.unstubAllGlobals();
  applyEffectiveLocales([]);
});

it("marks the sole built-in Studio frontend as builtin and protects it from removal alongside third-party frontends", () => {
  const controller = new SettingsController(new FakeSettings());
  const frontend = {
    version: "0.4.0",
    lifecycle: "on_demand",
    capabilities: [],
    permissions: [],
    has_identity: true,
  };
  controller.store.patch({
    packages: {
      default: BRAND.bundledFrontendId,
      plugins: [],
      frontends: [
        {
          ...frontend,
          id: BRAND.bundledFrontendId,
          name: "Featherpost",
          builtin: true,
        },
        {
          ...frontend,
          id: "org.example.custom",
          name: "Custom UI",
          builtin: false,
        },
      ],
    },
  });
  const html = renderToStaticMarkup(createElement(Extensions, { controller }));
  const [builtinCard, thirdPartyCard] = html.split("</article>");
  expect(builtinCard).toContain("内置前端");
  expect(builtinCard).not.toContain("当前窗口");
  expect(thirdPartyCard).toContain("Custom UI");
  expect(thirdPartyCard).not.toContain("内置前端");
  controller.dispose();
});

// Capture real React event handlers without replacing the renderer or controller.
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

it("registers every built-in settings section with Studio theme preview", () => {
  const host = new Host(new FakeBridge());
  const studio = settingsModule(host);
  expect(studio.settings?.map((s) => s.id).sort()).toEqual([
    "maild.settings.accounts",
    "maild.settings.addresses",
    "maild.settings.appearance",
    "maild.settings.connection",
    "maild.settings.extensions",
    "maild.settings.network",
    "maild.settings.notifications",
  ]);
  const appearanceHtml = renderToStaticMarkup(
    createElement(
      studio.settings!.find((s) => s.id.endsWith(".appearance"))!.render,
      { active: true },
    ),
  );
  expect(appearanceHtml).toContain("studio-theme-preview");
  studio.dispose?.();
});

it("renders the account editor in English when the effective locale is en", () => {
  applyEffectiveLocales(["en"]);
  const c = new SettingsController(new FakeSettings());
  const profiles = [account(), { ...account(), id: "readonly", editable: false }];
  c.store.patch({ profiles });
  c.beginAccount(profiles[0]);
  const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  for (const text of [
    "Connected mailboxes",
    "Config file account, read-only",
    "Display name",
    "Email address",
    "IMAP server",
    "SMTP server",
    "Use IMAP IDLE for new mail",
    "Polling interval (seconds)",
    "Check connection",
    "Save changes",
    "More account actions",
  ])
    expect(html).toContain(text);
  expect(html).not.toContain("显示名称");
  c.dispose();
});

it("keeps all accounts visible around the inline editor, including read-only profiles", () => {
  const c = new SettingsController(new FakeSettings());
  const profiles = [
    account(),
    {
      ...account(),
      id: "personal",
      display_name: "个人邮箱",
      address: "personal@example.test",
    },
    {
      ...account(),
      id: "readonly",
      display_name: "配置邮箱",
      address: "config@example.test",
      editable: false,
    },
  ];
  c.store.patch({ profiles });
  c.beginAccount(profiles[0]);
  const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  for (const text of [
    "个人邮箱",
    "配置邮箱",
    "配置文件账户，只读",
    "显示名称",
    "邮箱地址",
    "IMAP 服务器",
    "SMTP 服务器",
    "收信文件夹",
    "使用 IMAP IDLE",
    "轮询间隔（秒）",
    "发信认证",
    "探测连接",
    "保存更改",
    "更多账户操作",
  ]) {
    expect(html).toContain(text);
  }
  expect(html.match(/class="account-row"/g)).toHaveLength(3);
  expect(html.match(/class="account-editor"/g)).toHaveLength(1);
  expect(html).toContain("留空");
  expect(html).not.toContain('value="saved"');
  c.dispose();
});

it("preserves independently configured plugin transports and separate SMTP credentials", () => {
  const c = new SettingsController(new FakeSettings());
  c.beginAccount({
    ...account(),
    incoming: { kind: "plugin", provider: "receive.service", poll_secs: 5 },
    outgoing: { ...account().outgoing!, credential: "separate" },
  });
  const smtp = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  expect(smtp).toContain("SMTP 服务器");
  expect(smtp).toContain("发信密码");
  expect(smtp).toContain('value="5"');
  c.beginAccount({
    ...account(),
    outgoing: { kind: "plugin", provider: "send.service" },
  });
  const plugin = renderToStaticMarkup(
    createElement(Accounts, { controller: c }),
  );
  expect(plugin).not.toContain("SMTP 服务器");
  expect(plugin).toContain("send.service");
  c.dispose();
});

it("closes a clean editor before account actions and never drops dirty input", () => {
  const c = new SettingsController(new FakeSettings());
  c.connection(online);
  c.beginAccount(account());
  const action = () => {
    const { elements } = capture(() =>
      AccountMore({ profile: account(), controller: c }),
    );
    return elements.find(
      (el) => el.type === StudioDropdownMenuItem && el.props.children === "暂停同步",
    )!.props.onSelect as () => void;
  };
  c.setDisplayName("正在编辑");
  action()();
  expect(c.store.getSnapshot().editor?.draft.display_name).toBe("正在编辑");
  expect(c.store.getSnapshot().confirm).toBeNull();
  c.beginAccount(account());
  action()();
  expect(c.store.getSnapshot().editor).toBeNull();
  expect(c.store.getSnapshot().confirm?.title).toContain("暂停");
  c.cancelConfirmation();
  c.dispose();
});

it("blocks section changes for dirty input and navigates only after confirmation", () => {
  vi.stubGlobal("window", {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
  const host = new Host(new FakeBridge());
  host.state.patch({ snapshot: online });
  const c = new SettingsController(host.bridge.settings);
  const bundle = settingsModule(host, c);
  host.registerBuiltin(bundle);
  c.beginAccount(account());
  c.setDisplayName("未保存的名称");
  const Page = bundle.views![0].render as (props: {
    active: boolean;
  }) => ReactNode;
  const tree = capture(() => Page({ active: true }));
  const appearance = tree.elements.find(
    (el) =>
      el.type === "button" &&
      Array.isArray(el.props.children) &&
      (el.props.children[0] as ReactElement<{ children: string }>)?.props
        ?.children === "外观与操作",
  );
  expect(appearance).toBeDefined();
  (appearance!.props.onClick as () => void)();
  expect(c.store.getSnapshot().section).toBe("maild.settings.accounts");
  expect(c.store.getSnapshot().confirm?.title).toBe("放弃未保存的设置？");
  c.cancelConfirmation();
  expect(c.store.getSnapshot().editor?.draft.display_name).toBe("未保存的名称");
  (appearance!.props.onClick as () => void)();
  c.confirm();
  expect(c.store.getSnapshot().section).toBe("maild.settings.appearance");
  expect(c.store.getSnapshot().editor).toBeNull();
  bundle.dispose?.();
  host.stop();
});

it("applies theme and density cards through the existing persisted preferences", () => {
  const host = new Host(new FakeBridge());
  host.setPreferences({
    theme: "light",
    density: "compact",
    readerAppearance: "dark-adapted",
  });
  const change = (name: string, value: string) => {
    const { elements } = capture(() => Appearance({ host }));
    const control = elements.find(
      (el) =>
        el.type === "input" &&
        el.props.name === name &&
        el.props.value === value,
    )!;
    expect(control).toBeDefined();
    (control.props.onChange as () => void)();
  };
  change("studio-theme", "dark");
  expect(host.state.getSnapshot().preferences).toEqual({
    theme: "dark",
    density: "compact",
    readerAppearance: "dark-adapted",
  });
  change("studio-density", "comfortable");
  expect(host.state.getSnapshot().preferences.density).toBe("comfortable");
  expect(host.state.getSnapshot().preferences.theme).toBe("dark");
});

it("enables Studio offline recovery via daemon-independent openRecovery and falls back online", async () => {
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const bundle = settingsModule(host);
  const Connection = bundle.settings!.find(
    (section) => section.id === "maild.settings.connection",
  )!.render as (props: { active: boolean }) => ReactNode;

  // Without openRecovery, offline button remains disabled.
  const disabledHtml = renderToStaticMarkup(createElement(Connection, { active: true }));
  expect(disabledHtml).toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);

  // With openRecovery, offline button is enabled and invokes openRecovery directly.
  const openRecovery = vi.fn(async () => {});
  bridge.settings.openRecovery = openRecovery;
  const offlineTree = capture(() => Connection({ active: true }));
  const offlineBtn = offlineTree.elements.find(
    (el) => el.type === "button" && el.props.children === "打开修复窗口",
  )!;
  expect(offlineBtn).toBeDefined();
  expect(offlineBtn.props.disabled).toBe(false);
  (offlineBtn.props.onClick as () => void)();
  await Promise.resolve();
  expect(openRecovery).toHaveBeenCalledTimes(1);
  expect(bridge.settings.packageWrites).toEqual([]);

  // Online: prefers daemon packageAction and falls back to openRecovery if daemon rejects.
  host.state.patch({ snapshot: online });
  bridge.settings.packageAction = async () => {
    throw new Error("daemon offline mid-flight");
  };
  const onlineTree = capture(() => Connection({ active: true }));
  const onlineBtn = onlineTree.elements.find(
    (el) => el.type === "button" && el.props.children === "打开修复窗口",
  )!;
  (onlineBtn.props.onClick as () => void)();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(openRecovery).toHaveBeenCalledTimes(2);
  bundle.dispose?.();
});
