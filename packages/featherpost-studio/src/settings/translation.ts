import { m } from "@featherpost/i18n/messages";

import { BRIDGE_ERROR_REASONS } from "../bridge/reasons";
import {
  BUILTIN_OPENAI_PROVIDER_ID,
  type OpenAiConfigUpdate,
  type TranslationDisplayMode,
  type TranslationEndpointTransport,
  type TranslationPreferenceUpdate,
  type TranslationPreferenceView,
  type TranslationProbeStage,
  type UiTranslationProviderEntry,
} from "../bridge/translation";
import { uiLocale } from "../i18n";

export { BUILTIN_OPENAI_PROVIDER_ID };

export type OpenAiPresetId = "openai" | "deepseek" | "ollama" | "custom";

export interface OpenAiPreset {
  id: OpenAiPresetId;
  baseUrl: string;
  model: string;
}

export const OPENAI_PRESETS: readonly OpenAiPreset[] = [
  { id: "openai", baseUrl: "https://api.openai.com/v1", model: "gpt-4o-mini" },
  {
    id: "deepseek",
    baseUrl: "https://api.deepseek.com/v1",
    model: "deepseek-chat",
  },
  { id: "ollama", baseUrl: "http://127.0.0.1:11434/v1", model: "qwen2.5:7b" },
  { id: "custom", baseUrl: "", model: "" },
];

export const COMMON_LANGUAGE_TAGS: readonly string[] = [
  "zh-CN",
  "zh-TW",
  "en",
  "ja",
  "ko",
  "fr",
  "de",
  "es",
  "pt",
  "it",
  "nl",
  "ru",
  "ar",
];

export const TARGET_LANGUAGE_OPTIONS: readonly string[] = [
  "auto",
  ...COMMON_LANGUAGE_TAGS,
];

const MAX_BASE_URL_BYTES = 1024;
const MAX_MODEL_BYTES = 256;
const MAX_API_KEY_BYTES = 4096;
const MAX_IGNORED_LANGUAGES = 64;
const MAX_LANG_TAG_BYTES = 32;

const textEncoder = new TextEncoder();

function utf8Bytes(value: string): number {
  return textEncoder.encode(value).length;
}

function hasControlChars(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
}

function hasControlOrWhitespace(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code <= 0x20 || code === 0x7f || /\s/.test(value[i]!)) return true;
  }
  return false;
}

export interface TranslationDraft {
  activeProviderId: string;
  displayMode: TranslationDisplayMode;
  targetLang: string;
  detectBannerEnabled: boolean;
  autoTranslate: boolean;
  autoTranslateProviderId: string | null;
  ignoredLanguages: string[];
  openaiPreset: OpenAiPresetId;
  openaiBaseUrl: string;
  openaiModel: string;
  openaiApiKey: string;
  openaiClearApiKey: boolean;
  openaiAllowPlaintextLan: boolean;
}

export function normalizePresetId(raw: string | null | undefined): OpenAiPresetId {
  switch (raw?.trim().toLowerCase()) {
    case "openai":
      return "openai";
    case "deepseek":
      return "deepseek";
    case "ollama":
      return "ollama";
    case "custom":
      return "custom";
    default:
      return "custom";
  }
}

function inferPresetFromUrl(baseUrl: string): OpenAiPresetId {
  const clean = baseUrl.trim().replace(/\/+$/, "").toLowerCase();
  if (!clean) return "openai";
  for (const preset of OPENAI_PRESETS) {
    if (preset.baseUrl && preset.baseUrl.toLowerCase() === clean) {
      return preset.id;
    }
  }
  return "custom";
}

export function draftFromTranslationPreference(
  pref: TranslationPreferenceView,
): TranslationDraft {
  const openai = pref.openai_compatible ?? null;
  const preset = openai
    ? openai.preset
      ? normalizePresetId(openai.preset)
      : inferPresetFromUrl(openai.base_url)
    : "openai";
  return {
    activeProviderId: pref.active_provider_id,
    displayMode:
      pref.display_mode === "translated_only" ? "translated_only" : "bilingual",
    targetLang: pref.target_lang || "auto",
    detectBannerEnabled: pref.detect_banner_enabled,
    autoTranslate: pref.auto_translate,
    autoTranslateProviderId: pref.auto_translate
      ? (pref.auto_translate_provider_id ?? (pref.active_provider_id || null))
      : null,
    ignoredLanguages: [...pref.ignored_languages],
    openaiPreset: preset,
    openaiBaseUrl: openai?.base_url ?? "",
    openaiModel: openai?.model ?? "",
    // NEVER pre-fill stored credentials in the form draft.
    openaiApiKey: "",
    openaiClearApiKey: false,
    openaiAllowPlaintextLan: openai?.allow_plaintext_lan ?? false,
  };
}

export function serializeTranslationDraft(draft: TranslationDraft): string {
  return JSON.stringify(draft);
}

export function applyOpenAiPreset(
  draft: TranslationDraft,
  presetId: OpenAiPresetId,
): TranslationDraft {
  const preset = OPENAI_PRESETS.find((item) => item.id === presetId);
  if (!preset || presetId === "custom") {
    return {
      ...draft,
      openaiPreset: "custom",
    };
  }
  return {
    ...draft,
    openaiPreset: preset.id,
    openaiBaseUrl: preset.baseUrl,
    openaiModel: preset.model,
    openaiClearApiKey: false,
    openaiAllowPlaintextLan: false,
  };
}

export type EndpointInspection =
  | {
      ok: true;
      canonicalKey: string;
      transport: TranslationEndpointTransport;
      allowsKeyless: boolean;
      isPlaintextLan: boolean;
    }
  | {
      ok: false;
      reason:
        | "empty"
        | "too_long"
        | "invalid_syntax"
        | "scheme_invalid"
        | "userinfo_forbidden"
        | "query_or_fragment_forbidden"
        | "traversal_forbidden"
        | "forbidden_ip"
        | "public_http_forbidden";
    };

function parseIpv4(host: string): [number, number, number, number] | null {
  const parts = host.split(".");
  if (parts.length !== 4) return null;
  const nums: number[] = [];
  for (const part of parts) {
    if (!/^\d+$/.test(part) || (part.length > 1 && part.startsWith("0"))) {
      return null;
    }
    const n = Number(part);
    if (!Number.isInteger(n) || n < 0 || n > 255) return null;
    nums.push(n);
  }
  return [nums[0]!, nums[1]!, nums[2]!, nums[3]!];
}

function isIpv6Literal(host: string): boolean {
  return host.includes(":") && /^[0-9a-f:.]+$/i.test(host);
}

function isLoopbackHost(host: string): boolean {
  if (host === "localhost" || host.endsWith(".localhost")) return true;
  if (host === "::1" || host === "0:0:0:0:0:0:0:1") return true;
  const v4 = parseIpv4(host);
  if (v4) return v4[0] === 127;
  if (host.startsWith("::ffff:")) {
    const mapped = parseIpv4(host.slice("::ffff:".length));
    if (mapped) return mapped[0] === 127;
  }
  return false;
}

function isForbiddenIpv4(octets: [number, number, number, number]): boolean {
  const [a, b, c, d] = octets;
  if (a === 0) return true;
  if (a === 255 && b === 255 && c === 255 && d === 255) return true;
  if (a >= 224 && a <= 239) return true;
  return false;
}

function isPublicIpv4(octets: [number, number, number, number]): boolean {
  const [a, b, c] = octets;
  if (a === 0 || a === 10 || a === 127 || a >= 224) return false;
  if (a === 100 && b >= 64 && b <= 127) return false;
  if (a === 169 && b === 254) return false;
  if (a === 172 && b >= 16 && b <= 31) return false;
  if (a === 192 && b === 0 && c === 0) return false;
  if (a === 192 && b === 0 && c === 2) return false;
  if (a === 192 && b === 168) return false;
  if (a === 198 && (b === 18 || b === 19)) return false;
  if (a === 198 && b === 51 && c === 100) return false;
  if (a === 203 && b === 0 && c === 113) return false;
  return true;
}

function classifyHost(host: string): "loopback" | "lan" | "public" | "forbidden" {
  if (isLoopbackHost(host)) return "loopback";
  const v4 = parseIpv4(host);
  if (v4) {
    if (isForbiddenIpv4(v4)) return "forbidden";
    return isPublicIpv4(v4) ? "public" : "lan";
  }
  if (isIpv6Literal(host)) {
    const lower = host.toLowerCase();
    if (lower === "::" || lower === "0:0:0:0:0:0:0:0" || lower.startsWith("ff")) {
      return "forbidden";
    }
    if (lower.startsWith("::ffff:")) {
      const mapped = parseIpv4(lower.slice("::ffff:".length));
      if (mapped) {
        if (isForbiddenIpv4(mapped)) return "forbidden";
        return isPublicIpv4(mapped) ? "public" : "lan";
      }
    }
    if (
      lower.startsWith("fc") ||
      lower.startsWith("fd") ||
      lower.startsWith("fe8") ||
      lower.startsWith("fe9") ||
      lower.startsWith("fea") ||
      lower.startsWith("feb") ||
      lower.startsWith("fec") ||
      lower.startsWith("fed") ||
      lower.startsWith("fee") ||
      lower.startsWith("fef") ||
      lower.startsWith("2001:db8") ||
      lower.startsWith("2001:0db8")
    ) {
      return "lan";
    }
    return "public";
  }
  if (!host.includes(".")) return "lan";
  for (const suffix of [
    ".local",
    ".lan",
    ".internal",
    ".localdomain",
    ".home.arpa",
  ]) {
    if (host.endsWith(suffix)) return "lan";
  }
  return "public";
}

function normalizeHost(rawHostname: string): string {
  const trimmed = rawHostname.trim();
  const unbracketed =
    trimmed.startsWith("[") && trimmed.endsWith("]")
      ? trimmed.slice(1, -1)
      : trimmed;
  return unbracketed.replace(/\.+$/, "").toLowerCase();
}

/**
 * Mirrors `crates/maild-translate/src/endpoint.rs::NormalizedServiceIdentity::parse` so the
 * Settings UI accurately reflects normalized-endpoint API key binding and LAN HTTP notices
 * before hitting the backend.
 */
export function inspectTranslationEndpoint(rawBaseUrl: string): EndpointInspection {
  const trimmed = rawBaseUrl.trim();
  if (!trimmed) return { ok: false, reason: "empty" };
  if (utf8Bytes(trimmed) > MAX_BASE_URL_BYTES) {
    return { ok: false, reason: "too_long" };
  }
  if (hasControlOrWhitespace(trimmed)) {
    return { ok: false, reason: "invalid_syntax" };
  }

  const schemeIdx = trimmed.indexOf("://");
  if (schemeIdx <= 0) return { ok: false, reason: "invalid_syntax" };
  const afterScheme = trimmed.slice(schemeIdx + 3);
  const slashIdx = afterScheme.indexOf("/");
  if (slashIdx !== -1) {
    const rawTail = afterScheme.slice(slashIdx);
    const rawPath = rawTail.split(/[?#]/)[0] ?? "";
    const rawLower = rawPath.toLowerCase();
    if (
      rawLower.includes("%2e") ||
      rawLower.includes("%2f") ||
      rawLower.includes("\\") ||
      rawPath.split("/").some((seg) => seg === "." || seg === "..")
    ) {
      return { ok: false, reason: "traversal_forbidden" };
    }
  }

  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return { ok: false, reason: "invalid_syntax" };
  }

  const scheme = parsed.protocol.replace(/:$/, "").toLowerCase();
  if (scheme !== "http" && scheme !== "https") {
    return { ok: false, reason: "scheme_invalid" };
  }
  if (parsed.username !== "" || parsed.password !== "") {
    return { ok: false, reason: "userinfo_forbidden" };
  }
  if (trimmed.includes("?") || trimmed.includes("#")) {
    return { ok: false, reason: "query_or_fragment_forbidden" };
  }

  const host = normalizeHost(parsed.hostname);
  if (!host) return { ok: false, reason: "invalid_syntax" };

  const port = parsed.port ? Number(parsed.port) : scheme === "https" ? 443 : 80;
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return { ok: false, reason: "invalid_syntax" };
  }

  const hostClass = classifyHost(host);
  if (hostClass === "forbidden") {
    return { ok: false, reason: "forbidden_ip" };
  }

  let transport: TranslationEndpointTransport;
  if (hostClass === "loopback") {
    transport = scheme === "https" ? "loopback_https" : "loopback_http";
  } else if (hostClass === "lan") {
    transport = scheme === "https" ? "lan_https" : "lan_http";
  } else {
    if (scheme !== "https") {
      return { ok: false, reason: "public_http_forbidden" };
    }
    transport = "public_https";
  }

  const segments = parsed.pathname.split("/").filter((seg) => seg.length > 0);
  if (
    segments.length >= 2 &&
    segments[segments.length - 2]!.toLowerCase() === "chat" &&
    segments[segments.length - 1]!.toLowerCase() === "completions"
  ) {
    segments.splice(segments.length - 2, 2);
  }
  const apiRootPath = segments.length === 0 ? "" : `/${segments.join("/")}`;
  const hostFmt = host.includes(":") ? `[${host}]` : host;
  const canonicalKey = `${scheme}://${hostFmt}:${port}${apiRootPath}`;

  return {
    ok: true,
    canonicalKey,
    transport,
    allowsKeyless: transport === "loopback_http" || transport === "loopback_https",
    isPlaintextLan: transport === "lan_http",
  };
}

export function savedBoundIdentityOf(
  pref: TranslationPreferenceView | null,
): string | null {
  const openai = pref?.openai_compatible;
  if (!openai?.has_api_key) return null;
  return openai.bound_service_identity ?? openai.normalized_identity ?? null;
}

export function canReuseSavedApiKey(
  draft: TranslationDraft,
  pref: TranslationPreferenceView | null,
): boolean {
  const bound = savedBoundIdentityOf(pref);
  if (!bound || draft.openaiClearApiKey || draft.openaiApiKey.trim() !== "") {
    return false;
  }
  const inspected = inspectTranslationEndpoint(draft.openaiBaseUrl);
  return inspected.ok && inspected.canonicalKey === bound;
}

export function requiresApiKeyRebind(
  draft: TranslationDraft,
  pref: TranslationPreferenceView | null,
): boolean {
  const bound = savedBoundIdentityOf(pref);
  if (!bound || draft.openaiApiKey.trim() !== "") return false;
  const inspected = inspectTranslationEndpoint(draft.openaiBaseUrl);
  return inspected.ok && !inspected.allowsKeyless && inspected.canonicalKey !== bound;
}

export function validateLanguageTag(raw: string): string | null {
  const trimmed = raw.trim().replace(/_/g, "-");
  if (!trimmed || utf8Bytes(trimmed) > MAX_LANG_TAG_BYTES) return null;
  if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(trimmed)) return null;
  if (trimmed.includes("--")) return null;
  return trimmed;
}

export function resolveProbeTargetLang(targetLang: string): string {
  const clean = targetLang.trim();
  if (!clean || clean.toLowerCase() === "auto") {
    return uiLocale();
  }
  return clean;
}

export function validateOpenAiDraft(
  draft: TranslationDraft,
  pref: TranslationPreferenceView | null,
): string | null {
  const trimmedKey = draft.openaiApiKey.trim();
  if (draft.openaiClearApiKey && trimmedKey.length > 0) {
    return m.settings_translation_error_clear_and_set_key();
  }

  const inspected = inspectTranslationEndpoint(draft.openaiBaseUrl);
  if (!inspected.ok) {
    if (inspected.reason === "empty") {
      return m.settings_translation_error_base_url_required();
    }
    if (inspected.reason === "public_http_forbidden") {
      return m.settings_translation_error_public_https_required();
    }
    return m.settings_translation_error_base_url_invalid();
  }

  if (inspected.isPlaintextLan && !draft.openaiAllowPlaintextLan) {
    return m.settings_translation_error_plaintext_lan_confirm();
  }

  const model = draft.openaiModel.trim();
  if (!model || utf8Bytes(model) > MAX_MODEL_BYTES || hasControlChars(model)) {
    return m.settings_translation_error_model_required();
  }

  if (trimmedKey.length > 0) {
    if (utf8Bytes(trimmedKey) > MAX_API_KEY_BYTES || hasControlChars(trimmedKey)) {
      return m.settings_translation_error_api_key_invalid();
    }
  } else if (!inspected.allowsKeyless) {
    if (draft.openaiClearApiKey) {
      return m.settings_translation_error_api_key_required();
    }
    if (requiresApiKeyRebind(draft, pref)) {
      return m.common_bridge_reason_translation_credential_rebind_required();
    }
    if (!canReuseSavedApiKey(draft, pref)) {
      return m.settings_translation_error_api_key_required();
    }
  }

  return null;
}

export function validateTranslationDraft(
  draft: TranslationDraft,
  pref: TranslationPreferenceView | null,
): string | null {
  if (!validateLanguageTag(draft.targetLang)) {
    return m.settings_translation_error_target_lang_invalid();
  }
  if (draft.ignoredLanguages.length > MAX_IGNORED_LANGUAGES) {
    return m.settings_translation_error_ignored_too_many();
  }
  for (const tag of draft.ignoredLanguages) {
    if (!validateLanguageTag(tag)) {
      return m.settings_translation_error_ignored_invalid();
    }
  }
  if (draft.activeProviderId === BUILTIN_OPENAI_PROVIDER_ID) {
    return validateOpenAiDraft(draft, pref);
  }
  return null;
}

export function buildOpenAiUpdate(draft: TranslationDraft): OpenAiConfigUpdate {
  const trimmedKey = draft.openaiApiKey.trim();
  const clearApiKey = draft.openaiClearApiKey && trimmedKey.length === 0;
  return {
    preset: draft.openaiPreset,
    base_url: draft.openaiBaseUrl.trim(),
    model: draft.openaiModel.trim(),
    api_key: !clearApiKey && trimmedKey.length > 0 ? trimmedKey : null,
    clear_api_key: clearApiKey,
    allow_plaintext_lan: draft.openaiAllowPlaintextLan,
  };
}

export function buildTranslationUpdate(
  draft: TranslationDraft,
  pref: TranslationPreferenceView | null,
): TranslationPreferenceUpdate {
  const activeId = draft.activeProviderId.trim();
  const autoEnabled = draft.autoTranslate && activeId.length > 0;
  const targetLang = validateLanguageTag(draft.targetLang) ?? "auto";
  return {
    expected_revision: pref ? pref.revision : null,
    active_provider_id: activeId,
    display_mode: draft.displayMode,
    target_lang: targetLang,
    detect_banner_enabled: draft.detectBannerEnabled,
    auto_translate: autoEnabled,
    auto_translate_provider_id: autoEnabled ? activeId : null,
    ignored_languages: [...draft.ignoredLanguages],
    openai_compatible:
      activeId === BUILTIN_OPENAI_PROVIDER_ID ? buildOpenAiUpdate(draft) : null,
  };
}

export type TranslationProviderBadgeStatus =
  | "ready"
  | "unconfigured"
  | "proxy_pending"
  | "online"
  | "disabled"
  | "stopped"
  | "unregistered";

export function translationProviderStatus(
  entry: UiTranslationProviderEntry,
): TranslationProviderBadgeStatus {
  if (entry.kind === "builtin") {
    if (!entry.configured) return "unconfigured";
    return entry.proxy_ready ? "ready" : "proxy_pending";
  }
  if (!entry.enabled) return "disabled";
  if (entry.registered) return "online";
  if (entry.state === "stopped") return "stopped";
  return "unregistered";
}

const PROVIDER_STATUS_LABELS: Readonly<
  Record<TranslationProviderBadgeStatus, () => string>
> = {
  ready: m.settings_translation_status_ready,
  unconfigured: m.settings_translation_status_unconfigured,
  proxy_pending: m.settings_translation_status_proxy_pending,
  online: m.settings_translation_status_online,
  disabled: m.settings_translation_status_disabled,
  stopped: m.settings_translation_status_stopped,
  unregistered: m.settings_translation_status_unregistered,
};

export function translationProviderStatusLabel(
  status: TranslationProviderBadgeStatus,
): string {
  return PROVIDER_STATUS_LABELS[status]();
}

const PROBE_STAGE_LABELS: Readonly<Record<TranslationProbeStage, () => string>> =
  {
    config: m.settings_translation_probe_stage_config,
    proxy: m.settings_translation_probe_stage_proxy,
    network: m.settings_translation_probe_stage_network,
    tls: m.settings_translation_probe_stage_tls,
    auth: m.settings_translation_probe_stage_auth,
    upstream: m.settings_translation_probe_stage_upstream,
    response: m.settings_translation_probe_stage_response,
    done: m.settings_translation_probe_stage_done,
  };

export function translationProbeStageLabel(stage: TranslationProbeStage): string {
  return Object.prototype.hasOwnProperty.call(PROBE_STAGE_LABELS, stage)
    ? PROBE_STAGE_LABELS[stage]()
    : m.settings_translation_probe_stage_upstream();
}

export function translationProbeReasonText(reason: string): string {
  const clean = reason.trim();
  if (clean === "ok") return m.settings_translation_probe_reason_ok();
  const mapped = Object.prototype.hasOwnProperty.call(BRIDGE_ERROR_REASONS, clean)
    ? BRIDGE_ERROR_REASONS[clean]
    : undefined;
  return mapped ? mapped() : m.common_bridge_error_failed();
}

const LANGUAGE_LABELS: Readonly<Record<string, () => string>> = {
  auto: m.settings_translation_lang_auto,
  zh: m.settings_translation_lang_zh_cn,
  "zh-hans": m.settings_translation_lang_zh_cn,
  "zh-cn": m.settings_translation_lang_zh_cn,
  "zh-hant": m.settings_translation_lang_zh_tw,
  "zh-tw": m.settings_translation_lang_zh_tw,
  en: m.settings_translation_lang_en,
  ja: m.settings_translation_lang_ja,
  ko: m.settings_translation_lang_ko,
  fr: m.settings_translation_lang_fr,
  de: m.settings_translation_lang_de,
  es: m.settings_translation_lang_es,
  pt: m.settings_translation_lang_pt,
  it: m.settings_translation_lang_it,
  nl: m.settings_translation_lang_nl,
  ru: m.settings_translation_lang_ru,
  uk: m.settings_translation_lang_uk,
  ar: m.settings_translation_lang_ar,
  th: m.settings_translation_lang_th,
  el: m.settings_translation_lang_el,
  he: m.settings_translation_lang_he,
  hi: m.settings_translation_lang_hi,
};

export function languageTagLabel(tag: string): string {
  const key = tag.trim().replace(/_/g, "-").toLowerCase();
  const fn = Object.prototype.hasOwnProperty.call(LANGUAGE_LABELS, key)
    ? LANGUAGE_LABELS[key]
    : undefined;
  return fn ? fn() : tag;
}

export function formatBytes(bytes: number): string {
  const safe = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  const fmt = new Intl.NumberFormat(uiLocale(), { maximumFractionDigits: 1 });
  if (safe < 1024) return `${Math.round(safe)} B`;
  if (safe < 1024 * 1024) return `${fmt.format(safe / 1024)} KB`;
  return `${fmt.format(safe / (1024 * 1024))} MB`;
}
