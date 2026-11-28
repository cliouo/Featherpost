import { m } from "@featherpost/i18n/messages";

import { BridgeError, bridgeErrorText } from "../bridge/errors";
import {
  BUILTIN_OPENAI_PROVIDER_ID,
  type DetectedLanguage,
  type TranslationBridge,
  type TranslationDisplayMode,
  type TranslationPreferenceUpdate,
  type TranslationPreferenceView,
  type UiHtmlTranslationResult,
  type UiLanguageDetection,
  type UiPlainTranslationResult,
  type UiTranslationProgress,
  type UiTranslationProviderEntry,
  type UiTranslationProvidersResponse,
} from "../bridge/translation";
import type { MailBridge, MessageIdentity, UiMessage, UiSnapshot } from "../bridge/types";
import { Store } from "../host/store";
import { uiLocale } from "../i18n";
import { MAIL } from "../views";
import { messageKey } from "./controller";
import type { DocumentState } from "./document";

export type TranslationPhase =
  | "idle"
  | "translating"
  | "translated"
  | "original"
  | "error";

export type ProviderUnavailableReason =
  | "bridge_unavailable"
  | "config_unhealthy"
  | "unconfigured"
  | "proxy_pending"
  | "external_unsupported"
  | "disabled"
  | "offline"
  | "unregistered";

export type ProviderAvailability =
  | {
      ready: true;
      providerId: string;
      providerGeneration: number;
      providerVariantRevision: string;
    }
  | {
      ready: false;
      reason: ProviderUnavailableReason;
      providerId: string;
    };

export interface TranslationProgressView {
  jobId: string | null;
  completedBatches: number;
  totalBatches: number;
}

export interface TranslationErrorView {
  reason: string;
  message: string;
  action: "retry" | "open_settings";
}

export interface TranslationState {
  available: boolean;
  /** Currently bound message key (`account\0folder\0uid`). */
  messageKey: string | null;
  contentKind: "html" | "plain" | null;
  /** Live HTML document token when `contentKind === 'html'`; null for plain text. */
  documentToken: string | null;
  preference: TranslationPreferenceView | null;
  providers: readonly UiTranslationProviderEntry[];
  providersCatalogGeneration: number;
  externalSupported: boolean;
  availability: ProviderAvailability;
  phase: TranslationPhase;
  detection: UiLanguageDetection | null;
  bannerDismissed: boolean;
  progress: TranslationProgressView | null;
  htmlResult: UiHtmlTranslationResult | null;
  plainResult: UiPlainTranslationResult | null;
  error: TranslationErrorView | null;
  ignoringLanguage: boolean;
}

interface BoundTarget {
  kind: "html" | "plain";
  messageKey: string;
  identity: MessageIdentity;
  token: string | null;
}

interface ActiveAttempt {
  attemptId: number;
  targetEpoch: number;
  configEpoch: number;
  messageKey: string;
  contentKind: "html" | "plain";
  documentToken: string | null;
  settingsRev: number;
  providerId: string;
  providerGeneration: number;
  targetLang: string;
  jobId: string | null;
  cancelled: boolean;
  previousPhase: "idle" | "translated" | "original";
  previousHtmlResult: UiHtmlTranslationResult | null;
  previousPlainResult: UiPlainTranslationResult | null;
}

const SETTINGS_ERROR_REASONS = new Set([
  "translation_provider_not_configured",
  "translation_invalid_config",
  "translation_credential_rebind_required",
  "translation_endpoint_rejected",
  "translation_config_unavailable",
  "translation_auth_failed",
  "translation_provider_unavailable",
  "translation_external_unsupported",
  "unconfigured",
  "config_unhealthy",
  "disabled",
  "offline",
  "unregistered",
  "external_unsupported",
]);

/** Resolve `"auto"` target language to the active UI locale tag. */
export function resolveEffectiveTargetLanguage(targetLang: string | null | undefined): string {
  const clean = targetLang?.trim() ?? "";
  if (!clean || clean.toLowerCase() === "auto") {
    return uiLocale();
  }
  return clean;
}

function normalizeLangTag(raw: string): string {
  return raw.trim().replace(/_/g, "-").toLowerCase();
}

/** Mirrors Rust `DetectedLanguage::matches_target` for `"auto"` resolved UI locales. */
export function detectedLanguageMatchesTarget(
  detected: DetectedLanguage,
  targetLang: string,
): boolean {
  if (detected === "unknown") return false;
  const norm = normalizeLangTag(resolveEffectiveTargetLanguage(targetLang));
  if (!norm) return false;
  if (detected === "zh-CN") {
    return (
      norm === "zh" ||
      norm === "zh-cn" ||
      norm === "zh-sg" ||
      norm === "zh-hans" ||
      norm === "zh-hans-cn" ||
      norm === "zh-hans-sg"
    );
  }
  if (detected === "zh-TW") {
    return (
      norm === "zh" ||
      norm === "zh-tw" ||
      norm === "zh-hk" ||
      norm === "zh-mo" ||
      norm === "zh-hant" ||
      norm === "zh-hant-tw" ||
      norm === "zh-hant-hk"
    );
  }
  const primary = norm.split("-")[0] ?? "";
  return primary === detected.toLowerCase();
}

/** Mirrors Rust `DetectedLanguage::is_ignored` so client-side state stays aligned immediately. */
export function detectedLanguageIsIgnored(
  detected: DetectedLanguage,
  ignoredLanguages: readonly string[],
): boolean {
  if (detected === "unknown") return false;
  for (const entry of ignoredLanguages) {
    const norm = normalizeLangTag(entry);
    if (!norm) continue;
    if (detected === "zh-CN" || detected === "zh-TW") {
      if (norm === "zh") return true;
      if (detectedLanguageMatchesTarget(detected, norm)) return true;
    } else {
      const primary = norm.split("-")[0] ?? "";
      if (primary === detected.toLowerCase()) return true;
    }
  }
  return false;
}

export function evaluateProviderAvailability(
  available: boolean,
  preference: TranslationPreferenceView | null,
  providers: readonly UiTranslationProviderEntry[],
  externalSupported: boolean,
  daemonOnline: boolean,
): ProviderAvailability {
  if (!available) {
    return { ready: false, reason: "bridge_unavailable", providerId: "" };
  }
  if (!preference) {
    return { ready: false, reason: "unconfigured", providerId: "" };
  }
  if (!preference.state_healthy) {
    return {
      ready: false,
      reason: "config_unhealthy",
      providerId: preference.active_provider_id,
    };
  }
  const providerId = preference.active_provider_id.trim();
  if (!providerId) {
    return { ready: false, reason: "unconfigured", providerId: "" };
  }

  const entry = providers.find((p) => p.id === providerId) ?? null;

  if (providerId === BUILTIN_OPENAI_PROVIDER_ID) {
    if (!preference.openai_compatible) {
      return { ready: false, reason: "unconfigured", providerId };
    }
    if (entry && !entry.configured) {
      return { ready: false, reason: "unconfigured", providerId };
    }
    if (entry && !entry.proxy_ready) {
      return { ready: false, reason: "proxy_pending", providerId };
    }
    return {
      ready: true,
      providerId,
      providerGeneration: entry?.generation ?? 1,
      providerVariantRevision: entry?.variant_revision ?? "",
    };
  }

  // External plugin provider — NEVER silently fall back to built-in.
  if (!daemonOnline) {
    return { ready: false, reason: "offline", providerId };
  }
  if (!externalSupported) {
    return { ready: false, reason: "external_unsupported", providerId };
  }
  if (!entry) {
    return { ready: false, reason: "offline", providerId };
  }
  if (!entry.enabled) {
    return { ready: false, reason: "disabled", providerId };
  }
  if (!entry.registered) {
    if (entry.state === "stopped") {
      return { ready: false, reason: "offline", providerId };
    }
    return { ready: false, reason: "unregistered", providerId };
  }
  return {
    ready: true,
    providerId,
    providerGeneration: entry.generation,
    providerVariantRevision: entry.variant_revision,
  };
}

function unavailabilityError(reason: ProviderUnavailableReason): TranslationErrorView {
  switch (reason) {
    case "config_unhealthy":
      return {
        reason: "translation_config_unavailable",
        message: m.common_bridge_reason_translation_config_unavailable(),
        action: "open_settings",
      };
    case "proxy_pending":
      return {
        reason: "translation_proxy_not_ready",
        message: m.common_bridge_reason_translation_proxy_not_ready(),
        action: "retry",
      };
    case "external_unsupported":
      return {
        reason: "translation_external_unsupported",
        message: m.common_bridge_reason_translation_external_unsupported(),
        action: "open_settings",
      };
    case "disabled":
      return {
        reason: "disabled",
        message: m.studio_reader_translate_error_disabled(),
        action: "open_settings",
      };
    case "offline":
      return {
        reason: "offline",
        message: m.studio_reader_translate_error_offline(),
        action: "open_settings",
      };
    case "unregistered":
      return {
        reason: "unregistered",
        message: m.studio_reader_translate_error_unregistered(),
        action: "open_settings",
      };
    case "unconfigured":
    case "bridge_unavailable":
    default:
      return {
        reason: "translation_provider_not_configured",
        message: m.common_bridge_reason_translation_provider_not_configured(),
        action: "open_settings",
      };
  }
}

function classifyError(error: unknown): TranslationErrorView {
  if (error instanceof BridgeError) {
    const reason = error.detail.reason ?? error.detail.code;
    return {
      reason,
      message: bridgeErrorText(error.detail),
      action: SETTINGS_ERROR_REASONS.has(reason) ? "open_settings" : "retry",
    };
  }
  return {
    reason: "failed",
    message: m.common_bridge_error_failed(),
    action: "retry",
  };
}

/**
 * Semantic fingerprint of everything that affects the actual translated text.
 * Deliberately EXCLUDES `display_mode`, `detect_banner_enabled`, `auto_translate`, and
 * `ignored_languages` so changing ONLY `display_mode` in Settings redraws in-place with
 * ZERO outbound translation requests.
 */
function semanticConfigFingerprint(
  preference: TranslationPreferenceView | null,
  availability: ProviderAvailability,
): string {
  if (!preference || !preference.state_healthy || !preference.active_provider_id) {
    return "unconfigured";
  }
  const effectiveTarget = normalizeLangTag(
    resolveEffectiveTargetLanguage(preference.target_lang),
  );
  const openaiPart =
    preference.active_provider_id === BUILTIN_OPENAI_PROVIDER_ID &&
    preference.openai_compatible
      ? `${preference.openai_compatible.normalized_identity}|${preference.openai_compatible.model}`
      : "";
  const providerPart = availability.ready
    ? `ready:${availability.providerGeneration}:${availability.providerVariantRevision}`
    : `unavail:${availability.reason}`;
  return `${preference.active_provider_id}|${effectiveTarget}|${openaiPart}|${providerPart}`;
}

export function shouldShowDetectionBanner(state: TranslationState): boolean {
  if (!state.available) return false;
  if (state.phase !== "idle" || state.bannerDismissed) return false;
  if (!state.availability.ready) return false;
  if (!state.preference?.state_healthy || !state.preference.detect_banner_enabled) {
    return false;
  }
  const det = state.detection;
  if (!det || !det.should_offer_banner) return false;
  if (det.language === "unknown") return false;
  if (det.matches_target || det.is_ignored) return false;
  if (detectedLanguageMatchesTarget(det.language, state.preference.target_lang)) {
    return false;
  }
  if (detectedLanguageIsIgnored(det.language, state.preference.ignored_languages)) {
    return false;
  }
  return true;
}

/**
 * Shared translation controller for ReadingPane toolbar button, header banner, and `⌘K` commands.
 *
 * Enforces strict race-safety across A→B→A switching, HTML→plain-text switching, same-message
 * HTML token rotation (remote-image allow/revoke), and late results after target/provider changes,
 * while guaranteeing zero outbound requests when only `display_mode` changes.
 */
export class TranslationController {
  readonly store: Store<TranslationState>;
  private readonly translation: TranslationBridge | undefined;
  private target: BoundTarget | null = null;
  private targetEpoch = 0;
  private configEpoch = 0;
  private attemptSeq = 0;
  private detectSeq = 0;
  private loadSeq = 0;
  private activeAttempt: ActiveAttempt | null = null;
  private cancelledTokens = new Set<string>();
  private cancelledJobs = new Set<string>();
  private pendingPlainCancel = false;
  private semanticFingerprint = "unconfigured";
  private daemonOnline = true;
  private initialized = false;
  private initialLoadPromise: Promise<void> | null = null;
  private autoTranslatedFor: string | null = null;
  private userOptedOutAutoFor: string | null = null;
  private stopProgress: (() => void) | null = null;
  private stopProviders: (() => void) | null = null;
  private started = false;

  constructor(bridge: MailBridge) {
    this.translation = bridge.translation;
    const available = Boolean(this.translation);
    this.store = new Store<TranslationState>({
      available,
      messageKey: null,
      contentKind: null,
      documentToken: null,
      preference: null,
      providers: [],
      providersCatalogGeneration: 0,
      externalSupported: true,
      availability: available
        ? { ready: false, reason: "unconfigured", providerId: "" }
        : { ready: false, reason: "bridge_unavailable", providerId: "" },
      phase: "idle",
      detection: null,
      bannerDismissed: false,
      progress: null,
      htmlResult: null,
      plainResult: null,
      error: null,
      ignoringLanguage: false,
    });
  }

  get available(): boolean {
    return Boolean(this.translation);
  }

  get displayMode(): TranslationDisplayMode {
    return this.store.getSnapshot().preference?.display_mode === "translated_only"
      ? "translated_only"
      : "bilingual";
  }

  async start(): Promise<void> {
    if (!this.translation || this.started) return;
    this.started = true;
    try {
      this.stopProgress = await this.translation.onProgress((event) => {
        this.handleProgress(event);
      });
    } catch {
      this.stopProgress = null;
    }
    try {
      this.stopProviders = await this.translation.onProvidersChanged((event) => {
        this.applyProvidersSnapshot(event);
      });
    } catch {
      this.stopProviders = null;
    }
    await this.refreshState();
  }

  stop(): void {
    this.cancelInFlight();
    this.targetEpoch += 1;
    this.configEpoch += 1;
    this.detectSeq += 1;
    this.loadSeq += 1;
    this.stopProgress?.();
    this.stopProgress = null;
    this.stopProviders?.();
    this.stopProviders = null;
    this.started = false;
    this.target = null;
  }

  /**
   * Bind to mailbox detail, document lifecycle, and optional host state (for refreshing settings
   * when returning from Settings › Translation or reconnecting to `maild`).
   */
  bindTo(
    mailboxStore: Store<{ detail: UiMessage | null }>,
    documentStore: Store<DocumentState>,
    hostState?: Store<{ activeViewId: string; snapshot: UiSnapshot }>,
  ): () => void {
    const syncTarget = () => {
      const detail = mailboxStore.getSnapshot().detail;
      const docState = documentStore.getSnapshot();
      this.updateTargetFrom(detail, docState);
    };

    let lastViewId = hostState?.getSnapshot().activeViewId ?? MAIL;
    let lastEpoch = hostState?.getSnapshot().snapshot.connection_epoch ?? -1;
    if (hostState) {
      this.daemonOnline = hostState.getSnapshot().snapshot.phase === "online";
    }

    const syncHost = () => {
      if (!hostState) return;
      const h = hostState.getSnapshot();
      const nextOnline = h.snapshot.phase === "online";
      const onlineChanged = nextOnline !== this.daemonOnline;
      this.daemonOnline = nextOnline;

      const viewChanged = h.activeViewId !== lastViewId;
      const epochChanged = h.snapshot.connection_epoch !== lastEpoch;
      lastViewId = h.activeViewId;
      lastEpoch = h.snapshot.connection_epoch;

      if (onlineChanged) {
        this.recomputeAvailability();
      }
      if ((viewChanged && h.activeViewId === MAIL) || epochChanged || (onlineChanged && nextOnline)) {
        void this.refreshState();
      }
    };

    syncTarget();
    const stopMailbox = mailboxStore.subscribe(syncTarget);
    const stopDocuments = documentStore.subscribe(syncTarget);
    const stopHost = hostState ? hostState.subscribe(syncHost) : () => {};
    return () => {
      stopMailbox();
      stopDocuments();
      stopHost();
    };
  }

  /** Refresh both preferences and providers from the bridge. */
  async refreshState(): Promise<void> {
    if (!this.translation) return;
    const seq = ++this.loadSeq;
    const work = (async () => {
      const [prefResult, provResult] = await Promise.allSettled([
        this.translation!.getTranslationPreferences(),
        this.translation!.listTranslationProviders(),
      ]);
      if (seq !== this.loadSeq) return;
      const nextPref =
        prefResult.status === "fulfilled"
          ? prefResult.value
          : this.store.getSnapshot().preference;
      const nextProv = provResult.status === "fulfilled" ? provResult.value : null;
      this.initialized = true;
      this.applyLoadedState(nextPref, nextProv);
    })();
    if (!this.initialized) {
      this.initialLoadPromise = work;
    }
    await work;
  }

  /** Apply an already-saved preference directly (e.g., from Settings or tests). */
  applyPreference(preference: TranslationPreferenceView): void {
    this.initialized = true;
    this.applyLoadedState(preference, null);
  }

  /** Apply an incoming `maild://translation-providers` snapshot with monotonic generation guard. */
  applyProvidersSnapshot(response: UiTranslationProvidersResponse): void {
    const current = this.store.getSnapshot();
    if (
      current.providersCatalogGeneration > 0 &&
      response.catalog_generation < current.providersCatalogGeneration
    ) {
      return;
    }
    this.applyLoadedState(current.preference, response);
  }

  /**
   * Single 1-click toolbar button & `⌘K` `maild.mailbox.translate` entry point:
   * - Translating → cancels immediately.
   * - Translated → toggles back to original (retaining result in memory for instant restore).
   * - Original (with valid cached result) → restores translated view instantly with ZERO outbound request.
   * - Idle / Error → translates using Settings' display_mode & target_lang, OR if provider is
   *   unconfigured/offline/disabled/unregistered, shows clear error state and opens Settings › Translation.
   */
  async toggleTranslate(openTranslationSettings?: () => void): Promise<void> {
    if (!this.translation || !this.target) return;
    const state = this.store.getSnapshot();

    if (state.phase === "translating") {
      this.cancel();
      return;
    }
    if (state.phase === "translated") {
      this.showOriginal();
      return;
    }
    if (state.phase === "original" && this.hasValidCachedView(state)) {
      this.store.patch({ phase: "translated", error: null });
      return;
    }
    await this.translate(false, openTranslationSettings);
  }

  /** Toggle back to original text while retaining the in-memory translated view for instant restore. */
  showOriginal(): void {
    const state = this.store.getSnapshot();
    if (state.phase !== "translated") return;
    if (this.target) {
      this.userOptedOutAutoFor = this.targetIdentityKey(this.target);
    }
    this.store.patch({ phase: "original", error: null });
  }

  /** Force re-translation bypassing local cache (`force_refresh = true`). */
  async retranslate(openTranslationSettings?: () => void): Promise<void> {
    await this.translate(true, openTranslationSettings);
  }

  /** Trigger translation of the currently bound HTML document or plain-text message. */
  async translate(
    forceRefresh = false,
    openTranslationSettings?: () => void,
  ): Promise<void> {
    if (!this.translation || !this.target) return;

    if (!this.initialized) {
      await (this.initialLoadPromise ?? this.refreshState());
      if (!this.target) return;
    }

    const state = this.store.getSnapshot();
    if (!state.availability.ready) {
      const err = unavailabilityError(state.availability.reason);
      this.store.patch({
        phase: "error",
        progress: null,
        error: err,
      });
      if (err.action === "open_settings") {
        openTranslationSettings?.();
      }
      return;
    }

    // Reuse in-memory translated view when not force-refreshing and already available.
    if (!forceRefresh && state.phase === "original" && this.hasValidCachedView(state)) {
      this.store.patch({ phase: "translated", error: null });
      return;
    }

    const pref = state.preference!;
    const target = this.target;
    const availability = state.availability;
    const targetLang = resolveEffectiveTargetLanguage(pref.target_lang);

    // Cancel any previous in-flight attempt before starting a new one.
    this.cancelInFlight();

    const attempt: ActiveAttempt = {
      attemptId: ++this.attemptSeq,
      targetEpoch: this.targetEpoch,
      configEpoch: this.configEpoch,
      messageKey: target.messageKey,
      contentKind: target.kind,
      documentToken: target.token,
      settingsRev: pref.revision,
      providerId: availability.providerId,
      providerGeneration: availability.providerGeneration,
      targetLang,
      jobId: null,
      cancelled: false,
      previousPhase:
        state.phase === "translated" || state.phase === "original"
          ? state.phase
          : "idle",
      previousHtmlResult: state.htmlResult,
      previousPlainResult: state.plainResult,
    };
    this.activeAttempt = attempt;
    if (target.token) {
      this.cancelledTokens.delete(target.token);
    }
    if (target.kind === "plain") {
      this.pendingPlainCancel = false;
    }

    this.store.patch({
      phase: "translating",
      progress: { jobId: null, completedBatches: 0, totalBatches: 0 },
      error: null,
    });

    try {
      if (target.kind === "html" && target.token) {
        const result = await this.translation.translateHtmlDocument({
          token: target.token,
          target_lang: targetLang,
          provider_id: availability.providerId,
          force_refresh: forceRefresh,
        });
        if (!this.isResultValidForAttempt(attempt, result.job_id, result.provider_id, result.provider_generation, result.token)) {
          return;
        }
        this.activeAttempt = null;
        this.store.patch({
          phase: "translated",
          htmlResult: result,
          plainResult: null,
          progress: null,
          error: null,
        });
      } else {
        const result = await this.translation.translatePlainTextMessage({
          account: target.identity.account,
          folder: target.identity.folder,
          uid: target.identity.uid,
          target_lang: targetLang,
          provider_id: availability.providerId,
          force_refresh: forceRefresh,
        });
        if (!this.isResultValidForAttempt(attempt, result.job_id, result.provider_id, result.provider_generation, null)) {
          return;
        }
        this.activeAttempt = null;
        this.store.patch({
          phase: "translated",
          htmlResult: null,
          plainResult: result,
          progress: null,
          error: null,
        });
      }
    } catch (error) {
      if (!this.isAttemptStillActive(attempt)) return;
      this.activeAttempt = null;
      if (
        error instanceof BridgeError &&
        error.detail.reason === "translation_cancelled"
      ) {
        this.restoreAfterCancel(attempt);
        return;
      }
      this.store.patch({
        phase: "error",
        progress: null,
        error: classifyError(error),
      });
    }
  }

  /** Cancel the active in-flight translation immediately and notify Rust via `cancelTranslation`. */
  cancel(): void {
    const attempt = this.activeAttempt;
    if (!attempt) return;
    if (this.target) {
      this.userOptedOutAutoFor = this.targetIdentityKey(this.target);
    }
    this.cancelInFlight();
    this.restoreAfterCancel(attempt);
  }

  /** Dismiss the foreign-language detection banner or error banner for the current message. */
  dismissBanner(): void {
    const state = this.store.getSnapshot();
    if (state.phase === "error") {
      const nextPhase = this.hasValidCachedView(state) ? "original" : "idle";
      this.store.patch({ phase: nextPhase, error: null });
      return;
    }
    this.store.patch({ bannerDismissed: true });
  }

  /** Persist the detected language into `ignored_languages` and hide the banner immediately. */
  async ignoreDetectedLanguage(): Promise<void> {
    if (!this.translation) return;
    const state = this.store.getSnapshot();
    const pref = state.preference;
    const det = state.detection;
    if (
      !pref ||
      !pref.state_healthy ||
      !det ||
      det.language === "unknown" ||
      state.ignoringLanguage
    ) {
      return;
    }
    const langTag = det.language;
    if (detectedLanguageIsIgnored(langTag, pref.ignored_languages)) {
      this.store.patch({ bannerDismissed: true });
      return;
    }

    const epoch = this.targetEpoch;
    this.store.patch({ ignoringLanguage: true });
    try {
      const nextIgnored = [...pref.ignored_languages, langTag];
      const update: TranslationPreferenceUpdate = {
        expected_revision: pref.revision,
        active_provider_id: pref.active_provider_id,
        display_mode: pref.display_mode,
        target_lang: pref.target_lang,
        detect_banner_enabled: pref.detect_banner_enabled,
        auto_translate: pref.auto_translate,
        auto_translate_provider_id: pref.auto_translate
          ? (pref.auto_translate_provider_id ?? (pref.active_provider_id || null))
          : null,
        ignored_languages: nextIgnored,
        // Preserve stored OpenAI config & key untouched on disk.
        openai_compatible: null,
      };
      const saved = await this.translation.saveTranslationPreferences(update);
      this.applyLoadedState(saved, null);
      if (epoch === this.targetEpoch) {
        this.store.patch({
          bannerDismissed: true,
          ignoringLanguage: false,
        });
      }
    } catch {
      if (epoch === this.targetEpoch) {
        this.store.patch({ ignoringLanguage: false });
      }
    }
  }

  private updateTargetFrom(
    detail: UiMessage | null,
    docState: DocumentState,
  ): void {
    const next = this.resolveTarget(detail, docState);
    if (this.targetsEqual(this.target, next)) {
      return;
    }

    // Target changed (A→B, HTML→plain, token rotation, or cleared): cancel & invalidate immediately.
    this.cancelInFlight();
    this.targetEpoch += 1;
    this.detectSeq += 1;
    this.target = next;

    if (!next) {
      this.store.patch({
        messageKey: null,
        contentKind: null,
        documentToken: null,
        phase: "idle",
        detection: null,
        bannerDismissed: false,
        progress: null,
        htmlResult: null,
        plainResult: null,
        error: null,
        ignoringLanguage: false,
      });
      return;
    }

    this.store.patch({
      messageKey: next.messageKey,
      contentKind: next.kind,
      documentToken: next.token,
      phase: "idle",
      detection: null,
      bannerDismissed: false,
      progress: null,
      htmlResult: null,
      plainResult: null,
      error: null,
      ignoringLanguage: false,
    });

    void this.detectForCurrentTarget();
  }

  private resolveTarget(
    detail: UiMessage | null,
    docState: DocumentState,
  ): BoundTarget | null {
    if (!detail) return null;
    const key = messageKey(detail);
    const identity: MessageIdentity = {
      account: detail.account,
      folder: detail.folder,
      uid: detail.uid,
    };
    if (!detail.has_html) {
      return { kind: "plain", messageKey: key, identity, token: null };
    }
    if (docState.key !== key) return null;
    if (docState.status === "idle" || docState.status === "preparing") return null;
    if (docState.status === "ready" && docState.document?.token) {
      return {
        kind: "html",
        messageKey: key,
        identity,
        token: docState.document.token,
      };
    }
    // HTML prepare failed or document has no HTML → plain-text fallback.
    return { kind: "plain", messageKey: key, identity, token: null };
  }

  private targetsEqual(a: BoundTarget | null, b: BoundTarget | null): boolean {
    if (a === b) return true;
    if (!a || !b) return false;
    return (
      a.kind === b.kind &&
      a.messageKey === b.messageKey &&
      a.token === b.token
    );
  }

  private targetIdentityKey(target: BoundTarget): string {
    return `${target.kind}|${target.messageKey}|${target.token ?? ""}`;
  }

  private hasValidCachedView(state: TranslationState): boolean {
    if (!this.target) return false;
    if (this.target.kind === "html") {
      return Boolean(
        state.htmlResult && state.htmlResult.token === this.target.token,
      );
    }
    return Boolean(state.plainResult);
  }

  private recomputeAvailability(): void {
    const current = this.store.getSnapshot();
    this.applyLoadedState(current.preference, null);
  }

  private applyLoadedState(
    nextPref: TranslationPreferenceView | null,
    nextProvidersResp: UiTranslationProvidersResponse | null,
  ): void {
    const current = this.store.getSnapshot();
    const providers = nextProvidersResp
      ? nextProvidersResp.providers
      : current.providers;
    const catalogGeneration = nextProvidersResp
      ? nextProvidersResp.catalog_generation
      : current.providersCatalogGeneration;
    const externalSupported = nextProvidersResp
      ? nextProvidersResp.external_supported
      : current.externalSupported;

    const availability = evaluateProviderAvailability(
      this.available,
      nextPref,
      providers,
      externalSupported,
      this.daemonOnline,
    );
    const nextFingerprint = semanticConfigFingerprint(nextPref, availability);
    const semanticChanged = nextFingerprint !== this.semanticFingerprint;
    this.semanticFingerprint = nextFingerprint;

    if (semanticChanged) {
      // Provider, target language, endpoint/model, or provider generation changed:
      // cancel in-flight translation, discard late results, and clear stale translated views.
      this.cancelInFlight();
      this.configEpoch += 1;
      this.autoTranslatedFor = null;
      this.store.patch({
        preference: nextPref,
        providers,
        providersCatalogGeneration: catalogGeneration,
        externalSupported,
        availability,
        phase: "idle",
        detection: null,
        bannerDismissed: false,
        progress: null,
        htmlResult: null,
        plainResult: null,
        error: null,
      });
      if (this.target) {
        void this.detectForCurrentTarget();
      }
      return;
    }

    // Semantic translation config is UNCHANGED (e.g., ONLY `display_mode`, `detect_banner_enabled`,
    // `ignored_languages`, or `auto_translate` changed).
    // Preserve active/cached translated views in-place for ZERO-outbound redraw!
    const wasReady = current.availability.ready;
    this.store.patch({
      preference: nextPref,
      providers,
      providersCatalogGeneration: catalogGeneration,
      externalSupported,
      availability,
    });

    // If detection hasn't run yet for the current idle target (e.g., first load finished), run it.
    if (
      this.target &&
      current.phase === "idle" &&
      (!current.detection || !wasReady) &&
      availability.ready
    ) {
      void this.detectForCurrentTarget();
    }
  }

  private async detectForCurrentTarget(): Promise<void> {
    if (!this.translation || !this.target) return;
    if (!this.initialized) {
      await (this.initialLoadPromise ?? this.refreshState());
      if (!this.target) return;
    }

    const state = this.store.getSnapshot();
    const pref = state.preference;
    // Unconfigured / unhealthy / offline provider: hide detection banner completely (no noise).
    if (!pref || !pref.state_healthy || !state.availability.ready) {
      return;
    }
    const autoTranslateBound =
      pref.auto_translate &&
      pref.active_provider_id !== "" &&
      pref.auto_translate_provider_id === pref.active_provider_id;
    if (!pref.detect_banner_enabled && !autoTranslateBound) {
      return;
    }

    const target = this.target;
    const targetEpoch = this.targetEpoch;
    const configEpoch = this.configEpoch;
    const seq = ++this.detectSeq;

    try {
      const detection = await this.translation.detectEmailLanguage(
        target.kind === "html" && target.token
          ? { token: target.token }
          : {
              account: target.identity.account,
              folder: target.identity.folder,
              uid: target.identity.uid,
            },
      );
      if (
        seq !== this.detectSeq ||
        targetEpoch !== this.targetEpoch ||
        configEpoch !== this.configEpoch
      ) {
        return;
      }
      this.store.patch({ detection });

      // Re-check latest state before auto-translating.
      const latest = this.store.getSnapshot();
      const latestPref = latest.preference;
      const targetId = this.targetIdentityKey(target);
      const autoKey = `${targetId}|${this.semanticFingerprint}`;
      const canAutoTranslate =
        latest.phase === "idle" &&
        latest.availability.ready &&
        Boolean(latestPref?.state_healthy) &&
        Boolean(latestPref?.auto_translate) &&
        latestPref!.active_provider_id !== "" &&
        latestPref!.auto_translate_provider_id === latestPref!.active_provider_id &&
        detection.language !== "unknown" &&
        !detection.matches_target &&
        !detection.is_ignored &&
        !detectedLanguageMatchesTarget(detection.language, latestPref!.target_lang) &&
        !detectedLanguageIsIgnored(detection.language, latestPref!.ignored_languages) &&
        this.autoTranslatedFor !== autoKey &&
        this.userOptedOutAutoFor !== targetId;

      if (canAutoTranslate) {
        this.autoTranslatedFor = autoKey;
        void this.translate(false);
      }
    } catch {
      // Quietly ignore detection errors; manual 1-click translation remains available.
    }
  }

  private handleProgress(event: UiTranslationProgress): void {
    const attempt = this.activeAttempt;

    // If this progress belongs to a cancelled token or previously cancelled plain-text job,
    // immediately tell the backend to cancel that job_id if it's still running.
    if (event.status === "running") {
      const matchesCancelledToken =
        Boolean(event.token) && this.cancelledTokens.has(event.token!);
      const matchesCancelledPlain =
        !event.token && this.pendingPlainCancel && (!attempt || attempt.contentKind !== "plain");
      if (matchesCancelledToken || matchesCancelledPlain) {
        if (!this.cancelledJobs.has(event.job_id)) {
          this.cancelledJobs.add(event.job_id);
          void this.translation?.cancelTranslation(event.job_id).catch(() => {});
        }
        return;
      }
    }

    if (!attempt || attempt.cancelled) return;
    if (
      this.targetEpoch !== attempt.targetEpoch ||
      this.configEpoch !== attempt.configEpoch
    ) {
      return;
    }

    // Strict content-kind & token matching.
    if (attempt.contentKind === "html") {
      if (!event.token || event.token !== attempt.documentToken) return;
    } else {
      if (event.token) return;
    }

    // Strict job_id binding once assigned.
    if (attempt.jobId !== null && event.job_id !== attempt.jobId) {
      return;
    }
    if (attempt.jobId === null && event.job_id) {
      attempt.jobId = event.job_id;
    }

    if (event.status === "running") {
      this.store.patch({
        progress: {
          jobId: event.job_id,
          completedBatches: event.completed_batches,
          totalBatches: event.total_batches,
        },
      });
    }
  }

  private isAttemptStillActive(attempt: ActiveAttempt): boolean {
    return (
      this.activeAttempt === attempt &&
      !attempt.cancelled &&
      this.targetEpoch === attempt.targetEpoch &&
      this.configEpoch === attempt.configEpoch &&
      this.target !== null &&
      this.target.messageKey === attempt.messageKey &&
      this.target.kind === attempt.contentKind &&
      this.target.token === attempt.documentToken
    );
  }

  private isResultValidForAttempt(
    attempt: ActiveAttempt,
    resultJobId: string,
    resultProviderId: string,
    resultProviderGeneration: number,
    resultToken: string | null,
  ): boolean {
    if (!this.isAttemptStillActive(attempt)) return false;
    if (attempt.contentKind === "html" && resultToken !== attempt.documentToken) {
      return false;
    }
    if (attempt.jobId !== null && resultJobId !== attempt.jobId) {
      return false;
    }
    if (resultProviderId !== attempt.providerId) {
      return false;
    }
    if (
      attempt.providerGeneration > 0 &&
      resultProviderGeneration > 0 &&
      resultProviderGeneration !== attempt.providerGeneration
    ) {
      return false;
    }
    return true;
  }

  private cancelInFlight(): void {
    const attempt = this.activeAttempt;
    if (!attempt) return;
    attempt.cancelled = true;
    this.activeAttempt = null;
    if (attempt.jobId) {
      this.cancelledJobs.add(attempt.jobId);
      void this.translation?.cancelTranslation(attempt.jobId).catch(() => {});
    } else if (attempt.documentToken) {
      this.cancelledTokens.add(attempt.documentToken);
    } else if (attempt.contentKind === "plain") {
      this.pendingPlainCancel = true;
    }
  }

  private restoreAfterCancel(attempt: ActiveAttempt): void {
    if (
      this.targetEpoch !== attempt.targetEpoch ||
      this.configEpoch !== attempt.configEpoch
    ) {
      return;
    }
    const hadPrevious =
      attempt.contentKind === "html"
        ? Boolean(
            attempt.previousHtmlResult &&
              attempt.previousHtmlResult.token === attempt.documentToken,
          )
        : Boolean(attempt.previousPlainResult);
    this.store.patch({
      phase: hadPrevious ? attempt.previousPhase : "idle",
      htmlResult: hadPrevious ? attempt.previousHtmlResult : null,
      plainResult: hadPrevious ? attempt.previousPlainResult : null,
      progress: null,
      error: null,
    });
  }
}
