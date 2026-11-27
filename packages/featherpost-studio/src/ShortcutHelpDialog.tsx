import { m } from "@featherpost/i18n/messages";
import { useCallback, useMemo, type KeyboardEvent as ReactKeyboardEvent } from "react";

import { useExternal } from "./host/hooks";
import type { Host } from "./host/host";
import { useUiLocale } from "./i18n";
import { ShortcutKeys } from "./ShortcutKeys";
import {
  buildShortcutCatalog,
  detectShortcutPlatform,
  isQuestionMarkShortcut,
  type ShortcutPlatform,
} from "./shortcuts";
import { StudioDialog, type StudioDialogProps } from "./ui";

export interface ShortcutHelpDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  host: Host;
  platform?: ShortcutPlatform;
  finalFocus?: StudioDialogProps["finalFocus"];
  container?: HTMLElement | null;
}

/**
 * Unified keyboard shortcuts cheat sheet modal built on `StudioDialog`.
 *
 * - Reads live localized command labels via `buildShortcutCatalog(registry)`.
 * - Toggles closed when pressing non-repeat, non-IME `?` inside the dialog.
 * - Keeps header and footer fixed while only `.shortcut-help-body` scrolls.
 */
export function ShortcutHelpDialog({
  open,
  onOpenChange,
  host,
  platform,
  finalFocus,
  container,
}: ShortcutHelpDialogProps) {
  const locale = useUiLocale();
  const registry = useExternal(
    host.registry.subscribe,
    host.registry.getSnapshot,
  );
  const resolvedPlatform = platform ?? detectShortcutPlatform();

  const groups = useMemo(() => {
    if (!open) return [];
    return buildShortcutCatalog(registry);
  }, [open, locale, registry]);

  const handleKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (isQuestionMarkShortcut(event.nativeEvent)) {
        event.preventDefault();
        onOpenChange(false);
      }
    },
    [onOpenChange],
  );

  return (
    <StudioDialog
      open={open}
      onOpenChange={onOpenChange}
      title={m.studio_shortcuts_dialog_title()}
      description={m.studio_shortcuts_dialog_description()}
      descriptionClassName="shortcut-help-description"
      finalFocus={finalFocus}
      onKeyDown={handleKeyDown}
      contentClassName="shortcut-help-dialog"
      overlayClassName="shortcut-help-overlay"
      container={container}
    >
      <div className="shortcut-help-body">
        <div className="shortcut-help-grid">
          {groups.map((group) => {
            const headingId = `shortcut-help-group-${group.id}`;
            return (
              <section
                key={group.id}
                className="shortcut-help-group"
                aria-labelledby={headingId}
              >
                <h3 id={headingId} className="shortcut-help-group-title">
                  {group.label}
                </h3>
                <ul className="shortcut-help-list" role="list">
                  {group.items.map((item) => (
                    <li key={item.id} className="shortcut-help-row">
                      <span className="shortcut-help-copy">
                        <span className="shortcut-help-label">{item.label}</span>
                        {item.scope && (
                          <span className="shortcut-help-scope">
                            {item.scope}
                          </span>
                        )}
                      </span>
                      <ShortcutKeys
                        shortcut={item.keys}
                        variant="keycap"
                        platform={resolvedPlatform}
                      />
                    </li>
                  ))}
                </ul>
              </section>
            );
          })}
        </div>
      </div>
      <footer className="shortcut-help-footer">
        <span>{m.studio_shortcuts_footer_note()}</span>
        <span className="shortcut-help-close-hint">
          {m.studio_shortcuts_dialog_close_hint()}
        </span>
      </footer>
    </StudioDialog>
  );
}
