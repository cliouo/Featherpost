import { m } from "@featherpost/i18n/messages";
import { useState } from "react";
import {
  StudioDropdownMenu,
  StudioDropdownMenuItem,
} from "./app/DropdownMenu";
import type { Host } from "./host/host";
import { useDarkShell } from "./app/theme";
import { readingDocumentUrl } from "./readingTheme";
import { useStore } from "./host/hooks";
import { useUiLocale } from "./i18n";
import { Icon } from "./app/Icon";
import { compatBlocksWrites } from "./bridge/types";
import {
  messageKey,
  type MailboxController,
} from "./mailbox/controller";
import {
  presentation,
  type DocumentController,
} from "./mailbox/document";
import {
  displayName,
  addressOnly,
  fullDate,
} from "./mailbox/format";
import {
  AttachmentStrip,
  LinkSheet,
  OtpBar,
  RemoteBanner,
} from "./mailbox/ReaderParts";
import { Thread } from "./mailbox/Thread";
import { detectOtpCode } from "./mailbox/otp";
import {
  sourceKey,
  type ComposeController,
} from "./compose/controller";
import { Editor } from "./compose/Editor";
import { useContextMenu } from "./ContextMenu";
import { addressMenu, messageMenu } from "./menus";
import { useStudioActions } from "./studioActions";
import { useMailMenu } from "./Workspace";

export function ReaderMoreMenu({
  seen,
  threadOpen,
  canToggleSeen,
  canOpenThread,
  onToggleSeen,
  onOpenThread,
}: {
  seen?: boolean;
  threadOpen: boolean;
  canToggleSeen: boolean;
  canOpenThread: boolean;
  onToggleSeen(): void;
  onOpenThread(): void;
}) {
  useUiLocale();
  return (
    <StudioDropdownMenu
      className="reader-menu"
      contentClassName="reader-menu-panel"
      align="start"
      trigger={
        <button
          type="button"
          className="icon-button"
          aria-label={m.studio_reader_more()}
          title={m.studio_reader_more()}
        >
          <Icon name="more" />
        </button>
      }
    >
      <StudioDropdownMenuItem disabled={!canToggleSeen} onSelect={onToggleSeen}>
        {seen ? m.studio_reader_mark_unread() : m.studio_reader_mark_read()}
      </StudioDropdownMenuItem>
      <StudioDropdownMenuItem disabled={!canOpenThread} onSelect={onOpenThread}>
        {threadOpen
          ? m.studio_reader_thread_hide()
          : m.studio_reader_thread_show()}
      </StudioDropdownMenuItem>
    </StudioDropdownMenu>
  );
}

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
  const menu = useContextMenu(),
    actions = useStudioActions(),
    menuDeps = useMailMenu(controller, composer);
  return (
    <section className="reader-column" aria-label={m.studio_reader_label()}>
      <div className="reader-actions">
        <button
          className="icon-button back-button"
          title={m.studio_reader_back()}
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
          className="icon-button danger-action"
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
        <ReaderMoreMenu
          key={row ? messageKey(row) : "empty"}
          seen={row?.seen}
          threadOpen={s.threadOpen}
          canToggleSeen={!!row && writable}
          canOpenThread={!!row && s.snapshot.phase === "online"}
          onToggleSeen={() => void controller.toggleSelected("seen")}
          onOpenThread={() => void controller.openThread()}
        />
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
          <div
            className="letter-content"
            onContextMenu={(e) => {
              // The reply area holds the editor and its own controls.
              if ((e.target as Element).closest(".reply-section")) return;
              menu.offer(e, () => messageMenu(row, menuDeps, true), null);
            }}
          >
            <div className="letter-view" key={messageKey(row)}>
            <header className="letter-heading">
              <h2>{row.subject || m.studio_message_no_subject()}</h2>
              <div className="letter-sender">
                <div
                  onContextMenu={(e) =>
                    menu.offer(e, () =>
                      addressMenu(
                        row.sender,
                        {
                          compose: (address) => void composer.create(address),
                          copy: actions.copy,
                          findSender: (address) => actions.search(address, true),
                        },
                        writable,
                      ),
                    )
                  }
                >
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
            </div>
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
