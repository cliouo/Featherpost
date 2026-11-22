import { folderLabel, UI_LOCALE } from "../../maild-ui/src/i18n";
import { Choice } from "../../maild-ui/src/settings/Choice";
import { useEffect, useRef, useState, type CSSProperties } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useExternal, useStore } from "../../maild-ui/src/host/hooks";
import { Icon } from "../../maild-ui/src/app/Icon";
import { applyTheme, useDarkShell } from "../../maild-ui/src/app/theme";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { DRAFTS_VIEW } from "../../maild-ui/src/compose/controller";
import { MAIL, ACTIVITY } from "./views";
import { MailboxNavigation, type NavigationHandle } from "./MailboxNavigation";
import { readStudioView, saveStudioView, type StudioView } from "./layout";
import { ContextMenuHost, useContextMenu } from "./ContextMenu";
import { folderMenu, selectionMenu } from "./menus";
import {
  StudioActionsContext,
  clipboardCopier,
  useStudioActions,
  type StudioActions,
} from "./studioActions";
import {
  folderTree,
  folderIcon,
  selectedFolder,
  scopeContext,
  searchMail,
  type FolderNode,
} from "./navigation";

export function Studio({
  host,
  mailbox,
  composer,
  setup,
  manageAccounts,
}: {
  host: Host;
  mailbox: MailboxController;
  composer: ComposeController;
  setup(): void;
  manageAccounts(): void;
}) {
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
  const searchInput = useRef<HTMLInputElement>(null),
    navigation = useRef<NavigationHandle>(null);
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
  const openMailbox = async (action: () => void) => {
    const current = mailbox.navigationGuard();
    if (!(await host.approveNavigation(MAIL)) || !current()) return;
    if (host.openView(MAIL)) action();
  };
  const changeView = (next: StudioView) =>
    void openMailbox(() => {
      setView(next);
      saveStudioView(next);
      if (next === "unified" && view !== next) mailbox.selectSmart("inbox");
    });
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        !(e.metaKey || e.ctrlKey) ||
        e.altKey ||
        document.querySelector('dialog[open], [aria-modal="true"]')
      )
        return;
      if (e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchInput.current?.focus();
        searchInput.current?.select();
      }
      if (e.key.toLowerCase() === "l") {
        e.preventDefault();
        navigation.current?.focus();
      }
      if (e.key === ",") {
        e.preventDefault();
        host.openRole("settings");
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [host]);
  const actions: StudioActions = {
    search(text, all = false) {
      if (all) setAllMail(true);
      setSearch(text);
      void openMailbox(() => searchMail(mailbox, text, all || allMail, searchScope));
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
      void openMailbox(() => mailbox.selectFolder(folder)).then(() => {
        setAllMail(false);
        searchInput.current?.focus();
        searchInput.current?.select();
      }),
    sync: () => void mailbox.syncNow(),
    canSync: online && !mail.syncing,
  };
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
        跳到邮件内容
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
            aria-label="搜索邮件"
            placeholder="搜索邮件"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <Choice
            aria-label="搜索范围"
            value={
              allMail || (!searchContext.account && !searchContext.accounts)
                ? "all"
                : "current"
            }
            onValueChange={(value) => {
              const all = value === "all";
              setAllMail(all);
              if (search.trim())
                void openMailbox(() =>
                  searchMail(mailbox, search, all, searchScope),
                );
            }}
          >
            {(searchContext.account || searchContext.accounts) && (
              <option value="current">
                {searchContext.accounts
                  ? `${searchContext.group ?? "所选分组"}的全部邮件`
                  : searchContext.label}
              </option>
            )}
            <option value="all">全部邮件</option>
          </Choice>
          {searchText ? (
            <button
              type="button"
              aria-label="清除搜索"
              onClick={() => {
                setSearch("");
                void openMailbox(() => searchMail(mailbox, ""));
              }}
            >
              ×
            </button>
          ) : (
            <kbd>⌘ K</kbd>
          )}
        </form>
        <div className="topbar-actions">
          <div className="view-switch" role="group" aria-label="邮箱视图">
            <button
              aria-pressed={view === "standard"}
              onClick={() => changeView("standard")}
            >
              标准
            </button>
            <button
              aria-pressed={view === "unified"}
              onClick={() => changeView("unified")}
            >
              统一收件
            </button>
          </div>
          <button
            className={`sync-status ${mail.syncing ? "spinning" : ""}`}
            disabled={!online || mail.syncing}
            title="立即同步"
            onClick={() => void mailbox.syncNow()}
          >
            <Icon name="refresh" />
            <span>
              {mail.syncing ? "正在同步" : online ? "已连接" : "连接中"}
            </span>
          </button>
          <button
            className={`icon-button drafts-shortcut ${state.activeViewId === DRAFTS_VIEW ? "active" : ""}`}
            title="本机草稿"
            aria-label={`本机草稿${drafts.draftTotal ? ` ${drafts.draftTotal}` : ""}`}
            onClick={() => host.openView(DRAFTS_VIEW)}
          >
            <Icon name="drafts" />
            {drafts.draftTotal > 0 && <span>{drafts.draftTotal}</span>}
          </button>
          <button
            className="compose-button"
            onClick={() => void composer.create()}
            title="写邮件 ⌘ N"
          >
            <span aria-hidden="true">＋</span> 写邮件
          </button>
        </div>
      </header>
      <aside className="studio-sidebar" aria-label="账户和文件夹">
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
            <span aria-hidden="true">＋</span> 添加账户
          </button>
          <button
            className="icon-button"
            title="同步活动"
            aria-label="同步活动"
            onClick={() => host.openView(ACTIVITY)}
          >
            <Icon name="activity" />
          </button>
          <button
            className="icon-button"
            title="设置 ⌘ ,"
            aria-label="设置"
            onClick={() => host.openRole("settings")}
          >
            <Icon name="settings" />
          </button>
        </footer>
      </aside>
      <main id="workspace" className="studio-workspace">
        {state.snapshot.identity_failure && (
          <div className="notice error" role="alert">
            新版前端尚未注册，请运行安装包里的「安装 Studio.command」后重开窗口
          </div>
        )}
        {(state.snapshot.compat === "app_outdated" ||
          state.snapshot.compat === "kernel_outdated") && (
          <div className="notice" role="alert">
            界面与后台版本不同，当前只读。请更新 Featherpost 后重试。
          </div>
        )}
        {View && <View active />}
      </main>
      {state.toast && (
        <div className="toast" role="status">
          {state.toast}
        </div>
      )}
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
  sync(): void;
  canSync: boolean;
}

function FolderBranches({
  nodes,
  depth,
  filter,
  active,
  messageContext,
  mailbox,
  actions,
}: {
  nodes: FolderNode[];
  depth: number;
  filter: string;
  active: boolean;
  messageContext?: boolean;
  mailbox: MailboxController;
  actions: FolderActions;
}) {
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
    `${n.folder.name} ${n.folder.display_name} ${folderLabel(n.folder)}`
      .toLocaleLowerCase(UI_LOCALE)
      .includes(filter) ||
    n.children.some(matches);
  return (
    <div className="folder-branches">
      {nodes.filter(matches).map(({ folder, children }) => {
        const expanded = !!filter || !collapsed.has(folder.name);
        const rawLabel =
          depth &&
          folder.display_name === folder.name &&
          /[/.]/.test(folder.name)
            ? folder.name.split(/[/.]/).pop()
            : folder.display_name || folder.name;
        const label = folderLabel({ ...folder, display_name: rawLabel ?? folder.name });
        const count = folder.role === "drafts" ? folder.total : folder.unseen;
        return (
          <div key={folder.name}>
            <div
              className={`folder-row ${active && current(folder.account, folder.name) ? "is-active" : ""}`}
              style={{ "--depth": depth } as CSSProperties}
              onContextMenu={(e) =>
                menu.offer(e, () =>
                  folderMenu(folder, {
                    open: () => actions.open(folder),
                    openUnread: () => actions.openUnread(folder),
                    searchIn: () => actions.searchIn(folder),
                    sync: actions.sync,
                    canSync: actions.canSync,
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
                  aria-label={`${expanded ? "折叠" : "展开"} ${label}`}
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
                {count > 0 && <small>{count}</small>}
              </button>
            </div>
            {children.length > 0 && expanded && (
              <FolderBranches
                nodes={children}
                depth={depth + 1}
                messageContext={messageContext}
                filter={filter}
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
