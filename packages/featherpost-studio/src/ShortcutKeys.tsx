import { Fragment } from "react";

import {
  detectShortcutPlatform,
  formatShortcutParts,
  formatShortcutText,
  type ShortcutPlatform,
} from "./shortcuts";

export interface ShortcutKeysProps {
  /** Canonical shortcut string (e.g. `"mod+shift+f"`) or alternative bindings (e.g. `["j", "arrowdown"]`). */
  shortcut: string | readonly string[];
  /** `"keycap"` renders structured per-key `<kbd>` caps; `"inline"` renders compact inline `<kbd>` text. */
  variant?: "keycap" | "inline";
  platform?: ShortcutPlatform;
  className?: string;
  /** Separator between alternative bindings (defaults to `"/"`). */
  separator?: string;
  /** Defaults to `true` because surrounding text or `aria-keyshortcuts` conveys semantics. */
  ariaHidden?: boolean;
}

/**
 * Structured keyboard shortcut renderer backed by `formatShortcutParts` / `formatShortcutText`.
 * Never splits pre-formatted strings to build keycaps.
 */
export function ShortcutKeys({
  shortcut,
  variant = "keycap",
  platform,
  className,
  separator = "/",
  ariaHidden = true,
}: ShortcutKeysProps) {
  const resolvedPlatform = platform ?? detectShortcutPlatform();
  const bindings = typeof shortcut === "string" ? [shortcut] : shortcut;

  if (variant === "inline") {
    const joined = bindings
      .map((entry) => formatShortcutText(entry, resolvedPlatform))
      .filter(Boolean)
      .join(` ${separator} `);
    if (!joined) return null;
    return (
      <kbd
        className={className}
        aria-hidden={ariaHidden ? "true" : undefined}
      >
        {joined}
      </kbd>
    );
  }

  const rootClass = className ? `shortcut-keys ${className}` : "shortcut-keys";

  return (
    <span
      className={rootClass}
      aria-hidden={ariaHidden ? "true" : undefined}
    >
      {bindings.map((entry, bindingIndex) => {
        const parts = formatShortcutParts(entry, resolvedPlatform);
        if (parts.length === 0) return null;
        return (
          <Fragment key={`${bindingIndex}:${entry}`}>
            {bindingIndex > 0 && (
              <span className="shortcut-keys-alt">{separator}</span>
            )}
            <span className="shortcut-keys-chord">
              {parts.map((part, partIndex) => (
                <Fragment key={`${partIndex}:${part}`}>
                  {partIndex > 0 && resolvedPlatform === "other" && (
                    <span className="shortcut-keys-plus">+</span>
                  )}
                  <kbd className="shortcut-keycap">{part}</kbd>
                </Fragment>
              ))}
            </span>
          </Fragment>
        );
      })}
    </span>
  );
}
