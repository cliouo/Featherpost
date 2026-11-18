import type { MessageIdentity, UiError } from "../bridge/types";
export type ReplyMode = "reply" | "reply_all" | "forward";
export interface DraftAttachment {
  upload_id?: string;
  account: string;
  folder: string;
  uid: string;
  part_index: number;
  filename: string;
  mime_type: string;
  size: number;
  stored: boolean;
}
export type AttachmentInput =
  | { upload_id: string }
  | { account: string; folder: string; uid: string; part_index: number };
export interface Draft {
  id: string;
  account: string;
  from: string;
  to: string[];
  cc: string[];
  bcc: string[];
  subject: string;
  body: string;
  has_html: boolean;
  attachments: DraftAttachment[];
  version: number;
  updated_at: number;
  send_state: "editing" | "failed" | "sending" | "unknown" | "sent";
  sent_archive: boolean | null;
}
export interface DraftSummary {
  id: string;
  account: string;
  subject: string;
  snippet: string;
  to: string[];
  version: number;
  updated_at: number;
  attachment_count: number;
}
export interface Patch {
  account?: string;
  from?: string;
  to?: string[];
  cc?: string[];
  bcc?: string[];
  subject?: string;
  body?: string;
  body_html?: string;
  attachments?: AttachmentInput[];
}
export interface Sender {
  account: string;
  address: string;
  label: string;
  available: boolean;
  simple: boolean;
}
export interface PickedFile {
  token: string;
  filename: string;
  size: number;
}
export type PickResult = { Ok: PickedFile } | { Err: UiError };
export interface Progress {
  token: string;
  offset: number;
  size: number;
}
export interface ComposeBridge {
  senders(): Promise<Sender[]>;
  drafts(offset?: number): Promise<{ drafts: DraftSummary[]; total: number }>;
  draft(id: string): Promise<Draft>;
  create(account: string, patch: Patch): Promise<Draft>;
  save(id: string, version: number, patch: Patch): Promise<Draft>;
  reply(identity: MessageIdentity, mode: ReplyMode): Promise<Draft>;
  send(id: string, version: number): Promise<Draft>;
  resolve(id: string, version: number): Promise<Draft>;
  discard(id: string, version: number): Promise<void>;
  pick(): Promise<PickResult[]>;
  upload(draftId: string, token: string): Promise<string>;
  release(token: string): Promise<void>;
  finish(uploadId: string, version: number): Promise<Draft>;
  cancel(uploadId: string): Promise<void>;
  onProgress(fn: (progress: Progress) => void): Promise<() => void>;
  onPicked(fn: (files: PickResult[]) => void): Promise<() => void>;
  onClose(fn: () => void): Promise<() => void>;
  close(): Promise<void>;
  ready(): Promise<void>;
}
export const attachmentInput = (a: DraftAttachment): AttachmentInput =>
  a.upload_id
    ? { upload_id: a.upload_id }
    : {
        account: a.account,
        folder: a.folder,
        uid: a.uid,
        part_index: a.part_index,
      };
