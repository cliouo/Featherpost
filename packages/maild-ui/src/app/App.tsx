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

function connectionLabel(snapshot: UiSnapshot): { text: string; warn: boolean } {
  if (snapshot.identity_failure) return { text: '前端未注册', warn: true };
  if (compatBlocksWrites(snapshot)) return { text: '版本不一致，只读', warn: true };
  if (snapshot.phase === 'online') {
    const offline = snapshot.accounts.filter((account) => account.state !== 'online' && account.state !== 'disabled').length;
    if (snapshot.accounts.length === 0) return { text: '已连接后台，尚无账户', warn: false };
    return offline > 0 ? { text: `${offline} 个邮箱离线`, warn: true } : { text: '所有邮箱已连接', warn: false };
  }
  if (snapshot.phase === 'retry_waiting' && snapshot.retry_in_ms !== null)
    return { text: `后台连接不可用，${Math.max(1, Math.round(snapshot.retry_in_ms / 1000))} 秒后重试`, warn: true };
  return { text: phaseLabel(snapshot.phase), warn: snapshot.phase !== 'synchronizing' };
}

function IdentityPanel({ snapshot }: { snapshot: UiSnapshot }) {
  return (
    <div className="page">
      <div className="page-scroll">
        <div className="page-column identity-panel">
          <h2>{BRAND.productName} UI 尚未注册为前端包</h2>
          <p className="page-intro">这个窗口以自己的身份连接后台，而后台还不认识它。它不会退而以完整用户身份运行。</p>
          <p className="group-note">后台的说法：{snapshot.identity_failure}</p>
          <p>在终端执行一次：</p>
          <pre className="command">{`${BRAND.cliCtl} plugin install --frontend crates/maild-ui/package`}</pre>
          <p className="group-note">安装完成后会自动重试连接，不需要重开窗口。</p>
        </div>
      </div>
    </div>
  );
}

/** ADR-0016：与桥侧拒绝文案同义；放在工作区顶部，所有视图都看得到。 */
export function compatBanner(snapshot: Pick<UiSnapshot, 'compat'>): string | null {
  switch (snapshot.compat) {
    case 'kernel_outdated':
      return `后台版本较旧，与当前界面不兼容：可以继续读信，写信、标记和设置修改已暂停。请重新启动后台；若刚回退过 ${BRAND.productName}，请从菜单栏「修复与后台管理…」重装内置界面。`;
    case 'app_outdated':
      return `当前界面版本较旧，与后台不兼容：可以继续读信，写信、标记和设置修改已暂停。请从菜单栏「修复与后台管理…」重装内置界面，或更新 ${BRAND.productName}。`;
    default:
      return null;
  }
}

export function App({ host }: { host: Host }) {
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
      <a className="skip-link" href="#workspace">跳到工作区</a>
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
                <span className="account-name">扩展</span>
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
          <button type="button" className="status-button" onClick={() => host.openRole('settings')} title="查看连接状态">
            <span className={`status-dot${status.warn ? ' warn' : ''}`} aria-hidden="true" />
            <span className="status-text">{status.text}</span>
          </button>
          {settings && (
            <button type="button" className={`nav-item${active?.id === settings.id ? ' active' : ''}`} aria-current={active?.id === settings.id ? 'page' : undefined} onClick={() => host.openView(settings.id)}>
              <Icon name="settings" />
              <span className="nav-text">设置</span>
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
