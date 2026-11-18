import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { Accounts } from "../src/settings/Accounts";
import { SettingsController } from "../src/settings/controller";
import {
  PROVIDERS,
  cleanSecret,
  domainOf,
  guessServers,
  matchProvider,
  portForSecurity,
  probeAdvice,
  providerById,
  serversFor,
} from "../src/settings/providers";
import { online } from "./fakeBridge";
import { FakeSettings, account } from "./fakeSettings";

const setup = () => {
  const bridge = new FakeSettings(),
    c = new SettingsController(bridge);
  c.connection(online);
  return { bridge, c };
};
const editor = (c: SettingsController) => c.store.getSnapshot().editor!;

describe("服务商预设：按 @domain 自动识别", () => {
  it.each([
    ["me@qq.com", "qq", "imap.qq.com", 993, "implicit", "smtp.qq.com", 465, "implicit"],
    ["me@foxmail.com", "qq", "imap.qq.com", 993, "implicit", "smtp.qq.com", 465, "implicit"],
    ["me@163.com", "netease", "imap.163.com", 993, "implicit", "smtp.163.com", 465, "implicit"],
    ["me@126.com", "netease", "imap.126.com", 993, "implicit", "smtp.126.com", 465, "implicit"],
    ["me@Gmail.COM", "gmail", "imap.gmail.com", 993, "implicit", "smtp.gmail.com", 465, "implicit"],
    ["me@icloud.com", "icloud", "imap.mail.me.com", 993, "implicit", "smtp.mail.me.com", 587, "starttls"],
    ["me@hotmail.com", "outlook", "outlook.office365.com", 993, "implicit", "smtp-mail.outlook.com", 587, "starttls"],
    ["me@pm.me", "proton", "127.0.0.1", 1143, "starttls", "127.0.0.1", 1025, "starttls"],
    ["me@zohomail.cn", "zoho", "imap.zoho.com.cn", 993, "implicit", "smtp.zoho.com.cn", 465, "implicit"],
  ])("%s → %s", (address, id, imapHost, imapPort, imapSec, smtpHost, smtpPort, smtpSec) => {
    const preset = matchProvider(address);
    expect(preset?.id).toBe(id);
    const servers = serversFor(preset!, domainOf(address));
    expect(servers.incoming).toEqual({ server: imapHost, port: imapPort, security: imapSec });
    expect(servers.outgoing).toEqual({ server: smtpHost, port: smtpPort, security: smtpSec });
  });

  it("未知域名、残缺地址不匹配；猜测规则为 imap./smtp. 加标准 TLS 端口", () => {
    expect(matchProvider("hi@acme.com")).toBeNull();
    expect(matchProvider("hi@")).toBeNull();
    expect(matchProvider("qq.com")).toBeNull();
    expect(guessServers("acme.com")).toEqual({
      incoming: { server: "imap.acme.com", port: 993, security: "implicit" },
      outgoing: { server: "smtp.acme.com", port: 465, security: "implicit" },
    });
  });

  it("每个预设都有凭据标签、步骤，帮助页只用 https；域名不重复", () => {
    const domains = PROVIDERS.flatMap((p) => p.domains);
    expect(new Set(domains).size).toBe(domains.length);
    for (const p of PROVIDERS) {
      expect(p.secretLabel).toBeTruthy();
      expect(p.steps.length).toBeGreaterThan(0);
      if (p.helpUrl) expect(p.helpUrl).toMatch(/^https:\/\//);
    }
    expect(providerById("outlook")?.caveat).toContain("OAuth");
    expect(providerById("gmail")?.stripSecretSpaces).toBe(true);
    expect(providerById("icloud")?.stripSecretSpaces).toBe(true);
    expect(providerById("qq")?.stripSecretSpaces).toBe(true);
  });
});

describe("TLS ↔ STARTTLS 端口联动", () => {
  it("标准端口随模式切换，自定义端口保留", () => {
    expect(portForSecurity("imap", 993, "implicit", "starttls")).toBe(143);
    expect(portForSecurity("imap", 143, "starttls", "implicit")).toBe(993);
    expect(portForSecurity("smtp", 465, "implicit", "starttls")).toBe(587);
    expect(portForSecurity("smtp", 587, "starttls", "implicit")).toBe(465);
    expect(portForSecurity("imap", 1143, "starttls", "implicit")).toBe(1143);
    expect(portForSecurity("smtp", 2525, "implicit", "starttls")).toBe(2525);
  });

  it("控制器编辑加密方式时同步端口，并视为手工配置", () => {
    const { c } = setup();
    c.beginAccount(undefined, "gmail");
    c.editEndpoint("outgoing", { security: "starttls" });
    expect(editor(c).draft.outgoing).toMatchObject({ security: "starttls", port: 587 });
    c.editEndpoint("incoming", { security: "starttls" });
    expect(editor(c).draft.incoming).toMatchObject({ security: "starttls", port: 143 });
    expect(c.store.getSnapshot().wizard.manual).toBe(true);
    // 手工配置后，输入别的服务商地址不再覆盖服务器。
    c.setAddress("me@qq.com");
    expect(editor(c).draft.incoming.server).toBe("imap.gmail.com");
  });
});

describe("应用密码空格清理", () => {
  it("去掉首尾与分组空白；不清理的服务商原样保留", () => {
    expect(cleanSecret("  abcd efgh\tijkl mnop \n", true)).toEqual({ value: "abcdefghijklmnop", changed: true });
    expect(cleanSecret("abcdefghijklmnop", true)).toEqual({ value: "abcdefghijklmnop", changed: false });
    expect(cleanSecret(" pass word ", false)).toEqual({ value: " pass word ", changed: false });
  });

  it("粘贴分组授权码时自动去空格并提示；探测收到的是干净的值", async () => {
    const { bridge, c } = setup();
    c.beginAccount();
    c.setAddress("me@gmail.com");
    c.setSecret("incoming", "abcd efgh ijkl mnop");
    expect(editor(c).secrets.incoming).toBe("abcdefghijklmnop");
    expect(c.store.getSnapshot().wizard.secretNotice).toContain("应用专用密码");
    await c.probe();
    expect(bridge.probes[0].secrets.incoming).toBe("abcdefghijklmnop");
  });
});

describe("添加账户向导", () => {
  it("输入地址即自动套用预设、用户名与显示名称跟随", () => {
    const { c } = setup();
    c.beginAccount();
    c.setAddress("me@163.com");
    const d = editor(c).draft;
    expect(c.store.getSnapshot().wizard.provider).toBe("netease");
    expect(d.incoming).toMatchObject({ server: "imap.163.com", port: 993, security: "implicit", username: "me@163.com" });
    expect(d.outgoing).toMatchObject({ server: "smtp.163.com", port: 465, security: "implicit", username: "me@163.com" });
    expect(d.display_name).toBe("me@163.com");
    c.setDisplayName("网易");
    c.setAddress("me@126.com");
    expect(editor(c).draft.display_name).toBe("网易");
    expect(editor(c).draft.incoming.server).toBe("imap.126.com");
  });

  it("快捷胶囊选定企业邮后，输入自有域名不会被改成别的服务商", () => {
    const { c } = setup();
    c.beginAccount();
    c.chooseProvider("exmail");
    c.setAddress("hi@acme.com");
    expect(c.store.getSnapshot().wizard.provider).toBe("exmail");
    expect(editor(c).draft.incoming.server).toBe("imap.exmail.qq.com");
  });

  it("未知域名：没有后端发现时按惯例预填并展开参数", async () => {
    const { c } = setup();
    c.beginAccount();
    c.setAddress("hi@acme.com");
    expect(c.store.getSnapshot().wizard.provider).toBeNull();
    await c.discover();
    const s = c.store.getSnapshot();
    expect(s.wizard).toMatchObject({ provider: "custom", expanded: true, manual: true, discovering: false });
    expect(s.wizard.discovered).toContain("imap.acme.com");
    expect(editor(c).draft.incoming).toMatchObject({ server: "imap.acme.com", port: 993 });
    expect(editor(c).draft.outgoing).toMatchObject({ server: "smtp.acme.com", port: 465 });
  });

  it("未知域名：后端发现结果优先，失败则降级为猜测", async () => {
    const { bridge, c } = setup();
    const asked: string[] = [];
    bridge.discoverProvider = async (address) => {
      asked.push(address);
      return {
        incoming: { server: "mail.acme.com", port: 143, security: "starttls" },
        outgoing: { server: "mail.acme.com", port: 587, security: "starttls" },
        source: "autoconfig",
      };
    };
    c.beginAccount();
    c.setAddress("hi@acme.com");
    await c.discover();
    expect(asked).toEqual(["hi@acme.com"]);
    expect(editor(c).draft.incoming).toMatchObject({ server: "mail.acme.com", port: 143, security: "starttls" });
    expect(c.store.getSnapshot().wizard.discovered).toContain("已自动发现");
    bridge.discoverProvider = async () => {
      throw new Error("unregistered");
    };
    c.setAddress("hi@other.org");
    await c.discover();
    expect(editor(c).draft.incoming.server).toBe("imap.other.org");
  });

  it("探测在服务器类阶段失败时自动展开参数，并完整展示 detail 与建议", async () => {
    const { bridge, c } = setup();
    c.beginAccount(undefined, "qq");
    c.setAddress("me@qq.com");
    expect(c.store.getSnapshot().wizard.expanded).toBe(false);
    bridge.probeResult = {
      incoming: { ok: false, stage: "connect", detail: "connection refused (os error 61)" },
      outgoing: { ok: false, stage: "auth", detail: "535 Login fail. Account is abnormal" },
    };
    await c.probe();
    expect(c.store.getSnapshot().wizard.expanded).toBe(true);
    const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
    expect(html).toContain("connection refused (os error 61)");
    expect(html).toContain("535 Login fail. Account is abnormal");
    expect(html).toContain("未通过 · 连接");
    expect(html).toContain(probeAdvice("connect", "incoming", providerById("qq")));
    expect(html).toContain("客户端授权码");
  });

  it("认证失败不展开服务器参数；旧后端没有 detail 时只给建议", async () => {
    const { bridge, c } = setup();
    c.beginAccount(undefined, "gmail");
    bridge.probeResult = { incoming: { ok: false, stage: "auth", detail: "" }, outgoing: null };
    await c.probe();
    expect(c.store.getSnapshot().wizard.expanded).toBe(false);
    const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
    expect(html).toContain("应用专用密码");
    expect(html).not.toContain("probe-detail");
  });

  it("已识别服务商时把参数收进折叠面板，并给出获取指引与官方入口", () => {
    const { c } = setup();
    c.beginAccount();
    c.setAddress("me@qq.com");
    const html = renderToStaticMarkup(createElement(Accounts, { controller: c }));
    expect(html).toContain("服务器连接参数（已自动匹配 imap.qq.com:993）");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("IMAP 服务器");
    expect(html).toContain("客户端授权码（16 位）");
    expect(html).toContain("打开 QQ 邮箱 官方设置页");
    expect(html).toContain("显示");
    c.toggleServers();
    expect(renderToStaticMarkup(createElement(Accounts, { controller: c }))).toContain("IMAP 服务器");
  });

  it("官方帮助页经桥打开；桥不支持时退为提示链接", async () => {
    const { bridge, c } = setup();
    const opened: string[] = [];
    bridge.openExternalUrl = async (url) => void opened.push(url);
    await c.openHelp("https://myaccount.google.com/apppasswords");
    await c.openHelp("javascript:alert(1)");
    expect(opened).toEqual(["https://myaccount.google.com/apppasswords"]);
    bridge.openExternalUrl = undefined;
    await c.openHelp("https://account.apple.com/");
    expect(c.store.getSnapshot().notice).toContain("https://account.apple.com/");
  });

  it("欢迎页一键入口：切到账户分区并预选服务商向导", () => {
    const { c } = setup();
    c.store.patch({ section: "maild.settings.notifications" });
    c.openSetup("icloud");
    const s = c.store.getSnapshot();
    expect(s.section).toBe("maild.settings.accounts");
    expect(s.wizard.provider).toBe("icloud");
    expect(s.editor?.id).toBeNull();
    expect(s.editor?.draft.outgoing).toMatchObject({ server: "smtp.mail.me.com", port: 587, security: "starttls" });
    expect(c.dirty).toBe(false);
    c.openSetup("custom");
    expect(c.store.getSnapshot().wizard).toMatchObject({ provider: "custom", expanded: true });
  });

  it("管理已有账户不自动覆盖服务器", () => {
    const { c } = setup();
    c.beginAccount({ ...account(), address: "me@gmail.com" });
    c.setAddress("me@qq.com");
    expect(editor(c).draft.incoming.server).toBe("imap.example.test");
    expect(c.store.getSnapshot().wizard.manual).toBe(true);
  });
});
