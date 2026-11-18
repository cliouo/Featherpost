import type { MailboxController } from "./controller";
import type { ShortcutAction } from "./shortcuts";

/** 快捷键落到哪里：写入口一律经宿主命令执行，沿用 enabledWhen 与版本闸门。 */
export const SHORTCUT_COMMANDS: Partial<Record<ShortcutAction, string>> = {
  archive: "maild.mailbox.archive",
  reply: "maild.compose.reply",
  replyAll: "maild.compose.reply-all",
  forward: "maild.compose.forward",
  compose: "maild.compose.new",
};

export interface ShortcutDeps {
  controller: MailboxController;
  /** 宿主命令（host.execute）；未注册或未启用的命令由宿主忽略。 */
  execute(commandId: string): void;
  /** 写入是否放行（在线且版本兼容）。 */
  writable(): boolean;
  focusSearch(): void;
  toggleHelp(): void;
  sync(): void;
}

/** 执行一个快捷键动作；返回是否被处理（调用方据此 preventDefault）。 */
export function runShortcut(action: ShortcutAction, deps: ShortcutDeps): boolean {
  const { controller } = deps;
  switch (action) {
    case "next":
      controller.step(1);
      return true;
    case "previous":
      controller.step(-1);
      return true;
    case "star":
      if (deps.writable()) void controller.toggleSelected("flagged");
      return true;
    case "read":
      if (deps.writable()) void controller.toggleSelected("seen");
      return true;
    case "search":
      deps.focusSearch();
      return true;
    case "help":
      deps.toggleHelp();
      return true;
    case "sync":
      deps.sync();
      return true;
    default: {
      const command = SHORTCUT_COMMANDS[action];
      if (!command) return false;
      deps.execute(command);
      return true;
    }
  }
}
