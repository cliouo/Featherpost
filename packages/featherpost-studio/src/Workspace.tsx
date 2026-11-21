import { Fragment, useEffect } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useStore } from "../../maild-ui/src/host/hooks";
import { Icon } from "../../maild-ui/src/app/Icon";
import {
  messageKey,
  type MailboxController,
} from "../../maild-ui/src/mailbox/controller";
import { displayName, shortDate } from "../../maild-ui/src/mailbox/format";
import { Actions } from "../../maild-ui/src/mailbox/Actions";
import type { DocumentController } from "../../maild-ui/src/mailbox/document";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { ReadingPane } from "./ReadingPane";
import { scopeContext, filterUnread } from "./navigation";
import { ACTIVITY } from "./views";

export function Workspace({
  host,
  controller,
  documents,
  composer,
  setup,
}: {
  host: Host;
  controller: MailboxController;
  documents: DocumentController;
  composer: ComposeController;
  setup(): void;
}) {
  const s = useStore(controller.store);
  const context = scopeContext(controller);
  const searching = s.scope.kind === "search" && !!s.scope.query.query?.trim();
  const unread =
    s.scope.kind === "unread" ||
    (s.scope.kind === "smart" && s.scope.name === "unread");
  const folderUnread = context.folder?.unseen;
  const title = searching ? "搜索结果" : context.name;
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        e.metaKey ||
        e.ctrlKey ||
        e.altKey ||
        document.querySelector('dialog[open], [aria-modal="true"]') ||
        (e.target instanceof HTMLElement &&
          e.target.closest(
            'input,textarea,select,summary,[contenteditable="true"],[data-shortcuts-off]',
          ))
      )
        return;
      if (e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault();
        controller.step(1);
      }
      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        controller.step(-1);
      }
      if (e.key === "Escape") controller.clearSelection();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [controller]);
  const day = (at: number) =>
    new Date(at * 1000).toDateString() === new Date().toDateString()
      ? "今天"
      : "更早";
  return (
    <section className="mail-view">
      {s.syncError && (
        <div className="notice error" role="alert">
          {s.syncError}
        </div>
      )}
      <Actions
        controller={controller}
        openActivity={() => host.openView(ACTIVITY)}
      />
      <div
        className={`mail-columns ${s.selected ? "has-selection" : ""} ${s.snapshot.phase === "online" && !s.snapshot.accounts.length ? "is-onboarding" : ""}`}
      >
        <section className="message-column" aria-label="邮件列表">
          <header className="list-heading">
            <div className="breadcrumb">
              {context.account ? controller.label(context.account) : "全部账户"}
              <span>›</span>
              {context.name}
            </div>
            <h1>
              {title}
              <span className="mail-count">{s.total}</span>
            </h1>
            <div className="list-controls">
              {searching ? (
                <button
                  className="search-return text-button"
                  onClick={() => controller.exitSearch()}
                >
                  返回原邮件列表
                </button>
              ) : (
                <div className="filter-tabs" aria-label="邮件筛选">
                  <button
                    className={!unread ? "selected" : ""}
                    aria-pressed={!unread}
                    onClick={() => filterUnread(controller, false)}
                  >
                    全部
                  </button>
                  <button
                    className={unread ? "selected" : ""}
                    aria-pressed={unread}
                    onClick={() => filterUnread(controller, true)}
                  >
                    未读
                    {folderUnread !== undefined && <span>{folderUnread}</span>}
                  </button>
                </div>
              )}
              <span className="list-order" title="按邮件时间从新到旧排列">
                最新优先
              </span>
            </div>
          </header>
          <div className="message-scroll" aria-busy={s.loadingRows}>
            {s.rowsError && (
              <div className="notice error" role="alert">
                {s.rowsError}
                <button
                  className="text-button"
                  onClick={() => void controller.refresh()}
                >
                  重试
                </button>
              </div>
            )}
            {s.folderError && (
              <div className="notice error" role="alert">
                {s.folderError}
              </div>
            )}
            {s.loadingRows && !s.rows.length && (
              <div className="loading-mail" role="status">
                正在读取邮件…
              </div>
            )}
            {!s.loadingRows && !s.rows.length && (
              <div className="empty-list">
                <Icon name="inbox" />
                <h2>
                  {searching
                    ? "没有找到邮件"
                    : unread
                      ? "没有未读邮件"
                      : "这里暂时没有邮件"}
                </h2>
                <p>
                  {searching
                    ? "试试其他关键词或搜索范围"
                    : "同步后的邮件会显示在这里"}
                </p>
                {searching && controller.canSearchServer && (
                  <button
                    className="text-button"
                    onClick={() => void controller.searchServer()}
                  >
                    搜索服务器历史邮件
                  </button>
                )}
              </div>
            )}
            {s.rows.map((row, index) => {
              const selected =
                !!s.selected && messageKey(s.selected) === messageKey(row);
              const checked = s.checked.some(
                (r) => messageKey(r) === messageKey(row),
              );
              return (
                <Fragment key={messageKey(row)}>
                  {(index === 0 ||
                    day(row.sort_at) !== day(s.rows[index - 1]!.sort_at)) && (
                    <div className="date-group">{day(row.sort_at)}</div>
                  )}
                  <article
                    className={`message-row ${selected ? "selected" : ""} ${!row.seen ? "unread" : ""} ${checked ? "checked" : ""}`}
                  >
                    <span className="row-marker" aria-hidden="true" />
                    <input
                      className="row-check"
                      type="checkbox"
                      aria-label={`选择 ${row.subject || "无主题邮件"}`}
                      checked={checked}
                      onChange={() => controller.toggle(row)}
                    />
                    <button
                      className="message-open"
                      aria-current={selected ? "true" : undefined}
                      onClick={() => controller.selectMessage(row)}
                    >
                      <span className="row-top">
                        <strong>{displayName(row.sender) || row.sender}</strong>
                        {!context.account && (
                          <span
                            className="row-account"
                            title={controller.label(row.account)}
                          >
                            {controller.label(row.account)}
                          </span>
                        )}
                        <time>{shortDate(row.sort_at)}</time>
                      </span>
                      <span className="row-bottom">
                        <span className="row-subject">
                          {row.subject || "（无主题）"}
                        </span>
                        <span className="row-snippet">{row.snippet}</span>
                        <span className="row-extras">
                          {row.has_attachments && <Icon name="paperclip" />}
                          {row.flagged && <Icon name="star" />}
                          {row.answered && <Icon name="reply" />}
                        </span>
                      </span>
                    </button>
                  </article>
                </Fragment>
              );
            })}
            {controller.canLoadMore && (
              <button
                className="load-more"
                disabled={s.loadingRows}
                onClick={() => void controller.loadMore()}
              >
                加载更多邮件
              </button>
            )}
            {searching && s.rows.length > 0 && controller.canSearchServer && (
              <button
                className="load-more"
                disabled={s.remote.status === "loading"}
                onClick={() => void controller.searchServer()}
              >
                搜索服务器历史邮件
              </button>
            )}
            {s.remote.error && (
              <div className="notice error" role="alert">
                {s.remote.error}
              </div>
            )}
            {s.remote.rows.length > 0 && (
              <>
                <p className="date-group">服务器历史</p>
                {s.remote.rows.map((row) => (
                  <button
                    className="remote-row"
                    key={messageKey(row)}
                    onClick={() => controller.openRemote(row)}
                  >
                    <strong>{row.subject || "（无主题）"}</strong>
                    <span>
                      {row.sender} · {controller.label(row.account)}
                    </span>
                  </button>
                ))}
              </>
            )}
          </div>
          <footer className="list-footer">
            <span>
              {s.loadingRows ? "正在更新…" : `${s.total} 封邮件`}
              {unread ? " · 未读" : ""}
            </span>
            <span>↑ ↓ 切换</span>
          </footer>
        </section>
        {s.snapshot.phase === "online" && !s.snapshot.accounts.length ? (
          <section className="reader-column welcome-studio">
            <Icon name="inbox" />
            <h2>连接你的第一个邮箱</h2>
            <p>
              账户、邮件与草稿保存在本机
              <br />
              支持 IMAP / SMTP 与邮件服务插件
            </p>
            <button className="primary-button" onClick={setup}>
              添加邮箱账户
            </button>
          </section>
        ) : (
          <ReadingPane
            host={host}
            controller={controller}
            documents={documents}
            composer={composer}
          />
        )}
      </div>
    </section>
  );
}
