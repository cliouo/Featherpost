import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "@featherpost/i18n/react";
import { Children, Fragment, isValidElement, type ReactNode } from "react";
import type { ChoiceProps } from "./settings/Choice";
import { Select, type SelectOption } from "./ui";

function textOf(node: ReactNode): string {
  return Children.toArray(node)
    .map((child) =>
      isValidElement<{ children?: ReactNode }>(child)
        ? textOf(child.props.children)
        : String(child),
    )
    .join("");
}

function optionsOf(children: ReactNode): SelectOption[] {
  return Children.toArray(children).flatMap((child): SelectOption[] => {
    if (
      !isValidElement<{
        children?: ReactNode;
        value?: string;
        disabled?: boolean;
      }>(child)
    ) {
      return [];
    }
    if (child.type === Fragment) return optionsOf(child.props.children);
    if (child.type !== "option") return [];
    const text = textOf(child.props.children);
    return [
      {
        value: child.props.value ?? text,
        label: child.props.children,
        text,
        disabled: !!child.props.disabled,
      },
    ];
  });
}

/** Studio's themed Choice renderer backed by `src/ui/Select` (`@ui-sdk/react/select`). */
export function StudioSelect({
  value,
  onValueChange,
  children,
  disabled,
  className,
  ...label
}: ChoiceProps) {
  useUiLocale();
  const options = optionsOf(children);
  return (
    <Select
      {...label}
      value={value}
      onValueChange={onValueChange}
      options={options}
      disabled={disabled}
      className={className}
      placeholder={m.studio_select_placeholder()}
    />
  );
}
