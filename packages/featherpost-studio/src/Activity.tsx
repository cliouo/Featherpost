import { Choice } from "../../maild-ui/src/settings/Choice";
import { useEffect, useState } from "react";
import { Icon } from "../../maild-ui/src/app/Icon";
import { BRAND } from "../../maild-ui/src/brand";
import { useStore } from "../../maild-ui/src/host/hooks";
import { folderLabel } from "../../maild-ui/src/i18n";
import {
  ActivityController,
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

function operationTitle(op: Operation, targetRole?: string | null) {
  if (op.kind === "move" && targetRole === "archive") return "归档邮件";
  if (op.kind === "move" && targetRole === "trash") return "移至已删除";
  if (op.kind === "flag") {
    const labels: Record<string, [string, string]> = {
      seen: ["标为未读", "标为已读"],
      flagged: ["移除星标", "添加星标"],
      answered: ["取消已回复标记", "标为已回复"],
      deleted: ["取消删除标记", "标记删除"],
    };
    const label = Object.prototype.hasOwnProperty.call(labels, op.flag ?? "")
      ? labels[op.flag!]
      : undefined;
    return label && typeof op.value === "boolean"
      ? label[op.value ? 1 : 0]
      : "更新邮件标记";
  }
  return operationLabel(op);
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
  const s = useStore(controller.store),
    mail = useStore(mailbox.store);
  const [decision, setDecision] = useState<{
    op: Operation;
    resolution: "retry" | "discard";
  } | null>(null);
  useEffect(() => {
    document.title = `同步活动 · ${BRAND.productName}`;
    controller.show();
    return () => controller.hide();
  }, [controller]);
  return (
    <section className="page studio-activity">
      <header className="activity-heading" data-tauri-drag-region="deep">
        <div>
          <h1>同步活动</h1>
          <p>记录本机提交的邮件更改与服务器结果。</p>
        </div>
        <button
          className="activity-refresh"
          disabled={!s.online || s.loading || s.busy !== null}
          onClick={() => void controller.refresh()}
        >
          <Icon name="activity" />
          刷新
        </button>
      </header>
      <div className="activity-toolbar">
        <label className="activity-account-filter">
          账户
          <Choice
            value={s.account}
            disabled={s.busy !== null}
            onValueChange={(value) => controller.filter(value)}
          >
            <option value="">全部账户</option>
            {s.account &&
              !mail.snapshot.accounts.some((a) => a.account === s.account) && (
                <option value={s.account}>
                  {mailbox.label(s.account)}（账户不可用）
                </option>
              )}
            {mail.snapshot.accounts.map((a) => (
              <option key={a.account} value={a.account}>
                {mailbox.label(a.account)}
              </option>
            ))}
          </Choice>
        </label>
        <span>按提交顺序从新到旧</span>
      </div>
      <div className="activity-scroll" aria-busy={s.loading}>
        {!s.online && (
          <p className="activity-banner" role="status">
            后台连接不可用，恢复后可刷新。邮箱服务器离线时，本机操作会等待同步。
          </p>
        )}
        {s.error && (
          <p role="alert" className="activity-banner error">
            {s.error}
          </p>
        )}
        {!s.ops.length && s.loading && (
          <div className="activity-empty" role="status">
            正在读取同步活动…
          </div>
        )}
        {!s.ops.length && s.online && !s.loading && !s.error && (
          <div className="activity-empty">
            <h2>还没有同步操作</h2>
            <p>标记、归档、移动、删除与已发送副本会出现在这里。</p>
          </div>
        )}
        {!!s.ops.length && (
          <>
            <div
              className="activity-columns activity-table-head"
              aria-hidden="true"
            >
              <span className="activity-operation">操作与邮件</span>
              <span className="activity-account">账户与文件夹</span>
              <span className="activity-result">同步结果</span>
              <span className="activity-time">提交时间</span>
            </div>
            <ol className="activity-records" aria-label="同步操作记录">
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
                const discarded =
                  op.state === "failed" && op.detail === "用户放弃";
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
                      ? "未提供邮件主题"
                      : `邮件 ${op.uid}`
                    : op.subject || "（无主题）";
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
                      不再重试
                    </button>
                    <button
                      className={`activity-retry${op.state === "unknown" ? " primary" : ""}`}
                      disabled={!s.online || s.busy !== null}
                      onClick={() => setDecision({ op, resolution: "retry" })}
                    >
                      {op.state === "unknown" ? "核对后重试" : "重试"}
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
                            op.uid != null && <span> · 邮件 {op.uid}</span>}
                        </p>
                      </div>
                      <div className="activity-account">
                        <strong title={mailbox.label(op.account)}>
                          {mailbox.label(op.account)}
                        </strong>
                        {folder ? (
                          <button
                            className="activity-folder-link"
                            title={`查看目标文件夹 · ${path}`}
                            aria-label={`查看目标文件夹 · ${path}`}
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
                            ? "已停止重试"
                            : op.state === "unknown"
                              ? "结果待核对"
                              : stateLabel(
                                  supportedStates.has(op.state) ? op.state : "",
                                )}
                        </span>
                        <small>
                          {op.attempts
                            ? `已尝试 ${op.attempts} 次`
                            : "尚未尝试"}
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
                          先核对{op.kind === "flag" ? "邮件标记" : "目标文件夹"}
                          ，再决定是否重试
                        </strong>
                        <p>
                          操作已提交，但尚未收到服务器的最终结果。
                          {op.kind === "flag"
                            ? "请先检查服务器上的邮件标记。"
                            : "直接重试可能产生重复副本。"}
                        </p>
                        {!known && (
                          <p>
                            未知类型：{op.kind} / {op.state}
                            。请更新客户端后处理。
                          </p>
                        )}
                        <footer>
                          {folder ? (
                            <button className="activity-link" onClick={inspect}>
                              查看目标文件夹
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
                                未知类型：{op.kind} / {op.state}
                                。请更新客户端后处理。
                              </p>
                            )}
                            {op.detail && !discarded && (
                              <p
                                className={op.state === "failed" ? "error" : ""}
                              >
                                {op.detail}
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
              ? "读取中…"
              : `已加载 ${s.ops.length} 项${s.ops.length > 40 ? " · 查看历史时自动刷新暂停" : ""}`}
          </span>
          {s.more && (
            <button
              disabled={!s.online || s.loading || s.busy !== null}
              onClick={() => void controller.refresh(true)}
            >
              加载更早的操作 <span aria-hidden="true">→</span>
            </button>
          )}
        </footer>
      </div>
      {decision && (
        <Dialog
          title={
            decision.resolution === "retry"
              ? "确认重试同步操作"
              : "停止重试此操作"
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
                ? "将重新排队同步这个标记。"
                : "请先核对目标文件夹。若原操作已经成功，重试可能产生第二份副本。"
              : "这只停止要求后台重试，不会撤销服务器已经执行的操作。"}
          </p>
          <footer>
            <button
              className="text-button"
              disabled={s.busy !== null}
              onClick={() => setDecision(null)}
            >
              取消
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
                ? "正在提交"
                : decision.resolution === "retry"
                  ? "已核对，重新排队"
                  : "不再重试"}
            </button>
          </footer>
        </Dialog>
      )}
    </section>
  );
}
