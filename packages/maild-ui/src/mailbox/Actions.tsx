import { folderLabel } from "../i18n";
import { Choice } from "../settings/Choice";
import { useState } from "react";
import { useStore } from "../host/hooks";
import { Dialog } from "./Dialog";
import type { MailboxController } from "./controller";
export function Actions({
  controller,
  openActivity,
}: {
  controller: MailboxController;
  openActivity: () => void;
}) {
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
        <div className="batch-toolbar" aria-label="批量邮件操作">
          <strong>已选 {s.checked.length} 封</strong>
          <button
            className="text-button"
            disabled={s.busy}
            onClick={() => controller.selectLoaded(false)}
          >
            取消选择
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
            标为已读
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
            未读
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
            加星
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => void controller.perform({ action: "archive" })}
          >
            归档
          </button>
          <button
            className="text-button"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => controller.requestAction("move")}
          >
            移动
          </button>
          <button
            className="text-button danger-text"
            disabled={s.busy || s.snapshot.phase !== "online"}
            onClick={() => controller.requestAction("delete")}
          >
            删除
          </button>
        </div>
      )}
      {s.report && (
        <div
          className={`batch-report${s.report.unconfirmed ? " warn" : ""}`}
          role="status"
        >
          <span>
            {s.report.queued} 封已排队
            {s.report.unchanged ? `，${s.report.unchanged} 封已是目标状态` : ""}
            {s.report.unconfirmed ? `，${s.report.unconfirmed} 封未确认` : ""}。
            {s.report.error || "服务器同步结果可在活动页查看。"}
          </span>
          <button className="text-button" onClick={openActivity}>
            同步活动
          </button>
          <button
            className="text-button"
            onClick={() => controller.dismissReport()}
          >
            关闭
          </button>
        </div>
      )}
      {prompt && (
        <Dialog
          title={
            prompt.kind === "delete"
              ? `删除 ${frozen.length} 封邮件`
              : `移动 ${frozen.length} 封邮件`
          }
          busy={s.busy}
          onClose={() => controller.closePrompt()}
        >
          <ul className="action-message-list">
            {frozen.map((r) => (
              <li key={JSON.stringify([r.account, r.folder, r.uid])}>
                <strong>{r.subject || "（无主题）"}</strong>
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
                    {controller.label(r.account)} / {r.folder}：
                    {trash && trash.name !== r.folder
                      ? `移至 ${folderLabel(trash)}。`
                      : "标记为已删除，由服务器按邮件协议处理；本应用不提供撤销。"}
                  </p>
                );
              })}
              <p className="group-note">
                实际策略以后台入队时的文件夹映射为准。服务器离线时操作会等待同步。
              </p>
            </div>
          ) : (
            <label className="move-target">
              目标文件夹
              <Choice
                aria-label="目标文件夹"
                value={targets.some((f) => f.name === target) ? target : ""}
                onValueChange={setTarget}
              >
                <option value="">请选择</option>
                {targets.map((f) => (
                  <option key={f.name} value={f.name}>
                    {folderLabel(f)}
                  </option>
                ))}
              </Choice>
              {targets.length === 0 && (
                <span>该账户暂时没有其他可用文件夹。</span>
              )}
            </label>
          )}
          <footer>
            <button
              className="text-button"
              disabled={s.busy}
              onClick={() => controller.closePrompt()}
            >
              取消
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
                ? "正在提交"
                : prompt.kind === "delete"
                  ? "确认删除"
                  : "移动到此文件夹"}
            </button>
          </footer>
        </Dialog>
      )}
    </>
  );
}
