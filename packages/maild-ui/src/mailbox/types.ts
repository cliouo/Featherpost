import type {
  MessageIdentity,
  UiSummary,
  UiMessage,
  UiPage,
} from "../bridge/types";
export interface MailboxQuery {
  /** Client-side scope. Studio expands it into ordinary single-account queries before IPC. */
  accounts?: string[];
  search?: boolean;
  account?: string;
  folder?: string;
  role?: string;
  seen?: boolean;
  flagged?: boolean;
  query?: string;
  from?: string;
  since?: number;
  until?: number;
  has_attachments?: boolean;
}
export interface Profile {
  id: string;
  display_name: string;
  address: string;
  enabled: boolean;
}
export interface ThreadPage {
  thread_id: number;
  messages: UiSummary[];
  total: number;
}
export interface IndexStatus {
  pending: number;
  done: boolean;
}
export interface Operation extends MessageIdentity {
  subject?:string|null;
  id: number;
  kind: string;
  state: string;
  attempts: number;
  created_at: number;
  updated_at: number;
  detail: string;
  needs_decision: boolean;
  target: string | null;
  flag: string | null;
  value: boolean | null;
}
export interface ActivityPage {
  ops: Operation[];
  has_more: boolean;
}
export interface WriteRequest {
  action: "mark" | "move" | "delete";
  account: string;
  folder: string;
  uids: string[];
  flag?: "seen" | "flagged";
  value?: boolean;
  target?: string;
}
export interface Receipt {
  queued: number;
  queued_uids?: string[];
  unchanged_uids?: string[];
  missing_uids?: string[];
  mode: string | null;
}
export interface ServerSearchOptions {
  since?: number;
  until?: number;
}
export interface MailboxBridge {
  profiles(): Promise<Profile[]>;
  query(query: MailboxQuery, offset: number): Promise<UiPage>;
  thread(identity: MessageIdentity, offset: number): Promise<ThreadPage>;
  locate(identity: MessageIdentity): Promise<UiMessage>;
  index(): Promise<IndexStatus>;
  write(request: WriteRequest): Promise<Receipt>;
  activity(account?: string, before?: number): Promise<ActivityPage>;
  resolve(id: number, resolution: "retry" | "discard"): Promise<Operation>;
  /**
   * 可选：敲醒后台立即检查新邮件（`ui_sync_mailbox`）。account 为空 = 全部账户。
   * 后台尚未提供时实现应静默降级；调用方随后照常刷新列表。
   */
  syncNow?(account?: string): Promise<void>;
  /** 可选：在服务器上搜索更早的邮件（本地缓存有界）。返回远端摘要，不写本地库。 */
  serverSearch?(
    account: string,
    folder: string,
    query: string,
    options?: ServerSearchOptions,
  ): Promise<UiSummary[]>;
  /** 可选：把一封服务器历史邮件拉回本地库并返回完整详情；服务器上已不存在时返回 null。 */
  fetchServerMessage?(account: string, folder: string, uid: string): Promise<UiMessage | null>;
}
export type SmartFolder = "inbox" | "all" | "unread" | "starred";
export type ListScope =
  | { kind: "smart"; name: SmartFolder; group?: { id: string; name: string; accounts: string[] } }
  | { kind: "folder"; account: string; folder: string; title: string };
export type Scope =
  | ListScope
  | { kind: "unread"; base: ListScope }
  | { kind: "search"; query: MailboxQuery };
export type MailAction =
  | { action: "mark"; flag: "seen" | "flagged"; value: boolean }
  | { action: "move"; target: string }
  | { action: "archive" }
  | { action: "delete" };
export interface BatchReport {
  requested: number;
  unchanged?: number;
  queued: number;
  unconfirmed: number;
  error: string | null;
}
/**
 * 邮箱模块请宿主装配方打开账户设置：`provider` = 预选服务商向导，`repair` = 直达账户管理修复某账户。
 * 由 main.tsx 接到设置控制器；未接时退回打开设置页。
 */
export type AccountSetup = (request: { provider?: string; repair?: string }) => void;
