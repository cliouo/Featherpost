import { m } from "@featherpost/i18n/messages";

import { folderLabel, useUiLocale } from "../i18n";
import { Choice } from "../settings/Choice";
import { useState } from "react";
import { useStore } from "../host/hooks";
import { Dialog } from "./Dialog";
import type { MailboxController } from "./controller";
import { plural } from "./format";
export function Actions({
  controller,
  openActivity,
}: {
  controller: MailboxController;
  openActivity: () => void;
}) {
  useUiLocale();
  const s = useStore(controller.store),
    prompt = s.prompt;
  const [target, setTarget] = useState("");
  const frozen = prompt?.rows ?? [];
  const account = frozen[0]?.account ?? "";
  const targets = controller
    .foldersOf(account)
    .filter((f) => f.present && !frozen.some((r) => r.folder === f.name));
  return (
    <>
      {s.checked.length > 0 && (
        <div className="batch-toolbar" aria-label={m.mailbox_batch_toolbar_label()}>
          <strong>
            {plural(s.checked.length, {
              one: m.mailbox_batch_selected_one,
              other: m.mailbox_batch_selected_other,
            })}
          </strong>
          <button
            className="text-button"
            disabled={s.busy}
            onClick={() => controller.selectLoaded(false)}
          >
            {m.mailbox_batch_clear_selection()}
          </button>
          <span className="spacer" />
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() =>
              void controller.perform({
                action: "mark",
                flag: "seen",
                value: true,
              })
            }
          >
            {m.mailbox_batch_mark_read()}
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() =>
              void controller.perform({
                action: "mark",
                flag: "seen",
                value: false,
              })
            }
          >
            {m.mailbox_batch_mark_unread()}
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() =>
              void controller.perform({
                action: "mark",
                flag: "flagged",
                value: true,
              })
            }
          >
            {m.mailbox_batch_star()}
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => void controller.perform({ action: "archive" })}
          >
            {m.mailbox_batch_archive()}
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => controller.requestAction("move")}
          >
            {m.mailbox_batch_move()}
          </button>
          <button
            className="text-button danger-text"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => controller.requestAction("delete")}
          >
            {m.mailbox_batch_delete()}
          </button>
        </div>
      )}
      {s.report && (
        <div
          className={`batch-report${s.report.unconfirmed ? " warn" : ""}`}
          role="status"
        >
          <span>
            {plural(s.report.queued, {
              one: m.mailbox_batch_report_queued_one,
              other: m.mailbox_batch_report_queued_other,
            })}
            {s.report.unchanged
              ? m.mailbox_batch_report_unchanged({ count: s.report.unchanged })
              : ""}
            {s.report.unconfirmed
              ? m.mailbox_batch_report_unconfirmed({ count: s.report.unconfirmed })
              : ""}
            {m.mailbox_batch_report_end()}
            {s.report.error || m.mailbox_batch_report_hint()}
          </span>
          <button className="text-button" onClick={openActivity}>
            {m.mailbox_activity_title()}
          </button>
          <button
            className="text-button"
            onClick={() => controller.dismissReport()}
          >
            {m.mailbox_dialog_close()}
          </button>
        </div>
      )}
      {prompt && (
        <Dialog
          title={
            prompt.kind === "delete"
              ? plural(frozen.length, {
                  one: m.mailbox_batch_delete_title_one,
                  other: m.mailbox_batch_delete_title_other,
                })
              : plural(frozen.length, {
                  one: m.mailbox_batch_move_title_one,
                  other: m.mailbox_batch_move_title_other,
                })
          }
          busy={s.busy}
          onClose={() => controller.closePrompt()}
        >
          <ul className="action-message-list">
            {frozen.map((r) => (
              <li key={JSON.stringify([r.account, r.folder, r.uid])}>
                <strong>{r.subject || m.mailbox_subject_empty()}</strong>
                <span>
                  {controller.label(r.account)} · {r.folder} · {r.sender}
                </span>
              </li>
            ))}
          </ul>
          {prompt.kind === "delete" ? (
            <div className="delete-policies">
              {[
                ...new Map(
                  frozen.map((r) => [JSON.stringify([r.account, r.folder]), r]),
                ).values(),
              ].map((r) => {
                const trash = controller
                  .foldersOf(r.account)
                  .find((f) => f.role === "trash");
                return (
                  <p key={JSON.stringify([r.account, r.folder])}>
                    {m.mailbox_delete_policy_scope({
                      account: controller.label(r.account),
                      folder: r.folder,
                    })}
                    {trash && trash.name !== r.folder
                      ? m.mailbox_delete_policy_trash({ folder: folderLabel(trash) })
                      : m.mailbox_delete_policy_flag()}
                  </p>
                );
              })}
              <p className="group-note">
                {m.mailbox_delete_policy_note()}
              </p>
            </div>
          ) : (
            <label className="move-target">
              {m.mailbox_move_target()}
              <Choice
                aria-label={m.mailbox_move_target()}
                value={targets.some((f) => f.name === target) ? target : ""}
                onValueChange={setTarget}
              >
                <option value="">{m.mailbox_move_target_placeholder()}</option>
                {targets.map((f) => (
                  <option key={f.name} value={f.name}>
                    {folderLabel(f)}
                  </option>
                ))}
              </Choice>
              {targets.length === 0 && (
                <span>{m.mailbox_move_target_empty()}</span>
              )}
            </label>
          )}
          <footer>
            <button
              className="text-button"
              disabled={s.busy}
              onClick={() => controller.closePrompt()}
            >
              {m.mailbox_action_cancel()}
            </button>
            <button
              className="primary-button"
              disabled={
                s.busy ||
                s.snapshot.phase !== "online" ||
                (prompt.kind === "move" &&
                  !targets.some((f) => f.name === target))
              }
              onClick={async () => {
                await controller.perform(
                  prompt.kind === "delete"
                    ? { action: "delete" }
                    : { action: "move", target },
                  frozen,
                );
                controller.closePrompt();
              }}
            >
              {s.busy
                ? m.mailbox_activity_submitting()
                : prompt.kind === "delete"
                  ? m.mailbox_batch_delete_confirm()
                  : m.mailbox_batch_move_confirm()}
            </button>
          </footer>
        </Dialog>
      )}
    </>
  );
}
