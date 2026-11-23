import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { attachPopoverDismissal, computePopoverSide, type PopoverSide } from "./popover";

const useIsomorphicLayoutEffect = typeof window !== "undefined" ? useLayoutEffect : useEffect;

function scrollBounds(element: HTMLElement): { top: number; bottom: number } {
  let top = 0;
  let bottom = window.innerHeight;
  let parent = element.parentElement;
  while (parent) {
    const overflowY = window.getComputedStyle(parent).overflowY;
    if (overflowY === "auto" || overflowY === "scroll" || overflowY === "clip") {
      const rect = parent.getBoundingClientRect();
      top = Math.max(top, rect.top);
      bottom = Math.min(bottom, rect.bottom);
    }
    parent = parent.parentElement;
  }
  return { top, bottom };
}

export interface PopoverMenuProps {
  className?: string;
  triggerClassName?: string;
  panelClassName?: string;
  trigger: ReactNode;
  triggerLabel: string;
  triggerTitle?: string;
  panelRole?: "menu" | "group" | "region";
  panelLabel?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Forces the popover closed when true (e.g. while window close is locked). */
  forceClosed?: boolean;
  /** Flips above the trigger when there is insufficient room below (default: true). */
  flip?: boolean;
  children: ReactNode;
}

/**
 * Shared anchored popover/menu primitive for both Studio and the bundled UI.
 * Keeps the panel DOM-anchored to its trigger so scrolling moves it in lockstep,
 * while centralizing outside-click, Escape + focus-return, and single-open dismissal.
 */
export function PopoverMenu({
  className,
  triggerClassName,
  panelClassName,
  trigger,
  triggerLabel,
  triggerTitle,
  panelRole,
  panelLabel,
  open: controlledOpen,
  onOpenChange,
  forceClosed = false,
  flip = true,
  children,
}: PopoverMenuProps) {
  const [uncontrolledOpen, setUncontrolledOpen] = useState(false);
  const open = controlledOpen ?? uncontrolledOpen;
  const openRef = useRef(open);
  openRef.current = open;
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;

  const setOpen = (next: boolean) => {
    if (openRef.current === next) return;
    if (controlledOpen === undefined) setUncontrolledOpen(next);
    onOpenChangeRef.current?.(next);
  };

  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [side, setSide] = useState<PopoverSide>("bottom");
  const panelId = useId();

  useEffect(() => {
    if (forceClosed && open) setOpen(false);
  }, [forceClosed, open]);

  useEffect(() => {
    if (!open) return;
    const root = rootRef.current;
    if (!root) return;
    return attachPopoverDismissal({
      root: root as unknown as { contains(target: EventTarget | null): boolean },
      onDismiss: (_reason, returnFocus) => {
        setOpen(false);
        if (returnFocus) triggerRef.current?.focus();
      },
    });
  }, [open]);

  useIsomorphicLayoutEffect(() => {
    if (!open || !flip) {
      setSide("bottom");
      return;
    }
    const triggerEl = triggerRef.current;
    const panelEl = panelRef.current;
    if (!triggerEl || !panelEl) return;
    const triggerRect = triggerEl.getBoundingClientRect();
    const panelRect = panelEl.getBoundingClientRect();
    const bounds = scrollBounds(triggerEl);
    setSide(
      computePopoverSide({
        triggerTop: triggerRect.top,
        triggerBottom: triggerRect.bottom,
        panelHeight: panelRect.height,
        viewportTop: bounds.top,
        viewportBottom: bounds.bottom,
      }),
    );
  }, [open, flip]);

  return (
    <div className={className} ref={rootRef}>
      <button
        ref={triggerRef}
        type="button"
        className={triggerClassName}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        aria-label={triggerLabel}
        title={triggerTitle}
        onClick={() => setOpen(!open)}
      >
        {trigger}
      </button>
      {open && (
        <div
          ref={panelRef}
          id={panelId}
          className={panelClassName}
          role={panelRole}
          aria-label={panelLabel}
          data-side={side}
          data-shortcuts-off=""
          data-tauri-drag-region="false"
        >
          {children}
        </div>
      )}
    </div>
  );
}
