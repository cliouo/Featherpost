import { useEffect, useRef } from "react";

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
 * Single-dismiss window-lifecycle listener helper for popovers and menus:
 * - optional `window` `blur` (disabled when `@ui-sdk` already handles `window-blur`)
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

export interface UseWindowDismissOptions {
  /**
   * Whether to listen to `window` `blur`. Defaults to `false` because `@ui-sdk`
   * Select and Menu already handle `window-blur` natively; leaving this `false`
   * prevents double-firing while still handling `visibilitychange(hidden)`,
   * `pagehide`, and cross-layer `closeAllPopovers()`.
   */
  listenWindowBlur?: boolean;
}

/**
 * Shared single-dismiss window-lifecycle hook for `@ui-sdk` popovers.
 */
export function useWindowDismiss(
  open: boolean,
  onDismiss: (reason: WindowDismissReason) => void,
  options?: UseWindowDismissOptions,
): void {
  const onDismissRef = useRef(onDismiss);
  onDismissRef.current = onDismiss;
  const listenWindowBlur = options?.listenWindowBlur ?? false;

  useEffect(() => {
    if (!open) return;
    return attachWindowDismissal({
      listenWindowBlur,
      onDismiss: (reason) => onDismissRef.current(reason),
    });
  }, [open, listenWindowBlur]);
}
