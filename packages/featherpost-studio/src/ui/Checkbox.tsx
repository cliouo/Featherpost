import {
  Checkbox as UiSdkCheckbox,
  CheckboxIndicator,
  createCheckboxController,
  type CheckboxCheckedChangeReason,
  type CheckedState,
} from "@ui-sdk/react/checkbox";
import {
  forwardRef,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type FocusEvent,
  type KeyboardEvent,
} from "react";

export type CheckboxCheckedState = CheckedState;

export interface CheckboxChangeDetails {
  reason: CheckboxCheckedChangeReason;
  event?: Event;
  shiftKey: boolean;
}

export interface UiCheckboxProps
  extends Omit<
    ComponentPropsWithoutRef<"button">,
    "type" | "role" | "onChange" | "value" | "defaultChecked"
  > {
  checked?: CheckboxCheckedState;
  defaultChecked?: CheckboxCheckedState;
  onCheckedChange?: (checked: boolean, details: CheckboxChangeDetails) => void;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  name?: string;
  value?: string;
  form?: string;
}

function eventHasShiftKey(event: Event | undefined): boolean {
  return Boolean(event && "shiftKey" in event && (event as MouseEvent | KeyboardEvent).shiftKey);
}

/**
 * Studio's `@ui-sdk` Checkbox boundary.
 *
 * - Built on `@ui-sdk/react/checkbox` (`role="checkbox"` on a `<button>` root),
 *   supporting checked, unchecked, and `"indeterminate"` (`aria-checked="mixed"`).
 * - Exposes `details.shiftKey` so message-list multi-select supports Shift-click
 *   range selection without leaking extra text into the accessible name.
 * - Handles Space toggle cleanly on keyup while keeping Enter non-toggling
 *   (matching native `<input type="checkbox">` / WAI-ARIA APG).
 * - Works alongside `Workspace.tsx`'s `[role="checkbox"]` shortcut gate without
 *   setting `data-shortcuts-off` globally (preserving `Shell.tsx` Cmd+K/L/,).
 */
export const Checkbox = /* @__PURE__ */ forwardRef<HTMLButtonElement, UiCheckboxProps>(
  function Checkbox(
    {
      checked,
      defaultChecked,
      onCheckedChange,
      disabled,
      readOnly,
      required,
      name,
      value,
      form,
      className,
      onKeyDown,
      onKeyUp,
      onBlur,
      ...rest
    },
    ref,
  ) {
    const [controller] = useState(() => createCheckboxController());
    const spacePressedRef = useRef(false);

    const isDisabledControl = (el: HTMLButtonElement) =>
      el.disabled ||
      el.getAttribute("aria-disabled") === "true" ||
      el.matches(":disabled");

    const handleKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
      onKeyDown?.(event);
      if (
        event.defaultPrevented ||
        event.metaKey ||
        event.ctrlKey ||
        event.altKey ||
        event.nativeEvent.isComposing ||
        isDisabledControl(event.currentTarget)
      ) {
        return;
      }
      if (event.key === " ") {
        event.preventDefault();
        if (!event.repeat) {
          spacePressedRef.current = true;
        }
      }
    };

    const handleKeyUp = (event: KeyboardEvent<HTMLButtonElement>) => {
      onKeyUp?.(event);
      if (event.defaultPrevented) return;
      if (event.key === " " && spacePressedRef.current) {
        spacePressedRef.current = false;
        if (isDisabledControl(event.currentTarget)) return;
        event.preventDefault();
        controller.toggle({ reason: "press", event: event.nativeEvent });
      }
    };

    const handleBlur = (event: FocusEvent<HTMLButtonElement>) => {
      spacePressedRef.current = false;
      onBlur?.(event);
    };

    return (
      <UiSdkCheckbox
        ref={ref}
        controller={controller}
        checked={checked}
        defaultChecked={defaultChecked}
        disabled={disabled}
        readOnly={readOnly}
        required={required}
        name={name}
        value={value}
        form={form}
        className={
          className ? `studio-checkbox ${className}` : "studio-checkbox"
        }
        onCheckedChange={(next, details) => {
          onCheckedChange?.(next === true, {
            reason: details.reason,
            event: details.event,
            shiftKey: eventHasShiftKey(details.event),
          });
        }}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onBlur={handleBlur}
        {...rest}
      >
        <CheckboxIndicator className="studio-checkbox-indicator">
          {(state) =>
            state.indeterminate ? (
              <svg
                viewBox="0 0 12 12"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                aria-hidden="true"
              >
                <path d="M2.5 6h7" />
              </svg>
            ) : (
              <svg
                viewBox="0 0 12 12"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <path d="M2.5 6.2 5 8.7 9.5 3.5" />
              </svg>
            )
          }
        </CheckboxIndicator>
      </UiSdkCheckbox>
    );
  },
);
