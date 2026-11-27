import { m } from "@featherpost/i18n/messages";
import { Fragment, useEffect, useMemo } from "react";
import type { Host } from "./host/host";
import { useStore } from "./host/hooks";
import { uiLocale, useUiLocale } from "./i18n";
import { Icon } from "./app/Icon";
import {
  messageKey,
  type MailboxController,
} from "./mailbox/controller";
import type { MailboxQuery } from "./mailbox/types";
import { createMessageListActions } from "./mailbox/listActions";
import { useSelectedRowScroll } from "./mailbox/listScroll";
import { displayName, shortDate } from "./mailbox/format";
import { Actions } from "./mailbox/Actions";
import type { DocumentController } from "./mailbox/document";
import type { ComposeController } from "./compose/controller";
import { ReadingPane } from "./ReadingPane";
import { applySearchQuery, scopeContext, filterUnread } from "./navigation";
import {
  activeFilterChips,
  canonicalScopeKey,
  clearStructuredFilters,
  hasActiveSearch,
  removeFilterChip,
  type SearchFilterMeta,
} from "./searchFilters";
import { ACTIVITY } from "./views";
import { useContextMenu } from "./ContextMenu";
import { messageMenu, type MailMenuDeps } from "./menus";
import { useStudioActions } from "./studioActions";
import { Checkbox } from "./ui";

const SHORTCUT_BLOCKED_SELECTOR =
  'input,textarea,select,summary,[contenteditable="true"],[data-shortcuts-off],[role="checkbox"],[role="switch"],[role="combobox"],[role="listbox"],[role="option"],[role="menu"],[role="menuitem"]';

export function isShortcutBlockedTarget(
  target: { closest?(selector: string): unknown } | null | undefined,
): boolean {
  return Boolean(target && typeof target.closest === "function" && target.closest(SHORTCUT_BLOCKED_SELECTOR));
}

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
    findSender: (address) =>
      actions.findFromSender
        ? actions.findFromSender(address)
        : actions.search(address, true),
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
  const actions = useStudioActions();
  const context = scopeContext(controller);
  const searching = s.scope.kind === "search" && hasActiveSearch(s.scope.query);
  const unread =
    s.scope.kind === "unread" ||
    (s.scope.kind === "smart" && s.scope.name === "unread");
  const folderUnread = context.folder?.unseen;
  const title = searching ? m.studio_list_search_results() : context.name;
  const scopeKey = canonicalScopeKey(s.scope);
  const chips =
    s.scope.kind === "search"
      ? activeFilterChips(s.scope.query, s.scope.meta)
      : [];
  const applyNextFilterQuery = (
    nextQuery: MailboxQuery,
    nextMeta?: SearchFilterMeta,
  ) => {
    if (actions.updateSearchFilters) {
      actions.updateSearchFilters(nextQuery, nextMeta);
    } else {
      applySearchQuery(controller, nextQuery, nextMeta);
    }
  };
  const menu = useContextMenu(),
    menuDeps = useMailMenu(controller, composer);
  const listActions = useMemo(
    () => createMessageListActions(controller),
    [controller],
  );
  const selectedKey = s.selected ? messageKey(s.selected) : null;
  const {
    listRef,
    setSelectedRowRef,
    setPendingIntent,
    revealSelected,
    cancelScroll,
  } = useSelectedRowScroll<HTMLDivElement, HTMLElement>(
    selectedKey,
    scopeKey,
    s.rows,
    s.remote.rows,
  );
  useEffect(() => {
    const stepList = (delta: 1 | -1, repeat: boolean) => {
      const intent = { repeat, revealGroupHeader: delta < 0 };
      setPendingIntent(intent);
      const result = listActions.step(delta);
      if (result.atBoundary) {
        setPendingIntent(null);
        revealSelected(intent);
      }
    };
    const key = (e: KeyboardEvent) => {
      if (
        e.defaultPrevented ||
        e.metaKey ||
        e.ctrlKey ||
        e.altKey ||
        e.isComposing ||
        document.querySelector('dialog[open], [aria-modal="true"]') ||
        (e.target instanceof HTMLElement && isShortcutBlockedTarget(e.target))
      )
        return;
      if (e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault();
        stepList(1, e.repeat);
      }
      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        stepList(-1, e.repeat);
      }
      if (e.key === "Escape") listActions.clearOpenSelection();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [listActions, revealSelected, setPendingIntent]);
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
            {s.scope.kind === "search" && chips.length > 0 && (
              <div
                className="search-filter-chips"
                role="group"
                aria-label={m.studio_search_chips_label()}
              >
                {chips.map((chip) => (
                  <span className="search-filter-chip" key={chip.id}>
                    <button
                      type="button"
                      className="search-filter-chip-body"
                      title={chip.label}
                      aria-label={m.studio_search_chip_edit({
                        label: chip.label,
                      })}
                      onClick={() => actions.openSearchFilters?.()}
                    >
                      <span>{chip.label}</span>
                    </button>
                    <button
                      type="button"
                      className="search-filter-chip-remove"
                      title={m.studio_search_chip_remove({
                        label: chip.label,
                      })}
                      aria-label={m.studio_search_chip_remove({
                        label: chip.label,
                      })}
                      onClick={() => {
                        if (s.scope.kind !== "search") return;
                        const next = removeFilterChip(
                          s.scope.query,
                          s.scope.meta,
                          chip.id,
                        );
                        applyNextFilterQuery(next.query, next.meta);
                      }}
                    >
                      ×
                    </button>
                  </span>
                ))}
                <button
                  type="button"
                  className="search-filter-chips-clear"
                  onClick={() => {
                    if (s.scope.kind !== "search") return;
                    applyNextFilterQuery(clearStructuredFilters(s.scope.query));
                  }}
                >
                  {m.studio_search_chips_clear()}
                </button>
              </div>
            )}
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
              const key = messageKey(row);
              const selected = selectedKey === key;
              const checked = s.checked.some((r) => messageKey(r) === key);
              return (
                <Fragment key={key}>
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
                    <Checkbox
                      className="row-check"
                      aria-label={
                        row.subject
                          ? m.studio_list_select({ subject: row.subject })
                          : m.studio_list_select_untitled()
                      }
                      checked={checked}
                      disabled={s.busy}
                      onCheckedChange={(next, details) =>
                        listActions.setCheckedByKey(key, next, {
                          range: details.shiftKey,
                        })
                      }
                    />
                    <button
                      className="message-open"
                      aria-current={selected ? "true" : undefined}
                      onClick={() => listActions.openByKey(key)}
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
                      onClick={() => listActions.openByKey(key)}
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
