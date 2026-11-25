import { m } from "@featherpost/i18n/messages";
import { Choice } from "./Choice";
import { useStore } from "../host/hooks";
import { useUiLocale } from "../i18n";
import { expiryLabel } from "../mailbox/format";
import { Field } from "./Field";
import type { SettingsController } from "./controller";

const ADDRESS_STATE: Readonly<Record<string, () => string>> = {
  active: m.settings_address_state_active,
  disabled: m.settings_address_state_disabled,
  expired: m.settings_address_state_expired,
};
function addressState(state: string): string {
  const label = Object.prototype.hasOwnProperty.call(ADDRESS_STATE, state)
    ? ADDRESS_STATE[state]
    : undefined;
  return label ? label() : m.settings_address_state_unknown();
}

export function Addresses({ controller }: { controller: SettingsController }) {
  useUiLocale();
  const s = useStore(controller.store),
    f = s.addressForm,
    p = s.profiles?.find((p) => p.id === s.addressAccount),
    page = s.addresses;
  return (
    <>
      <h2>{m.settings_addresses_title()}</h2>
      <p className="page-intro">{m.settings_addresses_intro()}</p>
      <Field label={m.settings_addresses_account()}>
        <Choice
          disabled={s.busy || s.loading || !s.online}
          value={s.addressAccount}
          onValueChange={(value) => {
            const account = value;
            const next = () => void controller.selectAddressAccount(account);
            if (controller.requestLeave(next)) next();
          }}
        >
          <option value="" disabled>
            {m.settings_addresses_choose_account()}
          </option>
          {s.profiles?.map((p) => (
            <option key={p.id} value={p.id}>
              {p.display_name} · {p.address}
            </option>
          ))}
        </Choice>
      </Field>
      {p && (
        <div className="setting-row">
          <div className="setting-copy">
            <strong>{p.address}</strong>
            <p>{m.settings_address_primary_note()}</p>
          </div>
          <span className="tag">{m.settings_address_primary()}</span>
        </div>
      )}
      {page?.addresses.map((a) => {
        const expiry = expiryLabel(a.expires_at);
        return (
        <div className="setting-row address-row" key={a.id}>
          <div className="setting-copy">
            <strong>{a.address}</strong>
            <p>
              {a.label || m.settings_address_no_label()} ·{" "}
              {a.kind === "disposable"
                ? m.settings_address_kind_disposable()
                : m.settings_address_kind_alias()}
              {expiry ? ` · ${expiry}` : ""}
            </p>
          </div>
          <span className={`tag${a.state === "active" ? "" : " warn"}`}>
            {addressState(a.state)}
          </span>
          <div className="settings-actions">
            <button
              type="button"
              className="text-button"
              disabled={
                !s.online ||
                s.busy ||
                a.state === "expired" ||
                !["active", "disabled"].includes(a.state)
              }
              onClick={() => controller.confirmAddress(a, false)}
            >
              {a.state === "active"
                ? m.settings_address_action_disable()
                : m.settings_address_action_enable()}
            </button>
            <button
              type="button"
              className="text-button danger"
              disabled={!s.online || s.busy}
              onClick={() => controller.confirmAddress(a, true)}
            >
              {m.settings_address_action_remove()}
            </button>
          </div>
        </div>
        );
      })}
      {page && page.total > 100 && (
        <div className="settings-actions">
          <button
            className="secondary"
            disabled={s.loading || s.busy || s.addressOffset === 0 || !!f}
            onClick={() =>
              void controller.selectAddressAccount(
                s.addressAccount,
                s.addressOffset - 100,
              )
            }
          >
            {m.settings_addresses_previous()}
          </button>
          <span>
            {s.addressOffset + 1}–{s.addressOffset + page.addresses.length} /{" "}
            {page.total}
          </span>
          <button
            className="secondary"
            disabled={
              s.loading ||
              s.busy ||
              s.addressOffset + page.addresses.length >= page.total ||
              !!f
            }
            onClick={() =>
              void controller.selectAddressAccount(
                s.addressAccount,
                s.addressOffset + 100,
              )
            }
          >
            {m.settings_addresses_next()}
          </button>
        </div>
      )}
      {f ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void controller.createAddress();
          }}
        >
          <fieldset disabled={!s.online || s.busy}>
            <h3>{m.settings_address_form_title()}</h3>
            <div className="settings-field-pair">
              <Field label={m.settings_address_form_kind()}>
                <Choice disabled={s.busy || !s.online}
                  value={f.kind}
                  onValueChange={(value) =>
                    controller.store.patch({
                      addressForm: {
                        ...f,
                        kind: value as typeof f.kind,
                      },
                    })
                  }
                >
                  <option value="alias">
                    {m.settings_address_kind_alias()}
                  </option>
                  <option value="disposable">
                    {m.settings_address_kind_disposable()}
                  </option>
                </Choice>
              </Field>
              <Field
                label={m.settings_address_form_address()}
                hint={
                  f.kind === "disposable"
                    ? m.settings_address_form_address_hint()
                    : undefined
                }
              >
                <input
                  type="email"
                  value={f.address}
                  required={f.kind === "alias"}
                  maxLength={320}
                  onChange={(e) =>
                    controller.store.patch({
                      addressForm: { ...f, address: e.target.value },
                    })
                  }
                />
              </Field>
            </div>
            <Field label={m.settings_address_form_label()}>
              <input
                value={f.label}
                maxLength={160}
                onChange={(e) =>
                  controller.store.patch({
                    addressForm: { ...f, label: e.target.value },
                  })
                }
              />
            </Field>
            {f.kind === "disposable" && (
              <Field
                label={m.settings_address_form_days()}
                hint={m.settings_address_form_days_hint()}
              >
                <input
                  type="number"
                  min={1}
                  max={365}
                  value={f.days}
                  onChange={(e) =>
                    controller.store.patch({
                      addressForm: { ...f, days: e.target.value },
                    })
                  }
                />
              </Field>
            )}
            <p className="group-note">
              {m.settings_address_form_note()}
            </p>
            <div className="settings-actions">
              <button className="primary" type="submit">
                {m.settings_address_form_submit()}
              </button>
              <button
                className="text-button"
                type="button"
                onClick={() => controller.store.patch({ addressForm: null })}
              >
                {m.settings_address_form_cancel()}
              </button>
            </div>
          </fieldset>
        </form>
      ) : (
        <div className="settings-actions">
          <button
            type="button"
            className="secondary"
            disabled={
              !s.online ||
              s.busy ||
              s.loading ||
              !p ||
              p.outgoing?.kind !== "smtp"
            }
            onClick={() =>
              controller.store.patch({
                addressForm: {
                  account: s.addressAccount,
                  address: "",
                  label: "",
                  kind: "alias",
                  days: "",
                },
              })
            }
          >
            {m.settings_address_register()}
          </button>
        </div>
      )}
      {p?.outgoing?.kind !== "smtp" && (
        <p className="group-note">
          {m.settings_addresses_primary_only_note()}
        </p>
      )}
    </>
  );
}
