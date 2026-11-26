/**
 * Shared jsdom polyfills for Vitest tests.
 * Safely no-ops in pure Node test files where `Element` is undefined.
 */
if (typeof Element !== "undefined") {
  const proto = Element.prototype as Element & {
    getAnimations?: (options?: GetAnimationsOptions) => Animation[];
  };
  proto.getAnimations ??= () => [];
}
