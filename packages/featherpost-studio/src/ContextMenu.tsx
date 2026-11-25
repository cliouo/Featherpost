import { m } from "@featherpost/i18n/messages";
import * as Menu from "@radix-ui/react-context-menu";
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
  type ReactElement,
} from "react";
import { Icon } from "./app/Icon";
import type { IconName } from "./host/types";
import { isTextEntry, separator, tidy, type MenuEntry } from "./menus";

type Build = () => MenuEntry[];

export interface MenuOffers {
  /**
   * Called from a surface's `onContextMenu`. The innermost surface wins: the
   * event bubbles outward, so later offers for the same event are ignored.
   * `target` is outlined while the menu is open (defaults to currentTarget).
   */
  offer(event: MouseEvent, build: Build, target?: Element | null): void;
}

const Offers = createContext<MenuOffers>({ offer() {} });
export const useContextMenu = () => useContext(Offers);

/** Same-origin text selection, excluding text typed in form fields. */
function selectedText() {
  return globalThis.getSelection?.()?.toString() ?? "";
}

/**
 * The single app-wide context menu. It wraps the Studio root so that every
 * right-click is decided here: an offered object menu (plus copy/search for a
 * selection), the system menu for text fields, or nothing at all — the raw
 * WebView menu (Reload, Inspect…) never appears.
 */
export function ContextMenuHost({
  children,
  selection,
}: {
  children: ReactElement;
  selection(text: string): MenuEntry[];
}) {
  const offers = useRef(new WeakMap<Event, { build: Build; target: Element | null }>());
  const before = useRef<string | null>(null);
  const marked = useRef<Element | null>(null);
  const [entries, setEntries] = useState<MenuEntry[]>([]);
  const [container, setContainer] = useState<HTMLElement | undefined>();
  const api = useMemo<MenuOffers>(
    () => ({
      offer(event, build, target = event.currentTarget as Element) {
        if (!offers.current.has(event.nativeEvent))
          offers.current.set(event.nativeEvent, { build, target });
      },
    }),
    [],
  );
  const mark = (target: Element | null) => {
    marked.current?.removeAttribute("data-menu-target");
    marked.current = target;
    target?.setAttribute("data-menu-target", "");
  };
  useEffect(() => {
    // WebKit selects the word under a secondary click before dispatching
    // `contextmenu`. Remember what was selected beforehand so that an
    // accidental word selection never turns a row menu into a "copy" menu.
    const down = (e: globalThis.MouseEvent) => {
      if (e.button === 2 || (e.button === 0 && e.ctrlKey)) before.current = selectedText();
    };
    // Anything outside the wrapped tree (portals, the body) gets no WebView menu either.
    const fallback = (e: globalThis.MouseEvent) => {
      if (!e.defaultPrevented && !isTextEntry(e.target)) e.preventDefault();
    };
    document.addEventListener("mousedown", down, true);
    document.addEventListener("contextmenu", fallback);
    return () => {
      document.removeEventListener("mousedown", down, true);
      document.removeEventListener("contextmenu", fallback);
      mark(null);
    };
  }, []);
  const open = (e: MouseEvent) => {
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
      // Prevents both the Radix menu and the native WebView menu.
      e.preventDefault();
      return;
    }
    mark(offer?.target ?? null);
    // Native modal dialogs sit in the top layer; a body portal would be inert behind them.
    setContainer((target.closest("dialog") as HTMLElement | null) ?? undefined);
    setEntries(list);
  };
  return (
    <Offers.Provider value={api}>
      <Menu.Root
        onOpenChange={(next) => {
          if (!next) mark(null);
        }}
      >
        <Menu.Trigger
          asChild
          onContextMenuCapture={(e) => {
            // Stopping here keeps Radix from opening and leaves the system
            // text menu (paste, spelling, input methods) to the field.
            if (isTextEntry(e.target)) e.stopPropagation();
          }}
          onContextMenu={open}
        >
          {children}
        </Menu.Trigger>
        <Menu.Portal container={container}>
          <Menu.Content
            className="studio-menu"
            collisionPadding={10}
            loop
            data-shortcuts-off
            onCloseAutoFocus={(e) => {
              // Keep focus where an action moved it (a dialog, the editor).
              const active = document.activeElement;
              if (active && active !== document.body) e.preventDefault();
            }}
          >
            <Entries entries={entries} container={container} />
          </Menu.Content>
        </Menu.Portal>
      </Menu.Root>
    </Offers.Provider>
  );
}

function Slot({ icon, children }: { icon?: IconName; children?: ReactElement }) {
  return (
    <span className="studio-menu-icon" aria-hidden="true">
      {children ?? (icon ? <Icon name={icon} /> : null)}
    </span>
  );
}

function Entries({ entries, container }: { entries: MenuEntry[]; container?: HTMLElement }) {
  return entries.map((entry, index) => {
    const key = `${index}:${entry.kind}:${"label" in entry ? entry.label : ""}`;
    switch (entry.kind) {
      case "separator":
        return <Menu.Separator key={key} className="studio-menu-separator" />;
      case "label":
        return (
          <Menu.Label key={key} className="studio-menu-label">
            {entry.label}
          </Menu.Label>
        );
      case "check":
        return (
          <Menu.CheckboxItem
            key={key}
            className="studio-menu-item"
            checked={entry.checked}
            disabled={entry.disabled}
            onSelect={entry.run}
          >
            <Slot>
              <Menu.ItemIndicator className="studio-menu-check">
                <Check />
              </Menu.ItemIndicator>
            </Slot>
            <span className="studio-menu-text">{entry.label}</span>
          </Menu.CheckboxItem>
        );
      case "submenu":
        return (
          <Menu.Sub key={key}>
            <Menu.SubTrigger className="studio-menu-item" disabled={entry.disabled}>
              <Slot icon={entry.icon} />
              <span className="studio-menu-text">{entry.label}</span>
              <svg className="studio-menu-chevron" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
                <path d="m6 4 4 4-4 4" />
              </svg>
            </Menu.SubTrigger>
            <Menu.Portal container={container}>
              <Menu.SubContent
                className="studio-menu studio-submenu"
                sideOffset={4}
                alignOffset={-5}
                collisionPadding={10}
                loop
                data-shortcuts-off
              >
                {entry.entries.length ? (
                  <Entries entries={entry.entries} container={container} />
                ) : (
                  <div className="studio-menu-empty">{entry.empty ?? m.studio_menu_empty()}</div>
                )}
              </Menu.SubContent>
            </Menu.Portal>
          </Menu.Sub>
        );
      default:
        return (
          <Menu.Item
            key={key}
            className="studio-menu-item"
            data-tone={entry.tone}
            disabled={entry.disabled}
            onSelect={entry.run}
          >
            <Slot icon={entry.icon} />
            <span className="studio-menu-text">{entry.label}</span>
            {entry.hint && <kbd className="studio-menu-hint">{entry.hint}</kbd>}
          </Menu.Item>
        );
    }
  });
}

function Check() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <path d="m3.5 8.5 3 3 6-7" />
    </svg>
  );
}
