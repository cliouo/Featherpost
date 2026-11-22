import { applyEffectiveLocales } from '@featherpost/i18n';
import { describe, expect, it } from 'vitest';

import { Host } from '../src/host/host';
import type { View } from '../src/host/types';
import { compatBanner } from '../src/app/App';
import { FakeBridge, online } from './fakeBridge';

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
  core(host);
  await host.start();
  host.setSelection({ identity: { account: 'demo', folder: 'INBOX', uid: '1' }, subject: 's' });
  expect(host.isEnabled('online')).toBe(true);
  expect(host.isEnabled('online-message-selected')).toBe(true);
  expect(compatBanner(host.state.getSnapshot().snapshot)).toBeNull();

  bridge.snapshotHandler?.({ ...online, compat: 'kernel_outdated' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  expect(host.isEnabled('online')).toBe(false);
  expect(host.isEnabled('online-message-selected')).toBe(false);
  expect(host.isEnabled('message-selected')).toBe(true);
  expect(compatBanner(host.state.getSnapshot().snapshot)).toMatch(/修复与后台管理/);

  bridge.snapshotHandler?.({ ...online, revision: 0, compat: 'compatible' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  // 不同连接 epoch 且 revision 未推进的过期帧不得借同 revision 通道覆盖当前状态。
  bridge.snapshotHandler?.({ ...online, connection_epoch: 0, compat: 'compatible' });
  expect(host.state.getSnapshot().snapshot.compat).toBe('kernel_outdated');
  expect(compatBanner({ compat: 'app_outdated' })).toMatch(/界面版本较旧/);
  expect(compatBanner({ compat: 'unknown' })).toBeNull();
  // 英文界面：横幅随协商语言成文，菜单名与 maild-l10n 的英文菜单一致。
  applyEffectiveLocales(['en']);
  try {
    expect(compatBanner({ compat: 'kernel_outdated' })).toMatch(/Repair & Background…/);
    expect(compatBanner({ compat: 'app_outdated' })).toMatch(/^This interface is older/);
  } finally {
    applyEffectiveLocales([]);
  }
  expect(compatBanner({ compat: 'app_outdated' })).toMatch(/界面版本较旧/);
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
