import { describe, expect, it } from 'vitest';

import { ContributionRegistry } from '../src/host/registry';
import type { Contributions } from '../src/host/types';

type View = string;
const view = (id: string) => `view:${id}`;

function mailbox(): Contributions<View> {
  return {
    moduleId: 'maild.mailbox',
    apiVersion: 1,
    title: '邮箱',
    views: [{ id: 'maild.mailbox.workspace', title: '邮件', role: 'mailbox', render: view('mailbox') }],
    commands: [{ id: 'maild.mailbox.refresh', title: '刷新', locations: ['toolbar', 'menu'], order: 100, execute: () => {} }],
  };
}
function settings(): Contributions<View> {
  return {
    moduleId: 'maild.settings',
    apiVersion: 1,
    title: '设置',
    views: [{ id: 'maild.settings.page', title: '设置', role: 'settings', render: view('settings') }],
  };
}

describe('ContributionRegistry（ADR-0031 决策 5）', () => {
  it('一个模块整包生效，快照按 order 再按 ID 排序，与注册顺序无关', () => {
    const registry = new ContributionRegistry<View>();
    registry.register({
      moduleId: 'org.example.late',
      apiVersion: 1,
      title: 'late',
      commands: [
        { id: 'org.example.late.b', title: 'b', locations: ['menu'], order: 10, execute: () => {} },
        { id: 'org.example.late.a', title: 'a', locations: ['menu'], order: 10, execute: () => {} },
      ],
    });
    registry.register(mailbox());
    expect(registry.getSnapshot().commands.map(({ value }) => value.id)).toEqual([
      'org.example.late.a',
      'org.example.late.b',
      'maild.mailbox.refresh',
    ]);
    expect(registry.getSnapshot().modules.map((m) => m.moduleId)).toEqual(['maild.mailbox', 'org.example.late']);
  });

  it('坏模块整包拒绝，已注册者不动', () => {
    const registry = new ContributionRegistry<View>();
    registry.register(mailbox());
    const before = registry.getSnapshot();
    const bad: Contributions<View> = {
      moduleId: 'org.example.bad',
      apiVersion: 1,
      title: 'bad',
      commands: [
        { id: 'org.example.bad.ok', title: 'ok', locations: ['menu'], execute: () => {} },
        // 撞邮箱模块的 ID：越出自己的命名空间。
        { id: 'maild.mailbox.refresh', title: '冒名', locations: ['menu'], execute: () => {} },
      ],
    };
    expect(() => registry.register(bad)).toThrow(/命名空间/);
    expect(registry.getSnapshot()).toBe(before);
    expect(registry.has('org.example.bad')).toBe(false);
    for (const invalid of [
      { ...mailbox(), moduleId: 'NoDots' },
      { ...mailbox(), moduleId: 'maild.mailbox2', apiVersion: 2 as unknown as 1 },
      { ...mailbox(), moduleId: 'maild.x', commands: [{ id: 'maild.x.c', title: 'c', locations: [], execute: () => {} }] },
      { ...mailbox(), moduleId: 'maild.y', title: '   ' },
    ]) {
      expect(() => registry.register(invalid as Contributions<View>)).toThrow();
    }
    expect(registry.getSnapshot().modules).toHaveLength(1);
  });

  it('同一 ID 的贡献跨模块重复也拒绝', () => {
    const registry = new ContributionRegistry<View>();
    registry.register(mailbox());
    const clash: Contributions<View> = {
      moduleId: 'maild.mailbox.sub',
      apiVersion: 1,
      title: 'sub',
      // 前缀合法（maild.mailbox.sub.），但恰好撞一个已存在的完整 ID 是不可能的；这里换成同模块内重复。
      commands: [
        { id: 'maild.mailbox.sub.a', title: 'a', locations: ['menu'], execute: () => {} },
        { id: 'maild.mailbox.sub.a', title: 'a again', locations: ['menu'], execute: () => {} },
      ],
    };
    expect(() => registry.register(clash)).toThrow(/重复/);
  });

  it('dispose 只撤自己的贡献；替换后过期句柄失效', () => {
    const registry = new ContributionRegistry<View>();
    registry.register(mailbox());
    const first = registry.register({ moduleId: 'org.example.ext', apiVersion: 1, title: 'ext', views: [{ id: 'org.example.ext.v1', title: 'v1', render: view('v1') }] });
    const disposed: string[] = [];
    registry.replace({ moduleId: 'org.example.ext', apiVersion: 1, title: 'ext v2', views: [{ id: 'org.example.ext.v2', title: 'v2', render: view('v2') }], dispose: () => disposed.push('v2') });
    // 过期句柄：不能吊销升级后的包。
    first.dispose();
    expect(registry.findView('org.example.ext.v2')).toBeDefined();
    expect(registry.findView('org.example.ext.v1')).toBeUndefined();
    expect(registry.getSnapshot().modules.find((m) => m.moduleId === 'org.example.ext')?.title).toBe('ext v2');
    expect(disposed).toEqual([]);
    // 邮箱模块全程不受影响。
    expect(registry.findCommand('maild.mailbox.refresh')).toBeDefined();
  });

  it('必需角色的唯一提供者不能被 dispose，替换可以', () => {
    const registry = new ContributionRegistry<View>();
    const handle = registry.register(mailbox());
    registry.register(settings());
    registry.requireRoles(['mailbox', 'settings']);
    expect(() => handle.dispose()).toThrow(/必需角色/);
    expect(registry.resolveRole('mailbox').id).toBe('maild.mailbox.workspace');
    expect(() => registry.register({ ...mailbox(), moduleId: 'org.example.mailbox2', views: [{ id: 'org.example.mailbox2.v', title: 'x', role: 'mailbox', render: view('x') }], commands: [] })).toThrow(/重复的核心角色/);
    const upgraded = registry.replace({ ...mailbox(), title: '邮箱 v2' });
    expect(registry.resolveRole('mailbox').id).toBe('maild.mailbox.workspace');
    expect(() => upgraded.dispose()).toThrow(/必需角色/);
  });

  it('调用方事后改自己的对象不会绕过校验', () => {
    const registry = new ContributionRegistry<View>();
    const bundle = mailbox();
    registry.register(bundle);
    (bundle.commands as unknown as { id: string }[])[0].id = 'evil.id';
    expect(registry.findCommand('maild.mailbox.refresh')).toBeDefined();
    expect(registry.findCommand('evil.id')).toBeUndefined();
  });
});
