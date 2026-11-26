export type WindowDismissReason =
  | "window-blur"
  | "window-hidden"
  | "external";

export interface WindowDismissDocumentLike {
  hidden?: boolean;
  addEventListener(
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ): void;
}

export interface WindowDismissWindowLike {
  addEventListener(
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ): void;
}

export interface WindowDismissOptions {
  /** Called at most once per open session when the window blurs/hides or an external modal closes popovers. */
  onDismiss(reason: WindowDismissReason): void;
  /** Whether to attach a `window` `blur` listener (default `true`; set `false` when the primitive already handles `window-blur`). */
  listenWindowBlur?: boolean;
  doc?: WindowDismissDocumentLike;
  win?: WindowDismissWindowLike;
}

const openDismissers = new Set<() => void>();

/** Closes any open dropdown/popover menus registered with window dismissal (e.g. when a modal dialog opens). */
export function closeAllPopovers(): void {
  for (const dismiss of [...openDismissers]) {
    dismiss();
  }
}

/**
 * Supplements Radix `DismissableLayer` with window-lifecycle dismissal that
 * Radix does not handle on its own:
 * - `window` `blur` (e.g. focus moving into the sandboxed reading-pane iframe or another OS window)
 * - `window` `pagehide`
 * - `document` `visibilitychange` when `document.hidden` becomes true
 * - explicit `closeAllPopovers()` calls when a modal dialog opens
 */
export function attachWindowDismissal({
  onDismiss,
  listenWindowBlur = true,
  doc = typeof document !== "undefined"
    ? (document as unknown as WindowDismissDocumentLike)
    : undefined,
  win = typeof window !== "undefined"
    ? (window as unknown as WindowDismissWindowLike)
    : undefined,
}: WindowDismissOptions): () => void {
  let closed = false;
  const dismiss = (reason: WindowDismissReason) => {
    if (closed) return;
    closed = true;
    openDismissers.delete(externalDismiss);
    onDismiss(reason);
  };
  const externalDismiss = () => dismiss("external");
  openDismissers.add(externalDismiss);

  const onBlur = () => dismiss("window-blur");
  const onPageHide = () => dismiss("window-hidden");
  const onVisibility = () => {
    if (doc?.hidden) dismiss("window-hidden");
  };

  doc?.addEventListener("visibilitychange", onVisibility);
  if (listenWindowBlur) win?.addEventListener("blur", onBlur);
  win?.addEventListener("pagehide", onPageHide);

  return () => {
    closed = true;
    openDismissers.delete(externalDismiss);
    doc?.removeEventListener("visibilitychange", onVisibility);
    if (listenWindowBlur) win?.removeEventListener("blur", onBlur);
    win?.removeEventListener("pagehide", onPageHide);
  };
}
