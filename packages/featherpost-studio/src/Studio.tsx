import { useEffect } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useExternal, useStore } from "../../maild-ui/src/host/hooks";
import { Icon } from "../../maild-ui/src/app/Icon";
import { applyTheme } from "../../maild-ui/src/app/theme";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { DRAFTS_VIEW } from "../../maild-ui/src/compose/controller";
import type { IconName } from "../../maild-ui/src/host/types";
import type { SmartFolder } from "../../maild-ui/src/mailbox/types";
import { MAIL, ACTIVITY } from "./mailbox";

const destinations: { id: SmartFolder; label: string; icon: IconName }[] = [
  { id: "inbox", label: "收件箱", icon: "inbox" },
  { id: "unread", label: "未读邮件", icon: "mail" },
  { id: "starred", label: "星标邮件", icon: "star" },
  { id: "all", label: "全部邮件", icon: "archive" },
];

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
  const state = useStore(host.state);
  const mail = useStore(mailbox.store);
  const drafts = useStore(composer.store);
  const registry = useExternal(
    host.registry.subscribe,
    host.registry.getSnapshot,
  );
  const active = registry.views.find(
    (v) => v.value.id === state.activeViewId,
  )?.value;
  const View = active?.render;
  const online = state.snapshot.phase === "online";
  const unread = mail.folders
    .filter((f) => f.role === "inbox")
    .reduce((n, f) => n + f.unseen, 0);
  useEffect(() => applyTheme(state.preferences), [state.preferences]);
  useEffect(() => {
    document.title = "Featherpost Studio";
    void host.bridge.compose.ready().catch(() => {});
  }, [host]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        !(e.metaKey || e.ctrlKey) ||
        document.querySelector('dialog[open], [aria-modal="true"]')
      )
        return;
      if (e.key === ",") {
        e.preventDefault();
        host.openRole("settings");
      }
      if (e.key.toLowerCase() === "n") {
        e.preventDefault();
        void composer.create();
      }
      if (e.key.toLowerCase() === "k") {
        e.preventDefault();
        host.openView(MAIL);
        requestAnimationFrame(() =>
          document
            .querySelector<HTMLInputElement>("[data-mail-search]")
            ?.focus(),
        );
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [host, composer]);
  const nav = (id: SmartFolder) => {
    host.openView(MAIL);
    mailbox.selectSmart(id);
  };
  return (
    <div className="studio">
      <a className="skip-link" href="#workspace">
        跳到邮件
      </a>
      <aside className="studio-sidebar" aria-label="主导航">
        <div className="window-space" data-tauri-drag-region="deep" />
        <div className="studio-brand" data-tauri-drag-region="deep">
          <span className="brand-mark">
            <Icon name="sent" />
          </span>
          <strong>
            Featherpost<span>STUDIO</span>
          </strong>
        </div>
        <button
          className="new-mail"
          disabled={!online}
          onClick={() => void composer.create()}
        >
          <Icon name="drafts" />
          <span>写邮件</span>
          <kbd>⌘ N</kbd>
        </button>
        <div className="sidebar-scroll">
          <p className="section-label">邮件</p>
          <nav>
            {destinations.map((d) => (
              <button
                key={d.id}
                className={`side-link ${state.activeViewId === MAIL && mail.scope.kind === "smart" && mail.scope.name === d.id ? "is-active" : ""}`}
                onClick={() => nav(d.id)}
                aria-current={
                  state.activeViewId === MAIL &&
                  mail.scope.kind === "smart" &&
                  mail.scope.name === d.id
                    ? "page"
                    : undefined
                }
              >
                <Icon name={d.icon} />
                <span>{d.label}</span>
                {d.id === "inbox" && unread > 0 && <small>{unread}</small>}
              </button>
            ))}
            <button
              className={`side-link ${state.activeViewId === DRAFTS_VIEW ? "is-active" : ""}`}
              onClick={() => host.openView(DRAFTS_VIEW)}
            >
              <Icon name="drafts" />
              <span>草稿箱</span>
              {drafts.draftTotal > 0 && <small>{drafts.draftTotal}</small>}
            </button>
          </nav>
          <div className="section-label section-line">
            <span>邮箱账户</span>
            <button aria-label="添加邮箱账户" onClick={setup}>
              +
            </button>
          </div>
          {state.snapshot.accounts.map((account, index) => (
            <details className="account-tree" key={account.account} open>
              <summary>
                <span className={`account-dot color-${index % 4}`} />
                <span>{mailbox.label(account.account)}</span>
                <span
                  className={`connection-dot ${account.state === "online" ? "connected" : ""}`}
                  title={account.state}
                />
              </summary>
              {mailbox
                .foldersOf(account.account)
                .filter((f) => f.present)
                .map((folder) => (
                  <button
                    key={folder.name}
                    className={`side-link folder-link ${mail.scope.kind === "folder" && mail.scope.account === folder.account && mail.scope.folder === folder.name && state.activeViewId === MAIL ? "is-active" : ""}`}
                    onClick={() => {
                      host.openView(MAIL);
                      mailbox.selectFolder(folder);
                    }}
                  >
                    <Icon
                      name={
                        folder.role === "sent"
                          ? "sent"
                          : folder.role === "trash"
                            ? "trash"
                            : "folder"
                      }
                    />
                    <span>{folder.display_name || folder.name}</span>
                    {folder.unseen > 0 && <small>{folder.unseen}</small>}
                  </button>
                ))}
            </details>
          ))}
          {!state.snapshot.accounts.length && (
            <button className="add-account" onClick={setup}>
              <Icon name="mail" />
              <span>连接你的第一个邮箱</span>
            </button>
          )}
        </div>
        <div className="sidebar-footer">
          <button
            className={`side-link ${state.activeViewId === ACTIVITY ? "is-active" : ""}`}
            onClick={() => host.openView(ACTIVITY)}
          >
            <Icon name="activity" />
            <span>同步活动</span>
            <span className={`connection-dot ${online ? "connected" : ""}`} />
          </button>
          <button
            className={`side-link ${active?.role === "settings" ? "is-active" : ""}`}
            onClick={() => host.openRole("settings")}
          >
            <Icon name="settings" />
            <span>设置</span>
            <kbd>⌘ ,</kbd>
          </button>
          <div className="local-status">
            <span className={`connection-dot ${online ? "connected" : ""}`} />
            {online ? "邮件保存在本机" : "正在连接本机后台…"}
          </div>
        </div>
      </aside>
      <main id="workspace" className="studio-workspace">
        {state.snapshot.identity_failure && (
          <div className="notice error" role="alert">
            新版前端尚未注册，请先运行安装包里的「安装
            Studio.command」，再重开窗口
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
