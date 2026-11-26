/**
 * Shared jsdom polyfills for Vitest tests.
 * Safely no-ops in pure Node test files where `Element` / `HTMLElement` are undefined.
 */
if (typeof Element !== "undefined") {
  const proto = Element.prototype as Element & {
    getAnimations?: (options?: GetAnimationsOptions) => Animation[];
    checkVisibility?: (options?: { visibilityProperty?: boolean }) => boolean;
  };
  proto.getAnimations ??= () => [];
  proto.checkVisibility ??= function checkVisibility(options) {
    if (!this.isConnected) return false;
    for (let node: Element | null = this; node; node = node.parentElement) {
      if (node.hasAttribute("hidden")) return false;
      const style = getComputedStyle(node);
      if (style.display === "none") return false;
      if (options?.visibilityProperty && style.visibility === "hidden") return false;
    }
    return true;
  };
}

if (typeof HTMLElement !== "undefined") {
  // jsdom 31 does not implement HTMLElement.prototype.inert attribute reflection
  // (shipped in Safari 15.5+, Chrome 102+, Firefox 112+). Reflect to `[inert]`
  // so modal inertness checks match real browsers.
  if (!("inert" in HTMLElement.prototype)) {
    Object.defineProperty(HTMLElement.prototype, "inert", {
      configurable: true,
      enumerable: true,
      get(this: HTMLElement) {
        return this.hasAttribute("inert");
      },
      set(this: HTMLElement, value: boolean) {
        if (value) {
          this.setAttribute("inert", "");
        } else {
          this.removeAttribute("inert");
        }
      },
    });
  }

  // jsdom 31 ships `[popover] { display: none }` in its UA stylesheet without
  // implementing `showPopover()` / `:popover-open`, causing `focus()` inside
  // `[popover]` elements to silently fail even on the `!supportsTopLayer()`
  // fallback path (Safari 16 does not hide `[popover]`). Restore visibility
  // without stubbing `showPopover()`, preserving `!supportsTopLayer()` testing.
  if (
    typeof document !== "undefined" &&
    document.head &&
    !("showPopover" in HTMLElement.prototype) &&
    !document.getElementById("jsdom-popover-fallback")
  ) {
    const style = document.createElement("style");
    style.id = "jsdom-popover-fallback";
    style.textContent = "[popover]:not([hidden]) { display: block; }";
    document.head.appendChild(style);
  }
}
