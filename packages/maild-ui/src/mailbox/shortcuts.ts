/**
 * 键盘优先的日常处理流（纯函数，便于测试）：焦点在可编辑控件或有模态对话框时一律放行，
 * 只有 ⌘R / Ctrl+R「立即同步」在输入框里也生效（否则 WebView 会整页重载）。
 */
export type ShortcutAction =
  | "next"
  | "previous"
  | "archive"
  | "star"
  | "read"
  | "reply"
  | "replyAll"
  | "forward"
  | "compose"
  | "search"
  | "help"
  | "sync";

export interface KeyLike {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  isComposing?: boolean;
}

export interface TargetLike {
  tagName?: string;
  isContentEditable?: boolean;
  closest?: (selector: string) => unknown;
}

export const SHORTCUTS: readonly { keys: string; action: ShortcutAction; label: string }[] = [
  { keys: "j", action: "next", label: "下一封邮件" },
  { keys: "k", action: "previous", label: "上一封邮件" },
  { keys: "e", action: "archive", label: "归档当前邮件" },
  { keys: "s", action: "star", label: "切换星标" },
  { keys: "u", action: "read", label: "切换已读 / 未读" },
  { keys: "r", action: "reply", label: "回复" },
  { keys: "a", action: "replyAll", label: "全部回复" },
  { keys: "f", action: "forward", label: "转发" },
  { keys: "c", action: "compose", label: "新建邮件" },
  { keys: "/", action: "search", label: "聚焦搜索框" },
  { keys: "?", action: "help", label: "打开 / 关闭快捷键速查" },
  { keys: "⌘R", action: "sync", label: "立即同步并刷新" },
];

const SINGLE: Record<string, ShortcutAction> = {
  j: "next",
  k: "previous",
  e: "archive",
  s: "star",
  u: "read",
  r: "reply",
  a: "replyAll",
  f: "forward",
  c: "compose",
  "/": "search",
  "?": "help",
};

export function isEditableTarget(target: TargetLike | null | undefined): boolean {
  if (!target) return false;
  const tag = (target.tagName ?? "").toLowerCase();
  if (tag === "input" || tag === "textarea" || tag === "select") return true;
  if (target.isContentEditable) return true;
  return !!target.closest?.('[contenteditable]:not([contenteditable="false"]), [role="combobox"], [role="listbox"], [role="option"]');
}

export function shortcutFor(
  event: KeyLike,
  target: TargetLike | null | undefined,
  modalOpen: boolean,
): ShortcutAction | null {
  if (modalOpen || event.isComposing) return null;
  if ((event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "r")
    return "sync";
  if (event.metaKey || event.ctrlKey || event.altKey) return null;
  if (isEditableTarget(target)) return null;
  // 「?」在多数布局上需要 Shift；其余单键要求不按 Shift，避免与大写输入或系统组合冲突。
  if (event.key === "?") return "help";
  if (event.shiftKey) return null;
  return SINGLE[event.key] ?? null;
}
