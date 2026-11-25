import { m } from '@featherpost/i18n/messages';
import { useEffect, useState } from 'react';

import type { MailBridge, UiAttachment, UiLink, UiMessage } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import type { Host } from '../host/host';
import { useUiLocale } from '../i18n';
import { addressOnly, humanSize, plural } from './format';
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
  useUiLocale();
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
      host.notify(errorCode(error) === 'denied' ? m.mailbox_remote_policy_denied() : m.mailbox_remote_policy_failed());
    } finally {
      setBusy(false);
    }
  };

  if (doc.allow_remote) {
    let text = m.mailbox_remote_allowed_once();
    let action: { label: string; run: () => void } | null = null;
    if (reason === 'trusted_address') {
      text = m.mailbox_remote_allowed_address({ address });
      action = { label: m.mailbox_remote_untrust_address(), run: () => void patch({ untrust: [address] }) };
    } else if (reason === 'trusted_domain' && domain) {
      text = m.mailbox_remote_allowed_domain({ domain });
      action = { label: m.mailbox_remote_untrust_domain(), run: () => void patch({ untrust: [domain] }) };
    } else if (reason === 'allow_all') {
      text = m.mailbox_remote_allowed_all();
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
        {plural(doc.remote_refs, { one: m.mailbox_remote_blocked_one, other: m.mailbox_remote_blocked_other })}
        {doc.cid_missing > 0 &&
          plural(doc.cid_missing, { one: m.mailbox_remote_cid_missing_one, other: m.mailbox_remote_cid_missing_other })}
      </span>
      <button type="button" className="text-button" disabled={busy} onClick={() => documents.allowOnce(detail)}>
        {m.mailbox_remote_allow_once()}
      </button>
      {address.includes('@') && (
        <button type="button" className="text-button" disabled={busy} onClick={() => void patch({ trust: [address] })} title={m.mailbox_remote_trust_address_title()}>
          {m.mailbox_remote_trust_address({ address })}
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
  useUiLocale();
  const [saving, setSaving] = useState<number | null>(null);
  const visible = detail.attachments.filter((attachment) => !attachment.inline);
  if (visible.length === 0) return null;

  const save = async (attachment: UiAttachment) => {
    setSaving(attachment.part_index);
    try {
      const path = await bridge.saveAttachment({ account: detail.account, folder: detail.folder, uid: detail.uid }, attachment.part_index);
      if (path) notify(m.mailbox_attachment_saved({ path }));
    } catch (error) {
      notify(errorCode(error) === 'not_found' ? m.mailbox_attachment_not_cached() : m.mailbox_attachment_save_failed());
    } finally {
      setSaving(null);
    }
  };

  return (
    <div className="attachment-strip" aria-label={m.mailbox_attachment_strip_label()}>
      {visible.map((attachment) => (
        <div key={attachment.part_index} className="attachment">
          <span className="file-icon" aria-hidden="true">{(attachment.mime_type.split('/')[1] ?? 'file').slice(0, 4).toUpperCase()}</span>
          <span className="attachment-copy">
            <strong>{attachment.filename || m.mailbox_attachment_untitled({ index: attachment.part_index })}</strong>
            <small>
              {humanSize(attachment.size)}
              {attachment.stored ? '' : ` · ${m.mailbox_attachment_not_stored()}`}
            </small>
          </span>
          {attachment.stored && (
            <button type="button" className="text-button" disabled={saving !== null} onClick={() => void save(attachment)}>
              {saving === attachment.part_index ? m.mailbox_attachment_saving() : m.mailbox_attachment_save()}
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
  useUiLocale();
  const [copied, setCopied] = useState(false);
  useEffect(() => setCopied(false), [code]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1600);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <div className="otp-bar" role="group" aria-label={m.mailbox_otp_label()}>
      <span className="otp-label">{m.mailbox_otp_label()}</span>
      <code className="otp-code">{code}</code>
      <button
        type="button"
        className="otp-copy"
        aria-live="polite"
        onClick={() =>
          void copyText(code).then((ok) => {
            if (ok) setCopied(true);
            else notify(m.mailbox_clipboard_unavailable());
          })
        }
      >
        {copied ? m.mailbox_otp_copied() : m.mailbox_otp_copy()}
      </button>
    </div>
  );
}

/** 链接确认面板（决策 6）：先看见真实目标，再决定。 */
export function LinkSheet({ link, documents, notify, compose }: SheetProps) {
  useUiLocale();
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') documents.dismissLink();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [documents]);

  const web = link.kind === 'http' || link.kind === 'https';
  const copy = async (text: string, copied: () => string) => {
    notify((await copyText(text)) ? copied() : m.mailbox_clipboard_unavailable());
  };
  const title = web ? m.mailbox_link_title_web() : link.kind === 'mailto' ? m.mailbox_link_title_mailto() : m.mailbox_link_title_other();
  const scheme = link.href.split(':')[0]?.toLowerCase() ?? '';

  return (
    <div className="sheet-backdrop" onMouseDown={() => documents.dismissLink()}>
      <div className="sheet" role="dialog" aria-modal="true" aria-labelledby="link-sheet-title" onMouseDown={(event) => event.stopPropagation()}>
        <h2 id="link-sheet-title">{title}</h2>
        {web && (
          <p className="sheet-lead">
            {m.mailbox_link_web_lead()}<span className="link-host">{link.host || m.mailbox_link_unknown_host()}</span>
            {link.kind === 'http' && <span className="tag warn">{m.mailbox_link_http_tag()}</span>}
          </p>
        )}
        {link.kind === 'mailto' && <p className="sheet-lead">{m.mailbox_link_mailto_lead()}</p>}
        {link.kind === 'other' && (
          <p className="sheet-lead">
            {m.mailbox_link_other_lead_before()}<code>{scheme || link.host}</code>{m.mailbox_link_other_lead_after()}
            {m.mailbox_link_other_policy({ product: BRAND.productName })}
          </p>
        )}
        <code className="link-target" aria-label={m.mailbox_link_target_label()}>{link.href}</code>
        <div className="sheet-actions">
          <button type="button" className="text-button" onClick={() => documents.dismissLink()}>
            {web ? m.mailbox_action_cancel() : m.mailbox_dialog_close()}
          </button>
          <button type="button" className="text-button" onClick={() => void copy(link.kind === 'mailto' ? link.host || link.href : link.href, link.kind === 'mailto' ? m.mailbox_link_copied_address : m.mailbox_link_copied_link)}>
            {link.kind === 'mailto' ? m.mailbox_link_copy_address() : m.mailbox_link_copy_link()}
          </button>
          {link.kind === 'mailto' && compose && <button type="button" className="primary-button" onClick={()=>{try { const address=decodeURIComponent(link.href.slice(7).split('?')[0]!); compose(address); } catch { notify(m.mailbox_link_mailto_invalid()); }}}>{m.mailbox_link_compose()}</button>}
          {web && (
            <button
              type="button"
              className="primary-button"
              onClick={() => void documents.openPendingLink().then((ok) => { if (!ok) notify(m.mailbox_link_open_failed()); })}
            >
              {m.mailbox_link_open()}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
