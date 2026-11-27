import {
  Tooltip as UiSdkTooltip,
  TooltipContent,
  TooltipShortcut,
  TooltipTrigger,
  type TooltipOpenChangeReason,
} from "@ui-sdk/react/tooltip";
import {
  cloneElement,
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type HTMLAttributes,
  type ReactElement,
  type ReactNode,
  type Ref,
} from "react";

import { formatAriaKeyShortcuts } from "../shortcuts";
import { subscribeGlobalDismiss } from "./windowDismiss";
import { useFallbackTooltipZIndex } from "./zIndex";

export interface StudioTooltipDefaults {
  readonly delay: number;
  readonly closeDelay: number;
  readonly sideOffset: number;
  readonly collisionPadding: number;
}

const DEFAULT_TOOLTIP_SETTINGS: StudioTooltipDefaults = {
  delay: 450,
  closeDelay: 0,
  sideOffset: 6,
  collisionPadding: 10,
};

interface TooltipDismissalChannel {
  getEpoch(): number;
  subscribe(listener: (epoch: number) => void): () => void;
}

function createDismissalChannel(): {
  channel: TooltipDismissalChannel;
  bump(): void;
} {
  let epoch = 0;
  const listeners = new Set<(epoch: number) => void>();
  return {
    channel: {
      getEpoch: () => epoch,
      subscribe(listener) {
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      },
    },
    bump() {
      epoch += 1;
      for (const listener of [...listeners]) {
        listener(epoch);
      }
    },
  };
}

// Fallback channel used when a `<StudioTooltip>` is rendered outside
// `<StudioTooltipProvider>` (e.g. isolated component tests). Ref-counted so
// global listeners attach lazily and detach cleanly once no standalone tooltips remain.
let fallbackEpoch = 0;
const fallbackListeners = new Set<(epoch: number) => void>();
let fallbackUnsubscribe: (() => void) | null = null;

const fallbackChannel: TooltipDismissalChannel = {
  getEpoch: () => fallbackEpoch,
  subscribe(listener) {
    if (fallbackListeners.size === 0) {
      fallbackUnsubscribe = subscribeGlobalDismiss(() => {
        fallbackEpoch += 1;
        for (const fn of [...fallbackListeners]) {
          fn(fallbackEpoch);
        }
      });
    }
    fallbackListeners.add(listener);
    return () => {
      fallbackListeners.delete(listener);
      if (fallbackListeners.size === 0) {
        fallbackUnsubscribe?.();
        fallbackUnsubscribe = null;
      }
    };
  },
};

interface TooltipProviderContextValue {
  readonly defaults: StudioTooltipDefaults;
  readonly channel: TooltipDismissalChannel;
}

const TooltipProviderContext = createContext<TooltipProviderContextValue>({
  defaults: DEFAULT_TOOLTIP_SETTINGS,
  channel: fallbackChannel,
});

/**
 * Explicit suppression context provided ONLY around `StudioDropdownMenu` and
 * `StudioPopover` trigger slots when their own panel is open.
 * Never wraps panel content and never auto-suppresses on generic `aria-expanded`.
 */
const TooltipTriggerSuppressContext = createContext<boolean>(false);

export function TooltipTriggerSuppressProvider({
  suppressed,
  children,
}: {
  suppressed: boolean;
  children: ReactNode;
}) {
  return (
    <TooltipTriggerSuppressContext.Provider value={suppressed}>
      {children}
    </TooltipTriggerSuppressContext.Provider>
  );
}

export interface StudioTooltipProviderProps {
  delay?: number;
  closeDelay?: number;
  sideOffset?: number;
  collisionPadding?: number;
  children?: ReactNode;
}

/**
 * Root coordinator for Studio tooltips.
 *
 * - Supplies shared defaults (`delay: 450`, `closeDelay: 0`, `sideOffset: 6`,
 *   `collisionPadding: 10`) via React Context.
 * - Listens globally to `window.blur`, `document.visibilitychange(hidden)`,
 *   `window.pagehide`, and `closeAllPopovers()` to increment a dismissal epoch,
 *   close any open tooltip, and invalidate in-flight cold-hover timers until the
 *   next genuine `pointerenter` or `focusin`.
 */
export function StudioTooltipProvider({
  delay = DEFAULT_TOOLTIP_SETTINGS.delay,
  closeDelay = DEFAULT_TOOLTIP_SETTINGS.closeDelay,
  sideOffset = DEFAULT_TOOLTIP_SETTINGS.sideOffset,
  collisionPadding = DEFAULT_TOOLTIP_SETTINGS.collisionPadding,
  children,
}: StudioTooltipProviderProps) {
  const controllerRef = useRef<ReturnType<typeof createDismissalChannel> | null>(
    null,
  );
  if (!controllerRef.current) {
    controllerRef.current = createDismissalChannel();
  }

  useEffect(() => {
    const controller = controllerRef.current!;
    return subscribeGlobalDismiss(() => {
      controller.bump();
    });
  }, []);

  const value = useMemo<TooltipProviderContextValue>(
    () => ({
      defaults: { delay, closeDelay, sideOffset, collisionPadding },
      channel: controllerRef.current!.channel,
    }),
    [delay, closeDelay, sideOffset, collisionPadding],
  );

  return (
    <TooltipProviderContext.Provider value={value}>
      {children}
    </TooltipProviderContext.Provider>
  );
}

export interface StudioTooltipProps
  extends Omit<HTMLAttributes<HTMLElement>, "children"> {
  /** Tooltip text or rich content. Empty/nullish values disable the tooltip while preserving forwarded refs/props. */
  label: ReactNode;
  /** Optional canonical shortcut string (e.g. `STUDIO_SHORTCUT_KEYS.composeNew`). Visual hint only; global key handling stays in Studio. */
  shortcut?: string;
  /**
   * Maps to `@ui-sdk` tooltip `type`:
   * - `"label"`: sets `aria-labelledby` (use ONLY when trigger accessible name equals tooltip text; never on dynamic `aria-label` like live counts).
   * - `"description"`: merges `aria-describedby` without clobbering trigger `aria-label` or visible text.
   */
  roleType?: "label" | "description";
  /** Explicitly suppresses the tooltip (e.g. while an owning menu/popover/dialog is open). Never auto-inferred from generic `aria-expanded`. */
  suppress?: boolean;
  /** Disables tooltip behavior without unmounting the trigger DOM anchor or dropping outer refs/props. */
  disabled?: boolean;
  side?: "top" | "right" | "bottom" | "left";
  align?: "start" | "center" | "end";
  sideOffset?: number;
  alignOffset?: number;
  collisionPadding?: number;
  delay?: number;
  closeDelay?: number;
  contentClassName?: string;
  container?: HTMLElement | null;
  children: ReactElement;
}

type AnyProps = Record<string, unknown>;
type ElementRef = Ref<HTMLElement> | undefined;

function getChildRef(element: ReactElement): ElementRef {
  const propsRef = (element.props as { ref?: ElementRef }).ref;
  if (propsRef !== undefined) return propsRef;
  return (element as unknown as { ref?: ElementRef }).ref;
}

function attachSingleRef(
  ref: NonNullable<ElementRef>,
  node: HTMLElement,
): (() => void) | void {
  if (typeof ref === "function") {
    const cleanup = ref(node);
    return typeof cleanup === "function" ? cleanup : undefined;
  }
  (ref as { current: HTMLElement | null }).current = node;
}

function detachSingleRef(
  ref: NonNullable<ElementRef>,
  cleanup: (() => void) | void,
): void {
  if (typeof cleanup === "function") {
    cleanup();
    return;
  }
  if (typeof ref === "function") {
    ref(null);
    return;
  }
  (ref as { current: HTMLElement | null }).current = null;
}

/**
 * Maintains a stable callback ref across renders so React never tears down
 * underlying `@ui-sdk` bindings on re-renders, while still cleanly attaching
 * or detaching individual refs when `disabled` toggles or outer refs change.
 */
function useStableMultiRef() {
  const nodeRef = useRef<HTMLElement | null>(null);
  const activeRefsRef = useRef<Array<NonNullable<ElementRef>>>([]);
  const cleanupsRef = useRef(
    new Map<NonNullable<ElementRef>, (() => void) | void>(),
  );

  const setRefs = (refs: readonly ElementRef[]) => {
    activeRefsRef.current = refs.filter(
      (entry): entry is NonNullable<ElementRef> => Boolean(entry),
    );
  };

  useLayoutEffect(() => {
    const node = nodeRef.current;
    if (!node) return;

    const cleanups = cleanupsRef.current;
    const nextRefs = activeRefsRef.current;
    const nextSet = new Set(nextRefs);

    for (const [ref, cleanup] of [...cleanups.entries()]) {
      if (!nextSet.has(ref)) {
        cleanups.delete(ref);
        detachSingleRef(ref, cleanup);
      }
    }
    for (const ref of nextRefs) {
      if (!cleanups.has(ref)) {
        cleanups.set(ref, attachSingleRef(ref, node));
      }
    }
  });

  const mergedRef = useCallback((node: HTMLElement | null) => {
    const cleanups = cleanupsRef.current;
    if (nodeRef.current === node) return;

    if (nodeRef.current !== null) {
      for (const [ref, cleanup] of [...cleanups.entries()]) {
        cleanups.delete(ref);
        detachSingleRef(ref, cleanup);
      }
    }

    nodeRef.current = node;

    if (node !== null) {
      for (const ref of activeRefsRef.current) {
        if (!cleanups.has(ref)) {
          cleanups.set(ref, attachSingleRef(ref, node));
        }
      }
    }
  }, []);

  return { mergedRef, setRefs };
}

function mergeIdTokens(
  ...values: Array<unknown>
): string | undefined {
  const tokens: string[] = [];
  for (const value of values) {
    if (typeof value !== "string") continue;
    for (const part of value.trim().split(/\s+/)) {
      if (part && !tokens.includes(part)) {
        tokens.push(part);
      }
    }
  }
  return tokens.length > 0 ? tokens.join(" ") : undefined;
}

function mergeClassNames(...values: Array<unknown>): string | undefined {
  const names = values.filter(
    (value): value is string => typeof value === "string" && value.trim().length > 0,
  );
  return names.length > 0 ? names.join(" ") : undefined;
}

function mergeStyleObjects(...values: Array<unknown>): CSSProperties | undefined {
  let merged: CSSProperties | undefined;
  for (const value of values) {
    if (value && typeof value === "object") {
      merged = merged
        ? { ...merged, ...(value as CSSProperties) }
        : { ...(value as CSSProperties) };
    }
  }
  return merged;
}

type EventHandler = (...args: unknown[]) => void;

function chainHandlers(
  ...handlers: Array<unknown>
): EventHandler | undefined {
  const active = handlers.filter(
    (fn): fn is EventHandler => typeof fn === "function",
  );
  if (active.length === 0) return undefined;
  if (active.length === 1) return active[0];
  return (...args: unknown[]) => {
    for (const fn of active) {
      fn(...args);
    }
  };
}

function mergeTriggerProps(
  tooltipProps: AnyProps | null,
  outerProps: AnyProps,
  childProps: AnyProps,
  shortcut?: string,
): AnyProps {
  const result: AnyProps = {};
  const keys = new Set<string>([
    ...(tooltipProps ? Object.keys(tooltipProps) : []),
    ...Object.keys(outerProps),
    ...Object.keys(childProps),
  ]);

  for (const key of keys) {
    if (
      key === "ref" ||
      key === "children" ||
      key === "className" ||
      key === "style" ||
      key === "id" ||
      key === "type" ||
      key === "aria-describedby" ||
      key === "aria-labelledby" ||
      key === "aria-keyshortcuts"
    ) {
      continue;
    }

    const fromTooltip = tooltipProps?.[key];
    const fromOuter = outerProps[key];
    const fromChild = childProps[key];

    if (/^on[A-Z]/.test(key)) {
      // On pointerdown, let Tooltip clear its hover/open state first (it does not
      // preventDefault), then run child and outer MenuTrigger/PopoverTrigger handlers.
      const handler =
        key === "onPointerDown"
          ? chainHandlers(fromTooltip, fromChild, fromOuter)
          : chainHandlers(fromChild, fromOuter, fromTooltip);
      if (handler) result[key] = handler;
      continue;
    }

    if (fromChild !== undefined) {
      result[key] = fromChild;
    } else if (fromOuter !== undefined) {
      result[key] = fromOuter;
    } else if (fromTooltip !== undefined) {
      result[key] = fromTooltip;
    }
  }

  const className = mergeClassNames(
    tooltipProps?.className,
    outerProps.className,
    childProps.className,
  );
  if (className !== undefined) result.className = className;

  const style = mergeStyleObjects(
    tooltipProps?.style,
    outerProps.style,
    childProps.style,
  );
  if (style !== undefined) result.style = style;

  // Never let Tooltip overwrite an owning MenuTrigger/PopoverTrigger or caller `id`.
  const id = outerProps.id ?? childProps.id;
  if (id !== undefined) result.id = id;

  const type = childProps.type ?? outerProps.type ?? tooltipProps?.type;
  if (type !== undefined) result.type = type;

  const describedBy = mergeIdTokens(
    childProps["aria-describedby"],
    outerProps["aria-describedby"],
    tooltipProps?.["aria-describedby"],
  );
  if (describedBy !== undefined) result["aria-describedby"] = describedBy;

  const labelledBy = mergeIdTokens(
    childProps["aria-labelledby"],
    outerProps["aria-labelledby"],
    tooltipProps?.["aria-labelledby"],
  );
  if (labelledBy !== undefined) result["aria-labelledby"] = labelledBy;

  const keyShortcuts =
    childProps["aria-keyshortcuts"] ??
    outerProps["aria-keyshortcuts"] ??
    (shortcut ? formatAriaKeyShortcuts(shortcut) : undefined);
  if (keyShortcuts !== undefined) result["aria-keyshortcuts"] = keyShortcuts;

  return result;
}

function hasRenderableLabel(label: ReactNode): boolean {
  if (label === null || label === undefined || typeof label === "boolean") {
    return false;
  }
  if (typeof label === "string") {
    return label.trim().length > 0;
  }
  return true;
}

/**
 * Studio's single `@ui-sdk/react/tooltip` boundary.
 *
 * - Keeps the trigger DOM anchor stable across `suppress` and `disabled` transitions.
 * - Avoids `@ui-sdk`'s `:disabled` dev warning by leaving the SDK trigger ref unbound
 *   and skipping `TooltipContent` when natively disabled, while still forwarding
 *   outer `MenuTrigger`/`PopoverTrigger` refs and props cleanly onto `children`.
 * - Enforces dismissal-epoch invalidation so cold-hover timers started before
 *   `window.blur`, `visibilitychange(hidden)`, `pagehide`, or `closeAllPopovers()`
 *   cannot fire after the window loses focus.
 * - Never sets `data-shortcuts-off` on `TooltipContent` so hovering to discover a
 *   shortcut still lets the user press it immediately.
 */
export const StudioTooltip = /* @__PURE__ */ forwardRef<
  HTMLElement,
  StudioTooltipProps
>(function StudioTooltip(
  {
    label,
    shortcut,
    roleType,
    suppress = false,
    disabled = false,
    side = "top",
    align = "center",
    sideOffset,
    alignOffset = 0,
    collisionPadding,
    delay,
    closeDelay,
    contentClassName,
    container,
    children,
    ...rest
  },
  forwardedRef,
) {
  const { defaults, channel } = useContext(TooltipProviderContext);
  const contextSuppressed = useContext(TooltipTriggerSuppressContext);

  const childProps = children.props as AnyProps;
  const childRef = getChildRef(children);
  const childDisabled = Boolean(childProps.disabled);
  const isDisabled = Boolean(
    disabled || childDisabled || !hasRenderableLabel(label),
  );
  const effectiveSuppress = Boolean(suppress || contextSuppressed);

  const [open, setOpen] = useState(false);
  const effectiveOpen = !isDisabled && !effectiveSuppress && open;
  const openRef = useRef(effectiveOpen);
  openRef.current = effectiveOpen;

  const armedEpochRef = useRef(-1);
  const channelRef = useRef(channel);
  channelRef.current = channel;

  useEffect(() => {
    return channel.subscribe(() => {
      if (openRef.current) {
        setOpen(false);
      }
    });
  }, [channel]);

  useEffect(() => {
    if ((isDisabled || effectiveSuppress) && open) {
      setOpen(false);
    }
  }, [isDisabled, effectiveSuppress, open]);

  // Registered FIRST in `useStableMultiRef` (and in capture phase) so our epoch
  // arming ALWAYS runs before `@ui-sdk/core`'s native `pointerenter` (including
  // 0ms warm-up hovers) and `focusin` handlers.
  const armTriggerRef = useCallback((el: HTMLElement | null) => {
    if (!el) return;
    const arm = () => {
      armedEpochRef.current = channelRef.current.getEpoch();
    };
    el.addEventListener("pointerenter", arm, true);
    el.addEventListener("focusin", arm, true);
    return () => {
      el.removeEventListener("pointerenter", arm, true);
      el.removeEventListener("focusin", arm, true);
    };
  }, []);

  const handleOpenChange = useCallback(
    (
      nextOpen: boolean,
      details: { reason: TooltipOpenChangeReason; cancel(): void },
    ) => {
      if (!nextOpen) {
        setOpen(false);
        return;
      }
      if (isDisabled || effectiveSuppress) {
        details.cancel();
        return;
      }
      if (
        (details.reason === "trigger-hover" ||
          details.reason === "trigger-focus") &&
        armedEpochRef.current !== channelRef.current.getEpoch()
      ) {
        details.cancel();
        return;
      }
      setOpen(true);
    },
    [isDisabled, effectiveSuppress],
  );

  const fallbackZIndex = useFallbackTooltipZIndex(effectiveOpen);

  const { mergedRef, setRefs } = useStableMultiRef();

  return (
    <UiSdkTooltip
      open={effectiveOpen}
      onOpenChange={handleOpenChange}
      delay={delay ?? defaults.delay}
      closeDelay={closeDelay ?? defaults.closeDelay}
      disabled={isDisabled || effectiveSuppress}
      type={roleType}
      container={container ?? undefined}
    >
      <TooltipTrigger
        render={(triggerProps) => {
          const tp = triggerProps as AnyProps;
          setRefs(
            isDisabled
              ? [forwardedRef, childRef]
              : [armTriggerRef, tp.ref as ElementRef, forwardedRef, childRef],
          );

          const merged = mergeTriggerProps(
            isDisabled ? null : tp,
            rest as AnyProps,
            childProps,
            isDisabled ? undefined : shortcut,
          );
          merged.ref = mergedRef;
          return cloneElement(children, merged);
        }}
      />
      {!isDisabled && (
        <TooltipContent
          className={
            contentClassName
              ? `studio-tooltip ${contentClassName}`
              : "studio-tooltip"
          }
          side={side}
          align={align}
          sideOffset={sideOffset ?? defaults.sideOffset}
          alignOffset={alignOffset}
          collisionPadding={collisionPadding ?? defaults.collisionPadding}
          data-tauri-drag-region="false"
          style={
            fallbackZIndex !== undefined ? { zIndex: fallbackZIndex } : undefined
          }
        >
          <span className="studio-tooltip-label">{label}</span>
          {shortcut && (
            <TooltipShortcut
              shortcut={shortcut}
              className="studio-tooltip-shortcut"
            />
          )}
        </TooltipContent>
      )}
    </UiSdkTooltip>
  );
});
