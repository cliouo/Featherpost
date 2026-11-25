import { useCallback, useEffect, useRef } from "react";

export interface RevealScrollInput {
  scrollTop: number;
  viewportHeight: number;
  scrollHeight: number;
  itemTop: number;
  itemBottom: number;
  topInset?: number;
  bottomInset?: number;
  /** Top edge (content coordinates) of an immediately preceding non-sticky date group header. */
  groupHeaderTop?: number | null;
  /** Reveal a clipped preceding group header even if the row itself fits (e.g. keyboard Up onto row 0). */
  revealGroupHeader?: boolean;
}

export interface ScrollSurface {
  scrollTop: number;
  readonly clientHeight: number;
  readonly scrollHeight: number;
}

export interface ScrollOptions {
  repeat?: boolean;
  reducedMotion?: boolean;
}

export interface ScrollerDeps {
  now?: () => number;
  requestFrame?: (cb: (time: number) => void) => number;
  cancelFrame?: (id: number) => void;
  prefersReducedMotion?: () => boolean;
}

export interface ListScroller {
  scrollTo(container: ScrollSurface, targetTop: number, options?: ScrollOptions): void;
  cancel(): void;
}

export interface RevealIntent {
  repeat?: boolean;
  revealGroupHeader?: boolean;
}

export const SCROLL_DURATION_MS = 120;
const REPEAT_MAX_DURATION_MS = 45;
const REPEAT_SNAP_INTERVAL_MS = 20;
const EPSILON = 0.5;
const USER_SCROLL_TOLERANCE = 2;

const clamp = (value: number, min: number, max: number) =>
  Math.max(min, Math.min(max, value));

function parsePx(value: string | null | undefined): number {
  if (!value || value === "auto" || value === "normal") return 0;
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * Compute the minimal scrollTop needed to keep a selected row fully visible
 * inside its scroll container while staying clear of sticky bars/headers.
 * Returns null when the row is already fully visible (no scroll needed).
 */
export function computeRevealScrollTop(input: RevealScrollInput): number | null {
  const {
    scrollTop,
    viewportHeight,
    scrollHeight,
    itemTop,
    itemBottom,
    groupHeaderTop = null,
    revealGroupHeader = false,
  } = input;

  if (viewportHeight <= 0) return null;
  const maxScrollTop = Math.max(0, scrollHeight - viewportHeight);
  const topInset = Math.max(0, input.topInset ?? 0);
  const bottomInset = Math.max(0, input.bottomInset ?? 0);
  const visibleTop = scrollTop + topInset;
  const visibleBottom = scrollTop + viewportHeight - bottomInset;
  const effectiveHeight = visibleBottom - visibleTop;
  const itemHeight = Math.max(0, itemBottom - itemTop);

  let target: number | null = null;

  if (effectiveHeight <= 0 || itemHeight > effectiveHeight) {
    if (Math.abs(itemTop - visibleTop) > EPSILON) {
      target = clamp(itemTop - topInset, 0, maxScrollTop);
    }
  } else {
    const canIncludeHeader =
      groupHeaderTop !== null &&
      groupHeaderTop < itemTop &&
      itemBottom - groupHeaderTop <= effectiveHeight;

    const needsTopScroll = itemTop < visibleTop - EPSILON;
    const needsHeaderReveal =
      revealGroupHeader &&
      canIncludeHeader &&
      groupHeaderTop < visibleTop - EPSILON &&
      itemBottom <= visibleBottom + EPSILON;

    if (needsTopScroll || needsHeaderReveal) {
      const revealTop = canIncludeHeader ? groupHeaderTop : itemTop;
      target = clamp(revealTop - topInset, 0, maxScrollTop);
    } else if (itemBottom > visibleBottom + EPSILON) {
      target = clamp(itemBottom - (viewportHeight - bottomInset), 0, maxScrollTop);
    }
  }

  if (target === null || Math.abs(target - scrollTop) <= EPSILON) return null;
  return target;
}

const STICKY_CANDIDATE_SELECTOR =
  "[data-sticky-header], header, .date-group, .list-heading, .list-controls, .filter-tabs";

/**
 * Inspect scroll-padding and sticky elements inside `container` that can
 * obscure `item` at the top or bottom edge of the scrollport.
 */
export function measureStickyInsets(
  container: HTMLElement,
  item: HTMLElement,
): { topInset: number; bottomInset: number } {
  if (typeof window === "undefined" || typeof window.getComputedStyle !== "function") {
    return { topInset: 0, bottomInset: 0 };
  }

  const containerStyle = window.getComputedStyle(container);
  let topInset = parsePx(containerStyle.scrollPaddingTop);
  let bottomInset = parsePx(containerStyle.scrollPaddingBottom);

  const candidates = new Set<Element>();
  for (const child of Array.from(container.children)) {
    if (!child.classList.contains("message-row")) candidates.add(child);
  }
  for (const el of Array.from(container.querySelectorAll(STICKY_CANDIDATE_SELECTOR))) {
    candidates.add(el);
  }

  for (const el of candidates) {
    if (el === item || el.contains(item) || item.contains(el)) continue;
    const style = window.getComputedStyle(el);
    if (style.position !== "sticky") continue;
    const height = el.getBoundingClientRect().height;
    if (height <= 0) continue;

    const relation = el.compareDocumentPosition(item);
    if (
      style.top !== "auto" &&
      (relation & Node.DOCUMENT_POSITION_FOLLOWING) !== 0
    ) {
      topInset = Math.max(topInset, Math.max(0, parsePx(style.top)) + height);
    }
    if (
      style.bottom !== "auto" &&
      (relation & Node.DOCUMENT_POSITION_PRECEDING) !== 0
    ) {
      bottomInset = Math.max(bottomInset, Math.max(0, parsePx(style.bottom)) + height);
    }
  }

  return { topInset, bottomInset };
}

/**
 * Measure `item` inside `container` and return the minimal target scrollTop
 * to bring it into full view, or null if no scroll is needed.
 */
export function measureRevealScrollTop(
  container: HTMLElement,
  item: HTMLElement,
  options?: { revealGroupHeader?: boolean },
): number | null {
  const containerRect = container.getBoundingClientRect();
  const itemRect = item.getBoundingClientRect();
  const contentOrigin = containerRect.top + container.clientTop;
  const itemTop = container.scrollTop + (itemRect.top - contentOrigin);
  const itemBottom = container.scrollTop + (itemRect.bottom - contentOrigin);
  const { topInset, bottomInset } = measureStickyInsets(container, item);

  let groupHeaderTop: number | null = null;
  const prev = item.previousElementSibling;
  if (
    prev instanceof HTMLElement &&
    prev.classList.contains("date-group") &&
    (typeof window === "undefined" ||
      typeof window.getComputedStyle !== "function" ||
      window.getComputedStyle(prev).position !== "sticky")
  ) {
    groupHeaderTop = container.scrollTop + (prev.getBoundingClientRect().top - contentOrigin);
  }

  return computeRevealScrollTop({
    scrollTop: container.scrollTop,
    viewportHeight: container.clientHeight,
    scrollHeight: container.scrollHeight,
    itemTop,
    itemBottom,
    topInset,
    bottomInset,
    groupHeaderTop,
    revealGroupHeader: options?.revealGroupHeader,
  });
}

function defaultNow(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();
}

interface ActiveScroll {
  container: ScrollSurface;
  startTop: number;
  target: number;
  startTime: number;
  duration: number;
  lastWritten: number;
}

/**
 * Interruptible smooth scroll controller tuned for keyboard list navigation.
 * Uses short ease-out animation on single steps, retargets in place during key
 * repeat without queuing or starving animation frames, yields immediately to
 * manual scrolling, and falls back to instant scroll when
 * `prefers-reduced-motion: reduce` is set.
 */
export function createListScroller(deps: ScrollerDeps = {}): ListScroller {
  const now = deps.now ?? defaultNow;
  const requestFrame =
    deps.requestFrame ??
    ((cb: (time: number) => void) =>
      typeof requestAnimationFrame === "function"
        ? requestAnimationFrame(cb)
        : 0);
  const cancelFrame =
    deps.cancelFrame ??
    ((id: number) => {
      if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(id);
    });
  const isReducedMotion = deps.prefersReducedMotion ?? prefersReducedMotion;

  let rafId: number | null = null;
  let active: ActiveScroll | null = null;
  let lastCallAt: number | null = null;

  const cancel = () => {
    if (rafId !== null) {
      cancelFrame(rafId);
      rafId = null;
    }
    active = null;
  };

  const step = () => {
    rafId = null;
    const current = active;
    if (!current) return;
    if (Math.abs(current.container.scrollTop - current.lastWritten) > USER_SCROLL_TOLERANCE) {
      active = null;
      return;
    }
    const elapsed = Math.max(0, now() - current.startTime);
    const t = Math.min(1, elapsed / current.duration);
    const eased = 1 - (1 - t) ** 3;
    current.container.scrollTop =
      t >= 1
        ? current.target
        : current.startTop + (current.target - current.startTop) * eased;
    current.lastWritten = current.container.scrollTop;
    if (t < 1) {
      rafId = requestFrame(step);
    } else {
      active = null;
    }
  };

  const scrollTo = (
    container: ScrollSurface,
    targetTop: number,
    options?: ScrollOptions,
  ) => {
    const startTime = now();
    const dt = lastCallAt === null ? Infinity : Math.max(0, startTime - lastCallAt);
    lastCallAt = startTime;

    const maxScrollTop = Math.max(0, container.scrollHeight - container.clientHeight);
    const target = clamp(targetTop, 0, maxScrollTop);

    const reduced = options?.reducedMotion ?? isReducedMotion();
    let duration: number;
    if (reduced) {
      duration = 0;
    } else if (options?.repeat || dt < 60) {
      duration =
        dt <= REPEAT_SNAP_INTERVAL_MS
          ? 0
          : Math.min(REPEAT_MAX_DURATION_MS, Math.max(16, dt * 0.85));
    } else if (dt < SCROLL_DURATION_MS) {
      duration = Math.min(SCROLL_DURATION_MS, Math.max(45, dt * 0.9));
    } else {
      duration = SCROLL_DURATION_MS;
    }

    if (
      duration <= 0 ||
      Math.abs(target - container.scrollTop) <= EPSILON ||
      (typeof document !== "undefined" && document.hidden) ||
      (typeof requestAnimationFrame !== "function" && !deps.requestFrame)
    ) {
      cancel();
      container.scrollTop = target;
      return;
    }

    if (active && active.container === container && rafId !== null) {
      // Retarget in place so rapid key events update the in-flight animation
      // without cancelling and starving the scheduled animation frame.
      active.startTop = container.scrollTop;
      active.target = target;
      active.startTime = startTime;
      active.duration = duration;
      active.lastWritten = container.scrollTop;
      return;
    }

    cancel();
    active = {
      container,
      startTop: container.scrollTop,
      target,
      startTime,
      duration,
      lastWritten: container.scrollTop,
    };
    rafId = requestFrame(step);
  };

  return { scrollTo, cancel };
}

/**
 * Shared React hook that keeps the currently selected message row scrolled
 * into view across scopes, tabs, folders, and remote search results without
 * hijacking scroll position during background refreshes.
 */
export function useSelectedRowScroll<
  C extends HTMLElement,
  R extends HTMLElement,
>(
  selectedKey: string | null,
  scope: unknown,
  rows: readonly unknown[],
  remoteRows: readonly unknown[],
) {
  const listRef = useRef<C>(null);
  const selectedRowRef = useRef<R | null>(null);
  const scrollerRef = useRef<ListScroller | null>(null);
  const pendingIntentRef = useRef<RevealIntent | null>(null);
  const lastScrolledKeyRef = useRef<string | null>(null);

  const setSelectedRowRef = useCallback((el: R | null) => {
    selectedRowRef.current = el;
  }, []);

  const cancelScroll = useCallback(() => {
    scrollerRef.current?.cancel();
  }, []);

  const setPendingIntent = useCallback((intent: RevealIntent | null) => {
    pendingIntentRef.current = intent;
  }, []);

  const revealSelected = useCallback((intent: RevealIntent = {}) => {
    const container = listRef.current;
    const item = selectedRowRef.current;
    if (!container || !item) return false;
    const scroller = (scrollerRef.current ??= createListScroller());
    const targetTop = measureRevealScrollTop(container, item, {
      revealGroupHeader: intent.revealGroupHeader,
    });
    if (targetTop === null) scroller.cancel();
    else scroller.scrollTo(container, targetTop, { repeat: intent.repeat });
    return true;
  }, []);

  useEffect(() => () => scrollerRef.current?.cancel(), []);

  useEffect(() => {
    scrollerRef.current?.cancel();
    lastScrolledKeyRef.current = null;
  }, [scope]);

  useEffect(() => {
    if (!selectedKey) {
      scrollerRef.current?.cancel();
      lastScrolledKeyRef.current = null;
      pendingIntentRef.current = null;
      return;
    }
    if (selectedKey === lastScrolledKeyRef.current) return;
    const intent = pendingIntentRef.current;
    if (revealSelected(intent ?? undefined)) {
      lastScrolledKeyRef.current = selectedKey;
      pendingIntentRef.current = null;
    }
  }, [selectedKey, rows, remoteRows, revealSelected]);

  return {
    listRef,
    setSelectedRowRef,
    setPendingIntent,
    revealSelected,
    cancelScroll,
  };
}
