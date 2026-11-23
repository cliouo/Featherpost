import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { Accounts } from "../src/settings/Accounts";
import { Accounts as StudioAccounts } from "../../featherpost-studio/src/settings/Accounts";
import { SettingsController } from "../src/settings/controller";
import { FakeSettings, account } from "./fakeSettings";
import { online } from "./fakeBridge";

it.each([
  { name: "", label: "me@example.test", subtitle: false },
  { name: "   ", label: "me@example.test", subtitle: false },
  { name: "me@example.test", label: "me@example.test", subtitle: false },
  { name: " me@example.test ", label: "me@example.test", subtitle: false },
  { name: " 工作邮箱 ", label: "工作邮箱", subtitle: true },
])("shows a usable Settings identity for '$name'", async ({ name, label, subtitle }) => {
  const bridge = new FakeSettings();
  bridge.profileRows = [{ ...account(), display_name: name }];
  const controller = new SettingsController(bridge);
  controller.connection(online);
  await controller.load();
  try {
    for (const Component of [Accounts, StudioAccounts]) {
      const html = renderToStaticMarkup(createElement(Component, { controller }));
      expect(html).toContain(
        `<span class="account-monogram" aria-hidden="true">${label.slice(0, 1).toUpperCase()}</span>`,
      );
      expect(html).toContain(
        `<strong>${label}</strong>${subtitle ? "<p>me@example.test</p>" : ""}`,
      );
      if (!subtitle) expect(html).not.toContain("<p>me@example.test</p>");
      expect(html).toMatch(new RegExp(`aria-label="[^"]*${label}[^"]*"`));
    }
    controller.confirmAccount(bridge.profileRows[0], false);
    expect(controller.store.getSnapshot().confirm?.title).toContain(label);
    controller.cancelConfirmation();
    controller.beginAccount(bridge.profileRows[0]);
    await controller.saveAccount();
    expect(controller.store.getSnapshot().notice).toContain(label);
    // Rendering fallback must not silently rewrite the saved profile or editor input.
    expect(bridge.accountWrites[0].draft.display_name).toBe(name);
  } finally {
    controller.dispose();
  }
});
