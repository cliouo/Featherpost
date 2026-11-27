import { m } from "@featherpost/i18n/messages";
import {
  Menu as UiSdkMenu,
  MenuCheckboxItem,
  MenuContent,
  MenuContextTrigger,
  MenuGroup,
  MenuGroupLabel,
  MenuItem,
  MenuItemIndicator,
  MenuSeparator,
  MenuSub,
  MenuSubContent,
  MenuSubTrigger,
} from "@ui-sdk/react/menu";
import {
  createContext,
  isValidElement,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type HTMLAttributes,
  type HTMLElementType,
  type MouseEvent,
  type ReactElement,
  type ReactNode,
} from "react";

import { Icon } from "../app/Icon";
import type { IconName } from "../host/types";
import { isTextEntry, separator, tidy, type MenuEntry } from "../menus";
import { useWindowDismiss } from "./windowDismiss";
import { useFallbackZIndex } from "./zIndex";

type Build = () => MenuEntry[];

export interface MenuOffers {
  /**
   * Called from a surface's `onContextMenu`. The innermost surface wins: the
   * event bubbles outward, so later offers for the same event are ignored.
   * `target` is outlined while the menu is open (defaults to currentTarget).
   */
  offer(event: MouseEvent, build: Build, target?: Element | null): void;
}

export interface ContextMenuHostProps {
  /**
   * Must be a single intrinsic host DOM element (e.g. `<div>`) because
   * `<MenuContextTrigger render={children}>` clones `onContextMenu`,
   * `onContextMenuCapture`, `ref`, and `data-slot` directly onto it.
   * Passing a React Context Provider or non-forwarding component silently
   * drops those bindings.
   */
  children: ReactElement<HTMLAttributes<HTMLElement>, HTMLElementType>;
  selection(text: string): MenuEntry[];
}

const Offers = createContext<MenuOffers>({ offer() {} });
export const useContextMenu = () => useContext(Offers);

const MODAL_CONTAINER_SELECTOR =
  '[data-slot="dialog-content"], [data-slot="alert-dialog-content"], [aria-modal="true"]';

/** Same-origin text selection, excluding text typed in form fields. */
function selectedText(): string {
  return globalThis.getSelection?.()?.toString() ?? "";
}

/**
 * When a right-click originates inside a modal Dialog or AlertDialog, portal
 * the context menu into that modal container so `@ui-sdk`'s modal `inert`
 * handling never traps the context menu.
 */
function resolveModalContainer(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;
  const found = target.closest(MODAL_CONTAINER_SELECTOR);
  return found instanceof HTMLElement ? found : null;
}

/**
 * The single app-wide context menu built on `@ui-sdk/react/menu` +
 * `MenuContextTrigger`.
 *
 * - Uses `<MenuContextTrigger render={children}>` so Studio's root element gets
 *   zero extra wrapper DOM nodes.
 * - Preserves WKWebView secondary-click / Ctrl-click pre-selection snapshotting
 *   to discard accidental word selections.
 * - Passes through `isTextEntry` targets untouched so macOS spellcheck / IME
 *   system menus stay native.
 * - Suppresses empty menus (`e.preventDefault()`) so the raw WebView menu
 *   (Reload, Inspect...) never appears.
 * - Portals into an enclosing modal Dialog container when triggered inside one.
 * - Wires `useWindowDismiss` (`visibilitychange(hidden)`, `pagehide`,
 *   `closeAllPopovers()`) and Safari 16 fallback monotonic `zIndex`.
 * - Marks global context menu surfaces `data-agent-hidden=""` (human-only),
 *   `data-tauri-drag-region="false"`, and `data-shortcuts-off=""`.
 */
export function ContextMenuHost({ children, selection }: ContextMenuHostProps) {
  if (!isValidElement(children) || typeof children.type !== "string") {
    throw new Error(
      "<ContextMenuHost> requires a single intrinsic host DOM element (e.g. <div>) as its direct child so MenuContextTrigger can attach contextmenu handlers and ref.",
    );
  }
  const offers = useRef(
    new WeakMap<Event, { build: Build; target: Element | null }>(),
  );
  const before = useRef<string | null>(null);
  const marked = useRef<Element | null>(null);

  const [open, setOpenState] = useState(false);
  const openRef = useRef(open);
  openRef.current = open;

  const [entries, setEntries] = useState<MenuEntry[]>([]);
  const [menuContainer, setMenuContainer] = useState<HTMLElement | null>(null);
  const [session, setSession] = useState(0);
  const [suppressFinalFocus, setSuppressFinalFocus] = useState(false);

  const fallbackZIndex = useFallbackZIndex(open);

  const api = useMemo<MenuOffers>(
    () => ({
      offer(event, build, target = event.currentTarget as Element) {
        if (!offers.current.has(event.nativeEvent)) {
          offers.current.set(event.nativeEvent, { build, target });
        }
      },
    }),
    [],
  );

  const mark = useCallback((target: Element | null) => {
    marked.current?.removeAttribute("data-menu-target");
    marked.current = target;
    target?.setAttribute("data-menu-target", "");
  }, []);

  const closeMenu = useCallback(
    (suppressFocus = false) => {
      mark(null);
      if (!openRef.current) return;
      if (suppressFocus) setSuppressFinalFocus(true);
      setOpenState(false);
    },
    [mark],
  );

  // `@ui-sdk` Menu already handles `window-blur` natively; supplement with
  // `visibilitychange(hidden)`, `pagehide`, and `closeAllPopovers()`.
  useWindowDismiss(open, () => closeMenu(true));

  useEffect(() => {
    // WebKit selects the word under a secondary click before dispatching
    // `contextmenu`. Remember what was selected beforehand so that an
    // accidental word selection never turns a row menu into a "copy" menu.
    const down = (e: globalThis.MouseEvent) => {
      if (e.button === 2 || (e.button === 0 && e.ctrlKey)) {
        before.current = selectedText();
      }
    };
    // Anything outside the wrapped tree (portals, the body) gets no WebView menu either.
    const fallback = (e: globalThis.MouseEvent) => {
      if (!e.defaultPrevented && !isTextEntry(e.target)) {
        e.preventDefault();
      }
    };
    document.addEventListener("mousedown", down, true);
    document.addEventListener("contextmenu", fallback);
    return () => {
      document.removeEventListener("mousedown", down, true);
      document.removeEventListener("contextmenu", fallback);
      mark(null);
    };
  }, [mark]);

  const handleContextMenuCapture = (e: MouseEvent) => {
    // Stopping here keeps `@ui-sdk` from opening and leaves the system
    // text menu (paste, spelling, input methods) to the field.
    if (isTextEntry(e.target)) {
      closeMenu(true);
      e.stopPropagation();
    }
  };

  const handleContextMenu = (e: MouseEvent) => {
    const target = e.target as Element;
    const offer = offers.current.get(e.nativeEvent);
    const prior = before.current;
    before.current = null;
    const range = globalThis.getSelection?.();
    let text = selectedText();
    if (prior !== null && text !== prior) {
      range?.removeAllRanges();
      text = "";
    }
    const over = !!text.trim() && !!range?.containsNode(target, true);
    const list = tidy([
      ...(over ? selection(text) : []),
      separator,
      ...(offer?.build() ?? []),
    ]);
    if (!list.length) {
      // Prevents both `@ui-sdk`'s MenuContextTrigger and the native WebView menu.
      e.preventDefault();
      closeMenu(true);
      return;
    }
    mark(offer?.target ?? null);
    setEntries(list);
    setMenuContainer(resolveModalContainer(target));
    setSession((n) => n + 1);
    setSuppressFinalFocus(false);
  };

  return (
    <Offers.Provider value={api}>
      <UiSdkMenu
        open={open}
        onOpenChange={(next) => {
          if (next) {
            setSuppressFinalFocus(false);
            setOpenState(true);
          } else {
            mark(null);
            setOpenState(false);
          }
        }}
        onOpenChangeComplete={(next) => {
          if (!next) {
            setMenuContainer(null);
          }
        }}
        loop
        modal="pointer"
        container={menuContainer ?? undefined}
      >
        <MenuContextTrigger
          render={children}
          onContextMenuCapture={handleContextMenuCapture}
          onContextMenu={handleContextMenu}
        />
        <MenuContent
          className="studio-menu"
          sideOffset={0}
          collisionPadding={10}
          finalFocus={suppressFinalFocus ? "none" : undefined}
          data-shortcuts-off=""
          data-tauri-drag-region="false"
          data-agent-hidden=""
          style={
            fallbackZIndex !== undefined
              ? { zIndex: fallbackZIndex }
              : undefined
          }
        >
          <Entries key={session} entries={entries} />
        </MenuContent>
      </UiSdkMenu>
    </Offers.Provider>
  );
}

function Slot({
  icon,
  children,
}: {
  icon?: IconName;
  children?: ReactElement;
}) {
  return (
    <span className="studio-menu-icon" aria-hidden="true">
      {children ?? (icon ? <Icon name={icon} /> : null)}
    </span>
  );
}

function entryKey(entry: MenuEntry, index: number): string {
  return `${index}:${entry.kind}:${"label" in entry ? entry.label : ""}`;
}

function renderEntry(entry: MenuEntry, index: number): ReactNode {
  const key = entryKey(entry, index);
  switch (entry.kind) {
    case "separator":
      return <MenuSeparator key={key} className="studio-menu-separator" />;
    case "check":
      return (
        <MenuCheckboxItem
          key={key}
          value={key}
          label={entry.label}
          className="studio-menu-item"
          checked={entry.checked}
          disabled={entry.disabled}
          closeOnSelect
          onCheckedChange={() => {
            entry.run();
          }}
        >
          <Slot>
            <MenuItemIndicator className="studio-menu-check">
              <Check />
            </MenuItemIndicator>
          </Slot>
          <span className="studio-menu-text">{entry.label}</span>
        </MenuCheckboxItem>
      );
    case "submenu":
      return <SubmenuEntry key={key} entry={entry} />;
    case "label":
      return (
        <MenuGroup key={key}>
          <MenuGroupLabel className="studio-menu-label">
            {entry.label}
          </MenuGroupLabel>
        </MenuGroup>
      );
    default:
      return (
        <MenuItem
          key={key}
          label={entry.label}
          className="studio-menu-item"
          destructive={entry.tone === "danger" || undefined}
          data-tone={entry.tone}
          disabled={entry.disabled}
          onSelect={() => {
            entry.run();
          }}
        >
          <Slot icon={entry.icon} />
          <span className="studio-menu-text">{entry.label}</span>
          {entry.hint && <kbd className="studio-menu-hint">{entry.hint}</kbd>}
        </MenuItem>
      );
  }
}

function Entries({ entries }: { entries: MenuEntry[] }) {
  const nodes: ReactNode[] = [];
  for (let i = 0; i < entries.length; ) {
    const entry = entries[i]!;
    if (entry.kind === "label") {
      const groupKey = entryKey(entry, i);
      const groupChildren: ReactNode[] = [];
      let j = i + 1;
      while (j < entries.length) {
        const next = entries[j]!;
        if (next.kind === "separator" || next.kind === "label") break;
        groupChildren.push(renderEntry(next, j));
        j += 1;
      }
      nodes.push(
        <MenuGroup key={groupKey}>
          <MenuGroupLabel className="studio-menu-label">
            {entry.label}
          </MenuGroupLabel>
          {groupChildren}
        </MenuGroup>,
      );
      i = j;
      continue;
    }
    nodes.push(renderEntry(entry, i));
    i += 1;
  }
  return <>{nodes}</>;
}

function SubmenuEntry({
  entry,
}: {
  entry: Extract<MenuEntry, { kind: "submenu" }>;
}) {
  const [subOpen, setSubOpen] = useState(false);
  const fallbackZIndex = useFallbackZIndex(subOpen);

  return (
    <MenuSub onOpenChange={setSubOpen} loop>
      <MenuSubTrigger className="studio-menu-item" disabled={entry.disabled}>
        <Slot icon={entry.icon} />
        <span className="studio-menu-text">{entry.label}</span>
        <svg
          className="studio-menu-chevron"
          width="12"
          height="12"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          aria-hidden="true"
        >
          <path d="m6 4 4 4-4 4" />
        </svg>
      </MenuSubTrigger>
      <MenuSubContent
        className="studio-menu studio-submenu"
        sideOffset={4}
        alignOffset={-5}
        collisionPadding={10}
        data-shortcuts-off=""
        data-tauri-drag-region="false"
        data-agent-hidden=""
        style={
          fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : undefined
        }
      >
        {entry.entries.length ? (
          <Entries entries={entry.entries} />
        ) : (
          <div className="studio-menu-empty">
            {entry.empty ?? m.studio_menu_empty()}
          </div>
        )}
      </MenuSubContent>
    </MenuSub>
  );
}

function Check() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      aria-hidden="true"
    >
      <path d="m3.5 8.5 3 3 6-7" />
    </svg>
  );
}
