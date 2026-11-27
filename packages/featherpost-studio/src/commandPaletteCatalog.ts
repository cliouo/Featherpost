import { m } from "@featherpost/i18n/messages";

import type { UiFolder } from "./bridge/types";
import type { Host } from "./host/host";
import type {
  CommandContribution,
  HostContextSnapshot,
  IconName,
  RegisteredContribution,
  RegistrySnapshot,
  View,
} from "./host/types";
import { bilingualKeywords, folderLabel, uiLocale, type UiLocale } from "./i18n";
import type { StudioView } from "./layout";
import type { MailboxController } from "./mailbox/controller";
import {
  folderIcon,
  folderNavigationLabels,
  folderTree,
  selectedFolder,
  type FolderNode,
} from "./navigation";
import type { Preferences } from "./preferences";
import {
  languageChoice,
  type LanguageChoice,
  type LanguageController,
} from "./settings/language";
import { SETTINGS_VIEW } from "./settings/module";
import { MAIL } from "./views";

type NoArgMessage = (
  inputs?: Record<string, never>,
  options?: { locale?: UiLocale },
) => string;

export interface PaletteEntry {
  id: string;
  label: string;
  keywords?: readonly string[];
  subtitle?: string;
  /** Display-only shortcut string (never registered as a global hotkey). */
  shortcut?: string;
  disabled?: boolean;
  active?: boolean;
  danger?: boolean;
  icon?: IconName;
  focusHandoff?: boolean;
  onSelect: () => void;
}

export interface PaletteGroup {
  id: "selection" | "actions" | "navigation" | "preferences" | "extensions";
  label: string;
  items: readonly PaletteEntry[];
}

export interface CommandPaletteCatalog {
  /** Full searchable catalog (`items`). */
  groups: readonly PaletteGroup[];
  /** Compact catalog shown when query is empty (`emptyItems`). */
  emptyGroups: readonly PaletteGroup[];
  /** Whether leaving the current settings view/section requires the leave-guard confirmation. */
  settingsDirty: boolean;
}

export interface BuildCatalogDeps {
  host: Host;
  mailbox: MailboxController;
  view: StudioView;
  language?: LanguageController;
  focusSearch: () => void;
  openSearchFilters?: () => void;
  focusSidebarFilter: () => void;
  changeView: (next: StudioView) => void;
  openMailbox: (action: () => void) => void | Promise<unknown>;
  openSettingsSection: (sectionId: string) => void;
}

const SELECTION_COMMAND_IDS = new Set([
  "maild.compose.reply",
  "maild.compose.reply-all",
  "maild.compose.forward",
  "maild.mailbox.toggle-read",
  "maild.mailbox.toggle-star",
  "maild.mailbox.archive",
  "maild.mailbox.move",
  "maild.mailbox.delete",
  "maild.mailbox.toggle-thread",
  "maild.mailbox.clear-checked",
]);

const ACTION_COMMAND_IDS = new Set([
  "maild.compose.new",
  "maild.mailbox.sync",
  "maild.mailbox.filter-all",
  "maild.mailbox.filter-unread",
  "maild.settings.add-account",
]);

const NAVIGATION_COMMAND_IDS = new Set([
  "maild.mailbox.open-inbox",
  "maild.mailbox.open-starred",
  "maild.compose.open-drafts",
  "maild.mailbox.open-activity",
  "maild.settings.open",
]);

const FOCUS_HANDOFF_COMMAND_IDS = new Set([
  "maild.compose.new",
  "maild.compose.reply",
  "maild.compose.reply-all",
  "maild.compose.forward",
  "maild.mailbox.move",
  "maild.mailbox.delete",
  "maild.settings.add-account",
]);

const LEAVE_GUARD_COMMAND_IDS = new Set([
  "maild.compose.new",
  "maild.mailbox.open-inbox",
  "maild.mailbox.open-starred",
  "maild.mailbox.open-activity",
  "maild.mailbox.filter-all",
  "maild.mailbox.filter-unread",
  "maild.compose.open-drafts",
  "maild.settings.add-account",
]);

const SETTINGS_SECTION_KEYWORDS: Readonly<
  Record<string, { title: NoArgMessage; aliases: NoArgMessage }>
> = {
  "maild.settings.accounts": {
    title: m.studio_settings_section_accounts,
    aliases: m.studio_palette_nav_settings_accounts_keywords,
  },
  "maild.settings.appearance": {
    title: m.studio_settings_section_appearance,
    aliases: m.studio_palette_nav_settings_appearance_keywords,
  },
  "maild.mailbox.reading-privacy": {
    title: m.studio_settings_section_reading_privacy,
    aliases: m.studio_palette_nav_settings_reading_privacy_keywords,
  },
  "maild.settings.notifications": {
    title: m.studio_settings_section_notifications,
    aliases: m.studio_palette_nav_settings_notifications_keywords,
  },
  "maild.settings.addresses": {
    title: m.studio_settings_section_addresses,
    aliases: m.studio_palette_nav_settings_addresses_keywords,
  },
  "maild.settings.extensions": {
    title: m.studio_settings_section_extensions,
    aliases: m.studio_palette_nav_settings_extensions_keywords,
  },
  "maild.settings.network": {
    title: m.studio_settings_section_network,
    aliases: m.studio_palette_nav_settings_network_keywords,
  },
  "maild.settings.connection": {
    title: m.studio_settings_section_connection,
    aliases: m.studio_palette_nav_settings_connection_keywords,
  },
};

const FOLDER_ROLE_MESSAGES: Readonly<Record<string, NoArgMessage>> = {
  inbox: m.mailbox_folder_inbox,
  sent: m.mailbox_folder_sent,
  drafts: m.mailbox_folder_drafts,
  archive: m.mailbox_folder_archive,
  trash: m.mailbox_folder_trash,
  junk: m.mailbox_folder_junk,
  spam: m.mailbox_folder_junk,
  all: m.mailbox_folder_all,
};

function dedupeKeywords(tokens: Iterable<string | undefined>): readonly string[] {
  const seen = new Set<string>();
  for (const raw of tokens) {
    if (!raw) continue;
    const clean = raw.trim();
    if (clean) seen.add(clean);
  }
  return [...seen];
}

function splitAliases(message: NoArgMessage): readonly string[] {
  const out: string[] = [];
  for (const locale of ["zh-CN", "en"] as const) {
    for (const part of message({}, { locale }).split(/[,\n]/)) {
      const clean = part.trim();
      if (clean) out.push(clean);
    }
  }
  return out;
}

function truncateText(text: string, maxChars = 48): string {
  const clean = text.trim().replace(/\s+/g, " ");
  if (clean.length <= maxChars) return clean;
  return `${clean.slice(0, maxChars - 1)}…`;
}

function selectionCountLabel(count: number): string {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? m.studio_palette_selection_count_one({ count })
    : m.studio_palette_selection_count_other({ count });
}

function flattenPresentFolders(
  nodes: readonly FolderNode[],
  nested = false,
  out: Array<{ folder: UiFolder; navLabel: string }> = [],
): Array<{ folder: UiFolder; navLabel: string }> {
  const labels = folderNavigationLabels(
    nodes.map((node) => node.folder),
    nested,
  );
  for (const node of nodes) {
    if (node.folder.present) {
      out.push({
        folder: node.folder,
        navLabel: labels.get(node.folder) ?? folderLabel(node.folder),
      });
    }
    if (node.children.length > 0) {
      flattenPresentFolders(node.children, true, out);
    }
  }
  return out;
}

function sortByOrderThenId(
  a: RegisteredContribution<CommandContribution>,
  b: RegisteredContribution<CommandContribution>,
): number {
  return (a.value.order ?? 0) - (b.value.order ?? 0) || a.value.id.localeCompare(b.value.id);
}

/**
 * Derives the full and compact (empty-query) command palette catalogs.
 *
 * Reads `host.getContextSnapshot()` ONCE per derivation and passes it into every
 * `host.canExecute(command, ctx)` check so catalog derivation stays fast and pure.
 */
export function buildCommandPaletteCatalog(
  deps: BuildCatalogDeps,
): CommandPaletteCatalog {
  const {
    host,
    mailbox,
    view,
    language,
    focusSearch,
    openSearchFilters,
    focusSidebarFilter,
    changeView,
    openMailbox,
    openSettingsSection,
  } = deps;

  // Read context snapshot ONCE per catalog derivation.
  const ctx: HostContextSnapshot = host.getContextSnapshot();
  const hostState = host.state.getSnapshot();
  const mailState = mailbox.store.getSnapshot();
  const registry: RegistrySnapshot<View> = host.registry.getSnapshot();

  const builtinModuleIds = new Set(
    registry.modules.filter((mod) => mod.builtin).map((mod) => mod.moduleId),
  );
  const moduleTitles = new Map(
    registry.modules.map((mod) => [mod.moduleId, mod.title]),
  );

  const settingsDirty = Boolean(
    ctx.activeViewId === SETTINGS_VIEW && ctx.settings?.dirty,
  );
  const isMailboxActive = ctx.activeViewId === MAIL;

  const paletteCommands = registry.commands.filter((entry) =>
    entry.value.locations.includes("palette"),
  );
  const builtinPaletteCommands = paletteCommands.filter((entry) =>
    builtinModuleIds.has(entry.moduleId),
  );
  const extensionPaletteCommands = paletteCommands
    .filter((entry) => !builtinModuleIds.has(entry.moduleId))
    .sort(sortByOrderThenId);

  const byId = new Map(
    builtinPaletteCommands.map((entry) => [entry.value.id, entry.value]),
  );

  // ---------- 1. Selection group (strictly active mailbox view only) ----------
  const checkedCount = ctx.checkedMessages.length;
  const hasSelectedMessage = ctx.selectedMessage !== null;
  const targetCount =
    ctx.mailbox?.targetMessages !== undefined
      ? ctx.mailbox.targetMessages.length
      : checkedCount > 0
        ? checkedCount
        : hasSelectedMessage
          ? 1
          : 0;

  const selectionItems: PaletteEntry[] = [];
  if (isMailboxActive && (hasSelectedMessage || checkedCount > 0 || targetCount > 0)) {
    const rawSubject =
      mailState.selectedRow?.subject || hostState.selection?.subject || "";
    const subjectSubtitle = rawSubject
      ? truncateText(rawSubject)
      : m.studio_message_no_subject();
    const targetsSubtitle =
      checkedCount > 0 ? selectionCountLabel(checkedCount) : subjectSubtitle;

    const selectionEntries = builtinPaletteCommands
      .filter((entry) => SELECTION_COMMAND_IDS.has(entry.value.id))
      .sort(sortByOrderThenId);

    for (const { value: command } of selectionEntries) {
      const when = command.enabledWhen;
      if (
        (when === "message-selected" || when === "online-message-selected") &&
        !hasSelectedMessage
      ) {
        continue;
      }
      if (when === "checked-messages" && checkedCount === 0) {
        continue;
      }
      if (
        (when === "targets" || when === "online-targets") &&
        targetCount === 0
      ) {
        continue;
      }
      const isSingleMessage =
        when === "message-selected" || when === "online-message-selected";
      const enabled = host.canExecute(command, ctx);
      selectionItems.push({
        id: command.id,
        label: command.title,
        subtitle: isSingleMessage ? subjectSubtitle : targetsSubtitle,
        keywords: command.keywords,
        shortcut: command.shortcut,
        icon: command.icon,
        disabled: !enabled,
        danger: command.risk === "destructive",
        focusHandoff: FOCUS_HANDOFF_COMMAND_IDS.has(command.id),
        onSelect: () => host.execute(command.id),
      });
    }
  }

  // ---------- 2. Actions group ----------
  const toBuiltinEntry = (
    command: CommandContribution,
    overrides: Partial<PaletteEntry> = {},
  ): PaletteEntry => ({
    id: command.id,
    label: command.title,
    subtitle: command.description,
    keywords: command.keywords,
    shortcut: command.shortcut,
    icon: command.icon,
    disabled: !host.canExecute(command, ctx),
    danger: command.risk === "destructive",
    focusHandoff:
      FOCUS_HANDOFF_COMMAND_IDS.has(command.id) ||
      (settingsDirty && LEAVE_GUARD_COMMAND_IDS.has(command.id)),
    onSelect: () => host.execute(command.id),
    ...overrides,
  });

  const actionItems: PaletteEntry[] = [];
  const composeNew = byId.get("maild.compose.new");
  if (composeNew) actionItems.push(toBuiltinEntry(composeNew));

  const syncCmd = byId.get("maild.mailbox.sync");
  if (syncCmd) actionItems.push(toBuiltinEntry(syncCmd));

  actionItems.push({
    id: "studio.palette.action.search-mail",
    label: m.studio_palette_action_search_mail(),
    keywords: bilingualKeywords(
      m.studio_palette_action_search_mail,
      m.studio_palette_action_search_mail_keywords,
    ),
    shortcut: "mod+shift+f",
    icon: "search",
    focusHandoff: true,
    onSelect: focusSearch,
  });

  if (openSearchFilters) {
    actionItems.push({
      id: "studio.palette.action.search-filters",
      label: m.studio_palette_action_search_filters(),
      keywords: bilingualKeywords(
        m.studio_palette_action_search_filters,
        m.studio_palette_action_search_filters_keywords,
      ),
      icon: "sliders",
      focusHandoff: true,
      onSelect: openSearchFilters,
    });
  }

  actionItems.push({
    id: "studio.palette.action.focus-sidebar",
    label: m.studio_palette_action_focus_sidebar(),
    keywords: bilingualKeywords(
      m.studio_palette_action_focus_sidebar,
      m.studio_palette_action_focus_sidebar_keywords,
    ),
    shortcut: "mod+l",
    icon: "folder",
    focusHandoff: true,
    onSelect: focusSidebarFilter,
  });

  const filterAll = byId.get("maild.mailbox.filter-all");
  if (filterAll) actionItems.push(toBuiltinEntry(filterAll));

  const filterUnread = byId.get("maild.mailbox.filter-unread");
  if (filterUnread) actionItems.push(toBuiltinEntry(filterUnread));

  const addAccount = byId.get("maild.settings.add-account");
  if (addAccount) actionItems.push(toBuiltinEntry(addAccount));

  // Include any additional future builtin action commands.
  for (const { value: command } of builtinPaletteCommands.sort(sortByOrderThenId)) {
    if (
      ACTION_COMMAND_IDS.has(command.id) &&
      !actionItems.some((item) => item.id === command.id)
    ) {
      actionItems.push(toBuiltinEntry(command));
    }
  }

  // ---------- 3. Navigation group ----------
  const fixedNavOrder = [
    "maild.mailbox.open-inbox",
    "maild.mailbox.open-starred",
    "maild.compose.open-drafts",
    "maild.mailbox.open-activity",
    "maild.settings.open",
  ];
  const fixedNavItems: PaletteEntry[] = [];
  for (const id of fixedNavOrder) {
    const command = byId.get(id);
    if (command) {
      fixedNavItems.push(toBuiltinEntry(command));
    }
  }
  for (const { value: command } of builtinPaletteCommands.sort(sortByOrderThenId)) {
    if (
      NAVIGATION_COMMAND_IDS.has(command.id) &&
      !fixedNavItems.some((item) => item.id === command.id)
    ) {
      fixedNavItems.push(toBuiltinEntry(command));
    }
  }

  // Dynamic settings sections (searchable in full catalog, routed through leave-guard).
  const settingsKeywords = bilingualKeywords(
    m.studio_settings_title,
    m.studio_palette_nav_settings_keywords,
  );
  const settingsSectionItems: PaletteEntry[] = [...registry.settings]
    .sort(
      (a, b) =>
        (a.value.order ?? 0) - (b.value.order ?? 0) ||
        a.value.id.localeCompare(b.value.id),
    )
    .map(({ value: section }) => {
      const known = Object.prototype.hasOwnProperty.call(
        SETTINGS_SECTION_KEYWORDS,
        section.id,
      )
        ? SETTINGS_SECTION_KEYWORDS[section.id]
        : undefined;
      const sectionKeywords = dedupeKeywords([
        section.title,
        section.id,
        ...settingsKeywords,
        ...(known ? bilingualKeywords(known.title, known.aliases) : []),
      ]);
      const isCurrentSection =
        ctx.activeViewId === SETTINGS_VIEW &&
        ctx.activeSettingsSection === section.id;
      return {
        id: `studio.palette.settings-section:${section.id}`,
        label: m.studio_palette_nav_settings_section({
          section: section.title,
        }),
        keywords: sectionKeywords,
        icon: "settings" as const,
        active: isCurrentSection,
        focusHandoff: settingsDirty && !isCurrentSection,
        onSelect: () => openSettingsSection(section.id),
      };
    });

  // Dynamic accounts + cached present folders.
  const accountKeywordsBase = splitAliases(m.studio_palette_nav_account_keywords);
  const folderKeywordsBase = splitAliases(m.studio_palette_nav_folder_keywords);
  const accountItems: PaletteEntry[] = [];
  const folderItems: PaletteEntry[] = [];

  for (const accountId of ctx.accountIds) {
    const accountName = mailbox.label(accountId);
    const profile = mailState.profiles.find((p) => p.id === accountId);
    const address = profile?.address?.trim() || "";
    const allFolders = mailbox.foldersOf(accountId);
    const presentFolders = allFolders.filter((f) => f.present);
    const preferredTarget =
      mailbox.preferredInbox(accountId) ?? presentFolders[0];

    accountItems.push({
      id: `studio.palette.account:${encodeURIComponent(accountId)}`,
      label: accountName,
      subtitle:
        address && address !== accountName
          ? m.studio_palette_nav_account_subtitle({ address })
          : m.studio_palette_nav_account_subtitle_plain(),
      keywords: dedupeKeywords([
        accountId,
        accountName,
        address,
        ...accountKeywordsBase,
      ]),
      icon: "inbox",
      disabled: !preferredTarget,
      focusHandoff: settingsDirty,
      onSelect: () => {
        if (!preferredTarget) return;
        void openMailbox(() => mailbox.selectFolder(preferredTarget));
      },
    });

    for (const { folder, navLabel } of flattenPresentFolders(
      folderTree(allFolders),
    )) {
      const localized = folderLabel(folder);
      const roleMsg =
        folder.role &&
        Object.prototype.hasOwnProperty.call(FOLDER_ROLE_MESSAGES, folder.role)
          ? FOLDER_ROLE_MESSAGES[folder.role]
          : undefined;
      folderItems.push({
        id: `studio.palette.folder:${encodeURIComponent(accountId)}:${encodeURIComponent(folder.name)}`,
        label: navLabel,
        subtitle: `${accountName} / ${folder.name}`,
        keywords: dedupeKeywords([
          folder.name,
          folder.display_name,
          localized,
          navLabel,
          accountName,
          accountId,
          address,
          ...(roleMsg ? bilingualKeywords(roleMsg) : []),
          ...folderKeywordsBase,
        ]),
        icon: folderIcon(folder),
        active:
          isMailboxActive && selectedFolder(mailState.scope, accountId, folder.name),
        focusHandoff: settingsDirty,
        onSelect: () => {
          void openMailbox(() => mailbox.selectFolder(folder));
        },
      });
    }
  }

  const fullNavItems: PaletteEntry[] = [
    ...fixedNavItems,
    ...accountItems,
    ...settingsSectionItems,
    ...folderItems,
  ];
  const emptyNavItems: PaletteEntry[] = [...fixedNavItems, ...accountItems];

  // ---------- 4. Preferences group ----------
  const updatePrefs = (patch: Partial<Preferences>) => {
    const current = host.state.getSnapshot().preferences;
    host.setPreferences({ ...current, ...patch });
  };

  const preferenceItems: PaletteEntry[] = [
    {
      id: "studio.palette.pref.view.standard",
      label: m.studio_palette_pref_view_standard(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_view_standard,
        m.studio_palette_pref_view_standard_keywords,
      ),
      icon: "mail",
      active: view === "standard",
      focusHandoff: settingsDirty,
      onSelect: () => changeView("standard"),
    },
    {
      id: "studio.palette.pref.view.unified",
      label: m.studio_palette_pref_view_unified(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_view_unified,
        m.studio_palette_pref_view_unified_keywords,
      ),
      icon: "inbox",
      active: view === "unified",
      focusHandoff: settingsDirty,
      onSelect: () => changeView("unified"),
    },
    {
      id: "studio.palette.pref.theme.light",
      label: m.studio_palette_pref_theme_light(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_theme_light,
        m.studio_palette_pref_theme_light_keywords,
      ),
      icon: "settings",
      active: ctx.theme === "light",
      onSelect: () => updatePrefs({ theme: "light" }),
    },
    {
      id: "studio.palette.pref.theme.dark",
      label: m.studio_palette_pref_theme_dark(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_theme_dark,
        m.studio_palette_pref_theme_dark_keywords,
      ),
      icon: "settings",
      active: ctx.theme === "dark",
      onSelect: () => updatePrefs({ theme: "dark" }),
    },
    {
      id: "studio.palette.pref.theme.system",
      label: m.studio_palette_pref_theme_system(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_theme_system,
        m.studio_palette_pref_theme_system_keywords,
      ),
      icon: "settings",
      active: ctx.theme === "system",
      onSelect: () => updatePrefs({ theme: "system" }),
    },
    {
      id: "studio.palette.pref.density.comfortable",
      label: m.studio_palette_pref_density_comfortable(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_density_comfortable,
        m.studio_palette_pref_density_comfortable_keywords,
      ),
      icon: "settings",
      active: ctx.density === "comfortable",
      onSelect: () => updatePrefs({ density: "comfortable" }),
    },
    {
      id: "studio.palette.pref.density.compact",
      label: m.studio_palette_pref_density_compact(),
      keywords: bilingualKeywords(
        m.studio_palette_pref_density_compact,
        m.studio_palette_pref_density_compact_keywords,
      ),
      icon: "settings",
      active: ctx.density === "compact",
      onSelect: () => updatePrefs({ density: "compact" }),
    },
  ];

  if (language?.available) {
    const langState = language.store.getSnapshot();
    const currentChoice = languageChoice(langState.preference);
    const locked =
      langState.unsupported || langState.preference?.persisted === false;
    const disabled =
      langState.busy || locked || !ctx.online || currentChoice === null;

    const languageEntries: ReadonlyArray<{
      choice: LanguageChoice;
      label: NoArgMessage;
      aliases: NoArgMessage;
    }> = [
      {
        choice: "system",
        label: m.studio_palette_pref_language_system,
        aliases: m.studio_palette_pref_language_system_keywords,
      },
      {
        choice: "zh-Hans",
        label: m.studio_palette_pref_language_zh,
        aliases: m.studio_palette_pref_language_zh_keywords,
      },
      {
        choice: "en",
        label: m.studio_palette_pref_language_en,
        aliases: m.studio_palette_pref_language_en_keywords,
      },
    ];

    for (const entry of languageEntries) {
      preferenceItems.push({
        id: `studio.palette.pref.language.${entry.choice}`,
        label: entry.label(),
        keywords: bilingualKeywords(entry.label, entry.aliases),
        icon: "settings",
        active: currentChoice === entry.choice,
        disabled,
        onSelect: () => {
          if (disabled) return;
          void language.choose(entry.choice);
        },
      });
    }
  }

  // ---------- 5. Extensions group (explicit 'palette' opt-in non-builtin commands) ----------
  const extensionItems: PaletteEntry[] = extensionPaletteCommands.map(
    ({ moduleId, value: command }) => {
      const origin = moduleTitles.get(moduleId) ?? moduleId;
      return {
        id: command.id,
        label: command.title,
        subtitle: command.description || origin,
        keywords: dedupeKeywords([
          ...(command.keywords ?? []),
          origin,
          moduleId,
        ]),
        shortcut: command.shortcut,
        icon: command.icon ?? "puzzle",
        disabled: !host.canExecute(command, ctx),
        danger: command.risk === "destructive",
        onSelect: () => host.execute(command.id),
      };
    },
  );

  const buildGroups = (navItems: readonly PaletteEntry[]): PaletteGroup[] => {
    const out: PaletteGroup[] = [];
    if (selectionItems.length > 0) {
      out.push({
        id: "selection",
        label: m.studio_palette_group_selection(),
        items: selectionItems,
      });
    }
    if (actionItems.length > 0) {
      out.push({
        id: "actions",
        label: m.studio_palette_group_actions(),
        items: actionItems,
      });
    }
    if (navItems.length > 0) {
      out.push({
        id: "navigation",
        label: m.studio_palette_group_navigation(),
        items: navItems,
      });
    }
    if (preferenceItems.length > 0) {
      out.push({
        id: "preferences",
        label: m.studio_palette_group_preferences(),
        items: preferenceItems,
      });
    }
    if (extensionItems.length > 0) {
      out.push({
        id: "extensions",
        label: m.studio_palette_group_extensions(),
        items: extensionItems,
      });
    }
    return out;
  };

  return {
    groups: buildGroups(fullNavItems),
    emptyGroups: buildGroups(emptyNavItems),
    settingsDirty,
  };
}
