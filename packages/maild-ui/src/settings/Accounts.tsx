import { useState, type ReactNode } from "react";
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
export function accountState(p: AccountProfile) {
  return (
    (
      {
        online: "已连接",
        connecting: "连接中",
        reconnecting: "正在重连",
        disabled: "已暂停",
        needs_credentials: "需要更新认证信息",
        error: "连接失败",
      } as Record<string, string>
    )[p.runtime.state] ?? "状态未知"
  );
}
function ProviderPills({ controller }: { controller: SettingsController }) {
  const s = useStore(controller.store);
  return (
    <div className="provider-pills" role="group" aria-label="常用邮箱">
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
            {preset?.short ?? "自定义 IMAP"}
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
  const label = side === "incoming" ? "收信" : "发信";
  return (
    <div className={`probe-line${result.ok ? "" : " failed"}`}>
      <span className={`tag${result.ok ? "" : " warn"}`}>
        {label}：{result.ok ? "探测通过" : `未通过 · ${stageLabel(result.stage)}`}
      </span>
      {!result.ok && (
        <p className="probe-advice">{probeAdvice(result.stage, side, preset)}</p>
      )}
      {!result.ok && result.detail && (
        <code className="probe-detail" aria-label={`${label}诊断详情`}>
          {result.detail}
        </code>
      )}
    </div>
  );
}
function Editor({ controller }: { controller: SettingsController }) {
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
  const secretLabel = preset?.secretLabel ?? "密码或应用专用密码";
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
            aria-label={reveal ? `隐藏${label}` : `显示${label}`}
            onClick={() => setReveal(!reveal)}
          >
            {reveal ? "隐藏" : "显示"}
          </button>
        </span>
      </Field>
    );
  }
  function Server({ side }: { side: "incoming" | "outgoing" }) {
    const ep = draft[side]!;
    return (
      <div className="settings-fields">
        <Field label={side === "incoming" ? "IMAP 服务器" : "SMTP 服务器"}>
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
          <Field label="端口">
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
            label="连接安全"
            hint="切换时，标准端口会随之调整。"
          >
            <select
              value={ep.security ?? "implicit"}
              onChange={(v) => endpoint(side, { security: v.target.value })}
            >
              <option value="implicit">直接加密（TLS）</option>
              <option value="starttls">升级加密（STARTTLS）</option>
            </select>
          </Field>
        </div>
        <Field label={side === "incoming" ? "收信用户名" : "发信用户名"}>
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
            <Field label="收信文件夹">
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
              使用 IMAP IDLE 接收新邮件
            </label>
          </>
        ) : (
          <>
            <Field label="发信认证">
              <select
                value={ep.credential ?? "incoming"}
                onChange={(v) => endpoint(side, { credential: v.target.value })}
              >
                <option value="incoming" disabled={plugin}>
                  与收信共用{preset ? CREDENTIAL_LABEL[preset.credential] : "密码"}
                </option>
                <option value="separate">使用独立密码</option>
              </select>
            </Field>
            {ep.credential === "separate" &&
              SecretInput({
                role: "outgoing",
                label: "发信密码",
                hint: "已有密码不回填，留空表示不修改。",
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
            {e.id ? "管理账户" : "连接现有邮箱"}
          </div>
          <h2>{e.id ? draft.display_name : "添加邮箱账户"}</h2>
        </div>
        <button
          type="button"
          className="text-button"
          disabled={s.busy}
          onClick={end}
        >
          返回账户
        </button>
      </div>
      <p className="page-intro">
        {e.id
          ? "修改连接信息后可重新探测；已保存的密码不会回填。"
          : "输入邮箱地址，常见服务商会自动配置；粘贴授权码，探测通过后即可连接。"}
      </p>
      <fieldset disabled={s.busy || !s.online}>
        {!e.id && (
          <Field label="接入方式">
            <select
              value={plugin ? "plugin" : "imap"}
              onChange={(v) =>
                controller.editAccount({
                  ...e,
                  draft: {
                    ...draft,
                    incoming:
                      v.target.value === "plugin"
                        ? {
                            kind: "plugin",
                            provider: providers[0]?.id ?? "",
                            poll_secs: 60,
                          }
                        : newDraft().incoming,
                    outgoing:
                      v.target.value === "plugin"
                        ? { kind: "plugin", provider: providers[0]?.id ?? "" }
                        : newDraft().outgoing,
                  },
                  secrets: {},
                })
              }
            >
              <option value="imap">IMAP / SMTP</option>
              <option value="plugin" disabled={!providers.length}>
                已安装的邮箱服务扩展
              </option>
            </select>
          </Field>
        )}
        {!e.id && !plugin && <ProviderPills controller={controller} />}
        <div className="settings-field-pair">
          <Field label="邮箱地址">
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
          <Field label="显示名称">
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
                已识别为 <strong>{preset.name}</strong>，服务器参数已自动填好。
              </span>
            ) : domain ? (
              <>
                <span>
                  {w.discovered ??
                    `${domain} 不在内置服务商列表中。`}
                </span>
                {!w.discovered && (
                  <button
                    type="button"
                    className="secondary"
                    disabled={w.discovering}
                    onClick={() => void controller.discover()}
                  >
                    {w.discovering ? "正在检测…" : "自动检测服务器"}
                  </button>
                )}
              </>
            ) : (
              <span>输入地址后自动识别 QQ、网易、Gmail、iCloud 等常见邮箱。</span>
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
            <Field label="邮箱服务扩展">
              <select
                value={draft.incoming.provider ?? ""}
                onChange={(v) =>
                  edit({
                    incoming: { ...draft.incoming, provider: v.target.value },
                    outgoing: draft.outgoing,
                  })
                }
              >
                {!providers.some((p) => p.id === draft.incoming.provider) && (
                  <option value={draft.incoming.provider ?? ""}>
                    {draft.incoming.provider || "尚无可用扩展"}（不可用）
                  </option>
                )}
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name} · {p.provider_registered ? "可探测" : "尚未就绪"}
                  </option>
                ))}
              </select>
            </Field>
            <p className="group-note">
              填写扩展中已经存在的邮箱地址。探测只读取远程状态，不会创建邮箱或保存绑定。
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
                打开服务扩展管理
              </button>
            )}
          </>
        ) : (
          <>
            {SecretInput({
              role: "incoming",
              label: secretLabel,
              hint: saved
                ? "已保存的密码不会回填。留空表示保存时不修改；重新探测需要输入。"
                : preset
                  ? `${preset.name} 需要${CREDENTIAL_LABEL[preset.credential]}，不是网页登录密码。`
                  : "多数服务商要求使用应用专用密码或授权码。",
            })}
            {w.secretNotice && (
              <p className="secret-notice" role="status">
                {w.secretNotice}
              </p>
            )}
            {preset && (
              <div className="credential-guide">
                <strong>如何获取{CREDENTIAL_LABEL[preset.credential]}</strong>
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
                    打开 {preset.short} 官方设置页
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
            服务器连接参数（{w.manual ? "已手动调整" : "已自动匹配"} {summary}）
          </button>
        )}
        {open && (
          <div className={collapsible ? "server-panel" : undefined}>
            {!plugin && (
              <>
                <h3>收信</h3>
                {Server({ side: "incoming" })}
              </>
            )}
            <Field label="轮询间隔（秒）">
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
              启用发信
            </label>
            {draft.outgoing && (
              <>
                <Field label="发信方式">
                  <select
                    value={draft.outgoing.kind}
                    onChange={(v) =>
                      edit({
                        outgoing:
                          v.target.value === "smtp"
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
                    <option value="smtp">标准发信（SMTP）</option>
                    <option value="plugin" disabled={!providers.length}>
                      邮箱服务扩展
                    </option>
                  </select>
                </Field>
                {draft.outgoing.kind === "smtp" ? (
                  <>
                    <h3>发信</h3>
                    {Server({ side: "outgoing" })}
                  </>
                ) : (
                  <Field label="发信服务扩展">
                    <select
                      value={draft.outgoing.provider ?? ""}
                      onChange={(v) =>
                        endpoint("outgoing", { provider: v.target.value })
                      }
                    >
                      {!providers.some(
                        (p) => p.id === draft.outgoing?.provider,
                      ) && (
                        <option value={draft.outgoing.provider ?? ""}>
                          {draft.outgoing.provider || "尚无可用扩展"}（不可用）
                        </option>
                      )}
                      {providers.map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name} ·{" "}
                          {p.provider_registered ? "可探测" : "尚未就绪"}
                        </option>
                      ))}
                    </select>
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
            探测连接
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
            {s.busy ? "处理中…" : e.id ? "保存更改" : "保存并连接"}
          </button>
        </div>
        <p className="group-note">
          凭据只保存在系统钥匙串；关闭窗口后，后台以极低内存静默收信。
        </p>
      </fieldset>
      {e.id && (
        <p className="group-note">
          账户 ID：{e.id} · 版本 {e.revision} ·{" "}
          {saved?.credentials.incoming === "saved"
            ? "收信凭据已保存"
            : "凭据状态以账户列表为准"}
        </p>
      )}
    </form>
  );
}
export function Accounts({ controller }: { controller: SettingsController }) {
  const s = useStore(controller.store);
  if (s.editor) return <Editor controller={controller} />;
  return (
    <>
      <h2>账户</h2>
      <p className="page-intro">所有邮箱，各自井然有序。</p>
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
                ? ` · ${s.packages?.plugins.find((x) => x.id === p.incoming.provider)?.name ?? p.incoming.provider}`
                : ""}
            </small>
            {!p.editable && <small>配置文件账户，只读</small>}
            {needsRepair(p) && (
              <p className="account-alert" role="status">
                {p.runtime.state === "needs_credentials"
                  ? "服务器不再接受已保存的认证信息。重新生成授权码或应用专用密码后，在「更新凭据」中粘贴。"
                  : "最近一次连接失败，请检查服务器参数或网络后重新探测。"}
                {p.runtime.detail && <code>{p.runtime.detail}</code>}
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
                    ? "更新凭据"
                    : "修复连接"
                  : "管理"}
              </button>
              <details className="account-more">
                <summary aria-label={`${p.display_name} 的更多操作`}>
                  更多
                </summary>
                <div>
                  <button
                    type="button"
                    disabled={!s.online || s.busy}
                    onClick={() => controller.confirmAccount(p, false)}
                  >
                    {p.enabled ? "暂停同步" : "启用"}
                  </button>
                  <button
                    type="button"
                    className="danger"
                    disabled={!s.online || s.busy}
                    onClick={() => controller.confirmAccount(p, true)}
                  >
                    移除账户
                  </button>
                </div>
              </details>
            </div>
          )}
        </article>
      ))}
      {s.profiles?.length === 0 && (
        <div className="settings-empty">
          <h3>从你的第一个邮箱开始</h3>
          <p>连接现有邮箱，无需注册 {BRAND.productName} 账户。选一个服务商，向导会自动填好服务器。</p>
          <div className="provider-pills">
            {QUICK_PROVIDERS.map((id) => (
              <button
                key={id}
                type="button"
                className="provider-pill"
                disabled={!s.online || s.busy}
                onClick={() => controller.beginAccount(undefined, id)}
              >
                {providerById(id)?.short ?? "自定义 IMAP"}
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
          添加邮箱账户
        </button>
      </div>
      <p className="group-note">
        暂停同步会保留本地邮件。移除账户前，会单独说明本地数据、凭据和服务器邮件的处理范围。
      </p>
    </>
  );
}
