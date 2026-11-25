import { m } from '@featherpost/i18n/messages';

import type { MailBridge, MessageIdentity, OpenIntent, UiFrontendInfo, UiSnapshot } from '../bridge/types';
import { errorCode } from '../bridge/errors';
import { compatBlocksWrites } from '../bridge/types';
import type { Preferences } from '../preferences';
import { preferenceStorage, readPreferences, savePreferences } from '../preferences';
import { ContributionRegistry, copyContribution } from './registry';
import { Store } from './store';
import type {
  Bundle,
  CommandContext,
  EnabledWhen,
  HostComposeContext,
  HostContextProviders,
  HostContextSnapshot,
  HostMailboxContext,
  HostScopeSummary,
  HostSettingsContext,
  View,
} from './types';

const VALID_SCOPE_KINDS = new Set<HostScopeSummary['kind']>(['smart', 'folder', 'unread', 'search']);
const VALID_SMART_FOLDERS = new Set<NonNullable<HostScopeSummary['smartFolder']>>([
  'inbox',
  'all',
  'unread',
  'starred',
]);
const VALID_SEND_STATES = new Set<NonNullable<HostComposeContext['sendState']>>([
  'editing',
  'failed',
  'sending',
  'unknown',
  'sent',
]);

function sanitizeIdentity(value: MessageIdentity | null | undefined): Readonly<MessageIdentity> | null {
  if (!value || typeof value.account !== 'string' || typeof value.folder !== 'string' || typeof value.uid !== 'string') {
    return null;
  }
  return Object.freeze({
    account: value.account,
    folder: value.folder,
    uid: value.uid,
  });
}

function sanitizeScope(scope: HostScopeSummary | null | undefined): Readonly<HostScopeSummary> | null {
  if (!scope || !VALID_SCOPE_KINDS.has(scope.kind)) return null;
  const clean: HostScopeSummary = { kind: scope.kind };
  if (typeof scope.account === 'string' && scope.account) clean.account = scope.account;
  if (Array.isArray(scope.accounts)) {
    clean.accounts = Object.freeze(scope.accounts.filter((id): id is string => typeof id === 'string'));
  }
  if (typeof scope.folder === 'string' && scope.folder) clean.folder = scope.folder;
  if (scope.smartFolder && VALID_SMART_FOLDERS.has(scope.smartFolder)) clean.smartFolder = scope.smartFolder;
  if (typeof scope.groupId === 'string' && scope.groupId) clean.groupId = scope.groupId;
  if (typeof scope.groupName === 'string' && scope.groupName) clean.groupName = scope.groupName;
  if (typeof scope.unreadOnly === 'boolean') clean.unreadOnly = scope.unreadOnly;
  if (typeof scope.searchQuery === 'string' && scope.searchQuery) clean.searchQuery = scope.searchQuery;
  return Object.freeze(clean);
}

function sanitizeMailboxContext(raw: HostMailboxContext | null | undefined): Readonly<HostMailboxContext> | null {
  if (!raw || typeof raw !== 'object') return null;
  const checked = Array.isArray(raw.checkedMessages)
    ? raw.checkedMessages
        .map((item) => sanitizeIdentity(item))
        .filter((item): item is Readonly<MessageIdentity> => item !== null)
    : [];
  const selectedThreadId =
    typeof raw.selectedThreadId === 'number' && Number.isInteger(raw.selectedThreadId) && raw.selectedThreadId > 0
      ? raw.selectedThreadId
      : null;
  return Object.freeze({
    scope: sanitizeScope(raw.scope),
    selectedMessage: sanitizeIdentity(raw.selectedMessage),
    selectedThreadId,
    checkedMessages: Object.freeze(checked),
    threadOpen: Boolean(raw.threadOpen),
    totalMessages: typeof raw.totalMessages === 'number' && Number.isFinite(raw.totalMessages) ? Math.max(0, Math.trunc(raw.totalMessages)) : 0,
  });
}

function sanitizeComposeContext(raw: HostComposeContext | null | undefined): Readonly<HostComposeContext> | null {
  if (!raw || typeof raw !== 'object') return null;
  const draftId = typeof raw.draftId === 'string' && raw.draftId ? raw.draftId : null;
  const sendState = raw.sendState && VALID_SEND_STATES.has(raw.sendState) ? raw.sendState : null;
  return Object.freeze({
    draftId,
    sendState,
    inline: Boolean(raw.inline),
    collapsed: Boolean(raw.collapsed),
    dirty: Boolean(raw.dirty),
  });
}

function sanitizeSettingsContext(raw: HostSettingsContext | null | undefined): Readonly<HostSettingsContext> | null {
  if (!raw || typeof raw !== 'object') return null;
  return Object.freeze({
    section: typeof raw.section === 'string' && raw.section ? raw.section : null,
    dirty: Boolean(raw.dirty),
  });
}

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
  private contextProviders = new Map<keyof HostContextProviders, () => unknown>();

  constructor(readonly bridge: MailBridge) {}

  /** 内置模块：注册失败是宿主 bug，直接抛——没有邮箱视图的壳不该起来。 */
  registerBuiltin(bundle: Bundle) {
    this.registry.register(copyContribution(bundle, { builtin: true }));
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
        this.diagnose(
          m.extensions_host_enable_failed({
            id: moduleId,
            reason: error instanceof Error ? error.message : String(error),
          }),
        );
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
      this.diagnose(m.extensions_host_view_missing({ id }));
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
      this.diagnose(
        m.extensions_host_command_failed({
          id: commandId,
          reason: error instanceof Error ? error.message : String(error),
        }),
      );
      this.notify(m.extensions_host_command_failed_toast());
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

  /** 注册非秘密只读上下文提供者（邮箱范围/选中线程、草稿 ID、设置分区），返回注销函数。 */
  registerContextProvider<K extends keyof HostContextProviders>(
    key: K,
    provider: () => HostContextProviders[K] | null,
  ): () => void {
    this.contextProviders.set(key, provider);
    return () => {
      if (this.contextProviders.get(key) === provider) {
        this.contextProviders.delete(key);
      }
    };
  }

  /**
   * 返回只读、经过白名单净化的 UI 上下文快照。
   * 绝不包含任何凭据、密码、令牌、身份主体、邮件正文或原始 HTML。
   */
  getContextSnapshot(): HostContextSnapshot {
    const state = this.state.getSnapshot();
    const readProvider = <K extends keyof HostContextProviders>(key: K): HostContextProviders[K] | null => {
      const provider = this.contextProviders.get(key);
      if (!provider) return null;
      try {
        return (provider() as HostContextProviders[K] | null | undefined) ?? null;
      } catch {
        return null;
      }
    };

    const mailbox = sanitizeMailboxContext(readProvider('mailbox'));
    const compose = sanitizeComposeContext(readProvider('compose'));
    const settings = sanitizeSettingsContext(readProvider('settings'));
    const selectedMessage = mailbox?.selectedMessage ?? sanitizeIdentity(state.selection?.identity);
    const checkedMessages = mailbox?.checkedMessages ?? Object.freeze([]);
    const accountIds = Object.freeze(
      state.snapshot.accounts
        .map((item) => item.account)
        .filter((account): account is string => typeof account === 'string'),
    );

    return Object.freeze({
      activeViewId: state.activeViewId,
      online: state.snapshot.phase === 'online',
      phase: state.snapshot.phase,
      compat: state.snapshot.compat,
      accountIds,
      selectedMessage,
      selectedThreadId: mailbox?.selectedThreadId ?? null,
      checkedMessages,
      scope: mailbox?.scope ?? null,
      activeDraftId: compose?.draftId ?? null,
      activeSettingsSection: settings?.section ?? null,
      theme: state.preferences.theme,
      density: state.preferences.density,
      mailbox,
      compose,
      settings,
    });
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
      this.diagnose(m.extensions_host_subscribe_failed());
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
        frontendInfoError:
          errorCode(error) === 'denied'
            ? m.extensions_host_frontend_info_denied()
            : m.extensions_host_frontend_info_failed(),
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
