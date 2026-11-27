import { bindPersistentLayer, supportsTopLayer } from "@ui-sdk/core";
import { useEffect, useRef, type RefObject } from "react";

/**
 * Mirrors the `--z-*` stacking tokens in `src/styles/tokens.css`.
 * Used only on the Safari 16 (`!supportsTopLayer()`) fallback path; macOS 14+
 * uses the native Popover API top layer and requires no inline z-index.
 */
export const Z_INDEX_TOKENS = {
  dialogBackdrop: 60,
  dialog: 65,
  skipLink: 100,
  popover: 1000,
  tooltip: 1900,
  toast: 2000,
} as const;

const LAYER_STEP = 10;
const CONTENT_OFFSET = 5;

export interface FallbackLayer {
  readonly backdrop: number;
  readonly content: number;
  release(): void;
}

const activeLayers = new Set<symbol>();
let nextDepth = 0;

/**
 * Allocates a monotonic `(backdrop, content)` z-index pair for `!supportsTopLayer()`
 * overlays so nested dialogs/menus/selects open in strict stacking order and never
 * invert (e.g. a second dialog's backdrop always sits above the first dialog's content).
 */
export function acquireFallbackLayer(): FallbackLayer {
  const token = Symbol("ui-fallback-layer");
  activeLayers.add(token);
  nextDepth += 1;
  const backdrop = Z_INDEX_TOKENS.popover + nextDepth * LAYER_STEP;
  const content = backdrop + CONTENT_OFFSET;
  let released = false;
  return {
    backdrop,
    content,
    release() {
      if (released) return;
      released = true;
      activeLayers.delete(token);
      if (activeLayers.size === 0) {
        nextDepth = 0;
      }
    },
  };
}

/**
 * React hook returning a monotonic `{ backdrop, content }` z-index pair while `open`
 * is true on engines without Popover API top-layer support (`!supportsTopLayer()`).
 * Allocates synchronously during render so inline `zIndex` is present before
 * `@ui-sdk`'s layout bind runs.
 */
export function useFallbackLayer(
  open: boolean,
): { readonly backdrop: number; readonly content: number } | null {
  const layerRef = useRef<FallbackLayer | null>(null);
  const needsFallback = open && !supportsTopLayer();

  if (needsFallback) {
    if (!layerRef.current) {
      layerRef.current = acquireFallbackLayer();
    }
  } else if (layerRef.current) {
    layerRef.current.release();
    layerRef.current = null;
  }

  useEffect(() => {
    if (!needsFallback) return;
    if (!layerRef.current) {
      layerRef.current = acquireFallbackLayer();
    }
    const handle = layerRef.current;
    return () => {
      handle.release();
      if (layerRef.current === handle) {
        layerRef.current = null;
      }
    };
  }, [needsFallback]);

  return needsFallback && layerRef.current
    ? { backdrop: layerRef.current.backdrop, content: layerRef.current.content }
    : null;
}

/** Convenience hook for single-surface popovers (Select, Menu, Popover) on Safari 16 fallback. */
export function useFallbackZIndex(open: boolean): number | undefined {
  return useFallbackLayer(open)?.content;
}

/**
 * Isolated Safari 16 (`!supportsTopLayer()`) fallback z-index for tooltips.
 *
 * Intentionally does NOT allocate from `activeLayers` / `nextDepth`, so repeated
 * tooltip hovers while a Dialog stays open can never exhaust the stacking band or
 * overflow `.toast` (`2000`). Always sits above currently active fallback overlays
 * and strictly below `Z_INDEX_TOKENS.toast`.
 */
export function useFallbackTooltipZIndex(open: boolean): number | undefined {
  if (!open || supportsTopLayer()) return undefined;
  const activeTop =
    activeLayers.size > 0
      ? Z_INDEX_TOKENS.popover + nextDepth * LAYER_STEP + CONTENT_OFFSET
      : Z_INDEX_TOKENS.popover;
  return Math.min(
    Z_INDEX_TOKENS.toast - 1,
    Math.max(Z_INDEX_TOKENS.tooltip, activeTop + 1),
  );
}

/**
 * Keeps a persistent status overlay (such as Studio's `.toast` live region)
 * exempt from `@ui-sdk` modal `inert` trapping and promoted above modal dialogs
 * in Safari 17+ native top-layer via `popover="manual"` + `bindPersistentLayer()`.
 */
export function usePersistentOverlay(ref: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const topLayer = supportsTopLayer();
    if (topLayer) {
      el.setAttribute("popover", "manual");
      try {
        el.showPopover();
      } catch {
        // Ignore if already open or detached during teardown.
      }
    }

    const unbind = bindPersistentLayer(el);
    return () => {
      unbind();
      if (topLayer) {
        try {
          el.hidePopover();
        } catch {
          // Ignore if already hidden or detached.
        }
        el.removeAttribute("popover");
      }
    };
  }, [ref]);
}
