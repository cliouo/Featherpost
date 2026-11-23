import { m } from "@featherpost/i18n/messages";
import { Choice } from "./Choice";
import { useRef, useState, type ReactNode } from "react";
import { PopoverMenu } from "../app/PopoverMenu";
import { useStore } from "../host/hooks";
import type { AccountProfile, Endpoint, ProbeStage } from "./types";
import { SettingsController, newDraft } from "./controller";
import {
  CREDENTIAL_LABEL,
  QUICK_PROVIDERS,
  domainOf,
  probeAdvice,
  providerById,
  serversFor,
  stageLabel,
  type ProviderPreset,
} from "./providers";
import { BRAND } from "../brand";
import { AccountReason } from "./AccountReason";
import { packageText, providerName, useUiLocale } from "../i18n";

export function Field({
  label,
  children,
  hint,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
}) {
  return (
    <label className="settings-field">
      <span>{label}</span>
      {children}
      {hint && <small>{hint}</small>}
    </label>
  );
}
export function needsRepair(p: AccountProfile) {
  return p.enabled && (p.runtime.state === "needs_credentials" || p.runtime.state === "error");
}
const ACCOUNT_STATE: Readonly<Record<string, () => string>> = {
  online: m.settings_account_state_online,
  connecting: m.settings_account_state_connecting,
  reconnecting: m.settings_account_state_reconnecting,
  disabled: m.settings_account_state_disabled,
  needs_credentials: m.settings_account_state_needs_credentials,
  error: m.settings_account_state_error,
};
export function accountState(p: AccountProfile) {
  const label = Object.prototype.hasOwnProperty.call(
    ACCOUNT_STATE,
    p.runtime.state,
  )
    ? ACCOUNT_STATE[p.runtime.state]
    : undefined;
  return label ? label() : m.settings_account_state_unknown();
}
function ProviderPills({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store);
  return (
    <div
      className="provider-pills"
      role="group"
      aria-label={m.settings_wizard_providers_label()}
    >
      {QUICK_PROVIDERS.map((id) => {
        const preset = providerById(id);
        const active = s.wizard.provider === id;
        return (
          <button
            key={id}
            type="button"
            className={`provider-pill${active ? " active" : ""}`}
            aria-pressed={active}
            onClick={() => controller.chooseProvider(id)}
          >
            {preset?.short ?? m.settings_provider_custom()}
          </button>
        );
      })}
    </div>
  );
}
function ProbeLine({
  side,
  result,
  preset,
}: {
  side: "incoming" | "outgoing";
  result: ProbeStage;
  preset: ProviderPreset | null;
}) {
  useUiLocale();
  const label =
    side === "incoming"
      ? m.settings_probe_side_incoming()
      : m.settings_probe_side_outgoing();
  return (
    <div className={`probe-line${result.ok ? "" : " failed"}`}>
      <span className={`tag${result.ok ? "" : " warn"}`}>
        {result.ok
          ? m.settings_probe_passed({ side: label })
          : m.settings_probe_failed({
              side: label,
              stage: stageLabel(result.stage),
            })}
      </span>
      {!result.ok && (
        <p className="probe-advice">{probeAdvice(result.stage, side, preset)}</p>
      )}
      {!result.ok && result.detail && (
        <code className="probe-detail" aria-label={m.settings_probe_detail_label({ side: label })}>
          {result.detail}
        </code>
      )}
    </div>
  );
}
function Editor({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store),
    e = s.editor!,
    w = s.wizard;
  const [reveal, setReveal] = useState(false);
  const draft = e.draft,
    plugin = draft.incoming.kind === "plugin";
  const preset = plugin ? null : providerById(w.provider);
  const domain = domainOf(draft.address);
  const providers =
    s.packages?.plugins.filter((p) =>
      p.capabilities.includes("transport_provider"),
    ) ?? [];
  const edit = (patch: Partial<typeof draft>) =>
    controller.editAccount({ ...e, draft: { ...draft, ...patch } });
  const endpoint = (side: "incoming" | "outgoing", patch: Partial<Endpoint>) =>
    controller.editEndpoint(side, patch);
  const saved = s.profiles?.find((p) => p.id === e.id);
  const end = () => {
    if (controller.requestLeave(() => controller.store.patch({ editor: null })))
      controller.store.patch({ editor: null });
  };
  const collapsible = !plugin && !!preset;
  const open = !collapsible || w.expanded;
  const summary = `${draft.incoming.server || "?"}:${draft.incoming.port ?? "?"}`;
  const recognized = m
    .settings_wizard_recognized({ provider: "\u0000" })
    .split("\u0000");
  const secretLabel = preset?.secretLabel ?? m.settings_secret_label_generic();
  function SecretInput({
    role,
    label,
    hint,
  }: {
    role: "incoming" | "outgoing";
    label: string;
    hint?: string;
  }) {
    return (
      <Field label={label} hint={hint}>
        <span className="secret-input">
          <input
            type={reveal ? "text" : "password"}
            value={e.secrets[role] ?? ""}
            maxLength={8192}
            autoComplete="new-password"
            autoCapitalize="none"
            spellCheck={false}
            onChange={(v) => controller.setSecret(role, v.target.value)}
          />
          <button
            type="button"
            className="text-button"
            aria-pressed={reveal}
            aria-label={
              reveal
                ? m.settings_secret_hide_label({ label })
                : m.settings_secret_show_label({ label })
            }
            onClick={() => setReveal(!reveal)}
          >
            {reveal ? m.settings_secret_hide() : m.settings_secret_show()}
          </button>
        </span>
      </Field>
    );
  }
  function Server({ side }: { side: "incoming" | "outgoing" }) {
    const ep = draft[side]!;
    return (
      <div className="settings-fields">
        <Field
          label={
            side === "incoming"
              ? m.settings_server_imap_host()
              : m.settings_server_smtp_host()
          }
        >
          <input
            value={ep.server ?? ""}
            required
            maxLength={255}
            onChange={(v) => endpoint(side, { server: v.target.value.trim() })}
            autoCapitalize="none"
            spellCheck={false}
          />
        </Field>
        <div className="settings-field-pair">
          <Field label={m.settings_server_port()}>
            <input
              type="number"
              min={1}
              max={65535}
              required
              value={ep.port ?? ""}
              onChange={(v) => endpoint(side, { port: Number(v.target.value) })}
            />
          </Field>
          <Field
            label={m.settings_server_security()}
            hint={m.settings_server_security_hint()}
          >
            <Choice disabled={s.busy || !s.online}
              value={ep.security ?? "implicit"}
              onValueChange={(value) => endpoint(side, { security: value })}
            >
              <option value="implicit">
                {m.settings_server_security_implicit()}
              </option>
              <option value="starttls">
                {m.settings_server_security_starttls()}
              </option>
            </Choice>
          </Field>
        </div>
        <Field
          label={
            side === "incoming"
              ? m.settings_server_incoming_username()
              : m.settings_server_outgoing_username()
          }
        >
          <input
            value={ep.username ?? ""}
            required
            maxLength={320}
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            onChange={(v) => endpoint(side, { username: v.target.value })}
          />
        </Field>
        {side === "incoming" ? (
          <>
            <Field label={m.settings_server_mailbox()}>
              <input
                value={ep.mailbox ?? "INBOX"}
                required
                maxLength={320}
                onChange={(v) => endpoint(side, { mailbox: v.target.value })}
              />
            </Field>
            <label className="settings-check">
              <input
                type="checkbox"
                checked={ep.idle ?? true}
                onChange={(v) => endpoint(side, { idle: v.target.checked })}
              />
              {m.settings_server_idle()}
            </label>
          </>
        ) : (
          <>
            <Field label={m.settings_outgoing_credential()}>
              <Choice disabled={s.busy || !s.online}
                value={ep.credential ?? "incoming"}
                onValueChange={(value) => endpoint(side, { credential: value })}
              >
                <option value="incoming" disabled={plugin}>
                  {m.settings_outgoing_credential_shared({
                    credential: preset
                      ? CREDENTIAL_LABEL[preset.credential]
                      : m.settings_credential_generic(),
                  })}
                </option>
                <option value="separate">
                  {m.settings_outgoing_credential_separate()}
                </option>
              </Choice>
            </Field>
            {ep.credential === "separate" &&
              SecretInput({
                role: "outgoing",
                label: m.settings_outgoing_secret(),
                hint: m.settings_outgoing_secret_hint(),
              })}
          </>
        )}
      </div>
    );
  }
  // Render helper calls keep input component identity stable during typing.
  return (
    <form
      className="account-editor"
      onSubmit={(v) => {
        v.preventDefault();
        void controller.saveAccount();
      }}
    >
      <div className="settings-editor-heading">
        <div>
          <div className="section-label">
            {e.id
              ? m.settings_account_editor_manage()
              : m.settings_account_editor_connect()}
          </div>
          <h2>{e.id ? draft.display_name : m.settings_account_add()}</h2>
        </div>
        <button
          type="button"
          className="text-button"
          disabled={s.busy}
          onClick={end}
        >
          {m.settings_account_editor_back()}
        </button>
      </div>
      <p className="page-intro">
        {e.id
          ? m.settings_account_editor_intro_edit()
          : m.settings_account_editor_intro_new()}
      </p>
      <fieldset disabled={s.busy || !s.online}>
        {!e.id && (
          <Field label={m.settings_account_kind()}>
            <Choice disabled={s.busy || !s.online}
              value={plugin ? "plugin" : "imap"}
              onValueChange={(value) =>
                controller.editAccount({
                  ...e,
                  draft: {
                    ...draft,
                    incoming:
                      value === "plugin"
                        ? {
                            kind: "plugin",
                            provider: providers[0]?.id ?? "",
                            poll_secs: 60,
                          }
                        : newDraft().incoming,
                    outgoing:
                      value === "plugin"
                        ? { kind: "plugin", provider: providers[0]?.id ?? "" }
                        : newDraft().outgoing,
                  },
                  secrets: {},
                })
              }
            >
              <option value="imap">IMAP / SMTP</option>
              <option value="plugin" disabled={!providers.length}>
                {m.settings_account_kind_plugin()}
              </option>
            </Choice>
          </Field>
        )}
        {!e.id && !plugin && <ProviderPills controller={controller} />}
        <div className="settings-field-pair">
          <Field label={m.settings_account_address()}>
            <input
              type="email"
              value={draft.address}
              required
              maxLength={320}
              autoCapitalize="none"
              autoComplete="email"
              spellCheck={false}
              placeholder={preset ? `name@${preset.domains[0]}` : "name@example.com"}
              onChange={(v) => controller.setAddress(v.target.value)}
            />
          </Field>
          <Field label={m.settings_account_display_name()}>
            <input
              value={draft.display_name}
              required
              maxLength={160}
              onChange={(v) => controller.setDisplayName(v.target.value)}
            />
          </Field>
        </div>
        {!plugin && !e.id && (
          <div className="provider-status" role="status">
            {preset ? (
              <span>
                <span className="status-dot" aria-hidden="true" />
                {recognized[0]}
                <strong>{preset.name}</strong>
                {recognized[1]}
              </span>
            ) : domain ? (
              <>
                <span>
                  {w.discovered ??
                    m.settings_wizard_unknown_domain({ domain })}
                </span>
                {!w.discovered && (
                  <button
                    type="button"
                    className="secondary"
                    disabled={w.discovering}
                    onClick={() => void controller.discover()}
                  >
                    {w.discovering
                      ? m.settings_wizard_detecting()
                      : m.settings_wizard_detect()}
                  </button>
                )}
              </>
            ) : (
              <span>{m.settings_wizard_hint()}</span>
            )}
          </div>
        )}
        {preset?.caveat && (
          <p className="provider-caveat" role="note">
            {preset.caveat}
          </p>
        )}
        {plugin ? (
          <>
            <Field label={m.settings_account_provider_extension()}>
              <Choice disabled={s.busy || !s.online}
                value={draft.incoming.provider ?? ""}
                onValueChange={(value) =>
                  edit({
                    incoming: { ...draft.incoming, provider: value },
                    outgoing: draft.outgoing,
                  })
                }
              >
                {!providers.some((p) => p.id === draft.incoming.provider) && (
                  <option value={draft.incoming.provider ?? ""}>
                    {m.settings_provider_extension_unavailable({
                      name:
                        draft.incoming.provider ||
                        m.settings_provider_extension_none(),
                    })}
                  </option>
                )}
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {packageText(p).name} ·{" "}
                    {p.provider_registered
                      ? m.settings_provider_extension_ready()
                      : m.settings_provider_extension_not_ready()}
                  </option>
                ))}
              </Choice>
            </Field>
            <p className="group-note">
              {m.settings_account_plugin_note()}
            </p>
            {providers.find((p) => p.id === draft.incoming.provider)
              ?.has_ui && (
              <button
                type="button"
                className="secondary"
                onClick={() =>
                  void controller.openProvider(draft.incoming.provider!)
                }
              >
                {m.settings_account_open_provider()}
              </button>
            )}
          </>
        ) : (
          <>
            {SecretInput({
              role: "incoming",
              label: secretLabel,
              hint: saved
                ? m.settings_secret_hint_saved()
                : preset
                  ? m.settings_secret_hint_provider({
                      provider: preset.name,
                      credential: CREDENTIAL_LABEL[preset.credential],
                    })
                  : m.settings_secret_hint_generic(),
            })}
            {w.secretNotice && (
              <p className="secret-notice" role="status">
                {w.secretNotice}
              </p>
            )}
            {preset && (
              <div className="credential-guide">
                <strong>
                  {m.settings_credential_guide_title({
                    credential: CREDENTIAL_LABEL[preset.credential],
                  })}
                </strong>
                <ol>
                  {preset.steps.map((step) => (
                    <li key={step}>{step}</li>
                  ))}
                </ol>
                {preset.helpUrl && (
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => void controller.openHelp(preset.helpUrl!)}
                  >
                    {m.settings_credential_guide_open({ provider: preset.short })}
                  </button>
                )}
              </div>
            )}
          </>
        )}
        {!plugin && collapsible && (
          <button
            type="button"
            className="server-disclosure"
            aria-expanded={open}
            onClick={() => controller.toggleServers()}
          >
            <span aria-hidden="true">{open ? "▾" : "▸"}</span>
            {w.manual
              ? m.settings_servers_disclosure_manual({ summary })
              : m.settings_servers_disclosure_auto({ summary })}
          </button>
        )}
        {open && (
          <div className={collapsible ? "server-panel" : undefined}>
            {!plugin && (
              <>
                <h3>{m.settings_servers_incoming()}</h3>
                {Server({ side: "incoming" })}
              </>
            )}
            <Field label={m.settings_server_poll_secs()}>
              <input
                type="number"
                min={1}
                value={draft.incoming.poll_secs ?? 60}
                onChange={(v) =>
                  endpoint("incoming", { poll_secs: Number(v.target.value) })
                }
              />
            </Field>
            <div className="settings-divider" />
            <label className="settings-check">
              <input
                type="checkbox"
                checked={!!draft.outgoing}
                onChange={(v) =>
                  edit({
                    outgoing: v.target.checked
                      ? plugin
                        ? { kind: "plugin", provider: draft.incoming.provider }
                        : {
                            ...newDraft().outgoing!,
                            ...(preset
                              ? serversFor(preset, domain).outgoing
                              : {}),
                            username: draft.incoming.username,
                          }
                      : null,
                  })
                }
              />
              {m.settings_outgoing_enable()}
            </label>
            {draft.outgoing && (
              <>
                <Field label={m.settings_outgoing_kind()}>
                  <Choice disabled={s.busy || !s.online}
                    value={draft.outgoing.kind}
                    onValueChange={(value) =>
                      edit({
                        outgoing:
                          value === "smtp"
                            ? {
                                ...newDraft().outgoing!,
                                ...(preset
                                  ? serversFor(preset, domain).outgoing
                                  : {}),
                                username: draft.address,
                                credential: plugin ? "separate" : "incoming",
                              }
                            : {
                                kind: "plugin",
                                provider:
                                  draft.incoming.provider ??
                                  providers[0]?.id ??
                                  "",
                              },
                      })
                    }
                  >
                    <option value="smtp">{m.settings_outgoing_kind_smtp()}</option>
                    <option value="plugin" disabled={!providers.length}>
                      {m.settings_outgoing_kind_plugin()}
                    </option>
                  </Choice>
                </Field>
                {draft.outgoing.kind === "smtp" ? (
                  <>
                    <h3>{m.settings_servers_outgoing()}</h3>
                    {Server({ side: "outgoing" })}
                  </>
                ) : (
                  <Field label={m.settings_outgoing_provider_extension()}>
                    <Choice disabled={s.busy || !s.online}
                      value={draft.outgoing.provider ?? ""}
                      onValueChange={(value) =>
                        endpoint("outgoing", { provider: value })
                      }
                    >
                      {!providers.some(
                        (p) => p.id === draft.outgoing?.provider,
                      ) && (
                        <option value={draft.outgoing.provider ?? ""}>
                          {m.settings_provider_extension_unavailable({
                            name:
                              draft.outgoing.provider ||
                              m.settings_provider_extension_none(),
                          })}
                        </option>
                      )}
                      {providers.map((p) => (
                        <option key={p.id} value={p.id}>
                          {packageText(p).name} ·{" "}
                          {p.provider_registered
                            ? m.settings_provider_extension_ready()
                            : m.settings_provider_extension_not_ready()}
                        </option>
                      ))}
                    </Choice>
                  </Field>
                )}
              </>
            )}
          </div>
        )}
        {s.probe && (
          <div className="probe-results" role="status">
            <ProbeLine side="incoming" result={s.probe.incoming} preset={preset} />
            {s.probe.outgoing && (
              <ProbeLine side="outgoing" result={s.probe.outgoing} preset={preset} />
            )}
          </div>
        )}
        <div className="settings-actions">
          <button
            type="button"
            className="secondary"
            onClick={() => void controller.probe()}
          >
            {m.settings_probe_action()}
          </button>
          <button
            type="submit"
            className="primary"
            disabled={
              !e.id &&
              (!s.probe?.incoming.ok ||
                (!!draft.outgoing && !s.probe?.outgoing?.ok))
            }
          >
            {s.busy
              ? m.settings_busy()
              : e.id
                ? m.settings_account_save_changes()
                : m.settings_account_save_connect()}
          </button>
        </div>
        <p className="group-note">
          {m.settings_account_keychain_note()}
        </p>
      </fieldset>
      {e.id && (
        <p className="group-note">
          {m.settings_account_meta({ id: e.id, revision: e.revision ?? "" })} ·{" "}
          {saved?.credentials.incoming === "saved"
            ? m.settings_account_credentials_saved()
            : m.settings_account_credentials_see_list()}
        </p>
      )}
    </form>
  );
}
export function AccountMore({
  profile,
  controller,
}: {
  profile: AccountProfile;
  controller: SettingsController;
}) {
  useUiLocale();
  const s = useStore(controller.store);
  const [open, setOpen] = useState(false);
  const openRef = useRef(open);
  openRef.current = open;
  const confirm = (remove: boolean) => {
    if (!s.online || s.busy || s.closeLocked) return;
    if (openRef.current) setOpen(false);
    controller.confirmAccount(profile, remove);
  };
  return (
    <PopoverMenu
      className="account-more"
      open={open}
      onOpenChange={setOpen}
      forceClosed={s.closeLocked}
      triggerLabel={m.settings_account_more_label({
        name: profile.display_name,
      })}
      triggerTitle={m.settings_account_more_label({
        name: profile.display_name,
      })}
      trigger={m.settings_account_more()}
    >
      <button
        type="button"
        disabled={!s.online || s.busy}
        onClick={() => confirm(false)}
      >
        {profile.enabled
          ? m.settings_account_action_pause()
          : m.settings_account_menu_enable()}
      </button>
      <button
        type="button"
        className="danger"
        disabled={!s.online || s.busy}
        onClick={() => confirm(true)}
      >
        {m.settings_account_action_remove()}
      </button>
    </PopoverMenu>
  );
}
export function Accounts({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store);
  if (s.editor) return <Editor controller={controller} />;
  return (
    <>
      <h2>{m.settings_accounts_title()}</h2>
      <p className="page-intro">{m.settings_accounts_intro()}</p>
      {s.profiles?.map((p) => (
        <article className="account-row" key={p.id}>
          <span className="account-monogram" aria-hidden="true">
            {(p.display_name || p.address).slice(0, 1)}
          </span>
          <div className="account-details">
            <strong>{p.display_name || p.address}</strong>
            <p>{p.address}</p>
            <small>
              <span
                className={`status-dot${p.runtime.state === "online" ? "" : " warn"}`}
              />
              {accountState(p)}
              {p.incoming.provider
                ? ` · ${providerName(s.packages?.plugins, p.incoming.provider)}`
                : ""}
            </small>
            {!p.editable && <small>{m.settings_account_readonly()}</small>}
            {needsRepair(p) && (
              <p className="account-alert" role="status">
                {p.runtime.state === "needs_credentials"
                  ? m.settings_account_alert_credentials()
                  : m.settings_account_alert_error()}
                <AccountReason source={p.runtime} />
              </p>
            )}
          </div>
          {p.editable && (
            <div className="settings-actions">
              <button
                type="button"
                className="secondary"
                disabled={!s.online || s.busy}
                onClick={() => controller.beginAccount(p)}
              >
                {needsRepair(p)
                  ? p.runtime.state === "needs_credentials"
                    ? m.settings_account_update_credentials()
                    : m.settings_account_repair()
                  : m.settings_account_manage()}
              </button>
              <AccountMore profile={p} controller={controller} />
            </div>
          )}
        </article>
      ))}
      {s.profiles?.length === 0 && (
        <div className="settings-empty">
          <h3>{m.settings_accounts_empty_title()}</h3>
          <p>
            {m.settings_accounts_empty_body({ product: BRAND.productName })}
          </p>
          <div className="provider-pills">
            {QUICK_PROVIDERS.map((id) => (
              <button
                key={id}
                type="button"
                className="provider-pill"
                disabled={!s.online || s.busy}
                onClick={() => controller.beginAccount(undefined, id)}
              >
                {providerById(id)?.short ?? m.settings_provider_custom()}
              </button>
            ))}
          </div>
        </div>
      )}
      <div className="settings-actions">
        <button
          type="button"
          className="primary"
          disabled={!s.online || s.busy || s.loading || !s.profiles}
          onClick={() => controller.beginAccount()}
        >
          {m.settings_account_add()}
        </button>
      </div>
      <p className="group-note">
        {m.settings_accounts_note()}
      </p>
    </>
  );
}
