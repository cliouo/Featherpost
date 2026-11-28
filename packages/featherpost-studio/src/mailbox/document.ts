import { m } from '@featherpost/i18n/messages';
import type { MailBridge, MessageIdentity, UiDocument, UiLink, UiMessage } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import { Store } from '../host/store';
import { RequestGeneration, messageKey } from './controller';

/**
 * 正文如何呈现的真值表（ADR-0033 决策 7）。`html` 只在文档准备成功后成立；其余都是纯文本。
 * `note` 是可信 UI 里的注记，措辞中性：内核不记录一封信原本有没有 HTML。
 */
export type BodyPresentation =
  | { mode: 'html'; url: string; token: string }
  | { mode: 'text'; note: string | null }
  | { mode: 'loading' };

export interface DocumentState {
  /** 当前文档属于哪封信；换信后旧文档立即作废。 */
  key: string | null;
  status: 'idle' | 'preparing' | 'ready' | 'failed';
  document: UiDocument | null;
  error: string | null;
  /** 本次会话内点过「加载本封」的邮件（按消息键），不持久、不进内核（决策 5）。 */
  allowedOnce: readonly string[];
  /** 等待用户决定的链接（链接确认面板）。 */
  pendingLink: UiLink | null;
}

const MAX_ALLOWED_ONCE = 64;

export function describeDocumentError(error: unknown): string {
  switch (errorCode(error)) {
    case 'offline':
      return m.mailbox_document_error_offline();
    case 'denied':
      return m.mailbox_document_error_denied();
    case 'not_found':
      return m.mailbox_document_error_not_found();
    default:
      return m.mailbox_document_error_failed();
  }
}

/** 给横幅用：内核结论 + 本封是否已被会话内放行 → 该以什么身份准备文档。 */
export function shouldAllowRemote(detail: UiMessage, allowedOnce: readonly string[]): boolean {
  if (detail.remote_content.decision === 'allow') return true;
  return allowedOnce.includes(messageKey(detail));
}

export function presentation(detail: UiMessage | null, state: DocumentState): BodyPresentation {
  if (!detail) return { mode: 'loading' };
  const truncatedNote = m.mailbox_document_truncated();
  if (!detail.has_html) {
    return { mode: 'text', note: m.mailbox_document_no_html() };
  }
  if (state.key !== messageKey(detail)) return { mode: 'loading' };
  if (state.status === 'preparing' || state.status === 'idle') return { mode: 'loading' };
  if (state.status === 'ready' && state.document?.url && state.document.token) {
    return { mode: 'html', url: state.document.url, token: state.document.token };
  }
  return { mode: 'text', note: state.document?.truncated ? truncatedNote : null };
}

export function truncationNote(state: DocumentState): string | null {
  return state.status === 'ready' && state.document?.truncated
    ? m.mailbox_document_truncated()
    : null;
}

/**
 * 隔离文档的生命周期：一封信一个令牌；换信作废迟到的应答并释放旧令牌；「加载本封」只记在会话里。
 * 它不持有任何 HTML 字符串——只有 URL。
 */
export class DocumentController {
  readonly store: Store<DocumentState>;
  private requests = new RequestGeneration();
  private stopLink: (() => void) | null = null;

  constructor(private readonly bridge: MailBridge) {
    this.store = new Store<DocumentState>({
      key: null, status: 'idle', document: null, error: null, allowedOnce: [], pendingLink: null,
    });
  }

  async start() {
    this.stopLink = await this.bridge.onLink((link) => {
      // 只接受当前文档的链接事件：迟到的、别的令牌的一律忽略。
      if (this.store.getSnapshot().document?.token !== link.token) return;
      this.store.patch({ pendingLink: link });
    });
  }

  stop() {
    this.stopLink?.();
    this.stopLink = null;
    this.release();
  }

  /**
   * 换信或重取详情就重新准备，清了就释放；单独更新已读状态不影响正文。
   */
  bindTo(store: Store<{ detail: UiMessage | null }>): () => void {
    let last: UiMessage | null = null;
    const sync = () => {
      const detail = store.getSnapshot().detail;
      if (detail === last) return;
      const previous = last;
      last = detail;
      if (
        detail && previous && detail.seen !== previous.seen &&
        (Object.keys(detail) as (keyof UiMessage)[]).every((key) => key === 'seen' || detail[key] === previous[key])
      ) return;
      if (detail) this.show(detail);
      else this.clear();
    };
    sync();
    return store.subscribe(sync);
  }

  /** 选中一封信（详情已到）：按内核结论与会话放行准备文档。没有 HTML 的信不发请求。 */
  show(detail: UiMessage) {
    const key = messageKey(detail);
    const state = this.store.getSnapshot();
    this.requests.invalidate();
    this.release();
    if (!detail.has_html) {
      this.store.patch({ key, status: 'ready', document: null, error: null, pendingLink: null });
      return;
    }
    void this.prepare(detail, shouldAllowRemote(detail, state.allowedOnce));
  }

  /** 会话内放行本封：记下键，以 allow_remote 重新准备（决策 5）。 */
  allowOnce(detail: UiMessage) {
    const key = messageKey(detail);
    const allowedOnce = [...this.store.getSnapshot().allowedOnce.filter((k) => k !== key), key].slice(-MAX_ALLOWED_ONCE);
    this.store.patch({ allowedOnce });
    this.release();
    void this.prepare(detail, true);
  }

  /** 策略变了（信任/撤销/全局开关）之后按新详情重新准备。 */
  reprepare(detail: UiMessage) {
    this.release();
    void this.prepare(detail, shouldAllowRemote(detail, this.store.getSnapshot().allowedOnce));
  }

  /** 换信、清选择：作废在途请求并释放令牌。 */
  clear() {
    this.requests.invalidate();
    this.release();
    this.store.patch({ key: null, status: 'idle', document: null, error: null, pendingLink: null });
  }

  dismissLink() {
    this.store.patch({ pendingLink: null });
  }

  async openPendingLink(): Promise<boolean> {
    const link = this.store.getSnapshot().pendingLink;
    if (!link || (link.kind !== 'http' && link.kind !== 'https')) return false;
    try {
      await this.bridge.openLink(link.token, link.index);
      this.store.patch({ pendingLink: null });
      return true;
    } catch {
      return false;
    }
  }

  private release() {
    const token = this.store.getSnapshot().document?.token;
    if (token) void this.bridge.releaseHtml(token).catch(() => {});
  }

  private async prepare(detail: UiMessage, allowRemote: boolean) {
    const key = messageKey(detail);
    const identity: MessageIdentity = { account: detail.account, folder: detail.folder, uid: detail.uid };
    const current = this.requests.begin();
    this.store.patch({ key, status: 'preparing', document: null, error: null, pendingLink: null });
    try {
      const document = await this.bridge.prepareHtml(identity, allowRemote);
      if (!current()) {
        // 迟到的应答：它的令牌没人会用，立刻还回去。
        if (document.token) void this.bridge.releaseHtml(document.token).catch(() => {});
        return;
      }
      this.store.patch({ key, status: 'ready', document, error: null });
    } catch (error) {
      if (!current()) return;
      this.store.patch({ key, status: 'failed', document: null, error: describeDocumentError(error) });
    }
  }
}
