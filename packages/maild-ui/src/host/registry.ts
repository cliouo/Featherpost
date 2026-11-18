import type {
  CommandContribution, Contributions, CoreRole, RegisteredContribution, RegistrySnapshot,
  SettingsContribution, ViewContribution,
} from './types';

interface Entry<View> {
  token: symbol;
  contributions: Contributions<View>;
}

export interface Disposable {
  dispose(): void;
}

const MODULE_ID = /^[a-z][a-z0-9]*(?:\.[a-z][a-z0-9-]*)+$/;
const CONTRIBUTION_SUFFIX = /^[a-z][a-z0-9.-]*$/;

/**
 * 可信、编译期模块的贡献注册表（ADR-0031 决策 5）。第三方经 E02 的独立 broker 进入，不直接
 * register。一个模块的贡献整包校验、整包生效；校验失败整包拒绝而已注册者不动；dispose 句柄按
 * token 作用域，过期句柄吊销不了后来者的替换。
 */
export class ContributionRegistry<View> {
  private modules = new Map<string, Entry<View>>();
  private required = new Set<CoreRole>();
  private listeners = new Set<() => void>();
  private snapshot: RegistrySnapshot<View> = { modules: [], commands: [], views: [], settings: [] };

  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  register(bundle: Contributions<View>): Disposable {
    if (this.modules.has(bundle.moduleId)) throw new Error(`模块已注册: ${bundle.moduleId}`);
    return this.commit(bundle, false);
  }

  /** 升级：先校验新贡献与现有集合的兼容性，再整体切换，旧包的 dispose 最后跑。 */
  replace(bundle: Contributions<View>): Disposable {
    if (!this.modules.has(bundle.moduleId)) throw new Error(`模块未注册: ${bundle.moduleId}`);
    return this.commit(bundle, true);
  }

  has(moduleId: string): boolean {
    return this.modules.has(moduleId);
  }

  /** 宿主声明必需角色：没有替代者时不能停用仍被使用的角色。 */
  requireRoles(roles: readonly CoreRole[]) {
    const required = new Set(roles);
    this.validateRoles(this.modules, required);
    this.required = required;
  }

  resolveRole(role: CoreRole): ViewContribution<View> {
    const matches = this.snapshot.views.filter(({ value }) => value.role === role);
    if (matches.length !== 1) throw new Error(`角色必须恰有一个提供者: ${role}`);
    return matches[0].value;
  }

  findCommand(id: string): CommandContribution | undefined {
    return this.snapshot.commands.find(({ value }) => value.id === id)?.value;
  }

  findView(id: string): ViewContribution<View> | undefined {
    return this.snapshot.views.find(({ value }) => value.id === id)?.value;
  }

  private commit(bundle: Contributions<View>, replacing: boolean): Disposable {
    const token = Symbol(bundle.moduleId);
    // 复制描述符：调用方事后改自己的对象不能绕过校验。
    const stored: Contributions<View> = {
      ...bundle,
      commands: bundle.commands?.map((command) => ({ ...command, locations: [...command.locations] })),
      views: bundle.views?.map((view) => ({ ...view })),
      settings: bundle.settings?.map((section) => ({ ...section })),
    };
    const candidate = new Map(this.modules);
    const previous = candidate.get(bundle.moduleId);
    candidate.set(bundle.moduleId, { token, contributions: stored });
    this.validate(candidate);
    this.validateRoles(candidate, this.required);
    this.modules = candidate;
    this.publish();
    if (replacing) previous?.contributions.dispose?.();
    return {
      dispose: () => {
        if (this.modules.get(bundle.moduleId)?.token !== token) return;
        const next = new Map(this.modules);
        next.delete(bundle.moduleId);
        this.validateRoles(next, this.required);
        this.modules = next;
        this.publish();
        stored.dispose?.();
      },
    };
  }

  private validate(modules: Map<string, Entry<View>>) {
    const ids = new Set<string>();
    const roles = new Set<CoreRole>();
    for (const { contributions: bundle } of modules.values()) {
      if (!MODULE_ID.test(bundle.moduleId)) throw new Error(`模块命名空间无效: ${bundle.moduleId}`);
      if (bundle.apiVersion !== 1) throw new Error(`不支持的贡献 API 版本: ${bundle.moduleId}`);
      if (!bundle.title.trim()) throw new Error(`模块需要名字: ${bundle.moduleId}`);
      for (const value of [...(bundle.commands ?? []), ...(bundle.views ?? []), ...(bundle.settings ?? [])]) {
        const suffix = value.id.slice(bundle.moduleId.length + 1);
        if (!value.id.startsWith(`${bundle.moduleId}.`) || !CONTRIBUTION_SUFFIX.test(suffix))
          throw new Error(`贡献越出模块命名空间: ${value.id}`);
        if (ids.has(value.id)) throw new Error(`重复的贡献 ID: ${value.id}`);
        if (!value.title.trim()) throw new Error(`贡献需要标题: ${value.id}`);
        ids.add(value.id);
      }
      for (const command of bundle.commands ?? []) {
        if (command.locations.length === 0) throw new Error(`命令至少要有一个位置: ${command.id}`);
      }
      for (const view of bundle.views ?? []) {
        if (view.role && roles.has(view.role)) throw new Error(`重复的核心角色: ${view.role}`);
        if (view.role) roles.add(view.role);
      }
    }
  }

  private validateRoles(modules: Map<string, Entry<View>>, required: Set<CoreRole>) {
    const roles = new Set(
      [...modules.values()].flatMap(({ contributions }) =>
        (contributions.views ?? []).flatMap((view) => (view.role ? [view.role] : [])),
      ),
    );
    for (const role of required) {
      if (!roles.has(role)) throw new Error(`不能移除必需角色的唯一提供者: ${role}`);
    }
  }

  private publish() {
    const commands: RegisteredContribution<CommandContribution>[] = [];
    const views: RegisteredContribution<ViewContribution<View>>[] = [];
    const settings: RegisteredContribution<SettingsContribution<View>>[] = [];
    const modules: RegistrySnapshot<View>['modules'][number][] = [];
    for (const [moduleId, { contributions }] of this.modules) {
      for (const value of contributions.commands ?? []) commands.push({ moduleId, value });
      for (const value of contributions.views ?? []) views.push({ moduleId, value });
      for (const value of contributions.settings ?? []) settings.push({ moduleId, value });
      modules.push({
        moduleId,
        title: contributions.title,
        builtin: contributions.builtin ?? false,
        contributions:
          (contributions.commands?.length ?? 0) + (contributions.views?.length ?? 0) + (contributions.settings?.length ?? 0),
      });
    }
    // order 升序，相同按 ID 字典序：渲染结果不依赖注册顺序。
    const byOrder = <T extends { value: { order?: number; id: string } }>(a: T, b: T) =>
      (a.value.order ?? 0) - (b.value.order ?? 0) || a.value.id.localeCompare(b.value.id);
    this.snapshot = {
      modules: modules.sort((a, b) => a.moduleId.localeCompare(b.moduleId)),
      commands: commands.sort(byOrder),
      views: views.sort(byOrder),
      settings: settings.sort(byOrder),
    };
    for (const listener of this.listeners) listener();
  }
}
