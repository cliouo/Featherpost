import { Thread } from "./Thread";
import { Editor } from "../compose/Editor";
import { sourceKey, type ComposeController } from "../compose/controller";
import { useEffect, useMemo, useRef, useState } from "react";
import { useDarkShell } from "../app/theme";
import { detectOtpCode } from "./otp";

import { Icon } from "../app/Icon";
import { useExternal, useStore } from "../host/hooks";
import type { Host } from "../host/host";
import type { CommandContribution } from "../host/types";
import type { MailboxController } from "./controller";
import type { DocumentController } from "./document";
import { presentation, truncationNote } from "./document";
import { addressOnly, displayName, fullDate, initials } from "./format";
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
  return (
    <button
      type="button"
      className={iconOnly ? "icon-button" : "menu-item"}
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
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (
        event instanceof MouseEvent &&
        ref.current?.contains(event.target as Node)
      )
        return;
      setOpen(false);
    };
    // 捕获阶段：工具栏是拖动区，Tauri drag.js 会在冒泡阶段 stopImmediatePropagation。
    document.addEventListener("mousedown", close, true);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close, true);
      document.removeEventListener("keydown", close);
    };
  }, [open]);
  if (commands.length === 0) return null;
  return (
    <div className="overflow" ref={ref}>
      <button
        type="button"
        className="icon-button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="更多操作"
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="more" />
      </button>
      {open && (
        <div className="menu" role="menu" data-tauri-drag-region="false">
          {commands.map((command) => (
            <div
              key={command.id}
              role="menuitem"
              onClick={() => setOpen(false)}
            >
              <CommandButton host={host} command={command} iconOnly={false} />
            </div>
          ))}
        </div>
      )}
    </div>
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
    <section className="reader" aria-label="邮件正文">
      <div className="reader-toolbar" data-tauri-drag-region="deep">
        <button
          type="button"
          className="icon-button back-to-list"
          aria-label="返回列表"
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
              <h2>选择一封邮件</h2>
              <p>左侧列表里的邮件会在这里原位显示。</p>
            </div>
          </div>
        </div>
      )}
      {state.selected && row && (
        <div className="reader-stack">
          <Thread controller={controller} />
          <div className="reader-header">
            <div className="reader-inner">
              <div className="reader-path">
                <span className="source-chip">
                  {controller.label(row.account)}
                </span>
                <span aria-hidden="true">·</span>
                <span>
                  {controller
                    .foldersOf(row.account)
                    .find((f) => f.name === row.folder)?.display_name ??
                    row.folder}
                </span>
              </div>
              <h1>{row.subject || "（无主题）"}</h1>
              <div className="sender-row">
                <span className="avatar" aria-hidden="true">
                  {initials(row.sender)}
                </span>
                <div className="sender-info">
                  <strong>{displayName(row.sender)}</strong>
                  <span className="address">{addressOnly(row.sender)}</span>
                  <div className="recipient-line">
                    收件人 {row.recipient || "—"}
                    {detail?.cc && <> · 抄送 {detail.cc}</>}
                  </div>
                </div>
                <time
                  dateTime={new Date(row.sort_at * 1000).toISOString()}
                  title={`发件人声明 ${row.date_at ? fullDate(row.date_at) : "缺失"} · 收到 ${fullDate(row.received_at)}`}
                >
                  {fullDate(row.sort_at)}
                </time>
              </div>
              <div className="flag-line" aria-label="状态">
                {remote && <span className="tag remote-tag">服务器历史</span>}
                {!row.seen && <span className="tag">未读</span>}
                {row.flagged && <span className="tag">已加星标</span>}
                {row.answered && <span className="tag">已回复</span>}
                {row.deleted && <span className="tag">已标记删除</span>}
                {darkShell && detail && body.mode === "html" && (
                  <button
                    type="button"
                    className="appearance-toggle"
                    aria-pressed={adapted}
                    title={
                      adapted
                        ? "恢复发件人原本的白底配色"
                        : "在深色外壳下柔化白底邮件；图片颜色可能略有偏差"
                    }
                    onClick={() =>
                      host.setPreferences({
                        ...preferences,
                        readerAppearance: adapted ? "paper" : "dark-adapted",
                      })
                    }
                  >
                    {adapted ? "原色信纸" : "柔和深色"}
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
                title="邮件正文（隔离显示）"
                src={body.url}
                sandbox=""
                referrerPolicy="no-referrer"
              />
            )}
            {detail && body.mode === "text" && (
              <div className="reader-text">
                <div className="reader-inner">
                  <pre className="letter">{detail.body || "（正文为空）"}</pre>
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
