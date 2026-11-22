import { m } from "@featherpost/i18n/messages";
import { lifecycleLabel, permissionLabel, uiLocale, useUiLocale } from "../i18n";
import { useEffect, useState } from "react";
import { SettingsController } from "./controller";
import { Accounts } from "./Accounts";
import { Addresses } from "./Addresses";
import { Notifications } from "./Notifications";
import { Extensions as ManagedExtensions } from "./Extensions";
import { Dialog } from "../mailbox/Dialog";

import type { Host } from "../host/host";
import { useExternal, useStore } from "../host/hooks";
import type { Bundle, ViewProps } from "../host/types";
import type { Preferences } from "../preferences";
import { BRAND } from "../brand";
import { Choice } from "./Choice";
import { LANGUAGE_OPTIONS, LanguageController, languageChoice, type LanguageChoice } from "./language";

export const SETTINGS_VIEW = "maild.settings.page";


function Row({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="setting-row">
      <div className="setting-copy">
        <strong>{title}</strong>
        {description && <p>{description}</p>}
      </div>
      {children}
    </div>
  );
}

export function settingsModule(
  host: Host,
  controller = new SettingsController(host.bridge.settings),
): Bundle {
  const language = new LanguageController(host.bridge.locale);
  language.start();

  /** ADR-0048：界面语言。偏好归 daemon，所有窗口共用；语言名用其自身语言书写。 */
  function Language() {
    useUiLocale();
    const state = useStore(language.store);
    const online = useStore(host.state).snapshot.phase === "online";
    useEffect(() => {
      if (online) void language.load();
    }, [online]);
    if (!language.available) return null;
    const choice = languageChoice(state.preference);
    const locked = state.unsupported || state.preference?.persisted === false;
    return (
      <>
        <Row
          title={m.settings_language()}
          description={
            state.unsupported
              ? m.settings_language_unavailable()
              : state.preference?.persisted === false
                ? m.settings_language_not_persisted()
                : m.settings_language_hint()
          }
        >
          <Choice
            aria-label={m.settings_language()}
            value={choice ?? ""}
            disabled={state.busy || locked || !online || choice === null}
            onValueChange={(value) => {
              if (value === "system" || LANGUAGE_OPTIONS.some((option) => option.value === value))
                void language.choose(value as LanguageChoice);
            }}
          >
            {choice === null && <option value="" disabled />}
            <option value="system">{m.settings_language_system()}</option>
            {LANGUAGE_OPTIONS.map((option) => (
              <option key={option.value} value={option.value} lang={option.value}>
                {option.name()}
              </option>
            ))}
            {choice === "custom" && (
              <option value="custom" disabled>
                {m.settings_language_custom({ languages: state.preference!.languages.join(", ") })}
              </option>
            )}
          </Choice>
        </Row>
        {state.error && <p className="group-note" role="alert">{state.error}</p>}
      </>
    );
  }

  function Appearance(_props: ViewProps) {
    useUiLocale();
    const { preferences } = useStore(host.state);
    const update = (patch: Partial<Preferences>) =>
      host.setPreferences({ ...preferences, ...patch });
    return (
      <>
        <h2>{m.settings_appearance_title()}</h2>
        <p className="page-intro">
          {m.settings_appearance_intro()}
        </p>
        <Row
          title={m.settings_appearance_theme()}
          description={m.settings_appearance_theme_hint()}
        >
          <Choice
            aria-label={m.settings_appearance_theme()}
            value={preferences.theme}
            onValueChange={(value) =>
              update({ theme: value as Preferences["theme"] })
            }
          >
            <option value="system">{m.settings_appearance_theme_system()}</option>
            <option value="light">{m.settings_appearance_theme_light()}</option>
            <option value="dark">{m.settings_appearance_theme_dark()}</option>
          </Choice>
        </Row>
        <Row
          title={m.settings_appearance_density()}
          description={m.settings_appearance_density_hint()}
        >
          <Choice
            aria-label={m.settings_appearance_density()}
            value={preferences.density}
            onValueChange={(value) =>
              update({ density: value as Preferences["density"] })
            }
          >
            <option value="comfortable">
              {m.settings_appearance_density_comfortable()}
            </option>
            <option value="compact">
              {m.settings_appearance_density_compact()}
            </option>
          </Choice>
        </Row>
        <Language />
      </>
    );
  }

  function Connection(_props: ViewProps) {
    useUiLocale();
    const { snapshot, frontendInfo, frontendInfoError } = useStore(host.state);
    const [opening, setOpening] = useState(false);
    const [recoveryError, setRecoveryError] = useState<string | null>(null);
    async function openRecovery() {
      if (opening || snapshot.phase !== "online") return;
      setOpening(true);
      setRecoveryError(null);
      try { await host.bridge.settings.packageAction({ action: "recovery" }); }
      catch { setRecoveryError(m.settings_connection_recovery_failed({ app: BRAND.appBundleName })); }
      finally { setOpening(false); }
    }
    return (
      <>
        <h2>{m.settings_connection_title()}</h2>
        <p className="page-intro">
          {m.settings_connection_intro()}
        </p>
        <p className="group-note">
          {m.settings_connection_backup_note()}
        </p>
        <p className="page-intro">
          {m.settings_connection_frontend_note()}
        </p>
        <Row
          title={m.settings_connection_daemon()}
          description={
            snapshot.version
              ? `maild ${snapshot.version}`
              : m.settings_connection_version_unknown()
          }
        >
          <span className={`tag${snapshot.phase === "online" ? "" : " warn"}`}>
            {phaseLabel(snapshot.phase)}
          </span>
        </Row>
        <Row
          title={m.settings_connection_principal()}
          description={
            snapshot.principal ??
            snapshot.identity_failure ??
            m.settings_connection_unbound()
          }
        >
          <span className={`tag${snapshot.principal ? "" : " warn"}`}>
            {snapshot.principal
              ? m.settings_connection_principal_frontend()
              : m.settings_connection_unbound()}
          </span>
        </Row>
        <Row title={m.settings_connection_recovery()} description={snapshot.phase === "online"
          ? m.settings_connection_recovery_hint()
          : m.settings_connection_recovery_offline({ app: BRAND.appBundleName })}>
          <button type="button" disabled={opening || snapshot.phase !== "online"} onClick={() => void openRecovery()}>
            {opening
              ? m.settings_connection_recovery_opening()
              : m.settings_connection_recovery_open()}
          </button>
        </Row>
        {recoveryError && <p className="group-note" role="alert">{recoveryError}</p>}
        {frontendInfo && (
          <>
            <Row
              title={frontendInfo.name}
              description={`${frontendInfo.id} · ${frontendInfo.version} · ${lifecycleLabel(frontendInfo.lifecycle)}`}
            >
              <span className="tag">
                {frontendInfo.is_default
                  ? m.settings_connection_frontend_default()
                  : m.settings_connection_frontend_default_other({
                      id: frontendInfo.default_frontend,
                    })}
              </span>
            </Row>
            <div className="section-label">
              {m.settings_connection_permissions()}
            </div>
            {[
              ...frontendInfo.permissions,
              ...frontendInfo.capabilities.flatMap(routeToWords),
            ]
              .filter((word, index, all) => all.indexOf(word) === index)
              .sort()
              .map((word) => (
                <Row
                  key={word}
                  title={permissionLabel(word)}
                  description={word}
                />
              ))}
          </>
        )}
        {frontendInfoError && <p className="group-note">{frontendInfoError}</p>}
      </>
    );
  }

  function Extensions(_props: ViewProps) {
    useUiLocale();
    const registry = useExternal(
      host.registry.subscribe,
      host.registry.getSnapshot,
    );
    const { diagnostics } = useStore(host.state);
    // 已注册的模块从注册表来（含贡献数）；停用的可选模块从宿主来（只有名字）。
    const rows = [
      ...registry.modules,
      ...host
        .optionalModules()
        .filter((module) => !module.enabled)
        .map((module) => ({
          moduleId: module.moduleId,
          title: module.title,
          builtin: false,
          contributions: 0,
        })),
    ].sort(
      (a, b) =>
        Number(b.builtin) - Number(a.builtin) ||
        a.moduleId.localeCompare(b.moduleId),
    );
    return (
      <>
        <p className="group-note">
          {m.extensions_modules_disable_note()}
        </p>
        {rows.map((module) => {
          const enabled =
            host.isModuleEnabled(module.moduleId) || module.builtin;
          return (
            <Row
              key={module.moduleId}
              title={module.title}
              description={`${module.moduleId}${enabled ? ` · ${contributionCount(module.contributions)}` : ""}`}
            >
              {module.builtin ? (
                <span className="tag">{m.extensions_module_builtin()}</span>
              ) : (
                <button
                  type="button"
                  role="switch"
                  aria-checked={enabled}
                  aria-label={m.extensions_toggle_label({ name: module.title })}
                  className="toggle"
                  onClick={() =>
                    host.setModuleEnabled(module.moduleId, !enabled)
                  }
                >
                  <span className="toggle-track" />
                </button>
              )}
            </Row>
          );
        })}
        {diagnostics.length > 0 && (
          <>
            <div className="section-label">{m.extensions_diagnostics()}</div>
            {diagnostics.map((line, index) => (
              <p key={index} className="group-note">
                {line}
              </p>
            ))}
          </>
        )}
      </>
    );
  }

  function Page(_props: ViewProps) {
    useUiLocale();
    const registry = useExternal(
      host.registry.subscribe,
      host.registry.getSnapshot,
    );
    const sections = registry.settings.map(({ value }) => value);
    const state = useStore(controller.store);
    const active =
      sections.find((section) => section.id === state.section) ?? sections[0];
    useEffect(() => {
      void controller.load();
    }, []);
    const navigate = (id: string) => {
      const next = () => controller.activate(id);
      if (controller.requestLeave(next)) next();
    };
    const Section = active?.render;
    return (
      <div className="page settings-page">
        <div className="page-toolbar" data-tauri-drag-region="deep">
          <button
            type="button"
            className="text-button"
            onClick={() => host.openRole("mailbox")}
          >
            {m.settings_page_back()}
          </button>
          <h1>{m.settings_page_title()}</h1>
          <button
            type="button"
            className="text-button"
            disabled={
              state.busy ||
              state.loading ||
              state.closeLocked ||
              !state.online ||
              controller.dirty
            }
            onClick={() =>
              void (state.section === "maild.settings.extensions"
                ? controller.reconcilePackages()
                : controller.load())
            }
          >
            {m.settings_page_refresh()}
          </button>
          <small>{m.settings_page_shortcut()}</small>
        </div>
        <div className="settings-layout">
          <nav className="settings-nav" aria-label={m.settings_page_sections()}>
            {sections.map((section) => (
              <button
                key={section.id}
                type="button"
                className={section.id === active?.id ? "active" : ""}
                disabled={state.closeLocked}
                aria-current={section.id === active?.id ? "page" : undefined}
                onClick={() => navigate(section.id)}
              >
                {section.title}
              </button>
            ))}
          </nav>
          <div className="settings-content">
            <div className="page-column">
              {!state.online && (
                <p className="settings-alert" role="status">
                  {m.settings_page_offline()}
                </p>
              )}
              {state.error && (
                <p className="settings-alert" role="alert">
                  {state.error}
                </p>
              )}
              {state.notice && (
                <p className="settings-alert settings-notice" role="status">
                  {state.notice}
                </p>
              )}
              {(state.loading || state.busy) && (
                <p className="settings-progress" role="status">
                  {state.busy
                    ? m.settings_page_busy()
                    : m.settings_page_loading()}
                </p>
              )}
              <fieldset disabled={state.closeLocked}>
                {Section && <Section active />}
              </fieldset>
            </div>
          </div>
        </div>
        {state.confirm && (
          <Dialog
            title={state.confirm.title}
            onClose={() => controller.cancelConfirmation()}
          >
            <p className="settings-confirm-copy">{state.confirm.body}</p>
            <div className="settings-actions">
              <button
                className="secondary"
                onClick={() => controller.cancelConfirmation()}
              >
                {m.settings_confirm_cancel()}
              </button>
              <button
                className={
                  state.confirm.danger ? "secondary danger" : "primary"
                }
                onClick={() => controller.confirm()}
              >
                {state.confirm.label}
              </button>
            </div>
          </Dialog>
        )}
      </div>
    );
  }

  const stopState = host.state.subscribe(() =>
    controller.connection(host.state.getSnapshot().snapshot),
  );
  controller.connection(host.state.getSnapshot().snapshot);
  const stopGuard = host.beforeNavigate((_id, next, cancel) =>
    host.state.getSnapshot().activeViewId === SETTINGS_VIEW
      ? controller.requestLeave(next, cancel)
      : true,
  );
  const stopIntent = host.onIntent((intent) => {
    if (intent.route === "settings") host.openView(SETTINGS_VIEW);
  });

  return {
    moduleId: "maild.settings",
    apiVersion: 1,
    // 标题写成 getter：注册表保留访问器，切换语言后按当前语言重新成文。
    get title() {
      return m.settings_module_title();
    },
    views: [
      {
        id: SETTINGS_VIEW,
        get title() {
          return m.settings_page_title();
        },
        icon: "settings",
        role: "settings",
        order: 1000,
        render: Page,
      },
    ],
    settings: [
      {
        id: "maild.settings.accounts",
        get title() {
          return m.settings_section_accounts();
        },
        order: 0,
        render: () => <Accounts controller={controller} />,
      },
      {
        id: "maild.settings.notifications",
        get title() {
          return m.settings_section_notifications();
        },
        order: 50,
        render: () => <Notifications controller={controller} />,
      },
      {
        id: "maild.settings.addresses",
        get title() {
          return m.settings_section_addresses();
        },
        order: 60,
        render: () => <Addresses controller={controller} />,
      },
      {
        id: "maild.settings.appearance",
        get title() {
          return m.settings_section_appearance();
        },
        order: 10,
        render: Appearance,
      },
      {
        id: "maild.settings.connection",
        get title() {
          return m.settings_section_connection();
        },
        order: 90,
        render: Connection,
      },
      {
        id: "maild.settings.extensions",
        get title() {
          return m.settings_section_extensions();
        },
        order: 70,
        render: () => (
          <ManagedExtensions controller={controller}>
            <Extensions active />
          </ManagedExtensions>
        ),
      },
    ],
    dispose: () => {
      stopIntent();
      stopGuard();
      stopState();
      language.dispose();
      controller.dispose();
    },
  };
}

function routeToWords(route: string): string[] {
  switch (route) {
    case "inbox":
    case "message":
      return ["messages_read"];
    case "compose":
      return ["messages_send"];
    case "settings":
      return ["settings_read", "settings_write"];
    default:
      return [];
  }
}

function contributionCount(count: number): string {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? m.extensions_module_contributions_one({ count })
    : m.extensions_module_contributions_other({ count });
}

export function phaseLabel(phase: string): string {
  switch (phase) {
    case "online":
      return m.settings_phase_online();
    case "connecting":
      return m.settings_phase_connecting();
    case "synchronizing":
      return m.settings_phase_synchronizing();
    case "retry_waiting":
      return m.settings_phase_retry_waiting();
    case "stopped":
      return m.settings_phase_stopped();
    default:
      return m.settings_phase_disconnected();
  }
}
