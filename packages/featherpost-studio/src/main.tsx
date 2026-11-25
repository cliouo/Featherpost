import { createRoot } from "react-dom/client";
import { startupLocaleReady, tauriBridge } from "./bridge/tauri";
import { Host } from "./host/host";
import { ComposeController } from "./compose/controller";
import { composeModule } from "./compose/module";
import { SettingsController } from "./settings/controller";
import { settingsModule } from "./settings/module";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { studioMailbox } from "./mailbox";
import { Studio } from "./Shell";
import { ChoiceRenderer } from "./settings/Choice";
import { StudioSelect } from "./StudioSelect";
import "./styles/tokens.css";
import "./styles/scrollbars.css";
import "./settings/forms.css";
import "./compose/compose.css";
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
