import { useStore } from "../host/hooks";
import { messageKey, type MailboxController } from "./controller";
import { displayName, shortDate } from "./format";
export function Thread({ controller }: { controller: MailboxController }) {
  const s = useStore(controller.store),
    row = s.selectedRow;
  if (!row) return null;
  return (
    <div className="thread-section">
      <button
        className="text-button"
        disabled={
          s.snapshot.phase !== "online" ||
          (!s.threadOpen && row.thread_id === 0)
        }
        aria-expanded={s.threadOpen}
        onClick={() => void controller.openThread()}
      >
        {row.thread_id === 0 && !s.threadOpen
          ? "此邮件会话尚未索引"
          : s.threadOpen
            ? "收起会话"
            : "查看会话"}
      </button>
      {s.threadOpen && (
        <>
          <span className="thread-count">
            {s.threadId === 0 ? "" : `${s.threadTotal} 封 · 时间正序`}
          </span>
          <div className="thread-list" aria-label="会话邮件">
            {s.threadRows.map((r) => (
              <button
                key={messageKey(r)}
                type="button"
                className={`thread-item${s.selected && messageKey(s.selected) === messageKey(r) ? " active" : ""}`}
                onClick={() => controller.selectMessage(r, true)}
              >
                <span>
                  <strong>{displayName(r.sender)}</strong>
                  <span>{r.subject || "（无主题）"}</span>
                </span>
                <small>
                  {controller.label(r.account)} · {r.folder} ·{" "}
                  {shortDate(r.sort_at)}
                </small>
              </button>
            ))}
            {s.threadLoading && <p>读取会话中…</p>}
            {!s.threadLoading && !s.threadError && s.threadId === 0 && (
              <p>这封邮件尚未进入线程索引，暂时只能查看当前邮件。</p>
            )}
            {s.threadError && <p role="alert">{s.threadError}</p>}
            {s.threadOffset < s.threadTotal && s.threadRows.length < 1000 && (
              <button
                className="text-button"
                disabled={s.threadLoading || s.snapshot.phase !== "online"}
                onClick={() => void controller.moreThread()}
              >
                加载更多会话邮件
              </button>
            )}
            {s.threadRows.length >= 1000 && s.threadOffset < s.threadTotal && (
              <p>会话较长，请用搜索缩小范围。</p>
            )}
          </div>
        </>
      )}
    </div>
  );
}
