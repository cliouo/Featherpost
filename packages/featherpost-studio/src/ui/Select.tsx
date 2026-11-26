import {
  Select as UiSdkSelect,
  SelectContent,
  SelectItem,
  SelectItemIndicator,
  SelectItemText,
  SelectTrigger,
  SelectValue,
} from "@ui-sdk/react/select";
import {
  useEffect,
  useRef,
  useState,
  type AriaAttributes,
  type ReactNode,
} from "react";
import { useWindowDismiss } from "./windowDismiss";
import { useFallbackZIndex } from "./zIndex";

export interface SelectOption {
  value: string;
  label: ReactNode;
  text: string;
  disabled?: boolean;
}

export interface UiSelectProps extends AriaAttributes {
  value: string | null;
  onValueChange(value: string): void;
  options: readonly SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  menuClassName?: string;
  title?: string;
  sideOffset?: number;
  collisionPadding?: number;
}

/**
 * Studio's `@ui-sdk` Select boundary.
 *
 * - Supports valid `value=""` natively without sentinel prefixes; unmatched
 *   values map to `null` so the placeholder renders cleanly.
 * - Preserves `selectionPending` focus retention while an async save disables
 *   the surrounding form, blocking duplicate submissions and reopening until
 *   the disabled state clears.
 * - Wires `useWindowDismiss` (`visibilitychange(hidden)`, `pagehide`,
 *   `closeAllPopovers()`) without double-firing on `@ui-sdk`'s native
 *   `window-blur`.
 * - Applies monotonic inline `zIndex` on Safari 16 (`!supportsTopLayer()`).
 */
export function Select({
  value,
  onValueChange,
  options,
  placeholder,
  disabled = false,
  id,
  className,
  menuClassName,
  title,
  sideOffset = 6,
  collisionPadding = 12,
  ...aria
}: UiSelectProps) {
  const selected =
    value === null ? undefined : options.find((item) => item.value === value);
  const resolvedValue = selected ? selected.value : null;

  const [open, setOpen] = useState(false);
  const [selectionPending, setSelectionPending] = useState(false);
  const pendingRef = useRef(false);

  const isOpen = open && !disabled;
  const fallbackZIndex = useFallbackZIndex(isOpen);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  // A selection can synchronously disable its form while an async save runs.
  // Keep the trigger focusable until that save settles so focus return on close
  // is not rejected by a native disabled button, while controlled guards still
  // block reopening and duplicate submissions.
  useEffect(() => {
    if (!open && !disabled && selectionPending) {
      pendingRef.current = false;
      setSelectionPending(false);
    }
  }, [open, disabled, selectionPending]);

  useWindowDismiss(isOpen, () => setOpen(false));

  const triggerClass = className ? `studio-picker ${className}` : "studio-picker";
  const contentClass = menuClassName
    ? `studio-picker-menu ${menuClassName}`
    : "studio-picker-menu";

  return (
    <UiSdkSelect
      value={resolvedValue}
      open={isOpen}
      disabled={disabled && !selectionPending}
      placeholder={placeholder}
      itemToLabel={(val) => options.find((item) => item.value === val)?.text ?? ""}
      onOpenChange={(next) => {
        if (disabled || (next && (selectionPending || pendingRef.current))) {
          setOpen(false);
          return;
        }
        setOpen(next);
      }}
      onValueChange={(next) => {
        if (disabled || selectionPending || pendingRef.current || next === null) {
          return;
        }
        const item = options.find((option) => option.value === next);
        if (!item || item.disabled) return;
        pendingRef.current = true;
        setSelectionPending(true);
        onValueChange(item.value);
      }}
    >
      <SelectTrigger
        {...aria}
        id={id}
        title={title}
        aria-disabled={disabled || undefined}
        data-disabled={disabled ? "" : undefined}
        data-tauri-drag-region="false"
        className={triggerClass}
      >
        <SelectValue placeholder={placeholder}>
          {selected ? selected.label : placeholder}
        </SelectValue>
        <span className="studio-picker-chevron" aria-hidden="true">
          <svg
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            aria-hidden="true"
          >
            <path d="m4 6 4 4 4-4" />
          </svg>
        </span>
      </SelectTrigger>
      <SelectContent
        className={contentClass}
        sideOffset={sideOffset}
        collisionPadding={collisionPadding}
        data-tauri-drag-region="false"
        data-shortcuts-off=""
        style={fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : undefined}
      >
        {options.map((item) => (
          <SelectItem
            className="studio-picker-option"
            key={item.value}
            value={item.value}
            disabled={item.disabled}
            label={item.text}
          >
            <SelectItemText>{() => item.label}</SelectItemText>
            <SelectItemIndicator
              className="studio-picker-check"
              aria-hidden="true"
            >
              ✓
            </SelectItemIndicator>
          </SelectItem>
        ))}
      </SelectContent>
    </UiSdkSelect>
  );
}
