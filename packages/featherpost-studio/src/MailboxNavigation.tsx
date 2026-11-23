import { m } from "@featherpost/i18n/messages";
import { folderLabel, uiLocale, useUiLocale } from "../../maild-ui/src/i18n";
import { Choice } from "../../maild-ui/src/settings/Choice";
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
import { useContextMenu } from "./ContextMenu";
import { accountMenu, groupMenu } from "./menus";
import { useStudioActions } from "./studioActions";
import { matchingFolderNames } from "./navigation";

export interface NavigationHandle {
  focus(): void;
}
type Navigate = (action: () => void) => Promise<void>;

/** 「N 个邮箱」：按当前界面语言的复数类别选消息（中文只有 other）。 */
function mailboxCount(count: number): string {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? m.studio_mailbox_count_one({ count })
    : m.studio_mailbox_count_other({ count });
}

export function MailboxNavigation({
  mailbox,
  view,
  active,
  unread,
  navigate,
  focusRef,
  folders,
  setup,
  manageAccounts,
}: {
  mailbox: MailboxController;
  view: StudioView;
  active: boolean;
  unread: number;
  navigate: Navigate;
  focusRef: Ref<NavigationHandle>;
  folders(account: string, filter: string): ReactNode;
  setup(): void;
  manageAccounts(): void;
}) {
  useUiLocale();
  const mail = useStore(mailbox.store);
  const menu = useContextMenu(),
    { copy } = useStudioActions();
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

  const needle = filter.trim().toLocaleLowerCase(uiLocale());
  const label = (id: string) => mailbox.label(id);
  const address = (id: string) =>
    mail.profiles.find((p) => p.id === id)?.address || id;
  const accountMatches = (id: string) =>
    !needle ||
    `${label(id)} ${address(id)}`.toLocaleLowerCase(uiLocale()).includes(needle);
  const matching = (id: string) => {
    if (accountMatches(id)) return true;
    const names = matchingFolderNames(mailbox.foldersOf(id), needle);
    return mailbox.foldersOf(id).some((f) => f.present && names.has(f.name));
  };
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

  function selectAccount(id: string, unread = false) {
    const available = mailbox.foldersOf(id).filter((f) => f.present);
    const matches = !accountMatches(id)
      ? matchingFolderNames(mailbox.foldersOf(id), needle)
      : undefined;
    const firstMatch = matches?.values().next().value;
    const target =
      available.find((f) => f.name === firstMatch) ??
      available.find((f) => f.role === "inbox") ??
      available[0];
    if (!target) {
      setExpanded(id);
      return;
    }
    void navigate(() => {
      setExpanded(id);
      mailbox.selectFolder(target);
      if (unread) mailbox.setUnreadFilter(true);
      if (unified) {
        setFilter("");
        if (directory.current) directory.current.open = false;
      }
    });
  }
  const groupActions = (
    group: { id: string; name: string; accounts: string[] },
    expansion?: { expanded: boolean; toggle(): void },
  ) =>
    groupMenu({
      open: () =>
        void navigate(() =>
          mailbox.selectGroup(group.id, group.name, group.accounts),
        ),
      openUnread: () =>
        void navigate(() =>
          mailbox.selectGroup(group.id, group.name, group.accounts, true),
        ),
      manage: () => setManage(true),
      expansion,
    });
  const toggleGroup = (id: string, open: boolean) =>
    change((g) => ({
      ...g,
      collapsed: open
        ? [...g.collapsed, id]
        : g.collapsed.filter((c) => c !== id),
    }));
  const search = (
    <label className="account-filter">
      <Icon name="search" />
      <input
        ref={input}
        aria-label={m.studio_nav_filter_label()}
        placeholder={m.studio_nav_filter_placeholder()}
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />
      {filter ? (
        <button
          aria-label={m.studio_nav_filter_clear()}
          onClick={() => setFilter("")}
        >
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
        <span>{m.studio_nav_mailboxes_count({ count: ids.length })}</span>
        <button className="group-manage" onClick={() => setManage(true)}>
          {m.studio_nav_groups()}
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
            aria-label={m.studio_nav_group_label({ group: group.name })}
          >
            <button
              className="group-toggle"
              aria-expanded={open}
              onClick={() => toggleGroup(group.id, open)}
              onContextMenu={(e) =>
                menu.offer(e, () =>
                  groupActions(
                    group,
                    needle
                      ? undefined
                      : { expanded: open, toggle: () => toggleGroup(group.id, open) },
                  ),
                )
              }
            >
              <span className="chevron" aria-hidden="true">
                ⌄
              </span>
              <strong>{group.name}</strong>
              <small>{mailboxCount(group.accounts.length)}</small>
              <span
                className="group-count"
                aria-label={m.studio_nav_group_unread({ count: total })}
              >
                {total || ""}
              </span>
            </button>
            {open &&
              visible.map((id) => {
                const opened = !unified && expanded === id;
                const name = label(id);
                const email = address(id);
                const hasAddress = name.trim() !== email.trim();
                return (
                  <div
                    className={`account-tree grouped-account ${hasAddress ? "account-has-address" : ""} ${opened ? "account-expanded" : ""} ${active && selectedAccount === id ? "account-selected" : ""}`}
                    key={id}
                  >
                    <button
                      className="account-heading"
                      aria-expanded={unified ? undefined : opened}
                      title={hasAddress ? `${name}\n${email}` : name}
                      onClick={() =>
                        opened ? setExpanded(undefined) : selectAccount(id)
                      }
                      onContextMenu={(e) =>
                        menu.offer(e, () =>
                          accountMenu(address(id), {
                            open: () => selectAccount(id),
                            openUnread: () => selectAccount(id, true),
                            canOpen: mailbox
                              .foldersOf(id)
                              .some((f) => f.present),
                            groups: grouping.groups,
                            group: grouping.assignments[id] ?? OTHER_GROUP,
                            assign: (group) =>
                              change((previous) => ({
                                ...previous,
                                assignments: {
                                  ...previous.assignments,
                                  [id]: group,
                                },
                              })),
                            manageGroups: () => setManage(true),
                            copy,
                            settings: manageAccounts,
                            expansion: unified
                              ? undefined
                              : {
                                  expanded: opened,
                                  toggle: () =>
                                    setExpanded(opened ? undefined : id),
                                },
                          }),
                        )
                      }
                    >
                      <span
                        className={`chevron ${opened ? "" : "collapsed"}`}
                        aria-hidden="true"
                      >
                        ⌄
                      </span>
                      <span className="account-mark">
                        {name.slice(0, 1).toLocaleUpperCase()}
                      </span>
                      <span className="account-identity">
                        <strong>{name}</strong>
                        {hasAddress && <small>{email}</small>}
                      </span>
                      <span className="account-count">{count(id) || ""}</span>
                    </button>
                    {!unified && (
                      <div
                        className="account-folders"
                        aria-hidden={!opened}
                        inert={!opened}
                      >
                        <div>
                          {mailbox.foldersOf(id).some((f) => f.present) ? (
                            folders(id, accountMatches(id) ? "" : needle)
                          ) : (
                            <p className="nav-no-results">
                              {m.studio_nav_folders_missing_check()}
                            </p>
                          )}
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
            {open && !visible.length && (
              <p className="empty-group">{m.studio_nav_group_empty()}</p>
            )}
          </section>
        );
      })}
      {needle && !ids.some(matching) && (
        <p className="nav-no-results">{m.studio_nav_no_matches()}</p>
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
            <strong>
              {m.studio_nav_directory_title({ count: ids.length })}
            </strong>
            <span>⌄</span>
          </summary>
          <p>{m.studio_nav_directory_hint()}</p>
          <div className="directory-body">
            {search}
            {directoryGroups}
          </div>
        </details>
      )}
      <div className="section-label">
        {unified ? m.studio_nav_section_unified() : m.studio_nav_section_quick()}
      </div>
      <nav className="smart-folders" aria-label={m.studio_nav_section_quick()}>
        <button
          className={`side-link ${active && globalInbox && (!unified || !inboxUnread) ? "is-active" : ""}`}
          onClick={() => void navigate(() => mailbox.selectSmart("inbox"))}
        >
          <Icon name="inbox" />
          <span>{m.studio_nav_all_inboxes()}</span>
          <small>{unread || ""}</small>
        </button>
        <button
          className={`side-link ${active && scope.kind === "smart" && scope.name === "starred" ? "is-active" : ""}`}
          onClick={() => void navigate(() => mailbox.selectSmart("starred"))}
        >
          <Icon name="star" />
          <span>{m.studio_nav_starred()}</span>
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
            <span>{m.studio_nav_unread()}</span>
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
        <section
          className="group-filters"
          aria-label={m.studio_nav_group_filter()}
        >
          <div className="section-label account-label">
            <span>{m.studio_nav_group_filter()}</span>
            <button className="group-manage" onClick={() => setManage(true)}>
              {m.studio_nav_group_manage()}
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
                onContextMenu={(e) => menu.offer(e, () => groupActions(g))}
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
          aria-label={m.studio_nav_focused_label()}
        >
          <div className="section-label">
            {mail.selectedRow
              ? m.studio_nav_focused_selected()
              : m.studio_nav_focused_current()}
          </div>
          <div className="focused-identity">
            <strong>{label(focused)}</strong>
            <small>{address(focused)}</small>
          </div>
          {mail.selectedRow && (
            <p className="message-folder-context">
              {m.studio_nav_message_folder({
                folder: folderLabel(
                  mailbox
                    .foldersOf(focused)
                    .find((f) => f.name === mail.selectedRow!.folder),
                  mail.selectedRow.folder,
                ),
              })}
            </p>
          )}
          {folders(focused, "")}
          {!mailbox.foldersOf(focused).some((f) => f.present) && (
            <p className="nav-no-results">{m.studio_nav_folders_missing()}</p>
          )}
          {mail.selectedRow && (
            <p className="reply-context">{m.studio_nav_reply_identity()}</p>
          )}
        </section>
      )}
      {unified && !focused && ids.length > 0 && (
        <p className="reply-context">{m.studio_nav_select_hint()}</p>
      )}
      {!ids.length && (
        <div className="sidebar-empty">
          {m.studio_nav_empty()}
          <button className="text-button" onClick={setup}>
            {m.studio_account_add()}
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
  useUiLocale();
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
      setError(e instanceof Error ? e.message : m.studio_group_error_invalid());
    }
  }
  return (
    <Dialog title={m.studio_group_manager_title()} onClose={close}>
      <div className="group-manager">
        <p>{m.studio_group_manager_intro()}</p>
        <form
          className="group-name-form"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <label>
            {editing
              ? m.studio_group_rename_heading()
              : m.studio_group_new_heading()}
            <input
              aria-label={m.studio_group_name_label()}
              value={name}
              maxLength={40}
              placeholder={m.studio_group_name_placeholder()}
              onChange={(e) => {
                setName(e.target.value);
                setError("");
              }}
            />
          </label>
          <button className="primary" disabled={!name.trim()}>
            {editing ? m.studio_group_save_name() : m.studio_group_create()}
          </button>
          {editing && (
            <button
              type="button"
              onClick={() => {
                setEditing(null);
                setName("");
              }}
            >
              {m.studio_group_cancel()}
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
                {m.studio_group_rename()}
              </button>
              {g.id !== OTHER_GROUP && (
                <button
                  title={m.studio_group_delete_hint()}
                  onClick={() => {
                    change((previous) => removeGroup(previous, g.id));
                    if (editing === g.id) {
                      setEditing(null);
                      setName("");
                    }
                  }}
                >
                  {m.studio_group_delete()}
                </button>
              )}
            </div>
          ))}
        </div>
        <h3>{m.studio_group_assignment_heading()}</h3>
        {accounts.map((id) => (
          <label className="mailbox-assignment" key={id}>
            <span>
              <strong>{label(id)}</strong>
              {label(id).trim() !== address(id).trim() && (
                <small>{address(id)}</small>
              )}
            </span>
            <Choice
              aria-label={m.studio_group_assignment_label({ mailbox: label(id) })}
              value={grouping.assignments[id] ?? OTHER_GROUP}
              onValueChange={(group) => {
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
            </Choice>
          </label>
        ))}
      </div>
    </Dialog>
  );
}
