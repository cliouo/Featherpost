import { m } from "@featherpost/i18n/messages";
import { accountLabel } from "./accountLabel";

import { useStore } from "../host/hooks";
import { useUiLocale } from "../i18n";
import { Field } from "./Field";
import type { SettingsController } from "./controller";
import { BRAND } from "../brand";

export function formatUtcOffset(minutes: number): string {
  if (!Number.isFinite(minutes) || minutes === 0) return "UTC+0";
  const sign = minutes > 0 ? "+" : "−";
  const abs = Math.abs(Math.trunc(minutes));
  const hours = Math.floor(abs / 60);
  const rem = abs % 60;
  return rem === 0
    ? `UTC${sign}${hours}`
    : `UTC${sign}${hours}:${String(rem).padStart(2, "0")}`;
}

export function currentSystemOffsetMinutes(now = new Date()): number {
  const raw = now.getTimezoneOffset();
  if (!Number.isFinite(raw)) return NaN;
  return raw === 0 ? 0 : -raw;
}

export function Notifications({
  controller,
}: {
  controller: SettingsController;
}) {
  useUiLocale();
  const s = useStore(controller.store),
    n = s.notification,
    r = s.rules;
  const edit = (patch: Partial<NonNullable<typeof r>>) =>
    r && controller.setRules({ ...r, ...patch });
  // 旧后台不回传新键：如实标为「需要后台升级」，保存时也不发旧后台不认识的字段。
  const baseline = s.rulesBaseline
    ? (JSON.parse(s.rulesBaseline) as Partial<NonNullable<typeof r>>)
    : {};
  const supportsNewMail = baseline.notify_new_mail !== undefined;
  const supportsMute = baseline.muted_accounts !== undefined;
  const supportsFollowSystemTz =
    baseline.follow_system_timezone !== undefined ||
    r?.follow_system_timezone !== undefined;
  const followSystemTz = Boolean(r?.follow_system_timezone);
  const systemOffset = currentSystemOffsetMinutes();
  const showTzMismatch =
    !!r &&
    !followSystemTz &&
    r.quiet_hours.length > 0 &&
    Number.isFinite(systemOffset) &&
    r.timezone_offset_minutes !== systemOffset;
  const muted = r?.muted_accounts ?? [];
  const notifyNewMail = r?.notify_new_mail ?? true;
  return (
    <>
      <h2>{m.settings_notifications_title()}</h2>
      <p className="page-intro">{m.settings_notifications_intro()}</p>
      <div className="setting-row">
        <div className="setting-copy">
          <strong>{m.settings_notifications_system()}</strong>
          <p>
            {n
              ? notificationLabel(n.availability, n.authorization)
              : m.settings_notifications_loading()}
          </p>
        </div>
        <button
          className="secondary"
          type="button"
          disabled={
            !s.online ||
            s.busy ||
            !n ||
            n.availability !== "available" ||
            n.request_in_flight ||
            !["not_determined", "denied"].includes(n.authorization)
          }
          onClick={() => void controller.requestNotifications()}
        >
          {m.settings_notifications_request()}
        </button>
      </div>
      {n?.authorization === "denied" && (
        <p className="group-note">
          {m.settings_notifications_denied_note({ product: BRAND.productName })}
        </p>
      )}
      {r && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void controller.saveRules();
          }}
        >
          <fieldset disabled={!s.online || s.busy}>
            <div className="setting-row">
              <div className="setting-copy">
                <strong>{m.settings_notifications_new_mail()}</strong>
                <p>
                  {supportsNewMail
                    ? m.settings_notifications_new_mail_note()
                    : m.settings_notifications_new_mail_unsupported()}
                </p>
              </div>
              <button
                type="button"
                className="toggle"
                role="switch"
                aria-checked={notifyNewMail}
                aria-label={m.settings_notifications_new_mail()}
                disabled={!supportsNewMail}
                onClick={() => edit({ notify_new_mail: !notifyNewMail })}
              >
                <span className="toggle-track" />
              </button>
            </div>
            <h3>{m.settings_notifications_mute_title()}</h3>
            <p className="group-note">
              {supportsMute
                ? m.settings_notifications_mute_note()
                : m.settings_notifications_mute_unsupported()}
            </p>
            <div
              className="muted-accounts"
              role="group"
              aria-label={m.settings_notifications_mute_title()}
            >
              {s.profiles === null && (
                <small>{m.settings_notifications_mute_profiles_unavailable()}</small>
              )}
              {s.profiles?.length === 0 && (
                <small>{m.settings_notifications_mute_no_accounts()}</small>
              )}
              {s.profiles?.map((p) => (
                <label key={p.id} className="settings-check">
                  <input
                    type="checkbox"
                    disabled={!supportsMute || !notifyNewMail}
                    checked={muted.includes(p.id)}
                    onChange={(e) =>
                      edit({
                        muted_accounts: e.target.checked
                          ? [...muted.filter((id) => id !== p.id), p.id]
                          : muted.filter((id) => id !== p.id),
                      })
                    }
                  />
                  {m.settings_notifications_mute_account({
                    name: accountLabel(p),
                  })}
                  <small>{p.address}</small>
                </label>
              ))}
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <strong>{m.settings_notifications_sensitive()}</strong>
                <p>{m.settings_notifications_sensitive_note()}</p>
              </div>
              <button
                type="button"
                className="toggle"
                role="switch"
                aria-checked={r.show_sensitive}
                aria-label={m.settings_notifications_sensitive()}
                onClick={() => edit({ show_sensitive: !r.show_sensitive })}
              >
                <span className="toggle-track" />
              </button>
            </div>
            <Field
              label={m.settings_notifications_keywords()}
              hint={m.settings_notifications_keywords_hint()}
            >
              <textarea
                rows={4}
                value={r.silent_keywords.join("\n")}
                maxLength={8192}
                onChange={(e) =>
                  edit({ silent_keywords: e.target.value.split("\n") })
                }
              />
            </Field>
            <h3>{m.settings_quiet_hours_title()}</h3>
            <p className="group-note">
              {followSystemTz
                ? m.settings_quiet_hours_follow_note()
                : m.settings_quiet_hours_fixed_note()}
            </p>
            {supportsFollowSystemTz && (
              <div className="setting-row">
                <div className="setting-copy">
                  <strong>{m.settings_quiet_hours_follow()}</strong>
                  <p>{m.settings_quiet_hours_follow_hint()}</p>
                </div>
                <button
                  type="button"
                  className="toggle"
                  role="switch"
                  aria-checked={followSystemTz}
                  aria-label={m.settings_quiet_hours_follow()}
                  onClick={() =>
                    edit({ follow_system_timezone: !followSystemTz })
                  }
                >
                  <span className="toggle-track" />
                </button>
              </div>
            )}
            {showTzMismatch && (
              <div className="provider-caveat" role="status">
                <span>
                  {m.settings_quiet_hours_tz_mismatch({
                    offset: formatUtcOffset(r.timezone_offset_minutes),
                    minutes: r.timezone_offset_minutes,
                    systemOffset: formatUtcOffset(systemOffset),
                    systemMinutes: systemOffset,
                  })}
                </span>{" "}
                {supportsFollowSystemTz ? (
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => edit({ follow_system_timezone: true })}
                  >
                    {m.settings_quiet_hours_switch_follow()}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="text-button"
                    onClick={() =>
                      edit({ timezone_offset_minutes: systemOffset })
                    }
                  >
                    {m.settings_quiet_hours_fill_offset({ minutes: systemOffset })}
                  </button>
                )}
              </div>
            )}
            {r.quiet_hours.map((q, i) => (
              <div key={i} className="quiet-hours">
                <Field label={m.settings_quiet_hours_start({ index: i + 1 })}>
                  <input
                    type="time"
                    required
                    value={q.start === "24:00" ? "00:00" : q.start}
                    onChange={(e) =>
                      edit({
                        quiet_hours: r.quiet_hours.map((x, j) =>
                          i === j ? { ...x, start: e.target.value } : x,
                        ),
                      })
                    }
                  />
                </Field>
                <Field label={m.settings_quiet_hours_end({ index: i + 1 })}>
                  <input
                    type="time"
                    required
                    value={q.end === "24:00" ? "00:00" : q.end}
                    onChange={(e) =>
                      edit({
                        quiet_hours: r.quiet_hours.map((x, j) =>
                          i === j ? { ...x, end: e.target.value } : x,
                        ),
                      })
                    }
                  />
                </Field>
                <button
                  type="button"
                  className="text-button danger"
                  aria-label={m.settings_quiet_hours_remove_label({ index: i + 1 })}
                  onClick={() =>
                    edit({
                      quiet_hours: r.quiet_hours.filter((_, j) => j !== i),
                    })
                  }
                >
                  {m.settings_quiet_hours_remove()}
                </button>
              </div>
            ))}
            <button
              type="button"
              className="secondary"
              disabled={r.quiet_hours.length >= 32}
              onClick={() => {
                const autoFollow =
                  r.quiet_hours.length === 0 &&
                  supportsFollowSystemTz &&
                  !followSystemTz &&
                  r.timezone_offset_minutes === 0;
                edit({
                  quiet_hours: [
                    ...r.quiet_hours,
                    { start: "22:00", end: "08:00" },
                  ],
                  ...(autoFollow ? { follow_system_timezone: true } : {}),
                });
              }}
            >
              {m.settings_quiet_hours_add()}
            </button>
            {!followSystemTz && (
              <Field
                label={m.settings_quiet_hours_offset()}
                hint={m.settings_quiet_hours_offset_hint()}
              >
                <input
                  type="number"
                  min={-1440}
                  max={1440}
                  step={1}
                  required
                  value={r.timezone_offset_minutes}
                  onChange={(e) =>
                    edit({ timezone_offset_minutes: Number(e.target.value) })
                  }
                />
              </Field>
            )}
            <div className="settings-actions">
              <button
                type="submit"
                className="primary"
                disabled={!controller.dirty}
              >
                {m.settings_rules_apply()}
              </button>
              <button
                type="button"
                className="text-button"
                disabled={!controller.dirty}
                onClick={() => controller.discard()}
              >
                {m.settings_rules_revert()}
              </button>
            </div>
          </fieldset>
        </form>
      )}
      <p className="group-note" role="status">
        {s.rulesPersisted === true
          ? m.settings_rules_persisted()
          : s.rulesPersisted === false
            ? m.settings_rules_session_only()
            : m.settings_rules_persistence_unknown()}
      </p>
    </>
  );
}
const AUTHORIZATION_LABEL: Readonly<Record<string, () => string>> = {
  checking: m.settings_notifications_auth_checking,
  not_determined: m.settings_notifications_auth_not_determined,
  denied: m.settings_notifications_auth_denied,
  authorized: m.settings_notifications_auth_authorized,
  provisional: m.settings_notifications_auth_provisional,
  ephemeral: m.settings_notifications_auth_provisional,
  not_applicable: m.settings_notifications_auth_not_applicable,
};
function notificationLabel(availability: string, authorization: string) {
  if (availability !== "available")
    return availability === "identity_mismatch"
      ? m.settings_notifications_identity_mismatch()
      : m.settings_notifications_unavailable();
  const label = Object.prototype.hasOwnProperty.call(
    AUTHORIZATION_LABEL,
    authorization,
  )
    ? AUTHORIZATION_LABEL[authorization]
    : undefined;
  return label ? label() : m.settings_notifications_auth_unknown();
}
