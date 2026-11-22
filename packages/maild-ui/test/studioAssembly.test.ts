import { applyEffectiveLocales } from "@featherpost/i18n";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { Host } from "../src/host/host";
import { ComposeController, COMPOSE_VIEW } from "../src/compose/controller";
import { composeModule } from "../src/compose/module";
import { SettingsController } from "../src/settings/controller";
import { settingsModule } from "../../featherpost-studio/src/settings/module";
import {
  studioMailbox,
  MAIL,
  ACTIVITY,
} from "../../featherpost-studio/src/mailbox";
import { FakeBridge, flush } from "./fakeBridge";

afterEach(() => {
  vi.unstubAllGlobals();
  applyEffectiveLocales([]);
});

it("assembles Studio with real namespace validation and shared compose/settings roles", async () => {
  vi.stubGlobal("window", {
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  });
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const composer = new ComposeController(
    bridge.compose,
    (id) => host.openView(id),
    () => {},
  );
  const settings = new SettingsController(bridge.settings, () => {});
  const mailbox = studioMailbox(host, composer, () => {});
  const bundles = [
    mailbox.bundle,
    composeModule(host, composer),
    settingsModule(host, settings),
  ];
  try {
    bundles.forEach((bundle) => host.registerBuiltin(bundle));
    host.finishAssembly();
    expect(host.registry.resolveRole("mailbox").id).toBe(MAIL);
    expect(host.registry.resolveRole("compose").id).toBe(COMPOSE_VIEW);
    expect(host.registry.resolveRole("settings").id).toBe(
      "maild.settings.page",
    );
    expect(host.state.getSnapshot().activeViewId).toBe(MAIL);
    expect(host.openView(ACTIVITY)).toBe(true);
    expect(
      host.registry
        .getSnapshot()
        .settings.some((section) => section.value.title === "阅读与隐私"),
    ).toBe(true);
    // 注册表里的标题定格在登记时的语言；设置页按分区 ID 重新取消息，切换语言即生效。
    applyEffectiveLocales(["en"]);
    const page = renderToStaticMarkup(
      createElement(host.registry.resolveRole("settings").render, {
        active: true,
      }),
    );
    for (const text of [
      "Settings sections",
      "Mail &amp; preferences",
      "Reading &amp; privacy",
      "Accounts",
      "Background &amp; storage",
      "Back to mail",
    ])
      expect(page).toContain(text);
    expect(page).not.toContain("阅读与隐私");
    await flush();
  } finally {
    bundles.forEach((bundle) => bundle.dispose?.());
    host.stop();
  }
});
