import type { ComponentType } from 'react';

import type { ConnectionPhase, MessageIdentity, UiCompat } from '../bridge/types';
import type { Preferences } from '../preferences';

export type CoreRole = 'mailbox' | 'settings' | 'compose' | 'drafts' | 'activity';
export type CommandLocation = 'navigation' | 'toolbar' | 'menu' | 'palette';
/** 受限的声明式条件（UI-EXTENSIONS §5）：只有必要的非秘密布尔上下文，不给插件推全量对象。 */
export type EnabledWhen =
  | 'always'
  | 'message-selected'
  | 'online-message-selected'
  | 'online'
  | 'targets'
  | 'online-targets'
  | 'checked-messages';
/** 声明式操作风险等级，供未来的语义动作编排/确认策略使用。 */
export type CommandRisk = 'read' | 'stage' | 'write' | 'destructive';
/** 轻量级 JSON Schema 描述符（零外部校验依赖）。 */
export type CommandInputSchema = Record<string, unknown>;
export type IconName =
  | 'inbox' | 'folder' | 'sent' | 'drafts' | 'archive' | 'trash' | 'spam' | 'star'
  | 'search' | 'sliders' | 'activity' | 'mail' | 'reply' | 'forward' | 'paperclip' | 'settings' | 'refresh' | 'more' | 'clip' | 'back' | 'puzzle' | 'keyboard';

/** 命令执行时宿主给的上下文；它是命令能知道的全部。 */
export interface CommandContext {
  openView(id: string): void;
  selection: MessageIdentity | null;
  selectionSubject: string | null;
  online: boolean;
  notify(text: string): void;
}

export interface CommandContribution {
  id: string;
  title: string;
  description?: string;
  inputSchema?: CommandInputSchema;
  risk?: CommandRisk;
  icon?: IconName;
  locations: readonly CommandLocation[];
  order?: number;
  enabledWhen?: EnabledWhen;
  /** 可选的细粒度可用性守卫（如混合账户禁移、同步中防重入）；与 enabledWhen 共同决定可见/可执行状态。 */
  canExecute?: (context: CommandContext) => boolean;
  /** 可选的双语搜索别名（支持 getter 以随界面语言更新）。 */
  keywords?: readonly string[];
  /** 仅用于展示/ARIA 的规范化快捷键元数据（如 'mod+n'、'mod+,'），不自动绑定热键。 */
  shortcut?: string;
  execute(context: CommandContext): void;
}

export interface HostScopeSummary {
  kind: 'smart' | 'folder' | 'unread' | 'search';
  account?: string;
  accounts?: readonly string[];
  folder?: string;
  smartFolder?: 'inbox' | 'all' | 'unread' | 'starred';
  groupId?: string;
  groupName?: string;
  unreadOnly?: boolean;
  searchQuery?: string;
}

export interface HostMailboxContext {
  scope: HostScopeSummary | null;
  selectedMessage: MessageIdentity | null;
  selectedThreadId: number | null;
  checkedMessages: readonly MessageIdentity[];
  threadOpen: boolean;
  totalMessages: number;
  busy?: boolean;
  targetMessages?: readonly MessageIdentity[];
}

export interface HostComposeContext {
  draftId: string | null;
  sendState: 'editing' | 'failed' | 'sending' | 'unknown' | 'sent' | null;
  inline: boolean;
  collapsed: boolean;
  dirty: boolean;
}

export interface HostSettingsContext {
  section: string | null;
  dirty: boolean;
}

export interface HostContextProviders {
  mailbox: HostMailboxContext;
  compose: HostComposeContext;
  settings: HostSettingsContext;
}

/**
 * 宿主对外暴露的只读结构化 UI 上下文快照：仅含非秘密视图/选中标识/范围摘要/草稿 ID，
 * 严格排除凭据、令牌、密码、原始邮件正文与 HTML。
 */
export interface HostContextSnapshot {
  readonly activeViewId: string;
  readonly online: boolean;
  readonly phase: ConnectionPhase;
  readonly compat: UiCompat;
  readonly accountIds: readonly string[];
  readonly selectedMessage: Readonly<MessageIdentity> | null;
  readonly selectedThreadId: number | null;
  readonly checkedMessages: readonly Readonly<MessageIdentity>[];
  readonly scope: Readonly<HostScopeSummary> | null;
  readonly activeDraftId: string | null;
  readonly activeSettingsSection: string | null;
  readonly theme: Preferences['theme'];
  readonly density: Preferences['density'];
  readonly mailbox: Readonly<HostMailboxContext> | null;
  readonly compose: Readonly<HostComposeContext> | null;
  readonly settings: Readonly<HostSettingsContext> | null;
}

export interface ViewProps {
  active: boolean;
}

export interface ViewContribution<View> {
  id: string;
  title: string;
  icon?: IconName;
  /** 必需角色由宿主保证恰有一个提供者；没有角色的视图进侧栏「扩展」组。 */
  role?: CoreRole;
  order?: number;
  render: View;
  /** 侧栏伴随组件（如邮箱的账户/文件夹树）。 */
  navigation?: View;
}

export interface SettingsContribution<View> {
  id: string;
  title: string;
  order?: number;
  render: View;
}

export interface Contributions<View> {
  moduleId: string;
  apiVersion: 1;
  /** 「设置 → 扩展」里显示的名字。 */
  title: string;
  builtin?: boolean;
  commands?: readonly CommandContribution[];
  views?: readonly ViewContribution<View>[];
  settings?: readonly SettingsContribution<View>[];
  dispose?: () => void;
}

export type View = ComponentType<ViewProps>;
export type Bundle = Contributions<View>;

export interface RegisteredContribution<T> {
  moduleId: string;
  value: T;
}
export interface RegistrySnapshot<View> {
  modules: readonly { moduleId: string; title: string; builtin: boolean; contributions: number }[];
  commands: readonly RegisteredContribution<CommandContribution>[];
  views: readonly RegisteredContribution<ViewContribution<View>>[];
  settings: readonly RegisteredContribution<SettingsContribution<View>>[];
}
