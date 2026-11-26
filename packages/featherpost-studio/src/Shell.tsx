import { m } from "@featherpost/i18n/messages";
import { useUiLocale } from "./i18n";
import { Choice } from "./settings/Choice";
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import type { Host } from "./host/host";
import { useExternal, useStore } from "./host/hooks";
import { Icon } from "./app/Icon";
import { applyTheme, useDarkShell } from "./app/theme";
import type { MailboxController } from "./mailbox/controller";
import type { ComposeController } from "./compose/controller";
import { DRAFTS_VIEW } from "./compose/controller";
import { MAIL, ACTIVITY } from "./views";
import { MailboxNavigation, type NavigationHandle } from "./MailboxNavigation";
import { readStudioView, saveStudioView, type StudioView } from "./layout";
import { ContextMenuHost, useContextMenu } from "./ContextMenu";
import { CommandPalette } from "./CommandPalette";
import { folderMenu, selectionMenu } from "./menus";
import type { LanguageController } from "./settings/language";
import { SyncCount } from "./SyncCount";
import { usePersistentOverlay } from "./ui";
import { isShortcutBlockedTarget } from "./Workspace";
import {
  StudioActionsContext,
  clipboardCopier,
  useStudioActions,
  type StudioActions,
} from "./studioActions";
import {
  folderTree,
  folderIcon,
  folderNavigationLabels,
  matchingFolderNames,
  selectedFolder,
  scopeContext,
  searchMail,
  type FolderNode,
} from "./navigation";

// i18n-lint-allow-cjk: 全角加号是按钮的装饰图标（aria-hidden），不是文案
const PLUS = "＋";

export function isGlobalShortcutBlocked(
  event: Pick<KeyboardEvent, "defaultPrevented" | "target">,
): boolean {
  if (event.defaultPrevented) return true;
  if (typeof document === "undefined") return false;
  if (document.querySelector('dialog[open], [aria-modal="true"]')) return true;
  const isActiveGate = (el: Element | null | undefined) =>
    Boolean(el && !el.closest("[hidden]"));
  const target = event.target as
    | { closest?(selector: string): Element | null }
    | null
    | undefined;
  if (
    target &&
    typeof target.closest === "function" &&
    isActiveGate(target.closest("[data-shortcuts-off]"))
  ) {
    return true;
  }
  const active = document.activeElement;
  if (
    active &&
    typeof active.closest === "function" &&
    isActiveGate(active.closest("[data-shortcuts-off]"))
  ) {
    return true;
  }
  for (const el of document.querySelectorAll("[data-shortcuts-off]")) {
    if (isActiveGate(el)) return true;
  }
  return false;
}

export function Studio({
  host,
  mailbox,
  composer,
  language,
  setup,
  manageAccounts,
  openSettingsSection,
}: {
  host: Host;
  mailbox: MailboxController;
  composer: ComposeController;
  language?: LanguageController;
  setup(): void;
  manageAccounts(): void;
  openSettingsSection?: (sectionId: string) => void;
}) {
  // 订阅界面语言：切换时整棵树随根组件重新渲染（不重挂载、不丢状态）。
  useUiLocale();
  const state = useStore(host.state),
    mail = useStore(mailbox.store),
    drafts = useStore(composer.store);
  const registry = useExternal(
    host.registry.subscribe,
    host.registry.getSnapshot,
  );
  const active = registry.views.find(
    (v) => v.value.id === state.activeViewId,
  )?.value;
  const View = active?.render;
  const dark = useDarkShell(state.preferences.theme);
  const online = state.snapshot.phase === "online";
  const [view, setView] = useState(readStudioView);
  const [search, setSearch] = useState("");
  const [allMail, setAllMail] = useState(false);
  const [searchScope, setSearchScope] = useState(mail.scope);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [toastText, setToastText] = useState<string | null>(null);
  useEffect(() => {
    if (state.toast) setToastText(state.toast);
  }, [state.toast]);
  const searchInput = useRef<HTMLInputElement>(null),
    navigation = useRef<NavigationHandle>(null),
    toastRef = useRef<HTMLDivElement>(null);
  usePersistentOverlay(toastRef);
  const searchContext = scopeContext(mailbox, searchScope);
  const searchText =
    mail.scope.kind === "search" ? (mail.scope.query.query ?? "") : "";
  useEffect(() => {
    setSearch(searchText);
  }, [searchText]);
  useEffect(() => {
    if (mail.scope.kind !== "search") {
      setAllMail(false);
      setSearchScope(mail.scope);
    }
  }, [mail.scope]);
  useEffect(() => applyTheme(state.preferences), [state.preferences]);
  useEffect(() => {
    document.documentElement.dataset.studioTheme = dark ? "dark" : "light";
  }, [dark]);
  useEffect(() => {
    document.title = "Featherpost Studio";
    void host.bridge.compose.ready().catch(() => {});
  }, [host]);
  const openMailbox = useCallback(
    (action: () => void): Promise<boolean> => {
      const current = mailbox.navigationGuard();
      return new Promise<boolean>((resolve) => {
        host.runAfterLeave(
          MAIL,
          () => {
            if (!current()) {
              resolve(false);
              return;
            }
            if (host.openView(MAIL)) {
              action();
              resolve(true);
              return;
            }
            resolve(false);
          },
          () => resolve(false),
        );
      });
    },
    [host, mailbox],
  );
  const changeView = useCallback(
    (next: StudioView) =>
      void openMailbox(() => {
        setView((previous) => {
          saveStudioView(next);
          if (next === "unified" && previous !== next) {
            mailbox.selectSmart("inbox");
          }
          return next;
        });
      }),
    [mailbox, openMailbox],
  );
  const focusSearch = useCallback(() => {
    searchInput.current?.focus();
    searchInput.current?.select();
  }, []);
  const focusSidebarFilter = useCallback(() => {
    navigation.current?.focus();
  }, []);
  const navigateSettingsSection = useCallback(
    (sectionId: string) => {
      if (openSettingsSection) {
        openSettingsSection(sectionId);
        return;
      }
      host.openRole("settings");
    },
    [host, openSettingsSection],
  );
  const runPaletteMailSearch = useCallback(
    (query: string) => {
      void openMailbox(() => {
        setSearch(query);
        searchMail(mailbox, query, allMail, searchScope);
      });
    },
    [allMail, mailbox, openMailbox, searchScope],
  );
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.isComposing || e.keyCode === 229 || isGlobalShortcutBlocked(e))
        return;
      if (e.metaKey || e.ctrlKey) {
        if (e.altKey) return;
        const lower = e.key.toLowerCase();
        if (lower === "k" && !e.shiftKey && !e.repeat) {
          e.preventDefault();
          setPaletteOpen(true);
          return;
        }
        if (lower === "f" && e.shiftKey && !e.repeat) {
          e.preventDefault();
          focusSearch();
          return;
        }
        if (lower === "l" && !e.shiftKey) {
          e.preventDefault();
          focusSidebarFilter();
          return;
        }
        if (e.key === "," && !e.shiftKey) {
          e.preventDefault();
          host.openRole("settings");
        }
        return;
      }
      // Single-character `/` mail search is strictly scoped to message list focus.
      if (!e.altKey && !e.repeat && e.key === "/") {
        const target = e.target as
          | { closest?(selector: string): Element | null }
          | null
          | undefined;
        if (
          target &&
          typeof target.closest === "function" &&
          target.closest(".message-column") &&
          !isShortcutBlockedTarget(target)
        ) {
          e.preventDefault();
          focusSearch();
        }
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [focusSearch, focusSidebarFilter, host]);
  const actions: StudioActions = {
    search(text, all = false) {
      void openMailbox(() => {
        if (all) setAllMail(true);
        setSearch(text);
        searchMail(mailbox, text, all || allMail, searchScope);
      });
    },
    copy: clipboardCopier((text) => host.notify(text)),
  };
  const folderActions: FolderActions = {
    open: (folder) => void openMailbox(() => mailbox.selectFolder(folder)),
    openUnread: (folder) =>
      void openMailbox(() => {
        mailbox.selectFolder(folder);
        mailbox.setUnreadFilter(true);
      }),
    searchIn: (folder) =>
      void openMailbox(() => {
        mailbox.selectFolder(folder);
        setAllMail(false);
        searchInput.current?.focus();
        searchInput.current?.select();
      }),
    sync: (folder) => void mailbox.syncNow({ account: folder.account, folder: folder.name }),
    canSync: (folder) => online && !mailbox.isSyncPending(folder.account),
    syncing: (folder) => mail.syncingScopes.get(folder.account)?.folder === folder.name,
  };
  const syncing = mailbox.isSyncing();
  const unread = state.snapshot.accounts.reduce(
    (n, a) =>
      n +
      mailbox
        .foldersOf(a.account)
        .filter((f) => f.role === "inbox")
        .reduce((m, f) => m + f.unseen, 0),
    0,
  );
  return (
    <StudioActionsContext.Provider value={actions}>
    <ContextMenuHost
      selection={(text) =>
        selectionMenu(text, {
          copy: actions.copy,
          search: (query) => actions.search(query, true),
        })
      }
    >
    <div className={`studio studio-${view}`} data-studio-view={view}>
      <a className="skip-link" href="#workspace">
        {m.studio_shell_skip_to_content()}
      </a>
      <header className="studio-topbar" data-tauri-drag-region>
        <div className="studio-brand" data-tauri-drag-region>
          <svg
            className="brand-feather"
            viewBox="0 0 24 24"
            fill="none"
            aria-hidden="true"
          >
            <path d="M20 3C11 1 4 8 5 17l-2 4 5-2C17 20 22 12 20 3ZM5 19 16 7M9 15v-5M9 15h5" />
          </svg>
          <strong data-tauri-drag-region>Featherpost</strong>
        </div>
        <form
          className="studio-search"
          role="search"
          onSubmit={(e) => {
            e.preventDefault();
            void openMailbox(() =>
              searchMail(mailbox, search, allMail, searchScope),
            );
          }}
        >
          <Icon name="search" />
          <input
            ref={searchInput}
            data-mail-search
            aria-label={m.studio_search_label()}
            placeholder={m.studio_search_label()}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <Choice
            aria-label={m.studio_search_scope_label()}
            value={
              allMail || (!searchContext.account && !searchContext.accounts)
                ? "all"
                : "current"
            }
            onValueChange={(value) => {
              const all = value === "all";
              if (search.trim()) {
                void openMailbox(() => {
                  setAllMail(all);
                  searchMail(mailbox, search, all, searchScope);
                });
              } else {
                setAllMail(all);
              }
            }}
          >
            {(searchContext.account || searchContext.accounts) && (
              <option value="current">
                {searchContext.accounts
                  ? m.studio_search_scope_group({
                      group:
                        searchContext.group ?? m.studio_scope_selected_group(),
                    })
                  : searchContext.label}
              </option>
            )}
            <option value="all">{m.studio_search_scope_all()}</option>
          </Choice>
          {searchText ? (
            <button
              type="button"
              title={m.studio_search_clear()}
              aria-label={m.studio_search_clear()}
              onClick={() => {
                void openMailbox(() => {
                  setSearch("");
                  searchMail(mailbox, "");
                });
              }}
            >
              ×
            </button>
          ) : (
            <kbd>⌘ ⇧ F</kbd>
          )}
        </form>
        <div className="topbar-actions">
          <button
            type="button"
            className="palette-trigger"
            title={m.studio_palette_open_title()}
            aria-label={m.studio_palette_open()}
            aria-keyshortcuts="Meta+K Control+K"
            onClick={() => setPaletteOpen((prev) => !prev)}
          >
            <Icon name="search" />
            <span>{m.studio_palette_open()}</span>
            <kbd aria-hidden="true">⌘ K</kbd>
          </button>
          <div className="view-switch" role="group" aria-label={m.studio_view_switch_label()}>
            <button
              aria-pressed={view === "standard"}
              onClick={() => changeView("standard")}
            >
              {m.studio_view_switch_standard()}
            </button>
            <button
              aria-pressed={view === "unified"}
              onClick={() => changeView("unified")}
            >
              {m.studio_view_switch_unified()}
            </button>
          </div>
          <button
            className={`sync-status ${syncing ? "spinning" : ""}`}
            disabled={!online || mailbox.isSyncPending()}
            title={m.studio_sync_now()}
            onClick={() => void mailbox.syncNow()}
          >
            <Icon name="refresh" />
            <span>
              {syncing
                ? m.studio_sync_status_syncing()
                : online
                  ? m.studio_sync_status_online()
                  : m.studio_sync_status_connecting()}
            </span>
          </button>
          <button
            className={`icon-button drafts-shortcut ${state.activeViewId === DRAFTS_VIEW ? "active" : ""}`}
            title={m.studio_drafts_label()}
            aria-label={
              drafts.draftTotal
                ? m.studio_drafts_label_count({ count: drafts.draftTotal })
                : m.studio_drafts_label()
            }
            onClick={() => host.openView(DRAFTS_VIEW)}
          >
            <Icon name="drafts" />
            {drafts.draftTotal > 0 && <span>{drafts.draftTotal}</span>}
          </button>
          <button
            className="compose-button"
            onClick={() => void composer.create()}
            title={m.studio_compose_title()}
          >
            <span aria-hidden="true">{PLUS}</span> {m.studio_compose()}
          </button>
        </div>
      </header>
      <aside className="studio-sidebar" aria-label={m.studio_sidebar_label()}>
        <div className="sidebar-scroll">
          <MailboxNavigation
            mailbox={mailbox}
            view={view}
            active={state.activeViewId === MAIL}
            unread={unread}
            navigate={openMailbox}
            focusRef={navigation}
            setup={setup}
            manageAccounts={manageAccounts}
            folders={(account, filter) => (
              <FolderBranches
                nodes={folderTree(mailbox.foldersOf(account))}
                depth={0}
                filter={filter}
                matchingNames={filter ? matchingFolderNames(mailbox.foldersOf(account), filter) : undefined}
                messageContext={view === "unified"}
                active={state.activeViewId === MAIL}
                mailbox={mailbox}
                actions={folderActions}
              />
            )}
          />
        </div>
        <footer className="sidebar-footer">
          <button className="add-account" onClick={setup}>
            <span aria-hidden="true">{PLUS}</span>{" "}
            {m.studio_account_add()}
          </button>
          <button
            className={`icon-button${state.activeViewId === ACTIVITY ? " active" : ""}`}
            title={m.studio_view_activity()}
            aria-label={m.studio_view_activity()}
            onClick={() => host.openView(ACTIVITY)}
          >
            <Icon name="activity" />
          </button>
          <button
            className="icon-button"
            title={m.studio_sidebar_settings_title()}
            aria-label={m.studio_sidebar_settings()}
            onClick={() => host.openRole("settings")}
          >
            <Icon name="settings" />
          </button>
        </footer>
      </aside>
      <main id="workspace" className="studio-workspace">
        {state.snapshot.identity_failure && (
          <div className="notice error" role="alert">
            {m.studio_notice_identity_failure()}
          </div>
        )}
        {(state.snapshot.compat === "app_outdated" ||
          state.snapshot.compat === "kernel_outdated") && (
          <div className="notice" role="alert">
            {m.studio_notice_version_mismatch()}
          </div>
        )}
        {View && <View active />}
      </main>
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        host={host}
        mailbox={mailbox}
        composer={composer}
        view={view}
        language={language}
        focusSearch={focusSearch}
        focusSidebarFilter={focusSidebarFilter}
        changeView={changeView}
        openMailbox={openMailbox}
        openSettingsSection={navigateSettingsSection}
        runMailSearch={runPaletteMailSearch}
      />
      <div
        ref={toastRef}
        className={`toast${state.toast ? " show" : ""}`}
        role="status"
        aria-live="polite"
        aria-hidden={!state.toast || undefined}
      >
        {state.toast ?? toastText}
      </div>
    </div>
    </ContextMenuHost>
    </StudioActionsContext.Provider>
  );
}

type Folder = FolderNode["folder"];
interface FolderActions {
  open(folder: Folder): void;
  openUnread(folder: Folder): void;
  searchIn(folder: Folder): void;
  sync(folder: Folder): void;
  canSync(folder: Folder): boolean;
  syncing(folder: Folder): boolean;
}

function FolderBranches({
  nodes,
  depth,
  filter,
  matchingNames,
  active,
  messageContext,
  mailbox,
  actions,
}: {
  nodes: FolderNode[];
  depth: number;
  filter: string;
  matchingNames?: ReadonlySet<string>;
  active: boolean;
  messageContext?: boolean;
  mailbox: MailboxController;
  actions: FolderActions;
}) {
  useUiLocale();
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const menu = useContextMenu(),
    { copy } = useStudioActions();
  const toggle = (name: string) =>
    setCollapsed((previous) => {
      const next = new Set(previous);
      next.has(name) ? next.delete(name) : next.add(name);
      return next;
    });
  const { scope, selectedRow } = useStore(mailbox.store);
  const current = (account: string, name: string) =>
    selectedFolder(
      scope,
      account,
      name,
      messageContext ? selectedRow : undefined,
    );
  const matches = (n: FolderNode): boolean =>
    !filter ||
    !!matchingNames?.has(n.folder.name) ||
    n.children.some(matches);
  const labels = folderNavigationLabels(nodes.map((n) => n.folder), depth > 0);
  return (
    <div className={`folder-branches ${depth === 0 ? "folder-directory" : ""}`}>
      {nodes.filter(matches).map(({ folder, children }) => {
        const expanded = !!filter || !collapsed.has(folder.name);
        const label = labels.get(folder)!;
        const count = folder.role === "drafts" ? folder.total : folder.unseen;
        const syncing = actions.syncing(folder);
        return (
          <div key={folder.name}>
            <div
              className={`folder-row ${depth === 0 ? "folder-root-row" : ""} ${active && current(folder.account, folder.name) ? "is-active" : ""}`}
              aria-busy={syncing || undefined}
              style={{ "--depth": depth } as CSSProperties}
              onContextMenu={(e) =>
                menu.offer(e, () =>
                  folderMenu(folder, {
                    open: () => actions.open(folder),
                    openUnread: () => actions.openUnread(folder),
                    searchIn: () => actions.searchIn(folder),
                    sync: () => actions.sync(folder),
                    canSync: actions.canSync(folder),
                    copy,
                    expansion:
                      children.length && !filter
                        ? { expanded, toggle: () => toggle(folder.name) }
                      : undefined,
                  }),
                )
              }
            >
              {children.length > 0 ? (
                <button
                  className="folder-disclosure"
                  title={
                    expanded
                      ? m.studio_folder_collapse({ folder: label })
                      : m.studio_folder_expand({ folder: label })
                  }
                  aria-label={
                    expanded
                      ? m.studio_folder_collapse({ folder: label })
                      : m.studio_folder_expand({ folder: label })
                  }
                  aria-expanded={expanded}
                  onClick={() => toggle(folder.name)}
                >
                  <span className={expanded ? "chevron" : "chevron collapsed"}>
                    ⌄
                  </span>
                </button>
              ) : (
                <span className="folder-disclosure" />
              )}
              <button
                className="folder-link"
                aria-current={
                  active && current(folder.account, folder.name)
                    ? "page"
                    : undefined
                }
                disabled={!folder.present}
                title={folder.name}
                onClick={() => actions.open(folder)}
              >
                <Icon name={folderIcon(folder)} />
                <span>{label}</span>
                <SyncCount count={count} busy={syncing} />
              </button>
            </div>
            {children.length > 0 && expanded && (
              <FolderBranches
                nodes={children}
                depth={depth + 1}
                messageContext={messageContext}
                filter={filter}
                matchingNames={matchingNames}
                active={active}
                mailbox={mailbox}
                actions={actions}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
