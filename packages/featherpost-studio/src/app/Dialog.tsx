import { m } from "@featherpost/i18n/messages";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";

import { useUiLocale } from "../i18n";
import { closeAllPopovers } from "./popover";

const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect;

export type StudioDialogRole = "dialog" | "alertdialog";
export type StudioDialogVariant = "dialog" | "sheet";

export interface StudioDialogProps {
  /** Controlled open state. Defaults to `true` for conditionally mounted dialogs. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Convenience close callback invoked when dismissal is allowed. */
  onClose?: () => void;
  title: ReactNode;
  /** Optional accessible description rendered via Dialog.Description (`aria-describedby`). */
  description?: ReactNode;
  descriptionClassName?: string;
  /** ARIA dialog role; `alertdialog` disables outside-click dismissal by default. */
  role?: StudioDialogRole;
  /** Visual surface variant: `.mail-dialog` (with header) or `.sheet`. */
  variant?: StudioDialogVariant;
  /** Blocks Escape, outside-click, close button, and open->false dismissal while busy. */
  busy?: boolean;
  /** Whether clicking outside dismisses the dialog (default: true for `dialog`, false for `alertdialog`). */
  dismissOnOutsideClick?: boolean;
  /** Shows the header row with title and close button (default: true for `dialog`, false for `sheet`). */
  showCloseButton?: boolean;
  /** Optional element to receive initial focus instead of Radix's first tabbable element. */
  initialFocusRef?: RefObject<HTMLElement | null>;
  /** Keydown hook inside dialog content, called before stopping React propagation. */
  onKeyDown?: (event: KeyboardEvent<HTMLDivElement>) => void;
  contentClassName?: string;
  overlayClassName?: string;
  container?: HTMLElement | null;
  children?: ReactNode;
}

/**
 * Unified modal Dialog / AlertDialog built over `@radix-ui/react-dialog`.
 *
 * Avoids macOS 13 (Safari 16) native `<dialog showModal()>` top-layer portal
 * conflicts with Radix Select/DropdownMenu/ContextMenu while providing focus
 * trapping, initial focus, focus restoration, `busy` close-blocking, and global
 * shortcut gating (`aria-modal="true"` + `data-shortcuts-off`).
 */
export function StudioDialog({
  open = true,
  onOpenChange,
  onClose,
  title,
  description,
  descriptionClassName,
  role = "dialog",
  variant = "dialog",
  busy = false,
  dismissOnOutsideClick,
  showCloseButton,
  initialFocusRef,
  onKeyDown,
  contentClassName,
  overlayClassName,
  container,
  children,
}: StudioDialogProps) {
  useUiLocale();

  const allowOutsideClick = dismissOnOutsideClick ?? role !== "alertdialog";
  const hasCloseButton = showCloseButton ?? variant === "dialog";

  const contentRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const allowOutsideClickRef = useRef(allowOutsideClick);
  allowOutsideClickRef.current = allowOutsideClick;
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const restoreFocus = useCallback(() => {
    const previous = previousFocusRef.current;
    previousFocusRef.current = null;
    if (!previous || !previous.isConnected) return;
    const active = document.activeElement;
    const shouldRestore =
      !active ||
      active === document.body ||
      active === document.documentElement ||
      (contentRef.current?.contains(active) ?? false);
    if (shouldRestore) previous.focus();
  }, []);

  useIsomorphicLayoutEffect(() => {
    if (!open) return;
    closeAllPopovers();
    const active = document.activeElement;
    if (
      active instanceof HTMLElement &&
      !(contentRef.current?.contains(active) ?? false)
    ) {
      previousFocusRef.current = active;
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    return () => {
      // Fallback in case the dialog unmounts before Radix's FocusScope unmount callback runs.
      setTimeout(restoreFocus, 0);
    };
  }, [open, restoreFocus]);

  const requestClose = useCallback(() => {
    if (busyRef.current) return;
    onOpenChangeRef.current?.(false);
    onCloseRef.current?.();
  }, []);

  const handleOpenChange = useCallback(
    (next: boolean) => {
      if (!next) {
        requestClose();
        return;
      }
      onOpenChangeRef.current?.(true);
    },
    [requestClose],
  );

  const surfaceClass = variant === "sheet" ? "sheet" : "mail-dialog";
  const mergedContentClass = contentClassName
    ? `${surfaceClass} ${contentClassName}`
    : surfaceClass;
  const mergedOverlayClass = overlayClassName
    ? `dialog-overlay ${overlayClassName}`
    : "dialog-overlay";

  return (
    <DialogPrimitive.Root open={open} onOpenChange={handleOpenChange} modal>
      <DialogPrimitive.Portal container={container ?? undefined}>
        <DialogPrimitive.Overlay
          className={mergedOverlayClass}
          data-shortcuts-off=""
          data-tauri-drag-region="false"
        />
        <div className="dialog-positioner">
          <DialogPrimitive.Content
            ref={contentRef}
            role={role}
            aria-modal="true"
            {...(description === undefined
              ? { "aria-describedby": undefined }
              : {})}
            className={mergedContentClass}
            data-shortcuts-off=""
            data-tauri-drag-region="false"
            data-busy={busy ? "" : undefined}
            onOpenAutoFocus={(event) => {
              const target = initialFocusRef?.current;
              if (target && !target.hasAttribute("disabled")) {
                event.preventDefault();
                target.focus();
              }
            }}
            onCloseAutoFocus={(event) => {
              event.preventDefault();
              restoreFocus();
            }}
            onEscapeKeyDown={(event) => {
              if (
                typeof document !== "undefined" &&
                document.querySelector('[data-slot="select-content"][data-open]')
              ) {
                event.preventDefault();
                closeAllPopovers();
                return;
              }
              if (busyRef.current) event.preventDefault();
            }}
            onPointerDownOutside={(event) => {
              const target = event.target as Element | null;
              if (
                busyRef.current ||
                !allowOutsideClickRef.current ||
                target?.closest?.('[data-slot="select-content"]')
              ) {
                event.preventDefault();
              }
            }}
            onInteractOutside={(event) => {
              const target = event.target as Element | null;
              if (
                busyRef.current ||
                !allowOutsideClickRef.current ||
                target?.closest?.('[data-slot="select-content"]')
              ) {
                event.preventDefault();
              }
            }}
            onKeyDown={(event) => {
              onKeyDown?.(event);
              event.stopPropagation();
            }}
          >
            {hasCloseButton ? (
              <header>
                <DialogPrimitive.Title>{title}</DialogPrimitive.Title>
                <button
                  type="button"
                  className="text-button"
                  disabled={busy}
                  onClick={requestClose}
                  aria-label={m.mailbox_dialog_close_label()}
                >
                  {m.mailbox_dialog_close()}
                </button>
              </header>
            ) : (
              <DialogPrimitive.Title>{title}</DialogPrimitive.Title>
            )}
            {description !== undefined && (
              <DialogPrimitive.Description className={descriptionClassName}>
                {description}
              </DialogPrimitive.Description>
            )}
            {children}
          </DialogPrimitive.Content>
        </div>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

export type AlertDialogProps = Omit<StudioDialogProps, "role">;

/**
 * Confirmation / alert modal built on `StudioDialog` with `role="alertdialog"`
 * and outside-click dismissal disabled by default.
 */
export function AlertDialog({
  dismissOnOutsideClick = false,
  ...props
}: AlertDialogProps) {
  return (
    <StudioDialog
      {...props}
      role="alertdialog"
      dismissOnOutsideClick={dismissOnOutsideClick}
    />
  );
}
