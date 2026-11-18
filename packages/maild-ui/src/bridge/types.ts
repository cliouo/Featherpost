import type { MailboxBridge } from '../mailbox/types';
import type { ComposeBridge } from '../compose/types';
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

export interface UiFrontendInfo {
  id: string;
  name: string;
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
  message: string;
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
}

export function isUiError(value: unknown): value is UiError {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as UiError).code === 'string' &&
    typeof (value as UiError).message === 'string'
  );
}
