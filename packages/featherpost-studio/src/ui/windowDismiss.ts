import { useEffect, useRef } from "react";
import {
  attachWindowDismissal,
  closeAllPopovers,
  type WindowDismissReason,
} from "../app/popover";

export { closeAllPopovers, type WindowDismissReason };

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
 * Reuses `src/app/popover.ts` so Radix and `@ui-sdk` overlays share one
 * `closeAllPopovers()` registry during incremental migration.
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
