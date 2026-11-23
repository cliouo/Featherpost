import { m } from "@featherpost/i18n/messages";

import { folderLabel, useUiLocale } from "../i18n";
import { Thread } from "./Thread";
import { Editor } from "../compose/Editor";
import { sourceKey, type ComposeController } from "../compose/controller";
import { useMemo, useRef, useState } from "react";
import { PopoverMenu } from "../app/PopoverMenu";
import { useDarkShell } from "../app/theme";
import { detectOtpCode } from "./otp";

import { Icon } from "../app/Icon";
import { useExternal, useStore } from "../host/hooks";
import type { Host } from "../host/host";
import type { CommandContribution } from "../host/types";
import type { MailboxController } from "./controller";
import { messageKey } from "./controller";
import type { DocumentController } from "./document";
import { presentation, truncationNote } from "./document";
import { addressOnly, displayName, fullDate, initials, isoDateTime } from "./format";
import { AttachmentStrip, LinkSheet, OtpBar, RemoteBanner } from "./ReaderParts";

function CommandButton({
  host,
  command,
  iconOnly,
}: {
  host: Host;
  command: CommandContribution;
  iconOnly: boolean;
}) {
  const enabled = host.isEnabled(command.enabledWhen);
  const danger = command.id === "maild.mailbox.delete" ? " danger-action" : "";
  return (
    <button
      type="button"
      className={`${iconOnly ? "icon-button" : "menu-item"}${danger}`}
      title={iconOnly ? command.title : undefined}
      aria-label={iconOnly ? command.title : undefined}
      disabled={!enabled}
      onClick={() => host.execute(command.id)}
    >
      {command.icon && <Icon name={command.icon} />}
      {!iconOnly && <span>{command.title}</span>}
    </button>
  );
}

function OverflowMenu({
  host,
  commands,
}: {
  host: Host;
  commands: CommandContribution[];
}) {
  useUiLocale();
  const [open, setOpen] = useState(false);
  const openRef = useRef(open);
  openRef.current = open;
  if (commands.length === 0) return null;
  return (
    <PopoverMenu
      className="overflow"
      triggerClassName="icon-button"
      panelClassName="menu"
      panelRole="menu"
      open={open}
      onOpenChange={setOpen}
      triggerLabel={m.mailbox_reader_more_actions()}
      triggerTitle={m.mailbox_reader_more_actions()}
      trigger={<Icon name="more" />}
    >
      {commands.map((command) => (
        <div
          key={command.id}
          role="menuitem"
          onClick={() => {
            if (openRef.current) setOpen(false);
          }}
        >
          <CommandButton host={host} command={command} iconOnly={false} />
        </div>
      ))}
    </PopoverMenu>
  );
}

interface ReaderProps {
  host: Host;
  controller: MailboxController;
  documents: DocumentController;
  composer?: ComposeController;
}

/**
 * 阅读区（ADR-0033 决策 7）：可信头部不滚动；正文区占满剩余高度——HTML 在 sandbox iframe 里自己滚，
 * 纯文本在 `<pre>` 里滚；附件条固定在底部。iframe 背景默认恒白：邮件在深色外壳下保持原色；
 * 用户可在深色外壳下改用「柔和深色」——只在宿主侧给隔离容器加滤镜，不碰 CSP 与 sandbox。
 */
export function Reader({ host, controller, documents, composer }: ReaderProps) {
  useUiLocale();
  const state = useStore(controller.store);
  const docState = useStore(documents.store);
  const registry = useExternal(
    host.registry.subscribe,
    host.registry.getSnapshot,
  );
  // 命令的 enabledWhen 依赖宿主状态（选中、在线）；订阅一次让按钮随之刷新。
  const { preferences } = useStore(host.state);
  const darkShell = useDarkShell(preferences.theme);
  const toolbar = registry.commands
    .filter(({ value }) => value.locations.includes("toolbar"))
    .map(({ value }) => value);
  const menu = registry.commands
    .filter(({ value }) => value.locations.includes("menu"))
    .map(({ value }) => value);
  const detail = state.detail;
  const row = state.selectedRow;
  const body = presentation(detail, docState);
  const truncated = truncationNote(docState);
  const otp = useMemo(
    () => (row ? detectOtpCode(row.subject, detail?.body || row.snippet) : null),
    [row?.subject, row?.snippet, detail?.body],
  );
  const adapted =
    darkShell && preferences.readerAppearance === "dark-adapted";
  const remote = row ? controller.isRemote(row) : false;

  return (
    <section className="reader" aria-label={m.mailbox_reader_label()}>
      <div className="reader-toolbar" data-tauri-drag-region="deep">
        <button
          type="button"
          className="icon-button back-to-list"
          title={m.mailbox_reader_back()}
          aria-label={m.mailbox_reader_back()}
          onClick={() => controller.clearSelection()}
        >
          <Icon name="back" />
        </button>
        {toolbar.map((command) => (
          <CommandButton
            key={command.id}
            host={host}
            command={command}
            iconOnly
          />
        ))}
        <span className="spacer" />
        <OverflowMenu host={host} commands={menu} />
      </div>
      {!state.selected && (
        <div className="reader-scroll">
          <div className="reader-inner">
            <div className="empty">
              <Icon name="inbox" />
              <h2>{m.mailbox_reader_empty_title()}</h2>
              <p>{m.mailbox_reader_empty_body()}</p>
            </div>
          </div>
        </div>
      )}
      {state.selected && row && (
        <div className="reader-stack">
          <Thread controller={controller} />
          <div className="reader-header">
            <div className="reader-inner reader-fade" key={messageKey(row)}>
              <div className="reader-path">
                <span className="source-chip">
                  {controller.label(row.account)}
                </span>
                <span aria-hidden="true">·</span>
                <span>
                  {folderLabel(controller.foldersOf(row.account).find((f) => f.name === row.folder), row.folder)}
                </span>
              </div>
              <h1>{row.subject || m.mailbox_subject_empty()}</h1>
              <div className="sender-row">
                <span className="avatar" aria-hidden="true">
                  {initials(row.sender)}
                </span>
                <div className="sender-info">
                  <strong>{displayName(row.sender)}</strong>
                  <span className="address">{addressOnly(row.sender)}</span>
                  <div className="recipient-line">
                    {m.mailbox_reader_to({ recipient: row.recipient || "—" })}
                    {detail?.cc && <> · {m.mailbox_reader_cc({ cc: detail.cc })}</>}
                  </div>
                </div>
                <time
                  dateTime={isoDateTime(row.sort_at)}
                  title={m.mailbox_reader_date_title({
                    declared:
                      row.date_at && row.date_at > 0
                        ? fullDate(row.date_at)
                        : m.mailbox_reader_date_missing(),
                    received:
                      row.received_at > 0
                        ? fullDate(row.received_at)
                        : m.mailbox_reader_date_unknown(),
                  })}
                >
                  {fullDate(row.sort_at)}
                </time>
              </div>
              <div className="flag-line" aria-label={m.mailbox_reader_flags_label()}>
                {remote && <span className="tag remote-tag">{m.mailbox_remote_tag_server()}</span>}
                {!row.seen && <span className="tag">{m.mailbox_reader_tag_unread()}</span>}
                {row.flagged && <span className="tag">{m.mailbox_reader_tag_starred()}</span>}
                {row.answered && <span className="tag">{m.mailbox_reader_tag_answered()}</span>}
                {row.deleted && <span className="tag">{m.mailbox_reader_tag_deleted()}</span>}
                {darkShell && detail && body.mode === "html" && (
                  <button
                    type="button"
                    className="appearance-toggle"
                    aria-pressed={adapted}
                    title={
                      adapted
                        ? m.mailbox_reader_appearance_paper_title()
                        : m.mailbox_reader_appearance_dark_title()
                    }
                    onClick={() =>
                      host.setPreferences({
                        ...preferences,
                        readerAppearance: adapted ? "paper" : "dark-adapted",
                      })
                    }
                  >
                    {adapted ? m.mailbox_reader_appearance_paper() : m.mailbox_reader_appearance_dark()}
                  </button>
                )}
              </div>
              {otp && (
                <OtpBar code={otp} notify={(text) => host.notify(text)} />
              )}
              {state.detailError && (
                <div className="banner error" role="alert">
                  {state.detailError}
                </div>
              )}
              {detail &&
                docState.key &&
                docState.status === "failed" &&
                docState.error && (
                  <div className="banner error" role="alert">
                    {docState.error}
                  </div>
                )}
              {detail && body.mode === "text" && body.note && (
                <p className="body-note">{body.note}</p>
              )}
              {truncated && <p className="body-note">{truncated}</p>}
              {detail && (
                <RemoteBanner
                  host={host}
                  detail={detail}
                  state={docState}
                  documents={documents}
                  onPolicyChanged={() => controller.reloadSelected()}
                />
              )}
            </div>
          </div>
          <div className="reader-body">
            {(state.loadingDetail && !detail) || body.mode === "loading" ? (
              <div className="reader-text" aria-busy="true">
                <div className="reader-inner">
                  <div className="skeleton" />
                  <div className="skeleton short" />
                  <div className="skeleton body" />
                </div>
              </div>
            ) : null}
            {detail && body.mode === "html" && (
              // sandbox="" = 无脚本、不透明 origin、无表单、无弹窗、不能导航顶层（决策 2）。
              // key 用令牌：换文档就换 iframe，绝不在旧文档上换 src。
              <iframe
                key={body.token}
                className={`mail-frame${adapted ? " dark-adapted" : ""}`}
                title={m.mailbox_reader_frame_title()}
                src={body.url}
                sandbox=""
                referrerPolicy="no-referrer"
              />
            )}
            {detail && body.mode === "text" && (
              <div className="reader-text" key={messageKey(row)}>
                <div className="reader-inner">
                  <pre className="letter">{detail.body || m.mailbox_reader_body_empty()}</pre>
                </div>
              </div>
            )}
          </div>
          {detail && (
            <AttachmentStrip
              bridge={host.bridge}
              detail={detail}
              notify={(text) => host.notify(text)}
            />
          )}
          {composer && state.selected && (
            <InlineReply composer={composer} identity={state.selected} />
          )}
        </div>
      )}
      {docState.pendingLink && (
        <LinkSheet
          link={docState.pendingLink}
          documents={documents}
          notify={(text) => host.notify(text)}
          compose={
            composer
              ? (address) => {
                  documents.dismissLink();
                  void composer.create(address);
                }
              : undefined
          }
        />
      )}
    </section>
  );
}

function InlineReply({
  composer,
  identity,
}: {
  composer: ComposeController;
  identity: import("../bridge/types").MessageIdentity;
}) {
  const s = useStore(composer.store);
  return s.inline && s.source === sourceKey(identity) ? (
    <Editor controller={composer} inline />
  ) : null;
}
