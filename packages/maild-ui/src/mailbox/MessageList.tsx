import type { UiSummary } from "../bridge/types";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import { messageKey, scopeTitle, type MailboxController } from "./controller";
import { displayName, shortDate } from "./format";
function Row({
  row,
  controller,
}: {
  row: UiSummary;
  controller: MailboxController;
}) {
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
        aria-label={`选择 ${row.subject || "无主题"}`}
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
            {displayName(row.sender) || "（无发件人）"}
          </span>
          {row.has_attachments && <Icon name="paperclip" />}
          <time
            title={new Date(row.sort_at * 1000).toLocaleString()}
            dateTime={new Date(row.sort_at * 1000).toISOString()}
          >
            {shortDate(row.sort_at)}
          </time>
        </span>
        <span className="subject-line">{row.subject || "（无主题）"}</span>
        <span className="snippet">{row.snippet}</span>
        {s.scope.kind !== "folder" && (
          <span className="message-source">
            {controller.label(row.account)} · {row.folder}
          </span>
        )}
        <span className="sr-only">
          {row.seen ? "" : "未读，"}
          {row.flagged ? "已加星标，" : ""}
          {row.has_attachments ? "有附件" : ""}{row.deleted ? "，已标记删除" : ""}
        </span>
      </button>
      <button
        className={`message-star${row.flagged ? " starred" : ""}`}
        disabled={s.busy || s.snapshot.phase !== "online"}
        aria-label={row.flagged ? "取消星标" : "添加星标"}
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
            {displayName(row.sender) || "（无发件人）"}
          </span>
          {row.has_attachments && <Icon name="paperclip" />}
          <time
            title={new Date(row.sort_at * 1000).toLocaleString()}
            dateTime={new Date(row.sort_at * 1000).toISOString()}
          >
            {shortDate(row.sort_at)}
          </time>
        </span>
        <span className="subject-line">{row.subject || "（无主题）"}</span>
        <span className="snippet">{row.snippet}</span>
        <span className="message-source">
          <span className="tag remote-tag">{fetched ? "已取回本地" : "服务器历史"}</span>{" "}
          {controller.label(row.account)} · {row.folder}
        </span>
      </button>
    </li>
  );
}
/** 搜索结果之后：服务器历史邮件（本地缓存有界）。 */
function RemoteResults({ controller }: { controller: MailboxController }) {
  const s = useStore(controller.store);
  const r = s.remote;
  if (!controller.serverSearchSupported) return null;
  return (
    <li className="remote-section" aria-label="服务器历史邮件">
      <div className="remote-head">
        <span>
          {r.status === "idle"
            ? "本机只缓存最近的邮件。"
            : r.status === "loading"
              ? `正在服务器上搜索「${r.query}」…`
              : r.status === "error"
                ? r.error
                : r.rows.length
                  ? `服务器上另有 ${r.rows.length} 封匹配「${r.query}」的邮件，点开即取回本地。`
                  : `服务器上没有更多匹配「${r.query}」的邮件。`}
        </span>
        {r.status !== "loading" && (
          <button
            type="button"
            className="text-button"
            disabled={!controller.canSearchServer}
            title={
              s.scope.kind === "search" && !s.scope.query.query?.trim()
                ? "请先输入关键词"
                : undefined
            }
            onClick={() => void controller.searchServer()}
          >
            {r.status === "idle" ? "在服务器搜索更早邮件" : "重新搜索服务器"}
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
  const s = useStore(controller.store),
    all =
      s.rows.length > 0 &&
      s.rows.every((r) =>
        s.checked.some((i) => messageKey(i) === messageKey(r)),
      );
  return (
    <section className="list-pane" aria-label="邮件列表">
      <header className="list-title" data-tauri-drag-region="deep">
        <h1>{scopeTitle(s.scope)}</h1>
        <span className="total">{s.total} 封</span>
      </header>
      <div className="list-selection">
        <label>
          <input
            type="checkbox"
            checked={all}
            disabled={s.busy || !s.rows.length}
            onChange={(e) => controller.selectLoaded(e.target.checked)}
          />{" "}
          全选当前加载
        </label>
        <button
          className="text-button"
          disabled={s.loadingRows || s.syncing || s.snapshot.phase !== "online"}
          title="请后台立即检查新邮件并刷新列表（⌘R）"
          aria-busy={s.syncing}
          onClick={() => void controller.syncNow()}
        >
          {s.syncing ? "正在同步…" : "刷新"}
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
              ? "还没有连接邮箱。在「设置 → 账户」添加第一个邮箱。"
              : s.scope.kind === "search"
                ? "没有找到匹配的本地邮件。更早的邮件可能只在服务器上。"
                : "这里暂时没有邮件。"}
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
              清除筛选条件
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
          {s.loadingRows ? "读取中…" : `已加载 ${s.rows.length} / ${s.total}`}
        </span>
        {controller.canLoadMore && (
          <button
            className="text-button"
            onClick={() => void controller.loadMore()}
          >
            加载更多
          </button>
        )}
        {controller.capped && <span>请用搜索缩小范围</span>}
      </footer>
    </section>
  );
}
