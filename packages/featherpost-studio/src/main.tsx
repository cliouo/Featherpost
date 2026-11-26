import { createRoot } from "react-dom/client";
import { startupLocaleReady, tauriBridge } from "./bridge/tauri";
import { Host } from "./host/host";
import { ComposeController } from "./compose/controller";
import { composeModule } from "./compose/module";
import { SettingsController } from "./settings/controller";
import { LanguageController } from "./settings/language";
import { settingsModule, SETTINGS_VIEW } from "./settings/module";
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
import "./command-palette.css";

const host = new Host(tauriBridge);
const composer = new ComposeController(
  tauriBridge.compose,
  (id) => host.openView(id),
  (text) => host.notify(text),
);
const settings = new SettingsController(tauriBridge.settings, () => {
  void tauriBridge.refresh().catch(() => {});
});
const language = new LanguageController(tauriBridge.locale);
const openSettingsWith = (action: () => void) => {
  if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
    action();
    return;
  }
  host.runAfterLeave(SETTINGS_VIEW, () => {
    if (host.openView(SETTINGS_VIEW)) action();
  });
};
const openSettingsSection = (sectionId: string) => {
  if (host.state.getSnapshot().activeViewId === SETTINGS_VIEW) {
    settings.selectSection(sectionId);
    return;
  }
  openSettingsWith(() => {
    settings.activate(sectionId);
  });
};
const mailbox = studioMailbox(host, composer, () => {
  openSettingsWith(() => {
    settings.openSetup();
  });
});
host.registerBuiltin(mailbox.bundle);
host.registerBuiltin(composeModule(host, composer));
host.registerBuiltin(settingsModule(host, settings, language));
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
        language={language}
        setup={mailbox.setup}
        manageAccounts={() => {
          openSettingsWith(() => {
            settings.openAccounts();
          });
        }}
        openSettingsSection={openSettingsSection}
      />
      </ChoiceRenderer.Provider>
    </ErrorBoundary>,
  );
});
