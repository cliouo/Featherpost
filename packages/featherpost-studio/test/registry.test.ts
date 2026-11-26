import { applyEffectiveLocales } from '@featherpost/i18n';
import { afterEach, describe, expect, it } from 'vitest';

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

describe('贡献标题与诊断随界面语言（ADR-0048 决策 8）', () => {
  afterEach(() => {
    applyEffectiveLocales([]);
  });

  it('标题写成 getter 时注册表保留访问器，快照随语言重新成文', () => {
    const registry = new ContributionRegistry<View>();
    let lang = 'zh';
    registry.register({
      ...mailbox(),
      get title() {
        return lang === 'zh' ? '邮箱' : 'Mailbox';
      },
      views: [
        {
          id: 'maild.mailbox.workspace',
          get title() {
            return lang === 'zh' ? '邮件' : 'Mail';
          },
          role: 'mailbox',
          render: view('mailbox'),
        },
      ],
    });
    expect(registry.getSnapshot().modules[0].title).toBe('邮箱');
    lang = 'en';
    expect(registry.getSnapshot().modules[0].title).toBe('Mailbox');
    expect(registry.findView('maild.mailbox.workspace')?.title).toBe('Mail');
  });

  it('校验失败的说明按当前语言成文', () => {
    const registry = new ContributionRegistry<View>();
    registry.register(mailbox());
    applyEffectiveLocales(['en']);
    expect(() => registry.register(mailbox())).toThrow('Module already registered: maild.mailbox');
    applyEffectiveLocales([]);
    expect(() => registry.register(mailbox())).toThrow('模块已注册: maild.mailbox');
  });
});

describe('CommandContribution 可选语义元数据（向后兼容）', () => {
  it('保留可选 description（含 getter）、inputSchema（防御性克隆）与 risk，且零参数命令保持兼容', () => {
    const registry = new ContributionRegistry<View>();
    let locale = 'zh';
    const schema = {
      type: 'object',
      properties: { folder: { type: 'string' } },
      required: ['folder'],
    };
    registry.register({
      moduleId: 'org.example.semantic',
      apiVersion: 1,
      title: 'semantic',
      commands: [
        {
          id: 'org.example.semantic.legacy',
          title: 'Legacy',
          locations: ['menu'],
          execute: () => {},
        },
        {
          id: 'org.example.semantic.move',
          title: 'Move',
          get description() {
            return locale === 'zh' ? '移动邮件' : 'Move message';
          },
          inputSchema: schema,
          risk: 'write',
          locations: ['toolbar', 'menu'],
          execute: () => {},
        },
      ],
    });

    const legacy = registry.findCommand('org.example.semantic.legacy');
    expect(legacy).toBeDefined();
    expect(legacy?.description).toBeUndefined();
    expect(legacy?.inputSchema).toBeUndefined();
    expect(legacy?.risk).toBeUndefined();

    const move = registry.findCommand('org.example.semantic.move');
    expect(move?.risk).toBe('write');
    expect(move?.description).toBe('移动邮件');
    locale = 'en';
    expect(move?.description).toBe('Move message');

    // Mutating the caller's original schema object must not mutate the registered descriptor.
    (schema.properties as Record<string, unknown>).folder = { type: 'number' };
    expect(move?.inputSchema).toEqual({
      type: 'object',
      properties: { folder: { type: 'string' } },
      required: ['folder'],
    });
  });

  it('拒绝非法 risk、非对象 inputSchema 或非字符串 description', () => {
    const registry = new ContributionRegistry<View>();
    registry.register(mailbox());
    const before = registry.getSnapshot();

    for (const badCommand of [
      { id: 'org.example.bad.r', title: 'r', locations: ['menu'], risk: 'sudo', execute: () => {} },
      { id: 'org.example.bad.s', title: 's', locations: ['menu'], inputSchema: ['not-an-object'], execute: () => {} },
      { id: 'org.example.bad.d', title: 'd', locations: ['menu'], description: 123, execute: () => {} },
      { id: 'org.example.bad.l', title: 'l', locations: ['sidebar'], execute: () => {} },
      { id: 'org.example.bad.w', title: 'w', locations: ['palette'], enabledWhen: 'always-yes', execute: () => {} },
      { id: 'org.example.bad.c', title: 'c', locations: ['palette'], canExecute: true, execute: () => {} },
      { id: 'org.example.bad.k1', title: 'k1', locations: ['palette'], keywords: 'not-array', execute: () => {} },
      { id: 'org.example.bad.k2', title: 'k2', locations: ['palette'], keywords: ['ok', '   '], execute: () => {} },
      { id: 'org.example.bad.k3', title: 'k3', locations: ['palette'], keywords: [123], execute: () => {} },
      { id: 'org.example.bad.sc1', title: 'sc1', locations: ['palette'], shortcut: '   ', execute: () => {} },
      { id: 'org.example.bad.sc2', title: 'sc2', locations: ['palette'], shortcut: 'mod+', execute: () => {} },
      { id: 'org.example.bad.sc3', title: 'sc3', locations: ['palette'], shortcut: 42, execute: () => {} },
    ]) {
      expect(() =>
        registry.register({
          moduleId: 'org.example.bad',
          apiVersion: 1,
          title: 'bad',
          commands: [badCommand as unknown as NonNullable<Contributions<View>['commands']>[number]],
        }),
      ).toThrow();
    }
    expect(registry.getSnapshot()).toBe(before);
  });

  it('支持 palette 位置、新增 EnabledWhen、防御性拷贝 keywords/归一化 shortcut，并保留访问器', () => {
    const registry = new ContributionRegistry<View>();
    let locale = 'zh';
    const plainKeywords = ['写信', 'compose'];
    registry.register({
      moduleId: 'org.example.palette',
      apiVersion: 1,
      title: 'palette',
      commands: [
        {
          id: 'org.example.palette.plain',
          title: 'Compose',
          locations: ['palette'],
          enabledWhen: 'online-targets',
          keywords: plainKeywords,
          shortcut: ' Mod + N ',
          execute: () => {},
        },
        {
          id: 'org.example.palette.dynamic',
          title: 'Settings',
          locations: ['navigation', 'palette'],
          enabledWhen: 'checked-messages',
          get keywords() {
            return locale === 'zh' ? ['设置'] : ['settings'];
          },
          get shortcut() {
            return 'mod+,';
          },
          execute: () => {},
        },
        {
          id: 'org.example.palette.targets',
          title: 'Targets',
          locations: ['palette'],
          enabledWhen: 'targets',
          execute: () => {},
        },
      ],
    });

    const plain = registry.findCommand('org.example.palette.plain');
    expect(plain?.locations).toEqual(['palette']);
    expect(plain?.enabledWhen).toBe('online-targets');
    expect(plain?.shortcut).toBe('mod+n');
    expect(plain?.keywords).toEqual(['写信', 'compose']);

    // Mutating caller's plain array must not mutate registered keywords.
    plainKeywords.push('injected');
    expect(plain?.keywords).toEqual(['写信', 'compose']);

    // Getter keywords stay live across locale changes.
    const dynamic = registry.findCommand('org.example.palette.dynamic');
    expect(dynamic?.keywords).toEqual(['设置']);
    expect(dynamic?.shortcut).toBe('mod+,');
    locale = 'en';
    expect(dynamic?.keywords).toEqual(['settings']);

    expect(registry.findCommand('org.example.palette.targets')?.enabledWhen).toBe('targets');
  });
});
