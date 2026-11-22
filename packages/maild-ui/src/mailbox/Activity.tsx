import { m } from "@featherpost/i18n/messages";
import { useEffect, useState } from "react";

import { useUiLocale } from "../i18n";
import { Choice } from "../settings/Choice";
import { useStore } from "../host/hooks";
import type { MailboxController } from "./controller";
import {
  ActivityController,
  operationLabel,
  stateLabel,
} from "./activityController";
import type { Operation } from "./types";
import { Dialog } from "./Dialog";
import { fullDate, isoDateTime, plural } from "./format";
import { BRAND } from "../brand";
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
    mail = useStore(mailbox.store),
    [decision, setDecision] = useState<{
      op: Operation;
      resolution: "retry" | "discard";
    } | null>(null);
  useEffect(() => {
    document.title=m.mailbox_activity_document_title({ product: BRAND.productName });
  }, [locale]);
  useEffect(() => {
    controller.show();
    return () => controller.hide();
  }, [controller]);
  return (
    <section className="page activity-page">
      <header className="activity-heading" data-tauri-drag-region="deep">
        <div>
          <h1>{m.mailbox_activity_title()}</h1>
          <p>{m.mailbox_activity_lead()}</p>
        </div>
        <button
          className="text-button"
          disabled={!s.online || s.loading || s.busy !== null}
          onClick={() => void controller.refresh()}
        >
          {m.mailbox_activity_refresh()}
        </button>
      </header>
      <div className="activity-filter">
        <label>
          {m.mailbox_activity_filter_account()}{" "}
          <Choice
            value={s.account}
            disabled={s.busy !== null}
            onValueChange={(value) => controller.filter(value)}
          >
            <option value="">{m.mailbox_activity_filter_all_accounts()}</option>
            {mail.snapshot.accounts.map((a) => (
              <option key={a.account} value={a.account}>
                {mailbox.label(a.account)}
              </option>
            ))}
          </Choice>
        </label>
        <span>{m.mailbox_activity_filter_hint()}</span>
      </div>
      <div className="page-scroll">
        <div className="page-column">
          {!s.online && (
            <p className="list-banner">
              {m.mailbox_activity_offline()}
            </p>
          )}
          {s.error && (
            <p role="alert" className="list-banner">
              {s.error}
            </p>
          )}
          {!s.ops.length && !s.loading && !s.error && (
            <div className="empty">
              <h2>{m.mailbox_activity_empty_title()}</h2>
              <p>{m.mailbox_activity_empty_body()}</p>
            </div>
          )}
          <ol className="operation-list">
            {s.ops.map((op) => {
              const known =
                ["flag", "move", "append"].includes(op.kind) &&
                ["pending", "in_flight", "done", "failed", "unknown"].includes(
                  op.state,
                );
              // i18n-lint-allow-cjk: 「用户放弃」是内核写入 detail 的协议标记（api.rs），不是界面文案。
              const discarded =
                op.state === "failed" && op.detail === "用户放弃";
              const can =
                known && !discarded && ["failed", "unknown"].includes(op.state);
              const target =
                op.target ?? (op.kind === "append" ? op.folder : null);
              const folder = target
                ? mailbox.foldersOf(op.account).find((f) => f.name === target)
                : null;
              return (
                <li
                  key={op.id}
                  className={`operation-row state-${op.state === "unknown" ? "unknown" : op.state === "failed" && !discarded ? "failed" : "other"}`}
                >
                  <div className="operation-top">
                    <strong>{operationLabel(op)}</strong>
                    <span className="operation-state">
                      {discarded ? m.mailbox_activity_state_discarded() : stateLabel(op.state)}
                    </span>
                  </div>
                  <p>
                    {mailbox.label(op.account)} · {op.folder}
                    {op.kind === "move" && op.target ? ` → ${op.target}` : ""}
                    {op.kind === "append" ? "" : ` · ${m.mailbox_activity_message_uid({ uid: op.uid })}`}
                  </p>
                  {op.subject!==undefined&&op.subject!==null&&<p className="operation-subject" title={op.subject}>{op.subject||m.mailbox_subject_empty()}</p>}
                  <div className="operation-meta">
                    <time dateTime={isoDateTime(op.created_at)}>
                      {fullDate(op.created_at)}
                    </time>
                    <span>
                      {plural(op.attempts, {
                        one: m.mailbox_activity_attempts_one,
                        other: m.mailbox_activity_attempts_other,
                      })}
                    </span>
                  </div>
                  {!known && (
                    <p>
                      {m.mailbox_activity_unknown_kind({ kind: op.kind, state: op.state })}
                    </p>
                  )}
                  {op.state === "unknown" && (
                    <p className="operation-warning">
                      {m.mailbox_activity_unknown_warning()}
                    </p>
                  )}
                  {op.detail && !discarded && op.state !== "unknown" && (
                    <p className="operation-detail">{op.detail}</p>
                  )}
                  <div className="operation-actions">
                    {folder && (
                      <button
                        className="text-button"
                        onClick={() => {
                          mailbox.selectFolder(folder);
                          openMailbox();
                        }}
                      >
                        {m.mailbox_activity_open_target()}
                      </button>
                    )}
                    {can && (
                      <>
                        <button
                          className="text-button"
                          disabled={!s.online || s.busy !== null}
                          onClick={() =>
                            setDecision({ op, resolution: "retry" })
                          }
                        >
                          {m.mailbox_activity_retry()}
                        </button>
                        <button
                          className="text-button"
                          disabled={!s.online || s.busy !== null}
                          onClick={() =>
                            setDecision({ op, resolution: "discard" })
                          }
                        >
                          {m.mailbox_activity_discard()}
                        </button>
                      </>
                    )}
                  </div>
                </li>
              );
            })}
          </ol>
          <footer className="activity-foot">
            <span>
              {s.loading
                ? m.mailbox_activity_loading()
                : `${plural(s.ops.length, { one: m.mailbox_activity_loaded_one, other: m.mailbox_activity_loaded_other })}${s.ops.length > 40 ? ` · ${m.mailbox_activity_autorefresh_paused()}` : ""}`}
            </span>
            {s.more && (
              <button
                className="text-button"
                disabled={!s.online || s.loading || s.busy !== null}
                onClick={() => void controller.refresh(true)}
              >
                {m.mailbox_activity_load_more()}
              </button>
            )}
          </footer>
        </div>
      </div>
      {decision && (
        <Dialog
          title={
            decision.resolution === "retry"
              ? m.mailbox_activity_retry_title()
              : m.mailbox_activity_discard_title()
          }
          onClose={() => setDecision(null)}
          busy={s.busy !== null}
        >
          <p>
            {mailbox.label(decision.op.account)} · {operationLabel(decision.op)}{" "}
            · {decision.op.folder}
          </p>
          <p>
            {decision.resolution === "retry"
              ? decision.op.kind === "flag"
                ? m.mailbox_activity_retry_flag_body()
                : m.mailbox_activity_retry_copy_body()
              : m.mailbox_activity_discard_body()}
          </p>
          <footer>
            <button
              className="text-button"
              disabled={s.busy !== null}
              onClick={() => setDecision(null)}
            >
              {m.mailbox_action_cancel()}
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
                ? m.mailbox_activity_submitting()
                : decision.resolution === "retry"
                  ? m.mailbox_activity_retry_confirm()
                  : m.mailbox_activity_discard()}
            </button>
          </footer>
        </Dialog>
      )}
    </section>
  );
}
