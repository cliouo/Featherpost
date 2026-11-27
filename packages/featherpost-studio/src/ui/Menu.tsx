import {
  Menu as UiSdkMenu,
  MenuContent,
  MenuItem,
  MenuSeparator,
  MenuTrigger,
} from "@ui-sdk/react/menu";
import {
  forwardRef,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type ReactElement,
  type ReactNode,
} from "react";

import { TooltipTriggerSuppressProvider } from "./Tooltip";
import { useWindowDismiss } from "./windowDismiss";
import { useFallbackZIndex } from "./zIndex";

export interface StudioDropdownMenuProps {
  /** Optional container class around the trigger (preserves layout styling like `.reader-menu` / `.account-more`). */
  className?: string;
  /** Trigger button element rendered via `MenuTrigger render={trigger}`. */
  trigger: ReactElement;
  /** Optional extra className for the portaled `MenuContent`. */
  contentClassName?: string;
  /** Optional accessible label override for the menu panel. */
  contentLabel?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Forces the menu closed when true (e.g. while window close is locked). */
  forceClosed?: boolean;
  align?: "start" | "center" | "end";
  side?: "top" | "right" | "bottom" | "left";
  sideOffset?: number;
  collisionPadding?: number;
  loop?: boolean;
  /** Defaults to `false` so Tab exits naturally, outside clicks work on first click, and context menus on selected text stay reachable. */
  modal?: boolean;
  container?: HTMLElement | null;
  children?: ReactNode;
}

/**
 * Studio's `@ui-sdk` DropdownMenu boundary.
 *
 * - Built on `@ui-sdk/react/menu` for roving focus (ArrowUp/ArrowDown/Home/End),
 *   typeahead, Escape focus restoration, Menu->Dialog focus handoff without
 *   stealing focus back, and native `window-blur` dismissal.
 * - Wires `useWindowDismiss` (`visibilitychange(hidden)`, `pagehide`,
 *   `closeAllPopovers()`) with `listenWindowBlur: false` to avoid double-firing.
 * - Applies monotonic inline `zIndex` on Safari 16 (`!supportsTopLayer()`).
 * - Sets `data-shortcuts-off=""` and `data-tauri-drag-region="false"` on popup content.
 */
export function StudioDropdownMenu({
  className,
  trigger,
  contentClassName,
  contentLabel,
  open: controlledOpen,
  onOpenChange,
  forceClosed = false,
  align = "start",
  side = "bottom",
  sideOffset = 5,
  collisionPadding = 10,
  loop = true,
  modal = false,
  container,
  children,
}: StudioDropdownMenuProps) {
  const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
  const [suppressFinalFocus, setSuppressFinalFocus] = useState(false);
  const open = !forceClosed && (controlledOpen ?? uncontrolledOpen);
  const openRef = useRef(open);
  openRef.current = open;
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;

  const fallbackZIndex = useFallbackZIndex(open);

  const setOpen = useCallback(
    (next: boolean, suppressFocus = false) => {
      const target = forceClosed ? false : next;
      if (openRef.current === target) return;
      if (target) {
        setSuppressFinalFocus(false);
      } else if (suppressFocus) {
        setSuppressFinalFocus(true);
      }
      if (controlledOpen === undefined) setUncontrolledOpen(target);
      onOpenChangeRef.current?.(target);
    },
    [controlledOpen, forceClosed],
  );

  useEffect(() => {
    if (forceClosed && (controlledOpen ?? uncontrolledOpen)) {
      setSuppressFinalFocus(true);
      if (controlledOpen === undefined) setUncontrolledOpen(false);
      onOpenChangeRef.current?.(false);
    }
  }, [forceClosed, controlledOpen, uncontrolledOpen]);

  // `@ui-sdk` Menu already handles `window-blur` natively; only supplement
  // `visibilitychange(hidden)`, `pagehide`, and `closeAllPopovers()`.
  useWindowDismiss(open, () => setOpen(false, true));

  const menu = (
    <UiSdkMenu
      open={open}
      onOpenChange={(next) => {
        if (forceClosed && next) return;
        setOpen(next);
      }}
      loop={loop}
      modal={modal}
      container={container ?? undefined}
    >
      <TooltipTriggerSuppressProvider suppressed={open}>
        <MenuTrigger render={trigger} />
      </TooltipTriggerSuppressProvider>
      <MenuContent
        className={
          contentClassName
            ? `studio-dropdown ${contentClassName}`
            : "studio-dropdown"
        }
        aria-label={contentLabel}
        align={align}
        side={side}
        sideOffset={sideOffset}
        collisionPadding={collisionPadding}
        finalFocus={forceClosed || suppressFinalFocus ? "none" : undefined}
        data-shortcuts-off=""
        data-tauri-drag-region="false"
        style={fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : undefined}
      >
        {children}
      </MenuContent>
    </UiSdkMenu>
  );

  return className ? <div className={className}>{menu}</div> : menu;
}

export interface StudioDropdownMenuItemProps
  extends Omit<ComponentPropsWithoutRef<typeof MenuItem>, "destructive"> {
  tone?: "default" | "danger";
}

export const StudioDropdownMenuItem = forwardRef<
  HTMLDivElement,
  StudioDropdownMenuItemProps
>(function StudioDropdownMenuItem(
  { tone = "default", className, ...props },
  ref,
) {
  return (
    <MenuItem
      ref={ref}
      className={
        className ? `studio-dropdown-item ${className}` : "studio-dropdown-item"
      }
      destructive={tone === "danger" || undefined}
      data-tone={tone === "danger" ? "danger" : undefined}
      {...props}
    />
  );
});

export const StudioDropdownMenuSeparator = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof MenuSeparator>
>(function StudioDropdownMenuSeparator({ className, ...props }, ref) {
  return (
    <MenuSeparator
      ref={ref}
      className={
        className
          ? `studio-dropdown-separator ${className}`
          : "studio-dropdown-separator"
      }
      {...props}
    />
  );
});

/** Non-menu informational block inside a dropdown (e.g. AccountMore ID/revision); keeps text selectable. */
export function StudioDropdownMenuMeta({
  className,
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={
        className ? `studio-dropdown-meta ${className}` : "studio-dropdown-meta"
      }
    >
      {children}
    </div>
  );
}
