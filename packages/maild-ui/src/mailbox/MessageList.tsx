import { m } from "@featherpost/i18n/messages";

import type { UiSummary } from "../bridge/types";
import { useUiLocale } from "../i18n";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import { messageKey, scopeTitle, type MailboxController } from "./controller";
import { displayName, fullDate, isoDateTime, plural, shortDate } from "./format";
function Row({
  row,
  controller,
}: {
  row: UiSummary;
  controller: MailboxController;
}) {
  useUiLocale();
  const s = controller.state,
    selected = !!s.selected && messageKey(s.selected) === messageKey(row),
    checked = s.checked.some((i) => messageKey(i) === messageKey(row));
  return (
    <li
      className={`message-row${row.seen ? "" : " unread"}${selected ? " selected" : ""}${checked ? " checked" : ""}${row.deleted ? " deleted" : ""}`}
    >
      <input
        className="message-checkbox"
        type="checkbox"
        aria-label={m.mailbox_list_select_label({
          subject: row.subject || m.mailbox_list_select_no_subject(),
        })}
        checked={checked}
        disabled={s.busy}
        onChange={(e) =>
          controller.toggle(row, (e.nativeEvent as MouseEvent).shiftKey)
        }
      />
      <button
        type="button"
        className="message-hit"
        aria-current={selected ? "true" : undefined}
        onClick={(e) => {
          if (e.metaKey || e.ctrlKey || e.shiftKey)
            controller.toggle(row, e.shiftKey);
          else controller.selectMessage(row);
        }}
      >
        <span className="message-top">
          {!row.seen && <span className="unread-dot" aria-hidden="true" />}
          <span className="sender">
            {displayName(row.sender) || m.mailbox_sender_empty()}
          </span>
          {row.has_attachments && <Icon name="paperclip" />}
          <time
            title={fullDate(row.sort_at)}
            dateTime={isoDateTime(row.sort_at)}
          >
            {shortDate(row.sort_at)}
          </time>
        </span>
        <span className="subject-line">{row.subject || m.mailbox_subject_empty()}</span>
        <span className="snippet">{row.snippet}</span>
        {s.scope.kind !== "folder" && (
          <span className="message-source">
            {controller.label(row.account)} · {row.folder}
          </span>
        )}
        <span className="sr-only">
          {row.seen ? "" : m.mailbox_list_sr_unread()}
          {row.flagged ? m.mailbox_list_sr_starred() : ""}
          {row.has_attachments ? m.mailbox_list_sr_attachments() : ""}{row.deleted ? m.mailbox_list_sr_deleted() : ""}
        </span>
      </button>
      <button
        className={`message-star${row.flagged ? " starred" : ""}`}
        disabled={s.busy || s.snapshot.phase !== "online"}
        aria-label={row.flagged ? m.mailbox_star_remove() : m.mailbox_star_add()}
        aria-pressed={row.flagged}
        onClick={() =>
          void controller.perform(
            { action: "mark", flag: "flagged", value: !row.flagged },
            [row],
          )
        }
      >
        <Icon name="star" />
      </button>
    </li>
  );
}
function RemoteRow({
  row,
  controller,
}: {
  row: UiSummary;
  controller: MailboxController;
}) {
  useUiLocale();
  const s = controller.state,
    key = messageKey(row),
    selected = !!s.selected && messageKey(s.selected) === key,
    fetched = s.remote.fetched.includes(key);
  return (
    <li
      className={`message-row remote-row${selected ? " selected" : ""}`}
    >
      <span className="remote-mark" aria-hidden="true" />
      <button
        type="button"
        className="message-hit"
        aria-current={selected ? "true" : undefined}
        onClick={() => controller.openRemote(row)}
      >
        <span className="message-top">
          <span className="sender">
            {displayName(row.sender) || m.mailbox_sender_empty()}
          </span>
          {row.has_attachments && <Icon name="paperclip" />}
          <time
            title={fullDate(row.sort_at)}
            dateTime={isoDateTime(row.sort_at)}
          >
            {shortDate(row.sort_at)}
          </time>
        </span>
        <span className="subject-line">{row.subject || m.mailbox_subject_empty()}</span>
        <span className="snippet">{row.snippet}</span>
        <span className="message-source">
          <span className="tag remote-tag">{fetched ? m.mailbox_remote_tag_fetched() : m.mailbox_remote_tag_server()}</span>{" "}
          {controller.label(row.account)} · {row.folder}
        </span>
      </button>
    </li>
  );
}
/** 搜索结果之后：服务器历史邮件（本地缓存有界）。 */
function RemoteResults({ controller }: { controller: MailboxController }) {
  useUiLocale();
  const s = useStore(controller.store);
  const r = s.remote;
  if (!controller.serverSearchSupported) return null;
  return (
    <li className="remote-section" aria-label={m.mailbox_remote_section_label()}>
      <div className="remote-head">
        <span>
          {r.status === "idle"
            ? m.mailbox_remote_idle()
            : r.status === "loading"
              ? m.mailbox_remote_loading({ query: r.query })
              : r.status === "error"
                ? r.error
                : r.rows.length
                  ? plural(r.rows.length, {
                      one: (inputs) => m.mailbox_remote_found_one({ ...inputs, query: r.query }),
                      other: (inputs) => m.mailbox_remote_found_other({ ...inputs, query: r.query }),
                    })
                  : m.mailbox_remote_none({ query: r.query })}
        </span>
        {r.status !== "loading" && (
          <button
            type="button"
            className="text-button"
            disabled={!controller.canSearchServer}
            title={
              s.scope.kind === "search" && !s.scope.query.query?.trim()
                ? m.mailbox_remote_need_query()
                : undefined
            }
            onClick={() => void controller.searchServer()}
          >
            {r.status === "idle" ? m.mailbox_remote_search() : m.mailbox_remote_search_again()}
          </button>
        )}
      </div>
      {r.status === "ready" && r.error && (
        <p className="remote-warning" role="status">{r.error}</p>
      )}
      {r.rows.length > 0 && (
        <ul className="remote-list">
          {r.rows.map((row) => (
            <RemoteRow key={messageKey(row)} row={row} controller={controller} />
          ))}
        </ul>
      )}
    </li>
  );
}
export function MessageList({ controller }: { controller: MailboxController }) {
  useUiLocale();
  const s = useStore(controller.store),
    syncing = controller.isSyncing(),
    all =
      s.rows.length > 0 &&
      s.rows.every((r) =>
        s.checked.some((i) => messageKey(i) === messageKey(r)),
      );
  return (
    <section className="list-pane" aria-label={m.mailbox_list_label()}>
      <header className="list-title" data-tauri-drag-region="deep">
        <h1>{scopeTitle(s.scope)}</h1>
        <span className="total">
          {plural(s.total, { one: m.mailbox_list_total_one, other: m.mailbox_list_total_other })}
        </span>
      </header>
      <div className="list-selection">
        <label>
          <input
            type="checkbox"
            checked={all}
            disabled={s.busy || !s.rows.length}
            onChange={(e) => controller.selectLoaded(e.target.checked)}
          />{" "}
          {m.mailbox_list_select_loaded()}
        </label>
        <button
          className="text-button"
          disabled={s.loadingRows || syncing || s.snapshot.phase !== "online"}
          title={m.mailbox_list_sync_title()}
          aria-busy={syncing}
          onClick={() => void controller.syncNow()}
        >
          {syncing ? m.mailbox_list_syncing() : m.mailbox_list_sync()}
        </button>
      </div>
      {s.syncError && (
        <div className="list-banner" role="status">
          {s.syncError}
        </div>
      )}
      {s.rowsError && (
        <div className="list-banner" role="status">
          {s.rowsError}
        </div>
      )}
      {!s.rows.length && !s.loadingRows && !s.rowsError && (
        <div className="empty small">
          <p>
            {s.snapshot.accounts.length === 0
              ? m.mailbox_list_empty_no_accounts()
              : s.scope.kind === "search"
                ? m.mailbox_list_empty_search()
                : m.mailbox_list_empty()}
          </p>
          {s.scope.kind === "search" && (
            <button
              className="text-button"
              onClick={() =>
                controller.search({
                  query:
                    s.scope.kind === "search" ? s.scope.query.query : undefined,
                })
              }
            >
              {m.mailbox_list_clear_filters()}
            </button>
          )}
        </div>
      )}
      <ul
        className="message-list"
        onKeyDown={(e) => {
          if(e.target instanceof HTMLInputElement)return;
          const buttons=Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>(':scope > .message-row > .message-hit'));
          const index=buttons.indexOf(document.activeElement as HTMLButtonElement);if(index<0)return;
          if(e.key===' '&&s.checked.length){e.preventDefault();controller.toggle(s.rows[index],e.shiftKey);return;}
          if(!['ArrowDown','ArrowUp'].includes(e.key))return;
          e.preventDefault();const next=Math.max(0,Math.min(buttons.length-1,index+(e.key==='ArrowDown'?1:-1)));
          buttons[next]?.focus();controller.selectMessage(s.rows[next]);
        }}
      >
        {s.rows.map((row) => (
          <Row key={messageKey(row)} row={row} controller={controller} />
        ))}
        {s.scope.kind === "search" && <RemoteResults controller={controller} />}
      </ul>
      <footer className="list-foot">
        <span>
          {s.loadingRows
            ? m.mailbox_list_loading()
            : m.mailbox_list_loaded({ loaded: s.rows.length, total: s.total })}
        </span>
        {controller.canLoadMore && (
          <button
            className="text-button"
            onClick={() => void controller.loadMore()}
          >
            {m.mailbox_list_load_more()}
          </button>
        )}
        {controller.capped && <span>{m.mailbox_list_capped()}</span>}
      </footer>
    </section>
  );
}
