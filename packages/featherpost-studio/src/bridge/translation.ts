import type { PackageL10n } from "../settings/types";

/** Reserved provider ID prefix for built-in translation providers. */
export const BUILTIN_PROVIDER_PREFIX = "builtin.";

/** Built-in OpenAI-compatible provider identifier. */
export const BUILTIN_OPENAI_PROVIDER_ID = "builtin.openai_compatible";

/** Tauri event emitted ONLY to the owning window during HTML/plain-text translation. */
export const TRANSLATION_PROGRESS_EVENT = "maild://translation-progress";

/** Tauri event emitted whenever the external translation provider catalog changes. */
export const TRANSLATION_PROVIDERS_EVENT = "maild://translation-providers";

export type TranslationDisplayMode = "bilingual" | "translated_only";

export type TranslationProviderKind = "builtin" | "external";

export type TranslationEndpointTransport =
  | "public_https"
  | "loopback_http"
  | "loopback_https"
  | "lan_http"
  | "lan_https";

export type TranslationProbeStage =
  | "config"
  | "proxy"
  | "network"
  | "tls"
  | "auth"
  | "upstream"
  | "response"
  | "done";

export type DetectedLanguage =
  | "zh-CN"
  | "zh-TW"
  | "ja"
  | "ko"
  | "en"
  | "fr"
  | "de"
  | "es"
  | "pt"
  | "it"
  | "nl"
  | "ru"
  | "uk"
  | "ar"
  | "th"
  | "el"
  | "he"
  | "hi"
  | "unknown";

/** Safe read-only DTO for built-in OpenAI-compatible settings (never contains plaintext API key). */
export interface OpenAiConfigView {
  preset?: string | null;
  base_url: string;
  normalized_identity: string;
  transport: TranslationEndpointTransport;
  plaintext_lan_transport: boolean;
  model: string;
  has_api_key: boolean;
  bound_service_identity?: string | null;
  allow_plaintext_lan: boolean;
}

/** Safe read-only DTO for `translation_preference.toml`. */
export interface TranslationPreferenceView {
  revision: number;
  state_healthy: boolean;
  active_provider_id: string;
  display_mode: TranslationDisplayMode;
  target_lang: string;
  detect_banner_enabled: boolean;
  auto_translate: boolean;
  auto_translate_provider_id?: string | null;
  ignored_languages: string[];
  openai_compatible?: OpenAiConfigView | null;
}

/** Draft/update payload for built-in OpenAI-compatible settings (`deny_unknown_fields`). */
export interface OpenAiConfigUpdate {
  preset?: string | null;
  base_url: string;
  model: string;
  api_key?: string | null;
  clear_api_key?: boolean;
  allow_plaintext_lan?: boolean;
}

/** Update payload for saving `translation_preference.toml` (`deny_unknown_fields`). */
export interface TranslationPreferenceUpdate {
  expected_revision?: number | null;
  active_provider_id: string;
  display_mode: TranslationDisplayMode;
  target_lang: string;
  detect_banner_enabled: boolean;
  auto_translate: boolean;
  auto_translate_provider_id?: string | null;
  ignored_languages?: string[];
  openai_compatible?: OpenAiConfigUpdate | null;
}

/** Unified provider catalog entry (built-in or external Cordis plugin). */
export interface UiTranslationProviderEntry {
  id: string;
  kind: TranslationProviderKind;
  name: string;
  description?: string | null;
  l10n?: PackageL10n;
  version: string;
  lifecycle: string;
  enabled: boolean;
  state: string;
  has_ui: boolean;
  registered: boolean;
  configured: boolean;
  proxy_ready: boolean;
  generation: number;
  supported_languages: string[];
  max_batch_items: number;
  max_batch_chars: number;
  max_concurrency: number;
  supports_cancel: boolean;
  variant_revision: string;
}

/** Snapshot returned by `ui_translation_list_providers` and pushed on `maild://translation-providers`. */
export interface UiTranslationProvidersResponse {
  catalog_generation: number;
  external_supported: boolean;
  providers: UiTranslationProviderEntry[];
}

/** Request payload for `ui_translation_test_provider` (`deny_unknown_fields`). */
export interface UiTestTranslationProviderRequest {
  provider_id: string;
  target_lang: string;
  draft_openai?: OpenAiConfigUpdate | null;
}

/** Sanitized probe result returned by `ui_translation_test_provider`. */
export interface TranslationProbeResult {
  ok: boolean;
  stage: TranslationProbeStage;
  reason: string;
  latency_ms: number;
  route: string;
  plaintext_lan_transport: boolean;
}

/** Controlled language detection request (`deny_unknown_fields`). */
export interface UiDetectLanguageRequest {
  token?: string | null;
  account?: string | null;
  folder?: string | null;
  uid?: string | null;
}

/** Lightweight language detection result returned by `ui_translation_detect_language`. */
export interface UiLanguageDetection {
  language: DetectedLanguage;
  confidence: number;
  effective_chars: number;
  target_lang: string;
  matches_target: boolean;
  is_ignored: boolean;
  should_offer_banner: boolean;
}

/** Controlled HTML document translation request (`deny_unknown_fields`). */
export interface UiTranslateHtmlRequest {
  token: string;
  target_lang?: string | null;
  provider_id?: string | null;
  force_refresh?: boolean;
}

/** Result returned by `ui_translation_translate_html`. */
export interface UiHtmlTranslationResult {
  job_id: string;
  token: string;
  rev: number;
  bilingual_url: string;
  translated_url: string;
  bilingual_dark_url: string;
  translated_dark_url: string;
  translated_subject?: string | null;
  source_lang: string;
  target_lang: string;
  provider_id: string;
  provider_generation: number;
  from_cache: boolean;
}

/** Controlled plain-text translation request (`deny_unknown_fields`). */
export interface UiTranslatePlainRequest {
  account: string;
  folder: string;
  uid: string;
  target_lang?: string | null;
  provider_id?: string | null;
  force_refresh?: boolean;
}

/** Structured paragraph pair for safe React plain-text rendering. */
export interface PlainTextBlockView {
  id: number;
  original: string;
  translated: string;
}

/** Result returned by `ui_translation_translate_plain`. */
export interface UiPlainTranslationResult {
  job_id: string;
  original_subject?: string | null;
  translated_subject?: string | null;
  blocks: PlainTextBlockView[];
  source_lang: string;
  target_lang: string;
  provider_id: string;
  provider_generation: number;
  from_cache: boolean;
}

/** Sanitized progress payload emitted on `maild://translation-progress`. */
export interface UiTranslationProgress {
  job_id: string;
  token?: string | null;
  completed_batches: number;
  total_batches: number;
  status: "running" | "completed" | "cancelled" | "failed";
}

/** Separate logical quota vs physical on-disk occupancy report. */
export interface TranslationCacheStats {
  entry_count: number;
  logical_bytes: number;
  max_entries: number;
  max_logical_bytes: number;
  ttl_days: number;
  physical_db_bytes: number;
  physical_wal_bytes: number;
  physical_shm_bytes: number;
  clear_epoch: number;
}

/** Result of clearing local translation cache. */
export interface TranslationCacheClearReport {
  cleared_entries: number;
  new_clear_epoch: number;
  physical_reclaim_succeeded: boolean;
  physical_reclaim_note?: string | null;
  stats_after: TranslationCacheStats;
}

/** Typed client for all 10 ADR-0050 translation Tauri commands and 2 push events. */
export interface TranslationBridge {
  getTranslationPreferences(): Promise<TranslationPreferenceView>;
  saveTranslationPreferences(
    update: TranslationPreferenceUpdate,
  ): Promise<TranslationPreferenceView>;
  listTranslationProviders(): Promise<UiTranslationProvidersResponse>;
  testTranslationProvider(
    request: UiTestTranslationProviderRequest,
  ): Promise<TranslationProbeResult>;
  detectEmailLanguage(
    request: UiDetectLanguageRequest,
  ): Promise<UiLanguageDetection>;
  translateHtmlDocument(
    request: UiTranslateHtmlRequest,
  ): Promise<UiHtmlTranslationResult>;
  translatePlainTextMessage(
    request: UiTranslatePlainRequest,
  ): Promise<UiPlainTranslationResult>;
  cancelTranslation(jobId: string): Promise<boolean>;
  getTranslationCacheStats(): Promise<TranslationCacheStats>;
  clearTranslationCache(): Promise<TranslationCacheClearReport>;
  onProvidersChanged(
    handler: (event: UiTranslationProvidersResponse) => void,
  ): Promise<() => void>;
  onProgress(
    handler: (event: UiTranslationProgress) => void,
  ): Promise<() => void>;
}
