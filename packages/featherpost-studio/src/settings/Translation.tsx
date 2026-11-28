import { m } from "@featherpost/i18n/messages";
import { useState } from "react";

import { useStore } from "../host/hooks";
import { lifecycleLabel, packageText, useUiLocale } from "../i18n";
import { Choice } from "./Choice";
import { Field } from "./Field";
import { Switch } from "../ui";
import type { SettingsController } from "./controller";
import {
  BUILTIN_OPENAI_PROVIDER_ID,
  COMMON_LANGUAGE_TAGS,
  OPENAI_PRESETS,
  TARGET_LANGUAGE_OPTIONS,
  canReuseSavedApiKey,
  formatBytes,
  inspectTranslationEndpoint,
  languageTagLabel,
  requiresApiKeyRebind,
  savedBoundIdentityOf,
  translationProbeReasonText,
  translationProbeStageLabel,
  translationProviderStatus,
  translationProviderStatusLabel,
  type OpenAiPresetId,
} from "./translation";
import type { TranslationDisplayMode } from "../bridge/translation";

const PRESET_LABEL: Readonly<Record<OpenAiPresetId, () => string>> = {
  openai: m.settings_translation_preset_openai,
  deepseek: m.settings_translation_preset_deepseek,
  ollama: m.settings_translation_preset_ollama,
  custom: m.settings_translation_preset_custom,
};

const DISPLAY_MODE_OPTIONS: ReadonlyArray<{
  value: TranslationDisplayMode;
  title: () => string;
  description: () => string;
}> = [
  {
    value: "bilingual",
    title: m.settings_translation_display_bilingual,
    description: m.settings_translation_display_bilingual_desc,
  },
  {
    value: "translated_only",
    title: m.settings_translation_display_translated_only,
    description: m.settings_translation_display_translated_only_desc,
  },
];

function formatRoute(route: string): string {
  const clean = route.trim();
  if (!clean || clean === "direct") return m.settings_proxy_test_route_direct();
  if (clean === "ipc://daemon") return m.settings_translation_probe_route_ipc();
  return clean;
}

export function Translation({
  controller,
}: {
  controller: SettingsController;
}) {
  useUiLocale();
  const s = useStore(controller.store);
  const [revealApiKey, setRevealApiKey] = useState(false);
  const [customTargetMode, setCustomTargetMode] = useState(false);
  const [ignoredChoice, setIgnoredChoice] = useState<string>("en");
  const [customIgnoredTag, setCustomIgnoredTag] = useState("");

  const hasBridge = Boolean(controller.bridge.translation);
  const pref = s.translationPreference;
  const draft = s.translationDraft;
  const unhealthy = pref?.state_healthy === false;
  const dirty = controller.translationDirty;

  if (!hasBridge) {
    return (
      <>
        <h2>{m.settings_translation_title()}</h2>
        <p className="page-intro">{m.settings_translation_intro()}</p>
        <div className="provider-caveat proxy-banner" role="status">
          {m.settings_translation_unavailable_banner()}
        </div>
      </>
    );
  }

  const builtinEntry =
    s.translationProviders.find((p) => p.id === BUILTIN_OPENAI_PROVIDER_ID) ??
    null;
  const externalProviders = s.translationProviders.filter(
    (p) => p.kind === "external",
  );
  const activeId = draft?.activeProviderId ?? "";
  const isBuiltinSelected = activeId === BUILTIN_OPENAI_PROVIDER_ID;
  const activeExternal =
    activeId && !isBuiltinSelected
      ? (externalProviders.find((p) => p.id === activeId) ?? null)
      : null;
  const missingSavedExternal = Boolean(
    activeId && !isBuiltinSelected && !activeExternal,
  );

  const inspected = draft
    ? inspectTranslationEndpoint(draft.openaiBaseUrl)
    : null;
  const savedBound = savedBoundIdentityOf(pref);
  const savedHasApiKey = Boolean(pref?.openai_compatible?.has_api_key);
  const canReuseKey = draft ? canReuseSavedApiKey(draft, pref) : false;
  const rebindRequired = draft ? requiresApiKeyRebind(draft, pref) : false;
  const sameSavedEndpoint = Boolean(
    savedHasApiKey &&
      inspected?.ok &&
      savedBound !== null &&
      inspected.canonicalKey === savedBound,
  );

  const isPresetTarget = draft
    ? TARGET_LANGUAGE_OPTIONS.some(
        (tag) => tag.toLowerCase() === draft.targetLang.trim().toLowerCase(),
      )
    : true;
  const targetSelectValue =
    customTargetMode || !isPresetTarget
      ? "custom"
      : (TARGET_LANGUAGE_OPTIONS.find(
          (tag) =>
            tag.toLowerCase() ===
            (draft?.targetLang.trim().toLowerCase() ?? "auto"),
        ) ?? "auto");

  const activeProviderDisplayName = (() => {
    if (!activeId) return "";
    if (isBuiltinSelected) return m.settings_translation_builtin_name();
    if (activeExternal) return packageText(activeExternal).name;
    return activeId;
  })();

  const canTestActive = (() => {
    if (!draft || unhealthy || s.busy || s.translationTesting || !activeId) {
      return false;
    }
    if (isBuiltinSelected) return true;
    return Boolean(
      s.online &&
        s.translationExternalSupported &&
        activeExternal &&
        activeExternal.enabled &&
        activeExternal.registered,
    );
  })();

  const cacheStats = s.translationCacheStats;
  const physicalBytes = cacheStats
    ? cacheStats.physical_db_bytes +
      cacheStats.physical_wal_bytes +
      cacheStats.physical_shm_bytes
    : 0;
  const apiKeyLabel = m.settings_translation_api_key();

  const handleAddIgnored = () => {
    const raw = ignoredChoice === "custom" ? customIgnoredTag : ignoredChoice;
    if (controller.addIgnoredTranslationLanguage(raw)) {
      if (ignoredChoice === "custom") setCustomIgnoredTag("");
    }
  };

  return (
    <>
      <h2>{m.settings_translation_title()}</h2>
      <p className="page-intro">{m.settings_translation_intro()}</p>

      {unhealthy && (
        <div className="provider-caveat proxy-banner" role="alert">
          {m.settings_translation_config_unhealthy()}
        </div>
      )}

      {!draft ? (
        <p className="group-note">{m.settings_translation_loading()}</p>
      ) : (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void controller.saveTranslation();
          }}
        >
          <fieldset disabled={s.busy || unhealthy}>
            <h3>{m.settings_translation_provider_title()}</h3>
            <p className="group-note">
              {m.settings_translation_provider_intro()}
            </p>

            {!s.online && (
              <div className="provider-caveat" role="status">
                {m.settings_translation_offline_providers_note()}
              </div>
            )}

            {s.online && !s.translationExternalSupported && (
              <div className="provider-caveat" role="status">
                {m.settings_translation_external_unsupported_note()}
              </div>
            )}

            {s.online && s.translationProvidersError && (
              <div className="provider-caveat" role="status">
                {m.settings_translation_providers_load_error({
                  reason: s.translationProvidersError,
                })}
              </div>
            )}

            <div
              className="proxy-mode-group"
              role="radiogroup"
              aria-label={m.settings_translation_provider_title()}
            >
              <label
                className={`proxy-mode-option${activeId === "" ? " selected" : ""}`}
              >
                <input
                  type="radio"
                  name="translation-provider"
                  value=""
                  checked={activeId === ""}
                  onChange={() => controller.selectTranslationProvider("")}
                />
                <span className="proxy-mode-copy">
                  <strong>{m.settings_translation_provider_none()}</strong>
                  <small>{m.settings_translation_provider_none_desc()}</small>
                </span>
              </label>

              {(() => {
                const status = builtinEntry
                  ? translationProviderStatus(builtinEntry)
                  : pref?.openai_compatible
                    ? "ready"
                    : "unconfigured";
                return (
                  <label
                    className={`proxy-mode-option${isBuiltinSelected ? " selected" : ""}`}
                  >
                    <input
                      type="radio"
                      name="translation-provider"
                      value={BUILTIN_OPENAI_PROVIDER_ID}
                      checked={isBuiltinSelected}
                      onChange={() =>
                        controller.selectTranslationProvider(
                          BUILTIN_OPENAI_PROVIDER_ID,
                        )
                      }
                    />
                    <span className="proxy-mode-copy translation-provider-copy">
                      <span className="translation-provider-header">
                        <strong>
                          {m.settings_translation_builtin_name()}
                        </strong>
                        <span
                          className={`tag${status === "ready" ? "" : " warn"}`}
                        >
                          {translationProviderStatusLabel(status)}
                        </span>
                      </span>
                      <small>{m.settings_translation_builtin_desc()}</small>
                    </span>
                  </label>
                );
              })()}

              {externalProviders.map((entry) => {
                const selected = activeId === entry.id;
                const status = translationProviderStatus(entry);
                const text = packageText(entry);
                return (
                  <div
                    key={entry.id}
                    className={`proxy-mode-option translation-provider-card${selected ? " selected" : ""}`}
                  >
                    <label className="translation-provider-radio">
                      <input
                        type="radio"
                        name="translation-provider"
                        value={entry.id}
                        checked={selected}
                        onChange={() =>
                          controller.selectTranslationProvider(entry.id)
                        }
                      />
                      <span className="proxy-mode-copy translation-provider-copy">
                        <span className="translation-provider-header">
                          <strong>{text.name}</strong>
                          <span
                            className={`tag${status === "online" ? "" : " warn"}`}
                          >
                            {translationProviderStatusLabel(status)}
                          </span>
                        </span>
                        {text.description && <small>{text.description}</small>}
                        <small>
                          {`${entry.id} · ${entry.version} · ${lifecycleLabel(entry.lifecycle)}`}
                        </small>
                      </span>
                    </label>
                    {entry.has_ui && (
                      <div className="translation-provider-actions">
                        <button
                          type="button"
                          className="secondary"
                          disabled={!s.online || s.busy}
                          onClick={() => void controller.openProvider(entry.id)}
                        >
                          {m.settings_translation_open_plugin_settings()}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}

              {missingSavedExternal && (
                <label className="proxy-mode-option selected">
                  <input
                    type="radio"
                    name="translation-provider"
                    value={activeId}
                    checked
                    onChange={() => {}}
                  />
                  <span className="proxy-mode-copy translation-provider-copy">
                    <span className="translation-provider-header">
                      <strong>{activeId}</strong>
                      <span className="tag warn">
                        {m.settings_translation_status_unregistered()}
                      </span>
                    </span>
                    <small>
                      {m.settings_translation_saved_external_missing()}
                    </small>
                  </span>
                </label>
              )}
            </div>

            <div className="translation-provider-footer">
              <button
                type="button"
                className="text-button"
                disabled={s.busy || s.closeLocked}
                onClick={() =>
                  controller.selectSection("maild.settings.extensions")
                }
              >
                {m.settings_translation_manage_extensions()}
              </button>
            </div>

            {isBuiltinSelected && (
              <div className="proxy-mode-panel translation-openai-panel">
                <h3>{m.settings_translation_openai_title()}</h3>

                <div
                  className="provider-pills"
                  role="group"
                  aria-label={m.settings_translation_presets_label()}
                >
                  {OPENAI_PRESETS.map((preset) => {
                    const active = draft.openaiPreset === preset.id;
                    return (
                      <button
                        key={preset.id}
                        type="button"
                        className={`provider-pill${active ? " active" : ""}`}
                        aria-pressed={active}
                        onClick={() =>
                          controller.applyTranslationPreset(preset.id)
                        }
                      >
                        {PRESET_LABEL[preset.id]()}
                      </button>
                    );
                  })}
                </div>

                <Field
                  label={m.settings_translation_base_url()}
                  hint={m.settings_translation_base_url_hint()}
                >
                  <input
                    value={draft.openaiBaseUrl}
                    placeholder="https://api.openai.com/v1"
                    maxLength={1024}
                    autoComplete="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    onChange={(e) =>
                      controller.editTranslation({
                        openaiBaseUrl: e.target.value,
                        openaiAllowPlaintextLan: false,
                      })
                    }
                  />
                </Field>

                {inspected && !inspected.ok && draft.openaiBaseUrl.trim() !== "" && inspected.reason === "public_http_forbidden" && (
                  <div className="provider-caveat" role="status">
                    {m.settings_translation_error_public_https_required()}
                  </div>
                )}

                {inspected?.ok && inspected.isPlaintextLan && (
                  <div className="provider-caveat" role="status">
                    <p className="translation-notice-copy">
                      {m.settings_translation_plaintext_lan_notice()}
                    </p>
                    <label className="settings-check translation-lan-check">
                      <input
                        type="checkbox"
                        checked={draft.openaiAllowPlaintextLan}
                        onChange={(e) =>
                          controller.editTranslation({
                            openaiAllowPlaintextLan: e.target.checked,
                          })
                        }
                      />
                      <span>
                        {m.settings_translation_plaintext_lan_confirm()}
                      </span>
                    </label>
                  </div>
                )}

                <Field
                  label={apiKeyLabel}
                  hint={
                    inspected?.ok && inspected.allowsKeyless
                      ? m.settings_translation_key_optional_loopback()
                      : canReuseKey
                        ? m.settings_translation_key_saved_hint()
                        : undefined
                  }
                >
                  <span className="secret-input">
                    <input
                      type={revealApiKey ? "text" : "password"}
                      value={draft.openaiApiKey}
                      placeholder={
                        canReuseKey
                          ? m.settings_translation_key_placeholder_saved()
                          : inspected?.ok && inspected.allowsKeyless
                            ? m.settings_translation_key_placeholder_optional()
                            : m.settings_translation_key_placeholder()
                      }
                      maxLength={4096}
                      autoComplete="new-password"
                      autoCapitalize="none"
                      spellCheck={false}
                      onChange={(e) =>
                        controller.editTranslation({
                          openaiApiKey: e.target.value,
                        })
                      }
                    />
                    <button
                      type="button"
                      className="text-button"
                      aria-pressed={revealApiKey}
                      aria-label={
                        revealApiKey
                          ? m.settings_secret_hide_label({
                              label: apiKeyLabel,
                            })
                          : m.settings_secret_show_label({
                              label: apiKeyLabel,
                            })
                      }
                      onClick={() => setRevealApiKey(!revealApiKey)}
                    >
                      {revealApiKey
                        ? m.settings_secret_hide()
                        : m.settings_secret_show()}
                    </button>
                  </span>
                </Field>

                {rebindRequired && (
                  <div className="provider-caveat" role="status">
                    {m.settings_translation_key_rebind_notice()}
                  </div>
                )}

                {sameSavedEndpoint && (
                  <div className="proxy-password-actions">
                    {draft.openaiClearApiKey ? (
                      <>
                        <small role="status">
                          {m.settings_translation_key_cleared_notice()}
                        </small>
                        <button
                          type="button"
                          className="text-button"
                          onClick={() =>
                            controller.undoClearSavedTranslationApiKey()
                          }
                        >
                          {m.settings_translation_key_keep_undo()}
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="text-button danger"
                        onClick={() =>
                          controller.clearSavedTranslationApiKey()
                        }
                      >
                        {m.settings_translation_key_clear()}
                      </button>
                    )}
                  </div>
                )}

                <Field
                  label={m.settings_translation_model()}
                  hint={m.settings_translation_model_hint()}
                >
                  <input
                    value={draft.openaiModel}
                    placeholder="gpt-4o-mini"
                    maxLength={256}
                    autoComplete="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    onChange={(e) =>
                      controller.editTranslation({
                        openaiModel: e.target.value,
                      })
                    }
                  />
                </Field>
              </div>
            )}

            {activeId !== "" && (
              <div className="translation-test-block">
                <div className="settings-actions">
                  <button
                    type="button"
                    className="secondary"
                    disabled={!canTestActive}
                    onClick={() => void controller.testTranslationProvider()}
                  >
                    {s.translationTesting
                      ? m.settings_translation_test_running()
                      : m.settings_translation_test_action()}
                  </button>
                </div>

                {s.translationTestResult && (
                  <div
                    className="probe-results"
                    role={s.translationTestResult.ok ? "status" : "alert"}
                  >
                    <div
                      className={`probe-line${s.translationTestResult.ok ? "" : " failed"}`}
                    >
                      <span
                        className={`tag${s.translationTestResult.ok ? "" : " warn"}`}
                      >
                        {s.translationTestResult.ok
                          ? m.settings_translation_test_passed({
                              route: formatRoute(s.translationTestResult.route),
                              latency: s.translationTestResult.latency_ms,
                            })
                          : m.settings_translation_test_failed({
                              stage: translationProbeStageLabel(
                                s.translationTestResult.stage,
                              ),
                            })}
                      </span>
                      {!s.translationTestResult.ok && (
                        <p className="probe-advice">
                          {translationProbeReasonText(
                            s.translationTestResult.reason,
                          )}
                        </p>
                      )}
                      {!s.translationTestResult.ok &&
                        s.translationTestResult.route && (
                          <small className="probe-detail">
                            {m.settings_translation_test_meta({
                              route: formatRoute(s.translationTestResult.route),
                              latency: s.translationTestResult.latency_ms,
                            })}
                          </small>
                        )}
                      {s.translationTestResult.plaintext_lan_transport && (
                        <small className="probe-detail">
                          {m.settings_translation_plaintext_lan_notice()}
                        </small>
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}

            <div className="settings-divider" />

            <h3>{m.settings_translation_display_title()}</h3>
            <div
              className="proxy-mode-group"
              role="radiogroup"
              aria-label={m.settings_translation_display_title()}
            >
              {DISPLAY_MODE_OPTIONS.map((option) => {
                const selected = draft.displayMode === option.value;
                return (
                  <label
                    key={option.value}
                    className={`proxy-mode-option${selected ? " selected" : ""}`}
                  >
                    <input
                      type="radio"
                      name="translation-display-mode"
                      value={option.value}
                      checked={selected}
                      onChange={() =>
                        controller.editTranslation({
                          displayMode: option.value,
                        })
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

            <Field
              label={m.settings_translation_target_lang()}
              hint={m.settings_translation_target_lang_hint()}
            >
              <Choice
                aria-label={m.settings_translation_target_lang()}
                disabled={s.busy || unhealthy}
                value={targetSelectValue}
                onValueChange={(value) => {
                  if (value === "custom") {
                    setCustomTargetMode(true);
                  } else {
                    setCustomTargetMode(false);
                    controller.editTranslation({ targetLang: value });
                  }
                }}
              >
                {TARGET_LANGUAGE_OPTIONS.map((tag) => (
                  <option key={tag} value={tag}>
                    {tag === "auto"
                      ? languageTagLabel("auto")
                      : `${languageTagLabel(tag)} (${tag})`}
                  </option>
                ))}
                <option value="custom">
                  {m.settings_translation_lang_custom()}
                </option>
              </Choice>
            </Field>

            {targetSelectValue === "custom" && (
              <Field
                label={m.settings_translation_target_lang_custom_label()}
                hint={m.settings_translation_target_lang_custom_hint()}
              >
                <input
                  value={draft.targetLang}
                  placeholder="en-GB"
                  maxLength={32}
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  onChange={(e) =>
                    controller.editTranslation({ targetLang: e.target.value })
                  }
                />
              </Field>
            )}

            <div className="settings-divider" />

            <h3>{m.settings_translation_automation_title()}</h3>

            <div className="setting-row">
              <div className="setting-copy">
                <strong>{m.settings_translation_detect_banner_label()}</strong>
                <p>{m.settings_translation_detect_banner_desc()}</p>
              </div>
              <Switch
                checked={draft.detectBannerEnabled}
                aria-label={m.settings_translation_detect_banner_label()}
                disabled={s.busy || unhealthy}
                onCheckedChange={(checked) =>
                  controller.editTranslation({ detectBannerEnabled: checked })
                }
              />
            </div>

            <div className="setting-row">
              <div className="setting-copy">
                <strong>{m.settings_translation_auto_translate_label()}</strong>
                <p>
                  {activeId
                    ? m.settings_translation_auto_translate_bound_desc({
                        provider: activeProviderDisplayName,
                      })
                    : m.settings_translation_auto_translate_no_provider_desc()}
                </p>
              </div>
              <Switch
                checked={draft.autoTranslate}
                aria-label={m.settings_translation_auto_translate_label()}
                disabled={s.busy || unhealthy || !activeId}
                onCheckedChange={(checked) =>
                  controller.toggleTranslationAutoTranslate(checked)
                }
              />
            </div>

            {s.translationAutoSwitchNotice && (
              <div className="provider-caveat" role="status">
                {m.settings_translation_auto_switch_notice({
                  provider: activeProviderDisplayName,
                })}
              </div>
            )}

            <div className="translation-ignored-section">
              <h4>{m.settings_translation_ignored_title()}</h4>
              <p className="group-note">
                {m.settings_translation_ignored_desc()}
              </p>

              {draft.ignoredLanguages.length > 0 ? (
                <div className="translation-lang-chips">
                  {draft.ignoredLanguages.map((tag) => {
                    const label = languageTagLabel(tag);
                    const chipText =
                      label === tag ? tag : `${label} (${tag})`;
                    return (
                      <span key={tag} className="translation-lang-chip">
                        <span>{chipText}</span>
                        <button
                          type="button"
                          className="translation-lang-chip-remove"
                          aria-label={m.settings_translation_ignored_remove({
                            language: chipText,
                          })}
                          onClick={() =>
                            controller.removeIgnoredTranslationLanguage(tag)
                          }
                        >
                          ×
                        </button>
                      </span>
                    );
                  })}
                </div>
              ) : (
                <p className="group-note">
                  {m.settings_translation_ignored_empty()}
                </p>
              )}

              <div className="translation-ignored-add">
                <Choice
                  aria-label={m.settings_translation_ignored_pick_label()}
                  disabled={s.busy || unhealthy}
                  value={ignoredChoice}
                  onValueChange={(value) => setIgnoredChoice(value)}
                >
                  {COMMON_LANGUAGE_TAGS.map((tag) => (
                    <option key={tag} value={tag}>
                      {`${languageTagLabel(tag)} (${tag})`}
                    </option>
                  ))}
                  <option value="custom">
                    {m.settings_translation_lang_custom()}
                  </option>
                </Choice>
                {ignoredChoice === "custom" && (
                  <input
                    aria-label={m.settings_translation_ignored_custom_label()}
                    value={customIgnoredTag}
                    placeholder="pt-BR"
                    maxLength={32}
                    autoComplete="off"
                    autoCapitalize="none"
                    spellCheck={false}
                    onChange={(e) => setCustomIgnoredTag(e.target.value)}
                  />
                )}
                <button
                  type="button"
                  className="secondary"
                  disabled={
                    s.busy ||
                    unhealthy ||
                    (ignoredChoice === "custom" && !customIgnoredTag.trim())
                  }
                  onClick={handleAddIgnored}
                >
                  {m.settings_translation_ignored_add()}
                </button>
              </div>
            </div>

            <div className="settings-actions">
              <button
                type="submit"
                className="primary"
                disabled={!dirty || unhealthy}
              >
                {m.settings_translation_save()}
              </button>
              <button
                type="button"
                className="text-button"
                disabled={!dirty}
                onClick={() => controller.discard()}
              >
                {m.settings_translation_revert()}
              </button>
            </div>
          </fieldset>
        </form>
      )}

      <div className="settings-divider" />

      <section className="translation-cache-section">
        <h3>{m.settings_translation_cache_title()}</h3>
        <p className="group-note">{m.settings_translation_cache_desc()}</p>

        {cacheStats ? (
          <div className="proxy-system-card" role="status">
            <div className="translation-cache-grid">
              <div className="translation-cache-metric">
                <span>{m.settings_translation_cache_entries_label()}</span>
                <strong>
                  {m.settings_translation_cache_entries_value({
                    count: cacheStats.entry_count,
                    max: cacheStats.max_entries,
                  })}
                </strong>
              </div>
              <div className="translation-cache-metric">
                <span>{m.settings_translation_cache_logical_label()}</span>
                <strong>
                  {m.settings_translation_cache_size_quota({
                    used: formatBytes(cacheStats.logical_bytes),
                    max: formatBytes(cacheStats.max_logical_bytes),
                  })}
                </strong>
              </div>
              <div className="translation-cache-metric">
                <span>{m.settings_translation_cache_physical_label()}</span>
                <strong>{formatBytes(physicalBytes)}</strong>
              </div>
              <div className="translation-cache-metric">
                <span>{m.settings_translation_cache_ttl_label()}</span>
                <strong>
                  {m.settings_translation_cache_ttl_value({
                    days: cacheStats.ttl_days,
                  })}
                </strong>
              </div>
            </div>
          </div>
        ) : (
          <p className="group-note">{m.settings_translation_cache_loading()}</p>
        )}

        <div className="settings-actions">
          <button
            type="button"
            className="secondary danger"
            disabled={s.busy || s.closeLocked}
            onClick={() => controller.confirmClearTranslationCache()}
          >
            {m.settings_translation_cache_clear_action()}
          </button>
        </div>
      </section>
    </>
  );
}
