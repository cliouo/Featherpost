import { useStore } from "../host/hooks";
import { Field } from "./Accounts";
import type { SettingsController } from "./controller";
import { BRAND } from "../brand";
export function Notifications({
  controller,
}: {
  controller: SettingsController;
}) {
  const s = useStore(controller.store),
    n = s.notification,
    r = s.rules;
  const edit = (patch: Partial<NonNullable<typeof r>>) =>
    r && controller.setRules({ ...r, ...patch });
  // 旧后台不回传这两个键：如实标为「需要后台升级」，而不是假装能保存。
  const baseline = s.rulesBaseline
    ? (JSON.parse(s.rulesBaseline) as Partial<NonNullable<typeof r>>)
    : {};
  const supportsNewMail = baseline.notify_new_mail !== undefined;
  const supportsMute = baseline.muted_accounts !== undefined;
  const muted = r?.muted_accounts ?? [];
  const notifyNewMail = r?.notify_new_mail ?? true;
  return (
    <>
      <h2>通知与规则</h2>
      <p className="page-intro">决定何时提醒，以及通知中显示多少内容。</p>
      <div className="setting-row">
        <div className="setting-copy">
          <strong>系统通知</strong>
          <p>
            {n
              ? notificationLabel(n.availability, n.authorization)
              : "正在读取授权状态"}
          </p>
        </div>
        <button
          className="secondary"
          type="button"
          disabled={
            !s.online ||
            s.busy ||
            !n ||
            n.availability !== "available" ||
            n.request_in_flight ||
            !["not_determined", "denied"].includes(n.authorization)
          }
          onClick={() => void controller.requestNotifications()}
        >
          请求授权
        </button>
      </div>
      {n?.authorization === "denied" && (
        <p className="group-note">
          在 macOS「系统设置 → 通知 → {BRAND.productName}」中允许通知，然后刷新此页。系统已拒绝时可能不会再次弹窗。
        </p>
      )}
      {r && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void controller.saveRules();
          }}
        >
          <fieldset disabled={!s.online || s.busy}>
            <div className="setting-row">
              <div className="setting-copy">
                <strong>收件箱新邮件系统通知</strong>
                <p>
                  {supportsNewMail
                    ? "新邮件进入收件箱时弹出系统通知；验证码仍按下方敏感内容规则显示。"
                    : "当前后台版本尚未提供此开关，升级后台后可用。"}
                </p>
              </div>
              <button
                type="button"
                className="toggle"
                role="switch"
                aria-checked={notifyNewMail}
                aria-label="收件箱新邮件系统通知"
                disabled={!supportsNewMail}
                onClick={() => edit({ notify_new_mail: !notifyNewMail })}
              >
                <span className="toggle-track" />
              </button>
            </div>
            <h3>按账户静音通知</h3>
            <p className="group-note">
              {supportsMute
                ? "勾选的账户照常收信，只是不再弹出通知。"
                : "当前后台版本尚未提供按账户静音，升级后台后可用。"}
            </p>
            <div className="muted-accounts" role="group" aria-label="按账户静音通知">
              {s.profiles === null && <small>账户名单暂时无法读取。</small>}
              {s.profiles?.length === 0 && <small>还没有账户。</small>}
              {s.profiles?.map((p) => (
                <label key={p.id} className="settings-check">
                  <input
                    type="checkbox"
                    disabled={!supportsMute || !notifyNewMail}
                    checked={muted.includes(p.id)}
                    onChange={(e) =>
                      edit({
                        muted_accounts: e.target.checked
                          ? [...muted.filter((id) => id !== p.id), p.id]
                          : muted.filter((id) => id !== p.id),
                      })
                    }
                  />
                  静音 {p.display_name || p.address}
                  <small>{p.address}</small>
                </label>
              ))}
            </div>
            <div className="setting-row">
              <div className="setting-copy">
                <strong>显示敏感内容</strong>
                <p>允许通知显示验证码等敏感内容。</p>
              </div>
              <button
                type="button"
                className="toggle"
                role="switch"
                aria-checked={r.show_sensitive}
                aria-label="显示敏感内容"
                onClick={() => edit({ show_sensitive: !r.show_sensitive })}
              >
                <span className="toggle-track" />
              </button>
            </div>
            <Field
              label="静默关键词"
              hint="每行一个。命中的通知不提醒，不影响邮件接收。"
            >
              <textarea
                rows={4}
                value={r.silent_keywords.join("\n")}
                maxLength={8192}
                onChange={(e) =>
                  edit({ silent_keywords: e.target.value.split("\n") })
                }
              />
            </Field>
            <h3>静默时段</h3>
            <p className="group-note">
              按下面的固定 UTC 偏移计算；不会自动随夏令时切换。
            </p>
            {r.quiet_hours.map((q, i) => (
              <div key={i} className="quiet-hours">
                <Field label={`第 ${i + 1} 段开始`}>
                  <input
                    type="time"
                    required
                    value={q.start === "24:00" ? "00:00" : q.start}
                    onChange={(e) =>
                      edit({
                        quiet_hours: r.quiet_hours.map((x, j) =>
                          i === j ? { ...x, start: e.target.value } : x,
                        ),
                      })
                    }
                  />
                </Field>
                <Field label={`第 ${i + 1} 段结束`}>
                  <input
                    type="time"
                    required
                    value={q.end === "24:00" ? "00:00" : q.end}
                    onChange={(e) =>
                      edit({
                        quiet_hours: r.quiet_hours.map((x, j) =>
                          i === j ? { ...x, end: e.target.value } : x,
                        ),
                      })
                    }
                  />
                </Field>
                <button
                  type="button"
                  className="text-button"
                  aria-label={`移除第 ${i + 1} 段静默时段`}
                  onClick={() =>
                    edit({
                      quiet_hours: r.quiet_hours.filter((_, j) => j !== i),
                    })
                  }
                >
                  移除
                </button>
              </div>
            ))}
            <button
              type="button"
              className="secondary"
              disabled={r.quiet_hours.length >= 32}
              onClick={() =>
                edit({
                  quiet_hours: [
                    ...r.quiet_hours,
                    { start: "22:00", end: "08:00" },
                  ],
                })
              }
            >
              添加时段
            </button>
            <Field
              label="UTC 偏移（分钟）"
              hint="例如 UTC+8 为 480，UTC−5 为 −300。"
            >
              <input
                type="number"
                min={-1440}
                max={1440}
                step={1}
                required
                value={r.timezone_offset_minutes}
                onChange={(e) =>
                  edit({ timezone_offset_minutes: Number(e.target.value) })
                }
              />
            </Field>
            <div className="settings-actions">
              <button
                type="submit"
                className="primary"
                disabled={!controller.dirty}
              >
                应用规则
              </button>
              <button
                type="button"
                className="text-button"
                disabled={!controller.dirty}
                onClick={() => controller.discard()}
              >
                还原输入
              </button>
            </div>
          </fieldset>
        </form>
      )}
      <p className="group-note" role="status">
        {s.rulesPersisted === true
          ? "规则已持久保存，重启后台后继续生效。"
          : s.rulesPersisted === false
            ? "规则仅在当前后台运行期间生效；重启后恢复配置文件中的规则。"
            : "后台尚未说明规则是否持久保存；保存回执会明确说明。"}
      </p>
    </>
  );
}
function notificationLabel(availability: string, authorization: string) {
  if (availability !== "available")
    return availability === "identity_mismatch"
      ? "当前开发身份不能申请通知，请使用受支持的安装包。"
      : "当前平台或后台未提供系统通知。";
  return (
    (
      {
        checking: "正在检查",
        not_determined: "尚未授权",
        denied: "系统已拒绝",
        authorized: "已允许",
        provisional: "临时允许",
        ephemeral: "临时允许",
        not_applicable: "不适用",
      } as Record<string, string>
    )[authorization] ?? "授权状态未知"
  );
}
