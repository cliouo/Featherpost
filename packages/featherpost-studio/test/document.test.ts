import { describe, expect, it } from 'vitest';

import { Store } from '../src/host/store';
import { messageKey } from '../src/mailbox/controller';
import { DocumentController, presentation, shouldAllowRemote, truncationNote } from '../src/mailbox/document';
import type { UiMessage } from '../src/bridge/types';
import { BRAND } from '../src/brand';
import { FakeBridge, document as doc, flush, message, noHtml, offlineError } from './fakeBridge';

const htmlMail = (uid: string, extra: Partial<UiMessage> = {}) => message('INBOX', uid, { has_html: true, html_bytes: 100, ...extra });

describe('DocumentController', () => {
  it('keeps the HTML document when only the read flag changes', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const mail = htmlMail('1', { seen: false });
    const detail = new Store<{ detail: UiMessage | null }>({ detail: mail });
    documents.bindTo(detail);
    bridge.prepareRequests[0].d.resolve(doc('t1'));
    await flush();

    detail.patch({ detail: { ...mail, seen: true } });
    expect(documents.store.getSnapshot().document?.token).toBe('t1');
    expect(bridge.prepareRequests).toHaveLength(1);
    expect(bridge.released).toEqual([]);

    detail.patch({ detail: htmlMail('1') });
    expect(bridge.prepareRequests).toHaveLength(2);
  });

  it('prepares a document for html mail with the kernel decision and releases it on switch', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const detail = new Store<{ detail: UiMessage | null }>({ detail: null });
    documents.bindTo(detail);

    detail.patch({ detail: htmlMail('1') });
    expect(bridge.prepareRequests).toHaveLength(1);
    expect(bridge.prepareRequests[0].allowRemote).toBe(false);
    expect(documents.store.getSnapshot().status).toBe('preparing');
    expect(presentation(htmlMail('1'), documents.store.getSnapshot()).mode).toBe('loading');

    bridge.prepareRequests[0].d.resolve(doc('t1', { remote_refs: 2 }));
    await flush();
    const ready = documents.store.getSnapshot();
    expect(ready.status).toBe('ready');
    expect(presentation(htmlMail('1'), ready)).toEqual({ mode: 'html', url: `${BRAND.uriScheme}://localhost/doc/t1`, token: 't1' });

    // 换信：旧令牌立刻释放，新文档另起。
    detail.patch({ detail: htmlMail('2') });
    expect(bridge.released).toEqual(['t1']);
    expect(bridge.prepareRequests).toHaveLength(2);
    // 旧文档对新信不算「就绪」——不能把上一封的内容留给新选中项。
    expect(presentation(htmlMail('2'), documents.store.getSnapshot()).mode).toBe('loading');

    // 清选择：作废在途请求，迟到的令牌也还回去。
    detail.patch({ detail: null });
    bridge.prepareRequests[1].d.resolve(doc('t2'));
    await flush();
    expect(documents.store.getSnapshot().status).toBe('idle');
    expect(bridge.released).toEqual(['t1', 't2']);
  });

  it('shows plain text without a request when the mail has no html and notes truncation', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const plain = message('INBOX', '5');
    documents.show(plain);
    expect(bridge.prepareRequests).toHaveLength(0);
    expect(presentation(plain, documents.store.getSnapshot())).toEqual({ mode: 'text', note: '此缓存中没有 HTML 版式，显示纯文本。' });

    const cut = htmlMail('6');
    documents.show(cut);
    bridge.prepareRequests[0].d.resolve(doc('t6', { truncated: true }));
    await flush();
    expect(truncationNote(documents.store.getSnapshot())).toContain('不完整');
    expect(presentation(cut, documents.store.getSnapshot()).mode).toBe('html');
  });

  it('falls back to text with a fixed message when preparation fails', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const mail = htmlMail('7');
    documents.show(mail);
    bridge.prepareRequests[0].d.reject(offlineError());
    await flush();
    const state = documents.store.getSnapshot();
    expect(state.status).toBe('failed');
    expect(state.error).toBe('后台连接不可用，正文按纯文本显示。');
    expect(presentation(mail, state)).toEqual({ mode: 'text', note: null });
  });

  it('remembers "load this one" only for the session and only for that mail', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const first = htmlMail('1');
    const second = htmlMail('2');
    documents.show(first);
    bridge.prepareRequests[0].d.resolve(doc('a', { remote_refs: 3 }));
    await flush();

    documents.allowOnce(first);
    expect(bridge.released).toEqual(['a']);
    expect(bridge.prepareRequests[1].allowRemote).toBe(true);
    bridge.prepareRequests[1].d.resolve(doc('b', { remote_refs: 3, allow_remote: true }));
    await flush();
    expect(shouldAllowRemote(first, documents.store.getSnapshot().allowedOnce)).toBe(true);
    // 别的信不受影响；内核说 allow 的信本来就放行。
    expect(shouldAllowRemote(second, documents.store.getSnapshot().allowedOnce)).toBe(false);
    expect(shouldAllowRemote(htmlMail('9', { remote_content: { decision: 'allow', reason: 'trusted_address' } }), [])).toBe(true);

    // 重新显示同一封（比如重取详情）仍按会话记忆放行。
    documents.show(first);
    expect(bridge.prepareRequests[2].allowRemote).toBe(true);
    documents.show(second);
    expect(bridge.prepareRequests[3].allowRemote).toBe(false);
    // 会话记忆没有写回内核。
    expect(bridge.policyPatches).toEqual([]);
  });

  it('accepts link events only for the current document and opens only web links', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    await documents.start();
    const mail = htmlMail('1');
    documents.show(mail);
    bridge.prepareRequests[0].d.resolve(doc('cur', { links: 2 }));
    await flush();

    bridge.linkHandler?.({ token: 'stale', index: 0, href: 'https://x', kind: 'https', host: 'x' });
    expect(documents.store.getSnapshot().pendingLink).toBeNull();
    bridge.linkHandler?.({ token: 'cur', index: 1, href: 'javascript:alert(1)', kind: 'other', host: 'javascript' });
    expect(documents.store.getSnapshot().pendingLink?.kind).toBe('other');
    expect(await documents.openPendingLink()).toBe(false);
    expect(bridge.opened).toEqual([]);
    documents.dismissLink();

    bridge.linkHandler?.({ token: 'cur', index: 0, href: 'https://shop.example/x', kind: 'https', host: 'shop.example' });
    expect(await documents.openPendingLink()).toBe(true);
    expect(bridge.opened).toEqual([{ token: 'cur', index: 0 }]);
    expect(documents.store.getSnapshot().pendingLink).toBeNull();

    documents.stop();
    expect(bridge.linkHandler).toBeNull();
    expect(bridge.released).toEqual(['cur']);
  });

  it('advances generation when switching from preparing HTML mail to plain-text mail so stale HTML tokens are released and ignored', async () => {
    const bridge = new FakeBridge();
    const documents = new DocumentController(bridge);
    const html = htmlMail('10');
    const plain = message('INBOX', '11');

    documents.show(html);
    expect(bridge.prepareRequests).toHaveLength(1);
    expect(documents.store.getSnapshot().status).toBe('preparing');

    // Switch immediately to a plain-text message while HTML prepare is still in flight.
    documents.show(plain);
    expect(documents.store.getSnapshot()).toMatchObject({
      key: messageKey(plain),
      status: 'ready',
      document: null,
      error: null,
    });

    // Late HTML prepare resolves: must NOT overwrite the plain-text state and MUST release its token.
    bridge.prepareRequests[0]!.d.resolve(doc('stale-html-token'));
    await flush();
    expect(documents.store.getSnapshot()).toMatchObject({
      key: messageKey(plain),
      status: 'ready',
      document: null,
      error: null,
    });
    expect(bridge.released).toEqual(['stale-html-token']);
    expect(presentation(plain, documents.store.getSnapshot())).toEqual({
      mode: 'text',
      note: '此缓存中没有 HTML 版式，显示纯文本。',
    });
  });

  it('keeps the no-html document shape honest', () => {
    expect(noHtml.token).toBeNull();
    expect(presentation(message('INBOX', '1'), { key: null, status: 'idle', document: null, error: null, allowedOnce: [], pendingLink: null }).mode).toBe('text');
  });
});
