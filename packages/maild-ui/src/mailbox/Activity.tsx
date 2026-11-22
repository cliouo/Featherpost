import { useEffect, useState } from "react";
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
import { fullDate, isoDateTime } from "./format";
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
  const s = useStore(controller.store),
    mail = useStore(mailbox.store),
    [decision, setDecision] = useState<{
      op: Operation;
      resolution: "retry" | "discard";
    } | null>(null);
  useEffect(() => {
    document.title=`同步活动 · ${BRAND.productName}`;
    controller.show();
    return () => controller.hide();
  }, [controller]);
  return (
    <section className="page activity-page">
      <header className="activity-heading" data-tauri-drag-region="deep">
        <div>
          <h1>同步活动</h1>
          <p>本机已接受的操作，以及服务器返回的结果。</p>
        </div>
        <button
          className="text-button"
          disabled={!s.online || s.loading || s.busy !== null}
          onClick={() => void controller.refresh()}
        >
          刷新
        </button>
      </header>
      <div className="activity-filter">
        <label>
          账户{" "}
          <Choice
            value={s.account}
            disabled={s.busy !== null}
            onValueChange={(value) => controller.filter(value)}
          >
            <option value="">全部账户</option>
            {mail.snapshot.accounts.map((a) => (
              <option key={a.account} value={a.account}>
                {mailbox.label(a.account)}
              </option>
            ))}
          </Choice>
        </label>
        <span>按提交顺序倒序，含成功、失败与待核对项</span>
      </div>
      <div className="page-scroll">
        <div className="page-column">
          {!s.online && (
            <p className="list-banner">
              后台连接不可用，恢复后可刷新。邮箱服务器离线时，本机操作会等待同步。
            </p>
          )}
          {s.error && (
            <p role="alert" className="list-banner">
              {s.error}
            </p>
          )}
          {!s.ops.length && !s.loading && !s.error && (
            <div className="empty">
              <h2>还没有同步操作</h2>
              <p>标记、归档、移动、删除与已发送副本会出现在这里。</p>
            </div>
          )}
          <ol className="operation-list">
            {s.ops.map((op) => {
              const known =
                ["flag", "move", "append"].includes(op.kind) &&
                ["pending", "in_flight", "done", "failed", "unknown"].includes(
                  op.state,
                );
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
                      {discarded ? "已停止重试" : stateLabel(op.state)}
                    </span>
                  </div>
                  <p>
                    {mailbox.label(op.account)} · {op.folder}
                    {op.kind === "move" && op.target ? ` → ${op.target}` : ""}
                    {op.kind === "append" ? "" : ` · 邮件 ${op.uid}`}
                  </p>
                  {op.subject!==undefined&&op.subject!==null&&<p className="operation-subject" title={op.subject}>{op.subject||'（无主题）'}</p>}
                  <div className="operation-meta">
                    <time dateTime={isoDateTime(op.created_at)}>
                      {fullDate(op.created_at)}
                    </time>
                    <span>尝试 {op.attempts} 次</span>
                  </div>
                  {!known && (
                    <p>
                      未知类型：{op.kind} / {op.state}。请更新客户端后处理。
                    </p>
                  )}
                  {op.state === "unknown" && (
                    <p className="operation-warning">
                      命令已发出，但没有收到最终结果。请检查目标文件夹，重试可能产生重复副本。
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
                        查看目标文件夹
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
                          重试
                        </button>
                        <button
                          className="text-button"
                          disabled={!s.online || s.busy !== null}
                          onClick={() =>
                            setDecision({ op, resolution: "discard" })
                          }
                        >
                          不再重试
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
                ? "读取中…"
                : `已加载 ${s.ops.length} 项${s.ops.length > 40 ? " · 查看历史时自动刷新暂停" : ""}`}
            </span>
            {s.more && (
              <button
                className="text-button"
                disabled={!s.online || s.loading || s.busy !== null}
                onClick={() => void controller.refresh(true)}
              >
                加载更早的操作
              </button>
            )}
          </footer>
        </div>
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
            {mailbox.label(decision.op.account)} · {operationLabel(decision.op)}{" "}
            · {decision.op.folder}
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
