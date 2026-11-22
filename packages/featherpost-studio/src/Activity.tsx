import { m } from "@featherpost/i18n/messages";
import { Choice } from "../../maild-ui/src/settings/Choice";
import { useEffect, useState } from "react";
import { Icon } from "../../maild-ui/src/app/Icon";
import { BRAND } from "../../maild-ui/src/brand";
import { useStore } from "../../maild-ui/src/host/hooks";
import { folderLabel, uiLocale, useUiLocale } from "../../maild-ui/src/i18n";
import {
  ActivityController,
  operationDetail,
  operationDiscarded,
  operationLabel,
  stateLabel,
} from "../../maild-ui/src/mailbox/activityController";
import type { MailboxController } from "../../maild-ui/src/mailbox/controller";
import { Dialog } from "../../maild-ui/src/mailbox/Dialog";
import {
  clockTime,
  dateOnly,
  fullDate,
  isoDateTime,
} from "../../maild-ui/src/mailbox/format";
import type { Operation } from "../../maild-ui/src/mailbox/types";

const supportedStates = new Set([
  "pending",
  "in_flight",
  "done",
  "failed",
  "unknown",
]);

/** 标记操作的标题：[取消该标记, 设置该标记]，存消息函数，渲染时按当前语言取值。 */
const flagTitles: Record<string, readonly [() => string, () => string]> = {
  seen: [m.studio_activity_op_mark_unread, m.studio_activity_op_mark_read],
  flagged: [m.studio_activity_op_unstar, m.studio_activity_op_star],
  answered: [
    m.studio_activity_op_unmark_answered,
    m.studio_activity_op_mark_answered,
  ],
  deleted: [m.studio_activity_op_undelete, m.studio_activity_op_mark_deleted],
};

function operationTitle(op: Operation, targetRole?: string | null) {
  if (op.kind === "move" && targetRole === "archive")
    return m.studio_activity_op_archive();
  if (op.kind === "move" && targetRole === "trash")
    return m.studio_activity_op_trash();
  if (op.kind === "flag") {
    const label = Object.prototype.hasOwnProperty.call(flagTitles, op.flag ?? "")
      ? flagTitles[op.flag!]
      : undefined;
    return label && typeof op.value === "boolean"
      ? label[op.value ? 1 : 0]()
      : m.studio_activity_op_flags();
  }
  return operationLabel(op);
}

function plural(count: number, one: () => string, other: () => string) {
  return new Intl.PluralRules(uiLocale()).select(count) === "one"
    ? one()
    : other();
}

export function Activity({
  controller,
  mailbox,
  openMailbox,
}: {
  controller: ActivityController;
  mailbox: MailboxController;
  openMailbox: () => void;
}) {
  const locale = useUiLocale();
  const s = useStore(controller.store),
    mail = useStore(mailbox.store);
  const [decision, setDecision] = useState<{
    op: Operation;
    resolution: "retry" | "discard";
  } | null>(null);
  useEffect(() => {
    controller.show();
    return () => controller.hide();
  }, [controller]);
  useEffect(() => {
    document.title = m.studio_activity_document_title({
      product: BRAND.productName,
    });
  }, [locale]);
  return (
    <section className="page studio-activity">
      <header className="activity-heading" data-tauri-drag-region="deep">
        <div>
          <h1>{m.studio_view_activity()}</h1>
          <p>{m.studio_activity_intro()}</p>
        </div>
        <button
          className="activity-refresh"
          disabled={!s.online || s.loading || s.busy !== null}
          onClick={() => void controller.refresh()}
        >
          <Icon name="activity" />
          {m.studio_activity_refresh()}
        </button>
      </header>
      <div className="activity-toolbar">
        <label className="activity-account-filter">
          {m.studio_activity_account_filter()}
          <Choice
            value={s.account}
            disabled={s.busy !== null}
            onValueChange={(value) => controller.filter(value)}
          >
            <option value="">{m.studio_activity_all_accounts()}</option>
            {s.account &&
              !mail.snapshot.accounts.some((a) => a.account === s.account) && (
                <option value={s.account}>
                  {m.studio_activity_account_unavailable({
                    account: mailbox.label(s.account),
                  })}
                </option>
              )}
            {mail.snapshot.accounts.map((a) => (
              <option key={a.account} value={a.account}>
                {mailbox.label(a.account)}
              </option>
            ))}
          </Choice>
        </label>
        <span>{m.studio_activity_order()}</span>
      </div>
      <div className="activity-scroll" aria-busy={s.loading}>
        {!s.online && (
          <p className="activity-banner" role="status">
            {m.studio_activity_offline()}
          </p>
        )}
        {s.error && (
          <p role="alert" className="activity-banner error">
            {s.error}
          </p>
        )}
        {!s.ops.length && s.loading && (
          <div className="activity-empty" role="status">
            {m.studio_activity_loading()}
          </div>
        )}
        {!s.ops.length && s.online && !s.loading && !s.error && (
          <div className="activity-empty">
            <h2>{m.studio_activity_empty_title()}</h2>
            <p>{m.studio_activity_empty_hint()}</p>
          </div>
        )}
        {!!s.ops.length && (
          <>
            <div
              className="activity-columns activity-table-head"
              aria-hidden="true"
            >
              <span className="activity-operation">
                {m.studio_activity_column_operation()}
              </span>
              <span className="activity-account">
                {m.studio_activity_column_account()}
              </span>
              <span className="activity-result">
                {m.studio_activity_column_result()}
              </span>
              <span className="activity-time">
                {m.studio_activity_column_time()}
              </span>
            </div>
            <ol
              className="activity-records"
              aria-label={m.studio_activity_records_label()}
            >
              {s.ops.map((op) => {
                // Retain the shared Activity view's replay gates and confirmation semantics.
                const known =
                  ["flag", "move", "append"].includes(op.kind) &&
                  [
                    "pending",
                    "in_flight",
                    "done",
                    "failed",
                    "unknown",
                  ].includes(op.state);
                const discarded = operationDiscarded(op);
                const can =
                  known &&
                  !discarded &&
                  ["failed", "unknown"].includes(op.state);
                const target =
                  op.target ?? (op.kind === "append" ? op.folder : null);
                const folders = mailbox.foldersOf(op.account);
                const folder = target
                  ? folders.find((f) => f.name === target)
                  : undefined;
                const sourceName = folderLabel(
                  folders.find((f) => f.name === op.folder),
                  op.folder,
                );
                const targetName = target ? folderLabel(folder, target) : "";
                const path =
                  op.kind === "move" && op.target
                    ? `${sourceName} → ${targetName}`
                    : sourceName;
                const subject =
                  op.subject == null
                    ? op.uid == null
                      ? m.studio_activity_subject_missing()
                      : m.studio_activity_message_uid({ uid: op.uid })
                    : op.subject || m.studio_message_no_subject();
                const inspect = () => {
                  if (folder) {
                    mailbox.selectFolder(folder);
                    openMailbox();
                  }
                };
                const actions = can && (
                  <div className="activity-decisions">
                    <button
                      className="activity-stop"
                      disabled={!s.online || s.busy !== null}
                      onClick={() => setDecision({ op, resolution: "discard" })}
                    >
                      {m.studio_activity_discard()}
                    </button>
                    <button
                      className={`activity-retry${op.state === "unknown" ? " primary" : ""}`}
                      disabled={!s.online || s.busy !== null}
                      onClick={() => setDecision({ op, resolution: "retry" })}
                    >
                      {op.state === "unknown"
                        ? m.studio_activity_retry_after_review()
                        : m.studio_activity_retry()}
                    </button>
                  </div>
                );
                return (
                  <li
                    className={`activity-record state-${discarded ? "discarded" : known ? op.state : "unsupported"}`}
                    key={op.id}
                  >
                    <div className="activity-columns activity-record-main">
                      <div className="activity-operation">
                        <strong>{operationTitle(op, folder?.role)}</strong>
                        <p title={subject}>
                          {subject}
                          {op.subject != null &&
                            op.kind !== "append" &&
                            op.uid != null && (
                              <span>
                                {" · "}
                                {m.studio_activity_message_uid({ uid: op.uid })}
                              </span>
                            )}
                        </p>
                      </div>
                      <div className="activity-account">
                        <strong title={mailbox.label(op.account)}>
                          {mailbox.label(op.account)}
                        </strong>
                        {folder ? (
                          <button
                            className="activity-folder-link"
                            title={m.studio_activity_view_target_path({ path })}
                            aria-label={m.studio_activity_view_target_path({
                              path,
                            })}
                            onClick={inspect}
                          >
                            {path}
                          </button>
                        ) : (
                          <p title={path}>{path}</p>
                        )}
                      </div>
                      <div className="activity-result">
                        <span className="activity-state">
                          {discarded
                            ? m.studio_activity_state_discarded()
                            : op.state === "unknown"
                              ? m.studio_activity_state_review()
                              : stateLabel(
                                  supportedStates.has(op.state) ? op.state : "",
                                )}
                        </span>
                        <small>
                          {op.attempts
                            ? plural(
                                op.attempts,
                                () =>
                                  m.studio_activity_attempts_one({
                                    count: op.attempts,
                                  }),
                                () =>
                                  m.studio_activity_attempts_other({
                                    count: op.attempts,
                                  }),
                              )
                            : m.studio_activity_not_attempted()}
                        </small>
                      </div>
                      <div className="activity-time">
                        <time
                          dateTime={isoDateTime(op.created_at)}
                          title={fullDate(op.created_at)}
                          aria-label={fullDate(op.created_at)}
                        >
                          {clockTime(op.created_at)}
                          <small>{dateOnly(op.created_at)}</small>
                        </time>
                      </div>
                    </div>
                    {op.state === "unknown" ? (
                      <div className="activity-review">
                        <strong>
                          {op.kind === "flag"
                            ? m.studio_activity_review_flag_title()
                            : m.studio_activity_review_folder_title()}
                        </strong>
                        <p>
                          {op.kind === "flag"
                            ? m.studio_activity_review_flag_body()
                            : m.studio_activity_review_folder_body()}
                        </p>
                        {!known && (
                          <p>
                            {m.studio_activity_unknown_kind({
                              kind: op.kind,
                              state: op.state,
                            })}
                          </p>
                        )}
                        <footer>
                          {folder ? (
                            <button className="activity-link" onClick={inspect}>
                              {m.studio_activity_view_target()}
                            </button>
                          ) : (
                            <span />
                          )}
                          {actions}
                        </footer>
                      </div>
                    ) : (
                      ((!!op.detail && !discarded) || !known || can) && (
                        <div className="activity-record-detail">
                          <div>
                            {!known && (
                              <p>
                                {m.studio_activity_unknown_kind({
                                  kind: op.kind,
                                  state: op.state,
                                })}
                              </p>
                            )}
                            {op.detail && !discarded && (
                              <p
                                className={op.state === "failed" ? "error" : ""}
                              >
                                {operationDetail(op)}
                              </p>
                            )}
                          </div>
                          {actions}
                        </div>
                      )
                    )}
                  </li>
                );
              })}
            </ol>
          </>
        )}
        <footer className="activity-foot">
          <span role="status">
            {s.loading
              ? m.studio_activity_reading()
              : `${plural(
                  s.ops.length,
                  () => m.studio_activity_loaded_one({ count: s.ops.length }),
                  () => m.studio_activity_loaded_other({ count: s.ops.length }),
                )}${s.ops.length > 40 ? ` · ${m.studio_activity_history_paused()}` : ""}`}
          </span>
          {s.more && (
            <button
              disabled={!s.online || s.loading || s.busy !== null}
              onClick={() => void controller.refresh(true)}
            >
              {m.studio_activity_load_earlier()} <span aria-hidden="true">→</span>
            </button>
          )}
        </footer>
      </div>
      {decision && (
        <Dialog
          title={
            decision.resolution === "retry"
              ? m.studio_activity_confirm_retry_title()
              : m.studio_activity_confirm_discard_title()
          }
          onClose={() => setDecision(null)}
          busy={s.busy !== null}
        >
          <p>
            {mailbox.label(decision.op.account)} ·{" "}
            {operationTitle(
              decision.op,
              mailbox
                .foldersOf(decision.op.account)
                .find((f) => f.name === decision.op.target)?.role,
            )}{" "}
            ·{" "}
            {folderLabel(
              mailbox
                .foldersOf(decision.op.account)
                .find((f) => f.name === decision.op.folder),
              decision.op.folder,
            )}
          </p>
          <p>
            {decision.resolution === "retry"
              ? decision.op.kind === "flag"
                ? m.studio_activity_confirm_retry_flag()
                : m.studio_activity_confirm_retry_folder()
              : m.studio_activity_confirm_discard_body()}
          </p>
          <footer>
            <button
              className="text-button"
              disabled={s.busy !== null}
              onClick={() => setDecision(null)}
            >
              {m.studio_activity_cancel()}
            </button>
            <button
              className="primary-button"
              disabled={!s.online || s.busy !== null}
              onClick={async () => {
                await controller.resolve(decision.op, decision.resolution);
                setDecision(null);
              }}
            >
              {s.busy !== null
                ? m.studio_activity_submitting()
                : decision.resolution === "retry"
                  ? m.studio_activity_confirm_retry()
                  : m.studio_activity_discard()}
            </button>
          </footer>
        </Dialog>
      )}
    </section>
  );
}
