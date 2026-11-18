import { useEffect, useState } from 'react';

import type { MailBridge, UiAttachment, UiLink, UiMessage } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import type { Host } from '../host/host';
import { addressOnly, humanSize } from './format';
import type { DocumentController, DocumentState } from './document';
import { messageKey } from './controller';
import { BRAND } from '../brand';

/** 发件人地址的域名（信任「整个域」的选项用）。 */
export function senderDomain(sender: string): string | null {
  const address = addressOnly(sender);
  const at = address.lastIndexOf('@');
  return at > 0 && at < address.length - 1 ? address.slice(at + 1).toLowerCase() : null;
}

interface BannerProps {
  host: Host;
  detail: UiMessage;
  state: DocumentState;
  documents: DocumentController;
  onPolicyChanged: () => void;
}

/**
 * 远程内容横幅（ADR-0033 决策 5）：阻止时给「加载本封」（会话内）与「始终允许」（写回内核名单）；
 * 放行时写明依据并给「停止信任」。没有远程引用就没有横幅。
 */
export function RemoteBanner({ host, detail, state, documents, onPolicyChanged }: BannerProps) {
  const [busy, setBusy] = useState(false);
  const doc = state.document;
  if (!doc || state.key !== messageKey(detail) || state.status !== 'ready' || doc.remote_refs === 0) return null;
  const address = addressOnly(detail.sender).toLowerCase();
  const domain = senderDomain(detail.sender);
  const reason = detail.remote_content.reason;

  const patch = async (change: { trust?: string[]; untrust?: string[] }) => {
    setBusy(true);
    try {
      await host.bridge.setReadingPolicy(change);
      onPolicyChanged();
    } catch (error) {
      host.notify(errorCode(error) === 'denied' ? '当前前端没有修改阅读策略的授权。' : '阅读策略未能保存，请稍后重试。');
    } finally {
      setBusy(false);
    }
  };

  if (doc.allow_remote) {
    let text = '本封已加载远程内容（仅本次，不会记住）。';
    let action: { label: string; run: () => void } | null = null;
    if (reason === 'trusted_address') {
      text = `来自 ${address} 的远程内容已按信任名单加载。`;
      action = { label: '停止信任此地址', run: () => void patch({ untrust: [address] }) };
    } else if (reason === 'trusted_domain' && domain) {
      text = `来自 ${domain} 域的远程内容已按信任名单加载。`;
      action = { label: '停止信任此域', run: () => void patch({ untrust: [domain] }) };
    } else if (reason === 'allow_all') {
      text = '远程内容已按「阅读与隐私」设置全局加载。';
    }
    return (
      <div className="remote-banner allowed" role="status">
        <span>{text}</span>
        {action && (
          <button type="button" className="text-button" disabled={busy} onClick={action.run}>
            {action.label}
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="remote-banner" role="status">
      <span>
        已拦截 {doc.remote_refs} 处远程内容（图片等），未向任何服务器发出请求。
        {doc.cid_missing > 0 && ` 另有 ${doc.cid_missing} 张内嵌图片不在本地缓存中。`}
      </span>
      <button type="button" className="text-button" disabled={busy} onClick={() => documents.allowOnce(detail)}>
        加载本封
      </button>
      {address.includes('@') && (
        <button type="button" className="text-button" disabled={busy} onClick={() => void patch({ trust: [address] })} title="写入「阅读与隐私」的可信发件人名单；以后来自这个地址的邮件会请求远程资源">
          始终允许 {address}
        </button>
      )}
    </div>
  );
}

interface StripProps {
  bridge: MailBridge;
  detail: UiMessage;
  notify: (text: string) => void;
  compose?: (address: string) => void;
}

/** 附件条（决策 8）：只列非内联附件；`stored` 才有「保存」，否则如实写「尚未保存此附件」。 */
export function AttachmentStrip({ bridge, detail, notify }: StripProps) {
  const [saving, setSaving] = useState<number | null>(null);
  const visible = detail.attachments.filter((attachment) => !attachment.inline);
  if (visible.length === 0) return null;

  const save = async (attachment: UiAttachment) => {
    setSaving(attachment.part_index);
    try {
      const path = await bridge.saveAttachment({ account: detail.account, folder: detail.folder, uid: detail.uid }, attachment.part_index);
      if (path) notify(`已保存到 ${path}`);
    } catch (error) {
      notify(errorCode(error) === 'not_found' ? '这个附件的字节没有保存在本地缓存里。' : '保存未完成，已有文件未被改动。');
    } finally {
      setSaving(null);
    }
  };

  return (
    <div className="attachment-strip" aria-label="附件">
      {visible.map((attachment) => (
        <div key={attachment.part_index} className="attachment">
          <span className="file-icon" aria-hidden="true">{(attachment.mime_type.split('/')[1] ?? 'file').slice(0, 4).toUpperCase()}</span>
          <span className="attachment-copy">
            <strong>{attachment.filename || `附件 ${attachment.part_index}`}</strong>
            <small>
              {humanSize(attachment.size)}
              {attachment.stored ? '' : ' · 尚未保存此附件'}
            </small>
          </span>
          {attachment.stored && (
            <button type="button" className="text-button" disabled={saving !== null} onClick={() => void save(attachment)}>
              {saving === attachment.part_index ? '保存中…' : '保存'}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

interface SheetProps {
  link: UiLink;
  documents: DocumentController;
  notify: (text: string) => void;
  compose?: (address: string) => void;
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** 阅读区顶部的验证码条：一键复制，短暂显示「已复制」。 */
export function OtpBar({ code, notify }: { code: string; notify: (text: string) => void }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => setCopied(false), [code]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1600);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <div className="otp-bar" role="group" aria-label="验证码">
      <span className="otp-label">验证码</span>
      <code className="otp-code">{code}</code>
      <button
        type="button"
        className="otp-copy"
        aria-live="polite"
        onClick={() =>
          void copyText(code).then((ok) => {
            if (ok) setCopied(true);
            else notify('无法访问剪贴板。');
          })
        }
      >
        {copied ? '已复制' : '复制验证码'}
      </button>
    </div>
  );
}

/** 链接确认面板（决策 6）：先看见真实目标，再决定。 */
export function LinkSheet({ link, documents, notify, compose }: SheetProps) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') documents.dismissLink();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [documents]);

  const web = link.kind === 'http' || link.kind === 'https';
  const copy = async (text: string, what: string) => {
    notify((await copyText(text)) ? `已复制${what}。` : '无法访问剪贴板。');
  };
  const title = web ? '打开链接？' : link.kind === 'mailto' ? '这是一个写信地址' : '这个链接不会被执行';
  const scheme = link.href.split(':')[0]?.toLowerCase() ?? '';

  return (
    <div className="sheet-backdrop" onMouseDown={() => documents.dismissLink()}>
      <div className="sheet" role="dialog" aria-modal="true" aria-labelledby="link-sheet-title" onMouseDown={(event) => event.stopPropagation()}>
        <h2 id="link-sheet-title">{title}</h2>
        {web && (
          <p className="sheet-lead">
            将在系统浏览器中打开 <span className="link-host">{link.host || '（未知主机）'}</span>
            {link.kind === 'http' && <span className="tag warn">未加密 http</span>}
          </p>
        )}
        {link.kind === 'mailto' && <p className="sheet-lead">将在当前窗口新建草稿，不会自动发送。</p>}
        {link.kind === 'other' && (
          <p className="sheet-lead">
            此链接使用 <code>{scheme || link.host}</code> 协议。{BRAND.productName} 只把 http/https 交给浏览器，其他协议一律不执行。
          </p>
        )}
        <code className="link-target" aria-label="链接完整目标">{link.href}</code>
        <div className="sheet-actions">
          <button type="button" className="text-button" onClick={() => documents.dismissLink()}>
            {web ? '取消' : '关闭'}
          </button>
          <button type="button" className="text-button" onClick={() => void copy(link.kind === 'mailto' ? link.host || link.href : link.href, link.kind === 'mailto' ? '地址' : '链接')}>
            {link.kind === 'mailto' ? '复制地址' : '复制链接'}
          </button>
          {link.kind === 'mailto' && compose && <button type="button" className="primary-button" onClick={()=>{try { const address=decodeURIComponent(link.href.slice(7).split('?')[0]!); compose(address); } catch { notify('写信地址无效。'); }}}>写邮件</button>}
          {web && (
            <button
              type="button"
              className="primary-button"
              onClick={() => void documents.openPendingLink().then((ok) => { if (!ok) notify('无法打开系统浏览器。'); })}
            >
              在浏览器中打开
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
