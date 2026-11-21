import { useEffect, useState } from "react";
import type { Host } from "../../maild-ui/src/host/host";
import type { Bundle } from "../../maild-ui/src/host/types";
import { useStore } from "../../maild-ui/src/host/hooks";
import { Icon } from "../../maild-ui/src/app/Icon";
import {
  MailboxController,
  messageKey,
  scopeTitle,
} from "../../maild-ui/src/mailbox/controller";
import {
  DocumentController,
  presentation,
} from "../../maild-ui/src/mailbox/document";
import { ActivityController } from "../../maild-ui/src/mailbox/activityController";
import { Activity } from "../../maild-ui/src/mailbox/Activity";
import { Actions } from "../../maild-ui/src/mailbox/Actions";
import { ReadingPrivacySection } from "../../maild-ui/src/mailbox/ReadingPrivacy";
import {
  AttachmentStrip,
  LinkSheet,
  OtpBar,
  RemoteBanner,
} from "../../maild-ui/src/mailbox/ReaderParts";
import {
  displayName,
  addressOnly,
  fullDate,
  initials,
  shortDate,
} from "../../maild-ui/src/mailbox/format";
import { detectOtpCode } from "../../maild-ui/src/mailbox/otp";
import { followMailIntent } from "../../maild-ui/src/mailbox/navigation";
import { compatBlocksWrites } from "../../maild-ui/src/bridge/types";
import type { ComposeController } from "../../maild-ui/src/compose/controller";
import { Editor } from "../../maild-ui/src/compose/Editor";
import { sourceKey } from "../../maild-ui/src/compose/controller";

export const MAIL = "maild.mailbox.workspace";
export const ACTIVITY = "studio.activity";

export function studioMailbox(
  host: Host,
  composer: ComposeController,
  setup: () => void,
) {
  const controller = new MailboxController(host.bridge, (selection) =>
    host.setSelection(selection),
  );
  const documents = new DocumentController(host.bridge);
  const activity = new ActivityController(host.bridge.mailbox);
  const stopDocuments = documents.bindTo(controller.store);
  void documents.start();
  const sync = () => {
    const s = host.state.getSnapshot();
    controller.acceptSnapshot(s.snapshot);
    activity.setConnection(
      s.snapshot.phase === "online",
      s.snapshot.connection_epoch,
    );
  };
  const stopState = host.state.subscribe(sync);
  const stopIntent = host.onIntent((intent) => {
    if (intent.route === "background") host.openView(ACTIVITY);
    if (intent.route === "inbox" || intent.route === "message")
      void followMailIntent(
        controller,
        intent,
        () => composer.flush(),
        () => host.openView(MAIL),
        (text) => host.notify(text),
        () => host.approveNavigation(MAIL),
      );
  });
  function Workspace() {
    return (
      <Mailbox
        host={host}
        controller={controller}
        documents={documents}
        composer={composer}
        setup={setup}
      />
    );
  }
  function ActivityView() {
    return (
      <Activity
        controller={activity}
        mailbox={controller}
        openMailbox={() => host.openView(MAIL)}
      />
    );
  }
  function Privacy() {
    return (
      <ReadingPrivacySection
        host={host}
        onChanged={() => controller.reloadSelected()}
      />
    );
  }
  const bundle: Bundle = {
    moduleId: "featherpost.studio.mailbox",
    apiVersion: 1,
    title: "Studio 邮箱",
    views: [
      { id: MAIL, title: "邮件", role: "mailbox", render: Workspace },
      {
        id: ACTIVITY,
        title: "同步活动",
        role: "activity",
        render: ActivityView,
      },
    ],
    settings: [
      { id: "studio.reading", title: "阅读与隐私", order: 15, render: Privacy },
    ],
    dispose: () => {
      stopState();
      stopIntent();
      stopDocuments();
      documents.stop();
      controller.stop();
      activity.hide();
    },
  };
  return { controller, bundle, setup };
}

function Mailbox({
  host,
  controller,
  documents,
  composer,
  setup,
}: {
  host: Host;
  controller: MailboxController;
  documents: DocumentController;
  composer: ComposeController;
  setup(): void;
}) {
  const s = useStore(controller.store);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<"all" | "unread">("all");
  const title = scopeTitle(s.scope).replace("统一收件箱", "收件箱");
  const rows = filter === "unread" ? s.rows.filter((r) => !r.seen) : s.rows;
  useEffect(() => {
    setFilter("all");
    if (s.scope.kind !== "search") setSearch("");
  }, [s.scope]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (
        e.metaKey ||
        e.ctrlKey ||
        e.altKey ||
        document.querySelector('dialog[open], [aria-modal="true"]') ||
        (e.target instanceof HTMLElement &&
          e.target.closest('input,textarea,select,[contenteditable="true"]'))
      )
        return;
      if (e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault();
        controller.step(1);
      }
      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        controller.step(-1);
      }
      if (e.key === "Escape") controller.clearSelection();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [controller]);
  return (
    <section className="mail-view">
      <header className="workspace-header" data-tauri-drag-region="deep">
        <div>
          <h1>{title}</h1>
          <span className="mail-count">{s.total} 封邮件</span>
        </div>
        <form
          className="studio-search"
          role="search"
          onSubmit={(e) => {
            e.preventDefault();
            if (search.trim()) controller.search({ query: search.trim() });
            else controller.exitSearch();
          }}
        >
          <Icon name="search" />
          <input
            data-mail-search
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索邮件"
            aria-label="搜索邮件"
          />
          <kbd>⌘ K</kbd>
          {s.scope.kind === "search" && (
            <button
              type="button"
              aria-label="退出搜索"
              onClick={() => controller.exitSearch()}
            >
              ×
            </button>
          )}
        </form>
        <button
          className={`icon-button ${s.syncing ? "spinning" : ""}`}
          title="立即同步"
          aria-label="立即同步"
          disabled={s.snapshot.phase !== "online" || s.syncing}
          onClick={() => void controller.syncNow()}
        >
          <Icon name="refresh" />
        </button>
      </header>
      <Actions
        controller={controller}
        openActivity={() => host.openView(ACTIVITY)}
      />
      {s.syncError && (
        <div className="notice error" role="alert">
          {s.syncError}
        </div>
      )}
      <div className={`mail-columns ${s.selected ? "has-selection" : ""}`}>
        <section className="message-column" aria-label="邮件列表">
          <div className="list-controls">
            <div className="filter-tabs" aria-label="筛选邮件">
              <button
                className={filter === "all" ? "selected" : ""}
                aria-pressed={filter === "all"}
                onClick={() => setFilter("all")}
              >
                全部
              </button>
              <button
                className={filter === "unread" ? "selected" : ""}
                aria-pressed={filter === "unread"}
                onClick={() => setFilter("unread")}
              >
                未读
              </button>
            </div>
            <span>最新优先</span>
          </div>
          <div className="message-scroll" aria-busy={s.loadingRows}>
            {s.rowsError && (
              <div className="notice error" role="alert">
                {s.rowsError}
                <button onClick={() => void controller.refresh()}>重试</button>
              </div>
            )}
            {s.folderError && (
              <div className="notice error" role="alert">
                {s.folderError}
              </div>
            )}
            {s.loadingRows && !rows.length && (
              <div className="loading-mail" role="status">
                正在读取邮件…
              </div>
            )}
            {!s.loadingRows && !rows.length && (
              <div className="empty-list">
                <Icon name="inbox" />
                <h2>
                  {s.scope.kind === "search"
                    ? "没有找到邮件"
                    : "这里暂时没有邮件"}
                </h2>
                <p>
                  {s.scope.kind === "search"
                    ? "试试其他关键词"
                    : "新邮件同步后会出现在这里"}
                </p>
                {s.scope.kind === "search" && controller.canSearchServer && (
                  <button
                    className="text-button"
                    onClick={() => void controller.searchServer()}
                  >
                    搜索服务器历史邮件
                  </button>
                )}
              </div>
            )}
            {rows.map((row, i) => {
              const selected =
                s.selected && messageKey(s.selected) === messageKey(row);
              return (
                <article
                  className={`message-row ${selected ? "selected" : ""} ${!row.seen ? "unread" : ""}`}
                  key={messageKey(row)}
                >
                  <input
                    className="row-check"
                    type="checkbox"
                    aria-label={`选择 ${row.subject || "无主题邮件"}`}
                    checked={s.checked.some(
                      (r) => messageKey(r) === messageKey(row),
                    )}
                    onChange={() => controller.toggle(row)}
                  />
                  <button
                    className="message-open"
                    aria-current={selected ? "true" : undefined}
                    onClick={() => controller.selectMessage(row)}
                  >
                    <span className={`sender-avatar color-${i % 4}`}>
                      {initials(row.sender)}
                    </span>
                    <span className="row-copy">
                      <span className="row-top">
                        <strong>{displayName(row.sender) || row.sender}</strong>
                        <time>{shortDate(row.sort_at)}</time>
                      </span>
                      <span className="row-subject">
                        {!row.seen && <span className="unread-dot" />}
                        {row.subject || "（无主题）"}
                      </span>
                      <span className="row-snippet">
                        {row.snippet || "（无预览）"}
                      </span>
                      <span className="row-meta">
                        {controller.label(row.account)}
                        {row.has_attachments && <Icon name="paperclip" />}
                        {row.flagged && <Icon name="star" />}
                      </span>
                    </span>
                  </button>
                </article>
              );
            })}
            {controller.canLoadMore && (
              <button
                className="load-more"
                disabled={s.loadingRows}
                onClick={() => void controller.loadMore()}
              >
                加载更多邮件
              </button>
            )}
            {s.scope.kind === "search" &&
              rows.length > 0 &&
              controller.canSearchServer && (
                <button
                  className="load-more"
                  disabled={s.remote.status === "loading"}
                  onClick={() => void controller.searchServer()}
                >
                  搜索服务器历史邮件
                </button>
              )}
            {s.remote.error && (
              <div className="notice error">{s.remote.error}</div>
            )}
            {s.remote.rows.length > 0 && (
              <>
                <p className="section-label">服务器历史</p>
                {s.remote.rows.map((row) => (
                  <button
                    className="remote-row"
                    key={messageKey(row)}
                    onClick={() => controller.openRemote(row)}
                  >
                    <strong>{row.subject || "（无主题）"}</strong>
                    <span>{row.sender}</span>
                  </button>
                ))}
              </>
            )}
          </div>
          <footer className="list-footer">
            <span>
              {s.loadingRows ? "正在更新…" : `已载入 ${s.rows.length} 封`}
            </span>
            <span>↑ ↓ 切换邮件</span>
          </footer>
        </section>
        {s.snapshot.phase === "online" && !s.snapshot.accounts.length ? (
          <section className="reader-column welcome-studio">
            <div className="welcome-emblem">
              <Icon name="sent" />
            </div>
            <p className="eyebrow">FEATHERPOST STUDIO</p>
            <h2>给邮件，留一点空间。</h2>
            <p>
              连接你的邮箱，从一封邮件开始。
              <br />
              账户、邮件与草稿，依然保存在本机。
            </p>
            <button className="primary-button" onClick={setup}>
              连接邮箱 <span>→</span>
            </button>
            <span className="welcome-footnote">
              支持 IMAP / SMTP 与邮件服务插件
            </span>
          </section>
        ) : (
          <StudioReader
            host={host}
            controller={controller}
            documents={documents}
            composer={composer}
          />
        )}
      </div>
    </section>
  );
}

function StudioReader({
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
  const writable =
    s.snapshot.phase === "online" && !compatBlocksWrites(s.snapshot) && !s.busy;
  const otp = row
    ? detectOtpCode(row.subject, detail?.body || row.snippet)
    : null;
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
        <div className="action-group">
          <button
            className="icon-button"
            title="归档"
            aria-label="归档"
            disabled={!row || !writable}
            onClick={() => void controller.perform({ action: "archive" })}
          >
            <Icon name="archive" />
          </button>
          <button
            className="icon-button"
            title="移动到文件夹"
            aria-label="移动到文件夹"
            disabled={!row || !writable}
            onClick={() => controller.requestAction("move")}
          >
            <Icon name="folder" />
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
        </div>
        <span className="toolbar-divider" />
        <button
          className="icon-button"
          title="切换已读状态"
          aria-label="切换已读状态"
          disabled={!row || !writable}
          onClick={() => void controller.toggleSelected("seen")}
        >
          <Icon name="mail" />
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
        <span className="spacer" />
        <button
          className="icon-button"
          title="上一封"
          aria-label="上一封"
          disabled={!row}
          onClick={() => controller.step(-1)}
        >
          ↑
        </button>
        <button
          className="icon-button"
          title="下一封"
          aria-label="下一封"
          disabled={!row}
          onClick={() => controller.step(1)}
        >
          ↓
        </button>
      </div>
      {!row ? (
        <div className="reader-empty">
          <div className="empty-envelope">
            <Icon name="mail" />
          </div>
          <h2>一封一封，慢慢来。</h2>
          <p>选择左侧邮件，在这里阅读</p>
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
        <>
          <div className="letter-scroll">
            <div className="letter-heading">
              <div className="letter-location">
                <span>{controller.label(row.account)}</span>
                <span>/</span>
                <span>{row.folder}</span>
              </div>
              <h2>{row.subject || "（无主题）"}</h2>
              <div className="letter-sender">
                <span className="sender-avatar color-0">
                  {initials(row.sender)}
                </span>
                <div>
                  <strong>{displayName(row.sender)}</strong>
                  <span className="sender-address">
                    {addressOnly(row.sender)}
                  </span>
                  <p>
                    发送至 {row.recipient || "—"}
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
            </div>
            {s.loadingDetail || body.mode === "loading" ? (
              <div className="body-loading" role="status">
                正在读取正文…
              </div>
            ) : detail && body.mode === "html" ? (
              <iframe
                className="studio-mail-frame"
                key={body.token}
                src={body.url}
                sandbox=""
                referrerPolicy="no-referrer"
                title="邮件正文（隔离显示）"
              />
            ) : (
              detail && (
                <pre className="studio-letter">
                  {detail.body || "（正文为空）"}
                </pre>
              )
            )}
            {detail && (
              <AttachmentStrip
                bridge={host.bridge}
                detail={detail}
                notify={(text) => host.notify(text)}
              />
            )}
            {s.threadOpen && (
              <div className="thread-list">
                {s.threadError && <p role="alert">{s.threadError}</p>}
                {s.threadRows.map((r) => (
                  <button
                    key={messageKey(r)}
                    onClick={() => controller.selectMessage(r, true)}
                  >
                    {displayName(r.sender)}
                    <strong>{r.subject}</strong>
                    <span>{shortDate(r.sort_at)}</span>
                  </button>
                ))}
                {s.threadRows.length < s.threadTotal && (
                  <button onClick={() => void controller.moreThread()}>
                    加载更多会话
                  </button>
                )}
              </div>
            )}
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
                className="secondary-button"
                disabled={!writable}
                onClick={() => void composer.reply(row, "reply_all")}
              >
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
              <span className="spacer" />
              <button
                className="text-button"
                onClick={() => void controller.openThread()}
              >
                {s.threadOpen ? "收起会话" : "查看会话"}
              </button>
            </div>
            {compose.inline && compose.source === sourceKey(row) && (
              <Editor controller={composer} inline />
            )}
          </div>
        </>
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
