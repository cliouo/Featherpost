import { folderLabel, UI_LOCALE } from "../../maild-ui/src/i18n";
import {
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type Ref,
} from "react";
import { useStore } from "../../maild-ui/src/host/hooks";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import { queryOf } from "../../maild-ui/src/mailbox/controller";
import { Icon } from "../../maild-ui/src/app/Icon";
import { Dialog } from "../../maild-ui/src/mailbox/Dialog";
import {
  OTHER_GROUP,
  createGrouping,
  focusedAccount,
  groupAccounts,
  namedGroup,
  readGrouping,
  removeGroup,
  saveGrouping,
  type Grouping,
  type StudioView,
} from "./layout";

export interface NavigationHandle {
  focus(): void;
}
type Navigate = (action: () => void) => Promise<void>;

export function MailboxNavigation({
  mailbox,
  view,
  active,
  unread,
  navigate,
  focusRef,
  folders,
  setup,
}: {
  mailbox: MailboxController;
  view: StudioView;
  active: boolean;
  unread: number;
  navigate: Navigate;
  focusRef: Ref<NavigationHandle>;
  folders(account: string, filter: string): ReactNode;
  setup(): void;
}) {
  const mail = useStore(mailbox.store);
  const ids = useMemo(
    () => mail.snapshot.accounts.map((a) => a.account),
    [mail.snapshot.accounts],
  );
  const [stored, setStored] = useState(readGrouping);
  const grouping = stored ?? createGrouping(ids);
  const change = (update: (g: Grouping) => Grouping) =>
    setStored((g) => update(g ?? createGrouping(ids)));
  useEffect(() => {
    if (!stored && ids.length) setStored(createGrouping(ids));
  }, [stored, ids]);
  useEffect(() => {
    if (stored) saveGrouping(stored);
  }, [stored]);
  const [filter, setFilter] = useState("");
  const [manage, setManage] = useState(false);
  const [expanded, setExpanded] = useState(() => focusedAccount(mail));
  const directory = useRef<HTMLDetailsElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const selectedAccount =
    mail.selectedRow?.account ?? queryOf(mail.scope).account;
  const firstAccount = ids[0];
  useEffect(() => {
    setExpanded((previous) => previous ?? firstAccount);
  }, [firstAccount]);
  useEffect(() => {
    if (!selectedAccount) return;
    setExpanded(selectedAccount);
    change((g) => {
      const group = g.assignments[selectedAccount] ?? OTHER_GROUP;
      return g.collapsed.includes(group)
        ? { ...g, collapsed: g.collapsed.filter((id) => id !== group) }
        : g;
    });
  }, [selectedAccount]);
  useEffect(() => {
    if (!active) {
      setManage(false);
      if (directory.current) directory.current.open = false;
    }
  }, [active]);
  useImperativeHandle(
    focusRef,
    () => ({
      focus() {
        if (directory.current) directory.current.open = true;
        input.current?.focus();
        input.current?.select();
      },
    }),
    [view],
  );

  const needle = filter.trim().toLocaleLowerCase(UI_LOCALE);
  const label = (id: string) => mailbox.label(id);
  const address = (id: string) =>
    mail.profiles.find((p) => p.id === id)?.address || id;
  const accountMatches = (id: string) =>
    !needle ||
    `${label(id)} ${address(id)}`.toLocaleLowerCase(UI_LOCALE).includes(needle);
  const matching = (id: string) =>
    accountMatches(id) ||
    mailbox
      .foldersOf(id)
      .some(
        (f) =>
          f.present &&
          `${f.name} ${f.display_name} ${folderLabel(f)}`.toLocaleLowerCase(UI_LOCALE).includes(needle),
      );
  const count = (id: string) =>
    mailbox
      .foldersOf(id)
      .filter((f) => f.role === "inbox")
      .reduce((n, f) => n + f.unseen, 0);
  const scope = mail.scope.kind === "unread" ? mail.scope.base : mail.scope;
  const activeGroup = scope.kind === "smart" ? scope.group : undefined;
  const globalInbox =
    scope.kind === "smart" && scope.name === "inbox" && !activeGroup;
  const inboxUnread = globalInbox && mail.scope.kind === "unread";
  const unified = view === "unified";
  const focused = mail.selectedRow?.account ?? queryOf(mail.scope).account;
  useEffect(() => {
    if (!activeGroup) return;
    const group = groupAccounts(ids, grouping).find(
      (g) => g.id === activeGroup.id,
    );
    if (!group) {
      mailbox.selectSmart("inbox");
      return;
    }
    if (
      group.name !== activeGroup.name ||
      JSON.stringify(group.accounts) !== JSON.stringify(activeGroup.accounts)
    )
      mailbox.selectGroup(
        group.id,
        group.name,
        group.accounts,
        mail.scope.kind === "unread",
      );
  }, [activeGroup, grouping, ids, mail.scope.kind, mailbox]);

  function selectAccount(id: string) {
    const available = mailbox.foldersOf(id).filter((f) => f.present);
    const target =
      (!accountMatches(id)
        ? available.find((f) =>
            `${f.name} ${f.display_name} ${folderLabel(f)}`.toLocaleLowerCase(UI_LOCALE).includes(needle),
          )
        : undefined) ??
      available.find((f) => f.role === "inbox") ??
      available[0];
    if (!target) {
      setExpanded(id);
      return;
    }
    void navigate(() => {
      setExpanded(id);
      mailbox.selectFolder(target);
      if (unified) {
        setFilter("");
        if (directory.current) directory.current.open = false;
      }
    });
  }
  const search = (
    <label className="account-filter">
      <Icon name="search" />
      <input
        ref={input}
        aria-label="筛选账户或文件夹"
        placeholder="查找邮箱名称或地址"
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
  );
  const directoryGroups = (
    <>
      <div className="section-label account-label">
        <span>邮箱 · {ids.length}</span>
        <button className="group-manage" onClick={() => setManage(true)}>
          分组
        </button>
      </div>
      {groupAccounts(ids, grouping).map((group) => {
        const visible = group.accounts.filter(matching);
        if (needle && !visible.length) return null;
        const open = !!needle || !grouping.collapsed.includes(group.id);
        const total = group.accounts.reduce((n, id) => n + count(id), 0);
        return (
          <section
            className="mailbox-group"
            key={group.id}
            aria-label={`${group.name}分组`}
          >
            <button
              className="group-toggle"
              aria-expanded={open}
              onClick={() =>
                change((g) => ({
                  ...g,
                  collapsed: open
                    ? [...g.collapsed, group.id]
                    : g.collapsed.filter((id) => id !== group.id),
                }))
              }
            >
              <span className="chevron" aria-hidden="true">
                ⌄
              </span>
              <strong>{group.name}</strong>
              <small>{group.accounts.length} 个邮箱</small>
              <span className="group-count" aria-label={`${total} 封未读`}>
                {total || ""}
              </span>
            </button>
            {open &&
              visible.map((id) => {
                const opened = !unified && expanded === id;
                return (
                  <div
                    className={`account-tree grouped-account ${opened ? "account-expanded" : ""} ${active && selectedAccount === id ? "account-selected" : ""}`}
                    key={id}
                  >
                    <button
                      className="account-heading"
                      aria-expanded={unified ? undefined : opened}
                      title={`${label(id)}\n${address(id)}`}
                      onClick={() =>
                        opened ? setExpanded(undefined) : selectAccount(id)
                      }
                    >
                      <span
                        className={`chevron ${opened ? "" : "collapsed"}`}
                        aria-hidden="true"
                      >
                        ⌄
                      </span>
                      <span className="account-mark">
                        {label(id).slice(0, 1).toLocaleUpperCase()}
                      </span>
                      <span className="account-identity">
                        <strong>{label(id)}</strong>
                        <small>{address(id)}</small>
                      </span>
                      <span className="account-count">{count(id) || ""}</span>
                    </button>
                    {opened &&
                      (mailbox.foldersOf(id).some((f) => f.present) ? (
                        folders(id, accountMatches(id) ? "" : needle)
                      ) : (
                        <p className="nav-no-results">
                          尚未获取文件夹，请检查账户连接
                        </p>
                      ))}
                  </div>
                );
              })}
            {open && !visible.length && (
              <p className="empty-group">暂无邮箱，在「分组」中添加</p>
            )}
          </section>
        );
      })}
      {needle && !ids.some(matching) && (
        <p className="nav-no-results">没有匹配的账户或文件夹</p>
      )}
    </>
  );

  return (
    <>
      {unified && (
        <details
          className="account-directory"
          ref={directory}
          onKeyDown={(e) => {
            if (!e.metaKey && !e.ctrlKey) e.stopPropagation();
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              directory.current!.open = false;
              directory.current?.querySelector("summary")?.focus();
            }
          }}
        >
          <summary>
            <Icon name="folder" />
            <strong>账户目录 · {ids.length}</strong>
            <span>⌄</span>
          </summary>
          <p>查找邮箱，进入单个邮箱</p>
          <div className="directory-body">
            {search}
            {directoryGroups}
          </div>
        </details>
      )}
      <div className="section-label">{unified ? "统一收件" : "快捷访问"}</div>
      <nav className="smart-folders" aria-label="快捷访问">
        <button
          className={`side-link ${active && globalInbox && (!unified || !inboxUnread) ? "is-active" : ""}`}
          onClick={() => void navigate(() => mailbox.selectSmart("inbox"))}
        >
          <Icon name="inbox" />
          <span>全部收件箱</span>
          <small>{unread || ""}</small>
        </button>
        <button
          className={`side-link ${active && scope.kind === "smart" && scope.name === "starred" ? "is-active" : ""}`}
          onClick={() => void navigate(() => mailbox.selectSmart("starred"))}
        >
          <Icon name="star" />
          <span>星标邮件</span>
        </button>
        {unified && (
          <button
            className={`side-link ${active && inboxUnread ? "is-active" : ""}`}
            onClick={() =>
              void navigate(() => {
                mailbox.selectSmart("inbox");
                mailbox.setUnreadFilter(true);
              })
            }
          >
            <Icon name="inbox" />
            <span>未读邮件</span>
            <small>{unread || ""}</small>
          </button>
        )}
      </nav>
      {!unified && (
        <>
          {search}
          {directoryGroups}
        </>
      )}
      {unified && (
        <section className="group-filters" aria-label="按账户组筛选">
          <div className="section-label account-label">
            <span>按账户组筛选</span>
            <button className="group-manage" onClick={() => setManage(true)}>
              管理
            </button>
          </div>
          <div>
            {groupAccounts(ids, grouping).map((g) => (
              <button
                key={g.id}
                aria-pressed={activeGroup?.id === g.id}
                onClick={() =>
                  void navigate(() =>
                    mailbox.selectGroup(g.id, g.name, g.accounts),
                  )
                }
              >
                <span>{g.name}</span>
                <small>{g.accounts.reduce((n, id) => n + count(id), 0)}</small>
              </button>
            ))}
          </div>
        </section>
      )}
      {unified && focused && (
        <section
          className="focused-mailbox"
          aria-label="当前邮件的账户和文件夹"
        >
          <div className="section-label">
            {mail.selectedRow ? "选中邮件的邮箱" : "当前邮箱"}
          </div>
          <div className="focused-identity">
            <strong>{label(focused)}</strong>
            <small>{address(focused)}</small>
          </div>
          {mail.selectedRow && (
            <p className="message-folder-context">
              所在文件夹 ·{" "}
              {folderLabel(mailbox.foldersOf(focused).find((f) => f.name === mail.selectedRow!.folder), mail.selectedRow.folder)}
            </p>
          )}
          {folders(focused, "")}
          {!mailbox.foldersOf(focused).some((f) => f.present) && (
            <p className="nav-no-results">尚未获取文件夹</p>
          )}
          {mail.selectedRow && (
            <p className="reply-context">回复使用此邮件的邮箱身份</p>
          )}
        </section>
      )}
      {unified && !focused && ids.length > 0 && (
        <p className="reply-context">选中一封邮件，查看所属邮箱与文件夹</p>
      )}
      {!ids.length && (
        <div className="sidebar-empty">
          还没有连接邮箱
          <button className="text-button" onClick={setup}>
            添加账户
          </button>
        </div>
      )}
      {manage && (
        <GroupManager
          grouping={grouping}
          accounts={ids}
          label={label}
          address={address}
          change={change}
          close={() => setManage(false)}
        />
      )}
    </>
  );
}

function GroupManager({
  grouping,
  accounts,
  label,
  address,
  change,
  close,
}: {
  grouping: Grouping;
  accounts: string[];
  label(id: string): string;
  address(id: string): string;
  change(update: (g: Grouping) => Grouping): void;
  close(): void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  function save() {
    const id =
      editing ??
      `group-${globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`}`;
    try {
      const next = namedGroup(grouping, id, name);
      change(() => next);
      setName("");
      setEditing(null);
      setError("");
    } catch (e) {
      setError(e instanceof Error ? e.message : "分组名称无效");
    }
  }
  return (
    <Dialog title="管理邮箱分组" onClose={close}>
      <div className="group-manager">
        <p>分组仅整理此设备的导航，不会移动邮件或修改服务器文件夹。</p>
        <form
          className="group-name-form"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <label>
            {editing ? "重命名分组" : "新建分组"}
            <input
              aria-label="分组名称"
              value={name}
              maxLength={40}
              placeholder="例如：工作、项目、家人"
              onChange={(e) => {
                setName(e.target.value);
                setError("");
              }}
            />
          </label>
          <button className="primary" disabled={!name.trim()}>
            {editing ? "保存名称" : "新建"}
          </button>
          {editing && (
            <button
              type="button"
              onClick={() => {
                setEditing(null);
                setName("");
              }}
            >
              取消
            </button>
          )}
        </form>
        {error && (
          <p role="alert" className="notice error">
            {error}
          </p>
        )}
        <div className="group-definitions">
          {grouping.groups.map((g) => (
            <div key={g.id}>
              <strong>{g.name}</strong>
              <button
                onClick={() => {
                  setEditing(g.id);
                  setName(g.name);
                  setError("");
                }}
              >
                重命名
              </button>
              {g.id !== OTHER_GROUP && (
                <button
                  title="只删除分组，邮箱移到其他；不会删除账户或邮件"
                  onClick={() => {
                    change((previous) => removeGroup(previous, g.id));
                    if (editing === g.id) {
                      setEditing(null);
                      setName("");
                    }
                  }}
                >
                  删除分组
                </button>
              )}
            </div>
          ))}
        </div>
        <h3>邮箱归属</h3>
        {accounts.map((id) => (
          <label className="mailbox-assignment" key={id}>
            <span>
              <strong>{label(id)}</strong>
              <small>{address(id)}</small>
            </span>
            <select
              aria-label={`${label(id)}的分组`}
              value={grouping.assignments[id] ?? OTHER_GROUP}
              onChange={(e) => {
                const group = e.target.value;
                change((previous) => ({
                  ...previous,
                  assignments: { ...previous.assignments, [id]: group },
                }));
              }}
            >
              {grouping.groups.map((g) => (
                <option value={g.id} key={g.id}>
                  {g.name}
                </option>
              ))}
            </select>
          </label>
        ))}
      </div>
    </Dialog>
  );
}
