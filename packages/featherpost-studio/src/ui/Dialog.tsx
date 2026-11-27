import { m } from "@featherpost/i18n/messages";
import {
  AlertDialog as UiSdkAlertDialog,
  AlertDialogBackdrop,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogTitle,
} from "@ui-sdk/react/alert-dialog";
import {
  Dialog as UiSdkDialog,
  DialogBackdrop,
  DialogContent,
  DialogDescription,
  DialogTitle,
  type DialogFinalFocus,
  type DialogInitialFocus,
  type DialogOpenChangeDetails,
} from "@ui-sdk/react/dialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";

import { useUiLocale } from "../i18n";
import { closeAllPopovers } from "./windowDismiss";
import { useFallbackLayer } from "./zIndex";

const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect;

const PERSISTENT_OVERLAY_SELECTORS = [".toast"];

export type StudioDialogRole = "dialog" | "alertdialog";
export type StudioDialogVariant = "dialog" | "sheet";

export interface StudioDialogProps {
  /** Controlled open state. Defaults to `true` for conditionally mounted dialogs. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Invoked after open/close transitions settle. */
  onOpenChangeComplete?: (open: boolean) => void;
  /** Convenience close callback invoked when dismissal is allowed. */
  onClose?: () => void;
  title: ReactNode;
  /** Visually hides the dialog title with `.visually-hidden` while preserving ARIA labeling. */
  hideTitle?: boolean;
  /** Optional accessible description rendered via DialogDescription (`aria-describedby`). */
  description?: ReactNode;
  descriptionClassName?: string;
  /** ARIA dialog role; `alertdialog` uses `@ui-sdk/react/alert-dialog` and disables outside-click dismissal. */
  role?: StudioDialogRole;
  /** Visual surface variant: `.mail-dialog` (with header) or `.sheet`. */
  variant?: StudioDialogVariant;
  /** Blocks Escape, outside-click, close button, and open->false dismissal while busy. */
  busy?: boolean;
  /** Whether clicking outside dismisses the dialog (default: true for `dialog`, false for `alertdialog`). */
  dismissOnOutsideClick?: boolean;
  /** Shows the header row with title and close button (default: true for `dialog`, false for `sheet`). */
  showCloseButton?: boolean;
  /** Optional element ref to receive initial focus on open. */
  initialFocusRef?: RefObject<HTMLElement | null>;
  /** Optional `@ui-sdk` initial focus target (takes precedence over `initialFocusRef`). */
  initialFocus?: DialogInitialFocus;
  /** Optional `@ui-sdk` final focus target when the dialog closes. */
  finalFocus?: DialogFinalFocus;
  /** Keydown hook inside dialog content, called before stopping React propagation. */
  onKeyDown?: (event: KeyboardEvent<HTMLDivElement>) => void;
  contentClassName?: string;
  overlayClassName?: string;
  container?: HTMLElement | null;
  children?: ReactNode;
}

/**
 * Studio's `@ui-sdk` Dialog / AlertDialog boundary.
 *
 * - Uses `@ui-sdk/react/dialog` for `role="dialog"` and `@ui-sdk/react/alert-dialog`
 *   for `role="alertdialog"`, sharing `@ui-sdk`'s native layer tree with Select & Menu.
 * - Maps `busy` to `dismissible={!busy}` so Escape & outside press are blocked natively.
 * - Closes any open popovers (`closeAllPopovers()`) on open.
 * - Allocates paired monotonic `(backdrop, content)` z-index values on Safari 16
 *   (`!supportsTopLayer()`) so stacked dialogs never invert z-order.
 * - Exempts `.toast` from modal `inert` / outside-press dismissal via `persistentElements`.
 */
export function StudioDialog({
  open = true,
  onOpenChange,
  onOpenChangeComplete,
  onClose,
  title,
  hideTitle = false,
  description,
  descriptionClassName,
  role = "dialog",
  variant = "dialog",
  busy = false,
  dismissOnOutsideClick,
  showCloseButton,
  initialFocusRef,
  initialFocus,
  finalFocus,
  onKeyDown,
  contentClassName,
  overlayClassName,
  container,
  children,
}: StudioDialogProps) {
  useUiLocale();

  const isAlert = role === "alertdialog";
  const allowOutsideClick = dismissOnOutsideClick ?? !isAlert;
  const hasCloseButton = showCloseButton ?? (!hideTitle && variant === "dialog");
  const titleClassName = hideTitle ? "visually-hidden" : undefined;

  const fallbackLayer = useFallbackLayer(open);
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useIsomorphicLayoutEffect(() => {
    if (!open) return;
    closeAllPopovers();
  }, [open]);

  const requestClose = useCallback(() => {
    if (busyRef.current) return;
    onOpenChangeRef.current?.(false);
    onCloseRef.current?.();
  }, []);

  const handleOpenChange = useCallback(
    (next: boolean, details: DialogOpenChangeDetails<string>) => {
      if (!next) {
        if (busyRef.current) {
          details.cancel();
          return;
        }
        onOpenChangeRef.current?.(false);
        onCloseRef.current?.();
        return;
      }
      onOpenChangeRef.current?.(true);
    },
    [],
  );

  const resolvedInitialFocus = useMemo<DialogInitialFocus | undefined>(() => {
    if (initialFocus !== undefined) return initialFocus;
    if (!initialFocusRef) return undefined;
    return () => {
      const target = initialFocusRef.current;
      return target && !target.hasAttribute("disabled") ? target : undefined;
    };
  }, [initialFocus, initialFocusRef]);

  const surfaceClass = variant === "sheet" ? "sheet" : "mail-dialog";
  const mergedContentClass = contentClassName
    ? `${surfaceClass} ${contentClassName}`
    : surfaceClass;
  const mergedOverlayClass = overlayClassName
    ? `dialog-overlay ${overlayClassName}`
    : "dialog-overlay";

  const backdropStyle = fallbackLayer
    ? { zIndex: fallbackLayer.backdrop }
    : undefined;
  const contentStyle = fallbackLayer
    ? { zIndex: fallbackLayer.content }
    : undefined;

  const handleContentKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    onKeyDown?.(event);
    // Let Escape bubble to `@ui-sdk`'s document layer stack so nested overlays
    // (such as a Tooltip opened on a button inside the Dialog) dismiss first
    // before the Dialog itself handles Escape.
    if (event.key !== "Escape") {
      event.stopPropagation();
    }
  };

  if (isAlert) {
    return (
      <UiSdkAlertDialog
        open={open}
        onOpenChange={handleOpenChange}
        onOpenChangeComplete={onOpenChangeComplete}
        dismissible={!busy}
        persistentElements={PERSISTENT_OVERLAY_SELECTORS}
        container={container ?? undefined}
      >
        <AlertDialogBackdrop
          className={mergedOverlayClass}
          data-shortcuts-off=""
          data-tauri-drag-region="false"
          style={backdropStyle}
        />
        <AlertDialogContent
          className={mergedContentClass}
          aria-modal="true"
          data-shortcuts-off=""
          data-tauri-drag-region="false"
          data-busy={busy ? "" : undefined}
          initialFocus={resolvedInitialFocus}
          finalFocus={finalFocus}
          style={contentStyle}
          onKeyDown={handleContentKeyDown}
        >
          {hasCloseButton ? (
            <header>
              <AlertDialogTitle className={titleClassName}>{title}</AlertDialogTitle>
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
            <AlertDialogTitle className={titleClassName}>{title}</AlertDialogTitle>
          )}
          {description !== undefined && (
            <AlertDialogDescription className={descriptionClassName}>
              {description}
            </AlertDialogDescription>
          )}
          {children}
        </AlertDialogContent>
      </UiSdkAlertDialog>
    );
  }

  return (
    <UiSdkDialog
      open={open}
      onOpenChange={handleOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      modal
      dismissible={!busy}
      closeOnOutsidePress={allowOutsideClick}
      persistentElements={PERSISTENT_OVERLAY_SELECTORS}
      container={container ?? undefined}
    >
      <DialogBackdrop
        className={mergedOverlayClass}
        data-shortcuts-off=""
        data-tauri-drag-region="false"
        style={backdropStyle}
      />
      <DialogContent
        className={mergedContentClass}
        aria-modal="true"
        data-shortcuts-off=""
        data-tauri-drag-region="false"
        data-busy={busy ? "" : undefined}
        initialFocus={resolvedInitialFocus}
        finalFocus={finalFocus}
        style={contentStyle}
        onKeyDown={handleContentKeyDown}
      >
        {hasCloseButton ? (
          <header>
            <DialogTitle className={titleClassName}>{title}</DialogTitle>
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
          <DialogTitle className={titleClassName}>{title}</DialogTitle>
        )}
        {description !== undefined && (
          <DialogDescription className={descriptionClassName}>
            {description}
          </DialogDescription>
        )}
        {children}
      </DialogContent>
    </UiSdkDialog>
  );
}

export type AlertDialogProps = Omit<StudioDialogProps, "role">;

/**
 * Confirmation / alert modal built on `StudioDialog` with `role="alertdialog"`
 * (`@ui-sdk/react/alert-dialog`) and outside-click dismissal disabled.
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
