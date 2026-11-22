import { useState } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import { useDarkShell } from "../../maild-ui/src/app/theme";
import { readingDocumentUrl } from "./readingTheme";
import { useStore } from "../../maild-ui/src/host/hooks";
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
import { useContextMenu } from "./ContextMenu";
import { addressMenu, messageMenu } from "./menus";
import { useStudioActions } from "./studioActions";
import { useMailMenu } from "./Workspace";

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
    <section className="reader-column" aria-label="邮件正文">
      <div className="reader-actions">
        <button
          className="icon-button back-button"
          aria-label="返回邮件列表"
          onClick={() => controller.clearSelection()}
        >
          <Icon name="back" />
        </button>
        <button
          className="text-button"
          title="归档"
          aria-label="归档"
          disabled={!row || !writable}
          onClick={() => void controller.perform({ action: "archive" })}
        >
          <Icon name="archive" />
          <span>归档</span>
        </button>
        <button
          className="icon-button"
          title="删除"
          aria-label="删除"
          disabled={!row || !writable}
          onClick={() => controller.requestAction("delete")}
        >
          <Icon name="trash" />
        </button>
        <span className="toolbar-divider" />
        <button
          className="text-button"
          title="移动到文件夹"
          aria-label="移动到文件夹"
          disabled={!row || !writable}
          onClick={() => controller.requestAction("move")}
        >
          <Icon name="folder" />
          <span>移动到</span>
        </button>
        <button
          className={`icon-button ${row?.flagged ? "starred" : ""}`}
          title="切换星标"
          aria-label="切换星标"
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
            aria-label="更多邮件操作"
            title="更多邮件操作"
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
              {row?.seen ? "标为未读" : "标为已读"}
            </button>
            <button
              disabled={!row || s.snapshot.phase !== "online"}
              onClick={(e) => {
                e.currentTarget.closest("details")?.removeAttribute("open");
                void controller.openThread();
              }}
            >
              {s.threadOpen ? "收起会话" : "查看会话"}
            </button>
          </div>
        </details>
        <span className="spacer" />
        {dark && row && body.mode === "html" && (
          <button
            className="text-button reading-theme"
            title={originalColors ? "切换到深色正文" : "查看邮件的原始配色"}
            onClick={() =>
              setOriginalFor(originalColors ? null : messageKey(row))
            }
          >
            {originalColors ? "深色阅读" : "原始配色"}
          </button>
        )}
        <span className="reader-position">
          {index >= 0 ? `${index + 1} / ${navigationTotal}` : row ? "会话" : ""}
        </span>
        <button
          className="icon-button previous-mail"
          title="上一封"
          aria-label="上一封"
          disabled={!row || index <= 0}
          onClick={() => controller.step(-1)}
        >
          <Icon name="back" />
        </button>
        <button
          className="icon-button next-mail"
          title="下一封"
          aria-label="下一封"
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
          <h2>选择一封邮件</h2>
          <p>在这里阅读和回复</p>
          <div className="empty-shortcuts">
            <span>
              <kbd>⌘ N</kbd> 写邮件
            </span>
            <span>
              <kbd>⌘ K</kbd> 搜索
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
            <header className="letter-heading">
              <h2>{row.subject || "（无主题）"}</h2>
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
                    发至 {row.recipient || "—"}
                    {detail?.cc ? ` · 抄送 ${detail.cc}` : ""}
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
                正在读取正文…
              </div>
            ) : detail && body.mode === "html" ? (
              <iframe
                className="studio-mail-frame"
                key={body.token}
                src={readingDocumentUrl(body.url, dark && !originalColors)}
                sandbox=""
                referrerPolicy="no-referrer"
                title="邮件正文（隔离显示）"
              />
            ) : (
              detail && (
                <>
                  <pre className="studio-letter">
                    {detail.body || "（正文为空）"}
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
            <section className="reply-section" aria-label="回复邮件">
              <p className="reply-identity">
                使用 {controller.label(row.account)} ·{" "}
                {profile?.address || row.account}
              </p>
              {!inline && (
                <div className="reply-bar">
                  <button
                    className="primary-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "reply")}
                  >
                    <Icon name="reply" />
                    回复
                  </button>
                  <button
                    className="text-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "reply_all")}
                  >
                    <Icon name="reply" />
                    回复全部
                  </button>
                  <button
                    className="text-button"
                    disabled={!writable}
                    onClick={() => void composer.reply(row, "forward")}
                  >
                    <Icon name="forward" />
                    转发
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
