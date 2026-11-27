import { m } from "@featherpost/i18n/messages";
import { useEffect, useRef, useState } from "react";
import { AlertDialog } from "../app/Dialog";
import { Choice } from "../settings/Choice";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import { clockTime } from "../mailbox/format";
import { useUiLocale } from "../i18n";
import type { ComposeController } from "./controller";
import { RecipientField } from "./RecipientField";
const size = (n: number) =>
  n >= 1048576
    ? `${(n / 1048576).toFixed(1)} MiB`
    : `${Math.ceil(n / 1024)} KiB`;
export function Editor({
  controller,
  inline = false,
}: {
  controller: ComposeController;
  inline?: boolean;
}) {
  useUiLocale();
  const s = useStore(controller.store),
    root = useRef<HTMLFormElement>(null),
    composingRef = useRef(false);
  const [extras, setExtras] = useState(false);
  useEffect(() => {
    setExtras(
      Boolean(
        s.fields?.cc ||
          s.fields?.bcc ||
          s.recipients?.cc.chips.length ||
          s.recipients?.cc.pending ||
          s.recipients?.bcc.chips.length ||
          s.recipients?.bcc.pending,
      ),
    );
  }, [s.draft?.id]);
  useEffect(() => {
    if (s.errors.cc || s.errors.bcc) {
      setExtras(true);
    }
  }, [s.errors.cc, s.errors.bcc]);
  useEffect(() => {
    if (!Object.values(s.errors).some(Boolean)) return;
    root.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
  }, [s.errors, extras]);
  if (!s.draft || !s.fields)
    return (
      <div className="empty">
        <h2>{m.compose_empty_title()}</h2>
        <p>{m.compose_empty_body()}</p>
        <button
          className="primary-button"
          onClick={() => void controller.create()}
        >
          {m.compose_command_new()}
        </button>
      </div>
    );
  const d = s.draft,
    f = s.fields,
    locked =
      s.busy ||
      s.closing ||
      s.uncertain ||
      !["editing", "failed"].includes(d.send_state) ||
      d.has_html;
  const sender = s.senders.find(
    (v) => v.account === f.account && (v.address === f.from || !f.from),
  );
  const fromValue = JSON.stringify([
    f.account,
    f.from || sender?.address || "",
  ]);
  const sent = d.send_state === "sent",
    unknown = s.uncertain || d.send_state === "unknown",
    sending = d.send_state === "sending",
    savedClock = clockTime(d.updated_at);
  const status = s.closing
    ? m.compose_status_closing()
    : s.saving
      ? m.compose_status_saving()
      : s.revision !== s.savedRevision
        ? m.compose_status_unsaved()
        : savedClock
          ? m.compose_status_saved_at({ time: savedClock })
          : m.compose_status_saved();
  const field = (
    name: "subject",
    label: string,
    placeholder: string,
  ) => (
    <div className="compose-field" key={name}>
      <label htmlFor={`compose-${name}`}>{label}</label>
      <div className="compose-control">
        <input
          id={`compose-${name}`}
          name={name}
          value={f[name]}
          placeholder={placeholder}
          disabled={locked}
          onChange={(e) => controller.change(name, e.target.value)}
          aria-invalid={!!s.errors[name]}
          aria-describedby={s.errors[name] ? `error-${name}` : undefined}
        />
        {s.errors[name] && (
          <span className="field-error" id={`error-${name}`}>
            {s.errors[name]}
          </span>
        )}
      </div>
    </div>
  );
  return (
    <form
      ref={root}
      className={`composer${inline ? " inline-composer" : ""}${s.collapsed && inline ? " collapsed" : ""}`}
      onCompositionStartCapture={() => {
        composingRef.current = true;
      }}
      onCompositionEndCapture={() => {
        composingRef.current = false;
      }}
      onBlurCapture={(e) => {
        const next = e.relatedTarget as Node | null;
        if (!next || !root.current?.contains(next)) {
          composingRef.current = false;
        }
      }}
      onSubmit={(e) => {
        e.preventDefault();
        controller.commitAllPendingRecipients();
        void controller.send();
      }}
      onKeyDownCapture={(e) => {
        if (
          s.confirm ||
          !(e.target instanceof Node) ||
          !root.current?.contains(e.target)
        ) {
          return;
        }
        const native = e.nativeEvent;
        if (
          native.isComposing ||
          native.keyCode === 229 ||
          composingRef.current
        ) {
          return;
        }
        if (
          (e.metaKey || e.ctrlKey) &&
          !e.altKey &&
          !e.shiftKey &&
          e.key === "Enter"
        ) {
          e.preventDefault();
          e.stopPropagation();
          if (e.repeat) return;
          controller.commitAllPendingRecipients();
          void controller.send();
        }
      }}
      aria-label={
        inline ? m.compose_editor_reply_label() : m.compose_editor_label()
      }
    >
      <div
        className="compose-heading"
        data-tauri-drag-region={inline ? undefined : "deep"}
      >
        <div className="compose-title">
          <Icon name={inline ? "reply" : "drafts"} />
          <h1>
            {sent
              ? m.compose_title_sent()
              : inline
                ? m.compose_title_reply()
                : m.compose_command_new()}
          </h1>
          <span className="compose-save" role="status" aria-live="polite">
            {status}
          </span>
        </div>
        <div className="compose-heading-actions">
          {inline ? (
            <>
              <button
                type="button"
                className="text-button"
                onClick={() => controller.expand()}
              >
                {m.compose_inline_expand()}
              </button>
              <button
                type="button"
                className="text-button"
                onClick={() => controller.collapse()}
              >
                {s.collapsed
                  ? m.compose_inline_resume()
                  : m.compose_inline_collapse()}
              </button>
            </>
          ) : (
            <button
              type="button"
              className="text-button"
              onClick={() => void controller.leave()}
            >
              <Icon name="back" />
              {m.compose_back_to_reading()}
            </button>
          )}
        </div>
      </div>
      {(!inline || !s.collapsed) && (
        <>
          <div className="compose-scroll">
            {sent && (
              <div className="compose-notice success" role="status">
                <strong>{m.compose_sent_delivered()}</strong>
                <span>
                  {d.sent_archive === true
                    ? m.compose_sent_archived()
                    : m.compose_sent_not_archived()}
                </span>
              </div>
            )}
            {sending && (
              <div className="compose-notice" role="status">
                {m.compose_sending_note()}
              </div>
            )}
            {unknown && (
              <div className="compose-notice warn" role="alert">
                <strong>{m.compose_unknown_title()}</strong>
                <span>{m.compose_unknown_body()}</span>
                <div className="compose-inline-actions">
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => void controller.reconcile()}
                  >
                    {m.compose_unknown_recheck()}
                  </button>
                  {d.send_state === "unknown" && (
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => controller.confirm("resolve")}
                    >
                      {m.compose_unknown_resolve()}
                    </button>
                  )}
                </div>
              </div>
            )}
            {d.send_state === "failed" && (
              <div className="compose-notice warn" role="alert">
                {m.compose_failed_note()}
              </div>
            )}
            {d.has_html && (
              <div className="compose-notice warn">
                <span>{m.compose_html_note()}</span>
                <button
                  type="button"
                  className="text-button"
                  disabled={
                    s.busy || !["editing", "failed"].includes(d.send_state)
                  }
                  onClick={() => void controller.plainText()}
                >
                  {m.compose_html_convert()}
                </button>
              </div>
            )}
            {s.error && (
              <div className="compose-notice error" role="alert">
                <span>{s.error}</span>
                <div className="compose-inline-actions">
                  {!unknown && !sent && !sending && !s.conflict && (
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => void controller.flush().catch(() => {})}
                    >
                      {m.compose_error_retry_save()}
                    </button>
                  )}
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => void controller.copy()}
                  >
                    {m.compose_error_copy()}
                  </button>
                  {s.conflict && (
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => controller.confirm("reload")}
                    >
                      {m.compose_error_reload()}
                    </button>
                  )}
                </div>
              </div>
            )}
            <div className="compose-field">
              <label htmlFor="compose-from">{m.compose_field_from()}</label>
              <div className="compose-control">
                <Choice
                  id="compose-from"
                  value={fromValue}
                  disabled={locked}
                  onValueChange={(value) => controller.setSender(value)}
                  aria-invalid={!!s.errors.from}
                >
                  {!s.senders.some(
                    (v) => JSON.stringify([v.account, v.address]) === fromValue,
                  ) && (
                    <option value={fromValue}>
                      {m.compose_sender_unavailable({ sender: f.from || f.account })}
                    </option>
                  )}
                  {s.senders.map((v) => (
                    <option
                      key={JSON.stringify([v.account, v.address])}
                      value={JSON.stringify([v.account, v.address])}
                    >
                      {v.available
                        ? `${v.label} · ${v.address}`
                        : m.compose_sender_not_configured({
                            label: v.label,
                            address: v.address,
                          })}
                    </option>
                  ))}
                </Choice>
                {s.errors.from && (
                  <span className="field-error">{s.errors.from}</span>
                )}
              </div>
              <button
                type="button"
                className="text-button cc-toggle"
                onClick={() => setExtras(!extras)}
                aria-expanded={extras}
              >
                {m.compose_field_cc_bcc()}
              </button>
            </div>
            <RecipientField
              controller={controller}
              field="to"
              label={m.compose_field_to()}
              placeholder={m.compose_field_to_placeholder()}
              disabled={locked}
              error={s.errors.to}
            />
            {extras && (
              <>
                <RecipientField
                  controller={controller}
                  field="cc"
                  label={m.compose_field_cc()}
                  placeholder={m.compose_field_cc_placeholder()}
                  disabled={locked}
                  error={s.errors.cc}
                />
                <RecipientField
                  controller={controller}
                  field="bcc"
                  label={m.compose_field_bcc()}
                  placeholder={m.compose_field_bcc_placeholder()}
                  disabled={locked}
                  error={s.errors.bcc}
                />
              </>
            )}
            {field(
              "subject",
              m.compose_field_subject(),
              m.compose_field_subject_placeholder(),
            )}
            <div className="compose-body">
              <label className="visually-hidden" htmlFor="compose-body">
                {m.compose_field_body()}
              </label>
              <textarea
                id="compose-body"
                value={f.body}
                disabled={locked}
                onChange={(e) => controller.change("body", e.target.value)}
                placeholder={m.compose_field_body_placeholder()}
                spellCheck
                aria-invalid={!!s.errors.body}
              />
              {s.errors.body && (
                <span className="field-error">{s.errors.body}</span>
              )}
            </div>
            {(d.attachments.length > 0 || s.uploads.length > 0) && (
              <ul
                className="compose-attachments"
                aria-label={m.compose_attachments_label()}
              >
                {d.attachments.map((a, i) => (
                  <li
                    key={
                      a.upload_id ??
                      `${a.account}/${a.folder}/${a.uid}/${a.part_index}/${i}`
                    }
                  >
                    <Icon name="paperclip" />
                    <div className="attachment-name">
                      <strong>{a.filename || m.compose_attachment_unnamed()}</strong>
                      <small>
                        {size(a.size)} ·{" "}
                        {a.stored
                          ? m.compose_attachment_stored()
                          : m.compose_attachment_missing()}
                      </small>
                    </div>
                    <button
                      type="button"
                      className="text-button"
                      disabled={locked}
                      onClick={() => void controller.detach(i)}
                      aria-label={m.compose_attachment_remove_label({ name: a.filename })}
                    >
                      {m.compose_attachment_remove()}
                    </button>
                  </li>
                ))}
                {s.uploads.map((u) => (
                  <li key={u.token}>
                    <Icon name="paperclip" />
                    <div className="attachment-name">
                      <strong>{u.filename}</strong>
                      <small>
                        {u.error ??
                          (u.status === "finishing"
                            ? m.compose_upload_finishing()
                            : m.compose_upload_preparing({
                                done: size(u.offset),
                                total: size(u.size),
                              }))}
                      </small>
                      {u.status === "uploading" && (
                        <progress
                          value={u.offset}
                          max={Math.max(u.size, 1)}
                          aria-label={m.compose_upload_progress_label({ name: u.filename })}
                        />
                      )}
                    </div>
                    {u.status === "error" && (
                      <button
                        type="button"
                        className="text-button"
                        onClick={() => void controller.upload(u.token)}
                      >
                        {m.compose_upload_retry()}
                      </button>
                    )}
                    <button
                      type="button"
                      className="text-button"
                      disabled={u.status === "finishing"}
                      onClick={() => void controller.removeUpload(u.token)}
                    >
                      {m.compose_upload_cancel()}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="compose-footer">
            <div className="compose-inline-actions">
              <button
                type="submit"
                className="primary-button"
                disabled={
                  locked || s.conflict || !s.online || s.uploads.length > 0
                }
              >
                <Icon name="sent" />
                {s.busy || sending
                  ? m.compose_send_busy()
                  : d.send_state === "failed"
                    ? m.compose_send_retry()
                    : m.compose_send()}
              </button>
              {!sent && !unknown && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => void controller.pick()}
                  disabled={locked || !s.online || !!sender?.simple}
                >
                  <Icon name="paperclip" />
                  {m.compose_attach()}
                </button>
              )}
            </div>
            <span className="compose-hint">
              {inline
                ? ""
                : sent
                  ? ""
                  : s.online
                    ? m.compose_hint_online()
                    : m.compose_hint_offline()}
            </span>
            <button
              type="button"
              className="icon-button danger-action"
              aria-label={
                sent ? m.compose_discard_sent() : m.compose_discard()
              }
              title={sent ? m.compose_discard_sent() : m.compose_discard()}
              disabled={s.busy || sending}
              onClick={() => controller.confirm("discard")}
            >
              <Icon name="trash" />
            </button>
          </div>
        </>
      )}
      {s.confirm && <Confirm controller={controller} />}
    </form>
  );
}
const CONFIRM_LABELS = {
  subject: [m.compose_confirm_subject_title, m.compose_confirm_send_anyway],
  attachment: [m.compose_confirm_attachment_title, m.compose_confirm_send_anyway],
  discard: [m.compose_confirm_discard_title, m.compose_discard],
  resolve: [m.compose_confirm_resolve_title, m.compose_confirm_resolve_action],
  reload: [m.compose_confirm_reload_title, m.compose_confirm_reload_action],
} as const;
function Confirm({ controller }: { controller: ComposeController }) {
  useUiLocale();
  const { confirm, busy } = useStore(controller.store);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const labels = CONFIRM_LABELS;
  if (!confirm) return null;
  const action = () => {
    const selected = confirm;
    controller.confirm(null);
    if (selected === "discard") void controller.discard();
    else if (selected === "resolve") void controller.resolve();
    else if (selected === "reload") void controller.reload();
    else void controller.send(true);
  };
  const body =
    confirm === "resolve"
      ? m.compose_confirm_resolve_body()
      : confirm === "reload"
        ? m.compose_confirm_reload_body()
        : confirm === "discard"
          ? m.compose_confirm_discard_body()
          : m.compose_confirm_send_body();
  return (
    <AlertDialog
      variant="sheet"
      title={labels[confirm][0]()}
      description={body}
      descriptionClassName="sheet-lead"
      busy={busy}
      showCloseButton={false}
      initialFocusRef={cancelRef}
      onClose={() => controller.confirm(null)}
    >
      <div className="sheet-actions">
        <button
          ref={cancelRef}
          type="button"
          className="text-button"
          disabled={busy}
          onClick={() => controller.confirm(null)}
        >
          {m.compose_confirm_back()}
        </button>
        <button
          type="button"
          className={
            confirm === "discard"
              ? "primary-button danger-button"
              : "primary-button"
          }
          disabled={busy}
          onClick={action}
        >
          {labels[confirm][1]()}
        </button>
      </div>
    </AlertDialog>
  );
}
