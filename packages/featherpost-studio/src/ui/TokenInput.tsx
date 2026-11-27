import {
  Combobox,
  ComboboxChip,
  ComboboxChipRemove,
  ComboboxChips,
  ComboboxContent,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  createComboboxController,
} from "@ui-sdk/react/combobox";
import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type CompositionEvent as ReactCompositionEvent,
  type FocusEvent as ReactFocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useWindowDismiss } from "./windowDismiss";
import { useFallbackZIndex } from "./zIndex";

export interface TokenInputItem {
  value: string;
  label: string;
  secondary?: string;
  title?: string;
  invalid?: boolean;
}

export interface TokenInputSuggestion {
  value: string;
  label: string;
  secondary?: string;
  disabled?: boolean;
}

export interface TokenInputProps {
  id: string;
  name?: string;
  tokens: readonly TokenInputItem[];
  query: string;
  onQueryChange: (next: string) => void;
  suggestions: readonly TokenInputSuggestion[];
  loading?: boolean;
  disabled?: boolean;
  placeholder?: string;
  ariaLabel?: string;
  ariaInvalid?: boolean;
  ariaDescribedBy?: string;
  editingIndex?: number | null;
  onStartEdit?: (index: number) => void;
  onCancelEdit?: () => void;
  onCommitText: (
    raw: string,
    reason: "enter" | "delimiter" | "blur" | "tab",
  ) => void;
  onPasteText: (
    pasted: string,
    selectionStart: number,
    selectionEnd: number,
  ) => void;
  onSelectSuggestion: (suggestion: TokenInputSuggestion) => void;
  onRemoveToken: (index: number) => void;
  onOpenChange?: (open: boolean) => void;
  removeChipLabel: (item: TokenInputItem) => string;
  removeChipTitle: (item: TokenInputItem) => string;
  editChipHint?: string;
  emptyLabel?: string;
  loadingLabel?: string;
  suggestionsLabel?: string;
  isDelimiterKey?: (key: string, currentQuery: string) => boolean;
  /** Optional check after IME composition ends to commit delimiter-terminated input (e.g. full-width `，` / `；`). */
  shouldCommitOnCompositionEnd?: (text: string) => boolean;
  /** Optional dedup key normalizer used to exclude already-selected items from suggestions. */
  normalizeKey?: (value: string) => string;
  className?: string;
  contentClassName?: string;
  listClassName?: string;
}

function isComposingKeyboardEvent(
  event: ReactKeyboardEvent,
  composing: boolean,
): boolean {
  const native = event.nativeEvent;
  return native.isComposing === true || native.keyCode === 229 || composing;
}

const IME_SENSITIVE_KEYS = new Set([
  "Enter",
  "Escape",
  "ArrowDown",
  "ArrowUp",
  "Backspace",
  "Tab",
]);

/**
 * Studio's `@ui-sdk/react/combobox` multi-token boundary.
 *
 * - Only file in `src/` importing `@ui-sdk/react/combobox` (enforced by `test/uiBoundary.test.ts`).
 * - Places `data-shortcuts-off=""` ONLY on the open `<ComboboxContent>` portal, NEVER on the
 *   permanent chip input container (which instead carries `data-chip-input=""` for `Workspace.tsx`).
 * - Uses controlled `query` + `keepQuery` so `Tab`/`Shift+Tab` commits pending text once without
 *   silently wiping input while preserving native Tab focus traversal.
 * - Excludes already-selected values from suggestions and disables SDK secondary filtering
 *   (`filter={false}`) so backend/name-matched results are never hidden or toggle-deleted.
 */
export function TokenInput({
  id,
  name,
  tokens,
  query,
  onQueryChange,
  suggestions,
  loading = false,
  disabled = false,
  placeholder,
  ariaLabel,
  ariaInvalid = false,
  ariaDescribedBy,
  editingIndex = null,
  onStartEdit,
  onCancelEdit,
  onCommitText,
  onPasteText,
  onSelectSuggestion,
  onRemoveToken,
  onOpenChange,
  removeChipLabel,
  removeChipTitle,
  editChipHint,
  emptyLabel,
  loadingLabel,
  suggestionsLabel,
  isDelimiterKey,
  shouldCommitOnCompositionEnd,
  normalizeKey = (v) => v.trim(),
  className,
  contentClassName,
  listClassName,
}: TokenInputProps) {
  const [open, setOpen] = useState(false);
  const composingRef = useRef(false);
  const committedOnTabRef = useRef(false);
  const chipsRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const comboboxController = useMemo(
    () => createComboboxController<string, true>(),
    [],
  );

  const tokenValues = useMemo(() => tokens.map((t) => t.value), [tokens]);
  const selectedKeySet = useMemo(() => {
    const set = new Set<string>();
    tokens.forEach((token, idx) => {
      if (editingIndex !== null && idx === editingIndex) return;
      set.add(normalizeKey(token.value));
    });
    return set;
  }, [tokens, editingIndex, normalizeKey]);

  const visibleSuggestions = useMemo(
    () =>
      suggestions.filter(
        (suggestion) => !selectedKeySet.has(normalizeKey(suggestion.value)),
      ),
    [suggestions, selectedKeySet, normalizeKey],
  );

  const hasPopupContent =
    visibleSuggestions.length > 0 ||
    loading ||
    (Boolean(emptyLabel) && query.trim().length > 0);
  const isOpen = open && !disabled && hasPopupContent;
  const fallbackZIndex = useFallbackZIndex(isOpen);

  const setOpenState = useCallback(
    (next: boolean) => {
      setOpen((prev) => {
        if (prev === next) return prev;
        return next;
      });
      onOpenChange?.(next);
    },
    [onOpenChange],
  );

  useEffect(() => {
    if (disabled) setOpenState(false);
  }, [disabled, setOpenState]);

  useWindowDismiss(isOpen, () => setOpenState(false), {
    listenWindowBlur: true,
  });

  const prevEditingIndexRef = useRef<number | null>(editingIndex);
  useEffect(() => {
    if (editingIndex !== null && editingIndex !== prevEditingIndexRef.current) {
      const input = inputRef.current;
      if (input) {
        input.focus();
        input.select();
      }
    }
    prevEditingIndexRef.current = editingIndex;
  }, [editingIndex]);

  const handleOpenChange = useCallback(
    (next: boolean) => {
      if (disabled) {
        setOpenState(false);
        return;
      }
      setOpenState(next);
    },
    [disabled, setOpenState],
  );

  const handleQueryChange = useCallback(
    (
      nextQuery: string,
      details: { reason: string },
    ) => {
      // Ignore SDK's automatic query clearing on focus-out/escape/close:
      // controlled query + Tab/blur handlers own commits explicitly.
      if (
        details.reason === "focus-out" ||
        details.reason === "escape-key" ||
        details.reason === "outside-press" ||
        details.reason === "window-blur" ||
        details.reason === "item-press"
      ) {
        return;
      }
      committedOnTabRef.current = false;
      onQueryChange(nextQuery);
      if (details.reason === "input-change" && !open && !disabled) {
        setOpenState(true);
      }
    },
    [disabled, onQueryChange, open, setOpenState],
  );

  const handleValueChange = useCallback(
    (
      nextValues: string[],
      details: { reason: string },
    ) => {
      if (disabled) return;
      if (details.reason === "item-press") {
        const currentSet = new Set(tokenValues);
        const added = nextValues.find((v) => !currentSet.has(v));
        if (!added) return;
        const suggestion = visibleSuggestions.find((s) => s.value === added);
        if (!suggestion || suggestion.disabled) return;
        committedOnTabRef.current = false;
        onSelectSuggestion(suggestion);
        setOpenState(false);
        return;
      }
      if (details.reason === "remove-press") {
        const nextSet = new Set(nextValues);
        const removedIndex = tokenValues.findIndex((v) => !nextSet.has(v));
        if (removedIndex >= 0) {
          onRemoveToken(removedIndex);
        }
      }
    },
    [
      disabled,
      tokenValues,
      visibleSuggestions,
      onSelectSuggestion,
      onRemoveToken,
      setOpenState,
    ],
  );

  const handleInputKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLInputElement>) => {
      if (isComposingKeyboardEvent(event, composingRef.current)) {
        // Fallback guard when composingRef is set but browser omits isComposing/keyCode 229.
        if (
          composingRef.current &&
          !event.nativeEvent.isComposing &&
          event.nativeEvent.keyCode !== 229 &&
          IME_SENSITIVE_KEYS.has(event.key)
        ) {
          event.stopPropagation();
        }
        return;
      }

      // Let capture-phase Cmd/Ctrl+Enter form shortcut handle send without SDK swallowing it.
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
        return;
      }

      // Prevent Backspace while editing a chip in-place from deleting an adjacent chip.
      if (event.key === "Backspace" && editingIndex !== null) {
        event.stopPropagation();
        return;
      }

      if (event.key === "Escape") {
        if (editingIndex !== null && onCancelEdit) {
          event.preventDefault();
          event.stopPropagation();
          setOpenState(false);
          onCancelEdit();
          queueMicrotask(() => inputRef.current?.focus());
          return;
        }
        if (isOpen) {
          event.preventDefault();
          event.stopPropagation();
          setOpenState(false);
        }
        return;
      }

      if (event.key === "Tab") {
        if (isOpen) {
          setOpenState(false);
        }
        if (query.trim().length > 0 || editingIndex !== null) {
          committedOnTabRef.current = true;
          onCommitText(query, "tab");
        }
        // Intentionally do NOT call preventDefault so Tab / Shift+Tab focus traversal works normally.
        return;
      }

      if (
        !event.metaKey &&
        !event.ctrlKey &&
        !event.altKey &&
        isDelimiterKey?.(event.key, query)
      ) {
        event.preventDefault();
        event.stopPropagation();
        if (query.trim().length > 0 || editingIndex !== null) {
          onCommitText(query, "delimiter");
        }
        return;
      }

      if (event.key === "Enter") {
        if (event.repeat || event.altKey || event.shiftKey) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        const highlighted = isOpen ? comboboxController.getHighlighted() : null;
        if (isOpen && highlighted !== null) {
          // Let @ui-sdk select the highlighted suggestion item.
          return;
        }
        // Always prevent implicit HTML form submission, even on empty query with closed popup.
        event.preventDefault();
        event.stopPropagation();
        if (query.trim().length > 0 || editingIndex !== null) {
          onCommitText(query, "enter");
          setOpenState(false);
        }
      }
    },
    [
      comboboxController,
      editingIndex,
      isDelimiterKey,
      isOpen,
      onCancelEdit,
      onCommitText,
      query,
      setOpenState,
    ],
  );

  const handleCompositionStart = useCallback(() => {
    composingRef.current = true;
  }, []);

  const handleCompositionEnd = useCallback(
    (event: ReactCompositionEvent<HTMLInputElement>) => {
      composingRef.current = false;
      const value = event.currentTarget.value;
      if (shouldCommitOnCompositionEnd?.(value)) {
        onCommitText(value, "delimiter");
      } else if (value !== query) {
        onQueryChange(value);
      }
    },
    [onCommitText, onQueryChange, query, shouldCommitOnCompositionEnd],
  );

  const handleInputFocus = useCallback(() => {
    committedOnTabRef.current = false;
    if (!disabled) {
      setOpenState(true);
    }
  }, [disabled, setOpenState]);

  const handleContainerBlur = useCallback(
    (event: ReactFocusEvent<HTMLDivElement>) => {
      composingRef.current = false;
      const next = event.relatedTarget as Node | null;
      if (
        next &&
        (chipsRef.current?.contains(next) || contentRef.current?.contains(next))
      ) {
        return;
      }
      setOpenState(false);
      if (committedOnTabRef.current) {
        committedOnTabRef.current = false;
        return;
      }
      if (query.trim().length > 0 || editingIndex !== null) {
        onCommitText(query, "blur");
      }
    },
    [editingIndex, onCommitText, query, setOpenState],
  );

  const handlePaste = useCallback(
    (event: ReactClipboardEvent<HTMLInputElement>) => {
      if (disabled) return;
      const pasted = event.clipboardData?.getData("text/plain");
      if (!pasted) return;
      const input = event.currentTarget;
      const start = input.selectionStart ?? query.length;
      const end = input.selectionEnd ?? query.length;
      event.preventDefault();
      committedOnTabRef.current = false;
      onPasteText(pasted, start, end);
    },
    [disabled, onPasteText, query.length],
  );

  const handleChipKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>, index: number) => {
      if (disabled) return;
      if (event.key === "Enter" && !event.metaKey && !event.ctrlKey && !event.altKey) {
        event.preventDefault();
        event.stopPropagation();
        onStartEdit?.(index);
      }
    },
    [disabled, onStartEdit],
  );

  const itemToLabel = useCallback(
    (val: string) =>
      tokens.find((t) => t.value === val)?.label ??
      suggestions.find((s) => s.value === val)?.label ??
      val,
    [tokens, suggestions],
  );

  const inputElement = (
    <ComboboxInput
      key="token-input-field"
      ref={inputRef}
      id={id}
      name={name}
      className="recipient-input"
      placeholder={tokens.length === 0 && editingIndex === null ? placeholder : ""}
      aria-label={ariaLabel}
      aria-invalid={ariaInvalid || undefined}
      aria-describedby={ariaDescribedBy}
      autoCorrect="off"
      enterKeyHint="done"
      onKeyDown={handleInputKeyDown}
      onFocus={handleInputFocus}
      onPaste={handlePaste}
      onCompositionStart={handleCompositionStart}
      onCompositionEnd={handleCompositionEnd}
    />
  );

  const chipsClassName = className
    ? `recipient-chips ${className}`
    : "recipient-chips";
  const popupClassName = contentClassName
    ? `recipient-suggest-popup ${contentClassName}`
    : "recipient-suggest-popup";
  const suggestListClassName = listClassName
    ? `recipient-suggest-list ${listClassName}`
    : "recipient-suggest-list";

  return (
    <Combobox<string, true>
      multiple
      allowCustomValue
      filter={false}
      keepQuery
      open={isOpen}
      onOpenChange={handleOpenChange}
      value={tokenValues}
      onValueChange={handleValueChange}
      query={query}
      onQueryChange={handleQueryChange}
      disabled={disabled}
      controller={comboboxController}
      itemToLabel={itemToLabel}
    >
      <ComboboxChips
        ref={chipsRef}
        className={chipsClassName}
        data-chip-input=""
        data-tauri-drag-region="false"
        data-invalid={ariaInvalid ? "" : undefined}
        data-disabled={disabled ? "" : undefined}
        onBlur={handleContainerBlur}
      >
        {tokens.map((item, index) => {
          if (editingIndex === index) {
            return <Fragment key={`edit-${index}`}>{inputElement}</Fragment>;
          }
          const chipTitle = item.title
            ? editChipHint
              ? `${item.title} · ${editChipHint}`
              : item.title
            : editChipHint;
          return (
            <ComboboxChip
              key={`${item.value}-${index}`}
              value={item.value}
              className={`recipient-chip${item.invalid ? " invalid" : ""}`}
              data-invalid={item.invalid ? "" : undefined}
              title={chipTitle}
              onDoubleClick={() => {
                if (!disabled) onStartEdit?.(index);
              }}
              onKeyDown={(e) => handleChipKeyDown(e, index)}
            >
              <span className="recipient-chip-text">
                <span className="recipient-chip-label">{item.label}</span>
                {item.secondary && (
                  <span className="recipient-chip-address">{item.secondary}</span>
                )}
              </span>
              {!disabled && (
                <ComboboxChipRemove
                  className="recipient-chip-remove"
                  aria-label={removeChipLabel(item)}
                  title={removeChipTitle(item)}
                >
                  <span aria-hidden="true">×</span>
                </ComboboxChipRemove>
              )}
            </ComboboxChip>
          );
        })}
        {editingIndex === null && inputElement}
      </ComboboxChips>
      <ComboboxContent
        ref={contentRef}
        className={popupClassName}
        sideOffset={4}
        collisionPadding={12}
        data-tauri-drag-region="false"
        data-shortcuts-off=""
        style={
          fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : undefined
        }
      >
        <ComboboxList
          className={suggestListClassName}
          aria-label={suggestionsLabel}
        >
          {visibleSuggestions.map((item) => (
            <ComboboxItem
              key={item.value}
              value={item.value}
              label={item.label}
              disabled={item.disabled}
              className="recipient-suggest-item"
            >
              <span className="recipient-suggest-primary">{item.label}</span>
              {item.secondary && (
                <span className="recipient-suggest-secondary">
                  {item.secondary}
                </span>
              )}
            </ComboboxItem>
          ))}
        </ComboboxList>
        {loading && visibleSuggestions.length === 0 && loadingLabel && (
          <div
            className="recipient-suggest-status"
            role="status"
            aria-live="polite"
          >
            {loadingLabel}
          </div>
        )}
        {!loading &&
          visibleSuggestions.length === 0 &&
          emptyLabel &&
          query.trim().length > 0 && (
            <div
              className="recipient-suggest-empty"
              role="status"
              aria-live="polite"
            >
              {emptyLabel}
            </div>
          )}
      </ComboboxContent>
    </Combobox>
  );
}
