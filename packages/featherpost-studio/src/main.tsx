import { createRoot } from "react-dom/client";
import { tauriBridge } from "../../maild-ui/src/bridge/tauri";
import { Host } from "../../maild-ui/src/host/host";
import { ComposeController } from "../../maild-ui/src/compose/controller";
import { composeModule } from "../../maild-ui/src/compose/module";
import { SettingsController } from "../../maild-ui/src/settings/controller";
import { settingsModule } from "../../maild-ui/src/settings/module";
import { ErrorBoundary } from "../../maild-ui/src/app/ErrorBoundary";
import { studioMailbox } from "./mailbox";
import { Studio } from "./Studio";
import "../../maild-ui/src/styles/tokens.css";
import "../../maild-ui/src/settings/settings.css";
import "../../maild-ui/src/compose/compose.css";
import "./studio.css";

const host = new Host(tauriBridge);
const composer = new ComposeController(
  tauriBridge.compose,
  (id) => host.openView(id),
  (text) => host.notify(text),
);
const settings = new SettingsController(tauriBridge.settings, () => {
  void tauriBridge.refresh().catch(() => {});
});
const mailbox = studioMailbox(host, composer, () => {
  settings.openSetup();
  host.openRole("settings");
});
host.registerBuiltin(mailbox.bundle);
host.registerBuiltin(composeModule(host, composer));
host.registerBuiltin(settingsModule(host, settings));
composer.setBeforeClose(() => settings.beforeClose());
composer.store.subscribe(() => {
  if (!composer.store.getSnapshot().closing) settings.releaseClose();
});
host.finishAssembly();
void host.start();

createRoot(document.getElementById("root")!).render(
  <ErrorBoundary
    onReset={() => {
      void host.bridge.refresh().catch(() => {});
    }}
  >
    <Studio
      host={host}
      mailbox={mailbox.controller}
      composer={composer}
      setup={mailbox.setup}
    />
  </ErrorBoundary>,
);
