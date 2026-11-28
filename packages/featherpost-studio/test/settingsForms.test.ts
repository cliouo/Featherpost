import { createElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { applyEffectiveLocales } from "@featherpost/i18n";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Accounts } from "../src/settings/Accounts";
import {
  currentSystemOffsetMinutes,
  formatUtcOffset,
  Notifications,
} from "../src/settings/Notifications";
import { SettingsController } from "../src/settings/controller";
import { FakeSettings, account, modernRules, rules } from "./fakeSettings";
import { Host } from "../src/host/host";
import { settingsModule } from "../src/settings/module";
import { FakeBridge, online } from "./fakeBridge";
import { BRAND } from "../src/brand";
import { Switch } from "../src/ui";

it("offers native recovery online and enables daemon-independent offline recovery when supported", async () => {
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const bundle = settingsModule(host);
  const Connection = bundle.settings!.find(
    section => section.id === "maild.settings.connection",
  )!.render as (props: { active: boolean }) => ReactNode;
  // Without daemon-independent openRecovery (older/non-Tauri bridge), offline button stays disabled.
  const offlineWithoutCmd = renderToStaticMarkup(createElement(Connection, { active: true }));
  expect(offlineWithoutCmd).toContain(`后台未连接，请打开 ${BRAND.appBundleName} 的修复入口。`);
  expect(offlineWithoutCmd).toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);

  // With openRecovery (Tauri shell), offline button is enabled and invokes openRecovery directly.
  const openRecovery = vi.fn(async () => {});
  bridge.settings.openRecovery = openRecovery;
  let captured: ReactNode = null;
  function Probe() {
    captured = createElement(Connection, { active: true });
    return captured;
  }
  const offlineWithCmd = renderToStaticMarkup(createElement(Probe));
  expect(offlineWithCmd).not.toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);
  const findButton = (node: ReactNode): ReactElement<Record<string, unknown>> | null => {
    if (Array.isArray(node)) {
      for (const child of node) {
        const hit = findButton(child);
        if (hit) return hit;
      }
      return null;
    }
    if (!node || typeof node !== "object" || !("props" in node)) return null;
    const el = node as ReactElement<Record<string, unknown>>;
    if (el.type === "button" && el.props.children === "打开修复窗口") return el;
    return findButton(el.props.children as ReactNode);
  };
  let renderedRoot: ReactNode = null;
  function CaptureConnection() {
    renderedRoot = Connection({ active: true });
    return renderedRoot;
  }
  renderToStaticMarkup(createElement(CaptureConnection));
  const button = findButton(renderedRoot);
  expect(button).not.toBeNull();
  expect(button!.props.disabled).toBe(false);
  (button!.props.onClick as () => void)();
  await Promise.resolve();
  expect(openRecovery).toHaveBeenCalledTimes(1);
  expect(bridge.settings.packageWrites).toEqual([]);

  // Online: uses packageAction({ action: "recovery" }) and falls back to openRecovery on error.
  host.state.patch({ snapshot: online });
  const connected = renderToStaticMarkup(createElement(Connection, { active: true }));
  expect(connected).toContain("在独立原生窗口恢复内置前端、启动或停止后台。");
  expect(connected).not.toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);
  bridge.settings.packageAction = async () => {
    throw new Error("agent recovery unavailable");
  };
  renderToStaticMarkup(createElement(CaptureConnection));
  (findButton(renderedRoot)!.props.onClick as () => void)();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(openRecovery).toHaveBeenCalledTimes(2);
  bundle.dispose?.();
});

it("renders each existing account transport independently", () => {
  const c = new SettingsController(new FakeSettings());
  c.beginAccount({
    ...account(),
    incoming: { kind: "plugin", provider: "receive.service", poll_secs: 5 },
    outgoing: { ...account().outgoing!, credential: "separate" },
  });
  const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
  expect(html).toContain("SMTP 服务器");
  expect(html).toContain("发信密码");
  expect(html).not.toContain('min="15"');
  c.beginAccount({
    ...account(),
    outgoing: { kind: "plugin", provider: "send.service" },
  });
  const outgoing = renderToStaticMarkup(
    createElement(Accounts, { controller: c }),
  );
  expect(outgoing).not.toContain("SMTP 服务器");
  expect(outgoing).toContain("send.service");
});
it("represents existing 24:00 windows and the supported UTC offset range", () => {
  const c = new SettingsController(new FakeSettings());
  c.store.patch({
    rules: {
      show_sensitive: false,
      silent_keywords: [],
      quiet_hours: [{ start: "22:00", end: "24:00" }],
      timezone_offset_minutes: 900,
    },
  });
  const html = renderToStaticMarkup(
    createElement(Notifications, { controller: c }),
  );
  expect(html).toContain('value="00:00"');
  expect(html).not.toContain('value="24:00"');
  expect(html).toContain('max="1440"');
});

const proc = (globalThis as { process?: { env: Record<string, string | undefined> } }).process;
const initialTz = proc?.env.TZ;

afterEach(() => {
  if (!proc) return;
  if (initialTz === undefined) delete proc.env.TZ;
  else proc.env.TZ = initialTz;
});

interface CapturedButton {
  text: string;
  ariaLabel?: string;
  ariaChecked?: boolean;
  role?: string;
  disabled?: boolean;
  onClick?: () => void;
}

function renderNotifications(controller: SettingsController) {
  let rootTree: unknown = null;
  function Probe() {
    const tree = Notifications({ controller });
    rootTree = tree;
    return tree;
  }
  const html = renderToStaticMarkup(createElement(Probe));
  const buttons: CapturedButton[] = [];
  const textOf = (node: unknown): string => {
    if (typeof node === "string" || typeof node === "number") return String(node);
    if (!node || typeof node !== "object") return "";
    if (Array.isArray(node)) return node.map(textOf).join("");
    const el = node as { props?: { children?: unknown } };
    return textOf(el.props?.children);
  };
  const walk = (node: unknown) => {
    if (!node || typeof node !== "object") return;
    if (Array.isArray(node)) return node.forEach(walk);
    const el = node as {
      type?: unknown;
      props?: {
        children?: unknown;
        onClick?: () => void;
        onCheckedChange?: (checked: boolean) => void;
        checked?: boolean;
        disabled?: boolean;
        role?: string;
        "aria-label"?: string;
        "aria-checked"?: boolean;
      };
    };
    if (el.type === "button" && el.props) {
      buttons.push({
        text: textOf(el.props.children).trim(),
        ariaLabel: el.props["aria-label"],
        ariaChecked: el.props["aria-checked"],
        role: el.props.role,
        disabled: el.props.disabled,
        onClick: el.props.onClick,
      });
    } else if (el.type === Switch && el.props) {
      const current = Boolean(el.props.checked);
      buttons.push({
        text: "",
        ariaLabel: el.props["aria-label"],
        ariaChecked: current,
        role: "switch",
        disabled: el.props.disabled,
        onClick: () => el.props?.onCheckedChange?.(!current),
      });
    }
    walk(el.props?.children);
  };
  walk(rootTree);
  return { html, buttons };
}

describe("静默时段：跟随系统时区、首段自动启用与旧偏移差异提示", () => {
  it("formatUtcOffset 与 currentSystemOffsetMinutes 处理正负偏移、半时区与 -0", () => {
    expect(formatUtcOffset(0)).toBe("UTC+0");
    expect(formatUtcOffset(-0)).toBe("UTC+0");
    expect(formatUtcOffset(480)).toBe("UTC+8");
    expect(formatUtcOffset(-300)).toBe("UTC−5");
    expect(formatUtcOffset(330)).toBe("UTC+5:30");
    if (proc) {
      proc.env.TZ = "UTC";
      const zero = currentSystemOffsetMinutes(new Date());
      expect(Object.is(zero, 0)).toBe(true);
    }
  });

  it("新后台支持 follow_system_timezone：开关切换动态隐藏手动 UTC 偏移并更新说明文案", async () => {
    const bridge = new FakeSettings();
    bridge.rules = async () => ({ rules: modernRules(), persisted: true });
    const c = new SettingsController(bridge);
    c.connection(online);
    c.activate("maild.settings.notifications");
    await c.load();

    let view = renderNotifications(c);
    expect(view.html).toContain("跟随系统时区");
    expect(view.html).toContain("自动适配当前系统时区、夏令时切换与跨时区出行。");
    expect(view.html).toContain("按下面的固定 UTC 偏移计算；不会自动随夏令时切换。");
    expect(view.html).toContain("UTC 偏移（分钟）");

    const toggle = view.buttons.find((b) => b.ariaLabel === "跟随系统时区")!;
    expect(toggle.role).toBe("switch");
    expect(toggle.ariaChecked).toBe(false);
    toggle.onClick!();

    expect(c.store.getSnapshot().rules?.follow_system_timezone).toBe(true);
    view = renderNotifications(c);
    expect(view.html).toContain("按当前系统本地时区计算，自动跟随夏令时与出行。");
    expect(view.html).not.toContain("UTC 偏移（分钟）");
    expect(view.buttons.find((b) => b.ariaLabel === "跟随系统时区")?.ariaChecked).toBe(true);
  });

  it("空静默时段且偏移为 0 时点「添加时段」自动开启跟随系统时区；已有自定义偏移或旧后台不覆盖", async () => {
    const bridge = new FakeSettings();
    bridge.rules = async () => ({ rules: modernRules(), persisted: true });
    const c = new SettingsController(bridge);
    c.connection(online);
    c.activate("maild.settings.notifications");
    await c.load();

    const first = renderNotifications(c);
    first.buttons.find((b) => b.text === "添加时段")!.onClick!();
    expect(c.store.getSnapshot().rules).toMatchObject({
      quiet_hours: [{ start: "22:00", end: "08:00" }],
      follow_system_timezone: true,
    });

    // 用户已显式设置非零固定偏移（如 480）且尚未添加时段：保留固定偏移模式
    c.setRules({ ...modernRules(), timezone_offset_minutes: 480, follow_system_timezone: false });
    renderNotifications(c).buttons.find((b) => b.text === "添加时段")!.onClick!();
    expect(c.store.getSnapshot().rules?.follow_system_timezone).toBe(false);

    // 旧后台不支持 follow_system_timezone：绝不塞入新键
    const oldC = new SettingsController(new FakeSettings());
    oldC.connection(online);
    oldC.activate("maild.settings.notifications");
    await oldC.load();
    renderNotifications(oldC).buttons.find((b) => b.text === "添加时段")!.onClick!();
    expect(oldC.store.getSnapshot().rules?.follow_system_timezone).toBeUndefined();
  });

  it("固定偏移与当前系统时区不一致时如实提示（零静默篡改），新旧后台一键修复各有出口", async () => {
    if (proc) proc.env.TZ = "Asia/Shanghai"; // 系统偏移 +480
    const sysOffset = currentSystemOffsetMinutes();
    const mismatchedOffset = sysOffset === 0 ? 480 : 0;

    // 1. 新后台：展示「改为跟随系统时区」，初次渲染绝不篡改 dirty 状态
    const newBridge = new FakeSettings();
    newBridge.rules = async () => ({
      rules: {
        ...modernRules(),
        quiet_hours: [{ start: "22:00", end: "08:00" }],
        timezone_offset_minutes: mismatchedOffset,
        follow_system_timezone: false,
      },
      persisted: true,
    });
    const newC = new SettingsController(newBridge);
    newC.connection(online);
    newC.activate("maild.settings.notifications");
    await newC.load();
    expect(newC.dirty).toBe(false);

    let view = renderNotifications(newC);
    expect(newC.dirty).toBe(false);
    expect(view.html).toContain(formatUtcOffset(mismatchedOffset));
    expect(view.html).toContain("改为跟随系统时区");
    view.buttons.find((b) => b.text === "改为跟随系统时区")!.onClick!();
    expect(newC.store.getSnapshot().rules?.follow_system_timezone).toBe(true);
    expect(newC.dirty).toBe(true);
    view = renderNotifications(newC);
    expect(view.html).not.toContain("改为跟随系统时区");

    // 2. 旧后台：展示「填入当前系统偏移（... 分钟）」，点击后填入当前系统偏移且消除告警
    const oldBridge = new FakeSettings();
    oldBridge.rules = async () => ({
      rules: {
        ...rules(),
        quiet_hours: [{ start: "23:00", end: "07:00" }],
        timezone_offset_minutes: mismatchedOffset,
      },
      persisted: null,
    });
    const oldC = new SettingsController(oldBridge);
    oldC.connection(online);
    oldC.activate("maild.settings.notifications");
    await oldC.load();
    expect(oldC.dirty).toBe(false);

    let oldView = renderNotifications(oldC);
    const fillLabel = `填入当前系统偏移（${sysOffset} 分钟）`;
    expect(oldView.html).toContain(fillLabel);
    oldView.buttons.find((b) => b.text === fillLabel)!.onClick!();
    expect(oldC.store.getSnapshot().rules?.timezone_offset_minutes).toBe(sysOffset);
    expect(oldC.store.getSnapshot().rules?.follow_system_timezone).toBeUndefined();
    oldView = renderNotifications(oldC);
    expect(oldView.html).not.toContain(fillLabel);
  });
});

it("settings module titles and pages follow the UI language without re-registering", () => {
  const host = new Host(new FakeBridge());
  const bundle = settingsModule(host);
  host.registerBuiltin(bundle);
  try {
    const titles = () => host.registry.getSnapshot().settings.map(({ value }) => value.title);
    expect(titles()).toEqual(["账户", "外观与操作", "通知与规则", "地址与别名", "扩展", "网络与代理", "翻译", "后台与存储"]);
    applyEffectiveLocales(["en"]);
    expect(titles()).toEqual([
      "Accounts",
      "Appearance & behavior",
      "Notifications & rules",
      "Addresses & aliases",
      "Extensions",
      "Network & proxy",
      "Translation",
      "Background & storage",
    ]);
    expect(host.registry.findView("maild.settings.page")?.title).toBe("Settings");
    const Connection = bundle.settings!.find((section) => section.id === "maild.settings.connection")!.render;
    const offline = renderToStaticMarkup(createElement(Connection, { active: true }));
    expect(offline).toContain(`The background service isn&#x27;t connected. Open the repair entry in ${BRAND.appBundleName}.`);
    expect(offline).toMatch(/<button[^>]*disabled=""[^>]*>Open repair window<\/button>/);
  } finally {
    applyEffectiveLocales([]);
    bundle.dispose?.();
  }
});
