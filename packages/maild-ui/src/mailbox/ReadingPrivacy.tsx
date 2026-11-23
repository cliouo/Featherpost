import { m } from '@featherpost/i18n/messages';
import { useEffect, useState } from 'react';
import { Choice } from '../settings/Choice';

import type { UiReadingPolicy, UiReadingPolicyPatch } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import type { Host } from '../host/host';
import { useUiLocale } from '../i18n';

/**
 * 「设置 → 阅读与隐私」（ADR-0033 决策 9）：远程内容默认策略与可信发件人名单。
 * 读写都经桥到内核（ADR-0032）——策略是 daemon 的状态，不是这个窗口的。
 */
export function ReadingPrivacySection({ host, onChanged }: { host: Host; onChanged: () => void }) {
  useUiLocale();
  const [policy, setPolicy] = useState<UiReadingPolicy | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);

  const load = async () => {
    try {
      setPolicy(await host.bridge.readingPolicy());
      setError(null);
    } catch (fetchError) {
      setError(errorCode(fetchError) === 'denied' ? m.mailbox_reading_error_read_denied() : m.mailbox_reading_error_read_failed());
    }
  };
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const apply = async (patch: UiReadingPolicyPatch) => {
    setBusy(true);
    try {
      setPolicy(await host.bridge.setReadingPolicy(patch));
      setError(null);
      onChanged();
    } catch (applyError) {
      const code = errorCode(applyError);
      setError(
        code === 'invalid'
          ? m.mailbox_reading_error_invalid()
          : code === 'denied'
            ? m.mailbox_remote_policy_denied()
            : m.mailbox_reading_error_write_failed(),
      );
    } finally {
      setBusy(false);
    }
  };

  const trust = () => {
    const value = draft.trim();
    if (!value) return;
    void apply({ trust: [value] }).then(() => setDraft(''));
  };

  return (
    <>
      <h2>{m.mailbox_reading_title()}</h2>
      <p className="page-intro">
        {m.mailbox_reading_intro()}
      </p>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {policy && !policy.persisted && (
        <div className="banner" role="status">
          {m.mailbox_reading_not_persisted()}
        </div>
      )}
      <div className="setting-row">
        <div className="setting-copy">
          <strong>{m.mailbox_reading_remote_title()}</strong>
          <p>{m.mailbox_reading_remote_hint()}</p>
        </div>
        <Choice
          aria-label={m.mailbox_reading_remote_label()}
          value={policy?.remote_content ?? 'block'}
          disabled={!policy || busy || !policy.persisted}
          onValueChange={(value) => void apply({ remote_content: value === 'allow' ? 'allow' : 'block' })}
        >
          <option value="block">{m.mailbox_reading_remote_block()}</option>
          <option value="allow">{m.mailbox_reading_remote_allow()}</option>
        </Choice>
      </div>
      <div className="setting-row">
        <div className="setting-copy">
          <strong>{m.mailbox_reading_trusted_title()}</strong>
          <p>{m.mailbox_reading_trusted_hint()}</p>
        </div>
        <div className="trust-input">
          <input
            type="text"
            aria-label={m.mailbox_reading_trusted_add_label()}
            placeholder={m.mailbox_reading_trusted_placeholder()}
            value={draft}
            disabled={!policy || busy || !policy.persisted}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') trust();
            }}
          />
          <button type="button" className="text-button" disabled={!policy || busy || !draft.trim() || !policy.persisted} onClick={trust}>
            {m.mailbox_reading_trusted_add()}
          </button>
        </div>
      </div>
      {policy && policy.trusted_senders.length === 0 && <p className="group-note">{m.mailbox_reading_trusted_empty()}</p>}
      {policy?.trusted_senders.map((entry) => (
        <div key={entry.value} className="setting-row trusted-row">
          <div className="setting-copy">
            <strong>{entry.value}</strong>
            <p>{entry.kind === 'domain' ? m.mailbox_reading_trusted_kind_domain() : m.mailbox_reading_trusted_kind_address()}</p>
          </div>
          <button type="button" className="text-button" disabled={busy} onClick={() => void apply({ untrust: [entry.value] })}>
            {m.mailbox_reading_trusted_remove()}
          </button>
        </div>
      ))}
    </>
  );
}
