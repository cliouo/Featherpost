import { m } from "@featherpost/i18n/messages";
import { useCallback, useEffect, useMemo, useRef } from "react";

import { Icon } from "./app/Icon";
import {
  buildCommandPaletteCatalog,
  type PaletteGroup,
} from "./commandPaletteCatalog";
import type { ComposeController } from "./compose/controller";
import { useExternal, useStore } from "./host/hooks";
import type { Host } from "./host/host";
import { uiLocale, useUiLocale } from "./i18n";
import type { StudioView } from "./layout";
import type { MailboxController } from "./mailbox/controller";
import { LanguageController } from "./settings/language";
import {
  StudioCommandDialog,
  type StudioCommandAnnouncement,
  type StudioCommandGroup,
} from "./ui";

export interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  host: Host;
  mailbox: MailboxController;
  composer: ComposeController;
  view: StudioView;
  language?: LanguageController;
  focusSearch: () => void;
  focusSidebarFilter: () => void;
  changeView: (next: StudioView) => void;
  openMailbox: (action: () => void) => void | Promise<unknown>;
  openSettingsSection: (sectionId: string) => void;
  runMailSearch: (query: string) => void;
  container?: HTMLElement | null;
}

function toStudioCommandGroups(
  groups: readonly PaletteGroup[],
): StudioCommandGroup[] {
  return groups.map((group) => ({
    id: group.id,
    label: group.label,
    items: group.items.map((item) => ({
      id: item.id,
      label: item.label,
      keywords: item.keywords,
      subtitle: item.subtitle,
      shortcut: item.shortcut,
      disabled: item.disabled,
      active: item.active,
      danger: item.danger,
      focusHandoff: item.focusHandoff,
      icon: item.icon ? <Icon name={item.icon} /> : undefined,
      onSelect: item.onSelect,
    })),
  }));
}

function formatAnnouncement(
  change: StudioCommandAnnouncement,
): string | null {
  if (change.type === "removed") {
    return m.studio_palette_announce_removed({ label: change.label });
  }
  if (change.loading) {
    return m.studio_palette_announce_loading();
  }
  if (change.count === 0) {
    return m.studio_palette_announce_empty();
  }
  return new Intl.PluralRules(uiLocale()).select(change.count) === "one"
    ? m.studio_palette_announce_results_one({ count: change.count })
    : m.studio_palette_announce_results_other({ count: change.count });
}

/**
 * Studio's Global `⌘K` / `Ctrl+K` Command Palette.
 *
 * - Derives groups from `buildCommandPaletteCatalog` (single context snapshot read per derivation)
 *   only when open, subscribing to host, registry, mailbox, compose, language, and locale stores.
 * - Renders the localized "Search mail: '{query}'" fallback as a fixed button AFTER the listbox
 *   so `@ui-sdk` fuzzy ranking and group separators are never disrupted.
 */
export function CommandPalette({
  open,
  onOpenChange,
  host,
  mailbox,
  composer,
  view,
  language: externalLanguage,
  focusSearch,
  focusSidebarFilter,
  changeView,
  openMailbox,
  openSettingsSection,
  runMailSearch,
  container,
}: CommandPaletteProps) {
  const locale = useUiLocale();
  const hostState = useStore(host.state);
  const mailState = useStore(mailbox.store);
  const composeState = useStore(composer.store);
  const registry = useExternal(
    host.registry.subscribe,
    host.registry.getSnapshot,
  );

  // Fallback local LanguageController when Studio is mounted without an external one (e.g. unit tests).
  const fallbackLanguageRef = useRef<LanguageController | null>(null);
  if (!externalLanguage && !fallbackLanguageRef.current) {
    fallbackLanguageRef.current = new LanguageController(host.bridge.locale);
  }
  const language = externalLanguage ?? fallbackLanguageRef.current!;

  useEffect(() => {
    const local = fallbackLanguageRef.current;
    if (externalLanguage || !local) return;
    local.start();
    return () => {
      local.dispose();
      fallbackLanguageRef.current = null;
    };
  }, [externalLanguage]);

  const langState = useStore(language.store);
  const online = hostState.snapshot.phase === "online";
  useEffect(() => {
    if (open && online && language.available) {
      void language.load();
    }
  }, [open, online, language]);

  const catalog = useMemo(() => {
    if (!open) {
      return { groups: [], emptyGroups: [], settingsDirty: false };
    }
    return buildCommandPaletteCatalog({
      host,
      mailbox,
      view,
      language,
      focusSearch,
      focusSidebarFilter,
      changeView,
      openMailbox,
      openSettingsSection,
    });
  }, [
    open,
    locale,
    host,
    hostState,
    mailbox,
    mailState,
    composeState,
    registry,
    view,
    language,
    langState,
    focusSearch,
    focusSidebarFilter,
    changeView,
    openMailbox,
    openSettingsSection,
  ]);

  const groups = useMemo(
    () => toStudioCommandGroups(catalog.groups),
    [catalog.groups],
  );
  const emptyGroups = useMemo(
    () => toStudioCommandGroups(catalog.emptyGroups),
    [catalog.emptyGroups],
  );

  const announcement = useCallback(
    (change: StudioCommandAnnouncement) => formatAnnouncement(change),
    [locale],
  );

  return (
    <StudioCommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={m.studio_palette_title()}
      inputLabel={m.studio_palette_input_label()}
      inputPlaceholder={m.studio_palette_input_placeholder()}
      groups={groups}
      emptyGroups={emptyGroups}
      emptyTitle={m.studio_palette_empty()}
      emptyHint={m.studio_palette_empty_hint()}
      announcement={announcement}
      pendingAnnouncement={m.studio_palette_pending()}
      activeLabel={m.studio_palette_active_badge()}
      searchIcon={<Icon name="search" />}
      container={container}
      onEmptyEnter={(trimmed, runWithClose) => {
        runWithClose(() => runMailSearch(trimmed), {
          focusHandoff: catalog.settingsDirty,
        });
      }}
      footer={({ query, runWithClose }) => {
        const trimmed = query.trim();
        return (
          <div className="command-palette-footer-region">
            {trimmed.length > 0 && (
              <button
                type="button"
                className="command-palette-fallback"
                onClick={() =>
                  runWithClose(() => runMailSearch(trimmed), {
                    focusHandoff: catalog.settingsDirty,
                  })
                }
              >
                <span className="command-palette-fallback-icon" aria-hidden="true">
                  <Icon name="search" />
                </span>
                <span className="command-palette-fallback-copy">
                  <strong>
                    {m.studio_palette_search_mail_fallback({ query: trimmed })}
                  </strong>
                  <small>{m.studio_palette_search_mail_fallback_hint()}</small>
                </span>
                <kbd className="command-palette-shortcut" aria-hidden="true">
                  ↵
                </kbd>
              </button>
            )}
            <div className="command-palette-footer" aria-hidden="true">
              <span>
                <kbd>↑ ↓</kbd> {m.studio_palette_footer_navigate()}
              </span>
              <span>
                <kbd>↵</kbd> {m.studio_palette_footer_run()}
              </span>
              <span>
                <kbd>Esc</kbd> {m.studio_palette_footer_close()}
              </span>
            </div>
          </div>
        );
      }}
    />
  );
}
