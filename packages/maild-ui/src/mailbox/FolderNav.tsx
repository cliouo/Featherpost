import { m } from "@featherpost/i18n/messages";
import { useEffect, useState } from "react";

import { folderLabel, uiLocale, useUiLocale } from "../i18n";
import type { UiAccount, UiFolder } from "../bridge/types";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import type { IconName } from "../host/types";
import type { MailboxController } from "./controller";

const ROLE_ORDER = ["inbox", "drafts", "sent", "archive", "spam", "trash"];
const SMART_LABEL = {
  inbox: m.mailbox_smart_inbox,
  unread: m.mailbox_smart_unread,
  starred: m.mailbox_smart_starred,
  all: m.mailbox_smart_all,
} as const;

const ROLE_ICON: Record<string, IconName> = {
  inbox: "inbox",
  drafts: "drafts",
  sent: "sent",
  archive: "archive",
  spam: "spam",
  trash: "trash",
};

function sortFolders(folders: UiFolder[]): UiFolder[] {
  return [...folders]
    .filter((folder) => folder.present)
    .sort((a, b) => {
      const ra = ROLE_ORDER.indexOf(a.role);
      const rb = ROLE_ORDER.indexOf(b.role);
      if (ra !== rb) return (ra === -1 ? 99 : ra) - (rb === -1 ? 99 : rb);
      return folderLabel(a).localeCompare(folderLabel(b), uiLocale());
    });
}

function accountLabel(account: UiAccount): string {
  switch (account.state) {
    case "online":
      return m.mailbox_account_state_online();
    case "connecting":
    case "reconnecting":
      return m.mailbox_account_state_connecting();
    case "disabled":
      return m.mailbox_account_state_disabled();
    case "needs_credentials":
      return m.mailbox_account_state_needs_credentials();
    case "error":
      return m.mailbox_account_state_error();
    default:
      return account.state ? m.mailbox_account_state_offline() : "";
  }
}

/** 需要用户动手的账户状态：凭据失效或连接故障。 */
export function accountIssue(account: UiAccount): string | null {
  if (account.state === "needs_credentials")
    return m.mailbox_account_issue_credentials();
  if (account.state === "error") return m.mailbox_account_issue_error();
  return null;
}

export function FolderNav({
  controller,
  onOpenMailbox,
  onRepair,
}: {
  controller: MailboxController;
  onOpenMailbox: () => void;
  /** 「修复」：直达设置中的账户管理。 */
  onRepair?: (account: string) => void;
}) {
  useUiLocale();
  const state = useStore(controller.store);
  const accounts = state.snapshot.accounts;
  const [collapsed, setCollapsed] = useState<string[]>([]);
  useEffect(() => {
    for (const account of accounts)
      void controller.ensureFolders(account.account);
  }, [accounts, controller, state.snapshot.revision]);

  if (accounts.length === 0) return null;
  return (
    <nav aria-label={m.mailbox_nav_label()} className="folder-nav">
      <div className="smart-folders">
        {(["inbox", "unread", "starred", "all"] as const).map((name) => (
          <button
            type="button"
            key={name}
            className={`nav-item${state.scope.kind === "smart" && state.scope.name === name ? " active" : ""}`}
            aria-label={SMART_LABEL[name]()}
            aria-current={
              state.scope.kind === "smart" && state.scope.name === name
                ? "page"
                : undefined
            }
            title={SMART_LABEL[name]()}
            onClick={() => {
              controller.selectSmart(name);
              onOpenMailbox();
            }}
          >
            <Icon
              name={
                name === "starred"
                  ? "star"
                  : name === "unread"
                    ? "mail"
                    : name === "all"
                      ? "folder"
                      : "inbox"
              }
            />
            <span className="nav-text">{SMART_LABEL[name]()}</span>
          </button>
        ))}
      </div>
      {state.folderError && (
        <p className="nav-empty" role="status">
          {state.folderError}
        </p>
      )}
      {accounts.map((account, index) => {
        const folders =
          account.account === state.account
            ? state.folders
            : controller.foldersOf(account.account);
        const label = accountLabel(account);
        const issue = accountIssue(account);
        return (
          <section key={account.account} className="account-group">
            <button
              type="button"
              className="nav-heading account-toggle"
              aria-expanded={!collapsed.includes(account.account)}
              aria-label={m.mailbox_account_folders_label({
                account: controller.label(account.account),
              })}
              title={
                state.profiles.find((p) => p.id === account.account)?.address ||
                account.account
              }
              onClick={() =>
                setCollapsed((v) =>
                  v.includes(account.account)
                    ? v.filter((a) => a !== account.account)
                    : [...v, account.account],
                )
              }
            >
              <span
                className={`account-dot tone-${index % 3}`}
                aria-hidden="true"
              />
              <span className="account-name">
                {controller.label(account.account)}
              </span>
              {label && (
                <span
                  className={`account-state${account.state === "online" ? "" : " warn"}`}
                >
                  {label}
                </span>
              )}
            </button>
            {issue && (
              <div className="account-issue" role="status">
                <span className="nav-text" title={account.detail || issue}>
                  {issue}
                </span>
                {onRepair && (
                  <button
                    type="button"
                    className="text-button"
                    aria-label={m.mailbox_account_repair_label({
                      account: controller.label(account.account),
                    })}
                    onClick={() => onRepair(account.account)}
                  >
                    {m.mailbox_account_repair()}
                  </button>
                )}
              </div>
            )}
            {!collapsed.includes(account.account) &&
              sortFolders(folders).map((folder) => {
                const active =
                  state.account === folder.account &&
                  state.folder?.name === folder.name;
                return (
                  <button
                    key={folder.name}
                    aria-label={`${controller.label(folder.account)} · ${folderLabel(folder)}`}
                    title={folderLabel(folder)}
                    type="button"
                    className={`nav-item${active ? " active" : ""}`}
                    aria-current={active ? "page" : undefined}
                    onClick={() => {
                      controller.selectFolder(folder);
                      onOpenMailbox();
                    }}
                  >
                    <Icon name={ROLE_ICON[folder.role] ?? "folder"} />
                    <span className="nav-text">{folderLabel(folder)}</span>
                    {folder.unseen > 0 && (
                      <span className="counter count-badge">
                        {folder.unseen}
                      </span>
                    )}
                  </button>
                );
              })}
            {folders.length === 0 && (
              <div className="nav-empty">
                {state.loadingFolders
                  ? m.mailbox_folders_loading()
                  : m.mailbox_folders_empty()}
              </div>
            )}
          </section>
        );
      })}
    </nav>
  );
}
