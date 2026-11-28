import { m } from "@featherpost/i18n/messages";

import type { Bundle, RegistrySnapshot, View } from "./host/types";
import { bilingualKeywords } from "./i18n";

export type ShortcutPlatform = "mac" | "other";

export interface ShortcutNavigatorLike {
  readonly platform?: string;
  readonly userAgent?: string;
  readonly userAgentData?: { readonly platform?: string };
}

/**
 * Single source of truth for every keyboard shortcut actually bound in Featherpost Studio.
 * Only keys backed by real event handlers belong here; unbound keys must not be listed.
 */
export const STUDIO_SHORTCUT_KEYS = {
  openPalette: "mod+k",
  focusSearch: "mod+shift+f",
  quickSearch: "/",
  focusSidebar: "mod+l",
  openSettings: "mod+,",
  composeNew: "mod+n",
  openShortcuts: "?",
  sendMessage: "mod+enter",
  nextMessage: "j",
  nextMessageArrow: "arrowdown",
  previousMessage: "k",
  previousMessageArrow: "arrowup",
  clearSelection: "escape",
  copySelection: "mod+c",
  translateMessage: "shift+t",
} as const;

export type StudioShortcutKeyName = keyof typeof STUDIO_SHORTCUT_KEYS;
export type StudioShortcutKey =
  (typeof STUDIO_SHORTCUT_KEYS)[StudioShortcutKeyName];

export const STUDIO_SHELL_MODULE_ID = "maild.studio";
export const STUDIO_SHORTCUTS_COMMAND_ID = "maild.studio.shortcuts";

export function detectShortcutPlatform(
  nav: ShortcutNavigatorLike | null = typeof navigator !== "undefined"
    ? (navigator as ShortcutNavigatorLike)
    : null,
): ShortcutPlatform {
  if (!nav) return "other";
  const uaPlatform = nav.userAgentData?.platform?.trim();
  if (uaPlatform) {
    return /mac|iphone|ipad|ipod/i.test(uaPlatform) ? "mac" : "other";
  }
  const platform = nav.platform?.trim();
  if (platform) {
    return /mac|iphone|ipad|ipod/i.test(platform) ? "mac" : "other";
  }
  const ua = nav.userAgent?.trim();
  if (ua) {
    return /mac os x|macintosh|iphone|ipad|ipod/i.test(ua) ? "mac" : "other";
  }
  return "other";
}

function splitShortcutTokens(shortcut: string): string[] {
  const trimmed = shortcut.trim();
  if (!trimmed) return [];
  if (trimmed === "+") return ["+"];
  const tokens: string[] = [];
  for (const raw of trimmed.split("+")) {
    const clean = raw.trim();
    if (clean) tokens.push(clean);
  }
  return tokens;
}

const MAC_KEY_LABELS: Readonly<Record<string, string>> = {
  mod: "⌘",
  meta: "⌘",
  cmd: "⌘",
  command: "⌘",
  ctrl: "⌃",
  control: "⌃",
  shift: "⇧",
  alt: "⌥",
  option: "⌥",
  enter: "↩",
  return: "↩",
  escape: "Esc",
  esc: "Esc",
  arrowdown: "↓",
  down: "↓",
  arrowup: "↑",
  up: "↑",
  arrowleft: "←",
  left: "←",
  arrowright: "→",
  right: "→",
  space: "Space",
  tab: "⇥",
  backspace: "⌫",
  delete: "⌦",
};

const OTHER_KEY_LABELS: Readonly<Record<string, string>> = {
  mod: "Ctrl",
  meta: "Meta",
  cmd: "Meta",
  command: "Meta",
  ctrl: "Ctrl",
  control: "Ctrl",
  shift: "Shift",
  alt: "Alt",
  option: "Alt",
  enter: "Enter",
  return: "Enter",
  escape: "Esc",
  esc: "Esc",
  arrowdown: "↓",
  down: "↓",
  arrowup: "↑",
  up: "↑",
  arrowleft: "←",
  left: "←",
  arrowright: "→",
  right: "→",
  space: "Space",
  tab: "Tab",
  backspace: "Backspace",
  delete: "Delete",
};

const ARIA_KEY_NAMES: Readonly<Record<string, string>> = {
  meta: "Meta",
  cmd: "Meta",
  command: "Meta",
  ctrl: "Control",
  control: "Control",
  shift: "Shift",
  alt: "Alt",
  option: "Alt",
  enter: "Enter",
  return: "Enter",
  escape: "Escape",
  esc: "Escape",
  arrowdown: "ArrowDown",
  down: "ArrowDown",
  arrowup: "ArrowUp",
  up: "ArrowUp",
  arrowleft: "ArrowLeft",
  left: "ArrowLeft",
  arrowright: "ArrowRight",
  right: "ArrowRight",
  space: "Space",
  tab: "Tab",
  backspace: "Backspace",
  delete: "Delete",
};

/**
 * Layout-dependent shifted punctuation keys whose physical modifier combination varies
 * across keyboard layouts (e.g. `?` requires Shift on US layouts but not all ISO layouts).
 * Per WAI-ARIA 1.2, omit optional `aria-keyshortcuts` rather than declaring an inaccurate chord.
 */
const OMIT_ARIA_KEYS = new Set(["?"]);

function formatSingleToken(token: string, platform: ShortcutPlatform): string {
  const lower = token.toLowerCase();
  const table = platform === "mac" ? MAC_KEY_LABELS : OTHER_KEY_LABELS;
  if (Object.prototype.hasOwnProperty.call(table, lower)) {
    return table[lower]!;
  }
  if (/^[a-z]$/i.test(token)) {
    return token.toUpperCase();
  }
  return token;
}

/**
 * Formats a canonical shortcut chord (e.g. `"mod+shift+f"`) into structured keycap parts.
 * Consumers rendering `<kbd>` keycaps should iterate these parts directly instead of splitting text.
 */
export function formatShortcutParts(
  shortcut: string,
  platform: ShortcutPlatform = detectShortcutPlatform(),
): readonly string[] {
  return splitShortcutTokens(shortcut).map((token) =>
    formatSingleToken(token, platform),
  );
}

/**
 * Formats a canonical shortcut chord as a human-readable string:
 * macOS uses spaced glyphs (`⌘ ⇧ F`), while Windows/Linux use `+`-joined names (`Ctrl+Shift+F`).
 */
export function formatShortcutText(
  shortcut: string,
  platform: ShortcutPlatform = detectShortcutPlatform(),
): string {
  const parts = formatShortcutParts(shortcut, platform);
  if (parts.length === 0) return "";
  return platform === "mac" ? parts.join(" ") : parts.join("+");
}

/**
 * Formats a button/control tooltip combining localized label and platform-adapted shortcut text.
 */
export function formatShortcutTooltip(
  label: string,
  shortcut: string,
  platform: ShortcutPlatform = detectShortcutPlatform(),
): string {
  const formatted = formatShortcutText(shortcut, platform);
  return formatted ? `${label} ${formatted}` : label;
}

function formatSingleAriaShortcut(shortcut: string): string[] {
  const tokens = splitShortcutTokens(shortcut);
  if (tokens.length === 0) return [];
  if (tokens.length === 1 && OMIT_ARIA_KEYS.has(tokens[0]!)) {
    return [];
  }
  const hasMod = tokens.some((token) => token.toLowerCase() === "mod");
  const formatToken = (token: string, modName: "Meta" | "Control") => {
    const lower = token.toLowerCase();
    if (lower === "mod") return modName;
    if (Object.prototype.hasOwnProperty.call(ARIA_KEY_NAMES, lower)) {
      return ARIA_KEY_NAMES[lower]!;
    }
    if (/^[a-z]$/i.test(token)) return token.toUpperCase();
    return token;
  };
  if (hasMod) {
    return [
      tokens.map((token) => formatToken(token, "Meta")).join("+"),
      tokens.map((token) => formatToken(token, "Control")).join("+"),
    ];
  }
  return [tokens.map((token) => formatToken(token, "Meta")).join("+")];
}

/**
 * Formats one or more canonical shortcuts into a WAI-ARIA 1.2 `aria-keyshortcuts` value.
 * - Expands `mod+k` to `"Meta+K Control+K"` (cross-platform accessibility parity).
 * - Returns `undefined` for layout-dependent keys such as `"?"`.
 */
export function formatAriaKeyShortcuts(
  shortcut: string | readonly string[],
): string | undefined {
  const list = typeof shortcut === "string" ? [shortcut] : shortcut;
  const values: string[] = [];
  for (const entry of list) {
    for (const formatted of formatSingleAriaShortcut(entry)) {
      if (formatted && !values.includes(formatted)) {
        values.push(formatted);
      }
    }
  }
  return values.length > 0 ? values.join(" ") : undefined;
}

// ---------- Unified keyboard guards ----------

export function isImeEvent(
  event: Pick<KeyboardEvent, "isComposing" | "keyCode">,
): boolean {
  return event.isComposing === true || event.keyCode === 229;
}

const SHORTCUT_BLOCKED_SELECTOR =
  'input,textarea,select,summary,[data-chip-input],[role="textbox"],[role="searchbox"],[role="spinbutton"],[role="checkbox"],[role="switch"],[role="combobox"],[role="listbox"],[role="option"],[role="menu"],[role="menuitem"]';

export type ShortcutTargetLike =
  | EventTarget
  | {
      closest?(selector: string): unknown;
      getAttribute?(name: string): string | null;
      isContentEditable?: boolean;
    }
  | null
  | undefined;

function isVisibleElement(el: Element): boolean {
  if (el.closest("[hidden]")) return false;
  if (el instanceof HTMLElement) {
    if (el.style.display === "none" || el.style.visibility === "hidden") {
      return false;
    }
  }
  if (
    typeof window !== "undefined" &&
    typeof window.getComputedStyle === "function"
  ) {
    const computed = window.getComputedStyle(el);
    if (computed.display === "none" || computed.visibility === "hidden") {
      return false;
    }
  }
  return true;
}

function isActiveShortcutsOffGate(match: unknown): boolean {
  if (!match) return false;
  if (typeof Element !== "undefined" && match instanceof Element) {
    return isVisibleElement(match);
  }
  if (typeof (match as { closest?(sel: string): unknown }).closest === "function") {
    return !(match as { closest(sel: string): unknown }).closest("[hidden]");
  }
  return true;
}

/**
 * Returns true when `target` is an input, contenteditable surface (including bare or inherited
 * `contenteditable`), chip input, interactive ARIA widget, or inside an active `[data-shortcuts-off]`
 * region where single-key shortcuts must never fire.
 */
export function isShortcutBlockedTarget(target: ShortcutTargetLike): boolean {
  if (!target || typeof target !== "object") return false;
  const candidate = target as {
    closest?(selector: string): unknown;
    getAttribute?(name: string): string | null;
    isContentEditable?: boolean;
  };
  if (candidate.isContentEditable === true) return true;
  if (typeof candidate.closest !== "function") return false;

  if (candidate.closest(SHORTCUT_BLOCKED_SELECTOR)) return true;

  const shortcutsOff = candidate.closest("[data-shortcuts-off]");
  if (isActiveShortcutsOffGate(shortcutsOff)) return true;

  const editableAncestor = candidate.closest("[contenteditable]");
  if (editableAncestor) {
    if (
      typeof (editableAncestor as { getAttribute?(name: string): string | null })
        .getAttribute === "function"
    ) {
      const attr = (
        editableAncestor as { getAttribute(name: string): string | null }
      ).getAttribute("contenteditable");
      if (attr !== null && attr.toLowerCase() !== "false") {
        return true;
      }
    } else {
      return true;
    }
  }
  return false;
}

/**
 * Returns true when global shortcuts must be suppressed:
 * `defaultPrevented`, open `<dialog>` or `[aria-modal="true"]`, or any visible `[data-shortcuts-off]` gate.
 */
export function isGlobalShortcutBlocked(
  event: Pick<KeyboardEvent, "defaultPrevented" | "target">,
): boolean {
  if (event.defaultPrevented) return true;
  if (typeof document === "undefined") return false;
  if (document.querySelector('dialog[open], [aria-modal="true"]')) return true;

  const target = event.target as
    | { closest?(selector: string): Element | null }
    | null
    | undefined;
  if (
    target &&
    typeof target.closest === "function" &&
    isActiveShortcutsOffGate(target.closest("[data-shortcuts-off]"))
  ) {
    return true;
  }
  const active = document.activeElement;
  if (
    active &&
    typeof active.closest === "function" &&
    isActiveShortcutsOffGate(active.closest("[data-shortcuts-off]"))
  ) {
    return true;
  }
  for (const el of document.querySelectorAll("[data-shortcuts-off]")) {
    if (isActiveShortcutsOffGate(el)) return true;
  }
  return false;
}

export type QuestionMarkEventLike = Pick<
  KeyboardEvent,
  | "key"
  | "ctrlKey"
  | "metaKey"
  | "altKey"
  | "repeat"
  | "defaultPrevented"
  | "isComposing"
  | "keyCode"
> & {
  getModifierState?(key: string): boolean;
};

/**
 * Strictly recognizes the ASCII `?` key (allowing Shift used to produce `?`), without falling back
 * to physical `e.code === "Slash"` or `Shift+/` (which breaks international layouts and conflicts with `/`).
 * Rejects Ctrl/Meta/Alt/AltGraph, repeat, defaultPrevented, IME composition/229, and non-ASCII characters.
 */
export function isQuestionMarkShortcut(event: QuestionMarkEventLike): boolean {
  if (event.defaultPrevented || event.repeat || isImeEvent(event)) return false;
  if (
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    Boolean(event.getModifierState?.("AltGraph"))
  ) {
    return false;
  }
  return event.key === "?";
}

// ---------- Explicit real-handler shortcut catalog ----------

export type ShortcutGroupId = "global" | "mail" | "search" | "compose";

export interface ShortcutCatalogItem {
  readonly id: string;
  /** Optional command ID whose live localized title is read from `registry.commands`. */
  readonly commandId?: string;
  readonly label: string;
  readonly keys: readonly string[];
  /** Explicit scope or availability condition (rows are never hidden based on transient `canExecute`). */
  readonly scope?: string;
}

export interface ShortcutCatalogGroup {
  readonly id: ShortcutGroupId;
  readonly label: string;
  readonly items: readonly ShortcutCatalogItem[];
}

interface ExplicitShortcutBinding {
  readonly id: string;
  readonly group: ShortcutGroupId;
  readonly keys: readonly string[];
  readonly commandId?: string;
  readonly fallbackLabel: () => string;
  readonly scope?: () => string;
}

/**
 * Explicit binding table mapping ONLY actually implemented Studio key handlers.
 * Do NOT auto-scan `registry.commands` for arbitrary `shortcut` strings.
 */
const EXPLICIT_SHORTCUT_BINDINGS: readonly ExplicitShortcutBinding[] = [
  // Global & navigation
  {
    id: "global.palette",
    group: "global",
    keys: [STUDIO_SHORTCUT_KEYS.openPalette],
    fallbackLabel: m.studio_palette_open,
  },
  {
    id: "global.shortcuts",
    group: "global",
    keys: [STUDIO_SHORTCUT_KEYS.openShortcuts],
    commandId: STUDIO_SHORTCUTS_COMMAND_ID,
    fallbackLabel: m.studio_shortcuts_command_title,
    scope: m.studio_shortcuts_scope_outside_inputs,
  },
  {
    id: "global.settings",
    group: "global",
    keys: [STUDIO_SHORTCUT_KEYS.openSettings],
    commandId: "maild.settings.open",
    fallbackLabel: m.settings_command_open,
  },
  {
    id: "global.sidebar-filter",
    group: "global",
    keys: [STUDIO_SHORTCUT_KEYS.focusSidebar],
    fallbackLabel: m.studio_palette_action_focus_sidebar,
  },
  // Messages & reading
  {
    id: "mail.next",
    group: "mail",
    keys: [STUDIO_SHORTCUT_KEYS.nextMessage, STUDIO_SHORTCUT_KEYS.nextMessageArrow],
    fallbackLabel: m.studio_shortcuts_item_next_message,
    scope: m.studio_shortcuts_scope_mail_view,
  },
  {
    id: "mail.previous",
    group: "mail",
    keys: [
      STUDIO_SHORTCUT_KEYS.previousMessage,
      STUDIO_SHORTCUT_KEYS.previousMessageArrow,
    ],
    fallbackLabel: m.studio_shortcuts_item_prev_message,
    scope: m.studio_shortcuts_scope_mail_view,
  },
  {
    id: "mail.clear-selection",
    group: "mail",
    keys: [STUDIO_SHORTCUT_KEYS.clearSelection],
    fallbackLabel: m.studio_shortcuts_item_clear_selection,
    scope: m.studio_shortcuts_scope_mail_view,
  },
  {
    id: "mail.copy",
    group: "mail",
    keys: [STUDIO_SHORTCUT_KEYS.copySelection],
    fallbackLabel: m.studio_shortcuts_item_copy_selection,
    scope: m.studio_shortcuts_scope_text_selection,
  },
  {
    id: "mail.translate",
    group: "mail",
    keys: [STUDIO_SHORTCUT_KEYS.translateMessage],
    commandId: "maild.mailbox.translate",
    fallbackLabel: m.mailbox_command_translate,
    scope: m.studio_shortcuts_scope_mail_view,
  },
  // Search
  {
    id: "search.focus",
    group: "search",
    keys: [STUDIO_SHORTCUT_KEYS.focusSearch],
    fallbackLabel: m.studio_palette_action_search_mail,
  },
  {
    id: "search.quick",
    group: "search",
    keys: [STUDIO_SHORTCUT_KEYS.quickSearch],
    fallbackLabel: m.studio_shortcuts_item_quick_search,
    scope: m.studio_shortcuts_scope_message_list,
  },
  // Compose
  {
    id: "compose.new",
    group: "compose",
    keys: [STUDIO_SHORTCUT_KEYS.composeNew],
    commandId: "maild.compose.new",
    fallbackLabel: m.compose_command_new,
    scope: m.studio_shortcuts_scope_online,
  },
  {
    id: "compose.send",
    group: "compose",
    keys: [STUDIO_SHORTCUT_KEYS.sendMessage],
    fallbackLabel: m.studio_shortcuts_item_send_message,
    scope: m.studio_shortcuts_scope_compose_editor,
  },
];

const SHORTCUT_GROUP_ORDER: ReadonlyArray<{
  readonly id: ShortcutGroupId;
  readonly label: () => string;
}> = [
  { id: "global", label: m.studio_shortcuts_group_global },
  { id: "mail", label: m.studio_shortcuts_group_mail },
  { id: "search", label: m.studio_shortcuts_group_search },
  { id: "compose", label: m.studio_shortcuts_group_compose },
];

/**
 * Builds the localized keyboard shortcut catalog from the explicit handler-backed binding table.
 * Reads live localized titles from `registry.commands` by command ID when available, never hiding
 * rows based on transient `canExecute` state and never listing unbound commands.
 */
export function buildShortcutCatalog(
  registry?: Pick<RegistrySnapshot<View>, "commands"> | null,
): readonly ShortcutCatalogGroup[] {
  const commandById = new Map(
    (registry?.commands ?? []).map((entry) => [entry.value.id, entry.value]),
  );

  return SHORTCUT_GROUP_ORDER.map((group) => ({
    id: group.id,
    label: group.label(),
    items: EXPLICIT_SHORTCUT_BINDINGS.filter(
      (binding) => binding.group === group.id,
    ).map((binding) => {
      const command = binding.commandId
        ? commandById.get(binding.commandId)
        : undefined;
      return {
        id: binding.id,
        ...(binding.commandId ? { commandId: binding.commandId } : {}),
        label: command ? command.title : binding.fallbackLabel(),
        keys: binding.keys,
        ...(binding.scope ? { scope: binding.scope() } : {}),
      };
    }),
  }));
}

/**
 * Built-in Studio shell contribution registering `maild.studio.shortcuts`.
 * Uses getters so titles, descriptions, and keywords update live on UI locale switches.
 */
export function studioShellModule(openShortcuts: () => void): Bundle {
  return {
    moduleId: STUDIO_SHELL_MODULE_ID,
    apiVersion: 1,
    get title() {
      return m.studio_module_shell();
    },
    commands: [
      {
        id: STUDIO_SHORTCUTS_COMMAND_ID,
        get title() {
          return m.studio_shortcuts_command_title();
        },
        get description() {
          return m.studio_shortcuts_command_description();
        },
        get keywords() {
          return bilingualKeywords(
            m.studio_shortcuts_command_title,
            m.studio_shortcuts_command_keywords,
          );
        },
        shortcut: STUDIO_SHORTCUT_KEYS.openShortcuts,
        risk: "read",
        icon: "keyboard",
        locations: ["palette"],
        order: 80,
        execute: () => openShortcuts(),
      },
    ],
  };
}
