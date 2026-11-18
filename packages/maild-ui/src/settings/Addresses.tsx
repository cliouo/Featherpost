import { useStore } from "../host/hooks";
import { expiryLabel } from "../mailbox/format";
import { Field } from "./Accounts";
import type { SettingsController } from "./controller";
export function Addresses({ controller }: { controller: SettingsController }) {
  const s = useStore(controller.store),
    f = s.addressForm,
    p = s.profiles?.find((p) => p.id === s.addressAccount),
    page = s.addresses;
  return (
    <>
      <h2>地址与别名</h2>
      <p className="page-intro">管理这个邮箱可使用的本地发件身份。</p>
      <Field label="邮箱账户">
        <select
          disabled={s.busy || s.loading || !s.online}
          value={s.addressAccount}
          onChange={(e) => {
            const account = e.target.value;
            const next = () => void controller.selectAddressAccount(account);
            if (controller.requestLeave(next)) next();
          }}
        >
          <option value="" disabled>
            选择账户
          </option>
          {s.profiles?.map((p) => (
            <option key={p.id} value={p.id}>
              {p.display_name} · {p.address}
            </option>
          ))}
        </select>
      </Field>
      {p && (
        <div className="setting-row">
          <div className="setting-copy">
            <strong>{p.address}</strong>
            <p>账户主地址</p>
          </div>
          <span className="tag">主地址</span>
        </div>
      )}
      {page?.addresses.map((a) => {
        const expiry = expiryLabel(a.expires_at);
        return (
        <div className="setting-row address-row" key={a.id}>
          <div className="setting-copy">
            <strong>{a.address}</strong>
            <p>
              {a.label || "无备注"} ·{" "}
              {a.kind === "disposable" ? "一次性身份" : "别名"}
              {expiry ? ` · ${expiry}` : ""}
            </p>
          </div>
          <span className={`tag${a.state === "active" ? "" : " warn"}`}>
            {(
              {
                active: "已启用",
                disabled: "已停用",
                expired: "已过期",
              } as Record<string, string>
            )[a.state] ?? "未知状态"}
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
              {a.state === "active" ? "停用" : "启用"}
            </button>
            <button
              type="button"
              className="text-button danger"
              disabled={!s.online || s.busy}
              onClick={() => controller.confirmAddress(a, true)}
            >
              移除
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
            上一页
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
            下一页
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
            <h3>登记发件身份</h3>
            <div className="settings-field-pair">
              <Field label="类型">
                <select
                  value={f.kind}
                  onChange={(e) =>
                    controller.store.patch({
                      addressForm: {
                        ...f,
                        kind: e.target.value as typeof f.kind,
                      },
                    })
                  }
                >
                  <option value="alias">别名</option>
                  <option value="disposable">一次性身份</option>
                </select>
              </Field>
              <Field
                label="邮箱地址"
                hint={
                  f.kind === "disposable"
                    ? "可留空，由后台生成同域地址；需服务商支持接收。"
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
            <Field label="备注">
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
                label="有效期（天）"
                hint="留空表示永久；过期只停用本地身份。"
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
              登记不会创建远程别名。请先确认服务商允许接收和使用该地址发送邮件。
            </p>
            <div className="settings-actions">
              <button className="primary" type="submit">
                确认服务商已支持，登记
              </button>
              <button
                className="text-button"
                type="button"
                onClick={() => controller.store.patch({ addressForm: null })}
              >
                取消
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
            登记地址
          </button>
        </div>
      )}
      {p?.outgoing?.kind !== "smtp" && (
        <p className="group-note">
          此账户当前仅支持主地址，不能把本地别名作为可发地址。
        </p>
      )}
    </>
  );
}
