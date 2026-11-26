import {
  Popover as UiSdkPopover,
  PopoverArrow as UiSdkPopoverArrow,
  PopoverClose as UiSdkPopoverClose,
  PopoverContent,
  PopoverDescription as UiSdkPopoverDescription,
  PopoverTitle as UiSdkPopoverTitle,
  PopoverTrigger,
  type PopoverFinalFocus,
  type PopoverInitialFocus,
} from "@ui-sdk/react/popover";
import {
  forwardRef,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from "react";

import { useWindowDismiss } from "./windowDismiss";
import { useFallbackZIndex } from "./zIndex";

export interface StudioPopoverProps {
  /** Optional container class around the trigger. */
  className?: string;
  /** Trigger button element rendered via `PopoverTrigger render={trigger}`. */
  trigger: ReactElement;
  /** Optional accessible heading rendered via `PopoverTitle`. */
  title?: ReactNode;
  /** Optional accessible description rendered via `PopoverDescription`. */
  description?: ReactNode;
  /** Optional extra className for the portaled `PopoverContent`. */
  contentClassName?: string;
  /** Optional inline styles merged into `PopoverContent`. */
  contentStyle?: CSSProperties;
  /** Optional accessible label for the popover panel (when no `title` is provided). */
  contentLabel?: string;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Forces the popover closed when true. */
  forceClosed?: boolean;
  /** Whether Escape, outside press, and window blur/hide dismiss the popover (default: `true`). */
  dismissible?: boolean;
  align?: "start" | "center" | "end";
  side?: "top" | "right" | "bottom" | "left";
  sideOffset?: number;
  alignOffset?: number;
  collisionPadding?: number;
  /** Defaults to `false` so Tab flows naturally and outside clicks stay usable. */
  modal?: boolean | "trap-focus";
  initialFocus?: PopoverInitialFocus;
  finalFocus?: PopoverFinalFocus;
  keepMounted?: boolean;
  container?: HTMLElement | null;
  children?: ReactNode;
}

/**
 * Studio's `@ui-sdk` Popover boundary for non-menu floating panels.
 *
 * - Built on `@ui-sdk/react/popover` for anchored placement, Escape/outside
 *   dismissal, and accessible `aria-expanded`/`aria-labelledby` wiring.
 * - Wires `useWindowDismiss` with `listenWindowBlur: true` because `@ui-sdk`'s
 *   popover preset only blurs on iframe focus by default; this guarantees
 *   single-fire dismissal on `window-blur`, `visibilitychange(hidden)`,
 *   `pagehide`, and cross-layer `closeAllPopovers()`.
 * - Applies monotonic inline `zIndex` on Safari 16 (`!supportsTopLayer()`).
 * - Sets `data-tauri-drag-region="false"` and `data-shortcuts-off=""` on
 *   popup content.
 */
export function StudioPopover({
  className,
  trigger,
  title,
  description,
  contentClassName,
  contentStyle,
  contentLabel,
  open: controlledOpen,
  defaultOpen = false,
  onOpenChange,
  forceClosed = false,
  dismissible = true,
  align = "start",
  side = "bottom",
  sideOffset = 6,
  alignOffset = 0,
  collisionPadding = 10,
  modal = false,
  initialFocus,
  finalFocus,
  keepMounted = false,
  container,
  children,
}: StudioPopoverProps) {
  const [uncontrolledOpen, setUncontrolledOpen] = useState(defaultOpen);
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

  // `@ui-sdk` Popover uses `blur: "iframe"` natively; enable `listenWindowBlur`
  // so `window-blur`, `visibilitychange(hidden)`, `pagehide`, and
  // `closeAllPopovers()` all dismiss cleanly in a desktop Tauri window.
  useWindowDismiss(open && dismissible, () => setOpen(false, true), {
    listenWindowBlur: true,
  });

  const mergedStyle: CSSProperties | undefined =
    fallbackZIndex !== undefined || contentStyle
      ? {
          ...contentStyle,
          ...(fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : {}),
        }
      : undefined;

  const popover = (
    <UiSdkPopover
      open={open}
      onOpenChange={(next) => {
        if (forceClosed && next) return;
        setOpen(next);
      }}
      dismissible={dismissible}
      modal={modal}
      container={container ?? undefined}
    >
      <PopoverTrigger render={trigger} data-tauri-drag-region="false" />
      <PopoverContent
        className={
          contentClassName
            ? `studio-popover ${contentClassName}`
            : "studio-popover"
        }
        aria-label={contentLabel}
        align={align}
        side={side}
        sideOffset={sideOffset}
        alignOffset={alignOffset}
        collisionPadding={collisionPadding}
        initialFocus={initialFocus}
        finalFocus={
          forceClosed || suppressFinalFocus ? "none" : finalFocus
        }
        keepMounted={keepMounted}
        data-shortcuts-off=""
        data-tauri-drag-region="false"
        style={mergedStyle}
      >
        {title !== undefined && (
          <StudioPopoverTitle>{title}</StudioPopoverTitle>
        )}
        {description !== undefined && (
          <StudioPopoverDescription>{description}</StudioPopoverDescription>
        )}
        {children}
      </PopoverContent>
    </UiSdkPopover>
  );

  return className ? <div className={className}>{popover}</div> : popover;
}

export const StudioPopoverTitle = /* @__PURE__ */ forwardRef<
  HTMLHeadingElement,
  ComponentPropsWithoutRef<typeof UiSdkPopoverTitle>
>(function StudioPopoverTitle({ className, ...props }, ref) {
  return (
    <UiSdkPopoverTitle
      ref={ref}
      className={
        className ? `studio-popover-title ${className}` : "studio-popover-title"
      }
      {...props}
    />
  );
});

export const StudioPopoverDescription = /* @__PURE__ */ forwardRef<
  HTMLParagraphElement,
  ComponentPropsWithoutRef<typeof UiSdkPopoverDescription>
>(function StudioPopoverDescription({ className, ...props }, ref) {
  return (
    <UiSdkPopoverDescription
      ref={ref}
      className={
        className
          ? `studio-popover-description ${className}`
          : "studio-popover-description"
      }
      {...props}
    />
  );
});

export const StudioPopoverClose = /* @__PURE__ */ forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof UiSdkPopoverClose>
>(function StudioPopoverClose({ className, ...props }, ref) {
  return (
    <UiSdkPopoverClose
      ref={ref}
      className={
        className ? `studio-popover-close ${className}` : "studio-popover-close"
      }
      {...props}
    />
  );
});

export const StudioPopoverArrow = /* @__PURE__ */ forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof UiSdkPopoverArrow>
>(function StudioPopoverArrow({ className, ...props }, ref) {
  return (
    <UiSdkPopoverArrow
      ref={ref}
      className={
        className ? `studio-popover-arrow ${className}` : "studio-popover-arrow"
      }
      {...props}
    />
  );
});
