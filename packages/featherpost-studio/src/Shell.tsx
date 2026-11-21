import { useEffect, useRef, useState, type CSSProperties } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useExternal, useStore } from "../../maild-ui/src/host/hooks";
import { Icon } from "../../maild-ui/src/app/Icon";
import { applyTheme, useDarkShell } from "../../maild-ui/src/app/theme";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { DRAFTS_VIEW } from "../../maild-ui/src/compose/controller";
import { MAIL, ACTIVITY } from "./views";
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
}: {
  host: Host;
  mailbox: MailboxController;
  composer: ComposeController;
  setup(): void;
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
  const [filter, setFilter] = useState("");
  const [search, setSearch] = useState("");
  const [allMail, setAllMail] = useState(false);
  const [searchScope, setSearchScope] = useState(mail.scope);
  const searchInput = useRef<HTMLInputElement>(null),
    navInput = useRef<HTMLInputElement>(null);
  const context = scopeContext(mailbox);
  const navigationScope =
    mail.scope.kind === "unread" ? mail.scope.base : mail.scope;
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
        navInput.current?.focus();
        navInput.current?.select();
      }
      if (e.key === ",") {
        e.preventDefault();
        host.openRole("settings");
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [host]);
  const unread = state.snapshot.accounts.reduce(
    (n, a) =>
      n +
      mailbox
        .foldersOf(a.account)
        .filter((f) => f.role === "inbox")
        .reduce((m, f) => m + f.unseen, 0),
    0,
  );
  const needle = filter.trim().toLocaleLowerCase();
  return (
    <div className="studio">
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
          <select
            aria-label="搜索范围"
            value={allMail || !searchContext.account ? "all" : "current"}
            onChange={(e) => {
              const all = e.target.value === "all";
              setAllMail(all);
              if (search.trim())
                void openMailbox(() =>
                  searchMail(mailbox, search, all, searchScope),
                );
            }}
          >
            {searchContext.account && (
              <option value="current">{searchContext.label}</option>
            )}
            <option value="all">所有邮件</option>
          </select>
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
          <div className="section-label">快捷访问</div>
          <nav className="smart-folders" aria-label="快捷访问">
            <button
              className={`side-link ${navigationScope.kind === "smart" && navigationScope.name === "inbox" && state.activeViewId === MAIL ? "is-active" : ""}`}
              onClick={() =>
                void openMailbox(() => mailbox.selectSmart("inbox"))
              }
            >
              <Icon name="inbox" />
              <span>全部收件箱</span>
              <small>{unread || ""}</small>
            </button>
            <button
              className={`side-link ${navigationScope.kind === "smart" && navigationScope.name === "starred" && state.activeViewId === MAIL ? "is-active" : ""}`}
              onClick={() =>
                void openMailbox(() => mailbox.selectSmart("starred"))
              }
            >
              <Icon name="star" />
              <span>星标邮件</span>
            </button>
          </nav>
          <label className="account-filter">
            <Icon name="search" />
            <input
              ref={navInput}
              aria-label="筛选账户或文件夹"
              placeholder="筛选账户或文件夹"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
            {filter ? (
              <button aria-label="清除账户筛选" onClick={() => setFilter("")}>
                ×
              </button>
            ) : (
              <kbd>⌘ L</kbd>
            )}
          </label>
          <div className="section-label account-label">
            <span>账户</span>
            <span>{state.snapshot.accounts.length}</span>
          </div>
          {state.snapshot.accounts.map((account) => {
            const profile = mail.profiles.find((p) => p.id === account.account);
            const label = mailbox.label(account.account),
              address = profile?.address || account.account;
            const accountMatches =
              !needle ||
              `${label} ${address}`.toLocaleLowerCase().includes(needle);
            const nodes = folderTree(mailbox.foldersOf(account.account));
            const matches = (n: FolderNode): boolean =>
              `${n.folder.name} ${n.folder.display_name}`
                .toLocaleLowerCase()
                .includes(needle) || n.children.some(matches);
            if (!accountMatches && !nodes.some(matches)) return null;
            const total = mailbox
              .foldersOf(account.account)
              .filter((f) => f.role === "inbox")
              .reduce((n, f) => n + f.unseen, 0);
            const selected =
              context.account === account.account &&
              state.activeViewId === MAIL;
            return (
              <details
                className={`account-tree ${selected ? "account-selected" : ""}`}
                key={`${account.account}:${needle}`}
                open
              >
                <summary title={account.detail || account.state}>
                  <span className="chevron" aria-hidden="true">
                    ⌄
                  </span>
                  <span className="account-mark">
                    {label.slice(0, 1).toLocaleUpperCase()}
                  </span>
                  <span className="account-identity">
                    <strong>{label}</strong>
                    <small>{address}</small>
                  </span>
                  <span className="account-count">{total || ""}</span>
                </summary>
                <FolderBranches
                  nodes={nodes}
                  depth={0}
                  filter={accountMatches ? "" : needle}
                  active={state.activeViewId === MAIL}
                  mailbox={mailbox}
                  open={(folder) =>
                    void openMailbox(() => mailbox.selectFolder(folder))
                  }
                />
              </details>
            );
          })}
          {!state.snapshot.accounts.length && (
            <div className="sidebar-empty">
              还没有连接邮箱
              <button className="text-button" onClick={setup}>
                添加账户
              </button>
            </div>
          )}
          {needle &&
            !state.snapshot.accounts.some(
              (a) =>
                `${mailbox.label(a.account)} ${mail.profiles.find((p) => p.id === a.account)?.address || a.account}`
                  .toLocaleLowerCase()
                  .includes(needle) ||
                mailbox
                  .foldersOf(a.account)
                  .some((f) =>
                    `${f.name} ${f.display_name}`
                      .toLocaleLowerCase()
                      .includes(needle),
                  ),
            ) && <p className="nav-no-results">没有匹配的账户或文件夹</p>}
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
  );
}

function FolderBranches({
  nodes,
  depth,
  filter,
  active,
  mailbox,
  open,
}: {
  nodes: FolderNode[];
  depth: number;
  filter: string;
  active: boolean;
  mailbox: MailboxController;
  open(folder: FolderNode["folder"]): void;
}) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const { scope } = useStore(mailbox.store);
  const matches = (n: FolderNode): boolean =>
    !filter ||
    `${n.folder.name} ${n.folder.display_name}`
      .toLocaleLowerCase()
      .includes(filter) ||
    n.children.some(matches);
  return (
    <div className="folder-branches">
      {nodes.filter(matches).map(({ folder, children }) => {
        const expanded = !!filter || !collapsed.has(folder.name);
        const label =
          depth &&
          folder.display_name === folder.name &&
          /[/.]/.test(folder.name)
            ? folder.name.split(/[/.]/).pop()
            : folder.display_name || folder.name;
        const count = folder.role === "drafts" ? folder.total : folder.unseen;
        return (
          <div key={folder.name}>
            <div
              className={`folder-row ${active && selectedFolder(scope, folder.account, folder.name) ? "is-active" : ""}`}
              style={{ "--depth": depth } as CSSProperties}
            >
              {children.length > 0 ? (
                <button
                  className="folder-disclosure"
                  aria-label={`${expanded ? "折叠" : "展开"} ${label}`}
                  aria-expanded={expanded}
                  onClick={() =>
                    setCollapsed((previous) => {
                      const next = new Set(previous);
                      next.has(folder.name)
                        ? next.delete(folder.name)
                        : next.add(folder.name);
                      return next;
                    })
                  }
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
                  active && selectedFolder(scope, folder.account, folder.name)
                    ? "page"
                    : undefined
                }
                disabled={!folder.present}
                title={folder.name}
                onClick={() => open(folder)}
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
                filter={filter}
                active={active}
                mailbox={mailbox}
                open={open}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
