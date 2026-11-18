import type { MailBridge, MessageIdentity, OpenIntent, UiFrontendInfo, UiSnapshot } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import { compatBlocksWrites } from '../bridge/types';
import type { Preferences } from '../preferences';
import { preferenceStorage, readPreferences, savePreferences } from '../preferences';
import { ContributionRegistry } from './registry';
import { Store } from './store';
import type { Bundle, CommandContext, EnabledWhen, View } from './types';

export interface HostState {
  activeViewId: string;
  snapshot: UiSnapshot;
  preferences: Preferences;
  toast: string | null;
  /** 注册失败等宿主诊断（「设置 → 扩展」展示）。邮箱照常启动。 */
  diagnostics: readonly string[];
  frontendInfo: UiFrontendInfo | null;
  frontendInfoError: string | null;
  /** 当前选中的邮件——命令的受限上下文之一。 */
  selection: { identity: MessageIdentity; subject: string } | null;
}

const initialSnapshot: UiSnapshot = {
  frontend_id: '',
  phase: 'disconnected',
  attempt: null,
  retry_in_ms: null,
  revision: -1,
  connection_epoch: 0,
  version: null,
  accounts: [],
  principal: null,
  identity_failure: null,
  partial: false,
  compat: 'unknown',
};

export type OptionalModule = {
  /** 停用时「设置 → 扩展」仍要显示人话名字，不能只剩 id。 */
  title: string;
  bundle: () => Bundle;
  defaultEnabled: boolean;
};

/**
 * 宿主：注册表 + 布局状态 + 命令上下文 + 可选模块开关。它不知道任何邮件领域逻辑——
 * 那些在模块里；它只保证坏模块不拖垮邮箱、命令只拿到受限上下文。
 */
export class Host {
  readonly registry = new ContributionRegistry<View>();
  readonly state = new Store<HostState>({
    activeViewId: '',
    snapshot: initialSnapshot,
    preferences: readPreferences(preferenceStorage()),
    toast: null,
    diagnostics: [],
    frontendInfo: null,
    frontendInfoError: null,
    selection: null,
  });
  private optional = new Map<string, { module: OptionalModule; handle: { dispose(): void } | null }>();
  private toastTimer: ReturnType<typeof setTimeout> | null = null;
  private lifecycle = 0;
  private navigationGuards = new Set<(id:string, next:()=>void,cancel:()=>void)=>boolean>();
  private unsubscribe: Array<() => void> = [];
  private intentHandlers = new Set<(intent: OpenIntent) => void>();

  constructor(readonly bridge: MailBridge) {}

  /** 内置模块：注册失败是宿主 bug，直接抛——没有邮箱视图的壳不该起来。 */
  registerBuiltin(bundle: Bundle) {
    this.registry.register({ ...bundle, builtin: true });
  }

  /** 可选模块：失败只记诊断，不影响其他模块（ADR-0031 决策 5「失败可见」）。 */
  registerOptional(moduleId: string, module: OptionalModule) {
    this.optional.set(moduleId, { module, handle: null });
    if (module.defaultEnabled) this.setModuleEnabled(moduleId, true);
  }

  setModuleEnabled(moduleId: string, enabled: boolean): boolean {
    const entry = this.optional.get(moduleId);
    if (!entry) return false;
    if (enabled && !entry.handle) {
      try {
        entry.handle = this.registry.register(entry.module.bundle());
      } catch (error) {
        this.diagnose(`扩展 ${moduleId} 未能启用：${error instanceof Error ? error.message : String(error)}`);
        return false;
      }
    } else if (!enabled && entry.handle) {
      const wasActive = this.registry
        .getSnapshot()
        .views.some(({ moduleId: owner, value }) => owner === moduleId && value.id === this.state.getSnapshot().activeViewId);
      entry.handle.dispose();
      entry.handle = null;
      // 停用无遗留：正在看的视图属于它，就回邮箱。
      if (wasActive) this.openRole('mailbox');
    }
    return true;
  }

  isModuleEnabled(moduleId: string): boolean {
    return this.optional.get(moduleId)?.handle != null;
  }

  optionalModules(): { moduleId: string; title: string; enabled: boolean }[] {
    return [...this.optional.entries()].map(([moduleId, entry]) => ({
      moduleId,
      title: entry.module.title,
      enabled: entry.handle != null,
    }));
  }

  finishAssembly() {
    this.registry.requireRoles(['mailbox', 'settings']);
    this.openRole('mailbox');
  }

  openRole(role: 'mailbox' | 'settings') {
    this.openView(this.registry.resolveRole(role).id);
  }

  beforeNavigate(guard:(id:string,next:()=>void,cancel:()=>void)=>boolean) {
    this.navigationGuards.add(guard);
    return ()=>{this.navigationGuards.delete(guard);};
  }

  approveNavigation(id:string):Promise<boolean> {
    return new Promise(resolve=>{
      if(id!==this.state.getSnapshot().activeViewId){
        for(const guard of this.navigationGuards) if(!guard(id,()=>resolve(true),()=>resolve(false)))return;
      }
      resolve(true);
    });
  }

  openView(id: string): boolean {
    if (!this.registry.findView(id)) {
      this.diagnose(`视图不存在：${id}`);
      return false;
    }
    if (id !== this.state.getSnapshot().activeViewId) {
      for (const guard of this.navigationGuards) if (!guard(id,()=>this.openView(id),()=>{})) return false;
    }
    this.state.patch({ activeViewId: id });
    return true;
  }

  execute(commandId: string) {
    const command = this.registry.findCommand(commandId);
    if (!command) return;
    if (!this.isEnabled(command.enabledWhen)) return;
    try {
      command.execute(this.commandContext());
    } catch (error) {
      this.diagnose(`命令 ${commandId} 执行失败：${error instanceof Error ? error.message : String(error)}`);
      this.notify('操作未完成。');
    }
  }

  isEnabled(when: EnabledWhen | undefined) {
    const state = this.state.getSnapshot();
    // 版本不一致时，「在线」类命令（写信、标记、归档等写入口）一律禁用；桥侧另有同一道闸门。
    const writable = state.snapshot.phase === 'online' && !compatBlocksWrites(state.snapshot);
    switch (when ?? 'always') {
      case 'online-message-selected':
        return state.selection !== null && writable;
      case 'message-selected':
        return state.selection !== null;
      case 'online':
        return writable;
      default:
        return true;
    }
  }

  setSelection(selection: HostState['selection']) {
    this.state.patch({ selection });
  }

  notify(text: string) {
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.state.patch({ toast: text });
    this.toastTimer = setTimeout(() => this.state.patch({ toast: null }), 3200);
  }

  setPreferences(preferences: Preferences) {
    this.state.patch({ preferences });
    savePreferences(preferences, preferenceStorage());
  }

  onIntent(handler: (intent: OpenIntent) => void) {
    this.intentHandlers.add(handler);
    return () => {
      this.intentHandlers.delete(handler);
    };
  }

  async start() {
    const lifecycle = ++this.lifecycle;
    if (!this.bridge.available) return;
    try {
      let pushed = false;
      const stopState = await this.bridge.subscribe((snapshot) => {
        if (lifecycle !== this.lifecycle) return;
        pushed = true;
        this.acceptSnapshot(snapshot);
      });
      const stopIntent = await this.bridge.onIntent((intent) => {
        if (lifecycle !== this.lifecycle) return;
        for (const handler of this.intentHandlers) handler(intent);
      });
      if (lifecycle !== this.lifecycle) {
        stopState();
        stopIntent();
        return;
      }
      this.unsubscribe.push(stopState, stopIntent);
      // 先订阅再读：若等待期间状态泵已推过帧，迟到的首读快照直接丢弃，避免覆盖同 revision 的握手补推。
      const initial = await this.bridge.snapshot();
      if (lifecycle === this.lifecycle && !pushed) {
        this.acceptSnapshot(initial);
      }
    } catch {
      this.diagnose('无法订阅后台连接状态。');
    }
  }

  stop() {
    this.lifecycle++;
    for (const stop of this.unsubscribe) stop();
    this.unsubscribe = [];
  }

  private acceptSnapshot(snapshot: UiSnapshot) {
    const current = this.state.getSnapshot().snapshot;
    if (snapshot.connection_epoch < current.connection_epoch) return;
    if (snapshot.connection_epoch === current.connection_epoch && snapshot.revision < current.revision) return;
    // 握手结论变化时 Rust 侧以同一 revision 补推一帧（单一推送任务，顺序单调），只认同一连接 epoch 内的结论变化。
    if (
      snapshot.connection_epoch === current.connection_epoch &&
      snapshot.revision === current.revision &&
      snapshot.compat === current.compat
    ) {
      return;
    }
    const cameOnline = snapshot.phase === 'online' && (current.phase !== 'online' || snapshot.connection_epoch !== current.connection_epoch);
    this.state.patch({ snapshot });
    if (cameOnline) void this.loadFrontendInfo();
  }

  private async loadFrontendInfo() {
    try {
      const info = await this.bridge.frontendInfo();
      this.state.patch({ frontendInfo: info, frontendInfoError: null });
    } catch (error) {
      this.state.patch({
        frontendInfoError: errorCode(error) === 'denied' ? '当前前端没有读取前端清单的授权。' : '暂时无法读取前端清单。',
      });
    }
  }

  private commandContext(): CommandContext {
    const state = this.state.getSnapshot();
    return {
      openView: (id) => this.openView(id),
      selection: state.selection?.identity ?? null,
      selectionSubject: state.selection?.subject ?? null,
      online: state.snapshot.phase === 'online',
      notify: (text) => this.notify(text),
    };
  }

  private diagnose(text: string) {
    const diagnostics = [...this.state.getSnapshot().diagnostics, text].slice(-20);
    this.state.patch({ diagnostics });
  }
}
