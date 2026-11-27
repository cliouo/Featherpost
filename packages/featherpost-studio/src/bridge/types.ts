import type { MailboxBridge, ServerSearchOptions } from '../mailbox/types';
import type { ComposeBridge } from '../compose/types';
import type { TranslationBridge } from './translation';
export type {
  DetectedLanguage,
  OpenAiConfigUpdate,
  OpenAiConfigView,
  PlainTextBlockView,
  TranslationBridge,
  TranslationCacheClearReport,
  TranslationCacheStats,
  TranslationDisplayMode,
  TranslationEndpointTransport,
  TranslationPreferenceUpdate,
  TranslationPreferenceView,
  TranslationProbeResult,
  TranslationProbeStage,
  TranslationProviderKind,
  UiDetectLanguageRequest,
  UiHtmlTranslationResult,
  UiLanguageDetection,
  UiPlainTranslationResult,
  UiTestTranslationProviderRequest,
  UiTranslateHtmlRequest,
  UiTranslatePlainRequest,
  UiTranslationProgress,
  UiTranslationProviderEntry,
  UiTranslationProvidersResponse,
} from './translation';
import type {
  CustomProxyConfig,
  CustomProxyUpdate,
  ProxyConfig,
  ProxyConfigResult,
  ProxyConfigUpdate,
  ProxyEndpointInfo,
  ProxyMode,
  ProxyPreferenceEvent,
  ProxyProtocol,
  SystemProxySnapshot,
  TestProxyRequest,
  TestProxyResult,
} from '../settings/types';
export type {
  CustomProxyConfig,
  CustomProxyUpdate,
  ProxyConfig,
  ProxyConfigResult,
  ProxyConfigUpdate,
  ProxyEndpointInfo,
  ProxyMode,
  ProxyPreferenceEvent,
  ProxyProtocol,
  ServerSearchOptions,
  SystemProxySnapshot,
  TestProxyRequest,
  TestProxyResult,
};
// 与 crates/maild-ui/src/bridge.rs 的 DTO 一一对应（snake_case 原样）。这是 webview 侧知道的
// 全部协议：没有 endpoint、token、原始错误文本，也没有 body_html——HTML 只以隔离文档的 URL 形式
// 交给 iframe（ADR-0033），宿主脚本从不持有邮件 HTML 字符串。

export type ConnectionPhase =
  | 'disconnected'
  | 'connecting'
  | 'synchronizing'
  | 'online'
  | 'retry_waiting'
  | 'stopped';

export interface UiAccount {
  account: string;
  state: string;
  detail: string;
  /** 离线原因的稳定 code（ADR-0048 L6）；老壳或老后台没有。按 `accountReason` 成文。 */
  detail_code?: string | null;
  /** 服务器或系统原文（不含后台写的前缀），作为次要信息原样显示。 */
  server_detail?: string;
}

export interface UiSnapshot {
  frontend_id: string;
  phase: ConnectionPhase;
  attempt: number | null;
  retry_in_ms: number | null;
  revision: number;
  connection_epoch: number;
  version: string | null;
  accounts: UiAccount[];
  principal: string | null;
  identity_failure: string | null;
  /** 身份绑定失败的稳定原因（ADR-0048 决策 3）；老壳没有。按 `identityFailureText` 成文。 */
  identity_failure_code?: "not_installed" | "rejected" | "principal_mismatch" | "unavailable" | null;
  partial: boolean;
  /** ADR-0016 版本握手结论；不一致时桥只放行只读请求。 */
  compat: UiCompat;
}

export type UiCompat = 'unknown' | 'compatible' | 'kernel_outdated' | 'app_outdated';

/** 版本不一致时写入被桥挡下；界面同步禁用写入口。 */
export function compatBlocksWrites(snapshot: Pick<UiSnapshot, 'compat'>): boolean {
  return snapshot.compat === 'kernel_outdated' || snapshot.compat === 'app_outdated';
}

export interface UiFolder {
  account: string;
  name: string;
  display_name: string;
  role: string;
  is_primary?: boolean;
  is_search_inbox?: boolean;
  present: boolean;
  total: number;
  unseen: number;
}

export interface MessageIdentity {
  account: string;
  folder: string;
  uid: string;
}

export interface UiSummary extends MessageIdentity {
  sender: string;
  recipient: string;
  subject: string;
  snippet: string;
  received_at: number;
  date_at: number | null;
  sort_at: number;
  has_attachments: boolean;
  seen: boolean;
  flagged: boolean;
  answered: boolean;
  deleted: boolean;
  thread_id: number;
}

export interface UiAttachment {
  part_index: number;
  filename: string;
  mime_type: string;
  content_id: string;
  inline: boolean;
  size: number;
  /** 字节已落库、可保存（ADR-0032）。为假时显示「尚未保存此附件」，不给按钮。 */
  stored: boolean;
}

/** 内核对这封信远程内容的结论（ADR-0032 决策 5）。 */
export interface UiRemoteContent {
  decision: 'block' | 'allow' | string;
  reason: 'default' | 'trusted_address' | 'trusted_domain' | 'allow_all' | string;
}

export interface UiMessage extends UiSummary {
  cc: string;
  reply_to: string;
  body: string;
  has_html: boolean;
  html_bytes: number;
  attachments: UiAttachment[];
  remote_content: UiRemoteContent;
}

/** `ui_prepare_html` 的结果（ADR-0033）。`token`/`url` 为 null = 这封信没有 HTML。 */
export interface UiDocument {
  token: string | null;
  url: string | null;
  has_html: boolean;
  truncated: boolean;
  remote_refs: number;
  cid_resolved: number;
  cid_missing: number;
  dropped_refs: number;
  allow_remote: boolean;
  links: number;
}

export type LinkKind = 'http' | 'https' | 'mailto' | 'other';

/** 邮件里被点击的链接：事件 `maild://link` 的载荷。webview 只拿到分类与显示用文本。 */
export interface UiLink {
  token: string;
  index: number;
  href: string;
  kind: LinkKind;
  host: string;
}

export interface UiTrustedSender {
  value: string;
  kind: 'address' | 'domain' | string;
  added_at: number;
}

export interface UiReadingPolicy {
  remote_content: 'block' | 'allow' | string;
  trusted_senders: UiTrustedSender[];
  persisted: boolean;
}

export interface UiReadingPolicyPatch {
  remote_content?: 'block' | 'allow';
  trust?: string[];
  untrust?: string[];
}

export interface UiPage {
  messages: UiSummary[];
  total: number;
}

export interface UiContactSuggestion {
  address: string;
  name?: string | null;
}

export interface UiFrontendInfo {
  id: string;
  name: string;
  /** ADR-0048 决策 5：manifest 兜底说明与 `l10n`；渲染时经 `packageText` 协商。 */
  description?: string | null;
  l10n?: Record<string, import('../i18n').PackageL10nEntry>;
  version: string;
  lifecycle: string;
  capabilities: string[];
  permissions: string[];
  is_default: boolean;
  default_frontend: string;
}

export type UiErrorCode = 'offline' | 'denied' | 'not_found' | 'invalid' | 'failed' | 'conflict' | 'limit' | 'locked' | 'incompatible';
export interface UiError {
  code: UiErrorCode;
  /**
   * ADR-0048 决策 3：Rust 侧的稳定原因码，按它取当前界面语言的文案（`bridge/reasons.ts`）。
   * webview 自己构造的错误没有它，`message` 已是当前语言。
   */
  reason?: string;
  /** Rust 侧是中文兜底原文；webview 侧构造时是已成文的提示。 */
  message: string;
}

/** ADR-0048：daemon 解析后的有序语言偏好；`null` = 尚未取到或后台不提供，回落基础语言。 */
export interface UiLocale {
  effective: string[] | null;
}

export interface UiLocalePreference {
  mode: 'system' | 'explicit';
  languages: string[];
  effective: string[];
  /** 偏好文件停摆：读到的是缺省偏好，写会被拒绝。 */
  persisted: boolean;
}

export type UiLocaleUpdate = { mode: 'system' } | { mode: 'explicit'; languages: string[] };

/** 语言偏好（ADR-0048）。读写都经桥；协商在 `@featherpost/i18n`。 */
export interface LocaleBridge {
  /** 壳手里当前的偏好列表，不发请求。 */
  current(): Promise<UiLocale>;
  /** `maild://locale`：偏好列表变了。 */
  subscribe(handler: (locale: UiLocale) => void): Promise<() => void>;
  /** 设置页显示的模式与显式列表（发请求；老后台抛 `locale_unsupported`）。 */
  preference(): Promise<UiLocalePreference>;
  /** 需要 `settings_write`。 */
  setPreference(update: UiLocaleUpdate): Promise<UiLocalePreference>;
}

/** 全局代理偏好（不含明文密码回传）。 */
export interface ProxyBridge {
  config(): Promise<ProxyConfigResult>;
  setConfig(update: ProxyConfigUpdate): Promise<ProxyConfigResult>;
  test(request: TestProxyRequest): Promise<TestProxyResult>;
  /** `maild://proxy`：代理配置或系统代理变化。 */
  subscribe(handler: (event: ProxyPreferenceEvent) => void): Promise<() => void>;
}

export type OpenIntent =
  | { route: 'inbox'; account?: string }
  | { route: 'message'; account: string; uid: string; folder?: string }
  | { route: 'compose' }
  | { route: 'settings' }
  | { route: 'background' }
  | { route: string };

export interface FolderIdentity {
  account: string;
  folder: string;
}

/** 宿主与控制器依赖的桥接口；生产实现是 Tauri command，测试用假实现。 */
export interface MailBridge {
  settings: import("../settings/types").SettingsBridge;
  compose: ComposeBridge;
  mailbox: MailboxBridge;
  readonly available: boolean;
  snapshot(): Promise<UiSnapshot>;
  subscribe(handler: (snapshot: UiSnapshot) => void): Promise<() => void>;
  onIntent(handler: (intent: OpenIntent) => void): Promise<() => void>;
  folders(account: string): Promise<UiFolder[]>;
  messages(folder: FolderIdentity, offset: number, limit: number): Promise<UiPage>;
  message(identity: MessageIdentity): Promise<UiMessage>;
  searchContacts?(account: string, query?: string, limit?: number): Promise<UiContactSuggestion[]>;
  refresh(): Promise<void>;
  frontendInfo(): Promise<UiFrontendInfo>;
  // —— ADR-0033：HTML 原位阅读 ——
  prepareHtml(identity: MessageIdentity, allowRemote: boolean): Promise<UiDocument>;
  releaseHtml(token: string): Promise<boolean>;
  onLink(handler: (link: UiLink) => void): Promise<() => void>;
  openLink(token: string, index: number): Promise<void>;
  /** 用户取消返回 null；成功返回写入路径。 */
  saveAttachment(identity: MessageIdentity, partIndex: number): Promise<string | null>;
  readingPolicy(): Promise<UiReadingPolicy>;
  setReadingPolicy(patch: UiReadingPolicyPatch): Promise<UiReadingPolicy>;
  /** 可选：经宿主打开固定的 https 帮助页（`ui_open_external_url`）；不用于邮件正文链接。 */
  openExternalUrl?(url: string): Promise<void>;
  /** 可选：语言偏好（ADR-0048）。没有它时界面停在基础语言，设置页不显示语言选择。 */
  locale?: LocaleBridge;
  /** 可选：全局代理偏好通道。 */
  proxy?: ProxyBridge;
  /** 可选：受控邮件翻译桥（ADR-0050）。 */
  translation?: TranslationBridge;
}

export function isUiError(value: unknown): value is UiError {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as UiError).code === 'string' &&
    typeof (value as UiError).message === 'string'
  );
}
