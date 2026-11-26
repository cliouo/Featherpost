import { m } from "@featherpost/i18n/messages";
import { useState } from "react";

import { useStore } from "../host/hooks";
import { useUiLocale } from "../i18n";
import { Field } from "./Field";
import { Choice } from "./Choice";
import { Switch } from "../ui";
import type { SettingsController } from "./controller";
import {
  PROXY_PROTOCOLS,
  PROXY_TEST_PRESETS,
  defaultImapTarget,
  inferProxyFailureStage,
  proxyStageHint,
  proxyStageLabel,
  systemProxyItems,
  systemSourceLabel,
} from "./proxy";
import type { ProxyMode, ProxyProtocol } from "./types";

const PROTOCOL_LABEL: Readonly<Record<ProxyProtocol, () => string>> = {
  socks5h: m.settings_proxy_protocol_socks5h,
  socks5: m.settings_proxy_protocol_socks5,
  http: m.settings_proxy_protocol_http,
  https: m.settings_proxy_protocol_https,
};

const MODE_OPTIONS: ReadonlyArray<{
  value: ProxyMode;
  title: () => string;
  description: () => string;
}> = [
  {
    value: "system",
    title: m.settings_proxy_mode_system,
    description: m.settings_proxy_mode_system_desc,
  },
  {
    value: "direct",
    title: m.settings_proxy_mode_direct,
    description: m.settings_proxy_mode_direct_desc,
  },
  {
    value: "custom",
    title: m.settings_proxy_mode_custom,
    description: m.settings_proxy_mode_custom_desc,
  },
];

export function NetworkProxy({
  controller,
}: {
  controller: SettingsController;
}) {
  useUiLocale();
  const s = useStore(controller.store);
  const [revealPassword, setRevealPassword] = useState(false);

  const draft = s.proxyDraft;
  const readOnly = s.proxyPersisted === false;
  const dirty = controller.proxyDirty;
  const systemItems = systemProxyItems(s.proxySystem);
  const defaultTarget = defaultImapTarget(s.profiles);
  const testResult = s.proxyTestResult;
  const failureStage =
    testResult && !testResult.ok ? inferProxyFailureStage(testResult) : null;
  const passwordLabel = m.settings_proxy_password();

  return (
    <>
      <h2>{m.settings_proxy_title()}</h2>
      <p className="page-intro">{m.settings_proxy_intro()}</p>

      {s.proxyUnsupported ? (
        <div className="provider-caveat proxy-banner" role="status">
          {m.settings_proxy_unsupported_banner()}
        </div>
      ) : (
        <>
          {readOnly && (
            <div className="provider-caveat proxy-banner" role="alert">
              {m.settings_proxy_readonly_banner()}
            </div>
          )}

          {!draft ? (
            <p className="group-note">{m.settings_proxy_loading()}</p>
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void controller.saveProxy();
              }}
            >
              <fieldset disabled={!s.online || s.busy || readOnly}>
                <div
                  className="proxy-mode-group"
                  role="radiogroup"
                  aria-label={m.settings_proxy_mode_label()}
                >
                  {MODE_OPTIONS.map((option) => {
                    const selected = draft.mode === option.value;
                    return (
                      <label
                        key={option.value}
                        className={`proxy-mode-option${selected ? " selected" : ""}`}
                      >
                        <input
                          type="radio"
                          name="proxy-mode"
                          value={option.value}
                          checked={selected}
                          onChange={() =>
                            controller.editProxy({ mode: option.value })
                          }
                        />
                        <span className="proxy-mode-copy">
                          <strong>{option.title()}</strong>
                          <small>{option.description()}</small>
                        </span>
                      </label>
                    );
                  })}
                </div>

                {draft.mode === "system" && (
                  <div className="proxy-mode-panel">
                    <div className="proxy-system-card" role="status">
                      <div className="proxy-system-header">
                        <strong>{m.settings_proxy_system_summary_title()}</strong>
                        {s.proxySystem?.source && (
                          <span className="tag">
                            {m.settings_proxy_system_source({
                              source: systemSourceLabel(s.proxySystem.source),
                            })}
                          </span>
                        )}
                      </div>
                      {systemItems.length > 0 ? (
                        <div className="proxy-endpoint-list">
                          {systemItems.map((item) => (
                            <div
                              key={`${item.label}:${item.endpoint}`}
                              className="proxy-endpoint-row"
                            >
                              <code>{`${item.label} · ${item.endpoint}`}</code>
                            </div>
                          ))}
                        </div>
                      ) : (
                        <p className="proxy-system-empty">
                          {m.settings_proxy_system_direct()}
                        </p>
                      )}
                      {s.proxySystem && s.proxySystem.bypass.length > 0 && (
                        <small className="proxy-system-meta">
                          {m.settings_proxy_system_bypass_count({
                            count: s.proxySystem.bypass.length,
                          })}
                        </small>
                      )}
                    </div>
                    {s.proxySystem?.pac_url && (
                      <div
                        className="provider-caveat proxy-pac-notice"
                        role="status"
                      >
                        {m.settings_proxy_system_pac_notice({
                          url: s.proxySystem.pac_url,
                        })}
                      </div>
                    )}
                  </div>
                )}

                {draft.mode === "direct" && (
                  <div className="proxy-mode-panel">
                    <p className="group-note proxy-mode-note">
                      {m.settings_proxy_direct_note()}
                    </p>
                  </div>
                )}

                {draft.mode === "custom" && (
                  <div className="proxy-mode-panel proxy-custom-panel">
                    <h3>{m.settings_proxy_custom_title()}</h3>
                    <Field label={m.settings_proxy_protocol()}>
                      <Choice
                        aria-label={m.settings_proxy_protocol()}
                        disabled={!s.online || s.busy || readOnly}
                        value={draft.protocol}
                        onValueChange={(value) =>
                          controller.editProxy({
                            protocol: value as ProxyProtocol,
                          })
                        }
                      >
                        {PROXY_PROTOCOLS.map((protocol) => (
                          <option key={protocol} value={protocol}>
                            {PROTOCOL_LABEL[protocol]()}
                          </option>
                        ))}
                      </Choice>
                    </Field>

                    <div className="settings-field-pair">
                      <Field
                        label={m.settings_proxy_host()}
                        hint={m.settings_proxy_host_hint()}
                      >
                        <input
                          value={draft.host}
                          placeholder={m.settings_proxy_host_placeholder()}
                          maxLength={512}
                          autoComplete="off"
                          autoCapitalize="none"
                          spellCheck={false}
                          onPaste={(e) => {
                            const pasted = e.clipboardData?.getData("text");
                            if (pasted && pasted.includes("://")) {
                              e.preventDefault();
                              controller.setProxyHost(pasted);
                            }
                          }}
                          onChange={(e) =>
                            controller.setProxyHost(e.target.value)
                          }
                        />
                      </Field>

                      <Field label={m.settings_proxy_port()}>
                        <input
                          type="number"
                          min={1}
                          max={65535}
                          step={1}
                          placeholder="7890"
                          value={draft.port}
                          onChange={(e) =>
                            controller.editProxy({ port: e.target.value })
                          }
                        />
                      </Field>
                    </div>

                    {s.proxyUrlNotice && (
                      <p className="secret-notice" role="status">
                        {s.proxyUrlNotice}
                      </p>
                    )}

                    <div className="setting-row proxy-auth-row">
                      <div className="setting-copy">
                        <strong>{m.settings_proxy_auth_toggle()}</strong>
                        <p>{m.settings_proxy_auth_toggle_hint()}</p>
                      </div>
                      <Switch
                        checked={draft.authEnabled}
                        aria-label={m.settings_proxy_auth_toggle()}
                        onCheckedChange={(next) =>
                          controller.toggleProxyAuth(next)
                        }
                      />
                    </div>

                    {draft.authEnabled && (
                      <div className="proxy-auth-fields">
                        <Field label={m.settings_proxy_username()}>
                          <input
                            value={draft.username}
                            maxLength={255}
                            autoComplete="off"
                            autoCapitalize="none"
                            spellCheck={false}
                            onChange={(e) =>
                              controller.editProxy({
                                username: e.target.value,
                              })
                            }
                          />
                        </Field>

                        <Field
                          label={passwordLabel}
                          hint={
                            s.proxyHasPassword && !draft.clearPassword
                              ? m.settings_proxy_password_saved_hint()
                              : undefined
                          }
                        >
                          <span className="secret-input">
                            <input
                              type={revealPassword ? "text" : "password"}
                              value={draft.password}
                              placeholder={
                                s.proxyHasPassword && !draft.clearPassword
                                  ? m.settings_proxy_password_placeholder_saved()
                                  : m.settings_proxy_password_placeholder()
                              }
                              maxLength={1024}
                              autoComplete="new-password"
                              autoCapitalize="none"
                              spellCheck={false}
                              onChange={(e) =>
                                controller.editProxy({
                                  password: e.target.value,
                                })
                              }
                            />
                            <button
                              type="button"
                              className="text-button"
                              aria-pressed={revealPassword}
                              aria-label={
                                revealPassword
                                  ? m.settings_secret_hide_label({
                                      label: passwordLabel,
                                    })
                                  : m.settings_secret_show_label({
                                      label: passwordLabel,
                                    })
                              }
                              onClick={() =>
                                setRevealPassword(!revealPassword)
                              }
                            >
                              {revealPassword
                                ? m.settings_secret_hide()
                                : m.settings_secret_show()}
                            </button>
                          </span>
                        </Field>

                        {s.proxyHasPassword && (
                          <div className="proxy-password-actions">
                            {draft.clearPassword ? (
                              <>
                                <small role="status">
                                  {m.settings_proxy_password_cleared_notice()}
                                </small>
                                <button
                                  type="button"
                                  className="text-button"
                                  onClick={() =>
                                    controller.undoClearSavedProxyPassword()
                                  }
                                >
                                  {m.settings_proxy_password_keep_undo()}
                                </button>
                              </>
                            ) : (
                              <button
                                type="button"
                                className="text-button danger"
                                onClick={() =>
                                  controller.clearSavedProxyPassword()
                                }
                              >
                                {m.settings_proxy_password_clear()}
                              </button>
                            )}
                          </div>
                        )}
                      </div>
                    )}

                    <Field
                      label={m.settings_proxy_bypass()}
                      hint={m.settings_proxy_bypass_hint()}
                    >
                      <textarea
                        rows={3}
                        value={draft.bypassText}
                        placeholder={m.settings_proxy_bypass_placeholder()}
                        maxLength={16384}
                        autoCapitalize="none"
                        spellCheck={false}
                        onChange={(e) =>
                          controller.editProxy({ bypassText: e.target.value })
                        }
                      />
                    </Field>
                  </div>
                )}

                <div className="settings-actions">
                  <button
                    type="submit"
                    className="primary"
                    disabled={!dirty || readOnly}
                  >
                    {m.settings_proxy_save()}
                  </button>
                  <button
                    type="button"
                    className="text-button"
                    disabled={!dirty}
                    onClick={() => controller.discard()}
                  >
                    {m.settings_proxy_revert()}
                  </button>
                </div>
              </fieldset>
            </form>
          )}

          <div className="settings-divider" />

          <section className="proxy-test-section">
            <h3>{m.settings_proxy_test_title()}</h3>
            <p className="group-note">{m.settings_proxy_test_intro()}</p>

            <Field
              label={m.settings_proxy_test_target()}
              hint={
                defaultTarget
                  ? m.settings_proxy_test_target_default_hint({
                      target: defaultTarget,
                    })
                  : m.settings_proxy_test_target_manual_hint()
              }
            >
              <input
                value={s.proxyTestTarget}
                placeholder={defaultTarget ?? PROXY_TEST_PRESETS[0]}
                maxLength={265}
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
                disabled={!s.online || s.busy || s.proxyTesting}
                onChange={(e) =>
                  controller.setProxyTestTarget(e.target.value)
                }
              />
            </Field>

            {!defaultTarget && (
              <div
                className="provider-pills"
                role="group"
                aria-label={m.settings_proxy_test_presets_label()}
              >
                {PROXY_TEST_PRESETS.map((preset) => {
                  const active = s.proxyTestTarget.trim() === preset;
                  return (
                    <button
                      key={preset}
                      type="button"
                      className={`provider-pill${active ? " active" : ""}`}
                      aria-pressed={active}
                      disabled={!s.online || s.busy || s.proxyTesting}
                      onClick={() => controller.setProxyTestTarget(preset)}
                    >
                      {preset}
                    </button>
                  );
                })}
              </div>
            )}

            <div className="settings-actions">
              <button
                type="button"
                className="secondary"
                disabled={!s.online || s.busy || s.proxyTesting}
                onClick={() => void controller.testProxy()}
              >
                {s.proxyTesting
                  ? m.settings_proxy_test_running()
                  : m.settings_proxy_test_action()}
              </button>
            </div>

            {testResult && (
              <div
                className="probe-results"
                role={testResult.ok ? "status" : "alert"}
              >
                <div className={`probe-line${testResult.ok ? "" : " failed"}`}>
                  <span className={`tag${testResult.ok ? "" : " warn"}`}>
                    {testResult.ok
                      ? m.settings_proxy_test_passed({
                          route:
                            testResult.route === "direct"
                              ? m.settings_proxy_test_route_direct()
                              : testResult.route,
                          latency: testResult.latency_ms,
                        })
                      : m.settings_proxy_test_failed({
                          stage: proxyStageLabel(failureStage ?? "unknown"),
                        })}
                  </span>
                  {!testResult.ok && (
                    <p className="probe-advice">
                      {proxyStageHint(failureStage ?? "unknown")}
                    </p>
                  )}
                  {!testResult.ok && testResult.route && (
                    <small className="probe-detail">
                      {m.settings_proxy_test_route_label({
                        route:
                          testResult.route === "direct"
                            ? m.settings_proxy_test_route_direct()
                            : testResult.route,
                      })}
                    </small>
                  )}
                  {!testResult.ok && testResult.detail && (
                    <code
                      className="probe-detail"
                      aria-label={m.settings_proxy_test_detail_label()}
                    >
                      {testResult.detail}
                    </code>
                  )}
                </div>
              </div>
            )}
          </section>
        </>
      )}
    </>
  );
}
