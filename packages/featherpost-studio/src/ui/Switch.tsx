import {
  Switch as UiSdkSwitch,
  SwitchThumb,
  createSwitchController,
  type SwitchCheckedChangeReason,
} from "@ui-sdk/react/switch";
import {
  forwardRef,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type FocusEvent,
  type KeyboardEvent,
} from "react";

export interface SwitchChangeDetails {
  reason: SwitchCheckedChangeReason;
  event?: Event;
}

export interface UiSwitchProps
  extends Omit<
    ComponentPropsWithoutRef<"button">,
    "type" | "role" | "onChange" | "value"
  > {
  checked?: boolean;
  defaultChecked?: boolean;
  onCheckedChange?: (checked: boolean, details: SwitchChangeDetails) => void;
  disabled?: boolean;
  readOnly?: boolean;
  required?: boolean;
  name?: string;
  value?: string;
  form?: string;
}

/**
 * Studio's `@ui-sdk` Switch boundary.
 *
 * - Built on `@ui-sdk/react/switch` with semantic `role="switch"` and
 *   `aria-checked`, plus `@ui-sdk`'s `SwitchThumb` slot.
 * - Supports both Space and Enter activation (matching WAI-ARIA APG Switch
 *   guidance and Studio's prior `<button role="switch">` UX) without
 *   accidentally submitting enclosing settings forms on Enter.
 * - Preserves Studio's `.toggle` / `.toggle-track` / `.toggle-thumb` styling,
 *   light/dark theme tokens, `:focus-visible`, and `<fieldset disabled>` state.
 */
export const Switch = /* @__PURE__ */ forwardRef<HTMLButtonElement, UiSwitchProps>(
  function Switch(
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
    const [controller] = useState(() => createSwitchController());
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
      if (event.key === "Enter") {
        event.preventDefault();
        if (!event.repeat) {
          controller.toggle({ reason: "press", event: event.nativeEvent });
        }
      } else if (event.key === " ") {
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
      <UiSdkSwitch
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
        className={className ? `toggle ${className}` : "toggle"}
        onCheckedChange={(next, details) => {
          onCheckedChange?.(next, {
            reason: details.reason,
            event: details.event,
          });
        }}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onBlur={handleBlur}
        {...rest}
      >
        <span className="toggle-track" aria-hidden="true">
          <SwitchThumb className="toggle-thumb" />
        </span>
      </UiSdkSwitch>
    );
  },
);
