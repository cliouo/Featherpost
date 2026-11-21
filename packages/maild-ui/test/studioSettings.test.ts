import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { Accounts } from "../../featherpost-studio/src/settings/Accounts";
import { Appearance } from "../../featherpost-studio/src/settings/Appearance";
import { settingsModule } from "../../featherpost-studio/src/settings/module";
import { settingsModule as legacySettings } from "../src/settings/module";
import { SettingsController } from "../src/settings/controller";
import { Host } from "../src/host/host";
import { FakeBridge, online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";

afterEach(() => vi.unstubAllGlobals());

// Capture real React event handlers without replacing the renderer or controller.
function capture(render: () => ReactNode) {
  let root: ReactNode;
  function Probe() { root = render(); return root; }
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

it("retains every built-in settings section without changing the legacy renderer", () => {
  const host = new Host(new FakeBridge());
  const studio = settingsModule(host), legacy = legacySettings(host);
  expect(studio.settings?.map(s => s.id).sort()).toEqual(legacy.settings?.map(s => s.id).sort());
  const render = (bundle: typeof studio) => renderToStaticMarkup(createElement(bundle.settings!.find(s => s.id.endsWith(".appearance"))!.render, { active: true }));
  expect(render(studio)).toContain("studio-theme-preview");
  expect(render(legacy)).not.toContain("studio-theme-preview");
  studio.dispose?.();
  legacy.dispose?.();
});

it("keeps all accounts visible around the inline editor, including read-only profiles", () => {
  const c = new SettingsController(new FakeSettings());
  const profiles = [
    account(),
    { ...account(), id: "personal", display_name: "个人邮箱", address: "personal@example.test" },
    { ...account(), id: "readonly", display_name: "配置邮箱", address: "config@example.test", editable: false },
  ];
  c.store.patch({ profiles });
  c.beginAccount(profiles[0]);
  const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  for (const text of ["个人邮箱", "配置邮箱", "配置文件账户，只读", "显示名称", "邮箱地址", "IMAP 服务器", "SMTP 服务器", "收信文件夹", "使用 IMAP IDLE", "轮询间隔（秒）", "发信认证", "探测连接", "保存更改", "更多账户操作"]) {
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
  c.beginAccount({ ...account(), incoming: { kind: "plugin", provider: "receive.service", poll_secs: 5 }, outgoing: { ...account().outgoing!, credential: "separate" } });
  const smtp = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  expect(smtp).toContain("SMTP 服务器");
  expect(smtp).toContain("发信密码");
  expect(smtp).toContain('value="5"');
  c.beginAccount({ ...account(), outgoing: { kind: "plugin", provider: "send.service" } });
  const plugin = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  expect(plugin).not.toContain("SMTP 服务器");
  expect(plugin).toContain("send.service");
  c.dispose();
});

it("blocks section changes for dirty input and navigates only after confirmation", () => {
  vi.stubGlobal("window", { addEventListener: vi.fn(), removeEventListener: vi.fn() });
  const host = new Host(new FakeBridge());
  host.state.patch({ snapshot: online });
  const c = new SettingsController(host.bridge.settings);
  const bundle = settingsModule(host, c);
  host.registerBuiltin(bundle);
  c.beginAccount(account());
  c.setDisplayName("未保存的名称");
  const Page = bundle.views![0].render as (props: { active: boolean }) => ReactNode;
  const tree = capture(() => Page({ active: true }));
  const appearance = tree.elements.find(el => el.type === "button" && Array.isArray(el.props.children) &&
    (el.props.children[0] as ReactElement<{ children: string }>)?.props?.children === "外观与操作");
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
  host.setPreferences({ theme: "light", density: "compact", readerAppearance: "dark-adapted" });
  const change = (name: string, value: string) => {
    const { elements } = capture(() => Appearance({ host }));
    const control = elements.find(el => el.type === "input" && el.props.name === name && el.props.value === value)!;
    expect(control).toBeDefined();
    (control.props.onChange as () => void)();
  };
  change("studio-theme", "dark");
  expect(host.state.getSnapshot().preferences).toEqual({ theme: "dark", density: "compact", readerAppearance: "dark-adapted" });
  change("studio-density", "comfortable");
  expect(host.state.getSnapshot().preferences.density).toBe("comfortable");
  expect(host.state.getSnapshot().preferences.theme).toBe("dark");
});
