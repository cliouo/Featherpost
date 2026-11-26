import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "@featherpost/i18n/react";
import * as Select from "@radix-ui/react-select";
import { Children, Fragment, isValidElement, useEffect, useState, type ReactNode } from "react";
import type { ChoiceProps } from "./settings/Choice";

interface Option { value: string; label: ReactNode; text: string; disabled: boolean }
function textOf(node: ReactNode): string {
  return Children.toArray(node).map((child) => isValidElement<{ children?: ReactNode }>(child)
    ? textOf(child.props.children) : String(child)).join("");
}
function optionsOf(children: ReactNode): Option[] {
  return Children.toArray(children).flatMap((child): Option[] => {
    if (!isValidElement<{ children?: ReactNode; value?: string; disabled?: boolean }>(child)) return [];
    if (child.type === Fragment) return optionsOf(child.props.children);
    if (child.type !== "option") return [];
    const text = textOf(child.props.children);
    return [{ value: child.props.value ?? text, label: child.props.children, text, disabled: !!child.props.disabled }];
  });
}

/** Radix owns focus, typeahead, keyboard selection, dismissal and popup collision handling. */
export function StudioSelect({ value, onValueChange, children, disabled, className, ...label }: ChoiceProps) {
  useUiLocale();
  const items = optionsOf(children);
  const selected = items.findIndex((item) => item.value === value);
  const [open, setOpen] = useState(false);
  const [selectionPending, setSelectionPending] = useState(false);
  // Keep option identity stable when a profile/folder refresh reorders items.
  // The prefix also lets Radix represent our valid empty-string wire values.
  const keyOf = (wireValue: string) => `choice:${wireValue}`;
  useEffect(() => { if (disabled) setOpen(false); }, [disabled]);
  // A selection can synchronously disable its form while an async save runs.
  // Keep this trigger focusable until that save settles: Radix returns focus
  // on close, and a native disabled button would reject it (or lose it again).
  // The controlled open/value guards still block interaction while disabled.
  useEffect(() => {
    if (!open && !disabled) setSelectionPending(false);
  }, [open, disabled]);
  return (
    <Select.Root value={selected < 0 ? "" : keyOf(value)} open={open && !disabled}
      onOpenChange={(next) => setOpen(!disabled && next)} disabled={disabled && !selectionPending}
      onValueChange={(key) => {
        const item = items.find((option) => key === keyOf(option.value));
        if (!disabled && item && !item.disabled) {
          setSelectionPending(true);
          onValueChange(item.value);
        }
      }}>
      <Select.Trigger {...label} aria-disabled={disabled || undefined}
        data-disabled={disabled ? "" : undefined} className={`studio-picker ${className ?? ""}`}>
        <Select.Value placeholder={m.studio_select_placeholder()}>{items[selected]?.label ?? m.studio_select_placeholder()}</Select.Value>
        <Select.Icon className="studio-picker-chevron">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="m4 6 4 4 4-4" /></svg>
        </Select.Icon>
      </Select.Trigger>
      <Select.Portal>
        <Select.Content className="studio-picker-menu" position="popper" sideOffset={6} collisionPadding={12}>
          <Select.ScrollUpButton className="studio-picker-scroll" aria-label={m.studio_select_scroll_up()}>⌃</Select.ScrollUpButton>
          <Select.Viewport className="studio-picker-options">
            {items.map((item) => (
              <Select.Item className="studio-picker-option" key={item.value} value={keyOf(item.value)} disabled={item.disabled} textValue={item.text}>
                <Select.ItemText>{item.label}</Select.ItemText>
                <Select.ItemIndicator className="studio-picker-check" aria-hidden="true">✓</Select.ItemIndicator>
              </Select.Item>
            ))}
          </Select.Viewport>
          <Select.ScrollDownButton className="studio-picker-scroll" aria-label={m.studio_select_scroll_down()}>⌄</Select.ScrollDownButton>
        </Select.Content>
      </Select.Portal>
    </Select.Root>
  );
}
