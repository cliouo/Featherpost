import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "@featherpost/i18n/react";
import { packageText, providerName } from "../i18n";
import { AccountReason } from "./AccountReason";
import { Choice } from "./Choice";
import { useState, type ReactNode } from "react";
import {
  StudioDropdownMenu,
  StudioDropdownMenuItem,
  StudioDropdownMenuMeta,
  StudioDropdownMenuSeparator,
} from "../app/DropdownMenu";
import { useStore } from "../host/hooks";
import type { AccountProfile, Endpoint, ProbeStage } from "./types";
import { SettingsController, newDraft } from "./controller";
import { accountLabel } from "./accountLabel";
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
import { Icon } from "../app/Icon";
import { BRAND } from "../brand";
import { Field } from "./Field";

export { Field };
export function needsRepair(p: AccountProfile) {
  return p.enabled && (p.runtime.state === "needs_credentials" || p.runtime.state === "error");
}
const accountStates: Record<string, () => string> = {
  online: m.studio_account_state_online,
  connecting: m.studio_account_state_connecting,
  reconnecting: m.studio_account_state_reconnecting,
  disabled: m.studio_account_state_disabled,
  needs_credentials: m.studio_account_state_needs_credentials,
  error: m.studio_account_state_error,
};
export function accountState(p: AccountProfile) {
  const label = Object.prototype.hasOwnProperty.call(
    accountStates,
    p.runtime.state,
  )
    ? accountStates[p.runtime.state]
    : undefined;
  return label ? label() : m.studio_account_state_unknown();
}
/** 把带 `{占位}` 的整句消息拆成前后两段，中间的值以 <strong> 渲染；词序由各语言的消息决定。 */
const MARK = "\u0000";
function emphasize(text: (mark: string) => string, value: ReactNode) {
  const [before, after = ""] = text(MARK).split(MARK);
  return (
    <>
      {before}
      <strong>{value}</strong>
      {after}
    </>
  );
}
function ProviderPills({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store);
  return (
    <div
      className="provider-pills"
      role="group"
      aria-label={m.studio_account_providers_label()}
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
            {preset?.short ?? m.studio_account_custom_imap()}
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
    side === "incoming" ? m.studio_account_incoming() : m.studio_account_outgoing();
  return (
    <div className={`probe-line${result.ok ? "" : " failed"}`}>
      <span className={`tag${result.ok ? "" : " warn"}`}>
        {result.ok
          ? m.studio_account_probe_passed({ side: label })
          : m.studio_account_probe_failed({
              side: label,
              stage: stageLabel(result.stage),
            })}
      </span>
      {!result.ok && (
        <p className="probe-advice">{probeAdvice(result.stage, side, preset)}</p>
      )}
      {!result.ok && result.detail && (
        <code className="probe-detail" aria-label={m.studio_account_probe_detail({ side: label })}>
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
  const secretLabel = preset?.secretLabel ?? m.studio_account_secret_default();
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
      <div className="studio-secret-row"><Field label={label} hint={hint}>
        <span className="secret-input">
          <input
            type={reveal ? "text" : "password"}
            value={e.secrets[role] ?? ""}
            maxLength={8192}
            aria-label={label}
            placeholder={saved?.credentials[role] === "saved" ? m.studio_account_secret_keep() : m.studio_account_secret_enter({ label })}
            autoComplete="new-password"
            autoCapitalize="none"
            spellCheck={false}
            onChange={(v) => controller.setSecret(role, v.target.value)}
          />
          <button
            type="button"
            className="text-button"
            aria-pressed={reveal}
            aria-label={reveal ? m.studio_account_secret_hide_label({ label }) : m.studio_account_secret_show_label({ label })}
            onClick={() => setReveal(!reveal)}
          >
            {reveal ? m.studio_account_secret_hide() : m.studio_account_secret_show()}
          </button>
        </span>
      </Field></div>
    );
  }
  function Server({ side }: { side: "incoming" | "outgoing" }) {
    const ep = draft[side]!;
    return (
      <div className="settings-fields">
        <Field label={side === "incoming" ? m.studio_account_server_imap() : m.studio_account_server_smtp()}>
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
          <Field label={m.studio_account_port()}>
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
            label={m.studio_account_security()}
            hint={m.studio_account_security_hint()}
          >
            <Choice disabled={s.busy || !s.online}
              value={ep.security ?? "implicit"}
              onValueChange={(value) => endpoint(side, { security: value })}
            >
              <option value="implicit">{m.studio_account_security_tls()}</option>
              <option value="starttls">{m.studio_account_security_starttls()}</option>
            </Choice>
          </Field>
        </div>
        <Field label={side === "incoming" ? m.studio_account_username_incoming() : m.studio_account_username_outgoing()}>
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
            <Field label={m.studio_account_mailbox()}>
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
              {m.studio_account_idle()}
            </label>
          </>
        ) : (
          <>
            <Field label={m.studio_account_outgoing_auth()}>
              <Choice disabled={s.busy || !s.online}
                value={ep.credential ?? "incoming"}
                onValueChange={(value) => endpoint(side, { credential: value })}
              >
                <option value="incoming" disabled={plugin}>
                  {m.studio_account_outgoing_shared({
                    credential: preset
                      ? CREDENTIAL_LABEL[preset.credential]
                      : m.studio_account_credential_password(),
                  })}
                </option>
                <option value="separate">{m.studio_account_outgoing_separate()}</option>
              </Choice>
            </Field>
            {ep.credential === "separate" &&
              SecretInput({
                role: "outgoing",
                label: m.studio_account_outgoing_password(),
                hint: m.studio_account_outgoing_password_hint(),
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
      {!e.id && <div className="settings-editor-heading"><div><div className="section-label">{m.studio_account_new_eyebrow()}</div><h3>{m.studio_account_add_mailbox()}</h3><p className="page-intro">{m.studio_account_new_intro()}</p></div></div>}
      {e.id && <div className="studio-editor-label">{m.studio_account_profile()}</div>}
      <fieldset disabled={s.busy || !s.online}>
        {!e.id && (
          <Field label={m.studio_account_access()}>
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
                {m.studio_account_access_plugin()}
              </option>
            </Choice>
          </Field>
        )}
        {!e.id && !plugin && <ProviderPills controller={controller} />}
        <div className="settings-field-pair studio-account-identity">
          <Field label={m.studio_account_display_name()}><input value={draft.display_name} required maxLength={160} onChange={v => controller.setDisplayName(v.target.value)} /></Field>
          <Field label={m.studio_account_address()}><input type="email" value={draft.address} required maxLength={320} autoCapitalize="none" autoComplete="email" spellCheck={false}
            placeholder={preset ? `name@${preset.domains[0]}` : "name@example.com"} onChange={v => controller.setAddress(v.target.value)} /></Field>
        </div>
        {!plugin && !e.id && (
          <div className="provider-status" role="status">
            {preset ? (
              <span>
                <span className="status-dot" aria-hidden="true" />
                {emphasize(
                  (provider) => m.studio_account_provider_detected({ provider }),
                  preset.name,
                )}
              </span>
            ) : domain ? (
              <>
                <span>
                  {w.discovered ??
                    m.studio_account_domain_unknown({ domain })}
                </span>
                {!w.discovered && (
                  <button
                    type="button"
                    className="secondary"
                    disabled={w.discovering}
                    onClick={() => void controller.discover()}
                  >
                    {w.discovering
                      ? m.studio_account_discovering()
                      : m.studio_account_discover()}
                  </button>
                )}
              </>
            ) : (
              <span>{m.studio_account_detect_hint()}</span>
            )}
          </div>
        )}
        {plugin ? (
          <>
            <Field label={m.studio_account_plugin()}>
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
                    {m.studio_account_plugin_unavailable({
                      name:
                        draft.incoming.provider ||
                        m.studio_account_plugin_none(),
                    })}
                  </option>
                )}
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {packageText(p).name} ·{" "}
                    {p.provider_registered
                      ? m.studio_account_plugin_ready()
                      : m.studio_account_plugin_not_ready()}
                  </option>
                ))}
              </Choice>
            </Field>
            <p className="group-note">
              {m.studio_account_plugin_note()}
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
                {m.studio_account_plugin_open()}
              </button>
            )}
          </>
        ) : (
          <>
            {SecretInput({
              role: "incoming",
              label: secretLabel,
              hint: saved
                ? m.studio_account_secret_saved_hint()
                : preset
                  ? m.studio_account_secret_preset_hint({
                      provider: preset.name,
                      credential: CREDENTIAL_LABEL[preset.credential],
                    })
                  : m.studio_account_secret_generic_hint(),
            })}
            {w.secretNotice && (
              <p className="secret-notice" role="status">
                {w.secretNotice}
              </p>
            )}
            {preset && (
              <details className="credential-guide" open={!e.id}>
                <summary>
                  {m.studio_account_credential_guide({
                    credential: CREDENTIAL_LABEL[preset.credential],
                  })}
                </summary>
                {preset.caveat && <p className="provider-caveat" role="note">{preset.caveat}</p>}
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
                    {m.studio_account_open_help({ provider: preset.short })}
                  </button>
                )}
              </details>
            )}
          </>
        )}
        <div className="studio-connection-groups">
          {!plugin ? <details className="studio-connection" open={!e.id}>
            <summary><span>{m.studio_account_incoming()}</span><strong>{draft.incoming.server || m.studio_account_incoming_configure()}{draft.incoming.port ? ` : ${draft.incoming.port}` : ""}</strong><small>IMAP · {draft.incoming.security === "starttls" ? "STARTTLS" : "TLS"}</small><Icon name="back" /></summary>
            <div className="studio-connection-fields">{Server({ side: "incoming" })}{<Field label={m.studio_account_poll()}><input type="number" min={1} value={draft.incoming.poll_secs ?? 60} onChange={v => endpoint("incoming", { poll_secs: Number(v.target.value) })} /></Field>}</div>
          </details> : <Field label={m.studio_account_poll()}><input type="number" min={1} value={draft.incoming.poll_secs ?? 60} onChange={v => endpoint("incoming", { poll_secs: Number(v.target.value) })} /></Field>}
          <details className="studio-connection" open={!e.id}>
            <summary><span>{m.studio_account_outgoing()}</span><strong>{!draft.outgoing ? m.studio_account_outgoing_disabled() : draft.outgoing.kind === "plugin" ? (draft.outgoing.provider || m.studio_account_plugin()) : `${draft.outgoing.server || m.studio_account_outgoing_configure()} : ${draft.outgoing.port ?? ""}`}</strong><small>{draft.outgoing?.kind === "smtp" ? `SMTP · ${draft.outgoing.security === "starttls" ? "STARTTLS" : "TLS"}` : m.studio_account_service_extension()}</small><Icon name="back" /></summary>
            <div className="studio-connection-fields">
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
              {m.studio_account_outgoing_enable()}
            </label>
            {draft.outgoing && (
              <>
                <Field label={m.studio_account_outgoing_method()}>
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
                    <option value="smtp">{m.studio_account_outgoing_smtp()}</option>
                    <option value="plugin" disabled={!providers.length}>
                      {m.studio_account_plugin()}
                    </option>
                  </Choice>
                </Field>
                {draft.outgoing.kind === "smtp" ? (
                  <>
                    <h3>{m.studio_account_outgoing()}</h3>
                    {Server({ side: "outgoing" })}
                  </>
                ) : (
                  <Field label={m.studio_account_outgoing_plugin()}>
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
                          {m.studio_account_plugin_unavailable({
                            name:
                              draft.outgoing.provider ||
                              m.studio_account_plugin_none(),
                          })}
                        </option>
                      )}
                      {providers.map((p) => (
                        <option key={p.id} value={p.id}>
                          {packageText(p).name} ·{" "}
                          {p.provider_registered
                            ? m.studio_account_plugin_ready()
                            : m.studio_account_plugin_not_ready()}
                        </option>
                      ))}
                    </Choice>
                  </Field>
                )}
              </>
            )}
            </div>
          </details>
        </div>
        {s.probe && (
          <div className="probe-results" role="status">
            <ProbeLine side="incoming" result={s.probe.incoming} preset={preset} />
            {s.probe.outgoing && (
              <ProbeLine side="outgoing" result={s.probe.outgoing} preset={preset} />
            )}
          </div>
        )}
      </fieldset>
        <div className="studio-editor-options"><span>{m.studio_account_probe_needs_password()}</span>{saved && <AccountMore profile={saved} controller={controller} disabled={controller.dirty} />}</div>
        <div className="settings-actions studio-account-save">
          <span>{m.studio_account_save_note()}</span>
          <button type="button" className="secondary" disabled={s.busy || s.closeLocked} onClick={end}>{m.studio_account_cancel()}</button>
          <button
            type="button"
            className="secondary"
            disabled={!s.online || s.busy || s.closeLocked}
            onClick={() => void controller.probe()}
          >
            {m.studio_account_probe()}
          </button>
          <button
            type="submit"
            className="primary"
            disabled={
              !s.online || s.busy || s.closeLocked || (!e.id &&
              (!s.probe?.incoming.ok ||
                (!!draft.outgoing && !s.probe?.outgoing?.ok)))
            }
          >
            {s.busy
              ? m.studio_account_busy()
              : e.id
                ? m.studio_account_save()
                : m.studio_account_save_connect()}
          </button>
        </div>
        {!e.id && <p className="group-note">
          {m.studio_account_keychain_note()}
        </p>}
    </form>
  );
}

export function AccountMore({ profile, controller, disabled = false }: { profile: AccountProfile; controller: SettingsController; disabled?: boolean }) {
  useUiLocale();
  const s = useStore(controller.store);
  const itemDisabled = disabled || !s.online || s.busy || s.closeLocked;
  const confirm = (remove: boolean) => {
    if (disabled || controller.dirty || !s.online || s.busy || s.closeLocked) return;
    // Account actions refresh profile revisions. Never retain an obsolete editor.
    controller.discard();
    controller.confirmAccount(profile, remove);
  };
  const label = m.studio_account_more_label({ account: accountLabel(profile) });
  return (
    <StudioDropdownMenu
      className="account-more"
      contentClassName="account-more-panel"
      align="end"
      forceClosed={s.closeLocked}
      trigger={
        <button type="button" aria-label={label} title={label}>
          {m.studio_account_more()} <Icon name="more" />
        </button>
      }
    >
      <StudioDropdownMenuItem disabled={itemDisabled} onSelect={() => confirm(false)}>
        {profile.enabled ? m.studio_account_pause() : m.studio_account_enable()}
      </StudioDropdownMenuItem>
      <StudioDropdownMenuItem tone="danger" disabled={itemDisabled} onSelect={() => confirm(true)}>
        {m.studio_account_remove()}
      </StudioDropdownMenuItem>
      <StudioDropdownMenuSeparator />
      <StudioDropdownMenuMeta>
        {disabled && <small>{m.studio_account_save_first()}</small>}
        <small>{m.studio_account_id_revision({ id: profile.id, revision: profile.revision })}</small>
        <small>{profile.credentials.incoming === "saved" ? m.studio_account_credentials_saved() : m.studio_account_credentials_missing()}</small>
      </StudioDropdownMenuMeta>
    </StudioDropdownMenu>
  );
}

export function Accounts({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store);
  const change = (next: () => void) => { if (controller.requestLeave(next)) next(); };
  return <>
    <section className="studio-accounts">
      <div className="studio-account-list-heading"><span>{m.studio_accounts_connected()}</span>
        <button type="button" className="text-button" disabled={!s.online || s.busy || s.loading || s.closeLocked || controller.dirty} onClick={() => void controller.load()}><Icon name="refresh" />{m.studio_accounts_refresh()}</button>
      </div>
      {s.editor && (!s.editor.id || !s.profiles?.some(p => p.id === s.editor?.id)) && <Editor key={s.editor.id ?? "new"} controller={controller} />}
      {s.profiles?.map(p => {
        const expanded = s.editor?.id === p.id;
        return <article className={`studio-account${expanded ? " expanded" : ""}`} key={p.id}>
          <div className="account-row">
            <span className="account-monogram" aria-hidden="true">{accountLabel(p).slice(0,1).toUpperCase()}</span>
            <div className="account-details">
              <strong>{accountLabel(p)}</strong>{accountLabel(p) !== p.address.trim() && <p>{p.address}</p>}
              {p.incoming.provider && <small>{providerName(s.packages?.plugins, p.incoming.provider)}</small>}
              {!p.editable && <small>{m.studio_account_readonly()}</small>}
              {needsRepair(p) && <p className="account-alert" role="status">
                {p.runtime.state === "needs_credentials" ? m.studio_account_alert_credentials() : m.studio_account_alert_error()}
                <AccountReason source={p.runtime} />
              </p>}
            </div>
            <span className="studio-account-state"><i className={`status-dot${p.runtime.state === "online" ? "" : " warn"}`} />{accountState(p)}</span>
            {p.editable && <div className="settings-actions">
              <button type="button" className="text-button studio-account-manage" aria-expanded={expanded} disabled={!s.online || s.busy || s.closeLocked}
                onClick={() => change(() => expanded ? controller.store.patch({ editor: null }) : controller.beginAccount(p))}>
                {expanded ? m.studio_account_collapse() : needsRepair(p) ? (p.runtime.state === "needs_credentials" ? m.studio_account_update_credentials() : m.studio_account_repair()) : m.studio_account_manage()}<Icon name="back" />
              </button>
              {!expanded && <AccountMore profile={p} controller={controller} disabled={controller.dirty} />}
            </div>}
          </div>
          {expanded && <Editor key={p.id} controller={controller} />}
        </article>;
      })}
      {s.profiles?.length === 0 && !s.editor && <div className="settings-empty">
        <h3>{m.studio_accounts_empty_title()}</h3><p>{m.studio_accounts_empty_body({ product: BRAND.productName })}</p>
        <div className="provider-pills">{QUICK_PROVIDERS.map(id => <button key={id} type="button" className="provider-pill" disabled={!s.online || s.busy || s.closeLocked}
          onClick={() => controller.beginAccount(undefined, id)}>{providerById(id)?.short ?? m.studio_account_custom_imap()}</button>)}</div>
      </div>}
    </section>
    <p className="studio-accounts-note">{m.studio_accounts_note()}</p>
  </>;
}
