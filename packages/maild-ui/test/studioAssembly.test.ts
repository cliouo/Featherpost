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

afterEach(() => vi.unstubAllGlobals());

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
    await flush();
  } finally {
    bundles.forEach((bundle) => bundle.dispose?.());
    host.stop();
  }
});
