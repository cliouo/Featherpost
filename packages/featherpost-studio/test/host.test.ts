import { applyEffectiveLocales } from '@featherpost/i18n';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';

import { ComposeController, COMPOSE_VIEW } from '../src/compose/controller';
import { composeModule } from '../src/compose/module';
import { Host } from '../src/host/host';
import type { HostMailboxContext, View } from '../src/host/types';
import { studioMailbox } from '../src/mailbox';
import { MailboxController } from '../src/mailbox/controller';
import { SettingsController } from '../src/settings/controller';
import { settingsModule, SETTINGS_VIEW } from '../src/settings/module';
import { Studio } from '../src/Shell';
import { FakeBridge, flush, folder, message, online, summary } from './fakeBridge';
import { account } from './fakeSettings';

const view = (_id: string) => (() => null) as unknown as View;

function core(host: Host) {
  host.registerBuiltin({ moduleId: 'maild.mailbox', apiVersion: 1, title: '邮箱', views: [{ id: 'maild.mailbox.workspace', title: '邮件', role: 'mailbox', render: view('m') }] });
  host.registerBuiltin({ moduleId: 'maild.settings', apiVersion: 1, title: '设置', views: [{ id: 'maild.settings.page', title: '设置', role: 'settings', render: view('s') }] });
  host.finishAssembly();
}

describe('Host：坏模块不拖垮邮箱，命令只拿受限上下文', () => {
  it('可选模块注册失败只记诊断', () => {
    const host = new Host(new FakeBridge());
    core(host);
    host.registerOptional('org.example.broken', {
      title: 'broken',
      defaultEnabled: true,
      bundle: () => ({ moduleId: 'org.example.broken', apiVersion: 1, title: 'broken', commands: [{ id: 'elsewhere.cmd', title: 'x', locations: ['menu'], execute: () => {} }] }),
    });
    expect(host.isModuleEnabled('org.example.broken')).toBe(false);
    expect(host.optionalModules()).toEqual([{ moduleId: 'org.example.broken', title: 'broken', enabled: false }]);
    expect(host.state.getSnapshot().diagnostics[0]).toMatch(/org.example.broken/);
    expect(host.registry.resolveRole('mailbox').id).toBe('maild.mailbox.workspace');
    expect(host.state.getSnapshot().activeViewId).toBe('maild.mailbox.workspace');
  });

  it('停用扩展时正在看的扩展视图回到邮箱，入口一起消失', () => {
    const host = new Host(new FakeBridge());
    core(host);
    host.registerOptional('org.example.ext', {
      title: 'ext',
      defaultEnabled: true,
      bundle: () => ({
        moduleId: 'org.example.ext',
        apiVersion: 1,
        title: 'ext',
        views: [{ id: 'org.example.ext.page', title: 'page', render: view('p') }],
        commands: [{ id: 'org.example.ext.go', title: 'go', locations: ['toolbar', 'menu'], execute: (context) => context.openView('org.example.ext.page') }],
      }),
    });
    host.execute('org.example.ext.go');
    expect(host.state.getSnapshot().activeViewId).toBe('org.example.ext.page');
    host.setModuleEnabled('org.example.ext', false);
    expect(host.state.getSnapshot().activeViewId).toBe('maild.mailbox.workspace');
    expect(host.registry.findCommand('org.example.ext.go')).toBeUndefined();
    expect(host.registry.findView('org.example.ext.page')).toBeUndefined();
    host.setModuleEnabled('org.example.ext', true);
    expect(host.registry.findCommand('org.example.ext.go')).toBeDefined();
  });

  it('enabledWhen 受限条件与上下文', () => {
    const host = new Host(new FakeBridge());
    core(host);
    const seen: unknown[] = [];
    host.registerOptional('org.example.clip', {
      title: 'clip',
      defaultEnabled: true,
      bundle: () => ({
        moduleId: 'org.example.clip',
        apiVersion: 1,
        title: 'clip',
        commands: [{ id: 'org.example.clip.do', title: 'do', locations: ['toolbar'], enabledWhen: 'message-selected', execute: (context) => seen.push(context) }],
      }),
    });
    host.execute('org.example.clip.do');
    expect(seen).toHaveLength(0);
    host.setSelection({ identity: { account: 'demo', folder: 'INBOX', uid: '1' }, subject: 's' });
    host.execute('org.example.clip.do');
    expect(seen).toHaveLength(1);
    const context = seen[0] as Record<string, unknown>;
    expect(Object.keys(context).sort()).toEqual(['notify', 'online', 'openView', 'selection', 'selectionSubject']);
    expect(context.selection).toEqual({ account: 'demo', folder: 'INBOX', uid: '1' });
    expect(context.online).toBe(false);
  });

  it('执行抛错的命令只记诊断并提示，不让壳崩', () => {
    const host = new Host(new FakeBridge());
    core(host);
    host.registerOptional('org.example.boom', {
      title: 'boom',
      defaultEnabled: true,
      bundle: () => ({ moduleId: 'org.example.boom', apiVersion: 1, title: 'boom', commands: [{ id: 'org.example.boom.go', title: 'go', locations: ['menu'], execute: () => { throw new Error('nope'); } }] }),
    });
    host.execute('org.example.boom.go');
    expect(host.state.getSnapshot().diagnostics.at(-1)).toMatch(/nope/);
    expect(host.state.getSnapshot().toast).toBe('操作未完成。');
  });

  it('start 先订阅再读快照，并在上线时读前端清单', async () => {
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    core(host);
    await host.start();
    expect(host.state.getSnapshot().snapshot.phase).toBe('online');
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(host.state.getSnapshot().frontendInfo?.permissions).toEqual(['mailbox_write']);
    // 旧 revision 的推送被忽略。
    bridge.snapshotHandler?.({ ...online, revision: 0, accounts: [] });
    expect(host.state.getSnapshot().snapshot.accounts).toHaveLength(1);
    host.stop();
    expect(bridge.snapshotHandler).toBeNull();
  });
});

it('版本不一致时写入口禁用、横幅出现；同 revision 的握手补推被接受，旧 revision 仍被忽略', async () => {
  const bridge = new FakeBridge();
  const host = new Host(bridge);
  const mailbox = new MailboxController(bridge);
  const composer = new ComposeController(bridge.compose, () => {}, () => {});
  core(host);
  await host.start();
  const renderShell = () =>
    renderToStaticMarkup(
      createElement(Studio, {
        host,
        mailbox,
        composer,
        setup: () => {},
        manageAccounts: () => {},
      }),
    );
  host.setSelection({ identity: { account: 'demo', folder: 'INBOX', uid: '1' }, subject: 's' });
  expect(host.isEnabled('online')).toBe(true);
  expect(host.isEnabled('online-message-selected')).toBe(true);
  expect(renderShell()).not.toContain('版本不同');

  bridge.snapshotHandler?.({ ...online, compat: 'kernel_outdated' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  expect(host.isEnabled('online')).toBe(false);
  expect(host.isEnabled('online-message-selected')).toBe(false);
  expect(host.isEnabled('message-selected')).toBe(true);
  expect(renderShell()).toContain('界面与后台版本不同');

  bridge.snapshotHandler?.({ ...online, revision: 0, compat: 'compatible' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  // 不同连接 epoch 且 revision 未推进的过期帧不得借同 revision 通道覆盖当前状态。
  bridge.snapshotHandler?.({ ...online, connection_epoch: 0, compat: 'compatible' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  host.state.patch({ snapshot: { ...online, compat: 'app_outdated' } });
  expect(renderShell()).toContain('界面与后台版本不同');
  // 英文界面：横幅随协商语言成文。
  applyEffectiveLocales(['en']);
  try {
    expect(renderShell()).toContain('The interface and the background service are different versions');
  } finally {
    applyEffectiveLocales([]);
  }
  host.state.patch({ snapshot: { ...online, compat: 'unknown' } });
  expect(renderShell()).not.toContain('版本不同');
  composer.stop();
  mailbox.stop();
  host.stop();
});

it('start 期间先收到握手推送时，迟到的首读快照不会把结论覆盖回 unknown', async () => {
  const bridge = new FakeBridge();
  let resolveInitial!: (snapshot: typeof online) => void;
  bridge.snapshot = () =>
    new Promise((resolve) => {
      resolveInitial = resolve;
    });
  const host = new Host(bridge);
  core(host);
  const starting = host.start();
  await new Promise((resolve) => setTimeout(resolve, 0));
  bridge.snapshotHandler?.({ ...online, compat: 'kernel_outdated' });
  resolveInitial({ ...online, compat: 'unknown' });
  await starting;
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  host.stop();
});

it('mailbox write contributions need a selection and a live daemon connection',()=>{
 const host=new Host(new FakeBridge());host.setSelection({identity:{account:'a',folder:'Inbox',uid:'1'},subject:'test'});
 expect(host.isEnabled('online-message-selected')).toBe(false);host.state.patch({snapshot:{...host.state.getSnapshot().snapshot,phase:'online'}});expect(host.isEnabled('online-message-selected')).toBe(true);host.setSelection(null);expect(host.isEnabled('online-message-selected')).toBe(false);
});

describe('Host.getContextSnapshot() 只读语义上下文与防泄漏边界', () => {
  it('无模块提供者时返回基础冻结快照，并随选中和视图更新', () => {
    const host = new Host(new FakeBridge());
    core(host);
    host.state.patch({
      snapshot: { ...online, principal: 'frontend-secret-principal-xyz' },
    });
    host.setSelection({
      identity: { account: 'demo', folder: 'INBOX', uid: '42' },
      subject: 'Private Subject Should Not Leak',
    });

    const snap = host.getContextSnapshot();
    expect(Object.isFrozen(snap)).toBe(true);
    expect(Object.isFrozen(snap.accountIds)).toBe(true);
    expect(Object.isFrozen(snap.selectedMessage)).toBe(true);
    expect(snap).toEqual({
      activeViewId: 'maild.mailbox.workspace',
      online: true,
      phase: 'online',
      compat: 'compatible',
      accountIds: ['demo'],
      selectedMessage: { account: 'demo', folder: 'INBOX', uid: '42' },
      selectedThreadId: null,
      checkedMessages: [],
      scope: null,
      activeDraftId: null,
      activeSettingsSection: null,
      theme: snap.theme,
      density: snap.density,
      mailbox: null,
      compose: null,
      settings: null,
    });
    const serialized = JSON.stringify(snap);
    expect(serialized).not.toContain('frontend-secret-principal-xyz');
    expect(serialized).not.toContain('Private Subject Should Not Leak');
  });

  it('汇总邮箱/写信/设置模块上下文并严格排除凭据、密码、正文与原始 HTML', async () => {
    vi.stubGlobal('window', {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });
    const bridge = new FakeBridge();
    const host = new Host(bridge);
    const composer = new ComposeController(bridge.compose, (id) => host.openView(id), () => {});
    const settings = new SettingsController(bridge.settings, () => {});
    const mailbox = studioMailbox(host, composer, () => {});
    const bundles = [
      mailbox.bundle,
      composeModule(host, composer),
      settingsModule(host, settings),
    ];

    try {
      bundles.forEach((b) => host.registerBuiltin(b));
      host.finishAssembly();
      host.state.patch({
        snapshot: {
          ...online,
          principal: 'token-principal-never-leak',
        },
      });
      await flush();

      // Resolve initial folders and rows.
      for (const req of bridge.folderRequests) {
        req.d.resolve([folder('INBOX', 'inbox', 1), folder('Archive', 'archive')]);
      }
      const row = {
        ...summary('INBOX', '7', 'Confidential payroll'),
        sender: 'payroll@example.test',
        snippet: 'Sensitive snippet content',
        thread_id: 99,
      };
      bridge.mailbox.queries.at(-1)?.d.resolve({ messages: [row], total: 1 });
      await flush();

      mailbox.controller.selectMessage(row);
      bridge.detailRequests.at(-1)?.d.resolve(
        message('INBOX', '7', {
          thread_id: 99,
          body: 'Top-secret plaintext body <script>alert(1)</script>',
          has_html: true,
        }),
      );
      await flush();
      mailbox.controller.toggle(row);

      // Populate draft with body text and settings with secrets & proxy password.
      await composer.create('bob@example.test');
      composer.change('body', '<html><body>Secret draft body</body></html>');
      settings.beginAccount(account());
      settings.setSecret('incoming', 'imap-app-password-9999');
      settings.store.patch({
        proxyDraft: {
          mode: 'custom',
          protocol: 'socks5',
          host: '127.0.0.1',
          port: '1080',
          authEnabled: true,
          username: 'proxy-user',
          password: 'proxy-secret-password-8888',
          clearPassword: false,
          bypassText: '',
        },
      });

      const snap = host.getContextSnapshot();
      expect(snap.activeViewId).toBe(COMPOSE_VIEW);
      expect(snap.selectedMessage).toEqual({ account: 'demo', folder: 'INBOX', uid: '7' });
      expect(snap.selectedThreadId).toBe(99);
      expect(snap.checkedMessages).toEqual([{ account: 'demo', folder: 'INBOX', uid: '7' }]);
      expect(snap.scope).toEqual({ kind: 'smart', smartFolder: 'inbox', unreadOnly: false });
      expect(snap.activeDraftId).toBe('draft-1');
      expect(snap.activeSettingsSection).toBe('maild.settings.accounts');
      expect(snap.compose).toMatchObject({ draftId: 'draft-1', dirty: true });
      expect(snap.settings).toEqual({ section: 'maild.settings.accounts', dirty: true });

      // Search scope summary captures query and account/folder filters cleanly.
      mailbox.controller.search({ query: 'quarterly', account: 'demo', folder: 'Archive' });
      expect(host.getContextSnapshot().scope).toEqual({
        kind: 'search',
        account: 'demo',
        folder: 'Archive',
        searchQuery: 'quarterly',
      });

      // Unread tab scope summary captures unreadOnly flag.
      mailbox.controller.exitSearch();
      mailbox.controller.setUnreadFilter(true);
      expect(host.getContextSnapshot().scope).toEqual({
        kind: 'unread',
        smartFolder: 'inbox',
        unreadOnly: true,
      });

      // Switching active view updates activeViewId cleanly.
      settings.discard();
      host.openView(SETTINGS_VIEW);
      expect(host.getContextSnapshot().activeViewId).toBe(SETTINGS_VIEW);

      // Verify NO secrets, credentials, principal, email body, or raw HTML leak anywhere in snapshot.
      const serialized = JSON.stringify(host.getContextSnapshot());
      for (const forbidden of [
        'imap-app-password-9999',
        'proxy-secret-password-8888',
        'token-principal-never-leak',
        'Top-secret plaintext body',
        'Secret draft body',
        '<script>',
        '<html>',
        'Sensitive snippet content',
        'Confidential payroll',
      ]) {
        expect(serialized).not.toContain(forbidden);
      }

      // Verify compose built-in commands carry risk metadata and still execute with zero extra args.
      expect(host.registry.findCommand('maild.compose.new')?.risk).toBe('stage');
      expect(host.registry.findCommand('maild.compose.open-drafts')?.risk).toBe('read');
      expect(host.registry.findCommand('maild.compose.reply')?.risk).toBe('stage');
    } finally {
      bundles.forEach((b) => b.dispose?.());
      host.stop();
      vi.unstubAllGlobals();
    }

    // After dispose, module providers are cleaned up.
    expect(host.getContextSnapshot().mailbox).toBeNull();
    expect(host.getContextSnapshot().compose).toBeNull();
    expect(host.getContextSnapshot().settings).toBeNull();
  });

  it('白名单净化会剥离提供者意外混入的秘密与原始 HTML 字段，且坏提供者不拖垮宿主', () => {
    const host = new Host(new FakeBridge());
    core(host);

    const stopMailbox = host.registerContextProvider(
      'mailbox',
      () =>
        ({
          scope: { kind: 'folder', account: 'demo', folder: 'INBOX', rawHtml: '<b>leak</b>' },
          selectedMessage: { account: 'demo', folder: 'INBOX', uid: '1', body_html: '<p>secret</p>' },
          selectedThreadId: 12,
          checkedMessages: [],
          threadOpen: false,
          totalMessages: 3,
          secrets: { incoming: 'leaked-pass' },
          html: '<div>raw</div>',
        }) as unknown as HostMailboxContext,
    );
    const stopCompose = host.registerContextProvider('compose', () => {
      throw new Error('provider crashed');
    });

    const snap = host.getContextSnapshot();
    expect(snap.selectedMessage).toEqual({ account: 'demo', folder: 'INBOX', uid: '1' });
    expect(snap.scope).toEqual({ kind: 'folder', account: 'demo', folder: 'INBOX' });
    expect(snap.compose).toBeNull();
    const serialized = JSON.stringify(snap);
    expect(serialized).not.toContain('leaked-pass');
    expect(serialized).not.toContain('rawHtml');
    expect(serialized).not.toContain('body_html');
    expect(serialized).not.toContain('<div>raw</div>');

    stopMailbox();
    stopCompose();
  });
});
