import {
  Tabs as UiSdkTabs,
  TabsIndicator as UiSdkTabsIndicator,
  TabsList as UiSdkTabsList,
  TabsPanel as UiSdkTabsPanel,
  TabsTab as UiSdkTabsTab,
  type TabsOrientation,
  type TabsValueChangeReason,
} from "@ui-sdk/react/tabs";
import { forwardRef, type ComponentPropsWithoutRef } from "react";

export type { TabsOrientation, TabsValueChangeReason };

export interface StudioTabsProps
  extends Omit<
    ComponentPropsWithoutRef<"div">,
    "defaultValue" | "onChange" | "onSelect"
  > {
  value?: string | null;
  defaultValue?: string | null;
  onValueChange?: (
    value: string | null,
    details: { reason: TabsValueChangeReason; event?: Event },
  ) => void;
  orientation?: TabsOrientation;
  /** `"automatic"` selects a tab on arrow focus; `"manual"` waits for Enter/Space. */
  activation?: "automatic" | "manual";
  loop?: boolean;
  hiddenUntilFound?: boolean;
}

/**
 * Studio's `@ui-sdk` Tabs boundary for genuine `tablist` + `tabpanel` views.
 *
 * Note: Do NOT use `Tabs` for `aria-pressed` filter toggles (such as Workspace's
 * All/Unread filter) or page-level `<nav aria-current="page">` navigation.
 */
export const StudioTabs = /* @__PURE__ */ forwardRef<HTMLDivElement, StudioTabsProps>(
  function StudioTabs(
    {
      value,
      defaultValue,
      onValueChange,
      orientation = "horizontal",
      activation = "automatic",
      loop = true,
      hiddenUntilFound = false,
      className,
      ...rest
    },
    ref,
  ) {
    return (
      <UiSdkTabs
        ref={ref}
        value={value}
        defaultValue={defaultValue}
        orientation={orientation}
        activation={activation}
        loop={loop}
        hiddenUntilFound={hiddenUntilFound}
        className={className ? `studio-tabs ${className}` : "studio-tabs"}
        onValueChange={(next, details) => {
          onValueChange?.(next, {
            reason: details.reason,
            event: details.event,
          });
        }}
        {...rest}
      />
    );
  },
);

export type StudioTabsListProps = ComponentPropsWithoutRef<typeof UiSdkTabsList>;

export const StudioTabsList = /* @__PURE__ */ forwardRef<HTMLDivElement, StudioTabsListProps>(
  function StudioTabsList({ className, ...props }, ref) {
    return (
      <UiSdkTabsList
        ref={ref}
        className={
          className ? `studio-tabs-list ${className}` : "studio-tabs-list"
        }
        {...props}
      />
    );
  },
);

export type StudioTabsTabProps = ComponentPropsWithoutRef<typeof UiSdkTabsTab>;

export const StudioTabsTab = /* @__PURE__ */ forwardRef<HTMLButtonElement, StudioTabsTabProps>(
  function StudioTabsTab({ className, ...props }, ref) {
    return (
      <UiSdkTabsTab
        ref={ref}
        data-tauri-drag-region="false"
        className={className ? `studio-tab ${className}` : "studio-tab"}
        {...props}
      />
    );
  },
);

export type StudioTabsPanelProps = ComponentPropsWithoutRef<
  typeof UiSdkTabsPanel
>;

export const StudioTabsPanel = /* @__PURE__ */ forwardRef<HTMLDivElement, StudioTabsPanelProps>(
  function StudioTabsPanel({ className, ...props }, ref) {
    return (
      <UiSdkTabsPanel
        ref={ref}
        className={
          className ? `studio-tabs-panel ${className}` : "studio-tabs-panel"
        }
        {...props}
      />
    );
  },
);

export type StudioTabsIndicatorProps = ComponentPropsWithoutRef<
  typeof UiSdkTabsIndicator
>;

export const StudioTabsIndicator = /* @__PURE__ */ forwardRef<
  HTMLSpanElement,
  StudioTabsIndicatorProps
>(function StudioTabsIndicator({ className, ...props }, ref) {
  return (
    <UiSdkTabsIndicator
      ref={ref}
      className={
        className
          ? `studio-tabs-indicator ${className}`
          : "studio-tabs-indicator"
      }
      {...props}
    />
  );
});
