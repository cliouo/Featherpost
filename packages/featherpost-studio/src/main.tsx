import { createRoot } from "react-dom/client";
import { startupLocaleReady, tauriBridge } from "../../maild-ui/src/bridge/tauri";
import { Host } from "../../maild-ui/src/host/host";
import { ComposeController } from "../../maild-ui/src/compose/controller";
import { composeModule } from "../../maild-ui/src/compose/module";
import { SettingsController } from "../../maild-ui/src/settings/controller";
import { settingsModule } from "./settings/module";
import { ErrorBoundary } from "../../maild-ui/src/app/ErrorBoundary";
import { studioMailbox } from "./mailbox";
import { Studio } from "./Shell";
import { ChoiceRenderer } from "../../maild-ui/src/settings/Choice";
import { StudioSelect } from "./StudioSelect";
import "../../maild-ui/src/styles/tokens.css";
import "../../maild-ui/src/styles/scrollbars.css";
import "./settings/forms.css";
import "../../maild-ui/src/compose/compose.css";
import "./studio.css";
import "./navigation.css";
import "./activity.css";
import "./settings/settings.css";
import "./select.css";
import "./contextmenu.css";

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

// ADR-0048：首帧前等启动语言（最多几百毫秒），英文用户不会先看到一帧中文。
void startupLocaleReady().then(() => {
  createRoot(document.getElementById("root")!).render(
    <ErrorBoundary
      onReset={() => {
        void host.bridge.refresh().catch(() => {});
      }}
    >
      <ChoiceRenderer.Provider value={StudioSelect}>
      <Studio
        host={host}
        mailbox={mailbox.controller}
        composer={composer}
        setup={mailbox.setup}
        manageAccounts={() => {
          settings.openAccounts();
          host.openRole("settings");
        }}
      />
      </ChoiceRenderer.Provider>
    </ErrorBoundary>,
  );
});
