import {
  BUILTIN_OPENAI_PROVIDER_ID,
  type TranslationBridge,
  type TranslationCacheClearReport,
  type TranslationCacheStats,
  type TranslationPreferenceUpdate,
  type TranslationPreferenceView,
  type TranslationProbeResult,
  type UiDetectLanguageRequest,
  type UiHtmlTranslationResult,
  type UiLanguageDetection,
  type UiPlainTranslationResult,
  type UiTestTranslationProviderRequest,
  type UiTranslateHtmlRequest,
  type UiTranslatePlainRequest,
  type UiTranslationProgress,
  type UiTranslationProviderEntry,
  type UiTranslationProvidersResponse,
} from "../src/bridge/translation";
import { inspectTranslationEndpoint } from "../src/settings/translation";

export function defaultBuiltinEntry(
  overrides: Partial<UiTranslationProviderEntry> = {},
): UiTranslationProviderEntry {
  return {
    id: BUILTIN_OPENAI_PROVIDER_ID,
    kind: "builtin",
    name: "OpenAI Compatible",
    description: null,
    version: "1.0.0",
    lifecycle: "builtin",
    enabled: true,
    state: "unconfigured",
    has_ui: false,
    registered: true,
    configured: false,
    proxy_ready: true,
    generation: 1,
    supported_languages: [],
    max_batch_items: 24,
    max_batch_chars: 12000,
    max_concurrency: 2,
    supports_cancel: true,
    variant_revision: "",
    ...overrides,
  };
}

export function defaultTranslationPreference(
  overrides: Partial<TranslationPreferenceView> = {},
): TranslationPreferenceView {
  return {
    revision: 1,
    state_healthy: true,
    active_provider_id: "",
    display_mode: "bilingual",
    target_lang: "auto",
    detect_banner_enabled: true,
    auto_translate: false,
    auto_translate_provider_id: null,
    ignored_languages: [],
    openai_compatible: null,
    ...overrides,
  };
}

export function defaultTranslationCacheStats(
  overrides: Partial<TranslationCacheStats> = {},
): TranslationCacheStats {
  return {
    entry_count: 12,
    logical_bytes: 24_576,
    max_entries: 2_000,
    max_logical_bytes: 32 * 1024 * 1024,
    ttl_days: 30,
    physical_db_bytes: 65_536,
    physical_wal_bytes: 8_192,
    physical_shm_bytes: 32_768,
    clear_epoch: 0,
    ...overrides,
  };
}

export class FakeTranslation implements TranslationBridge {
  preference: TranslationPreferenceView = defaultTranslationPreference();
  providersResponse: UiTranslationProvidersResponse = {
    catalog_generation: 1,
    external_supported: true,
    providers: [defaultBuiltinEntry()],
  };
  cacheStats: TranslationCacheStats = defaultTranslationCacheStats();
  probeResult: TranslationProbeResult = {
    ok: true,
    stage: "done",
    reason: "ok",
    latency_ms: 128,
    route: "direct",
    plaintext_lan_transport: false,
  };

  savedUpdates: TranslationPreferenceUpdate[] = [];
  testRequests: UiTestTranslationProviderRequest[] = [];
  detectRequests: UiDetectLanguageRequest[] = [];
  htmlRequests: UiTranslateHtmlRequest[] = [];
  plainRequests: UiTranslatePlainRequest[] = [];
  cancelledJobs: string[] = [];
  clearCalls = 0;

  preferenceError: Error | null = null;
  saveError: Error | null = null;
  providersError: Error | null = null;
  probeError: Error | null = null;
  cacheStatsError: Error | null = null;
  clearCacheError: Error | null = null;
  clearReclaimSucceeded = true;

  providerHandlers = new Set<(event: UiTranslationProvidersResponse) => void>();
  progressHandlers = new Set<(event: UiTranslationProgress) => void>();

  async getTranslationPreferences(): Promise<TranslationPreferenceView> {
    if (this.preferenceError) throw this.preferenceError;
    return structuredClone(this.preference);
  }

  async saveTranslationPreferences(
    update: TranslationPreferenceUpdate,
  ): Promise<TranslationPreferenceView> {
    if (this.saveError) throw this.saveError;
    this.savedUpdates.push(structuredClone(update));
    let nextOpenAi = this.preference.openai_compatible
      ? structuredClone(this.preference.openai_compatible)
      : null;
    if (update.openai_compatible) {
      const upd = update.openai_compatible;
      const inspected = inspectTranslationEndpoint(upd.base_url);
      const canonical = inspected.ok ? inspected.canonicalKey : upd.base_url;
      const hadKeyOnSame =
        Boolean(nextOpenAi?.has_api_key) &&
        (nextOpenAi?.bound_service_identity ?? nextOpenAi?.normalized_identity) ===
          canonical;
      const newKey = upd.api_key?.trim() ?? "";
      const hasKey = upd.clear_api_key
        ? false
        : newKey.length > 0
          ? true
          : hadKeyOnSame;
      nextOpenAi = {
        preset: upd.preset ?? null,
        base_url: upd.base_url,
        normalized_identity: canonical,
        transport: inspected.ok ? inspected.transport : "public_https",
        plaintext_lan_transport: inspected.ok ? inspected.isPlaintextLan : false,
        model: upd.model,
        has_api_key: hasKey,
        bound_service_identity: hasKey ? canonical : null,
        allow_plaintext_lan: Boolean(upd.allow_plaintext_lan),
      };
    }
    this.preference = {
      ...this.preference,
      revision: this.preference.revision + 1,
      active_provider_id: update.active_provider_id,
      display_mode: update.display_mode,
      target_lang: update.target_lang,
      detect_banner_enabled: update.detect_banner_enabled,
      auto_translate:
        update.auto_translate &&
        update.active_provider_id !== "" &&
        update.auto_translate_provider_id === update.active_provider_id,
      auto_translate_provider_id:
        update.auto_translate &&
        update.active_provider_id !== "" &&
        update.auto_translate_provider_id === update.active_provider_id
          ? update.active_provider_id
          : null,
      ignored_languages: [...(update.ignored_languages ?? [])],
      openai_compatible: nextOpenAi,
    };
    return structuredClone(this.preference);
  }

  async listTranslationProviders(): Promise<UiTranslationProvidersResponse> {
    if (this.providersError) throw this.providersError;
    return structuredClone(this.providersResponse);
  }

  async testTranslationProvider(
    request: UiTestTranslationProviderRequest,
  ): Promise<TranslationProbeResult> {
    this.testRequests.push(structuredClone(request));
    if (this.probeError) throw this.probeError;
    return structuredClone(this.probeResult);
  }

  async detectEmailLanguage(
    request: UiDetectLanguageRequest,
  ): Promise<UiLanguageDetection> {
    this.detectRequests.push(structuredClone(request));
    return {
      language: "en",
      confidence: 0.95,
      effective_chars: 120,
      target_lang: this.preference.target_lang,
      matches_target: false,
      is_ignored: false,
      should_offer_banner: true,
    };
  }

  async translateHtmlDocument(
    request: UiTranslateHtmlRequest,
  ): Promise<UiHtmlTranslationResult> {
    this.htmlRequests.push(structuredClone(request));
    return {
      job_id: "tr-1",
      token: request.token,
      rev: 1,
      bilingual_url: `featherpost-mail://localhost/doc/${request.token}?translate=bilingual&rev=1`,
      translated_url: `featherpost-mail://localhost/doc/${request.token}?translate=translated&rev=1`,
      bilingual_dark_url: `featherpost-mail://localhost/doc/${request.token}?translate=bilingual&rev=1&appearance=dark`,
      translated_dark_url: `featherpost-mail://localhost/doc/${request.token}?translate=translated&rev=1&appearance=dark`,
      translated_subject: "Translated subject",
      source_lang: "en",
      target_lang: request.target_lang ?? this.preference.target_lang,
      provider_id: request.provider_id ?? this.preference.active_provider_id,
      provider_generation: 1,
      from_cache: false,
    };
  }

  async translatePlainTextMessage(
    request: UiTranslatePlainRequest,
  ): Promise<UiPlainTranslationResult> {
    this.plainRequests.push(structuredClone(request));
    return {
      job_id: "tr-2",
      original_subject: "Hello",
      translated_subject: "Translated Hello",
      blocks: [{ id: 1, original: "Body", translated: "Translated Body" }],
      source_lang: "en",
      target_lang: request.target_lang ?? this.preference.target_lang,
      provider_id: request.provider_id ?? this.preference.active_provider_id,
      provider_generation: 1,
      from_cache: false,
    };
  }

  async cancelTranslation(jobId: string): Promise<boolean> {
    this.cancelledJobs.push(jobId);
    return true;
  }

  async getTranslationCacheStats(): Promise<TranslationCacheStats> {
    if (this.cacheStatsError) throw this.cacheStatsError;
    return structuredClone(this.cacheStats);
  }

  async clearTranslationCache(): Promise<TranslationCacheClearReport> {
    if (this.clearCacheError) throw this.clearCacheError;
    this.clearCalls += 1;
    const cleared = this.cacheStats.entry_count;
    this.cacheStats = {
      ...this.cacheStats,
      entry_count: 0,
      logical_bytes: 0,
      physical_db_bytes: this.clearReclaimSucceeded ? 4_096 : this.cacheStats.physical_db_bytes,
      physical_wal_bytes: this.clearReclaimSucceeded ? 0 : this.cacheStats.physical_wal_bytes,
      clear_epoch: this.cacheStats.clear_epoch + 1,
    };
    return {
      cleared_entries: cleared,
      new_clear_epoch: this.cacheStats.clear_epoch,
      physical_reclaim_succeeded: this.clearReclaimSucceeded,
      physical_reclaim_note: this.clearReclaimSucceeded
        ? null
        : "wal_checkpoint_busy_active_reader",
      stats_after: structuredClone(this.cacheStats),
    };
  }

  async onProvidersChanged(
    handler: (event: UiTranslationProvidersResponse) => void,
  ): Promise<() => void> {
    this.providerHandlers.add(handler);
    return () => {
      this.providerHandlers.delete(handler);
    };
  }

  async onProgress(
    handler: (event: UiTranslationProgress) => void,
  ): Promise<() => void> {
    this.progressHandlers.add(handler);
    return () => {
      this.progressHandlers.delete(handler);
    };
  }

  pushProviders(event: UiTranslationProvidersResponse) {
    this.providersResponse = structuredClone(event);
    for (const handler of this.providerHandlers) {
      handler(structuredClone(event));
    }
  }

  pushProgress(event: UiTranslationProgress) {
    for (const handler of this.progressHandlers) {
      handler(structuredClone(event));
    }
  }
}
