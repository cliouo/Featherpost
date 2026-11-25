import * as DropdownMenuPrimitive from "@radix-ui/react-dropdown-menu";
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

import { attachWindowDismissal } from "./popover";

export interface StudioDropdownMenuProps {
  /** Optional container class around the trigger (preserves layout styling like `.reader-menu` / `.account-more`). */
  className?: string;
  /** Trigger button element rendered via `DropdownMenu.Trigger asChild`. */
  trigger: ReactElement;
  /** Optional extra className for the portaled `DropdownMenu.Content`. */
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

function isInContextMenu(target: EventTarget | null): boolean {
  const closest = (
    target as { closest?: (selector: string) => unknown } | null
  )?.closest;
  if (typeof closest !== "function") return false;
  return Boolean(closest.call(target, ".studio-menu"));
}

/**
 * Unified dropdown menu primitive built over `@radix-ui/react-dropdown-menu`.
 *
 * Provides full roving focus (ArrowUp/ArrowDown/Home/End), typeahead,
 * Escape focus restoration, outside-click dismissal, collision flipping,
 * native Radix `DismissableLayer` mutual exclusion with Select/ContextMenu,
 * and `window-blur` / `visibilitychange` / `pagehide` dismissal.
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
  const open = !forceClosed && (controlledOpen ?? uncontrolledOpen);
  const openRef = useRef(open);
  openRef.current = open;
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  const suppressCloseFocusRef = useRef(false);

  const setOpen = useCallback(
    (next: boolean, suppressFocus = false) => {
      const target = forceClosed ? false : next;
      if (openRef.current === target) return;
      if (!target && suppressFocus) suppressCloseFocusRef.current = true;
      if (controlledOpen === undefined) setUncontrolledOpen(target);
      onOpenChangeRef.current?.(target);
    },
    [controlledOpen, forceClosed],
  );

  useEffect(() => {
    if (forceClosed && (controlledOpen ?? uncontrolledOpen)) {
      suppressCloseFocusRef.current = true;
      if (controlledOpen === undefined) setUncontrolledOpen(false);
      onOpenChangeRef.current?.(false);
    }
  }, [forceClosed, controlledOpen, uncontrolledOpen]);

  useEffect(() => {
    if (!open) return;
    return attachWindowDismissal({
      onDismiss: () => {
        setOpen(false, true);
      },
    });
  }, [open, setOpen]);

  const menu = (
    <DropdownMenuPrimitive.Root
      open={open}
      onOpenChange={(next) => setOpen(next)}
      modal={modal}
    >
      <DropdownMenuPrimitive.Trigger asChild>
        {trigger}
      </DropdownMenuPrimitive.Trigger>
      <DropdownMenuPrimitive.Portal container={container ?? undefined}>
        <DropdownMenuPrimitive.Content
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
          loop={loop}
          data-shortcuts-off=""
          data-tauri-drag-region="false"
          onFocusOutside={(event) => {
            if (isInContextMenu(event.target)) event.preventDefault();
          }}
          onPointerDownOutside={(event) => {
            if (isInContextMenu(event.target)) event.preventDefault();
          }}
          onCloseAutoFocus={(event) => {
            if (
              suppressCloseFocusRef.current ||
              (typeof document !== "undefined" &&
                document.querySelector('dialog[open], [aria-modal="true"]'))
            ) {
              suppressCloseFocusRef.current = false;
              event.preventDefault();
            }
          }}
        >
          {children}
        </DropdownMenuPrimitive.Content>
      </DropdownMenuPrimitive.Portal>
    </DropdownMenuPrimitive.Root>
  );

  return className ? <div className={className}>{menu}</div> : menu;
}

export interface StudioDropdownMenuItemProps
  extends ComponentPropsWithoutRef<typeof DropdownMenuPrimitive.Item> {
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
    <DropdownMenuPrimitive.Item
      ref={ref}
      className={
        className ? `studio-dropdown-item ${className}` : "studio-dropdown-item"
      }
      data-tone={tone === "danger" ? "danger" : undefined}
      {...props}
    />
  );
});

export const StudioDropdownMenuSeparator = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof DropdownMenuPrimitive.Separator>
>(function StudioDropdownMenuSeparator({ className, ...props }, ref) {
  return (
    <DropdownMenuPrimitive.Separator
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
