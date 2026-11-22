import { useEffect, useRef, useState } from "react";
import { Choice } from "../settings/Choice";
import { Icon } from "../app/Icon";
import { useStore } from "../host/hooks";
import { clockTime } from "../mailbox/format";
import type { ComposeController } from "./controller";
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
  const s = useStore(controller.store),
    root = useRef<HTMLFormElement>(null);
  const [extras, setExtras] = useState(false);
  useEffect(() => {
    setExtras(!!(s.fields?.cc || s.fields?.bcc));
  }, [s.draft?.id]);
  useEffect(() => {
    root.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
  }, [s.errors]);
  if (!s.draft || !s.fields)
    return (
      <div className="empty">
        <h2>写一封邮件</h2>
        <p>从左侧「写邮件」开始，或打开一份草稿。</p>
        <button
          className="primary-button"
          onClick={() => void controller.create()}
        >
          写邮件
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
    ? "正在保存后关闭"
    : s.saving
      ? "正在保存"
      : s.revision !== s.savedRevision
        ? "尚未保存"
        : `已保存到本机${savedClock ? ` · ${savedClock}` : ""}`;
  const field = (
    name: "to" | "cc" | "bcc" | "subject",
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
      onSubmit={(e) => {
        e.preventDefault();
        void controller.send();
      }}
      onKeyDown={(e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
          e.preventDefault();
          void controller.send();
        }
      }}
      aria-label={inline ? "回复编辑器" : "写信编辑器"}
    >
      <div
        className="compose-heading"
        data-tauri-drag-region={inline ? undefined : "deep"}
      >
        <div className="compose-title">
          <Icon name={inline ? "reply" : "drafts"} />
          <h1>{sent ? "邮件已发送" : inline ? "回复邮件" : "写邮件"}</h1>
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
                展开编辑
              </button>
              <button
                type="button"
                className="text-button"
                onClick={() => controller.collapse()}
              >
                {s.collapsed ? "继续写" : "收起"}
              </button>
            </>
          ) : (
            <button
              type="button"
              className="text-button"
              onClick={() => void controller.leave()}
            >
              <Icon name="back" />
              返回阅读
            </button>
          )}
        </div>
      </div>
      {(!inline || !s.collapsed) && (
        <>
          <div className="compose-scroll">
            {sent && (
              <div className="compose-notice success" role="status">
                <strong>邮件已投递。</strong>
                <span>
                  {d.sent_archive === true
                    ? "副本已加入同步队列。"
                    : "副本未加入同步队列，请稍后核对已发送文件夹。"}
                </span>
              </div>
            )}
            {sending && (
              <div className="compose-notice" role="status">
                正在等待投递结果。可以继续阅读邮件，请勿重复发送。
              </div>
            )}
            {unknown && (
              <div className="compose-notice warn" role="alert">
                <strong>投递结果尚未确认</strong>
                <span>
                  请先核对已发送文件夹或向收件人确认，直接重发可能造成重复邮件。
                </span>
                <div className="compose-inline-actions">
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => void controller.reconcile()}
                  >
                    重新查询
                  </button>
                  {d.send_state === "unknown" && (
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => controller.confirm("resolve")}
                    >
                      我已核对，尚未送达
                    </button>
                  )}
                </div>
              </div>
            )}
            {d.send_state === "failed" && (
              <div className="compose-notice warn" role="alert">
                邮件未发出。请检查发件身份、邮箱连接和收件地址，再主动重试。
              </div>
            )}
            {d.has_html && (
              <div className="compose-notice warn">
                <span>
                  这份草稿包含 HTML
                  版式。当前编辑器仅支持纯文本，转换会移除草稿的 HTML 版本。
                </span>
                <button
                  type="button"
                  className="text-button"
                  disabled={
                    s.busy || !["editing", "failed"].includes(d.send_state)
                  }
                  onClick={() => void controller.plainText()}
                >
                  转换为纯文本并编辑
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
                      重试保存
                    </button>
                  )}
                  <button
                    type="button"
                    className="text-button"
                    onClick={() => void controller.copy()}
                  >
                    复制草稿内容
                  </button>
                  {s.conflict && (
                    <button
                      type="button"
                      className="text-button"
                      onClick={() => controller.confirm("reload")}
                    >
                      重新载入已保存版本
                    </button>
                  )}
                </div>
              </div>
            )}
            <div className="compose-field">
              <label htmlFor="compose-from">发件人</label>
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
                      {f.from || f.account}（不可用）
                    </option>
                  )}
                  {s.senders.map((v) => (
                    <option
                      key={JSON.stringify([v.account, v.address])}
                      value={JSON.stringify([v.account, v.address])}
                    >
                      {v.label} · {v.address}
                      {v.available ? "" : "（未配置发信）"}
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
                抄送 / 密送
              </button>
            </div>
            {field("to", "收件人", "邮箱地址，多个地址用逗号分隔")}
            {extras && (
              <>
                {field("cc", "抄送", "可选")}
                {field("bcc", "密送", "其他收件人不会看到这些地址")}
              </>
            )}
            {field("subject", "主题", "给这封邮件写一个主题")}
            <div className="compose-body">
              <label className="visually-hidden" htmlFor="compose-body">
                正文
              </label>
              <textarea
                id="compose-body"
                value={f.body}
                disabled={locked}
                onChange={(e) => controller.change("body", e.target.value)}
                placeholder="从这里开始写…"
                spellCheck
                aria-invalid={!!s.errors.body}
              />
              {s.errors.body && (
                <span className="field-error">{s.errors.body}</span>
              )}
            </div>
            {(d.attachments.length > 0 || s.uploads.length > 0) && (
              <ul className="compose-attachments" aria-label="附件">
                {d.attachments.map((a, i) => (
                  <li
                    key={
                      a.upload_id ??
                      `${a.account}/${a.folder}/${a.uid}/${a.part_index}/${i}`
                    }
                  >
                    <Icon name="paperclip" />
                    <div className="attachment-name">
                      <strong>{a.filename || "未命名附件"}</strong>
                      <small>
                        {size(a.size)} ·{" "}
                        {a.stored ? "已保存到草稿" : "附件字节不可用"}
                      </small>
                    </div>
                    <button
                      type="button"
                      className="text-button"
                      disabled={locked}
                      onClick={() => void controller.detach(i)}
                      aria-label={`移除 ${a.filename}`}
                    >
                      移除
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
                            ? "正在保存到草稿"
                            : `正在准备 ${size(u.offset)} / ${size(u.size)}`)}
                      </small>
                      {u.status === "uploading" && (
                        <progress
                          value={u.offset}
                          max={Math.max(u.size, 1)}
                          aria-label={`${u.filename} 上传进度`}
                        />
                      )}
                    </div>
                    {u.status === "error" && (
                      <button
                        type="button"
                        className="text-button"
                        onClick={() => void controller.upload(u.token)}
                      >
                        重试
                      </button>
                    )}
                    <button
                      type="button"
                      className="text-button"
                      disabled={u.status === "finishing"}
                      onClick={() => void controller.removeUpload(u.token)}
                    >
                      取消
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
                  ? "正在处理"
                  : d.send_state === "failed"
                    ? "重新发送"
                    : "发送"}
              </button>
              {!sent && !unknown && (
                <button
                  type="button"
                  className="text-button"
                  onClick={() => void controller.pick()}
                  disabled={locked || !s.online || !!sender?.simple}
                >
                  <Icon name="paperclip" />
                  添加附件
                </button>
              )}
            </div>
            <span className="compose-hint">
              {inline
                ? ""
                : sent
                  ? ""
                  : s.online
                    ? "⌘ Enter 发送 · 纯文本编辑"
                    : "后台未连接，输入保留在当前窗口"}
            </span>
            <button
              type="button"
              className="icon-button"
              aria-label={sent ? "移除已发送草稿" : "丢弃草稿"}
              title={sent ? "移除已发送草稿" : "丢弃草稿"}
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
function Confirm({ controller }: { controller: ComposeController }) {
  const { confirm } = useStore(controller.store),
    ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, []);
  const labels = {
    subject: ["这封邮件没有主题", "仍然发送"],
    attachment: ["正文提到了附件，但还没有添加附件", "仍然发送"],
    discard: ["丢弃这份草稿？", "丢弃草稿"],
    resolve: ["已确认这封邮件没有送达？", "解锁草稿"],
    reload: ["用已保存版本替换当前输入？", "重新载入"],
  } as const;
  if (!confirm) return null;
  const action = () => {
    const selected = confirm;
    controller.confirm(null);
    if (selected === "discard") void controller.discard();
    else if (selected === "resolve") void controller.resolve();
    else if (selected === "reload") void controller.reload();
    else void controller.send(true);
  };
  return (
    <div className="sheet-backdrop">
      <div
        ref={ref}
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="compose-confirm-title"
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            e.preventDefault();
            controller.confirm(null);
          }
          if (e.key === "Tab") {
            const all = Array.from(
              ref.current!.querySelectorAll<HTMLButtonElement>("button"),
            );
            if (e.shiftKey && document.activeElement === all[0]) {
              e.preventDefault();
              all.at(-1)?.focus();
            } else if (!e.shiftKey && document.activeElement === all.at(-1)) {
              e.preventDefault();
              all[0]?.focus();
            }
          }
        }}
      >
        <h2 id="compose-confirm-title">{labels[confirm][0]}</h2>
        <p className="sheet-lead">
          {confirm === "resolve"
            ? "仅在核对后解锁。解锁不会自动发送。"
            : confirm === "reload"
              ? "当前窗口中尚未保存的输入会被替换。建议先复制草稿内容。"
              : confirm === "discard"
                ? "草稿及其中的新附件会从本机移除，原始文件不会删除。"
                : "确认内容无误后再发送。"}
        </p>
        <div className="sheet-actions">
          <button
            type="button"
            className="text-button"
            onClick={() => controller.confirm(null)}
          >
            返回编辑
          </button>
          <button type="button" className="primary-button" onClick={action}>
            {labels[confirm][1]}
          </button>
        </div>
      </div>
    </div>
  );
}
