import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandGroupLabel,
  CommandInput,
  CommandItem,
  CommandItemIcon,
  CommandItemText,
  CommandList,
  CommandSeparator,
  CommandShortcut,
  type CommandData,
  type CommandDataGroup,
  type CommandDataItem,
  type ListAnnouncement,
} from "@ui-sdk/react/command";
import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";

import { StudioDialog } from "./Dialog";

export type StudioCommandAnnouncement = ListAnnouncement;

export interface StudioCommandItem {
  id: string;
  label: string;
  keywords?: readonly string[];
  subtitle?: string;
  /**
   * Display-only shortcut (e.g. `"mod+n"`, `"mod+,"`).
   * Stored in side metadata and rendered via `<CommandShortcut shortcut={...} />` only;
   * never passed to `@ui-sdk`'s `CommandDataItem` / `CommandItem` so global hotkeys are never hijacked.
   */
  shortcut?: string;
  disabled?: boolean;
  active?: boolean;
  danger?: boolean;
  icon?: ReactNode;
  /**
   * When true, the dialog closes with `finalFocus="none"` first so `inert` lifts cleanly
   * before `onSelect` moves focus (e.g. search bar, compose editor, or confirmation dialog).
   */
  focusHandoff?: boolean;
  onSelect: () => void;
}

export interface StudioCommandGroup {
  id: string;
  label: string;
  items: readonly StudioCommandItem[];
}

export interface StudioCommandRunOptions {
  focusHandoff?: boolean;
}

export interface StudioCommandFooterContext {
  query: string;
  runWithClose: (action: () => void, options?: StudioCommandRunOptions) => void;
}

export interface StudioCommandDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  inputLabel: string;
  inputPlaceholder?: string;
  /** Full searchable catalog (`items`). */
  groups: readonly StudioCommandGroup[];
  /** Compact catalog shown when the search input is empty (`emptyItems`). */
  emptyGroups?: readonly StudioCommandGroup[];
  /** Optional controlled search query. */
  query?: string;
  onQueryChange?: (query: string) => void;
  /** Localized empty-state copy rendered in `<CommandEmpty>` outside `<CommandList>`. */
  emptyTitle: ReactNode;
  emptyHint?: ReactNode;
  /** Localized screen-reader result announcements. */
  announcement?: (change: StudioCommandAnnouncement) => string | null | undefined;
  /** Localized pending action announcement for screen readers. */
  pendingAnnouncement?: string;
  /** Optional accessible label for the active indicator checkmark. */
  activeLabel?: string;
  /** Optional leading icon rendered beside the search input. */
  searchIcon?: ReactNode;
  /** Invoked on plain Enter in the input when the query is non-empty and 0 commands match. */
  onEmptyEnter?: (
    query: string,
    runWithClose: (action: () => void, options?: StudioCommandRunOptions) => void,
  ) => void;
  /** Fixed footer rendered AFTER `<CommandList>` and `<CommandEmpty>`. */
  footer?: (context: StudioCommandFooterContext) => ReactNode;
  /** Whether `Mod+K` inside the open dialog toggles it closed (default: `true`). */
  closeOnModK?: boolean;
  container?: HTMLElement | null;
  contentClassName?: string;
  overlayClassName?: string;
}

function toCommandData(groups: readonly StudioCommandGroup[]): CommandData {
  const result: CommandDataGroup[] = [];
  for (const group of groups) {
    if (!group.items.length) continue;
    const items: CommandDataItem[] = group.items.map((item) => ({
      value: item.id,
      label: item.label,
      ...(item.keywords && item.keywords.length > 0
        ? { keywords: [...item.keywords] }
        : {}),
      ...(item.disabled ? { disabled: true } : {}),
      // Intentionally omit `shortcut` here so `@ui-sdk` never binds global hotkeys.
    }));
    result.push({
      type: "group",
      label: group.label,
      items,
    });
  }
  return result;
}

function countVisibleItems(data: CommandData): number {
  let count = 0;
  for (const entry of data) {
    if (typeof entry === "string") {
      count++;
    } else if ("type" in entry) {
      if (entry.type === "group") {
        count += countVisibleItems(entry.items);
      }
    } else {
      count++;
    }
  }
  return count;
}

function isNativeComposing(event: KeyboardEvent): boolean {
  return event.isComposing === true || event.keyCode === 229;
}

function isReactComposing(event: ReactKeyboardEvent): boolean {
  const native = event.nativeEvent;
  return native.isComposing === true || native.keyCode === 229;
}

const IME_SENSITIVE_KEYS = new Set([
  "Enter",
  "Escape",
  "ArrowDown",
  "ArrowUp",
  "PageDown",
  "PageUp",
  "Backspace",
]);

/**
 * Studio's `@ui-sdk/react/command` modal boundary built on `StudioDialog`.
 *
 * - Only this module imports `@ui-sdk/react/command` (enforced by `test/uiBoundary.test.ts`).
 * - Keeps `StudioDialog` mounted across open/close transitions so `finalFocus="none"`
 *   is honored cleanly during focus handoff (search, compose, confirmation dialogs).
 * - Blocks modified Enter (`Alt`/`Ctrl`/`Meta`/`Shift` + `Enter`) and repeated Enter.
 * - Preserves Chinese IME composition via `@ui-sdk`'s `isComposing`/`keyCode === 229`
 *   plus a capture-phase `composingRef` fallback when both flags are missing.
 * - Never passes `shortcut` to `CommandDataItem` / `CommandItem`, using `<CommandShortcut>`
 *   for display only.
 */
export function StudioCommandDialog({
  open,
  onOpenChange,
  title,
  inputLabel,
  inputPlaceholder,
  groups,
  emptyGroups,
  query: controlledQuery,
  onQueryChange,
  emptyTitle,
  emptyHint,
  announcement,
  pendingAnnouncement,
  activeLabel,
  searchIcon,
  onEmptyEnter,
  footer,
  closeOnModK = true,
  container,
  contentClassName,
  overlayClassName,
}: StudioCommandDialogProps) {
  const [internalQuery, setInternalQuery] = useState("");
  const query = controlledQuery ?? internalQuery;
  const [suppressFinalFocus, setSuppressFinalFocus] = useState(false);

  const inputRef = useRef<HTMLInputElement | null>(null);
  const [inputNode, setInputNode] = useState<HTMLInputElement | null>(null);
  const composingRef = useRef(false);
  const matchCountRef = useRef(0);
  const queryRef = useRef(query);
  queryRef.current = query;

  const pendingActionRef = useRef<(() => void) | null>(null);
  const onEmptyEnterRef = useRef(onEmptyEnter);
  onEmptyEnterRef.current = onEmptyEnter;

  const setQuery = useCallback(
    (next: string) => {
      if (controlledQuery === undefined) {
        setInternalQuery(next);
      }
      onQueryChange?.(next);
    },
    [controlledQuery, onQueryChange],
  );

  // Reset transient composition, query, and finalFocus override each time the dialog opens.
  const prevOpenRef = useRef(open);
  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;
    if (open && !wasOpen) {
      composingRef.current = false;
      setSuppressFinalFocus(false);
      if (controlledQuery === undefined) {
        setInternalQuery("");
      }
    } else if (!open && wasOpen) {
      composingRef.current = false;
      const deferred = pendingActionRef.current;
      pendingActionRef.current = null;
      if (deferred) {
        deferred();
      }
    }
  }, [open, controlledQuery]);

  const handleOpenChange = useCallback(
    (next: boolean) => {
      if (!next) {
        composingRef.current = false;
      }
      onOpenChange(next);
    },
    [onOpenChange],
  );

  const runWithClose = useCallback(
    (action: () => void, options?: StudioCommandRunOptions) => {
      if (options?.focusHandoff) {
        pendingActionRef.current = action;
        setSuppressFinalFocus(true);
        handleOpenChange(false);
        return;
      }
      setSuppressFinalFocus(false);
      handleOpenChange(false);
      action();
    },
    [handleOpenChange],
  );

  const runWithCloseRef = useRef(runWithClose);
  runWithCloseRef.current = runWithClose;

  // Bind native keydown on the input element so IME fallback, modified/repeat Enter blocking,
  // and 0-match plain Enter execute BEFORE React's delegated `@ui-sdk` handlers and document Escape.
  useEffect(() => {
    if (!open || !inputNode) return;
    const onNativeKeyDown = (event: KeyboardEvent) => {
      if (isNativeComposing(event)) {
        return;
      }
      if (composingRef.current) {
        if (IME_SENSITIVE_KEYS.has(event.key)) {
          event.stopPropagation();
        }
        return;
      }
      if (event.key === "Enter") {
        if (
          event.metaKey ||
          event.ctrlKey ||
          event.altKey ||
          event.shiftKey ||
          event.repeat
        ) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        const trimmed = queryRef.current.trim();
        if (trimmed && matchCountRef.current === 0 && onEmptyEnterRef.current) {
          event.preventDefault();
          event.stopPropagation();
          onEmptyEnterRef.current(trimmed, runWithCloseRef.current);
        }
      }
    };
    inputNode.addEventListener("keydown", onNativeKeyDown, { capture: true });
    return () => {
      inputNode.removeEventListener("keydown", onNativeKeyDown, { capture: true });
    };
  }, [open, inputNode]);

  const attachInputRef = useCallback((el: HTMLInputElement | null) => {
    inputRef.current = el;
    setInputNode(el);
  }, []);

  const itemMap = useMemo(() => {
    const map = new Map<string, StudioCommandItem>();
    for (const group of groups) {
      for (const item of group.items) {
        map.set(item.id, item);
      }
    }
    if (emptyGroups) {
      for (const group of emptyGroups) {
        for (const item of group.items) {
          if (!map.has(item.id)) {
            map.set(item.id, item);
          }
        }
      }
    }
    return map;
  }, [groups, emptyGroups]);

  const dataItems = useMemo(() => toCommandData(groups), [groups]);
  const dataEmptyItems = useMemo(
    () => (emptyGroups ? toCommandData(emptyGroups) : undefined),
    [emptyGroups],
  );

  const handleAction = useCallback(
    (value: string) => {
      const item = itemMap.get(value);
      if (!item || item.disabled) return;
      runWithClose(item.onSelect, { focusHandoff: item.focusHandoff });
    },
    [itemMap, runWithClose],
  );

  const handleDialogKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (isReactComposing(event) || composingRef.current) {
        return;
      }
      if (
        closeOnModK &&
        (event.metaKey || event.ctrlKey) &&
        !event.altKey &&
        !event.shiftKey &&
        !event.repeat &&
        event.key.toLowerCase() === "k"
      ) {
        event.preventDefault();
        setSuppressFinalFocus(false);
        handleOpenChange(false);
      }
    },
    [closeOnModK, handleOpenChange],
  );

  const renderDataEntries = (entries: CommandData): ReactNode =>
    entries.map((entry, index) => {
      if (typeof entry === "string") {
        const meta = itemMap.get(entry);
        return (
          <CommandItem
            key={entry}
            value={entry}
            label={meta?.label ?? entry}
            className="command-palette-item"
          >
            <CommandItemText className="command-palette-item-title">
              {meta?.label ?? entry}
            </CommandItemText>
          </CommandItem>
        );
      }
      if ("type" in entry) {
        if (entry.type === "separator") {
          return (
            <CommandSeparator
              key={`separator-${index}`}
              className="command-palette-separator"
            />
          );
        }
        return (
          <Fragment key={`group-${entry.label ?? index}`}>
            {index > 0 && (
              <CommandSeparator className="command-palette-separator" />
            )}
            <CommandGroup className="command-palette-group">
              {entry.label && (
                <CommandGroupLabel className="command-palette-group-label">
                  {entry.label}
                </CommandGroupLabel>
              )}
              {renderDataEntries(entry.items)}
            </CommandGroup>
          </Fragment>
        );
      }
      const meta = itemMap.get(entry.value);
      const label = meta?.label ?? entry.label;
      return (
        <CommandItem
          key={entry.value}
          value={entry.value}
          label={label}
          keywords={entry.keywords}
          disabled={entry.disabled}
          className="command-palette-item"
          data-danger={meta?.danger ? "" : undefined}
          data-active={meta?.active ? "" : undefined}
        >
          {meta?.icon && (
            <CommandItemIcon className="command-palette-item-icon">
              {meta.icon}
            </CommandItemIcon>
          )}
          <span className="command-palette-item-copy">
            <CommandItemText className="command-palette-item-title">
              {label}
            </CommandItemText>
            {meta?.subtitle && (
              <span className="command-palette-item-subtitle">
                {meta.subtitle}
              </span>
            )}
          </span>
          {meta?.active && (
            <span
              className="command-palette-item-active"
              aria-label={activeLabel}
            >
              ✓
            </span>
          )}
          {meta?.shortcut && (
            <CommandShortcut
              shortcut={meta.shortcut}
              className="command-palette-shortcut"
            />
          )}
        </CommandItem>
      );
    });

  const mergedContentClass = contentClassName
    ? `command-palette-dialog ${contentClassName}`
    : "command-palette-dialog";
  const mergedOverlayClass = overlayClassName
    ? `command-palette-overlay ${overlayClassName}`
    : "command-palette-overlay";

  return (
    <StudioDialog
      open={open}
      onOpenChange={handleOpenChange}
      title={title}
      hideTitle
      showCloseButton={false}
      initialFocusRef={inputRef}
      finalFocus={suppressFinalFocus ? "none" : undefined}
      onKeyDown={handleDialogKeyDown}
      contentClassName={mergedContentClass}
      overlayClassName={mergedOverlayClass}
      container={container}
    >
      <Command
        className="command-palette"
        query={query}
        onQueryChange={(next) => setQuery(next)}
        items={dataItems}
        emptyItems={dataEmptyItems}
        onAction={handleAction}
        announcement={announcement}
        pendingAnnouncement={pendingAnnouncement}
      >
        <div className="command-palette-input-row">
          {searchIcon && (
            <span className="command-palette-input-icon" aria-hidden="true">
              {searchIcon}
            </span>
          )}
          <CommandInput
            ref={attachInputRef}
            className="command-palette-input"
            aria-label={inputLabel}
            placeholder={inputPlaceholder}
            onCompositionStart={() => {
              composingRef.current = true;
            }}
            onCompositionEnd={() => {
              composingRef.current = false;
            }}
            onBlur={() => {
              composingRef.current = false;
            }}
          />
        </div>
        <CommandList className="command-palette-list">
          {(listState) => {
            matchCountRef.current = countVisibleItems(listState.items);
            return renderDataEntries(listState.items);
          }}
        </CommandList>
        <CommandEmpty className="command-palette-empty">
          <strong className="command-palette-empty-title">{emptyTitle}</strong>
          {emptyHint && (
            <span className="command-palette-empty-hint">{emptyHint}</span>
          )}
        </CommandEmpty>
        {footer?.({ query, runWithClose })}
      </Command>
    </StudioDialog>
  );
}
