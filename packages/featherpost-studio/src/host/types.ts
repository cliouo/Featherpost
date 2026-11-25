import type { ComponentType } from 'react';

import type { MessageIdentity } from '../bridge/types';

export type CoreRole = 'mailbox' | 'settings' | 'compose' | 'drafts' | 'activity';
export type CommandLocation = 'navigation' | 'toolbar' | 'menu';
/** 受限的声明式条件（UI-EXTENSIONS §5）：只有必要的非秘密布尔上下文，不给插件推全量对象。 */
export type EnabledWhen = 'always' | 'message-selected' | 'online-message-selected' | 'online';
export type IconName =
  | 'inbox' | 'folder' | 'sent' | 'drafts' | 'archive' | 'trash' | 'spam' | 'star'
  | 'search' | 'activity' | 'mail' | 'reply' | 'forward' | 'paperclip' | 'settings' | 'refresh' | 'more' | 'clip' | 'back' | 'puzzle';

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
  icon?: IconName;
  locations: readonly CommandLocation[];
  order?: number;
  enabledWhen?: EnabledWhen;
  execute(context: CommandContext): void;
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
