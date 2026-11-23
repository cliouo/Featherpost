import { m } from "@featherpost/i18n/messages";
import { Fragment, useEffect } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useStore } from "../../maild-ui/src/host/hooks";
import { uiLocale, useUiLocale } from "../../maild-ui/src/i18n";
import { Icon } from "../../maild-ui/src/app/Icon";
import {
  messageKey,
  type MailboxController,
} from "../../maild-ui/src/mailbox/controller";
import { useSelectedRowScroll } from "../../maild-ui/src/mailbox/listScroll";
import { displayName, shortDate } from "../../maild-ui/src/mailbox/format";
import { Actions } from "../../maild-ui/src/mailbox/Actions";
import type { DocumentController } from "../../maild-ui/src/mailbox/document";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { ReadingPane } from "./ReadingPane";
import { scopeContext, filterUnread } from "./navigation";
import { ACTIVITY } from "./views";
import { useContextMenu } from "./ContextMenu";
import { messageMenu, type MailMenuDeps } from "./menus";
import { useStudioActions } from "./studioActions";

/** Menu actions shared by list rows and the reading pane. */
export function useMailMenu(
  controller: MailboxController,
  composer: ComposeController,
): MailMenuDeps {
  const actions = useStudioActions();
  return {
    controller,
    reply(row, mode) {
      // Replies open inline under the message, so the message must be open.
      if (mode !== "forward") controller.selectMessage(row);
      void composer.reply(row, mode);
    },
    copy: actions.copy,
    findSender: (address) => actions.search(address, true),
  };
}

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
  useUiLocale();
  const s = useStore(controller.store);
  const context = scopeContext(controller);
  const searching = s.scope.kind === "search" && !!s.scope.query.query?.trim();
  const unread =
    s.scope.kind === "unread" ||
    (s.scope.kind === "smart" && s.scope.name === "unread");
  const folderUnread = context.folder?.unseen;
  const title = searching ? m.studio_list_search_results() : context.name;
  const scopeKey = JSON.stringify(s.scope);
  const menu = useContextMenu(),
    menuDeps = useMailMenu(controller, composer);
  const selectedKey = s.selected ? messageKey(s.selected) : null;
  const {
    listRef,
    setSelectedRowRef,
    setPendingIntent,
    revealSelected,
    cancelScroll,
  } = useSelectedRowScroll<HTMLDivElement, HTMLElement>(
    selectedKey,
    s.scope,
    s.rows,
    s.remote.rows,
  );
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
        const before = controller.state.selected
          ? messageKey(controller.state.selected)
          : null;
        const intent = { repeat: e.repeat, revealGroupHeader: false };
        setPendingIntent(intent);
        const next = controller.step(1);
        if (next && before === messageKey(next)) {
          setPendingIntent(null);
          revealSelected(intent);
        }
      }
      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        const before = controller.state.selected
          ? messageKey(controller.state.selected)
          : null;
        const intent = { repeat: e.repeat, revealGroupHeader: true };
        setPendingIntent(intent);
        const next = controller.step(-1);
        if (next && before === messageKey(next)) {
          setPendingIntent(null);
          revealSelected(intent);
        }
      }
      if (e.key === "Escape") controller.clearSelection();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [controller, revealSelected, setPendingIntent]);
  const day = (at: number) =>
    new Date(at * 1000).toDateString() === new Date().toDateString()
      ? m.studio_list_day_today()
      : m.studio_list_day_earlier();
  const total = (count: number) =>
    new Intl.PluralRules(uiLocale()).select(count) === "one"
      ? m.studio_list_total_one({ count })
      : m.studio_list_total_other({ count });
  return (
    <section className="mail-view">
      {s.syncNotice && (
        <div className="notice" role="status">
          {s.syncNotice}
        </div>
      )}
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
        <section className="message-column" aria-label={m.studio_list_label()}>
          <header className="list-heading">
            <div className="breadcrumb">
              {context.account
                ? controller.label(context.account)
                : (context.group ??
                  (context.accounts
                    ? m.studio_scope_selected_group()
                    : m.studio_scope_all_accounts()))}
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
                  {m.studio_list_exit_search()}
                </button>
              ) : (
                <div className="filter-tabs" aria-label={m.studio_list_filter_label()}>
                  <button
                    className={!unread ? "selected" : ""}
                    aria-pressed={!unread}
                    onClick={() => filterUnread(controller, false)}
                  >
                    {m.studio_list_filter_all()}
                  </button>
                  <button
                    className={unread ? "selected" : ""}
                    aria-pressed={unread}
                    onClick={() => filterUnread(controller, true)}
                  >
                    {m.studio_list_filter_unread()}
                    {folderUnread !== undefined && <span>{folderUnread}</span>}
                  </button>
                </div>
              )}
              <span className="list-order" title={m.studio_list_order_title()}>
                {m.studio_list_order()}
              </span>
            </div>
          </header>
          <div
            ref={listRef}
            className="message-scroll"
            aria-busy={s.loadingRows}
            onWheel={cancelScroll}
          >
            <div className="message-list-body" key={scopeKey}>
            {s.rowsError && (
              <div className="notice error" role="alert">
                {s.rowsError}
                <button
                  className="text-button"
                  onClick={() => void controller.refresh()}
                >
                  {m.studio_list_retry()}
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
                {m.studio_list_loading()}
              </div>
            )}
            {!s.loadingRows && !s.rows.length && (
              <div className="empty-list">
                <Icon name="inbox" />
                <h2>
                  {searching
                    ? m.studio_list_empty_search()
                    : unread
                      ? m.studio_list_empty_unread()
                      : m.studio_list_empty()}
                </h2>
                <p>
                  {searching
                    ? m.studio_list_empty_search_hint()
                    : m.studio_list_empty_hint()}
                </p>
                {searching && controller.canSearchServer && (
                  <button
                    className="text-button"
                    onClick={() => void controller.searchServer()}
                  >
                    {m.studio_list_search_server()}
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
                    ref={selected ? setSelectedRowRef : undefined}
                    className={`message-row ${selected ? "selected" : ""} ${!row.seen ? "unread" : ""} ${checked ? "checked" : ""}`}
                    onContextMenu={(e) =>
                      menu.offer(e, () => messageMenu(row, menuDeps))
                    }
                  >
                    <span className="row-marker" aria-hidden="true" />
                    <input
                      className="row-check"
                      type="checkbox"
                      aria-label={
                        row.subject
                          ? m.studio_list_select({ subject: row.subject })
                          : m.studio_list_select_untitled()
                      }
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
                          {row.subject || m.studio_message_no_subject()}
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
                {m.studio_list_load_more()}
              </button>
            )}
            {searching && s.rows.length > 0 && controller.canSearchServer && (
              <button
                className="load-more"
                disabled={s.remote.status === "loading"}
                onClick={() => void controller.searchServer()}
              >
                {m.studio_list_search_server()}
              </button>
            )}
            {s.remote.error && (
              <div className="notice error" role="alert">
                {s.remote.error}
              </div>
            )}
            {s.remote.rows.length > 0 && (
              <>
                <p className="date-group">{m.studio_list_server_history()}</p>
                {s.remote.rows.map((row) => {
                  const key = messageKey(row);
                  const selected = selectedKey === key;
                  return (
                    <button
                      ref={selected ? setSelectedRowRef : undefined}
                      className={`remote-row ${selected ? "selected" : ""}`}
                      aria-current={selected ? "true" : undefined}
                      key={key}
                      onClick={() => controller.openRemote(row)}
                    >
                      <strong>{row.subject || m.studio_message_no_subject()}</strong>
                      <span>
                        {row.sender} · {controller.label(row.account)}
                      </span>
                    </button>
                  );
                })}
              </>
            )}
            </div>
          </div>
          <footer className="list-footer">
            <span>
              {s.loadingRows ? m.studio_list_updating() : total(s.total)}
              {unread ? ` · ${m.studio_list_footer_unread()}` : ""}
            </span>
            <span>{m.studio_list_keyboard_hint()}</span>
          </footer>
        </section>
        {s.snapshot.phase === "online" && !s.snapshot.accounts.length ? (
          <section className="reader-column welcome-studio">
            <Icon name="inbox" />
            <h2>{m.studio_welcome_title()}</h2>
            <p>
              {m.studio_welcome_local()}
              <br />
              {m.studio_welcome_protocols()}
            </p>
            <button className="primary-button" onClick={setup}>
              {m.studio_account_add_mailbox()}
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
