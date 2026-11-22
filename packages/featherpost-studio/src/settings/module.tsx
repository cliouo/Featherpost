import { m } from "@featherpost/i18n/messages";
import {
  lifecycleLabel,
  permissionLabel,
  uiLocale,
  useUiLocale,
} from "../../../maild-ui/src/i18n";
import { useEffect, useState } from "react";
import { SettingsController } from "../../../maild-ui/src/settings/controller";
import { Accounts } from "./Accounts";
import { Addresses } from "../../../maild-ui/src/settings/Addresses";
import { Notifications } from "../../../maild-ui/src/settings/Notifications";
import { Extensions as ManagedExtensions } from "../../../maild-ui/src/settings/Extensions";
import { Dialog } from "../../../maild-ui/src/mailbox/Dialog";

import type { Host } from "../../../maild-ui/src/host/host";
import { useExternal, useStore } from "../../../maild-ui/src/host/hooks";
import type { Bundle, ViewProps } from "../../../maild-ui/src/host/types";
import { Appearance } from "./Appearance";
import { LanguageController } from "../../../maild-ui/src/settings/language";
import { Icon } from "../../../maild-ui/src/app/Icon";
import { BRAND } from "../../../maild-ui/src/brand";

export const SETTINGS_VIEW = "maild.settings.page";

// i18n-lint-allow-cjk: 全角加号是按钮的装饰图标（aria-hidden），不是文案
const PLUS = "＋";

function plural(count: number, one: () => string, other: () => string) {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? one()
    : other();
}

/** 各设置分区的说明；存消息函数，渲染时按当前语言取值。 */
const descriptions: Record<string, () => string> = {
  "maild.settings.accounts": m.studio_settings_description_accounts,
  "maild.settings.appearance": m.studio_settings_description_appearance,
  "maild.settings.notifications": m.studio_settings_description_notifications,
  "maild.settings.addresses": m.studio_settings_description_addresses,
  "maild.settings.extensions": m.studio_settings_description_extensions,
  "maild.settings.connection": m.studio_settings_description_connection,
};

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
  // ADR-0048：语言选择与默认前端共用一个控制器；偏好归 daemon。
  const language = new LanguageController(host.bridge.locale);
  language.start();

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
      catch { setRecoveryError(m.studio_connection_recovery_error({ app: BRAND.appBundleName })); }
      finally { setOpening(false); }
    }
    return (
      <>
        <h2>{m.studio_settings_section_connection()}</h2>
        <p className="page-intro">{m.studio_connection_intro()}</p>
        <p className="group-note">{m.studio_connection_backup_note()}</p>
        <p className="page-intro">{m.studio_connection_frontend_intro()}</p>
        <Row
          title={m.studio_connection_daemon()}
          description={
            snapshot.version
              ? `maild ${snapshot.version}`
              : m.studio_connection_version_unknown()
          }
        >
          <span className={`tag${snapshot.phase === "online" ? "" : " warn"}`}>
            {phaseLabel(snapshot.phase)}
          </span>
        </Row>
        <Row
          title={m.studio_connection_principal()}
          description={
            snapshot.principal ??
            snapshot.identity_failure ??
            m.studio_connection_unbound()
          }
        >
          <span className={`tag${snapshot.principal ? "" : " warn"}`}>
            {snapshot.principal
              ? m.studio_connection_principal_frontend()
              : m.studio_connection_unbound()}
          </span>
        </Row>
        <Row title={m.studio_connection_recovery()} description={snapshot.phase === "online"
          ? m.studio_connection_recovery_online()
          : m.studio_connection_recovery_offline({ app: BRAND.appBundleName })}>
          <button type="button" disabled={opening || snapshot.phase !== "online"} onClick={() => void openRecovery()}>
            {opening ? m.studio_connection_opening() : m.studio_connection_open_recovery()}
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
                  ? m.studio_connection_default_frontend()
                  : m.studio_connection_default_other({
                      frontend: frontendInfo.default_frontend,
                    })}
              </span>
            </Row>
            <div className="section-label">
              {m.studio_connection_permissions()}
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
        <p className="group-note">{m.studio_extensions_note()}</p>
        {rows.map((module) => {
          const enabled =
            host.isModuleEnabled(module.moduleId) || module.builtin;
          return (
            <Row
              key={module.moduleId}
              title={module.title}
              description={`${module.moduleId}${
                enabled
                  ? ` · ${plural(
                      module.contributions,
                      () =>
                        m.studio_extensions_contributions_one({
                          count: module.contributions,
                        }),
                      () =>
                        m.studio_extensions_contributions_other({
                          count: module.contributions,
                        }),
                    )}`
                  : ""
              }`}
            >
              {module.builtin ? (
                <span className="tag">{m.studio_extensions_builtin()}</span>
              ) : (
                <button
                  type="button"
                  role="switch"
                  aria-checked={enabled}
                  aria-label={m.studio_extensions_enable({
                    module: module.title,
                  })}
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
            <div className="section-label">
              {m.studio_extensions_diagnostics()}
            </div>
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
      document.body.dataset.studioSettings = "true";
      void controller.load();
      return () => { delete document.body.dataset.studioSettings; };
    }, []);
    const navigate = (id: string) => {
      const next = () => controller.activate(id);
      if (controller.requestLeave(next)) next();
    };
    const Section = active?.render;
    const accounts = active?.id === "maild.settings.accounts";
    const refreshDisabled = state.busy || state.loading || state.closeLocked || !state.online || controller.dirty;
    const refresh = () => void (state.section === "maild.settings.extensions" ? controller.reconcilePackages() : controller.load());
    return (
      <div className="page settings-page studio-settings">
        <header className="studio-settings-topbar" data-tauri-drag-region>
          <div className="studio-brand" data-tauri-drag-region>
            <svg className="brand-feather" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <path d="M20 3C11 1 4 8 5 17l-2 4 5-2C17 20 22 12 20 3ZM5 19 16 7M9 15v-5M9 15h5" />
            </svg>
            <strong data-tauri-drag-region>Featherpost</strong>
          </div>
          <span className="studio-settings-location" data-tauri-drag-region>{m.studio_settings_title()}</span>
          <button type="button" className="secondary studio-return" onClick={() => host.openRole("mailbox")}>
            <Icon name="back" />{m.studio_settings_back()}
          </button>
        </header>
        <div className="settings-layout">
          <nav className="settings-nav" aria-label={m.studio_settings_nav_label()}>
            <h1>{m.studio_settings_title()}</h1>
            {[
              { label: m.studio_settings_group_mail(), rows: sections.filter(section => (section.order ?? 0) < 70) },
              { label: m.studio_settings_group_app(), rows: sections.filter(section => (section.order ?? 0) >= 70) },
            ].map(group => group.rows.length > 0 && (
              <div className="studio-settings-nav-group" key={group.label}>
                <p>{group.label}</p>
                {group.rows.map(section => (
                  <button key={section.id} type="button" className={section.id === active?.id ? "active" : ""}
                    disabled={state.closeLocked} aria-current={section.id === active?.id ? "page" : undefined}
                    onClick={() => navigate(section.id)}>
                    <span>{section.title}</span>
                    {section.id === "maild.settings.accounts" && state.profiles !== null && <small>{state.profiles.length}</small>}
                  </button>
                ))}
              </div>
            ))}
            <div className="studio-settings-nav-footer">
              <strong>Featherpost Studio</strong>
              <span><i className={state.online ? "status-dot" : "status-dot warn"} />{state.online ? m.studio_settings_daemon_online() : m.studio_settings_daemon_offline()}</span>
            </div>
          </nav>
          <div className="settings-content">
            <div className="page-column">
              <div className="studio-settings-heading">
                <div>
                  <div className="studio-settings-title"><h2>{active ? active.title : m.studio_settings_title()}</h2>
                    {accounts && state.profiles !== null && <span>{plural(state.profiles.length, () => m.studio_mailbox_count_one({ count: state.profiles!.length }), () => m.studio_mailbox_count_other({ count: state.profiles!.length }))}</span>}
                  </div>
                  {active && Object.prototype.hasOwnProperty.call(descriptions, active.id) && <p>{descriptions[active.id]()}</p>}
                </div>
                {accounts ? (
                  <button type="button" className="primary" disabled={!state.online || state.busy || state.loading || state.closeLocked || state.profiles === null}
                    onClick={() => { const next = () => controller.beginAccount(); if (controller.requestLeave(next)) next(); }}>
                    <span aria-hidden="true">{PLUS}</span> {m.studio_account_add_mailbox()}
                  </button>
                ) : active?.id === "maild.settings.appearance" ? (
                  <div className="studio-heading-actions"><span className="studio-instant">✓ {m.studio_settings_instant()}</span>
                    <button type="button" className="text-button" aria-label={m.studio_settings_refresh_label()} title={m.studio_settings_refresh_label()} disabled={refreshDisabled} onClick={refresh}><Icon name="refresh" /></button>
                  </div>
                ) : (
                  <button type="button" className="text-button" disabled={refreshDisabled} onClick={refresh}><Icon name="refresh" />{m.studio_settings_refresh()}</button>
                )}
              </div>
              {!state.online && <p className="settings-alert" role="status">{m.studio_settings_offline()}</p>}
              {state.error && <p className="settings-alert" role="alert">{state.error}</p>}
              {state.notice && <p className="settings-alert settings-notice" role="status">{state.notice}</p>}
              {(state.loading || state.busy) && <p className="settings-progress" role="status">{state.busy ? m.studio_settings_busy() : m.studio_settings_loading()}</p>}
              <fieldset disabled={state.closeLocked} className={accounts || active?.id === "maild.settings.appearance" ? "studio-section-bare" : "studio-settings-section"}>
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
                {m.studio_settings_cancel()}
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

  // 标题写成 getter：注册表保留访问器（copyContribution），切换界面语言后按当前语言重新成文。
  return {
    moduleId: "maild.settings",
    apiVersion: 1,
    get title() {
      return m.studio_settings_title();
    },
    views: [
      {
        id: SETTINGS_VIEW,
        get title() {
          return m.studio_settings_title();
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
          return m.studio_settings_section_accounts();
        },
        order: 0,
        render: () => <Accounts controller={controller} />,
      },
      {
        id: "maild.settings.notifications",
        get title() {
          return m.studio_settings_section_notifications();
        },
        order: 50,
        render: () => <Notifications controller={controller} />,
      },
      {
        id: "maild.settings.addresses",
        get title() {
          return m.studio_settings_section_addresses();
        },
        order: 60,
        render: () => <Addresses controller={controller} />,
      },
      {
        id: "maild.settings.appearance",
        get title() {
          return m.studio_settings_section_appearance();
        },
        order: 10,
        render: () => <Appearance host={host} language={language} />,
      },
      {
        id: "maild.settings.connection",
        get title() {
          return m.studio_settings_section_connection();
        },
        order: 90,
        render: Connection,
      },
      {
        id: "maild.settings.extensions",
        get title() {
          return m.studio_settings_section_extensions();
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

export function phaseLabel(phase: string): string {
  switch (phase) {
    case "online":
      return m.studio_phase_online();
    case "connecting":
      return m.studio_phase_connecting();
    case "synchronizing":
      return m.studio_phase_synchronizing();
    case "retry_waiting":
      return m.studio_phase_retry_waiting();
    case "stopped":
      return m.studio_phase_stopped();
    default:
      return m.studio_phase_offline();
  }
}
