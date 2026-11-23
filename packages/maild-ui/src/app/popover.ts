export type PopoverDismissReason =
  | "outside-pointer"
  | "escape"
  | "focus-outside"
  | "window-blur"
  | "window-hidden"
  | "other-popover";

export type PopoverSide = "bottom" | "top";

export interface PopoverRootLike {
  contains(target: EventTarget | null): boolean;
}

export interface PopoverDocumentLike {
  hidden?: boolean;
  querySelectorAll?(selector: string): Iterable<PopoverRootLike>;
  addEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions): void;
  removeEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions): void;
}

export interface PopoverWindowLike {
  addEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions): void;
  removeEventListener(type: string, listener: EventListener, options?: boolean | AddEventListenerOptions): void;
}

export interface PopoverDismissOptions {
  /** Element containing both trigger and popover panel. */
  root: PopoverRootLike;
  /** Called at most once per open session. `returnFocus` is true only for Escape. */
  onDismiss(reason: PopoverDismissReason, returnFocus: boolean): void;
  doc?: PopoverDocumentLike;
  win?: PopoverWindowLike;
}

let activeDismiss: (() => void) | null = null;

/** Ensures only one popover menu is open at a time across the window. */
export function claimActivePopover(dismiss: () => void): () => void {
  if (activeDismiss && activeDismiss !== dismiss) {
    const previous = activeDismiss;
    activeDismiss = null;
    previous();
  }
  activeDismiss = dismiss;
  return () => {
    if (activeDismiss === dismiss) activeDismiss = null;
  };
}

/** Closes whichever popover menu is currently open, if any (e.g. when a modal dialog opens). */
export function closeAllPopovers(): void {
  if (!activeDismiss) return;
  const current = activeDismiss;
  activeDismiss = null;
  current();
}

function rootContains(root: PopoverRootLike, target: EventTarget | null): boolean {
  if (!target) return false;
  if (typeof Node !== "undefined" && !(target instanceof Node)) return false;
  return root.contains(target);
}

function isInPortaledMenu(target: EventTarget | null): boolean {
  const closest = (target as { closest?: (selector: string) => unknown } | null)?.closest;
  if (typeof closest !== "function") return false;
  return !!closest.call(target, "[data-radix-popper-content-wrapper], .studio-menu");
}

function hasExternalModal(doc: PopoverDocumentLike, root: PopoverRootLike): boolean {
  if (typeof doc.querySelectorAll !== "function") return false;
  for (const modal of doc.querySelectorAll('dialog[open], [aria-modal="true"]')) {
    if (typeof modal.contains === "function" && rootContains(modal, root as unknown as EventTarget)) continue;
    return true;
  }
  return false;
}

/**
 * Attaches shared dismissal behavior for an open popover menu:
 * - Capture-phase pointerdown/mousedown outside `root` (runs before Tauri drag.js).
 * - Capture-phase Escape key, returning focus to the trigger.
 * - Focus leaving `root` via keyboard (ignoring portaled context menus).
 * - Window blur / pagehide / document hidden (window close/hide or iframe click).
 * - Single-open mutual exclusion via `claimActivePopover`.
 */
export function attachPopoverDismissal({
  root,
  onDismiss,
  doc = typeof document !== "undefined" ? (document as unknown as PopoverDocumentLike) : undefined,
  win = typeof window !== "undefined" ? (window as unknown as PopoverWindowLike) : undefined,
}: PopoverDismissOptions): () => void {
  let closed = false;
  let release = () => {};
  const dismiss = (reason: PopoverDismissReason, returnFocus: boolean) => {
    if (closed) return;
    closed = true;
    release();
    onDismiss(reason, returnFocus);
  };

  release = claimActivePopover(() => dismiss("other-popover", false));
  if (!doc) return () => {
    closed = true;
    release();
  };

  const onPointer = (event: Event) => {
    if (rootContains(root, event.target)) return;
    dismiss("outside-pointer", false);
  };
  const onKeyDown = (event: Event) => {
    const keyEvent = event as KeyboardEvent;
    if (keyEvent.key !== "Escape" || keyEvent.isComposing) return;
    if (hasExternalModal(doc, root)) return;
    event.preventDefault();
    event.stopPropagation();
    dismiss("escape", true);
  };
  const onFocusIn = (event: Event) => {
    if (rootContains(root, event.target)) return;
    if (isInPortaledMenu(event.target)) return;
    dismiss("focus-outside", false);
  };
  const onBlur = () => dismiss("window-blur", false);
  const onPageHide = () => dismiss("window-hidden", false);
  const onVisibility = () => {
    if (doc.hidden) dismiss("window-hidden", false);
  };

  // Capture phase ensures outside clicks still dismiss over data-tauri-drag-region toolbars.
  doc.addEventListener("pointerdown", onPointer, true);
  doc.addEventListener("mousedown", onPointer, true);
  doc.addEventListener("keydown", onKeyDown, true);
  doc.addEventListener("focusin", onFocusIn, true);
  doc.addEventListener("visibilitychange", onVisibility);
  win?.addEventListener("blur", onBlur);
  win?.addEventListener("pagehide", onPageHide);

  return () => {
    closed = true;
    release();
    doc.removeEventListener("pointerdown", onPointer, true);
    doc.removeEventListener("mousedown", onPointer, true);
    doc.removeEventListener("keydown", onKeyDown, true);
    doc.removeEventListener("focusin", onFocusIn, true);
    doc.removeEventListener("visibilitychange", onVisibility);
    win?.removeEventListener("blur", onBlur);
    win?.removeEventListener("pagehide", onPageHide);
  };
}

/** Chooses whether an anchored popover opens below or flips above its trigger. */
export function computePopoverSide(metrics: {
  triggerTop: number;
  triggerBottom: number;
  panelHeight: number;
  viewportTop: number;
  viewportBottom: number;
  gap?: number;
}): PopoverSide {
  const gap = metrics.gap ?? 8;
  const spaceBelow = metrics.viewportBottom - metrics.triggerBottom;
  const spaceAbove = metrics.triggerTop - metrics.viewportTop;
  if (metrics.panelHeight > 0 && spaceBelow < metrics.panelHeight + gap && spaceAbove > spaceBelow) {
    return "top";
  }
  return "bottom";
}
