import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { Accounts } from "../src/settings/Accounts";
import { Notifications } from "../src/settings/Notifications";
import { SettingsController } from "../src/settings/controller";
import { FakeSettings, account } from "./fakeSettings";
import { Host } from "../src/host/host";
import { settingsModule } from "../src/settings/module";
import { FakeBridge, online } from "./fakeBridge";
import { BRAND } from "../src/brand";

it("offers native recovery online and directs disconnected users to the installed app", () => {
  const host = new Host(new FakeBridge());
  const bundle = settingsModule(host);
  const Connection = bundle.settings!.find(section => section.id === "maild.settings.connection")!.render;
  const offline = renderToStaticMarkup(createElement(Connection, { active: true }));
  expect(offline).toContain(`后台未连接，请打开 ${BRAND.appBundleName} 的修复入口。`);
  expect(offline).toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);
  host.state.patch({ snapshot: online });
  const connected = renderToStaticMarkup(createElement(Connection, { active: true }));
  expect(connected).toContain("在独立原生窗口恢复内置前端、启动或停止后台。");
  expect(connected).not.toMatch(/<button[^>]*disabled=""[^>]*>打开修复窗口<\/button>/);
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
