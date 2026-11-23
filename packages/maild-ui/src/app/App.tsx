import { m } from '@featherpost/i18n/messages';
import { useEffect } from 'react';

import type { UiSnapshot } from '../bridge/types';
import { compatBlocksWrites } from '../bridge/types';
import type { Host } from '../host/host';
import { useExternal, useStore } from '../host/hooks';
import { phaseLabel } from '../settings/module';
import { Icon } from './Icon';
import { ErrorBoundary } from './ErrorBoundary';
import { applyTheme } from './theme';
import { BRAND } from '../brand';
import { identityFailureText, uiLocale, useUiLocale } from '../i18n';

function connectionLabel(snapshot: UiSnapshot): { text: string; warn: boolean } {
  if (snapshot.identity_failure) return { text: m.common_app_status_unregistered(), warn: true };
  if (compatBlocksWrites(snapshot)) return { text: m.common_app_status_read_only(), warn: true };
  if (snapshot.phase === 'online') {
    const offline = snapshot.accounts.filter((account) => account.state !== 'online' && account.state !== 'disabled').length;
    if (snapshot.accounts.length === 0) return { text: m.common_app_status_no_accounts(), warn: false };
    if (offline === 0) return { text: m.common_app_status_all_online(), warn: false };
    const one = new Intl.PluralRules(uiLocale()).select(offline) === 'one';
    return {
      text: one
        ? m.common_app_status_offline_one({ count: offline })
        : m.common_app_status_offline_other({ count: offline }),
      warn: true,
    };
  }
  if (snapshot.phase === 'retry_waiting' && snapshot.retry_in_ms !== null)
    return {
      text: m.common_app_status_retrying({ seconds: Math.max(1, Math.round(snapshot.retry_in_ms / 1000)) }),
      warn: true,
    };
  return { text: phaseLabel(snapshot.phase), warn: snapshot.phase !== 'synchronizing' };
}

function IdentityPanel({ snapshot }: { snapshot: UiSnapshot }) {
  return (
    <div className="page">
      <div className="page-scroll">
        <div className="page-column identity-panel">
          <h2>{m.common_app_identity_title({ product: BRAND.productName })}</h2>
          <p className="page-intro">{m.common_app_identity_intro()}</p>
          <p className="group-note">{m.common_app_identity_reason()}{identityFailureText(snapshot)}</p>
          <p>{m.common_app_identity_command()}</p>
          <pre className="command">{`${BRAND.cliCtl} plugin install --frontend crates/maild-ui/package`}</pre>
          <p className="group-note">{m.common_app_identity_retry()}</p>
        </div>
      </div>
    </div>
  );
}

/** ADR-0016：与桥侧拒绝文案同义；放在工作区顶部，所有视图都看得到。 */
export function compatBanner(snapshot: Pick<UiSnapshot, 'compat'>): string | null {
  switch (snapshot.compat) {
    case 'kernel_outdated':
      return m.common_app_compat_kernel_outdated({ product: BRAND.productName });
    case 'app_outdated':
      return m.common_app_compat_app_outdated({ product: BRAND.productName });
    default:
      return null;
  }
}

export function App({ host }: { host: Host }) {
  // 订阅界面语言：切换时整棵树随根组件重新渲染（不重挂载、不丢状态）。
  useUiLocale();
  const state = useStore(host.state);
  const registry = useExternal(host.registry.subscribe, host.registry.getSnapshot);
  useEffect(() => { void host.bridge.compose.ready().catch(()=>{}); }, [host]);
  useEffect(() => applyTheme(state.preferences), [state.preferences]);
  // index.html 的静态 <title> 只是首帧兜底；运行时以 brand.ts 为准（各视图随后再细化标题）。
  useEffect(() => { document.title = BRAND.productName; }, []);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === ',') {
        event.preventDefault();
        host.openRole('settings');
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [host]);

  const active = registry.views.find(({ value }) => value.id === state.activeViewId)?.value ?? null;
  const settings = registry.views.find(({ value }) => value.role === 'settings')?.value;
  const navigationCommands = registry.commands.filter(({ value }) => value.locations.includes('navigation'));
  const companions = registry.views.filter(({ value }) => value.navigation);
  const extensionViews = registry.views.filter(({ value }) => !value.role);
  const status = connectionLabel(state.snapshot);
  const banner = compatBanner(state.snapshot);
  const ActiveView = active?.render;

  return (
    <div className="shell">
      <a className="skip-link" href="#workspace">{m.common_app_skip_to_workspace()}</a>
      <aside className="sidebar">
        <div className="brandbar" data-tauri-drag-region="deep">
          <span className="brand">{BRAND.productName}</span>
        </div>
        {navigationCommands.map(({ value }) => (
          <button key={value.id} type="button" className="nav-item" aria-label={value.title} title={value.title} disabled={!host.isEnabled(value.enabledWhen)} onClick={() => host.execute(value.id)}>
            {value.icon && <Icon name={value.icon} />}
            <span className="nav-text">{value.title}</span>
          </button>
        ))}
        <div className="nav-scroll">
          {companions.map(({ value }) => {
            const Navigation = value.navigation!;
            return <Navigation key={value.id} active={active?.id === value.id} />;
          })}
          {extensionViews.length > 0 && (
            <section className="account-group">
              <div className="nav-heading">
                <Icon name="puzzle" />
                <span className="account-name">{m.common_app_nav_extensions()}</span>
              </div>
              {extensionViews.map(({ value }) => (
                <button key={value.id} type="button" className={`nav-item${active?.id === value.id ? ' active' : ''}`} aria-current={active?.id === value.id ? 'page' : undefined} onClick={() => host.openView(value.id)}>
                  {value.icon && <Icon name={value.icon} />}
                  <span className="nav-text">{value.title}</span>
                </button>
              ))}
            </section>
          )}
        </div>
        <div className="sidebar-bottom">
          <button type="button" className="status-button" onClick={() => host.openRole('settings')} title={m.common_app_status_title()}>
            <span className={`status-dot${status.warn ? ' warn' : ''}`} aria-hidden="true" />
            <span className="status-text">{status.text}</span>
          </button>
          {settings && (
            <button type="button" className={`nav-item${active?.id === settings.id ? ' active' : ''}`} aria-current={active?.id === settings.id ? 'page' : undefined} onClick={() => host.openView(settings.id)}>
              <Icon name="settings" />
              <span className="nav-text">{m.common_app_nav_settings()}</span>
              <kbd>⌘,</kbd>
            </button>
          )}
        </div>
      </aside>
      <main id="workspace" tabIndex={-1}>
        {banner && <div className="banner compat-banner" role="alert">{banner}</div>}
        {state.snapshot.identity_failure && active?.role !== 'settings' ? (
          <IdentityPanel snapshot={state.snapshot} />
        ) : (
          ActiveView && (
            <ErrorBoundary resetKey={state.activeViewId}>
              <ActiveView active />
            </ErrorBoundary>
          )
        )}
      </main>
      <div id="toast" className={state.toast ? 'show' : ''} role="status" aria-live="polite">
        {state.toast}
      </div>
    </div>
  );
}
