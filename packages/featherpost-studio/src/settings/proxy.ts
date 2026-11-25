import { m } from "@featherpost/i18n/messages";

import type {
  AccountProfile,
  ProxyConfigResult,
  ProxyConfigUpdate,
  ProxyEndpointInfo,
  ProxyMode,
  ProxyProtocol,
  SystemProxySnapshot,
  TestProxyRequest,
  TestProxyResult,
} from "./types";

export const PROXY_PROTOCOLS: readonly ProxyProtocol[] = [
  "socks5h",
  "socks5",
  "http",
  "https",
];

export const PROXY_TEST_PRESETS: readonly string[] = [
  "imap.gmail.com:993",
  "1.1.1.1:443",
];

const MAX_HOST_BYTES = 253;
const MAX_USERNAME_BYTES = 255;
const MAX_PASSWORD_BYTES = 1024;
const MAX_BYPASS_ENTRIES = 256;
const MAX_BYPASS_ENTRY_BYTES = 255;

export interface ProxyDraft {
  mode: ProxyMode;
  protocol: ProxyProtocol;
  host: string;
  port: string;
  authEnabled: boolean;
  username: string;
  password: string;
  clearPassword: boolean;
  bypassText: string;
}

export interface ParsedProxyUrl {
  protocol: ProxyProtocol;
  host: string;
  port: string;
  username: string | null;
  password: string | null;
}

const textEncoder = new TextEncoder();

export function utf8Bytes(value: string): number {
  return textEncoder.encode(value).length;
}

export function normalizeProxyMode(mode: string | undefined): ProxyMode {
  if (mode === "direct" || mode === "custom" || mode === "system") return mode;
  return "system";
}

export function normalizeProxyProtocol(protocol: string | undefined): ProxyProtocol {
  const lower = protocol?.toLowerCase();
  if (lower === "socks5h" || lower === "socks5" || lower === "http" || lower === "https") {
    return lower;
  }
  return "socks5h";
}

export function defaultPortForProtocol(protocol: ProxyProtocol): number {
  switch (protocol) {
    case "http":
      return 80;
    case "https":
      return 443;
    case "socks5":
    case "socks5h":
      return 1080;
  }
}

export function draftFromProxyConfig(result: ProxyConfigResult): ProxyDraft {
  const custom = result.config.custom;
  return {
    mode: normalizeProxyMode(result.config.mode),
    protocol: normalizeProxyProtocol(custom?.protocol),
    host: custom?.host ?? "",
    port: custom?.port ? String(custom.port) : "",
    authEnabled: Boolean(custom?.username) || result.has_password,
    username: custom?.username ?? "",
    password: "",
    clearPassword: false,
    bypassText: (custom?.bypass ?? []).join("\n"),
  };
}

export function serializeProxyDraft(draft: ProxyDraft): string {
  return JSON.stringify(draft);
}

export function parseBypassText(raw: string): string[] {
  return raw
    .split(/[\r\n,;]+/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
}

function hasControlOrWhitespace(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code <= 0x20 || code === 0x7f || /\s/.test(value[i]!)) return true;
  }
  return false;
}

function hasControlChars(value: string): boolean {
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
}

function isLikelyIpv6(value: string): boolean {
  return value.includes(":") && /^[0-9a-fA-F:.]+$/.test(value) && value.indexOf(":") !== value.lastIndexOf(":");
}

function unbracketHost(raw: string): string | null {
  const trimmed = raw.trim();
  if (trimmed.startsWith("[") || trimmed.endsWith("]")) {
    if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return null;
    const inner = trimmed.slice(1, -1);
    return isLikelyIpv6(inner) ? inner : null;
  }
  return trimmed;
}

function isValidHost(raw: string): boolean {
  const host = unbracketHost(raw);
  if (!host) return false;
  if (utf8Bytes(host) > MAX_HOST_BYTES) return false;
  if (hasControlOrWhitespace(host)) return false;
  if (/[/?#\\@]/.test(host)) return false;
  if (host.includes(":") && !isLikelyIpv6(host)) return false;
  return true;
}

function parsePort(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const num = Number(trimmed);
  if (!Number.isInteger(num) || num < 1 || num > 65535) return null;
  return num;
}

/**
 * Parse a full proxy URL pasted into the host field, e.g.
 * `socks5h://127.0.0.1:7890`, `http://user:pass@proxy.example.com:8080`, `https://[::1]:443`.
 */
export function parseProxyUrl(raw: string): ParsedProxyUrl | null {
  const trimmed = raw.trim();
  const sep = trimmed.indexOf("://");
  if (sep <= 0) return null;
  const scheme = trimmed.slice(0, sep).toLowerCase();
  if (scheme !== "socks5h" && scheme !== "socks5" && scheme !== "http" && scheme !== "https") {
    return null;
  }
  if (hasControlChars(trimmed)) return null;
  const rest = trimmed.slice(sep + 3);
  const slash = rest.indexOf("/");
  let authority = rest;
  if (slash !== -1) {
    if (rest.slice(slash + 1).length > 0) return null;
    authority = rest.slice(0, slash);
  }
  if (!authority || authority.includes("?") || authority.includes("#")) return null;

  let username: string | null = null;
  let password: string | null = null;
  let hostport = authority;
  const at = authority.lastIndexOf("@");
  if (at !== -1) {
    const userinfo = authority.slice(0, at);
    hostport = authority.slice(at + 1);
    const colon = userinfo.indexOf(":");
    try {
      if (colon !== -1) {
        const u = decodeURIComponent(userinfo.slice(0, colon));
        const p = decodeURIComponent(userinfo.slice(colon + 1));
        username = u.length > 0 ? u : null;
        password = p.length > 0 ? p : null;
      } else {
        const u = decodeURIComponent(userinfo);
        username = u.length > 0 ? u : null;
      }
    } catch {
      return null;
    }
  }
  if (!hostport) return null;

  let host = "";
  let portNum = defaultPortForProtocol(scheme);
  if (hostport.startsWith("[")) {
    const close = hostport.indexOf("]");
    if (close === -1) return null;
    const ipv6 = hostport.slice(1, close);
    if (!isLikelyIpv6(ipv6)) return null;
    host = ipv6;
    const tail = hostport.slice(close + 1);
    if (tail.length > 0) {
      if (!tail.startsWith(":")) return null;
      const parsed = parsePort(tail.slice(1));
      if (parsed === null) return null;
      portNum = parsed;
    }
  } else if (isLikelyIpv6(hostport)) {
    host = hostport;
  } else {
    const colon = hostport.lastIndexOf(":");
    if (colon !== -1) {
      host = hostport.slice(0, colon);
      if (host.includes(":")) return null;
      const parsed = parsePort(hostport.slice(colon + 1));
      if (parsed === null) return null;
      portNum = parsed;
    } else {
      host = hostport;
    }
  }

  if (!isValidHost(host)) return null;
  return {
    protocol: scheme,
    host,
    port: String(portNum),
    username,
    password,
  };
}

export function validateProxyDraft(draft: ProxyDraft): string | null {
  if (draft.mode !== "custom") return null;
  if (!draft.host.trim()) return m.settings_proxy_error_host_required();
  if (!isValidHost(draft.host)) return m.settings_proxy_error_host_invalid();
  if (parsePort(draft.port) === null) return m.settings_proxy_error_port_invalid();

  if (draft.authEnabled) {
    const username = draft.username.trim();
    if (
      username.length > 0 &&
      (utf8Bytes(username) > MAX_USERNAME_BYTES || hasControlChars(username) || username.includes(":"))
    ) {
      return m.settings_proxy_error_username_invalid();
    }
    if (
      draft.password.length > 0 &&
      (utf8Bytes(draft.password) > MAX_PASSWORD_BYTES || /[\r\n\0]/.test(draft.password))
    ) {
      return m.settings_proxy_error_password_invalid();
    }
  }

  const bypass = parseBypassText(draft.bypassText);
  if (bypass.length > MAX_BYPASS_ENTRIES) return m.settings_proxy_error_bypass_too_many();
  for (const entry of bypass) {
    if (utf8Bytes(entry) > MAX_BYPASS_ENTRY_BYTES || hasControlOrWhitespace(entry)) {
      return m.settings_proxy_error_bypass_invalid();
    }
  }
  return null;
}

export function validateTestTarget(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return m.settings_proxy_error_target_required();
  if (hasControlOrWhitespace(trimmed)) return m.settings_proxy_error_target_invalid();
  if (trimmed.startsWith("[")) {
    const close = trimmed.indexOf("]");
    if (close === -1) return m.settings_proxy_error_target_invalid();
    const ipv6 = trimmed.slice(1, close);
    const tail = trimmed.slice(close + 1);
    if (!isLikelyIpv6(ipv6) || !tail.startsWith(":")) return m.settings_proxy_error_target_invalid();
    if (parsePort(tail.slice(1)) === null) return m.settings_proxy_error_target_invalid();
    return null;
  }
  const colon = trimmed.lastIndexOf(":");
  if (colon <= 0) return m.settings_proxy_error_target_invalid();
  const host = trimmed.slice(0, colon);
  const port = trimmed.slice(colon + 1);
  if (host.includes(":") || !isValidHost(host) || parsePort(port) === null) {
    return m.settings_proxy_error_target_invalid();
  }
  return null;
}

export function buildProxyUpdate(
  draft: ProxyDraft,
  hasSavedPassword: boolean,
): ProxyConfigUpdate {
  if (draft.mode !== "custom") {
    return { mode: draft.mode };
  }
  const clearPassword =
    (!draft.authEnabled && hasSavedPassword) ||
    (draft.authEnabled && draft.clearPassword && draft.password === "");
  const username = draft.authEnabled ? draft.username.trim() || null : null;
  const password =
    draft.authEnabled && !clearPassword && draft.password !== ""
      ? draft.password
      : undefined;
  const host = unbracketHost(draft.host) ?? draft.host.trim();
  return {
    mode: "custom",
    custom: {
      protocol: draft.protocol,
      host,
      port: Number(draft.port.trim()),
      username,
      ...(password !== undefined ? { password } : {}),
      ...(clearPassword ? { clear_password: true } : {}),
      bypass: parseBypassText(draft.bypassText),
    },
  };
}

export function buildTestProxyRequest(
  draft: ProxyDraft | null,
  dirty: boolean,
  hasSavedPassword: boolean,
  target: string | null,
): TestProxyRequest {
  const cleanTarget = target?.trim() || null;
  if (!dirty || !draft) {
    return cleanTarget ? { target: cleanTarget } : {};
  }
  const config = buildProxyUpdate(draft, hasSavedPassword);
  const useSavedPassword =
    draft.mode === "custom" &&
    draft.authEnabled &&
    hasSavedPassword &&
    !draft.clearPassword &&
    draft.password === "";
  return {
    config,
    use_saved_password: useSavedPassword,
    ...(cleanTarget ? { target: cleanTarget } : {}),
  };
}

export function formatHostPort(host: string, port: number): string {
  const clean = host.replace(/^\[/, "").replace(/\]$/, "");
  return clean.includes(":") ? `[${clean}]:${port}` : `${clean}:${port}`;
}

export function defaultImapTarget(profiles: AccountProfile[] | null): string | null {
  if (!profiles) return null;
  const match = profiles.find(
    (p) => p.enabled && p.incoming.kind === "imap" && p.incoming.server && p.incoming.port,
  );
  if (!match?.incoming.server || !match.incoming.port) return null;
  return formatHostPort(match.incoming.server, match.incoming.port);
}

export interface SystemProxyItem {
  label: string;
  endpoint: string;
}

export function systemProxyItems(snapshot: SystemProxySnapshot | null): SystemProxyItem[] {
  if (!snapshot) return [];
  const items: SystemProxyItem[] = [];
  const push = (label: string, ep: ProxyEndpointInfo | null | undefined) => {
    if (!ep?.host || !ep.port) return;
    items.push({ label, endpoint: formatHostPort(ep.host, ep.port) });
  };
  push("SOCKS5", snapshot.socks5);
  push("HTTPS", snapshot.https);
  push("HTTP", snapshot.http);
  return items;
}

const SOURCE_LABELS: Readonly<Record<string, () => string>> = {
  macos: m.settings_proxy_system_source_macos,
  windows: m.settings_proxy_system_source_windows,
  env: m.settings_proxy_system_source_env,
  none: m.settings_proxy_system_source_none,
};

export function systemSourceLabel(source: string): string {
  const lower = source.toLowerCase();
  const fn = Object.prototype.hasOwnProperty.call(SOURCE_LABELS, lower)
    ? SOURCE_LABELS[lower]
    : undefined;
  return fn ? fn() : source;
}

export function inferProxyFailureStage(result: TestProxyResult): string {
  if (result.stage === "proxy_connect" && result.detail) {
    if (/\bTLS\b/i.test(result.detail)) return "proxy_tls";
    if (/\bDNS\b/i.test(result.detail)) return "proxy_dns";
  }
  return result.stage || "unknown";
}

const PROXY_STAGE_LABELS: Readonly<Record<string, () => string>> = {
  proxy_dns: m.settings_proxy_stage_proxy_dns,
  proxy_connect: m.settings_proxy_stage_proxy_connect,
  proxy_tls: m.settings_proxy_stage_proxy_tls,
  proxy_auth: m.settings_proxy_stage_proxy_auth,
  proxy_tunnel: m.settings_proxy_stage_proxy_tunnel,
  target_tunnel: m.settings_proxy_stage_proxy_tunnel,
  target_tls: m.settings_proxy_stage_target_tls,
  tls: m.settings_proxy_stage_target_tls,
  connect: m.settings_proxy_stage_connect,
  dns: m.settings_proxy_stage_dns,
};

export function proxyStageLabel(stage: string): string {
  const fn = Object.prototype.hasOwnProperty.call(PROXY_STAGE_LABELS, stage)
    ? PROXY_STAGE_LABELS[stage]
    : undefined;
  return fn ? fn() : m.settings_proxy_stage_unknown();
}

const PROXY_STAGE_HINTS: Readonly<Record<string, () => string>> = {
  proxy_dns: m.settings_proxy_hint_proxy_dns,
  proxy_connect: m.settings_proxy_hint_proxy_connect,
  proxy_tls: m.settings_proxy_hint_proxy_tls,
  proxy_auth: m.settings_proxy_hint_proxy_auth,
  proxy_tunnel: m.settings_proxy_hint_proxy_tunnel,
  target_tunnel: m.settings_proxy_hint_proxy_tunnel,
  target_tls: m.settings_proxy_hint_target_tls,
  tls: m.settings_proxy_hint_target_tls,
  connect: m.settings_proxy_hint_connect,
  dns: m.settings_proxy_hint_connect,
};

export function proxyStageHint(stage: string): string {
  const fn = Object.prototype.hasOwnProperty.call(PROXY_STAGE_HINTS, stage)
    ? PROXY_STAGE_HINTS[stage]
    : undefined;
  return fn ? fn() : m.settings_proxy_hint_unknown();
}
