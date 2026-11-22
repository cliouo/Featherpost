import { folderLabel, UI_LOCALE } from "../i18n";
import { useEffect, useState } from "react";

import type { UiAccount, UiFolder } from "../bridge/types";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import type { IconName } from "../host/types";
import type { MailboxController } from "./controller";

const ROLE_ORDER = ["inbox", "drafts", "sent", "archive", "spam", "trash"];
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
      return folderLabel(a).localeCompare(folderLabel(b), UI_LOCALE);
    });
}

function accountLabel(account: UiAccount): string {
  switch (account.state) {
    case "online":
      return "在线";
    case "connecting":
    case "reconnecting":
      return "连接中";
    case "disabled":
      return "已停用";
    case "needs_credentials":
      return "需重新认证";
    case "error":
      return "连接故障";
    default:
      return account.state ? "离线" : "";
  }
}

/** 需要用户动手的账户状态：凭据失效或连接故障。 */
export function accountIssue(account: UiAccount): string | null {
  if (account.state === "needs_credentials")
    return "授权码或应用专用密码已失效，需要重新填写。";
  if (account.state === "error") return "连接失败，请检查服务器参数或网络。";
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
  const state = useStore(controller.store);
  const accounts = state.snapshot.accounts;
  const [collapsed, setCollapsed] = useState<string[]>([]);
  useEffect(() => {
    for (const account of accounts)
      void controller.ensureFolders(account.account);
  }, [accounts, controller, state.snapshot.revision]);

  if (accounts.length === 0) return null;
  return (
    <nav aria-label="账户与文件夹" className="folder-nav">
      <div className="smart-folders">
        {(["inbox", "unread", "starred", "all"] as const).map((name) => (
          <button
            type="button"
            key={name}
            className={`nav-item${state.scope.kind === "smart" && state.scope.name === name ? " active" : ""}`}
            aria-label={
              {
                inbox: "统一收件箱",
                unread: "未读邮件",
                starred: "星标邮件",
                all: "全部邮件",
              }[name]
            }
            aria-current={
              state.scope.kind === "smart" && state.scope.name === name
                ? "page"
                : undefined
            }
            title={
              {
                inbox: "统一收件箱",
                unread: "未读邮件",
                starred: "星标邮件",
                all: "全部邮件",
              }[name]
            }
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
            <span className="nav-text">
              {
                {
                  inbox: "统一收件箱",
                  unread: "未读邮件",
                  starred: "星标邮件",
                  all: "全部邮件",
                }[name]
              }
            </span>
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
              aria-label={`${controller.label(account.account)} 文件夹`}
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
                    aria-label={`修复 ${controller.label(account.account)}`}
                    onClick={() => onRepair(account.account)}
                  >
                    修复
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
                {state.loadingFolders ? "读取文件夹…" : "尚无文件夹"}
              </div>
            )}
          </section>
        );
      })}
    </nav>
  );
}
