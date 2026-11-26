import { m } from "@featherpost/i18n/messages";
import { Choice } from "./Choice";
import { lifecycleLabel, packageText, permissionLabel, useUiLocale } from "../i18n";
import { useState } from "react";
import { useStore } from "../host/hooks";
import type { SettingsController } from "./controller";
import type { PackageKind, PackagePreview } from "./types";
import { BRAND } from "../brand";
import { Switch } from "../ui";
export { permissionLabels as permissions } from "../i18n";
export function PermissionList({ values }: { values: string[] }) {
  useUiLocale();
  return (
    <ul className="permission-list">
      {values.length ? (
        values.map((v) => (
          <li key={v}>
            <strong>{permissionLabel(v)}</strong>
            <code>{v}</code>
          </li>
        ))
      ) : (
        <li>{m.extensions_permissions_none()}</li>
      )}
    </ul>
  );
}
function Preview({ p }: { p: PackagePreview }) {
  useUiLocale();
  const kind =
    p.kind === "frontend"
      ? m.extensions_kind_frontend()
      : m.extensions_kind_plugin();
  // 只在渲染时本地化：预览对象本身原样回传给 commit。
  const text = packageText(p);
  return (
    <>
      <h3>{text.name}</h3>
      {text.description && <p className="package-description">{text.description}</p>}
      <p className="group-note">
        {p.id} · {p.version} · {kind} ·{" "}
        {lifecycleLabel(p.lifecycle)}
      </p>
      <h4>{m.extensions_preview_permissions()}</h4>
      <PermissionList
        values={[...new Set([...p.capabilities, ...p.permissions])]}
      />
      <p className="group-note">
        {m.extensions_preview_warning()}
      </p>
    </>
  );
}
export function Extensions({
  controller,
  children,
}: {
  controller: SettingsController;
  children?: React.ReactNode;
}) {
  useUiLocale();
  const s = useStore(controller.store),
    [kind, setKind] = useState<PackageKind>("plugin"),
    catalog = s.packages;
  return (
    <>
      <h2>{m.extensions_title()}</h2>
      <p className="page-intro">{m.extensions_intro()}</p>
      {s.choice ? (
        <section className="package-preview">
          <div className="section-label">{m.extensions_install_confirm()}</div>
          <Preview p={s.choice.preview} />
          <p className="package-source">
            {m.extensions_preview_source({ source: s.choice.source })}
          </p>
          {controller.dependencies(s.choice.preview.id).length > 0 && (
            <p className="group-note">
              {m.extensions_dependencies({
                accounts: controller
                  .dependencies(s.choice.preview.id)
                  .join(m.extensions_list_separator()),
              })}
            </p>
          )}
          <div className="settings-actions">
            <button
              className="primary"
              disabled={s.busy || !s.online}
              onClick={() => void controller.install()}
            >
              {m.extensions_install_action()}
            </button>
            <button
              className="secondary"
              disabled={s.busy}
              onClick={() => controller.cancelPackage()}
            >
              {m.extensions_action_cancel()}
            </button>
          </div>
        </section>
      ) : s.rollback ? (
        <section className="package-preview">
          <div className="section-label">{m.extensions_rollback_confirm()}</div>
          <Preview p={s.rollback} />
          <p>{m.extensions_rollback_note()}</p>
          <div className="settings-actions">
            <button
              className="primary"
              disabled={s.busy || !s.online}
              onClick={() => void controller.rollback()}
            >
              {m.extensions_rollback_action()}
            </button>
            <button
              className="secondary"
              disabled={s.busy}
              onClick={() => controller.cancelPackage()}
            >
              {m.extensions_action_cancel()}
            </button>
          </div>
        </section>
      ) : (
        <>
          <h3>{m.extensions_frontends_title()}</h3>
          {catalog?.frontends.map((p) => (
            <article className="managed-package" key={p.id}>
              <div className="setting-row">
                <div className="setting-copy">
                  <strong>{packageText(p).name}</strong>
                  {packageText(p).description && <p className="package-description">{packageText(p).description}</p>}
                  <p>
                    {p.id} · {p.version}
                  </p>
                </div>
                {catalog.default === p.id && (
                  <span className="tag">{m.extensions_frontend_default()}</span>
                )}
                {p.builtin && (
                  <span className="tag">{m.extensions_frontend_builtin()}</span>
                )}
              </div>
              <details>
                <summary>{m.extensions_details()}</summary>
                <PermissionList
                  values={[...new Set([...p.capabilities, ...p.permissions])]}
                />
                <div className="settings-actions">
                  <button
                    className="secondary"
                    disabled={s.busy || !s.online || catalog.default === p.id}
                    onClick={() =>
                      controller.confirmPackage("frontend", p.id, "default")
                    }
                  >
                    {m.extensions_action_make_default()}
                  </button>
                  {!p.builtin && (
                    <>
                      <button
                        className="secondary"
                        disabled={s.busy || !s.online}
                        onClick={() =>
                          void controller.previewRollback("frontend", p.id)
                        }
                      >
                        {m.extensions_rollback_view()}
                      </button>
                      <button
                        className="text-button danger"
                        disabled={
                          s.busy ||
                          !s.online ||
                          catalog.default === p.id ||
                          p.id === BRAND.bundledFrontendId
                        }
                        onClick={() =>
                          controller.confirmPackage("frontend", p.id, "remove")
                        }
                      >
                        {m.extensions_action_remove()}
                      </button>
                    </>
                  )}
                </div>
              </details>
            </article>
          ))}
          <h3>{m.extensions_plugins_title()}</h3>
          {catalog?.plugins.length === 0 && (
            <p className="group-note">
              {m.extensions_plugins_empty()}
            </p>
          )}
          {catalog?.plugins.map((p) => (
            <article className="managed-package" key={p.id}>
              <div className="setting-row">
                <div className="setting-copy">
                  <strong>{packageText(p).name}</strong>
                  {packageText(p).description && <p className="package-description">{packageText(p).description}</p>}
                  <p>
                    {p.id} · {p.version} · {pluginState(p.state)}
                  </p>
                  {controller.dependencies(p.id).length > 0 && (
                    <p>
                      {m.extensions_dependencies({
                        accounts: controller
                          .dependencies(p.id)
                          .join(m.extensions_list_separator()),
                      })}
                    </p>
                  )}
                </div>
                <Switch
                  checked={p.enabled}
                  aria-label={m.extensions_toggle_label({ name: packageText(p).name })}
                  disabled={s.busy || !s.online}
                  onCheckedChange={(next) =>
                    controller.confirmPackage(
                      "plugin",
                      p.id,
                      next ? "enable" : "disable",
                    )
                  }
                />
              </div>
              <details>
                <summary>{m.extensions_details()}</summary>
                <PermissionList values={p.capabilities} />
                <div className="settings-actions">
                  {p.has_ui && (
                    <button
                      className="secondary"
                      disabled={s.busy || !s.online}
                      onClick={() => void controller.openProvider(p.id)}
                    >
                      {m.extensions_open_provider()}
                    </button>
                  )}
                  <button
                    className="secondary"
                    disabled={s.busy || !s.online}
                    onClick={() =>
                      void controller.previewRollback("plugin", p.id)
                    }
                  >
                    {m.extensions_rollback_view()}
                  </button>
                  <button
                    className="text-button danger"
                    disabled={s.busy || !s.online}
                    onClick={() =>
                      controller.confirmPackage("plugin", p.id, "remove")
                    }
                  >
                    {m.extensions_action_remove()}
                  </button>
                </div>
              </details>
            </article>
          ))}
          <div className="settings-actions">
            <Choice
              aria-label={m.extensions_package_kind_label()}
              value={kind}
              onValueChange={(value) => setKind(value as PackageKind)}
              disabled={s.busy}
            >
              <option value="plugin">{m.extensions_package_kind_plugin()}</option>
              <option value="frontend">
                {m.extensions_package_kind_frontend()}
              </option>
            </Choice>
            <button
              className="secondary"
              disabled={!s.online || s.busy || s.loading || !catalog}
              onClick={() => void controller.pickPackage(kind)}
            >
              {m.extensions_install_from_folder()}
            </button>
          </div>
          <p className="group-note">
            {m.extensions_install_note()}
          </p>
          <div className="settings-divider" />
          <h3>{m.extensions_modules_title()}</h3>
          {children}
          <p className="group-note">
            {m.extensions_modules_note()}
          </p>
        </>
      )}
    </>
  );
}
const PLUGIN_STATE: Readonly<Record<string, () => string>> = {
  running: m.extensions_plugin_state_running,
  disabled: m.extensions_plugin_state_disabled,
  stopped: m.extensions_plugin_state_stopped,
  independent: m.extensions_plugin_state_independent,
  blocked: m.extensions_plugin_state_blocked,
  backoff: m.extensions_plugin_state_backoff,
  starting: m.extensions_plugin_state_starting,
};
function pluginState(state: string) {
  const label = Object.prototype.hasOwnProperty.call(PLUGIN_STATE, state)
    ? PLUGIN_STATE[state]
    : undefined;
  return label ? label() : m.extensions_plugin_state_unknown({ state });
}
