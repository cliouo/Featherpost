import {FakeSettings} from './fakeSettings';
import {FakeMailbox} from './fakeMailbox';
import { FakeCompose } from './fakeCompose';
import type {
  FolderIdentity, MailBridge, MessageIdentity, OpenIntent, UiDocument, UiFolder, UiFrontendInfo, UiLink,
  UiMessage, UiPage, UiReadingPolicy, UiReadingPolicyPatch, UiSnapshot, UiSummary,
} from '../src/bridge/types';
import { BridgeError } from '../src/bridge/errors';
import { BRAND } from '../src/brand';

export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

export const online: UiSnapshot = {
  frontend_id: BRAND.bundledFrontendId,
  phase: 'online',
  attempt: null,
  retry_in_ms: null,
  revision: 1,
  connection_epoch: 1,
  version: '0.1.0',
  accounts: [{ account: 'demo', state: 'online', detail: '' }],
  principal: `frontend:${BRAND.bundledFrontendId}`,
  identity_failure: null,
  partial: false,
  compat: 'compatible',
};

export function folder(name: string, role = '', unseen = 0): UiFolder {
  return { account: 'demo', name, display_name: name, role, present: true, total: 1, unseen };
}

export function summary(folderName: string, uid: string, subject = uid): UiSummary {
  return {
    account: 'demo', folder: folderName, uid, sender: 'a@b', recipient: 'me@b', subject, snippet: '',
    received_at: 100, date_at: 100, sort_at: 100, has_attachments: false, seen: true, flagged: false,
    answered: false, deleted: false, thread_id: 0,
  };
}

export function message(folderName: string, uid: string, extra: Partial<UiMessage> = {}): UiMessage {
  return {
    ...summary(folderName, uid), cc: '', reply_to: '', body: `body ${uid}`, has_html: false, html_bytes: 0, attachments: [],
    remote_content: { decision: 'block', reason: 'default' }, ...extra,
  };
}

export function document(token: string, extra: Partial<UiDocument> = {}): UiDocument {
  return {
    token, url: `${BRAND.uriScheme}://localhost/doc/${token}`, has_html: true, truncated: false, remote_refs: 0,
    cid_resolved: 0, cid_missing: 0, dropped_refs: 0, allow_remote: false, links: 0, ...extra,
  };
}

export const noHtml: UiDocument = {
  token: null, url: null, has_html: false, truncated: false, remote_refs: 0, cid_resolved: 0, cid_missing: 0,
  dropped_refs: 0, allow_remote: false, links: 0,
};

/** 每个请求都可挂起，测试自己决定何时、以何顺序完成。 */
export class FakeBridge implements MailBridge {
  settings=new FakeSettings();
  compose=new FakeCompose();
  mailbox=new FakeMailbox();
  available = true;
  snapshotHandler: ((snapshot: UiSnapshot) => void) | null = null;
  intentHandler: ((intent: OpenIntent) => void) | null = null;
  folderRequests: Array<{ account: string; d: ReturnType<typeof deferred<UiFolder[]>> }> = [];
  pageRequests: Array<{ folder: FolderIdentity; offset: number; d: ReturnType<typeof deferred<UiPage>> }> = [];
  detailRequests: Array<{ identity: MessageIdentity; d: ReturnType<typeof deferred<UiMessage>> }> = [];
  refreshes = 0;
  prepareRequests: Array<{ identity: MessageIdentity; allowRemote: boolean; d: ReturnType<typeof deferred<UiDocument>> }> = [];
  released: string[] = [];
  linkHandler: ((link: UiLink) => void) | null = null;
  opened: Array<{ token: string; index: number }> = [];
  saveRequests: Array<{ identity: MessageIdentity; partIndex: number; d: ReturnType<typeof deferred<string | null>> }> = [];
  policy: UiReadingPolicy = { remote_content: 'block', trusted_senders: [], persisted: true };
  policyPatches: UiReadingPolicyPatch[] = [];

  async snapshot() {
    return online;
  }
  async subscribe(handler: (snapshot: UiSnapshot) => void) {
    this.snapshotHandler = handler;
    return () => {
      this.snapshotHandler = null;
    };
  }
  async onIntent(handler: (intent: OpenIntent) => void) {
    this.intentHandler = handler;
    return () => {
      this.intentHandler = null;
    };
  }
  folders(account: string) {
    const d = deferred<UiFolder[]>();
    this.folderRequests.push({ account, d });
    return d.promise;
  }
  messages(folder: FolderIdentity, offset: number) {
    const d = deferred<UiPage>();
    this.pageRequests.push({ folder, offset, d });
    return d.promise;
  }
  message(identity: MessageIdentity) {
    const d = deferred<UiMessage>();
    this.detailRequests.push({ identity, d });
    return d.promise;
  }
  async refresh() {
    this.refreshes++;
  }
  async frontendInfo(): Promise<UiFrontendInfo> {
    return { id: BRAND.bundledFrontendId, name: `${BRAND.productName} UI`, version: '0.1.0', lifecycle: 'independent', capabilities: ['inbox'], permissions: ['mailbox_write'], is_default: false, default_frontend: 'builtin-egui' };
  }
  prepareHtml(identity: MessageIdentity, allowRemote: boolean) {
    const d = deferred<UiDocument>();
    this.prepareRequests.push({ identity, allowRemote, d });
    return d.promise;
  }
  async releaseHtml(token: string) {
    this.released.push(token);
    return true;
  }
  async onLink(handler: (link: UiLink) => void) {
    this.linkHandler = handler;
    return () => {
      this.linkHandler = null;
    };
  }
  async openLink(token: string, index: number) {
    this.opened.push({ token, index });
  }
  saveAttachment(identity: MessageIdentity, partIndex: number) {
    const d = deferred<string | null>();
    this.saveRequests.push({ identity, partIndex, d });
    return d.promise;
  }
  async readingPolicy() {
    return this.policy;
  }
  async setReadingPolicy(patch: UiReadingPolicyPatch) {
    this.policyPatches.push(patch);
    const trusted = this.policy.trusted_senders
      .filter((entry) => !(patch.untrust ?? []).includes(entry.value))
      .concat((patch.trust ?? []).map((value) => ({ value: value.toLowerCase(), kind: value.includes('@') ? 'address' : 'domain', added_at: 1 })));
    this.policy = { ...this.policy, remote_content: patch.remote_content ?? this.policy.remote_content, trusted_senders: trusted };
    return this.policy;
  }
}

export const offlineError = () => new BridgeError({ code: 'offline', message: '后台连接不可用，正在自动重试。' });
export const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
