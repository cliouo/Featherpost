import { m } from "@featherpost/i18n/messages";

import { useStore } from "../host/hooks";
import { useUiLocale } from "../i18n";
import { messageKey, type MailboxController } from "./controller";
import { displayName, plural, shortDate } from "./format";
export function Thread({ controller }: { controller: MailboxController }) {
  useUiLocale();
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
          ? m.mailbox_thread_not_indexed()
          : s.threadOpen
            ? m.mailbox_thread_collapse()
            : m.mailbox_thread_expand()}
      </button>
      {s.threadOpen && (
        <>
          <span className="thread-count">
            {s.threadId === 0
              ? ""
              : plural(s.threadTotal, {
                  one: m.mailbox_thread_count_one,
                  other: m.mailbox_thread_count_other,
                })}
          </span>
          <div className="thread-list" aria-label={m.mailbox_thread_list_label()}>
            {s.threadRows.map((r) => (
              <button
                key={messageKey(r)}
                type="button"
                className={`thread-item${s.selected && messageKey(s.selected) === messageKey(r) ? " active" : ""}`}
                onClick={() => controller.selectMessage(r, true)}
              >
                <span>
                  <strong>{displayName(r.sender)}</strong>
                  <span>{r.subject || m.mailbox_subject_empty()}</span>
                </span>
                <small>
                  {controller.label(r.account)} · {r.folder} ·{" "}
                  {shortDate(r.sort_at)}
                </small>
              </button>
            ))}
            {s.threadLoading && <p>{m.mailbox_thread_loading()}</p>}
            {!s.threadLoading && !s.threadError && s.threadId === 0 && (
              <p>{m.mailbox_thread_unindexed_body()}</p>
            )}
            {s.threadError && <p role="alert">{s.threadError}</p>}
            {s.threadOffset < s.threadTotal && s.threadRows.length < 1000 && (
              <button
                className="text-button"
                disabled={s.threadLoading || s.snapshot.phase !== "online"}
                onClick={() => void controller.moreThread()}
              >
                {m.mailbox_thread_load_more()}
              </button>
            )}
            {s.threadRows.length >= 1000 && s.threadOffset < s.threadTotal && (
              <p>{m.mailbox_thread_capped()}</p>
            )}
          </div>
        </>
      )}
    </div>
  );
}
