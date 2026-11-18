import { useEffect, useState } from 'react';

import type { UiReadingPolicy, UiReadingPolicyPatch } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import type { Host } from '../host/host';

/**
 * 「设置 → 阅读与隐私」（ADR-0033 决策 9）：远程内容默认策略与可信发件人名单。
 * 读写都经桥到内核（ADR-0032）——策略是 daemon 的状态，不是这个窗口的。
 */
export function ReadingPrivacySection({ host, onChanged }: { host: Host; onChanged: () => void }) {
  const [policy, setPolicy] = useState<UiReadingPolicy | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);

  const load = async () => {
    try {
      setPolicy(await host.bridge.readingPolicy());
      setError(null);
    } catch (fetchError) {
      setError(errorCode(fetchError) === 'denied' ? '当前前端没有读取阅读策略的授权。' : '暂时无法读取阅读策略。');
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
          ? '没有保存：请输入完整的邮件地址（a@b.c）或域名（b.c）。'
          : code === 'denied'
            ? '当前前端没有修改阅读策略的授权。'
            : '没有保存：阅读策略文件不可用或后台离线。',
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
      <h2>阅读与隐私</h2>
      <p className="page-intro">
        邮件里的远程图片、样式与字体默认不联网；脚本、表单与应用接口在正文里始终不可用。这里的设置保存在后台，换一个前端也一样生效。
      </p>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {policy && !policy.persisted && (
        <div className="banner" role="status">
          后台的阅读策略文件不可用，当前按默认（阻止）处理；修改会被拒绝，直到文件修复。
        </div>
      )}
      <div className="setting-row">
        <div className="setting-copy">
          <strong>远程内容</strong>
          <p>「阻止」时每封邮件可单独选择「加载本封」，只在本次会话内生效。</p>
        </div>
        <select
          aria-label="远程内容默认策略"
          value={policy?.remote_content ?? 'block'}
          disabled={!policy || busy || !policy.persisted}
          onChange={(event) => void apply({ remote_content: event.target.value === 'allow' ? 'allow' : 'block' })}
        >
          <option value="block">阻止（推荐）</option>
          <option value="allow">对所有发件人加载</option>
        </select>
      </div>
      <div className="setting-row">
        <div className="setting-copy">
          <strong>可信发件人</strong>
          <p>来自这些地址或域的邮件会加载远程图片。只匹配完整地址或精确域名——信任 example.com 不会放行 news.example.com。</p>
        </div>
        <div className="trust-input">
          <input
            type="text"
            aria-label="添加可信发件人"
            placeholder="a@example.com 或 example.com"
            value={draft}
            disabled={!policy || busy || !policy.persisted}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') trust();
            }}
          />
          <button type="button" className="text-button" disabled={!policy || busy || !draft.trim() || !policy.persisted} onClick={trust}>
            添加
          </button>
        </div>
      </div>
      {policy && policy.trusted_senders.length === 0 && <p className="group-note">名单为空。在阅读一封被拦截的邮件时点「始终允许」也会加到这里。</p>}
      {policy?.trusted_senders.map((entry) => (
        <div key={entry.value} className="setting-row trusted-row">
          <div className="setting-copy">
            <strong>{entry.value}</strong>
            <p>{entry.kind === 'domain' ? '整个域（精确匹配）' : '单个地址'}</p>
          </div>
          <button type="button" className="text-button" disabled={busy} onClick={() => void apply({ untrust: [entry.value] })}>
            移除
          </button>
        </div>
      ))}
    </>
  );
}
