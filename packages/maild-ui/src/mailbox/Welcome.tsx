import { m } from "@featherpost/i18n/messages";

import { Icon } from "../app/Icon";
import { CUSTOM_PROVIDER, QUICK_PROVIDERS, providerById } from "../settings/providers";

/**
 * 零账户首次进入：主区域的「连接你的第一个邮箱」引导卡。
 * 不调用 hook（测试把它当纯函数调用）；语言切换时随父组件 `Workspace` 的 `useUiLocale()` 重新渲染。
 */
export function Welcome({ onSetup }: { onSetup: (provider?: string) => void }) {
  return (
    <section className="welcome" aria-labelledby="welcome-title">
      <div className="welcome-card">
        <Icon name="mail" />
        <h1 id="welcome-title">{m.mailbox_welcome_title()}</h1>
        <p className="welcome-lead">
          {m.mailbox_welcome_lead()}
        </p>
        <div className="welcome-providers" role="group" aria-label={m.mailbox_welcome_providers_label()}>
          {QUICK_PROVIDERS.filter((id) => id !== CUSTOM_PROVIDER).map((id) => {
            const preset = providerById(id)!;
            return (
              <button key={id} type="button" className="welcome-provider" onClick={() => onSetup(id)}>
                <strong>{preset.short}</strong>
                <small>{preset.domains.slice(0, 2).join(" · ")}</small>
              </button>
            );
          })}
          <button type="button" className="welcome-provider" onClick={() => onSetup(CUSTOM_PROVIDER)}>
            <strong>{m.mailbox_welcome_other_provider()}</strong>
            <small>{m.mailbox_welcome_other_provider_hint()}</small>
          </button>
        </div>
        <ul className="welcome-facts">
          <li>{m.mailbox_welcome_fact_keychain()}</li>
          <li>{m.mailbox_welcome_fact_background()}</li>
        </ul>
      </div>
    </section>
  );
}
