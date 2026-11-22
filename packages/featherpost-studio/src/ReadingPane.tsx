import { m } from "@featherpost/i18n/messages";
import { useState } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useDarkShell } from "../../maild-ui/src/app/theme";
import { readingDocumentUrl } from "./readingTheme";
import { useStore } from "../../maild-ui/src/host/hooks";
import { useUiLocale } from "../../maild-ui/src/i18n";
import { Icon } from "../../maild-ui/src/app/Icon";
import { compatBlocksWrites } from "../../maild-ui/src/bridge/types";
import {
  messageKey,
  type MailboxController,
} from "../../maild-ui/src/mailbox/controller";
import {
  presentation,
  type DocumentController,
} from "../../maild-ui/src/mailbox/document";
import {
  displayName,
  addressOnly,
  fullDate,
} from "../../maild-ui/src/mailbox/format";
import {
  AttachmentStrip,
  LinkSheet,
  OtpBar,
  RemoteBanner,
} from "../../maild-ui/src/mailbox/ReaderParts";
import { Thread } from "../../maild-ui/src/mailbox/Thread";
import { detectOtpCode } from "../../maild-ui/src/mailbox/otp";
import {
  sourceKey,
  type ComposeController,
} from "../../maild-ui/src/compose/controller";
import { Editor } from "../../maild-ui/src/compose/Editor";

export function ReadingPane({
  host,
  controller,
  documents,
  composer,
}: {
  host: Host;
  controller: MailboxController;
  documents: DocumentController;
  composer: ComposeController;
}) {
  useUiLocale();
  const s = useStore(controller.store),
    d = useStore(documents.store),
    compose = useStore(composer.store);
  const row = s.selectedRow,
    detail = s.detail,
    body = presentation(detail, d);
  const dark = useDarkShell(useStore(host.state).preferences.theme);
  const [originalFor, setOriginalFor] = useState<string | null>(null);
  const originalColors = !!row && originalFor === messageKey(row);
  const writable =
    s.snapshot.phase === "online" && !compatBlocksWrites(s.snapshot) && !s.busy;
  const otp = row
    ? detectOtpCode(row.subject, detail?.body || row.snippet)
    : null;
  const remote = !!row && controller.isRemote(row);
  const navigationRows = remote ? s.remote.rows : s.rows;
  const navigationTotal = remote ? navigationRows.length : s.total;
  const index = row
    ? navigationRows.findIndex((r) => messageKey(r) === messageKey(row))
    : -1;
  const profile = row
    ? s.profiles.find((p) => p.id === row.account)
    : undefined;
  const inline = !!row && compose.inline && compose.source === sourceKey(row);
  return (
    <section className="reader-column" aria-label={m.studio_reader_label()}>
      <div className="reader-actions">
        <button
          className="icon-button back-button"
          aria-label={m.studio_reader_back()}
          onClick={() => controller.clearSelection()}
        >
          <Icon name="back" />
        </button>
        <button
          className="text-button"
          title={m.studio_reader_archive()}
          aria-label={m.studio_reader_archive()}
          disabled={!row || !writable}
          onClick={() => void controller.perform({ action: "archive" })}
        >
          <Icon name="archive" />
          <span>{m.studio_reader_archive()}</span>
        </button>
        <button
          className="icon-button"
          title={m.studio_reader_delete()}
          aria-label={m.studio_reader_delete()}
          disabled={!row || !writable}
          onClick={() => controller.requestAction("delete")}
        >
          <Icon name="trash" />
        </button>
        <span className="toolbar-divider" />
        <button
          className="text-button"
          title={m.studio_reader_move_title()}
          aria-label={m.studio_reader_move_title()}
          disabled={!row || !writable}
          onClick={() => controller.requestAction("move")}
        >
          <Icon name="folder" />
          <span>{m.studio_reader_move()}</span>
        </button>
        <button
          className={`icon-button ${row?.flagged ? "starred" : ""}`}
          title={m.studio_reader_toggle_star()}
          aria-label={m.studio_reader_toggle_star()}
          aria-pressed={row?.flagged ?? false}
          disabled={!row || !writable}
          onClick={() => void controller.toggleSelected("flagged")}
        >
          <Icon name="star" />
        </button>
        <details
          className="reader-menu"
          key={row ? messageKey(row) : "empty"}
          data-shortcuts-off
        >
          <summary
            className="icon-button"
            aria-label={m.studio_reader_more()}
            title={m.studio_reader_more()}
          >
            <Icon name="more" />
          </summary>
          <div className="reader-menu-panel">
            <button
              disabled={!row || !writable}
              onClick={(e) => {
                e.currentTarget.closest("details")?.removeAttribute("open");
                void controller.toggleSelected("seen");
              }}
            >
              {row?.seen
                ? m.studio_reader_mark_unread()
                : m.studio_reader_mark_read()}
            </button>
            <button
              disabled={!row || s.snapshot.phase !== "online"}
              onClick={(e) => {
                e.currentTarget.closest("details")?.removeAttribute("open");
                void controller.openThread();
              }}
            >
              {s.threadOpen
                ? m.studio_reader_thread_hide()
                : m.studio_reader_thread_show()}
            </button>
          </div>
        </details>
        <span className="spacer" />
        {dark && row && body.mode === "html" && (
          <button
            className="text-button reading-theme"
            title={
              originalColors
                ? m.studio_reader_theme_dark_title()
                : m.studio_reader_theme_original_title()
            }
            onClick={() =>
              setOriginalFor(originalColors ? null : messageKey(row))
            }
          >
            {originalColors
              ? m.studio_reader_theme_dark()
              : m.studio_reader_theme_original()}
          </button>
        )}
        <span className="reader-position">
          {index >= 0
            ? `${index + 1} / ${navigationTotal}`
            : row
              ? m.studio_reader_position_thread()
              : ""}
        </span>
        <button
          className="icon-button previous-mail"
          title={m.studio_reader_previous()}
          aria-label={m.studio_reader_previous()}
          disabled={!row || index <= 0}
          onClick={() => controller.step(-1)}
        >
          <Icon name="back" />
        </button>
        <button
          className="icon-button next-mail"
          title={m.studio_reader_next()}
          aria-label={m.studio_reader_next()}
          disabled={
            !row ||
            (index >= navigationRows.length - 1 &&
              (remote || !controller.canLoadMore))
          }
          onClick={() => controller.step(1)}
        >
          <Icon name="back" />
        </button>
      </div>
      {!row ? (
        <div className="reader-empty">
          <Icon name="mail" />
          <h2>{m.studio_reader_empty_title()}</h2>
          <p>{m.studio_reader_empty_hint()}</p>
          <div className="empty-shortcuts">
            <span>
              <kbd>⌘ N</kbd> {m.studio_reader_shortcut_compose()}
            </span>
            <span>
              <kbd>⌘ K</kbd> {m.studio_reader_shortcut_search()}
            </span>
          </div>
        </div>
      ) : (
        <div className="letter-scroll">
          <div className="letter-content">
            <header className="letter-heading">
              <h2>{row.subject || m.studio_message_no_subject()}</h2>
              <div className="letter-sender">
                <div>
                  <strong>{displayName(row.sender) || row.sender}</strong>
                  <span className="sender-address">
                    {addressOnly(row.sender)}
                  </span>
                  <p>
                    {m.studio_reader_to({ recipient: row.recipient || "—" })}
                    {detail?.cc
                      ? ` · ${m.studio_reader_cc({ cc: detail.cc })}`
                      : ""}
                  </p>
                </div>
                <time>{fullDate(row.sort_at)}</time>
              </div>
              {otp && (
                <OtpBar code={otp} notify={(text) => host.notify(text)} />
              )}
              {s.detailError && (
                <div className="notice error" role="alert">
                  {s.detailError}
                </div>
              )}
              {d.error && (
                <div className="notice error" role="alert">
                  {d.error}
                </div>
              )}
              {detail && (
                <RemoteBanner
                  host={host}
                  detail={detail}
                  state={d}
                  documents={documents}
                  onPolicyChanged={() => controller.reloadSelected()}
                />
              )}
            </header>
            {s.loadingDetail || body.mode === "loading" ? (
              <div className="body-loading" role="status">
                {m.studio_reader_loading_body()}
              </div>
            ) : detail && body.mode === "html" ? (
              <iframe
                className="studio-mail-frame"
                key={body.token}
                src={readingDocumentUrl(body.url, dark && !originalColors)}
                sandbox=""
                referrerPolicy="no-referrer"
                title={m.studio_reader_frame_title()}
              />
            ) : (
              detail && (
                <>
                  <pre className="studio-letter">
                    {detail.body || m.studio_reader_body_empty()}
                  </pre>
                  {body.mode === "text" && body.note && detail.has_html && (
                    <p className="body-note">{body.note}</p>
                  )}
                </>
              )
            )}
            {detail && (
              <AttachmentStrip
                bridge={host.bridge}
                detail={detail}
                notify={(text) => host.notify(text)}
              />
            )}
            {s.threadOpen && <Thread controller={controller} />}
            <section className="reply-section" aria-label={m.studio_reader_reply_label()}>
              <p className="reply-identity">
                {m.studio_reader_reply_identity({
                  account: controller.label(row.account),
                  address: profile?.address || row.account,
                })}
              </p>
              {!inline && (
                <div className="reply-bar">
                  <button
                    className="primary-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "reply")}
                  >
                    <Icon name="reply" />
                    {m.studio_reader_reply()}
                  </button>
                  <button
                    className="text-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "reply_all")}
                  >
                    <Icon name="reply" />
                    {m.studio_reader_reply_all()}
                  </button>
                  <button
                    className="text-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "forward")}
                  >
                    <Icon name="forward" />
                    {m.studio_reader_forward()}
                  </button>
                </div>
              )}
              {inline && <Editor controller={composer} inline />}
            </section>
          </div>
        </div>
      )}
      {d.pendingLink && (
        <LinkSheet
          link={d.pendingLink}
          documents={documents}
          notify={(text) => host.notify(text)}
          compose={(address) => {
            documents.dismissLink();
            void composer.create(address);
          }}
        />
      )}
    </section>
  );
}
